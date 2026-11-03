use osmo_core::duml::{rtmp, Frame};

#[test]
fn configure_matches_the_documented_action4_wire_layout() {
    let url = "rtmp://192.168.0.131:1946/test/stream0/";
    let wire = rtmp::configure_stream(0x0a, 6000, 3, url, false).unwrap();
    assert_eq!(&wire[..3], &[0x55, 0x42, 0x04]);
    assert_eq!(&wire[4..11], &[0x02, 0x08, 0xb3, 0xbb, 0x40, 0x08, 0x78]);
    let frame = Frame::decode(&wire).unwrap();
    assert_eq!(
        &frame.payload[..14],
        &[0, 0x2a, 0, 0x0a, 0x70, 0x17, 2, 0, 3, 0, 0, 0, 39, 0]
    );
    assert_eq!(&frame.payload[14..], url.as_bytes());
}

#[test]
fn control_frames_use_the_reference_targets_ids_and_payloads() {
    for (wire, id, target, payload) in [
        (rtmp::prepare_stage1(), [0xfe, 0xab], 0x0802, vec![0x1a]),
        (
            rtmp::prepare_stage2(),
            [0xff, 0xab],
            0x0802,
            vec![0, 1, 0x1c, 0],
        ),
        (
            rtmp::start_stream(),
            [0xb4, 0xbb],
            0x0802,
            vec![1, 1, 0x1a, 0, 1, 1],
        ),
        (
            rtmp::stop_stream(),
            [0xb5, 0xbb],
            0x0802,
            vec![1, 1, 0x1a, 0, 1, 2],
        ),
        (
            rtmp::stabilization(3, false),
            [0, 0],
            0x0102,
            vec![1, 1, 8, 0, 1, 3],
        ),
    ] {
        assert_eq!(&wire[6..8], &id);
        let f = Frame::decode(&wire).unwrap();
        assert_eq!(f.target, target);
        assert_eq!(f.payload, payload);
    }
}

#[test]
fn wifi_credentials_are_length_prefixed_and_oversized_inputs_are_rejected() {
    let f = Frame::decode(&rtmp::connect_wifi("café", "password", 0x8c19).unwrap()).unwrap();
    assert_eq!(f.target, 0x0702);
    assert_eq!((f.cmd_set(), f.cmd_id()), (7, 0x47));
    assert_eq!(f.payload[0], 5);
    assert_eq!(&f.payload[1..6], "café".as_bytes());
    assert_eq!(&f.payload[6..], b"\x08password");
    assert!(rtmp::connect_wifi(&"x".repeat(256), "x", 1).is_err());
    assert!(rtmp::connect_wifi(&"x".repeat(200), &"y".repeat(41), 1).is_err());
}

#[test]
fn url_limit_is_measured_in_bytes_and_never_wraps_frame_length() {
    assert!(rtmp::configure_stream(0x0a, 6000, 3, &"x".repeat(228), false).is_ok());
    assert!(rtmp::configure_stream(0x0a, 6000, 3, &"x".repeat(229), false).is_err());
    assert!(rtmp::configure_stream(0x0a, 6000, 3, &"é".repeat(115), false).is_err());
}
