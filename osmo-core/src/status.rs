//! Live camera status decoded from the pushes the session already receives. Ported from Osmosis
//! `DumlSession.applyStatusFrame`; only the layouts Osmosis verified are decoded, everything else is ignored.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub battery_percent: Option<u8>,
    pub battery_millivolts: Option<u32>,
    pub charging: Option<bool>,
    pub docked: Option<bool>,
    pub sd_total_mb: Option<u32>,
    pub sd_free_mb: Option<u32>,
    /// `Some(0)` means the camera confirmed it has no built-in store (a single-store body).
    pub internal_total_mb: Option<u32>,
    pub internal_free_mb: Option<u32>,
}

fn u32le(p: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]])
}

/// A capacity above 50 TB of MiB is garbage, not a card.
fn sane(v: u32) -> bool {
    v <= 50_000_000
}

impl Status {
    /// Fold one pushed frame into the status. Returns whether it carried anything we decode.
    pub fn apply(&mut self, set: u8, cmd: u8, p: &[u8]) -> bool {
        match (set, cmd) {
            // `0x02/0xDC`: one `[total][free]` u32-LE MiB block per store. Card at 6/10, built-in at 24/28;
            // a single-store body sends a 22-byte frame with only the first block.
            (0x02, 0xDC) if p.len() >= 22 => {
                let (sd_total, sd_free) = (u32le(p, 6), u32le(p, 10));
                // A Nano pushes `first=0/0` while playback is held, whatever is mounted. Keep real figures.
                let blanked = sd_total == 0 && self.sd_total_mb.is_some_and(|t| t > 0);
                if sane(sd_total) && !blanked {
                    self.sd_total_mb = Some(sd_total);
                }
                if sane(sd_free) && !blanked {
                    self.sd_free_mb = Some(sd_free);
                }
                if p.len() >= 32 {
                    let (total, free) = (u32le(p, 24), u32le(p, 28));
                    if sane(total) {
                        self.internal_total_mb = Some(total);
                    }
                    if sane(free) {
                        self.internal_free_mb = Some(free);
                    }
                } else {
                    (self.internal_total_mb, self.internal_free_mb) = (Some(0), Some(0));
                }
                true
            }
            // `0x0D/0x02`: percent at 20; on the 34-byte form also pack mV at 1, dock at 27, charging at 32.
            (0x0D, 0x02) if p.len() >= 21 => {
                if p.len() >= 34 {
                    let mv = u16::from_le_bytes([p[1], p[2]]) as u32;
                    if (2000..=5000).contains(&mv) {
                        self.battery_millivolts = Some(mv);
                    }
                    self.docked = Some(p[27] != 0);
                    self.charging = Some(p[32] == 1);
                }
                if p[20] <= 100 {
                    self.battery_percent = Some(p[20]);
                    return true;
                }
                p.len() >= 34
            }
            _ => false,
        }
    }
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
    fn storage(payload: &str) -> Status {
        let mut s = Status::default();
        assert!(s.apply(0x02, 0xDC, &hex(payload)));
        s
    }

    // Verbatim 0x02/0xDC frames off real cameras (Osmosis StorageStatusTest).
    const ACTION6: &str = "001202000001b9db0100b4ac0100000000002a2b000001011dc8000072c50000";
    const ACTION5: &str = "00120200000166e80000647f000000000000de0a0000010154bf000075b20000";
    const XTRA_NO_CARD: &str = "11120200000200000000000000000000000000000000010154bf000003b50000";
    const OSMO360: &str = "0012020000011f7207008c71070000000000c66b000001015fa60100e2a50100";
    const ACTION5_BIG: &str = "001202000001b9db01003ddb010000000000ed270000010154bf000024bf0000";
    const POCKET3: &str = "001201000001b9db0100230a0100000000004e180000";

    #[test]
    fn card_block_matches_the_capacity_the_camera_shows() {
        let s = storage(ACTION6);
        assert_eq!(
            (s.sd_total_mb, s.sd_free_mb),
            (Some(121_785), Some(109_748))
        );
    }

    #[test]
    fn no_card_reports_zero_capacity_and_the_built_in_still_decodes() {
        let s = storage(XTRA_NO_CARD);
        assert_eq!(
            (s.sd_total_mb, s.internal_total_mb),
            (Some(0), Some(48_980))
        );
    }

    #[test]
    fn both_stores_decode_independently() {
        let s = storage(ACTION5);
        assert_eq!(
            (s.sd_total_mb, s.internal_total_mb),
            (Some(59_494), Some(48_980))
        );
        let big = storage(ACTION5_BIG);
        assert_eq!(
            (big.sd_total_mb, big.internal_total_mb),
            (Some(121_785), Some(48_980))
        );
        let s360 = storage(OSMO360);
        assert_eq!(
            (s360.sd_total_mb, s360.internal_total_mb),
            (Some(487_967), Some(108_127))
        );
    }

    #[test]
    fn single_store_22_byte_frame_decodes_the_card_and_reports_no_built_in() {
        let s = storage(POCKET3);
        assert_eq!((s.sd_total_mb, s.sd_free_mb), (Some(121_785), Some(68_131)));
        assert_eq!(
            (s.internal_total_mb, s.internal_free_mb),
            (Some(0), Some(0))
        );
    }

    #[test]
    fn a_zeroed_first_block_after_a_real_capacity_is_ignored() {
        let mut s = storage(ACTION6);
        let mut blanked = hex(ACTION6);
        blanked[6..14].fill(0);
        assert!(s.apply(0x02, 0xDC, &blanked));
        assert_eq!(
            (s.sd_total_mb, s.sd_free_mb),
            (Some(121_785), Some(109_748)),
            "kept, not blanked"
        );
    }

    #[test]
    fn battery_push_decodes_percent_voltage_charging_and_dock() {
        let mut p = vec![0u8; 34];
        p[1..3].copy_from_slice(&4100u16.to_le_bytes());
        p[20] = 87;
        p[27] = 1;
        p[32] = 1;
        let mut s = Status::default();
        assert!(s.apply(0x0D, 0x02, &p));
        assert_eq!(
            (
                s.battery_percent,
                s.battery_millivolts,
                s.charging,
                s.docked
            ),
            (Some(87), Some(4100), Some(true), Some(true))
        );
    }

    #[test]
    fn a_short_battery_push_still_gives_the_percentage_and_a_bogus_one_is_ignored() {
        let mut short = vec![0u8; 21];
        short[20] = 55;
        let mut s = Status::default();
        assert!(s.apply(0x0D, 0x02, &short));
        assert_eq!(s.battery_percent, Some(55));
        let mut bogus = vec![0u8; 21];
        bogus[20] = 150;
        assert!(!s.apply(0x0D, 0x02, &bogus));
        assert_eq!(
            s.battery_percent,
            Some(55),
            "an out-of-range reading never overwrites a good one"
        );
    }

    #[test]
    fn unknown_frames_and_short_payloads_are_ignored_without_panicking() {
        let mut s = Status::default();
        assert!(!s.apply(0x7F, 0x01, &[1, 2, 3]));
        assert!(!s.apply(0x02, 0xDC, &[0; 5]));
        assert!(!s.apply(0x0D, 0x02, &[]));
        assert_eq!(s, Status::default());
    }
}
