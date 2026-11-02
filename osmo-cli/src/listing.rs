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
