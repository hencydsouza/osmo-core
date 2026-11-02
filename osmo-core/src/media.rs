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
    /// Camera-assigned handle from the record's fixed marker (videos only); 0 = unknown.
    pub handle: u32,
    /// Which of the manifest's concatenated per-store lists the record came from (0 first, 1 second).
    pub group: u8,
}

impl CameraFile {
    /// Path of the original on the camera's HTTP file API. The path is deliberately not percent-encoded.
    pub fn url_path(&self) -> String {
        format!("/v2?storage={}&path={}", self.storage, self.path)
    }
}
