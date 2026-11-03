use osmo_core::ble::{BleLink, LinkError};
use osmo_core::rsdk::{connect, connection_request, Frame, GpsFix, Identity, SessionError};
use std::collections::VecDeque;
use std::time::Duration;

struct Camera {
    incoming: VecDeque<Vec<u8>>,
    sent: Vec<Frame>,
    approve: bool,
    reject: bool,
    status: Option<Vec<u8>>,
}
impl Camera {
    fn new() -> Self {
        Self {
            incoming: VecDeque::new(),
            sent: vec![],
            approve: true,
            reject: false,
            status: None,
        }
    }
}
impl BleLink for Camera {
    fn write_command(&mut self, bytes: &[u8]) -> Result<(), LinkError> {
        let f = Frame::decode(bytes).unwrap();
        if (f.set, f.id, f.kind) == (0, 0x19, 2) {
            if self.reject {
                let mut p = vec![0; 9];
                p[4] = 3;
                self.incoming.push_back(
                    Frame {
                        set: 0,
                        id: 0x19,
                        kind: 0x20,
                        seq: f.seq,
                        payload: p,
                    }
                    .encode()
                    .unwrap(),
                );
            } else if self.approve {
                let p = connection_request(0x33, [1, 2, 3, 4, 5, 6], 2, 0);
                let approval = Frame {
                    set: 0,
                    id: 0x19,
                    kind: 0,
                    seq: 0x1234,
                    payload: p.to_vec(),
                }
                .encode()
                .unwrap();
                self.incoming.push_back(approval[..8].to_vec());
                self.incoming.push_back(approval[8..].to_vec());
                if let Some(payload) = self.status.take() {
                    self.incoming.push_back(
                        Frame {
                            set: 0x1d,
                            id: 2,
                            kind: 0,
                            seq: 7,
                            payload,
                        }
                        .encode()
                        .unwrap(),
                    );
                }
            }
        }
        self.sent.push(f);
        Ok(())
    }
    fn recv(&mut self, wait: Duration) -> Result<Option<Vec<u8>>, LinkError> {
        if let Some(f) = self.incoming.pop_front() {
            return Ok(Some(f));
        }
        std::thread::sleep(wait.min(Duration::from_millis(1)));
        Ok(None)
    }
}
fn identity() -> Identity {
    Identity {
        device_id: 0x12345678,
        mac: [1, 2, 3, 4, 5, 6],
    }
}
fn fix() -> GpsFix {
    GpsFix {
        date: 20260718,
        time: 143005,
        longitude_e7: 21600000,
        latitude_e7: 414000000,
        height_mm: 42000,
        north_cm_s: 12.5,
        east_cm_s: -3.25,
        down_cm_s: 0.0,
        vertical_accuracy_mm: 1000,
        horizontal_accuracy_mm: 1000,
        speed_accuracy_cm_s: 10,
        satellites: 11,
    }
}

#[test]
fn approved_connection_is_acked_before_status_subscription_and_gps() {
    let mut cam = Camera::new();
    {
        let mut session = connect(
            &mut cam,
            identity(),
            1234,
            Duration::from_millis(100),
            &|| false,
        )
        .unwrap();
        session.send_fix(&fix()).unwrap();
    }
    assert_eq!(
        cam.sent
            .iter()
            .map(|f| (f.set, f.id, f.kind))
            .collect::<Vec<_>>(),
        vec![(0, 0x19, 2), (0, 0x19, 0x20), (0x1d, 5, 0), (0, 0x17, 0)]
    );
    assert_eq!(cam.sent[1].seq, 0x1234);
    assert_eq!(&cam.sent[1].payload[..4], &0x12345678u32.to_le_bytes());
    assert_eq!(cam.sent[2].payload, [3, 20, 0, 0, 0, 0]);
    assert_eq!(cam.sent[3].payload.len(), 48);
}

#[test]
fn rejection_and_timeout_never_send_gps() {
    let mut cam = Camera::new();
    cam.reject = true;
    assert!(matches!(
        connect(
            &mut cam,
            identity(),
            1234,
            Duration::from_millis(20),
            &|| false
        ),
        Err(SessionError::Rejected)
    ));
    assert!(!cam.sent.iter().any(|f| f.id == 0x17));
    let mut cam = Camera::new();
    cam.approve = false;
    assert!(matches!(
        connect(
            &mut cam,
            identity(),
            1234,
            Duration::from_millis(20),
            &|| false
        ),
        Err(SessionError::Timeout)
    ));
}

#[test]
fn cancelled_before_connection_sends_nothing() {
    let mut cam = Camera::new();
    assert!(matches!(
        connect(
            &mut cam,
            identity(),
            1234,
            Duration::from_millis(20),
            &|| true
        ),
        Err(SessionError::Cancelled)
    ));
    assert!(cam.sent.is_empty());
}

#[test]
fn status_is_decoded_and_invalid_fix_is_never_sent() {
    let mut cam = Camera::new();
    let mut status = vec![0; 46];
    status[0] = 1;
    status[1] = 3;
    status[5] = 5;
    status[45] = 77;
    cam.status = Some(status);
    {
        let mut session = connect(
            &mut cam,
            identity(),
            1234,
            Duration::from_millis(100),
            &|| false,
        )
        .unwrap();
        let status = session.poll(Duration::from_millis(50)).unwrap().unwrap();
        assert_eq!(
            (
                status.mode,
                status.state,
                status.record_seconds,
                status.battery_percent
            ),
            (1, 3, 5, Some(77))
        );
        let mut bad = fix();
        bad.latitude_e7 = 900_000_001;
        assert!(session.send_fix(&bad).is_err());
    }
    assert!(!cam.sent.iter().any(|f| f.id == 0x17));
}

#[test]
fn quiet_status_feed_is_resubscribed() {
    let mut cam = Camera::new();
    {
        let mut session = connect(
            &mut cam,
            identity(),
            1234,
            Duration::from_millis(100),
            &|| false,
        )
        .unwrap();
        assert!(session.poll(Duration::from_millis(1700)).unwrap().is_none());
    }
    let subscriptions = cam
        .sent
        .iter()
        .filter(|f| (f.set, f.id) == (0x1d, 5))
        .count();
    assert!(
        subscriptions >= 2,
        "the silent status feed must be resubscribed"
    );
}
