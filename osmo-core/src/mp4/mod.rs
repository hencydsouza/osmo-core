//! Pure-Rust MP4 (ISO/IEC 14496-12) reader over a range-addressable source, for trimming DJI
//! clips without a re-encode. No ffmpeg, no process spawn.
use thiserror::Error;

pub mod boxes;
pub mod plan;
mod write;
pub use plan::{plan_trim, TrackCut, TrimPlan};
pub use write::{trim, TrimOutcome};
pub mod tables;

use boxes::{find, walk_mem, walk_top_level, MemBox};

#[derive(Debug, Error)]
pub enum Mp4Error {
    #[error("I/O error: {0}")]
    Io(String),
    #[error("not an MP4 file")]
    NotMp4,
    #[error("the file is truncated or a box's declared size runs past the end")]
    Truncated,
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    #[error("bad time range: {0}")]
    BadRange(String),
    #[error("cancelled")]
    Cancelled,
}

/// A source `read_movie` and `trim` can fetch byte ranges from, without assuming the whole file
/// fits in memory. Implemented over a local file (`FileSource`) and, by the SDK, over HTTP.
#[allow(clippy::len_without_is_empty)] // `len` here is fallible and asks the source for its
// size; an `is_empty` beside it would add nothing the Interfaces block doesn't already cover.
pub trait RangeSource {
    fn len(&mut self) -> Result<u64, Mp4Error>;
    fn read_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, Mp4Error>;
}

pub struct FileSource {
    file: std::fs::File,
}

impl FileSource {
    pub fn open(path: &std::path::Path) -> Result<Self, Mp4Error> {
        Ok(Self {
            file: std::fs::File::open(path).map_err(|e| Mp4Error::Io(e.to_string()))?,
        })
    }
}

impl RangeSource for FileSource {
    fn len(&mut self) -> Result<u64, Mp4Error> {
        self.file
            .metadata()
            .map(|m| m.len())
            .map_err(|e| Mp4Error::Io(e.to_string()))
    }

    fn read_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, Mp4Error> {
        use std::io::{Read, Seek, SeekFrom};
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|e| Mp4Error::Io(e.to_string()))?;
        let mut buf = vec![0u8; len];
        self.file
            .read_exact(&mut buf)
            .map_err(|e| Mp4Error::Io(e.to_string()))?;
        Ok(buf)
    }
}

/// An in-memory source, used by tests and by callers that already hold the bytes.
pub struct MemSource {
    bytes: Vec<u8>,
}

impl MemSource {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }
}

impl RangeSource for MemSource {
    fn len(&mut self) -> Result<u64, Mp4Error> {
        Ok(self.bytes.len() as u64)
    }

    fn read_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, Mp4Error> {
        let start = offset as usize;
        let end = start.checked_add(len).ok_or(Mp4Error::Truncated)?;
        self.bytes
            .get(start..end)
            .map(|s| s.to_vec())
            .ok_or(Mp4Error::Truncated)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    pub offset: u64,
    pub size: u32,
    pub dts: u64,
    pub duration: u32,
    pub cts_offset: i32,
    pub sync: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub id: u32,
    pub handler: [u8; 4],
    pub timescale: u32,
    pub duration: u64,
    pub samples: Vec<Sample>,
    pub raw_stsd: Vec<u8>,
    pub raw_hdlr: Vec<u8>,
    pub raw_dinf: Vec<u8>,
    pub raw_media_header: Vec<u8>,
    pub raw_tkhd: Vec<u8>,
    pub raw_tref: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Movie {
    pub movie_timescale: u32,
    pub tracks: Vec<Track>,
    pub ftyp: Vec<u8>,
}

fn version(payload: &[u8]) -> Result<u8, Mp4Error> {
    payload.first().copied().ok_or(Mp4Error::Truncated)
}

fn u32_be(buf: &[u8], off: usize) -> Result<u32, Mp4Error> {
    buf.get(off..off + 4)
        .map(|s| u32::from_be_bytes(s.try_into().unwrap()))
        .ok_or(Mp4Error::Truncated)
}

fn u64_be(buf: &[u8], off: usize) -> Result<u64, Mp4Error> {
    buf.get(off..off + 8)
        .map(|s| u64::from_be_bytes(s.try_into().unwrap()))
        .ok_or(Mp4Error::Truncated)
}

/// `mvhd`/`mdhd` share this (version, timescale, duration) layout: a 1-byte version, 3 bytes of
/// flags, then either 32-bit or 64-bit creation/modification/duration depending on version, with
/// timescale always a plain `u32` between them.
fn timescale_and_duration(payload: &[u8]) -> Result<(u32, u64), Mp4Error> {
    let v = version(payload)?;
    if v == 1 {
        let timescale = u32_be(payload, 20)?;
        let duration = u64_be(payload, 28)?;
        Ok((timescale, duration))
    } else {
        let timescale = u32_be(payload, 12)?;
        let duration = u32_be(payload, 16)? as u64;
        Ok((timescale, duration))
    }
}

fn track_id(tkhd_payload: &[u8]) -> Result<u32, Mp4Error> {
    let v = version(tkhd_payload)?;
    if v == 1 {
        u32_be(tkhd_payload, 20)
    } else {
        u32_be(tkhd_payload, 12)
    }
}

fn handler_type(hdlr_payload: &[u8]) -> Result<[u8; 4], Mp4Error> {
    hdlr_payload
        .get(8..12)
        .map(|s| s.try_into().unwrap())
        .ok_or(Mp4Error::Truncated)
}

/// Builds one track's samples from its `stbl` children. Missing sample tables (seen on DJI cover
/// tracks) produce an empty sample list rather than an error, so an unusual track never aborts
/// the whole parse.
fn samples_from_stbl(buf: &[u8], stbl: &MemBox) -> Result<Vec<Sample>, Mp4Error> {
    let (p_start, p_len) = stbl.payload_bounds(buf);
    let children = walk_mem(buf, p_start, p_len)?;
    let (Some(stts), Some(stsz), Some(stsc)) = (
        find(&children, b"stts"),
        find(&children, b"stsz"),
        find(&children, b"stsc"),
    ) else {
        return Ok(Vec::new());
    };
    let chunk_offsets = if let Some(co64) = find(&children, b"co64") {
        tables::decode_co64(co64.payload(buf))?
    } else if let Some(stco) = find(&children, b"stco") {
        tables::decode_stco(stco.payload(buf))?
    } else {
        return Ok(Vec::new());
    };

    let durations = tables::decode_stts(stts.payload(buf))?;
    let sizes = tables::decode_stsz(stsz.payload(buf))?;
    let stsc_entries = tables::decode_stsc(stsc.payload(buf))?;
    let offsets = tables::sample_offsets(&stsc_entries, &chunk_offsets, &sizes)?;
    let cts_offsets = find(&children, b"ctts")
        .map(|b| tables::decode_ctts(b.payload(buf)))
        .transpose()?
        .unwrap_or_else(|| vec![0i32; sizes.len()]);
    let sync: Option<Vec<u32>> = find(&children, b"stss")
        .map(|b| tables::decode_stss(b.payload(buf)))
        .transpose()?;

    if durations.len() != sizes.len() || offsets.len() != sizes.len() {
        return Err(Mp4Error::Truncated);
    }

    Ok((0..sizes.len())
        .map(|i| Sample {
            offset: offsets[i],
            size: sizes[i],
            dts: durations[i].0,
            duration: durations[i].1,
            cts_offset: cts_offsets.get(i).copied().unwrap_or(0),
            sync: sync
                .as_ref()
                .map(|s| s.contains(&(i as u32 + 1)))
                .unwrap_or(true),
        })
        .collect())
}

fn read_track(buf: &[u8], trak: &MemBox) -> Result<Option<Track>, Mp4Error> {
    let (p_start, p_len) = trak.payload_bounds(buf);
    let trak_children = walk_mem(buf, p_start, p_len)?;
    let Some(tkhd) = find(&trak_children, b"tkhd") else {
        return Ok(None);
    };
    let Some(mdia) = find(&trak_children, b"mdia") else {
        return Ok(None);
    };
    let tref = find(&trak_children, b"tref")
        .map(|b| b.whole(buf).to_vec())
        .unwrap_or_default();

    let (p_start, p_len) = mdia.payload_bounds(buf);
    let mdia_children = walk_mem(buf, p_start, p_len)?;
    let (Some(mdhd), Some(hdlr), Some(minf)) = (
        find(&mdia_children, b"mdhd"),
        find(&mdia_children, b"hdlr"),
        find(&mdia_children, b"minf"),
    ) else {
        return Ok(None);
    };
    let (timescale, duration) = timescale_and_duration(mdhd.payload(buf))?;
    let handler = handler_type(hdlr.payload(buf))?;

    let (p_start, p_len) = minf.payload_bounds(buf);
    let minf_children = walk_mem(buf, p_start, p_len)?;
    let media_header = find(&minf_children, b"vmhd")
        .or_else(|| find(&minf_children, b"smhd"))
        .map(|b| b.whole(buf).to_vec())
        .unwrap_or_default();
    let dinf = find(&minf_children, b"dinf")
        .map(|b| b.whole(buf).to_vec())
        .unwrap_or_default();
    let Some(stbl) = find(&minf_children, b"stbl") else {
        return Ok(None);
    };
    let (p_start, p_len) = stbl.payload_bounds(buf);
    let stbl_children = walk_mem(buf, p_start, p_len)?;
    let stsd = find(&stbl_children, b"stsd")
        .map(|b| b.whole(buf).to_vec())
        .unwrap_or_default();

    Ok(Some(Track {
        id: track_id(tkhd.payload(buf))?,
        handler,
        timescale,
        duration,
        samples: samples_from_stbl(buf, stbl)?,
        raw_stsd: stsd,
        raw_hdlr: hdlr.whole(buf).to_vec(),
        raw_dinf: dinf,
        raw_media_header: media_header,
        raw_tkhd: tkhd.whole(buf).to_vec(),
        raw_tref: tref,
    }))
}

/// Parses an MP4's metadata (`moov`): every track's sample table, decoded into one `Sample` per
/// sample. Never reads `mdat` itself — sample bytes are fetched later, by `trim`. `moov` may come
/// before or after `mdat` in the file.
pub fn read_movie(src: &mut dyn RangeSource) -> Result<Movie, Mp4Error> {
    let top = walk_top_level(src)?;
    if top.iter().any(|b| &b.kind == b"moof") {
        return Err(Mp4Error::Unsupported("fragmented mp4"));
    }
    if top.is_empty() {
        return Err(Mp4Error::NotMp4);
    }
    let ftyp = match top.iter().find(|b| &b.kind == b"ftyp") {
        Some(b) => src.read_at(b.body_start, b.body_len as usize)?,
        None => Vec::new(),
    };
    let Some(moov_hdr) = top.iter().find(|b| &b.kind == b"moov") else {
        return Err(Mp4Error::Unsupported("missing moov box"));
    };
    let moov_bytes = src.read_at(moov_hdr.body_start, moov_hdr.body_len as usize)?;
    let moov_children = walk_mem(&moov_bytes, 0, moov_bytes.len())?;
    let Some(mvhd) = find(&moov_children, b"mvhd") else {
        return Err(Mp4Error::Unsupported("missing mvhd box"));
    };
    let (movie_timescale, _movie_duration) = timescale_and_duration(mvhd.payload(&moov_bytes))?;

    let mut tracks = Vec::new();
    for trak in moov_children.iter().filter(|b| &b.kind == b"trak") {
        if let Some(t) = read_track(&moov_bytes, trak)? {
            tracks.push(t);
        }
    }

    Ok(Movie {
        movie_timescale,
        tracks,
        ftyp,
    })
}
