//! DJI DUML checksums (reflected forms of CRC-8 init 0xEE/poly 0x31 and CRC-16 init 0x496C/poly 0x1021).

pub fn crc8(data: &[u8]) -> u8 {
    let mut crc: u8 = 0x77;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0x8C } else { crc >> 1 };
        }
    }
    crc
}

pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0x3692;
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0x8408 } else { crc >> 1 };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc8_known_values() {
        assert_eq!(crc8(&[0x55, 0x33, 0x04]), 0xC2);
        assert_eq!(crc8(&[0x55, 0x0F, 0x04]), 0xA2);
        assert_eq!(crc8(&[0x55, 0x0D, 0x04]), 0x33);
    }

    #[test]
    fn crc16_known_value() {
        // trailer of the documented SSID query frame 550d0433020700a04007077472
        let body = [0x55, 0x0D, 0x04, 0x33, 0x02, 0x07, 0x00, 0xA0, 0x40, 0x07, 0x07];
        assert_eq!(crc16(&body), 0x7274);
    }
}
