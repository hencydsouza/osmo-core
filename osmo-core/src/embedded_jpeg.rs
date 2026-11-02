//! Extract the JPEG thumbnail embedded in a still's EXIF block, from just the first 64 KiB of the file.
//! Ported from Osmosis `EmbeddedJpeg.kt`. Walks the JPEG segments to the APP1 (EXIF) segment and returns the
//! inner JPEG's bytes; `None` when there is none. Never panics on arbitrary input.

/// How much of the start of a file is enough to find the EXIF thumbnail.
pub const HEAD_BYTES: usize = 65_536;

fn u16be(b: &[u8], i: usize) -> usize {
    ((b[i] as usize) << 8) | b[i + 1] as usize
}

/// `jpeg`/`png` if the bytes start with that format's magic, else `None` (an error page, an empty body).
pub fn image_kind(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(&[0xFF, 0xD8]) {
        Some("jpeg")
    } else if b.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("png")
    } else {
        None
    }
}

pub fn embedded_jpeg(head: &[u8]) -> Option<Vec<u8>> {
    if head.len() < 4 || head[0] != 0xFF || head[1] != 0xD8 {
        return None;
    }
    let mut i = 2;
    while i + 4 <= head.len() {
        if head[i] != 0xFF {
            i += 1; // resync on padding
            continue;
        }
        match head[i + 1] {
            0xD8 | 0x01 | 0xD0..=0xD7 => i += 2, // standalone markers carry no length
            0xDA | 0xD9 => return None,          // image data starts: nothing embedded
            marker => {
                let len = u16be(head, i + 2);
                if len < 2 {
                    return None;
                }
                if marker == 0xE1 {
                    let end = (i + 2 + len).min(head.len());
                    if let Some(j) = jpeg_within(head, i + 4, end) {
                        return Some(j);
                    }
                }
                i += 2 + len;
            }
        }
    }
    None
}

/// The first SOI inside `[from, to)` through the last EOI before `to`.
fn jpeg_within(b: &[u8], from: usize, to: usize) -> Option<Vec<u8>> {
    let to = to.min(b.len());
    let soi = (from..to.saturating_sub(1)).find(|&i| b[i] == 0xFF && b[i + 1] == 0xD8)?;
    let eoi = (soi + 1..to.saturating_sub(1))
        .rev()
        .find(|&j| b[j] == 0xFF && b[j + 1] == 0xD9)?;
    Some(b[soi..eoi + 2].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SOI, an APP1 segment whose body is `Exif\0\0` + an inner JPEG, then start-of-scan.
    fn jpeg_with_inner(inner: &[u8]) -> Vec<u8> {
        let mut body = b"Exif\0\0".to_vec();
        body.extend(inner);
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xE1];
        v.extend(((body.len() + 2) as u16).to_be_bytes());
        v.extend(body);
        v.extend([0xFF, 0xDA, 0x00, 0x04, 0x00, 0x00, 0x11, 0x22]);
        v
    }

    const INNER: [u8; 6] = [0xFF, 0xD8, 0xAA, 0xBB, 0xFF, 0xD9];

    #[test]
    fn extracts_the_jpeg_inside_an_app1_segment() {
        assert_eq!(
            embedded_jpeg(&jpeg_with_inner(&INNER)),
            Some(INNER.to_vec())
        );
    }

    #[test]
    fn returns_none_when_image_data_starts_before_any_app1() {
        assert_eq!(
            embedded_jpeg(&[0xFF, 0xD8, 0xFF, 0xDA, 0x00, 0x04, 0x00, 0x00]),
            None
        );
    }

    #[test]
    fn returns_none_on_truncated_or_non_jpeg_input() {
        assert_eq!(embedded_jpeg(&[]), None);
        assert_eq!(embedded_jpeg(&[0xFF]), None);
        assert_eq!(embedded_jpeg(&[0x00, 0x11, 0x22, 0x33, 0x44]), None);
        let whole = jpeg_with_inner(&INNER);
        assert_eq!(
            embedded_jpeg(&whole[..14]),
            None,
            "cut before the inner EOI"
        );
    }

    #[test]
    fn never_panics_on_garbage_or_any_prefix() {
        let junk: Vec<u8> = (0..2000u32).map(|i| (i * 131 % 251) as u8).collect();
        let _ = embedded_jpeg(&junk);
        let whole = jpeg_with_inner(&INNER);
        for n in 0..=whole.len() {
            let _ = embedded_jpeg(&whole[..n]);
        }
        let mut ff = vec![0xFF; 64];
        ff[0] = 0xFF;
        ff[1] = 0xD8;
        let _ = embedded_jpeg(&ff);
    }

    #[test]
    fn image_kind_recognises_jpeg_and_png_only() {
        assert_eq!(image_kind(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("jpeg"));
        assert_eq!(image_kind(&[0x89, b'P', b'N', b'G', 0x0D]), Some("png"));
        assert_eq!(image_kind(b"<html>404</html>"), None);
        assert_eq!(image_kind(&[]), None);
    }
}
