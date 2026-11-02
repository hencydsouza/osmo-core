//! Writes to the camera (favourite now, delete next): the wire payloads and the errors every write can end in.
//! A write rides one guarded path in the session: playback must be confirmed, the session must be recently
//! registered (the camera stops answering writes on old sessions), and a missing answer is never success.
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WriteError {
    #[error("the session is {age_secs}s old, and the camera stops answering writes on sessions older than about 40 s: reconnect and try again")]
    SessionTooOld { age_secs: u64 },
    #[error(
        "the camera is not in playback mode, so it would ignore this write: run the command again"
    )]
    NotInPlayback,
    #[error("this file has no usable command handle (its list could not be fitted), so the command cannot address it")]
    NoHandle,
    #[error("{0} has no verified, unshared handle, so it will not be deleted")]
    NotDeletable(String),
    #[error("at most {max} files can be deleted at once")]
    TooMany { max: usize },
    #[error("the camera refused the command (status 0x{0:04x})")]
    Rejected(u16),
    #[error("the camera did not answer, so it may or may not have happened: re-list to check")]
    NoReply,
}

/// `0x02/0xBF` favourite: `01 01 [handle:u32-LE][counter:u32-LE] 00 [on:u8] 00 00 00`. The handle is the
/// record's command handle (`cmd_handle`), which stills have too.
pub fn favourite_payload(handle: u32, counter: u32, on: bool) -> Vec<u8> {
    let mut p = vec![0x01, 0x01];
    p.extend(handle.to_le_bytes());
    p.extend(counter.to_le_bytes());
    p.extend([0x00, u8::from(on), 0x00, 0x00, 0x00]);
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duml::Frame;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn favourite_payload_matches_the_documented_frame() {
        let payload = favourite_payload(0x4010_4040, 1, true);
        assert_eq!(payload, hex("010140401040010000000001000000"));
        let frame = Frame {
            target: 0x0102,
            id: 0xFDBE,
            ty: 0x40 | 0x02 << 8 | 0xBF << 16,
            payload,
        }
        .encode();
        assert_eq!(
            frame,
            hex("551c041b0201befd4002bf0101404010400100000000010000008c88")
        );
        assert_eq!(
            favourite_payload(0x4010_4040, 2, false)[10],
            0x00,
            "on=false clears the flag byte"
        );
    }

    #[test]
    fn write_errors_tell_the_user_what_to_do() {
        let msg = |e: WriteError| e.to_string();
        assert!(
            msg(WriteError::SessionTooOld { age_secs: 55 }).contains("55")
                && msg(WriteError::SessionTooOld { age_secs: 55 }).contains("reconnect")
        );
        assert!(msg(WriteError::NotInPlayback).contains("playback"));
        assert!(msg(WriteError::NoHandle).contains("handle"));
        assert!(msg(WriteError::NotDeletable("DCIM/a/b.JPG".into())).contains("DCIM/a/b.JPG"));
        assert!(msg(WriteError::TooMany { max: 20 }).contains("20"));
        assert!(msg(WriteError::Rejected(0xE0)).contains("0x00e0"));
        let no_reply = msg(WriteError::NoReply);
        assert!(
            no_reply.contains("may or may not") && no_reply.contains("re-list"),
            "{no_reply}"
        );
    }
}
