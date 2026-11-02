use osmo_core::ble::is_dji_company_id;

/// DJI manufacturer id present, or a name that looks like an Osmo/Xtra camera.
pub fn looks_like_camera(name: Option<&str>, company_ids: &[u16]) -> bool {
    if company_ids.iter().any(|&c| is_dji_company_id(c)) {
        return true;
    }
    let n = name.unwrap_or("").to_lowercase();
    ["osmo", "nano", "dji", "pocket", "action", "xtra", "edge"]
        .iter()
        .any(|k| n.contains(k))
}

/// `--device` matches a peripheral id exactly or a case-insensitive name substring.
pub fn device_matches(id: &str, name: Option<&str>, wanted: &str) -> bool {
    id.eq_ignore_ascii_case(wanted)
        || name.is_some_and(|n| n.to_lowercase().contains(&wanted.to_lowercase()))
}

pub fn bluetooth_hint() -> String {
    "Bluetooth unavailable: check Bluetooth is on and this terminal has Bluetooth permission \
     (System Settings > Privacy & Security > Bluetooth)."
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_dji_company_id_with_any_name() {
        assert!(looks_like_camera(None, &[0x08AA]));
        assert!(looks_like_camera(Some("whatever"), &[0xF7AA]));
    }

    #[test]
    fn accepts_osmo_names() {
        assert!(looks_like_camera(Some("OsmoAction4-1234"), &[]));
        assert!(looks_like_camera(Some("Osmo Pocket 3"), &[]));
        assert!(looks_like_camera(Some("XtraEdgePro-6E55"), &[]));
    }

    #[test]
    fn rejects_everything_else() {
        assert!(!looks_like_camera(None, &[]));
        assert!(!looks_like_camera(Some("JBL Flip 5"), &[0x004C]));
    }

    #[test]
    fn bluetooth_hint_mentions_permission_and_power() {
        let h = bluetooth_hint();
        assert!(h.contains("Bluetooth is on") && h.contains("Privacy"));
    }

    #[test]
    fn device_matches_by_id_or_name_substring() {
        assert!(device_matches(
            "ABC-123",
            Some("OsmoAction4-1234"),
            "abc-123"
        ));
        assert!(device_matches(
            "ABC-123",
            Some("OsmoAction4-1234"),
            "action4"
        ));
        assert!(!device_matches("ABC-123", Some("OsmoNano-C2D8"), "action4"));
        assert!(!device_matches("ABC-123", None, "action4"));
    }
}
