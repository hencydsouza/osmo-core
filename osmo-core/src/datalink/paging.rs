//! Paging through the library: one cursor per store, each the oldest handle of that store's last page.
//! Ported from Osmosis `CameraSession` (`SliceInfo`, `storeHasOlderPage`, `stepPagination`, `oldestHandle`).
//! A page request is a pair of queries, counter 1 selecting the SD store and counter 2 the internal one; the
//! cursor's top bit is the store selector, so each store is walked inside its own handle namespace.
use super::stream::{chunk_tally, count_media_paths, has_end_marker, manifest_bytes};
use crate::media::CameraFile;
use std::collections::{HashMap, HashSet};

/// "Start from the newest" selectors, one per store.
pub const NEWEST_SD: u32 = 0x0000_0001;
pub const NEWEST_INTERNAL: u32 = 0x4000_0001;

/// A real page count never exceeds the records asked for; some bodies write a large non-count value at the
/// head of the manifest, which must not read as "declared 300 million, 41 arrived".
const COUNT_MAX: i64 = 512;

/// What one store's answer to a page query looked like on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceInfo {
    /// The manifest's own leading `u32-LE` count, or -1 when there is none.
    pub declared: i64,
    /// Media paths actually present in the bytes.
    pub records: usize,
    /// The `0c 01` TLV before the last record's name: the camera saying "nothing older".
    pub end_marker: bool,
    /// The camera sent its `4A 03` end frame for this request counter.
    pub ended: bool,
}

impl SliceInfo {
    /// Fewer records than the header promised: the answer was cut short in transit, not short.
    pub fn incomplete(&self) -> bool {
        (1..=COUNT_MAX).contains(&self.declared) && (self.records as i64) < self.declared
    }
}

/// Info about one reassembled manifest (a bare list, no frames).
pub fn manifest_info(manifest: &[u8], ended: bool) -> SliceInfo {
    let declared = if manifest.len() >= 4 {
        u32::from_le_bytes([manifest[0], manifest[1], manifest[2], manifest[3]]) as i64
    } else {
        -1
    };
    SliceInfo {
        declared,
        records: count_media_paths(manifest),
        end_marker: has_end_marker(manifest),
        ended,
    }
}

/// Info about the slice of a collected blob that answered request counter `ctr`.
pub fn slice_info(blob: &[u8], ctr: u8) -> SliceInfo {
    let ended = chunk_tally(blob)
        .get(&ctr)
        .and_then(|s| s.get(&0x03))
        .is_some_and(|n| *n > 0);
    manifest_info(&manifest_bytes(blob, Some(ctr)), ended)
}

/// Does `store`'s slice of a page leave an older page to fetch? Needs the cursor to have moved, then: a cut
/// slice means more (what it lost is older than the cursor); the camera's end marker is final; otherwise a
/// full page means more and a short one means done.
pub fn store_has_older_page(
    slice_len: usize,
    cursor_moved: bool,
    info: Option<&SliceInfo>,
    page_size: usize,
) -> bool {
    if !cursor_moved {
        return false;
    }
    match info {
        None => slice_len >= page_size,
        Some(i) if i.incomplete() => true,
        Some(i) if i.end_marker => false,
        Some(_) => slice_len >= page_size,
    }
}

/// The oldest usable handle in `store`'s slice of `page` that is strictly below `below`. Handle 0 means
/// "no handle", not a very old file: taking it would park the cursor at the bottom forever. A still whose
/// handle was never verified (a page with fewer than two handles cannot be fitted) falls back to its candidate:
/// paging only reads, so one source is enough here, unlike delete, which still needs the verified handle.
pub fn oldest_handle(page: &[CameraFile], store: u8, below: u32) -> Option<u32> {
    page.iter()
        .filter(|f| f.storage == store)
        .map(|f| {
            if f.handle != 0 {
                f.handle
            } else {
                f.handle_candidate
            }
        })
        .filter(|&h| h != 0 && h < below)
        .min()
}

fn slice_len(page: &[CameraFile], store: u8) -> usize {
    page.iter().filter(|f| f.storage == store).count()
}

/// Cursors and "is there more" after the newest page: each store's oldest handle, or its newest-page
/// selector when the store had nothing. `info` is keyed by store (0 SD, 1 internal).
pub fn seed_cursors(
    files: &[CameraFile],
    info: &HashMap<u8, SliceInfo>,
    page_size: usize,
) -> (u32, u32, bool) {
    let (sd, int) = (
        oldest_handle(files, 0, u32::MAX),
        oldest_handle(files, 1, u32::MAX),
    );
    let more = store_has_older_page(slice_len(files, 0), sd.is_some(), info.get(&0), page_size)
        || store_has_older_page(slice_len(files, 1), int.is_some(), info.get(&1), page_size);
    (
        sd.unwrap_or(NEWEST_SD),
        int.unwrap_or(NEWEST_INTERNAL),
        more,
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageStep {
    pub fresh: Vec<CameraFile>,
    pub sd_cursor: u32,
    pub internal_cursor: u32,
    pub more_available: bool,
}

/// One step of the walk: the unseen files of `page` (deduped by store and path, since the same name can exist
/// on both stores), each store's next cursor advanced independently, and whether another page is likely.
pub fn step_pagination(
    sd_cursor: u32,
    internal_cursor: u32,
    page: &[CameraFile],
    seen: &mut HashSet<(u8, String)>,
    info: &HashMap<u8, SliceInfo>,
    page_size: usize,
) -> PageStep {
    let fresh: Vec<CameraFile> = page
        .iter()
        .filter(|f| seen.insert((f.storage, f.path.clone())))
        .cloned()
        .collect();
    let sd = oldest_handle(page, 0, sd_cursor);
    let int = oldest_handle(page, 1, internal_cursor);
    let more_available = !fresh.is_empty()
        && (store_has_older_page(slice_len(page, 0), sd.is_some(), info.get(&0), page_size)
            || store_has_older_page(slice_len(page, 1), int.is_some(), info.get(&1), page_size));
    PageStep {
        fresh,
        sd_cursor: sd.unwrap_or(sd_cursor),
        internal_cursor: int.unwrap_or(internal_cursor),
        more_available,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datalink::stream::manifest_bytes;
    use std::collections::{HashMap, HashSet};

    fn fx(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/tests/fixtures/manifests/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    fn file(storage: u8, handle: u32, path: &str) -> CameraFile {
        CameraFile {
            path: path.to_string(),
            thumb_path: String::new(),
            name: path.rsplit('/').next().unwrap().to_string(),
            ext: "JPG".into(),
            storage,
            size_bytes: 1,
            handle,
            group: 0,
            seq: 0,
            cmd_handle: 0,
            handle_candidate: 0,
            handle_shared: false,
            starred: false,
        }
    }

    fn info(declared: i64, records: usize, end_marker: bool) -> SliceInfo {
        SliceInfo {
            declared,
            records,
            end_marker,
            ended: true,
        }
    }

    #[test]
    fn a_short_page_ends_the_library_and_a_full_one_does_not() {
        assert!(!store_has_older_page(13, true, None, 45));
        assert!(store_has_older_page(45, true, None, 45));
        assert!(
            !store_has_older_page(45, false, None, 45),
            "a cursor that never moved is the end"
        );
    }

    #[test]
    fn the_end_marker_is_final_even_on_a_full_page() {
        assert!(!store_has_older_page(
            45,
            true,
            Some(&info(45, 45, true)),
            45
        ));
        assert!(store_has_older_page(
            45,
            true,
            Some(&info(45, 45, false)),
            45
        ));
    }

    #[test]
    fn truncation_outranks_the_end_marker() {
        let cut = info(21, 17, true);
        assert!(cut.incomplete());
        assert!(
            store_has_older_page(17, true, Some(&cut), 45),
            "what was lost is older than the cursor"
        );
    }

    #[test]
    fn a_declared_count_that_is_not_a_plausible_page_size_is_not_truncation() {
        assert!(!info(0x1310_0000, 41, false).incomplete());
        assert!(!info(0, 41, false).incomplete());
        assert!(!info(-1, 0, false).incomplete());
        assert!(info(14, 6, false).incomplete());
    }

    #[test]
    fn a_store_that_ran_out_stops_advancing_while_the_other_pages() {
        let page = vec![
            file(1, 0x40000100, "DCIM/a/1.JPG"),
            file(1, 0x40000090, "DCIM/a/2.JPG"),
        ];
        let mut seen = HashSet::new();
        let step = step_pagination(
            0x0004_0100,
            0x4000_0200,
            &page,
            &mut seen,
            &HashMap::new(),
            2,
        );
        assert_eq!(
            step.sd_cursor, 0x0004_0100,
            "no SD records: cursor unchanged"
        );
        assert_eq!(
            step.internal_cursor, 0x4000_0090,
            "oldest internal handle below the old cursor"
        );
        assert!(step.more_available);
        assert_eq!(step.fresh.len(), 2);
    }

    #[test]
    fn files_are_deduped_by_store_and_path() {
        let mut seen = HashSet::new();
        let page = vec![
            file(0, 0x40010, "DCIM/a/1.JPG"),
            file(1, 0x4000_0010, "DCIM/a/1.JPG"),
        ];
        let first = step_pagination(u32::MAX, u32::MAX, &page, &mut seen, &HashMap::new(), 45);
        assert_eq!(
            first.fresh.len(),
            2,
            "the same name on two stores is two files"
        );
        let again = step_pagination(u32::MAX, u32::MAX, &page, &mut seen, &HashMap::new(), 45);
        assert!(again.fresh.is_empty(), "nothing new the second time");
        assert!(!again.more_available, "no fresh files means no more pages");
    }

    #[test]
    fn handle_zero_never_becomes_a_cursor() {
        let page = vec![file(0, 0, "DCIM/a/1.JPG"), file(0, 0x40050, "DCIM/a/2.JPG")];
        assert_eq!(oldest_handle(&page, 0, u32::MAX), Some(0x40050));
        assert_eq!(
            oldest_handle(&[file(0, 0, "DCIM/a/1.JPG")], 0, u32::MAX),
            None
        );
        assert_eq!(
            oldest_handle(&page, 0, 0x40050),
            None,
            "strictly below the current cursor"
        );
    }

    #[test]
    fn a_page_of_stills_with_no_verified_handles_still_advances_from_their_candidates() {
        // A page with no video has fewer than two handles, so the list cannot be fitted and no still is
        // promoted. Paging only reads, so the candidate (the value at the record's fixed position) is enough.
        let mut a = file(0, 0, "DCIM/a/1.JPG");
        a.handle_candidate = 0x40050;
        let mut b = file(0, 0, "DCIM/a/2.JPG");
        b.handle_candidate = 0x40040;
        let page = vec![a, b];
        assert_eq!(oldest_handle(&page, 0, u32::MAX), Some(0x40040));
        let step = step_pagination(
            0x40060,
            NEWEST_INTERNAL,
            &page,
            &mut HashSet::new(),
            &HashMap::new(),
            2,
        );
        assert_eq!(
            step.sd_cursor, 0x40040,
            "the cursor moved, so a full page means there is more"
        );
        assert!(step.more_available);
        let (sd, _, more) = seed_cursors(&page, &HashMap::new(), 2);
        assert_eq!((sd, more), (0x40040, true));
        assert!(
            page.iter().all(|f| !f.deletable()),
            "...and none of them became deletable"
        );
    }

    #[test]
    fn seeding_picks_each_stores_oldest_handle_and_the_newest_selector_for_an_empty_store() {
        let page = vec![
            file(0, 0x40050, "DCIM/a/1.JPG"),
            file(0, 0x40020, "DCIM/a/2.JPG"),
        ];
        let (sd, internal, more) = seed_cursors(&page, &HashMap::new(), 2);
        assert_eq!((sd, internal), (0x40020, NEWEST_INTERNAL));
        assert!(more, "a full page (2 of 2) means there may be more");
        assert!(
            !seed_cursors(&page, &HashMap::new(), 45).2,
            "a short page is the end"
        );
    }

    #[test]
    fn end_markers_are_on_final_pages_and_not_on_full_ones() {
        for f in [
            "xtra_13.bin",
            "oa4_6.bin",
            "oa6_sd_3.bin",
            "oa6_internal_2.bin",
            "op3_15.bin",
        ] {
            assert!(
                manifest_info(&manifest_bytes(&fx(f), None), true).end_marker,
                "{f} is a final page"
            );
        }
        for f in ["nano_45.bin", "oa4_45.bin", "op4_45.bin"] {
            assert!(
                !manifest_info(&manifest_bytes(&fx(f), None), true).end_marker,
                "{f} is a full page"
            );
        }
    }

    #[test]
    fn no_captured_fixture_reads_as_truncated() {
        let dir = format!("{}/tests/fixtures/manifests", env!("CARGO_MANIFEST_DIR"));
        let mut n = 0;
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "bin") {
                let raw = std::fs::read(&p).unwrap();
                let i = manifest_info(&manifest_bytes(&raw, None), true);
                assert!(
                    !i.incomplete(),
                    "{}: declared {} records {}",
                    p.display(),
                    i.declared,
                    i.records
                );
                n += 1;
            }
        }
        assert!(n >= 10, "only {n} fixtures scanned");
    }

    #[test]
    fn a_cut_manifest_reads_as_truncated() {
        let live = fx("oa4_live_14.bin");
        let whole = manifest_info(&live, true);
        assert_eq!(
            (whole.declared, whole.records, whole.incomplete()),
            (14, 14, false)
        );
        let cut = manifest_info(&live[..3000], true);
        assert_eq!(cut.declared, 14);
        assert!(cut.records < 14 && cut.incomplete());
    }
}
