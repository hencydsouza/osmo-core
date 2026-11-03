//! DJI R-SDK framing and GPS payloads. Wire layouts are ported from Osmosis's RsdkProtocol.
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("invalid R-SDK frame")]
    Frame,
    #[error("R-SDK frame is too large")]
    TooLarge,
    #[error("invalid GPS fix: {0}")]
    InvalidFix(&'static str),
}

fn crc16(bytes: &[u8]) -> u16 {
    let mut c = 0x3aa3u16;
    for &b in bytes {
        c ^= b as u16;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xa001
            } else {
                c >> 1
            };
        }
    }
    c
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut c = 0x3aa3u32;
    for &b in bytes {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xedb88320
            } else {
                c >> 1
            };
        }
    }
    c
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub set: u8,
    pub id: u8,
    pub kind: u8,
    pub seq: u16,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let len = 18 + self.payload.len();
        if len > 0x3ff {
            return Err(Error::TooLarge);
        }
        let mut bytes = vec![0xaa, len as u8, (len >> 8) as u8, self.kind, 0, 0, 0, 0];
        bytes.extend(self.seq.to_le_bytes());
        bytes.extend(crc16(&bytes).to_le_bytes());
        bytes.extend([self.set, self.id]);
        bytes.extend(&self.payload);
        bytes.extend(crc32(&bytes).to_le_bytes());
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < 18 || bytes[0] != 0xaa || bytes[2] & 0xfc != 0 || bytes[4] != 0 {
            return Err(Error::Frame);
        }
        let len = u16::from_le_bytes([bytes[1], bytes[2]]) as usize;
        if len != bytes.len() || crc16(&bytes[..10]) != u16::from_le_bytes([bytes[10], bytes[11]]) {
            return Err(Error::Frame);
        }
        let end = len - 4;
        if crc32(&bytes[..end])
            != u32::from_le_bytes(bytes[end..].try_into().map_err(|_| Error::Frame)?)
        {
            return Err(Error::Frame);
        }
        Ok(Self {
            kind: bytes[3],
            seq: u16::from_le_bytes([bytes[8], bytes[9]]),
            set: bytes[12],
            id: bytes[13],
            payload: bytes[14..end].to_vec(),
        })
    }

    pub fn is_response(&self) -> bool {
        self.kind & 0x20 != 0
    }
}

/// Reassembles fragmented BLE notifications, resynchronizing after corrupt or unrelated bytes.
#[derive(Default)]
pub struct Reassembler {
    buffer: Vec<u8>,
}

impl Reassembler {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Frame> {
        self.buffer.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            let Some(start) = self.buffer.iter().position(|b| *b == 0xaa) else {
                self.buffer.clear();
                break;
            };
            self.buffer.drain(..start);
            if self.buffer.len() < 3 {
                break;
            }
            let len = u16::from_le_bytes([self.buffer[1], self.buffer[2]]) as usize;
            if !(18..=0x3ff).contains(&len) || self.buffer[2] & 0xfc != 0 {
                self.buffer.remove(0);
                continue;
            }
            if self.buffer.len() < 12 {
                break;
            }
            if crc16(&self.buffer[..10]) != u16::from_le_bytes([self.buffer[10], self.buffer[11]]) {
                self.buffer.remove(0);
                continue;
            }
            if self.buffer.len() < len {
                break;
            }
            if let Ok(frame) = Frame::decode(&self.buffer[..len]) {
                out.push(frame);
                self.buffer.drain(..len);
            } else {
                self.buffer.remove(0);
            }
        }
        out
    }
}

pub fn connection_request(
    device_id: u32,
    mac: [u8; 6],
    verify_mode: u8,
    verify_data: u16,
) -> [u8; 33] {
    let mut p = [0; 33];
    p[..4].copy_from_slice(&device_id.to_le_bytes());
    p[4] = 6;
    p[5..11].copy_from_slice(&mac);
    p[26] = verify_mode;
    p[27..29].copy_from_slice(&verify_data.to_le_bytes());
    p
}

pub fn connection_response(device_id: u32, ret_code: u8) -> [u8; 9] {
    let mut p = [0; 9];
    p[..4].copy_from_slice(&device_id.to_le_bytes());
    p[4] = ret_code;
    p
}

pub fn status_subscription(mode: u8, frequency: u8) -> [u8; 6] {
    [mode, frequency, 0, 0, 0, 0]
}

/// One GPS fix in DJI's units. The date/time are local wall-clock values, not UTC or an epoch.
pub struct GpsFix {
    pub date: u32,
    pub time: u32,
    pub longitude_e7: i32,
    pub latitude_e7: i32,
    pub height_mm: i32,
    pub north_cm_s: f32,
    pub east_cm_s: f32,
    pub down_cm_s: f32,
    pub vertical_accuracy_mm: u32,
    pub horizontal_accuracy_mm: u32,
    pub speed_accuracy_cm_s: u32,
    pub satellites: u32,
}

pub fn gps_payload(fix: &GpsFix) -> Result<[u8; 48], Error> {
    if !(-1_800_000_000..=1_800_000_000).contains(&fix.longitude_e7)
        || !(-900_000_000..=900_000_000).contains(&fix.latitude_e7)
    {
        return Err(Error::InvalidFix("coordinates out of range"));
    }
    if ![fix.north_cm_s, fix.east_cm_s, fix.down_cm_s]
        .iter()
        .all(|v| v.is_finite())
    {
        return Err(Error::InvalidFix("velocity must be finite"));
    }
    let year = fix.date / 10000;
    let month = (fix.date / 100) % 100;
    let day = fix.date % 100;
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        _ => 0,
    };
    if year == 0
        || day == 0
        || day > max_day
        || fix.time / 10000 > 23
        || (fix.time / 100) % 100 > 59
        || fix.time % 100 > 59
    {
        return Err(Error::InvalidFix("invalid local date or time"));
    }
    let mut p = [0; 48];
    let fields = [
        fix.date.to_le_bytes(),
        fix.time.to_le_bytes(),
        fix.longitude_e7.to_le_bytes(),
        fix.latitude_e7.to_le_bytes(),
        fix.height_mm.to_le_bytes(),
        fix.north_cm_s.to_le_bytes(),
        fix.east_cm_s.to_le_bytes(),
        fix.down_cm_s.to_le_bytes(),
        fix.vertical_accuracy_mm.to_le_bytes(),
        fix.horizontal_accuracy_mm.to_le_bytes(),
        fix.speed_accuracy_cm_s.to_le_bytes(),
        fix.satellites.to_le_bytes(),
    ];
    for (chunk, value) in p.chunks_exact_mut(4).zip(fields) {
        chunk.copy_from_slice(&value);
    }
    Ok(p)
}
