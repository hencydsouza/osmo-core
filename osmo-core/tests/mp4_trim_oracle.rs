//! Checks `mp4::trim` against the source bytes and against ffmpeg/ffprobe, which are used
//! only as oracles (the SDK never links or ships them).
use osmo_core::mp4::boxes::{walk_mem, walk_top_level, MemBox};
use osmo_core::mp4::plan::plan_trim;
use osmo_core::mp4::{read_movie, trim, FileSource, Mp4Error, RangeSource};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

/// The synthetic fixtures, plus the real DJI proxy when `$OSMO_REAL_MP4` points at one.
fn sources() -> Vec<PathBuf> {
    let mut v = vec![fixture("h264_aac.mp4"), fixture("hevc_aac.mp4"), fixture("faststart.mp4")];
    if let Ok(p) = std::env::var("OSMO_REAL_MP4") {
        v.push(PathBuf::from(p));
    }
    v
}

fn have(tool: &str) -> bool {
    Command::new(tool).arg("-version").output().is_ok()
}

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("osmo-trim-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn no_cancel() -> bool {
    false
}

/// A cut inside the clip: from a quarter of the way in to three quarters.
fn window(path: &Path) -> (f64, f64) {
    let m = read_movie(&mut FileSource::open(path).unwrap()).unwrap();
    let v = m.tracks.iter().find(|t| &t.handler == b"vide").unwrap();
    let len = v.duration as f64 / f64::from(v.timescale);
    (len * 0.25, len * 0.75)
}

fn run_trim(src: &Path, dest: &Path, from: f64, to: f64) -> osmo_core::mp4::TrimOutcome {
    trim(&mut FileSource::open(src).unwrap(), dest, from, to, &no_cancel, &mut |_, _| {}).unwrap()
}

fn sample_bytes(src: &mut FileSource, s: &osmo_core::mp4::Sample) -> Vec<u8> {
    src.read_at(s.offset, s.size as usize).unwrap()
}

/// Every output sample must equal, byte for byte, the source sample the plan selected.
fn assert_samples_identical(src_path: &Path, out_path: &Path, from: f64, to: f64) {
    let mut src = FileSource::open(src_path).unwrap();
    let src_movie = read_movie(&mut src).unwrap();
    let plan = plan_trim(&src_movie, from, to).unwrap();
    let mut out = FileSource::open(out_path).unwrap();
    let out_movie = read_movie(&mut out).unwrap();
    assert_eq!(out_movie.tracks.len(), plan.cuts.len(), "track count");
    for (cut, out_track) in plan.cuts.iter().zip(&out_movie.tracks) {
        let src_track = &src_movie.tracks[cut.track];
        assert_eq!(out_track.handler, src_track.handler);
        assert_eq!(out_track.samples.len(), cut.end - cut.first, "{:?} sample count", src_track.handler);
        for (i, o) in out_track.samples.iter().enumerate() {
            let s = &src_track.samples[cut.first + i];
            assert_eq!(o.size, s.size);
            assert_eq!(o.sync, s.sync, "sync flag at {i}");
            assert_eq!(sample_bytes(&mut out, o), sample_bytes(&mut src, s), "bytes at {i}");
        }
    }
}

#[test]
fn samples_are_byte_identical_to_the_source() {
    for src in sources() {
        let (from, to) = window(&src);
        let dir = tmp("ident");
        let dest = dir.join("out.mp4");
        run_trim(&src, &dest, from, to);
        assert_samples_identical(&src, &dest, from, to);
    }
}

fn ffprobe_streams(path: &Path) -> Vec<(String, u32)> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-count_packets", "-show_entries", "stream=index,codec_name,nb_read_packets", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let f: Vec<&str> = l.trim().split(',').collect();
            (f[1].to_string(), f.last().unwrap().parse().unwrap_or(0))
        })
        .collect()
}

fn ffprobe_duration(path: &Path) -> f64 {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

#[test]
fn ffprobe_agrees() {
    if !have("ffprobe") {
        eprintln!("skipped: no ffprobe");
        return;
    }
    for src in sources() {
        let (from, to) = window(&src);
        let dest = tmp("probe").join("out.mp4");
        let outcome = run_trim(&src, &dest, from, to);
        let movie = read_movie(&mut FileSource::open(&src).unwrap()).unwrap();
        let plan = plan_trim(&movie, from, to).unwrap();
        let probed = ffprobe_streams(&dest);
        let original = ffprobe_streams(&src);
        assert_eq!(probed.len(), plan.cuts.len(), "stream count");
        for (i, cut) in plan.cuts.iter().enumerate() {
            assert_eq!(probed[i].0, original[cut.track].0, "codec of stream {i}");
            assert_eq!(probed[i].1 as usize, cut.end - cut.first, "packets of stream {i}");
        }
        let v = movie.tracks.iter().find(|t| &t.handler == b"vide").unwrap();
        let frame = f64::from(v.samples[0].duration) / f64::from(v.timescale);
        let d = ffprobe_duration(&dest);
        assert!((d - outcome.duration_secs).abs() <= frame * 1.5, "duration {d} vs {}", outcome.duration_secs);
    }
}

#[test]
fn ffmpeg_decodes_without_errors() {
    if !have("ffmpeg") {
        eprintln!("skipped: no ffmpeg");
        return;
    }
    for src in sources() {
        let (from, to) = window(&src);
        let dest = tmp("decode").join("out.mp4");
        run_trim(&src, &dest, from, to);
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&dest)
            .args(["-map", "0:v", "-f", "null", "-"])
            .output()
            .unwrap();
        assert!(out.stderr.is_empty(), "{}: {}", src.display(), String::from_utf8_lossy(&out.stderr));
    }
}

fn first_video_pts_and_count(path: &Path) -> (f64, u32) {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts_time", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    let pts: Vec<f64> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().trim_end_matches(',').parse().ok())
        .collect();
    (pts.iter().cloned().fold(f64::INFINITY, f64::min), pts.len() as u32)
}

#[test]
fn matches_ffmpeg_copy_cut() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skipped: no ffmpeg/ffprobe");
        return;
    }
    for src in sources() {
        let (from, to) = window(&src);
        let dir = tmp("oracle");
        let dest = dir.join("ours.mp4");
        let outcome = run_trim(&src, &dest, from, to);
        let theirs = dir.join("theirs.mp4");
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-ss", &from.to_string(), "-i"])
            .arg(&src)
            // ffmpeg measures `-t` from the requested `-ss`, so the absolute end is `to`.
            .args(["-t", &(to - from).to_string(), "-c", "copy", "-map", "0:v"])
            .arg(&theirs)
            .status()
            .unwrap();
        assert!(ok.success());
        let movie = read_movie(&mut FileSource::open(&src).unwrap()).unwrap();
        let v = movie.tracks.iter().find(|t| &t.handler == b"vide").unwrap();
        let frame = f64::from(v.samples[0].duration) / f64::from(v.timescale);
        let (our_pts, our_n) = first_video_pts_and_count(&dest);
        let (their_pts, their_n) = first_video_pts_and_count(&theirs);
        // ffmpeg puts the requested `-ss` at 0, so its keyframe sits at `actual_start - from`;
        // ours starts at 0 by design (edit list), which is the same cut shifted by that gap.
        assert!(our_pts.abs() <= frame * 1.5, "{}: our first pts {our_pts}", src.display());
        let gap = outcome.actual_start_secs - from;
        assert!((their_pts - gap).abs() <= frame * 1.5, "{}: ffmpeg first pts {their_pts} vs gap {gap}", src.display());
        // ffmpeg's `-c copy` keeps B-frame pre-roll packets (negative pts, flagged discard) and
        // cuts the tail by shifted dts, so it runs a few packets over; the exact count is pinned
        // against our own plan in `ffprobe_agrees`.
        assert!(our_n.abs_diff(their_n) <= 5, "{}: packets {our_n} vs {their_n}", src.display());
        assert!(outcome.actual_start_secs <= from + 1e-6);
    }
}

#[test]
fn from_equal_to_zero_to_end_is_a_lossless_copy_of_all_samples() {
    for src in sources() {
        let dest = tmp("full").join("out.mp4");
        let outcome = run_trim(&src, &dest, 0.0, 1.0e9);
        assert_eq!(outcome.actual_start_secs, 0.0);
        assert_samples_identical(&src, &dest, 0.0, 1.0e9);
        let a = read_movie(&mut FileSource::open(&src).unwrap()).unwrap();
        let b = read_movie(&mut FileSource::open(&dest).unwrap()).unwrap();
        for (x, y) in a.tracks.iter().zip(&b.tracks) {
            assert_eq!(x.samples.len(), y.samples.len(), "{:?}", x.handler);
        }
    }
}

/// Fails every read after the first `after`.
struct Flaky {
    inner: FileSource,
    after: usize,
    n: usize,
}

impl RangeSource for Flaky {
    fn len(&mut self) -> Result<u64, Mp4Error> {
        self.inner.len()
    }
    fn read_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, Mp4Error> {
        self.n += 1;
        if self.n > self.after {
            return Err(Mp4Error::Io("injected failure".into()));
        }
        self.inner.read_at(offset, len)
    }
}

fn leftovers(dest: &Path) -> bool {
    let part = PathBuf::from(format!("{}.part", dest.display()));
    dest.exists() || part.exists()
}

#[test]
fn cancel_and_io_error_leave_no_file() {
    let src = fixture("h264_aac.mp4");
    let (from, to) = window(&src);
    let dir = tmp("fail");
    let dest = dir.join("out.mp4");
    let mut failures = 0;
    for after in 0..60 {
        let mut flaky = Flaky { inner: FileSource::open(&src).unwrap(), after, n: 0 };
        match trim(&mut flaky, &dest, from, to, &no_cancel, &mut |_, _| {}) {
            Ok(_) => {
                std::fs::remove_file(&dest).unwrap();
            }
            Err(_) => {
                failures += 1;
                assert!(!leftovers(&dest), "files left after io failure at read {after}");
            }
        }
    }
    assert!(failures > 5, "the injected failures never fired");

    let mut cancelled = 0;
    for k in 0..30 {
        let calls = Cell::new(0);
        let cancel = || {
            calls.set(calls.get() + 1);
            calls.get() > k
        };
        match trim(&mut FileSource::open(&src).unwrap(), &dest, from, to, &cancel, &mut |_, _| {}) {
            Ok(_) => {
                std::fs::remove_file(&dest).unwrap();
            }
            Err(e) => {
                assert!(matches!(e, Mp4Error::Cancelled), "{e:?}");
                cancelled += 1;
                assert!(!leftovers(&dest), "files left after cancel at call {k}");
            }
        }
    }
    assert!(cancelled > 0, "cancel never fired");
}

#[test]
fn existing_dest_is_not_overwritten_on_failure() {
    let src = fixture("h264_aac.mp4");
    let (from, to) = window(&src);
    let dest = tmp("keep").join("out.mp4");
    std::fs::write(&dest, b"precious").unwrap();
    let r = trim(&mut FileSource::open(&src).unwrap(), &dest, from, to, &|| true, &mut |_, _| {});
    assert!(matches!(r, Err(Mp4Error::Cancelled)));
    assert_eq!(std::fs::read(&dest).unwrap(), b"precious");
    assert!(!PathBuf::from(format!("{}.part", dest.display())).exists());
}

// ---- co64: a source whose media data sits above 4 GiB ---------------------------------------

const HOLE: u64 = 5 << 30;

fn wrap(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(kind);
    v.extend_from_slice(body);
    v
}

/// Copies `b`, turning every `stco` into a `co64` whose offsets are moved to `HOLE`.
fn rewrite(buf: &[u8], b: &MemBox, mdat_body: u64) -> Vec<u8> {
    match &b.kind {
        b"stco" => {
            let p = b.payload(buf);
            let n = u32::from_be_bytes(p[4..8].try_into().unwrap()) as usize;
            let mut body = vec![0u8; 4];
            body.extend_from_slice(&(n as u32).to_be_bytes());
            for i in 0..n {
                let off = u64::from(u32::from_be_bytes(p[8 + 4 * i..12 + 4 * i].try_into().unwrap()));
                body.extend_from_slice(&(HOLE + off - mdat_body).to_be_bytes());
            }
            wrap(b"co64", &body)
        }
        b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" => {
            let (s, l) = b.payload_bounds(buf);
            let body: Vec<u8> = walk_mem(buf, s, l)
                .unwrap()
                .iter()
                .flat_map(|c| rewrite(buf, c, mdat_body))
                .collect();
            wrap(&b.kind, &body)
        }
        _ => b.whole(buf).to_vec(),
    }
}

struct Virtual {
    prefix: Vec<u8>,
    data: Vec<u8>,
}

impl RangeSource for Virtual {
    fn len(&mut self) -> Result<u64, Mp4Error> {
        Ok(HOLE + self.data.len() as u64)
    }
    fn read_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, Mp4Error> {
        let mut out = vec![0u8; len];
        let end = offset + len as u64;
        if offset < self.prefix.len() as u64 {
            let e = end.min(self.prefix.len() as u64);
            out[..(e - offset) as usize].copy_from_slice(&self.prefix[offset as usize..e as usize]);
        }
        if end > HOLE && offset < HOLE + self.data.len() as u64 {
            let s = offset.max(HOLE);
            let e = end.min(HOLE + self.data.len() as u64);
            out[(s - offset) as usize..(e - offset) as usize]
                .copy_from_slice(&self.data[(s - HOLE) as usize..(e - HOLE) as usize]);
        }
        Ok(out)
    }
}

#[test]
fn co64_source_trims_correctly() {
    let path = fixture("h264_aac.mp4");
    let bytes = std::fs::read(&path).unwrap();
    let top = walk_top_level(&mut FileSource::open(&path).unwrap()).unwrap();
    let mdat = top.iter().find(|b| &b.kind == b"mdat").unwrap();
    let moov = top.iter().find(|b| &b.kind == b"moov").unwrap();
    let ftyp = top.iter().find(|b| &b.kind == b"ftyp").unwrap();

    let moov_start = (moov.body_start - 8) as usize;
    let moov_buf = &bytes[moov_start..(moov.body_start + moov.body_len) as usize];
    let moov_box = walk_mem(moov_buf, 0, moov_buf.len()).unwrap().remove(0);
    let mut prefix = wrap(b"ftyp", &bytes[ftyp.body_start as usize..(ftyp.body_start + ftyp.body_len) as usize]);
    prefix.extend(rewrite(moov_buf, &moov_box, mdat.body_start));
    let data = bytes[mdat.body_start as usize..(mdat.body_start + mdat.body_len) as usize].to_vec();
    let mdat_box_len = HOLE + data.len() as u64 - prefix.len() as u64;
    prefix.extend_from_slice(&1u32.to_be_bytes());
    prefix.extend_from_slice(b"mdat");
    prefix.extend_from_slice(&mdat_box_len.to_be_bytes());

    let (from, to) = window(&path);
    let dir = tmp("co64");
    let big = dir.join("big.mp4");
    let mut v = Virtual { prefix, data };
    let a = trim(&mut v, &big, from, to, &no_cancel, &mut |_, _| {}).unwrap();
    let small = dir.join("small.mp4");
    let b = run_trim(&path, &small, from, to);
    assert_eq!(a.duration_secs, b.duration_secs);
    assert_eq!(std::fs::read(&big).unwrap(), std::fs::read(&small).unwrap());
}
