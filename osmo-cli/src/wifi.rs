//! macOS camera Wi-Fi workflow. The protocol core remains platform-independent.
use std::ffi::OsString;

#[cfg(any(target_os = "macos", test))]
trait Host {
    fn current_network(&mut self) -> Option<String>;
    fn wake_camera(&mut self) -> Result<String, String>;
    fn join(&mut self, ssid: &str) -> Result<(), String>;
    fn wait_camera(&mut self, ssid: &str) -> Result<(), String>;
    fn run_command(&mut self, args: &[OsString]) -> Result<(), String>;
    fn verify_restore(&mut self, ssid: &str) -> Result<(), String>;
    fn check_interrupt(&mut self) -> Result<(), String>;
}

#[cfg(any(target_os = "macos", test))]
fn with_wifi(
    host: &mut impl Host,
    return_to: Option<&str>,
    args: &[OsString],
) -> Result<(), String> {
    let original = return_to
        .map(str::to_owned)
        .or_else(|| host.current_network())
        .filter(|s| !s.is_empty() && s != "<redacted>")
        .ok_or("macOS did not report your Wi-Fi name; use --return-to or set OSMO_RETURN_TO")?;
    host.check_interrupt()?;
    let camera = host.wake_camera()?;
    host.check_interrupt()?;
    if camera.is_empty() {
        return Err("The camera did not provide a Wi-Fi name".into());
    }
    let switched = original != camera;
    let result = (|| {
        if switched {
            eprintln!("Connecting to camera Wi-Fi {camera} ...");
            host.join(&camera).map_err(|e| {
                format!("{e}; join the camera Wi-Fi once in macOS to save its password")
            })?;
        }
        host.check_interrupt()?;
        host.wait_camera(&camera)?;
        host.check_interrupt()?;
        host.run_command(args)
    })();
    let restore = if switched {
        eprintln!("Reconnecting to {original} ...");
        host.join(&original)
            .and_then(|()| host.verify_restore(&original))
    } else {
        Ok(())
    };
    match (result, restore) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(e), Ok(())) => Err(e),
        (result, Err(e)) => Err(format!(
            "{}could not reconnect to {original}: {e}; reconnect using the macOS Wi-Fi menu",
            result.err().map_or(String::new(), |e| format!("{e}; "))
        )),
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::net::Ipv4Addr;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::thread;
    use std::time::Duration;

    static INTERRUPTED: AtomicI32 = AtomicI32::new(0);

    extern "C" fn interrupt(signal: libc::c_int) {
        INTERRUPTED.store(signal, Ordering::SeqCst);
    }

    struct SignalGuard {
        int: libc::sighandler_t,
        term: libc::sighandler_t,
    }
    impl SignalGuard {
        fn install() -> Result<Self, String> {
            INTERRUPTED.store(0, Ordering::SeqCst);
            // The handler only updates an atomic; network operations run in normal Rust code.
            unsafe {
                let int = libc::signal(libc::SIGINT, interrupt as *const () as libc::sighandler_t);
                if int == libc::SIG_ERR {
                    return Err("could not install SIGINT handler".into());
                }
                let term =
                    libc::signal(libc::SIGTERM, interrupt as *const () as libc::sighandler_t);
                if term == libc::SIG_ERR {
                    libc::signal(libc::SIGINT, int);
                    return Err("could not install SIGTERM handler".into());
                }
                Ok(Self { int, term })
            }
        }
    }
    impl Drop for SignalGuard {
        fn drop(&mut self) {
            unsafe {
                libc::signal(libc::SIGINT, self.int);
                libc::signal(libc::SIGTERM, self.term);
            }
        }
    }

    struct Mac {
        device: String,
    }
    impl Mac {
        fn new() -> Result<Self, String> {
            let out = Command::new("networksetup")
                .arg("-listallhardwareports")
                .output()
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                return Err("could not list Wi-Fi interfaces".into());
            }
            let text = String::from_utf8_lossy(&out.stdout);
            let mut wifi = false;
            for line in text.lines() {
                if let Some(port) = line.strip_prefix("Hardware Port: ") {
                    wifi = port == "Wi-Fi" || port == "AirPort";
                }
                if wifi {
                    if let Some(device) = line.strip_prefix("Device: ") {
                        return Ok(Self {
                            device: device.to_owned(),
                        });
                    }
                }
            }
            Err("could not find the macOS Wi-Fi interface".into())
        }
    }

    impl Host for Mac {
        fn current_network(&mut self) -> Option<String> {
            let out = Command::new("networksetup")
                .args(["-getairportnetwork", &self.device])
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let s = String::from_utf8_lossy(&out.stdout);
            let s = s.trim_end();
            s.strip_prefix("Current Wi-Fi Network: ")
                .or_else(|| s.strip_prefix("Current AirPort Network: "))
                .filter(|s| !s.is_empty() && *s != "<redacted>")
                .map(str::to_owned)
        }

        fn wake_camera(&mut self) -> Result<String, String> {
            let out = Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
                .arg("pair")
                .stderr(Stdio::inherit())
                .output()
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                return Err("could not wake and pair with the camera".into());
            }
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .find_map(|l| l.strip_prefix("SSID: ").map(str::to_owned))
                .ok_or_else(|| "the camera did not provide its Wi-Fi name".into())
        }

        fn join(&mut self, ssid: &str) -> Result<(), String> {
            let out = Command::new("networksetup")
                .args(["-setairportnetwork", &self.device, ssid])
                .output()
                .map_err(|e| e.to_string())?;
            let message = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            let lower = message.to_lowercase();
            if !out.status.success()
                || lower.contains("failed")
                || lower.contains("error")
                || lower.contains("could not")
            {
                return Err(format!("could not join {ssid}: {}", message.trim()));
            }
            Ok(())
        }

        fn wait_camera(&mut self, ssid: &str) -> Result<(), String> {
            for _ in 0..20 {
                self.check_interrupt()?;
                let address = Command::new("ipconfig")
                    .args(["getifaddr", &self.device])
                    .output()
                    .ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .and_then(|s| s.trim().parse::<Ipv4Addr>().ok());
                if address.is_some_and(|a| a.octets()[..3] == [192, 168, 2]) {
                    if let Some(current) = self.current_network() {
                        if current != ssid {
                            return Err(format!("the Mac is connected to {current}, not {ssid}"));
                        }
                    }
                    return Ok(());
                }
                thread::sleep(Duration::from_secs(1));
            }
            Err("the camera network did not acquire a 192.168.2.x address".into())
        }

        fn run_command(&mut self, args: &[OsString]) -> Result<(), String> {
            let mut child = Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
                .args(args)
                .spawn()
                .map_err(|e| e.to_string())?;
            loop {
                if let Err(e) = self.check_interrupt() {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(e);
                }
                match child.try_wait().map_err(|e| e.to_string())? {
                    Some(status) if status.success() => return Ok(()),
                    Some(status) => return Err(format!("osmo command failed ({status})")),
                    None => thread::sleep(Duration::from_millis(100)),
                }
            }
        }

        fn verify_restore(&mut self, ssid: &str) -> Result<(), String> {
            for _ in 0..10 {
                match self.current_network() {
                    Some(actual) if actual == ssid => return Ok(()),
                    Some(_) => thread::sleep(Duration::from_secs(1)),
                    None => {
                        eprintln!("Reconnect requested; macOS did not reveal the current Wi-Fi name to verify it.");
                        return Ok(());
                    }
                }
            }
            Err("macOS did not report the requested network".into())
        }

        fn check_interrupt(&mut self) -> Result<(), String> {
            let signal = INTERRUPTED.load(Ordering::SeqCst);
            if signal == 0 {
                Ok(())
            } else {
                Err(format!("interrupted by signal {signal}"))
            }
        }
    }

    pub fn run(return_to: Option<&str>, args: &[OsString]) -> Result<(), String> {
        let _signals = SignalGuard::install()?;
        let mut host = Mac::new()?;
        super::with_wifi(&mut host, return_to, args)
    }

    #[cfg(test)]
    #[test]
    fn termination_is_recorded_for_normal_cleanup() {
        let _guard = SignalGuard::install().unwrap();
        unsafe {
            libc::raise(libc::SIGTERM);
        }
        assert_eq!(INTERRUPTED.load(Ordering::SeqCst), libc::SIGTERM);
    }
}

#[cfg(target_os = "macos")]
pub use macos::run;

#[cfg(not(target_os = "macos"))]
pub fn run(_: Option<&str>, _: &[OsString]) -> Result<(), String> {
    Err("with-wifi is currently supported only on macOS".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    struct Fake {
        current: Option<String>,
        joins: Vec<String>,
        calls: Vec<OsString>,
        command_fails: bool,
        join_fails: bool,
        restore_fails: bool,
        wait_interrupted: bool,
    }

    impl Host for Fake {
        fn current_network(&mut self) -> Option<String> {
            self.current.clone()
        }
        fn wake_camera(&mut self) -> Result<String, String> {
            Ok("Camera WiFi".into())
        }
        fn join(&mut self, name: &str) -> Result<(), String> {
            self.joins.push(name.into());
            if (name == "Camera WiFi" && self.join_fails)
                || (name == "Home WiFi" && self.restore_fails)
            {
                return Err("join failed".into());
            }
            self.current = Some(name.into());
            Ok(())
        }
        fn wait_camera(&mut self, _: &str) -> Result<(), String> {
            if self.wait_interrupted {
                Err("interrupted".into())
            } else {
                Ok(())
            }
        }
        fn run_command(&mut self, args: &[OsString]) -> Result<(), String> {
            self.calls = args.to_vec();
            if self.command_fails {
                Err("command failed".into())
            } else {
                Ok(())
            }
        }
        fn verify_restore(&mut self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn check_interrupt(&mut self) -> Result<(), String> {
            Ok(())
        }
    }

    fn fake() -> Fake {
        Fake {
            current: Some("Home WiFi".into()),
            joins: vec![],
            calls: vec![],
            command_fails: false,
            join_fails: false,
            restore_fails: false,
            wait_interrupted: false,
        }
    }

    #[test]
    fn runs_with_arguments_then_restores() {
        let mut h = fake();
        let args = [
            OsString::from("-v"),
            OsString::from("burst"),
            OsString::from("a path.JPG"),
        ];
        with_wifi(&mut h, None, &args).unwrap();
        assert_eq!(h.calls, args);
        assert_eq!(h.joins, ["Camera WiFi", "Home WiFi"]);
    }

    #[test]
    fn restores_when_command_or_join_fails() {
        let mut h = fake();
        h.command_fails = true;
        assert!(with_wifi(&mut h, None, &["ls".into()]).is_err());
        assert_eq!(h.joins, ["Camera WiFi", "Home WiFi"]);
        let mut h = fake();
        h.join_fails = true;
        assert!(with_wifi(&mut h, None, &["ls".into()]).is_err());
        assert_eq!(h.joins, ["Camera WiFi", "Home WiFi"]);
        assert!(h.calls.is_empty());
    }

    #[test]
    fn hidden_network_requires_a_return_target() {
        let mut h = fake();
        h.current = None;
        assert!(with_wifi(&mut h, None, &["ls".into()])
            .unwrap_err()
            .contains("--return-to"));
        assert!(h.joins.is_empty());
        with_wifi(&mut h, Some("Home WiFi"), &["ls".into()]).unwrap();
        assert_eq!(h.joins, ["Camera WiFi", "Home WiFi"]);
    }

    #[test]
    fn restore_failure_is_an_error() {
        let mut h = fake();
        h.restore_fails = true;
        assert!(with_wifi(&mut h, None, &["ls".into()])
            .unwrap_err()
            .contains("reconnect"));
    }

    #[test]
    fn interruption_after_switch_still_restores() {
        let mut h = fake();
        h.wait_interrupted = true;
        assert!(with_wifi(&mut h, None, &["ls".into()])
            .unwrap_err()
            .contains("interrupted"));
        assert_eq!(h.joins, ["Camera WiFi", "Home WiFi"]);
        assert!(h.calls.is_empty());
    }
}
