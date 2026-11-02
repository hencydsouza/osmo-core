mod ble_btleplug;

use clap::{Parser, Subcommand};
use osmo_cli::listing::{
    dest_filename, download_error_message, file_from_path, fill_unknown_sizes, format_listing,
    format_status, human_size, known_size, progress_line, thumb_sources, truncation_warning,
    validate_get_path, ThumbSource,
};
use osmo_cli::{config, route::route_warning};
use osmo_core::ble::{model_name, pair_and_read_wifi, PairOptions};
use osmo_core::datalink::{has_media_records, CameraSession, SessionConfig, SessionError};
use osmo_core::embedded_jpeg::{embedded_jpeg, image_kind, HEAD_BYTES};
use osmo_core::http::{HttpClient, Outcome};
use osmo_core::media::url_path;
use osmo_core::media::CameraFile;
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

#[derive(Subcommand)]
enum Cmd {
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
    /// Download one file, resuming a partial one. PATH is the camera path exactly as `osmo ls` prints it.
    Get {
        path: String,
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

fn ls_cmd(
    ip: &str,
    port: u16,
    dump: Option<&PathBuf>,
    all: bool,
    page_size: Option<u8>,
    verbose: bool,
) -> Result<(), String> {
    let mut s = open_session(ip, port, verbose)?;
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
    println!("{}", format_listing(&files?));
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

fn get_cmd(
    path: &str,
    out: &PathBuf,
    ip: &str,
    port: u16,
    storage: Option<u8>,
    keepalive: bool,
    verbose: bool,
) -> Result<(), String> {
    let path = validate_get_path(path)?;
    let http = HttpClient::new(ip);
    if verbose {
        http.set_logger(|l| eprintln!("  {l}"));
    }
    let (storage, size, session) = match storage {
        Some(s) => {
            warn_route(ip, port);
            (s, http.head(&url_path(s, path)), None)
        }
        None => {
            let mut sess = open_session(ip, port, verbose)?;
            eprintln!("Looking up {path} ...");
            let found = sess.find_file(path).map_err(session_err)?;
            warn_if_unconfirmed(&sess);
            let f = found.ok_or("not found anywhere in the camera's library: check the path against `osmo ls --all`")?;
            (
                f.storage,
                known_size(&f, || http.head(&f.url_path())),
                Some(sess),
            )
        }
    };

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

    if let Some(h) = bg {
        stop.store(true, Ordering::Relaxed);
        if let Ok(mut s) = h.join() {
            s.close();
        }
    }
    match result.map_err(|e| download_error_message(&e))? {
        Outcome::Saved => println!("Saved {}", dest.display()),
        Outcome::AlreadyComplete => println!("Already complete: {}", dest.display()),
    }
    Ok(())
}

fn pair_cmd(
    rt: &Arc<Runtime>,
    adapter: &btleplug::platform::Adapter,
    device: Option<String>,
    show: bool,
) -> Result<(), String> {
    let wanted = device.or_else(config::last_device);
    let (cams, _) = ble_btleplug::scan(rt, adapter, 6)?;
    let cam = ble_btleplug::pick(cams, wanted.as_deref())
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
    let _ = config::save_last_device(&cam.id);
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

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Ls {
            ip,
            port,
            dump,
            all,
            page_size,
        } => ls_cmd(&ip, port, dump.as_ref(), all, page_size, cli.verbose),
        Cmd::Get {
            path,
            out,
            ip,
            port,
            storage,
            no_keepalive,
        } => get_cmd(&path, &out, &ip, port, storage, !no_keepalive, cli.verbose),
        Cmd::Thumb {
            path,
            out,
            ip,
            port,
            storage,
        } => thumb_cmd(&path, &out, &ip, port, storage, cli.verbose),
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
                Cmd::Ls { .. } | Cmd::Get { .. } | Cmd::Thumb { .. } => {
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
