//! Regression test from a live Osmo Action 4 listing (2026-11-03): 14 files on the SD card, captured as the
//! reassembled manifest. Sizes of two files are ground truth: they equal the bytes downloaded from the camera.
use osmo_core::datalink::decode_manifest;

fn files() -> Vec<osmo_core::media::CameraFile> {
    let raw = std::fs::read(format!(
        "{}/tests/fixtures/manifests/oa4_live_14.bin",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    decode_manifest(&raw)
}

fn find<'a>(f: &'a [osmo_core::media::CameraFile], stem: &str) -> &'a osmo_core::media::CameraFile {
    f.iter()
        .find(|x| x.name.contains(stem))
        .unwrap_or_else(|| panic!("no file {stem}"))
}

#[test]
fn live_listing_decodes_all_14_files_with_extensions_on_the_sd_card() {
    let f = files();
    assert_eq!(f.len(), 14);
    assert!(
        f.iter()
            .all(|x| x.storage == 0 && (x.ext == "JPG" || x.ext == "MP4")),
        "{f:#?}"
    );
    assert!(f
        .iter()
        .all(|x| x.path.starts_with("DCIM/DJI_001/") && x.name.ends_with(&x.ext)));
}

#[test]
fn manifest_sizes_match_the_bytes_actually_downloaded() {
    let f = files();
    assert_eq!(
        find(&f, "0005_D").size_bytes,
        133_959_725,
        "the resumed video downloaded exactly this many bytes"
    );
    assert_eq!(
        find(&f, "0012_D").size_bytes,
        6_373_376,
        "the photo downloaded exactly this many bytes"
    );
}

#[test]
fn the_four_longest_videos_carry_size_zero_in_the_manifest() {
    // 316 to 817 s recordings: the u32 size field is 0, so a caller must ask the camera over HTTP.
    let f = files();
    let zero: Vec<&str> = f
        .iter()
        .filter(|x| x.size_bytes == 0)
        .map(|x| &x.name[4..24])
        .collect();
    assert_eq!(zero.len(), 4, "{zero:?}");
    for stem in ["0013_D", "0004_D", "0003_D", "0002_D"] {
        assert_eq!(find(&f, stem).size_bytes, 0, "{stem}");
    }
}
