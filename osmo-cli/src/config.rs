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

/// Whole seconds since 2026-01-01, never 0: a counter seed that no earlier run can have reached from below.
pub fn clock_seed(unix_secs: u64) -> u32 {
    unix_secs
        .saturating_sub(1_767_225_600)
        .clamp(1, u32::MAX as u64) as u32
}

/// The counter for the next write: the persisted one, or the clock seed if that is later. The camera ignores a
/// repeated counter, so it must never go backwards or repeat across runs.
pub fn next_write_counter_in(dir: &Path, clock_seed: u32) -> u32 {
    let persisted = fs::read_to_string(dir.join("write_counter"))
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);
    persisted.max(clock_seed)
}

pub fn save_write_counter_in(dir: &Path, n: u32) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    fs::write(dir.join("write_counter"), n.to_string())
}

pub fn next_write_counter() -> u32 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match config_dir() {
        Ok(d) => next_write_counter_in(&d, clock_seed(now)),
        Err(_) => clock_seed(now),
    }
}

pub fn save_write_counter(n: u32) -> io::Result<()> {
    save_write_counter_in(&config_dir()?, n)
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
    fn write_counter_never_repeats_an_earlier_value_and_follows_the_clock_forward() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            next_write_counter_in(d.path(), 1_000_000),
            1_000_000,
            "first run: the clock seed"
        );
        save_write_counter_in(d.path(), 1_000_003).unwrap();
        assert_eq!(
            next_write_counter_in(d.path(), 1_000_000),
            1_000_003,
            "a rerun in the same second continues"
        );
        assert_eq!(
            next_write_counter_in(d.path(), 2_000_000),
            2_000_000,
            "a later run follows the clock"
        );
    }

    #[test]
    fn clock_seed_counts_seconds_since_2026_and_is_never_zero() {
        assert_eq!(clock_seed(1_767_225_600 + 5), 5);
        assert_eq!(clock_seed(1_767_225_600), 1, "never zero");
        assert_eq!(
            clock_seed(1_000),
            1,
            "a clock set before 2026 still gives a valid counter"
        );
    }

    #[test]
    fn last_device_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(last_device_in(d.path()), None);
        save_last_device_in(d.path(), "abc-123").unwrap();
        assert_eq!(last_device_in(d.path()).as_deref(), Some("abc-123"));
    }
}
