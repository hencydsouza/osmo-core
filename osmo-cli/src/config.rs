//! Per-user state (pairing identity, last camera) in the OS config dir. Never in the repo.
use directories::ProjectDirs;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub fn config_dir() -> io::Result<PathBuf> {
    if let Some(d) = std::env::var_os("OSMO_CONFIG_DIR") {
        return Ok(PathBuf::from(d));
    }
    ProjectDirs::from("dev", "osmo", "osmo")
        .map(|d| d.config_dir().to_path_buf())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))
}

/// The 32-char app identity the camera keys its remembered approval on; minted once.
pub fn load_or_create_identity_in(dir: &Path) -> io::Result<String> {
    let path = dir.join("identity");
    if let Ok(s) = fs::read_to_string(&path) {
        let s = s.trim().to_string();
        if s.len() == 32 {
            return Ok(s);
        }
    }
    fs::create_dir_all(dir)?;
    let id = osmo_core::duml::commands::new_install_identity();
    fs::write(&path, &id)?;
    Ok(id)
}

pub fn load_or_create_identity() -> io::Result<String> {
    load_or_create_identity_in(&config_dir()?)
}

pub fn save_last_device_in(dir: &Path, id: &str) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    fs::write(dir.join("last_device"), id)
}

pub fn last_device_in(dir: &Path) -> Option<String> {
    fs::read_to_string(dir.join("last_device"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn save_last_device(id: &str) -> io::Result<()> {
    save_last_device_in(&config_dir()?, id)
}

pub fn last_device() -> Option<String> {
    last_device_in(&config_dir().ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_stable_across_calls() {
        let d = tempfile::tempdir().unwrap();
        let a = load_or_create_identity_in(d.path()).unwrap();
        let b = load_or_create_identity_in(d.path()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn identity_is_32_chars_and_differs_per_install() {
        let d1 = tempfile::tempdir().unwrap();
        let d2 = tempfile::tempdir().unwrap();
        let a = load_or_create_identity_in(d1.path()).unwrap();
        assert_eq!(a.len(), 32);
        assert_ne!(a, load_or_create_identity_in(d2.path()).unwrap());
    }

    #[test]
    fn last_device_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(last_device_in(d.path()), None);
        save_last_device_in(d.path(), "abc-123").unwrap();
        assert_eq!(last_device_in(d.path()).as_deref(), Some("abc-123"));
    }
}
