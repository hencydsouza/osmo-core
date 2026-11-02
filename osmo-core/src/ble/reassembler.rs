use crate::duml::Frame;

/// Rebuilds DUML frames from BLE notification fragments (a frame can span several, or many can
/// share one). Ported from Osmosis `DumlReassembler`, except that a corrupt frame is skipped one
/// byte at a time instead of stalling the stream until the buffer overflows.
#[derive(Default)]
pub struct Reassembler {
    carry: Vec<u8>,
}

impl Reassembler {
    pub fn feed(&mut self, raw: &[u8]) -> Vec<Frame> {
        let mut buf = std::mem::take(&mut self.carry);
        buf.extend_from_slice(raw);
        let mut out = Vec::new();
        let mut off = 0;
        loop {
            while off < buf.len() && buf[off] != 0x55 {
                off += 1;
            }
            if off + 2 > buf.len() {
                break;
            }
            let len = buf[off + 1] as usize;
            if len < 13 {
                off += 1;
                continue;
            }
            if off + len > buf.len() {
                break;
            }
            match Frame::decode(&buf[off..off + len]) {
                Ok(f) => {
                    out.push(f);
                    off += len;
                }
                Err(_) => off += 1,
            }
        }
        if buf.len() - off <= 512 {
            self.carry = buf[off..].to_vec();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duml::Frame;

    fn frame(id: u16, payload: Vec<u8>) -> Vec<u8> {
        Frame { target: 0x0207, id, ty: 0xC0, payload }.encode()
    }

    #[test]
    fn reassembler_joins_fragmented_frame() {
        let f = frame(1, vec![1, 2, 3, 4, 5]);
        let mut r = Reassembler::default();
        assert!(r.feed(&f[..7]).is_empty());
        let got = r.feed(&f[7..]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].payload, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn reassembler_handles_two_frames_in_one_fragment() {
        let mut both = frame(1, vec![1]);
        both.extend(frame(2, vec![2, 2]));
        let got = Reassembler::default().feed(&both);
        assert_eq!(got.iter().map(|f| f.id).collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn reassembler_resyncs_past_noise() {
        let mut raw = vec![0x00, 0x13];
        raw.extend(frame(9, vec![7]));
        assert_eq!(Reassembler::default().feed(&raw).len(), 1);
    }

    #[test]
    fn corrupt_frame_does_not_wedge_the_stream() {
        let mut bad = frame(1, vec![1, 2, 3]);
        let n = bad.len();
        bad[n - 1] ^= 0xFF;
        let mut raw = bad;
        raw.extend(frame(2, vec![9]));
        let got = Reassembler::default().feed(&raw);
        assert_eq!(got.iter().map(|f| f.id).collect::<Vec<_>>(), vec![2]);
    }
}
