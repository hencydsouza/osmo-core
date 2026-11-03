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

/// Select a unique camera; exact identifiers win over partial name matches.
pub fn select_camera_index(
    cameras: &[(&str, Option<&str>)],
    wanted: Option<&str>,
) -> Result<Option<usize>, &'static str> {
    if let Some(wanted) = wanted {
        if let Some((i, _)) = cameras
            .iter()
            .enumerate()
            .find(|(_, (id, _))| id.eq_ignore_ascii_case(wanted))
        {
            return Ok(Some(i));
        }
    }
    let mut matches = cameras
        .iter()
        .enumerate()
        .filter(|(_, (id, name))| wanted.is_none_or(|w| device_matches(id, *name, w)));
    let first = matches.next().map(|(i, _)| i);
    if matches.next().is_some() {
        return Err("multiple cameras match; use --device with an exact id from `osmo scan`");
    }
    Ok(first)
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

    #[test]
    fn camera_selection_refuses_ambiguous_names_and_prefers_exact_ids() {
        let cameras = [("abc", Some("Action One")), ("def", Some("Action Two"))];
        assert!(select_camera_index(&cameras, None).is_err());
        assert!(select_camera_index(&cameras, Some("action")).is_err());
        assert_eq!(select_camera_index(&cameras, Some("ABC")).unwrap(), Some(0));
        assert_eq!(select_camera_index(&cameras, Some("two")).unwrap(), Some(1));
        assert_eq!(
            select_camera_index(&cameras, Some("missing")).unwrap(),
            None
        );
    }
}
