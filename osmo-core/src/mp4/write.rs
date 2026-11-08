//! Stream-copy trim writer: keeps the planned samples byte for byte and rebuilds the index.
//!
//! Layout written: `ftyp`, `free`, `mdat` (chunks interleaved by time), `moov` last, so the
//! sample offsets are known before any media is copied and `moov` is built in one pass.

use super::plan::{plan_trim, TrimPlan};
use super::{read_movie, Mp4Error, RangeSource, Sample, Track};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Largest single read issued against the source.
const MAX_READ: usize = 4 << 20;
/// Samples per chunk. Small enough to interleave tracks finely, large enough to keep the
/// index compact.
const CHUNK_SAMPLES: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub struct TrimOutcome {
    pub actual_start_secs: f64,
    pub duration_secs: f64,
    pub bytes: u64,
}

fn io(e: std::io::Error) -> Mp4Error {
    Mp4Error::Io(e.to_string())
}

fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(kind);
    v.extend_from_slice(body);
    v
}

/// A box that starts with a version/flags word.
fn full(kind: &[u8; 4], version_flags: u32, body: &[u8]) -> Vec<u8> {
    let mut b = version_flags.to_be_bytes().to_vec();
    b.extend_from_slice(body);
    bx(kind, &b)
}

fn u32s(vals: impl IntoIterator<Item = u32>) -> Vec<u8> {
    vals.into_iter().flat_map(u32::to_be_bytes).collect()
}

/// Run-length `(count, value)` pairs.
fn runs<T: PartialEq + Copy>(vals: impl Iterator<Item = T>) -> Vec<(u32, T)> {
    let mut out: Vec<(u32, T)> = Vec::new();
    for v in vals {
        match out.last_mut() {
            Some((n, last)) if *last == v => *n += 1,
            _ => out.push((1, v)),
        }
    }
    out
}

/// One output track: the surviving samples and where each chunk lands in the new `mdat`.
struct Out<'a> {
    track: &'a Track,
    samples: Vec<Sample>,
    /// Sample-index ranges (within `samples`), one per chunk.
    chunks: Vec<(usize, usize)>,
    offsets: Vec<u64>,
}

fn chunk_size(o: &Out, c: usize) -> u64 {
    let (a, b) = o.chunks[c];
    o.samples[a..b].iter().map(|s| u64::from(s.size)).sum()
}

fn media_duration(o: &Out) -> u64 {
    o.samples.iter().map(|s| u64::from(s.duration)).sum()
}

fn patch_tkhd(raw: &[u8], duration: u64) -> Vec<u8> {
    let mut t = raw.to_vec();
    match t.get(8) {
        Some(1) if t.len() >= 44 => t[36..44].copy_from_slice(&duration.to_be_bytes()),
        Some(0) if t.len() >= 32 => t[28..32].copy_from_slice(&(duration as u32).to_be_bytes()),
        _ => {}
    }
    t
}

fn stbl(o: &Out) -> Vec<u8> {
    let mut body = o.track.raw_stsd.clone();

    let stts: Vec<u32> = runs(o.samples.iter().map(|s| s.duration))
        .into_iter()
        .flat_map(|(n, d)| [n, d])
        .collect();
    body.extend(full(b"stts", 0, &[&(stts.len() as u32 / 2).to_be_bytes()[..], &u32s(stts)].concat()));

    if o.samples.iter().any(|s| s.cts_offset != 0) {
        let version = u32::from(o.samples.iter().any(|s| s.cts_offset < 0)) << 24;
        let ctts: Vec<u32> = runs(o.samples.iter().map(|s| s.cts_offset))
            .into_iter()
            .flat_map(|(n, d)| [n, d as u32])
            .collect();
        body.extend(full(b"ctts", version, &[&(ctts.len() as u32 / 2).to_be_bytes()[..], &u32s(ctts)].concat()));
    }

    if o.samples.iter().any(|s| !s.sync) {
        let sync: Vec<u32> = o
            .samples
            .iter()
            .enumerate()
            .filter(|(_, s)| s.sync)
            .map(|(i, _)| i as u32 + 1)
            .collect();
        body.extend(full(b"stss", 0, &[&(sync.len() as u32).to_be_bytes()[..], &u32s(sync)].concat()));
    }

    let sizes = u32s(o.samples.iter().map(|s| s.size));
    body.extend(full(
        b"stsz",
        0,
        &[&0u32.to_be_bytes()[..], &(o.samples.len() as u32).to_be_bytes(), &sizes].concat(),
    ));

    let mut entries = Vec::new();
    let mut first = 1u32;
    for (n, spc) in runs(o.chunks.iter().map(|&(a, b)| (b - a) as u32)) {
        entries.extend([first, spc, 1]);
        first += n;
    }
    body.extend(full(b"stsc", 0, &[&(entries.len() as u32 / 3).to_be_bytes()[..], &u32s(entries)].concat()));

    // `co64` only when some chunk starts beyond what 32 bits can address.
    if o.offsets.iter().any(|&x| x > u64::from(u32::MAX)) {
        let offs: Vec<u8> = o.offsets.iter().flat_map(|x| x.to_be_bytes()).collect();
        body.extend(full(b"co64", 0, &[&(o.offsets.len() as u32).to_be_bytes()[..], &offs].concat()));
    } else {
        let offs = u32s(o.offsets.iter().map(|&x| x as u32));
        body.extend(full(b"stco", 0, &[&(o.offsets.len() as u32).to_be_bytes()[..], &offs].concat()));
    }
    bx(b"stbl", &body)
}

fn trak(o: &Out, movie_timescale: u32) -> (Vec<u8>, u64) {
    let media = media_duration(o);
    let in_movie = (media as f64 / f64::from(o.track.timescale.max(1)) * f64::from(movie_timescale)).round() as u64;

    let mut m = vec![0u8; 8]; // creation, modification
    m.extend(o.track.timescale.to_be_bytes());
    m.extend((media as u32).to_be_bytes());
    m.extend(0x55c4u16.to_be_bytes()); // language "und"
    m.extend(0u16.to_be_bytes());
    let mdhd = full(b"mdhd", 0, &m);

    let mut minf = o.track.raw_media_header.clone();
    minf.extend_from_slice(&o.track.raw_dinf);
    minf.extend(stbl(o));
    let mut mdia = mdhd;
    mdia.extend_from_slice(&o.track.raw_hdlr);
    mdia.extend(bx(b"minf", &minf));

    let mut t = patch_tkhd(&o.track.raw_tkhd, in_movie);
    t.extend_from_slice(&o.track.raw_tref);
    if &o.track.handler == b"vide" {
        // Start playback at the first kept frame's presentation time so the clip begins at 0.
        let media_time = o.samples.first().map_or(0, |s| s.cts_offset);
        let mut e = 1u32.to_be_bytes().to_vec();
        e.extend((in_movie as u32).to_be_bytes());
        e.extend(media_time.to_be_bytes());
        e.extend(1u16.to_be_bytes());
        e.extend(0u16.to_be_bytes());
        t.extend(bx(b"edts", &full(b"elst", 0, &e)));
    }
    t.extend(bx(b"mdia", &mdia));
    (bx(b"trak", &t), in_movie)
}

fn mvhd(timescale: u32, duration: u64, next_track: u32) -> Vec<u8> {
    let mut b = vec![0u8; 8]; // creation, modification
    b.extend(timescale.to_be_bytes());
    b.extend((duration as u32).to_be_bytes());
    b.extend(0x0001_0000u32.to_be_bytes()); // rate 1.0
    b.extend(0x0100u16.to_be_bytes()); // volume 1.0
    b.extend([0u8; 10]);
    for m in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
        b.extend(m.to_be_bytes());
    }
    b.extend([0u8; 24]);
    b.extend(next_track.to_be_bytes());
    full(b"mvhd", 0, &b)
}

/// Copies `[offset, offset + len)` of the source into `out`, reading at most `MAX_READ` at once.
#[allow(clippy::too_many_arguments)] // threads one copy loop's state; a struct would only rename them
fn copy_range(
    src: &mut dyn RangeSource,
    out: &mut std::fs::File,
    mut offset: u64,
    mut len: u64,
    cancel: &dyn Fn() -> bool,
    done: &mut u64,
    total: u64,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(), Mp4Error> {
    while len > 0 {
        if cancel() {
            return Err(Mp4Error::Cancelled);
        }
        let n = len.min(MAX_READ as u64) as usize;
        out.write_all(&src.read_at(offset, n)?).map_err(io)?;
        offset += n as u64;
        len -= n as u64;
        *done += n as u64;
        progress(*done, total);
    }
    Ok(())
}

fn write_all(
    src: &mut dyn RangeSource,
    part: &Path,
    from_secs: f64,
    to_secs: f64,
    cancel: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<TrimOutcome, Mp4Error> {
    if cancel() {
        return Err(Mp4Error::Cancelled);
    }
    let movie = read_movie(src)?;
    let plan: TrimPlan = plan_trim(&movie, from_secs, to_secs)?;

    let mut outs: Vec<Out> = plan
        .cuts
        .iter()
        .filter(|c| c.end > c.first)
        .map(|c| {
            let track = &movie.tracks[c.track];
            let mut samples = track.samples[c.first..c.end].to_vec();
            if samples.len() == 1 {
                // A lone sample (timecode) spans the whole original clip; shrink it to the cut.
                samples[0].duration = (plan.duration_secs * f64::from(track.timescale)).round() as u32;
            }
            let chunks = (0..samples.len())
                .step_by(CHUNK_SAMPLES)
                .map(|a| (a, (a + CHUNK_SAMPLES).min(samples.len())))
                .collect::<Vec<_>>();
            Out { track, samples, offsets: vec![0; chunks.len()], chunks }
        })
        .collect();

    // Chunks are laid out in time order across tracks so a reader streams them evenly.
    let mut order: Vec<(f64, usize, usize)> = Vec::new();
    for (t, o) in outs.iter().enumerate() {
        for (c, &(a, _)) in o.chunks.iter().enumerate() {
            order.push((o.samples[a].dts as f64 / f64::from(o.track.timescale.max(1)), t, c));
        }
    }
    order.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)).then(x.2.cmp(&y.2)));

    let payload: u64 = outs.iter().flat_map(|o| (0..o.chunks.len()).map(move |c| chunk_size(o, c))).sum();
    let ftyp = if movie.ftyp.is_empty() { Vec::new() } else { bx(b"ftyp", &movie.ftyp) };
    let mdat_header: u64 = if payload + 8 > u64::from(u32::MAX) { 16 } else { 8 };
    let mut pos = ftyp.len() as u64 + 8 + mdat_header;
    for &(_, t, c) in &order {
        outs[t].offsets[c] = pos;
        pos += chunk_size(&outs[t], c);
    }

    let mut file = std::fs::File::create(part).map_err(io)?;
    file.write_all(&ftyp).map_err(io)?;
    file.write_all(&8u32.to_be_bytes()).map_err(io)?;
    file.write_all(b"free").map_err(io)?;
    if mdat_header == 16 {
        file.write_all(&1u32.to_be_bytes()).map_err(io)?;
        file.write_all(b"mdat").map_err(io)?;
        file.write_all(&(payload + 16).to_be_bytes()).map_err(io)?;
    } else {
        file.write_all(&((payload + 8) as u32).to_be_bytes()).map_err(io)?;
        file.write_all(b"mdat").map_err(io)?;
    }

    let mut done = 0u64;
    for &(_, t, c) in &order {
        let o = &outs[t];
        let (a, b) = o.chunks[c];
        // Adjacent samples in the source are fetched as one range.
        let mut cur: Option<(u64, u64)> = None;
        for s in &o.samples[a..b] {
            cur = match cur {
                Some((off, len)) if off + len == s.offset => Some((off, len + u64::from(s.size))),
                Some((off, len)) => {
                    copy_range(src, &mut file, off, len, cancel, &mut done, payload, progress)?;
                    Some((s.offset, u64::from(s.size)))
                }
                None => Some((s.offset, u64::from(s.size))),
            };
        }
        if let Some((off, len)) = cur {
            copy_range(src, &mut file, off, len, cancel, &mut done, payload, progress)?;
        }
    }

    let mut moov = Vec::new();
    let mut movie_duration = 0u64;
    let mut max_id = 0u32;
    let mut traks = Vec::new();
    for o in &outs {
        let (t, d) = trak(o, movie.movie_timescale);
        movie_duration = movie_duration.max(d);
        max_id = max_id.max(o.track.id);
        traks.push(t);
    }
    moov.extend(mvhd(movie.movie_timescale, movie_duration, max_id + 1));
    moov.extend(traks.concat());
    let moov = bx(b"moov", &moov);
    file.write_all(&moov).map_err(io)?;
    file.sync_all().map_err(io)?;

    Ok(TrimOutcome {
        actual_start_secs: plan.actual_start_secs,
        duration_secs: plan.duration_secs,
        bytes: pos + moov.len() as u64,
    })
}

/// Cuts `from_secs..to_secs` out of `src` into `dest` without re-encoding.
///
/// The output is written to `dest` + `.part` and renamed on success; on any error or cancel the
/// partial file is removed and an existing `dest` is left alone.
pub fn trim(
    src: &mut dyn RangeSource,
    dest: &Path,
    from_secs: f64,
    to_secs: f64,
    cancel: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<TrimOutcome, Mp4Error> {
    let mut part: std::ffi::OsString = dest.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let result = write_all(src, &part, from_secs, to_secs, cancel, progress)
        .and_then(|o| std::fs::rename(&part, dest).map(|()| o).map_err(io));
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
}
