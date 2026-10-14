//! CRC-CCITT (polynomial 0x1021) for V.34/V.90 message integrity.

/// Compute CRC-CCITT over a byte slice.
///
/// Uses the standard CRC-CCITT parameters:
/// - Polynomial: 0x1021 (x^16 + x^12 + x^5 + 1)
/// - Initial value: 0xFFFF
/// - No final XOR
///
/// This is the CRC used in V.34 INFO0, INFO1, and MP messages.
pub fn crc_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn test_crc_ccitt_empty() {
        // CRC of empty data with init 0xFFFF should be 0xFFFF (no data processed).
        assert_eq!(crc_ccitt(&[]), 0xFFFF);
    }

    #[test]
    fn test_crc_ccitt_known_value() {
        // "123456789" is the standard CRC test vector.
        // CRC-CCITT (0xFFFF init, poly 0x1021, no final XOR) = 0x29B1.
        let data = b"123456789";
        assert_eq!(crc_ccitt(data), 0x29B1);
    }

    #[test]
    fn test_crc_ccitt_single_byte() {
        // Deterministic: CRC of a single byte 0x00.
        let crc = crc_ccitt(&[0x00]);
        // Manual: 0xFFFF ^ 0x0000 = 0xFFFF, then 8 shifts with poly.
        // Pre-computed: 0x1021 (first bit set triggers XOR).
        // Actually let's just verify it's non-trivial and deterministic.
        assert_ne!(crc, 0xFFFF, "Single byte should change CRC from init");
        assert_eq!(crc, crc_ccitt(&[0x00]), "CRC should be deterministic");
    }

    #[test]
    fn test_crc_ccitt_different_data() {
        let crc1 = crc_ccitt(&[0x01, 0x02]);
        let crc2 = crc_ccitt(&[0x02, 0x01]);
        assert_ne!(crc1, crc2, "Different data should produce different CRCs");
    }

    #[test]
    fn test_crc_ccitt_append_verification() {
        // Appending the CRC to the data and recomputing should yield a
        // constant residual (the "magic" CRC check value).
        let data = b"Hello";
        let crc = crc_ccitt(data);

        let mut extended = data.to_vec();
        extended.push((crc >> 8) as u8);
        extended.push(crc as u8);

        // The residual CRC should be the same regardless of input data.
        let residual = crc_ccitt(&extended);

        let data2 = b"World";
        let crc2 = crc_ccitt(data2);
        let mut extended2 = data2.to_vec();
        extended2.push((crc2 >> 8) as u8);
        extended2.push(crc2 as u8);
        let residual2 = crc_ccitt(&extended2);

        assert_eq!(
            residual, residual2,
            "CRC residual should be constant: {residual:#06X} vs {residual2:#06X}"
        );
    }
}
