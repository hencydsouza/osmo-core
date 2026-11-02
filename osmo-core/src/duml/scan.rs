use super::crc::{crc16, crc8};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scanned {
    pub set: u8,
    pub cmd: u8,
    pub payload: Vec<u8>,
}

fn frame_len(raw: &[u8], i: usize) -> usize {
    (raw[i + 1] as usize | (raw[i + 2] as usize) << 8) & 0x3FF
}

/// Every CRC-valid DUML frame in `raw`. Length is 10 bits plus a 6-bit version packed LE into
/// bytes 1-2; advances one byte at a time so a frame tunnelled inside another is still found.
pub fn scan_frames(raw: &[u8]) -> Vec<Scanned> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 13 <= raw.len() {
        if raw[i] != 0x55 {
            i += 1;
            continue;
        }
        let len = frame_len(raw, i);
        if raw[i + 2] >> 2 != 1 || len < 13 || i + len > raw.len() || crc8(&raw[i..i + 3]) != raw[i + 3] {
            i += 1;
            continue;
        }
        let want = u16::from_le_bytes([raw[i + len - 2], raw[i + len - 1]]);
        if crc16(&raw[i..i + len - 2]) != want {
            i += 1;
            continue;
        }
        out.push(Scanned { set: raw[i + 9], cmd: raw[i + 10], payload: raw[i + 11..i + len - 2].to_vec() });
        i += 1;
    }
    out
}

/// Payload of the first reply frame with the given set/cmd across `datagrams`, skipping the
/// empty-payload transport ACK the camera sends before the real reply.
pub fn find_reply(datagrams: &[Vec<u8>], set: u8, cmd: u8) -> Option<Vec<u8>> {
    for d in datagrams {
        let mut i = 0;
        while i + 11 <= d.len() {
            if d[i] != 0x55 {
                i += 1;
                continue;
            }
            let len = frame_len(d, i);
            if len < 13 || i + len > d.len() || crc8(&d[i..i + 3]) != d[i + 3] {
                i += 1;
                continue;
            }
            if d[i + 9] == set && d[i + 10] == cmd {
                let payload = &d[i + 11..i + len - 2];
                if !payload.is_empty() {
                    return Some(payload.to_vec());
                }
            }
            i += len;
        }
    }
    None
}

/// Leading status word of a response frame with `set`/`cmd` inside one datagram.
pub fn find_resp_status(d: &[u8], set: u8, cmd: u8) -> Option<u16> {
    let mut i = 0;
    while i + 13 <= d.len() {
        if d[i] != 0x55 {
            i += 1;
            continue;
        }
        let len = frame_len(d, i);
        if len < 13 || i + len > d.len() {
            i += 1;
            continue;
        }
        if d[i + 9] == set && d[i + 10] == cmd {
            let p = &d[i + 11..i + len - 2];
            return Some(match p.len() {
                0 => 0,
                1 => p[0] as u16,
                _ => u16::from_le_bytes([p[0], p[1]]),
            });
        }
        i += len;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duml::Frame;

    fn frame(set: u8, cmd: u8, payload: Vec<u8>) -> Vec<u8> {
        Frame { target: 0x0201, id: 1, ty: 0x40 | (set as u32) << 8 | (cmd as u32) << 16, payload }.encode()
    }

    #[test]
    fn scan_frames_finds_frame_with_length_over_255() {
        let big = frame(0x00, 0x27, vec![7u8; 300]);
        assert_eq!(big[2] & 0xFC, 0x04);
        assert_ne!(big[2], 0x04);
        let mut raw = vec![0xAA, 0xBB];
        raw.extend(&big);
        raw.extend([0xCC]);
        let got = scan_frames(&raw);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].set, got[0].cmd, got[0].payload.len()), (0x00, 0x27, 300));
    }

    #[test]
    fn scan_frames_skips_false_sof() {
        let mut payload = vec![0x55, 0x20, 0x04, 0x00];
        payload.extend([0x11; 30]);
        let raw = frame(0x02, 0x80, payload);
        assert_eq!(scan_frames(&raw).len(), 1);
    }

    #[test]
    fn scan_frames_finds_two_back_to_back() {
        let mut raw = frame(0x00, 0x27, vec![1]);
        raw.extend(frame(0x02, 0x80, vec![2, 3]));
        let got = scan_frames(&raw);
        assert_eq!(got.iter().map(|s| (s.set, s.cmd)).collect::<Vec<_>>(), vec![(0, 0x27), (2, 0x80)]);
    }

    #[test]
    fn find_reply_skips_empty_transport_ack() {
        let d1 = frame(0x02, 0x0C, vec![]);
        let d2 = frame(0x02, 0x0C, vec![0x00]);
        assert_eq!(find_reply(&[d1, d2], 0x02, 0x0C), Some(vec![0x00]));
    }

    #[test]
    fn find_reply_none_when_absent() {
        assert_eq!(find_reply(&[frame(0x02, 0x80, vec![1])], 0x02, 0x0C), None);
    }

    #[test]
    fn find_resp_status_reads_first_word() {
        assert_eq!(find_resp_status(&frame(0x00, 0x28, vec![0x34, 0x12]), 0x00, 0x28), Some(0x1234));
        assert_eq!(find_resp_status(&frame(0x00, 0x28, vec![]), 0x00, 0x28), Some(0));
    }
}
