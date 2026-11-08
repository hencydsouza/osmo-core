//! Generic ISO BMFF box walking: header-only reads for the top level (over a `RangeSource`, so
//! `read_movie` never has to load the potentially huge `mdat` payload), plus a pure in-memory
//! walker for `moov`'s children once `moov` itself has been fetched whole.

use super::{Mp4Error, RangeSource};

/// A box found during a top-level walk: its 4-byte type and where its payload (the bytes after
/// the size/type header, and the optional 64-bit largesize) lives in the file.
pub struct BoxHeader {
    pub kind: [u8; 4],
    pub body_start: u64,
    pub body_len: u64,
}

/// Reads one box header at `offset`. `Ok(None)` at a clean end (`offset == file_len`); an error
/// if the header or its declared size runs past `file_len`.
fn read_header(
    src: &mut dyn RangeSource,
    offset: u64,
    file_len: u64,
) -> Result<Option<BoxHeader>, Mp4Error> {
    if offset == file_len {
        return Ok(None);
    }
    if offset + 8 > file_len {
        return Err(Mp4Error::Truncated);
    }
    let head = src.read_at(offset, 8)?;
    let size32 = u32::from_be_bytes(head[0..4].try_into().unwrap());
    let kind: [u8; 4] = head[4..8].try_into().unwrap();

    let (header_len, size): (u64, u64) = if size32 == 1 {
        if offset + 16 > file_len {
            return Err(Mp4Error::Truncated);
        }
        let ext = src.read_at(offset + 8, 8)?;
        (16, u64::from_be_bytes(ext.try_into().unwrap()))
    } else if size32 == 0 {
        (8, file_len - offset)
    } else {
        (8, size32 as u64)
    };
    if size < header_len || offset + size > file_len {
        return Err(Mp4Error::Truncated);
    }
    Ok(Some(BoxHeader {
        kind,
        body_start: offset + header_len,
        body_len: size - header_len,
    }))
}

/// Every top-level box, in file order: `ftyp`, `free`, `mdat`, `moov`, in whatever order the
/// file actually has them.
pub fn walk_top_level(src: &mut dyn RangeSource) -> Result<Vec<BoxHeader>, Mp4Error> {
    let file_len = src.len()?;
    let mut out = Vec::new();
    let mut offset = 0u64;
    while let Some(h) = read_header(src, offset, file_len)? {
        offset = h.body_start + h.body_len;
        out.push(h);
    }
    Ok(out)
}

/// A box found while walking an in-memory buffer (`moov` or one of its descendants, already
/// fetched whole). `start`/`len` describe the *whole box* (header included), since raw boxes are
/// kept verbatim for the trim writer to copy.
#[derive(Clone, Copy)]
pub struct MemBox {
    pub kind: [u8; 4],
    pub start: usize,
    pub len: usize,
}

impl MemBox {
    /// This box's payload only (after the size/type header and any 64-bit largesize).
    pub fn payload<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        let (start, len) = self.payload_bounds(buf);
        &buf[start..start + len]
    }

    /// The whole box, header included — what a raw passthrough field stores.
    pub fn whole<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        &buf[self.start..self.start + self.len]
    }

    fn header_len(&self, buf: &[u8]) -> usize {
        if self.len >= 16 {
            let size32 = u32::from_be_bytes(buf[self.start..self.start + 4].try_into().unwrap());
            if size32 == 1 {
                return 16;
            }
        }
        8
    }

    /// `(start, len)` of this box's payload in `buf` — what a caller recursing into this box's
    /// children must walk, never `(self.start, self.len)` (that includes this box's own header,
    /// which `walk_mem` would then misread as a nested box).
    pub fn payload_bounds(&self, buf: &[u8]) -> (usize, usize) {
        let header_len = self.header_len(buf);
        (self.start + header_len, self.len - header_len)
    }
}

/// Every immediate child box within `buf[start..start+len)`. `Truncated` on a bad size; never
/// panics on a short buffer.
pub fn walk_mem(buf: &[u8], start: usize, len: usize) -> Result<Vec<MemBox>, Mp4Error> {
    let end = start.checked_add(len).ok_or(Mp4Error::Truncated)?;
    if end > buf.len() {
        return Err(Mp4Error::Truncated);
    }
    let mut out = Vec::new();
    let mut o = start;
    while o < end {
        if o + 8 > end {
            return Err(Mp4Error::Truncated);
        }
        let size32 = u32::from_be_bytes(buf[o..o + 4].try_into().unwrap());
        let kind: [u8; 4] = buf[o + 4..o + 8].try_into().unwrap();
        let size = if size32 == 1 {
            if o + 16 > end {
                return Err(Mp4Error::Truncated);
            }
            u64::from_be_bytes(buf[o + 8..o + 16].try_into().unwrap())
        } else if size32 == 0 {
            (end - o) as u64
        } else {
            size32 as u64
        };
        if size < 8 {
            return Err(Mp4Error::Truncated);
        }
        let box_len = size as usize;
        if o + box_len > end {
            return Err(Mp4Error::Truncated);
        }
        out.push(MemBox {
            kind,
            start: o,
            len: box_len,
        });
        o += box_len;
    }
    Ok(out)
}

/// The first child of `kind` among `children`, if any.
pub fn find<'a>(children: &'a [MemBox], kind: &[u8; 4]) -> Option<&'a MemBox> {
    children.iter().find(|b| &b.kind == kind)
}
