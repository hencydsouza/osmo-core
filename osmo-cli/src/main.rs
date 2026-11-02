mod ble_btleplug;

use clap::{Parser, Subcommand};
use osmo_cli::config;
use osmo_core::ble::{model_name, pair_and_read_wifi, PairOptions};
use std::process::ExitCode;
use std::sync::Arc;
use tokio::runtime::Runtime;

#[derive(Parser)]
#[command(name = "osmo", about = "Talk to DJI Osmo cameras")]
struct Cli {
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
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let rt = Arc::new(Runtime::new().map_err(|e| e.to_string())?);
    let adapter = ble_btleplug::adapter(&rt)?;
    match cli.cmd {
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
    }
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

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
