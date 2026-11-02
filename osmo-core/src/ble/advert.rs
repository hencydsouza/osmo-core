//! BLE constants and manufacturer-advert decoding. Ported from Osmosis `BleConstants.kt` / `BleAdvert.kt`.

pub const SERVICE_FFF0: &str = "0000fff0-0000-1000-8000-00805f9b34fb";
pub const CHAR_FFF4: &str = "0000fff4-0000-1000-8000-00805f9b34fb";
pub const CHAR_FFF5: &str = "0000fff5-0000-1000-8000-00805f9b34fb";

/// DJI manufacturer company ids as the OS reports them (wire bytes are little-endian).
pub const DJI_COMPANY_IDS: [u16; 3] = [0x08AA, 0xF7AA, 0xE5C0];

pub fn is_dji_company_id(id: u16) -> bool {
    DJI_COMPANY_IDS.contains(&id)
}

const MODEL_NAMES: &[(u16, &str)] = &[
    (0x0006, "OsmoAction"),
    (0x0010, "OsmoAction2"),
    (0x0012, "OsmoAction3"),
    (0x0014, "OsmoAction4"),
    (0x0015, "OsmoAction5Pro"),
    (0x0017, "Osmo360"),
    (0x0018, "OsmoAction6"),
    (0x0019, "OsmoNano"),
    (0x0020, "OsmoPocket3"),
    (0x0021, "OsmoPocket4"),
    (0x0022, "OsmoPocket4Pro"),
    (0x0070, "Mavic3"),
    (0x007e, "Neo2"),
];

/// DJI product type (newer advert format) to the classic model id.
const PRODUCT_TYPE_TO_MODEL_ID: &[(u16, u16)] = &[
    (40, 0x0006),
    (143, 0x0010),
    (231, 0x0012),
    (203, 0x0014),
    (235, 0x0015),
    (224, 0x0017),
    (223, 0x0018),
    (222, 0x0019),
    (145, 0x0020),
    (219, 0x0021),
    (218, 0x0022),
    (229, 0x0087),
];

pub fn model_name(id: u16) -> Option<&'static str> {
    MODEL_NAMES.iter().find(|(i, _)| *i == id).map(|(_, n)| *n)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advert {
    pub model_id: Option<u16>,
    pub new_format: bool,
    pub raw_product_type: Option<u16>,
}

fn product_type(p: &[u8]) -> Option<u16> {
    if p.len() <= 5 || p[5] & 0x04 == 0 || p.len() < 12 {
        return None;
    }
    Some(u16::from_le_bytes([p[10], p[11]]))
}

fn legacy_model_id(p: &[u8]) -> Option<u16> {
    if p.len() < 2 {
        return None;
    }
    let id = u16::from_le_bytes([p[0], p[1]]);
    (id != 0).then_some(id)
}

/// Decode a DJI manufacturer payload (company id already stripped).
pub fn decode_advert(payload: &[u8]) -> Advert {
    if let Some(pt) = product_type(payload) {
        let mapped = PRODUCT_TYPE_TO_MODEL_ID
            .iter()
            .find(|(t, _)| *t == pt)
            .map(|(_, m)| *m);
        if mapped.is_some() || legacy_model_id(payload).is_none() {
            return Advert {
                model_id: mapped,
                new_format: true,
                raw_product_type: Some(pt),
            };
        }
    }
    Advert {
        model_id: legacy_model_id(payload),
        new_format: false,
        raw_product_type: product_type(payload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_payload_decodes_action4() {
        let a = decode_advert(&[0x14, 0x00, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x03]);
        assert_eq!(
            a,
            Advert {
                model_id: Some(0x0014),
                new_format: false,
                raw_product_type: None
            }
        );
    }

    #[test]
    fn pocket4_pro_new_format_decodes() {
        let p = [
            0x00, 0x00, 0x00, 0xee, 0x00, 0x04, 0xbd, 0x6e, 0x56, 0x20, 0xda, 0x00, 0x00, 0x10,
        ];
        let a = decode_advert(&p);
        assert_eq!(
            a,
            Advert {
                model_id: Some(0x0022),
                new_format: true,
                raw_product_type: Some(218)
            }
        );
    }

    #[test]
    fn new_format_product_type_203_is_action4() {
        let mut p = [0u8; 14];
        p[5] = 0x04;
        p[10] = 203;
        assert_eq!(decode_advert(&p).model_id, Some(0x0014));
    }

    #[test]
    fn unmapped_new_format_reports_product_type() {
        let mut p = [0u8; 14];
        p[5] = 0x04;
        p[10] = 0x99;
        let a = decode_advert(&p);
        assert_eq!(
            (a.model_id, a.new_format, a.raw_product_type),
            (None, true, Some(0x99))
        );
    }

    #[test]
    fn short_or_zero_payload_has_no_model() {
        assert_eq!(decode_advert(&[0x14]).model_id, None);
        assert_eq!(decode_advert(&[]).model_id, None);
        assert_eq!(decode_advert(&[0, 0, 0, 0]).model_id, None);
    }

    #[test]
    fn company_ids_and_names() {
        assert!(
            is_dji_company_id(0x08AA) && is_dji_company_id(0xF7AA) && is_dji_company_id(0xE5C0)
        );
        assert!(!is_dji_company_id(0x004C));
        assert_eq!(model_name(0x0014), Some("OsmoAction4"));
        assert_eq!(model_name(0x7777), None);
    }
}
