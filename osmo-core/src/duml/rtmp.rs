//! RTMP control frames ported from Osmosis / djictl (CC0-1.0).
use super::commands::{command, TARGET_APP_TO_CAMERA, TARGET_APP_TO_WIFI};
use super::pack_string;
use thiserror::Error;

const VIDEO_TRANSMISSION: u16 = 0x0802;
const MAX_PAYLOAD: usize = 242;
pub const MAX_URL_BYTES: usize = 228;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BuildError {
    #[error("RTMP URL exceeds 228 UTF-8 bytes")]
    UrlTooLong,
    #[error("Wi-Fi SSID or password is too long for the camera command")]
    WifiTooLong,
}

fn stream_cmd(set: u8, id: u8, payload: &[u8], target: u16, wire_id: u16) -> Vec<u8> {
    command(set, id, payload, target, wire_id.swap_bytes())
}

pub fn prepare_stage1() -> Vec<u8> {
    stream_cmd(2, 0xe1, &[0x1a], VIDEO_TRANSMISSION, 0xfeab)
}

pub fn prepare_stage2() -> Vec<u8> {
    stream_cmd(2, 0x8e, &[0, 1, 0x1c, 0], VIDEO_TRANSMISSION, 0xffab)
}

pub fn connect_wifi(ssid: &str, password: &str, id: u16) -> Result<Vec<u8>, BuildError> {
    if ssid.len() > u8::MAX as usize
        || password.len() > u8::MAX as usize
        || ssid.len() + password.len() + 2 > MAX_PAYLOAD
    {
        return Err(BuildError::WifiTooLong);
    }
    let mut payload = pack_string(ssid);
    payload.extend(pack_string(password));
    Ok(command(7, 0x47, &payload, TARGET_APP_TO_WIFI, id))
}

pub fn configure_stream(
    resolution: u8,
    bitrate_kbps: u16,
    fps: u8,
    url: &str,
    action5_pro: bool,
) -> Result<Vec<u8>, BuildError> {
    if url.len() > MAX_URL_BYTES {
        return Err(BuildError::UrlTooLong);
    }
    let mut payload = vec![0, if action5_pro { 0x2e } else { 0x2a }, 0, resolution];
    payload.extend(bitrate_kbps.to_le_bytes());
    payload.extend([2, 0, fps, 0, 0, 0]);
    payload.extend((url.len() as u16).to_le_bytes());
    payload.extend(url.as_bytes());
    Ok(stream_cmd(8, 0x78, &payload, VIDEO_TRANSMISSION, 0xb3bb))
}

pub fn start_stream() -> Vec<u8> {
    stream_cmd(2, 0x8e, &[1, 1, 0x1a, 0, 1, 1], VIDEO_TRANSMISSION, 0xb4bb)
}

pub fn stop_stream() -> Vec<u8> {
    stream_cmd(2, 0x8e, &[1, 1, 0x1a, 0, 1, 2], VIDEO_TRANSMISSION, 0xb5bb)
}

pub fn stabilization(value: u8, action5_pro: bool) -> Vec<u8> {
    stream_cmd(
        2,
        0x8e,
        &[1, 1, if action5_pro { 0x1a } else { 8 }, 0, 1, value],
        TARGET_APP_TO_CAMERA,
        0,
    )
}
