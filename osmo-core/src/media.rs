/// One media item as listed by the camera's manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraFile {
    /// e.g. `DCIM/DJI_001/DJI_20260903165023_0012_D.JPG`
    pub path: String,
    /// e.g. `MISC/THM/DJI_001/DJI_20260903165023_0012_D.scr`
    pub thumb_path: String,
    /// Last path component, e.g. `DJI_20260903165023_0012_D.JPG`.
    pub name: String,
    /// Upper-case extension (`JPG`, `MP4`, ...), empty if the record carried none.
    pub ext: String,
    /// Camera mount serving this path: 0 = SD card, 1 = internal storage.
    pub storage: u8,
    /// Full size in bytes from the manifest; 0 = unknown.
    pub size_bytes: u64,
    /// Camera-assigned handle used to address the file in delete and paging; 0 = unknown. Videos carry it at
    /// their marker; a still gets one only when the per-list fit vouches for the record's own candidate.
    pub handle: u32,
    /// Which of the manifest's concatenated per-store lists the record came from (0 first, 1 second).
    pub group: u8,
    /// The 4-digit shot number in the name (`..._0014_D.MP4` is 14); 0 if the name has none.
    pub seq: u32,
    /// `base + seq * step`, fitted over the list's own confirmed handles; 0 if the list could not be fitted.
    /// Favourite uses this; delete never does.
    pub cmd_handle: u32,
    /// The value at the record's fixed handle position, before anything has vouched for it.
    pub handle_candidate: u32,
    /// Another record in the same list holds this file's handle, so delete could hit the wrong file.
    pub handle_shared: bool,
    /// The camera's favourite flag.
    pub starred: bool,
}

impl CameraFile {
    /// A video (as opposed to a still), by its stored upper-case extension.
    pub fn is_video(&self) -> bool {
        matches!(self.ext.as_str(), "MP4" | "MOV" | "OSV" | "INSV")
    }

    /// A delete command can name this file safely: it has a verified handle that nobody else holds.
    pub fn deletable(&self) -> bool {
        self.handle != 0 && !self.handle_shared
    }

    /// Path of the original on the camera's HTTP file API. The path is deliberately not percent-encoded.
    pub fn url_path(&self) -> String {
        url_path(self.storage, &self.path)
    }
}

/// The thumbnail (screennail) path the camera keeps for a media path: `DCIM/<dir>/<base>.<ext>` is
/// `MISC/THM/<dir>/<base>.scr`.
pub fn derive_thumb_path(path: &str) -> String {
    let p = path.replacen("DCIM/", "MISC/THM/", 1);
    let (dir, name) = p.rsplit_once('/').unwrap_or(("", &p));
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    if dir.is_empty() {
        format!("{stem}.scr")
    } else {
        format!("{dir}/{stem}.scr")
    }
}

/// `/v2?storage=N&path=P`, the HTTP path of a file given only its store and camera path.
pub fn url_path(storage: u8, path: &str) -> String {
    format!("/v2?storage={storage}&path={path}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, ext: &str) -> CameraFile {
        CameraFile {
            path: path.to_string(),
            thumb_path: String::new(),
            name: path.rsplit('/').next().unwrap().to_string(),
            ext: ext.to_string(),
            storage: 0,
            size_bytes: 0,
            handle: 0,
            group: 0,
            seq: 0,
            cmd_handle: 0,
            handle_candidate: 0,
            handle_shared: false,
            starred: false,
        }
    }

    #[test]
    fn is_video_covers_the_stored_upper_case_video_extensions() {
        for e in ["MP4", "MOV", "OSV", "INSV"] {
            assert!(file("DCIM/a/x", e).is_video(), "{e}");
        }
        for e in ["JPG", "DNG", "HEIC", ""] {
            assert!(!file("DCIM/a/x", e).is_video(), "{e}");
        }
    }

    #[test]
    fn derive_thumb_path_swaps_the_dcim_root_and_the_extension() {
        assert_eq!(
            derive_thumb_path("DCIM/DJI_001/DJI_20260903165023_0012_D.JPG"),
            "MISC/THM/DJI_001/DJI_20260903165023_0012_D.scr"
        );
        assert_eq!(
            derive_thumb_path("DCIM/DJI_001/NOEXT"),
            "MISC/THM/DJI_001/NOEXT.scr"
        );
    }
}
