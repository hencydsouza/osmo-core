//! Decodes every captured manifest and compares against Osmosis's golden snapshots.
//! Golden line: `name|ext|handle|cmdHandle|size|resolution|resLabel|duration|starred|mediaType|group|deletable`.
//! Only name, ext, size and group (cols 0, 1, 4, 10) are in scope for the prototype; handle, cmdHandle,
//! resolution, fps, duration, star and media type are not decoded here.
use osmo_core::datalink::{collect_stores, decode_manifest};
use std::fs;

fn dir() -> String {
    format!("{}/tests/fixtures/manifests", env!("CARGO_MANIFEST_DIR"))
}

fn goldens() -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(format!("{}/golden", dir()))
        .unwrap()
        .map(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .trim_end_matches(".golden.txt")
                .to_string()
        })
        .collect();
    v.sort();
    v
}

#[test]
fn every_golden_fixture_matches_name_ext_size_group() {
    let names = goldens();
    assert_eq!(names.len(), 14);
    for name in names {
        let raw = fs::read(format!("{}/{name}", dir())).unwrap();
        let mut got: Vec<String> = decode_manifest(&raw)
            .iter()
            .map(|f| format!("{}|{}|{}|{}", f.name, f.ext, f.size_bytes, f.group))
            .collect();
        let golden = fs::read_to_string(format!("{}/golden/{name}.golden.txt", dir())).unwrap();
        let mut want: Vec<String> = golden
            .lines()
            .map(|l| {
                let c: Vec<&str> = l.split('|').collect();
                format!("{}|{}|{}|{}", c[0], c[1], c[4], c[10])
            })
            .collect();
        got.sort();
        want.sort();
        assert_eq!(got.len(), want.len(), "{name}: record count");
        assert_eq!(got, want, "{name}: decoded fields differ");
    }
}

#[test]
fn url_path_is_v2_storage_and_unencoded_path() {
    let raw = fs::read(format!("{}/oa4_45.bin", dir())).unwrap();
    let files = decode_manifest(&raw);
    let f = files
        .iter()
        .find(|f| f.name == "DJI_20260903165023_0012_D.JPG")
        .unwrap();
    assert!(f.path.starts_with("DCIM/") && f.path.ends_with(&f.name));
    assert!(f.thumb_path.starts_with("MISC/") && f.thumb_path.ends_with(".scr"));
    assert_eq!(f.url_path(), format!("/v2?storage=0&path={}", f.path));
}

#[test]
fn empty_blob_decodes_to_empty() {
    assert!(decode_manifest(&[]).is_empty());
    assert!(collect_stores(&[], 1, 2).is_empty());
}

#[test]
fn truncated_blob_does_not_panic_and_yields_fewer_files() {
    let raw = fs::read(format!("{}/oa4_45.bin", dir())).unwrap();
    let full = decode_manifest(&raw).len();
    for cut in [100, 1000, 5000, raw.len() - 1] {
        let n = decode_manifest(&raw[..cut]).len();
        assert!(n <= full, "cut {cut}");
    }
}

#[test]
fn garbage_does_not_panic() {
    let junk: Vec<u8> = (0..5000u32).map(|i| (i * 7 % 253) as u8).collect();
    let _ = decode_manifest(&junk);
}

#[test]
fn collect_stores_stamps_storage_from_the_request_counter() {
    // nano_45: SD query (ctr 1) opens and returns nothing; internal (ctr 2) carries the 45 records.
    let raw = fs::read(format!("{}/nano_45.bin", dir())).unwrap();
    let files = collect_stores(&raw, 1, 2);
    assert_eq!(files.len(), 45);
    assert!(files.iter().all(|f| f.storage == 1));
    assert!(files[0].url_path().starts_with("/v2?storage=1&path=DCIM/"));
}
