//! BLE pairing and WiFi-credential retrieval, independent of any BLE stack.
//! Sequence (Osmosis `MEDIA_PROTOCOL.md` "Waking a sleeping camera", §22-26):
//! wake `0x00/0x2b 04 00` -> SetPairingPIN `0x07/0x45` -> keepalive `0x00/0x2b 01 01` (about 1 Hz)
//! -> `0x53/0x10` (wakes the camera and its WiFi AP) -> GetWifiSsid / GetWifiPassword / GetWifiMac, with a gap between `fff5` writes.
use super::link::{BleLink, LinkError};
use super::reassembler::Reassembler;
use crate::duml::commands::*;
use crate::duml::rtmp;
use crate::duml::Frame;
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiCreds {
    pub ssid: String,
    /// Some cameras (the Nano) may not return a password over BLE.
    pub password: Option<String>,
    pub mac: Option<[u8; 6]>,
}

#[derive(Debug, Clone)]
pub struct PairOptions {
    pub identifier: String,
    pub pin: String,
    /// How long to wait for a human to approve on the camera.
    pub approve_timeout: Duration,
    pub step_timeout: Duration,
    /// Minimum spacing between consecutive `fff5` writes.
    pub write_gap: Duration,
    pub keepalive_interval: Duration,
}

impl Default for PairOptions {
    fn default() -> Self {
        PairOptions {
            identifier: DEFAULT_IDENTIFIER.to_string(),
            pin: "osmo".to_string(),
            approve_timeout: Duration::from_secs(60),
            step_timeout: Duration::from_secs(8),
            write_gap: Duration::from_millis(150),
            keepalive_interval: Duration::from_secs(1),
        }
    }
}

#[derive(Debug, Error)]
pub enum PairError {
    #[error("pairing was not approved on the camera")]
    NotApproved,
    #[error("no reply from the camera to {0}")]
    NoReply(&'static str),
    #[error(transparent)]
    Link(#[from] LinkError),
}

struct Session<'a> {
    link: &'a mut dyn BleLink,
    opts: &'a PairOptions,
    reasm: Reassembler,
    pending: VecDeque<Frame>,
    approved: bool,
    woken: bool,
    last_write: Option<Instant>,
    last_keepalive: Instant,
    next_id: u16,
    battery: Option<u8>,
}

impl<'a> Session<'a> {
    fn new(link: &'a mut dyn BleLink, opts: &'a PairOptions) -> Self {
        Self {
            link,
            opts,
            reasm: Reassembler::default(),
            pending: VecDeque::new(),
            approved: false,
            woken: false,
            last_write: None,
            last_keepalive: Instant::now(),
            next_id: 0xA000,
            battery: None,
        }
    }

    fn wake_and_pair(&mut self, on_step: &mut dyn FnMut(&str)) -> Result<(), PairError> {
        on_step("Waking the camera");
        let id = self.id();
        self.write(&session_ping(&SESSION_WAKE, id))?;
        self.woken = true;
        self.last_keepalive = Instant::now();
        on_step("Pairing");
        self.pair(on_step)?;
        self.keepalive()
    }

    fn id(&mut self) -> u16 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), PairError> {
        if let Some(t) = self.last_write {
            let el = t.elapsed();
            if el < self.opts.write_gap {
                std::thread::sleep(self.opts.write_gap - el);
            }
        }
        self.link.write_command(bytes)?;
        self.last_write = Some(Instant::now());
        Ok(())
    }

    fn keepalive(&mut self) -> Result<(), PairError> {
        let id = self.id();
        self.write(&session_ping(&SESSION_KEEPALIVE, id))?;
        self.last_keepalive = Instant::now();
        Ok(())
    }

    /// Receive for up to `wait`, answering the camera's own requests and queueing replies.
    fn pump(&mut self, wait: Duration) -> Result<bool, PairError> {
        if self.woken && self.last_keepalive.elapsed() >= self.opts.keepalive_interval {
            self.keepalive()?;
        }
        let Some(frag) = self.link.recv(wait.min(Duration::from_millis(50)))? else {
            return Ok(false);
        };
        for f in self.reasm.feed(&frag) {
            if f.flags() == 0x40 {
                // The camera drops the link if its own requests go unanswered.
                let payload = if f.cmd_set() == 0x00 && f.cmd_id() == 0x81 {
                    APP_DEVICE_INFO.to_vec()
                } else {
                    f.payload.clone()
                };
                let ack = Frame {
                    target: f.target.swap_bytes(),
                    id: f.id,
                    ty: (f.ty & 0xFFFF00) | 0xC0,
                    payload,
                };
                self.write(&ack.encode())?;
                if f.cmd_set() == 0x07 && f.cmd_id() == 0x46 {
                    self.approved = true;
                }
            } else if f.cmd_set() == 0x0D && f.cmd_id() == 0x02 {
                let pct = if f.payload.len() >= 21 {
                    f.payload.get(20)
                } else {
                    f.payload.get(12)
                };
                if let Some(&pct) = pct.filter(|&&v| v <= 100) {
                    self.battery = Some(pct);
                }
            } else {
                self.pending.push_back(f);
            }
        }
        Ok(true)
    }

    fn take_reply(&mut self, set: u8, cmd: u8) -> Option<Frame> {
        let p = self
            .pending
            .iter()
            .position(|f| f.flags() & 0x80 != 0 && f.cmd_set() == set && f.cmd_id() == cmd)?;
        self.pending.remove(p)
    }

    fn await_reply(
        &mut self,
        set: u8,
        cmd: u8,
        timeout: Duration,
    ) -> Result<Option<Frame>, PairError> {
        let end = Instant::now() + timeout;
        loop {
            if let Some(f) = self.take_reply(set, cmd) {
                return Ok(Some(f));
            }
            let now = Instant::now();
            if now >= end {
                return Ok(None);
            }
            self.pump(end - now)?;
        }
    }

    fn pair(&mut self, on_step: &mut dyn FnMut(&str)) -> Result<(), PairError> {
        let deadline = Instant::now() + self.opts.approve_timeout;
        let (mut got_reply, mut prompted) = (false, false);
        loop {
            self.pending.clear();
            let id = self.id();
            let frame = set_pairing_pin(&self.opts.pin, &self.opts.identifier, id);
            self.write(&frame)?;
            let step_end = Instant::now() + self.opts.step_timeout;
            while Instant::now() < step_end && !self.approved {
                if let Some(f) = self.take_reply(0x07, 0x45) {
                    got_reply = true;
                    match f.payload.get(1) {
                        Some(1) => return Ok(()),
                        Some(2) if !prompted => {
                            prompted = true;
                            on_step("Approve pairing on the camera");
                        }
                        _ => {}
                    }
                }
                self.pump(step_end.saturating_duration_since(Instant::now()))?;
            }
            if self.approved {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(if got_reply {
                    PairError::NotApproved
                } else {
                    PairError::NoReply("SetPairingPIN")
                });
            }
        }
    }

    /// Request with one retry: `fff5` is write-without-response, so frames can be dropped.
    fn query(&mut self, cmd: u8) -> Result<Option<Frame>, PairError> {
        for _ in 0..2 {
            self.pending.clear();
            let id = self.id();
            self.write(&wifi_query(cmd, &[], id))?;
            if let Some(f) = self.await_reply(0x07, cmd, self.opts.step_timeout)? {
                return Ok(Some(f));
            }
        }
        Ok(None)
    }
}

/// `[status:1][PackString]` reply body, `None` on a non-zero status or truncated string.
fn parse_string(f: &Frame) -> Option<String> {
    let p = &f.payload;
    if p.first() != Some(&0) {
        return None;
    }
    let len = *p.get(1)? as usize;
    String::from_utf8(p.get(2..2 + len)?.to_vec())
        .ok()
        .filter(|s| !s.is_empty())
}

pub fn pair_and_read_wifi(
    link: &mut dyn BleLink,
    opts: &PairOptions,
    on_step: &mut dyn FnMut(&str),
) -> Result<WifiCreds, PairError> {
    let mut s = Session::new(link, opts);
    s.wake_and_pair(on_step)?;
    // `0x53/0x10` (to type 0x1C, not the camera) is what actually wakes a sleeping camera and brings its
    // WiFi AP up; the camera answers `01 00 00 00`. Fire and forget, as Osmosis does.
    let id = s.id();
    s.write(&session_5310(id))?;

    on_step("Reading WiFi credentials");
    let ssid = s
        .query(CMD_WIFI_SSID)?
        .as_ref()
        .and_then(parse_string)
        .ok_or(PairError::NoReply("ssid"))?;
    let password = s.query(CMD_WIFI_PASSWORD)?.as_ref().and_then(parse_string);
    let mac = s
        .query(CMD_WIFI_MAC)?
        .and_then(|f| match f.payload.as_slice() {
            [0, m @ ..] if m.len() >= 6 => Some([m[0], m[1], m[2], m[3], m[4], m[5]]),
            _ => None,
        });
    Ok(WifiCreds {
        ssid,
        password,
        mac,
    })
}

/// Livestream configuration. Keep the URL and Wi-Fi password private; both may contain secrets.
pub struct StreamConfig<'a> {
    pub ssid: &'a str,
    pub password: &'a str,
    pub url: &'a str,
    /// 0x47 = 480p, 0x04 = 720p, 0x0a = 1080p.
    pub resolution: u8,
    pub bitrate_kbps: u16,
    /// 0x02 = 25 fps, 0x03 = 30 fps.
    pub fps: u8,
    /// Action 4/5 Pro stabilization setting 0–4; `None` skips the command.
    pub stabilization: Option<u8>,
    pub action5_pro: bool,
}

#[derive(Debug, Error)]
pub enum StreamError {
    #[error(transparent)]
    Pair(#[from] PairError),
    #[error(transparent)]
    Build(#[from] rtmp::BuildError),
    #[error("invalid livestream setting: {0}")]
    Invalid(&'static str),
    #[error("no camera reply to {0}")]
    NoReply(&'static str),
    #[error("camera rejected {step} (status 0x{status:02x})")]
    Rejected { step: &'static str, status: u8 },
}

fn stream_request(
    s: &mut Session<'_>,
    frame: &[u8],
    set: u8,
    cmd: u8,
    step: &'static str,
    attempts: usize,
) -> Result<Frame, StreamError> {
    for _ in 0..attempts {
        // Discard previously queued replies before a new request: several stream steps share 0x02/0x8e.
        for _ in 0..64 {
            if !s.pump(Duration::ZERO)? {
                break;
            }
        }
        s.pending.clear();
        s.write(frame)?;
        if let Some(reply) = s.await_reply(set, cmd, s.opts.step_timeout)? {
            return Ok(reply);
        }
    }
    Err(StreamError::NoReply(step))
}

/// An active BLE livestream. Call `poll` regularly to maintain the connection and observe battery pushes.
/// Dropping it attempts to send stop; a successful send does not prove that the camera stopped.
pub struct LiveStream<'a> {
    session: Session<'a>,
    active: bool,
}

impl LiveStream<'_> {
    /// Service keepalives and incoming camera requests for up to `wait`, returning a new battery percentage.
    pub fn poll(&mut self, wait: Duration) -> Result<Option<u8>, StreamError> {
        if !self.active {
            return Err(StreamError::Invalid("stream already stopped"));
        }
        if let Some(pct) = self.session.battery.take() {
            return Ok(Some(pct));
        }
        let end = Instant::now() + wait;
        while Instant::now() < end {
            self.session
                .pump(end.saturating_duration_since(Instant::now()))?;
            if let Some(pct) = self.session.battery.take() {
                return Ok(Some(pct));
            }
        }
        Ok(None)
    }

    /// Send the stop frame. The camera does not provide a verified stop acknowledgment here.
    pub fn stop(&mut self) -> Result<(), StreamError> {
        if self.active {
            self.session.write(&rtmp::stop_stream())?;
            self.active = false;
        }
        Ok(())
    }
}

impl Drop for LiveStream<'_> {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Pair over the already-open BLE link, send stream setup, and retain the link for the stream lifetime.
/// Fixture-tested; not yet live-verified in osmo-core. Joining the supplied Wi-Fi turns off the camera's AP.
pub fn start_livestream<'a>(
    link: &'a mut dyn BleLink,
    opts: &'a PairOptions,
    config: &StreamConfig<'_>,
    on_step: &mut dyn FnMut(&str),
) -> Result<LiveStream<'a>, StreamError> {
    if config.ssid.is_empty() {
        return Err(StreamError::Invalid("Wi-Fi SSID is empty"));
    }
    if config.url.is_empty() {
        return Err(StreamError::Invalid("RTMP URL is empty"));
    }
    if !matches!(config.resolution, 0x47 | 0x04 | 0x0a) {
        return Err(StreamError::Invalid("unsupported resolution"));
    }
    if !matches!(config.fps, 2 | 3) {
        return Err(StreamError::Invalid("unsupported frame rate"));
    }
    if config.bitrate_kbps == 0 {
        return Err(StreamError::Invalid("bitrate is zero"));
    }
    if config.stabilization.is_some_and(|v| v > 4) {
        return Err(StreamError::Invalid("unsupported stabilization"));
    }
    // Validate length before making any camera changes.
    let wifi = rtmp::connect_wifi(config.ssid, config.password, 0x8c19)?;
    let configure = rtmp::configure_stream(
        config.resolution,
        config.bitrate_kbps,
        config.fps,
        config.url,
        config.action5_pro,
    )?;
    let mut s = Session::new(link, opts);
    s.wake_and_pair(on_step)?;
    on_step("Preparing stream");
    let reply = stream_request(
        &mut s,
        &rtmp::prepare_stage1(),
        2,
        0xe1,
        "prepare stage 1",
        2,
    )?;
    match reply.payload.first() {
        Some(0) => {}
        Some(&status) => {
            return Err(StreamError::Rejected {
                step: "prepare stage 1",
                status,
            })
        }
        None => return Err(StreamError::Invalid("empty prepare response")),
    }
    stream_request(
        &mut s,
        &rtmp::prepare_stage2(),
        2,
        0x8e,
        "prepare stage 2",
        2,
    )?;
    on_step("Connecting camera to Wi-Fi");
    let reply = stream_request(&mut s, &wifi, 7, 0x47, "Wi-Fi connection", 3)?;
    if reply.payload.len() < 2 {
        return Err(StreamError::Invalid("short Wi-Fi response"));
    }
    if reply.payload[0] != 0 {
        return Err(StreamError::Rejected {
            step: "Wi-Fi connection",
            status: reply.payload[0],
        });
    }
    if let Some(eis) = config.stabilization {
        on_step("Setting stabilization");
        stream_request(
            &mut s,
            &rtmp::stabilization(eis, config.action5_pro),
            2,
            0x8e,
            "stabilization",
            2,
        )?;
    }
    on_step("Configuring stream");
    let reply = stream_request(&mut s, &configure, 8, 0x78, "stream configuration", 2)?;
    if let Some(&status) = reply.payload.first().filter(|&&v| v != 0) {
        return Err(StreamError::Rejected {
            step: "stream configuration",
            status,
        });
    }
    on_step("Starting stream");
    if let Err(e) = stream_request(&mut s, &rtmp::start_stream(), 2, 0x8e, "stream start", 2) {
        // A lost acknowledgement does not prove the camera ignored start.
        let _ = s.write(&rtmp::stop_stream());
        return Err(e);
    }
    Ok(LiveStream {
        session: s,
        active: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ble::link::{BleLink, LinkError};
    use crate::duml::{pack_string, Frame};
    use std::collections::VecDeque;
    use std::time::{Duration, Instant};

    struct Rule {
        set: u8,
        cmd: u8,
        replies: Vec<Vec<u8>>,
    }

    struct FakeLink {
        rules: Vec<Rule>,
        incoming: VecDeque<Vec<u8>>,
        writes: Vec<(Instant, Frame)>,
    }

    impl FakeLink {
        fn new() -> Self {
            FakeLink {
                rules: vec![],
                incoming: VecDeque::new(),
                writes: vec![],
            }
        }
        fn on(mut self, set: u8, cmd: u8, replies: Vec<Vec<u8>>) -> Self {
            self.rules.push(Rule { set, cmd, replies });
            self
        }
        fn inject(mut self, f: Vec<u8>) -> Self {
            self.incoming.push_back(f);
            self
        }
        fn sent(&self, set: u8, cmd: u8) -> Vec<&Frame> {
            self.writes
                .iter()
                .map(|(_, f)| f)
                .filter(|f| f.cmd_set() == set && f.cmd_id() == cmd)
                .collect()
        }
    }

    impl BleLink for FakeLink {
        fn write_command(&mut self, b: &[u8]) -> Result<(), LinkError> {
            let f = Frame::decode(b).expect("session wrote a malformed frame");
            if f.flags() == 0x40 {
                if let Some(p) = self
                    .rules
                    .iter()
                    .position(|r| r.set == f.cmd_set() && r.cmd == f.cmd_id())
                {
                    let r = self.rules.remove(p);
                    self.incoming.extend(r.replies);
                }
            }
            self.writes.push((Instant::now(), f));
            Ok(())
        }
        fn recv(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, LinkError> {
            if let Some(x) = self.incoming.pop_front() {
                return Ok(Some(x));
            }
            std::thread::sleep(timeout.min(Duration::from_millis(10)));
            Ok(None)
        }
    }

    fn frame(flags: u32, set: u8, cmd: u8, id: u16, payload: &[u8]) -> Vec<u8> {
        Frame {
            target: 0x0207,
            id,
            ty: flags | (set as u32) << 8 | (cmd as u32) << 16,
            payload: payload.to_vec(),
        }
        .encode()
    }
    fn resp(set: u8, cmd: u8, payload: &[u8]) -> Vec<u8> {
        frame(0xC0, set, cmd, 0xA000, payload)
    }
    fn req(set: u8, cmd: u8, payload: &[u8]) -> Vec<u8> {
        frame(0x40, set, cmd, 0x1234, payload)
    }
    fn string_reply(cmd: u8, s: &str) -> Vec<u8> {
        let mut p = vec![0];
        p.extend(pack_string(s));
        resp(7, cmd, &p)
    }
    fn fast() -> PairOptions {
        PairOptions {
            identifier: DEFAULT_IDENTIFIER.into(),
            pin: "osmo".into(),
            approve_timeout: Duration::from_millis(300),
            step_timeout: Duration::from_millis(60),
            write_gap: Duration::from_millis(15),
            keepalive_interval: Duration::from_secs(60),
        }
    }
    fn creds_rules(l: FakeLink) -> FakeLink {
        l.on(7, 0x07, vec![string_reply(0x07, "OsmoAction4-1234")])
            .on(7, 0x0e, vec![string_reply(0x0e, "secret99")])
            .on(7, 0x0c, vec![resp(7, 0x0c, &[0, 1, 2, 3, 4, 5, 6])])
    }

    #[test]
    fn happy_path_returns_creds_in_documented_order() {
        let mut link = creds_rules(FakeLink::new().on(7, 0x45, vec![resp(7, 0x45, &[0, 1])]));
        let creds = pair_and_read_wifi(&mut link, &fast(), &mut |_| {}).unwrap();
        assert_eq!(creds.ssid, "OsmoAction4-1234");
        assert_eq!(creds.password.as_deref(), Some("secret99"));
        assert_eq!(creds.mac, Some([1, 2, 3, 4, 5, 6]));
        let seq: Vec<(u8, u8)> = link
            .writes
            .iter()
            .map(|(_, f)| (f.cmd_set(), f.cmd_id()))
            .collect();
        assert_eq!(
            seq,
            vec![
                (0, 0x2b),
                (7, 0x45),
                (0, 0x2b),
                (0x53, 0x10),
                (7, 0x07),
                (7, 0x0e),
                (7, 0x0c)
            ]
        );
        assert_eq!(link.writes[0].1.payload, vec![4, 0]);
        assert_eq!(link.writes[2].1.payload, vec![1, 1]);
        let wake = &link.writes[3].1;
        assert_eq!(
            (wake.target, wake.payload.clone()),
            (0x1C02, vec![0, 0, 0, 0]),
            "0x53/0x10 goes to type 0x1C, not the camera"
        );
        for w in link.writes.windows(2) {
            assert!(
                w[1].0 - w[0].0 >= Duration::from_millis(15),
                "writes closer than the gap"
            );
        }
    }

    #[test]
    fn approval_flow_acks_the_0x46_request_and_proceeds() {
        let pair = vec![resp(7, 0x45, &[0, 2]), req(7, 0x46, &[1])];
        let mut link = creds_rules(FakeLink::new().on(7, 0x45, pair));
        let mut steps = vec![];
        pair_and_read_wifi(&mut link, &fast(), &mut |s| steps.push(s.to_string())).unwrap();
        let ack = link.sent(7, 0x46);
        assert_eq!(ack.len(), 1);
        assert_eq!(
            (ack[0].flags(), ack[0].id, ack[0].target),
            (0xC0, 0x1234, 0x0702)
        );
        assert!(steps.iter().any(|s| s.contains("Approve")));
    }

    #[test]
    fn inbound_00_81_gets_app_device_info_reply() {
        let mut link = creds_rules(
            FakeLink::new()
                .on(7, 0x45, vec![resp(7, 0x45, &[0, 1])])
                .inject(req(0, 0x81, &[9, 9])),
        );
        pair_and_read_wifi(&mut link, &fast(), &mut |_| {}).unwrap();
        let r = link.sent(0, 0x81);
        assert_eq!(r.len(), 1);
        assert_eq!(
            (r[0].flags(), r[0].payload.as_slice()),
            (0xC0, &APP_DEVICE_INFO[..])
        );
    }

    #[test]
    fn keepalive_sent_repeatedly_while_waiting() {
        let mut link = FakeLink::new().on(7, 0x45, vec![resp(7, 0x45, &[0, 2])]);
        let o = PairOptions {
            approve_timeout: Duration::from_millis(250),
            keepalive_interval: Duration::from_millis(30),
            ..fast()
        };
        let _ = pair_and_read_wifi(&mut link, &o, &mut |_| {});
        let pings = link
            .sent(0, 0x2b)
            .into_iter()
            .filter(|f| f.payload == vec![1, 1])
            .count();
        assert!(pings >= 3, "only {pings} keepalives");
    }

    #[test]
    fn never_approved_times_out() {
        let mut link = FakeLink::new().on(7, 0x45, vec![resp(7, 0x45, &[0, 2])]);
        let o = PairOptions {
            approve_timeout: Duration::from_millis(150),
            step_timeout: Duration::from_millis(40),
            ..fast()
        };
        let t = Instant::now();
        let r = pair_and_read_wifi(&mut link, &o, &mut |_| {});
        assert!(matches!(r, Err(PairError::NotApproved)), "{r:?}");
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn silent_camera_reports_no_reply_not_approval() {
        let mut link = FakeLink::new();
        let o = PairOptions {
            approve_timeout: Duration::from_millis(100),
            step_timeout: Duration::from_millis(30),
            ..fast()
        };
        assert!(matches!(
            pair_and_read_wifi(&mut link, &o, &mut |_| {}),
            Err(PairError::NoReply(_))
        ));
    }

    #[test]
    fn password_and_mac_missing_are_none_not_error() {
        let mut link = FakeLink::new()
            .on(7, 0x45, vec![resp(7, 0x45, &[0, 1])])
            .on(7, 0x07, vec![string_reply(0x07, "OsmoNano-C2D8")]);
        let creds = pair_and_read_wifi(&mut link, &fast(), &mut |_| {}).unwrap();
        assert_eq!((creds.password, creds.mac), (None, None));
    }

    #[test]
    fn missing_ssid_is_an_error() {
        let mut link = FakeLink::new().on(7, 0x45, vec![resp(7, 0x45, &[0, 1])]);
        assert!(matches!(
            pair_and_read_wifi(&mut link, &fast(), &mut |_| {}),
            Err(PairError::NoReply("ssid"))
        ));
    }
}
