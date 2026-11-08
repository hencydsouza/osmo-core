//! `osmo trim` cuts in-process: with no ffmpeg anywhere on PATH it must get as far as talking to the camera.
use std::process::Command;

#[test]
fn trim_command_uses_pure_rust_and_needs_no_ffmpeg() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_osmo"))
        .env("PATH", "")
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path())
        .args([
            "trim",
            "DCIM/DJI_001/DJI_20260904142344_0055_D.MP4",
            "--from",
            "1",
            "--to",
            "3",
            "--ip",
            "127.0.0.1",
            "--port",
            "9",
            "--out",
        ])
        .arg(dir.path())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(!err.to_lowercase().contains("ffmpeg"), "{err}");
    assert!(err.contains("no response from"), "{err}");
}
