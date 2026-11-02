//! Ready-to-write DUML command frames (BLE `fff5`). Ported from Osmosis `OsmoCommands.kt`.
use super::frame::{pack_string, Frame};

/// Identifier accepted by every Osmo camera; real installs should mint their own.
pub const DEFAULT_IDENTIFIER: &str = "284ae5b8d76b3375a04a6417ad71bea3";

pub const TARGET_APP_TO_WIFI: u16 = 0x0702;
pub const TARGET_APP_TO_CAMERA: u16 = 0x0102;
/// Receiver byte is `(id << 5) | type`: 0xF0 = type 0x10, id 7.
pub const TARGET_APP_TO_SESSION: u16 = 0xF002;
/// 0x1C = type 0x1C, id 0.
pub const TARGET_APP_TO_1C: u16 = 0x1C02;

pub const CMD_WIFI_SSID: u8 = 0x07;
pub const CMD_WIFI_PASSWORD: u8 = 0x0E;
pub const CMD_WIFI_MAC: u8 = 0x0C;

pub const SESSION_WAKE: [u8; 2] = [0x04, 0x00];
pub const SESSION_KEEPALIVE: [u8; 2] = [0x01, 0x01];

/// App device-info blob answering the camera's inbound `0x00/0x81` request.
pub const APP_DEVICE_INFO: [u8; 62] = {
    let mut b = [0u8; 62];
    b[1] = 0x41;
    b[2] = 0x50;
    b[3] = 0x50;
    b[41] = 0x02;
    b[50] = 0x02;
    b[51] = 0x08;
    b
};

/// A fresh per-install identity: UUID-shaped, last group truncated to 8 hex (32 chars total).
/// Mint once and persist; a new one per launch re-prompts for approval on the camera.
pub fn new_install_identity() -> String {
    let b: [u8; 16] = rand::random();
    let h = |r: std::ops::Range<usize>| b[r].iter().map(|x| format!("{x:02x}")).collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        h(0..4),
        h(4..6),
        h(6..8),
        h(8..10),
        h(10..14)
    )
}

fn ty(set: u8, cmd: u8) -> u32 {
    0x40 | (set as u32) << 8 | (cmd as u32) << 16
}

/// Generic request (`flags 0x40`) to an arbitrary command set/id.
pub fn command(set: u8, cmd: u8, payload: &[u8], target: u16, id: u16) -> Vec<u8> {
    Frame {
        target,
        id,
        ty: ty(set, cmd),
        payload: payload.to_vec(),
    }
    .encode()
}

/// SetPairingPIN (`0x07/0x45`): `PackString(identifier) + PackString(pin)`.
pub fn set_pairing_pin(pin: &str, identifier: &str, id: u16) -> Vec<u8> {
    let mut p = pack_string(identifier);
    p.extend(pack_string(pin));
    command(0x07, 0x45, &p, TARGET_APP_TO_WIFI, id)
}

/// Request to the WiFi subsystem (`cmd set 0x07`).
pub fn wifi_query(cmd_id: u8, payload: &[u8], id: u16) -> Vec<u8> {
    command(0x07, cmd_id, payload, TARGET_APP_TO_WIFI, id)
}

/// `0x00/0x2b` session wake / keepalive frame.
pub fn session_ping(payload: &[u8], id: u16) -> Vec<u8> {
    command(0x00, 0x2B, payload, TARGET_APP_TO_SESSION, id)
}

/// `0x53/0x10 00 00 00 00` to type 0x1C: the command that correlates with the camera waking.
pub fn session_5310(id: u16) -> Vec<u8> {
    command(0x53, 0x10, &[0, 0, 0, 0], TARGET_APP_TO_1C, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
    fn sender(f: &[u8]) -> u8 {
        f[4]
    }
    fn receiver(f: &[u8]) -> u8 {
        f[5]
    }
    fn payload(f: &[u8]) -> &[u8] {
        &f[11..f.len() - 2]
    }

    #[test]
    fn pairing_frame_matches_doc() {
        assert_eq!(
            set_pairing_pin("osmo", DEFAULT_IDENTIFIER, 0xA000),
            hex("553304c2020700a0400745203238346165356238643736623333373561303461363431376164373162656133046f736d6f8c02")
        );
    }

    #[test]
    fn wifi_getters_match_docs() {
        assert_eq!(
            wifi_query(CMD_WIFI_SSID, &[], 0xA000),
            hex("550d0433020700a04007077472")
        );
        assert_eq!(
            wifi_query(CMD_WIFI_PASSWORD, &[], 0xA000),
            hex("550d0433020700a040070eb5ef")
        );
        assert_eq!(
            wifi_query(CMD_WIFI_MAC, &[], 0xA000),
            hex("550d0433020700a040070ca7cc")
        );
    }

    #[test]
    fn session_ping_targets_f0_not_camera() {
        let f = session_ping(&SESSION_WAKE, 0xCB1B);
        assert_eq!(f, hex("550f04a202f01bcb40002b04009ab9"));
        assert_eq!(sender(&f), 0x02);
        assert_eq!(receiver(&f), 0xF0);
        assert_eq!(receiver(&f) & 0x1F, 0x10);
        assert_eq!(receiver(&f) >> 5, 7);
        assert_eq!(payload(&f), &[4, 0]);
        assert_eq!(
            session_ping(&SESSION_KEEPALIVE, 0xCB1B),
            hex("550f04a202f01bcb40002b0101abd6")
        );
    }

    #[test]
    fn session_5310_targets_1c() {
        let f = session_5310(0xCB1D);
        assert_eq!(f, hex("55110492021c1dcb40531000000000894a"));
        assert_eq!(receiver(&f), 0x1C);
        assert_eq!((f[9], f[10]), (0x53, 0x10));
        assert_eq!(payload(&f), &[0, 0, 0, 0]);
    }

    #[test]
    fn wifi_query_addresses_wifi_subsystem() {
        let f = wifi_query(0x07, &[], 0x8000);
        assert_eq!((receiver(&f), f[9], f[10]), (0x07, 0x07, 0x07));
    }

    #[test]
    fn app_device_info_is_62_bytes_with_app_tag() {
        assert_eq!(APP_DEVICE_INFO.len(), 62);
        assert_eq!(&APP_DEVICE_INFO[..4], &[0x00, 0x41, 0x50, 0x50]);
        assert_eq!(APP_DEVICE_INFO[41], 0x02);
        assert_eq!(&APP_DEVICE_INFO[50..52], &[0x02, 0x08]);
    }

    #[test]
    fn install_identity_is_32_chars_and_random() {
        let a = new_install_identity();
        assert_eq!(a.len(), 32);
        assert_eq!(a.matches('-').count(), 4);
        assert_ne!(a, new_install_identity());
    }
}
