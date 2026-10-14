//! F80 extended-precision type and x87 operation wrappers — software backend.
//!
//! This is the non-x86_64 sibling of `x87_ops.rs`. It exposes the **exact same**
//! public surface (`F80`, its constants/methods, the free `f80_*` functions, and
//! the `Debug`/`PartialEq`/`Default` impls) so that the consumer `fpu.rs`
//! compiles unchanged regardless of host architecture.
//!
//! Where `x87_ops.rs` reaches the host x87 unit through inline `asm!`, this
//! module computes bit-exact 80-bit results in pure Rust:
//! - arithmetic / conversions / compares / rounding go through `rustc_apfloat`'s
//!   `X87DoubleExtended` (BITS=80, EXP_BITS=15, PRECISION=64, explicit integer
//!   bit — its in-memory encoding is byte-identical to Intel 80-bit / `F80`).
//! - `FSQRT` borrows quad-precision Newton-Raphson from `ieee-apsqrt`.
//! - the transcendentals (`FSIN`/`FCOS`/`FPTAN`/`FPATAN`/`FYL2X`/`FYL2XP1`/
//!   `F2XM1`/`FSINCOS`) are computed in `f64` and are **approximate, not
//!   bit-exact** (matching the design note: these do not need to match the host
//!   x87 transcendental microcode bit-for-bit).
//!
//! Selected by `lib.rs` on non-x86_64 hosts and included again under a distinct
//! module name in x86_64 test builds so its portable arithmetic oracle always runs.

use core::cmp::Ordering;

use ieee_apsqrt::sqrt_fast;
use rustc_apfloat::ieee::{Double, Quad, Single, X87DoubleExtended};
use rustc_apfloat::{Float, FloatConvert, Round, Status};

// ============================================================================
// F80 type
// ============================================================================

/// Intel 80-bit extended precision floating point value.
///
/// Stored in little-endian Intel format:
/// - bytes 0-7: 64-bit significand (with explicit integer bit)
/// - bytes 8-9: 15-bit exponent + 1-bit sign
#[derive(Clone, Copy, Eq, serde::Serialize, serde::Deserialize)]
#[allow(clippy::unsafe_derive_deserialize)]
#[repr(C, align(16))]
pub struct F80(pub [u8; 10]);

// ----------------------------------------------------------------------------
// apfloat bridge helpers (private)
// ----------------------------------------------------------------------------

/// Reinterpret the 10 `F80` bytes as an `X87DoubleExtended`. The 80-bit Intel
/// encoding maps directly onto apfloat's X87 `from_bits` input (low 80 bits of a
/// `u128`, little-endian).
#[inline]
fn to_x87(f: &F80) -> X87DoubleExtended {
    let mut bits: u128 = 0;
    for (i, &b) in f.0.iter().enumerate() {
        bits |= (b as u128) << (8 * i);
    }
    X87DoubleExtended::from_bits(bits)
}

/// Serialize an `X87DoubleExtended` back into the 10 `F80` bytes.
#[inline]
fn from_x87(x: X87DoubleExtended) -> F80 {
    let bits = x.to_bits();
    let mut out = [0u8; 10];
    for (i, b) in out.iter_mut().enumerate() {
        *b = ((bits >> (8 * i)) & 0xFF) as u8;
    }
    F80(out)
}

/// Decode the x87 control-word rounding-control field (bits 10-11) into an
/// apfloat rounding mode.
#[inline]
fn round_from_cw(cw: u16) -> Round {
    match (cw >> 10) & 0b11 {
        0b00 => Round::NearestTiesToEven,
        0b01 => Round::TowardNegative,
        0b10 => Round::TowardPositive,
        _ => Round::TowardZero,
    }
}

/// Map an apfloat operation `Status` onto x87 status-word exception bits.
///
/// apfloat only models INVALID_OP/DIV_BY_ZERO/OVERFLOW/UNDERFLOW/INEXACT; the
/// denormal-operand flag (DE) has no apfloat equivalent, so it is synthesized by
/// the caller from the operands and passed in.
///
/// Bit layout: IE bit0, DE bit1, ZE bit2, OE bit3, UE bit4, PE bit5.
/// C1 (bit 9) is left clear here.
#[inline]
fn status_to_sw(status: Status, denormal_operand: bool) -> u16 {
    let mut sw = 0u16;
    if status.contains(Status::INVALID_OP) {
        sw |= 0x01; // IE
    }
    if denormal_operand {
        sw |= 0x02; // DE
    }
    if status.contains(Status::DIV_BY_ZERO) {
        sw |= 0x04; // ZE
    }
    if status.contains(Status::OVERFLOW) {
        sw |= 0x08; // OE
    }
    if status.contains(Status::UNDERFLOW) {
        sw |= 0x10; // UE
    }
    if status.contains(Status::INEXACT) {
        sw |= 0x20; // PE
    }
    sw
}

impl F80 {
    // Compile-time constants (exact hardware values from Intel docs)

    /// Positive zero
    pub const ZERO: F80 = F80([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

    /// Negative zero (sign bit is bit 7 of byte 9)
    pub const NEG_ZERO: F80 = F80([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80]);

    /// Floating-point indefinite (QNaN) — Intel SDM Vol 1 §4.8.3.7.
    /// Written on masked invalid-operation exceptions (e.g., FLD of empty
    /// ST(i), FST of empty ST(0), FSTP/FIST on empty stack). Encoded as
    /// sign=1, exponent=0x7FFF, mantissa bits 63 and 62 set.
    pub const INDEFINITE: F80 = F80([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0xFF, 0xFF]);

    /// 1.0 (exponent 0x3FFF, mantissa 0x8000000000000000)
    pub const ONE: F80 = F80([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0xFF, 0x3F]);

    /// Pi
    pub const PI: F80 = F80([0x35, 0xC2, 0x68, 0x21, 0xA2, 0xDA, 0x0F, 0xC9, 0x00, 0x40]);

    /// log2(e)
    pub const LOG2_E: F80 = F80([0xBC, 0xF0, 0x17, 0x5C, 0x29, 0x3B, 0xAA, 0xB8, 0xFF, 0x3F]);

    /// log2(10)
    pub const LOG2_10: F80 = F80([0xFE, 0x8A, 0x1B, 0xCD, 0x4B, 0x78, 0x9A, 0xD4, 0x00, 0x40]);

    /// log10(2)
    pub const LOG10_2: F80 = F80([0x99, 0xF7, 0xCF, 0xFB, 0x84, 0x9A, 0x20, 0x9A, 0xFD, 0x3F]);

    /// ln(2)
    pub const LN_2: F80 = F80([0xAC, 0x79, 0xCF, 0xD1, 0xF7, 0x17, 0x72, 0xB1, 0xFE, 0x3F]);

    // ---- Conversions ----

    /// Convert to f64 (round-to-nearest), mirroring `FLD TBYTE; FSTP QWORD`.
    pub fn to_f64(self) -> f64 {
        self.to_f64_round(Round::NearestTiesToEven)
    }

    /// Convert from f64, mirroring `FLD QWORD; FSTP TBYTE` (widening, exact).
    pub fn from_f64(val: f64) -> F80 {
        if let Some(bytes) = crate::x87_common::f64_normal_or_zero_to_f80(val) {
            return F80(bytes);
        }
        Self::from_f64_apfloat(val)
    }

    /// The apfloat widening `from_f64` falls back to for subnormal, infinite
    /// and NaN inputs; kept separate so tests can pin the bit path to it.
    fn from_f64_apfloat(val: f64) -> F80 {
        let d = Double::from_bits(u128::from(val.to_bits()));
        let mut loses = false;
        from_x87(d.convert_r(Round::NearestTiesToEven, &mut loses).value)
    }

    /// Convert from f32, mirroring `FLD DWORD; FSTP TBYTE` (widening, exact).
    pub fn from_f32(val: f32) -> F80 {
        if let Some(bytes) = crate::x87_common::f32_normal_or_zero_to_f80(val) {
            return F80(bytes);
        }
        let s = Single::from_bits(u128::from(val.to_bits()));
        let mut loses = false;
        from_x87(s.convert_r(Round::NearestTiesToEven, &mut loses).value)
    }

    /// Convert to f32 (round-to-nearest), mirroring `FLD TBYTE; FSTP DWORD`.
    pub fn to_f32(self) -> f32 {
        self.to_f32_round(Round::NearestTiesToEven)
    }

    /// Convert to f64 with explicit control word for rounding mode.
    pub fn to_f64_with_cw(self, cw: u16) -> f64 {
        self.to_f64_round(round_from_cw(cw))
    }

    /// Convert to f32 with explicit control word for rounding mode.
    pub fn to_f32_with_cw(self, cw: u16) -> f32 {
        if let Some(value) = crate::x87_common::f80_to_exact_normal_or_zero_f32(&self.0) {
            return value;
        }
        self.to_f32_round(round_from_cw(cw))
    }

    /// The status bits an `FST`/`FSTP` to memory can report: IE, OE, UE, PE
    /// and C1 (MDD-BUG-TEM-00168). A store raises no #D or #Z.
    pub const FST_STATUS_MASK: u16 = 0x0239;

    /// `FST m32`: the narrowed value and the status the store reports.
    pub fn to_f32_status_with_cw(self, cw: u16) -> (f32, u16) {
        if self.is_unsupported_encoding() {
            // An unsupported encoding is an invalid operand: the store writes
            // the binary32 real indefinite (negative quiet NaN) and raises IE.
            return (f32::from_bits(0xFFC0_0000), 0x0001);
        }
        let source = to_x87(&self);
        let mut loses = false;
        let mut narrowed = source.convert_r(round_from_cw(cw), &mut loses);
        if overflows_unbounded(source, cw, 0b00 << 8, Single::largest()) {
            narrowed.status |= Status::OVERFLOW | Status::INEXACT;
        }
        let single: Single = narrowed.value;
        let mut widening_loses = false;
        let back: X87DoubleExtended = single
            .convert_r(Round::NearestTiesToEven, &mut widening_loses)
            .value;
        let sw = narrowing_store_status(narrowed.status, source, back);
        (f32::from_bits(single.to_bits() as u32), sw)
    }

    /// `FST m64`: the narrowed value and the status the store reports.
    pub fn to_f64_status_with_cw(self, cw: u16) -> (f64, u16) {
        if self.is_unsupported_encoding() {
            // The binary64 real indefinite, as in `to_f32_status_with_cw`.
            return (f64::from_bits(0xFFF8_0000_0000_0000), 0x0001);
        }
        let source = to_x87(&self);
        let mut loses = false;
        let mut narrowed = source.convert_r(round_from_cw(cw), &mut loses);
        if overflows_unbounded(source, cw, 0b10 << 8, Double::largest()) {
            narrowed.status |= Status::OVERFLOW | Status::INEXACT;
        }
        let double: Double = narrowed.value;
        let mut widening_loses = false;
        let back: X87DoubleExtended = double
            .convert_r(Round::NearestTiesToEven, &mut widening_loses)
            .value;
        let sw = narrowing_store_status(narrowed.status, source, back);
        (f64::from_bits(double.to_bits() as u64), sw)
    }

    // Takes `self` by value (F80 is Copy) per clippy's wrong_self_convention for
    // `to_*` methods; the public to_f64/to_f32 keep &self for x87_ops.rs parity.
    #[inline]
    fn to_f64_round(self, round: Round) -> f64 {
        let mut loses = false;
        let d: Double = to_x87(&self).convert_r(round, &mut loses).value;
        f64::from_bits(d.to_bits() as u64)
    }

    #[inline]
    fn to_f32_round(self, round: Round) -> f32 {
        let mut loses = false;
        let s: Single = to_x87(&self).convert_r(round, &mut loses).value;
        f32::from_bits(s.to_bits() as u32)
    }

    // ---- Integer conversions (with control word rounding) ----

    /// The x87 status bits `FIST`/`FISTP` may report on this conversion:
    /// `IE` (bit 0), `PE` (bit 5), and the `C1` condition code (bit 9, set
    /// when the store rounded up). Mirror of
    /// `x87_ops::F80::FIST_STATUS_MASK`; see the rationale there
    pub const FIST_STATUS_MASK: u16 = 0x0221;

    /// `C1` for an inexact `FIST` store: set iff the store rounded the
    /// MAGNITUDE up. Exact stores and the IE (indefinite) route never reach
    /// this and leave C1 cleared.
    ///
    /// Sign-SYMMETRIC, and deliberately not the value-based rule ("stored >
    /// source") the first fix used — that inverts every negative operand and
    /// is what reopened MDD-BUG-FLUX-00127. Probed on host silicon
    /// (`fld tbyte` / `fistp` / `fnstsw`, round-nearest):
    ///
    /// | source | stored | C1 | | source | stored | C1 |
    /// |---|---|---|---|---|---|---|
    /// | 2.7 | 3 | 1 | | -2.7 | -3 | **1** |
    /// | 2.2 | 2 | 0 | | -2.2 | -2 | **0** |
    /// | 0.25 | 0 | 0 | | -0.25 | 0 | 0 |
    /// | 1.5 | 2 | 1 | | -1.5 | -2 | 1 |
    /// | 2.5 | 2 | 0 | | -2.5 | -2 | 0 |
    ///
    /// `-2.7 -> -3` is the discriminating case: the stored VALUE decreased
    /// while the magnitude rose, and silicon sets C1.
    ///
    /// The i128 widening is exact (any i64 fits the 64-bit significand), so
    /// the magnitude comparison is exact too.
    fn fist_c1(&self, stored: i128) -> u16 {
        let stored_magnitude = X87DoubleExtended::from_i128(stored.unsigned_abs() as i128).value;
        // Clear the sign bit (bit 15 of the high-order word) to take |source|.
        let mut source_magnitude = *self;
        source_magnitude.0[9] &= 0x7F;
        if stored_magnitude > to_x87(&source_magnitude) {
            0x0200
        } else {
            0
        }
    }

    /// Convert to i16 with rounding per control word.
    /// Returns `(value, invalid)`; see [`F80::to_i16_status`] for the flags.
    pub fn to_i16(self, cw: u16) -> (i16, bool) {
        let (value, sw) = self.to_i16_status(cw);
        (value, (sw & 0x01) != 0)
    }

    /// Convert to i16, returning the exception bits rather than just `invalid`.
    pub fn to_i16_status(self, cw: u16) -> (i16, u16) {
        if self.is_nan() || self.is_infinite() {
            return (i16::MIN, 0x0001); // IE: x87 integer indefinite
        }
        let mut exact = true;
        let r = to_x87(&self).to_i128_r(16, round_from_cw(cw), &mut exact);
        if r.status.contains(Status::INVALID_OP) {
            return (i16::MIN, 0x0001);
        }
        let mut sw = 0u16;
        if r.status.contains(Status::INEXACT) {
            sw |= 0x0020; // PE
            sw |= self.fist_c1(r.value); // C1: rounded up
        }
        (r.value as i16, sw)
    }

    /// Convert to i32 with rounding per control word.
    /// Returns `(value, invalid)`; see [`F80::to_i32_status`] for the flags.
    pub fn to_i32(self, cw: u16) -> (i32, bool) {
        let (value, sw) = self.to_i32_status(cw);
        (value, (sw & 0x01) != 0)
    }

    /// Convert to i32, returning the exception bits rather than just `invalid`.
    pub fn to_i32_status(self, cw: u16) -> (i32, u16) {
        if let Some(result) = crate::x87_common::canonical_f80_to_i32(&self.0, cw) {
            return result;
        }
        if self.is_nan() || self.is_infinite() {
            return (i32::MIN, 0x0001); // IE: x87 integer indefinite
        }
        let mut exact = true;
        let r = to_x87(&self).to_i128_r(32, round_from_cw(cw), &mut exact);
        if r.status.contains(Status::INVALID_OP) {
            return (i32::MIN, 0x0001);
        }
        let mut sw = 0u16;
        if r.status.contains(Status::INEXACT) {
            sw |= 0x0020; // PE
            sw |= self.fist_c1(r.value); // C1: rounded up
        }
        (r.value as i32, sw)
    }

    /// Convert to i64 with rounding per control word.
    /// Returns `(value, invalid)`; see [`F80::to_i64_status`] for the flags.
    pub fn to_i64(self, cw: u16) -> (i64, bool) {
        let (value, sw) = self.to_i64_status(cw);
        (value, (sw & 0x01) != 0)
    }

    /// Convert to i64, returning the exception bits rather than just `invalid`.
    pub fn to_i64_status(self, cw: u16) -> (i64, u16) {
        if self.is_nan() || self.is_infinite() {
            return (i64::MIN, 0x0001); // IE: x87 integer indefinite
        }
        let mut exact = true;
        let r = to_x87(&self).to_i128_r(64, round_from_cw(cw), &mut exact);
        if r.status.contains(Status::INVALID_OP) {
            return (i64::MIN, 0x0001);
        }
        let mut sw = 0u16;
        if r.status.contains(Status::INEXACT) {
            sw |= 0x0020; // PE
            sw |= self.fist_c1(r.value); // C1: rounded up
        }
        (r.value as i64, sw)
    }

    // ---- Truncating integer conversions (FISTTP, SSE3) ----

    /// Convert to i16 with truncation (FISTTP). Returns (value, status_word).
    pub fn to_i16_truncate_status(self) -> (i16, u16) {
        if self.is_nan() || self.is_infinite() {
            return (i16::MIN, 0x0001); // IE: x87 integer indefinite
        }
        let mut exact = true;
        let r = to_x87(&self).to_i128_r(16, Round::TowardZero, &mut exact);
        if r.status.contains(Status::INVALID_OP) {
            return (i16::MIN, 0x0001);
        }
        let mut sw = 0u16;
        if r.status.contains(Status::INEXACT) {
            sw |= 0x0020; // PE
        }
        (r.value as i16, sw)
    }

    /// Convert to i16 with truncation (FISTTP). Returns (value, invalid).
    pub fn to_i16_truncate(self) -> (i16, bool) {
        let (result, sw) = self.to_i16_truncate_status();
        let invalid = (sw & 0x0001) != 0;
        (result, invalid)
    }

    /// Convert to i32 with truncation (FISTTP). Returns (value, status_word).
    pub fn to_i32_truncate_status(self) -> (i32, u16) {
        if self.is_nan() || self.is_infinite() {
            return (i32::MIN, 0x0001); // IE: x87 integer indefinite
        }
        let mut exact = true;
        let r = to_x87(&self).to_i128_r(32, Round::TowardZero, &mut exact);
        if r.status.contains(Status::INVALID_OP) {
            return (i32::MIN, 0x0001);
        }
        let mut sw = 0u16;
        if r.status.contains(Status::INEXACT) {
            sw |= 0x0020; // PE
        }
        (r.value as i32, sw)
    }

    /// Convert to i32 with truncation (FISTTP). Returns (value, invalid).
    pub fn to_i32_truncate(self) -> (i32, bool) {
        let (result, sw) = self.to_i32_truncate_status();
        let invalid = (sw & 0x0001) != 0;
        (result, invalid)
    }

    /// Convert to i64 with truncation (FISTTP). Returns (value, status_word).
    pub fn to_i64_truncate_status(self) -> (i64, u16) {
        if self.is_nan() || self.is_infinite() {
            return (i64::MIN, 0x0001); // IE: x87 integer indefinite
        }
        let mut exact = true;
        let r = to_x87(&self).to_i128_r(64, Round::TowardZero, &mut exact);
        if r.status.contains(Status::INVALID_OP) {
            return (i64::MIN, 0x0001);
        }
        let mut sw = 0u16;
        if r.status.contains(Status::INEXACT) {
            sw |= 0x0020; // PE
        }
        (r.value as i64, sw)
    }

    /// Convert to i64 with truncation (FISTTP). Returns (value, invalid).
    pub fn to_i64_truncate(self) -> (i64, bool) {
        let (result, sw) = self.to_i64_truncate_status();
        let invalid = (sw & 0x0001) != 0;
        (result, invalid)
    }

    // ---- Integer to F80 (FILD) ----

    /// Convert i16 to F80 via FILD (exact).
    pub fn from_i16(val: i16) -> F80 {
        from_x87(X87DoubleExtended::from_i128(i128::from(val)).value)
    }

    /// Convert i32 to F80 via FILD (exact).
    pub fn from_i32(val: i32) -> F80 {
        F80(crate::x87_common::i32_to_f80(val))
    }

    /// Convert i64 to F80 via FILD (exact).
    pub fn from_i64(val: i64) -> F80 {
        from_x87(X87DoubleExtended::from_i128(i128::from(val)).value)
    }

    // ---- BCD conversions ----

    /// Convert to BCD (FBSTP). Returns 10-byte packed BCD.
    ///
    /// Software reimplementation of `FBSTP`: round ST(0) to an integer using the
    /// control-word rounding mode, then store the magnitude as 18 packed-decimal
    /// digits (2 per byte, little-endian) with the sign in bit 7 of byte 9.
    /// Out-of-range / non-finite inputs produce the packed-BCD indefinite.
    pub fn to_bcd(self, cw: u16) -> [u8; 10] {
        // Packed-BCD indefinite (Intel SDM Vol 1 §4.8.3.7 / §8.2.2):
        // 0xFFFF_C000_0000_0000_0000, i.e. little-endian bytes 7=0xC0, 8=0xFF,
        // 9=0xFF — the same image as the 80-bit float INDEFINITE constant.
        const BCD_INDEFINITE: [u8; 10] =
            [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0xFF, 0xFF];
        if self.is_nan() || self.is_infinite() {
            return BCD_INDEFINITE;
        }
        let rounded = to_x87(&self).round_to_integral(round_from_cw(cw)).value;
        let mut exact = true;
        let i = rounded.to_i128_r(64, Round::TowardZero, &mut exact).value;
        let negative = i < 0;
        let mut mag = i.unsigned_abs();
        // 18 decimal digits maximum (10^18 - 1).
        if mag >= 1_000_000_000_000_000_000u128 {
            return BCD_INDEFINITE;
        }
        let mut result = [0u8; 10];
        for byte in result.iter_mut().take(9) {
            let lo = (mag % 10) as u8;
            mag /= 10;
            let hi = (mag % 10) as u8;
            mag /= 10;
            *byte = lo | (hi << 4);
        }
        result[9] = if negative { 0x80 } else { 0x00 };
        result
    }

    /// Convert from BCD (FBLD).
    ///
    /// Software reimplementation of `FBLD`: decode 18 packed-decimal digits plus
    /// the sign bit and load the resulting integer as an F80.
    pub fn from_bcd(bytes: &[u8; 10]) -> F80 {
        let mut mag: i128 = 0;
        let mut place: i128 = 1;
        for &b in bytes.iter().take(9) {
            let lo = i128::from(b & 0x0F);
            let hi = i128::from(b >> 4);
            mag += lo * place;
            place *= 10;
            mag += hi * place;
            place *= 10;
        }
        if (bytes[9] & 0x80) != 0 {
            mag = -mag;
        }
        from_x87(X87DoubleExtended::from_i128(mag).value)
    }

    // ---- Bit inspection ----

    /// Check if this value is NaN.
    /// NaN: exponent all 1s (0x7FFF), explicit integer bit set, and a
    /// non-zero fraction. Encodings with the integer bit clear are unsupported
    /// x87 pseudo-infinities/pseudo-NaNs, not NaNs.
    pub fn is_nan(&self) -> bool {
        let exp = self.exponent_bits();
        let mantissa = self.mantissa_bits();
        exp == 0x7FFF && mantissa & (1 << 63) != 0 && mantissa & 0x7FFF_FFFF_FFFF_FFFF != 0
    }

    /// Check if this value is infinity (+inf or -inf).
    /// Infinity: exponent all 1s (0x7FFF), mantissa == 0x8000000000000000.
    pub fn is_infinite(&self) -> bool {
        let exp = self.exponent_bits();
        let mantissa = self.mantissa_bits();
        exp == 0x7FFF && mantissa == 0x8000_0000_0000_0000
    }

    /// Check if this value is zero (+0 or -0).
    pub fn is_zero(&self) -> bool {
        let exp = self.exponent_bits();
        let mantissa = self.mantissa_bits();
        exp == 0 && mantissa == 0
    }

    /// Check if this value is a denormal (subnormal).
    /// Arithmetic treats every non-zero exponent-0 encoding as a denormal
    /// operand, including the accepted pseudo-denormal form with its explicit
    /// integer bit set. NaN/unsupported short-circuiting suppresses DE before
    /// this predicate is consulted.
    pub fn is_denormal(&self) -> bool {
        self.exponent_bits() == 0 && self.mantissa_bits() != 0
    }

    /// Check if the sign bit is set (negative).
    pub fn is_sign_negative(&self) -> bool {
        (self.0[9] & 0x80) != 0
    }

    /// The x87 tag this value earns when it is written into a register:
    /// `0` Valid, `1` Zero, `2` Special.
    ///
    /// `3` (Empty) is deliberately not reachable here — Empty is an occupancy
    /// fact, not a property of the bits, so it stays the caller's decision.
    ///
    /// Kept bit-for-bit identical to `x87_ops::F80::x87_tag`: the two types
    /// are the same architectural encoding behind a `cfg(target_arch)`
    /// switch, so a classification that differed between them would make the
    /// guest-visible tag word host-dependent.
    #[inline]
    pub fn x87_tag(&self) -> u16 {
        if self.is_zero() {
            1 // Zero
        } else if self.is_nan()
            || self.is_infinite()
            || self.is_denormal()
            || self.is_unsupported_encoding()
        {
            2 // Special
        } else {
            0 // Valid
        }
    }

    /// Direct access to the raw bytes.
    pub fn bytes(&self) -> &[u8; 10] {
        &self.0
    }

    // ---- Internal helpers ----

    /// Extract the 15-bit biased exponent.
    #[inline]
    fn exponent_bits(&self) -> u16 {
        u16::from_le_bytes([self.0[8], self.0[9]]) & 0x7FFF
    }

    /// Extract the 64-bit mantissa (significand with explicit integer bit).
    #[inline]
    fn mantissa_bits(&self) -> u64 {
        u64::from_le_bytes([
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5], self.0[6], self.0[7],
        ])
    }

    /// True for encodings on which a 387+ arithmetic instruction raises #IA:
    /// an unnormal finite value, pseudo-infinity, or pseudo-NaN. The exponent-0
    /// pseudo-denormal form is deliberately excluded because silicon accepts it.
    ///
    /// Public because the x87 comparison instructions in `fpu.rs` need it and
    /// must classify identically on every host; the mirror lives in
    /// `x87_ops::F80::is_unsupported_encoding`.
    #[inline]
    pub fn is_unsupported_encoding(&self) -> bool {
        self.exponent_bits() != 0 && self.mantissa_bits() & (1 << 63) == 0
    }

    /// True for a signaling NaN: a NaN whose most-significant fraction bit is
    /// clear. Quiet NaNs have it set. Pseudo-NaNs are not NaNs here — they are
    /// reported by [`F80::is_unsupported_encoding`].
    #[inline]
    pub fn is_signaling_nan(&self) -> bool {
        self.is_nan() && self.mantissa_bits() & (1 << 62) == 0
    }

    #[inline]
    fn quieted_nan(mut self) -> Self {
        debug_assert!(self.is_nan());
        let significand = self.mantissa_bits() | (1 << 62);
        self.0[..8].copy_from_slice(&significand.to_le_bytes());
        self
    }
}

// ---- Trait implementations ----

impl std::fmt::Debug for F80 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "F80({})", self.to_f64())
    }
}

impl PartialEq for F80 {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Default for F80 {
    fn default() -> Self {
        Self::ZERO
    }
}

// ============================================================================
// x87 arithmetic operations
// ============================================================================

/// Apply x87 precision control (control-word bits 8-9) to an arithmetic result:
/// round the significand to 24 bits (single, PC=00) or 53 bits (double, PC=10),
/// while keeping the full 80-bit exponent range. PC=11 (extended, the x87
/// default) and the reserved PC=01 leave the 64-bit result unchanged.
///
/// On x86_64 the host FPU does this in hardware from the loaded control word;
/// the soft path must replicate it. The significand is narrowed via a scale to
/// [1,2) (exact power-of-two `scalbn`, so the narrowing convert can never
/// overflow the smaller format's exponent), a round to Single/Double in the
/// control word's rounding mode, then a scale back. NaN/Inf/zero pass through
/// (ilogb is undefined for them). This double-rounds (the op already rounded to
/// 64-bit), which can differ from a single round-to-PC in rare halfway cases —
/// an accepted approximation, far better than ignoring PC entirely.
struct PcRounded {
    value: X87DoubleExtended,
    inexact: bool,
}

fn round_to_pc(value: X87DoubleExtended, cw: u16) -> PcRounded {
    let pc = (cw >> 8) & 0b11;
    if pc == 0b11 || pc == 0b01 {
        return PcRounded {
            value,
            inexact: false,
        }; // 64-bit extended (or reserved): no narrowing
    }
    if !value.is_finite() || value.is_zero() {
        return PcRounded {
            value,
            inexact: false,
        };
    }
    let round = round_from_cw(cw);
    let e = value.ilogb();
    let scaled = value.scalbn_r(-e, Round::NearestTiesToEven); // -> [1,2), exact
    let mut loses = false;
    let narrowed: X87DoubleExtended = if pc == 0b00 {
        let s: Single = scaled.convert_r(round, &mut loses).value;
        let mut widening_loses = false;
        let widened = s.convert_r(round, &mut widening_loses).value;
        debug_assert!(!widening_loses);
        widened
    } else {
        let d: Double = scaled.convert_r(round, &mut loses).value;
        let mut widening_loses = false;
        let widened = d.convert_r(round, &mut widening_loses).value;
        debug_assert!(!widening_loses);
        widened
    };
    PcRounded {
        value: narrowed.scalbn_r(e, Round::NearestTiesToEven),
        inexact: loses,
    }
}

/// Whether narrowing `source` overflows the target format in the IEEE sense:
/// its value rounded in the control word's mode as if the exponent were
/// unbounded exceeds the format's largest finite value. A directed rounding
/// that clamps the stored result to that largest value still overflows —
/// host silicon reports OE for it — which the conversion's own status does
/// not say. `pc_bits` selects the target precision for `round_to_pc`.
fn overflows_unbounded<T>(source: X87DoubleExtended, cw: u16, pc_bits: u16, largest: T) -> bool
where
    T: FloatConvert<X87DoubleExtended>,
{
    if !source.is_finite() {
        return false;
    }
    let unbounded = round_to_pc(source, (cw & !0x0300) | pc_bits).value;
    let mut loses = false;
    let largest: X87DoubleExtended = largest
        .convert_r(Round::NearestTiesToEven, &mut loses)
        .value;
    magnitude_bits(unbounded) > magnitude_bits(largest)
}

/// The x87 status word an `FST`/`FSTP` narrowing to memory reports, from the
/// conversion's IEEE status: IE for an SNaN source, OE (with PE) on overflow,
/// UE on a tiny inexact result, PE when the value was rounded, and C1 when
/// that rounding increased the magnitude — `back` is the stored value widened
/// back exactly, so comparing it with the source decides the direction.
fn narrowing_store_status(
    status: Status,
    source: X87DoubleExtended,
    back: X87DoubleExtended,
) -> u16 {
    let mut sw = 0u16;
    if status.contains(Status::INVALID_OP) {
        return 0x0001;
    }
    if status.contains(Status::OVERFLOW) {
        sw |= 0x0008;
    }
    if status.contains(Status::UNDERFLOW) {
        sw |= 0x0010;
    }
    if status.contains(Status::INEXACT) {
        sw |= 0x0020;
        if magnitude_bits(back) > magnitude_bits(source) {
            sw |= 0x0200;
        }
    }
    sw
}

fn magnitude_bits(value: X87DoubleExtended) -> u128 {
    value.to_bits() & ((1u128 << 79) - 1)
}

fn finish_arithmetic(
    value: X87DoubleExtended,
    status: Status,
    denormal_operand: bool,
    cw: u16,
    toward_zero_operation: impl FnOnce() -> X87DoubleExtended,
) -> (F80, u16) {
    let mut sw = status_to_sw(status, denormal_operand);
    if status.contains(Status::INVALID_OP) {
        return (F80::INDEFINITE, sw);
    }

    let operation_inexact = status.contains(Status::INEXACT);
    let pc = (cw >> 8) & 0b11;
    let narrowing = pc == 0b00 || pc == 0b10;

    // Single-rounding correction for narrowed precision (PC=24/53): the op
    // above already rounded to 64 bits, so feeding that into `round_to_pc`
    // would round twice — off by one ulp from real x87 (which rounds the
    // infinitely-precise result once to PC precision) whenever the 64-bit
    // rounding landed on a PC-precision boundary. Cure is round-to-odd
    // (Boldo–Melquiond): redo the op truncated toward zero, force the
    // significand's LSB to 1 so the discarded sticky bits stay visible, and
    // let the single `round_to_pc` below make the only value-determining
    // rounding. Valid because the 64→53/24-bit precision gap is ≥ 2 bits.
    // PC=64 keeps the direct result: no narrowing follows, so the op's own
    // rounding IS the single rounding.
    // The odd trick is restricted to safely-normal values: in the x87
    // denormal range the result's precision is set by denormalization, not
    // PC, and the old two-step behavior stays the accepted approximation
    // there (the host-float fast path never engages that low either).
    let use_odd_candidate = narrowing
        && operation_inexact
        && value.is_finite()
        && !value.is_zero()
        && !value.is_denormal();
    // The truncated re-op feeds the odd trick, and separately the nearest-mode
    // C1 comparison below; compute it once, and only when one of them needs it
    // (directed-rounding PC=64 keeps the old fully-lazy behavior).
    let need_tz = operation_inexact && (use_odd_candidate || (cw >> 10) & 0b11 == 0b00);
    let tz = if need_tz {
        Some(toward_zero_operation())
    } else {
        None
    };
    let use_odd = use_odd_candidate && tz.is_some_and(|t| !t.is_denormal());
    let pc_input = if use_odd {
        X87DoubleExtended::from_bits(tz.expect("use_odd implies need_tz").to_bits() | 1)
    } else {
        value
    };

    let rounded = round_to_pc(pc_input, cw);
    let inexact = operation_inexact || rounded.inexact;
    if inexact {
        sw |= 0x20;

        // x87 C1 says whether precision rounding increased the result's
        // magnitude. Directed rounding determines that from the final sign, and
        // truncation can never increase it. Only nearest-even needs a value to
        // compare with toward-zero; `pc_input` already carries the sticky
        // information (odd LSB), so truncating IT to PC precision is the exact
        // toward-zero result.
        let rounded_up = match (cw >> 10) & 0b11 {
            0b01 => rounded.value.is_negative(),  // toward -infinity
            0b10 => !rounded.value.is_negative(), // toward +infinity
            0b11 => false,                        // toward zero
            _ => {
                let toward_zero_value = if use_odd {
                    pc_input
                } else {
                    tz.unwrap_or(value)
                };
                let toward_zero_cw = (cw & !0x0c00) | 0x0c00;
                let toward_zero = round_to_pc(toward_zero_value, toward_zero_cw);
                magnitude_bits(rounded.value) > magnitude_bits(toward_zero.value)
            }
        };
        if rounded_up {
            sw |= 1 << 9;
        }
    }
    (from_x87(rounded.value), sw)
}

#[cfg(test)]
fn finish_arithmetic_eager(
    value: X87DoubleExtended,
    toward_zero_value: X87DoubleExtended,
    status: Status,
    denormal_operand: bool,
    cw: u16,
) -> (F80, u16) {
    let mut sw = status_to_sw(status, denormal_operand);
    if status.contains(Status::INVALID_OP) {
        return (F80::INDEFINITE, sw);
    }

    // Mirrors `finish_arithmetic`'s round-to-odd single-rounding scheme with
    // the toward-zero operation supplied eagerly.
    let operation_inexact = status.contains(Status::INEXACT);
    let pc = (cw >> 8) & 0b11;
    let narrowing = pc == 0b00 || pc == 0b10;
    let use_odd = narrowing
        && operation_inexact
        && value.is_finite()
        && !value.is_zero()
        && !value.is_denormal()
        && !toward_zero_value.is_denormal();
    let pc_input = if use_odd {
        X87DoubleExtended::from_bits(toward_zero_value.to_bits() | 1)
    } else {
        value
    };
    let rounded = round_to_pc(pc_input, cw);
    let toward_zero_cw = (cw & !0x0c00) | 0x0c00;
    let c1_base = if use_odd { pc_input } else { toward_zero_value };
    let toward_zero = round_to_pc(c1_base, toward_zero_cw);
    let inexact = operation_inexact || rounded.inexact;
    if inexact {
        sw |= 0x20;
        if magnitude_bits(rounded.value) > magnitude_bits(toward_zero.value) {
            sw |= 1 << 9;
        }
    }
    (from_x87(rounded.value), sw)
}

// ============================================================================
// Host-float fast path
// ============================================================================
//
// Hardware floating-point shortcut: when the guest's control word narrows
// precision to single (PC=00) or double (PC=10) and both operands are
// zero/normal 80-bit values that map onto the host's `f32`/`f64` with no
// rounding, arithmetic can be done directly in the host type instead of
// through `rustc_apfloat`. Extended precision (PC=11, the x87 default) and
// the reserved PC=01 always fall back, as does every directed rounding mode
// (RC=01/10/11): Rust's host float operators only round to nearest-even, and
// this module intentionally stays arch-neutral Rust (no per-target rounding
// control), so anything else takes the pure `rustc_apfloat` path below, which
// remains authoritative. `PE`/`C1` are derived from an error-free transform of
// the host operation (TwoSum for add/sub, an FMA residual for mul/div, and a
// back-substitution FMA residual for div) rather than reproduced by a second
// pass — see `finish_arithmetic` for what these bits mean. Every fast
// function returns `None` on any doubt so its caller falls through to the
// pure path unconditionally; `mod tests` below asserts the two arms agree
// bit-for-bit across a large differential corpus.

// The error-free-transform cores and their helpers live in the arch-neutral
// `x87_fast_core` module so `fpu.rs`'s NumericShadow arithmetic shares the
// exact same numerics on every host (single numerics source, HLD 2026.07.29
// D4). This module wraps them behind F80 operand decode / result encode.
use crate::x87_fast_core::{
    fast_add_f32_core, fast_add_f64_core, fast_div_f32_core, fast_div_f64_core, fast_mul_f32_core,
    fast_mul_f64_core, record_fast_path_hit,
};

/// Recover the exact `f64` value of a canonical F80 zero/normal, when
/// binary64 represents it with no rounding. NaN/Inf, denormals, and
/// unnormal/pseudo-denormal forms (explicit integer bit clear with a nonzero
/// exponent) return `None`, as does any magnitude outside binary64's normal
/// exponent range — all of these are the caller's cue to fall back. Mirrors
/// `x87_common::f80_to_exact_normal_or_zero_f32`, widened to 52 mantissa bits.
#[inline]
fn fast_f80_exact_f64(bytes: &[u8; 10]) -> Option<f64> {
    crate::x87_common::f80_to_exact_normal_or_zero_f64(bytes)
}

/// Widen a finite `f64` that is zero or in binary64's normal range to its
/// exact Intel F80 encoding. Widening f64→f80 never loses information, so
/// this is exact for any value the fast-path arithmetic below can produce —
/// each caller post-checks its host result is zero-or-normal before calling
/// this; it is not a general-purpose f64→F80 constructor.
#[inline]
fn fast_f64_to_f80(v: f64) -> F80 {
    debug_assert!(
        v == 0.0 || v.is_normal(),
        "fast_f64_to_f80 called with a non-normal, nonzero value"
    );
    match crate::x87_common::f64_normal_or_zero_to_f80(v) {
        Some(bytes) => F80(bytes),
        // Callers guarantee zero-or-normal; preserve the pre-refactor release
        // behaviour (signed zero) if that contract is ever violated.
        None => {
            let mut bytes = [0u8; 10];
            bytes[9] = ((v.to_bits() >> 63) as u8) << 7;
            F80(bytes)
        }
    }
}

/// Host-float fast path for FADD: decode exact operands per the control
/// word's PC, run the shared core, encode the result.
fn fast_add(a: &F80, b: &F80, cw: u16) -> Option<(F80, u16)> {
    let rc = (cw >> 10) & 0b11;
    match (cw >> 8) & 0b11 {
        0b00 => {
            let fa = crate::x87_common::f80_to_exact_normal_or_zero_f32(a.bytes())?;
            let fb = crate::x87_common::f80_to_exact_normal_or_zero_f32(b.bytes())?;
            let (s_adj, sw) = fast_add_f32_core(fa, fb, rc)?;
            let bytes = crate::x87_common::f32_normal_or_zero_to_f80(s_adj)?;
            Some((F80(bytes), sw))
        }
        0b10 => {
            let fa = fast_f80_exact_f64(a.bytes())?;
            let fb = fast_f80_exact_f64(b.bytes())?;
            let (s_adj, sw) = fast_add_f64_core(fa, fb, rc)?;
            Some((fast_f64_to_f80(s_adj), sw))
        }
        _ => None, // PC=01 (reserved) / PC=11 (extended): no host-width fast path
    }
}

/// Host-float fast path for FSUB (`a - b` via the add core on `a, -b`).
fn fast_sub(a: &F80, b: &F80, cw: u16) -> Option<(F80, u16)> {
    let rc = (cw >> 10) & 0b11;
    match (cw >> 8) & 0b11 {
        0b00 => {
            let fa = crate::x87_common::f80_to_exact_normal_or_zero_f32(a.bytes())?;
            let fb = crate::x87_common::f80_to_exact_normal_or_zero_f32(b.bytes())?;
            let (s_adj, sw) = fast_add_f32_core(fa, -fb, rc)?;
            let bytes = crate::x87_common::f32_normal_or_zero_to_f80(s_adj)?;
            Some((F80(bytes), sw))
        }
        0b10 => {
            let fa = fast_f80_exact_f64(a.bytes())?;
            let fb = fast_f80_exact_f64(b.bytes())?;
            let (s_adj, sw) = fast_add_f64_core(fa, -fb, rc)?;
            Some((fast_f64_to_f80(s_adj), sw))
        }
        _ => None,
    }
}

/// Host-float fast path for FMUL. `p == 0.0` from two nonzero operands is
/// treated as a possible fast-path underflow (the host format's exponent
/// range is far narrower than F80's) and falls back rather than risk a false
/// zero.
fn fast_mul(a: &F80, b: &F80, cw: u16) -> Option<(F80, u16)> {
    let rc = (cw >> 10) & 0b11;
    match (cw >> 8) & 0b11 {
        0b00 => {
            let fa = crate::x87_common::f80_to_exact_normal_or_zero_f32(a.bytes())?;
            let fb = crate::x87_common::f80_to_exact_normal_or_zero_f32(b.bytes())?;
            let (p_adj, sw) = fast_mul_f32_core(fa, fb, rc)?;
            let bytes = crate::x87_common::f32_normal_or_zero_to_f80(p_adj)?;
            Some((F80(bytes), sw))
        }
        0b10 => {
            let fa = fast_f80_exact_f64(a.bytes())?;
            let fb = fast_f80_exact_f64(b.bytes())?;
            let (p_adj, sw) = fast_mul_f64_core(fa, fb, rc)?;
            Some((fast_f64_to_f80(p_adj), sw))
        }
        _ => None,
    }
}

/// Host-float fast path for FDIV. Divide-by-zero always falls back (the
/// pure path owns #ZE and the infinite/indefinite result); `q == 0.0` from a
/// nonzero dividend falls back for the same false-underflow reason as
/// `fast_mul`.
fn fast_div(a: &F80, b: &F80, cw: u16) -> Option<(F80, u16)> {
    let rc = (cw >> 10) & 0b11;
    match (cw >> 8) & 0b11 {
        0b00 => {
            let fa = crate::x87_common::f80_to_exact_normal_or_zero_f32(a.bytes())?;
            let fb = crate::x87_common::f80_to_exact_normal_or_zero_f32(b.bytes())?;
            let (q_adj, sw) = fast_div_f32_core(fa, fb, rc)?;
            let bytes = crate::x87_common::f32_normal_or_zero_to_f80(q_adj)?;
            Some((F80(bytes), sw))
        }
        0b10 => {
            let fa = fast_f80_exact_f64(a.bytes())?;
            let fb = fast_f80_exact_f64(b.bytes())?;
            let (q_adj, sw) = fast_div_f64_core(fa, fb, rc)?;
            Some((fast_f64_to_f80(q_adj), sw))
        }
        _ => None,
    }
}

/// Resolve the x87 operand classes that `rustc_apfloat` cannot model.
/// Unsupported encodings take precedence over NaNs. Otherwise a NaN result is
/// the source with the larger significand, quieted, with IE set if either
/// source was signaling. A short-circuited NaN never raises DE for the other
/// operand.
#[inline]
fn binary_arithmetic_special_case(a: &F80, b: &F80) -> Option<(F80, u16)> {
    if a.is_unsupported_encoding() || b.is_unsupported_encoding() {
        return Some((F80::INDEFINITE, 0x01));
    }

    let a_nan = a.is_nan();
    let b_nan = b.is_nan();
    if !a_nan && !b_nan {
        return None;
    }

    let selected = match (a_nan, b_nan) {
        (true, true) if b.mantissa_bits() > a.mantissa_bits() => *b,
        (true, _) => *a,
        (_, true) => *b,
        (false, false) => unreachable!("at least one operand is a NaN"),
    };
    let sw = u16::from(a.is_signaling_nan() || b.is_signaling_nan());
    Some((selected.quieted_nan(), sw))
}

/// Unary counterpart to [`binary_arithmetic_special_case`].
#[inline]
fn unary_arithmetic_special_case_status(a: &F80) -> Option<(F80, u16)> {
    if a.is_unsupported_encoding() {
        Some((F80::INDEFINITE, 0x01))
    } else if a.is_nan() {
        // A signalling NaN is quieted and raises IE, exactly as in
        // `binary_arithmetic_special_case`; a quiet NaN propagates silently.
        Some((a.quieted_nan(), u16::from(a.is_signaling_nan())))
    } else {
        None
    }
}

fn unary_arithmetic_special_case(a: &F80) -> Option<F80> {
    if a.is_unsupported_encoding() {
        Some(F80::INDEFINITE)
    } else if a.is_nan() {
        Some(a.quieted_nan())
    } else {
        None
    }
}

/// `rustc_apfloat` follows IEEE's result-dependent overflow reporting: under a
/// directed mode an overflowing exact result can round to the largest finite
/// value and lose OVERFLOW. x87 still reports OE. A nearest-even replay is used
/// only for inexact directed results where apfloat omitted it; the directed
/// operation remains the source of the value and every other status bit.
#[inline]
fn restore_x87_overflow(
    status: Status,
    cw: u16,
    nearest_status: impl FnOnce() -> Status,
) -> Status {
    if (cw >> 10) & 0b11 != 0
        && status.contains(Status::INEXACT)
        && !status.contains(Status::OVERFLOW)
        && nearest_status().contains(Status::OVERFLOW)
    {
        status | Status::OVERFLOW
    } else {
        status
    }
}

/// F80 addition: a + b, using the given control word for rounding/precision.
pub fn f80_add(a: &F80, b: &F80, cw: u16) -> F80 {
    f80_add_status(a, b, cw).0
}

/// F80 addition: a + b, returning the modeled x87 status word.
pub fn f80_add_status(a: &F80, b: &F80, cw: u16) -> (F80, u16) {
    f80_add_status_impl(a, b, cw, true)
}

/// `f80_add_status`'s implementation, with the host-float fast path gated by
/// `allow_fast` so the differential tests can force the pure path and compare
/// both arms.
fn f80_add_status_impl(a: &F80, b: &F80, cw: u16, allow_fast: bool) -> (F80, u16) {
    if let Some(result) = binary_arithmetic_special_case(a, b) {
        return result;
    }
    if allow_fast {
        if let Some(result) = fast_add(a, b, cw) {
            return result;
        }
    }
    let fa = to_x87(a);
    let fb = to_x87(b);
    let r = fa.add_r(fb, round_from_cw(cw));
    let status = restore_x87_overflow(r.status, cw, || {
        fa.add_r(fb, Round::NearestTiesToEven).status
    });
    finish_arithmetic(
        r.value,
        status,
        a.is_denormal() || b.is_denormal(),
        cw,
        || fa.add_r(fb, Round::TowardZero).value,
    )
}

/// F80 subtraction: a - b, using the given control word.
pub fn f80_sub(a: &F80, b: &F80, cw: u16) -> F80 {
    let (result, _) = f80_sub_status(a, b, cw);
    result
}

/// F80 subtraction: a - b, returning the x87 status word for the op.
pub fn f80_sub_status(a: &F80, b: &F80, cw: u16) -> (F80, u16) {
    f80_sub_status_impl(a, b, cw, true)
}

/// `f80_sub_status`'s implementation; see `f80_add_status_impl`.
fn f80_sub_status_impl(a: &F80, b: &F80, cw: u16, allow_fast: bool) -> (F80, u16) {
    if let Some(result) = binary_arithmetic_special_case(a, b) {
        return result;
    }
    if allow_fast {
        if let Some(result) = fast_sub(a, b, cw) {
            return result;
        }
    }
    let fa = to_x87(a);
    let fb = to_x87(b);
    let r = fa.sub_r(fb, round_from_cw(cw));
    let status = restore_x87_overflow(r.status, cw, || {
        fa.sub_r(fb, Round::NearestTiesToEven).status
    });
    finish_arithmetic(
        r.value,
        status,
        a.is_denormal() || b.is_denormal(),
        cw,
        || fa.sub_r(fb, Round::TowardZero).value,
    )
}

/// F80 multiplication: a * b, using the given control word.
pub fn f80_mul(a: &F80, b: &F80, cw: u16) -> F80 {
    f80_mul_status(a, b, cw).0
}

/// F80 multiplication: a * b, returning the modeled x87 status word.
pub fn f80_mul_status(a: &F80, b: &F80, cw: u16) -> (F80, u16) {
    f80_mul_status_impl(a, b, cw, true)
}

/// `f80_mul_status`'s implementation; see `f80_add_status_impl`.
fn f80_mul_status_impl(a: &F80, b: &F80, cw: u16, allow_fast: bool) -> (F80, u16) {
    if let Some(result) = binary_arithmetic_special_case(a, b) {
        return result;
    }
    if allow_fast {
        if let Some(result) = fast_mul(a, b, cw) {
            return result;
        }
    }
    let fa = to_x87(a);
    let fb = to_x87(b);
    let r = fa.mul_r(fb, round_from_cw(cw));
    let status = restore_x87_overflow(r.status, cw, || {
        fa.mul_r(fb, Round::NearestTiesToEven).status
    });
    finish_arithmetic(
        r.value,
        status,
        a.is_denormal() || b.is_denormal(),
        cw,
        || fa.mul_r(fb, Round::TowardZero).value,
    )
}

/// F80 division: a / b, using the given control word.
pub fn f80_div(a: &F80, b: &F80, cw: u16) -> F80 {
    let (result, _) = f80_div_status(a, b, cw);
    result
}

/// F80 division: a / b, returning the x87 status word for the op.
pub fn f80_div_status(a: &F80, b: &F80, cw: u16) -> (F80, u16) {
    f80_div_status_impl(a, b, cw, true)
}

/// `f80_div_status`'s implementation; see `f80_add_status_impl`.
fn f80_div_status_impl(a: &F80, b: &F80, cw: u16, allow_fast: bool) -> (F80, u16) {
    if let Some(result) = binary_arithmetic_special_case(a, b) {
        return result;
    }
    if allow_fast {
        if let Some(result) = fast_div(a, b, cw) {
            return result;
        }
    }
    let fa = to_x87(a);
    let fb = to_x87(b);
    let r = fa.div_r(fb, round_from_cw(cw));
    let status = restore_x87_overflow(r.status, cw, || {
        fa.div_r(fb, Round::NearestTiesToEven).status
    });
    finish_arithmetic(
        r.value,
        status,
        b.is_denormal() || (a.is_denormal() && !b.is_zero()),
        cw,
        || fa.div_r(fb, Round::TowardZero).value,
    )
}

// ============================================================================
// Unary operations
// ============================================================================

/// Host-float fast path for FSQRT: exact (perfect-square) cases only. An
/// exact result needs no rounding decision, so it applies under every RC. It
/// does need a PC gate: FSQRT honours precision control like add/sub/mul/div
/// (SDM Vol 1 8.1.5.2; MDD-BUG-TEM-00170), and an exact binary64 root always
/// fits PC=53 and PC=64 but fits PC=24 only when its significand has at most
/// 24 bits — otherwise the pure path below rounds it. Any inexact case must
/// go through the quad-precision Newton-Raphson pure path — a single host
/// sqrt would double-round against its correctly-rounded result.
fn fast_sqrt(a: &F80, cw: u16) -> Option<F80> {
    let v = fast_f80_exact_f64(a.bytes())?;
    if v <= 0.0 {
        return None; // zero/negative: let the pure path own sign and #IE
    }
    if v < 1e-289 {
        // ~2^-960 floor: the s*s - v exactness residual is a multiple of
        // ~2^(exp(v)-104); below this it can flush to zero and claim a
        // false perfect square.
        return None;
    }
    let s = v.sqrt();
    if !s.is_normal() {
        return None; // defensive: unreachable given v's normal-range floor
    }
    let residual = s.mul_add(s, -v);
    if residual != 0.0 {
        return None; // inexact: not reproducible bit-exact from a host sqrt
    }
    if (cw >> 8) & 0b11 == 0b00 && s.to_bits() & ((1u64 << 29) - 1) != 0 {
        return None; // exact, but wider than PC=24: the pure path rounds it
    }
    record_fast_path_hit();
    Some(fast_f64_to_f80(s))
}

/// Round FSQRT's quad-precision root to the precision the control word asks
/// for, in its rounding mode, with ONE rounding. Returns the value and whether
/// that rounding lost bits.
///
/// `root_q` is the round-to-odd root (LSB forced to 1 when inexact), so the
/// 113-bit sticky information survives into this single rounding; the gap to
/// 64, 53 or 24 bits is far more than the two bits round-to-odd needs. PC=24
/// and PC=53 narrow the SIGNIFICAND only, keeping the 80-bit exponent range:
/// the root is scaled to [1,2) by an exact power of two, rounded as a
/// Single/Double, widened back (exact) and rescaled. A root of an x87 value is
/// never denormal or overflowing at x87 scale, so the rescale is exact too.
/// This is what `round_to_pc` does for the arithmetic ops, applied to the
/// unrounded quad root rather than to a 64-bit result — FSQRT honours PC like
/// FADD/FSUB/FMUL/FDIV (SDM Vol 1 8.1.5.2; MDD-BUG-TEM-00170).
fn round_sqrt_root(root_q: Quad, cw: u16, round: Round) -> (X87DoubleExtended, bool) {
    let mut loses = false;
    let pc = (cw >> 8) & 0b11;
    if pc == 0b11 || pc == 0b01 || root_q.is_zero() || !root_q.is_finite() {
        let value = root_q.convert_r(round, &mut loses).value;
        return (value, loses);
    }
    let e = root_q.ilogb();
    let scaled = root_q.scalbn_r(-e, Round::NearestTiesToEven); // -> [1,2), exact
    let mut widening_loses = false;
    let narrowed: X87DoubleExtended = if pc == 0b00 {
        let single: Single = scaled.convert_r(round, &mut loses).value;
        single.convert_r(round, &mut widening_loses).value
    } else {
        let double: Double = scaled.convert_r(round, &mut loses).value;
        double.convert_r(round, &mut widening_loses).value
    };
    debug_assert!(!widening_loses);
    (narrowed.scalbn_r(e, Round::NearestTiesToEven), loses)
}

/// Square root of a, using the given control word.
///
/// Computed at quad precision (113-bit) via `ieee-apsqrt`'s Newton-Raphson, then
/// rounded to 80-bit. Negative (non-zero) operands return the FP indefinite.
pub fn f80_sqrt(a: &F80, cw: u16) -> F80 {
    f80_sqrt_impl(a, cw, true)
}

/// FSQRT returning the x87 exception flags for the operation
/// (MDD-BUG-CRUCIBLE-00579).
///
/// Reports IE, DE, PE and C1. `x87_oracle::adjudicate_sqrt` compares every bit
/// of this against host silicon, so the model is measured rather than asserted.
pub fn f80_sqrt_status(a: &F80, cw: u16) -> (F80, u16) {
    f80_sqrt_status_impl(a, cw, true)
}

fn f80_sqrt_status_impl(a: &F80, cw: u16, allow_fast: bool) -> (F80, u16) {
    if let Some(result) = unary_arithmetic_special_case_status(a) {
        return result;
    }
    if allow_fast {
        if let Some(result) = fast_sqrt(a, cw) {
            // `fast_sqrt` only returns on an EXACT root of a safely-normal
            // operand (it rejects anything with a nonzero residual, and its
            // 1e-289 floor keeps denormals out), so nothing is raised.
            return (result, 0);
        }
    }
    let fa = to_x87(a);
    if fa.is_negative() && !fa.is_zero() {
        return (F80::INDEFINITE, 0x01); // IE: sqrt of a negative
    }
    let mut loses = false;
    let q: Quad = fa.convert_r(Round::TowardZero, &mut loses).value;
    let (root, _iters) = sqrt_fast::<u128>(q.to_bits(), Round::TowardZero);
    let inexact = root.status.contains(Status::INEXACT);
    let root_bits = if inexact { root.value | 1 } else { root.value };
    let root_q = Quad::from_bits(root_bits);
    let (x87_root, loses2) = round_sqrt_root(root_q, cw, round_from_cw(cw));
    let mut sw = 0u16;
    if a.is_denormal() {
        sw |= 0x02; // DE: denormal source operand
    }
    if inexact || loses2 {
        sw |= 0x20; // PE: the root did not fit exactly
                    // C1 is the round-up indicator, same convention as `finish_arithmetic`:
                    // did the final rounding increase the magnitude? A root is never
                    // negative here, so the directed modes are decided by direction alone
                    // and only nearest needs the toward-zero comparison. `root_q` already
                    // carries the sticky bit (odd LSB), so truncating IT is exactly the
                    // toward-zero result.
        let rounded_up = match (cw >> 10) & 0b11 {
            0b01 => false, // toward -infinity: away from a non-negative root
            0b10 => true,  // toward +infinity
            0b11 => false, // toward zero: never increases magnitude
            _ => {
                let (tz, _) = round_sqrt_root(root_q, cw, Round::TowardZero);
                magnitude_bits(x87_root) > magnitude_bits(tz)
            }
        };
        if rounded_up {
            sw |= 1 << 9;
        }
    }
    (from_x87(x87_root), sw)
}

/// `f80_sqrt`'s implementation, with the host-float fast path gated by
/// `allow_fast` so the differential tests can force the pure path and compare
/// both arms.
fn f80_sqrt_impl(a: &F80, cw: u16, allow_fast: bool) -> F80 {
    if let Some(result) = unary_arithmetic_special_case(a) {
        return result;
    }
    if allow_fast {
        if let Some(result) = fast_sqrt(a, cw) {
            return result;
        }
    }
    let fa = to_x87(a);
    // sqrt of a negative (non-zero) value is an invalid operation.
    if fa.is_negative() && !fa.is_zero() {
        return F80::INDEFINITE;
    }
    // Widen 80-bit -> 128-bit quad (exact), root in quad, narrow back to
    // 80-bit. The root is always taken toward zero: `ieee_apsqrt`'s directed
    // modes mis-round EXACT roots (observed: sqrt(9) under RC=+inf returns
    // 3.0 + 1 quad-ulp with a false INEXACT), and truncation is also what the
    // single-rounding scheme below needs. An exact root converts exactly (a
    // 64-bit-significand input's exact root fits in 32 bits); an inexact root
    // gets the round-to-odd treatment — significand LSB forced to 1 so the
    // one quad→x87 conversion in the control word's true rounding mode is the
    // only value-determining rounding (113→64-bit gap ≥ 2 bits). This also
    // fixes the former nearest-then-nearest double rounding.
    let mut loses = false;
    let q: Quad = fa.convert_r(Round::TowardZero, &mut loses).value;
    let (root, _iters) = sqrt_fast::<u128>(q.to_bits(), Round::TowardZero);
    let inexact = root.status.contains(Status::INEXACT);
    let root_bits = if inexact { root.value | 1 } else { root.value };
    let root_q = Quad::from_bits(root_bits);
    let (x87_root, _) = round_sqrt_root(root_q, cw, round_from_cw(cw));
    from_x87(x87_root)
}

/// Absolute value (pure bit manipulation — clears sign bit in byte 9).
pub fn f80_abs(a: &F80) -> F80 {
    let mut result = *a;
    result.0[9] &= 0x7F;
    result
}

/// Change sign (pure bit manipulation — flips sign bit in byte 9).
pub fn f80_chs(a: &F80) -> F80 {
    let mut result = *a;
    result.0[9] ^= 0x80;
    result
}

/// Round to integer (FRNDINT) using the rounding mode in the control word.
pub fn f80_rndint(a: &F80, cw: u16) -> F80 {
    f80_rndint_status(a, cw).0
}

/// FRNDINT returning the x87 exception flags for the operation
/// (MDD-BUG-CRUCIBLE-00579).
///
/// Reports IE, DE, PE and C1, checked bit-for-bit against host silicon by
/// `x87_oracle::adjudicate_rndint`.
pub fn f80_rndint_status(a: &F80, cw: u16) -> (F80, u16) {
    if let Some(result) = unary_arithmetic_special_case_status(a) {
        return result;
    }
    let fa = to_x87(a);
    let r = fa.round_to_integral(round_from_cw(cw));
    let mut sw = status_to_sw(r.status, a.is_denormal());
    // `round_to_integral` of a denormal produces 0 or +-1; apfloat reports that
    // as UNDERFLOW, which real FRNDINT does not raise (the result is exact in
    // the destination format once rounded to an integer).
    sw &= !0x10;
    if r.status.contains(Status::INEXACT) {
        // C1: did rounding to an integer increase the magnitude? Compare with
        // the toward-zero rounding, which by definition never does.
        let tz = fa.round_to_integral(Round::TowardZero).value;
        if magnitude_bits(r.value) > magnitude_bits(tz) {
            sw |= 1 << 9;
        }
    }
    (from_x87(r.value), sw)
}

// ============================================================================
// Transcendental operations (approximate — computed in f64)
// ============================================================================

#[derive(Clone, Copy)]
enum TrigOperand {
    NaN,
    Infinite,
    OutOfRange,
    InRange,
}

/// Classify the original extended operand before narrowing to binary64.
/// For supported finite normal encodings the explicit integer bit is set,
/// so exponent >= bias + 63 means |a| >= 2^63, including the boundary.
/// Subnormals and pseudo-denormals (exponent zero) are below that boundary.
fn classify_trig_operand(a: &F80) -> TrigOperand {
    if a.is_nan() || a.is_unsupported_encoding() {
        TrigOperand::NaN
    } else if a.is_infinite() {
        TrigOperand::Infinite
    } else if a.exponent_bits() >= 0x403e {
        TrigOperand::OutOfRange
    } else {
        TrigOperand::InRange
    }
}

/// FSIN: compute sin(a). Returns (result, C2_set) where C2 indicates out of range.
/// When C2 is set, ST(0) is unchanged (still the original operand).
pub fn f80_sin(a: &F80, cw: u16) -> (F80, bool) {
    match classify_trig_operand(a) {
        TrigOperand::NaN => return (*a, false),
        // Invalid infinity produces indefinite only when IE is masked.
        // Keep the source intact for the CPU's unmasked exception path.
        TrigOperand::Infinite if cw & 1 != 0 => return (F80::INDEFINITE, false),
        TrigOperand::Infinite | TrigOperand::OutOfRange => return (*a, true),
        TrigOperand::InRange => {}
    }
    let x = a.to_f64();
    (F80::from_f64(x.sin()), false)
}

/// FCOS: compute cos(a). Returns (result, C2_set).
/// When C2 is set, ST(0) is unchanged (still the original operand).
pub fn f80_cos(a: &F80, cw: u16) -> (F80, bool) {
    match classify_trig_operand(a) {
        TrigOperand::NaN => return (*a, false),
        // Invalid infinity produces indefinite only when IE is masked.
        // Keep the source intact for the CPU's unmasked exception path.
        TrigOperand::Infinite if cw & 1 != 0 => return (F80::INDEFINITE, false),
        TrigOperand::Infinite | TrigOperand::OutOfRange => return (*a, true),
        TrigOperand::InRange => {}
    }
    let x = a.to_f64();
    (F80::from_f64(x.cos()), false)
}

/// FSINCOS: compute sin and cos simultaneously.
/// Returns (sin, cos, C2_set).
/// When C2 is set, the first element is the original operand (no push occurred)
/// and the second is unused.
pub fn f80_sincos(a: &F80, _cw: u16) -> (F80, F80, bool) {
    match classify_trig_operand(a) {
        // NaN propagates to both results and completes the push.
        TrigOperand::NaN => return (*a, *a, false),
        // Actual infinity is invalid; fpu.rs sets IE and pushes indefinite.
        TrigOperand::Infinite => return (F80::INDEFINITE, F80::INDEFINITE, false),
        // Finite |a| >= 2^63 leaves ST(0) unchanged and does not push.
        TrigOperand::OutOfRange => return (*a, F80::ZERO, true),
        TrigOperand::InRange => {}
    }
    let x = a.to_f64();
    (F80::from_f64(x.sin()), F80::from_f64(x.cos()), false)
}

/// FPTAN: compute partial tangent.
/// Returns (tan_result, 1.0, C2_set, status_word).
/// When C2 is set, `tan_result` is the original operand and 1.0 is not pushed.
/// The VALUE stays approximate by design — this backend computes the
/// transcendentals in `f64`, which `x87_oracle` documents and deliberately
/// excludes from its value sweep. The STATUS is a different matter: leaving PE
/// and C1 clear made an ordinary finite instruction's guest-visible status word
/// depend on the host (MDD-BUG-CRUCIBLE-00639).
///
/// Adjudicated against host silicon on x86_64 rather than assumed
/// (`CW_DEFAULT`, round-to-nearest): `fptan(±0)` returns exactly zero with
/// PE and C1 clear, while `1.0`, `0.5`, `-1.0`, `2.0` and `1e-30` all return
/// `PE | C1`. That matches the native wrapper's documented policy in
/// `x87_ops::f80_ptan` — the true tangent of any non-zero finite angle is
/// transcendental, so the rounded result is inexact, and the SDM
/// (Vol 1 §8.1.3.4 / §8.3.6) has C1 report the rounding direction, which that
/// wrapper synthesizes as a roundup.
pub fn f80_ptan(a: &F80, _cw: u16) -> (F80, F80, bool, u16) {
    match classify_trig_operand(a) {
        TrigOperand::NaN => return (*a, F80::ONE, false, 0),
        TrigOperand::Infinite | TrigOperand::OutOfRange => {
            return (*a, F80::ONE, true, 0x0400);
        }
        TrigOperand::InRange => {}
    }
    let x = a.to_f64();
    let tan = x.tan();
    // Exact only where the tangent is exactly zero, i.e. a zero angle.
    let sw = if tan == 0.0 { 0 } else { 0x0020 | 0x0200 }; // PE | C1
    (F80::from_f64(tan), F80::ONE, false, sw)
}

/// FPATAN: compute atan2(y, x).
pub fn f80_patan(y: &F80, x: &F80, _cw: u16) -> F80 {
    F80::from_f64(y.to_f64().atan2(x.to_f64()))
}

/// FYL2X: compute y * log2(x).
pub fn f80_yl2x(y: &F80, x: &F80, _cw: u16) -> F80 {
    F80::from_f64(y.to_f64() * x.to_f64().log2())
}

/// FYL2XP1: compute y * log2(x + 1).
/// Uses `ln_1p` for accuracy near x = 0 (the instruction's intended domain).
pub fn f80_yl2xp1(y: &F80, x: &F80, _cw: u16) -> F80 {
    let l = x.to_f64().ln_1p() / std::f64::consts::LN_2;
    F80::from_f64(y.to_f64() * l)
}

/// F2XM1: compute 2^a - 1. Input must be in range [-1.0, 1.0].
/// Uses `exp_m1` for accuracy near a = 0.
// TODO(oracle): result is computed at f64 precision so directed rounding (RC
// bits) cannot affect the low 80-bit mantissa bits; the host x87 result is
// correctly rounded at 80-bit. Not bit-exact by design.
pub fn f80_f2xm1(a: &F80, _cw: u16) -> F80 {
    let x = a.to_f64();
    F80::from_f64((x * std::f64::consts::LN_2).exp_m1())
}

/// FSCALE: compute a * 2^trunc(b).
///
/// The operand classes go through the same resolver as the arithmetic
/// operations (MDD-BUG-TEM-00171): an unsupported encoding in either operand
/// is the indefinite; a NaN in either operand propagates — the larger
/// significand when both are NaNs — quieted. The hand-rolled checks this
/// replaces returned ST(0)'s SNaN unquieted, an unnormal ST(0) unchanged, and
/// the indefinite for a NaN in ST(1), none of which is what FSCALE does; the
/// x87 oracle now adjudicates FSCALE against host silicon.
pub fn f80_scale(a: &F80, b: &F80, cw: u16) -> F80 {
    if let Some((special, _)) = binary_arithmetic_special_case(a, b) {
        return special;
    }
    let fa = to_x87(a);
    let fb = to_x87(b);
    // An infinite scale is exact, never an overflow or underflow that a
    // directed rounding mode would pull back to the largest finite value or
    // the smallest denormal: the SDM's FSCALE table gives ±inf for +inf and
    // ±0 for -inf (sign of ST(0)), and the indefinite for its two invalid
    // pairings, a zero scaled by +inf and an infinity scaled by -inf. Tested
    // on the extended operand: a huge finite scale remains an ordinary
    // exponent that overflows or underflows under the rounding mode.
    if fb.is_infinite() {
        let up = !fb.is_negative();
        if (fa.is_zero() && up) || (fa.is_infinite() && !up) {
            return F80::INDEFINITE;
        }
        let sign = fa.is_negative();
        return if up {
            F80::from_f64(if sign {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            })
        } else {
            F80::from_f64(if sign { -0.0 } else { 0.0 })
        };
    }
    // Truncate the extended value directly, independently of CW rounding.
    // apfloat saturates out-of-range integer conversions to the signed bounds;
    // scalbn handles huge exponents with the result's rounding mode.
    let n = fb.to_i128(32).value as i32;
    from_x87(fa.scalbn_r(n, round_from_cw(cw)))
}

/// FXTRACT: decompose a into significand and exponent.
/// Returns (significand, exponent), where `a == significand * 2^exponent` and
/// the significand is in [1, 2) with the sign of `a`.
pub fn f80_xtract(a: &F80, _cw: u16) -> (F80, F80) {
    // Same resolver as the unary arithmetic (MDD-BUG-TEM-00171): an
    // unsupported encoding yields the indefinite in both results, a NaN is
    // quieted into both. The old check returned an SNaN unquieted.
    if let Some(special) = unary_arithmetic_special_case(a) {
        return (special, special);
    }
    let fa = to_x87(a);
    if fa.is_zero() {
        // significand = signed zero; exponent = -inf.
        return (*a, F80::from_f64(f64::NEG_INFINITY));
    }
    if fa.is_infinite() {
        // significand = inf (sign of a); exponent = +inf.
        return (*a, F80::from_f64(f64::INFINITY));
    }
    let e = fa.ilogb(); // unbiased exponent (floor(log2|a|))
    let sig = fa.scalbn_r(-e, Round::NearestTiesToEven);
    let exp = X87DoubleExtended::from_i128(i128::from(e)).value;
    (from_x87(sig), from_x87(exp))
}

/// FPREM: partial IEEE-incompatible remainder of a / b.
/// Returns (remainder, status_word_bits).
/// Status word bits: C2 (bit 10) = 1 means reduction incomplete.
/// C0, C3, C1 contain the three low bits of the quotient (final step only).
///
/// Implements the real hardware *partial* reduction: when the dividend/divisor
/// base-2 exponent gap is >= 64 it reduces by one bounded step and sets C2=1 so
/// the guest re-issues; the final step (gap < 64) completes with C2=0 and the
/// low three quotient bits in C0/C3/C1. See `f80_prem_impl`.
pub fn f80_prem(a: &F80, b: &F80, cw: u16) -> (F80, u16) {
    f80_prem_impl(a, b, cw, Round::TowardZero)
}

/// FPREM1: IEEE-compliant partial remainder of a / b.
/// Same interface and partial-reduction semantics as `f80_prem`; the final step
/// rounds the quotient to nearest-even rather than truncating.
pub fn f80_prem1(a: &F80, b: &F80, cw: u16) -> (F80, u16) {
    f80_prem_impl(a, b, cw, Round::NearestTiesToEven)
}

/// Shared FPREM/FPREM1 body. `quot_round` selects the quotient rounding used for
/// the remainder (truncate for FPREM, nearest-even for FPREM1).
fn f80_prem_impl(a: &F80, b: &F80, _cw: u16, quot_round: Round) -> (F80, u16) {
    let fa = to_x87(a);
    let fb = to_x87(b);
    if fb.is_zero() || fa.is_nan() || fb.is_nan() || fa.is_infinite() {
        // Invalid operation: indefinite result, reduction "complete".
        return (F80::INDEFINITE, status_to_sw(Status::INVALID_OP, false));
    }
    if fb.is_infinite() {
        // Finite dividend / infinite divisor: remainder is the dividend.
        return (*a, 0);
    }
    // A zero dividend reduces trivially: remainder = a, quotient 0, complete.
    // (Also guards ilogb(zero), which returns a large-negative sentinel that
    // would overflow the `d` subtraction below in debug builds.)
    if fa.is_zero() {
        return (
            *a,
            status_to_sw(Status::OK, a.is_denormal() || b.is_denormal()),
        );
    }
    // Partial-remainder step. Real x87 FPREM/FPREM1 strip at most ~2^63 of
    // magnitude per execution: when the unbiased base-2 exponent gap
    // D = ilogb(a) - ilogb(b) >= 64, reduce by b * 2^(D-N) (N implementation-
    // defined, here 32) and return C2=1 so the guest re-issues. The scale is a
    // power of two ≥ 2^32 — an exact integer multiple of b — so the result
    // stays congruent to a mod b. C0/C1/C3 (quotient bits) are undefined on a
    // partial step; only the final (D<64) step yields them. Both FPREM and
    // FPREM1 truncate the partial step (c_fmod) — the round/IEEE distinction
    // only applies to the final reduction below.
    const N: i32 = 32;
    let d = fa.ilogb() - fb.ilogb();
    if d >= 64 {
        let b_scaled = fb.scalbn_r(d - N, Round::NearestTiesToEven); // exact: pure exponent shift
        let rem = fa.c_fmod(b_scaled);
        let mut sw = status_to_sw(rem.status, a.is_denormal() || b.is_denormal());
        sw |= 1 << 10; // C2 = reduction incomplete
        return (from_x87(rem.value), sw);
    }
    let rem = if quot_round == Round::TowardZero {
        fa.c_fmod(fb)
    } else {
        fa.ieee_rem(fb)
    };
    let mut sw = status_to_sw(rem.status, a.is_denormal() || b.is_denormal());
    // Quotient low three bits: q = round(a / b) under `quot_round`.
    let quotient = fa
        .div_r(fb, quot_round)
        .value
        .round_to_integral(quot_round)
        .value;
    let mut exact = true;
    let q = quotient
        .to_i128_r(64, Round::TowardZero, &mut exact)
        .value
        .unsigned_abs();
    if q & 0b001 != 0 {
        sw |= 1 << 9; // C1 = Q0
    }
    if q & 0b010 != 0 {
        sw |= 1 << 14; // C3 = Q1
    }
    if q & 0b100 != 0 {
        sw |= 1 << 8; // C0 = Q2
    }
    (from_x87(rem.value), sw)
}

// ============================================================================
// Comparison
// ============================================================================

/// Host-float fast path for FUCOM-family comparisons: exact for any operand
/// pair that maps onto binary64 with no rounding — comparison doesn't round,
/// so this needs no PC/RC gate. NaN, infinity, denormal, and unnormal/pseudo
/// operands fall back to the pure path (the shared exact-f64 extraction
/// rejects all of them, conservatively bundling infinity in with NaN even
/// though an exact infinity compare would also be safe).
fn fast_compare(a: &F80, b: &F80) -> Option<u16> {
    let fa = fast_f80_exact_f64(a.bytes())?;
    let fb = fast_f80_exact_f64(b.bytes())?;
    record_fast_path_hit();
    Some(match fa.partial_cmp(&fb) {
        Some(Ordering::Less) => 0x0100,
        Some(Ordering::Equal) => 0x4000,
        Some(Ordering::Greater) => 0x0000,
        None => unreachable!("fast_f80_exact_f64 excludes NaN"),
    })
}

/// FUCOM: unordered comparison of a and b.
/// Returns status word bits (C0=bit 8, C2=bit 10, C3=bit 14):
/// - a == b: C3 set (0x4000)
/// - a <  b: C0 set (0x0100)
/// - a >  b: all clear
/// - unordered (NaN operand): C3|C2|C0 set (0x4500)
pub fn f80_compare(a: &F80, b: &F80) -> u16 {
    f80_compare_impl(a, b, true)
}

/// `f80_compare`'s implementation, with the host-float fast path gated by
/// `allow_fast` so the differential tests can force the pure path and compare
/// both arms.
fn f80_compare_impl(a: &F80, b: &F80, allow_fast: bool) -> u16 {
    if allow_fast {
        if let Some(sw) = fast_compare(a, b) {
            return sw;
        }
    }
    let fa = to_x87(a);
    let fb = to_x87(b);
    if fa.is_nan() || fb.is_nan() {
        return 0x4500; // C3 | C2 | C0 — unordered
    }
    match fa.partial_cmp(&fb) {
        Some(Ordering::Less) => 0x0100,    // C0
        Some(Ordering::Equal) => 0x4000,   // C3
        Some(Ordering::Greater) => 0x0000, // all clear
        None => 0x4500,                    // unordered (NaN already handled)
    }
}

/// FUCOM with status word (same as `f80_compare` — provided for API completeness).
pub fn f80_compare_and_pop(a: &F80, b: &F80) -> u16 {
    f80_compare(a, b)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::approx_constant)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const CW_DEFAULT: u16 = 0x037F; // round-to-nearest, 64-bit, all masked
    const CW_TRUNCATE: u16 = 0x0F7F; // RC=11 (truncate toward zero)
    const CW_ROUND_DOWN: u16 = 0x077F; // RC=01 (toward -inf)
    const CW_ROUND_UP: u16 = 0x0B7F; // RC=10 (toward +inf)

    fn assert_trig_infinity_invalid(op: fn(&F80, u16) -> (F80, bool)) {
        let indefinite = [0, 0, 0, 0, 0, 0, 0, 0xc0, 0xff, 0xff];
        for sign in [0u16, 0x8000] {
            let mut bytes = [0; 10];
            bytes[7] = 0x80;
            bytes[8..].copy_from_slice(&(0x7fff | sign).to_le_bytes());
            let operand = F80(bytes);
            let (result, c2) = op(&operand, CW_DEFAULT);
            assert_eq!(
                result.0, indefinite,
                "masked infinity must produce indefinite"
            );
            assert!(result.is_nan());
            assert_eq!(result.x87_tag(), 2);
            assert!(!c2, "infinity is invalid, not a finite range refusal");
            let (result, _) = op(&operand, CW_DEFAULT & !1);
            assert_eq!(result.0, bytes, "unmasked invalid must retain the operand");
        }
    }

    #[test]
    fn trig_infinity_invalid_sin() {
        assert_trig_infinity_invalid(f80_sin);
    }

    #[test]
    fn trig_infinity_invalid_cos() {
        assert_trig_infinity_invalid(f80_cos);
    }

    fn assert_trig_extended_range(op: u8, huge_only: bool) {
        // Hardware encodings, independent of binary64 rounding: 2^63-0.5,
        // 2^63, 2^63+1, and finite 2^1024.
        let cases = if huge_only {
            vec![(0x8000_0000_0000_0000u64, 0x43ffu16, true)]
        } else {
            vec![
                (u64::MAX, 0x403d, false),
                (0x8000_0000_0000_0000, 0x403e, true),
                (0x8000_0000_0000_0001, 0x403e, true),
                (0x8000_0000_0000_0000, 0x43ff, true),
            ]
        };
        for sign in [0, 0x8000] {
            for &(significand, exponent, out_of_range) in &cases {
                let mut bytes = [0; 10];
                bytes[..8].copy_from_slice(&significand.to_le_bytes());
                bytes[8..].copy_from_slice(&(exponent | sign).to_le_bytes());
                let operand = F80(bytes);
                let (result, second, c2) = match op {
                    0 => {
                        let (r, c2) = f80_sin(&operand, CW_DEFAULT);
                        (r, None, c2)
                    }
                    1 => {
                        let (r, c2) = f80_cos(&operand, CW_DEFAULT);
                        (r, None, c2)
                    }
                    2 => {
                        let (r, s, c2) = f80_sincos(&operand, CW_DEFAULT);
                        (r, Some(s), c2)
                    }
                    _ => {
                        let (r, s, c2, sw) = f80_ptan(&operand, CW_DEFAULT);
                        assert_eq!(
                            sw & 0x0400 != 0,
                            out_of_range,
                            "FPTAN status C2, {bytes:02x?}"
                        );
                        (r, Some(s), c2)
                    }
                };
                assert_eq!(
                    c2, out_of_range,
                    "exact extended range op={op}, {bytes:02x?}"
                );
                if out_of_range {
                    assert_eq!(result.0, bytes, "C2 must preserve all operand bytes");
                } else {
                    assert!(!result.is_nan() && !result.is_infinite());
                    if let Some(second) = second {
                        assert!(!second.is_nan() && !second.is_infinite());
                        if op == 3 {
                            assert_eq!(second.0, F80::ONE.0);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn trig_extended_range_sin() {
        assert_trig_extended_range(0, false);
    }
    #[test]
    fn trig_extended_range_cos() {
        assert_trig_extended_range(1, false);
    }
    #[test]
    fn trig_extended_range_sincos() {
        assert_trig_extended_range(2, false);
    }
    #[test]
    fn trig_extended_range_ptan() {
        assert_trig_extended_range(3, false);
    }
    #[test]
    fn trig_extended_range_sincos_finite_huge() {
        assert_trig_extended_range(2, true);
    }

    /// `from_f64`'s exact bit path must agree with the apfloat widening for
    /// every class: normals across the exponent range, signed zeros, and the
    /// subnormal/infinite/NaN inputs that fall back to apfloat.
    #[test]
    fn from_f64_bit_path_matches_apfloat_widening() {
        let mut bits_list = vec![
            0u64,
            0x8000_0000_0000_0000,
            1,
            0x000f_ffff_ffff_ffff,
            0x8000_0000_0000_0001,
            0x0010_0000_0000_0000,
            0x3ff0_0000_0000_0000,
            0x7fef_ffff_ffff_ffff,
            0xffef_ffff_ffff_ffff,
            0x7ff0_0000_0000_0000,
            0xfff0_0000_0000_0000,
            0x7ff8_0000_0000_0000,
            0x7ff0_0000_0000_0001,
            0xfff8_dead_beef_0001,
        ];
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..20_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            bits_list.push(x);
        }
        for bits in bits_list {
            let value = f64::from_bits(bits);
            assert_eq!(
                F80::from_f64(value).0,
                F80::from_f64_apfloat(value).0,
                "from_f64 diverges from apfloat for {bits:#018x}"
            );
        }
    }

    /// The soft `to_iNN_status` mirrors report FIST's IE/PE exception bits
    /// exactly like `x87_ops::F80::to_iNN_status` on every host architecture.
    #[test]
    fn to_int_status_reports_ie_and_pe_like_the_host_helpers() {
        // Exact conversions: no exception bits, any width.
        for cw in [CW_DEFAULT, CW_TRUNCATE, CW_ROUND_DOWN, CW_ROUND_UP] {
            let five = F80::from_f64(5.0);
            assert_eq!(five.to_i16_status(cw), (5, 0));
            assert_eq!(five.to_i32_status(cw), (5, 0));
            assert_eq!(five.to_i64_status(cw), (5, 0));
            assert_eq!(F80::ZERO.to_i32_status(cw), (0, 0));
        }

        // Inexact conversions: PE (0x20) alone, value rounded per RC. FLDPI
        // then FIST m32 reads SW=0x…20 on host silicon — the defect-D probe.
        let pi = F80::PI;
        assert_eq!(pi.to_i16_status(CW_DEFAULT), (3, 0x20));
        assert_eq!(pi.to_i32_status(CW_DEFAULT), (3, 0x20));
        assert_eq!(pi.to_i64_status(CW_DEFAULT), (3, 0x20));
        // C1 (0x200) joins PE wherever the store rounded the MAGNITUDE up.
        // These two literals predate C1 being computed at all, so they read
        // PE-only (MDD-BUG-FLUX-00127). Measured on host silicon:
        // RD -2.5 -> -3 sw=0x0220; pi under +inf rounds 3.14 -> 4, magnitude up.
        assert_eq!(pi.to_i32_status(CW_ROUND_UP), (4, 0x220));
        let neg = F80::from_f64(-2.5);
        assert_eq!(neg.to_i32_status(CW_DEFAULT), (-2, 0x20)); // ties-to-even
        assert_eq!(neg.to_i32_status(CW_TRUNCATE), (-2, 0x20));
        assert_eq!(neg.to_i32_status(CW_ROUND_DOWN), (-3, 0x220));

        // Invalid conversions: IE (0x01) alone, integer indefinite.
        for bad in [F80::from_f64(f64::NAN), F80::from_f64(f64::INFINITY)] {
            assert_eq!(bad.to_i16_status(CW_DEFAULT), (i16::MIN, 0x01));
            assert_eq!(bad.to_i32_status(CW_DEFAULT), (i32::MIN, 0x01));
            assert_eq!(bad.to_i64_status(CW_DEFAULT), (i64::MIN, 0x01));
        }
        let big = F80::from_f64(1e12);
        assert_eq!(big.to_i16_status(CW_DEFAULT), (i16::MIN, 0x01));
        assert_eq!(big.to_i32_status(CW_DEFAULT), (i32::MIN, 0x01));

        // The i32 fast path (canonical_f80_to_i32) and the apfloat
        // fallback agree on PE: a denormal input skips the fast path.
        let denormal = F80::from_f64(f64::MIN_POSITIVE / 4.0);
        let (value, sw) = denormal.to_i32_status(CW_DEFAULT);
        assert_eq!((value, sw), (0, 0x20));
    }

    #[derive(Clone, Copy, Debug)]
    enum ArithmeticOp {
        Add,
        Sub,
        Mul,
        Div,
    }

    fn eager_arithmetic(op: ArithmeticOp, a: &F80, b: &F80, cw: u16) -> (F80, u16) {
        if let Some(result) = binary_arithmetic_special_case(a, b) {
            return result;
        }
        let fa = to_x87(a);
        let fb = to_x87(b);
        let round = round_from_cw(cw);
        let (value, toward_zero, status) = match op {
            ArithmeticOp::Add => {
                let r = fa.add_r(fb, round);
                (r.value, fa.add_r(fb, Round::TowardZero).value, r.status)
            }
            ArithmeticOp::Sub => {
                let r = fa.sub_r(fb, round);
                (r.value, fa.sub_r(fb, Round::TowardZero).value, r.status)
            }
            ArithmeticOp::Mul => {
                let r = fa.mul_r(fb, round);
                (r.value, fa.mul_r(fb, Round::TowardZero).value, r.status)
            }
            ArithmeticOp::Div => {
                let r = fa.div_r(fb, round);
                (r.value, fa.div_r(fb, Round::TowardZero).value, r.status)
            }
        };
        let status = restore_x87_overflow(status, cw, || match op {
            ArithmeticOp::Add => fa.add_r(fb, Round::NearestTiesToEven).status,
            ArithmeticOp::Sub => fa.sub_r(fb, Round::NearestTiesToEven).status,
            ArithmeticOp::Mul => fa.mul_r(fb, Round::NearestTiesToEven).status,
            ArithmeticOp::Div => fa.div_r(fb, Round::NearestTiesToEven).status,
        });
        let denormal_operand = match op {
            ArithmeticOp::Div => b.is_denormal() || (a.is_denormal() && !b.is_zero()),
            _ => a.is_denormal() || b.is_denormal(),
        };
        finish_arithmetic_eager(value, toward_zero, status, denormal_operand, cw)
    }

    fn lazy_arithmetic(op: ArithmeticOp, a: &F80, b: &F80, cw: u16) -> (F80, u16) {
        match op {
            ArithmeticOp::Add => f80_add_status(a, b, cw),
            ArithmeticOp::Sub => f80_sub_status(a, b, cw),
            ArithmeticOp::Mul => f80_mul_status(a, b, cw),
            ArithmeticOp::Div => f80_div_status(a, b, cw),
        }
    }

    fn extended(sign: bool, exponent: u16, significand: u64) -> F80 {
        let mut bytes = [0; 10];
        bytes[..8].copy_from_slice(&significand.to_le_bytes());
        bytes[8..].copy_from_slice(&(exponent | u16::from(sign) << 15).to_le_bytes());
        F80(bytes)
    }

    // ---- bit inspection on known patterns ----

    #[test]
    fn x87_soft_is_nan_known_patterns() {
        assert!(F80::INDEFINITE.is_nan());
        assert!(!F80::INDEFINITE.is_infinite());
        assert!(!F80::INDEFINITE.is_zero());

        let nan = F80::from_f64(f64::NAN);
        assert!(nan.is_nan());
        assert!(!nan.is_infinite());
        assert!(!nan.is_zero());
    }

    #[test]
    fn x87_soft_exceptional_encoding_precedence() {
        let denormal = extended(false, 0, 1);
        let pseudo_denormal = extended(false, 0, 0x8000_0000_0000_0000);
        let unnormal = extended(false, 0x4000, 0x4000_0000_0000_0000);
        let snan = extended(false, 0x7fff, 0x8000_0000_0000_0001);
        let quieted_snan = extended(false, 0x7fff, 0xc000_0000_0000_0001);
        let larger_qnan = extended(false, 0x7fff, 0xc000_0000_0000_0002);

        assert!(snan.is_nan());
        assert!(snan.is_signaling_nan());
        assert!(!unnormal.is_nan());
        assert!(unnormal.is_unsupported_encoding());

        assert_eq!(
            f80_add_status(&snan, &F80::ONE, CW_DEFAULT),
            (quieted_snan, 0x01),
            "SNaN must be quieted with IE and its payload preserved"
        );
        assert_eq!(
            f80_add_status(&quieted_snan, &larger_qnan, CW_DEFAULT),
            (larger_qnan, 0),
            "two NaNs must select the larger significand"
        );
        assert_eq!(
            f80_add_status(&quieted_snan, &denormal, CW_DEFAULT),
            (quieted_snan, 0),
            "NaN propagation must suppress an unexamined denormal's DE"
        );
        assert_eq!(
            f80_add_status(&unnormal, &quieted_snan, CW_DEFAULT),
            (F80::INDEFINITE, 0x01),
            "unsupported encoding must take precedence over NaN propagation"
        );

        let (_, pseudo_sw) = f80_add_status(&pseudo_denormal, &F80::ONE, CW_DEFAULT);
        assert_eq!(
            pseudo_sw & 0x3f,
            0x22,
            "inexact pseudo-denormal arithmetic raises DE+PE"
        );
        let (_, div_sw) = f80_div_status(&denormal, &F80::ZERO, CW_DEFAULT);
        assert_eq!(
            div_sw & 0x3f,
            0x04,
            "divide-by-zero takes precedence over numerator DE"
        );

        assert_eq!(f80_sqrt(&snan, CW_DEFAULT), quieted_snan);
        assert_eq!(f80_rndint(&unnormal, CW_DEFAULT), F80::INDEFINITE);
    }

    #[test]
    fn x87_soft_is_infinite_known_patterns() {
        let pos_inf = F80::from_f64(f64::INFINITY);
        assert!(pos_inf.is_infinite());
        assert!(!pos_inf.is_nan());
        assert!(!pos_inf.is_sign_negative());

        let neg_inf = F80::from_f64(f64::NEG_INFINITY);
        assert!(neg_inf.is_infinite());
        assert!(neg_inf.is_sign_negative());
    }

    #[test]
    fn x87_soft_is_zero_and_sign() {
        assert!(F80::ZERO.is_zero());
        assert!(!F80::ZERO.is_sign_negative());
        assert!(F80::NEG_ZERO.is_zero());
        assert!(F80::NEG_ZERO.is_sign_negative());
        assert!(!F80::ONE.is_zero());
        assert!(!F80::ONE.is_nan());
        assert!(!F80::ONE.is_infinite());
    }

    // ---- constants decode to the right magnitudes ----

    #[test]
    fn x87_soft_constants_decode() {
        assert_eq!(F80::ZERO.to_f64(), 0.0);
        assert_eq!(F80::ONE.to_f64(), 1.0);
        assert!(F80::NEG_ZERO.to_f64().is_sign_negative());
        assert!((F80::PI.to_f64() - std::f64::consts::PI).abs() < 1e-15);
        assert!((F80::LOG2_E.to_f64() - std::f64::consts::LOG2_E).abs() < 1e-14);
        assert!((F80::LOG2_10.to_f64() - 10.0_f64.log2()).abs() < 1e-14);
        assert!((F80::LOG10_2.to_f64() - 2.0_f64.log10()).abs() < 1e-15);
        assert!((F80::LN_2.to_f64() - std::f64::consts::LN_2).abs() < 1e-15);
    }

    // ---- f64 / f32 round-trips (representable values) ----

    #[test]
    fn x87_soft_f64_round_trip() {
        for &v in &[
            0.0,
            1.0,
            -1.0,
            3.14159,
            1e100,
            -1e-100,
            0.1,
            42.0,
            -0.0,
            2.5,
            -123456.789,
        ] {
            let back = F80::from_f64(v).to_f64();
            assert_eq!(v.to_bits(), back.to_bits(), "f64 round-trip failed for {v}");
        }
        assert_eq!(F80::from_f64(f64::INFINITY).to_f64(), f64::INFINITY);
        assert_eq!(F80::from_f64(f64::NEG_INFINITY).to_f64(), f64::NEG_INFINITY);
        assert!(F80::from_f64(f64::NAN).to_f64().is_nan());
    }

    #[test]
    fn x87_soft_f32_round_trip() {
        for &v in &[0.0f32, 1.0, -1.0, 3.14, 1e10, -1e-10, 0.5, -7.25] {
            let back = F80::from_f32(v).to_f32();
            assert_eq!(v.to_bits(), back.to_bits(), "f32 round-trip failed for {v}");
        }
        assert_eq!(F80::from_f32(f32::INFINITY).to_f32(), f32::INFINITY);
        assert!(F80::from_f32(f32::NAN).to_f32().is_nan());
    }

    // ---- arithmetic algebraic identities ----

    #[test]
    fn x87_soft_add_identity_and_commutativity() {
        for &v in &[0.0, 1.0, -1.0, 3.5, 1e20, -2.25, 0.1] {
            let a = F80::from_f64(v);
            // a + 0 == a
            assert_eq!(f80_add(&a, &F80::ZERO, CW_DEFAULT), a, "a+0 != a for {v}");
            // commutativity: a + b == b + a
            let b = F80::from_f64(7.0);
            assert_eq!(
                f80_add(&a, &b, CW_DEFAULT),
                f80_add(&b, &a, CW_DEFAULT),
                "add not commutative for {v}"
            );
        }
    }

    #[test]
    fn x87_soft_mul_identity_and_commutativity() {
        for &v in &[0.0, 1.0, -1.0, 3.5, 1e20, -2.25, 0.1] {
            let a = F80::from_f64(v);
            // a * 1 == a
            assert_eq!(f80_mul(&a, &F80::ONE, CW_DEFAULT), a, "a*1 != a for {v}");
            let b = F80::from_f64(-9.0);
            assert_eq!(
                f80_mul(&a, &b, CW_DEFAULT),
                f80_mul(&b, &a, CW_DEFAULT),
                "mul not commutative for {v}"
            );
        }
    }

    #[test]
    fn x87_soft_add_multiply_publish_exception_status() {
        let (_value, sw) = f80_add_status(
            &F80::from_f64(f64::INFINITY),
            &F80::from_f64(f64::NEG_INFINITY),
            CW_DEFAULT,
        );
        assert_ne!(sw & 0x01, 0, "FADD invalid must report IE");

        let (_value, sw) = f80_mul_status(&F80::from_f64(f64::INFINITY), &F80::ZERO, CW_DEFAULT);
        assert_ne!(sw & 0x01, 0, "FMUL invalid must report IE");

        let denormal = F80([1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let (_value, sw) = f80_add_status(&denormal, &F80::ONE, CW_DEFAULT);
        assert_ne!(sw & 0x02, 0, "FADD denormal operand must report DE");

        let max_finite = F80([0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe, 0x7f]);
        let (_value, sw) = f80_mul_status(&max_finite, &F80::from_i16(2), CW_DEFAULT);
        assert_eq!(sw & 0x28, 0x28, "FMUL overflow must report OE+PE");

        for cw in [CW_ROUND_DOWN, CW_TRUNCATE] {
            let (value, sw) = f80_mul_status(&max_finite, &F80::from_i16(2), cw);
            assert_eq!(
                value, max_finite,
                "directed positive overflow rounds finite"
            );
            assert_eq!(
                sw & 0x28,
                0x28,
                "directed finite overflow must still report OE+PE"
            );
        }
    }

    #[test]
    fn x87_soft_pc_rounding_reports_precision_and_c1() {
        let cw_single_nearest = 0x007f;
        let half_plus_one = F80::from_f64(3.0 * 2f64.powi(-24));
        let (_value, sw) = f80_add_status(&F80::ONE, &half_plus_one, cw_single_nearest);
        assert_eq!(
            sw & 0x0220,
            0x0220,
            "rounded-up PC-single add must report PE+C1"
        );

        let (_value, sw) = f80_add_status(&F80::ONE, &F80::ONE, cw_single_nearest);
        assert_eq!(sw & 0x0220, 0, "exact PC-single add must clear PE+C1");
    }

    #[test]
    fn x87_soft_lazy_arithmetic_matches_eager_reference() {
        let values = [
            F80::ZERO,
            F80::NEG_ZERO,
            F80::ONE,
            F80::from_f64(-1.0),
            F80::from_f64(0.1),
            F80::from_f64(-3.0 * 2f64.powi(-24)),
            extended(false, 0x3fff, 0x8000_0000_0000_0001),
            extended(true, 0x4000, 0xffff_ffff_ffff_ffff),
            extended(false, 0, 1),
            extended(true, 1, 0x8000_0000_0000_0000),
            extended(false, 0x7ffe, 0xffff_ffff_ffff_ffff),
            F80::from_f64(f64::INFINITY),
            F80::from_f64(f64::NEG_INFINITY),
            F80::from_f64(f64::NAN),
            extended(false, 0x7fff, 0x8000_0000_0000_0001),
        ];

        for op in [
            ArithmeticOp::Add,
            ArithmeticOp::Sub,
            ArithmeticOp::Mul,
            ArithmeticOp::Div,
        ] {
            for pc in 0..4 {
                for rc in 0..4 {
                    let cw = 0x007f | (pc << 8) | (rc << 10);
                    for a in &values {
                        for b in &values {
                            assert_eq!(
                                lazy_arithmetic(op, a, b, cw),
                                eager_arithmetic(op, a, b, cw),
                                "{op:?} diverged for a={a:?}, b={b:?}, PC={pc:02b}, RC={rc:02b}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn x87_soft_toward_zero_operation_is_strictly_lazy() {
        fn finish_counted(
            value: X87DoubleExtended,
            status: Status,
            cw: u16,
        ) -> ((F80, u16), usize) {
            let calls = Cell::new(0);
            let result = finish_arithmetic(value, status, false, cw, || {
                calls.set(calls.get() + 1);
                value
            });
            (result, calls.get())
        }

        let one = to_x87(&F80::ONE);
        assert_eq!(finish_counted(one, Status::OK, CW_DEFAULT).1, 0);

        let pc_inexact = to_x87(&F80::from_f64(1.0 + 3.0 * 2f64.powi(-24)));
        let ((_, sw), calls) = finish_counted(pc_inexact, Status::OK, 0x007f);
        assert_eq!(sw & 0x20, 0x20, "PC narrowing must report PE");
        assert_eq!(calls, 0, "PC-only inexactness must reuse the exact result");

        assert_eq!(
            finish_counted(one, Status::INEXACT, CW_DEFAULT).1,
            1,
            "nearest-even inexact arithmetic needs the toward-zero operation"
        );

        for (value, cw, expected_c1) in [
            (F80::ONE, CW_ROUND_DOWN, 0),
            (F80::from_f64(-1.0), CW_ROUND_DOWN, 1 << 9),
            (F80::ONE, CW_ROUND_UP, 1 << 9),
            (F80::from_f64(-1.0), CW_ROUND_UP, 0),
            (F80::ONE, CW_TRUNCATE, 0),
        ] {
            let ((_, sw), calls) = finish_counted(to_x87(&value), Status::INEXACT, cw);
            assert_eq!(calls, 0, "directed RC must not repeat the operation");
            assert_eq!(sw & (1 << 9), expected_c1);
        }

        assert_eq!(
            finish_counted(one, Status::INVALID_OP, CW_DEFAULT).1,
            0,
            "invalid operations return before C1 derivation"
        );
    }

    #[test]
    fn x87_soft_sub_and_div_values() {
        let a = F80::from_f64(10.0);
        let b = F80::from_f64(4.0);
        assert!((f80_sub(&a, &b, CW_DEFAULT).to_f64() - 6.0).abs() < 1e-12);
        assert!((f80_div(&a, &b, CW_DEFAULT).to_f64() - 2.5).abs() < 1e-12);
        // a - a == 0
        assert!(f80_sub(&a, &a, CW_DEFAULT).is_zero());
        // x / 0 reports the divide-by-zero exception bit (ZE, bit 2).
        let (_q, sw) = f80_div_status(&a, &F80::ZERO, CW_DEFAULT);
        assert_ne!(sw & 0x04, 0, "ZE not set on divide by zero");
    }

    // ---- abs / chs ----

    #[test]
    fn x87_soft_abs_chs() {
        for &v in &[1.0, -1.0, 3.5, -42.0, 0.0] {
            let a = F80::from_f64(v);
            assert!(!f80_abs(&a).is_sign_negative(), "abs left sign for {v}");
            // chs flips the sign bit.
            assert_eq!(
                f80_chs(&a).is_sign_negative(),
                !a.is_sign_negative(),
                "chs did not flip sign for {v}"
            );
            // chs is an involution.
            assert_eq!(f80_chs(&f80_chs(&a)), a, "chs not involutive for {v}");
        }
    }

    // ---- sqrt ----

    #[test]
    fn x87_soft_sqrt() {
        for &(v, r) in &[
            (4.0, 2.0),
            (9.0, 3.0),
            (2.0, std::f64::consts::SQRT_2),
            (0.0, 0.0),
        ] {
            let got = f80_sqrt(&F80::from_f64(v), CW_DEFAULT).to_f64();
            assert!((got - r).abs() < 1e-15, "sqrt({v}) = {got}, want {r}");
        }
        // sqrt of a negative is the FP indefinite.
        assert_eq!(f80_sqrt(&F80::from_f64(-4.0), CW_DEFAULT), F80::INDEFINITE);
    }

    /// The 80-bit image of a significand and a biased exponent (sign clear).
    fn f80_bits(significand: u64, exponent: u16) -> F80 {
        let mut bytes = [0u8; 10];
        bytes[..8].copy_from_slice(&significand.to_le_bytes());
        bytes[8..].copy_from_slice(&exponent.to_le_bytes());
        F80(bytes)
    }

    /// MDD-BUG-TEM-00170: FSQRT honours precision control (SDM Vol 1 8.1.5.2).
    /// sqrt(2)'s 64-bit significand is 0xB504F333_F9DE6484; its 24-bit rounding
    /// (the bits after 0xB504F3 start 0x33, below half) is 0xB504F3 and its
    /// 53-bit rounding keeps bits 63..11 of 0xB504F333F9DE6484 rounded at bit
    /// 10 (0x484 = 0b100_1000_0100: bit 10 set, lower bits nonzero -> up).
    /// Both the pure path and the public (fast-path-first) entry must agree.
    #[test]
    fn fsqrt_rounds_to_the_precision_control_width() {
        let two = F80::from_f64(2.0);
        let cases = [
            (0x037Fu16, 0xB504_F333_F9DE_6484u64), // PC=64, nearest
            (0x007F, 0xB504_F300_0000_0000),       // PC=24, nearest
            (0x0C7F, 0xB504_F300_0000_0000),       // PC=24, toward zero
            (0x087F, 0xB504_F400_0000_0000),       // PC=24, toward +inf
            (0x027F, 0xB504_F333_F9DE_6800),       // PC=53, nearest
            (0x0E7F, 0xB504_F333_F9DE_6000),       // PC=53, toward zero
        ];
        for (cw, significand) in cases {
            let want = f80_bits(significand, 0x3FFF);
            for allow_fast in [false, true] {
                let (got, sw) = f80_sqrt_status_impl(&two, cw, allow_fast);
                assert_eq!(got, want, "sqrt(2) under CW {cw:#06x}");
                assert_ne!(sw & 0x20, 0, "sqrt(2) is inexact under CW {cw:#06x}");
                assert_eq!(
                    f80_sqrt_impl(&two, cw, allow_fast),
                    want,
                    "value path, CW {cw:#06x}"
                );
            }
        }
    }

    /// The exact-root fast path must not bypass PC either: (2^24 + 1)^2 has an
    /// exact 25-bit root, which PC=24 nearest-even rounds to 2^24.
    #[test]
    fn an_exact_root_wider_than_pc24_is_still_rounded() {
        let v = ((1u64 << 24) + 1) as f64;
        let square = F80::from_f64(v * v);
        let (got, sw) = f80_sqrt_status(&square, 0x007F);
        assert_eq!(
            got,
            F80::from_f64((1u64 << 24) as f64),
            "PC=24 rounds the 25-bit root"
        );
        assert_ne!(sw & 0x20, 0, "the narrowing is inexact");
        assert_eq!(
            f80_sqrt(&square, 0x037F),
            F80::from_f64(v),
            "PC=64 keeps it exact"
        );
    }

    // ---- integer round-trips and FRNDINT ----

    #[test]
    fn x87_soft_int_round_trips() {
        for &v in &[0i16, 1, -1, 32767, -32768, 100, -100] {
            let (back, invalid) = F80::from_i16(v).to_i16(CW_DEFAULT);
            assert!(!invalid && back == v, "i16 round-trip failed for {v}");
        }
        for &v in &[0i32, 1, -1, i32::MAX, i32::MIN, 123_456, -654_321] {
            let (back, invalid) = F80::from_i32(v).to_i32(CW_DEFAULT);
            assert!(!invalid && back == v, "i32 round-trip failed for {v}");
        }
        for &v in &[0i64, 1, -1, i64::MAX, i64::MIN, 123_456_789_012_345] {
            let (back, invalid) = F80::from_i64(v).to_i64(CW_DEFAULT);
            assert!(!invalid && back == v, "i64 round-trip failed for {v}");
        }
    }

    #[test]
    fn x87_soft_to_i32_rounding_modes() {
        let val = F80::from_f64(2.5);
        assert_eq!(val.to_i32(CW_DEFAULT).0, 2); // nearest-even
        assert_eq!(val.to_i32(CW_TRUNCATE).0, 2); // toward zero
        assert_eq!(val.to_i32(CW_ROUND_DOWN).0, 2); // toward -inf
        assert_eq!(val.to_i32(CW_ROUND_UP).0, 3); // toward +inf
                                                  // truncation drops the fraction.
        assert_eq!(F80::from_f64(-2.9).to_i32_truncate().0, -2);
    }

    #[test]
    fn x87_soft_rndint_integer_identity() {
        for &v in &[0.0, 1.0, -1.0, 42.0, -100.0, 1e15] {
            let a = F80::from_f64(v);
            assert_eq!(
                f80_rndint(&a, CW_DEFAULT),
                a,
                "FRNDINT not identity for {v}"
            );
        }
        // 2.5 rounds to even (2) under nearest, up to 3 under round-up.
        assert_eq!(f80_rndint(&F80::from_f64(2.5), CW_DEFAULT).to_f64(), 2.0);
        assert_eq!(f80_rndint(&F80::from_f64(2.5), CW_ROUND_UP).to_f64(), 3.0);
    }

    // ---- comparison trichotomy ----

    #[test]
    fn x87_soft_compare_trichotomy() {
        let one = F80::from_f64(1.0);
        let two = F80::from_f64(2.0);
        // 1 < 2 -> C0 set
        assert_eq!(f80_compare(&one, &two), 0x0100);
        // 2 > 1 -> all clear
        assert_eq!(f80_compare(&two, &one), 0x0000);
        // 1 == 1 -> C3 set
        assert_eq!(f80_compare(&one, &one), 0x4000);
        // +0 == -0
        assert_eq!(f80_compare(&F80::ZERO, &F80::NEG_ZERO), 0x4000);
        // unordered (NaN) -> C3|C2|C0
        let nan = F80::from_f64(f64::NAN);
        assert_eq!(f80_compare(&nan, &one), 0x4500);
        assert_eq!(f80_compare(&one, &nan), 0x4500);
        assert_eq!(f80_compare_and_pop(&one, &two), f80_compare(&one, &two));
    }

    // ---- BCD round-trip ----

    #[test]
    fn x87_soft_bcd_round_trip() {
        for &v in &[0i64, 1, -1, 42, -42, 999_999, -1_000_000, 123_456_789_012] {
            let bcd = F80::from_i64(v).to_bcd(CW_DEFAULT);
            let back = F80::from_bcd(&bcd);
            let (iv, invalid) = back.to_i64(CW_DEFAULT);
            assert!(!invalid && iv == v, "BCD round-trip failed for {v}");
        }
    }

    // ---- xtract / scale ----

    #[test]
    fn x87_soft_scale_extended_exponent_truncation() {
        // Raw extended neighbours retain the low bits that binary64 loses.
        // Each tuple is (exponent, significand, truncated positive magnitude).
        let neighbours = [
            (0x3ffe, u64::MAX, 0), // 1 - 2^-64
            (0x3fff, 0x8000_0000_0000_0000, 1),
            (0x3fff, 0x8000_0000_0000_0001, 1),
            (0x3fff, u64::MAX, 1), // 2 - 2^-63
            (0x4000, 0x8000_0000_0000_0000, 2),
            (0x4000, 0x8000_0000_0000_0001, 2),
        ];
        for rc in 0..4 {
            let cw = CW_DEFAULT | (rc << 10);
            for negative in [false, true] {
                for (exp, sig, magnitude) in neighbours {
                    let b = extended(negative, exp, sig);
                    let n = if negative { -magnitude } else { magnitude };
                    let expected = extended(false, (0x3fff + n) as u16, 1 << 63);
                    assert_eq!(
                        f80_scale(&F80::ONE, &b, cw).0,
                        expected.0,
                        "FSCALE must truncate extended ST1 before narrowing: b={b:?}, CW={cw:04x}"
                    );
                }
            }

            // Both signs of huge finite exponents saturate the integer input;
            // scalbn must still honour directed rounding of its result.
            for exp in [0x401e, 0x7ffe] {
                for negative_a in [false, true] {
                    let a = extended(negative_a, 0x3fff, 1 << 63);
                    let overflow_to_inf =
                        rc == 0 || (rc == 1 && negative_a) || (rc == 2 && !negative_a);
                    let overflow = if overflow_to_inf {
                        extended(negative_a, 0x7fff, 1 << 63)
                    } else {
                        extended(negative_a, 0x7ffe, u64::MAX)
                    };
                    let away_from_zero = (rc == 1 && negative_a) || (rc == 2 && !negative_a);
                    let underflow = extended(negative_a, 0, u64::from(away_from_zero));
                    assert_eq!(
                        f80_scale(&a, &extended(false, exp, 1 << 63), cw).0,
                        overflow.0
                    );
                    assert_eq!(
                        f80_scale(&a, &extended(true, exp, 1 << 63), cw).0,
                        underflow.0
                    );
                }
            }

            let inf = extended(false, 0x7fff, 1 << 63);
            let neg_inf = extended(true, 0x7fff, 1 << 63);
            let snan = extended(false, 0x7fff, (1 << 63) | 1);
            let qnan = extended(false, 0x7fff, (3 << 62) | 1);
            let unsupported = extended(false, 0x3fff, 1);
            for (a, b, expected) in [
                (F80::ONE, F80::ZERO, F80::ONE),
                (F80::ONE, F80::NEG_ZERO, F80::ONE),
                (F80::ONE, extended(false, 0, 1), F80::ONE),
                (F80::ONE, extended(true, 0, 1), F80::ONE),
                (F80::NEG_ZERO, F80::ONE, F80::NEG_ZERO),
                (inf, F80::ONE, inf),
                (F80::ONE, inf, inf),
                (F80::from_i16(-1), neg_inf, F80::NEG_ZERO),
                (F80::ZERO, inf, F80::INDEFINITE),
                (inf, neg_inf, F80::INDEFINITE),
                (F80::ONE, snan, qnan),
                (snan, F80::ONE, qnan),
                (F80::ONE, unsupported, F80::INDEFINITE),
                (unsupported, F80::ONE, F80::INDEFINITE),
            ] {
                assert_eq!(
                    f80_scale(&a, &b, cw).0,
                    expected.0,
                    "a={a:?}, b={b:?}, CW={cw:04x}"
                );
            }
        }
    }

    #[test]
    fn x87_soft_xtract_scale() {
        // 12.0 = 1.5 * 2^3
        let (sig, exp) = f80_xtract(&F80::from_f64(12.0), CW_DEFAULT);
        assert!((sig.to_f64() - 1.5).abs() < 1e-15);
        assert_eq!(exp.to_f64(), 3.0);
        // FSCALE re-composes: sig * 2^exp == 12.
        let recomposed = f80_scale(&sig, &exp, CW_DEFAULT);
        assert!((recomposed.to_f64() - 12.0).abs() < 1e-12);
    }

    // ---- transcendentals (approximate) ----

    #[test]
    fn x87_soft_transcendentals_approx() {
        let (s, c2) = f80_sin(&F80::from_f64(std::f64::consts::FRAC_PI_2), CW_DEFAULT);
        assert!(!c2 && (s.to_f64() - 1.0).abs() < 1e-12);
        let (c, _) = f80_cos(&F80::from_f64(0.0), CW_DEFAULT);
        assert!((c.to_f64() - 1.0).abs() < 1e-12);
        // y * log2(x): 3 * log2(2) = 3.
        let r = f80_yl2x(&F80::from_f64(3.0), &F80::from_f64(2.0), CW_DEFAULT);
        assert!((r.to_f64() - 3.0).abs() < 1e-12);
        // 2^0.5 - 1.
        let r = f80_f2xm1(&F80::from_f64(0.5), CW_DEFAULT);
        assert!((r.to_f64() - (std::f64::consts::SQRT_2 - 1.0)).abs() < 1e-12);
        // out-of-range sin sets C2 and leaves the operand unchanged.
        let big = F80::from_f64(1e30);
        let (unchanged, c2) = f80_sin(&big, CW_DEFAULT);
        assert!(c2 && unchanged == big);
    }

    // ---- prem ----

    #[test]
    fn x87_soft_prem() {
        // 7 mod 4 = 3, reduction complete (C2 clear).
        let (r, sw) = f80_prem(&F80::from_f64(7.0), &F80::from_f64(4.0), CW_DEFAULT);
        assert!((r.to_f64() - 3.0).abs() < 1e-12);
        assert_eq!(sw & (1 << 10), 0, "C2 should be clear (complete)");
        // FPREM1 IEEE remainder of 5 / 2 = 1.
        let (r1, _) = f80_prem1(&F80::from_f64(5.0), &F80::from_f64(2.0), CW_DEFAULT);
        assert!((r1.to_f64() - 1.0).abs() < 1e-12);
    }

    /// MDD-BUG-TEM-00177: each quotient bit reaches its own condition code.
    ///
    /// This test asserted only that C2 was clear, and its caller's fixture
    /// set all three quotient bits at once, so transposing Q0->C1 with
    /// Q2->C0 passed the whole crate. A guest uses C0/C3/C1 as an octant
    /// index when reducing an angle before FSIN/FCOS, so a transposition
    /// picks the wrong octant with no fault raised.
    #[test]
    fn x87_soft_prem_maps_each_quotient_bit_to_its_own_condition_code() {
        for (dividend, quotient, c1, c3, c0) in [
            (1.5_f64, 1, 1 << 9, 0, 0),
            (2.5, 2, 0, 1 << 14, 0),
            (4.5, 4, 0, 0, 1 << 8),
        ] {
            let (_, sw) = f80_prem(&F80::from_f64(dividend), &F80::ONE, CW_DEFAULT);
            assert_eq!(sw & (1 << 10), 0, "q={quotient}: reduction complete");
            assert_eq!(sw & (1 << 9), c1, "q={quotient}: C1 is Q0");
            assert_eq!(sw & (1 << 14), c3, "q={quotient}: C3 is Q1");
            assert_eq!(sw & (1 << 8), c0, "q={quotient}: C0 is Q2");
        }
    }

    // ---- Default / Debug / PartialEq ----

    #[test]
    fn x87_soft_traits() {
        assert_eq!(F80::default(), F80::ZERO);
        assert_eq!(F80::ONE, F80::ONE);
        assert_ne!(F80::ONE, F80::ZERO);
        assert_eq!(format!("{:?}", F80::ONE), "F80(1)");
    }

    // ============================================================================
    // Host-float fast-path differential oracle
    // ============================================================================
    //
    // The permanent guard for the fast path added above (see "Host-float fast
    // path" near `fast_add` et al.): for every op, across a boundary-focused
    // corpus and a deterministic random sweep, `_impl(..., true)` (fast path
    // allowed) and `_impl(..., false)` (pure apfloat path forced) must return
    // IDENTICAL results — value bits and status bits alike. `FAST_PATH_HITS`
    // proves the fast path actually engages rather than vacuously always
    // falling back.
    //
    // Note on "sticky PE preset vs clear": `f80_*_status` computes only the
    // exception bits for THIS operation — the caller (fpu.rs) OR's them into
    // the live sticky status word — so there is no sticky-status input for
    // these functions to vary. That test dimension is therefore exercised
    // vacuously here (both "presets" are the same call), not skipped.

    fn call_arith_impl(
        op: ArithmeticOp,
        a: &F80,
        b: &F80,
        cw: u16,
        allow_fast: bool,
    ) -> (F80, u16) {
        match op {
            ArithmeticOp::Add => f80_add_status_impl(a, b, cw, allow_fast),
            ArithmeticOp::Sub => f80_sub_status_impl(a, b, cw, allow_fast),
            ArithmeticOp::Mul => f80_mul_status_impl(a, b, cw, allow_fast),
            ArithmeticOp::Div => f80_div_status_impl(a, b, cw, allow_fast),
        }
    }

    fn fast_path_hits() -> u64 {
        crate::x87_fast_core::fast_path_hits()
    }

    fn reset_fast_path_hits() {
        crate::x87_fast_core::reset_fast_path_hits()
    }

    /// splitmix64: a minimal deterministic PRNG for the random operand
    /// sweep. No external dependency and no wall-clock/OS entropy, so the
    /// sweep is bit-for-bit reproducible across hosts and CI runs.
    struct Splitmix64(u64);

    impl Splitmix64 {
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
    }

    /// Draw an operand for the random fast-path sweep. Mostly plain f64 bit
    /// patterns reinterpreted through `from_f64` (the common case the fast
    /// path targets, plus a healthy incidental share of NaN/Inf since random
    /// bits often land in that exponent range), plus explicit f80-denormal
    /// and raw-NaN draws so fallback gets deliberate coverage too.
    fn random_f80(rng: &mut Splitmix64) -> F80 {
        match rng.next_u64() % 8 {
            0 => extended(rng.next_u64() & 1 == 0, 0, rng.next_u64() | 1), // f80 denormal
            1 => extended(
                rng.next_u64() & 1 == 0,
                0x7fff,
                (rng.next_u64() | 1) & 0x7fff_ffff_ffff_ffff,
            ), // raw NaN
            _ => F80::from_f64(f64::from_bits(rng.next_u64())),
        }
    }

    /// Small corpus for the exhaustive PC x RC x op cross product (same scale
    /// as `x87_soft_lazy_arithmetic_matches_eager_reference` above).
    fn fast_path_small_corpus() -> Vec<F80> {
        vec![
            F80::ZERO,
            F80::NEG_ZERO,
            F80::ONE,
            F80::from_f64(-1.0),
            F80::from_f64(2.0),
            F80::from_f64(0.5),
            F80::from_f64(3.0 * 2f64.powi(-24)), // PC=24 half-way-rounding operand
            F80::from_f64(-3.0 * 2f64.powi(-24)),
            F80::from_f32(1.0 + f32::EPSILON),
            F80::from_f64(1.0 + f64::EPSILON),
            F80::from_f64(f64::MAX),
            F80::from_f64(f64::MIN_POSITIVE),
            F80::from_f64(f64::NAN),
            F80::from_f64(f64::INFINITY),
            F80::from_f64(f64::NEG_INFINITY),
            extended(false, 0, 1),                          // f80 denormal
            extended(false, 0x3fff, 0x8000_0000_0000_0001), // needs the full 64-bit significand
            extended(false, 0x7fff, 0x8000_0000_0000_0001), // unnormal/pseudo (NaN by is_nan)
        ]
    }

    /// Wider boundary/exactness corpus: exact powers of two spanning from
    /// well below binary64's normal floor to above its normal ceiling — each
    /// built as a raw F80 bit pattern (sidestepping any `f64::powi` rounding
    /// at extreme exponents) — plus f32/f64/f80 extremes and a few
    /// exceptional forms. Exercised only at the fast path's own PC/RC (PC in
    /// {24, 53}, RC=nearest): directed rounding and PC=64 fall back
    /// unconditionally regardless of corpus, already proven equal by the
    /// small-corpus full cross above, so repeating that here on a much
    /// larger corpus would add cost with no new information.
    fn fast_path_boundary_corpus() -> Vec<F80> {
        let mut v = vec![
            F80::from_f64(4.0),
            F80::from_f64(9.0),
            F80::from_f64(0.25),
            F80::from_f32(f32::MIN_POSITIVE),
            F80::from_f32(f32::MAX),
            F80::from_i32(i32::MAX),
            F80::from_i32(i32::MIN),
            F80::from_i64(i64::MAX),
            F80::from_i64(i64::MIN),
            F80::INDEFINITE,
            extended(false, 0x7ffe, 0xffff_ffff_ffff_ffff), // f80 near-max normal
            extended(true, 1, 0x8000_0000_0000_0000),       // f80 min-exponent normal
        ];
        for e in [
            -16000i32, -1075, -1074, -1023, -1022, -150, -149, -128, -127, -24, -23, 23, 24, 127,
            128, 149, 150, 1022, 1023, 1024, 16000,
        ] {
            let biased = (16_383 + e) as u16;
            v.push(extended(false, biased, 0x8000_0000_0000_0000));
            v.push(extended(true, biased, 0x8000_0000_0000_0000));
        }
        v
    }

    #[test]
    fn x87_soft_fast_path_matches_pure_path_arithmetic_full_cross() {
        reset_fast_path_hits();
        let corpus = fast_path_small_corpus();
        let mut compared = 0u64;

        for op in [
            ArithmeticOp::Add,
            ArithmeticOp::Sub,
            ArithmeticOp::Mul,
            ArithmeticOp::Div,
        ] {
            for pc in 0..4u16 {
                for rc in 0..4u16 {
                    let cw = (pc << 8) | (rc << 10);
                    for a in &corpus {
                        for b in &corpus {
                            let fast = call_arith_impl(op, a, b, cw, true);
                            let pure = call_arith_impl(op, a, b, cw, false);
                            assert_eq!(
                                fast, pure,
                                "{op:?} fast/pure mismatch a={a:?} b={b:?} cw={cw:#06x}"
                            );
                            compared += 1;
                        }
                    }
                }
            }
        }

        assert!(
            fast_path_hits() > 0,
            "fast path never engaged across {compared} full-cross comparisons"
        );
    }

    #[test]
    fn x87_soft_fast_path_matches_pure_path_arithmetic_boundary() {
        reset_fast_path_hits();
        let corpus = fast_path_boundary_corpus();
        let mut compared = 0u64;

        for op in [
            ArithmeticOp::Add,
            ArithmeticOp::Sub,
            ArithmeticOp::Mul,
            ArithmeticOp::Div,
        ] {
            for pc in [0u16, 2u16] {
                let cw = pc << 8;
                for a in &corpus {
                    for b in &corpus {
                        let fast = call_arith_impl(op, a, b, cw, true);
                        let pure = call_arith_impl(op, a, b, cw, false);
                        assert_eq!(
                            fast, pure,
                            "{op:?} fast/pure mismatch a={a:?} b={b:?} cw={cw:#06x}"
                        );
                        compared += 1;
                    }
                }
            }
        }

        assert!(
            fast_path_hits() > 0,
            "fast path never engaged across {compared} boundary comparisons"
        );
    }

    #[test]
    fn x87_soft_fast_path_random_sweep() {
        reset_fast_path_hits();
        let mut rng = Splitmix64(0xC0FF_EE15_1DEA_D00D);
        let mut compared = 0u64;

        for _ in 0..4000 {
            let a = random_f80(&mut rng);
            let b = random_f80(&mut rng);
            for op in [
                ArithmeticOp::Add,
                ArithmeticOp::Sub,
                ArithmeticOp::Mul,
                ArithmeticOp::Div,
            ] {
                for pc in [0u16, 2u16] {
                    for rc in [0u16, 1, 2, 3] {
                        let cw = (pc << 8) | (rc << 10);
                        let fast = call_arith_impl(op, &a, &b, cw, true);
                        let pure = call_arith_impl(op, &a, &b, cw, false);
                        assert_eq!(
                            fast, pure,
                            "{op:?} fast/pure mismatch a={a:?} b={b:?} cw={cw:#06x}"
                        );
                        compared += 1;
                    }
                }
            }
        }

        assert!(
            fast_path_hits() > 0,
            "fast path never engaged across {compared} random comparisons"
        );
    }

    #[test]
    fn x87_soft_fast_path_matches_pure_path_sqrt() {
        reset_fast_path_hits();
        let mut corpus = fast_path_small_corpus();
        corpus.extend(fast_path_boundary_corpus());
        let mut compared = 0u64;

        for cw in [CW_DEFAULT, CW_TRUNCATE, CW_ROUND_DOWN, CW_ROUND_UP] {
            for a in &corpus {
                let fast = f80_sqrt_impl(a, cw, true);
                let pure = f80_sqrt_impl(a, cw, false);
                assert_eq!(fast, pure, "sqrt fast/pure mismatch a={a:?} cw={cw:#06x}");
                compared += 1;
            }
        }

        assert!(
            fast_path_hits() > 0,
            "sqrt fast path never engaged across {compared} cases"
        );
    }

    #[test]
    fn x87_soft_fast_path_matches_pure_path_compare() {
        reset_fast_path_hits();
        let mut corpus = fast_path_small_corpus();
        corpus.extend(fast_path_boundary_corpus());
        let mut compared = 0u64;

        for a in &corpus {
            for b in &corpus {
                let fast = f80_compare_impl(a, b, true);
                let pure = f80_compare_impl(a, b, false);
                assert_eq!(fast, pure, "compare fast/pure mismatch a={a:?} b={b:?}");
                compared += 1;
            }
        }

        assert!(
            fast_path_hits() > 0,
            "compare fast path never engaged across {compared} cases"
        );
    }
}
