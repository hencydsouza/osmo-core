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

/// Where a thumbnail might come from, tried in order: a whole small file, or the EXIF-embedded JPEG inside the
/// first 64 KiB of a still's original.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThumbSource {
    Whole(String),
    EmbeddedHead(String),
}

/// The camera keeps a `.scr` screennail per file (videos) or a `.thm` (stills), never both; a still can also
/// carry a JPEG in its own EXIF. Each entry is an HTTP path on the file's store.
pub fn thumb_sources(f: &CameraFile) -> Vec<ThumbSource> {
    let on = |path: &str| osmo_core::media::url_path(f.storage, path);
    let mut v = vec![ThumbSource::Whole(on(&f.thumb_path))];
    if let Some(stem) = f.thumb_path.strip_suffix(".scr") {
        v.push(ThumbSource::Whole(on(&format!("{stem}.thm"))));
    }
    if !f.is_video() {
        v.push(ThumbSource::EmbeddedHead(f.url_path()));
    }
    v
}

/// The sidecar candidates the camera actually has: each candidate's HTTP path is passed to `head`, which returns
/// its size or `None` (a 404 means there is none, which is not an error).
pub fn sidecars_to_fetch(
    f: &CameraFile,
    head: &mut dyn FnMut(&str) -> Option<u64>,
) -> Vec<(String, u64)> {
    f.sidecar_paths()
        .into_iter()
        .filter_map(|p| head(&osmo_core::media::url_path(f.storage, &p)).map(|n| (p, n)))
        .collect()
}

/// A file known only by its store and camera path (the `--storage` shortcut skips the listing).
pub fn file_from_path(storage: u8, path: &str) -> CameraFile {
    let name = path.rsplit('/').next().unwrap_or(path).to_string();
    CameraFile {
        ext: name
            .rsplit_once('.')
            .map(|(_, e)| e.to_uppercase())
            .unwrap_or_default(),
        name,
        path: path.to_string(),
        thumb_path: osmo_core::media::derive_thumb_path(path),
        storage,
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

/// When a page arrived cut short even after a retry, the listing may be missing files.
pub fn truncation_warning(truncated: bool) -> Option<String> {
    truncated.then(|| {
        "the camera cut a page short, so this listing may be incomplete; run it again (add -v to see which store)".to_string()
    })
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
        let star = if f.starred { " \u{2605}" } else { "" };
        lines.push(row(
            store,
            &human_size(f.size_bytes),
            &format!("{}{star}", f.path),
        ));
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

/// The listed files for the requested paths, in request order without repeats, plus the paths that are not listed.
pub fn resolve_requested<'a>(
    files: &'a [CameraFile],
    requested: &[String],
) -> (Vec<&'a CameraFile>, Vec<String>) {
    let mut found: Vec<&CameraFile> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for path in requested {
        match find_file(files, path) {
            Some(f) if !found.iter().any(|c| c.path == f.path) => found.push(f),
            Some(_) => {}
            None if !missing.contains(path) => missing.push(path.clone()),
            None => {}
        }
    }
    (found, missing)
}

pub fn download_summary(saved: usize, skipped: usize, failed: usize) -> String {
    format!("{saved} saved, {skipped} already complete, {failed} failed")
}

/// Turn the paths the user typed into the exact listed files to delete, or refuse. All or nothing: one bad path
/// refuses the whole request so a typo never leaves a partial deletion. Paths must match a listed path exactly
/// (no globs, no directories), and the file must be safely deletable.
pub fn plan_deletions<'a>(
    files: &'a [CameraFile],
    requested: &[String],
) -> Result<Vec<&'a CameraFile>, Vec<String>> {
    if requested.is_empty() {
        return Err(vec!["nothing to delete: give at least one path".to_string()]);
    }
    let mut chosen: Vec<&CameraFile> = Vec::new();
    let mut refusals = Vec::new();
    for path in requested {
        if validate_get_path(path).is_err() || path.contains(['*', '?', '[']) || path.ends_with('/')
        {
            refusals.push(format!(
                "{path}: not a single file path as printed by `osmo ls`"
            ));
            continue;
        }
        match find_file(files, path) {
            None => refusals.push(format!(
                "{path}: not in the camera's newest listing, so it will not be deleted (check it against `osmo ls`)"
            )),
            Some(f) if !f.deletable() => refusals.push(format!(
                "{path}: no verified, unshared handle, so it will not be deleted"
            )),
            Some(f) => {
                if !chosen.iter().any(|c| c.path == f.path) {
                    chosen.push(f);
                }
            }
        }
    }
    if refusals.is_empty() {
        Ok(chosen)
    } else {
        Err(refusals)
    }
}

/// `date +%z` output (`+0530`, `-0700`) as signed minutes east of UTC.
pub fn parse_utc_offset(s: &str) -> Option<i16> {
    let b = s.trim().as_bytes();
    if b.len() != 5 || !b[1..].iter().all(u8::is_ascii_digit) || !matches!(b[0], b'+' | b'-') {
        return None;
    }
    let n = |i: usize| ((b[i] - b'0') * 10 + b[i + 1] - b'0') as i16;
    let (h, m) = (n(1), n(3));
    if h > 14 || m > 59 {
        return None;
    }
    let mins = h * 60 + m;
    Some(if b[0] == b'-' { -mins } else { mins })
}

/// The IANA zone name from the target of `/etc/localtime`, e.g. `Asia/Kolkata`.
pub fn zone_from_link(target: &str) -> Option<String> {
    target
        .split_once("zoneinfo/")
        .map(|(_, z)| z.to_string())
        .filter(|z| !z.is_empty())
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
    fn truncation_warning_names_the_risk() {
        let w = truncation_warning(true).unwrap();
        assert!(
            w.contains("incomplete") && w.contains("run it again"),
            "{w}"
        );
        assert_eq!(truncation_warning(false), None);
    }

    #[test]
    fn star_marker_appears_only_on_starred_rows() {
        let mut starred = file("DCIM/DJI_001/A.JPG", 0, 10);
        starred.starred = true;
        let plain = file("DCIM/DJI_001/B.JPG", 0, 10);
        let out = format_listing(&[starred, plain]);
        let rows: Vec<&str> = out.lines().skip(1).collect();
        assert!(rows[0].ends_with("A.JPG \u{2605}"), "{}", rows[0]);
        assert!(
            rows[1].ends_with("B.JPG") && !rows[1].contains('\u{2605}'),
            "{}",
            rows[1]
        );
    }

    #[test]
    fn thumb_sources_try_scr_then_thm_then_the_embedded_jpeg_for_stills() {
        let mut still = file("DCIM/DJI_001/DJI_X_D.JPG", 0, 10);
        still.ext = "JPG".into();
        still.thumb_path = "MISC/THM/DJI_001/DJI_X_D.scr".into();
        assert_eq!(
            thumb_sources(&still),
            vec![
                ThumbSource::Whole("/v2?storage=0&path=MISC/THM/DJI_001/DJI_X_D.scr".into()),
                ThumbSource::Whole("/v2?storage=0&path=MISC/THM/DJI_001/DJI_X_D.thm".into()),
                ThumbSource::EmbeddedHead("/v2?storage=0&path=DCIM/DJI_001/DJI_X_D.JPG".into()),
            ]
        );
        let mut video = file("DCIM/DJI_001/DJI_Y_D.MP4", 1, 10);
        video.thumb_path = "MISC/THM/DJI_001/DJI_Y_D.scr".into();
        let sources = thumb_sources(&video);
        assert_eq!(
            sources.len(),
            2,
            "a video has no EXIF thumbnail to fall back on"
        );
        assert_eq!(
            sources[1],
            ThumbSource::Whole("/v2?storage=1&path=MISC/THM/DJI_001/DJI_Y_D.thm".into())
        );
    }

    #[test]
    fn file_from_path_derives_the_thumbnail_path_for_the_storage_shortcut() {
        let f = file_from_path(1, "DCIM/DJI_001/DJI_20260903165023_0012_D.JPG");
        assert_eq!((f.storage, f.ext.as_str()), (1, "JPG"));
        assert_eq!(
            f.thumb_path,
            "MISC/THM/DJI_001/DJI_20260903165023_0012_D.scr"
        );
        assert_eq!(f.name, "DJI_20260903165023_0012_D.JPG");
    }

    #[test]
    fn sidecars_to_fetch_keeps_only_the_candidates_the_camera_has() {
        let still = file_from_path(1, "DCIM/DJI_001/DJI_X_D.JPG");
        let mut asked = vec![];
        let have = sidecars_to_fetch(&still, &mut |url| {
            asked.push(url.to_string());
            Some(80_332_744)
        });
        assert_eq!(asked, vec!["/v2?storage=1&path=DCIM/DJI_001/DJI_X_D.DNG"]);
        assert_eq!(
            have,
            vec![("DCIM/DJI_001/DJI_X_D.DNG".to_string(), 80_332_744)]
        );
        assert!(
            sidecars_to_fetch(&still, &mut |_| None).is_empty(),
            "a 404 means none, and is not an error"
        );
        let video = file_from_path(0, "DCIM/DJI_001/DJI_Y_D.MP4");
        assert_eq!(
            sidecars_to_fetch(&video, &mut |_| Some(925_740)),
            vec![("DCIM/DJI_001/DJI_Y_D.WAV".to_string(), 925_740)]
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

    fn deletable_file(path: &str, handle: u32) -> CameraFile {
        CameraFile {
            handle,
            ..file(path, 0, 10)
        }
    }

    #[test]
    fn plan_deletions_refuses_unknown_paths_by_name_and_deletes_nothing() {
        let files = vec![deletable_file("DCIM/DJI_001/A_0001_D.JPG", 0x40100040)];
        let req = vec![
            "DCIM/DJI_001/A_0001_D.JPG".to_string(),
            "DCIM/DJI_001/TYPO_0002_D.JPG".to_string(),
        ];
        let err = plan_deletions(&files, &req).unwrap_err();
        assert_eq!(err.len(), 1);
        assert!(err[0].contains("TYPO_0002_D.JPG"), "{err:?}");
    }

    #[test]
    fn plan_deletions_refuses_a_non_deletable_file() {
        let mut shared = deletable_file("DCIM/DJI_001/A_0001_D.JPG", 0x40100040);
        shared.handle_shared = true;
        let unverified = deletable_file("DCIM/DJI_001/B_0002_D.JPG", 0);
        let files = vec![shared, unverified];
        let req = vec![
            "DCIM/DJI_001/A_0001_D.JPG".to_string(),
            "DCIM/DJI_001/B_0002_D.JPG".to_string(),
        ];
        let err = plan_deletions(&files, &req).unwrap_err();
        assert_eq!(err.len(), 2);
        assert!(err[0].contains("A_0001_D.JPG") && err[1].contains("B_0002_D.JPG"));
    }

    #[test]
    fn plan_deletions_dedupes_repeated_paths() {
        let files = vec![deletable_file("DCIM/DJI_001/A_0001_D.JPG", 0x40100040)];
        let req = vec![
            "DCIM/DJI_001/A_0001_D.JPG".to_string(),
            "DCIM/DJI_001/A_0001_D.JPG".to_string(),
        ];
        assert_eq!(plan_deletions(&files, &req).unwrap().len(), 1);
    }

    #[test]
    fn plan_deletions_refuses_globs_directories_and_nothing_at_all() {
        let files = vec![deletable_file("DCIM/DJI_001/A_0001_D.JPG", 0x40100040)];
        for bad in [
            "DCIM/DJI_001/*",
            "DCIM/DJI_001/",
            "DCIM/",
            "../etc/passwd",
            "DCIM/DJI_001/A_000?_D.JPG",
        ] {
            let err = plan_deletions(&files, &[bad.to_string()]).unwrap_err();
            assert_eq!(err.len(), 1, "{bad}");
        }
        assert!(plan_deletions(&files, &[]).unwrap_err()[0].contains("nothing"));
    }

    #[test]
    fn utc_offset_parses_date_z_output() {
        assert_eq!(parse_utc_offset("+0530"), Some(330));
        assert_eq!(parse_utc_offset("-0700\n"), Some(-420));
        assert_eq!(parse_utc_offset("+0000"), Some(0));
        assert_eq!(parse_utc_offset("IST"), None);
        assert_eq!(parse_utc_offset("+05"), None);
    }

    #[test]
    fn zone_name_comes_from_the_localtime_link() {
        assert_eq!(
            zone_from_link("/var/db/timezone/zoneinfo/Asia/Kolkata"),
            Some("Asia/Kolkata".to_string())
        );
        assert_eq!(
            zone_from_link("/usr/share/zoneinfo/UTC"),
            Some("UTC".to_string())
        );
        assert_eq!(zone_from_link("/etc/something-else"), None);
    }

    #[test]
    fn resolve_requested_keeps_order_dedupes_and_reports_missing() {
        let files = vec![
            deletable_file("DCIM/DJI_001/A_0001_D.JPG", 1),
            deletable_file("DCIM/DJI_001/B_0002_D.JPG", 2),
        ];
        let req: Vec<String> = [
            "DCIM/DJI_001/B_0002_D.JPG",
            "DCIM/DJI_001/NOPE_0003_D.JPG",
            "DCIM/DJI_001/A_0001_D.JPG",
            "DCIM/DJI_001/B_0002_D.JPG",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let (found, missing) = resolve_requested(&files, &req);
        let names: Vec<&str> = found.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            names,
            ["DCIM/DJI_001/B_0002_D.JPG", "DCIM/DJI_001/A_0001_D.JPG"]
        );
        assert_eq!(missing, ["DCIM/DJI_001/NOPE_0003_D.JPG"]);
    }

    #[test]
    fn download_summary_counts_every_outcome() {
        assert_eq!(
            download_summary(2, 1, 0),
            "2 saved, 1 already complete, 0 failed"
        );
    }
}
