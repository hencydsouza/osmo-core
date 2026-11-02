//! Pure presentation and validation helpers for `osmo ls` / `osmo get`.
use osmo_core::media::CameraFile;

pub fn human_size(n: u64) -> String {
    if n == 0 {
        return "?".to_string();
    }
    if n < 1024 {
        return format!("{n} B");
    }
    let (mut v, mut unit) = (n as f64 / 1024.0, "KiB");
    for next in ["MiB", "GiB", "TiB"] {
        if v < 1024.0 {
            break;
        }
        v /= 1024.0;
        unit = next;
    }
    format!("{v:.1} {unit}")
}

/// `3.0 MiB / 6.0 MiB (50%)`, or `3.0 MiB / ?` when the size is unknown.
pub fn progress_line(done: u64, total: Option<u64>) -> String {
    match total.filter(|t| *t > 0) {
        Some(t) => format!(
            "{} / {} ({}%)",
            human_size(done),
            human_size(t),
            (done * 100 / t).min(100)
        ),
        None => format!("{} / ?", human_size(done)),
    }
}

/// One line like the camera's own screen: `battery 87%  SD 107.2 GiB free of 118.9 GiB`. Absent or zero-capacity
/// fields are omitted; an empty status gives `""`.
pub fn format_status(s: &osmo_core::status::Status) -> String {
    let gib = |mb: u32| mb as f64 / 1024.0;
    let mut parts: Vec<String> = Vec::new();
    if let Some(b) = s.battery_percent {
        let charging = if s.charging == Some(true) {
            " (charging)"
        } else {
            ""
        };
        parts.push(format!("battery {b}%{charging}"));
    }
    for (label, total, free) in [
        ("SD", s.sd_total_mb, s.sd_free_mb),
        ("internal", s.internal_total_mb, s.internal_free_mb),
    ] {
        if let (Some(total), Some(free)) = (total, free) {
            if total > 0 {
                parts.push(format!(
                    "{label} {:.1} GiB free of {:.1} GiB",
                    gib(free),
                    gib(total)
                ));
            }
        }
    }
    parts.join("  ")
}

/// What to tell the user when a download gave up. A stall is nearly always the camera's WiFi going to sleep.
pub fn download_error_message(e: &osmo_core::http::DownloadError) -> String {
    match e {
        osmo_core::http::DownloadError::Stalled { .. } => format!(
            "{e}\nThe camera's WiFi may have gone to sleep: run `osmo wifi`, rejoin it, then run the same command again to resume."
        ),
        other => other.to_string(),
    }
}

/// The manifest's size, or (when it is 0, as for the longest videos) whatever the camera reports over HTTP.
pub fn known_size(f: &CameraFile, head: impl FnOnce() -> Option<u64>) -> Option<u64> {
    if f.size_bytes > 0 {
        Some(f.size_bytes)
    } else {
        head()
    }
}

/// Fill in sizes the manifest left at 0 by asking the camera. Returns how many were filled.
pub fn fill_unknown_sizes(
    files: &mut [CameraFile],
    head: &mut dyn FnMut(&CameraFile) -> Option<u64>,
) -> usize {
    let mut filled = 0;
    for f in files.iter_mut().filter(|f| f.size_bytes == 0) {
        if let Some(n) = head(f) {
            f.size_bytes = n;
            filled += 1;
        }
    }
    filled
}

pub fn format_listing(files: &[CameraFile]) -> String {
    if files.is_empty() {
        return "no files".to_string();
    }
    let row = |store: &str, size: &str, path: &str| format!("{store:<8}  {size:>9}  {path}");
    let mut lines = vec![row("STORE", "SIZE", "PATH")];
    for f in files {
        let store = if f.storage == 1 { "internal" } else { "sd" };
        lines.push(row(store, &human_size(f.size_bytes), &f.path));
    }
    lines.join("\n")
}

/// `get` takes the camera path exactly as `ls` prints it; anything else is refused before any network use.
pub fn validate_get_path(p: &str) -> Result<&str, String> {
    if !p.starts_with("DCIM/") || p.split('/').any(|c| c == "..") || p.contains('\\') {
        return Err(format!(
            "not a camera media path: {p:?} (expected a path under DCIM/ as printed by `osmo ls`)"
        ));
    }
    Ok(p)
}

pub fn dest_filename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub fn find_file<'a>(files: &'a [CameraFile], path: &str) -> Option<&'a CameraFile> {
    files.iter().find(|f| f.path == path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use osmo_core::media::CameraFile;

    fn file(path: &str, storage: u8, size: u64) -> CameraFile {
        CameraFile {
            path: path.to_string(),
            thumb_path: String::new(),
            name: path.rsplit('/').next().unwrap().to_string(),
            ext: "MP4".into(),
            storage,
            size_bytes: size,
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
    fn empty_listing_says_no_files() {
        assert_eq!(format_listing(&[]), "no files");
    }

    #[test]
    fn listing_aligns_path_column() {
        let out = format_listing(&[
            file("DCIM/DJI_001/DJI_A.MP4", 0, 1_536),
            file("DCIM/DJI_001/DJI_B.MP4", 1, 3_221_225_472),
        ]);
        let cols: Vec<usize> = out
            .lines()
            .map(|l| l.find("DCIM/").unwrap_or_else(|| l.find("PATH").unwrap()))
            .collect();
        assert_eq!(out.lines().count(), 3, "header + two rows");
        assert!(cols.windows(2).all(|w| w[0] == w[1]), "{cols:?}\n{out}");
        assert!(out.contains("sd") && out.contains("internal"));
        assert!(out.contains("1.5 KiB") && out.contains("3.0 GiB"));
    }

    #[test]
    fn progress_line_shows_percentage_or_unknown_total() {
        assert_eq!(
            progress_line(3_145_728, Some(6_291_456)),
            "3.0 MiB / 6.0 MiB (50%)"
        );
        assert_eq!(progress_line(1_048_576, None), "1.0 MiB / ?");
        assert_eq!(
            progress_line(7_000_000, Some(6_000_000)),
            "6.7 MiB / 5.7 MiB (100%)"
        );
    }

    #[test]
    fn fill_unknown_sizes_asks_only_for_files_the_manifest_could_not_size() {
        let mut files = vec![
            file("DCIM/DJI_001/A.MP4", 0, 0),
            file("DCIM/DJI_001/B.MP4", 0, 99),
            file("DCIM/DJI_001/C.MP4", 1, 0),
        ];
        let mut asked = vec![];
        let filled = fill_unknown_sizes(&mut files, &mut |f| {
            asked.push(f.name.clone());
            (f.name == "A.MP4").then_some(5_000_000_000)
        });
        assert_eq!(
            asked,
            vec!["A.MP4", "C.MP4"],
            "a file with a known size is never asked about"
        );
        assert_eq!(filled, 1);
        assert_eq!(
            files.iter().map(|f| f.size_bytes).collect::<Vec<_>>(),
            vec![5_000_000_000, 99, 0]
        );
    }

    #[test]
    fn known_size_prefers_the_manifest_and_only_then_asks_the_camera() {
        let f = file("DCIM/DJI_001/A.MP4", 0, 123);
        assert_eq!(known_size(&f, || panic!("must not ask")), Some(123));
        let z = file("DCIM/DJI_001/B.MP4", 0, 0);
        assert_eq!(known_size(&z, || Some(7)), Some(7));
        assert_eq!(known_size(&z, || None), None);
    }

    #[test]
    fn a_stalled_download_tells_the_user_how_to_wake_the_camera_and_resume() {
        let m =
            download_error_message(&osmo_core::http::DownloadError::Stalled { at: 2_590_527_922 });
        assert!(m.contains("2590527922"), "{m}");
        assert!(m.contains("osmo wifi") && m.contains("resume"), "{m}");
        let io = download_error_message(&osmo_core::http::DownloadError::Io(
            std::io::Error::other("disk full"),
        ));
        assert!(
            io.contains("disk full") && !io.contains("osmo wifi"),
            "{io}"
        );
    }

    #[test]
    fn format_status_shows_battery_and_free_space_like_the_camera_screen() {
        let s = osmo_core::status::Status {
            battery_percent: Some(87),
            sd_total_mb: Some(121_785),
            sd_free_mb: Some(109_748),
            ..Default::default()
        };
        assert_eq!(
            format_status(&s),
            "battery 87%  SD 107.2 GiB free of 118.9 GiB"
        );
    }

    #[test]
    fn format_status_omits_what_is_absent_or_zero_and_marks_charging() {
        use osmo_core::status::Status;
        assert_eq!(format_status(&Status::default()), "");
        let s = Status {
            battery_percent: Some(40),
            charging: Some(true),
            sd_total_mb: Some(0),
            internal_total_mb: Some(0),
            ..Default::default()
        };
        assert_eq!(format_status(&s), "battery 40% (charging)");
        let both = Status {
            internal_total_mb: Some(48_980),
            internal_free_mb: Some(10_240),
            ..Default::default()
        };
        assert_eq!(format_status(&both), "internal 10.0 GiB free of 47.8 GiB");
    }

    #[test]
    fn human_size_formats() {
        assert_eq!(human_size(0), "?");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1_536), "1.5 KiB");
        assert_eq!(human_size(4_173_824), "4.0 MiB");
        assert_eq!(human_size(1_073_741_824), "1.0 GiB");
    }

    #[test]
    fn get_rejects_paths_outside_dcim_before_any_network_use() {
        assert!(validate_get_path("DCIM/DJI_001/DJI_A.MP4").is_ok());
        assert!(validate_get_path("../etc/passwd").is_err());
        assert!(validate_get_path("MISC/THM/x.scr").is_err());
        assert!(validate_get_path("DCIM/../../secret").is_err());
        assert!(validate_get_path("").is_err());
    }

    #[test]
    fn dest_filename_is_the_last_component() {
        assert_eq!(dest_filename("DCIM/DJI_001/DJI_A.MP4"), "DJI_A.MP4");
    }

    #[test]
    fn find_file_matches_exact_path() {
        let files = vec![
            file("DCIM/DJI_001/DJI_A.MP4", 1, 5),
            file("DCIM/DJI_001/DJI_B.MP4", 0, 6),
        ];
        assert_eq!(
            find_file(&files, "DCIM/DJI_001/DJI_B.MP4").unwrap().storage,
            0
        );
        assert!(find_file(&files, "DCIM/DJI_001/NOPE.MP4").is_none());
    }
}
