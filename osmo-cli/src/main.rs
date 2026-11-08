mod ble_btleplug;

use clap::{Parser, Subcommand, ValueEnum};
use osmo_cli::listing::{
    dest_filename, download_summary, format_listing, human_size, parse_utc_offset, progress_line,
    validate_get_path, zone_from_link, LsFilter,
};
use osmo_cli::listing::{format_marks, Kind};
use osmo_cli::{cameras, config, route::route_warning};
use osmo_core::ble::{
    model_name, pair_and_read_wifi, start_livestream_cancellable, PairOptions, StreamConfig,
    WifiCreds,
};
use osmo_core::datalink::{CameraSession, SessionConfig, SessionError};
use osmo_core::embedded_jpeg::image_kind;
use osmo_core::media::url_path;
use std::ffi::OsString;
use std::io::{BufRead, Read};
use std::net::{IpAddr, UdpSocket};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;

const CAMERA_IP: &str = "192.168.2.1";

#[derive(Parser)]
#[command(name = "osmo", about = "Talk to DJI Osmo cameras")]
struct Cli {
    /// Print protocol events (handshake, playback route, each list batch) to stderr.
    #[arg(short, long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum StreamResolution {
    #[value(name = "480p")]
    P480,
    #[value(name = "720p")]
    P720,
    #[value(name = "1080p")]
    P1080,
}

#[derive(Clone, Copy, ValueEnum)]
enum StreamFps {
    #[value(name = "25")]
    F25,
    #[value(name = "30")]
    F30,
}

#[derive(Clone, Copy, ValueEnum)]
enum StreamStabilization {
    Off,
    RockSteady,
    HorizonSteady,
    RockSteadyPlus,
    HorizonBalancing,
}

#[derive(Clone, Copy, ValueEnum)]
enum StreamModel {
    Action4,
    Action5Pro,
    Other,
}

#[derive(Subcommand)]
enum Cmd {
    /// List remembered cameras; optionally scan for nearby cameras.
    Cameras {
        /// Scan over Bluetooth and include newly discovered cameras.
        #[arg(long)]
        scan: bool,
        /// Scan duration in seconds.
        #[arg(long, default_value_t = 8)]
        seconds: u64,
    },
    /// Sync GPS to the camera from newline-delimited JSON on stdin (R-SDK).
    Gps {
        /// Camera identifier or name (defaults to the last paired camera).
        #[arg(long)]
        device: Option<String>,
    },
    /// Start an RTMP livestream over Bluetooth. Set OSMO_RTMP_URL in .env; Ctrl-C sends stop.
    Stream {
        /// Camera identifier or name (defaults to the last paired camera).
        #[arg(long)]
        device: Option<String>,
        #[arg(long, value_enum, default_value = "1080p")]
        resolution: StreamResolution,
        /// Bitrate in kilobits per second.
        #[arg(long, default_value_t = 6000, value_parser = clap::value_parser!(u16).range(1..))]
        bitrate: u16,
        #[arg(long, value_enum, default_value = "30")]
        fps: StreamFps,
        /// Stabilization effect is not hardware-verified.
        #[arg(long, value_enum, default_value = "off")]
        stabilization: StreamStabilization,
        #[arg(long, value_enum, default_value = "action4")]
        model: StreamModel,
    },
    /// Run an osmo command on camera Wi-Fi, then reconnect (macOS).
    WithWifi {
        /// Network to return to if macOS hides the current SSID. Can also use OSMO_RETURN_TO.
        #[arg(long)]
        return_to: Option<String>,
        /// osmo arguments after --, e.g. `-- -v ls`.
        #[arg(last = true, required = true)]
        args: Vec<OsString>,
    },
    /// Scan for cameras over Bluetooth.
    Scan {
        #[arg(long, default_value_t = 8)]
        seconds: u64,
    },
    /// Pair with a camera (approve on its screen when asked).
    Pair {
        #[arg(long)]
        device: Option<String>,
    },
    /// Print the camera's WiFi SSID and password.
    Wifi {
        #[arg(long)]
        device: Option<String>,
    },
    /// List the newest media on the camera (this machine must be on the camera's WiFi).
    Ls {
        #[arg(long, default_value = CAMERA_IP)]
        ip: String,
        /// Datalink UDP port. Default: the one the saved camera's model speaks (9004 for DJI, 10004 for Xtra).
        #[arg(long)]
        port: Option<u16>,
        /// Also save the raw reply datagrams here, to reproduce a decode problem offline. Mostly file names and
        /// sizes, but also the camera's status pushes (device info, firmware, battery): review before sharing.
        #[arg(long)]
        dump: Option<PathBuf>,
        /// Walk every page instead of only the newest 45 files per store.
        #[arg(long)]
        all: bool,
        /// Files per store per query, 1 to 255 (default 45). A small value exercises paging on a small card.
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..))]
        page_size: Option<u8>,
        /// Only photos.
        #[arg(long, conflicts_with = "videos")]
        photos: bool,
        /// Only videos.
        #[arg(long)]
        videos: bool,
        /// Only favourites (starred).
        #[arg(long)]
        starred: bool,
        /// Only videos the camera says carry highlight marks (the camera does the filtering, over the whole store).
        #[arg(long)]
        highlights: bool,
    },
    /// Show a video's highlight marks (the moments tagged while recording). PATH is as `osmo ls` prints it.
    /// UNVERIFIED on an Action 4: the tested clip returned 'bad parameter' (0xe3); its cause is unknown.
    Marks {
        path: String,
        #[arg(long, default_value = CAMERA_IP)]
        ip: String,
        /// Datalink UDP port. Default: the one the saved camera's model speaks (9004 for DJI, 10004 for Xtra).
        #[arg(long)]
        port: Option<u16>,
    },
    /// List the frames of a burst or interval group. Not yet verified on an Action 4.
    Burst {
        /// The group lead path as printed by `osmo ls`.
        path: String,
        #[arg(long, default_value = CAMERA_IP)]
        ip: String,
        /// Datalink UDP port. Default: the one the saved camera's model speaks (9004 for DJI, 10004 for Xtra).
        #[arg(long)]
        port: Option<u16>,
    },
    /// Save just a slice of a video, in original quality (no re-encoding; the cut lands on the keyframe at or before
    /// the start). Needs `ffmpeg`. Times are seconds, m:ss or h:mm:ss.
    Trim {
        path: String,
        /// Where the slice starts, e.g. 0:10.
        #[arg(long)]
        from: String,
        /// Where the slice ends, e.g. 0:25.
        #[arg(long)]
        to: String,
        /// Directory to save into.
        #[arg(short, long, default_value = ".")]
        out: PathBuf,
        #[arg(long, default_value = CAMERA_IP)]
        ip: String,
        /// Datalink UDP port. Default: the one the saved camera's model speaks.
        #[arg(long)]
        port: Option<u16>,
    },
    /// Save one full-resolution frame of a video as a JPEG. Needs `ffmpeg`. Time is seconds, m:ss or h:mm:ss.
    Frame {
        path: String,
        /// The moment to grab, e.g. 1:23.5.
        #[arg(long)]
        at: String,
        /// Directory to save into.
        #[arg(short, long, default_value = ".")]
        out: PathBuf,
        #[arg(long, default_value = CAMERA_IP)]
        ip: String,
        /// Datalink UDP port. Default: the one the saved camera's model speaks.
        #[arg(long)]
        port: Option<u16>,
    },
    /// Delete files from the camera. IRREVERSIBLE. Paths are exactly as `osmo ls` prints them (no globs). Without
    /// --yes it only prints what it would delete. Older pages are searched until every path is found.
    Rm {
        #[arg(required = true)]
        paths: Vec<String>,
        /// Really delete. Without this nothing is sent to the camera.
        #[arg(long)]
        yes: bool,
        #[arg(long, default_value = CAMERA_IP)]
        ip: String,
        /// Datalink UDP port. Default: the one the saved camera's model speaks (9004 for DJI, 10004 for Xtra).
        #[arg(long)]
        port: Option<u16>,
    },
    /// Set the camera's clock and time zone from this computer's. NOT WORKING on an Action 4: it answers 'refused' (0xe0).
    SyncTime {
        #[arg(long, default_value = CAMERA_IP)]
        ip: String,
        /// Datalink UDP port. Default: the one the saved camera's model speaks (9004 for DJI, 10004 for Xtra).
        #[arg(long)]
        port: Option<u16>,
    },
    /// Save a file's thumbnail as an image. PATH is the camera path exactly as `osmo ls` prints it.
    Thumb {
        path: String,
        /// Where to write the image.
        #[arg(short, long)]
        out: PathBuf,
        #[arg(long, default_value = CAMERA_IP)]
        ip: String,
        /// Datalink UDP port. Default: the one the saved camera's model speaks (9004 for DJI, 10004 for Xtra).
        #[arg(long)]
        port: Option<u16>,
        /// Skip the listing and use this store (0 = SD card, 1 = internal). This opens no datalink.
        #[arg(long)]
        storage: Option<u8>,
    },
    /// Favourite (star) a file on the camera. PATH is the camera path as `osmo ls` prints it. --off does not work on an Action 4 yet.
    Star {
        path: String,
        /// Remove the star instead of adding it. NOT WORKING on an Action 4: the camera says OK but keeps the star.
        #[arg(long)]
        off: bool,
        #[arg(long, default_value = CAMERA_IP)]
        ip: String,
        /// Datalink UDP port. Default: the one the saved camera's model speaks (9004 for DJI, 10004 for Xtra).
        #[arg(long)]
        port: Option<u16>,
    },
    /// Download files, resuming partial ones. Paths are exactly as `osmo ls` prints them.
    Get {
        /// One or more camera paths exactly as `osmo ls` prints them. Several are fetched in order; a completed
        /// file is skipped, a failure is reported and the rest continue (a stall stops the batch).
        #[arg(required = true)]
        paths: Vec<String>,
        /// Directory to save into.
        #[arg(short, long, default_value = ".")]
        out: PathBuf,
        #[arg(long, default_value = CAMERA_IP)]
        ip: String,
        /// Datalink UDP port. Default: the one the saved camera's model speaks (9004 for DJI, 10004 for Xtra).
        #[arg(long)]
        port: Option<u16>,
        /// Skip the listing and fetch by path from this store (0 = SD card, 1 = internal). This opens no datalink,
        /// so nothing holds the camera's WiFi up.
        #[arg(long)]
        storage: Option<u8>,
        /// Don't hold the datalink open while downloading. By default it is held: the camera puts its WiFi
        /// to sleep when idle, and a long transfer has quiet gaps between the camera's periodic cuts.
        #[arg(long)]
        no_keepalive: bool,
        /// Also fetch the unlisted `.DNG` beside a still or `.WAV` beside a clip, when the camera has one.
        #[arg(long)]
        sidecars: bool,
        /// Download each video's small low-res proxy instead of the full clip (the `.LRF` the camera keeps for
        /// scrubbing). Photos and clips without a proxy are reported and skipped.
        #[arg(long)]
        proxy: bool,
    },
}

fn session_err(e: SessionError) -> String {
    match e {
        SessionError::NoHandshake { .. } => format!(
            "{e}\nHint: run `osmo wifi` for the camera's WiFi name and password, join it, then retry."
        ),
        _ => e.to_string(),
    }
}

fn warn_route(ip: &str, port: u16) {
    let local = UdpSocket::bind("0.0.0.0:0").ok().and_then(|s| {
        s.connect((ip, port)).ok()?;
        match s.local_addr().ok()?.ip() {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(_) => None,
        }
    });
    if let Some(w) = route_warning(ip, local) {
        eprintln!("warning: {w}");
    }
}

/// The datalink port for this run: the flag, else the last-used camera's model profile, else 9004.
fn datalink_port(flag: Option<u16>) -> u16 {
    let last = config::last_device();
    let saved = cameras::load().ok().and_then(|all| {
        let id = last?;
        all.into_iter().find(|c| c.id.eq_ignore_ascii_case(&id))
    });
    cameras::datalink_port(flag, saved.as_ref())
}

fn open_session(
    ip: &str,
    port: u16,
    verbose: bool,
) -> Result<CameraSession<osmo_core::datalink::UdpPeer>, String> {
    warn_route(ip, port);
    // The line is either 9004 with the TCP poke or 10004 without it.
    let cfg = SessionConfig {
        port,
        tcp_poke: port == 9004,
        ..SessionConfig::action4(ip)
    };
    let mut s = CameraSession::open(&cfg).map_err(session_err)?;
    if verbose {
        s.set_logger(|l| eprintln!("  {l}"));
    }
    Ok(s)
}

fn warn_if_unconfirmed(s: &CameraSession<osmo_core::datalink::UdpPeer>) {
    if !s.diagnostics().playback_confirmed {
        eprintln!("warning: the camera did not report playback mode, so the list may be empty or incomplete; run it again (add -v to see why)");
    }
}

fn ls_cmd(
    ip: &str,
    port: u16,
    dump: Option<&PathBuf>,
    all: bool,
    page_size: Option<u8>,
    filter: LsFilter,
    verbose: bool,
) -> Result<(), String> {
    let cam = sdk_camera(ip, port, verbose);
    eprintln!("Listing {ip}:{port} ...");
    let report = cam.list_report(&filter, all, page_size).map_err(sdk_err)?;
    if !report.status.is_empty() {
        eprintln!("{}", report.status);
    }
    if let Some(path) = dump {
        match &report.manifest {
            Some(blob) => {
                std::fs::write(path, blob)
                    .map_err(|e| format!("could not write {}: {e}", path.display()))?;
                eprintln!("Raw manifest saved to {}", path.display());
            }
            None => eprintln!(
                "Not saving {}: this run captured no media records (keeping any earlier dump).",
                path.display()
            ),
        }
    }
    println!("{}", format_listing(&report.files));
    Ok(())
}

/// The persisted write counter, so a second process continues the sequence the camera has already seen.
struct DiskCounter;

impl osmo_sdk::camera::WriteCounter for DiskCounter {
    fn next(&self) -> u32 {
        config::next_write_counter()
    }
    fn save(&self, next: u32) {
        let _ = config::save_write_counter(next);
    }
}

fn sdk_camera(ip: &str, port: u16, verbose: bool) -> osmo_sdk::camera::Camera {
    warn_route(ip, port);
    let mut cam = osmo_sdk::camera::Camera::new(ip, port);
    cam.set_write_counter(Arc::new(DiskCounter));
    cam.set_notice(|m| eprintln!("warning: {m}"));
    if verbose {
        cam.set_logger(|l| eprintln!("  {l}"));
    }
    cam
}

fn sdk_err(e: osmo_sdk::camera::SdkError) -> String {
    let text = e.to_string();
    if text.starts_with("no response from") {
        format!("{text}\nHint: run `osmo wifi` for the camera's WiFi name and password, join it, then retry.")
    } else {
        text
    }
}

fn star_cmd(path: &str, on: bool, ip: &str, port: u16, verbose: bool) -> Result<(), String> {
    let path = validate_get_path(path)?;
    eprintln!("Sending favourite {} ...", if on { "on" } else { "off" });
    sdk_camera(ip, port, verbose)
        .set_favourite(path, on)
        .map_err(sdk_err)?;
    println!(
        "The camera accepted it: {path} {}",
        if on {
            "is now starred"
        } else {
            "is no longer starred"
        }
    );
    Ok(())
}

// shortcut: macOS/Linux only (`date`, /etc/localtime); Windows needs another source, when a Windows shell exists.
fn local_time() -> Result<(u64, i16, String), String> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let z = std::process::Command::new("date")
        .arg("+%z")
        .output()
        .map_err(|e| format!("could not run `date`: {e}"))?;
    let offset = parse_utc_offset(&String::from_utf8_lossy(&z.stdout))
        .ok_or("could not read this computer's UTC offset from `date +%z`")?;
    let zone = std::fs::read_link("/etc/localtime")
        .ok()
        .and_then(|p| zone_from_link(&p.to_string_lossy()))
        .ok_or("could not read this computer's time zone name from /etc/localtime")?;
    Ok((secs, offset, zone))
}

fn sync_time_cmd(ip: &str, port: u16, verbose: bool) -> Result<(), String> {
    let (secs, offset, zone) = local_time()?;
    let mut sess = open_session(ip, port, verbose)?;
    sess.register().map_err(session_err)?;
    eprintln!("Setting the camera clock to {secs} (UTC{offset:+} min, {zone}) ...");
    match sess.sync_time(secs, offset, &zone) {
        Ok(()) => println!("The camera acknowledged the new time."),
        Err(osmo_core::datalink::WriteError::NoReply) => println!(
            "Sent, but the camera did not acknowledge (Osmosis does not wait for one either): check the clock on the camera."
        ),
        Err(e) => return Err(e.to_string()),
    }
    Ok(())
}

fn marks_cmd(path: &str, ip: &str, port: u16, verbose: bool) -> Result<(), String> {
    let path = validate_get_path(path)?;
    let mut sess = open_session(ip, port, verbose)?;
    eprintln!("Looking up {path} ...");
    let found = sess.find_file(path).map_err(session_err)?;
    warn_if_unconfirmed(&sess);
    let file = found.ok_or(
        "not found anywhere in the camera's library: check the path against `osmo ls --all`",
    )?;
    if !file.is_video() {
        return Err(format!(
            "{path} is not a video, so it has no highlight marks"
        ));
    }
    let marks = sess.highlights(&file).map_err(|e| e.to_string())?;
    println!("{}", format_marks(&marks));
    Ok(())
}

fn burst_cmd(path: &str, ip: &str, port: u16, verbose: bool) -> Result<(), String> {
    let path = validate_get_path(path)?;
    let mut sess = open_session(ip, port, verbose)?;
    eprintln!("Looking up {path} ...");
    let file = sess.find_file(path).map_err(session_err)?.ok_or(
        "not found anywhere in the camera's library: check the path against `osmo ls --all`",
    )?;
    let files = sess.expand_burst_group(&file).map_err(session_err)?;
    println!("{}", format_listing(&files));
    Ok(())
}

/// Run `ffmpeg` on a video straight from the camera's HTTP address while the datalink is held open. `name_for`
/// turns the camera's file name into the output name; `args_for` builds the arguments from the input address and
/// the output path.
fn ffmpeg_on_camera(
    path: &str,
    ip: &str,
    port: u16,
    out_dir: &std::path::Path,
    verbose: bool,
    name_for: impl FnOnce(&str) -> String,
    args_for: impl FnOnce(&str, &str) -> Vec<String>,
) -> Result<std::path::PathBuf, String> {
    let path = validate_get_path(path)?;
    match std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
    {
        Ok(o) if o.status.success() => {}
        _ => return Err(
            "ffmpeg was not found: install it (for example `brew install ffmpeg`) and try again"
                .into(),
        ),
    }
    let mut sess = open_session(ip, port, verbose)?;
    eprintln!("Looking up {path} ...");
    let found = sess.find_file(path).map_err(session_err)?;
    warn_if_unconfirmed(&sess);
    let file = found.ok_or(
        "not found anywhere in the camera's library: check the path against `osmo ls --all`",
    )?;
    if !file.is_video() {
        return Err(format!("{path} is not a video"));
    }
    let url = format!("http://{ip}{}", url_path(file.storage, &file.path));
    std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    let dest = out_dir.join(name_for(&file.name));
    if dest.exists() {
        return Err(format!(
            "{} already exists; move it or choose another folder",
            dest.display()
        ));
    }
    let args = args_for(&url, &dest.to_string_lossy());
    let held = hold_datalink(sess);
    let status = std::process::Command::new("ffmpeg").args(&args).status();
    release_datalink(held);
    match status {
        Ok(s) if s.success() && dest.exists() => Ok(dest),
        Ok(s) => Err(format!("ffmpeg failed ({s}); nothing usable was saved")),
        Err(e) => Err(format!("could not run ffmpeg: {e}")),
    }
}

fn trim_cmd(
    path: &str,
    from: &str,
    to: &str,
    out: &std::path::Path,
    ip: &str,
    port: u16,
    verbose: bool,
) -> Result<(), String> {
    let (from_ms, to_ms) = osmo_cli::listing::trim_range(from, to)?;
    let path = validate_get_path(path)?;
    let dest = out.join(osmo_cli::listing::clip_name(
        dest_filename(path),
        from_ms,
        to_ms,
    ));
    if dest.exists() {
        return Err(format!(
            "{} already exists; move it or choose another folder",
            dest.display()
        ));
    }
    let mut last = Instant::now() - Duration::from_secs(1);
    let outcome = sdk_camera(ip, port, verbose)
        .trim(
            path,
            from_ms as f64 / 1000.0,
            to_ms as f64 / 1000.0,
            &dest,
            &mut |done, total| {
                if last.elapsed() >= Duration::from_millis(250) {
                    eprint!("\r{}      ", progress_line(done, Some(total)));
                    last = Instant::now();
                }
            },
        )
        .map_err(sdk_err)?;
    eprintln!();
    println!("Saved {}", dest.display());
    if let Some(w) = osmo_cli::listing::short_clip_warning(to_ms - from_ms, outcome.duration_secs) {
        eprintln!("warning: {w}");
    }
    Ok(())
}

fn frame_cmd(
    path: &str,
    at: &str,
    out: &std::path::Path,
    ip: &str,
    port: u16,
    verbose: bool,
) -> Result<(), String> {
    let at_ms = osmo_cli::listing::parse_clock(at)?;
    let dest = ffmpeg_on_camera(
        path,
        ip,
        port,
        out,
        verbose,
        |name| osmo_cli::listing::frame_name(name, at_ms),
        |url, dest| osmo_cli::listing::frame_args(url, at_ms, dest),
    )?;
    println!("Saved {}", dest.display());
    Ok(())
}

fn rm_cmd(paths: &[String], yes: bool, ip: &str, port: u16, verbose: bool) -> Result<(), String> {
    let cam = sdk_camera(ip, port, verbose);
    eprintln!("Listing {ip}:{port} ...");
    let plan = match cam.plan_delete(paths) {
        Ok(p) => p,
        Err(osmo_sdk::camera::SdkError::Refused(refusals)) => {
            for r in &refusals {
                eprintln!("refused: {r}");
            }
            return Err("nothing was deleted".to_string());
        }
        Err(e) => return Err(sdk_err(e)),
    };
    if !yes {
        println!("would delete:");
        for f in &plan {
            println!("  {:>10}  {}", human_size(f.size_bytes), f.path);
        }
        println!(
            "Nothing was deleted. Add --yes to delete {} file(s); this cannot be undone.",
            plan.len()
        );
        return Ok(());
    }
    cam.delete(paths).map_err(sdk_err)?;
    println!("deleted {} file(s)", plan.len());
    Ok(())
}

fn thumb_cmd(
    path: &str,
    out: &PathBuf,
    ip: &str,
    port: u16,
    storage: Option<u8>,
    verbose: bool,
) -> Result<(), String> {
    let path = validate_get_path(path)?;
    let cam = sdk_camera(ip, port, verbose);
    if storage.is_none() {
        eprintln!("Looking up {path} ...");
    }
    let thumb = cam.thumbnail_from(path, storage).map_err(sdk_err)?;
    let kind = image_kind(&thumb.bytes).unwrap_or("image");
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(out, &thumb.bytes)
        .map_err(|e| format!("could not write {}: {e}", out.display()))?;
    println!(
        "Saved {} ({}, {kind}, from {})",
        out.display(),
        human_size(thumb.bytes.len() as u64),
        thumb.label
    );
    Ok(())
}

#[derive(Clone, Copy)]
struct GetOpts<'a> {
    ip: &'a str,
    port: u16,
    storage: Option<u8>,
    keepalive: bool,
    sidecars: bool,
    proxy: bool,
    verbose: bool,
}

type Held = (
    Arc<AtomicBool>,
    std::thread::JoinHandle<CameraSession<osmo_core::datalink::UdpPeer>>,
);

/// Keep the datalink alive on a background thread: the camera puts its Wi-Fi to sleep when idle, and a long
/// transfer has quiet gaps.
fn hold_datalink(mut s: CameraSession<osmo_core::datalink::UdpPeer>) -> Held {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let handle = std::thread::spawn(move || {
        while !flag.load(Ordering::Relaxed) {
            s.keepalive_tick();
        }
        s
    });
    (stop, handle)
}

fn release_datalink((stop, handle): Held) {
    stop.store(true, Ordering::Relaxed);
    if let Ok(mut s) = handle.join() {
        s.close();
    }
}

fn get_cmd(paths: &[String], out: &std::path::Path, opts: GetOpts) -> Result<(), String> {
    use osmo_sdk::camera::{GetEvent, GetOptions};
    let cam = sdk_camera(opts.ip, opts.port, opts.verbose);
    let mut last = Instant::now() - Duration::from_secs(1);
    let main_progress = |done: u64, size: Option<u64>, last: &mut Instant| {
        if last.elapsed() >= Duration::from_millis(250) {
            eprint!("\r{}      ", progress_line(done, size));
            *last = Instant::now();
        }
    };
    let report = cam
        .download_many(
            paths,
            out,
            GetOptions {
                storage: opts.storage,
                keepalive: opts.keepalive,
                sidecars: opts.sidecars,
                proxy: opts.proxy,
            },
            &mut |ev| match ev {
                GetEvent::Lookup(n) => eprintln!("Looking up {n} file(s) ..."),
                GetEvent::NotFound(m) => eprintln!(
                    "not found anywhere in the camera's library: {m} (check it against `osmo ls --all`)"
                ),
                GetEvent::NoProxy(why) => eprintln!("{why}"),
                GetEvent::Start { index, total, path } => {
                    last = Instant::now() - Duration::from_secs(1);
                    if total > 1 {
                        eprintln!("[{}/{}] {path}", index + 1, total);
                    }
                }
                GetEvent::Progress { done, size } => main_progress(done, size, &mut last),
                GetEvent::FileDone { dest, size } => {
                    let len = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
                    eprintln!("\r{}      ", progress_line(len, size));
                }
                GetEvent::NoSidecar => println!("no sidecar"),
                GetEvent::SidecarProgress { done, size } => {
                    main_progress(done, Some(size), &mut last)
                }
                GetEvent::SidecarSaved(dest) => {
                    eprintln!();
                    println!("Saved sidecar {}", dest.display());
                    last = Instant::now() - Duration::from_secs(1);
                }
                GetEvent::SidecarFailed { name, message } => {
                    eprintln!();
                    eprintln!("sidecar {name}: {message}");
                    last = Instant::now() - Duration::from_secs(1);
                }
                GetEvent::Saved(dest) => println!("Saved {}", dest.display()),
                GetEvent::AlreadyComplete(dest) => println!("Already complete: {}", dest.display()),
                GetEvent::Failed { path, message } => {
                    eprintln!();
                    eprintln!("failed {path}: {message}")
                }
                GetEvent::Stopping { left } => eprintln!(
                    "stopping: {left} file(s) not tried; the camera's WiFi is probably gone"
                ),
            },
        )
        .map_err(sdk_err)?;
    if report.total > 1 || report.not_found > 0 {
        println!(
            "{}",
            download_summary(report.saved, report.skipped, report.failed)
        );
    }
    if report.failed > 0 {
        return Err(format!("{} file(s) not downloaded", report.failed));
    }
    Ok(())
}

fn remember_camera(camera: &ble_btleplug::Found) {
    let saved = cameras::SavedCamera {
        id: camera.id.clone(),
        name: camera.name.clone(),
        model_id: camera.model_id,
    };
    if let Err(e) = cameras::remember(&saved) {
        eprintln!("warning: could not remember camera: {e}");
    }
    if let Err(e) = config::save_last_device(&camera.id) {
        eprintln!("warning: could not save last camera: {e}");
    }
}

fn cameras_cmd(scan: bool, seconds: u64) -> Result<(), String> {
    let saved = cameras::load().map_err(|e| format!("could not load saved cameras: {e}"))?;
    let visible = if scan {
        let rt = Runtime::new().map_err(|e| e.to_string())?;
        let adapter = ble_btleplug::adapter(&rt)?;
        let (found, _) = ble_btleplug::scan(&rt, &adapter, seconds)?;
        Some(
            found
                .into_iter()
                .map(|c| cameras::VisibleCamera {
                    id: c.id,
                    name: c.name,
                    model_id: c.model_id,
                })
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
    println!(
        "{}",
        cameras::format_listing(&saved, visible.as_deref(), config::last_device().as_deref())
    );
    Ok(())
}

fn camera_wifi_creds(
    rt: &Arc<Runtime>,
    adapter: &btleplug::platform::Adapter,
    device: Option<String>,
) -> Result<WifiCreds, String> {
    let wanted = device.or_else(config::last_device);
    let cam = ble_btleplug::find_camera(rt, adapter, wanted.as_deref(), 3, &|| false)?
        .ok_or("no matching camera found: is it on and awake? try `osmo scan`")?;
    eprintln!(
        "Connecting to {} ...",
        cam.name.as_deref().unwrap_or(&cam.id)
    );
    let mut link = ble_btleplug::connect(rt.clone(), &cam)?;
    let opts = PairOptions {
        identifier: config::load_or_create_identity().map_err(|e| e.to_string())?,
        ..Default::default()
    };
    let creds = pair_and_read_wifi(&mut link, &opts, &mut |s| eprintln!("{s}..."))
        .map_err(|e| e.to_string())?;
    remember_camera(&cam);
    Ok(creds)
}

fn wake_camera_for_wifi() -> Result<WifiCreds, String> {
    let rt = Arc::new(Runtime::new().map_err(|e| e.to_string())?);
    let adapter = ble_btleplug::adapter(&rt)?;
    camera_wifi_creds(&rt, &adapter, None)
}

fn pair_cmd(
    rt: &Arc<Runtime>,
    adapter: &btleplug::platform::Adapter,
    device: Option<String>,
    show: bool,
) -> Result<(), String> {
    let creds = camera_wifi_creds(rt, adapter, device)?;
    println!("SSID: {}", creds.ssid);
    if show {
        match creds.password {
            Some(p) => println!("Password: {p}"),
            None => println!("Password: (camera did not return one over Bluetooth)"),
        }
    } else {
        println!("Paired.");
    }
    Ok(())
}

fn wraps_camera_control(args: &[OsString]) -> bool {
    let arguments = std::iter::once(OsString::from("osmo")).chain(args.iter().cloned());
    match Cli::try_parse_from(arguments) {
        Ok(Cli {
            cmd: Cmd::Stream { .. } | Cmd::Gps { .. },
            ..
        }) => true,
        Ok(Cli {
            cmd: Cmd::WithWifi { args, .. },
            ..
        }) => wraps_camera_control(&args),
        _ => false,
    }
}

struct StreamOpts {
    device: Option<String>,
    resolution: StreamResolution,
    bitrate: u16,
    fps: StreamFps,
    stabilization: StreamStabilization,
    model: StreamModel,
    verbose: bool,
}

fn stream_cmd(options: StreamOpts) -> Result<(), String> {
    let verbose = options.verbose;
    let env =
        osmo_cli::stream::StreamEnv::load(std::path::Path::new(".env"), |k| std::env::var(k).ok())?;
    let resolution = match options.resolution {
        StreamResolution::P480 => 0x47,
        StreamResolution::P720 => 0x04,
        StreamResolution::P1080 => 0x0a,
    };
    let fps = match options.fps {
        StreamFps::F25 => 2,
        StreamFps::F30 => 3,
    };
    let eis = match options.stabilization {
        StreamStabilization::Off => 0,
        StreamStabilization::RockSteady => 1,
        StreamStabilization::HorizonSteady => 2,
        StreamStabilization::RockSteadyPlus => 3,
        StreamStabilization::HorizonBalancing => 4,
    };
    let settings = StreamConfig {
        ssid: &env.ssid,
        password: &env.password,
        url: &env.url,
        resolution,
        bitrate_kbps: options.bitrate,
        fps,
        stabilization: match options.model {
            StreamModel::Other => None,
            _ => Some(eis),
        },
        action5_pro: matches!(options.model, StreamModel::Action5Pro),
    };
    // Reject oversized inputs before scanning, pairing, or changing the camera's network.
    osmo_core::duml::rtmp::connect_wifi(settings.ssid, settings.password, 0x8c19)
        .map_err(|e| e.to_string())?;
    osmo_core::duml::rtmp::configure_stream(
        settings.resolution,
        settings.bitrate_kbps,
        settings.fps,
        settings.url,
        settings.action5_pro,
    )
    .map_err(|e| e.to_string())?;
    let interrupted = Arc::new(AtomicBool::new(false));
    let flag = interrupted.clone();
    ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst))
        .map_err(|e| format!("could not install stop handler: {e}"))?;
    let rt = Arc::new(Runtime::new().map_err(|e| e.to_string())?);
    let adapter = ble_btleplug::adapter(&rt)?;
    let wanted = options.device.or_else(config::last_device);
    let found = ble_btleplug::find_camera(&rt, &adapter, wanted.as_deref(), 3, &|| {
        interrupted.load(Ordering::SeqCst)
    })?;
    if interrupted.load(Ordering::SeqCst) {
        return Err("interrupted before streaming".into());
    }
    let camera = found.ok_or("no matching camera found: is it on and awake? try `osmo scan`")?;
    eprintln!(
        "Connecting to {} ...",
        camera.name.as_deref().unwrap_or(&camera.id)
    );
    let mut link = ble_btleplug::connect(rt, &camera)?;
    let pair = PairOptions {
        identifier: config::load_or_create_identity().map_err(|e| e.to_string())?,
        ..Default::default()
    };
    let mut live = start_livestream_cancellable(
        &mut link,
        &pair,
        &settings,
        &mut |step| match step.strip_prefix("rx ") {
            Some(line) if verbose => eprintln!("  rx {line}"),
            Some(_) => {}
            None => eprintln!("{step} ..."),
        },
        &|| interrupted.load(Ordering::SeqCst),
    )
    .map_err(|e| {
        let wifi_hint = if e.to_string().contains("Wi-Fi connection") {
            "\nThe camera could not join that network: check its name and password, that the network is on (2.4 GHz is safest) and in range of the camera."
        } else {
            ""
        };
        format!(
            "{e}{wifi_hint}\nThe camera may now be on your Wi-Fi with its own hotspot off; restart the camera before using `osmo wifi`, `ls` or `get` again."
        )
    })?;
    remember_camera(&camera);
    eprintln!("Start acknowledged; check the RTMP server for video. Press Ctrl-C to stop.");
    let mut last_battery = None;
    while !interrupted.load(Ordering::SeqCst) {
        if let Some(pct) = live
            .poll(Duration::from_millis(250))
            .map_err(|e| e.to_string())?
        {
            if last_battery != Some(pct) {
                eprintln!("Camera battery {pct}%");
                last_battery = Some(pct);
            }
        }
    }
    live.stop().map_err(|e| e.to_string())?;
    eprintln!("Stop requested; check the camera or server to confirm.");
    Ok(())
}

enum GpsFeed {
    Line(String),
    Closed,
    Invalid,
}

fn gps_local_time() -> Result<(u64, u32, u32), String> {
    use chrono::{Datelike, Timelike};
    let now = chrono::Local::now();
    let year = u32::try_from(now.year()).map_err(|_| "local clock is invalid")?;
    if !(1..=9999).contains(&year) {
        return Err("local clock is invalid".into());
    }
    let date = year * 10_000 + now.month() * 100 + now.day();
    let time = now.hour() * 10_000 + now.minute() * 100 + now.second();
    let millis = u64::try_from(now.timestamp_millis()).map_err(|_| "local clock is invalid")?;
    Ok((millis, date, time))
}

fn gps_cmd(device: Option<String>) -> Result<(), String> {
    let interrupted = Arc::new(AtomicBool::new(false));
    let flag = interrupted.clone();
    ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst))
        .map_err(|e| format!("could not install stop handler: {e}"))?;
    let identity =
        config::load_or_create_rsdk_identity().map_err(|e| format!("R-SDK identity: {e}"))?;
    let rt = Arc::new(Runtime::new().map_err(|e| e.to_string())?);
    let adapter = ble_btleplug::adapter(&rt)?;
    let wanted = device.or_else(config::last_device);
    let found = ble_btleplug::find_camera(&rt, &adapter, wanted.as_deref(), 3, &|| {
        interrupted.load(Ordering::SeqCst)
    })?;
    if interrupted.load(Ordering::SeqCst) {
        return Err("interrupted before GPS connection".into());
    }
    let camera = found.ok_or("no matching camera found: is it on and awake? try `osmo scan`")?;
    eprintln!(
        "Connecting to {} ...",
        camera.name.as_deref().unwrap_or(&camera.id)
    );
    let mut link = ble_btleplug::connect_rsdk(rt, &camera)?;
    let random = osmo_core::duml::commands::new_install_identity();
    let challenge = u16::from_str_radix(&random[..4], 16)
        .map_err(|_| "could not create R-SDK challenge")?
        % 10_000;
    eprintln!("Waiting for R-SDK approval; approve on the camera if prompted ...");
    let mut session = osmo_core::rsdk::connect(
        &mut link,
        identity,
        challenge,
        Duration::from_secs(40),
        &|| interrupted.load(Ordering::SeqCst),
    )
    .map_err(|e| e.to_string())?;
    remember_camera(&camera);
    eprintln!("GPS connected. Reading newline-delimited GPS fixes from stdin; Ctrl-C disconnects.");
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    std::thread::spawn(move || {
        const MAX_LINE: u64 = 8192;
        let stdin = std::io::stdin();
        let mut reader = stdin.lock();
        loop {
            let mut bytes = Vec::new();
            let result = (&mut reader)
                .take(MAX_LINE + 1)
                .read_until(b'\n', &mut bytes);
            match result {
                Ok(0) => {
                    let _ = tx.send(GpsFeed::Closed);
                    break;
                }
                Ok(n) if n as u64 <= MAX_LINE => match String::from_utf8(bytes) {
                    Ok(line) => {
                        if tx.send(GpsFeed::Line(line)).is_err() {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = tx.send(GpsFeed::Invalid);
                        break;
                    }
                },
                _ => {
                    let _ = tx.send(GpsFeed::Invalid);
                    break;
                }
            }
        }
    });
    let mut feed = osmo_cli::gps::GpsFeedState::default();
    let mut closed = false;
    let mut sent = 0u64;
    let mut next_push = Instant::now();
    let mut stale_reported = false;
    let mut last_battery = None;
    while !interrupted.load(Ordering::SeqCst) {
        for _ in 0..16 {
            match rx.try_recv() {
                Ok(GpsFeed::Line(line)) => {
                    let input = osmo_cli::gps::GpsInput::parse(&line)?;
                    let (now, date, time) = gps_local_time()?;
                    if feed.accept(input, now, date, time)? {
                        stale_reported = false;
                    }
                }
                Ok(GpsFeed::Closed) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    closed = true;
                    break;
                }
                Ok(GpsFeed::Invalid) => {
                    return Err(
                        "GPS input must be UTF-8 JSON lines no longer than 8192 bytes".into(),
                    )
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
            }
        }
        let (now, date, time) = gps_local_time()?;
        if let Some(fix) = feed.next_fix(now, date, time, Instant::now() >= next_push, closed)? {
            session.send_fix(&fix).map_err(|e| e.to_string())?;
            sent += 1;
            next_push = Instant::now() + Duration::from_secs(1);
        } else if feed.is_stale(now) && !stale_reported {
            eprintln!("GPS fix is stale; waiting for a fresh fix.");
            stale_reported = true;
        }
        if closed {
            if sent == 0 {
                return Err("GPS input ended without a fresh fix".into());
            }
            eprintln!("GPS input ended; disconnecting.");
            return Ok(());
        }
        if let Some(status) = session
            .poll(Duration::from_millis(100))
            .map_err(|e| e.to_string())?
        {
            if status.battery_percent != last_battery {
                if let Some(battery) = status.battery_percent {
                    eprintln!("Camera battery {battery}%");
                }
                last_battery = status.battery_percent;
            }
        }
    }
    eprintln!("GPS sync stopped.");
    Ok(())
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Cameras { scan, seconds } => cameras_cmd(scan, seconds),
        Cmd::WithWifi { return_to, args } => {
            if wraps_camera_control(&args) {
                return Err(
                    "run `osmo stream` or `osmo gps` directly, without the camera Wi-Fi wrapper"
                        .into(),
                );
            }
            osmo_cli::wifi::run(return_to.as_deref(), &args, wake_camera_for_wifi)
        }
        Cmd::Stream {
            device,
            resolution,
            bitrate,
            fps,
            stabilization,
            model,
        } => stream_cmd(StreamOpts {
            device,
            resolution,
            bitrate,
            fps,
            stabilization,
            model,
            verbose: cli.verbose,
        }),
        Cmd::Gps { device } => gps_cmd(device),
        Cmd::Ls {
            ip,
            port,
            dump,
            all,
            page_size,
            photos,
            videos,
            starred,
            highlights,
        } => {
            let kind = match (photos, videos) {
                (true, _) => Kind::Photos,
                (_, true) => Kind::Videos,
                _ => Kind::All,
            };
            ls_cmd(
                &ip,
                datalink_port(port),
                dump.as_ref(),
                all,
                page_size,
                LsFilter {
                    kind,
                    starred,
                    highlights,
                },
                cli.verbose,
            )
        }
        Cmd::Get {
            paths,
            out,
            ip,
            port,
            storage,
            no_keepalive,
            sidecars,
            proxy,
        } => get_cmd(
            &paths,
            &out,
            GetOpts {
                ip: &ip,
                port: datalink_port(port),
                storage,
                keepalive: !no_keepalive,
                sidecars,
                proxy,
                verbose: cli.verbose,
            },
        ),
        Cmd::Thumb {
            path,
            out,
            ip,
            port,
            storage,
        } => thumb_cmd(&path, &out, &ip, datalink_port(port), storage, cli.verbose),
        Cmd::Trim {
            path,
            from,
            to,
            out,
            ip,
            port,
        } => trim_cmd(
            &path,
            &from,
            &to,
            &out,
            &ip,
            datalink_port(port),
            cli.verbose,
        ),
        Cmd::Frame {
            path,
            at,
            out,
            ip,
            port,
        } => frame_cmd(&path, &at, &out, &ip, datalink_port(port), cli.verbose),
        Cmd::SyncTime { ip, port } => sync_time_cmd(&ip, datalink_port(port), cli.verbose),
        Cmd::Marks { path, ip, port } => marks_cmd(&path, &ip, datalink_port(port), cli.verbose),
        Cmd::Burst { path, ip, port } => burst_cmd(&path, &ip, datalink_port(port), cli.verbose),
        Cmd::Rm {
            paths,
            yes,
            ip,
            port,
        } => rm_cmd(&paths, yes, &ip, datalink_port(port), cli.verbose),
        Cmd::Star {
            path,
            off,
            ip,
            port,
        } => star_cmd(&path, !off, &ip, datalink_port(port), cli.verbose),
        ble_cmd => {
            let rt = Arc::new(Runtime::new().map_err(|e| e.to_string())?);
            let adapter = ble_btleplug::adapter(&rt)?;
            match ble_cmd {
                Cmd::Scan { seconds } => {
                    let (cams, others) = ble_btleplug::scan(&rt, &adapter, seconds)?;
                    for c in &cams {
                        println!(
                            "{:<24} id={}  rssi={}  model={}",
                            c.name.as_deref().unwrap_or("?"),
                            c.id,
                            c.rssi.map_or("?".to_string(), |r| r.to_string()),
                            c.model_id.and_then(model_name).unwrap_or("unknown"),
                        );
                    }
                    println!(
                        "{} camera(s); {} other BLE device(s) seen, not listed",
                        cams.len(),
                        others
                    );
                    Ok(())
                }
                Cmd::Pair { device } => pair_cmd(&rt, &adapter, device, false),
                Cmd::Wifi { device } => pair_cmd(&rt, &adapter, device, true),
                Cmd::Ls { .. }
                | Cmd::Get { .. }
                | Cmd::Thumb { .. }
                | Cmd::Star { .. }
                | Cmd::Rm { .. }
                | Cmd::Trim { .. }
                | Cmd::Frame { .. }
                | Cmd::SyncTime { .. }
                | Cmd::Marks { .. }
                | Cmd::Burst { .. }
                | Cmd::WithWifi { .. }
                | Cmd::Stream { .. }
                | Cmd::Gps { .. }
                | Cmd::Cameras { .. } => {
                    unreachable!("handled above")
                }
            }
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn livestream_options_are_parsed_and_invalid_resolutions_rejected() {
        let cli = Cli::try_parse_from([
            "osmo",
            "stream",
            "--resolution",
            "720p",
            "--bitrate",
            "4000",
        ])
        .unwrap();
        assert!(matches!(cli.cmd, Cmd::Stream { bitrate: 4000, .. }));
        assert!(Cli::try_parse_from(["osmo", "stream", "--resolution", "4k"]).is_err());
    }

    #[test]
    fn wifi_wrapper_refuses_a_nested_livestream() {
        assert!(wraps_camera_control(&["-v".into(), "stream".into()]));
        assert!(!wraps_camera_control(&["get".into(), "stream".into()]));
        assert!(wraps_camera_control(&["gps".into()]));
    }
}
