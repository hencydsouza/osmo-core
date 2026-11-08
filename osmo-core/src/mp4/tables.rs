//! ISO/IEC 14496-12 sample-table decoders. Each function takes a box's full payload
//! (version/flags byte onward is included where the box has them) and returns a decoded,
//! per-sample view. Pure byte parsing: no I/O, no RangeSource.

use super::Mp4Error;

fn need(buf: &[u8], len: usize) -> Result<(), Mp4Error> {
    if buf.len() < len {
        return Err(Mp4Error::Truncated);
    }
    Ok(())
}

fn u32_at(buf: &[u8], off: usize) -> u32 {
    u32::from_be_bytes(buf[off..off + 4].try_into().unwrap())
}

fn i32_at(buf: &[u8], off: usize) -> i32 {
    i32::from_be_bytes(buf[off..off + 4].try_into().unwrap())
}

fn u64_at(buf: &[u8], off: usize) -> u64 {
    u64::from_be_bytes(buf[off..off + 8].try_into().unwrap())
}

/// `stts`: decoding-time-to-sample. Returns `(dts, duration)` for every sample, in order.
/// `dts` is the cumulative sum of every earlier sample's duration (the first sample is 0).
pub fn decode_stts(payload: &[u8]) -> Result<Vec<(u64, u32)>, Mp4Error> {
    need(payload, 8)?;
    let entry_count = u32_at(payload, 4) as usize;
    let mut out = Vec::new();
    let mut dts: u64 = 0;
    let mut off = 8;
    for _ in 0..entry_count {
        need(payload, off + 8)?;
        let count = u32_at(payload, off);
        let delta = u32_at(payload, off + 4);
        for _ in 0..count {
            out.push((dts, delta));
            dts += delta as u64;
        }
        off += 8;
    }
    Ok(out)
}

/// `ctts`: composition-time-to-sample. Returns a per-sample signed offset. Version 0 stores an
/// unsigned `u32` that every real-world encoder still means as non-negative; version 1 stores a
/// true signed `i32`. Both are read as `i32` here, which is correct for both: a version-0 file
/// never sets the sign bit.
pub fn decode_ctts(payload: &[u8]) -> Result<Vec<i32>, Mp4Error> {
    need(payload, 8)?;
    let entry_count = u32_at(payload, 4) as usize;
    let mut out = Vec::new();
    let mut off = 8;
    for _ in 0..entry_count {
        need(payload, off + 8)?;
        let count = u32_at(payload, off);
        let offset = i32_at(payload, off + 4);
        for _ in 0..count {
            out.push(offset);
        }
        off += 8;
    }
    Ok(out)
}

/// `stsz`: sample sizes. A non-zero `sample_size` field means every sample shares that size and
/// the box carries no per-sample table; this still returns one entry per sample so callers never
/// special-case it.
pub fn decode_stsz(payload: &[u8]) -> Result<Vec<u32>, Mp4Error> {
    need(payload, 12)?;
    let sample_size = u32_at(payload, 4);
    let sample_count = u32_at(payload, 8) as usize;
    if sample_size != 0 {
        return Ok(vec![sample_size; sample_count]);
    }
    need(payload, 12 + sample_count * 4)?;
    Ok((0..sample_count)
        .map(|i| u32_at(payload, 12 + i * 4))
        .collect())
}

/// One `stsc` entry: samples per chunk starting at `first_chunk` (1-indexed), until the next
/// entry's `first_chunk`.
pub struct StscEntry {
    pub first_chunk: u32,
    pub samples_per_chunk: u32,
}

pub fn decode_stsc(payload: &[u8]) -> Result<Vec<StscEntry>, Mp4Error> {
    need(payload, 8)?;
    let entry_count = u32_at(payload, 4) as usize;
    need(payload, 8 + entry_count * 12)?;
    Ok((0..entry_count)
        .map(|i| {
            let off = 8 + i * 12;
            StscEntry {
                first_chunk: u32_at(payload, off),
                samples_per_chunk: u32_at(payload, off + 4),
            }
        })
        .collect())
}

/// `stco`: 32-bit chunk offsets.
pub fn decode_stco(payload: &[u8]) -> Result<Vec<u64>, Mp4Error> {
    need(payload, 8)?;
    let entry_count = u32_at(payload, 4) as usize;
    need(payload, 8 + entry_count * 4)?;
    Ok((0..entry_count)
        .map(|i| u32_at(payload, 8 + i * 4) as u64)
        .collect())
}

/// `co64`: 64-bit chunk offsets, for files whose sample data lies above 4 GiB.
pub fn decode_co64(payload: &[u8]) -> Result<Vec<u64>, Mp4Error> {
    need(payload, 8)?;
    let entry_count = u32_at(payload, 4) as usize;
    need(payload, 8 + entry_count * 8)?;
    Ok((0..entry_count)
        .map(|i| u64_at(payload, 8 + i * 8))
        .collect())
}

/// Combine `stsc` and chunk offsets (from `stco` or `co64`) with per-sample sizes into a
/// per-sample absolute byte offset. `sizes.len()` is the sample count; `chunk_offsets.len()` is
/// the chunk count. The last `stsc` entry's `samples_per_chunk` applies to every chunk from its
/// `first_chunk` to the last chunk.
pub fn sample_offsets(
    stsc: &[StscEntry],
    chunk_offsets: &[u64],
    sizes: &[u32],
) -> Result<Vec<u64>, Mp4Error> {
    if stsc.is_empty() {
        return if sizes.is_empty() {
            Ok(Vec::new())
        } else {
            Err(Mp4Error::Truncated)
        };
    }
    let mut out = Vec::with_capacity(sizes.len());
    let mut sample_i = 0usize;
    for (entry_i, entry) in stsc.iter().enumerate() {
        let next_first_chunk = stsc
            .get(entry_i + 1)
            .map_or(chunk_offsets.len() as u32 + 1, |n| n.first_chunk);
        for chunk in entry.first_chunk..next_first_chunk {
            let chunk_idx = (chunk - 1) as usize;
            let Some(&base) = chunk_offsets.get(chunk_idx) else {
                return Err(Mp4Error::Truncated);
            };
            let mut pos = base;
            for _ in 0..entry.samples_per_chunk {
                let Some(&size) = sizes.get(sample_i) else {
                    return Err(Mp4Error::Truncated);
                };
                out.push(pos);
                pos += size as u64;
                sample_i += 1;
            }
        }
    }
    Ok(out)
}

/// `stss`: sync (key-frame) sample numbers, 1-indexed. `None` (the box is absent) means every
/// sample is sync, which the caller decides, not this function.
pub fn decode_stss(payload: &[u8]) -> Result<Vec<u32>, Mp4Error> {
    need(payload, 8)?;
    let entry_count = u32_at(payload, 4) as usize;
    need(payload, 8 + entry_count * 4)?;
    Ok((0..entry_count)
        .map(|i| u32_at(payload, 8 + i * 4))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn box_u32s(version_flags: u32, entries: &[u32]) -> Vec<u8> {
        let mut b = version_flags.to_be_bytes().to_vec();
        for e in entries {
            b.extend(e.to_be_bytes());
        }
        b
    }

    #[test]
    fn stts_expands_runs_to_per_sample_dts() {
        // entry_count=2: run of 3 samples @ delta 10, then 2 samples @ delta 25.
        let payload = box_u32s(0, &[2, 3, 10, 2, 25]);
        let out = decode_stts(&payload).unwrap();
        assert_eq!(out, vec![(0, 10), (10, 10), (20, 10), (30, 25), (55, 25)]);
    }

    #[test]
    fn ctts_v0_and_v1_signed_offsets() {
        // v0: unsigned-looking small positive offsets.
        let v0 = box_u32s(0, &[1, 2, 3000]);
        assert_eq!(decode_ctts(&v0).unwrap(), vec![3000, 3000]);

        // v1: entry_count=2, first run negative (-500), second positive (1200).
        let mut v1 = 0x0100_0000u32.to_be_bytes().to_vec(); // version=1
        v1.extend(2u32.to_be_bytes());
        v1.extend(1u32.to_be_bytes());
        v1.extend((-500i32).to_be_bytes());
        v1.extend(1u32.to_be_bytes());
        v1.extend(1200i32.to_be_bytes());
        assert_eq!(decode_ctts(&v1).unwrap(), vec![-500, 1200]);
    }

    #[test]
    fn stsc_stco_to_sample_offsets_across_chunks() {
        // 2 chunks: chunk 1 has 2 samples, chunk 2 has 1 sample (one stsc entry covers both,
        // since samples_per_chunk only changes at chunk 1 here — use two entries to exercise
        // the "changes partway" case).
        let stsc = decode_stsc(&box_u32s(0, &[2, 1, 2, 1, 2, 1, 1, 1])).unwrap();
        // entries: first_chunk=1 samples_per_chunk=2 desc=1; first_chunk=2 samples_per_chunk=1 desc=1
        let chunk_offsets = decode_stco(&box_u32s(0, &[2, 1000, 2000])).unwrap();
        let sizes = vec![100, 100, 50];
        let offsets = sample_offsets(&stsc, &chunk_offsets, &sizes).unwrap();
        assert_eq!(offsets, vec![1000, 1100, 2000]);
    }

    #[test]
    fn co64_offsets_above_4gib() {
        let payload = {
            let mut b = 0u32.to_be_bytes().to_vec();
            b.extend(1u32.to_be_bytes());
            b.extend(5_000_000_000u64.to_be_bytes());
            b
        };
        let offsets = decode_co64(&payload).unwrap();
        assert_eq!(offsets, vec![5_000_000_000u64]);
    }

    #[test]
    fn stss_marks_sync_samples_and_absent_stss_means_all_sync() {
        let payload = box_u32s(0, &[2, 1, 10]);
        assert_eq!(decode_stss(&payload).unwrap(), vec![1, 10]);
        // "Absent stss means all sync" is a caller-level rule (no box to decode), asserted in
        // mp4_parse.rs against a real fixture rather than here.
    }
}
