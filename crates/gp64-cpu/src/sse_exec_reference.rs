//! Frozen migration oracle: the iced-x86-driven SSE/SSE2 executor.
//!
//! This is the executor `sse_exec` shipped before the iced-free port, moved
//! here verbatim (only its imports changed) and compiled **only under
//! `#[cfg(test)]`**. It exists so the old-vs-new differential in
//! `sse_exec`'s tests can prove the port behaviour-preserving; nothing in
//! production reaches it, and the source-scan guard exempts this file for
//! exactly that reason.
//!
//! Do not fix bugs here. A divergence the differential finds is adjudicated
//! against the new executor, per the HLD's D7.

use crate::addressing::{
    calc_effective_address, read_operand_bytes, read_register, read_rmw_operand_bytes,
    write_operand_bytes,
};
use crate::sse::SseRounding;
use crate::sse_exec::SseLevel;
use crate::{Cpu, CpuError, PageAccess, PagedMemory, Result};
use iced_x86::{CpuidFeature, Decoder, DecoderOptions, Instruction, Mnemonic, OpKind, Register};
use ieee_apsqrt::sqrt_accurate;
use rustc_apfloat::ieee::{Double, Single};
use rustc_apfloat::{Float, FloatConvert, Round, Status};

// =========================================================================
// Frozen value helpers
// =========================================================================
//
// Copied verbatim from the pre-port `sse_exec` at the moment the oracle was
// frozen. They live here, private, so the differential compares two
// independent implementations: the production helpers may be rewritten (lane
// arrays, host intrinsics) and this snapshot stays the fixed reference.
// Do not edit them to follow a production change — that would silently turn
// the oracle into a mirror.

#[inline]
fn from_bytes(bytes: &[u8]) -> u128 {
    let mut full = [0u8; 16];
    full[..bytes.len()].copy_from_slice(bytes);
    u128::from_le_bytes(full)
}

#[inline]
fn to_bytes(value: u128) -> [u8; 16] {
    value.to_le_bytes()
}

#[inline]
fn mask(bytes: usize) -> u128 {
    if bytes == 16 {
        u128::MAX
    } else {
        (1u128 << (bytes * 8)) - 1
    }
}

#[derive(Clone, Copy)]
enum FloatOperation {
    Add,
    Subtract,
    Multiply,
    Divide,
}

#[inline]
fn apfloat_round(rounding: SseRounding) -> Round {
    match rounding {
        SseRounding::Nearest => Round::NearestTiesToEven,
        SseRounding::Down => Round::TowardNegative,
        SseRounding::Up => Round::TowardPositive,
        SseRounding::TowardZero => Round::TowardZero,
    }
}

#[inline]
fn is_denormal32(bits: u32) -> bool {
    bits & 0x7f80_0000 == 0 && bits & 0x007f_ffff != 0
}

#[inline]
fn is_denormal64(bits: u64) -> bool {
    bits & 0x7ff0_0000_0000_0000 == 0 && bits & 0x000f_ffff_ffff_ffff != 0
}

#[inline]
fn daz32(bits: u32, enabled: bool) -> (u32, bool) {
    let denormal = is_denormal32(bits);
    if enabled && denormal {
        (bits & 0x8000_0000, false)
    } else {
        (bits, denormal)
    }
}

#[inline]
fn daz64(bits: u64, enabled: bool) -> (u64, bool) {
    let denormal = is_denormal64(bits);
    if enabled && denormal {
        (bits & 0x8000_0000_0000_0000, false)
    } else {
        (bits, denormal)
    }
}

#[inline]
fn ftz32(bits: u32, status: Status, enabled: bool) -> u32 {
    if enabled && status.contains(Status::UNDERFLOW) && is_denormal32(bits) {
        bits & 0x8000_0000
    } else {
        bits
    }
}

#[inline]
fn ftz64(bits: u64, status: Status, enabled: bool) -> u64 {
    if enabled && status.contains(Status::UNDERFLOW) && is_denormal64(bits) {
        bits & 0x8000_0000_0000_0000
    } else {
        bits
    }
}

#[inline]
fn is_nan32(bits: u32) -> bool {
    bits & 0x7f80_0000 == 0x7f80_0000 && bits & 0x007f_ffff != 0
}

#[inline]
fn is_signaling_nan32(bits: u32) -> bool {
    is_nan32(bits) && bits & 0x0040_0000 == 0
}

#[inline]
fn is_nan64(bits: u64) -> bool {
    bits & 0x7ff0_0000_0000_0000 == 0x7ff0_0000_0000_0000 && bits & 0x000f_ffff_ffff_ffff != 0
}

#[inline]
fn is_signaling_nan64(bits: u64) -> bool {
    is_nan64(bits) && bits & 0x0008_0000_0000_0000 == 0
}

/// Whether a lane's result is settled before a denormal operand matters.
///
/// Hardware raises Denormal for an operand the arithmetic actually USES. When
/// the operation short-circuits - a NaN operand decides the result outright,
/// or a finite non-zero over zero raises Zero-Divide - the denormal never
/// reaches the arithmetic and no Denormal is raised. Deriving DE from operand
/// inspection alone gave a spurious sticky DE, and a spurious #XM whenever DM
/// was unmasked (MDD-BUG-CRUCIBLE-00786).
///
/// Measured on this box's silicon rather than assumed. Suppressed: `den +
/// QNaN`, `QNaN + den`, `den + SNaN` (IE alone), `den / 0` (ZE alone) and
/// `den / QNaN`. NOT suppressed, and in the regression corpus precisely
/// because a coarser rule would break them: `den + inf`, `den + 0`,
/// `den * one`, `0 / den` and `inf / den`.
///
/// Scope: the ARITHMETIC forms only. MIN/MAX and the compares derive their
/// denormal flag in their own loops and their short-circuit behaviour was not
/// measured here, so they are deliberately left alone.
fn lane_short_circuits32(left: u32, right: u32, operation: FloatOperation) -> bool {
    if is_nan32(left) || is_nan32(right) {
        return true;
    }
    // A denormal operand is finite and non-zero, so a zero DIVISOR alongside
    // one is always the divide-by-zero case, never 0/0 or inf/0.
    matches!(operation, FloatOperation::Divide) && right & 0x7fff_ffff == 0
}

/// The double-precision twin of [`lane_short_circuits32`].
fn lane_short_circuits64(left: u64, right: u64, operation: FloatOperation) -> bool {
    if is_nan64(left) || is_nan64(right) {
        return true;
    }
    matches!(operation, FloatOperation::Divide) && right & 0x7fff_ffff_ffff_ffff == 0
}

/// Flush-to-zero as the architecture defines it: ANY tiny result, not only a
/// tiny one that was also inexact.
///
/// `ftz32`/`ftz64` gate on apfloat's `UNDERFLOW`, which means tiny AND
/// inexact, so a tiny-but-EXACT result kept its denormal and raised nothing -
/// where hardware delivers a signed zero with UE|PE. Measured on this box:
/// `ADDSS 0x00800001, 0x80800000` and `DIVSS 0.25, 2^126` both have exactly
/// representable tiny results, and silicon flushes both. The tiny-and-inexact
/// cases already agreed and must keep agreeing, which is why the regression
/// carries both halves (MDD-BUG-CRUCIBLE-00787).
///
/// The bool says a flush happened, so the caller can raise the UE|PE that
/// came from the flush rather than from the arithmetic.
///
/// Scope: the ARITHMETIC lanes. The square-root and convert paths still use
/// the older gate above - a square root can never deliver a tiny result from
/// a tiny operand, and the convert case was not measured here.
#[inline]
fn flush_tiny32(bits: u32, enabled: bool) -> (u32, bool) {
    if enabled && is_denormal32(bits) {
        (bits & 0x8000_0000, true)
    } else {
        (bits, false)
    }
}

/// The double-precision twin of [`flush_tiny32`].
#[inline]
fn flush_tiny64(bits: u64, enabled: bool) -> (u64, bool) {
    if enabled && is_denormal64(bits) {
        (bits & 0x8000_0000_0000_0000, true)
    } else {
        (bits, false)
    }
}

fn apfloat32(
    left: u128,
    right: u128,
    lanes: usize,
    operation: FloatOperation,
    rounding: SseRounding,
    daz: bool,
    ftz: bool,
) -> (u128, Status, bool) {
    let mut output = left;
    let mut status = Status::OK;
    let mut denormal = false;
    for lane in 0..lanes {
        let (left_bits, left_denormal) = daz32(lane32(left, lane), daz);
        let (right_bits, right_denormal) = daz32(lane32(right, lane), daz);
        denormal |= (left_denormal || right_denormal)
            && !lane_short_circuits32(left_bits, right_bits, operation);
        let left_value = Single::from_bits(u128::from(left_bits));
        let right_value = Single::from_bits(u128::from(right_bits));
        let result = match operation {
            FloatOperation::Add => left_value.add_r(right_value, apfloat_round(rounding)),
            FloatOperation::Subtract => left_value.sub_r(right_value, apfloat_round(rounding)),
            FloatOperation::Multiply => left_value.mul_r(right_value, apfloat_round(rounding)),
            FloatOperation::Divide => left_value.div_r(right_value, apfloat_round(rounding)),
        };
        status |= result.status;
        // A flush IS an underflow-and-inexact event, so it contributes those
        // flags itself: apfloat reports neither for a tiny result that was
        // exact (MDD-BUG-CRUCIBLE-00787).
        let (bits, flushed) = flush_tiny32(result.value.to_bits() as u32, ftz);
        if flushed {
            status |= Status::UNDERFLOW | Status::INEXACT;
        }
        put32(&mut output, lane, bits);
    }
    (output, status, denormal)
}

fn apfloat64(
    left: u128,
    right: u128,
    lanes: usize,
    operation: FloatOperation,
    rounding: SseRounding,
    daz: bool,
    ftz: bool,
) -> (u128, Status, bool) {
    let mut output = left;
    let mut status = Status::OK;
    let mut denormal = false;
    for lane in 0..lanes {
        let (left_bits, left_denormal) = daz64(lane64(left, lane), daz);
        let (right_bits, right_denormal) = daz64(lane64(right, lane), daz);
        denormal |= (left_denormal || right_denormal)
            && !lane_short_circuits64(left_bits, right_bits, operation);
        let left_value = Double::from_bits(u128::from(left_bits));
        let right_value = Double::from_bits(u128::from(right_bits));
        let result = match operation {
            FloatOperation::Add => left_value.add_r(right_value, apfloat_round(rounding)),
            FloatOperation::Subtract => left_value.sub_r(right_value, apfloat_round(rounding)),
            FloatOperation::Multiply => left_value.mul_r(right_value, apfloat_round(rounding)),
            FloatOperation::Divide => left_value.div_r(right_value, apfloat_round(rounding)),
        };
        status |= result.status;
        let (bits, flushed) = flush_tiny64(result.value.to_bits() as u64, ftz);
        if flushed {
            status |= Status::UNDERFLOW | Status::INEXACT;
        }
        put64(&mut output, lane, bits);
    }
    (output, status, denormal)
}

/// The Underflow bit an UNMASKED underflow adds beyond apfloat's masked rule.
///
/// The reasoning, the SDM citation and the known remaining gap all live on
/// `sse_exec::unmasked_underflow_bit`. This model must agree with that one —
/// `new_executor_matches_the_iced_executor` holds them together
/// (MDD-BUG-CRUCIBLE-00774).
fn unmasked_underflow_bit(output: u128, lanes: usize, double: bool, mxcsr: u32) -> u32 {
    const UNDERFLOW_FLAG: u32 = 1 << 4;
    const UNDERFLOW_MASK: u32 = 1 << 11;
    if mxcsr & UNDERFLOW_MASK != 0 {
        return 0;
    }
    let tiny = (0..lanes).any(|lane| {
        if double {
            is_denormal64(lane64(output, lane))
        } else {
            is_denormal32(lane32(output, lane))
        }
    });
    if tiny {
        UNDERFLOW_FLAG
    } else {
        0
    }
}

fn publish_fp_status(cpu: &mut Cpu, status: Status, denormal: bool) -> Result<()> {
    publish_fp_status_with(cpu, status, denormal, 0)
}

/// [`publish_fp_status`] plus flag bits the caller derived from the result
/// itself, which `Status` cannot carry (MDD-BUG-CRUCIBLE-00774).
fn publish_fp_status_with(
    cpu: &mut Cpu,
    status: Status,
    denormal: bool,
    extra_flags: u32,
) -> Result<()> {
    let mut flags = extra_flags;
    if status.contains(Status::INVALID_OP) {
        flags |= 1 << 0;
    }
    if denormal {
        flags |= 1 << 1;
    }
    if status.contains(Status::DIV_BY_ZERO) {
        flags |= 1 << 2;
    }
    if status.contains(Status::OVERFLOW) {
        flags |= 1 << 3;
    }
    if status.contains(Status::UNDERFLOW) {
        flags |= 1 << 4;
    }
    if status.contains(Status::INEXACT) {
        flags |= 1 << 5;
    }
    cpu.sse.mxcsr |= flags;
    let masks = (cpu.sse.mxcsr >> 7) & 0x3f;
    if flags & !masks != 0 {
        if cpu.regs.cr4 & crate::CR4_OSXMMEXCPT != 0 {
            Err(CpuError::Interrupt(19))
        } else {
            Err(CpuError::Interrupt(6))
        }
    } else {
        Ok(())
    }
}

fn single_to_i32(
    bits: u32,
    truncating: bool,
    rounding: SseRounding,
    daz: bool,
) -> (i32, Status, bool) {
    let (bits, _) = daz32(bits, daz);
    let value = Single::from_bits(u128::from(bits));
    let mut exact = true;
    let result = value.to_i128_r(
        32,
        if truncating {
            Round::TowardZero
        } else {
            apfloat_round(rounding)
        },
        &mut exact,
    );
    let output = if result.status.contains(Status::INVALID_OP) {
        i32::MIN
    } else {
        result.value as i32
    };
    // Float-to-int32 conversions never report Denormal: silicon (Intel
    // i7-14700KF and Core Ultra 7 270K Plus, AMD Ryzen 9 9950X3D) leaves DE
    // clear for a denormal source in every CVT*2SI/2PI/2DQ form, and the SDM
    // lists only Invalid and Precision (MDD-BUG-FLU-02382).
    (output, result.status, false)
}

fn double_to_i32(
    bits: u64,
    truncating: bool,
    rounding: SseRounding,
    daz: bool,
) -> (i32, Status, bool) {
    let (bits, _) = daz64(bits, daz);
    let value = Double::from_bits(u128::from(bits));
    let mut exact = true;
    let result = value.to_i128_r(
        32,
        if truncating {
            Round::TowardZero
        } else {
            apfloat_round(rounding)
        },
        &mut exact,
    );
    let output = if result.status.contains(Status::INVALID_OP) {
        i32::MIN
    } else {
        result.value as i32
    };
    // Float-to-int32 conversions never report Denormal: silicon (Intel
    // i7-14700KF and Core Ultra 7 270K Plus, AMD Ryzen 9 9950X3D) leaves DE
    // clear for a denormal source in every CVT*2SI/2PI/2DQ form, and the SDM
    // lists only Invalid and Precision (MDD-BUG-FLU-02382).
    (output, result.status, false)
}

#[inline]
fn lane32(x: u128, i: usize) -> u32 {
    (x >> (i * 32)) as u32
}
#[inline]
fn lane64(x: u128, i: usize) -> u64 {
    (x >> (i * 64)) as u64
}
#[inline]
fn put32(x: &mut u128, i: usize, v: u32) {
    *x = (*x & !(0xffff_ffffu128 << (i * 32))) | ((v as u128) << (i * 32));
}
#[inline]
fn put64(x: &mut u128, i: usize, v: u64) {
    *x = (*x & !(0xffff_ffff_ffff_ffffu128 << (i * 64))) | ((v as u128) << (i * 64));
}

fn fp32(a: u128, b: u128, lanes: usize, op: fn(f32, f32) -> f32) -> u128 {
    let mut out = a;
    for i in 0..lanes {
        put32(
            &mut out,
            i,
            op(f32::from_bits(lane32(a, i)), f32::from_bits(lane32(b, i))).to_bits(),
        );
    }
    out
}

fn cmp32(a: f32, b: f32, imm: u8) -> bool {
    use core::cmp::Ordering::{Equal, Less};
    match imm & 7 {
        0 => a == b,
        1 => a < b,
        2 => a <= b,
        3 => a.is_nan() || b.is_nan(),
        4 => a != b,
        5 => a.partial_cmp(&b) != Some(Less),
        6 => !matches!(a.partial_cmp(&b), Some(Less | Equal)),
        _ => !(a.is_nan() || b.is_nan()),
    }
}
fn cmp64(a: f64, b: f64, imm: u8) -> bool {
    use core::cmp::Ordering::{Equal, Less};
    match imm & 7 {
        0 => a == b,
        1 => a < b,
        2 => a <= b,
        3 => a.is_nan() || b.is_nan(),
        4 => a != b,
        5 => a.partial_cmp(&b) != Some(Less),
        6 => !matches!(a.partial_cmp(&b), Some(Less | Equal)),
        _ => !(a.is_nan() || b.is_nan()),
    }
}

fn packed_int(a: u128, b: u128, width: usize, signed_saturating: bool, subtract: bool) -> u128 {
    let mut out = 0;
    let n = 128 / width;
    for i in 0..n {
        let av = ((a >> (i * width)) & ((1u128 << width) - 1)) as i64;
        let bv = ((b >> (i * width)) & ((1u128 << width) - 1)) as i64;
        let v = if signed_saturating {
            let sign = 1i64 << (width - 1);
            let aa = (av ^ sign) - sign;
            let bb = (bv ^ sign) - sign;
            let raw = if subtract { aa - bb } else { aa + bb };
            raw.clamp(-sign, sign - 1) as u128
        } else if subtract {
            av.wrapping_sub(bv) as u128
        } else {
            av.wrapping_add(bv) as u128
        };
        out |= (v & ((1u128 << width) - 1)) << (i * width);
    }
    out
}
fn packed_unsigned_sat(a: u128, b: u128, width: usize, subtract: bool) -> u128 {
    let mut out = 0;
    let n = 128 / width;
    let max = (1u128 << width) - 1;
    for i in 0..n {
        let av = (a >> (i * width)) & max;
        let bv = (b >> (i * width)) & max;
        let v = if subtract {
            av.saturating_sub(bv)
        } else {
            av.saturating_add(bv).min(max)
        };
        out |= v << (i * width);
    }
    out
}

#[inline]
fn bit_mask(width: usize) -> u128 {
    if width == 128 {
        u128::MAX
    } else {
        (1u128 << width) - 1
    }
}

fn map_integer_lanes(
    a: u128,
    b: u128,
    width: usize,
    mut operation: impl FnMut(u128, u128) -> u128,
) -> u128 {
    let lane_mask = bit_mask(width);
    let mut output = 0u128;
    for lane in 0..128 / width {
        let shift = lane * width;
        output |=
            (operation((a >> shift) & lane_mask, (b >> shift) & lane_mask) & lane_mask) << shift;
    }
    output
}

#[inline]
fn signed_lane(value: u128, width: usize) -> i128 {
    ((value << (128 - width)) as i128) >> (128 - width)
}

fn compare_integer(a: u128, b: u128, width: usize, greater: bool) -> u128 {
    map_integer_lanes(a, b, width, |left, right| {
        let matches = if greater {
            signed_lane(left, width) > signed_lane(right, width)
        } else {
            left == right
        };
        if matches {
            bit_mask(width)
        } else {
            0
        }
    })
}

fn minmax_integer(a: u128, b: u128, width: usize, signed: bool, maximum: bool) -> u128 {
    map_integer_lanes(a, b, width, |left, right| {
        let take_left = if signed {
            if maximum {
                signed_lane(left, width) > signed_lane(right, width)
            } else {
                signed_lane(left, width) < signed_lane(right, width)
            }
        } else if maximum {
            left > right
        } else {
            left < right
        };
        if take_left {
            left
        } else {
            right
        }
    })
}

fn average_integer(a: u128, b: u128, width: usize) -> u128 {
    map_integer_lanes(a, b, width, |left, right| (left + right + 1) >> 1)
}

fn multiply_integer(a: u128, b: u128, width: usize, high: bool, unsigned: bool) -> u128 {
    map_integer_lanes(a, b, width, |left, right| {
        let product = if unsigned {
            (left * right) as i128
        } else {
            signed_lane(left, width) * signed_lane(right, width)
        };
        if high {
            (product >> width) as u128
        } else {
            product as u128
        }
    })
}

fn pack_signed(a: u128, b: u128, source_width: usize, unsigned: bool) -> u128 {
    let destination_width = source_width / 2;
    let lanes_per_source = 128 / source_width;
    let mut output = 0u128;
    for (source_index, source) in [a, b].into_iter().enumerate() {
        for lane in 0..lanes_per_source {
            let raw = (source >> (lane * source_width)) & bit_mask(source_width);
            let signed_value = signed_lane(raw, source_width);
            let packed = if unsigned {
                signed_value.clamp(0, bit_mask(destination_width) as i128) as u128
            } else {
                let bound = 1i128 << (destination_width - 1);
                signed_value.clamp(-bound, bound - 1) as u128
            };
            let destination_lane = source_index * lanes_per_source + lane;
            output |=
                (packed & bit_mask(destination_width)) << (destination_lane * destination_width);
        }
    }
    output
}

fn classify_restored_x87(bytes: &[u8]) -> u16 {
    let significand = u64::from_le_bytes(bytes[..8].try_into().expect("x87 significand"));
    let sign_exponent = u16::from_le_bytes(bytes[8..10].try_into().expect("x87 exponent"));
    let exponent = sign_exponent & 0x7fff;
    if exponent == 0 && significand == 0 {
        1
    } else if exponent != 0 && exponent != 0x7fff && significand >> 63 != 0 {
        0
    } else {
        2
    }
}
/// Decode one instruction with the same iced adapter the pre-port SSE decode
/// cache used, so the differential drives the reference from the exact bytes
/// the new decoder saw.
#[inline]
pub(crate) fn decode_uncached(bytes: &[u8], bitness: u8) -> Instruction {
    let mut decoder = Decoder::with_ip(u32::from(bitness), bytes, 0, DecoderOptions::NONE);
    decoder.decode()
}

/// Returns the extension advertised by iced-x86 for this instruction.
///
/// FXSAVE/FXRSTOR are FXSR instructions even though iced also models their
/// XMM-bearing variants as requiring SSE.  Checking them first keeps dispatch
/// independent from iced's representation of those encodings.
#[must_use]
pub fn level(instr: &Instruction) -> Option<SseLevel> {
    match instr.mnemonic() {
        Mnemonic::Fxsave | Mnemonic::Fxrstor => return Some(SseLevel::Fxsr),
        _ => {}
    }
    let features = instr.cpuid_features();
    if features.contains(&CpuidFeature::SSE2) {
        Some(SseLevel::Sse2)
    } else if features.contains(&CpuidFeature::SSE) {
        Some(SseLevel::Sse)
    } else if features.contains(&CpuidFeature::FXSR) {
        Some(SseLevel::Fxsr)
    } else {
        None
    }
}

#[inline]
fn unsupported(instr: &Instruction) -> CpuError {
    CpuError::Unimplemented(format!("SSE/SSE2 {}", instr))
}

fn xmm(reg: Register) -> Result<usize> {
    match reg {
        Register::XMM0 => Ok(0),
        Register::XMM1 => Ok(1),
        Register::XMM2 => Ok(2),
        Register::XMM3 => Ok(3),
        Register::XMM4 => Ok(4),
        Register::XMM5 => Ok(5),
        Register::XMM6 => Ok(6),
        Register::XMM7 => Ok(7),
        _ => Err(CpuError::Unimplemented(format!("non-XMM register {reg:?}"))),
    }
}

fn mmx(reg: Register) -> Result<usize> {
    match reg {
        Register::MM0 => Ok(0),
        Register::MM1 => Ok(1),
        Register::MM2 => Ok(2),
        Register::MM3 => Ok(3),
        Register::MM4 => Ok(4),
        Register::MM5 => Ok(5),
        Register::MM6 => Ok(6),
        Register::MM7 => Ok(7),
        _ => Err(CpuError::Unimplemented(format!("non-MMX register {reg:?}"))),
    }
}

#[inline]
fn is_mmx(reg: Register) -> bool {
    mmx(reg).is_ok()
}

fn source(
    cpu: &mut Cpu,
    instr: &Instruction,
    mem: &mut impl PagedMemory,
    bytes: usize,
) -> Result<u128> {
    if instr.op1_kind() == OpKind::Register {
        let reg = instr.op1_register();
        if is_mmx(reg) {
            Ok(u128::from(cpu.fpu.read_mmx_register_committed(mmx(reg)?)) & mask(bytes))
        } else {
            Ok(cpu.sse.xmm[xmm(reg)?] & mask(bytes))
        }
    } else {
        let mut buf = [0u8; 16];
        read_operand_bytes(cpu, instr, mem, &mut buf[..bytes])?;
        Ok(from_bytes(&buf[..bytes]))
    }
}

fn require_aligned<M: PagedMemory>(
    cpu: &Cpu,
    instr: &Instruction,
    mem: &mut M,
    alignment: usize,
) -> Result<()> {
    if instr.op0_kind() == OpKind::Memory || instr.op1_kind() == OpKind::Memory {
        let access = if instr.op0_kind() == OpKind::Memory {
            PageAccess::Write
        } else {
            PageAccess::Read
        };
        if calc_effective_address(cpu, instr, mem, access)? & (alignment - 1) != 0 {
            return Err(CpuError::GeneralProtectionFault);
        }
    }
    Ok(())
}

fn write_xmm_or_memory<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &Instruction,
    mem: &mut M,
    value: u128,
    bytes: usize,
) -> Result<()> {
    if instr.op0_kind() == OpKind::Register {
        let reg = instr.op0_register();
        if is_mmx(reg) {
            cpu.fpu.enter_mmx_state();
            cpu.fpu.write_mmx_register(mmx(reg)?, value as u64);
        } else {
            cpu.sse.xmm[xmm(reg)?] = if bytes == 16 {
                value
            } else {
                let dst = cpu.sse.xmm[xmm(reg)?];
                (dst & !mask(bytes)) | (value & mask(bytes))
            };
        }
        Ok(())
    } else {
        let raw = to_bytes(value);
        write_operand_bytes(cpu, instr, mem, &raw[..bytes])
    }
}

fn set_gpr(cpu: &mut Cpu, reg: Register, value: u32) -> Result<()> {
    match reg {
        Register::EAX => cpu.regs.eax = value,
        Register::EBX => cpu.regs.ebx = value,
        Register::ECX => cpu.regs.ecx = value,
        Register::EDX => cpu.regs.edx = value,
        Register::ESI => cpu.regs.esi = value,
        Register::EDI => cpu.regs.edi = value,
        Register::EBP => cpu.regs.ebp = value,
        Register::ESP => cpu.regs.esp = value,
        Register::AX => cpu.regs.set_ax(value as u16),
        Register::BX => cpu.regs.set_bx(value as u16),
        Register::CX => cpu.regs.set_cx(value as u16),
        Register::DX => cpu.regs.set_dx(value as u16),
        Register::SI => cpu.regs.set_si(value as u16),
        Register::DI => cpu.regs.set_di(value as u16),
        Register::BP => cpu.regs.set_bp(value as u16),
        Register::SP => cpu.regs.set_sp(value as u16),
        _ => {
            return Err(CpuError::Unimplemented(format!(
                "SSE GPR destination {reg:?}"
            )))
        }
    }
    Ok(())
}

/// Execute the bounded SSE/SSE2 subset shared by the interpreter's opcode paths.
///
/// The caller has already performed CPU feature and CR0/CR4 gating.  Sources
/// are staged before destinations, so register aliases and memory faults do not
/// publish partial XMM state.
pub fn execute<M: PagedMemory>(cpu: &mut Cpu, instr: &Instruction, mem: &mut M) -> Result<()> {
    use Mnemonic::{
        Addpd, Addps, Addsd, Addss, Andnpd, Andnps, Andpd, Andps, Cmppd, Cmpps, Cmpsd, Cmpss,
        Comisd, Comiss, Cvtdq2pd, Cvtdq2ps, Cvtpd2dq, Cvtpd2pi, Cvtpd2ps, Cvtpi2pd, Cvtpi2ps,
        Cvtps2dq, Cvtps2pd, Cvtps2pi, Cvtsd2si, Cvtsd2ss, Cvtsi2sd, Cvtsi2ss, Cvtss2sd, Cvtss2si,
        Cvttpd2dq, Cvttpd2pi, Cvttps2dq, Cvttps2pi, Cvttsd2si, Cvttss2si, Divpd, Divps, Divsd,
        Divss, Fxrstor, Fxsave, Ldmxcsr, Lfence, Maskmovdqu, Maskmovq, Maxpd, Maxps, Maxsd, Maxss,
        Mfence, Minpd, Minps, Minsd, Minss, Movapd, Movaps, Movd, Movdq2q, Movdqa, Movdqu, Movhlps,
        Movhpd, Movhps, Movlhps, Movlpd, Movlps, Movmskpd, Movmskps, Movntdq, Movnti, Movntpd,
        Movntps, Movntq, Movq, Movq2dq, Movsd, Movss, Movupd, Movups, Mulpd, Mulps, Mulsd, Mulss,
        Orpd, Orps, Packssdw, Packsswb, Packuswb, Paddb, Paddd, Paddq, Paddsb, Paddsw, Paddusb,
        Paddusw, Paddw, Pand, Pandn, Pavgb, Pavgw, Pcmpeqb, Pcmpeqd, Pcmpeqw, Pcmpgtb, Pcmpgtd,
        Pcmpgtw, Pextrw, Pinsrw, Pmaddwd, Pmaxsw, Pmaxub, Pminsw, Pminub, Pmovmskb, Pmulhuw,
        Pmulhw, Pmullw, Pmuludq, Por, Prefetchnta, Prefetcht0, Prefetcht1, Prefetcht2, Psadbw,
        Pshufd, Pshufhw, Pshuflw, Pshufw, Pslld, Pslldq, Psllq, Psllw, Psrad, Psraw, Psrld, Psrldq,
        Psrlq, Psrlw, Psubb, Psubd, Psubq, Psubsb, Psubsw, Psubusb, Psubusw, Psubw, Punpckhbw,
        Punpckhdq, Punpckhqdq, Punpckhwd, Punpcklbw, Punpckldq, Punpcklqdq, Punpcklwd, Pxor, Rcpps,
        Rcpss, Rsqrtps, Rsqrtss, Sfence, Shufpd, Shufps, Sqrtpd, Sqrtps, Sqrtsd, Sqrtss, Stmxcsr,
        Subpd, Subps, Subsd, Subss, Ucomisd, Ucomiss, Unpckhpd, Unpckhps, Unpcklpd, Unpcklps,
        Xorpd, Xorps,
    };
    let m = instr.mnemonic();
    match m {
        Movaps | Movapd | Movdqa | Movntdq | Movntpd | Movntps => {
            require_aligned(cpu, instr, mem, 16)?;
            let v = if instr.op0_kind() == OpKind::Memory {
                cpu.sse.xmm[xmm(instr.op1_register())?]
            } else {
                source(cpu, instr, mem, 16)?
            };
            write_xmm_or_memory(cpu, instr, mem, v, 16)
        }
        Movups | Movupd | Movdqu => {
            let v = if instr.op0_kind() == OpKind::Memory {
                cpu.sse.xmm[xmm(instr.op1_register())?]
            } else {
                source(cpu, instr, mem, 16)?
            };
            write_xmm_or_memory(cpu, instr, mem, v, 16)
        }
        Movntq => {
            let value = u128::from(
                cpu.fpu
                    .read_mmx_register_committed(mmx(instr.op1_register())?),
            );
            write_xmm_or_memory(cpu, instr, mem, value, 8)?;
            cpu.fpu.enter_mmx_state();
            Ok(())
        }
        Movnti => {
            let value = read_register(cpu, instr.op1_register()) as u32;
            let raw = value.to_le_bytes();
            write_operand_bytes(cpu, instr, mem, &raw)
        }
        Movss | Movsd => {
            let bytes = if m == Movss { 4 } else { 8 };
            if instr.op0_kind() == OpKind::Memory {
                return write_xmm_or_memory(
                    cpu,
                    instr,
                    mem,
                    cpu.sse.xmm[xmm(instr.op1_register())?],
                    bytes,
                );
            }
            let src = source(cpu, instr, mem, bytes)?;
            let dst = xmm(instr.op0_register())?;
            cpu.sse.xmm[dst] = if instr.op1_kind() == OpKind::Memory {
                src
            } else {
                (cpu.sse.xmm[dst] & !mask(bytes)) | src
            };
            Ok(())
        }
        Movlps | Movlpd | Movhps | Movhpd => {
            let high = matches!(m, Movhps | Movhpd);
            let dst_mem = instr.op0_kind() == OpKind::Memory;
            if dst_mem {
                let value = cpu.sse.xmm[xmm(instr.op1_register())?] >> if high { 64 } else { 0 };
                return write_xmm_or_memory(cpu, instr, mem, value, 8);
            }
            let src = source(cpu, instr, mem, 8)?;
            let d = xmm(instr.op0_register())?;
            let old = cpu.sse.xmm[d];
            cpu.sse.xmm[d] = if high {
                (old & mask(8)) | (src << 64)
            } else {
                (old & !mask(8)) | src
            };
            Ok(())
        }
        Movhlps => {
            let d = xmm(instr.op0_register())?;
            cpu.sse.xmm[d] =
                (cpu.sse.xmm[d] & !mask(8)) | (cpu.sse.xmm[xmm(instr.op1_register())?] >> 64);
            Ok(())
        }
        Movlhps => {
            let d = xmm(instr.op0_register())?;
            cpu.sse.xmm[d] = (cpu.sse.xmm[d] & mask(8))
                | ((cpu.sse.xmm[xmm(instr.op1_register())?] & mask(8)) << 64);
            Ok(())
        }
        Movd => {
            let v = if instr.op0_kind() == OpKind::Register && instr.op0_register().is_xmm() {
                if instr.op1_kind() == OpKind::Register {
                    read_register(cpu, instr.op1_register()) as u32 as u128
                } else {
                    source(cpu, instr, mem, 4)?
                }
            } else {
                cpu.sse.xmm[xmm(instr.op1_register())?] & mask(4)
            };
            if instr.op0_kind() == OpKind::Register && !instr.op0_register().is_xmm() {
                set_gpr(cpu, instr.op0_register(), v as u32)
            } else if instr.op0_kind() == OpKind::Memory {
                write_xmm_or_memory(cpu, instr, mem, v, 4)
            } else {
                write_xmm_or_memory(cpu, instr, mem, v, 16)
            }
        }
        Movq => {
            let v = if instr.op0_kind() == OpKind::Register && instr.op0_register().is_xmm() {
                source(cpu, instr, mem, 8)?
            } else {
                cpu.sse.xmm[xmm(instr.op1_register())?] & mask(8)
            };
            if instr.op0_kind() == OpKind::Register && !instr.op0_register().is_xmm() {
                set_gpr(cpu, instr.op0_register(), v as u32)
            } else if instr.op0_kind() == OpKind::Memory {
                write_xmm_or_memory(cpu, instr, mem, v, 8)
            } else {
                write_xmm_or_memory(cpu, instr, mem, v, 16)
            }
        }
        Movdq2q => {
            let value = cpu.sse.xmm[xmm(instr.op1_register())?] as u64;
            cpu.fpu.enter_mmx_state();
            cpu.fpu
                .write_mmx_register(mmx(instr.op0_register())?, value);
            Ok(())
        }
        Movq2dq => {
            let value = cpu
                .fpu
                .read_mmx_register_committed(mmx(instr.op1_register())?);
            cpu.sse.xmm[xmm(instr.op0_register())?] = u128::from(value);
            cpu.fpu.enter_mmx_state();
            Ok(())
        }
        Maskmovq | Maskmovdqu => maskmov(cpu, instr, mem),
        Ldmxcsr => {
            let mut bytes = [0u8; 4];
            read_operand_bytes(cpu, instr, mem, &mut bytes)?;
            let v = u32::from_le_bytes(bytes);
            if v & !cpu.sse.mxcsr_mask != 0 {
                Err(CpuError::GeneralProtectionFault)
            } else {
                cpu.sse.mxcsr = v;
                Ok(())
            }
        }
        Stmxcsr => write_xmm_or_memory(cpu, instr, mem, cpu.sse.mxcsr as u128, 4),
        Fxsave => fxsave(cpu, instr, mem),
        Fxrstor => fxrstor(cpu, instr, mem),
        Andps | Andpd => binary(cpu, instr, mem, |a, b| a & b),
        Orps | Orpd => binary(cpu, instr, mem, |a, b| a | b),
        Xorps | Xorpd => binary(cpu, instr, mem, |a, b| a ^ b),
        Andnps | Andnpd => binary(cpu, instr, mem, |a, b| (!a) & b),
        Pand => binary(cpu, instr, mem, |a, b| a & b),
        Por => binary(cpu, instr, mem, |a, b| a | b),
        Pxor => binary(cpu, instr, mem, |a, b| a ^ b),
        Pandn => binary(cpu, instr, mem, |a, b| (!a) & b),
        Addps => float_binary(cpu, instr, mem, false, 4, FloatOperation::Add),
        Addss => float_binary(cpu, instr, mem, false, 1, FloatOperation::Add),
        Subps => float_binary(cpu, instr, mem, false, 4, FloatOperation::Subtract),
        Subss => float_binary(cpu, instr, mem, false, 1, FloatOperation::Subtract),
        Mulps => float_binary(cpu, instr, mem, false, 4, FloatOperation::Multiply),
        Mulss => float_binary(cpu, instr, mem, false, 1, FloatOperation::Multiply),
        Divps => float_binary(cpu, instr, mem, false, 4, FloatOperation::Divide),
        Divss => float_binary(cpu, instr, mem, false, 1, FloatOperation::Divide),
        Minps => minmax_float(cpu, instr, mem, false, 4, false),
        Minss => minmax_float(cpu, instr, mem, false, 1, false),
        Maxps => minmax_float(cpu, instr, mem, false, 4, true),
        Maxss => minmax_float(cpu, instr, mem, false, 1, true),
        Sqrtps => square_root(cpu, instr, mem, false, 4),
        Sqrtss => square_root(cpu, instr, mem, false, 1),
        Addpd => float_binary(cpu, instr, mem, true, 2, FloatOperation::Add),
        Addsd => float_binary(cpu, instr, mem, true, 1, FloatOperation::Add),
        Subpd => float_binary(cpu, instr, mem, true, 2, FloatOperation::Subtract),
        Subsd => float_binary(cpu, instr, mem, true, 1, FloatOperation::Subtract),
        Mulpd => float_binary(cpu, instr, mem, true, 2, FloatOperation::Multiply),
        Mulsd => float_binary(cpu, instr, mem, true, 1, FloatOperation::Multiply),
        Divpd => float_binary(cpu, instr, mem, true, 2, FloatOperation::Divide),
        Divsd => float_binary(cpu, instr, mem, true, 1, FloatOperation::Divide),
        Minpd => minmax_float(cpu, instr, mem, true, 2, false),
        Minsd => minmax_float(cpu, instr, mem, true, 1, false),
        Maxpd => minmax_float(cpu, instr, mem, true, 2, true),
        Maxsd => minmax_float(cpu, instr, mem, true, 1, true),
        Sqrtpd => square_root(cpu, instr, mem, true, 2),
        Sqrtsd => square_root(cpu, instr, mem, true, 1),
        Rcpps => unary(cpu, instr, mem, 16, |a| fp32(a, 0, 4, |x, _| x.recip())),
        Rcpss => unary(cpu, instr, mem, 4, |a| fp32(a, 0, 1, |x, _| x.recip())),
        Rsqrtps => unary(cpu, instr, mem, 16, |a| {
            fp32(a, 0, 4, |x, _| x.sqrt().recip())
        }),
        Rsqrtss => unary(cpu, instr, mem, 4, |a| {
            fp32(a, 0, 1, |x, _| x.sqrt().recip())
        }),
        Paddb => intop(cpu, instr, mem, 8, false, false),
        Paddw => intop(cpu, instr, mem, 16, false, false),
        Paddd => intop(cpu, instr, mem, 32, false, false),
        Paddq => intop(cpu, instr, mem, 64, false, false),
        Psubb => intop(cpu, instr, mem, 8, false, true),
        Psubw => intop(cpu, instr, mem, 16, false, true),
        Psubd => intop(cpu, instr, mem, 32, false, true),
        Psubq => intop(cpu, instr, mem, 64, false, true),
        Paddsb => intop(cpu, instr, mem, 8, true, false),
        Paddsw => intop(cpu, instr, mem, 16, true, false),
        Psubsb => intop(cpu, instr, mem, 8, true, true),
        Psubsw => intop(cpu, instr, mem, 16, true, true),
        Paddusb => uintsat(cpu, instr, mem, 8, false),
        Paddusw => uintsat(cpu, instr, mem, 16, false),
        Psubusb => uintsat(cpu, instr, mem, 8, true),
        Psubusw => uintsat(cpu, instr, mem, 16, true),
        Pcmpeqb => integer_binary(cpu, instr, mem, |a, b| compare_integer(a, b, 8, false)),
        Pcmpeqw => integer_binary(cpu, instr, mem, |a, b| compare_integer(a, b, 16, false)),
        Pcmpeqd => integer_binary(cpu, instr, mem, |a, b| compare_integer(a, b, 32, false)),
        Pcmpgtb => integer_binary(cpu, instr, mem, |a, b| compare_integer(a, b, 8, true)),
        Pcmpgtw => integer_binary(cpu, instr, mem, |a, b| compare_integer(a, b, 16, true)),
        Pcmpgtd => integer_binary(cpu, instr, mem, |a, b| compare_integer(a, b, 32, true)),
        Pminub => integer_binary(cpu, instr, mem, |a, b| {
            minmax_integer(a, b, 8, false, false)
        }),
        Pmaxub => integer_binary(cpu, instr, mem, |a, b| minmax_integer(a, b, 8, false, true)),
        Pminsw => integer_binary(cpu, instr, mem, |a, b| {
            minmax_integer(a, b, 16, true, false)
        }),
        Pmaxsw => integer_binary(cpu, instr, mem, |a, b| minmax_integer(a, b, 16, true, true)),
        Pavgb => integer_binary(cpu, instr, mem, |a, b| average_integer(a, b, 8)),
        Pavgw => integer_binary(cpu, instr, mem, |a, b| average_integer(a, b, 16)),
        Pmullw => integer_binary(cpu, instr, mem, |a, b| {
            multiply_integer(a, b, 16, false, false)
        }),
        Pmulhw => integer_binary(cpu, instr, mem, |a, b| {
            multiply_integer(a, b, 16, true, false)
        }),
        Pmulhuw => integer_binary(cpu, instr, mem, |a, b| {
            multiply_integer(a, b, 16, true, true)
        }),
        Packsswb => integer_binary(cpu, instr, mem, |a, b| pack_signed(a, b, 16, false)),
        Packssdw => integer_binary(cpu, instr, mem, |a, b| pack_signed(a, b, 32, false)),
        Packuswb => integer_binary(cpu, instr, mem, |a, b| pack_signed(a, b, 16, true)),
        Cmpps => compare32(cpu, instr, mem, 4),
        Cmpss => compare32(cpu, instr, mem, 1),
        Cmppd => compare64(cpu, instr, mem, 2),
        Cmpsd => compare64(cpu, instr, mem, 1),
        Unpcklps => unpack(cpu, instr, mem, 4, false),
        Unpckhps => unpack(cpu, instr, mem, 4, true),
        Unpcklpd => unpack(cpu, instr, mem, 8, false),
        Unpckhpd => unpack(cpu, instr, mem, 8, true),
        Punpcklbw => unpack(cpu, instr, mem, 1, false),
        Punpcklwd => unpack(cpu, instr, mem, 2, false),
        Punpckldq => unpack(cpu, instr, mem, 4, false),
        Punpcklqdq => unpack(cpu, instr, mem, 8, false),
        Punpckhbw => unpack(cpu, instr, mem, 1, true),
        Punpckhwd => unpack(cpu, instr, mem, 2, true),
        Punpckhdq => unpack(cpu, instr, mem, 4, true),
        Punpckhqdq => unpack(cpu, instr, mem, 8, true),
        Shufps => shufps(cpu, instr, mem),
        Shufpd => shufpd(cpu, instr, mem),
        Pshufd | Pshufhw | Pshuflw | Pshufw => pshuffle(cpu, instr, mem),
        Pmaddwd => pmaddwd(cpu, instr, mem),
        Pmuludq => pmuludq(cpu, instr, mem),
        Psadbw => psadbw(cpu, instr, mem),
        Movmskps | Movmskpd | Pmovmskb => move_mask(cpu, instr),
        Pinsrw => pinsrw(cpu, instr, mem),
        Pextrw => pextrw(cpu, instr),
        Comiss | Ucomiss | Comisd | Ucomisd => compare_flags(cpu, instr, mem),
        Lfence | Mfence | Sfence | Prefetchnta | Prefetcht0 | Prefetcht1 | Prefetcht2 => Ok(()),
        Pslld | Psllq | Psllw | Psrld | Psrlq | Psrlw | Psrad | Psraw | Pslldq | Psrldq => {
            shift(cpu, instr, mem)
        }
        Cvtdq2ps | Cvtps2dq | Cvttps2dq | Cvtps2pd | Cvtpd2ps | Cvtdq2pd | Cvtpd2dq | Cvttpd2dq
        | Cvtsi2ss | Cvtsi2sd | Cvtss2si | Cvttss2si | Cvtsd2si | Cvttsd2si | Cvtss2sd
        | Cvtsd2ss | Cvtpi2ps | Cvtpi2pd | Cvtps2pi | Cvttps2pi | Cvtpd2pi | Cvttpd2pi => {
            convert(cpu, instr, mem)
        }
        _ => Err(unsupported(instr)),
    }
}

fn maskmov<M: PagedMemory>(cpu: &mut Cpu, instruction: &Instruction, memory: &mut M) -> Result<()> {
    let bytes = if instruction.mnemonic() == Mnemonic::Maskmovq {
        8
    } else {
        16
    };
    let data_register = instruction.op1_register();
    let mask_register = instruction.op2_register();
    let data = if bytes == 8 {
        u128::from(cpu.fpu.read_mmx_register_committed(mmx(data_register)?))
    } else {
        cpu.sse.xmm[xmm(data_register)?]
    }
    .to_le_bytes();
    let select = if bytes == 8 {
        u128::from(cpu.fpu.read_mmx_register_committed(mmx(mask_register)?))
    } else {
        cpu.sse.xmm[xmm(mask_register)?]
    }
    .to_le_bytes();
    let mut output = [0u8; 16];
    read_rmw_operand_bytes(cpu, instruction, memory, &mut output[..bytes])?;
    for index in 0..bytes {
        if select[index] & 0x80 != 0 {
            output[index] = data[index];
        }
    }
    write_operand_bytes(cpu, instruction, memory, &output[..bytes])?;
    if bytes == 8 {
        cpu.fpu.enter_mmx_state();
    }
    Ok(())
}

fn fxsave<M: PagedMemory>(cpu: &mut Cpu, instruction: &Instruction, memory: &mut M) -> Result<()> {
    require_aligned(cpu, instruction, memory, 16)?;
    debug_assert!(
        !cpu.fpu_host_resident.get(),
        "FXSAVE requires dynamic engines to spill host-resident x87 state"
    );
    // FXSAVE observes the complete x87 image, including reverse-stale F80
    // mirrors left by a DRC binary64 write.
    cpu.fpu.materialize_drc_f80_register_file();
    let mut image = [0u8; 512];
    image[0..2].copy_from_slice(&cpu.fpu.control_word.to_le_bytes());
    image[2..4].copy_from_slice(&cpu.fpu.status_word.to_le_bytes());
    let mut abridged_tags = 0u8;
    for physical in 0..8 {
        if (cpu.fpu.tag_word >> (physical * 2)) & 3 != 3 {
            abridged_tags |= 1 << physical;
        }
    }
    image[4] = abridged_tags;
    image[6..8].copy_from_slice(&(cpu.fpu.opcode & 0x07ff).to_le_bytes());
    image[8..12].copy_from_slice(&cpu.fpu.ip_offset.to_le_bytes());
    image[12..14].copy_from_slice(&cpu.fpu.ip_selector.to_le_bytes());
    image[16..20].copy_from_slice(&cpu.fpu.operand_offset.to_le_bytes());
    image[20..22].copy_from_slice(&cpu.fpu.operand_selector.to_le_bytes());
    image[24..28].copy_from_slice(&cpu.sse.mxcsr.to_le_bytes());
    image[28..32].copy_from_slice(&cpu.sse.mxcsr_mask.to_le_bytes());
    // The register area is ST-relative: slot i holds ST(i), the physical
    // register (TOP + i) & 7 (SDM Vol 1 Fig 10-2, "ST0/MM0" ... "ST7/MM7") —
    // the order FNSAVE uses too. MDD-BUG-TEM-00163: this wrote physical order.
    let top = cpu.fpu.get_top();
    for st in 0..8 {
        image[32 + st * 16..42 + st * 16].copy_from_slice(&cpu.fpu.regs[(top + st) & 7].0);
    }
    for (index, register) in cpu.sse.xmm.iter().enumerate() {
        image[160 + index * 16..176 + index * 16].copy_from_slice(&register.to_le_bytes());
    }
    write_operand_bytes(cpu, instruction, memory, &image)
}

fn fxrstor<M: PagedMemory>(cpu: &mut Cpu, instruction: &Instruction, memory: &mut M) -> Result<()> {
    require_aligned(cpu, instruction, memory, 16)?;
    let mut image = [0u8; 512];
    read_operand_bytes(cpu, instruction, memory, &mut image)?;
    let mxcsr = u32::from_le_bytes(image[24..28].try_into().expect("MXCSR"));
    if mxcsr & !cpu.sse.mxcsr_mask != 0 {
        return Err(CpuError::GeneralProtectionFault);
    }

    cpu.fpu.control_word = u16::from_le_bytes(image[0..2].try_into().expect("FCW"));
    cpu.fpu.status_word = u16::from_le_bytes(image[2..4].try_into().expect("FSW"));
    cpu.fpu.opcode = u16::from_le_bytes(image[6..8].try_into().expect("FOP")) & 0x07ff;
    cpu.fpu.ip_offset = u32::from_le_bytes(image[8..12].try_into().expect("FIP"));
    cpu.fpu.ip_selector = u16::from_le_bytes(image[12..14].try_into().expect("FCS"));
    cpu.fpu.operand_offset = u32::from_le_bytes(image[16..20].try_into().expect("FDP"));
    cpu.fpu.operand_selector = u16::from_le_bytes(image[20..22].try_into().expect("FDS"));
    // The abridged tag byte is physical-indexed (bit j is Rj, like the tag
    // word), but the register area is ST-relative under the TOP just
    // restored, so the register behind tag bit j is slot (j - TOP) & 7 —
    // FXSAVE's order reversed.
    let top = cpu.fpu.get_top();
    let abridged_tags = image[4];
    let mut tag_word = 0u16;
    for physical in 0..8 {
        let st = (physical + 8 - top) & 7;
        let register_bytes = &image[32 + st * 16..42 + st * 16];
        cpu.fpu.regs[physical].0.copy_from_slice(register_bytes);
        let tag = if abridged_tags & (1 << physical) == 0 {
            3
        } else {
            classify_restored_x87(register_bytes)
        };
        tag_word |= tag << (physical * 2);
    }
    cpu.fpu.tag_word = tag_word;
    cpu.sse.mxcsr = mxcsr;
    for index in 0..8 {
        cpu.sse.xmm[index] = u128::from_le_bytes(
            image[160 + index * 16..176 + index * 16]
                .try_into()
                .expect("XMM register"),
        );
    }
    // FXRSTOR has replaced the raw F80 register file. Drop old numeric shadow
    // proofs only after the image has passed validation and loaded fully.
    cpu.fpu.invalidate_drc_f80_register_file();
    Ok(())
}

fn binary<M: PagedMemory>(
    cpu: &mut Cpu,
    i: &Instruction,
    mem: &mut M,
    op: fn(u128, u128) -> u128,
) -> Result<()> {
    let register = i.op0_register();
    let bytes = if is_mmx(register) { 8 } else { 16 };
    let a = if is_mmx(register) {
        u128::from(cpu.fpu.read_mmx_register_committed(mmx(register)?))
    } else {
        cpu.sse.xmm[xmm(register)?]
    };
    let b = source(cpu, i, mem, bytes)?;
    write_xmm_or_memory(cpu, i, mem, op(a, b), bytes)
}

fn integer_binary<M: PagedMemory>(
    cpu: &mut Cpu,
    instruction: &Instruction,
    memory: &mut M,
    operation: fn(u128, u128) -> u128,
) -> Result<()> {
    binary(cpu, instruction, memory, operation)
}

fn pshuffle<M: PagedMemory>(cpu: &mut Cpu, i: &Instruction, mem: &mut M) -> Result<()> {
    use Mnemonic::{Pshufd, Pshufhw, Pshuflw, Pshufw};
    let mmx_destination = is_mmx(i.op0_register());
    let bytes = if mmx_destination { 8 } else { 16 };
    let source_value = source(cpu, i, mem, bytes)?;
    let immediate = i.immediate8();
    let mut output = match i.mnemonic() {
        Pshufhw | Pshuflw => source_value,
        _ => 0,
    };
    match i.mnemonic() {
        Pshufw => {
            for lane in 0..4 {
                let source_lane = usize::from((immediate >> (lane * 2)) & 3);
                let value = ((source_value >> (source_lane * 16)) & 0xffff) << (lane * 16);
                output |= value;
            }
        }
        Pshufd => {
            for lane in 0..4 {
                put32(
                    &mut output,
                    lane,
                    lane32(source_value, usize::from((immediate >> (lane * 2)) & 3)),
                );
            }
        }
        Pshuflw => {
            for lane in 0..4 {
                let source_lane = usize::from((immediate >> (lane * 2)) & 3);
                let value = ((source_value >> (source_lane * 16)) & 0xffff) << (lane * 16);
                output = (output & !(0xffffu128 << (lane * 16))) | value;
            }
        }
        Pshufhw => {
            for lane in 0..4 {
                let source_lane = 4 + usize::from((immediate >> (lane * 2)) & 3);
                let destination_lane = lane + 4;
                let value =
                    ((source_value >> (source_lane * 16)) & 0xffff) << (destination_lane * 16);
                output = (output & !(0xffffu128 << (destination_lane * 16))) | value;
            }
        }
        _ => unreachable!(),
    }
    write_xmm_or_memory(cpu, i, mem, output, bytes)
}

fn pmaddwd<M: PagedMemory>(cpu: &mut Cpu, i: &Instruction, mem: &mut M) -> Result<()> {
    let left = cpu.sse.xmm[xmm(i.op0_register())?];
    let right = source(cpu, i, mem, 16)?;
    let mut output = 0u128;
    for lane in 0..4 {
        let index = lane * 2;
        let product0 = signed_lane((left >> (index * 16)) & 0xffff, 16)
            * signed_lane((right >> (index * 16)) & 0xffff, 16);
        let product1 = signed_lane((left >> ((index + 1) * 16)) & 0xffff, 16)
            * signed_lane((right >> ((index + 1) * 16)) & 0xffff, 16);
        put32(&mut output, lane, product0.wrapping_add(product1) as u32);
    }
    cpu.sse.xmm[xmm(i.op0_register())?] = output;
    Ok(())
}

fn pmuludq<M: PagedMemory>(cpu: &mut Cpu, i: &Instruction, mem: &mut M) -> Result<()> {
    let register = i.op0_register();
    let bytes = if is_mmx(register) { 8 } else { 16 };
    let left = if is_mmx(register) {
        u128::from(cpu.fpu.read_mmx_register_committed(mmx(register)?))
    } else {
        cpu.sse.xmm[xmm(register)?]
    };
    let right = source(cpu, i, mem, bytes)?;
    let low = u64::from(lane32(left, 0)) * u64::from(lane32(right, 0));
    if bytes == 8 {
        return write_xmm_or_memory(cpu, i, mem, u128::from(low), 8);
    }
    let high = u64::from(lane32(left, 2)) * u64::from(lane32(right, 2));
    write_xmm_or_memory(cpu, i, mem, u128::from(low) | (u128::from(high) << 64), 16)
}

fn psadbw<M: PagedMemory>(cpu: &mut Cpu, i: &Instruction, mem: &mut M) -> Result<()> {
    let register = i.op0_register();
    let bytes = if is_mmx(register) { 8 } else { 16 };
    let left = if is_mmx(register) {
        u128::from(cpu.fpu.read_mmx_register_committed(mmx(register)?))
    } else {
        cpu.sse.xmm[xmm(register)?]
    }
    .to_le_bytes();
    let right = source(cpu, i, mem, bytes)?.to_le_bytes();
    let mut sums = [0u64; 2];
    for lane in 0..bytes {
        sums[lane / 8] += u64::from(left[lane].abs_diff(right[lane]));
    }
    write_xmm_or_memory(
        cpu,
        i,
        mem,
        u128::from(sums[0]) | (u128::from(sums[1]) << 64),
        bytes,
    )
}

fn move_mask(cpu: &mut Cpu, i: &Instruction) -> Result<()> {
    use Mnemonic::{Movmskpd, Movmskps, Pmovmskb};
    let source_register = i.op1_register();
    let source_value = if is_mmx(source_register) {
        u128::from(cpu.fpu.read_mmx_register_committed(mmx(source_register)?))
    } else {
        cpu.sse.xmm[xmm(source_register)?]
    };
    let result = match i.mnemonic() {
        Movmskps => (0..4).fold(0, |mask, lane| {
            mask | (((lane32(source_value, lane) >> 31) & 1) << lane)
        }),
        Movmskpd => (0..2).fold(0, |mask, lane| {
            mask | (((lane64(source_value, lane) >> 63) as u32 & 1) << lane)
        }),
        Pmovmskb => source_value
            .to_le_bytes()
            .into_iter()
            .take(if is_mmx(source_register) { 8 } else { 16 })
            .enumerate()
            .fold(0u32, |mask, (lane, byte)| {
                mask | (u32::from(byte >> 7) << lane)
            }),
        _ => unreachable!(),
    };
    set_gpr(cpu, i.op0_register(), result)?;
    if is_mmx(source_register) {
        cpu.fpu.enter_mmx_state();
    }
    Ok(())
}

fn pinsrw<M: PagedMemory>(cpu: &mut Cpu, i: &Instruction, mem: &mut M) -> Result<()> {
    let value = if i.op1_kind() == OpKind::Register {
        read_register(cpu, i.op1_register()) as u16
    } else {
        source(cpu, i, mem, 2)? as u16
    };
    let register = i.op0_register();
    let bytes = if is_mmx(register) { 8 } else { 16 };
    let lane = usize::from(i.immediate8() & if bytes == 8 { 3 } else { 7 });
    let old = if bytes == 8 {
        u128::from(cpu.fpu.read_mmx_register_committed(mmx(register)?))
    } else {
        cpu.sse.xmm[xmm(register)?]
    };
    let result = (old & !(0xffffu128 << (lane * 16))) | (u128::from(value) << (lane * 16));
    write_xmm_or_memory(cpu, i, mem, result, bytes)
}

fn pextrw(cpu: &mut Cpu, i: &Instruction) -> Result<()> {
    let register = i.op1_register();
    let source_value = if is_mmx(register) {
        u128::from(cpu.fpu.read_mmx_register_committed(mmx(register)?))
    } else {
        cpu.sse.xmm[xmm(register)?]
    };
    let lane = usize::from(i.immediate8() & if is_mmx(register) { 3 } else { 7 });
    set_gpr(
        cpu,
        i.op0_register(),
        ((source_value >> (lane * 16)) & 0xffff) as u32,
    )?;
    if is_mmx(register) {
        cpu.fpu.enter_mmx_state();
    }
    Ok(())
}

fn compare_flags<M: PagedMemory>(cpu: &mut Cpu, i: &Instruction, mem: &mut M) -> Result<()> {
    use Mnemonic::{Comisd, Comiss, Ucomisd};
    let is_double = matches!(i.mnemonic(), Comisd | Ucomisd);
    let left_register = xmm(i.op0_register())?;
    let ordered_compare = matches!(i.mnemonic(), Comiss | Comisd);
    let daz = cpu.sse.mxcsr & (1 << 6) != 0;
    let unordered_or_ordering = if is_double {
        let (left_bits, left_denormal) = daz64(lane64(cpu.sse.xmm[left_register], 0), daz);
        let (right_bits, right_denormal) = daz64(source(cpu, i, mem, 8)? as u64, daz);
        let invalid = if ordered_compare {
            is_nan64(left_bits) || is_nan64(right_bits)
        } else {
            is_signaling_nan64(left_bits) || is_signaling_nan64(right_bits)
        };
        let status = if invalid {
            Status::INVALID_OP
        } else {
            Status::OK
        };
        publish_fp_status(cpu, status, left_denormal || right_denormal)?;
        let left = f64::from_bits(left_bits);
        let right = f64::from_bits(right_bits);
        left.partial_cmp(&right)
    } else {
        let (left_bits, left_denormal) = daz32(lane32(cpu.sse.xmm[left_register], 0), daz);
        let (right_bits, right_denormal) = daz32(source(cpu, i, mem, 4)? as u32, daz);
        let invalid = if ordered_compare {
            is_nan32(left_bits) || is_nan32(right_bits)
        } else {
            is_signaling_nan32(left_bits) || is_signaling_nan32(right_bits)
        };
        let status = if invalid {
            Status::INVALID_OP
        } else {
            Status::OK
        };
        publish_fp_status(cpu, status, left_denormal || right_denormal)?;
        let left = f32::from_bits(left_bits);
        let right = f32::from_bits(right_bits);
        left.partial_cmp(&right)
    };
    cpu.regs.flags.remove(
        crate::Flags::OVERFLOW
            | crate::Flags::SIGN
            | crate::Flags::AUX_CARRY
            | crate::Flags::ZERO
            | crate::Flags::PARITY
            | crate::Flags::CARRY,
    );
    match unordered_or_ordering {
        None => cpu
            .regs
            .flags
            .insert(crate::Flags::ZERO | crate::Flags::PARITY | crate::Flags::CARRY),
        Some(core::cmp::Ordering::Less) => cpu.regs.flags.insert(crate::Flags::CARRY),
        Some(core::cmp::Ordering::Equal) => cpu.regs.flags.insert(crate::Flags::ZERO),
        Some(core::cmp::Ordering::Greater) => {}
    }
    Ok(())
}
fn float_binary<M: PagedMemory>(
    cpu: &mut Cpu,
    i: &Instruction,
    mem: &mut M,
    double: bool,
    lanes: usize,
    operation: FloatOperation,
) -> Result<()> {
    let destination = xmm(i.op0_register())?;
    let left = cpu.sse.xmm[destination];
    let lane_bytes = if double { 8 } else { 4 };
    let right = source(cpu, i, mem, lane_bytes * lanes)?;
    let daz = cpu.sse.mxcsr & (1 << 6) != 0;
    let ftz = cpu.sse.mxcsr & (1 << 15) != 0;
    let (output, status, denormal) = if double {
        apfloat64(left, right, lanes, operation, cpu.sse.rounding(), daz, ftz)
    } else {
        apfloat32(left, right, lanes, operation, cpu.sse.rounding(), daz, ftz)
    };
    // The oracle moves with the executor: an overflow that CLAMPED to the
    // largest finite must raise Overflow in both models or the migration
    // differential diverges (MDD-BUG-CRUCIBLE-00785, as
    // MDD-BUG-CRUCIBLE-00784 and -00782 did before it).
    let extra = unmasked_underflow_bit(output, lanes, double, cpu.sse.mxcsr)
        | crate::sse_exec::clamped_overflow_bit(
            left,
            right,
            output,
            lanes,
            double,
            match operation {
                FloatOperation::Add => crate::sse_exec::FloatOperation::Add,
                FloatOperation::Subtract => crate::sse_exec::FloatOperation::Subtract,
                FloatOperation::Multiply => crate::sse_exec::FloatOperation::Multiply,
                FloatOperation::Divide => crate::sse_exec::FloatOperation::Divide,
            },
            cpu.sse.mxcsr,
            crate::sse_exec::status_bits(status, denormal),
        );
    publish_fp_status_with(cpu, status, denormal, extra)?;
    cpu.sse.xmm[destination] = output;
    Ok(())
}

fn square_root<M: PagedMemory>(
    cpu: &mut Cpu,
    instruction: &Instruction,
    memory: &mut M,
    double: bool,
    lanes: usize,
) -> Result<()> {
    let destination = xmm(instruction.op0_register())?;
    let lane_bytes = if double { 8 } else { 4 };
    let source_value = source(cpu, instruction, memory, lane_bytes * lanes)?;
    let mut output = cpu.sse.xmm[destination];
    let mut status = Status::OK;
    let mut denormal = false;
    let daz = cpu.sse.mxcsr & (1 << 6) != 0;
    let ftz = cpu.sse.mxcsr & (1 << 15) != 0;
    for lane in 0..lanes {
        if double {
            let (bits, lane_denormal) = daz64(lane64(source_value, lane), daz);
            denormal |= lane_denormal;
            // The oracle moves with the executor here. `ieee_apsqrt` rounds a
            // denormal root as inexact even when it is exact, so a directed
            // mode moved it one ulp off what silicon returns; fixing only one
            // model would diverge the migration differential
            // (MDD-BUG-CRUCIBLE-00784, the same treatment
            // MDD-BUG-CRUCIBLE-00782 had).
            let (value, lane_status) = if crate::sse_exec::is_denormal64(bits) {
                crate::sse_exec::sqrt_denormal64(bits, apfloat_round(cpu.sse.rounding()))
            } else {
                let (result, _) = sqrt_accurate(bits, apfloat_round(cpu.sse.rounding()));
                (result.value, result.status)
            };
            status |= lane_status;
            put64(&mut output, lane, ftz64(value, lane_status, ftz));
        } else {
            let (bits, lane_denormal) = daz32(lane32(source_value, lane), daz);
            denormal |= lane_denormal;
            let (value, lane_status) = if crate::sse_exec::is_denormal32(bits) {
                crate::sse_exec::sqrt_denormal32(bits, apfloat_round(cpu.sse.rounding()))
            } else {
                let (result, _) = sqrt_accurate(bits, apfloat_round(cpu.sse.rounding()));
                (result.value, result.status)
            };
            status |= lane_status;
            put32(&mut output, lane, ftz32(value, lane_status, ftz));
        }
    }
    publish_fp_status(cpu, status, denormal)?;
    cpu.sse.xmm[destination] = output;
    Ok(())
}

fn minmax_float<M: PagedMemory>(
    cpu: &mut Cpu,
    instruction: &Instruction,
    memory: &mut M,
    double: bool,
    lanes: usize,
    maximum: bool,
) -> Result<()> {
    let destination = xmm(instruction.op0_register())?;
    let left = cpu.sse.xmm[destination];
    let lane_bytes = if double { 8 } else { 4 };
    let right = source(cpu, instruction, memory, lane_bytes * lanes)?;
    let daz = cpu.sse.mxcsr & (1 << 6) != 0;
    let mut output = left;
    let mut status = Status::OK;
    let mut denormal = false;
    for lane in 0..lanes {
        if double {
            let (left_bits, left_denormal) = daz64(lane64(left, lane), daz);
            let (right_bits, right_denormal) = daz64(lane64(right, lane), daz);
            denormal |= left_denormal || right_denormal;
            // Quiet NaNs signal here too — see `sse_exec::soft_min_max`,
            // which carries the SDM citation and the silicon probe
            // (MDD-BUG-CRUCIBLE-00772). The two models are held together by
            // `new_executor_matches_the_iced_executor`, so they must agree.
            if is_nan64(left_bits) || is_nan64(right_bits) {
                status |= Status::INVALID_OP;
            }
            let selected = if is_nan64(left_bits) || is_nan64(right_bits) {
                right_bits
            } else {
                let take_left = if maximum {
                    f64::from_bits(left_bits) > f64::from_bits(right_bits)
                } else {
                    f64::from_bits(left_bits) < f64::from_bits(right_bits)
                };
                if take_left {
                    left_bits
                } else {
                    right_bits
                }
            };
            put64(&mut output, lane, selected);
        } else {
            let (left_bits, left_denormal) = daz32(lane32(left, lane), daz);
            let (right_bits, right_denormal) = daz32(lane32(right, lane), daz);
            denormal |= left_denormal || right_denormal;
            if is_nan32(left_bits) || is_nan32(right_bits) {
                status |= Status::INVALID_OP;
            }
            let selected = if is_nan32(left_bits) || is_nan32(right_bits) {
                right_bits
            } else {
                let take_left = if maximum {
                    f32::from_bits(left_bits) > f32::from_bits(right_bits)
                } else {
                    f32::from_bits(left_bits) < f32::from_bits(right_bits)
                };
                if take_left {
                    left_bits
                } else {
                    right_bits
                }
            };
            put32(&mut output, lane, selected);
        }
    }
    publish_fp_status(cpu, status, denormal)?;
    cpu.sse.xmm[destination] = output;
    Ok(())
}
fn unary<M: PagedMemory>(
    cpu: &mut Cpu,
    i: &Instruction,
    mem: &mut M,
    bytes: usize,
    op: fn(u128) -> u128,
) -> Result<()> {
    let a = source(cpu, i, mem, bytes)?;
    let d = xmm(i.op0_register())?;
    let out = op((cpu.sse.xmm[d] & !mask(bytes)) | a);
    cpu.sse.xmm[d] = (cpu.sse.xmm[d] & !mask(bytes)) | (out & mask(bytes));
    Ok(())
}
fn intop<M: PagedMemory>(
    cpu: &mut Cpu,
    i: &Instruction,
    mem: &mut M,
    w: usize,
    s: bool,
    sub: bool,
) -> Result<()> {
    let register = i.op0_register();
    let bytes = if is_mmx(register) { 8 } else { 16 };
    let a = if is_mmx(register) {
        u128::from(cpu.fpu.read_mmx_register_committed(mmx(register)?))
    } else {
        cpu.sse.xmm[xmm(register)?]
    };
    let b = source(cpu, i, mem, bytes)?;
    write_xmm_or_memory(cpu, i, mem, packed_int(a, b, w, s, sub), bytes)
}
fn uintsat<M: PagedMemory>(
    cpu: &mut Cpu,
    i: &Instruction,
    mem: &mut M,
    w: usize,
    sub: bool,
) -> Result<()> {
    let a = cpu.sse.xmm[xmm(i.op0_register())?];
    let b = source(cpu, i, mem, 16)?;
    write_xmm_or_memory(cpu, i, mem, packed_unsigned_sat(a, b, w, sub), 16)
}
fn compare32<M: PagedMemory>(cpu: &mut Cpu, i: &Instruction, mem: &mut M, n: usize) -> Result<()> {
    let a = cpu.sse.xmm[xmm(i.op0_register())?];
    let b = source(cpu, i, mem, 4 * n)?;
    let mut o = a;
    let mut status = Status::OK;
    let mut denormal = false;
    let daz = cpu.sse.mxcsr & (1 << 6) != 0;
    let predicate = i.immediate8() & 7;
    for x in 0..n {
        let (a_bits, a_denormal) = daz32(lane32(a, x), daz);
        let (b_bits, b_denormal) = daz32(lane32(b, x), daz);
        denormal |= a_denormal || b_denormal;
        if is_signaling_nan32(a_bits)
            || is_signaling_nan32(b_bits)
            || (matches!(predicate, 1 | 2 | 5 | 6) && (is_nan32(a_bits) || is_nan32(b_bits)))
        {
            status |= Status::INVALID_OP;
        }
        put32(
            &mut o,
            x,
            if cmp32(f32::from_bits(a_bits), f32::from_bits(b_bits), predicate) {
                u32::MAX
            } else {
                0
            },
        );
    }
    publish_fp_status(cpu, status, denormal)?;
    cpu.sse.xmm[xmm(i.op0_register())?] = o;
    Ok(())
}
fn compare64<M: PagedMemory>(cpu: &mut Cpu, i: &Instruction, mem: &mut M, n: usize) -> Result<()> {
    let a = cpu.sse.xmm[xmm(i.op0_register())?];
    let b = source(cpu, i, mem, 8 * n)?;
    let mut o = a;
    let mut status = Status::OK;
    let mut denormal = false;
    let daz = cpu.sse.mxcsr & (1 << 6) != 0;
    let predicate = i.immediate8() & 7;
    for x in 0..n {
        let (a_bits, a_denormal) = daz64(lane64(a, x), daz);
        let (b_bits, b_denormal) = daz64(lane64(b, x), daz);
        denormal |= a_denormal || b_denormal;
        if is_signaling_nan64(a_bits)
            || is_signaling_nan64(b_bits)
            || (matches!(predicate, 1 | 2 | 5 | 6) && (is_nan64(a_bits) || is_nan64(b_bits)))
        {
            status |= Status::INVALID_OP;
        }
        put64(
            &mut o,
            x,
            if cmp64(f64::from_bits(a_bits), f64::from_bits(b_bits), predicate) {
                u64::MAX
            } else {
                0
            },
        );
    }
    publish_fp_status(cpu, status, denormal)?;
    cpu.sse.xmm[xmm(i.op0_register())?] = o;
    Ok(())
}
fn unpack<M: PagedMemory>(
    cpu: &mut Cpu,
    i: &Instruction,
    mem: &mut M,
    w: usize,
    high: bool,
) -> Result<()> {
    let a = cpu.sse.xmm[xmm(i.op0_register())?];
    let b = source(cpu, i, mem, 16)?;
    let lane_bits = w * 8;
    let n = 16 / w;
    let start = if high { n / 2 } else { 0 };
    let mut o = 0;
    for x in 0..n / 2 {
        let shift = lane_bits * (2 * x);
        let av = (a >> ((start + x) * lane_bits)) & mask(w);
        let bv = (b >> ((start + x) * lane_bits)) & mask(w);
        o |= av << shift;
        o |= bv << (shift + lane_bits);
    }
    cpu.sse.xmm[xmm(i.op0_register())?] = o;
    Ok(())
}
fn shufps<M: PagedMemory>(cpu: &mut Cpu, i: &Instruction, mem: &mut M) -> Result<()> {
    let a = cpu.sse.xmm[xmm(i.op0_register())?];
    let b = source(cpu, i, mem, 16)?;
    let q = i.immediate8();
    let mut o = 0;
    for x in 0..2 {
        put32(&mut o, x, lane32(a, ((q >> (2 * x)) & 3) as usize));
    }
    for x in 2..4 {
        put32(&mut o, x, lane32(b, ((q >> (2 * x)) & 3) as usize));
    }
    cpu.sse.xmm[xmm(i.op0_register())?] = o;
    Ok(())
}
fn shufpd<M: PagedMemory>(cpu: &mut Cpu, i: &Instruction, mem: &mut M) -> Result<()> {
    let a = cpu.sse.xmm[xmm(i.op0_register())?];
    let b = source(cpu, i, mem, 16)?;
    let q = i.immediate8();
    let mut o = 0;
    put64(&mut o, 0, lane64(a, (q & 1) as usize));
    put64(&mut o, 1, lane64(b, ((q >> 1) & 1) as usize));
    cpu.sse.xmm[xmm(i.op0_register())?] = o;
    Ok(())
}
fn shift<M: PagedMemory>(cpu: &mut Cpu, i: &Instruction, mem: &mut M) -> Result<()> {
    use Mnemonic::{Pslld, Pslldq, Psllq, Psllw, Psrad, Psraw, Psrld, Psrldq, Psrlq, Psrlw};
    let a = cpu.sse.xmm[xmm(i.op0_register())?];
    let c = if i.op1_kind() == OpKind::Immediate8 {
        i.immediate8() as u64
    } else {
        source(cpu, i, mem, 16)? as u64
    };
    let mut o = 0;
    let (w, left, arith, bytes) = match i.mnemonic() {
        Psllw => (16, true, false, 0),
        Pslld => (32, true, false, 0),
        Psllq => (64, true, false, 0),
        Psrlw => (16, false, false, 0),
        Psrld => (32, false, false, 0),
        Psrlq => (64, false, false, 0),
        Psraw => (16, false, true, 0),
        Psrad => (32, false, true, 0),
        Pslldq => (0, true, false, 1),
        Psrldq => (0, false, false, 1),
        _ => unreachable!(),
    };
    if bytes != 0 {
        o = if c >= 16 {
            0
        } else if left {
            a << (c * 8)
        } else {
            a >> (c * 8)
        }
    } else {
        for x in 0..128 / w {
            let v = (a >> (x * w)) & bit_mask(w);
            let r = if c >= w as u64 {
                if arith && ((v >> (w - 1)) != 0) {
                    bit_mask(w)
                } else {
                    0
                }
            } else if arith {
                (((v as i128) << (128 - w)) >> (128 - w) >> c) as u128 & bit_mask(w)
            } else if left {
                // Mask to the lane: an unmasked `v << c` lets the bits carried
                // out of this lane OR into the next one, which is a whole
                // register shift rather than a per-lane one.
                (v << c) & bit_mask(w)
            } else {
                v >> c
            };
            o |= r << (x * w);
        }
    }
    cpu.sse.xmm[xmm(i.op0_register())?] = o;
    Ok(())
}
fn convert<M: PagedMemory>(cpu: &mut Cpu, i: &Instruction, mem: &mut M) -> Result<()> {
    use Mnemonic::{
        Cvtdq2pd, Cvtdq2ps, Cvtpd2dq, Cvtpd2pi, Cvtpd2ps, Cvtpi2pd, Cvtpi2ps, Cvtps2dq, Cvtps2pd,
        Cvtps2pi, Cvtsd2si, Cvtsd2ss, Cvtsi2sd, Cvtsi2ss, Cvtss2sd, Cvtss2si, Cvttpd2dq, Cvttpd2pi,
        Cvttps2dq, Cvttps2pi, Cvttsd2si, Cvttss2si,
    };
    let d = xmm(i.op0_register()).ok();
    match i.mnemonic() {
        Cvtpi2ps => {
            let mmx_source = is_mmx(i.op1_register());
            let source = source(cpu, i, mem, 8)?;
            let destination = d.expect("CVTPI2PS has an XMM destination");
            let mut output = cpu.sse.xmm[destination];
            let mut status = Status::OK;
            for lane in 0..2 {
                let result = Single::from_i128_r(
                    lane32(source, lane) as i32 as i128,
                    apfloat_round(cpu.sse.rounding()),
                );
                status |= result.status;
                put32(&mut output, lane, result.value.to_bits() as u32);
            }
            publish_fp_status(cpu, status, false)?;
            cpu.sse.xmm[destination] = output;
            if mmx_source {
                cpu.fpu.enter_mmx_state();
            }
            Ok(())
        }
        Cvtpi2pd => {
            let mmx_source = is_mmx(i.op1_register());
            let source = source(cpu, i, mem, 8)?;
            let mut output = 0u128;
            for lane in 0..2 {
                put64(
                    &mut output,
                    lane,
                    (lane32(source, lane) as i32 as f64).to_bits(),
                );
            }
            cpu.sse.xmm[d.expect("CVTPI2PD has an XMM destination")] = output;
            if mmx_source {
                cpu.fpu.enter_mmx_state();
            }
            Ok(())
        }
        Cvtps2pi | Cvttps2pi => {
            let source = source(cpu, i, mem, 8)?;
            let truncate = i.mnemonic() == Cvttps2pi;
            let mut output = 0u64;
            let mut status = Status::OK;
            let mut denormal = false;
            for lane in 0..2 {
                let (value, lane_status, lane_denormal) = single_to_i32(
                    lane32(source, lane),
                    truncate,
                    cpu.sse.rounding(),
                    cpu.sse.mxcsr & (1 << 6) != 0,
                );
                status |= lane_status;
                denormal |= lane_denormal;
                output |= u64::from(value as u32) << (lane * 32);
            }
            publish_fp_status(cpu, status, denormal)?;
            cpu.fpu.enter_mmx_state();
            cpu.fpu.write_mmx_register(mmx(i.op0_register())?, output);
            Ok(())
        }
        Cvtpd2pi | Cvttpd2pi => {
            let source = source(cpu, i, mem, 16)?;
            let truncate = i.mnemonic() == Cvttpd2pi;
            let mut output = 0u64;
            let mut status = Status::OK;
            let mut denormal = false;
            for lane in 0..2 {
                let (value, lane_status, lane_denormal) = double_to_i32(
                    lane64(source, lane),
                    truncate,
                    cpu.sse.rounding(),
                    cpu.sse.mxcsr & (1 << 6) != 0,
                );
                status |= lane_status;
                denormal |= lane_denormal;
                output |= u64::from(value as u32) << (lane * 32);
            }
            publish_fp_status(cpu, status, denormal)?;
            cpu.fpu.enter_mmx_state();
            cpu.fpu.write_mmx_register(mmx(i.op0_register())?, output);
            Ok(())
        }
        Cvtdq2ps => {
            let b = source(cpu, i, mem, 16)?;
            let mut o = 0;
            let mut status = Status::OK;
            for x in 0..4 {
                let result = Single::from_i128_r(
                    lane32(b, x) as i32 as i128,
                    apfloat_round(cpu.sse.rounding()),
                );
                status |= result.status;
                put32(&mut o, x, result.value.to_bits() as u32);
            }
            publish_fp_status(cpu, status, false)?;
            cpu.sse.xmm[d.unwrap()] = o;
            Ok(())
        }
        Cvtps2dq | Cvttps2dq => {
            let b = source(cpu, i, mem, 16)?;
            let mut o = 0;
            let mut status = Status::OK;
            let mut denormal = false;
            for x in 0..4 {
                let (value, lane_status, lane_denormal) = single_to_i32(
                    lane32(b, x),
                    i.mnemonic() == Cvttps2dq,
                    cpu.sse.rounding(),
                    cpu.sse.mxcsr & (1 << 6) != 0,
                );
                status |= lane_status;
                denormal |= lane_denormal;
                put32(&mut o, x, value as u32);
            }
            publish_fp_status(cpu, status, denormal)?;
            cpu.sse.xmm[d.unwrap()] = o;
            Ok(())
        }
        Cvtps2pd => {
            let b = source(cpu, i, mem, 8)?;
            let mut o = 0;
            let mut status = Status::OK;
            let mut denormal = false;
            let daz = cpu.sse.mxcsr & (1 << 6) != 0;
            for x in 0..2 {
                let (bits, lane_denormal) = daz32(lane32(b, x), daz);
                denormal |= lane_denormal;
                let mut loses_info = false;
                let result: rustc_apfloat::StatusAnd<Double> = Single::from_bits(u128::from(bits))
                    .convert_r(apfloat_round(cpu.sse.rounding()), &mut loses_info);
                status |= result.status;
                put64(&mut o, x, result.value.to_bits() as u64);
            }
            publish_fp_status(cpu, status, denormal)?;
            cpu.sse.xmm[d.unwrap()] = o;
            Ok(())
        }
        Cvtpd2ps => {
            let b = source(cpu, i, mem, 16)?;
            let mut o = 0;
            let mut status = Status::OK;
            let mut denormal = false;
            let daz = cpu.sse.mxcsr & (1 << 6) != 0;
            let ftz = cpu.sse.mxcsr & (1 << 15) != 0;
            for x in 0..2 {
                let (bits, lane_denormal) = daz64(lane64(b, x), daz);
                denormal |= lane_denormal;
                let mut loses_info = false;
                let result: rustc_apfloat::StatusAnd<Single> = Double::from_bits(u128::from(bits))
                    .convert_r(apfloat_round(cpu.sse.rounding()), &mut loses_info);
                status |= result.status;
                put32(
                    &mut o,
                    x,
                    ftz32(result.value.to_bits() as u32, result.status, ftz),
                );
            }
            publish_fp_status(cpu, status, denormal)?;
            cpu.sse.xmm[d.unwrap()] = o;
            Ok(())
        }
        Cvtdq2pd => {
            let b = source(cpu, i, mem, 8)?;
            let mut o = 0;
            for x in 0..2 {
                put64(&mut o, x, (lane32(b, x) as i32 as f64).to_bits())
            }
            cpu.sse.xmm[d.unwrap()] = o;
            Ok(())
        }
        Cvtpd2dq | Cvttpd2dq => {
            let b = source(cpu, i, mem, 16)?;
            let mut o = 0;
            let mut status = Status::OK;
            let mut denormal = false;
            for x in 0..2 {
                let (value, lane_status, lane_denormal) = double_to_i32(
                    lane64(b, x),
                    i.mnemonic() == Cvttpd2dq,
                    cpu.sse.rounding(),
                    cpu.sse.mxcsr & (1 << 6) != 0,
                );
                status |= lane_status;
                denormal |= lane_denormal;
                put32(&mut o, x, value as u32);
            }
            publish_fp_status(cpu, status, denormal)?;
            cpu.sse.xmm[d.unwrap()] = o;
            Ok(())
        }
        Cvtsi2ss | Cvtsi2sd => {
            let v = if i.op1_kind() == OpKind::Register {
                read_register(cpu, i.op1_register()) as i32
            } else {
                source(cpu, i, mem, 4)? as i32
            };
            let r = d.unwrap();
            if i.mnemonic() == Cvtsi2ss {
                let result = Single::from_i128_r(v as i128, apfloat_round(cpu.sse.rounding()));
                publish_fp_status(cpu, result.status, false)?;
                put32(&mut cpu.sse.xmm[r], 0, result.value.to_bits() as u32)
            } else {
                let result = Double::from_i128_r(v as i128, apfloat_round(cpu.sse.rounding()));
                publish_fp_status(cpu, result.status, false)?;
                put64(&mut cpu.sse.xmm[r], 0, result.value.to_bits() as u64)
            }
            Ok(())
        }
        Cvtss2si | Cvttss2si => {
            let bits = if i.op1_kind() == OpKind::Register {
                lane32(cpu.sse.xmm[xmm(i.op1_register())?], 0)
            } else {
                source(cpu, i, mem, 4)? as u32
            };
            let (value, status, denormal) = single_to_i32(
                bits,
                i.mnemonic() == Cvttss2si,
                cpu.sse.rounding(),
                cpu.sse.mxcsr & (1 << 6) != 0,
            );
            publish_fp_status(cpu, status, denormal)?;
            set_gpr(cpu, i.op0_register(), value as u32)
        }
        Cvtsd2si | Cvttsd2si => {
            let bits = if i.op1_kind() == OpKind::Register {
                lane64(cpu.sse.xmm[xmm(i.op1_register())?], 0)
            } else {
                source(cpu, i, mem, 8)? as u64
            };
            let (value, status, denormal) = double_to_i32(
                bits,
                i.mnemonic() == Cvttsd2si,
                cpu.sse.rounding(),
                cpu.sse.mxcsr & (1 << 6) != 0,
            );
            publish_fp_status(cpu, status, denormal)?;
            set_gpr(cpu, i.op0_register(), value as u32)
        }
        Cvtss2sd => {
            let b = source(cpu, i, mem, 4)?;
            let (bits, denormal) = daz32(b as u32, cpu.sse.mxcsr & (1 << 6) != 0);
            let mut loses_info = false;
            let result: rustc_apfloat::StatusAnd<Double> = Single::from_bits(u128::from(bits))
                .convert_r(apfloat_round(cpu.sse.rounding()), &mut loses_info);
            publish_fp_status(cpu, result.status, denormal)?;
            put64(
                &mut cpu.sse.xmm[d.unwrap()],
                0,
                result.value.to_bits() as u64,
            );
            Ok(())
        }
        Cvtsd2ss => {
            let b = source(cpu, i, mem, 8)?;
            let (bits, denormal) = daz64(b as u64, cpu.sse.mxcsr & (1 << 6) != 0);
            let mut loses_info = false;
            let result: rustc_apfloat::StatusAnd<Single> = Double::from_bits(u128::from(bits))
                .convert_r(apfloat_round(cpu.sse.rounding()), &mut loses_info);
            publish_fp_status(cpu, result.status, denormal)?;
            put32(
                &mut cpu.sse.xmm[d.unwrap()],
                0,
                ftz32(
                    result.value.to_bits() as u32,
                    result.status,
                    cpu.sse.mxcsr & (1 << 15) != 0,
                ),
            );
            Ok(())
        }
        _ => Err(unsupported(i)),
    }
}
