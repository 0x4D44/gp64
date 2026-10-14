//! Host-independent bit mappings shared by the native and software x87 backends.

/// Widen a normal binary32 value (or signed zero) to its exact Intel F80 bytes.
/// Exceptional and subnormal values return `None` so each backend can preserve
/// its established conversion behavior for those edge cases.
#[inline(always)]
pub(crate) fn f32_normal_or_zero_to_f80(value: f32) -> Option<[u8; 10]> {
    let bits = value.to_bits();
    let sign = bits >> 31;
    let exponent = (bits >> 23) & 0xff;
    let fraction = bits & 0x007f_ffff;

    if exponent == 0 {
        if fraction != 0 {
            return None;
        }
        let mut bytes = [0u8; 10];
        bytes[9] = (sign as u8) << 7;
        return Some(bytes);
    }
    if exponent == 0xff {
        return None;
    }

    let f80_exponent = (exponent as u16) + (16_383 - 127);
    let significand = (u64::from(fraction | 0x0080_0000)) << 40;
    let sign_exponent = f80_exponent | ((sign as u16) << 15);
    let mut bytes = [0u8; 10];
    bytes[..8].copy_from_slice(&significand.to_le_bytes());
    bytes[8..].copy_from_slice(&sign_exponent.to_le_bytes());
    Some(bytes)
}

/// Recover binary32 only when the F80 input is a canonical normal value that
/// binary32 represents exactly as a normal value. No rounding occurs here.
#[inline(always)]
pub(crate) fn f80_to_exact_normal_f32(bytes: &[u8; 10]) -> Option<f32> {
    let sign_exponent = u16::from_le_bytes([bytes[8], bytes[9]]);
    let f80_exponent = i32::from(sign_exponent & 0x7fff);
    let f32_exponent = f80_exponent - 16_383 + 127;
    if !(1..=254).contains(&f32_exponent) {
        return None;
    }

    let significand = u64::from_le_bytes(bytes[..8].try_into().expect("fixed F80 width"));
    if significand & 0x8000_0000_0000_0000 == 0 || significand & 0x0000_00ff_ffff_ffff != 0 {
        return None;
    }

    let sign = u32::from(sign_exponent >> 15);
    let fraction = ((significand >> 40) as u32) & 0x007f_ffff;
    Some(f32::from_bits(
        (sign << 31) | ((f32_exponent as u32) << 23) | fraction,
    ))
}

/// Recover binary32 only when the F80 input is signed zero or a canonical
/// normal value that binary32 represents exactly. No rounding occurs here.
#[inline(always)]
pub(crate) fn f80_to_exact_normal_or_zero_f32(bytes: &[u8; 10]) -> Option<f32> {
    let sign_exponent = u16::from_le_bytes([bytes[8], bytes[9]]);
    let exponent = sign_exponent & 0x7fff;
    let significand = u64::from_le_bytes(bytes[..8].try_into().expect("fixed F80 width"));
    if exponent == 0 && significand == 0 {
        return Some(f32::from_bits(u32::from(sign_exponent >> 15) << 31));
    }
    f80_to_exact_normal_f32(bytes)
}

/// Recover the exact `f64` value of a canonical F80 zero/normal, when
/// binary64 represents it with no rounding. NaN/Inf, denormals, and
/// unnormal/pseudo-denormal forms (explicit integer bit clear with a nonzero
/// exponent) return `None`, as does any magnitude outside binary64's normal
/// exponent range — all of these are the caller's cue to fall back. Mirrors
/// `f80_to_exact_normal_or_zero_f32`, widened to 52 mantissa bits.
#[inline(always)]
pub(crate) fn f80_to_exact_normal_or_zero_f64(bytes: &[u8; 10]) -> Option<f64> {
    let sign_exponent = u16::from_le_bytes([bytes[8], bytes[9]]);
    let exponent = sign_exponent & 0x7fff;
    let significand = u64::from_le_bytes(bytes[..8].try_into().expect("fixed F80 width"));
    let sign = u64::from(sign_exponent >> 15);

    if exponent == 0 && significand == 0 {
        return Some(f64::from_bits(sign << 63));
    }
    if exponent == 0 || exponent == 0x7fff {
        return None; // denormal, or NaN/infinity
    }
    if significand & 0x8000_0000_0000_0000 == 0 {
        return None; // unnormal / pseudo form: explicit integer bit clear
    }
    if significand & 0x7ff != 0 {
        return None; // low 11 fraction bits nonzero: not exactly binary64
    }
    let unbiased = i32::from(exponent) - 16_383;
    if !(-1022..=1023).contains(&unbiased) {
        return None; // outside binary64's normal exponent range
    }
    let f64_exponent = (unbiased + 1023) as u64;
    let fraction = (significand >> 11) & 0x000f_ffff_ffff_ffff;
    Some(f64::from_bits(
        (sign << 63) | (f64_exponent << 52) | fraction,
    ))
}

/// Widen a normal binary64 value (or signed zero) to its exact Intel F80
/// bytes. Subnormal and exceptional values return `None`, mirroring
/// `f32_normal_or_zero_to_f80`.
#[inline(always)]
pub(crate) fn f64_normal_or_zero_to_f80(value: f64) -> Option<[u8; 10]> {
    let bits = value.to_bits();
    let sign = bits >> 63;
    let exponent = (bits >> 52) & 0x7ff;
    let fraction = bits & 0x000f_ffff_ffff_ffff;

    if exponent == 0 {
        if fraction != 0 {
            return None;
        }
        let mut bytes = [0u8; 10];
        bytes[9] = (sign as u8) << 7;
        return Some(bytes);
    }
    if exponent == 0x7ff {
        return None;
    }

    let f80_exponent = (exponent as u16) + (16_383 - 1023);
    let significand = 0x8000_0000_0000_0000u64 | (fraction << 11);
    let sign_exponent = f80_exponent | ((sign as u16) << 15);
    let mut bytes = [0u8; 10];
    bytes[..8].copy_from_slice(&significand.to_le_bytes());
    bytes[8..].copy_from_slice(&sign_exponent.to_le_bytes());
    Some(bytes)
}

/// Convert an i32 to its exact canonical Intel F80 representation.
#[inline(always)]
pub(crate) fn i32_to_f80(value: i32) -> [u8; 10] {
    if value == 0 {
        return [0; 10];
    }

    let magnitude = value.unsigned_abs();
    let leading_bit = magnitude.ilog2();
    let significand = u64::from(magnitude) << (63 - leading_bit);
    let sign_exponent = (16_383 + leading_bit as u16) | (u16::from(value < 0) << 15);
    let mut bytes = [0u8; 10];
    bytes[..8].copy_from_slice(&significand.to_le_bytes());
    bytes[8..].copy_from_slice(&sign_exponent.to_le_bytes());
    bytes
}

/// Convert a canonical finite F80 value to i32 using the x87 control-word
/// rounding mode. Returns `None` for encodings that need the backend's full
/// exceptional-value handling.
///
/// The `u16` is the FIST status bits: `IE` (0x01) on overflow with the
/// integer-indefinite result stored, `PE` (0x20) when rounding discarded a
/// non-zero fraction, `C1` (0x200) when the store rounded up — the same bits
/// a host `FISTP` reports under `F80::FIST_STATUS_MASK`.
#[inline(always)]
pub(crate) fn canonical_f80_to_i32(bytes: &[u8; 10], cw: u16) -> Option<(i32, u16)> {
    let sign_exponent = u16::from_le_bytes([bytes[8], bytes[9]]);
    let negative = sign_exponent & 0x8000 != 0;
    let exponent = sign_exponent & 0x7fff;
    let significand = u64::from_le_bytes(bytes[..8].try_into().expect("fixed F80 width"));

    if exponent == 0 {
        return (significand == 0).then_some((0, 0));
    }
    if exponent == 0x7fff || significand & 0x8000_0000_0000_0000 == 0 {
        return None;
    }

    let unbiased_exponent = i32::from(exponent) - 16_383;
    if unbiased_exponent > 31 {
        return Some((i32::MIN, 0x0001)); // IE: x87 integer indefinite
    }

    let (integer, remainder, half) = if unbiased_exponent >= 0 {
        let shift = (63 - unbiased_exponent) as u32;
        let integer = significand >> shift;
        let remainder_mask = (1u64 << shift) - 1;
        (integer, significand & remainder_mask, 1u64 << (shift - 1))
    } else if unbiased_exponent == -1 {
        (0, significand, 0x8000_0000_0000_0000)
    } else {
        // The magnitude is below 0.5. Keep a non-zero remainder so directed
        // rounding can still produce +/-1.
        (0, significand, u64::MAX)
    };

    let increment = match (cw >> 10) & 3 {
        0 => remainder > half || (remainder == half && integer & 1 != 0),
        1 => negative && remainder != 0,
        2 => !negative && remainder != 0,
        3 => false,
        _ => unreachable!(),
    };
    let rounded = integer + u64::from(increment);
    let limit = if negative { 0x8000_0000 } else { 0x7fff_ffff };
    if rounded > limit {
        return Some((i32::MIN, 0x0001)); // IE: x87 integer indefinite
    }

    let result = if negative {
        (-(rounded as i64)) as i32
    } else {
        rounded as i32
    };
    let mut sw = 0u16;
    if remainder != 0 {
        sw |= 0x0020; // PE: inexact
                      // C1: set iff the store rounded the MAGNITUDE up, i.e. the
                      // significand was incremented. This is sign-SYMMETRIC and is
                      // not the value-based reading ("stored > source") the first
                      // fix used (MDD-BUG-FLUX-00127). Probed on host silicon
                      // (`fld tbyte` / `fistp` / `fnstsw`, round-nearest):
                      //
                      //   2.7 -> 3   C1=1     -2.7 -> -3  C1=1
                      //   2.2 -> 2   C1=0     -2.2 -> -2  C1=0
                      //   0.25 -> 0  C1=0    -0.25 -> 0   C1=0
                      //   1.5 -> 2   C1=1     -1.5 -> -2  C1=1
                      //   2.5 -> 2   C1=0     -2.5 -> -2  C1=0
                      //
                      // `-2.7 -> -3` is the discriminating case: the stored value
                      // DECREASED while the magnitude rose, and silicon sets C1.
                      // A value-based test inverts every negative operand, which
                      // is what reopened this record.
        if increment {
            sw |= 0x0200;
        }
    }
    Some((result, sw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_binary32_mapping_is_a_bit_exact_round_trip() {
        for bits in [
            0x0080_0000,
            0x3f80_0000,
            0x4049_0fdb,
            0x7f7f_ffff,
            0x8080_0000,
            0xbf80_0000,
            0xff7f_ffff,
        ] {
            let value = f32::from_bits(bits);
            let f80 = f32_normal_or_zero_to_f80(value).expect("normal value");
            assert_eq!(f80_to_exact_normal_f32(&f80).unwrap().to_bits(), bits);
        }
    }

    #[test]
    fn exact_binary32_recovery_rejects_rounding_and_range_cases() {
        assert!(f32_normal_or_zero_to_f80(f32::from_bits(1)).is_none());
        assert!(f32_normal_or_zero_to_f80(f32::INFINITY).is_none());
        assert!(f32_normal_or_zero_to_f80(f32::NAN).is_none());

        let mut inexact = f32_normal_or_zero_to_f80(1.0).unwrap();
        inexact[0] = 1;
        assert!(f80_to_exact_normal_f32(&inexact).is_none());

        let too_wide = [0, 0, 0, 0, 0, 0, 0, 0x80, 0x00, 0x3f];
        assert!(f80_to_exact_normal_f32(&too_wide).is_none());
    }

    #[test]
    fn exact_binary32_recovery_preserves_signed_zero() {
        assert_eq!(
            f80_to_exact_normal_or_zero_f32(&[0; 10]).unwrap().to_bits(),
            0
        );
        let mut negative_zero = [0; 10];
        negative_zero[9] = 0x80;
        assert_eq!(
            f80_to_exact_normal_or_zero_f32(&negative_zero)
                .unwrap()
                .to_bits(),
            0x8000_0000
        );
    }

    #[test]
    fn normal_binary64_mapping_is_a_bit_exact_round_trip() {
        for bits in [
            0x0010_0000_0000_0000u64, // f64::MIN_POSITIVE
            0x3ff0_0000_0000_0000,    // 1.0
            0x4009_21fb_5444_2d18,    // pi
            0x7fef_ffff_ffff_ffff,    // f64::MAX
            0x8010_0000_0000_0000,
            0xbff0_0000_0000_0000,
            0xffef_ffff_ffff_ffff,
        ] {
            let value = f64::from_bits(bits);
            let f80 = f64_normal_or_zero_to_f80(value).expect("normal value");
            assert_eq!(
                f80_to_exact_normal_or_zero_f64(&f80).unwrap().to_bits(),
                bits
            );
        }
    }

    #[test]
    fn exact_binary64_recovery_rejects_rounding_range_and_zero_cases() {
        assert!(f64_normal_or_zero_to_f80(f64::from_bits(1)).is_none()); // subnormal
        assert!(f64_normal_or_zero_to_f80(f64::INFINITY).is_none());
        assert!(f64_normal_or_zero_to_f80(f64::NAN).is_none());

        // Signed zeros round-trip.
        for (value, expected_bits) in [(0.0f64, 0u64), (-0.0f64, 0x8000_0000_0000_0000)] {
            let f80 = f64_normal_or_zero_to_f80(value).unwrap();
            assert_eq!(
                f80_to_exact_normal_or_zero_f64(&f80).unwrap().to_bits(),
                expected_bits
            );
        }

        // A low fraction bit set in the F80 is not exactly binary64.
        let mut inexact = f64_normal_or_zero_to_f80(1.0).unwrap();
        inexact[0] = 1;
        assert!(f80_to_exact_normal_or_zero_f64(&inexact).is_none());

        // Explicit-integer-bit-clear (unnormal) forms are rejected.
        let unnormal = [0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0x3f];
        assert!(f80_to_exact_normal_or_zero_f64(&unnormal).is_none());

        // Exponents outside binary64's normal range are rejected.
        let mut huge = [0u8; 10];
        huge[7] = 0x80;
        huge[8..].copy_from_slice(&(16_383u16 + 1024).to_le_bytes());
        assert!(f80_to_exact_normal_or_zero_f64(&huge).is_none());
    }

    #[test]
    fn i32_mapping_round_trips_boundaries() {
        for value in [i32::MIN, -16_777_217, -1, 0, 1, 16_777_217, i32::MAX] {
            let bytes = i32_to_f80(value);
            assert_eq!(canonical_f80_to_i32(&bytes, 0x037f), Some((value, 0)));
        }
    }

    #[test]
    fn i32_rounding_obeys_all_control_word_modes() {
        let positive_half = f32_normal_or_zero_to_f80(2.5).unwrap();
        let negative_half = f32_normal_or_zero_to_f80(-2.5).unwrap();
        // Every 2.5 rounding discards the fraction, so PE (0x20) rides along,
        // and C1 (0x200) marks the cases where the result exceeds the source:
        // 2.5→3 going up, and -2.5→-2 whenever the magnitude shrinks.
        assert_eq!(
            canonical_f80_to_i32(&positive_half, 0x0000),
            Some((2, 0x20))
        );
        assert_eq!(
            canonical_f80_to_i32(&positive_half, 0x0400),
            Some((2, 0x20))
        );
        assert_eq!(
            canonical_f80_to_i32(&positive_half, 0x0800),
            Some((3, 0x220))
        );
        assert_eq!(
            canonical_f80_to_i32(&positive_half, 0x0c00),
            Some((2, 0x20))
        );
        // Negative expectations measured on host silicon, not reasoned:
        //   RN -2.5 -> -2 sw=0x0020    RD -2.5 -> -3 sw=0x0220
        //   RU -2.5 -> -2 sw=0x0020    RZ -2.5 -> -2 sw=0x0020
        // C1 tracks the MAGNITUDE, so only the -inf mode (which rounds -2.5
        // away from zero to -3) sets it. These were inverted while C1 was
        // computed value-based (MDD-BUG-FLUX-00127).
        assert_eq!(
            canonical_f80_to_i32(&negative_half, 0x0000),
            Some((-2, 0x20))
        );
        assert_eq!(
            canonical_f80_to_i32(&negative_half, 0x0400),
            Some((-3, 0x220))
        );
        assert_eq!(
            canonical_f80_to_i32(&negative_half, 0x0800),
            Some((-2, 0x20))
        );
        assert_eq!(
            canonical_f80_to_i32(&negative_half, 0x0c00),
            Some((-2, 0x20))
        );
    }

    #[test]
    fn i32_conversion_defers_noncanonical_encodings() {
        let mut denormal = [0u8; 10];
        denormal[0] = 1;
        assert_eq!(canonical_f80_to_i32(&denormal, 0x037f), None);

        let mut pseudo_denormal = [0u8; 10];
        pseudo_denormal[7] = 0x80;
        assert_eq!(canonical_f80_to_i32(&pseudo_denormal, 0x037f), None);

        let mut unnormal = [0u8; 10];
        unnormal[8..].copy_from_slice(&0x3fffu16.to_le_bytes());
        assert_eq!(canonical_f80_to_i32(&unnormal, 0x037f), None);

        let mut infinity = [0u8; 10];
        infinity[7] = 0x80;
        infinity[8..].copy_from_slice(&0x7fffu16.to_le_bytes());
        assert_eq!(canonical_f80_to_i32(&infinity, 0x037f), None);
    }
}
