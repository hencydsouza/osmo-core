use osmo_core::ble::{
    start_livestream, start_livestream_cancellable, BleLink, LinkError, PairOptions, StreamConfig,
};
use osmo_core::duml::Frame;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

struct Camera {
    inbox: VecDeque<Vec<u8>>,
    sent: Vec<Frame>,
    reject_prepare: bool,
    silent_start: bool,
    stale_prepare: bool,
    cancel_on_start: Option<Arc<AtomicBool>>,
    wifi_status: [u8; 2],
}

impl Camera {
    fn new() -> Self {
        Self {
            inbox: VecDeque::new(),
            sent: vec![],
            reject_prepare: false,
            silent_start: false,
            stale_prepare: false,
            cancel_on_start: None,
            wifi_status: [0, 0],
        }
    }
}

impl BleLink for Camera {
    fn write_command(&mut self, bytes: &[u8]) -> Result<(), LinkError> {
        let f = Frame::decode(bytes).unwrap();
        let (set, cmd) = (f.cmd_set(), f.cmd_id());
        let start = (set, cmd) == (2, 0x8e) && f.payload == [1, 1, 0x1a, 0, 1, 1];
        if start {
            if let Some(flag) = &self.cancel_on_start {
                flag.store(true, Ordering::SeqCst);
            }
        }
        if f.flags() == 0x40 {
            let reply = match (set, cmd) {
                (7, 0x45) => Some(vec![0, 1]),
                (2, 0xe1) => Some(vec![if self.reject_prepare { 0xe0 } else { 0 }]),
                (7, 0x47) => Some(self.wifi_status.to_vec()),
                (2, 0x8e) if start && self.silent_start => None,
                (2, 0x8e) if self.stale_prepare && f.payload == [0, 1, 0x1c, 0] => None,
                (2, 0x8e) | (8, 0x78) => Some(vec![0]),
                _ => None,
            };
            if let Some(payload) = reply {
                self.inbox.push_back(
                    Frame {
                        target: 0x0208,
                        id: f.id,
                        ty: 0xc0 | (set as u32) << 8 | (cmd as u32) << 16,
                        payload,
                    }
                    .encode(),
                );
                if (set, cmd) == (2, 0xe1) && self.stale_prepare {
                    for _ in 0..2 {
                        self.inbox.push_back(
                            Frame {
                                target: 0x0208,
                                id: 1,
                                ty: 0x8e02c0,
                                payload: vec![0],
                            }
                            .encode(),
                        );
                    }
                }
                if start {
                    let mut battery = vec![0; 21];
                    battery[20] = 73;
                    self.inbox.push_back(
                        Frame {
                            target: 0x0208,
                            id: 1,
                            ty: 0x00020d00,
                            payload: battery,
                        }
                        .encode(),
                    );
                }
            }
        }
        self.sent.push(f);
        Ok(())
    }
    fn recv(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, LinkError> {
        if let Some(frame) = self.inbox.pop_front() {
            return Ok(Some(frame));
        }
        std::thread::sleep(timeout.min(Duration::from_millis(1)));
        Ok(None)
    }
}

fn opts() -> PairOptions {
    PairOptions {
        step_timeout: Duration::from_millis(20),
        approve_timeout: Duration::from_millis(60),
        write_gap: Duration::ZERO,
        keepalive_interval: Duration::from_secs(1),
        ..Default::default()
    }
}
fn config() -> StreamConfig<'static> {
    StreamConfig {
        ssid: "Home",
        password: "password",
        url: "rtmp://host/live/key",
        resolution: 0x0a,
        bitrate_kbps: 6000,
        fps: 3,
        stabilization: Some(0),
        action5_pro: false,
    }
}

#[test]
fn pairs_configures_starts_receives_battery_and_stops() {
    let mut cam = Camera::new();
    let options = opts();
    let settings = config();
    {
        let mut live = start_livestream(&mut cam, &options, &settings, &mut |_| {}).unwrap();
        assert_eq!(live.poll(Duration::from_millis(30)).unwrap(), Some(73));
        live.stop().unwrap();
    }
    let controls: Vec<_> = cam
        .sent
        .iter()
        .filter(|f| {
            matches!(
                (f.cmd_set(), f.cmd_id()),
                (2, 0xe1) | (2, 0x8e) | (7, 0x47) | (8, 0x78)
            )
        })
        .collect();
    assert_eq!(
        controls
            .iter()
            .map(|f| (f.cmd_set(), f.cmd_id()))
            .collect::<Vec<_>>(),
        vec![
            (2, 0xe1),
            (2, 0x8e),
            (7, 0x47),
            (2, 0x8e),
            (8, 0x78),
            (2, 0x8e),
            (2, 0x8e)
        ]
    );
    assert_eq!(controls.last().unwrap().payload, [1, 1, 0x1a, 0, 1, 2]);
    assert!(!cam
        .sent
        .iter()
        .any(|f| (f.cmd_set(), f.cmd_id()) == (0x53, 0x10)));
}

#[test]
fn refused_prepare_does_not_change_the_cameras_network() {
    let mut cam = Camera::new();
    cam.reject_prepare = true;
    assert!(start_livestream(&mut cam, &opts(), &config(), &mut |_| {}).is_err());
    assert!(!cam
        .sent
        .iter()
        .any(|f| (f.cmd_set(), f.cmd_id()) == (7, 0x47)));
}

#[test]
fn nonzero_second_wifi_status_byte_is_rejected() {
    let mut cam = Camera::new();
    cam.wifi_status = [0, 1];
    assert!(start_livestream(&mut cam, &opts(), &config(), &mut |_| {}).is_err());
    assert!(!cam
        .sent
        .iter()
        .any(|f| (f.cmd_set(), f.cmd_id()) == (8, 0x78)));
}

#[test]
fn lost_start_reply_triggers_best_effort_stop() {
    let mut cam = Camera::new();
    cam.silent_start = true;
    assert!(start_livestream(&mut cam, &opts(), &config(), &mut |_| {}).is_err());
    assert!(cam.sent.iter().any(|f| f.payload == [1, 1, 0x1a, 0, 1, 2]));
}

#[test]
fn invalid_inputs_are_rejected_before_any_camera_commands() {
    let mut cam = Camera::new();
    let mut c = config();
    c.url = "";
    assert!(start_livestream(&mut cam, &opts(), &c, &mut |_| {}).is_err());
    assert!(cam.sent.is_empty());
}

#[test]
fn multiple_stale_shared_command_replies_do_not_complete_the_next_step() {
    let mut cam = Camera::new();
    cam.stale_prepare = true;
    assert!(start_livestream(&mut cam, &opts(), &config(), &mut |_| {}).is_err());
    assert!(!cam
        .sent
        .iter()
        .any(|f| (f.cmd_set(), f.cmd_id()) == (7, 0x47)));
}

#[test]
fn cancellation_before_network_join_sends_no_join_command() {
    let flag = AtomicBool::new(false);
    let mut cam = Camera::new();
    let options = opts();
    let result = start_livestream_cancellable(
        &mut cam,
        &options,
        &config(),
        &mut |step| {
            if step == "Connecting camera to Wi-Fi" {
                flag.store(true, Ordering::SeqCst);
            }
        },
        &|| flag.load(Ordering::SeqCst),
    );
    assert!(result.is_err());
    drop(result);
    assert!(!cam
        .sent
        .iter()
        .any(|f| (f.cmd_set(), f.cmd_id()) == (7, 0x47)));
}

#[test]
fn cancellation_after_start_request_attempts_stop() {
    let flag = Arc::new(AtomicBool::new(false));
    let mut cam = Camera::new();
    cam.silent_start = true;
    cam.cancel_on_start = Some(flag.clone());
    let options = opts();
    let result = start_livestream_cancellable(&mut cam, &options, &config(), &mut |_| {}, &|| {
        flag.load(Ordering::SeqCst)
    });
    assert!(result.is_err());
    drop(result);
    assert!(cam.sent.iter().any(|f| f.payload == [1, 1, 0x1a, 0, 1, 2]));
}
