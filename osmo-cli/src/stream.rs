//! Local settings for an RTMP stream. Values may contain credentials and are never logged.
use std::path::Path;

pub struct StreamEnv {
    pub ssid: String,
    pub password: String,
    pub url: String,
}

impl StreamEnv {
    pub fn load(path: &Path, environment: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let mut values = std::collections::HashMap::new();
        match dotenvy::from_path_iter(path) {
            Ok(iter) => {
                for item in iter {
                    let (key, value) =
                        item.map_err(|_| "invalid .env file; check its syntax".to_string())?;
                    if matches!(
                        key.as_str(),
                        "OSMO_RETURN_TO"
                            | "OSMO_HOME_WIFI_PASSWORD"
                            | "OSMO_RTMP_SSID"
                            | "OSMO_RTMP_WIFI_PASSWORD"
                            | "OSMO_RTMP_URL"
                    ) {
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
                .filter(|s| !s.is_empty())
        };
        let (ssid, password) = if let Some(ssid) = get("OSMO_RTMP_SSID") {
            let password = get("OSMO_RTMP_WIFI_PASSWORD")
                .ok_or("set OSMO_RTMP_WIFI_PASSWORD for the selected stream network")?;
            (ssid, password)
        } else {
            let ssid =
                get("OSMO_RETURN_TO").ok_or("set OSMO_RTMP_SSID or OSMO_RETURN_TO in .env")?;
            let password = get("OSMO_RTMP_WIFI_PASSWORD")
                .or_else(|| get("OSMO_HOME_WIFI_PASSWORD"))
                .ok_or("set OSMO_RTMP_WIFI_PASSWORD or OSMO_HOME_WIFI_PASSWORD in .env")?;
            (ssid, password)
        };
        let url = get("OSMO_RTMP_URL").ok_or("set OSMO_RTMP_URL in .env")?;
        if !url.starts_with("rtmp://") || url.len() <= "rtmp://".len() {
            return Err("OSMO_RTMP_URL must start with rtmp:// and include a destination".into());
        }
        Ok(Self {
            ssid,
            password,
            url,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stream_uses_home_wifi_and_a_secret_url_from_dotenv() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join(".env");
        std::fs::write(&path, "OSMO_RETURN_TO='Home WiFi'\nOSMO_HOME_WIFI_PASSWORD='pass $ # word'\nOSMO_RTMP_URL='rtmp://example/live/key'\n").unwrap();
        let s = StreamEnv::load(&path, |_| None).unwrap();
        assert_eq!(s.ssid, "Home WiFi");
        assert_eq!(s.password, "pass $ # word");
        assert_eq!(s.url, "rtmp://example/live/key");
    }

    #[test]
    fn dedicated_stream_network_and_environment_override_home_defaults() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join(".env");
        std::fs::write(&path, "OSMO_RETURN_TO=home\nOSMO_HOME_WIFI_PASSWORD=homepass\nOSMO_RTMP_SSID=stream\nOSMO_RTMP_WIFI_PASSWORD=streampass\nOSMO_RTMP_URL='rtmp://host/key'\n").unwrap();
        let s = StreamEnv::load(&path, |key| {
            (key == "OSMO_RTMP_SSID").then(|| "shellstream".into())
        })
        .unwrap();
        assert_eq!(s.ssid, "shellstream");
        assert_eq!(s.password, "streampass");
    }

    #[test]
    fn bad_configuration_never_includes_secrets_in_errors() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join(".env");
        std::fs::write(&path, "OSMO_HOME_WIFI_PASSWORD='private secret\n").unwrap();
        let error = StreamEnv::load(&path, |_| None).err().unwrap();
        assert!(!error.contains("private secret"));
        std::fs::write(
            &path,
            "OSMO_RETURN_TO=home\nOSMO_HOME_WIFI_PASSWORD=secret\nOSMO_RTMP_URL=''\n",
        )
        .unwrap();
        assert!(StreamEnv::load(&path, |_| None)
            .err()
            .unwrap()
            .contains("OSMO_RTMP_URL"));
    }

    #[test]
    fn a_different_stream_network_requires_its_own_password() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join(".env");
        std::fs::write(&path, "OSMO_RETURN_TO=home\nOSMO_HOME_WIFI_PASSWORD=homepass\nOSMO_RTMP_SSID=other\nOSMO_RTMP_URL='rtmp://host/key'\n").unwrap();
        assert!(StreamEnv::load(&path, |_| None)
            .err()
            .unwrap()
            .contains("OSMO_RTMP_WIFI_PASSWORD"));
    }
}
