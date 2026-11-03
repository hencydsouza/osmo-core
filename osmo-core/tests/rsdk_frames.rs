use osmo_core::rsdk::{
    connection_request, gps_payload, status_subscription, Frame, GpsFix, Reassembler,
};

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn connection_request_matches_djis_reference_frame() {
    let payload = connection_request(0x12345678, [0x38, 0x34, 0x56, 0x78, 0x9a, 0xbc], 0, 1234);
    let frame = Frame {
        set: 0,
        id: 0x19,
        kind: 2,
        seq: 1,
        payload: payload.to_vec(),
    };
    assert_eq!(frame.encode().unwrap(), hex("AA330002000000000100982F00197856341206383456789ABC00000000000000000000000000000000D20400000000A70F381B"));
}

#[test]
fn gps_payload_matches_djis_reference_frame() {
    let fix = GpsFix {
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
    };
    let frame = Frame {
        set: 0,
        id: 0x17,
        kind: 0,
        seq: 2,
        payload: gps_payload(&fix).unwrap().to_vec(),
    };
    assert_eq!(frame.encode().unwrap(), hex("AA420000000000000200D34D00176E2735019D2E0200009749018023AD1810A4000000004841000050C000000000E8030000E80300000A0000000B000000FE2937C4"));
}

#[test]
fn status_frame_roundtrips_and_rejects_corruption() {
    let f = Frame {
        set: 0x1d,
        id: 5,
        kind: 0,
        seq: 3,
        payload: status_subscription(2, 20).to_vec(),
    };
    let mut wire = f.encode().unwrap();
    assert_eq!(
        wire,
        hex("AA180000000000000300507E1D05021400000000FA4A4931")
    );
    assert_eq!(Frame::decode(&wire).unwrap(), f);
    wire[20] ^= 1;
    assert!(Frame::decode(&wire).is_err());
    assert!(Frame::decode(&[0xaa; 17]).is_err());
}

#[test]
fn reassembler_handles_noise_fragments_and_multiple_frames() {
    let f = Frame {
        set: 0,
        id: 0x19,
        kind: 0x20,
        seq: 12,
        payload: vec![1, 2, 3],
    };
    let bytes = f.encode().unwrap();
    let mut decoder = Reassembler::default();
    assert!(decoder.feed(&[0, 0xff, bytes[0], bytes[1]]).is_empty());
    assert!(decoder.feed(&bytes[2..8]).is_empty());
    let mut tail = bytes[8..].to_vec();
    tail.extend(&bytes);
    assert_eq!(decoder.feed(&tail), vec![f.clone(), f]);
}

#[test]
fn invalid_gps_coordinates_and_nonfinite_velocity_are_rejected() {
    let mut f = GpsFix {
        date: 20260718,
        time: 143005,
        longitude_e7: 0,
        latitude_e7: 0,
        height_mm: 0,
        north_cm_s: 0.0,
        east_cm_s: 0.0,
        down_cm_s: 0.0,
        vertical_accuracy_mm: 0,
        horizontal_accuracy_mm: 0,
        speed_accuracy_cm_s: 0,
        satellites: 0,
    };
    f.latitude_e7 = 900_000_001;
    assert!(gps_payload(&f).is_err());
    f.latitude_e7 = 0;
    f.north_cm_s = f32::NAN;
    assert!(gps_payload(&f).is_err());
}
