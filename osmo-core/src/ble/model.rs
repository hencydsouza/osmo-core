//! Per-model transport profile, ported from Osmosis `CameraModel.kt`. Only the datalink port, the TCP poke and the
//! Wi-Fi security vary across the line. Only the Action 4 is verified on hardware in this project; the `verified` flag
//! on the other entries records what Osmosis's testers confirmed. An unknown id gets the most common config
//! (UDP 9004 + TCP-7001 poke + WPA2), so it is attempted, not refused. Drones are not supported here.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraModel {
    pub name: &'static str,
    pub datalink_port: u16,
    pub tcp_poke: bool,
    pub wpa3: bool,
    pub verified: bool,
    /// Exactly one store, the microSD, always served at `storage=0` (Pocket 3).
    pub single_sd_storage: bool,
}

const DEFAULT: CameraModel = CameraModel {
    name: "Unknown Osmo",
    datalink_port: 9004,
    tcp_poke: true,
    wpa3: false,
    verified: false,
    single_sd_storage: false,
};

const fn dji(name: &'static str, verified: bool) -> CameraModel {
    CameraModel {
        name,
        verified,
        ..DEFAULT
    }
}

pub fn resolve_model(id: Option<u16>) -> CameraModel {
    match id {
        Some(0x0006) => dji("Osmo Action", false),
        Some(0x0010) => dji("Osmo Action 2", false),
        Some(0x0012) => dji("Osmo Action 3", false),
        Some(0x0014) => dji("Osmo Action 4", true),
        Some(0x0015) => dji("Osmo Action 5 Pro", true),
        Some(0x0017) => CameraModel {
            name: "Osmo 360",
            wpa3: true,
            ..DEFAULT
        },
        Some(0x0018) => dji("Osmo Action 6", true),
        Some(0x0019) => dji("Osmo Nano", true),
        Some(0x0020) => CameraModel {
            name: "Osmo Pocket 3",
            verified: true,
            single_sd_storage: true,
            ..DEFAULT
        },
        Some(0x0021) => dji("Osmo Pocket 4", true),
        Some(0x0022) => dji("Osmo Pocket 4 Pro", true),
        _ => DEFAULT,
    }
}

/// Xtra is a shell company selling rebadged Osmo cameras. It advertises the same model id as its DJI twin, so it is
/// told apart by name here (Osmosis also uses the Bluetooth address prefix, which macOS does not expose).
fn is_xtra(name: Option<&str>) -> bool {
    name.is_some_and(|n| {
        let n = n.to_lowercase();
        n.contains("xtra") || n.contains("edge")
    })
}

/// The profile for a camera seen with `id` and advertised `name`: an Xtra rebadge speaks UDP 10004 with no TCP poke
/// (their firmware, confirmed only on the Edge Pro); anything else uses its DJI profile.
pub fn resolve_profile(id: Option<u16>, name: Option<&str>) -> CameraModel {
    let base = resolve_model(id);
    if !is_xtra(name) {
        return base;
    }
    let (xtra_name, verified) = match id {
        Some(0x0019) => ("Xtra Atto", false),
        Some(0x0014) => ("Xtra Edge", false),
        Some(0x0015) => ("Xtra Edge Pro", true),
        Some(0x0020) => ("Xtra Muse", false),
        _ => ("Xtra Osmo", false),
    };
    CameraModel {
        name: xtra_name,
        datalink_port: 10004,
        tcp_poke: false,
        verified,
        ..base
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

    #[test]
    fn every_osmosis_camera_id_resolves_to_its_name_and_the_dji_profile() {
        for (id, name, verified) in [
            (0x0006, "Osmo Action", false),
            (0x0010, "Osmo Action 2", false),
            (0x0012, "Osmo Action 3", false),
            (0x0014, "Osmo Action 4", true),
            (0x0015, "Osmo Action 5 Pro", true),
            (0x0018, "Osmo Action 6", true),
            (0x0019, "Osmo Nano", true),
            (0x0020, "Osmo Pocket 3", true),
            (0x0021, "Osmo Pocket 4", true),
            (0x0022, "Osmo Pocket 4 Pro", true),
        ] {
            let m = resolve_model(Some(id));
            assert_eq!(
                (m.name, m.datalink_port, m.tcp_poke, m.verified),
                (name, 9004, true, verified),
                "{id:#06x}"
            );
        }
    }

    #[test]
    fn only_the_pocket_3_has_a_single_sd_store() {
        assert!(resolve_model(Some(0x0020)).single_sd_storage);
        assert!(!resolve_model(Some(0x0014)).single_sd_storage);
    }

    #[test]
    fn xtra_cameras_speak_10004_without_the_poke_and_only_the_edge_pro_is_verified() {
        let edge_pro = resolve_profile(Some(0x0015), Some("XtraEdgePro-6E55"));
        assert_eq!(
            (
                edge_pro.name,
                edge_pro.datalink_port,
                edge_pro.tcp_poke,
                edge_pro.verified
            ),
            ("Xtra Edge Pro", 10004, false, true)
        );
        let edge = resolve_profile(Some(0x0014), Some("XtraEdge-1234"));
        assert_eq!(
            (edge.name, edge.datalink_port, edge.tcp_poke, edge.verified),
            ("Xtra Edge", 10004, false, false)
        );
        assert_eq!(
            resolve_profile(Some(0x0019), Some("Xtra Atto")).name,
            "Xtra Atto"
        );
        assert_eq!(
            resolve_profile(Some(0x0020), Some("xtra muse")).name,
            "Xtra Muse"
        );
        let dji = resolve_profile(Some(0x0014), Some("OsmoAction4-1234"));
        assert_eq!((dji.datalink_port, dji.tcp_poke), (9004, true));
    }
}
