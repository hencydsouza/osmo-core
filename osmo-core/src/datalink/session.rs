//! Camera media session over the DUML datalink: handshake, registration, playback mode and the
//! newest-page file list. Ported from Osmosis `DumlSession.kt` / `CameraSession.kt` for the Action 4
//! path (single page, no delete/favourite/burst, no time sync, no `0x01/0x01` playback route).
use super::manifest::collect_stores;
use super::paging::{
    seed_cursors, slice_info, step_pagination, SliceInfo, NEWEST_INTERNAL, NEWEST_SD,
};
use super::stream::{chunk_tally, count_media_paths, manifest_bytes, streams_ended};
use super::transport::{Datagram, Transport, UdpPeer};
use super::writes::{
    delete_payload, favourite_payload, highlight_payload, parse_highlights, set_time_payload,
    WriteError,
};
use crate::duml::commands::{set_pairing_pin, APP_DEVICE_INFO, DEFAULT_IDENTIFIER};
use crate::duml::{find_reply, scan_frames};
use crate::media::CameraFile;
use crate::status::Status;
use std::collections::{HashMap, HashSet};
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
/// `0x01/0x01` playback entry (Action 4, Pocket 3): about six prelude frames, then the enter payload at
/// ~20 Hz until the camera's own playback bit sets (~350 ms in). `cmd_type 0`, no reply.
const CONTROL_PRELUDE: [u8; 11] = [0x03, 0, 0, 0, 0, 0x04, 0, 0, 0, 0x07, 0x01];
const CONTROL_ENTER: [u8; 11] = [0x00, 0, 0, 0, 0, 0x04, 0, 0, 0, 0x04, 0x01];
const CONTROL_PRELUDE_FRAMES: u32 = 6;
const CONTROL_SILENT_LIMIT: u32 = 8;
const LIST_TRIGGER: [u8; 14] = [0x4a, 0x04, 0x0e, 0x10, 0x01, 0, 0, 0, 0, 0, 0x01, 0, 0, 0];
/// Records asked for per store per query (byte 14 of the `0x00/0x26` payload); Mimo's own page size.
const DEFAULT_PAGE_SIZE: u8 = 45;
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
fn list_cmd(ctr: u8, cursor: u32, page_size: u8) -> Vec<u8> {
    let mut c =
        hex("4a002a10010000000000010000002d000d0100ffffffffffffffff000100000000000000000000000000");
    c[4] = ctr;
    c[10..14].copy_from_slice(&cursor.to_le_bytes());
    c[14] = page_size;
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
    /// How long to keep sending `0x01/0x01` before giving up on playback.
    pub control_window: Duration,
    pub control_poll: Duration,
    /// A store is not mounted the instant playback is confirmed; allow about 1.7 s before the first query.
    pub playback_settle: Duration,
    pub batch: Duration,
    pub retry_batch: Duration,
    pub max_batches: u32,
    pub tick: Duration,
    /// A write is refused on a session registered longer ago than this: the camera stops answering writes on
    /// old sessions (measured: answered at 45 to 66 s, silent from 74 s; Osmosis refreshes at 40 s).
    pub write_window: Duration,
    /// How long to wait for a write's reply before reporting no answer.
    pub write_reply_wait: Duration,
    /// How long to wait for a delete's reply (the camera takes about a second; a large clip can take longer).
    pub delete_reply_wait: Duration,
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
            control_window: ms(2500),
            control_poll: ms(50),
            playback_settle: ms(1700),
            batch: ms(800),
            retry_batch: ms(700),
            max_batches: 15,
            tick: ms(200),
            write_window: Duration::from_secs(40),
            write_reply_wait: Duration::from_secs(2),
            delete_reply_wait: Duration::from_secs(10),
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
            control_window: ms(60),
            control_poll: ms(1),
            playback_settle: ms(2),
            batch: ms(3),
            retry_batch: ms(3),
            max_batches: 15,
            tick: ms(1),
            write_window: Duration::from_secs(5),
            write_reply_wait: ms(30),
            delete_reply_wait: ms(30),
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

type Logger = Box<dyn FnMut(&str) + Send>;

/// What the last listing observed, so callers can warn instead of silently showing a partial list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diagnostics {
    /// The camera itself reported playback mode (bit 30 of `0x02/0x80`). Without it a camera still in
    /// capture answers the list with nothing or a partial page.
    pub playback_confirmed: bool,
    /// A page arrived with fewer records than its header promised, even after one retry: the listing may be
    /// missing files, and the library must not be treated as finished.
    pub truncated: bool,
}

pub struct CameraSession<D: Datagram> {
    tx: Transport<D>,
    addr: String,
    timing: Timing,
    playback_reported: Option<bool>,
    playback_held: bool,
    last_presence: Instant,
    last_blob: Vec<u8>,
    logger: Option<Logger>,
    status: Status,
    page_size: u8,
    sd_cursor: u32,
    internal_cursor: u32,
    seen: HashSet<(u8, String)>,
    more_available: bool,
    last_slices: HashMap<u8, SliceInfo>,
    truncated: bool,
    registered_at: Option<Instant>,
    write_counter: u32,
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
            logger: None,
            status: Status::default(),
            page_size: DEFAULT_PAGE_SIZE,
            sd_cursor: NEWEST_SD,
            internal_cursor: NEWEST_INTERNAL,
            seen: HashSet::new(),
            more_available: false,
            last_slices: HashMap::new(),
            truncated: false,
            registered_at: None,
            write_counter: 1,
        }
    }

    /// Receive one line per protocol event (handshake, playback route, each batch, per-store counts).
    pub fn set_logger(&mut self, f: impl FnMut(&str) + Send + 'static) {
        self.logger = Some(Box::new(f));
    }

    fn log(&mut self, msg: impl AsRef<str>) {
        if let Some(l) = self.logger.as_mut() {
            l(msg.as_ref());
        }
    }

    /// Records asked for per store per query, 1 to 255 (default 45). A small value lets paging be exercised on a
    /// small card.
    pub fn set_page_size(&mut self, n: u8) {
        self.page_size = n.max(1);
    }

    /// Is there probably an older page? Meaningful after `list_newest`.
    pub fn more_available(&self) -> bool {
        self.more_available
    }

    /// Battery and storage as last pushed by the camera.
    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn base_seq(&self) -> u16 {
        self.tx.base_seq()
    }

    pub fn diagnostics(&self) -> Diagnostics {
        Diagnostics {
            playback_confirmed: self.playback_held,
            truncated: self.truncated,
        }
    }

    pub fn playback_held(&self) -> bool {
        self.playback_held
    }

    /// Raw reply datagrams of the last listing. Re-decodable with `decode_manifest`, so a live capture
    /// can be turned into a regression fixture. Holds file names and sizes plus the camera's status pushes
    /// (device info, firmware, battery), so review it before sharing.
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
                self.status.apply(f.set, f.cmd, &f.payload);
            }
        }
    }

    fn send(&mut self, set: u8, cmd: u8, payload: &[u8], recv_type: u8, recv_id: u8) {
        self.tx.send_duml(set, cmd, payload, recv_type, recv_id, 2);
    }

    fn send_typed(
        &mut self,
        set: u8,
        cmd: u8,
        payload: &[u8],
        recv_type: u8,
        recv_id: u8,
        cmd_type: u8,
    ) {
        self.tx
            .send_duml(set, cmd, payload, recv_type, recv_id, cmd_type);
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
        let (sess, base, chan) = (
            self.tx.session(),
            self.tx.base_seq(),
            self.tx.peer_channel(),
        );
        self.log(format!(
            "datalink: handshake ok session=0x{sess:04x} base=0x{base:04x} channel=0x{chan:04x}"
        ));
        if chan != base {
            self.log("datalink: the camera answered on its own sequence channel, not the base we proposed: it may be holding a session from a previous connection");
        }

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
        self.registered_at = Some(Instant::now());
        Ok(())
    }

    /// Enter playback mode and wait for the camera itself to say so (bit 30 of `0x02/0x80`); a command's
    /// own ack only means "received". Tries `0x02/0x0C` first; a refusal (`e0`, as the Action 4 and
    /// Pocket 3 answer) or no change falls through to the `0x01/0x01` route. Not fatal if neither works,
    /// but a camera still in capture serves an empty or partial list.
    fn enter_playback(&mut self) -> bool {
        self.recv(self.timing.playback_probe);
        if self.playback_reported == Some(true) {
            self.playback_held = true;
            self.log("playback: already held (the camera says so)");
            return true;
        }
        for _ in 0..3 {
            self.send(0x02, 0x0C, &ENTER_PLAYBACK, 0x01, 0);
            let end = Instant::now() + self.timing.playback_attempt;
            while Instant::now() < end {
                let dg = self.recv(self.timing.poll);
                if self.playback_reported == Some(true) {
                    self.playback_held = true;
                    self.log("playback: held via 0x02/0x0C");
                    return true;
                }
                if let Some(r) = find_reply(&dg, 0x02, 0x0C).filter(|r| r[0] != 0) {
                    // A refusal is definitive: re-sending a command the body does not implement cannot help.
                    self.log(format!(
                        "playback: 0x02/0x0C refused (0x{:02x}), trying 0x01/0x01",
                        r[0]
                    ));
                    return self.enter_playback_via_control();
                }
                self.tx.send_ack();
            }
        }
        self.enter_playback_via_control()
    }

    fn enter_playback_via_control(&mut self) -> bool {
        let end = Instant::now() + self.timing.control_window;
        let (mut sent, mut silent) = (0u32, 0u32);
        while Instant::now() < end {
            let payload = if sent < CONTROL_PRELUDE_FRAMES {
                CONTROL_PRELUDE
            } else {
                CONTROL_ENTER
            };
            self.send_typed(0x01, 0x01, &payload, 0x01, 0, 0);
            sent += 1;
            let got = self.recv(self.timing.control_poll);
            if self.playback_reported == Some(true) {
                self.playback_held = true;
                self.log(format!("playback: held via 0x01/0x01 after {sent} frames"));
                return true;
            }
            // cmd_type 0 has no reply, so the state bit is the only feedback; stop writing unknown control
            // commands into a camera that has gone quiet (it may be powering down).
            silent = if got.is_empty() { silent + 1 } else { 0 };
            if silent >= CONTROL_SILENT_LIMIT {
                self.log(format!(
                    "playback: 0x01/0x01 camera went quiet after {sent} frames, giving up"
                ));
                return false;
            }
        }
        self.log(format!("playback: NOT confirmed after {sent} 0x01/0x01 frames: the camera still reports capture"));
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

    /// One page query pair at the given cursors (SD under counter 1, the trigger frame, internal under counter 2),
    /// collected until the camera has closed its answers or the path count stops growing.
    fn query_page(
        &mut self,
        sd_cursor: u32,
        internal_cursor: u32,
        wait: Duration,
        retry_empty_sd: bool,
    ) -> Vec<CameraFile> {
        let mut blob = Vec::new();
        let ps = self.page_size;
        self.send(0x00, 0x26, &list_cmd(SD_CTR, sd_cursor, ps), 0x01, 0);
        let (mut last, mut stable) = (None::<usize>, 0);
        for batch in 0..self.timing.max_batches {
            self.collect(&mut blob, wait);
            if batch == 1 {
                self.send(0x00, 0x26, &LIST_TRIGGER, 0x01, 0);
            }
            if batch == 2 {
                self.send(
                    0x00,
                    0x26,
                    &list_cmd(INTERNAL_CTR, internal_cursor, ps),
                    0x01,
                    0,
                );
            }
            let count = count_media_paths(&manifest_bytes(&blob, None));
            self.log(format!(
                "list: batch {batch}: {} bytes, {count} paths, replies by counter {:?}",
                blob.len(),
                chunk_tally(&blob)
            ));
            if batch >= 3 && streams_ended(&blob, &[SD_CTR, INTERNAL_CTR]) {
                self.log("list: every answer closed by the camera's end frame");
                break;
            }
            if batch >= 4 && count > 0 && last == Some(count) {
                stable += 1;
                if stable >= 2 {
                    self.log("list: path count stopped growing");
                    break;
                }
            } else {
                stable = 0;
            }
            last = Some(count);
        }
        if retry_empty_sd {
            self.retry_sd_if_empty(&mut blob, sd_cursor);
        }
        let files = collect_stores(&blob, SD_CTR, INTERNAL_CTR);
        self.last_slices = HashMap::from([
            (0, slice_info(&blob, SD_CTR)),
            (1, slice_info(&blob, INTERNAL_CTR)),
        ]);
        let sd = files.iter().filter(|f| f.storage == 0).count();
        self.log(format!(
            "list: {} files (sd {sd}, internal {})",
            files.len(),
            files.len() - sd
        ));
        self.last_blob = blob;
        files
    }

    /// `query_page`, asked once more with doubled waits if a store's answer arrived cut short (fewer records
    /// than its header promised). Whether it still is cut is left in `truncated`.
    fn query_page_checked(
        &mut self,
        sd_cursor: u32,
        internal_cursor: u32,
        retry_empty_sd: bool,
    ) -> Vec<CameraFile> {
        let wait = self.timing.batch;
        let mut files = self.query_page(sd_cursor, internal_cursor, wait, retry_empty_sd);
        if self.last_slices.values().any(SliceInfo::incomplete) {
            self.log("list: a store's answer was cut short, asking again with longer waits");
            files = self.query_page(sd_cursor, internal_cursor, wait * 2, false);
        }
        self.truncated = self.last_slices.values().any(SliceInfo::incomplete);
        if self.truncated {
            self.log("list: STILL cut short; the listing may be missing files");
        }
        files
    }

    /// A store can answer empty because it was not mounted yet; ask the SD once more after a delay.
    fn retry_sd_if_empty(&mut self, blob: &mut Vec<u8>, sd_cursor: u32) {
        if !manifest_bytes(blob, Some(SD_CTR)).is_empty() {
            return;
        }
        self.log("list: SD answered empty, asking once more (it may not have been mounted yet)");
        let ps = self.page_size;
        self.send(0x00, 0x26, &list_cmd(SD_CTR, sd_cursor, ps), 0x01, 0);
        for batch in 0..6 {
            self.collect(blob, self.timing.retry_batch);
            if batch == 1 {
                self.send(0x00, 0x26, &LIST_TRIGGER, 0x01, 0);
            }
        }
    }

    /// Handshake, register, enter playback and return the newest page (`set_page_size`, default 45, records per
    /// store). An empty card yields `Ok(vec![])`.
    pub fn list_newest(&mut self) -> Result<Vec<CameraFile>, SessionError> {
        self.open_and_register()?;
        self.enter_playback();
        self.settle_after_playback();
        let files = self.query_page_checked(NEWEST_SD, NEWEST_INTERNAL, true);
        self.seen = files.iter().map(|f| (f.storage, f.path.clone())).collect();
        (self.sd_cursor, self.internal_cursor, self.more_available) =
            seed_cursors(&files, &self.last_slices, self.page_size as usize);
        Ok(files)
    }

    /// Continue the write counter sequence from an earlier process. Every write carries a per-request counter, and
    /// Osmosis restarts its counters at 1 per session, so a repeated value is probably harmless; a continued
    /// sequence is kept anyway (unproven either way). One sequence is shared by every
    /// kind of write; a caller that makes one session per run must persist `next_write_counter()` and pass it
    /// here next time, starting above anything it used before.
    pub fn set_write_counter_start(&mut self, n: u32) {
        self.write_counter = n;
    }

    /// The counter the next write will use.
    pub fn next_write_counter(&self) -> u32 {
        self.write_counter
    }

    /// How long ago this session finished registering; a write is only attempted while this is within
    /// `Timing.write_window`.
    pub fn session_age(&self) -> Duration {
        self.registered_at.map_or(Duration::MAX, |t| t.elapsed())
    }

    /// Every write goes through here: the camera itself must report playback (a write outside it is ignored), and
    /// the session must be young enough for the camera to still answer writes.
    fn write_guard(&self) -> Result<(), WriteError> {
        if self.playback_reported != Some(true) {
            return Err(WriteError::NotInPlayback);
        }
        let age = self.session_age();
        if age > self.timing.write_window {
            return Err(WriteError::SessionTooOld {
                age_secs: age.as_secs(),
            });
        }
        Ok(())
    }

    /// Wait for the camera's reply to a write on `set/cmd`: status 0 is success, anything else a refusal, and
    /// silence is `NoReply` (never success).
    fn write_reply(&mut self, set: u8, cmd: u8, wait: Duration) -> Result<(), WriteError> {
        let r = self
            .await_reply(set, cmd, wait)
            .ok_or(WriteError::NoReply)?;
        let status = if r.len() >= 2 {
            u16::from_le_bytes([r[0], r[1]])
        } else {
            r[0] as u16
        };
        if status == 0 {
            Ok(())
        } else {
            Err(WriteError::Rejected(status))
        }
    }

    /// The first non-empty reply payload to `set/cmd` within `wait`, acknowledging as it waits.
    fn await_reply(&mut self, set: u8, cmd: u8, wait: Duration) -> Option<Vec<u8>> {
        let end = Instant::now() + wait;
        while Instant::now() < end {
            let dg = self.recv(self.timing.poll);
            if let Some(r) = find_reply(&dg, set, cmd) {
                return Some(r);
            }
            self.tx.send_ack();
        }
        None
    }

    /// A video's highlight marks in ms (`0x02/0xFF`, read-only), addressed by its manifest handle. Needs playback
    /// held, like every per-file command. An empty list means the clip has no marks.
    pub fn highlights(&mut self, file: &CameraFile) -> Result<Vec<u32>, WriteError> {
        if file.handle == 0 {
            return Err(WriteError::NoHandle);
        }
        self.write_guard()?;
        self.send(0x02, 0xFF, &highlight_payload(file.handle), 0x01, 0);
        self.log(format!("read: highlights 0x{:08x}", file.handle));
        let reply = self
            .await_reply(0x02, 0xFF, self.timing.write_reply_wait)
            .ok_or(WriteError::NoReply)?;
        let marks = parse_highlights(&reply);
        self.log(format!("read: highlights reply {marks:?}"));
        marks
    }

    /// Favourite or un-favourite a file (`0x02/0xBF`, addressed by its command handle). Must follow a listing on
    /// this session, shortly before: it refuses rather than send a write the camera would ignore.
    pub fn set_favourite(&mut self, file: &CameraFile, on: bool) -> Result<(), WriteError> {
        if file.cmd_handle == 0 {
            return Err(WriteError::NoHandle);
        }
        self.write_guard()?;
        let counter = self.write_counter;
        self.write_counter = self.write_counter.wrapping_add(1);
        self.send(
            0x02,
            0xBF,
            &favourite_payload(file.cmd_handle, counter, on),
            0x01,
            0,
        );
        self.log(format!(
            "write: favourite 0x{:08x} on={on}",
            file.cmd_handle
        ));
        let result = self.write_reply(0x02, 0xBF, self.timing.write_reply_wait);
        self.log(format!("write: favourite reply {result:?}"));
        result
    }

    /// Handshake and register if this session has not yet; a no-op afterwards. `list_newest` does this itself
    /// (and re-handshakes if called twice), so call `register` only before commands that need no listing.
    pub fn register(&mut self) -> Result<(), SessionError> {
        if self.registered_at.is_some() {
            return Ok(());
        }
        self.open_and_register()
    }

    /// Set the camera's clock and time zone (`0x00/0x6A`, to the system receiver; the file receiver drops it).
    /// Needs no playback. `NoReply` means sent but not acknowledged: check the camera's clock.
    pub fn sync_time(
        &mut self,
        unix_secs: u64,
        utc_offset_min: i16,
        tz_id: &str,
    ) -> Result<(), WriteError> {
        if !tz_id.is_ascii() || tz_id.len() > 255 {
            return Err(WriteError::BadZoneName);
        }
        if self.registered_at.is_none() {
            return Err(WriteError::NotRegistered);
        }
        self.send(
            0x00,
            0x6A,
            &set_time_payload(unix_secs, utc_offset_min, tz_id),
            0x08,
            1,
        );
        self.log(format!(
            "write: time {unix_secs} UTC{utc_offset_min:+}min {tz_id}"
        ));
        self.write_reply(0x00, 0x6A, self.timing.write_reply_wait)
    }

    /// Delete files (`0x00/0x28`). IRREVERSIBLE. Every file is checked first (a verified handle nobody else
    /// holds), and if any is unsafe nothing at all is sent. The camera's "OK" is not proof: the caller must
    /// re-list to confirm, as a favourite "OK" has been seen to do nothing.
    pub fn delete_files(&mut self, files: &[&CameraFile]) -> Result<(), WriteError> {
        const MAX: usize = 20;
        if files.len() > MAX {
            return Err(WriteError::TooMany { max: MAX });
        }
        if let Some(f) = files.iter().find(|f| !f.deletable()) {
            return Err(WriteError::NotDeletable(f.path.clone()));
        }
        if files.is_empty() {
            return Ok(());
        }
        self.write_guard()?;
        let handles: Vec<u32> = files.iter().map(|f| f.handle).collect();
        let counter = self.write_counter;
        self.write_counter = self.write_counter.wrapping_add(1);
        self.send(0x00, 0x28, &delete_payload(&handles, counter), 0x01, 0);
        self.log(format!(
            "write: delete {} file(s) {handles:08x?}",
            files.len()
        ));
        let result = self.write_reply(0x00, 0x28, self.timing.delete_reply_wait);
        self.log(format!("write: delete reply {result:?}"));
        result
    }

    /// Find a file by its camera path, walking from the newest page back through older ones until it turns up or
    /// the library is exhausted. Starts with a fresh listing, so call it on a session that has not listed yet.
    pub fn find_file(&mut self, path: &str) -> Result<Option<CameraFile>, SessionError> {
        if let Some(f) = self.list_newest()?.into_iter().find(|f| f.path == path) {
            return Ok(Some(f));
        }
        while self.more_available() {
            let next = self.list_next_page()?;
            if next.is_empty() {
                break;
            }
            if let Some(f) = next.into_iter().find(|f| f.path == path) {
                return Ok(Some(f));
            }
        }
        Ok(None)
    }

    /// The next older page on the same session, returning only files not seen before; empty once the library is
    /// exhausted. A page that stays cut short is still returned (and flagged in `diagnostics().truncated`), and
    /// does not end the walk.
    pub fn list_next_page(&mut self) -> Result<Vec<CameraFile>, SessionError> {
        if !self.more_available {
            return Ok(Vec::new());
        }
        let page = self.query_page_checked(self.sd_cursor, self.internal_cursor, false);
        let step = step_pagination(
            self.sd_cursor,
            self.internal_cursor,
            &page,
            &mut self.seen,
            &self.last_slices,
            self.page_size as usize,
        );
        (self.sd_cursor, self.internal_cursor, self.more_available) =
            (step.sd_cursor, step.internal_cursor, step.more_available);
        self.log(format!(
            "list: next page: {} new, cursors sd=0x{:08x} internal=0x{:08x}, more={}",
            step.fresh.len(),
            step.sd_cursor,
            step.internal_cursor,
            step.more_available
        ));
        Ok(step.fresh)
    }

    /// Call every few hundred ms to keep the link (and the camera's WiFi AP) up while idle.
    pub fn keepalive_tick(&mut self) {
        self.recv(self.timing.tick);
        self.tx.send_ack();
        if self.last_presence.elapsed() >= Duration::from_secs(1) {
            self.beat();
        }
    }

    /// Release playback mode (the camera stays in it otherwise) and stop. Idempotent; also runs on drop, so
    /// an early return cannot leave the camera in playback.
    pub fn close(&mut self) {
        if self.playback_held {
            self.send(0x02, 0x0C, &LEAVE_PLAYBACK, 0x01, 0);
            self.recv(Duration::from_millis(150));
            self.playback_held = false;
        }
    }
}

impl<D: Datagram> Drop for CameraSession<D> {
    fn drop(&mut self) {
        self.close();
    }
}
