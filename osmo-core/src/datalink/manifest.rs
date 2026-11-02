//! Structural decoder for DJI's CompositePack media manifest. Ported from Osmosis
//! `CameraSession.decodeComposite` / `resolveRecord` for the fields the prototype needs: path,
//! thumbnail, name, extension, size, handle and which store-list a record belongs to.
//! Every read is bounds-checked: arbitrary bytes must never panic.
use super::stream::{manifest_bytes, read_path_field};
use crate::media::CameraFile;
use std::collections::HashSet;

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
    CameraFile {
        name: path.rsplit('/').next().unwrap_or(&path).to_string(),
        path,
        thumb_path,
        ext,
        storage: 0,
        size_bytes: size,
        handle: head.map_or(0, |h| u32le(bytes, h) as u32),
        group: 0,
    }
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
    out
}

/// Decode a raw captured/collected blob of reply datagrams into the merged file list.
pub fn decode_manifest(raw_blob: &[u8]) -> Vec<CameraFile> {
    decode_composite(&manifest_bytes(raw_blob, None))
}

/// Split one collected blob into its per-store answers using the request counters, stamping each
/// file's `storage` (0 = SD under `sd_ctr`, 1 = internal under `internal_ctr`). Falls back to the
/// merged decode when neither slice has records or both answered with the same list.
pub fn collect_stores(raw_blob: &[u8], sd_ctr: u8, internal_ctr: u8) -> Vec<CameraFile> {
    let slice = |ctr: u8| decode_composite(&manifest_bytes(raw_blob, Some(ctr)));
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
