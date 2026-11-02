//! Structural decoder for DJI's CompositePack media manifest. Ported from Osmosis
//! `CameraSession.decodeComposite` / `resolveRecord` for the fields the prototype needs: path,
//! thumbnail, name, extension, size, handle and which store-list a record belongs to.
//! Every read is bounds-checked: arbitrary bytes must never panic.
use super::stream::{manifest_bytes, read_path_field};
use crate::media::CameraFile;
use std::collections::{BTreeSet, HashMap, HashSet};

const VIDEO_EXTS: [&str; 4] = ["MP4", "MOV", "OSV", "INSV"];
const PHOTO_EXTS: [&str; 4] = ["JPG", "JPEG", "DNG", "HEIC"];
const PROXY_EXTS: [&str; 3] = ["LRF", "LRV", "XRF"];

fn u32le(b: &[u8], o: usize) -> u64 {
    match b.get(o..o + 4) {
        Some(s) => u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as u64,
        None => 0,
    }
}

fn is_marker_at(b: &[u8], q: usize) -> bool {
    b.get(q).is_some_and(|&x| x == 0xFF || x == 0xFE)
        && b.get(q + 1) == Some(&0x19)
        && b.get(q + 2) == Some(&0x06)
}

/// Index of the first record of the second concatenated list, if the manifest holds two. The leading
/// u32 count covers only the first list.
fn list_boundary(bytes: &[u8], records: usize) -> Option<usize> {
    if bytes.len() < 4 {
        return None;
    }
    let declared = u32le(bytes, 0) as usize;
    (1..records).contains(&declared).then_some(declared)
}

struct Media {
    pos: usize,
    end: usize,
    path: String,
}

fn resolve_record(
    bytes: &[u8],
    media_dir: &str,
    lo: usize,
    hi: usize,
    self_pos: usize,
) -> CameraFile {
    let base = media_dir.rsplit('/').next().unwrap_or(media_dir);
    let base_b = base.as_bytes();

    let mut thumb: Option<String> = None;
    for t in lo..hi {
        if let Some((v, _)) = read_path_field(bytes, t, 2, "MISC/") {
            if v.ends_with(base) {
                thumb = Some(v);
                break;
            }
        }
    }

    let (mut ext, mut _proxy) = (String::new(), None::<String>);
    let mut n = lo;
    while n + 2 < hi {
        if bytes[n] == 0x0D {
            let len = bytes[n + 1] as usize;
            if len > base_b.len() && n + 2 + len <= bytes.len() {
                let v = &bytes[n + 2..n + 2 + len];
                if v.len() > base_b.len() + 1 && v.starts_with(base_b) && v[base_b.len()] == b'.' {
                    let e = String::from_utf8_lossy(&v[base_b.len() + 1..]).to_uppercase();
                    if VIDEO_EXTS.contains(&e.as_str()) || PHOTO_EXTS.contains(&e.as_str()) {
                        ext = e;
                    } else if PROXY_EXTS.contains(&e.as_str()) {
                        _proxy = Some(e);
                    }
                }
            }
        }
        n += 1;
    }

    // Video records expose handle and size through the marker `03|00 ff|fe 19 06` at head+8.
    let marker_end = if self_pos > lo && self_pos <= hi {
        self_pos
    } else {
        hi
    };
    let mut head: Option<usize> = None;
    let mut m = lo;
    while m + 4 < marker_end {
        let (kind, star) = (bytes[m], bytes[m + 1]);
        if (kind == 3 || kind == 0)
            && (star == 0xFF || star == 0xFE)
            && bytes[m + 2] == 0x19
            && bytes[m + 3] == 0x06
            && m >= 8
        {
            head = Some(m - 8);
            break;
        }
        m += 1;
    }
    let is_video = VIDEO_EXTS.contains(&ext.as_str());

    let mut photo_size = 0u64;
    if !is_video {
        if self_pos >= 21 {
            let fixed = self_pos - 7;
            if bytes.get(fixed) == Some(&0x19) && bytes.get(fixed + 1) == Some(&0x06) {
                photo_size = u32le(bytes, fixed - 14);
            }
        }
        let mut q = lo;
        while photo_size == 0 && q + 3 < marker_end {
            if is_marker_at(bytes, q) {
                let mk = q + 1;
                if mk >= 14 {
                    photo_size = u32le(bytes, mk - 14);
                }
                break;
            }
            q += 1;
        }
    }

    let path = if ext.is_empty() {
        media_dir.to_string()
    } else {
        format!("{media_dir}.{ext}")
    };
    let thumb_path = format!(
        "{}.scr",
        thumb.unwrap_or_else(|| media_dir.replacen("DCIM/", "MISC/THM/", 1))
    );
    let size = match head {
        Some(h) if is_video && h >= 4 => u32le(bytes, h - 4),
        _ => photo_size,
    };
    let name = path.rsplit('/').next().unwrap_or(&path).to_string();
    // A handle read at the fixed position `(19 06) - 10`, before anything has vouched for it.
    let handle_candidate = if self_pos >= 17
        && bytes.get(self_pos - 7) == Some(&0x19)
        && bytes.get(self_pos - 6) == Some(&0x06)
    {
        u32le(bytes, self_pos - 17) as u32
    } else {
        0
    };
    CameraFile {
        seq: seq_from_name(&name),
        name,
        path,
        thumb_path,
        ext,
        storage: 0,
        size_bytes: size,
        handle: head.map_or(0, |h| u32le(bytes, h) as u32),
        group: 0,
        cmd_handle: 0,
        handle_candidate,
        handle_shared: false,
        starred: star_by_signature(bytes, self_pos, hi).unwrap_or_else(|| star_flag(bytes, lo, hi)),
    }
}

/// The favourite flag anchors on a 12-byte signature that sits after the record's media path, once per
/// record, on the bodies that write it (Action 4, Action 5 Pro, Pocket 3); the byte after it is the flag.
const STAR_SIG: [u8; 12] = [
    0x1b, 0x0a, 0x00, 0x00, 0x00, 0x02, 0x02, 0x01, 0x14, 0x02, 0x15, 0x03,
];

fn star_by_signature(bytes: &[u8], lo: usize, hi: usize) -> Option<bool> {
    let end = hi.min(bytes.len().checked_sub(STAR_SIG.len() + 1)?);
    (lo..=end)
        .find(|&q| bytes[q..q + STAR_SIG.len()] == STAR_SIG)
        .map(|q| bytes[q + STAR_SIG.len()] == 1)
}

/// Fallback for bodies with no signature (Nano, Action 6): the byte 9 past the `[ff|fe] 19 06` marker, trusted
/// only when it is exactly 1 (on other layouts that offset is a length byte).
fn star_flag(bytes: &[u8], lo: usize, hi: usize) -> bool {
    // The first marker decides; a later one belongs to a different part of the record.
    (lo..hi.saturating_sub(9))
        .find(|&q| is_marker_at(bytes, q))
        .is_some_and(|q| bytes.get(q + 9) == Some(&1))
}

/// The 4-digit number in `_NNNN_D`, 0 if absent.
fn seq_from_name(name: &str) -> u32 {
    let b = name.as_bytes();
    (0..b.len().saturating_sub(6))
        .find(|&i| {
            b[i] == b'_'
                && b[i + 1..i + 5].iter().all(u8::is_ascii_digit)
                && &b[i + 5..i + 7] == b"_D"
        })
        .map_or(0, |i| name[i + 1..i + 5].parse().unwrap_or(0))
}

/// The most common value, ties going to the one seen first.
fn most_common(v: &[i64]) -> Option<i64> {
    let mut counts: Vec<(i64, usize)> = Vec::new();
    for &x in v {
        match counts.iter_mut().find(|(k, _)| *k == x) {
            Some(c) => c.1 += 1,
            None => counts.push((x, 1)),
        }
    }
    counts
        .iter()
        .fold(None::<(i64, usize)>, |best, &(k, n)| match best {
            Some((_, bn)) if bn >= n => best,
            _ => Some((k, n)),
        })
        .map(|(k, _)| k)
}

/// Fit `handle = base + seq * step` per list to the records that expose a handle, then use it to (a) fill
/// `cmd_handle` for every record, (b) promote a still's candidate to its `handle` when the fit agrees exactly,
/// and (c) revoke a handle the fit contradicts. Two independent sources must agree before a file can be
/// deleted: one source is not the bar for an irreversible command.
pub(crate) fn with_cmd_handles(mut files: Vec<CameraFile>) -> Vec<CameraFile> {
    let groups: BTreeSet<u8> = files.iter().map(|f| f.group).collect();
    let mut fits: HashMap<u8, (i64, i64)> = HashMap::new();
    for g in groups {
        let mut pts: Vec<(i64, i64)> = Vec::new();
        for f in files
            .iter()
            .filter(|f| f.group == g && f.handle != 0 && f.seq > 0)
        {
            if !pts.iter().any(|(s, _)| *s == f.seq as i64) {
                pts.push((f.seq as i64, f.handle as i64));
            }
        }
        pts.sort_by_key(|p| p.0);
        if pts.len() < 2 {
            continue;
        }
        let steps: Vec<i64> = pts
            .windows(2)
            .filter_map(|w| Some((w[1].1 - w[0].1) / (w[1].0 - w[0].0)).filter(|d| *d > 0))
            .collect();
        let Some(step) = most_common(&steps) else {
            continue;
        };
        let bases: Vec<i64> = pts.iter().map(|(s, h)| h - s * step).collect();
        let Some(base) = most_common(&bases) else {
            continue;
        };
        fits.insert(g, (base, step));
    }
    for f in &mut files {
        let Some(&(base, step)) = fits.get(&f.group) else {
            continue;
        };
        if f.seq == 0 {
            continue;
        }
        let Ok(fitted) = u32::try_from(base + f.seq as i64 * step) else {
            continue;
        };
        if f.handle == 0 && f.handle_candidate != 0 && f.handle_candidate == fitted {
            f.handle = f.handle_candidate;
        } else if f.handle != 0 && f.handle != fitted {
            f.handle = 0;
        }
        f.cmd_handle = fitted;
    }
    files
}

/// A handle held by more than one record would make delete hit whichever file the camera has under it, which
/// reads as success. Disable delete for every holder.
pub(crate) fn flag_handle_collisions(mut files: Vec<CameraFile>) -> Vec<CameraFile> {
    let mut counts: HashMap<u32, usize> = HashMap::new();
    for f in files.iter().filter(|f| f.handle != 0) {
        *counts.entry(f.handle).or_default() += 1;
    }
    for f in &mut files {
        f.handle_shared = f.handle != 0 && counts[&f.handle] > 1;
    }
    files
}

fn decode_composite(bytes: &[u8]) -> Vec<CameraFile> {
    let mut medias = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match read_path_field(bytes, i, 1, "DCIM/") {
            Some((path, end)) => {
                medias.push(Media { pos: i, end, path });
                i = end;
            }
            None => i += 1,
        }
    }
    let boundary = list_boundary(bytes, medias.len());
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (k, m) in medias.iter().enumerate() {
        if !seen.insert(m.path.clone()) {
            continue;
        }
        let lo = if k > 0 { medias[k - 1].end } else { 0 };
        let hi = medias.get(k + 1).map_or(bytes.len(), |n| n.pos);
        let mut f = resolve_record(bytes, &m.path, lo, hi, m.pos);
        f.group = u8::from(boundary.is_some_and(|b| k >= b));
        out.push(f);
    }
    with_cmd_handles(out)
}

/// Decode a raw captured/collected blob of reply datagrams into the merged file list.
pub fn decode_manifest(raw_blob: &[u8]) -> Vec<CameraFile> {
    flag_handle_collisions(decode_composite(&manifest_bytes(raw_blob, None)))
}

/// Split one collected blob into its per-store answers using the request counters, stamping each
/// file's `storage` (0 = SD under `sd_ctr`, 1 = internal under `internal_ctr`). Falls back to the
/// merged decode when neither slice has records or both answered with the same list.
pub fn collect_stores(raw_blob: &[u8], sd_ctr: u8, internal_ctr: u8) -> Vec<CameraFile> {
    let slice =
        |ctr: u8| flag_handle_collisions(decode_composite(&manifest_bytes(raw_blob, Some(ctr))));
    let (sd, internal) = (slice(sd_ctr), slice(internal_ctr));
    let paths = |v: &[CameraFile]| v.iter().map(|f| f.path.clone()).collect::<HashSet<_>>();
    let ambiguous = !sd.is_empty() && !internal.is_empty() && paths(&sd) == paths(&internal);
    if (sd.is_empty() && internal.is_empty()) || ambiguous {
        return decode_manifest(raw_blob);
    }
    let mut seen = HashSet::new();
    internal
        .into_iter()
        .map(|f| CameraFile { storage: 1, ..f })
        .chain(sd.into_iter().map(|f| CameraFile { storage: 0, ..f }))
        .filter(|f| seen.insert(f.path.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(seq: u32, handle: u32, candidate: u32) -> CameraFile {
        CameraFile {
            path: format!("DCIM/DJI_001/DJI_20260101000000_{seq:04}_D.JPG"),
            thumb_path: String::new(),
            name: format!("DJI_20260101000000_{seq:04}_D.JPG"),
            ext: "JPG".into(),
            storage: 0,
            size_bytes: 1,
            handle,
            group: 0,
            seq,
            cmd_handle: 0,
            handle_candidate: candidate,
            handle_shared: false,
            starred: false,
        }
    }

    #[test]
    fn shared_handle_disables_delete_for_every_holder() {
        let out = flag_handle_collisions(vec![
            file(1, 0x40010, 0),
            file(2, 0x40010, 0),
            file(3, 0x40030, 0),
        ]);
        assert!(
            !out[0].deletable() && !out[1].deletable(),
            "both holders of a shared handle"
        );
        assert!(out[2].deletable(), "a unique handle stays deletable");
    }

    #[test]
    fn a_marker_handle_the_fit_contradicts_is_revoked() {
        let out = with_cmd_handles(vec![
            file(1, 0x40010, 0),
            file(2, 0x40020, 0),
            file(3, 0x40030, 0),
            file(4, 0x99999, 0),
        ]);
        assert_eq!((out[3].handle, out[3].cmd_handle), (0, 0x40040));
        assert!(!out[3].deletable());
        assert!(out[0].deletable() && out[1].deletable() && out[2].deletable());
    }

    #[test]
    fn candidate_is_promoted_only_when_the_fit_agrees() {
        let out = with_cmd_handles(vec![
            file(1, 0x40010, 0),
            file(2, 0x40020, 0),
            file(3, 0x40030, 0),
            file(5, 0, 0x40050),
            file(6, 0, 0x40061),
        ]);
        assert_eq!(
            (out[3].handle, out[3].cmd_handle),
            (0x40050, 0x40050),
            "agreeing candidate is promoted"
        );
        assert!(out[3].deletable());
        assert_eq!(
            (out[4].handle, out[4].cmd_handle),
            (0, 0x40060),
            "disagreeing candidate is not"
        );
        assert!(!out[4].deletable());
    }

    #[test]
    fn fewer_than_two_handles_gets_no_fit() {
        let out = with_cmd_handles(vec![file(1, 0x40010, 0), file(2, 0, 0x40020)]);
        assert!(out.iter().all(|f| f.cmd_handle == 0));
        assert_eq!(
            out[1].handle, 0,
            "nothing vouches for the candidate, so it is not promoted"
        );
    }
}
