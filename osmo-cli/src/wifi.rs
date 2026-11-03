//! macOS camera Wi-Fi workflow. The protocol core remains platform-independent.
use osmo_core::ble::WifiCreds;
use std::ffi::OsString;

#[cfg(any(target_os = "macos", test))]
struct WifiSettings {
    return_to: Option<String>,
    home_password: Option<String>,
}

#[cfg(any(target_os = "macos", test))]
impl WifiSettings {
    fn from_path(
        path: &std::path::Path,
        environment: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, String> {
        let mut values = std::collections::HashMap::new();
        match dotenvy::from_path_iter(path) {
            Ok(iter) => {
                for entry in iter {
                    let (key, value) =
                        entry.map_err(|_| "invalid .env file; check its syntax".to_string())?;
                    if matches!(key.as_str(), "OSMO_RETURN_TO" | "OSMO_HOME_WIFI_PASSWORD") {
                        values.insert(key, value);
                    }
                }
            }
            Err(dotenvy::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("could not read .env file".into()),
        }
        let get = |key: &str| {
            environment(key)
                .or_else(|| values.get(key).cloned())
                .filter(|value| !value.is_empty())
        };
        Ok(Self {
            return_to: get("OSMO_RETURN_TO"),
            home_password: get("OSMO_HOME_WIFI_PASSWORD"),
        })
    }

    fn passwords_for(
        &self,
        original: &str,
        camera: &WifiCreds,
    ) -> Result<std::collections::HashMap<String, String>, String> {
        let home = self
            .home_password
            .clone()
            .ok_or("set OSMO_HOME_WIFI_PASSWORD in .env before switching")?;
        let camera_password = camera
            .password
            .as_ref()
            .filter(|p| !p.is_empty())
            .ok_or("the camera did not return a Wi-Fi password over Bluetooth")?
            .clone();
        Ok(std::collections::HashMap::from([
            (original.to_owned(), home),
            (camera.ssid.clone(), camera_password),
        ]))
    }
}

#[cfg(any(target_os = "macos", test))]
trait Host {
    fn current_network(&mut self) -> Option<String>;
    fn wake_camera(&mut self) -> Result<WifiCreds, String>;
    fn prepare_credentials(&mut self, original: &str, camera: &WifiCreds) -> Result<(), String>;
    fn join(&mut self, ssid: &str) -> Result<(), String>;
    fn wait_camera(&mut self, ssid: &str) -> Result<(), String>;
    fn run_command(&mut self, args: &[OsString]) -> Result<(), String>;
    fn verify_restore(&mut self, ssid: &str) -> Result<(), String>;
    fn check_interrupt(&mut self) -> Result<(), String>;
    fn wait_retry(&mut self);
}

#[cfg(any(target_os = "macos", test))]
fn join_with_retry(
    host: &mut impl Host,
    ssid: &str,
    attempts: usize,
    interruptible: bool,
) -> Result<(), String> {
    for attempt in 1..=attempts {
        if interruptible {
            host.check_interrupt()?;
        }
        match host.join(ssid) {
            Ok(()) => return Ok(()),
            Err(e) if attempt == attempts => return Err(e),
            Err(e) => {
                eprintln!("Wi-Fi join attempt {attempt}/{attempts} failed: {e}; retrying ...");
                host.wait_retry();
            }
        }
    }
    Err("no Wi-Fi attempts configured".into())
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
    if camera.ssid.is_empty() {
        return Err("The camera did not provide a Wi-Fi name".into());
    }
    let switched = original != camera.ssid;
    if switched {
        host.prepare_credentials(&original, &camera)?;
        host.check_interrupt()?;
    }
    let mut joined_camera = false;
    let result = (|| {
        if switched {
            eprintln!("Connecting to camera Wi-Fi {} ...", camera.ssid);
            join_with_retry(host, &camera.ssid, 6, true)
                .map_err(|e| format!("{e}; check that the camera hotspot is awake"))?;
            joined_camera = true;
        }
        host.check_interrupt()?;
        host.wait_camera(&camera.ssid)?;
        host.check_interrupt()?;
        host.run_command(args)
    })();
    let still_original =
        switched && !joined_camera && host.current_network().as_deref() == Some(original.as_str());
    let restore = if switched && !still_original {
        eprintln!("Reconnecting to {original} ...");
        join_with_retry(host, &original, 4, false).and_then(|()| host.verify_restore(&original))
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
    use std::collections::HashMap;
    use std::net::Ipv4Addr;
    use std::process::Command;
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
        settings: WifiSettings,
        passwords: HashMap<String, String>,
        wake: fn() -> Result<WifiCreds, String>,
    }
    impl Mac {
        fn new(
            settings: WifiSettings,
            wake: fn() -> Result<WifiCreds, String>,
        ) -> Result<Self, String> {
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
                            settings,
                            passwords: HashMap::new(),
                            wake,
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

        fn wake_camera(&mut self) -> Result<WifiCreds, String> {
            (self.wake)()
        }

        fn prepare_credentials(
            &mut self,
            original: &str,
            camera: &WifiCreds,
        ) -> Result<(), String> {
            self.passwords = self.settings.passwords_for(original, camera)?;
            Ok(())
        }

        fn join(&mut self, ssid: &str) -> Result<(), String> {
            let password = self
                .passwords
                .get(ssid)
                .ok_or_else(|| format!("no configured password for {ssid}"))?;
            let out = Command::new("networksetup")
                .args(["-setairportnetwork", &self.device, ssid, password])
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
                return Err(format!(
                    "could not join {ssid}: {}",
                    message.trim().replace(password, "[redacted]")
                ));
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

        fn wait_retry(&mut self) {
            thread::sleep(Duration::from_secs(2));
        }
    }

    pub fn run(
        return_to: Option<&str>,
        args: &[OsString],
        wake: fn() -> Result<WifiCreds, String>,
    ) -> Result<(), String> {
        let _signals = SignalGuard::install()?;
        let settings =
            WifiSettings::from_path(std::path::Path::new(".env"), |key| std::env::var(key).ok())?;
        let target = return_to
            .map(str::to_owned)
            .or_else(|| settings.return_to.clone());
        let mut host = Mac::new(settings, wake)?;
        super::with_wifi(&mut host, target.as_deref(), args)
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
pub fn run(
    _: Option<&str>,
    _: &[OsString],
    _: fn() -> Result<WifiCreds, String>,
) -> Result<(), String> {
    Err("with-wifi is currently supported only on macOS".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn loads_wifi_settings_from_dotenv_without_losing_special_characters() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".env");
        std::fs::write(
            &path,
            "OSMO_RETURN_TO='Home WiFi'\nOSMO_HOME_WIFI_PASSWORD='home $ # pass'\n",
        )
        .unwrap();
        let settings = WifiSettings::from_path(&path, |_| None).unwrap();
        assert_eq!(settings.return_to.as_deref(), Some("Home WiFi"));
        assert_eq!(settings.home_password.as_deref(), Some("home $ # pass"));
    }

    #[test]
    fn environment_overrides_dotenv_without_printing_malformed_secrets() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".env");
        std::fs::write(&path, "OSMO_RETURN_TO='File WiFi'\n").unwrap();
        let settings = WifiSettings::from_path(&path, |key| {
            (key == "OSMO_RETURN_TO").then(|| "Shell WiFi".into())
        })
        .unwrap();
        assert_eq!(settings.return_to.as_deref(), Some("Shell WiFi"));
        std::fs::write(&path, "OSMO_HOME_WIFI_PASSWORD='private secret\n").unwrap();
        let error = WifiSettings::from_path(&path, |_| None).err().unwrap();
        assert!(!error.contains("private secret"));
    }

    #[test]
    fn associates_each_configured_password_with_the_correct_network() {
        let settings = WifiSettings {
            return_to: Some("Home WiFi".into()),
            home_password: Some("home secret".into()),
        };
        let camera = WifiCreds {
            ssid: "Camera WiFi".into(),
            password: Some("camera secret".into()),
            mac: None,
        };
        let passwords = settings.passwords_for("Home WiFi", &camera).unwrap();
        assert_eq!(passwords.get("Home WiFi").unwrap(), "home secret");
        assert_eq!(passwords.get("Camera WiFi").unwrap(), "camera secret");
    }

    #[test]
    fn missing_password_is_reported_without_including_the_other_password() {
        let settings = WifiSettings {
            return_to: None,
            home_password: Some("private home password".into()),
        };
        let camera = WifiCreds {
            ssid: "Camera".into(),
            password: None,
            mac: None,
        };
        let error = settings.passwords_for("Home", &camera).unwrap_err();
        assert!(error.contains("camera did not return"));
        assert!(!error.contains("private home password"));
    }

    struct Fake {
        current: Option<String>,
        joins: Vec<String>,
        calls: Vec<OsString>,
        command_fails: bool,
        join_fails: bool,
        restore_fails: bool,
        wait_interrupted: bool,
        camera_failures: usize,
        home_failures: usize,
        retry_waits: usize,
        credentials_missing: bool,
        camera_password: Option<String>,
        prepared: std::collections::HashMap<String, String>,
        interrupt_on_retry: bool,
        disconnect_on_failure: bool,
    }

    impl Host for Fake {
        fn current_network(&mut self) -> Option<String> {
            self.current.clone()
        }
        fn wake_camera(&mut self) -> Result<WifiCreds, String> {
            Ok(WifiCreds {
                ssid: "Camera WiFi".into(),
                password: self.camera_password.clone(),
                mac: None,
            })
        }
        fn prepare_credentials(
            &mut self,
            original: &str,
            camera: &WifiCreds,
        ) -> Result<(), String> {
            let settings = WifiSettings {
                return_to: None,
                home_password: (!self.credentials_missing).then(|| "home secret".into()),
            };
            self.prepared = settings.passwords_for(original, camera)?;
            Ok(())
        }
        fn join(&mut self, name: &str) -> Result<(), String> {
            self.joins.push(name.into());
            if name == "Camera WiFi" && self.camera_failures > 0 {
                self.camera_failures -= 1;
                if self.disconnect_on_failure {
                    self.current = None;
                }
                return Err("Could not find network".into());
            }
            if name == "Home WiFi" && self.home_failures > 0 {
                self.home_failures -= 1;
                return Err("temporary join failure".into());
            }
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
            if self.interrupt_on_retry && self.retry_waits > 0 {
                Err("interrupted".into())
            } else {
                Ok(())
            }
        }
        fn wait_retry(&mut self) {
            self.retry_waits += 1;
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
            camera_failures: 0,
            home_failures: 0,
            retry_waits: 0,
            credentials_missing: false,
            camera_password: Some("camera secret".into()),
            prepared: Default::default(),
            interrupt_on_retry: false,
            disconnect_on_failure: false,
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
        assert_eq!(h.prepared.get("Camera WiFi").unwrap(), "camera secret");
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
        assert_eq!(h.joins, ["Camera WiFi"; 6]);
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

    #[test]
    fn retries_camera_hotspot_and_original_network_before_giving_up() {
        let mut h = fake();
        h.camera_failures = 2;
        h.home_failures = 2;
        with_wifi(&mut h, None, &["ls".into()]).unwrap();
        assert_eq!(
            h.joins,
            [
                "Camera WiFi",
                "Camera WiFi",
                "Camera WiFi",
                "Home WiFi",
                "Home WiFi",
                "Home WiFi"
            ]
        );
        assert_eq!(h.retry_waits, 4);
    }

    #[test]
    fn refuses_to_switch_without_a_home_password() {
        let mut h = fake();
        h.credentials_missing = true;
        assert!(with_wifi(&mut h, None, &["ls".into()])
            .unwrap_err()
            .contains("OSMO_HOME_WIFI_PASSWORD"));
        assert!(h.joins.is_empty());
    }

    #[test]
    fn refuses_to_switch_if_camera_does_not_provide_a_password() {
        let mut h = fake();
        h.camera_password = None;
        assert!(with_wifi(&mut h, None, &["ls".into()])
            .unwrap_err()
            .contains("camera did not return"));
        assert!(h.joins.is_empty());
    }

    #[test]
    fn interrupt_during_retry_stops_camera_attempts_and_restores() {
        let mut h = fake();
        h.camera_failures = 3;
        h.interrupt_on_retry = true;
        h.disconnect_on_failure = true;
        assert!(with_wifi(&mut h, None, &["ls".into()])
            .unwrap_err()
            .contains("interrupted"));
        assert_eq!(h.joins, ["Camera WiFi", "Home WiFi"]);
        assert!(h.calls.is_empty());
    }
}
