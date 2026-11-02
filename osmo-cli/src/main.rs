mod ble_btleplug;

use clap::{Parser, Subcommand};
use osmo_cli::listing::{
    dest_filename, fill_unknown_sizes, find_file, format_listing, known_size, progress_line,
    validate_get_path,
};
use osmo_cli::{config, route::route_warning};
use osmo_core::ble::{model_name, pair_and_read_wifi, PairOptions};
use osmo_core::datalink::{has_media_records, CameraSession, SessionConfig, SessionError};
use osmo_core::http::{HttpClient, Outcome};
use osmo_core::media::url_path;
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
        /// Also save the raw manifest bytes here (file names and sizes only), to reproduce a decode problem offline.
        #[arg(long)]
        dump: Option<PathBuf>,
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
        /// Skip the listing and fetch by path from this store (0 = SD card, 1 = internal).
        #[arg(long)]
        storage: Option<u8>,
        /// Keep the datalink alive while downloading, in case the camera drops its WiFi when idle.
        #[arg(long)]
        keepalive: bool,
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

fn ls_cmd(ip: &str, port: u16, dump: Option<&PathBuf>, verbose: bool) -> Result<(), String> {
    let mut s = open_session(ip, port, verbose)?;
    eprintln!("Listing {ip}:{port} ...");
    let files = s.list_newest().map_err(session_err);
    warn_if_unconfirmed(&s);
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
    let (storage, size, session) = match storage {
        Some(s) => {
            warn_route(ip, port);
            (s, http.head(&url_path(s, path)), None)
        }
        None => {
            let mut sess = open_session(ip, port, verbose)?;
            eprintln!("Looking up {path} ...");
            let files = sess.list_newest().map_err(session_err)?;
            warn_if_unconfirmed(&sess);
            let f = find_file(&files, path)
                .ok_or("not in the camera's newest listing: pass --storage N to fetch by path")?;
            (
                f.storage,
                known_size(f, || http.head(&f.url_path())),
                Some(sess),
            )
        }
    };

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

    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
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
    match result.map_err(|e| e.to_string())? {
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
        Cmd::Ls { ip, port, dump } => ls_cmd(&ip, port, dump.as_ref(), cli.verbose),
        Cmd::Get {
            path,
            out,
            ip,
            port,
            storage,
            keepalive,
        } => get_cmd(&path, &out, &ip, port, storage, keepalive, cli.verbose),
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
                Cmd::Ls { .. } | Cmd::Get { .. } => unreachable!("handled above"),
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
