//! DJI R-SDK framing and GPS payloads. Wire layouts are ported from Osmosis's RsdkProtocol.
use crate::ble::{BleLink, LinkError};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
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
    if [
        fix.vertical_accuracy_mm,
        fix.horizontal_accuracy_mm,
        fix.speed_accuracy_cm_s,
        fix.satellites,
    ]
    .iter()
    .any(|v| *v > i32::MAX as u32)
    {
        return Err(Error::InvalidFix(
            "accuracy or satellite count out of range",
        ));
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
    if !(1..=9999).contains(&year)
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
    #[allow(unknown_lints, clippy::chunks_exact_to_as_chunks)]
    // as_chunks needs a newer Rust than we support
    for (chunk, value) in p.chunks_exact_mut(4).zip(fields) {
        chunk.copy_from_slice(&value);
    }
    Ok(p)
}

/// Stable identity used by the camera to remember R-SDK controller approval.
#[derive(Clone, Copy)]
pub struct Identity {
    pub device_id: u32,
    pub mac: [u8; 6],
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error(transparent)]
    Protocol(#[from] Error),
    #[error(transparent)]
    Link(#[from] LinkError),
    #[error("camera rejected the R-SDK connection ({0})")]
    Rejected(String),
    #[error("camera did not approve the R-SDK connection in time")]
    Timeout,
    #[error("operation interrupted")]
    Cancelled,
}

/// Camera state received on an R-SDK status subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraStatus {
    pub mode: u8,
    pub state: u8,
    pub resolution: u8,
    pub fps: u8,
    pub record_seconds: u16,
    pub battery_percent: Option<u8>,
}

fn camera_status(p: &[u8]) -> Option<CameraStatus> {
    if p.len() < 7 {
        return None;
    }
    Some(CameraStatus {
        mode: p[0],
        state: p[1],
        resolution: p[2],
        fps: p[3],
        record_seconds: u16::from_le_bytes([p[5], p[6]]),
        battery_percent: (p.len() > 7).then(|| p[p.len() - 1]).filter(|v| *v <= 100),
    })
}

/// An approved R-SDK connection; caller supplies fresh GPS fixes and polls for camera messages.
pub struct Session<'a> {
    link: &'a mut dyn BleLink,
    decoder: Reassembler,
    pending: VecDeque<Frame>,
    seq: u16,
    last_status: Option<Instant>,
    last_subscription: Instant,
}

impl Session<'_> {
    fn next_seq(&mut self) -> u16 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }

    fn send(
        &mut self,
        set: u8,
        id: u8,
        kind: u8,
        payload: &[u8],
        seq: u16,
    ) -> Result<(), SessionError> {
        let wire = Frame {
            set,
            id,
            kind,
            seq,
            payload: payload.to_vec(),
        }
        .encode()?;
        self.link.write_command(&wire)?;
        Ok(())
    }

    fn receive(&mut self, wait: Duration) -> Result<Option<Frame>, SessionError> {
        if let Some(frame) = self.pending.pop_front() {
            return Ok(Some(frame));
        }
        if let Some(bytes) = self.link.recv(wait.min(Duration::from_millis(50)))? {
            self.pending.extend(self.decoder.feed(&bytes));
        }
        Ok(self.pending.pop_front())
    }

    /// Send a GPS fix. The caller is responsible for ensuring it is fresh and supplying local wall time.
    pub fn send_fix(&mut self, fix: &GpsFix) -> Result<(), SessionError> {
        let payload = gps_payload(fix)?;
        let seq = self.next_seq();
        self.send(0, 0x17, 0, &payload, seq)
    }

    /// Receive a camera status push for up to `wait`. Other frames are consumed silently.
    pub fn poll(&mut self, wait: Duration) -> Result<Option<CameraStatus>, SessionError> {
        let end = Instant::now() + wait;
        while Instant::now() < end {
            let stale = self
                .last_status
                .is_none_or(|last| last.elapsed() > Duration::from_secs(2));
            if stale && self.last_subscription.elapsed() >= Duration::from_millis(1500) {
                let seq = self.next_seq();
                self.send(0x1d, 5, 0, &status_subscription(3, 20), seq)?;
                self.last_subscription = Instant::now();
            }
            if let Some(frame) = self.receive(end.saturating_duration_since(Instant::now()))? {
                if (frame.set, frame.id) == (0x1d, 0x02) {
                    if let Some(status) = camera_status(&frame.payload) {
                        self.last_status = Some(Instant::now());
                        return Ok(Some(status));
                    }
                } else if (frame.set, frame.id) == (0x1d, 0x06)
                    && frame.payload.len() >= 46
                    && frame.payload[0] == 1
                    && frame.payload[23] == 2
                {
                    self.last_status = Some(Instant::now());
                }
            }
        }
        Ok(None)
    }
}

/// Request R-SDK approval over an already-open BLE link. `verify_data` is the request's challenge value.
/// Does not send GPS before the camera approves. Live: an Action 4 approved this and took fixes (2026-11-07).
pub fn connect<'a>(
    link: &'a mut dyn BleLink,
    identity: Identity,
    verify_data: u16,
    timeout: Duration,
    cancelled: &dyn Fn() -> bool,
) -> Result<Session<'a>, SessionError> {
    if cancelled() {
        return Err(SessionError::Cancelled);
    }
    let mut session = Session {
        link,
        decoder: Reassembler::default(),
        pending: VecDeque::new(),
        seq: 0,
        last_status: None,
        last_subscription: Instant::now(),
    };
    let request = connection_request(identity.device_id, identity.mac, 0, verify_data);
    let seq = session.next_seq();
    session.send(0, 0x19, 2, &request, seq)?;
    let end = Instant::now() + timeout;
    loop {
        if cancelled() {
            return Err(SessionError::Cancelled);
        }
        if Instant::now() >= end {
            return Err(SessionError::Timeout);
        }
        let Some(f) = session.receive(end.saturating_duration_since(Instant::now()))? else {
            continue;
        };
        if (f.set, f.id) != (0, 0x19) {
            continue;
        }
        if f.is_response() {
            if let Some(&code) = f.payload.get(4).filter(|&&c| c != 0) {
                return Err(SessionError::Rejected(format!("response code {code}")));
            }
            continue;
        }
        if f.payload.len() < 29 || f.payload[26] != 2 {
            continue;
        }
        let verify = u16::from_le_bytes([f.payload[27], f.payload[28]]);
        if verify != 0 {
            return Err(SessionError::Rejected(format!("verify data {verify}")));
        }
        session.send(
            0,
            0x19,
            0x20,
            &connection_response(identity.device_id, 0),
            f.seq,
        )?;
        let seq = session.next_seq();
        session.send(0x1d, 5, 0, &status_subscription(3, 20), seq)?;
        session.last_subscription = Instant::now();
        return Ok(session);
    }
}
