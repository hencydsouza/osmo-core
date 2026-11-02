use super::crc::{crc16, crc8};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub target: u16,
    pub id: u16,
    /// 24-bit type field: `flags | set << 8 | cmd << 16`.
    pub ty: u32,
    pub payload: Vec<u8>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FrameError {
    #[error("frame too short")]
    TooShort,
    #[error("bad start byte")]
    BadSof,
    #[error("length field does not match data")]
    BadLength,
    #[error("unsupported version")]
    BadVersion,
    #[error("bad header crc")]
    BadHeaderCrc,
    #[error("bad crc")]
    BadCrc,
}

impl Frame {
    pub fn flags(&self) -> u8 {
        (self.ty & 0xFF) as u8
    }
    pub fn cmd_set(&self) -> u8 {
        ((self.ty >> 8) & 0xFF) as u8
    }
    pub fn cmd_id(&self) -> u8 {
        ((self.ty >> 16) & 0xFF) as u8
    }

    pub fn encode(&self) -> Vec<u8> {
        let len = 13 + self.payload.len();
        let mut b = Vec::with_capacity(len);
        b.push(0x55);
        b.push((len & 0xFF) as u8);
        b.push((1 << 2) | ((len >> 8) & 0x03) as u8);
        b.push(crc8(&b[..3]));
        b.extend(self.target.to_le_bytes());
        b.extend(self.id.to_le_bytes());
        b.extend(&self.ty.to_le_bytes()[..3]);
        b.extend(&self.payload);
        let c = crc16(&b);
        b.extend(c.to_le_bytes());
        b
    }

    pub fn decode(data: &[u8]) -> Result<Frame, FrameError> {
        if data.len() < 13 {
            return Err(FrameError::TooShort);
        }
        if data[0] != 0x55 {
            return Err(FrameError::BadSof);
        }
        if data[2] >> 2 != 1 {
            return Err(FrameError::BadVersion);
        }
        let len = (data[1] as usize | (data[2] as usize) << 8) & 0x3FF;
        if len != data.len() {
            return Err(FrameError::BadLength);
        }
        if crc8(&data[..3]) != data[3] {
            return Err(FrameError::BadHeaderCrc);
        }
        let n = data.len();
        if crc16(&data[..n - 2]) != u16::from_le_bytes([data[n - 2], data[n - 1]]) {
            return Err(FrameError::BadCrc);
        }
        Ok(Frame {
            target: u16::from_le_bytes([data[4], data[5]]),
            id: u16::from_le_bytes([data[6], data[7]]),
            ty: u32::from_le_bytes([data[8], data[9], data[10], 0]),
            payload: data[11..n - 2].to_vec(),
        })
    }
}

/// DJI string field: `[len:u8][utf8]`.
pub fn pack_string(s: &str) -> Vec<u8> {
    let mut v = vec![s.len() as u8];
    v.extend(s.as_bytes());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn session_wake_frame_matches_doc() {
        let f = Frame { target: 0xF002, id: 0xCB1B, ty: 0x2B0040, payload: vec![4, 0] };
        assert_eq!(f.encode(), hex("550f04a202f01bcb40002b04009ab9"));
    }

    #[test]
    fn session_keepalive_frame_matches_doc() {
        let f = Frame { target: 0xF002, id: 0xCB1B, ty: 0x2B0040, payload: vec![1, 1] };
        assert_eq!(f.encode(), hex("550f04a202f01bcb40002b0101abd6"));
    }

    #[test]
    fn roundtrip() {
        let f = Frame { target: 0x0702, id: 0x1234, ty: 0x112233, payload: vec![1, 2, 3] };
        assert_eq!(Frame::decode(&f.encode()).unwrap(), f);
    }

    #[test]
    fn type_accessors() {
        let f = Frame { target: 0x0702, id: 1, ty: 0x450740, payload: vec![] };
        assert_eq!((f.flags(), f.cmd_set(), f.cmd_id()), (0x40, 0x07, 0x45));
    }

    #[test]
    fn pair_message_is_51_bytes() {
        let mut payload = pack_string("284ae5b8d76b3375a04a6417ad71bea3");
        payload.extend(pack_string("osmo"));
        let bytes = Frame { target: 0x0702, id: 0x8092, ty: 0x450740, payload }.encode();
        assert_eq!(bytes.len(), 51);
        assert_eq!(bytes[1], 51);
        assert_eq!(bytes[2], 0x04);
    }

    #[test]
    fn decode_rejects_bad_crc() {
        let mut b = Frame { target: 1, id: 2, ty: 3, payload: vec![9] }.encode();
        let n = b.len();
        b[n - 1] ^= 0xFF;
        assert_eq!(Frame::decode(&b), Err(FrameError::BadCrc));
    }

    #[test]
    fn decode_rejects_corrupt_header() {
        let mut b = Frame { target: 1, id: 2, ty: 3, payload: vec![9] }.encode();
        b[2] = 0x05;
        assert!(Frame::decode(&b).is_err());
    }

    #[test]
    fn decode_rejects_wrong_length() {
        let b = Frame { target: 1, id: 2, ty: 3, payload: vec![9, 9] }.encode();
        assert_eq!(Frame::decode(&b[..b.len() - 1]), Err(FrameError::BadLength));
    }

    #[test]
    fn pack_string_prefixes_length() {
        assert_eq!(pack_string("osmo"), vec![4, b'o', b's', b'm', b'o']);
    }
}
