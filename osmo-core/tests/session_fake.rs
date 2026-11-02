//! Datalink session against a scripted fake camera that replays the captured manifests.
//! Fixture-tested only: this proves our side of the protocol, not that a real camera accepts it.
use osmo_core::datalink::{
    decode_manifest, tcp_poke_bytes, tcp_poke_to, CameraSession, Datagram, SessionError, Timing,
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
    playback: bool,
    on_list: OnList,
    list_counts: [usize; 4],
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
                playback: true,
                on_list,
                list_counts: [0; 4],
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
            if (s.set, s.cmd) == (0x02, 0x0C) && s.payload == [1, 1, 0, 1] && self.playback {
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
    cam.playback = false;
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
