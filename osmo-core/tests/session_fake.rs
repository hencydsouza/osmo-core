//! Datalink session against a scripted fake camera that replays the captured manifests.
//! Fixture-tested only: this proves our side of the protocol, not that a real camera accepts it.
use osmo_core::datalink::{
    decode_manifest, delete_payload, favourite_payload, highlight_payload, set_time_payload,
    tcp_poke_bytes, tcp_poke_to, CameraSession, Datagram, SessionError, Timing, WriteError,
};
use osmo_core::duml::commands::DEFAULT_IDENTIFIER;
use osmo_core::duml::{pack_string, Frame};
use std::collections::VecDeque;
use std::io;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
struct Sent {
    pkt_type: u8,
    target: u16,
    set: u8,
    cmd: u8,
    payload: Vec<u8>,
}

type Log = Arc<Mutex<Vec<Sent>>>;
type OnList = Box<dyn FnMut(u8, usize) -> Vec<Vec<u8>>>;

struct FakeCamera {
    inbox: VecDeque<Vec<u8>>,
    log: Log,
    answer_handshake: bool,
    playback: Playback,
    on_list: OnList,
    list_counts: [usize; 4],
    write_replies: Vec<((u8, u8), Vec<u8>)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Playback {
    /// `0x02/0x0C` works (most cameras).
    Works,
    /// Action 4 / Pocket 3: `0x02/0x0C` is refused with `e0`; only `0x01/0x01` enters playback.
    RefusesUntilControl,
    /// The camera never reports playback.
    Never,
}

fn refusal() -> Vec<u8> {
    wrap(
        Frame {
            target: 0x0201,
            id: 1,
            ty: 0xC0 | 0x02 << 8 | 0x0C << 16,
            payload: vec![0xE0],
        }
        .encode(),
    )
}

fn wrap(frame: Vec<u8>) -> Vec<u8> {
    let mut d = vec![0u8; 20];
    d[6] = 5;
    d.extend(frame);
    d
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/manifests/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn split_frames(blob: &[u8]) -> Vec<Vec<u8>> {
    let (mut out, mut i) = (vec![], 0);
    while i + 13 <= blob.len() {
        let len = (blob[i + 1] as usize | (blob[i + 2] as usize) << 8) & 0x3FF;
        if blob[i] == 0x55 && len >= 13 && i + len <= blob.len() {
            out.push(blob[i..i + len].to_vec());
            i += len;
        } else {
            i += 1;
        }
    }
    out
}

/// A reassembled manifest as the camera streams it: start, 400-byte data chunks, end, all under `ctr`.
fn manifest_frames(manifest: &[u8], ctr: u8) -> Vec<Vec<u8>> {
    let mut v = vec![chunk(ctr, 0x04, &[])];
    v.extend(manifest.chunks(400).map(|c| chunk(ctr, 0x01, c)));
    v.push(chunk(ctr, 0x03, &[]));
    v
}

fn chunk(ctr: u8, sub: u8, body: &[u8]) -> Vec<u8> {
    let mut p = vec![0x4A, sub, 0, 0, ctr, 0, 0, 0, 0, 0];
    p.extend(body);
    wrap(
        Frame {
            target: 0x0201,
            id: 1,
            ty: 0xC0 | 0x27 << 16,
            payload: p,
        }
        .encode(),
    )
}

fn playback_status() -> Vec<u8> {
    let mut p = vec![0u8; 16];
    p[3] = 0x40; // bit 30 of the LE flags word
    wrap(
        Frame {
            target: 0x0201,
            id: 1,
            ty: 0x40 | 0x02 << 8 | 0x80 << 16,
            payload: p,
        }
        .encode(),
    )
}

impl FakeCamera {
    fn new(on_list: OnList) -> (Self, Log) {
        let log: Log = Arc::default();
        (
            FakeCamera {
                inbox: VecDeque::new(),
                log: log.clone(),
                answer_handshake: true,
                playback: Playback::Works,
                on_list,
                list_counts: [0; 4],
                write_replies: vec![],
            },
            log,
        )
    }
    fn silent() -> (Self, Log) {
        let (mut c, l) = Self::new(Box::new(|_, _| vec![]));
        c.answer_handshake = false;
        (c, l)
    }
}

impl Datagram for FakeCamera {
    fn send(&mut self, pkt: &[u8]) -> io::Result<()> {
        let pkt_type = pkt[6];
        let mut s = Sent {
            pkt_type,
            target: 0,
            set: 0xFF,
            cmd: 0xFF,
            payload: pkt[8..].to_vec(),
        };
        if pkt_type == 0 && self.answer_handshake {
            let mut r = vec![0u8; 12];
            r[6] = 0;
            self.inbox.push_back(r);
        }
        if pkt_type == 5 {
            let f = Frame::decode(&pkt[20..]).expect("session sent a malformed DUML frame");
            (s.set, s.cmd, s.payload) = (f.cmd_set(), f.cmd_id(), f.payload.clone());
            s.target = f.target;
            if (s.set, s.cmd) == (0x02, 0x0C) && s.payload == [1, 1, 0, 1] {
                match self.playback {
                    Playback::Works => self.inbox.push_back(playback_status()),
                    Playback::RefusesUntilControl => self.inbox.push_back(refusal()),
                    Playback::Never => {}
                }
            }
            if (s.set, s.cmd) == (0x01, 0x01)
                && s.payload.first() == Some(&0x00)
                && self.playback == Playback::RefusesUntilControl
            {
                self.inbox.push_back(playback_status());
            }
            if (s.set, s.cmd) == (0x00, 0x26)
                && s.payload.len() > 20
                && matches!(s.payload[4], 1 | 2)
            {
                let ctr = s.payload[4];
                let n = self.list_counts[ctr as usize];
                self.list_counts[ctr as usize] += 1;
                self.inbox.extend((self.on_list)(ctr, n));
            }
        }
        if pkt_type == 5 {
            if let Some(p) = self
                .write_replies
                .iter()
                .find(|(k, _)| *k == (s.set, s.cmd))
                .map(|(_, p)| p.clone())
            {
                let ty = 0xC0 | (s.set as u32) << 8 | (s.cmd as u32) << 16;
                self.inbox.push_back(wrap(
                    Frame {
                        target: 0x0201,
                        id: 1,
                        ty,
                        payload: p,
                    }
                    .encode(),
                ));
            }
        }
        self.log.lock().unwrap().push(s);
        Ok(())
    }
    fn recv(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        if let Some(x) = self.inbox.pop_front() {
            return Ok(Some(x));
        }
        std::thread::sleep(timeout.min(Duration::from_millis(1)));
        Ok(None)
    }
}

fn session(cam: FakeCamera) -> CameraSession<FakeCamera> {
    CameraSession::new(cam, "192.168.2.1:9004".to_string(), Timing::fast())
}

fn duml(log: &Log) -> Vec<(u8, u8, Vec<u8>)> {
    log.lock()
        .unwrap()
        .iter()
        .filter(|s| s.pkt_type == 5)
        .map(|s| (s.set, s.cmd, s.payload.clone()))
        .collect()
}

const TRIGGER: [u8; 14] = [0x4a, 0x04, 0x0e, 0x10, 0x01, 0, 0, 0, 0, 0, 0x01, 0, 0, 0];

fn group_manifest(paths: &[&str]) -> Vec<u8> {
    let mut out = (paths.len() as u32).to_le_bytes().to_vec();
    for path in paths {
        let name = path.rsplit('/').next().unwrap();
        let stem = path.strip_suffix(".JPG").unwrap();
        out.extend([0x0d, name.len() as u8]);
        out.extend(name.as_bytes());
        out.extend([0x1a, (stem.len() + 6) as u8, 0, 0, 0, 1]);
        out.extend(stem.as_bytes());
    }
    out
}

#[test]
fn expands_a_group_in_order_and_keeps_the_leads_storage() {
    let paths = [
        "DCIM/DJI_001/SHOT_D_003.JPG",
        "DCIM/DJI_001/SHOT_D_001.JPG",
        "DCIM/DJI_001/SHOT_D_EXTRA_002.JPG",
        "DCIM/DJI_002/SHOT_D_002.JPG",
        "DCIM/DJI_001/SHOT_D_002.JPG",
    ];
    let frames = manifest_frames(&group_manifest(&paths), 1);
    let listing = manifest_frames(&fixture("oa4_45.bin"), 1);
    let (cam, log) = FakeCamera::new(Box::new(move |ctr, n| match (ctr, n) {
        (1, 0) => listing.clone(),
        (1, 1) => frames.clone(),
        _ => vec![],
    }));
    let mut sess = session(cam);
    sess.list_newest().unwrap();
    let mut lead = decode_manifest(&fixture("oa4_45.bin")).remove(0);
    lead.path = paths[1].into();
    lead.name = "SHOT_D_001.JPG".into();
    lead.storage = 1;
    lead.cmd_handle = 0x40123456;
    let files = sess.expand_burst_group(&lead).unwrap();
    assert_eq!(
        files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        vec![paths[1], paths[4], paths[0]]
    );
    assert!(files.iter().all(|f| f.storage == 1));
    assert!(files.iter().all(|f| !f.deletable() && f.cmd_handle == 0));
    let requests = duml(&log);
    let query = requests
        .iter()
        .find(|(set, cmd, p)| *set == 0 && *cmd == 0x26 && p.len() > 20 && p[16] == 0x10)
        .unwrap();
    assert_eq!(&query.2[10..14], &0x40123456u32.to_le_bytes());
    assert_eq!(query.2[14], 120);
    assert_eq!(query.2[39], 1);
}

#[test]
fn burst_expansion_does_not_treat_silence_as_a_complete_group() {
    let (cam, _) = FakeCamera::new(Box::new(|_, _| vec![]));
    let mut sess = session(cam);
    sess.list_newest().unwrap();
    let mut lead = decode_manifest(&fixture("oa4_45.bin")).remove(0);
    lead.path = "DCIM/DJI_001/SHOT_D_001.JPG".into();
    lead.name = "SHOT_D_001.JPG".into();
    lead.cmd_handle = 0x40010;
    assert!(sess.expand_burst_group(&lead).is_err());
}

#[test]
fn burst_expansion_rejects_a_reply_with_missing_records() {
    let mut manifest = group_manifest(&["DCIM/DJI_001/SHOT_D_001.JPG"]);
    manifest[..4].copy_from_slice(&3u32.to_le_bytes());
    let group = manifest_frames(&manifest, 1);
    let listing = manifest_frames(&fixture("oa4_45.bin"), 1);
    let (cam, _) = FakeCamera::new(Box::new(move |ctr, n| match (ctr, n) {
        (1, 0) => listing.clone(),
        (1, 1) => group.clone(),
        _ => vec![],
    }));
    let mut sess = session(cam);
    sess.list_newest().unwrap();
    let mut lead = decode_manifest(&fixture("oa4_45.bin")).remove(0);
    lead.path = "DCIM/DJI_001/SHOT_D_001.JPG".into();
    lead.name = "SHOT_D_001.JPG".into();
    lead.cmd_handle = 0x40010;
    assert!(matches!(
        sess.expand_burst_group(&lead),
        Err(SessionError::IncompleteGroup)
    ));
}

#[test]
fn lists_oa4_newest_page_from_fixture_in_the_documented_order() {
    let frames = manifest_frames(&fixture("oa4_45.bin"), 1);
    let (cam, log) = FakeCamera::new(Box::new(move |ctr, n| {
        if ctr == 1 && n == 0 {
            frames.clone()
        } else {
            vec![]
        }
    }));
    let mut s = session(cam);
    let files = s.list_newest().unwrap();
    assert_eq!(files.len(), 45);
    assert_eq!(
        decode_manifest(s.last_blob()).len(),
        45,
        "the retained raw blob re-decodes offline, so a live capture can become a fixture"
    );
    assert!(files
        .iter()
        .any(|f| f.name == "DJI_20260903165023_0012_D.JPG"));
    assert!(s.playback_held());

    let hs = log
        .lock()
        .unwrap()
        .iter()
        .find(|p| p.pkt_type == 0)
        .cloned()
        .unwrap();
    assert_eq!(
        &hs.payload[..2],
        &s.base_seq().to_le_bytes(),
        "handshake proposes our base seq"
    );

    let d = duml(&log);
    let pos = |set: u8, cmd: u8| {
        d.iter()
            .position(|(a, b, _)| (*a, *b) == (set, cmd))
            .unwrap()
    };
    assert!(
        pos(0x00, 0x81) < pos(0x00, 0x88)
            && pos(0x00, 0x88) < pos(0x03, 0xDA)
            && pos(0x03, 0xDA) < pos(0x00, 0x99)
    );
    assert_eq!(
        d.iter()
            .filter(|(a, b, _)| (*a, *b) == (0x00, 0x99))
            .count(),
        8
    );
    let play = d
        .iter()
        .position(|(a, b, p)| (*a, *b) == (0x02, 0x0C) && *p == [1, 1, 0, 1])
        .unwrap();
    let q1 = d
        .iter()
        .position(|(a, b, p)| (*a, *b) == (0, 0x26) && p.len() > 20 && p[4] == 1 && p[10] == 1)
        .unwrap();
    let q2 = d
        .iter()
        .position(|(a, b, p)| (*a, *b) == (0, 0x26) && p.len() > 20 && p[4] == 2 && p[13] == 0x40)
        .unwrap();
    let trig = d
        .iter()
        .position(|(a, b, p)| (*a, *b) == (0, 0x26) && *p == TRIGGER)
        .unwrap();
    assert!(
        play < q1 && q1 < trig && trig < q2,
        "playback, SD query, trigger, internal query"
    );
    assert_eq!(d[q1].2[14], 45, "page size");
}

#[test]
fn registration_frames_carry_app_identity_and_subscriptions() {
    let (cam, log) = FakeCamera::new(Box::new(|_, _| vec![]));
    let _ = session(cam).list_newest();
    let d = duml(&log);
    assert_eq!(d[0].2, osmo_core::duml::commands::APP_DEVICE_INFO.to_vec());
    let sub = d.iter().find(|(a, b, _)| (*a, *b) == (0, 0x99)).unwrap();
    assert_eq!(&sub.2[..4], &[0x02, 0x02, 0, 0]);
    assert_eq!(&sub.2[4..8], &0x69DFu32.to_le_bytes());
    assert!(String::from_utf8_lossy(&sub.2).contains("camcap_mode_profile"));
}

#[test]
fn empty_store_returns_empty_list_without_error() {
    let (cam, log) = FakeCamera::new(Box::new(|ctr, _| vec![chunk(ctr, 0x04, &[])]));
    let t = Instant::now();
    let files = session(cam).list_newest().unwrap();
    assert!(files.is_empty());
    assert!(t.elapsed() < Duration::from_secs(5), "bounded");
    let sd_queries = duml(&log)
        .iter()
        .filter(|(a, b, p)| (*a, *b) == (0, 0x26) && p.len() > 20 && p[4] == 1 && p[10] == 1)
        .count();
    assert_eq!(sd_queries, 2, "an empty SD answer is re-asked once");
}

#[test]
fn empty_sd_is_requeried_once_and_the_retry_is_used() {
    let oa4_6 = fixture("oa4_6.bin");
    let want = decode_manifest(&fixture("oa4_6.bin")).len();
    assert!(want > 0);
    let (cam, log) = FakeCamera::new(Box::new(move |ctr, n| match (ctr, n) {
        (1, 0) | (2, _) => vec![chunk(ctr, 0x04, &[])],
        (1, _) => manifest_frames(&oa4_6, 1),
        _ => vec![],
    }));
    let files = session(cam).list_newest().unwrap();
    assert_eq!(files.len(), want);
    assert!(files.iter().all(|f| f.storage == 0));
    let sd_queries = duml(&log)
        .iter()
        .filter(|(a, b, p)| (*a, *b) == (0, 0x26) && p.len() > 20 && p[4] == 1 && p[10] == 1)
        .count();
    assert_eq!(sd_queries, 2);
}

const PRELUDE: [u8; 11] = [0x03, 0, 0, 0, 0, 0x04, 0, 0, 0, 0x07, 0x01];
const ENTER: [u8; 11] = [0x00, 0, 0, 0, 0, 0x04, 0, 0, 0, 0x04, 0x01];

#[test]
fn action4_refuses_0x02_0x0c_and_enters_playback_via_0x01_0x01() {
    let frames = manifest_frames(&fixture("oa4_45.bin"), 1);
    let (mut cam, log) = FakeCamera::new(Box::new(move |ctr, n| {
        if ctr == 1 && n == 0 {
            frames.clone()
        } else {
            vec![]
        }
    }));
    cam.playback = Playback::RefusesUntilControl;
    let mut s = session(cam);
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = lines.clone();
    s.set_logger(move |l| sink.lock().unwrap().push(l.to_string()));
    let files = s.list_newest().unwrap();
    let log_text = lines.lock().unwrap().join("\n");
    for want in [
        "handshake ok",
        "0x02/0x0C refused",
        "held via 0x01/0x01",
        "files",
    ] {
        assert!(
            log_text.contains(want),
            "log is missing {want:?}:\n{log_text}"
        );
    }
    assert_eq!(files.len(), 45);
    assert!(
        s.playback_held(),
        "the camera's own playback bit is the only completion signal"
    );

    let d = duml(&log);
    let enter_0c = d
        .iter()
        .filter(|(a, b, _)| (*a, *b) == (0x02, 0x0C))
        .count();
    assert_eq!(
        enter_0c, 1,
        "a refusal is definitive: no point re-sending 0x02/0x0C"
    );
    let control: Vec<&Vec<u8>> = d
        .iter()
        .filter(|(a, b, _)| (*a, *b) == (0x01, 0x01))
        .map(|(_, _, p)| p)
        .collect();
    assert!(
        control.len() >= 7,
        "6 prelude frames then the enter payload: {}",
        control.len()
    );
    assert!(
        control[..6].iter().all(|p| **p == PRELUDE),
        "first six frames are the prelude"
    );
    assert_eq!(control[6], &ENTER.to_vec());
    let first_control = d
        .iter()
        .position(|(a, b, _)| (*a, *b) == (0x01, 0x01))
        .unwrap();
    let first_list = d
        .iter()
        .position(|(a, b, p)| (*a, *b) == (0, 0x26) && p.len() > 20)
        .unwrap();
    assert!(
        first_control < first_list,
        "playback is entered before the first list query"
    );
}

#[test]
fn playback_never_confirmed_is_reported_and_bounded() {
    let frames = manifest_frames(&fixture("oa4_6.bin"), 1);
    let (mut cam, _log) = FakeCamera::new(Box::new(move |ctr, n| {
        if ctr == 1 && n == 0 {
            frames.clone()
        } else {
            vec![]
        }
    }));
    cam.playback = Playback::Never;
    let mut s = session(cam);
    let t = Instant::now();
    s.list_newest().unwrap();
    assert!(!s.playback_held());
    assert!(
        !s.diagnostics().playback_confirmed,
        "the CLI warns that the listing may be incomplete"
    );
    assert!(t.elapsed() < Duration::from_secs(5));
}

#[test]
fn unconfirmed_playback_is_not_fatal() {
    let frames = manifest_frames(&fixture("oa4_6.bin"), 1);
    let (mut cam, _log) = FakeCamera::new(Box::new(move |ctr, n| {
        if ctr == 1 && n == 0 {
            frames.clone()
        } else {
            vec![]
        }
    }));
    cam.playback = Playback::Never;
    let mut s = session(cam);
    assert!(!s.list_newest().unwrap().is_empty());
    assert!(!s.playback_held());
}

#[test]
fn replays_a_real_captured_frame_stream() {
    // nano_45.bin is a raw capture: SD answered "nothing" (start only), internal streamed 45 records.
    let frames: Vec<Vec<u8>> = split_frames(&fixture("nano_45.bin"))
        .into_iter()
        .map(wrap)
        .collect();
    assert!(!frames.is_empty());
    let (cam, _log) = FakeCamera::new(Box::new(move |ctr, n| {
        if ctr == 1 && n == 0 {
            frames.clone()
        } else {
            vec![]
        }
    }));
    let files = session(cam).list_newest().unwrap();
    assert_eq!(files.len(), 45);
    assert!(
        files.iter().all(|f| f.storage == 1),
        "internal store answered"
    );
}

#[test]
fn no_handshake_error_names_the_address() {
    let (cam, log) = FakeCamera::silent();
    let err = session(cam).list_newest().unwrap_err();
    assert!(matches!(err, SessionError::NoHandshake { .. }));
    let msg = err.to_string();
    assert!(
        msg.contains("192.168.2.1:9004") && msg.contains("WiFi"),
        "{msg}"
    );
    assert_eq!(
        log.lock()
            .unwrap()
            .iter()
            .filter(|s| s.pkt_type == 0)
            .count(),
        20
    );
}

#[test]
fn tcp_poke_bytes_are_the_set_pairing_pin_osmo_frame() {
    let f = Frame::decode(&tcp_poke_bytes()).unwrap();
    assert_eq!((f.cmd_set(), f.cmd_id()), (0x07, 0x45));
    let mut want = pack_string(DEFAULT_IDENTIFIER);
    want.extend(pack_string("osmo"));
    assert_eq!(f.payload, want);
}

#[test]
fn tcp_poke_writes_the_frame_to_the_port() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let h = std::thread::spawn(move || {
        let (mut c, _) = l.accept().unwrap();
        let mut got = vec![];
        c.read_to_end(&mut got).unwrap();
        got
    });
    tcp_poke_to(addr, Duration::ZERO);
    assert_eq!(h.join().unwrap(), tcp_poke_bytes());
}

#[test]
fn tcp_poke_to_a_closed_port_is_not_fatal() {
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    tcp_poke_to(addr, Duration::ZERO);
}

fn leave_frames(log: &Log) -> usize {
    duml(log)
        .iter()
        .filter(|(a, b, p)| (*a, *b) == (0x02, 0x0C) && p.as_slice() == [1, 1, 0, 0])
        .count()
}

#[test]
fn dropping_a_session_releases_playback() {
    let frames = manifest_frames(&fixture("oa4_6.bin"), 1);
    let (cam, log) = FakeCamera::new(Box::new(move |ctr, n| {
        if ctr == 1 && n == 0 {
            frames.clone()
        } else {
            vec![]
        }
    }));
    {
        let mut s = session(cam);
        s.list_newest().unwrap();
        assert!(s.playback_held());
        assert_eq!(leave_frames(&log), 0);
    }
    assert_eq!(
        leave_frames(&log),
        1,
        "an early return must not leave the camera in playback"
    );
}

#[test]
fn explicit_close_then_drop_releases_playback_only_once() {
    let frames = manifest_frames(&fixture("oa4_6.bin"), 1);
    let (cam, log) = FakeCamera::new(Box::new(move |ctr, n| {
        if ctr == 1 && n == 0 {
            frames.clone()
        } else {
            vec![]
        }
    }));
    {
        let mut s = session(cam);
        s.list_newest().unwrap();
        s.close();
    }
    assert_eq!(leave_frames(&log), 1);
}

#[test]
fn status_pushes_during_a_listing_are_decoded() {
    let mut battery = vec![0u8; 34];
    battery[1..3].copy_from_slice(&4000u16.to_le_bytes());
    battery[20] = 87;
    let push = wrap(
        Frame {
            target: 0x0201,
            id: 1,
            ty: 0x40 | 0x0D << 8 | 0x02 << 16,
            payload: battery,
        }
        .encode(),
    );
    let mut frames = manifest_frames(&fixture("oa4_6.bin"), 1);
    frames.push(push);
    let (cam, _log) = FakeCamera::new(Box::new(move |ctr, n| {
        if ctr == 1 && n == 0 {
            frames.clone()
        } else {
            vec![]
        }
    }));
    let mut s = session(cam);
    s.list_newest().unwrap();
    assert_eq!(s.status().battery_percent, Some(87));
    assert_eq!(s.status().battery_millivolts, Some(4000));
}

fn live_manifest() -> Vec<u8> {
    fixture("oa4_live_14.bin")
}

fn sd_handles(files: &[osmo_core::media::CameraFile]) -> Vec<u32> {
    files
        .iter()
        .filter(|f| f.storage == 0 && f.handle != 0)
        .map(|f| f.handle)
        .collect()
}

fn sd_page_queries(log: &Log) -> Vec<Vec<u8>> {
    duml(log)
        .into_iter()
        .filter(|(a, b, p)| (*a, *b) == (0, 0x26) && p.len() > 20 && p[4] == 1)
        .map(|(_, _, p)| p)
        .collect()
}

#[test]
fn page_size_reaches_byte_14_of_the_query() {
    let frames = manifest_frames(&live_manifest(), 1);
    let (cam, log) = FakeCamera::new(Box::new(move |ctr, n| {
        if ctr == 1 && n == 0 {
            frames.clone()
        } else {
            vec![]
        }
    }));
    let mut s = session(cam);
    s.set_page_size(5);
    s.list_newest().unwrap();
    assert_eq!(sd_page_queries(&log)[0][14], 5);
}

#[test]
fn second_page_query_carries_the_oldest_sd_handle_as_cursor_and_returns_only_new_files() {
    let page1 = manifest_frames(&fixture("oa4_45.bin"), 1);
    let page2 = manifest_frames(&fixture("oa4_6.bin"), 1);
    let (cam, log) = FakeCamera::new(Box::new(move |ctr, n| match (ctr, n) {
        (1, 0) => page1.clone(),
        (1, 1) => page2.clone(),
        _ => vec![],
    }));
    let mut s = session(cam);
    let first = s.list_newest().unwrap();
    assert_eq!(first.len(), 45);
    assert!(
        s.more_available(),
        "a full page of 45 with no end marker means there may be more"
    );
    let oldest = *sd_handles(&first).iter().min().unwrap();

    let second = s.list_next_page().unwrap();
    let want_new: Vec<String> = decode_manifest(&fixture("oa4_6.bin"))
        .into_iter()
        .map(|f| f.path)
        .filter(|p| !first.iter().any(|f| &f.path == p))
        .collect();
    assert!(
        !want_new.is_empty(),
        "fixture choice must give the page at least one new file"
    );
    assert_eq!(
        second.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
        want_new,
        "only unseen files are returned"
    );

    let q = sd_page_queries(&log);
    assert_eq!(q.len(), 2);
    assert_eq!(
        &q[1][10..14],
        &oldest.to_le_bytes(),
        "page 2 asks for files older than page 1's oldest SD handle"
    );
    assert!(
        !s.more_available(),
        "oa4_6 is a short page with the end marker: the library is finished"
    );
    assert!(
        s.list_next_page().unwrap().is_empty(),
        "exhausted: no further query"
    );
    assert_eq!(sd_page_queries(&log).len(), 2);
}

#[test]
fn a_truncated_page_keeps_more_available_and_sets_the_flag() {
    let cut = live_manifest()[..3000].to_vec();
    let frames = manifest_frames(&cut, 1);
    let (cam, _log) = FakeCamera::new(Box::new(move |ctr, n| {
        if ctr == 1 && n <= 1 {
            frames.clone()
        } else {
            vec![]
        }
    }));
    let mut s = session(cam);
    let files = s.list_newest().unwrap();
    assert!(files.len() < 14, "the page really was cut short");
    assert!(s.diagnostics().truncated, "the caller is told");
    assert!(
        s.more_available(),
        "a truncated page is not the end of the library"
    );
}

#[test]
fn find_file_walks_older_pages_until_it_finds_the_path() {
    let page1 = manifest_frames(&fixture("oa4_45.bin"), 1);
    let page2 = manifest_frames(&fixture("oa4_6.bin"), 1);
    let on_page1: std::collections::HashSet<String> = decode_manifest(&fixture("oa4_45.bin"))
        .into_iter()
        .map(|f| f.path)
        .collect();
    let only_on_page2 = decode_manifest(&fixture("oa4_6.bin"))
        .into_iter()
        .find(|f| !on_page1.contains(&f.path))
        .expect("page 2 has a new file")
        .path;
    let (cam, log) = FakeCamera::new(Box::new(move |ctr, n| match (ctr, n) {
        (1, 0) => page1.clone(),
        (1, 1) => page2.clone(),
        _ => vec![],
    }));
    let mut s = session(cam);
    let found = s
        .find_file(&only_on_page2)
        .unwrap()
        .expect("found on the second page");
    assert_eq!(found.path, only_on_page2);
    assert_eq!(
        sd_page_queries(&log).len(),
        2,
        "had to ask for the older page"
    );
}

#[test]
fn find_file_returns_none_after_exhausting_the_library_and_stops_early_when_found() {
    let page1 = manifest_frames(&fixture("oa4_45.bin"), 1);
    let first_path = decode_manifest(&fixture("oa4_45.bin")).remove(0).path;
    let page2 = manifest_frames(&fixture("oa4_6.bin"), 1);
    let (cam, log) = FakeCamera::new(Box::new(move |ctr, n| match (ctr, n) {
        (1, 0) => page1.clone(),
        (1, 1) => page2.clone(),
        _ => vec![],
    }));
    let mut s = session(cam);
    assert!(s.find_file(&first_path).unwrap().is_some());
    assert_eq!(
        sd_page_queries(&log).len(),
        1,
        "found on the newest page: no further query"
    );
    assert!(s
        .find_file("DCIM/DJI_001/DOES_NOT_EXIST.MP4")
        .unwrap()
        .is_none());
}

fn session_with(cam: FakeCamera, timing: Timing) -> CameraSession<FakeCamera> {
    CameraSession::new(cam, "192.168.2.1:9004".to_string(), timing)
}

/// A listed session on an Action 4-like fake whose `0x02/0xBF` write is answered with `reply` (None: silence).
fn writable(
    reply: Option<Vec<u8>>,
    playback: Playback,
    timing: Timing,
) -> (CameraSession<FakeCamera>, Log, osmo_core::media::CameraFile) {
    let frames = manifest_frames(&fixture("oa4_45.bin"), 1);
    let (mut cam, log) = FakeCamera::new(Box::new(move |ctr, n| {
        if ctr == 1 && n == 0 {
            frames.clone()
        } else {
            vec![]
        }
    }));
    cam.playback = playback;
    if let Some(p) = reply {
        cam.write_replies.push(((0x02, 0xBF), p));
    }
    let mut s = session_with(cam, timing);
    let files = s.list_newest().unwrap();
    let file = files
        .into_iter()
        .find(|f| f.cmd_handle != 0)
        .expect("a file with a command handle");
    (s, log, file)
}

fn favourite_frames(log: &Log) -> Vec<Vec<u8>> {
    duml(log)
        .into_iter()
        .filter(|(a, b, _)| (*a, *b) == (0x02, 0xBF))
        .map(|(_, _, p)| p)
        .collect()
}

#[test]
fn favourite_sends_0x02_0xbf_and_accepts_status_0() {
    let (mut s, log, file) = writable(
        Some(vec![0x00]),
        Playback::RefusesUntilControl,
        Timing::fast(),
    );
    s.set_favourite(&file, true).unwrap();
    assert_eq!(
        favourite_frames(&log),
        vec![favourite_payload(file.cmd_handle, 1, true)]
    );
    s.set_favourite(&file, false).unwrap();
    assert_eq!(
        favourite_frames(&log)[1],
        favourite_payload(file.cmd_handle, 2, false),
        "the counter runs per session"
    );
}

#[test]
fn favourite_rejection_is_reported() {
    let (mut s, _log, file) = writable(Some(vec![0xE0]), Playback::Works, Timing::fast());
    assert!(matches!(
        s.set_favourite(&file, true),
        Err(WriteError::Rejected(0xE0))
    ));
}

#[test]
fn favourite_with_no_reply_is_no_reply_not_success_and_returns_promptly() {
    let (mut s, _log, file) = writable(None, Playback::Works, Timing::fast());
    let t = Instant::now();
    assert!(matches!(
        s.set_favourite(&file, true),
        Err(WriteError::NoReply)
    ));
    assert!(t.elapsed() < Duration::from_secs(2));
}

#[test]
fn a_write_on_an_old_session_is_refused_without_sending() {
    let old = Timing {
        write_window: Duration::from_millis(1),
        ..Timing::fast()
    };
    let (mut s, log, file) = writable(Some(vec![0x00]), Playback::Works, old);
    assert!(matches!(
        s.set_favourite(&file, true),
        Err(WriteError::SessionTooOld { .. })
    ));
    assert!(
        favourite_frames(&log).is_empty(),
        "nothing was sent on a session the camera would ignore"
    );
}

#[test]
fn a_file_without_a_command_handle_is_refused_without_sending() {
    let (mut s, log, file) = writable(Some(vec![0x00]), Playback::Works, Timing::fast());
    let no_handle = osmo_core::media::CameraFile {
        cmd_handle: 0,
        ..file
    };
    assert!(matches!(
        s.set_favourite(&no_handle, true),
        Err(WriteError::NoHandle)
    ));
    assert!(favourite_frames(&log).is_empty());
}

#[test]
fn a_write_outside_playback_is_refused_without_sending() {
    let (mut s, log, file) = writable(Some(vec![0x00]), Playback::Never, Timing::fast());
    assert!(matches!(
        s.set_favourite(&file, true),
        Err(WriteError::NotInPlayback)
    ));
    assert!(favourite_frames(&log).is_empty());
}

#[test]
fn the_write_counter_can_continue_a_sequence_from_an_earlier_process() {
    // The camera treats a repeated counter as a repeat and acknowledges without acting, so a new process
    // must continue where the last one stopped.
    let (mut s, log, file) = writable(Some(vec![0x00]), Playback::Works, Timing::fast());
    s.set_write_counter_start(41);
    s.set_favourite(&file, true).unwrap();
    s.set_favourite(&file, false).unwrap();
    assert_eq!(
        favourite_frames(&log),
        vec![
            favourite_payload(file.cmd_handle, 41, true),
            favourite_payload(file.cmd_handle, 42, false)
        ]
    );
    assert_eq!(
        s.next_write_counter(),
        43,
        "the caller persists this for the next run"
    );
}

/// A listed Action 4-like session whose `0x00/0x28` delete is answered with `reply` (None: silence).
fn deletable_session(
    reply: Option<Vec<u8>>,
    timing: Timing,
) -> (
    CameraSession<FakeCamera>,
    Log,
    Vec<osmo_core::media::CameraFile>,
) {
    let frames = manifest_frames(&fixture("oa4_45.bin"), 1);
    let (mut cam, log) = FakeCamera::new(Box::new(move |ctr, n| {
        if ctr == 1 && n == 0 {
            frames.clone()
        } else {
            vec![]
        }
    }));
    cam.playback = Playback::RefusesUntilControl;
    if let Some(p) = reply {
        cam.write_replies.push(((0x00, 0x28), p));
    }
    let mut s = session_with(cam, timing);
    let files = s.list_newest().unwrap();
    (s, log, files)
}

fn delete_frames(log: &Log) -> Vec<Vec<u8>> {
    duml(log)
        .into_iter()
        .filter(|(a, b, _)| (*a, *b) == (0x00, 0x28))
        .map(|(_, _, p)| p)
        .collect()
}

#[test]
fn delete_sends_the_documented_payload_and_accepts_0000() {
    let (mut s, log, files) = deletable_session(Some(vec![0x00, 0x00]), Timing::fast());
    let two: Vec<&osmo_core::media::CameraFile> =
        files.iter().filter(|f| f.deletable()).take(2).collect();
    assert_eq!(two.len(), 2, "the fixture has deletable files");
    s.delete_files(&two).unwrap();
    assert_eq!(
        delete_frames(&log),
        vec![delete_payload(&[two[0].handle, two[1].handle], 1)]
    );
}

#[test]
fn delete_refuses_a_shared_handle_before_sending_anything() {
    let (mut s, log, files) = deletable_session(Some(vec![0x00, 0x00]), Timing::fast());
    let good = files.iter().find(|f| f.deletable()).unwrap().clone();
    let shared = osmo_core::media::CameraFile {
        handle_shared: true,
        ..good.clone()
    };
    let r = s.delete_files(&[&good, &shared]);
    assert!(
        matches!(&r, Err(WriteError::NotDeletable(p)) if *p == shared.path),
        "{r:?}"
    );
    assert!(
        delete_frames(&log).is_empty(),
        "nothing is sent when any file is unsafe, not even the safe one"
    );
}

#[test]
fn delete_refuses_a_file_with_no_verified_handle() {
    let (mut s, log, files) = deletable_session(Some(vec![0x00, 0x00]), Timing::fast());
    let good = files.iter().find(|f| f.deletable()).unwrap().clone();
    let unverified = osmo_core::media::CameraFile { handle: 0, ..good };
    assert!(matches!(
        s.delete_files(&[&unverified]),
        Err(WriteError::NotDeletable(_))
    ));
    assert!(delete_frames(&log).is_empty());
}

#[test]
fn delete_refuses_more_than_20_files() {
    let (mut s, log, files) = deletable_session(Some(vec![0x00, 0x00]), Timing::fast());
    let good = files.iter().find(|f| f.deletable()).unwrap();
    let many: Vec<&osmo_core::media::CameraFile> = std::iter::repeat_n(good, 21).collect();
    assert!(matches!(
        s.delete_files(&many),
        Err(WriteError::TooMany { max: 20 })
    ));
    assert!(delete_frames(&log).is_empty());
}

#[test]
fn delete_no_reply_is_not_success() {
    let (mut s, _log, files) = deletable_session(None, Timing::fast());
    let good = files.iter().find(|f| f.deletable()).unwrap();
    assert!(matches!(s.delete_files(&[good]), Err(WriteError::NoReply)));
}

#[test]
fn delete_status_00d6_is_rejected() {
    let (mut s, _log, files) = deletable_session(Some(vec![0xD6, 0x00]), Timing::fast());
    let good = files.iter().find(|f| f.deletable()).unwrap();
    assert!(matches!(
        s.delete_files(&[good]),
        Err(WriteError::Rejected(0xD6))
    ));
}

#[test]
fn delete_on_an_old_session_is_refused_without_sending() {
    let old = Timing {
        write_window: Duration::from_millis(1),
        ..Timing::fast()
    };
    let (mut s, log, files) = deletable_session(Some(vec![0x00, 0x00]), old);
    let good = files.iter().find(|f| f.deletable()).unwrap();
    assert!(matches!(
        s.delete_files(&[good]),
        Err(WriteError::SessionTooOld { .. })
    ));
    assert!(delete_frames(&log).is_empty());
}

#[test]
fn sync_time_sends_one_0x00_0x6a_frame_to_the_system_receiver() {
    let (mut s, log, _files) = deletable_session(Some(vec![0x00, 0x00]), Timing::fast());
    let r = s.sync_time(1_700_000_000, 330, "Asia/Kolkata");
    let sent: Vec<_> = log
        .lock()
        .unwrap()
        .iter()
        .filter(|x| (x.set, x.cmd) == (0x00, 0x6A))
        .cloned()
        .collect();
    assert_eq!(sent.len(), 1, "{r:?}");
    assert_eq!(
        sent[0].payload,
        set_time_payload(1_700_000_000, 330, "Asia/Kolkata")
    );
    assert_eq!(
        sent[0].target >> 8,
        0x28,
        "receiver (id 1 << 5) | type 0x08"
    );
}

#[test]
fn sync_time_refuses_a_zone_name_that_is_not_ascii_before_sending() {
    let (mut s, log, _files) = deletable_session(None, Timing::fast());
    assert!(s.sync_time(0, 0, "Zürich").is_err());
    assert!(!log
        .lock()
        .unwrap()
        .iter()
        .any(|x| (x.set, x.cmd) == (0x00, 0x6A)));
}

#[test]
fn sync_time_on_an_unregistered_session_is_refused_without_sending() {
    let (cam, log) = FakeCamera::new(Box::new(|_, _| vec![]));
    let mut s = session(cam);
    assert!(matches!(
        s.sync_time(1, 0, "UTC"),
        Err(WriteError::NotRegistered)
    ));
    assert!(!log
        .lock()
        .unwrap()
        .iter()
        .any(|x| (x.set, x.cmd) == (0x00, 0x6A)));
}

#[test]
fn register_then_sync_time_sends_the_frame_and_register_is_idempotent() {
    let (cam, log) = FakeCamera::new(Box::new(|_, _| vec![]));
    let mut s = session(cam);
    let count = |log: &Log| {
        log.lock()
            .unwrap()
            .iter()
            .filter(|x| x.pkt_type == 0)
            .count()
    };
    s.register().unwrap();
    let once = count(&log);
    assert!(once >= 1);
    s.register().unwrap();
    assert_eq!(count(&log), once, "no second handshake");
    let _ = s.sync_time(1, 0, "UTC");
    assert!(log
        .lock()
        .unwrap()
        .iter()
        .any(|x| (x.set, x.cmd) == (0x00, 0x6A)));
}

fn highlights_session(
    reply: Option<Vec<u8>>,
) -> (
    CameraSession<FakeCamera>,
    Log,
    Vec<osmo_core::media::CameraFile>,
) {
    let frames = manifest_frames(&fixture("oa4_45.bin"), 1);
    let (mut cam, log) = FakeCamera::new(Box::new(move |ctr, n| {
        if ctr == 1 && n == 0 {
            frames.clone()
        } else {
            vec![]
        }
    }));
    cam.playback = Playback::RefusesUntilControl;
    if let Some(p) = reply {
        cam.write_replies.push(((0x02, 0xFF), p));
    }
    let mut s = session(cam);
    let files = s.list_newest().unwrap();
    (s, log, files)
}

#[test]
fn highlights_send_0x02_0xff_and_decode_the_marks() {
    let reply = {
        let mut r = vec![0x00, 0x40, 0x2F, 0x00, 0x01, 17, 0, 0, 0, 0, 0, 0, 0, 2, 0];
        for m in [4000u32, 7000] {
            r.push(0);
            r.extend(m.to_le_bytes());
        }
        r
    };
    let (mut s, log, files) = highlights_session(Some(reply));
    let video = files
        .iter()
        .find(|f| f.is_video() && f.handle != 0)
        .unwrap();
    assert_eq!(s.highlights(video), Ok(vec![4000, 7000]));
    let sent: Vec<_> = duml(&log)
        .into_iter()
        .filter(|(a, b, _)| (*a, *b) == (0x02, 0xFF))
        .collect();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].2, highlight_payload(video.handle));
}

#[test]
fn highlights_with_no_reply_or_no_handle_are_errors_not_empty() {
    let (mut s, _log, files) = highlights_session(None);
    let video = files
        .iter()
        .find(|f| f.is_video() && f.handle != 0)
        .unwrap();
    assert_eq!(s.highlights(video), Err(WriteError::NoReply));
    let no_handle = osmo_core::media::CameraFile {
        handle: 0,
        ..video.clone()
    };
    assert_eq!(s.highlights(&no_handle), Err(WriteError::NoHandle));
}

#[test]
fn highlights_only_sets_byte_37_of_the_list_query() {
    let run = |only: bool| {
        let frames = manifest_frames(&live_manifest(), 1);
        let (cam, log) = FakeCamera::new(Box::new(move |ctr, n| {
            if ctr == 1 && n == 0 {
                frames.clone()
            } else {
                vec![]
            }
        }));
        let mut s = session(cam);
        s.set_highlights_only(only);
        s.list_newest().unwrap();
        sd_page_queries(&log)[0][37]
    };
    assert_eq!(run(false), 0, "default lists everything");
    assert_eq!(run(true), 2, "only files carrying highlight marks");
}
