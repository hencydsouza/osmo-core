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
    /// Shared filename stem for numbered burst/interval frames, if present.
    pub fn group_key(&self) -> Option<&str> {
        let (stem, ext) = self.name.rsplit_once('.')?;
        if ext.is_empty() || !ext.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return None;
        }
        let (base, index) = stem.rsplit_once('_')?;
        (!base.is_empty() && index.len() == 3 && index.bytes().all(|b| b.is_ascii_digit()))
            .then_some(base)
    }

    /// Three-digit frame index, or `None` for an ordinary file.
    pub fn sub_index(&self) -> Option<u16> {
        self.group_key()?;
        self.name
            .rsplit_once('.')?
            .0
            .rsplit_once('_')?
            .1
            .parse()
            .ok()
    }

    /// Unlisted files the camera may keep beside this one, same path with the extension swapped and on the
    /// same store: a `.DNG` beside a still (JPEG+RAW mode) and a `.WAV` beside a clip (audio backup). These are
    /// candidates only; the manifest never lists them, so ask the camera.
    pub fn sidecar_paths(&self) -> Vec<String> {
        let ext = if self.is_video() {
            "WAV"
        } else if matches!(self.ext.as_str(), "JPG" | "JPEG" | "HEIC") {
            "DNG"
        } else {
            return Vec::new();
        };
        let name_start = self.path.rfind('/').map_or(0, |i| i + 1);
        match self.path[name_start..].rfind('.') {
            Some(dot) => vec![format!("{}.{ext}", &self.path[..name_start + dot])],
            None => Vec::new(),
        }
    }

    /// The low-res proxy clip the camera keeps beside a video, derived from the file-name family as Osmosis does:
    /// `DJI_` clips have an `.LRF`, `CAM_` clips an unlisted `.XRF`. `None` for stills and unknown naming.
    pub fn proxy_path(&self) -> Option<String> {
        if !self.is_video() {
            return None;
        }
        let ext = if self.name.starts_with("CAM_") {
            "XRF"
        } else if self.name.starts_with("DJI_") {
            "LRF"
        } else {
            return None;
        };
        let (stem, _) = self.path.rsplit_once('.')?;
        Some(format!("{stem}.{ext}"))
    }

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
    fn burst_names_require_an_exact_three_digit_suffix() {
        let f = file("DCIM/DJI_001/DJI_20260903165023_0012_D_003.JPG", "JPG");
        assert_eq!(f.group_key(), Some("DJI_20260903165023_0012_D"));
        assert_eq!(f.sub_index(), Some(3));
        for path in [
            "DCIM/x/PHOTO.JPG",
            "DCIM/x/PHOTO_01.JPG",
            "DCIM/x/PHOTO_1234.JPG",
            "DCIM/x/_001.JPG",
            "DCIM/x/PHOTO_001.",
        ] {
            assert_eq!(file(path, "JPG").group_key(), None, "{path}");
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
    fn a_still_has_a_dng_sidecar_candidate_beside_it() {
        assert_eq!(
            file("DCIM/DJI_001/DJI_X_D.JPG", "JPG").sidecar_paths(),
            vec!["DCIM/DJI_001/DJI_X_D.DNG"]
        );
        assert_eq!(
            file("DCIM/DJI_001/DJI_X_D.HEIC", "HEIC").sidecar_paths(),
            vec!["DCIM/DJI_001/DJI_X_D.DNG"]
        );
    }

    #[test]
    fn a_video_has_a_wav_sidecar_candidate_beside_it() {
        assert_eq!(
            file("DCIM/DJI_001/DJI_Y_D.MP4", "MP4").sidecar_paths(),
            vec!["DCIM/DJI_001/DJI_Y_D.WAV"]
        );
    }

    #[test]
    fn a_dng_or_an_extensionless_file_has_no_sidecar_candidates() {
        assert!(file("DCIM/DJI_001/DJI_X_D.DNG", "DNG")
            .sidecar_paths()
            .is_empty());
        assert!(file("DCIM/DJI_001/DJI_X_D", "").sidecar_paths().is_empty());
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

    #[test]
    fn proxy_path_follows_the_file_name_family() {
        let dji = file("DCIM/DJI_001/DJI_20261101084646_0008_D.MP4", "MP4");
        assert_eq!(
            dji.proxy_path().as_deref(),
            Some("DCIM/DJI_001/DJI_20261101084646_0008_D.LRF")
        );
        let cam = file("DCIM/100MEDIA/CAM_20261101_0001.MP4", "MP4");
        assert_eq!(
            cam.proxy_path().as_deref(),
            Some("DCIM/100MEDIA/CAM_20261101_0001.XRF")
        );
        assert_eq!(file("DCIM/DJI_001/DJI_1_D.JPG", "JPG").proxy_path(), None);
        assert_eq!(file("DCIM/x/GOPR0001.MP4", "MP4").proxy_path(), None);
    }
}
