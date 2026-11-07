//! Remembered cameras, stored independently so updates to different cameras do not overwrite each other.
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedCamera {
    pub id: String,
    pub name: Option<String>,
    pub model_id: Option<u16>,
}

/// The datalink port: the `--port` flag if given, else the one the saved camera's model speaks, else 9004.
pub fn datalink_port(flag: Option<u16>, saved: Option<&SavedCamera>) -> u16 {
    flag.unwrap_or_else(|| {
        saved.map_or(9004, |c| {
            osmo_core::ble::resolve_profile(c.model_id, c.name.as_deref()).datalink_port
        })
    })
}

pub struct VisibleCamera {
    pub id: String,
    pub name: Option<String>,
    pub model_id: Option<u16>,
}

/// Format saved cameras and, when a scan was requested, newly discovered cameras.
pub fn format_listing(
    saved: &[SavedCamera],
    visible: Option<&[VisibleCamera]>,
    last: Option<&str>,
) -> String {
    let mut lines = Vec::new();
    for camera in saved {
        let found =
            visible.and_then(|list| list.iter().find(|c| c.id.eq_ignore_ascii_case(&camera.id)));
        let name = found
            .and_then(|c| c.name.as_deref())
            .or(camera.name.as_deref())
            .unwrap_or("Unknown camera");
        let model = found
            .and_then(|c| c.model_id)
            .or(camera.model_id)
            .and_then(osmo_core::ble::model_name)
            .unwrap_or("unknown");
        let status = match (visible, found) {
            (Some(_), Some(_)) => "  nearby",
            (Some(_), None) => "  not seen",
            (None, _) => "",
        };
        let last = if last.is_some_and(|id| id.eq_ignore_ascii_case(&camera.id)) {
            "  last"
        } else {
            ""
        };
        lines.push(format!(
            "{name}  id={}  model={model}{status}{last}",
            camera.id
        ));
    }
    if let Some(visible) = visible {
        for camera in visible
            .iter()
            .filter(|c| !saved.iter().any(|s| s.id.eq_ignore_ascii_case(&c.id)))
        {
            let name = camera.name.as_deref().unwrap_or("Unknown camera");
            let model = camera
                .model_id
                .and_then(osmo_core::ble::model_name)
                .unwrap_or("unknown");
            lines.push(format!(
                "{name}  id={}  model={model}  nearby  new",
                camera.id
            ));
        }
    }
    if lines.is_empty() {
        "No cameras found or saved.".into()
    } else {
        lines.join("\n")
    }
}

fn filename(id: &str) -> io::Result<String> {
    if id.is_empty() || id.len() > 120 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid camera identifier",
        ));
    }
    let encoded: String = id
        .to_ascii_lowercase()
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(format!("{encoded}.json"))
}

fn directory(dir: &Path) -> PathBuf {
    dir.join("cameras")
}

pub fn remember_in(dir: &Path, camera: &SavedCamera) -> io::Result<()> {
    use std::io::Write;
    let name = filename(&camera.id)?;
    let directory = directory(dir);
    fs::create_dir_all(&directory)?;
    let temporary = directory.join(format!(
        ".{}.tmp",
        osmo_core::duml::commands::new_install_identity()
    ));
    let bytes = serde_json::to_vec(camera)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid camera data"))?;
    let result = (|| {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        fs::rename(&temporary, directory.join(name))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn load_in(dir: &Path) -> io::Result<Vec<SavedCamera>> {
    let mut cameras = Vec::new();
    match fs::read_dir(directory(dir)) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                if !name.ends_with(".json") {
                    continue;
                }
                let bytes = fs::read(entry.path())?;
                let camera: SavedCamera = serde_json::from_slice(&bytes).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid saved camera data")
                })?;
                if filename(&camera.id)? != name {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "saved camera identifier does not match its file",
                    ));
                }
                cameras.push(camera);
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    if let Some(last) = crate::config::last_device_in(dir) {
        if !cameras.iter().any(|c| c.id.eq_ignore_ascii_case(&last)) {
            cameras.push(SavedCamera {
                id: last,
                name: None,
                model_id: None,
            });
        }
    }
    cameras.sort_by(|a, b| {
        a.name
            .as_deref()
            .unwrap_or(&a.id)
            .to_lowercase()
            .cmp(&b.name.as_deref().unwrap_or(&b.id).to_lowercase())
    });
    Ok(cameras)
}

pub fn remember(camera: &SavedCamera) -> io::Result<()> {
    remember_in(&crate::config::config_dir()?, camera)
}
pub fn load() -> io::Result<Vec<SavedCamera>> {
    load_in(&crate::config::config_dir()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera(id: &str, name: &str) -> SavedCamera {
        SavedCamera {
            id: id.into(),
            name: Some(name.into()),
            model_id: Some(0x14),
        }
    }

    #[test]
    fn remembers_multiple_cameras_and_updates_existing_details() {
        let d = tempfile::tempdir().unwrap();
        remember_in(d.path(), &camera("id-1", "One")).unwrap();
        remember_in(d.path(), &camera("id-2", "Two")).unwrap();
        remember_in(d.path(), &camera("id-1", "Renamed")).unwrap();
        let saved = load_in(d.path()).unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(
            saved
                .iter()
                .find(|c| c.id == "id-1")
                .unwrap()
                .name
                .as_deref(),
            Some("Renamed")
        );
    }

    #[test]
    fn legacy_last_camera_is_visible_without_overwriting_existing_cameras() {
        let d = tempfile::tempdir().unwrap();
        crate::config::save_last_device_in(d.path(), "legacy-id").unwrap();
        remember_in(d.path(), &camera("other", "Other")).unwrap();
        let saved = load_in(d.path()).unwrap();
        assert!(saved.iter().any(|c| c.id == "legacy-id"));
        assert!(saved.iter().any(|c| c.id == "other"));
    }

    #[test]
    fn camera_identifiers_cannot_escape_the_config_directory() {
        let d = tempfile::tempdir().unwrap();
        remember_in(d.path(), &camera("../../camera", "Name")).unwrap();
        assert_eq!(load_in(d.path()).unwrap()[0].id, "../../camera");
        assert_eq!(fs::read_dir(d.path().join("cameras")).unwrap().count(), 1);
    }

    #[test]
    fn malformed_saved_data_is_reported_instead_of_silently_discarded() {
        let d = tempfile::tempdir().unwrap();
        remember_in(d.path(), &camera("id-1", "One")).unwrap();
        let path = std::fs::read_dir(d.path().join("cameras"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::write(path, "not JSON").unwrap();
        assert!(load_in(d.path()).is_err());
    }

    #[test]
    fn listing_distinguishes_seen_unseen_and_new_cameras() {
        let saved = vec![camera("id-1", "One"), camera("id-2", "Two")];
        let visible = vec![
            VisibleCamera {
                id: "id-1".into(),
                name: Some("One".into()),
                model_id: Some(0x14),
            },
            VisibleCamera {
                id: "id-3".into(),
                name: Some("Three".into()),
                model_id: None,
            },
        ];
        let out = format_listing(&saved, Some(&visible), Some("id-1"));
        assert!(out
            .lines()
            .any(|line| line.contains("id-1") && line.contains("nearby") && line.contains("last")));
        assert!(out
            .lines()
            .any(|line| line.contains("id-2") && line.contains("not seen")));
        assert!(out
            .lines()
            .any(|line| line.contains("id-3") && line.contains("new")));
        let offline = format_listing(&saved, None, None);
        assert!(!offline.contains("not seen"));
    }

    #[test]
    fn datalink_port_prefers_the_flag_then_the_saved_cameras_model_then_9004() {
        let xtra = SavedCamera {
            id: "x".into(),
            name: Some("XtraEdgePro-6E55".into()),
            model_id: Some(0x0015),
        };
        let dji = SavedCamera {
            id: "d".into(),
            name: Some("OsmoAction4-1".into()),
            model_id: Some(0x0014),
        };
        assert_eq!(datalink_port(Some(9999), Some(&xtra)), 9999);
        assert_eq!(datalink_port(None, Some(&xtra)), 10004);
        assert_eq!(datalink_port(None, Some(&dji)), 9004);
        assert_eq!(datalink_port(None, None), 9004);
    }
}
