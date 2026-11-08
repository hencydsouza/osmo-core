//! Parses real MP4 fixtures end-to-end through `read_movie`.
use osmo_core::mp4::{read_movie, FileSource, Mp4Error};
use std::path::Path;
use std::process::Command;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// `ffprobe -count_packets`'s packet count for one stream, or `None` if ffprobe is not
/// installed (the test then relies on the hard-coded count alone).
fn ffprobe_packet_count(path: &Path, stream_index: usize) -> Option<u32> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            &format!("{stream_index}"),
            "-count_packets",
            "-show_entries",
            "stream=nb_read_packets",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn assert_shape(name: &str, expected_video: u32, expected_audio: u32) {
    let path = fixture(name);
    let mut src = FileSource::open(&path).expect("open fixture");
    let movie = read_movie(&mut src).expect("read_movie");

    let video: Vec<_> = movie
        .tracks
        .iter()
        .filter(|t| &t.handler == b"vide")
        .collect();
    let audio: Vec<_> = movie
        .tracks
        .iter()
        .filter(|t| &t.handler == b"soun")
        .collect();
    assert_eq!(video.len(), 1, "{name}: expected exactly one video track");
    assert_eq!(audio.len(), 1, "{name}: expected exactly one audio track");

    assert_eq!(
        video[0].samples.len() as u32,
        expected_video,
        "{name}: video sample count"
    );
    assert_eq!(
        audio[0].samples.len() as u32,
        expected_audio,
        "{name}: audio sample count"
    );
    if let Some(n) = ffprobe_packet_count(&path, 0) {
        assert_eq!(
            video[0].samples.len() as u32,
            n,
            "{name}: video sample count disagrees with ffprobe"
        );
    }
    if let Some(n) = ffprobe_packet_count(&path, 1) {
        assert_eq!(
            audio[0].samples.len() as u32,
            n,
            "{name}: audio sample count disagrees with ffprobe"
        );
    }

    assert!(
        video[0].samples[0].sync,
        "{name}: the first video sample must be a sync sample"
    );
}

#[test]
fn h264_aac_parses_with_the_expected_shape() {
    assert_shape("h264_aac.mp4", 50, 88);
}

#[test]
fn hevc_aac_parses_with_the_expected_shape() {
    assert_shape("hevc_aac.mp4", 50, 88);
}

#[test]
fn faststart_parses_with_moov_before_mdat() {
    let path = fixture("faststart.mp4");
    let bytes = std::fs::read(&path).unwrap();
    // moov (ftyp+moov header say 'moov') must appear before mdat in the raw bytes, confirming
    // this fixture actually exercises the "moov first" layout the parser must also handle.
    let moov_pos = bytes.windows(4).position(|w| w == b"moov").unwrap();
    let mdat_pos = bytes.windows(4).position(|w| w == b"mdat").unwrap();
    assert!(moov_pos < mdat_pos, "fixture must have moov before mdat");

    assert_shape("faststart.mp4", 50, 88);
}

#[test]
fn fragmented_mp4_is_rejected_not_misparsed() {
    // A minimal fragmented-MP4 marker: ftyp + moov (empty, no trak) + moof. read_movie must see
    // the moof and refuse rather than silently return a movie with zero tracks.
    let mut bytes = Vec::new();
    bytes.extend(24u32.to_be_bytes());
    bytes.extend(b"ftyp");
    bytes.extend(b"isom");
    bytes.extend(0u32.to_be_bytes());
    bytes.extend(b"isom");
    bytes.extend(b"mp42");
    bytes.extend(8u32.to_be_bytes());
    bytes.extend(b"moov");
    bytes.extend(16u32.to_be_bytes());
    bytes.extend(b"moof");
    bytes.extend(0u32.to_be_bytes());
    bytes.extend(0u32.to_be_bytes());

    let mut src = osmo_core::mp4::MemSource::new(bytes);
    let err = read_movie(&mut src).unwrap_err();
    assert!(
        matches!(err, Mp4Error::Unsupported(_)),
        "expected Unsupported, got {err:?}"
    );
}

/// Live hardware evidence, not a fixture test: when `OSMO_REAL_MP4` points at a real DJI proxy
/// file, parses it and checks the shape this repo has actually observed on the Action 4 (5
/// tracks: video, audio, djmd/meta, tmcd, and a cover image that must not abort parsing).
// Ruling (Task 2): the real proxy has exactly 4 ISO BMFF `trak` boxes (video, audio, djmd/meta,
// tmcd), confirmed directly by walking moov's children. The "5 tracks" in the plan came from
// ffprobe, which synthesizes a 5th pseudo-stream for an embedded cover thumbnail that lives in
// moov's udta/meta, never as a trak with its own sample table — there is nothing at the trak
// level to parse or abort on for it. Cost if wrong: a DJI model that does embed a real cover
// trak would need this assertion loosened back to "at least 4".
#[test]
fn real_dji_proxy_parses() {
    let Ok(path) = std::env::var("OSMO_REAL_MP4") else {
        return;
    };
    let mut src = FileSource::open(Path::new(&path)).expect("open OSMO_REAL_MP4");
    let movie = read_movie(&mut src).expect("read_movie on a real DJI proxy");
    assert_eq!(
        movie.tracks.len(),
        4,
        "expected 4 real trak boxes on the real proxy"
    );
    let handlers: Vec<String> = movie
        .tracks
        .iter()
        .map(|t| String::from_utf8_lossy(&t.handler).to_string())
        .collect();
    for want in ["vide", "soun", "meta", "tmcd"] {
        assert!(
            handlers.contains(&want.to_string()),
            "missing handler {want} among {handlers:?}"
        );
    }
}
