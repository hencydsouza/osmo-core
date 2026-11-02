use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LinkError {
    #[error("BLE link disconnected")]
    Disconnected,
    #[error("BLE link error: {0}")]
    Other(String),
}

/// Blocking GATT channel to a camera. The implementation has already connected, enabled
/// notifications on `fff4`/`fff5` and armed pairing; the core only moves DUML bytes.
pub trait BleLink {
    /// Write a DUML frame to `fff5` (write-without-response).
    fn write_command(&mut self, bytes: &[u8]) -> Result<(), LinkError>;
    /// Next raw notification fragment, or `None` if none arrived within `timeout`.
    fn recv(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, LinkError>;
}
