//! Camera media session over the DUML datalink: handshake, registration, playback mode and the
//! newest-page file list. Ported from Osmosis `DumlSession.kt` / `CameraSession.kt` for the Action 4
//! path (single page, no delete/favourite/burst, no time sync, no `0x01/0x01` playback route).
use super::manifest::collect_stores;
use super::stream::{count_media_paths, manifest_bytes, streams_ended};
use super::transport::{Datagram, Transport, UdpPeer};
use crate::duml::commands::{set_pairing_pin, APP_DEVICE_INFO, DEFAULT_IDENTIFIER};
use crate::duml::{find_reply, scan_frames};
use crate::media::CameraFile;
use std::io;
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};
use thiserror::Error;

/// Registered once and beaten ~1 Hz: tells the camera the app is still here (holds playback mode).
const APP_PRESENCE: [u8; 14] = [
    0x17, 0x00, 0x46, 0x23, 0x7c, 0x41, 0x50, 0x50, 0, 0, 0, 0, 0, 0x02,
];
/// Eight status keys, deliberately not Mimo's 54: more floods the manifest stream with parameter pushes.
const PARAM_SUBS: [&str; 8] = [
    "camcap_mode_profile",
    "camcap_video_format",
    "camcap_fov",
    "camcap_iso",
    "camcap_photo_storage_format",
    "camcap_color_mode",
    "cam_storage",
    "cam_status",
];
const ENTER_PLAYBACK: [u8; 4] = [1, 1, 0, 1];
const LEAVE_PLAYBACK: [u8; 4] = [1, 1, 0, 0];
const LIST_TRIGGER: [u8; 14] = [0x4a, 0x04, 0x0e, 0x10, 0x01, 0, 0, 0, 0, 0, 0x01, 0, 0, 0];
const NEWEST_SD: u32 = 0x0000_0001;
const NEWEST_INTERNAL: u32 = 0x4000_0001;
const PAGE_SIZE: u8 = 45;
const SD_CTR: u8 = 1;
const INTERNAL_CTR: u8 = 2;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// The handshake (SYN) payload: our base sequence, then the window/MTU parameters the official app offers.
fn handshake_frame(base_seq: u16) -> Vec<u8> {
    let mut h =
        hex("000064006400c005140000640000019001c005140000640014006400c00514000064000101040102");
    h[..2].copy_from_slice(&base_seq.to_le_bytes());
    h
}

fn subscription(name: &str, id: u32) -> Vec<u8> {
    let mut v = vec![0x02, 0x02, 0x00, 0x00];
    v.extend(id.to_le_bytes());
    v.extend([0, 0, 0]);
    v.extend(((name.len() + 6) as u16).to_le_bytes());
    v.extend((name.len() as u16).to_le_bytes());
    v.extend(name.as_bytes());
    v.extend([0, 0, 0, 0]);
    v
}

/// `0x00/0x26` media-list query: counter at byte 4, cursor at 10..14 (top bit selects the store), page size at 14.
fn list_cmd(ctr: u8, cursor: u32) -> Vec<u8> {
    let mut c =
        hex("4a002a10010000000000010000002d000d0100ffffffffffffffff000100000000000000000000000000");
    c[4] = ctr;
    c[10..14].copy_from_slice(&cursor.to_le_bytes());
    c[14] = PAGE_SIZE;
    c
}

/// The frame written to TCP 7001 to arm the datalink on Osmo 360 / Nano / Pocket 3 / Action 4.
pub fn tcp_poke_bytes() -> Vec<u8> {
    set_pairing_pin("osmo", DEFAULT_IDENTIFIER, 0x8092)
}

/// Best-effort poke: failures are ignored, as the camera may not need it.
pub fn tcp_poke_to(addr: SocketAddr, settle: Duration) {
    use std::io::Write;
    if let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(1200)) {
        let _ = s.write_all(&tcp_poke_bytes());
        let _ = s.flush();
        std::thread::sleep(settle);
    }
}

#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub ip: String,
    pub port: u16,
    pub tcp_poke: bool,
    pub poke_port: u16,
}

impl SessionConfig {
    /// Osmo Action 4: UDP 9004, TCP-7001 poke required (hardware-verified in Osmosis).
    pub fn action4(ip: &str) -> Self {
        SessionConfig {
            ip: ip.to_string(),
            port: 9004,
            tcp_poke: true,
            poke_port: 7001,
        }
    }
}

/// Every wait in the flow, so tests can run it in milliseconds.
#[derive(Debug, Clone)]
pub struct Timing {
    pub handshake_attempts: u32,
    pub handshake_wait: Duration,
    pub drain_wait: Duration,
    pub reg_wait: Duration,
    pub sub_wait: Duration,
    pub poll: Duration,
    pub playback_probe: Duration,
    pub playback_attempt: Duration,
    /// A store is not mounted the instant playback is confirmed; allow about 1.7 s before the first query.
    pub playback_settle: Duration,
    pub batch: Duration,
    pub retry_batch: Duration,
    pub max_batches: u32,
    pub tick: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        let ms = Duration::from_millis;
        Timing {
            handshake_attempts: 20,
            handshake_wait: ms(350),
            drain_wait: ms(400),
            reg_wait: ms(400),
            sub_wait: ms(20),
            poll: ms(100),
            playback_probe: ms(120),
            playback_attempt: ms(900),
            playback_settle: ms(1700),
            batch: ms(800),
            retry_batch: ms(700),
            max_batches: 15,
            tick: ms(200),
        }
    }
}

impl Timing {
    /// Millisecond waits for tests against a fake peer.
    pub fn fast() -> Self {
        let ms = Duration::from_millis;
        Timing {
            handshake_attempts: 20,
            handshake_wait: ms(1),
            drain_wait: ms(1),
            reg_wait: ms(1),
            sub_wait: ms(1),
            poll: ms(1),
            playback_probe: ms(2),
            playback_attempt: ms(20),
            playback_settle: ms(2),
            batch: ms(3),
            retry_batch: ms(3),
            max_batches: 15,
            tick: ms(1),
        }
    }
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("no response from {addr}: is this machine joined to the camera's WiFi?")]
    NoHandshake { addr: String },
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub struct CameraSession<D: Datagram> {
    tx: Transport<D>,
    addr: String,
    timing: Timing,
    playback_reported: Option<bool>,
    playback_held: bool,
    last_presence: Instant,
    last_blob: Vec<u8>,
}

impl CameraSession<UdpPeer> {
    /// Poke TCP 7001 if the model needs it, then bind a UDP socket to the camera.
    pub fn open(cfg: &SessionConfig) -> Result<Self, SessionError> {
        if cfg.tcp_poke {
            if let Ok(a) = format!("{}:{}", cfg.ip, cfg.poke_port).parse() {
                tcp_poke_to(a, Duration::from_millis(400));
            }
        }
        let udp = UdpPeer::connect(&cfg.ip, cfg.port)?;
        Ok(Self::new(
            udp,
            format!("{}:{}", cfg.ip, cfg.port),
            Timing::default(),
        ))
    }
}

impl<D: Datagram> CameraSession<D> {
    pub fn new(d: D, addr: String, timing: Timing) -> Self {
        let mut tx = Transport::random(d);
        tx.handshake_wait = timing.handshake_wait;
        CameraSession {
            tx,
            addr,
            timing,
            playback_reported: None,
            playback_held: false,
            last_presence: Instant::now(),
            last_blob: Vec::new(),
        }
    }

    pub fn base_seq(&self) -> u16 {
        self.tx.base_seq()
    }

    pub fn playback_held(&self) -> bool {
        self.playback_held
    }

    /// Raw reply datagrams of the last listing. Re-decodable with `decode_manifest`, so a live capture
    /// can be turned into a regression fixture. Contains file names and sizes, no credentials.
    pub fn last_blob(&self) -> &[u8] {
        &self.last_blob
    }

    fn recv(&mut self, dur: Duration) -> Vec<Vec<u8>> {
        let got = self.tx.recv_all(dur);
        self.parse_status(&got);
        got
    }

    /// The camera's own answer to "am I in playback" is bit 30 of the `0x02/0x80` flags word.
    fn parse_status(&mut self, datagrams: &[Vec<u8>]) {
        for d in datagrams {
            for f in scan_frames(d) {
                if f.set == 0x02 && f.cmd == 0x80 && f.payload.len() >= 13 {
                    let flags = u32::from_le_bytes([
                        f.payload[0],
                        f.payload[1],
                        f.payload[2],
                        f.payload[3],
                    ]);
                    self.playback_reported = Some(flags & 0x4000_0000 != 0);
                }
            }
        }
    }

    fn send(&mut self, set: u8, cmd: u8, payload: &[u8], recv_type: u8, recv_id: u8) {
        self.tx.send_duml(set, cmd, payload, recv_type, recv_id, 2);
    }

    fn beat(&mut self) {
        self.send(0x00, 0x88, &APP_PRESENCE, 0x08, 1);
        self.last_presence = Instant::now();
    }

    fn settle(&mut self, wait: Duration) {
        self.recv(wait);
        self.tx.send_ack();
    }

    fn open_and_register(&mut self) -> Result<(), SessionError> {
        let hs = handshake_frame(self.tx.base_seq());
        if self
            .tx
            .handshake(&hs, self.timing.handshake_attempts)
            .is_none()
        {
            return Err(SessionError::NoHandshake {
                addr: self.addr.clone(),
            });
        }
        for _ in 0..5 {
            self.settle(self.timing.drain_wait);
        }
        self.tx.sync_seq_to_peer_channel();

        self.tx.send_duml(0x00, 0x81, &APP_DEVICE_INFO, 0x08, 2, 4);
        self.settle(self.timing.reg_wait);
        self.beat();
        self.settle(self.timing.reg_wait);
        self.send(0x03, 0xDA, &hex("05ffffffff"), 0x03, 0);
        self.settle(self.timing.reg_wait);
        for (i, name) in PARAM_SUBS.iter().enumerate() {
            self.send(0x00, 0x99, &subscription(name, 0x69DF + i as u32), 0x08, 1);
            if i % 8 == 7 {
                self.settle(self.timing.sub_wait);
            }
        }
        for _ in 0..4 {
            self.settle(self.timing.reg_wait);
        }
        Ok(())
    }

    /// Enter playback mode and wait for the camera itself to say so (bit 30 of `0x02/0x80`); the
    /// command's own ack only means "received". Not fatal if it never confirms.
    fn enter_playback(&mut self) -> bool {
        self.recv(self.timing.playback_probe);
        if self.playback_reported == Some(true) {
            self.playback_held = true;
            return true;
        }
        for _ in 0..3 {
            self.send(0x02, 0x0C, &ENTER_PLAYBACK, 0x01, 0);
            let end = Instant::now() + self.timing.playback_attempt;
            while Instant::now() < end {
                let dg = self.recv(self.timing.poll);
                if self.playback_reported == Some(true) {
                    self.playback_held = true;
                    return true;
                }
                if find_reply(&dg, 0x02, 0x0C).is_some_and(|r| r[0] != 0) {
                    return false;
                }
                self.tx.send_ack();
            }
        }
        false
    }

    /// Hold playback with the presence beat while the camera finishes mounting its stores.
    fn settle_after_playback(&mut self) {
        let end = Instant::now() + self.timing.playback_settle;
        while Instant::now() < end {
            self.beat();
            self.settle(
                self.timing
                    .poll
                    .min(end.saturating_duration_since(Instant::now())),
            );
        }
    }

    fn collect(&mut self, blob: &mut Vec<u8>, wait: Duration) {
        for r in self.recv(wait) {
            blob.extend(r);
        }
        self.tx.send_ack();
        self.beat();
    }

    fn query_newest(&mut self) -> Vec<CameraFile> {
        let mut blob = Vec::new();
        self.send(0x00, 0x26, &list_cmd(SD_CTR, NEWEST_SD), 0x01, 0);
        let (mut last, mut stable) = (None::<usize>, 0);
        for batch in 0..self.timing.max_batches {
            self.collect(&mut blob, self.timing.batch);
            if batch == 1 {
                self.send(0x00, 0x26, &LIST_TRIGGER, 0x01, 0);
            }
            if batch == 2 {
                self.send(
                    0x00,
                    0x26,
                    &list_cmd(INTERNAL_CTR, NEWEST_INTERNAL),
                    0x01,
                    0,
                );
            }
            let count = count_media_paths(&manifest_bytes(&blob, None));
            if batch >= 3 && streams_ended(&blob, &[SD_CTR, INTERNAL_CTR]) {
                break;
            }
            if batch >= 4 && count > 0 && last == Some(count) {
                stable += 1;
                if stable >= 2 {
                    break;
                }
            } else {
                stable = 0;
            }
            last = Some(count);
        }
        self.retry_sd_if_empty(&mut blob);
        let files = collect_stores(&blob, SD_CTR, INTERNAL_CTR);
        self.last_blob = blob;
        files
    }

    /// A store can answer empty because it was not mounted yet; ask the SD once more after a delay.
    fn retry_sd_if_empty(&mut self, blob: &mut Vec<u8>) {
        if !manifest_bytes(blob, Some(SD_CTR)).is_empty() {
            return;
        }
        self.send(0x00, 0x26, &list_cmd(SD_CTR, NEWEST_SD), 0x01, 0);
        for batch in 0..6 {
            self.collect(blob, self.timing.retry_batch);
            if batch == 1 {
                self.send(0x00, 0x26, &LIST_TRIGGER, 0x01, 0);
            }
        }
    }

    /// Handshake, register, enter playback and return the newest page (45 records per store).
    /// An empty card yields `Ok(vec![])`.
    pub fn list_newest(&mut self) -> Result<Vec<CameraFile>, SessionError> {
        self.open_and_register()?;
        self.enter_playback();
        self.settle_after_playback();
        Ok(self.query_newest())
    }

    /// Call every few hundred ms to keep the link (and the camera's WiFi AP) up while idle.
    pub fn keepalive_tick(&mut self) {
        self.recv(self.timing.tick);
        self.tx.send_ack();
        if self.last_presence.elapsed() >= Duration::from_secs(1) {
            self.beat();
        }
    }

    /// Release playback mode (the camera stays in it otherwise) and stop.
    pub fn close(&mut self) {
        if self.playback_held {
            self.send(0x02, 0x0C, &LEAVE_PLAYBACK, 0x01, 0);
            self.recv(Duration::from_millis(150));
            self.playback_held = false;
        }
    }
}
