mod ble_btleplug;

use clap::{Parser, Subcommand, ValueEnum};
use osmo_cli::listing::{
    dest_filename, download_error_message, download_summary, file_from_path, fill_unknown_sizes,
    format_listing, format_status, human_size, known_size, parse_utc_offset, plan_deletions,
    progress_line, resolve_requested, sidecars_to_fetch, thumb_sources, truncation_warning,
    validate_get_path, zone_from_link, ThumbSource,
};
use osmo_cli::listing::{filter_files, format_marks, Kind};
use osmo_cli::{cameras, config, route::route_warning};
use osmo_core::ble::{
    model_name, pair_and_read_wifi, start_livestream_cancellable, PairOptions, StreamConfig,
    WifiCreds,
};
use osmo_core::datalink::{has_media_records, CameraSession, SessionConfig, SessionError};
use osmo_core::embedded_jpeg::{embedded_jpeg, image_kind, HEAD_BYTES};
use osmo_core::http::{HttpClient, Outcome};
use osmo_core::media::url_path;
use osmo_core::media::CameraFile;
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
        #[arg(long, default_value_t = 9004)]
        port: u16,
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
        #[arg(long, default_value_t = 9004)]
        port: u16,
    },
    /// List the frames of a burst or interval group. Not yet verified on an Action 4.
    Burst {
        /// The group lead path as printed by `osmo ls`.
        path: String,
        #[arg(long, default_value = CAMERA_IP)]
        ip: String,
        #[arg(long, default_value_t = 9004)]
        port: u16,
    },
    /// Delete files from the camera. IRREVERSIBLE. Paths are exactly as `osmo ls` prints them (no globs). Without
    /// --yes it only prints what it would delete. Only files in the newest listing can be deleted.
    Rm {
        #[arg(required = true)]
        paths: Vec<String>,
        /// Really delete. Without this nothing is sent to the camera.
        #[arg(long)]
        yes: bool,
        #[arg(long, default_value = CAMERA_IP)]
        ip: String,
        #[arg(long, default_value_t = 9004)]
        port: u16,
    },
    /// Set the camera's clock and time zone from this computer's. NOT WORKING on an Action 4: it answers 'refused' (0xe0).
    SyncTime {
        #[arg(long, default_value = CAMERA_IP)]
        ip: String,
        #[arg(long, default_value_t = 9004)]
        port: u16,
    },
    /// Save a file's thumbnail as an image. PATH is the camera path exactly as `osmo ls` prints it.
    Thumb {
        path: String,
        /// Where to write the image.
        #[arg(short, long)]
        out: PathBuf,
        #[arg(long, default_value = CAMERA_IP)]
        ip: String,
        #[arg(long, default_value_t = 9004)]
        port: u16,
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
        #[arg(long, default_value_t = 9004)]
        port: u16,
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
        #[arg(long, default_value_t = 9004)]
        port: u16,
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

fn open_session(
    ip: &str,
    port: u16,
    verbose: bool,
) -> Result<CameraSession<osmo_core::datalink::UdpPeer>, String> {
    warn_route(ip, port);
    let cfg = SessionConfig {
        port,
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

fn list_pages(
    s: &mut CameraSession<osmo_core::datalink::UdpPeer>,
    all: bool,
) -> Result<Vec<CameraFile>, SessionError> {
    let mut files = s.list_newest()?;
    if all {
        // A page cap guards against a camera that keeps answering with files we have not seen.
        for _ in 0..500 {
            if !s.more_available() {
                break;
            }
            let next = s.list_next_page()?;
            if next.is_empty() {
                break;
            }
            files.extend(next);
        }
    }
    Ok(files)
}

#[derive(Clone, Copy)]
struct LsFilter {
    kind: Kind,
    starred: bool,
    highlights: bool,
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
    let mut s = open_session(ip, port, verbose)?;
    s.set_highlights_only(filter.highlights);
    if let Some(n) = page_size {
        s.set_page_size(n);
    }
    eprintln!("Listing {ip}:{port} ...");
    let files = list_pages(&mut s, all).map_err(session_err);
    warn_if_unconfirmed(&s);
    if let Some(w) = truncation_warning(s.diagnostics().truncated) {
        eprintln!("warning: {w}");
    }
    let status_line = format_status(s.status());
    if !status_line.is_empty() {
        eprintln!("{status_line}");
    }
    let files = files.map(|mut f| {
        let http = HttpClient::new(ip);
        fill_unknown_sizes(&mut f, &mut |x| http.head(&x.url_path()));
        f
    });
    if let Some(path) = dump {
        if has_media_records(s.last_blob()) {
            std::fs::write(path, s.last_blob())
                .map_err(|e| format!("could not write {}: {e}", path.display()))?;
            eprintln!("Raw manifest saved to {}", path.display());
        } else {
            eprintln!(
                "Not saving {}: this run captured no media records (keeping any earlier dump).",
                path.display()
            );
        }
    }
    s.close();
    println!(
        "{}",
        format_listing(&filter_files(&files?, filter.kind, filter.starred))
    );
    Ok(())
}

fn star_cmd(path: &str, on: bool, ip: &str, port: u16, verbose: bool) -> Result<(), String> {
    let path = validate_get_path(path)?;
    {
        let mut sess = open_session(ip, port, verbose)?;
        eprintln!("Looking up {path} ...");
        let found = sess.find_file(path).map_err(session_err)?;
        warn_if_unconfirmed(&sess);
        let file = found.ok_or(
            "not found anywhere in the camera's library: check the path against `osmo ls --all`",
        )?;
        eprintln!("Sending favourite {} ...", if on { "on" } else { "off" });
        sess.set_write_counter_start(config::next_write_counter());
        let result = sess.set_favourite(&file, on);
        // Advance the persisted counter whatever happened: the camera may have seen this one.
        let _ = config::save_write_counter(sess.next_write_counter());
        result.map_err(|e| e.to_string())?;
        println!(
            "The camera accepted it: {path} {}",
            if on {
                "is now starred"
            } else {
                "is no longer starred"
            }
        );
    }
    // Confirm on a fresh session: a second handshake on the same transport is not attempted.
    let mut check = open_session(ip, port, verbose)?;
    match check.find_file(path).map_err(session_err)? {
        Some(f) if f.starred == on => println!(
            "A fresh listing confirms it ({}).",
            if on { "starred" } else { "not starred" }
        ),
        Some(_) => eprintln!(
            "warning: a fresh listing does not show the change yet; run `osmo ls` to check"
        ),
        None => eprintln!("warning: could not find the file again to confirm"),
    }
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

fn rm_cmd(paths: &[String], yes: bool, ip: &str, port: u16, verbose: bool) -> Result<(), String> {
    if paths.len() > 20 {
        return Err("at most 20 files can be deleted per command".to_string());
    }
    let mut sess = open_session(ip, port, verbose)?;
    eprintln!("Listing {ip}:{port} ...");
    let files = sess.list_newest().map_err(session_err)?;
    warn_if_unconfirmed(&sess);
    let plan = match plan_deletions(&files, paths) {
        Ok(p) => p,
        Err(refusals) => {
            for r in &refusals {
                eprintln!("refused: {r}");
            }
            return Err("nothing was deleted".to_string());
        }
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
    sess.set_write_counter_start(config::next_write_counter());
    let result = sess.delete_files(&plan);
    let _ = config::save_write_counter(sess.next_write_counter());
    result.map_err(|e| e.to_string())?;
    eprintln!("The camera answered OK; checking the files are gone ...");
    // Check on a fresh session: a second handshake on the same session makes the camera answer empty.
    drop(sess);
    let after = open_session(ip, port, verbose)
        .map_err(|e| format!("{e}\nthe camera answered OK but the check could not start: run `osmo ls` to see what is left"))?
        .list_newest()
        .map_err(|e| {
            format!(
                "the camera answered OK but the check failed ({}): run `osmo ls` to see what is left",
                session_err(e)
            )
        })?;
    if after.is_empty() {
        return Err("the camera answered OK but the check returned no files at all, so it cannot be confirmed: run `osmo ls`".to_string());
    }
    let left: Vec<&str> = plan
        .iter()
        .filter(|f| after.iter().any(|a| a.path == f.path))
        .map(|f| f.path.as_str())
        .collect();
    if !left.is_empty() {
        return Err(format!(
            "the camera answered OK but these files are still listed: {}",
            left.join(", ")
        ));
    }
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
    let (file, _session) = match storage {
        Some(s) => {
            warn_route(ip, port);
            (file_from_path(s, path), None)
        }
        None => {
            let mut sess = open_session(ip, port, verbose)?;
            eprintln!("Looking up {path} ...");
            let found = sess.find_file(path).map_err(session_err)?;
            warn_if_unconfirmed(&sess);
            let f = found.ok_or("not found anywhere in the camera's library: check the path against `osmo ls --all`")?;
            (f, Some(sess))
        }
    };
    let http = HttpClient::new(ip);
    let mut tried = Vec::new();
    let mut saved = None;
    for src in thumb_sources(&file) {
        let (label, bytes) = match &src {
            ThumbSource::Whole(p) => (p.clone(), http.get_bytes(p)),
            ThumbSource::EmbeddedHead(p) => (
                format!("{p} (embedded EXIF)"),
                http.get_range(p, 0, HEAD_BYTES as u64 - 1)
                    .and_then(|head| embedded_jpeg(&head)),
            ),
        };
        match bytes.as_deref().and_then(image_kind) {
            Some(kind) => {
                saved = Some((label, kind, bytes.unwrap_or_default()));
                break;
            }
            None => tried.push(label),
        }
    }
    let (label, kind, bytes) = saved.ok_or_else(|| {
        format!(
            "no thumbnail available for {path} (tried: {})",
            tried.join(", ")
        )
    })?;
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(out, &bytes).map_err(|e| format!("could not write {}: {e}", out.display()))?;
    println!(
        "Saved {} ({}, {kind}, from {label})",
        out.display(),
        human_size(bytes.len() as u64)
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
    verbose: bool,
}

fn download_one(
    http: &HttpClient,
    out: &std::path::Path,
    storage: u8,
    path: &str,
    size: Option<u64>,
    sidecars: bool,
) -> Result<Outcome, osmo_core::http::DownloadError> {
    let dest = out.join(dest_filename(path));
    let mut last = Instant::now() - Duration::from_secs(1);
    let result = http.download_resumable(&url_path(storage, path), &dest, size, &mut |n| {
        if last.elapsed() >= Duration::from_millis(250) {
            eprint!("\r{}      ", progress_line(n, size));
            last = Instant::now();
        }
    });
    let final_len = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    eprintln!("\r{}      ", progress_line(final_len, size));

    if sidecars && result.is_ok() {
        let file = file_from_path(storage, path);
        let found = sidecars_to_fetch(&file, &mut |u| http.head(u));
        if found.is_empty() {
            println!("no sidecar");
        }
        for (sidecar, n) in found {
            let dest = out.join(dest_filename(&sidecar));
            let mut last = Instant::now() - Duration::from_secs(1);
            let r =
                http.download_resumable(&url_path(storage, &sidecar), &dest, Some(n), &mut |d| {
                    if last.elapsed() >= Duration::from_millis(250) {
                        eprint!("\r{}      ", progress_line(d, Some(n)));
                        last = Instant::now();
                    }
                });
            eprintln!();
            match r {
                Ok(_) => println!("Saved sidecar {}", dest.display()),
                Err(e) => eprintln!("sidecar {sidecar}: {}", download_error_message(&e)),
            }
        }
    }
    match &result {
        Ok(Outcome::Saved) => println!("Saved {}", dest.display()),
        Ok(Outcome::AlreadyComplete) => println!("Already complete: {}", dest.display()),
        Err(_) => {}
    }
    result
}

fn get_cmd(paths: &[String], out: &PathBuf, opts: GetOpts) -> Result<(), String> {
    let GetOpts {
        ip,
        port,
        storage,
        keepalive,
        sidecars,
        verbose,
    } = opts;
    let paths: Vec<&str> = paths
        .iter()
        .map(|p| validate_get_path(p))
        .collect::<Result<_, _>>()?;
    let http = HttpClient::new(ip);
    if verbose {
        http.set_logger(|l| eprintln!("  {l}"));
    }
    // (path, store, known size)
    let mut jobs: Vec<(String, u8, Option<u64>)> = Vec::new();
    let mut not_found = 0usize;
    let session = match storage {
        Some(s) => {
            warn_route(ip, port);
            for p in &paths {
                jobs.push((p.to_string(), s, http.head(&url_path(s, p))));
            }
            None
        }
        None => {
            let mut sess = open_session(ip, port, verbose)?;
            eprintln!("Looking up {} file(s) ...", paths.len());
            let files = if let [p] = paths[..] {
                sess.find_file(p)
                    .map_err(session_err)?
                    .into_iter()
                    .collect()
            } else {
                list_pages(&mut sess, true).map_err(session_err)?
            };
            warn_if_unconfirmed(&sess);
            let requested: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
            let (found, missing) = resolve_requested(&files, &requested);
            for m in &missing {
                eprintln!("not found anywhere in the camera's library: {m} (check it against `osmo ls --all`)");
            }
            not_found = missing.len();
            for f in found {
                jobs.push((
                    f.path.clone(),
                    f.storage,
                    known_size(f, || http.head(&f.url_path())),
                ));
            }
            Some(sess)
        }
    };
    if jobs.is_empty() {
        if let Some(mut s) = session {
            s.close();
        }
        return Err("nothing to download".to_string());
    }

    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    let mut bg = None;
    match session {
        Some(mut s) if keepalive => {
            let flag = stop.clone();
            bg = Some(std::thread::spawn(move || {
                while !flag.load(Ordering::Relaxed) {
                    s.keepalive_tick();
                }
                s
            }));
        }
        Some(mut s) => s.close(),
        None => {}
    }

    let (mut saved, mut skipped, mut failed) = (0, 0, not_found);
    let total = jobs.len();
    for (n, (path, store, size)) in jobs.iter().enumerate() {
        if total > 1 {
            eprintln!("[{}/{}] {path}", n + 1, total);
        }
        match download_one(&http, out, *store, path, *size, sidecars) {
            Ok(Outcome::Saved) => saved += 1,
            Ok(Outcome::AlreadyComplete) => skipped += 1,
            Err(e) => {
                failed += 1;
                eprintln!("failed {path}: {}", download_error_message(&e));
                if matches!(e, osmo_core::http::DownloadError::Stalled { .. }) {
                    let left = total - n - 1;
                    if left > 0 {
                        eprintln!("stopping: {left} file(s) not tried; the camera's WiFi is probably gone");
                        failed += left;
                    }
                    break;
                }
            }
        }
    }

    if let Some(h) = bg {
        stop.store(true, Ordering::Relaxed);
        if let Ok(mut s) = h.join() {
            s.close();
        }
    }
    if total > 1 || not_found > 0 {
        println!("{}", download_summary(saved, skipped, failed));
    }
    if failed > 0 {
        return Err(format!("{failed} file(s) not downloaded"));
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
    let (cams, _) = ble_btleplug::scan(rt, adapter, 6)?;
    let cam = ble_btleplug::pick(cams, wanted.as_deref())?
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
}

fn stream_cmd(options: StreamOpts) -> Result<(), String> {
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
    let (cams, _) = ble_btleplug::scan(&rt, &adapter, 6)?;
    if interrupted.load(Ordering::SeqCst) {
        return Err("interrupted before streaming".into());
    }
    let camera = ble_btleplug::pick(cams, wanted.as_deref())?
        .ok_or("no matching camera found: is it on and awake? try `osmo scan`")?;
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
        &mut |step| eprintln!("{step} ..."),
        &|| interrupted.load(Ordering::SeqCst),
    )
    .map_err(|e| e.to_string())?;
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
    let (cams, _) = ble_btleplug::scan(&rt, &adapter, 6)?;
    if interrupted.load(Ordering::SeqCst) {
        return Err("interrupted before GPS connection".into());
    }
    let camera = ble_btleplug::pick(cams, wanted.as_deref())?
        .ok_or("no matching camera found: is it on and awake? try `osmo scan`")?;
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
                port,
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
        } => get_cmd(
            &paths,
            &out,
            GetOpts {
                ip: &ip,
                port,
                storage,
                keepalive: !no_keepalive,
                sidecars,
                verbose: cli.verbose,
            },
        ),
        Cmd::Thumb {
            path,
            out,
            ip,
            port,
            storage,
        } => thumb_cmd(&path, &out, &ip, port, storage, cli.verbose),
        Cmd::SyncTime { ip, port } => sync_time_cmd(&ip, port, cli.verbose),
        Cmd::Marks { path, ip, port } => marks_cmd(&path, &ip, port, cli.verbose),
        Cmd::Burst { path, ip, port } => burst_cmd(&path, &ip, port, cli.verbose),
        Cmd::Rm {
            paths,
            yes,
            ip,
            port,
        } => rm_cmd(&paths, yes, &ip, port, cli.verbose),
        Cmd::Star {
            path,
            off,
            ip,
            port,
        } => star_cmd(&path, !off, &ip, port, cli.verbose),
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
