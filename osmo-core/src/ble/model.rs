//! Per-model transport profile. Only the Action 4 is hardware-verified here; everything else
//! gets the most common config (UDP 9004 + TCP-7001 poke + WPA2) so it is attempted, not refused.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraModel {
    pub name: &'static str,
    pub datalink_port: u16,
    pub tcp_poke: bool,
    pub wpa3: bool,
    pub verified: bool,
}

const DEFAULT: CameraModel = CameraModel {
    name: "Unknown Osmo",
    datalink_port: 9004,
    tcp_poke: true,
    wpa3: false,
    verified: false,
};

pub fn resolve_model(id: Option<u16>) -> CameraModel {
    match id {
        Some(0x0014) => CameraModel {
            name: "Osmo Action 4",
            verified: true,
            ..DEFAULT
        },
        Some(0x0017) => CameraModel {
            name: "Osmo 360",
            wpa3: true,
            ..DEFAULT
        },
        _ => DEFAULT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action4_profile_is_verified_9004_with_poke() {
        let m = resolve_model(Some(0x0014));
        assert_eq!(
            (m.name, m.datalink_port, m.tcp_poke, m.wpa3, m.verified),
            ("Osmo Action 4", 9004, true, false, true)
        );
    }

    #[test]
    fn unknown_model_falls_back_to_defaults_unverified() {
        let m = resolve_model(None);
        assert_eq!(
            (m.datalink_port, m.tcp_poke, m.wpa3, m.verified),
            (9004, true, false, false)
        );
        assert!(!resolve_model(Some(0x7777)).verified);
    }

    #[test]
    fn osmo_360_uses_wpa3() {
        assert!(resolve_model(Some(0x0017)).wpa3);
    }
}
