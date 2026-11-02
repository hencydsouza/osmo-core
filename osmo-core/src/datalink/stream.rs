//! Reassembly of the media-list reply: a stream of `0x00/0x27` DUML frames whose payload is
//! `[10B sub-header][chunk]`. Ported from Osmosis `CameraSession.kt` (`manifestBytes`, `chunkTally`,
//! `streamsEnded`, `countMediaPaths`, `hasEndMarker`). Frames are selected by DUML command, never by the
//! `4A 01` payload prefix, which parameter pushes (`0x00/0x99`) share.
use std::collections::{BTreeMap, HashSet};

/// A length-delimited path field `1a [total] 00 00 00 [sub] <ascii>` at `i`; returns the value and
/// the offset just past it. Only matches printable ASCII beginning with `prefix`.
pub(crate) fn read_path_field(
    bytes: &[u8],
    i: usize,
    sub: u8,
    prefix: &str,
) -> Option<(String, usize)> {
    if i + 6 > bytes.len()
        || bytes[i] != 0x1A
        || bytes[i + 2..i + 5] != [0, 0, 0]
        || bytes[i + 5] != sub
    {
        return None;
    }
    let slen = (bytes[i + 1] as usize).checked_sub(6)?;
    if slen < prefix.len() || i + 6 + slen > bytes.len() {
        return None;
    }
    let raw = &bytes[i + 6..i + 6 + slen];
    if !raw.iter().all(|b| (0x20..=0x7E).contains(b)) {
        return None;
    }
    let s: String = raw.iter().map(|&b| b as char).collect();
    s.starts_with(prefix).then_some((s, i + 6 + slen))
}

pub fn count_media_paths(bytes: &[u8]) -> usize {
    let mut seen = HashSet::new();
    let mut i = 0;
    while i < bytes.len() {
        match read_path_field(bytes, i, 1, "DCIM/") {
            Some((p, end)) => {
                seen.insert(p);
                i = end;
            }
            None => i += 1,
        }
    }
    seen.len()
}

/// Walks the frames in `raw` (no CRC check, as in Osmosis) yielding `(set, cmd, payload)`.
fn frames(raw: &[u8]) -> impl Iterator<Item = (u8, u8, &[u8])> {
    let mut i = 0;
    std::iter::from_fn(move || {
        while i + 13 <= raw.len() {
            if raw[i] != 0x55 {
                i += 1;
                continue;
            }
            let len = (raw[i + 1] as usize | (raw[i + 2] as usize) << 8) & 0x3FF;
            if len < 13 || i + len > raw.len() {
                i += 1;
                continue;
            }
            let f = (raw[i + 9], raw[i + 10], &raw[i + 11..i + len - 2]);
            i += len;
            return Some(f);
        }
        None
    })
}

/// The contiguous manifest from the data chunks (`0x00/0x27`, sub-header `4A 01`) in arrival order.
/// With `request_ctr` only chunks answering that request counter (sub-header byte 4) are kept.
/// Without it, falls back to `raw` if reassembly found fewer media paths than the raw blob.
pub fn manifest_bytes(raw: &[u8], request_ctr: Option<u8>) -> Vec<u8> {
    let mut out = Vec::new();
    for (set, cmd, p) in frames(raw) {
        if set == 0
            && cmd == 0x27
            && p.len() > 10
            && p[0] == 0x4A
            && p[1] == 0x01
            && request_ctr.is_none_or(|c| p[4] == c)
        {
            out.extend_from_slice(&p[10..]);
        }
    }
    if request_ctr.is_some() {
        return out;
    }
    if !out.is_empty() && count_media_paths(&out) >= count_media_paths(raw) {
        out
    } else {
        raw.to_vec()
    }
}

/// `0x00/0x27` frames by request counter, then by `4A` subtype (0x04 start, 0x01 data, 0x03 end).
pub fn chunk_tally(raw: &[u8]) -> BTreeMap<u8, BTreeMap<u8, u32>> {
    let mut tally: BTreeMap<u8, BTreeMap<u8, u32>> = BTreeMap::new();
    for (set, cmd, p) in frames(raw) {
        if set == 0 && cmd == 0x27 && p.len() >= 10 && p[0] == 0x4A {
            *tally.entry(p[4]).or_default().entry(p[1]).or_insert(0) += 1;
        }
    }
    tally
}

/// Has the camera closed every answer? A counter is closed when it sent its `4A 03` end frame, or
/// opened (`4A 04`) and sent no data (how a store with nothing on it answers). Every counter seen and
/// every `required` one must be closed, and at least one must have carried data.
pub fn streams_ended(raw: &[u8], required: &[u8]) -> bool {
    let tally = chunk_tally(raw);
    if !tally
        .values()
        .any(|s| s.get(&0x01).copied().unwrap_or(0) > 0)
    {
        return false;
    }
    let closed = |ctr: &u8| {
        tally.get(ctr).is_some_and(|s| {
            let n = |k: u8| s.get(&k).copied().unwrap_or(0);
            n(0x03) > 0 || (n(0x04) > 0 && n(0x01) == 0)
        })
    };
    required.iter().all(closed) && tally.keys().all(closed)
}

/// The `0c 01 0d` run: the camera's "nothing older" marker before the last record's name field.
pub fn has_end_marker(bytes: &[u8]) -> bool {
    bytes.windows(3).any(|w| w == [0x0C, 0x01, 0x0D])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duml::Frame;

    fn fx(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/tests/fixtures/manifests/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    /// One reply frame on `set/cmd` carrying `4A <sub> .. <ctr> ..` plus `body`.
    fn chunk(set: u8, cmd: u8, ctr: u8, sub: u8, body: &[u8]) -> Vec<u8> {
        let mut p = vec![0x4A, sub, 0, 0, ctr, 0, 0, 0, 0, 0];
        p.extend(body);
        Frame {
            target: 0x0201,
            id: 1,
            ty: 0xC0 | (set as u32) << 8 | (cmd as u32) << 16,
            payload: p,
        }
        .encode()
    }

    #[test]
    fn oa4_full_page_reassembles_45_paths() {
        let raw = fx("oa4_45.bin");
        assert_eq!(count_media_paths(&manifest_bytes(&raw, None)), 45);
    }

    #[test]
    fn end_marker_is_on_final_pages_and_not_on_full_ones() {
        for f in [
            "xtra_13.bin",
            "oa4_6.bin",
            "oa6_sd_3.bin",
            "oa6_internal_2.bin",
            "op3_15.bin",
        ] {
            assert!(
                has_end_marker(&manifest_bytes(&fx(f), None)),
                "{f} is a final page"
            );
        }
        for f in ["nano_45.bin", "oa4_45.bin", "op4_45.bin"] {
            assert!(
                !has_end_marker(&manifest_bytes(&fx(f), None)),
                "{f} is a full page"
            );
        }
    }

    #[test]
    fn stream_is_complete_when_every_counter_has_closed() {
        let nano = fx("nano_45.bin");
        assert!(
            streams_ended(&nano, &[1, 2]),
            "start-only SD + ended internal = closed"
        );
        assert!(
            !streams_ended(&nano, &[1, 2, 3]),
            "a counter the camera never opened is not closed"
        );
        // Rewrite every 4A 03 end frame's subtype: no end frame means still streaming.
        let mut cut = nano.clone();
        let mut i = 0;
        while i + 13 <= cut.len() {
            if cut[i] != 0x55 {
                i += 1;
                continue;
            }
            let len = (cut[i + 1] as usize | (cut[i + 2] as usize) << 8) & 0x3FF;
            if len < 13 || i + len > cut.len() {
                i += 1;
                continue;
            }
            if cut[i + 9] == 0 && cut[i + 10] == 0x27 && cut[i + 11] == 0x4A && cut[i + 12] == 0x03
            {
                cut[i + 12] = 0x7F;
            }
            i += len;
        }
        assert!(!streams_ended(&cut, &[1, 2]));
    }

    #[test]
    fn empty_store_stream_is_closed_and_slices_by_counter() {
        let mut raw = chunk(0, 0x27, 1, 0x04, &[]);
        raw.extend(chunk(0, 0x27, 2, 0x04, &[]));
        raw.extend(chunk(0, 0x27, 2, 0x01, b"hello"));
        raw.extend(chunk(0, 0x27, 2, 0x03, &[]));
        assert!(streams_ended(&raw, &[1, 2]));
        assert!(manifest_bytes(&raw, Some(1)).is_empty());
        assert_eq!(manifest_bytes(&raw, Some(2)), b"hello");
    }

    #[test]
    fn stream_with_no_data_is_not_ended() {
        let raw = chunk(0, 0x27, 1, 0x04, &[]);
        assert!(!streams_ended(&raw, &[1]));
    }

    #[test]
    fn parameter_push_chunks_are_ignored() {
        let mut raw = chunk(0x00, 0x99, 1, 0x01, b"camcap_iso");
        raw.extend(chunk(0x00, 0x27, 1, 0x01, b"real"));
        assert_eq!(manifest_bytes(&raw, Some(1)), b"real");
        assert!(chunk_tally(&raw)[&1].get(&0x01) == Some(&1));
    }

    #[test]
    fn short_or_garbage_blobs_do_not_panic() {
        assert!(manifest_bytes(&[], None).is_empty());
        assert!(manifest_bytes(&[0x55, 0x0D, 0x04], Some(1)).is_empty());
        let junk: Vec<u8> = (0..2000u32).map(|i| (i * 31 % 251) as u8).collect();
        let _ = (
            manifest_bytes(&junk, None),
            chunk_tally(&junk),
            streams_ended(&junk, &[1]),
            count_media_paths(&junk),
        );
    }
}
