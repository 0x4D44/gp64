//! Shared interpreter semantics for the architectural SSE XMM register file.
//!
//! Arithmetic uses `rustc_apfloat` where guest rounding and exception status
//! affect the result. Integer and bitwise lanes remain ordinary fixed-width
//! Rust operations so the compiler can inline and unroll the hot path.

use crate::addressing::{
    calc_sse_effective_address, read_rmw_sse_operand_bytes, read_sse_operand_bytes,
    write_sse_operand_bytes,
};
use crate::sse::SseRounding;
use crate::sse_decode::{SseInstr, SseMemory, SseOp, SseOperand};
use crate::{Cpu, CpuError, PageAccess, PagedMemory, Result};
use ieee_apsqrt::sqrt_accurate;
use rustc_apfloat::ieee::{Double, Quad, Single};
use rustc_apfloat::{Float, FloatConvert, Round, Status};

/// Architectural extension required by an instruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SseLevel {
    Sse,
    Sse2,
    Fxsr,
}

/// Register-register PADDD semantics.
#[inline]
pub fn paddd_registers(cpu: &mut Cpu, destination: usize, source: usize) {
    let left = cpu.sse.xmm[destination];
    let right = cpu.sse.xmm[source];
    let mut result = 0u128;
    for lane in 0..4 {
        let value = lane32(left, lane).wrapping_add(lane32(right, lane));
        put32(&mut result, lane, value);
    }
    cpu.sse.xmm[destination] = result;
}

// The value helpers below are private. The frozen oracle in
// `crate::sse_exec_reference` (test-only) carries its own verbatim copy of the
// pre-port versions rather than importing these, which is what makes the
// old-vs-new differential a genuine oracle for a rewrite of the arithmetic
// itself (lane arrays, host intrinsics); sharing them would only have compared
// dispatch and operand handling.

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

#[derive(Clone, Copy, Debug)]
pub(crate) enum FloatOperation {
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
pub(crate) fn is_denormal32(bits: u32) -> bool {
    bits & 0x7f80_0000 == 0 && bits & 0x007f_ffff != 0
}

#[inline]
pub(crate) fn is_denormal64(bits: u64) -> bool {
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

/// Translate an apfloat status plus the separately tracked denormal-operand
/// observation into the six MXCSR status bits.
///
/// The host kernels in `sse_host_kernels` produce those bits directly, which is
/// why this mapping and the sticky-OR below are separate functions: the two
/// backends meet at the bit pattern, and everything after that point — the
/// sticky OR, the mask test, the fault identity — is shared code neither can
/// diverge in.
pub(crate) fn status_bits(status: Status, denormal: bool) -> u32 {
    let mut flags = 0u32;
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
    flags
}

/// The Underflow bit an UNMASKED underflow adds beyond the masked rule.
///
/// x86 has two underflow rules, and only one of them is IEEE's default. With
/// UM **masked**, Underflow is signalled when the result is tiny AND inexact.
/// With UM **unmasked**, it is signalled whenever the result is tiny, exact or
/// not. `rustc_apfloat` implements the masked rule only, so an exactly
/// representable tiny result — `2^-100 * 2^-49` in single precision — carried
/// no UE, and a guest that unmasked UE never took the #XM silicon raises
/// (MDD-BUG-CRUCIBLE-00774).
///
/// A delivered non-zero subnormal is exactly the gap: a tiny inexact result
/// already carries UE from apfloat, and a tiny result that rounds to zero is
/// inexact, so it does too. The check can therefore never invent UE for a
/// result that is not tiny.
///
/// KNOWN REMAINING GAP: x86 detects tininess BEFORE rounding, so a result
/// just below the smallest normal that rounds UP to it is tiny to silicon
/// while the delivered value is normal. apfloat cannot report the pre-rounding
/// exponent, so neither rule sees it here. That is a narrower, separate
/// defect from the one this record names.
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

/// The Overflow bit an overflow that CLAMPED never reported.
///
/// IEEE-754 7.4 signals overflow on the result computed with an UNBOUNDED
/// exponent range, whatever value is finally delivered. Under a rounding mode
/// that rounds toward zero for the result's sign, an overflowing operation
/// delivers the largest finite instead of an infinity, and apfloat then
/// reports only Inexact - so MXCSR.OE never went sticky and an unmasked OE
/// never faulted (MDD-BUG-CRUCIBLE-00785).
///
/// The delivered value cannot decide it alone, and assuming otherwise is the
/// trap here. `max + max` and `max + tiny` BOTH deliver the largest finite,
/// inexact, under round-toward-negative - and silicon raises Overflow for the
/// first and not the second. What separates them is where the exact result
/// sits: at source precision there is no representable value between the
/// largest finite and `2^(emax+1)`, so an exact result below that threshold
/// rounds back onto the largest finite and never overflowed, while one at or
/// above it rounds to something larger and did.
///
/// So the cheap test above is only a pre-filter, and the operation is redone
/// in binary128 - wide enough in both exponent and precision to hold the
/// exact result for either source width - to decide. That costs a second
/// arithmetic only for a result sitting exactly on the clamp.
pub(crate) fn clamped_overflow_bit(
    left: u128,
    right: u128,
    output: u128,
    lanes: usize,
    double: bool,
    operation: FloatOperation,
    mxcsr: u32,
    flags: u32,
) -> u32 {
    const OVERFLOW_FLAG: u32 = 1 << 3;
    const PRECISION_FLAG: u32 = 1 << 5;
    // An exact result never overflowed: the clamp is what makes it inexact.
    if flags & PRECISION_FLAG == 0 {
        return 0;
    }
    let rounding = SseRounding::from_mxcsr(mxcsr);
    let round = apfloat_round(rounding);
    let overflowed = (0..lanes).any(|lane| {
        let (magnitude, negative, max) = if double {
            let bits = lane64(output, lane);
            (
                bits & 0x7fff_ffff_ffff_ffff,
                bits >> 63 != 0,
                0x7fef_ffff_ffff_ffff,
            )
        } else {
            let bits = u64::from(lane32(output, lane));
            (bits & 0x7fff_ffff, bits >> 31 != 0, 0x7f7f_ffff)
        };
        if magnitude != max {
            return false;
        }
        // Only a mode that rounds toward zero FOR THIS SIGN can clamp; the
        // other direction reaches the largest finite from below instead.
        let clamping = match rounding {
            SseRounding::TowardZero => true,
            SseRounding::Down => !negative,
            SseRounding::Up => negative,
            SseRounding::Nearest => false,
        };
        if !clamping {
            return false;
        }
        let (wide_left, wide_right) = if double {
            (widen64(lane64(left, lane)), widen64(lane64(right, lane)))
        } else {
            (widen32(lane32(left, lane)), widen32(lane32(right, lane)))
        };
        let exact = match operation {
            FloatOperation::Add => wide_left.add_r(wide_right, round).value,
            FloatOperation::Subtract => wide_left.sub_r(wide_right, round).value,
            FloatOperation::Multiply => wide_left.mul_r(wide_right, round).value,
            FloatOperation::Divide => wide_left.div_r(wide_right, round).value,
        };
        // `2^(emax+1)`: the first value a source-precision significand can
        // reach above the largest finite.
        let threshold = if double { 1024 } else { 128 };
        exact.is_finite() && !exact.is_zero() && exact.ilogb() >= threshold
    });
    if overflowed {
        OVERFLOW_FLAG
    } else {
        0
    }
}

/// One single-precision lane as binary128, exactly.
fn widen32(bits: u32) -> Quad {
    Single::from_bits(u128::from(bits))
        .convert(&mut false)
        .value
}

/// One double-precision lane as binary128, exactly.
fn widen64(bits: u64) -> Quad {
    Double::from_bits(u128::from(bits))
        .convert(&mut false)
        .value
}

fn publish_fp_status(cpu: &mut Cpu, status: Status, denormal: bool) -> Result<()> {
    publish_fp_status_raw(cpu, status_bits(status, denormal))
}

/// Make one instruction's exceptions architecturally visible.
///
/// Callers must reach this *before* committing the destination: an unmasked
/// exception leaves the destination untouched, which is expressed here by
/// returning `Err` and there by not writing.
fn publish_fp_status_raw(cpu: &mut Cpu, flags: u32) -> Result<()> {
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

// Float-to-int32 conversions report Invalid and Precision only, never
// Denormal: the SDM lists no Denormal exception for CVT(T)SS2SI/SD2SI,
// CVT(T)PS2PI/PD2PI or CVT(T)PS2DQ/PD2DQ, and Intel (i7-14700KF, Core Ultra 7
// 270K Plus) and AMD (Ryzen 9 9950X3D) silicon leave DE clear for a denormal
// source (MDD-BUG-FLU-02382). DAZ still flushes the input first.
fn single_to_i32(bits: u32, truncating: bool, rounding: SseRounding, daz: bool) -> (i32, Status) {
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
    (output, result.status)
}

fn double_to_i32(bits: u64, truncating: bool, rounding: SseRounding, daz: bool) -> (i32, Status) {
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
    (output, result.status)
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

// -------------------------------------------------------------------------
// Lane arrays
// -------------------------------------------------------------------------
//
// The packed-integer helpers below work on fixed-size lane arrays rather than
// shifting lanes in and out of a `u128`. Each body is one straight
// element-wise loop over a `[T; N]`, which is the shape LLVM turns into a
// single host SIMD instruction; the `u128` <-> array conversions are bitcasts
// on a little-endian host.
//
// The frozen oracle in `sse_exec_reference` still carries the u128 shift/mask
// originals, so the migration differential compares the two implementations
// directly.

macro_rules! lane_array {
    ($split:ident, $join:ident, $lane:ty, $count:expr) => {
        #[inline]
        fn $split(value: u128) -> [$lane; $count] {
            let bytes = value.to_le_bytes();
            let mut lanes = [0; $count];
            for (lane, chunk) in bytes
                .chunks_exact(core::mem::size_of::<$lane>())
                .enumerate()
            {
                lanes[lane] = <$lane>::from_le_bytes(chunk.try_into().expect("lane bytes"));
            }
            lanes
        }

        #[inline]
        fn $join(lanes: [$lane; $count]) -> u128 {
            let mut bytes = [0u8; 16];
            for (lane, chunk) in bytes
                .chunks_exact_mut(core::mem::size_of::<$lane>())
                .enumerate()
            {
                chunk.copy_from_slice(&lanes[lane].to_le_bytes());
            }
            u128::from_le_bytes(bytes)
        }
    };
}

lane_array!(split8, join8, u8, 16);
lane_array!(split16, join16, u16, 8);
lane_array!(split32, join32, u32, 4);
lane_array!(split64, join64, u64, 2);

/// Element-wise combine of two lane arrays.
macro_rules! zip_lanes {
    ($split:ident, $join:ident, $count:expr, $a:expr, $b:expr, $op:expr) => {{
        let left = $split($a);
        let right = $split($b);
        let mut out = [0; $count];
        for lane in 0..$count {
            out[lane] = $op(left[lane], right[lane]);
        }
        $join(out)
    }};
}

// -------------------------------------------------------------------------
// Host SIMD for the lanes a straight loop cannot express
// -------------------------------------------------------------------------
//
// Horizontal reductions, saturating packs and the sign-bit gather have no
// element-wise form, so LLVM cannot vectorize them out of a loop. Those get
// `core::arch::x86_64` intrinsics; SSE2 is baseline on x86-64, so no runtime
// detection is involved and the portable lane loop stays as the arm for every
// other host (HLD D5). MM-form operands arrive here already zero-extended into
// the low half of a `u128`, so the host register file is never MMX (D8).

#[cfg(target_arch = "x86_64")]
#[inline]
pub(crate) fn to_m128i(value: u128) -> core::arch::x86_64::__m128i {
    // SAFETY: `u128` and `__m128i` are both 16-byte plain data, and the
    // unaligned load imposes no alignment requirement on the temporary.
    unsafe { core::arch::x86_64::_mm_loadu_si128(core::ptr::addr_of!(value).cast()) }
}

#[cfg(target_arch = "x86_64")]
#[inline]
pub(crate) fn from_m128i(value: core::arch::x86_64::__m128i) -> u128 {
    let mut out = 0u128;
    // SAFETY: as above, in the other direction.
    unsafe { core::arch::x86_64::_mm_storeu_si128(core::ptr::addr_of_mut!(out).cast(), value) };
    out
}

#[cfg(all(target_arch = "aarch64", target_endian = "little"))]
#[inline]
pub(crate) fn to_vq(value: u128) -> core::arch::aarch64::uint8x16_t {
    // SAFETY: reads 16 initialized bytes from a live local; the explicit
    // little-endian byte order is the executor's u128 representation by
    // definition (and this arm only mounts little-endian).
    unsafe { core::arch::aarch64::vld1q_u8(value.to_le_bytes().as_ptr()) }
}

#[cfg(all(target_arch = "aarch64", target_endian = "little"))]
#[inline]
pub(crate) fn from_vq(value: core::arch::aarch64::uint8x16_t) -> u128 {
    let mut bytes = [0u8; 16];
    // SAFETY: writes 16 bytes into a live 16-byte local.
    unsafe { core::arch::aarch64::vst1q_u8(bytes.as_mut_ptr(), value) };
    u128::from_le_bytes(bytes)
}

/// Packed integer add or subtract, optionally signed-saturating.
///
/// Saturation is only encodable at 8 and 16 bits (PADDS\*/PSUBS\*); the wider
/// saturating arms exist only to keep the match total.
#[inline]
fn packed_int(a: u128, b: u128, width: usize, signed_saturating: bool, subtract: bool) -> u128 {
    macro_rules! add_sub {
        ($split:ident, $join:ident, $count:expr, $unsigned:ty, $signed:ty) => {
            match (signed_saturating, subtract) {
                (false, false) => {
                    zip_lanes!($split, $join, $count, a, b, |x: $unsigned, y: $unsigned| x
                        .wrapping_add(y))
                }
                (false, true) => {
                    zip_lanes!($split, $join, $count, a, b, |x: $unsigned, y: $unsigned| x
                        .wrapping_sub(y))
                }
                (true, false) => {
                    zip_lanes!($split, $join, $count, a, b, |x: $unsigned, y: $unsigned| {
                        (x as $signed).saturating_add(y as $signed) as $unsigned
                    })
                }
                (true, true) => {
                    zip_lanes!($split, $join, $count, a, b, |x: $unsigned, y: $unsigned| {
                        (x as $signed).saturating_sub(y as $signed) as $unsigned
                    })
                }
            }
        };
    }
    debug_assert!(matches!(width, 8 | 16 | 32 | 64), "packed lane width");
    match width {
        8 => add_sub!(split8, join8, 16, u8, i8),
        16 => add_sub!(split16, join16, 8, u16, i16),
        32 => add_sub!(split32, join32, 4, u32, i32),
        _ => add_sub!(split64, join64, 2, u64, i64),
    }
}

/// PADDUS\*/PSUBUS\*: unsigned saturating add or subtract, 8- or 16-bit lanes.
#[inline]
fn packed_unsigned_sat(a: u128, b: u128, width: usize, subtract: bool) -> u128 {
    debug_assert!(matches!(width, 8 | 16), "unsigned saturation lane width");
    match (width, subtract) {
        (8, false) => zip_lanes!(split8, join8, 16, a, b, |x: u8, y: u8| x.saturating_add(y)),
        (8, true) => zip_lanes!(split8, join8, 16, a, b, |x: u8, y: u8| x.saturating_sub(y)),
        (_, false) => {
            zip_lanes!(split16, join16, 8, a, b, |x: u16, y: u16| x
                .saturating_add(y))
        }
        (_, true) => zip_lanes!(split16, join16, 8, a, b, |x: u16, y: u16| x
            .saturating_sub(y)),
    }
}

/// PCMPEQ\*/PCMPGT\*: an all-ones lane where the predicate holds, else zero.
/// The greater-than form is always signed.
#[inline]
fn compare_integer(a: u128, b: u128, width: usize, greater: bool) -> u128 {
    debug_assert!(matches!(width, 8 | 16 | 32), "compare lane width");
    match (width, greater) {
        (8, false) => zip_lanes!(split8, join8, 16, a, b, |x: u8, y: u8| if x == y {
            u8::MAX
        } else {
            0
        }),
        (8, true) => zip_lanes!(
            split8,
            join8,
            16,
            a,
            b,
            |x: u8, y: u8| if (x as i8) > (y as i8) { u8::MAX } else { 0 }
        ),
        (16, false) => zip_lanes!(split16, join16, 8, a, b, |x: u16, y: u16| if x == y {
            u16::MAX
        } else {
            0
        }),
        (16, true) => {
            zip_lanes!(
                split16,
                join16,
                8,
                a,
                b,
                |x: u16, y: u16| if (x as i16) > (y as i16) { u16::MAX } else { 0 }
            )
        }
        (_, false) => zip_lanes!(split32, join32, 4, a, b, |x: u32, y: u32| if x == y {
            u32::MAX
        } else {
            0
        }),
        (_, true) => {
            zip_lanes!(
                split32,
                join32,
                4,
                a,
                b,
                |x: u32, y: u32| if (x as i32) > (y as i32) { u32::MAX } else { 0 }
            )
        }
    }
}

/// PMINUB.
#[inline]
fn min_unsigned_bytes(a: u128, b: u128) -> u128 {
    zip_lanes!(split8, join8, 16, a, b, |x: u8, y: u8| x.min(y))
}

/// PMAXUB.
#[inline]
fn max_unsigned_bytes(a: u128, b: u128) -> u128 {
    zip_lanes!(split8, join8, 16, a, b, |x: u8, y: u8| x.max(y))
}

/// PMINSW.
#[inline]
fn min_signed_words(a: u128, b: u128) -> u128 {
    zip_lanes!(
        split16,
        join16,
        8,
        a,
        b,
        |x: u16, y: u16| (x as i16).min(y as i16) as u16
    )
}

/// PMAXSW.
#[inline]
fn max_signed_words(a: u128, b: u128) -> u128 {
    zip_lanes!(
        split16,
        join16,
        8,
        a,
        b,
        |x: u16, y: u16| (x as i16).max(y as i16) as u16
    )
}

/// PAVGB: unsigned rounding average.
#[inline]
fn average_bytes(a: u128, b: u128) -> u128 {
    zip_lanes!(split8, join8, 16, a, b, |x: u8, y: u8| {
        ((u16::from(x) + u16::from(y) + 1) >> 1) as u8
    })
}

/// PAVGW: unsigned rounding average.
#[inline]
fn average_words(a: u128, b: u128) -> u128 {
    zip_lanes!(split16, join16, 8, a, b, |x: u16, y: u16| {
        ((u32::from(x) + u32::from(y) + 1) >> 1) as u16
    })
}

/// PMULLW: the low half of the product, where signedness does not matter.
#[inline]
fn multiply_low_words(a: u128, b: u128) -> u128 {
    zip_lanes!(split16, join16, 8, a, b, |x: u16, y: u16| x.wrapping_mul(y))
}

/// PMULHW: the high half of the signed product.
#[inline]
fn multiply_high_signed_words(a: u128, b: u128) -> u128 {
    zip_lanes!(split16, join16, 8, a, b, |x: u16, y: u16| {
        ((i32::from(x as i16) * i32::from(y as i16)) >> 16) as u16
    })
}

/// PMULHUW: the high half of the unsigned product.
#[inline]
fn multiply_high_unsigned_words(a: u128, b: u128) -> u128 {
    zip_lanes!(split16, join16, 8, a, b, |x: u16, y: u16| {
        ((u32::from(x) * u32::from(y)) >> 16) as u16
    })
}

/// PACKSSWB: signed-saturate both sources' words to bytes, `a` low.
#[cfg(target_arch = "x86_64")]
#[inline]
fn pack_signed_words(a: u128, b: u128) -> u128 {
    // SAFETY: SSE2 is baseline on x86-64.
    from_m128i(unsafe { core::arch::x86_64::_mm_packs_epi16(to_m128i(a), to_m128i(b)) })
}

/// PACKSSWB on NEON: SQXTN each source's words to saturated bytes.
#[cfg(all(target_arch = "aarch64", target_endian = "little"))]
#[inline]
fn pack_signed_words(a: u128, b: u128) -> u128 {
    use core::arch::aarch64::*;
    // SAFETY: NEON is baseline on aarch64.
    unsafe {
        let low = vqmovn_s16(vreinterpretq_s16_u8(to_vq(a)));
        let high = vqmovn_s16(vreinterpretq_s16_u8(to_vq(b)));
        from_vq(vreinterpretq_u8_s8(vcombine_s8(low, high)))
    }
}

#[cfg(not(any(
    target_arch = "x86_64",
    all(target_arch = "aarch64", target_endian = "little")
)))]
fn pack_signed_words(a: u128, b: u128) -> u128 {
    let mut out = [0u8; 16];
    for (half, source) in [a, b].into_iter().enumerate() {
        let lanes = split16(source);
        for lane in 0..8 {
            out[half * 8 + lane] = (lanes[lane] as i16).clamp(-128, 127) as u8;
        }
    }
    join8(out)
}

/// PACKSSDW: signed-saturate both sources' dwords to words, `a` low.
#[cfg(target_arch = "x86_64")]
#[inline]
fn pack_signed_dwords(a: u128, b: u128) -> u128 {
    // SAFETY: SSE2 is baseline on x86-64.
    from_m128i(unsafe { core::arch::x86_64::_mm_packs_epi32(to_m128i(a), to_m128i(b)) })
}

/// PACKSSDW on NEON: SQXTN each source's dwords to saturated words.
#[cfg(all(target_arch = "aarch64", target_endian = "little"))]
#[inline]
fn pack_signed_dwords(a: u128, b: u128) -> u128 {
    use core::arch::aarch64::*;
    // SAFETY: NEON is baseline on aarch64.
    unsafe {
        let low = vqmovn_s32(vreinterpretq_s32_u8(to_vq(a)));
        let high = vqmovn_s32(vreinterpretq_s32_u8(to_vq(b)));
        from_vq(vreinterpretq_u8_s16(vcombine_s16(low, high)))
    }
}

#[cfg(not(any(
    target_arch = "x86_64",
    all(target_arch = "aarch64", target_endian = "little")
)))]
fn pack_signed_dwords(a: u128, b: u128) -> u128 {
    let mut out = [0u16; 8];
    for (half, source) in [a, b].into_iter().enumerate() {
        let lanes = split32(source);
        for lane in 0..4 {
            out[half * 4 + lane] = (lanes[lane] as i32).clamp(-32768, 32767) as u16;
        }
    }
    join16(out)
}

/// PACKUSWB: unsigned-saturate both sources' signed words to bytes, `a` low.
#[cfg(target_arch = "x86_64")]
#[inline]
fn pack_unsigned_words(a: u128, b: u128) -> u128 {
    // SAFETY: SSE2 is baseline on x86-64.
    from_m128i(unsafe { core::arch::x86_64::_mm_packus_epi16(to_m128i(a), to_m128i(b)) })
}

/// PACKUSWB on NEON: SQXTUN each source's signed words to unsigned bytes.
#[cfg(all(target_arch = "aarch64", target_endian = "little"))]
#[inline]
fn pack_unsigned_words(a: u128, b: u128) -> u128 {
    use core::arch::aarch64::*;
    // SAFETY: NEON is baseline on aarch64.
    unsafe {
        let low = vqmovun_s16(vreinterpretq_s16_u8(to_vq(a)));
        let high = vqmovun_s16(vreinterpretq_s16_u8(to_vq(b)));
        from_vq(vcombine_u8(low, high))
    }
}

#[cfg(not(any(
    target_arch = "x86_64",
    all(target_arch = "aarch64", target_endian = "little")
)))]
fn pack_unsigned_words(a: u128, b: u128) -> u128 {
    let mut out = [0u8; 16];
    for (half, source) in [a, b].into_iter().enumerate() {
        let lanes = split16(source);
        for lane in 0..8 {
            out[half * 8 + lane] = (lanes[lane] as i16).clamp(0, 255) as u8;
        }
    }
    join8(out)
}

/// PSADBW: the two 8-byte halves' sums of absolute byte differences, each
/// zero-extended into the low word of its 64-bit lane. A horizontal
/// reduction, so there is no element-wise loop for LLVM to vectorize.
#[cfg(target_arch = "x86_64")]
#[inline]
fn sum_absolute_byte_differences(a: u128, b: u128) -> u128 {
    // SAFETY: SSE2 is baseline on x86-64.
    from_m128i(unsafe { core::arch::x86_64::_mm_sad_epu8(to_m128i(a), to_m128i(b)) })
}

/// PSADBW on NEON: absolute byte differences, then three pairwise widening
/// adds fold each 8-byte half into its own 64-bit lane.
#[cfg(all(target_arch = "aarch64", target_endian = "little"))]
#[inline]
fn sum_absolute_byte_differences(a: u128, b: u128) -> u128 {
    use core::arch::aarch64::*;
    // SAFETY: NEON is baseline on aarch64.
    unsafe {
        let differences = vabdq_u8(to_vq(a), to_vq(b));
        let sums = vpaddlq_u32(vpaddlq_u16(vpaddlq_u8(differences)));
        from_vq(vreinterpretq_u8_u64(sums))
    }
}

#[cfg(not(any(
    target_arch = "x86_64",
    all(target_arch = "aarch64", target_endian = "little")
)))]
fn sum_absolute_byte_differences(a: u128, b: u128) -> u128 {
    let left = a.to_le_bytes();
    let right = b.to_le_bytes();
    let mut sums = [0u64; 2];
    for lane in 0..16 {
        sums[lane / 8] += u64::from(left[lane].abs_diff(right[lane]));
    }
    u128::from(sums[0]) | (u128::from(sums[1]) << 64)
}

/// PMADDWD: signed word products summed in adjacent pairs, wrapping into each
/// dword lane. Pairwise, so again not an element-wise loop.
#[cfg(target_arch = "x86_64")]
#[inline]
fn multiply_add_words(a: u128, b: u128) -> u128 {
    // SAFETY: SSE2 is baseline on x86-64.
    from_m128i(unsafe { core::arch::x86_64::_mm_madd_epi16(to_m128i(a), to_m128i(b)) })
}

/// PMADDWD on NEON: widening word products, then one pairwise add lands
/// each adjacent pair's wrapping sum in its dword lane.
#[cfg(all(target_arch = "aarch64", target_endian = "little"))]
#[inline]
fn multiply_add_words(a: u128, b: u128) -> u128 {
    use core::arch::aarch64::*;
    // SAFETY: NEON is baseline on aarch64.
    unsafe {
        let left = vreinterpretq_s16_u8(to_vq(a));
        let right = vreinterpretq_s16_u8(to_vq(b));
        let low = vmull_s16(vget_low_s16(left), vget_low_s16(right));
        let high = vmull_high_s16(left, right);
        from_vq(vreinterpretq_u8_s32(vpaddq_s32(low, high)))
    }
}

#[cfg(not(any(
    target_arch = "x86_64",
    all(target_arch = "aarch64", target_endian = "little")
)))]
fn multiply_add_words(a: u128, b: u128) -> u128 {
    let left = split16(a);
    let right = split16(b);
    let mut out = [0u32; 4];
    for lane in 0..4 {
        let low = i32::from(left[2 * lane] as i16) * i32::from(right[2 * lane] as i16);
        let high = i32::from(left[2 * lane + 1] as i16) * i32::from(right[2 * lane + 1] as i16);
        out[lane] = low.wrapping_add(high) as u32;
    }
    join32(out)
}

/// PMOVMSKB: gather the 16 byte sign bits into the low 16 bits of a GPR.
#[cfg(target_arch = "x86_64")]
#[inline]
fn byte_sign_mask(value: u128) -> u32 {
    // SAFETY: SSE2 is baseline on x86-64.
    unsafe { core::arch::x86_64::_mm_movemask_epi8(to_m128i(value)) as u32 }
}

/// PMOVMSKB on NEON: shift each byte's sign bit to bit 0, weight the lanes
/// by their bit position, and sum each half (the standard movemask shape).
#[cfg(all(target_arch = "aarch64", target_endian = "little"))]
#[inline]
fn byte_sign_mask(value: u128) -> u32 {
    use core::arch::aarch64::*;
    const WEIGHTS: [u8; 16] = [1, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128];
    // SAFETY: NEON is baseline on aarch64.
    unsafe {
        let signs = vshrq_n_u8(to_vq(value), 7);
        let weighted = vmulq_u8(signs, vld1q_u8(WEIGHTS.as_ptr()));
        let low = u32::from(vaddv_u8(vget_low_u8(weighted)));
        let high = u32::from(vaddv_u8(vget_high_u8(weighted)));
        low | (high << 8)
    }
}

#[cfg(not(any(
    target_arch = "x86_64",
    all(target_arch = "aarch64", target_endian = "little")
)))]
fn byte_sign_mask(value: u128) -> u32 {
    value
        .to_le_bytes()
        .into_iter()
        .enumerate()
        .fold(0u32, |mask, (lane, byte)| {
            mask | (u32::from(byte >> 7) << lane)
        })
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

// =========================================================================
// Executor
// =========================================================================
//
// Operands come from a decoded [`SseInstr`]. Because [`SseOp`] is per
// encoding form, every choice the pre-port executor made by probing an
// operand at run time (its `is_mmx` and memory-operand-kind tests) is static
// in the variant here. The pre-port executor is frozen as the migration
// oracle in `sse_exec_reference`, which the tests below diff against.

#[inline]
fn sse_operand_error(instr: &SseInstr, what: &str) -> CpuError {
    CpuError::Unimplemented(format!("SSE/SSE2 {} {what}", instr.op.name()))
}

#[inline]
fn xmm_operand(instr: &SseInstr, operand: SseOperand) -> Result<usize> {
    match operand {
        SseOperand::Xmm(index) => Ok(usize::from(index)),
        _ => Err(sse_operand_error(instr, "expects an XMM operand")),
    }
}

#[inline]
fn mm_operand(instr: &SseInstr, operand: SseOperand) -> Result<usize> {
    match operand {
        SseOperand::Mm(index) => Ok(usize::from(index)),
        _ => Err(sse_operand_error(instr, "expects an MMX operand")),
    }
}

#[inline]
fn gpr_operand(instr: &SseInstr, operand: SseOperand) -> Result<usize> {
    match operand {
        SseOperand::Gpr(index) => Ok(usize::from(index)),
        _ => Err(sse_operand_error(instr, "expects a GPR operand")),
    }
}

#[inline]
fn memory_of(instr: &SseInstr) -> Result<&SseMemory> {
    instr
        .operands
        .memory_operand()
        .ok_or_else(|| sse_operand_error(instr, "expects a memory operand"))
}

/// The GPR operands of this instruction set are all 32-bit (`r/m32`, `r32`),
/// including PINSRW's `r32/m16` source, whose register form reads a full
/// doubleword and truncates.
#[inline]
fn read_gpr32(cpu: &Cpu, index: usize) -> u32 {
    match index {
        0 => cpu.regs.eax,
        1 => cpu.regs.ecx,
        2 => cpu.regs.edx,
        3 => cpu.regs.ebx,
        4 => cpu.regs.esp,
        5 => cpu.regs.ebp,
        6 => cpu.regs.esi,
        _ => cpu.regs.edi,
    }
}

#[inline]
fn write_gpr32(cpu: &mut Cpu, index: usize, value: u32) {
    match index {
        0 => cpu.regs.eax = value,
        1 => cpu.regs.ecx = value,
        2 => cpu.regs.edx = value,
        3 => cpu.regs.ebx = value,
        4 => cpu.regs.esp = value,
        5 => cpu.regs.ebp = value,
        6 => cpu.regs.esi = value,
        _ => cpu.regs.edi = value,
    }
}

/// Stage the source operand, masked to `bytes`.
fn source_value<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &SseInstr,
    mem: &mut M,
    bytes: usize,
) -> Result<u128> {
    match instr.operands.src {
        SseOperand::Xmm(index) => Ok(cpu.sse.xmm[usize::from(index)] & mask(bytes)),
        SseOperand::Mm(index) => {
            Ok(u128::from(cpu.fpu.read_mmx_register_committed(usize::from(index))) & mask(bytes))
        }
        SseOperand::Memory => {
            let mut buf = [0u8; 16];
            read_sse_operand_bytes(cpu, memory_of(instr)?, mem, &mut buf[..bytes])?;
            Ok(from_bytes(&buf[..bytes]))
        }
        _ => Err(sse_operand_error(instr, "has no readable source")),
    }
}

/// The destination register's current value and the width its register file
/// writes back, mirroring the old executor's `is_mmx(op0)` probe.
fn destination_register(cpu: &mut Cpu, instr: &SseInstr) -> Result<(u128, usize)> {
    match instr.operands.dst {
        SseOperand::Xmm(index) => Ok((cpu.sse.xmm[usize::from(index)], 16)),
        SseOperand::Mm(index) => Ok((
            u128::from(cpu.fpu.read_mmx_register_committed(usize::from(index))),
            8,
        )),
        _ => Err(sse_operand_error(instr, "has no register destination")),
    }
}

fn write_destination<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &SseInstr,
    mem: &mut M,
    value: u128,
    bytes: usize,
) -> Result<()> {
    match instr.operands.dst {
        SseOperand::Xmm(index) => {
            let index = usize::from(index);
            cpu.sse.xmm[index] = if bytes == 16 {
                value
            } else {
                (cpu.sse.xmm[index] & !mask(bytes)) | (value & mask(bytes))
            };
            Ok(())
        }
        SseOperand::Mm(index) => {
            cpu.fpu.enter_mmx_state();
            cpu.fpu.write_mmx_register(usize::from(index), value as u64);
            Ok(())
        }
        SseOperand::Memory => {
            let raw = to_bytes(value);
            write_sse_operand_bytes(cpu, memory_of(instr)?, mem, &raw[..bytes])
        }
        _ => Err(sse_operand_error(instr, "has no writable destination")),
    }
}

/// #GP when the encoding's memory operand is misaligned, checked after the
/// segment, access, and paging checks the address calculation performs.
fn require_aligned_operand<M: PagedMemory>(
    cpu: &Cpu,
    instr: &SseInstr,
    mem: &mut M,
    alignment: usize,
) -> Result<()> {
    if let Some(memory) = instr.operands.memory_operand() {
        let access = if matches!(instr.operands.dst, SseOperand::Memory) {
            PageAccess::Write
        } else {
            PageAccess::Read
        };
        if calc_sse_effective_address(cpu, memory, mem, access)? & (alignment - 1) != 0 {
            return Err(CpuError::GeneralProtectionFault);
        }
    }
    Ok(())
}

/// Execute one decoded SSE/SSE2 instruction.
///
/// The caller has already performed CPU feature and CR0/CR4 gating. Sources
/// are staged before destinations, so register aliases and memory faults do
/// not publish partial XMM state.
pub fn execute<M: PagedMemory>(cpu: &mut Cpu, instr: &SseInstr, mem: &mut M) -> Result<()> {
    use SseOp as O;
    match instr.op {
        // ---- 16-byte moves; the aligned class checks alignment -----------
        O::MovapsLoad
        | O::MovapsStore
        | O::MovapdLoad
        | O::MovapdStore
        | O::MovdqaLoad
        | O::MovdqaStore
        | O::Movntps
        | O::Movntpd
        | O::Movntdq
        | O::MovupsLoad
        | O::MovupsStore
        | O::MovupdLoad
        | O::MovupdStore
        | O::MovdquLoad
        | O::MovdquStore => {
            if instr.op.requires_16_byte_alignment() {
                require_aligned_operand(cpu, instr, mem, 16)?;
            }
            let value = source_value(cpu, instr, mem, 16)?;
            write_destination(cpu, instr, mem, value, 16)
        }
        O::Movntq => {
            let value = u128::from(
                cpu.fpu
                    .read_mmx_register_committed(mm_operand(instr, instr.operands.src)?),
            );
            write_destination(cpu, instr, mem, value, 8)?;
            cpu.fpu.enter_mmx_state();
            Ok(())
        }
        O::Movnti => {
            let value = read_gpr32(cpu, gpr_operand(instr, instr.operands.src)?);
            write_destination(cpu, instr, mem, u128::from(value), 4)
        }
        // ---- scalar moves -----------------------------------------------
        O::MovssLoad | O::MovsdLoad => {
            let bytes = if instr.op == O::MovssLoad { 4 } else { 8 };
            let value = source_value(cpu, instr, mem, bytes)?;
            let destination = xmm_operand(instr, instr.operands.dst)?;
            // A memory source replaces the whole register; a register source
            // merges into it.
            cpu.sse.xmm[destination] = if matches!(instr.operands.src, SseOperand::Memory) {
                value
            } else {
                (cpu.sse.xmm[destination] & !mask(bytes)) | value
            };
            Ok(())
        }
        O::MovssStore | O::MovsdStore => {
            let bytes = if instr.op == O::MovssStore { 4 } else { 8 };
            if matches!(instr.operands.dst, SseOperand::Memory) {
                let value = cpu.sse.xmm[xmm_operand(instr, instr.operands.src)?];
                return write_destination(cpu, instr, mem, value, bytes);
            }
            let value = source_value(cpu, instr, mem, bytes)?;
            let destination = xmm_operand(instr, instr.operands.dst)?;
            cpu.sse.xmm[destination] = (cpu.sse.xmm[destination] & !mask(bytes)) | value;
            Ok(())
        }
        O::MovlpsLoad | O::MovlpdLoad => {
            let value = source_value(cpu, instr, mem, 8)?;
            write_destination(cpu, instr, mem, value, 8)
        }
        O::MovhpsLoad | O::MovhpdLoad => {
            let value = source_value(cpu, instr, mem, 8)?;
            let destination = xmm_operand(instr, instr.operands.dst)?;
            cpu.sse.xmm[destination] = (cpu.sse.xmm[destination] & mask(8)) | (value << 64);
            Ok(())
        }
        O::MovlpsStore | O::MovlpdStore => {
            let value = cpu.sse.xmm[xmm_operand(instr, instr.operands.src)?] & mask(8);
            write_destination(cpu, instr, mem, value, 8)
        }
        O::MovhpsStore | O::MovhpdStore => {
            let value = cpu.sse.xmm[xmm_operand(instr, instr.operands.src)?] >> 64;
            write_destination(cpu, instr, mem, value, 8)
        }
        O::Movhlps => {
            let source = cpu.sse.xmm[xmm_operand(instr, instr.operands.src)?];
            let destination = xmm_operand(instr, instr.operands.dst)?;
            cpu.sse.xmm[destination] = (cpu.sse.xmm[destination] & !mask(8)) | (source >> 64);
            Ok(())
        }
        O::Movlhps => {
            let source = cpu.sse.xmm[xmm_operand(instr, instr.operands.src)?];
            let destination = xmm_operand(instr, instr.operands.dst)?;
            cpu.sse.xmm[destination] =
                (cpu.sse.xmm[destination] & mask(8)) | ((source & mask(8)) << 64);
            Ok(())
        }
        O::MovdXmmLoad => {
            let value = match instr.operands.src {
                SseOperand::Gpr(index) => u128::from(read_gpr32(cpu, usize::from(index))),
                _ => source_value(cpu, instr, mem, 4)?,
            };
            write_destination(cpu, instr, mem, value, 16)
        }
        O::MovdXmmStore => {
            let value = cpu.sse.xmm[xmm_operand(instr, instr.operands.src)?] & mask(4);
            match instr.operands.dst {
                SseOperand::Gpr(index) => {
                    write_gpr32(cpu, usize::from(index), value as u32);
                    Ok(())
                }
                _ => write_destination(cpu, instr, mem, value, 4),
            }
        }
        O::MovqXmmLoad => {
            let value = source_value(cpu, instr, mem, 8)?;
            write_destination(cpu, instr, mem, value, 16)
        }
        O::MovqStore => {
            let value = cpu.sse.xmm[xmm_operand(instr, instr.operands.src)?] & mask(8);
            // An XMM destination takes the whole register, zero-extending.
            let bytes = if matches!(instr.operands.dst, SseOperand::Memory) {
                8
            } else {
                16
            };
            write_destination(cpu, instr, mem, value, bytes)
        }
        O::Movdq2q => {
            let value = cpu.sse.xmm[xmm_operand(instr, instr.operands.src)?] as u64;
            cpu.fpu.enter_mmx_state();
            cpu.fpu
                .write_mmx_register(mm_operand(instr, instr.operands.dst)?, value);
            Ok(())
        }
        O::Movq2dq => {
            let value = cpu
                .fpu
                .read_mmx_register_committed(mm_operand(instr, instr.operands.src)?);
            cpu.sse.xmm[xmm_operand(instr, instr.operands.dst)?] = u128::from(value);
            cpu.fpu.enter_mmx_state();
            Ok(())
        }
        O::Maskmovq | O::Maskmovdqu => maskmov_instr(cpu, instr, mem),
        // ---- control and state ------------------------------------------
        O::Ldmxcsr => {
            let mut bytes = [0u8; 4];
            read_sse_operand_bytes(cpu, memory_of(instr)?, mem, &mut bytes)?;
            let value = u32::from_le_bytes(bytes);
            if value & !cpu.sse.mxcsr_mask != 0 {
                Err(CpuError::GeneralProtectionFault)
            } else {
                cpu.sse.mxcsr = value;
                Ok(())
            }
        }
        O::Stmxcsr => write_destination(cpu, instr, mem, u128::from(cpu.sse.mxcsr), 4),
        O::Fxsave => fxsave_instr(cpu, instr, mem),
        O::Fxrstor => fxrstor_instr(cpu, instr, mem),
        // ---- bitwise -----------------------------------------------------
        O::Andps | O::Andpd | O::Pand => binary_instr(cpu, instr, mem, |a, b| a & b),
        O::Orps | O::Orpd | O::Por => binary_instr(cpu, instr, mem, |a, b| a | b),
        O::Xorps | O::Xorpd | O::Pxor => binary_instr(cpu, instr, mem, |a, b| a ^ b),
        O::Andnps | O::Andnpd | O::Pandn => binary_instr(cpu, instr, mem, |a, b| (!a) & b),
        // ---- packed and scalar float arithmetic --------------------------
        O::Addps => float_binary_instr(cpu, instr, mem, false, 4, FloatOperation::Add),
        O::Addss => float_binary_instr(cpu, instr, mem, false, 1, FloatOperation::Add),
        O::Subps => float_binary_instr(cpu, instr, mem, false, 4, FloatOperation::Subtract),
        O::Subss => float_binary_instr(cpu, instr, mem, false, 1, FloatOperation::Subtract),
        O::Mulps => float_binary_instr(cpu, instr, mem, false, 4, FloatOperation::Multiply),
        O::Mulss => float_binary_instr(cpu, instr, mem, false, 1, FloatOperation::Multiply),
        O::Divps => float_binary_instr(cpu, instr, mem, false, 4, FloatOperation::Divide),
        O::Divss => float_binary_instr(cpu, instr, mem, false, 1, FloatOperation::Divide),
        O::Minps => minmax_float_instr(cpu, instr, mem, false, 4, false),
        O::Minss => minmax_float_instr(cpu, instr, mem, false, 1, false),
        O::Maxps => minmax_float_instr(cpu, instr, mem, false, 4, true),
        O::Maxss => minmax_float_instr(cpu, instr, mem, false, 1, true),
        O::Sqrtps => square_root_instr(cpu, instr, mem, false, 4),
        O::Sqrtss => square_root_instr(cpu, instr, mem, false, 1),
        O::Addpd => float_binary_instr(cpu, instr, mem, true, 2, FloatOperation::Add),
        O::Addsd => float_binary_instr(cpu, instr, mem, true, 1, FloatOperation::Add),
        O::Subpd => float_binary_instr(cpu, instr, mem, true, 2, FloatOperation::Subtract),
        O::Subsd => float_binary_instr(cpu, instr, mem, true, 1, FloatOperation::Subtract),
        O::Mulpd => float_binary_instr(cpu, instr, mem, true, 2, FloatOperation::Multiply),
        O::Mulsd => float_binary_instr(cpu, instr, mem, true, 1, FloatOperation::Multiply),
        O::Divpd => float_binary_instr(cpu, instr, mem, true, 2, FloatOperation::Divide),
        O::Divsd => float_binary_instr(cpu, instr, mem, true, 1, FloatOperation::Divide),
        O::Minpd => minmax_float_instr(cpu, instr, mem, true, 2, false),
        O::Minsd => minmax_float_instr(cpu, instr, mem, true, 1, false),
        O::Maxpd => minmax_float_instr(cpu, instr, mem, true, 2, true),
        O::Maxsd => minmax_float_instr(cpu, instr, mem, true, 1, true),
        O::Sqrtpd => square_root_instr(cpu, instr, mem, true, 2),
        O::Sqrtsd => square_root_instr(cpu, instr, mem, true, 1),
        O::Rcpps => unary_instr(cpu, instr, mem, 16, |a| fp32(a, 0, 4, |x, _| x.recip())),
        O::Rcpss => unary_instr(cpu, instr, mem, 4, |a| fp32(a, 0, 1, |x, _| x.recip())),
        O::Rsqrtps => unary_instr(cpu, instr, mem, 16, |a| {
            fp32(a, 0, 4, |x, _| x.sqrt().recip())
        }),
        O::Rsqrtss => unary_instr(cpu, instr, mem, 4, |a| {
            fp32(a, 0, 1, |x, _| x.sqrt().recip())
        }),
        // ---- packed integer arithmetic -----------------------------------
        O::Paddb => intop_instr(cpu, instr, mem, 8, false, false),
        O::Paddw => intop_instr(cpu, instr, mem, 16, false, false),
        O::Paddd => intop_instr(cpu, instr, mem, 32, false, false),
        O::Paddq => intop_instr(cpu, instr, mem, 64, false, false),
        O::Psubb => intop_instr(cpu, instr, mem, 8, false, true),
        O::Psubw => intop_instr(cpu, instr, mem, 16, false, true),
        O::Psubd => intop_instr(cpu, instr, mem, 32, false, true),
        O::Psubq | O::PsubqMm => intop_instr(cpu, instr, mem, 64, false, true),
        O::Paddsb => intop_instr(cpu, instr, mem, 8, true, false),
        O::Paddsw => intop_instr(cpu, instr, mem, 16, true, false),
        O::Psubsb => intop_instr(cpu, instr, mem, 8, true, true),
        O::Psubsw => intop_instr(cpu, instr, mem, 16, true, true),
        O::Paddusb => uintsat_instr(cpu, instr, mem, 8, false),
        O::Paddusw => uintsat_instr(cpu, instr, mem, 16, false),
        O::Psubusb => uintsat_instr(cpu, instr, mem, 8, true),
        O::Psubusw => uintsat_instr(cpu, instr, mem, 16, true),
        O::Pcmpeqb => binary_instr(cpu, instr, mem, |a, b| compare_integer(a, b, 8, false)),
        O::Pcmpeqw => binary_instr(cpu, instr, mem, |a, b| compare_integer(a, b, 16, false)),
        O::Pcmpeqd => binary_instr(cpu, instr, mem, |a, b| compare_integer(a, b, 32, false)),
        O::Pcmpgtb => binary_instr(cpu, instr, mem, |a, b| compare_integer(a, b, 8, true)),
        O::Pcmpgtw => binary_instr(cpu, instr, mem, |a, b| compare_integer(a, b, 16, true)),
        O::Pcmpgtd => binary_instr(cpu, instr, mem, |a, b| compare_integer(a, b, 32, true)),
        O::Pminub | O::PminubMm => binary_instr(cpu, instr, mem, min_unsigned_bytes),
        O::Pmaxub | O::PmaxubMm => binary_instr(cpu, instr, mem, max_unsigned_bytes),
        O::Pminsw | O::PminswMm => binary_instr(cpu, instr, mem, min_signed_words),
        O::Pmaxsw | O::PmaxswMm => binary_instr(cpu, instr, mem, max_signed_words),
        O::Pavgb | O::PavgbMm => binary_instr(cpu, instr, mem, average_bytes),
        O::Pavgw | O::PavgwMm => binary_instr(cpu, instr, mem, average_words),
        O::Pmullw => binary_instr(cpu, instr, mem, multiply_low_words),
        O::Pmulhw => binary_instr(cpu, instr, mem, multiply_high_signed_words),
        O::Pmulhuw | O::PmulhuwMm => binary_instr(cpu, instr, mem, multiply_high_unsigned_words),
        O::Packsswb => binary_instr(cpu, instr, mem, pack_signed_words),
        O::Packssdw => binary_instr(cpu, instr, mem, pack_signed_dwords),
        O::Packuswb => binary_instr(cpu, instr, mem, pack_unsigned_words),
        // ---- float compares ----------------------------------------------
        O::Cmpps => compare32_instr(cpu, instr, mem, 4),
        O::Cmpss => compare32_instr(cpu, instr, mem, 1),
        O::Cmppd => compare64_instr(cpu, instr, mem, 2),
        O::Cmpsd => compare64_instr(cpu, instr, mem, 1),
        O::Comiss | O::Ucomiss | O::Comisd | O::Ucomisd => compare_flags_instr(cpu, instr, mem),
        // ---- interleave and shuffle ---------------------------------------
        O::Unpcklps => unpack_instr(cpu, instr, mem, 4, false),
        O::Unpckhps => unpack_instr(cpu, instr, mem, 4, true),
        O::Unpcklpd => unpack_instr(cpu, instr, mem, 8, false),
        O::Unpckhpd => unpack_instr(cpu, instr, mem, 8, true),
        O::Punpcklbw => unpack_instr(cpu, instr, mem, 1, false),
        O::Punpcklwd => unpack_instr(cpu, instr, mem, 2, false),
        O::Punpckldq => unpack_instr(cpu, instr, mem, 4, false),
        O::Punpcklqdq => unpack_instr(cpu, instr, mem, 8, false),
        O::Punpckhbw => unpack_instr(cpu, instr, mem, 1, true),
        O::Punpckhwd => unpack_instr(cpu, instr, mem, 2, true),
        O::Punpckhdq => unpack_instr(cpu, instr, mem, 4, true),
        O::Punpckhqdq => unpack_instr(cpu, instr, mem, 8, true),
        O::Shufps => shufps_instr(cpu, instr, mem),
        O::Shufpd => shufpd_instr(cpu, instr, mem),
        O::Pshufw | O::Pshufd | O::Pshufhw | O::Pshuflw => pshuffle_instr(cpu, instr, mem),
        // ---- horizontal integer and lane extraction ------------------------
        O::Pmaddwd => pmaddwd_instr(cpu, instr, mem),
        O::Pmuludq | O::PmuludqMm => pmuludq_instr(cpu, instr, mem),
        O::Psadbw | O::PsadbwMm => psadbw_instr(cpu, instr, mem),
        O::Movmskps | O::Movmskpd | O::PmovmskbMm | O::PmovmskbXmm => move_mask_instr(cpu, instr),
        O::PinsrwMm | O::PinsrwXmm => pinsrw_instr(cpu, instr, mem),
        O::PextrwMm | O::PextrwXmm => pextrw_instr(cpu, instr),
        // ---- shifts ---------------------------------------------------------
        O::PsllwImm => shift_instr(cpu, instr, u64::from(instr.operands.imm8), 16, true, false),
        O::PslldImm => shift_instr(cpu, instr, u64::from(instr.operands.imm8), 32, true, false),
        O::PsllqImm => shift_instr(cpu, instr, u64::from(instr.operands.imm8), 64, true, false),
        O::PsrlwImm => shift_instr(cpu, instr, u64::from(instr.operands.imm8), 16, false, false),
        O::PsrldImm => shift_instr(cpu, instr, u64::from(instr.operands.imm8), 32, false, false),
        O::PsrlqImm => shift_instr(cpu, instr, u64::from(instr.operands.imm8), 64, false, false),
        O::PsrawImm => shift_instr(cpu, instr, u64::from(instr.operands.imm8), 16, false, true),
        O::PsradImm => shift_instr(cpu, instr, u64::from(instr.operands.imm8), 32, false, true),
        O::PslldqImm => shift_bytes_instr(cpu, instr, u64::from(instr.operands.imm8), true),
        O::PsrldqImm => shift_bytes_instr(cpu, instr, u64::from(instr.operands.imm8), false),
        O::Psllw => {
            let count = source_value(cpu, instr, mem, 16)? as u64;
            shift_instr(cpu, instr, count, 16, true, false)
        }
        O::Pslld => {
            let count = source_value(cpu, instr, mem, 16)? as u64;
            shift_instr(cpu, instr, count, 32, true, false)
        }
        O::Psllq => {
            let count = source_value(cpu, instr, mem, 16)? as u64;
            shift_instr(cpu, instr, count, 64, true, false)
        }
        O::Psrlw => {
            let count = source_value(cpu, instr, mem, 16)? as u64;
            shift_instr(cpu, instr, count, 16, false, false)
        }
        O::Psrld => {
            let count = source_value(cpu, instr, mem, 16)? as u64;
            shift_instr(cpu, instr, count, 32, false, false)
        }
        O::Psrlq => {
            let count = source_value(cpu, instr, mem, 16)? as u64;
            shift_instr(cpu, instr, count, 64, false, false)
        }
        O::Psraw => {
            let count = source_value(cpu, instr, mem, 16)? as u64;
            shift_instr(cpu, instr, count, 16, false, true)
        }
        O::Psrad => {
            let count = source_value(cpu, instr, mem, 16)? as u64;
            shift_instr(cpu, instr, count, 32, false, true)
        }
        // ---- conversions ------------------------------------------------------
        O::Cvtpi2ps
        | O::Cvtpi2pd
        | O::Cvtps2pi
        | O::Cvttps2pi
        | O::Cvtpd2pi
        | O::Cvttpd2pi
        | O::Cvtdq2ps
        | O::Cvtps2dq
        | O::Cvttps2dq
        | O::Cvtps2pd
        | O::Cvtpd2ps
        | O::Cvtdq2pd
        | O::Cvtpd2dq
        | O::Cvttpd2dq
        | O::Cvtsi2ss
        | O::Cvtsi2sd
        | O::Cvtss2si
        | O::Cvttss2si
        | O::Cvtsd2si
        | O::Cvttsd2si
        | O::Cvtss2sd
        | O::Cvtsd2ss => convert_instr(cpu, instr, mem),
        // ---- no architectural effect ------------------------------------------
        O::Lfence
        | O::Mfence
        | O::Sfence
        | O::Prefetchnta
        | O::Prefetcht0
        | O::Prefetcht1
        | O::Prefetcht2 => Ok(()),
    }
}

fn maskmov_instr<M: PagedMemory>(cpu: &mut Cpu, instr: &SseInstr, mem: &mut M) -> Result<()> {
    let bytes = if instr.op == SseOp::Maskmovq { 8 } else { 16 };
    let data = if bytes == 8 {
        u128::from(
            cpu.fpu
                .read_mmx_register_committed(mm_operand(instr, instr.operands.src)?),
        )
    } else {
        cpu.sse.xmm[xmm_operand(instr, instr.operands.src)?]
    }
    .to_le_bytes();
    let select = if bytes == 8 {
        u128::from(
            cpu.fpu
                .read_mmx_register_committed(mm_operand(instr, instr.operands.src2)?),
        )
    } else {
        cpu.sse.xmm[xmm_operand(instr, instr.operands.src2)?]
    }
    .to_le_bytes();
    let memory = *memory_of(instr)?;
    let mut output = [0u8; 16];
    // A read-modify-write, so a masked-off byte still faults as the write it
    // architecturally is.
    read_rmw_sse_operand_bytes(cpu, &memory, mem, &mut output[..bytes])?;
    for index in 0..bytes {
        if select[index] & 0x80 != 0 {
            output[index] = data[index];
        }
    }
    write_sse_operand_bytes(cpu, &memory, mem, &output[..bytes])?;
    if instr.op == SseOp::Maskmovq {
        cpu.fpu.enter_mmx_state();
    }
    Ok(())
}

fn fxsave_instr<M: PagedMemory>(cpu: &mut Cpu, instr: &SseInstr, mem: &mut M) -> Result<()> {
    debug_assert!(instr.op.requires_16_byte_alignment());
    require_aligned_operand(cpu, instr, mem, 16)?;
    // FXSAVE observes the complete x87 image, including deferred F80 mirrors.
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
    write_sse_operand_bytes(cpu, memory_of(instr)?, mem, &image)
}

fn fxrstor_instr<M: PagedMemory>(cpu: &mut Cpu, instr: &SseInstr, mem: &mut M) -> Result<()> {
    debug_assert!(instr.op.requires_16_byte_alignment());
    require_aligned_operand(cpu, instr, mem, 16)?;
    let mut image = [0u8; 512];
    read_sse_operand_bytes(cpu, memory_of(instr)?, mem, &mut image)?;
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
    // FXRSTOR replaced the raw F80 register file. Its values are now the
    // source of truth; invalidate any old binary64 proofs/mirror state only
    // after the image has passed all validation and has been fully loaded.
    cpu.fpu.invalidate_drc_f80_register_file();
    Ok(())
}

/// The value operation is a generic parameter rather than a `fn` pointer. A
/// pointer forces an indirect call, and the lane loop is then compiled once
/// against the u128-in-GPR calling convention — which is exactly the shape
/// that cannot become a single host SIMD instruction. As a type parameter it
/// inlines into the 16-byte register access at the call site instead.
fn binary_instr<M: PagedMemory, F: FnOnce(u128, u128) -> u128>(
    cpu: &mut Cpu,
    instr: &SseInstr,
    mem: &mut M,
    operation: F,
) -> Result<()> {
    let (left, bytes) = destination_register(cpu, instr)?;
    let right = source_value(cpu, instr, mem, bytes)?;
    write_destination(cpu, instr, mem, operation(left, right), bytes)
}

fn intop_instr<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &SseInstr,
    mem: &mut M,
    width: usize,
    saturating: bool,
    subtract: bool,
) -> Result<()> {
    let (left, bytes) = destination_register(cpu, instr)?;
    let right = source_value(cpu, instr, mem, bytes)?;
    write_destination(
        cpu,
        instr,
        mem,
        packed_int(left, right, width, saturating, subtract),
        bytes,
    )
}

fn uintsat_instr<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &SseInstr,
    mem: &mut M,
    width: usize,
    subtract: bool,
) -> Result<()> {
    let left = cpu.sse.xmm[xmm_operand(instr, instr.operands.dst)?];
    let right = source_value(cpu, instr, mem, 16)?;
    write_destination(
        cpu,
        instr,
        mem,
        packed_unsigned_sat(left, right, width, subtract),
        16,
    )
}

fn unary_instr<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &SseInstr,
    mem: &mut M,
    bytes: usize,
    operation: fn(u128) -> u128,
) -> Result<()> {
    let value = source_value(cpu, instr, mem, bytes)?;
    let destination = xmm_operand(instr, instr.operands.dst)?;
    let output = operation((cpu.sse.xmm[destination] & !mask(bytes)) | value);
    cpu.sse.xmm[destination] = (cpu.sse.xmm[destination] & !mask(bytes)) | (output & mask(bytes));
    Ok(())
}

fn float_binary_instr<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &SseInstr,
    mem: &mut M,
    double: bool,
    lanes: usize,
    operation: FloatOperation,
) -> Result<()> {
    let destination = xmm_operand(instr, instr.operands.dst)?;
    let left = cpu.sse.xmm[destination];
    let lane_bytes = if double { 8 } else { 4 };
    let right = source_value(cpu, instr, mem, lane_bytes * lanes)?;
    let (output, flags) = float_binary_values(left, right, lanes, double, operation, cpu.sse.mxcsr);
    publish_fp_status_raw(cpu, flags)?;
    cpu.sse.xmm[destination] = output;
    Ok(())
}

/// The value and status bits of one ADD/SUB/MUL/DIV, from whichever backend
/// owns this input.
///
/// The host kernel answers first and declines whatever it cannot prove; the
/// software model is the fallback and the authority (HLD D9).
fn float_binary_values(
    left: u128,
    right: u128,
    lanes: usize,
    double: bool,
    operation: FloatOperation,
    mxcsr: u32,
) -> (u128, u32) {
    let (value, status) = float_binary_backend(left, right, lanes, double, operation, mxcsr);
    // BOTH backends answer with the masked underflow rule — the host kernel
    // because the host's own UM is masked while it runs, the software model
    // because that is all apfloat implements — so the unmasked rule is
    // applied once, here, to whichever answered (MDD-BUG-CRUCIBLE-00774).
    (
        value,
        status
            | unmasked_underflow_bit(value, lanes, double, mxcsr)
            | clamped_overflow_bit(left, right, value, lanes, double, operation, mxcsr, status),
    )
}

/// The backend answer, before the unmasked-underflow rule is applied.
fn float_binary_backend(
    left: u128,
    right: u128,
    lanes: usize,
    double: bool,
    operation: FloatOperation,
    mxcsr: u32,
) -> (u128, u32) {
    #[cfg(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", target_endian = "little")
    ))]
    if let Some(promoted) =
        crate::sse_host_kernels::float_binary(left, right, lanes, double, operation, mxcsr)
    {
        return promoted;
    }
    soft_float_binary(left, right, lanes, double, operation, mxcsr)
}

fn soft_float_binary(
    left: u128,
    right: u128,
    lanes: usize,
    double: bool,
    operation: FloatOperation,
    mxcsr: u32,
) -> (u128, u32) {
    let daz = mxcsr & (1 << 6) != 0;
    let ftz = mxcsr & (1 << 15) != 0;
    let rounding = SseRounding::from_mxcsr(mxcsr);
    let (output, status, denormal) = if double {
        apfloat64(left, right, lanes, operation, rounding, daz, ftz)
    } else {
        apfloat32(left, right, lanes, operation, rounding, daz, ftz)
    };
    (output, status_bits(status, denormal))
}

/// CVTPS2DQ / CVTTPS2DQ.
fn packed_to_int32_values(input: u128, truncating: bool, mxcsr: u32) -> (u128, u32) {
    #[cfg(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", target_endian = "little")
    ))]
    if let Some(promoted) = crate::sse_host_kernels::packed_to_int32(input, truncating, mxcsr) {
        return promoted;
    }
    soft_packed_to_int32(input, truncating, mxcsr)
}

fn soft_packed_to_int32(input: u128, truncating: bool, mxcsr: u32) -> (u128, u32) {
    let mut output = 0u128;
    let mut status = Status::OK;
    for lane in 0..4 {
        let (value, lane_status) = single_to_i32(
            lane32(input, lane),
            truncating,
            SseRounding::from_mxcsr(mxcsr),
            mxcsr & (1 << 6) != 0,
        );
        status |= lane_status;
        put32(&mut output, lane, value as u32);
    }
    (output, status_bits(status, false))
}

/// CVTSS2SI / CVTTSS2SI.
fn single_to_int32_values(bits: u32, truncating: bool, mxcsr: u32) -> (i32, u32) {
    #[cfg(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", target_endian = "little")
    ))]
    if let Some(promoted) = crate::sse_host_kernels::single_to_int32(bits, truncating, mxcsr) {
        return promoted;
    }
    soft_single_to_int32(bits, truncating, mxcsr)
}

fn soft_single_to_int32(bits: u32, truncating: bool, mxcsr: u32) -> (i32, u32) {
    let (value, status) = single_to_i32(
        bits,
        truncating,
        SseRounding::from_mxcsr(mxcsr),
        mxcsr & (1 << 6) != 0,
    );
    (value, status_bits(status, false))
}

/// CVTSD2SI / CVTTSD2SI.
fn double_to_int32_values(bits: u64, truncating: bool, mxcsr: u32) -> (i32, u32) {
    #[cfg(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", target_endian = "little")
    ))]
    if let Some(promoted) = crate::sse_host_kernels::double_to_int32(bits, truncating, mxcsr) {
        return promoted;
    }
    soft_double_to_int32(bits, truncating, mxcsr)
}

fn soft_double_to_int32(bits: u64, truncating: bool, mxcsr: u32) -> (i32, u32) {
    let (value, status) = double_to_i32(
        bits,
        truncating,
        SseRounding::from_mxcsr(mxcsr),
        mxcsr & (1 << 6) != 0,
    );
    (value, status_bits(status, false))
}

/// CVTSI2SS / CVTSI2SD. Lane 0 is replaced; the rest of the destination is
/// carried through, which is why the whole register goes in and comes back.
fn int32_to_float_values(destination: u128, value: i32, double: bool, mxcsr: u32) -> (u128, u32) {
    #[cfg(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", target_endian = "little")
    ))]
    if let Some(promoted) =
        crate::sse_host_kernels::int32_to_float(destination, value, double, mxcsr)
    {
        return promoted;
    }
    soft_int32_to_float(destination, value, double, mxcsr)
}

fn soft_int32_to_float(destination: u128, value: i32, double: bool, mxcsr: u32) -> (u128, u32) {
    let mut output = destination;
    let status = if double {
        let result = Double::from_i128_r(
            i128::from(value),
            apfloat_round(SseRounding::from_mxcsr(mxcsr)),
        );
        put64(&mut output, 0, result.value.to_bits() as u64);
        result.status
    } else {
        let result = Single::from_i128_r(
            i128::from(value),
            apfloat_round(SseRounding::from_mxcsr(mxcsr)),
        );
        put32(&mut output, 0, result.value.to_bits() as u32);
        result.status
    };
    (output, status_bits(status, false))
}

fn square_root_instr<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &SseInstr,
    mem: &mut M,
    double: bool,
    lanes: usize,
) -> Result<()> {
    let destination = xmm_operand(instr, instr.operands.dst)?;
    let lane_bytes = if double { 8 } else { 4 };
    let input = source_value(cpu, instr, mem, lane_bytes * lanes)?;
    let (output, flags) = square_root_values(
        cpu.sse.xmm[destination],
        input,
        lanes,
        double,
        cpu.sse.mxcsr,
    );
    publish_fp_status_raw(cpu, flags)?;
    cpu.sse.xmm[destination] = output;
    Ok(())
}

fn square_root_values(
    destination: u128,
    input: u128,
    lanes: usize,
    double: bool,
    mxcsr: u32,
) -> (u128, u32) {
    #[cfg(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", target_endian = "little")
    ))]
    if let Some(promoted) =
        crate::sse_host_kernels::square_root(destination, input, lanes, double, mxcsr)
    {
        return promoted;
    }
    soft_square_root(destination, input, lanes, double, mxcsr)
}

/// `sqrt_accurate` of a denormal, computed in normal range.
///
/// MDD-BUG-CRUCIBLE-00784: `ieee_apsqrt` rounds a denormal square root as
/// inexact even when the exact answer is representable, so under a directed
/// mode it returns one ulp the wrong side of it. Measured on this box's
/// silicon under round-up, `SQRTSS 0x00000002` is `0x1A800000` with DE and no
/// PE; the dependency answered `0x1A800001` with PE. The same call is exact
/// for NORMAL operands in every rounding mode - `sqrt(4.0)`, `sqrt(2^-74)` -
/// so the repair is to hand it a normal operand.
///
/// `x * 2^150` is exact for every single denormal and always normal, and
/// `sqrt(x * 2^150) = sqrt(x) * 2^75`, so undoing it is a subtraction on the
/// result's exponent. The undo cannot underflow: the smallest denormal gives
/// `sqrt(2^-149) * 2^75`, whose biased exponent is 127, well clear of 75.
pub(crate) fn sqrt_denormal32(bits: u32, rounding: Round) -> (u32, Status) {
    debug_assert!(bits & 0x7f80_0000 == 0 && bits & 0x007f_ffff != 0);
    const SCALE_HALVED: u32 = 75;
    let mantissa = bits & 0x007f_ffff;
    // Place the leading set bit at bit 23, the implicit one's position.
    let shift = mantissa.leading_zeros() - 8;
    let scaled = ((127 + 24 - shift) << 23) | ((mantissa << shift) & 0x007f_ffff);
    let (result, _) = sqrt_accurate(scaled, rounding);
    let exponent = (result.value >> 23) & 0xff;
    debug_assert!(exponent > SCALE_HALVED, "the undo stays in normal range");
    let value = (result.value & 0x807f_ffff) | ((exponent - SCALE_HALVED) << 23);
    (value, result.status)
}

/// The double-precision twin of [`sqrt_denormal32`], scaling by `2^1076`.
pub(crate) fn sqrt_denormal64(bits: u64, rounding: Round) -> (u64, Status) {
    debug_assert!(bits & 0x7ff0_0000_0000_0000 == 0 && bits & 0x000f_ffff_ffff_ffff != 0);
    const SCALE_HALVED: u64 = 538;
    let mantissa = bits & 0x000f_ffff_ffff_ffff;
    let shift = u64::from(mantissa.leading_zeros() - 11);
    let scaled = ((1023 + 53 + 1 - shift) << 52) | ((mantissa << shift) & 0x000f_ffff_ffff_ffff);
    let (result, _) = sqrt_accurate(scaled, rounding);
    let exponent = (result.value >> 52) & 0x7ff;
    debug_assert!(exponent > SCALE_HALVED, "the undo stays in normal range");
    let value = (result.value & 0x800f_ffff_ffff_ffff) | ((exponent - SCALE_HALVED) << 52);
    (value, result.status)
}

fn soft_square_root(
    destination: u128,
    input: u128,
    lanes: usize,
    double: bool,
    mxcsr: u32,
) -> (u128, u32) {
    let mut output = destination;
    let mut status = Status::OK;
    let mut denormal = false;
    let daz = mxcsr & (1 << 6) != 0;
    let ftz = mxcsr & (1 << 15) != 0;
    let rounding = apfloat_round(SseRounding::from_mxcsr(mxcsr));
    for lane in 0..lanes {
        if double {
            let (bits, lane_denormal) = daz64(lane64(input, lane), daz);
            denormal |= lane_denormal;
            let (value, lane_status) = if is_denormal64(bits) {
                sqrt_denormal64(bits, rounding)
            } else {
                let (result, _) = sqrt_accurate(bits, rounding);
                (result.value, result.status)
            };
            status |= lane_status;
            put64(&mut output, lane, ftz64(value, lane_status, ftz));
        } else {
            let (bits, lane_denormal) = daz32(lane32(input, lane), daz);
            denormal |= lane_denormal;
            let (value, lane_status) = if is_denormal32(bits) {
                sqrt_denormal32(bits, rounding)
            } else {
                let (result, _) = sqrt_accurate(bits, rounding);
                (result.value, result.status)
            };
            status |= lane_status;
            put32(&mut output, lane, ftz32(value, lane_status, ftz));
        }
    }
    (output, status_bits(status, denormal))
}

fn minmax_float_instr<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &SseInstr,
    mem: &mut M,
    double: bool,
    lanes: usize,
    maximum: bool,
) -> Result<()> {
    let destination = xmm_operand(instr, instr.operands.dst)?;
    let left = cpu.sse.xmm[destination];
    let lane_bytes = if double { 8 } else { 4 };
    let right = source_value(cpu, instr, mem, lane_bytes * lanes)?;
    let (output, flags) = min_max_values(left, right, lanes, double, maximum, cpu.sse.mxcsr);
    publish_fp_status_raw(cpu, flags)?;
    cpu.sse.xmm[destination] = output;
    Ok(())
}

fn min_max_values(
    left: u128,
    right: u128,
    lanes: usize,
    double: bool,
    maximum: bool,
    mxcsr: u32,
) -> (u128, u32) {
    #[cfg(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", target_endian = "little")
    ))]
    if let Some(promoted) =
        crate::sse_host_kernels::min_max(left, right, lanes, double, maximum, mxcsr)
    {
        return promoted;
    }
    soft_min_max(left, right, lanes, double, maximum, mxcsr)
}

/// MIN/MAX raise Invalid on a QUIET NaN as well as a signaling one.
///
/// That is unusual — almost every other SSE arithmetic operation passes a
/// quiet NaN through silently — which is why it is spelled out here. The SDM
/// lists the MINPS/MAXPS/MINSS/MAXSS SIMD floating-point exceptions as
/// "Invalid (including QNaN source operand)", and a probe of this host's
/// silicon agrees: MINPS, MAXPS, MINSS, MINPD and MAXSD each set MXCSR.IE
/// for a quiet-NaN operand in either position, while ADDPS with the same
/// operand leaves IE clear. We signalled only on a signaling NaN, so a guest
/// running with IM unmasked never took the #XM real hardware raises
/// (MDD-BUG-CRUCIBLE-00772).
///
/// The VALUE is unchanged and was already right: with any NaN operand the
/// second (source) operand is returned, which the same probe confirms.
///
/// The host kernels decline every NaN case (`sse_host_kernels::min_max`
/// returns `None` when either side is a NaN), so this is the only model that
/// decides it.
fn soft_min_max(
    left: u128,
    right: u128,
    lanes: usize,
    double: bool,
    maximum: bool,
    mxcsr: u32,
) -> (u128, u32) {
    let daz = mxcsr & (1 << 6) != 0;
    let mut output = left;
    let mut status = Status::OK;
    let mut denormal = false;
    for lane in 0..lanes {
        if double {
            let (left_bits, left_denormal) = daz64(lane64(left, lane), daz);
            let (right_bits, right_denormal) = daz64(lane64(right, lane), daz);
            denormal |= left_denormal || right_denormal;
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
    (output, status_bits(status, denormal))
}

/// CMPPS/CMPPD predicates that raise IE on a quiet NaN as well as a
/// signaling one: LT, LE, NLT, NLE. EQ/UNORD/NEQ/ORD stay quiet.
#[inline]
fn predicate_signals_on_quiet_nan(predicate: u8) -> bool {
    matches!(predicate, 1 | 2 | 5 | 6)
}

fn compare32_instr<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &SseInstr,
    mem: &mut M,
    lanes: usize,
) -> Result<()> {
    let destination = xmm_operand(instr, instr.operands.dst)?;
    let left = cpu.sse.xmm[destination];
    let right = source_value(cpu, instr, mem, 4 * lanes)?;
    let mut output = left;
    let mut status = Status::OK;
    let mut denormal = false;
    let daz = cpu.sse.mxcsr & (1 << 6) != 0;
    let predicate = instr.operands.imm8 & 7;
    for lane in 0..lanes {
        let (left_bits, left_denormal) = daz32(lane32(left, lane), daz);
        let (right_bits, right_denormal) = daz32(lane32(right, lane), daz);
        denormal |= left_denormal || right_denormal;
        if is_signaling_nan32(left_bits)
            || is_signaling_nan32(right_bits)
            || (predicate_signals_on_quiet_nan(predicate)
                && (is_nan32(left_bits) || is_nan32(right_bits)))
        {
            status |= Status::INVALID_OP;
        }
        put32(
            &mut output,
            lane,
            if cmp32(
                f32::from_bits(left_bits),
                f32::from_bits(right_bits),
                predicate,
            ) {
                u32::MAX
            } else {
                0
            },
        );
    }
    publish_fp_status(cpu, status, denormal)?;
    cpu.sse.xmm[destination] = output;
    Ok(())
}

fn compare64_instr<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &SseInstr,
    mem: &mut M,
    lanes: usize,
) -> Result<()> {
    let destination = xmm_operand(instr, instr.operands.dst)?;
    let left = cpu.sse.xmm[destination];
    let right = source_value(cpu, instr, mem, 8 * lanes)?;
    let mut output = left;
    let mut status = Status::OK;
    let mut denormal = false;
    let daz = cpu.sse.mxcsr & (1 << 6) != 0;
    let predicate = instr.operands.imm8 & 7;
    for lane in 0..lanes {
        let (left_bits, left_denormal) = daz64(lane64(left, lane), daz);
        let (right_bits, right_denormal) = daz64(lane64(right, lane), daz);
        denormal |= left_denormal || right_denormal;
        if is_signaling_nan64(left_bits)
            || is_signaling_nan64(right_bits)
            || (predicate_signals_on_quiet_nan(predicate)
                && (is_nan64(left_bits) || is_nan64(right_bits)))
        {
            status |= Status::INVALID_OP;
        }
        put64(
            &mut output,
            lane,
            if cmp64(
                f64::from_bits(left_bits),
                f64::from_bits(right_bits),
                predicate,
            ) {
                u64::MAX
            } else {
                0
            },
        );
    }
    publish_fp_status(cpu, status, denormal)?;
    cpu.sse.xmm[destination] = output;
    Ok(())
}

fn compare_flags_instr<M: PagedMemory>(cpu: &mut Cpu, instr: &SseInstr, mem: &mut M) -> Result<()> {
    let is_double = matches!(instr.op, SseOp::Comisd | SseOp::Ucomisd);
    let ordered_compare = matches!(instr.op, SseOp::Comiss | SseOp::Comisd);
    let left_register = xmm_operand(instr, instr.operands.dst)?;
    let daz = cpu.sse.mxcsr & (1 << 6) != 0;
    let unordered_or_ordering = if is_double {
        let (left_bits, left_denormal) = daz64(lane64(cpu.sse.xmm[left_register], 0), daz);
        let (right_bits, right_denormal) = daz64(source_value(cpu, instr, mem, 8)? as u64, daz);
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
        f64::from_bits(left_bits).partial_cmp(&f64::from_bits(right_bits))
    } else {
        let (left_bits, left_denormal) = daz32(lane32(cpu.sse.xmm[left_register], 0), daz);
        let (right_bits, right_denormal) = daz32(source_value(cpu, instr, mem, 4)? as u32, daz);
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
        f32::from_bits(left_bits).partial_cmp(&f32::from_bits(right_bits))
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

fn unpack_instr<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &SseInstr,
    mem: &mut M,
    width: usize,
    high: bool,
) -> Result<()> {
    let destination = xmm_operand(instr, instr.operands.dst)?;
    let left = cpu.sse.xmm[destination];
    let right = source_value(cpu, instr, mem, 16)?;
    macro_rules! interleave {
        ($split:ident, $join:ident, $count:expr) => {{
            let low = $split(left);
            let high_side = $split(right);
            let start = if high { $count / 2 } else { 0 };
            let mut out = [0; $count];
            for lane in 0..$count / 2 {
                out[2 * lane] = low[start + lane];
                out[2 * lane + 1] = high_side[start + lane];
            }
            $join(out)
        }};
    }
    debug_assert!(matches!(width, 1 | 2 | 4 | 8), "unpack lane width in bytes");
    cpu.sse.xmm[destination] = match width {
        1 => interleave!(split8, join8, 16),
        2 => interleave!(split16, join16, 8),
        4 => interleave!(split32, join32, 4),
        _ => interleave!(split64, join64, 2),
    };
    Ok(())
}

fn shufps_instr<M: PagedMemory>(cpu: &mut Cpu, instr: &SseInstr, mem: &mut M) -> Result<()> {
    let destination = xmm_operand(instr, instr.operands.dst)?;
    let left = cpu.sse.xmm[destination];
    let right = source_value(cpu, instr, mem, 16)?;
    let selector = instr.operands.imm8;
    let mut output = 0u128;
    for lane in 0..2 {
        put32(
            &mut output,
            lane,
            lane32(left, ((selector >> (2 * lane)) & 3) as usize),
        );
    }
    for lane in 2..4 {
        put32(
            &mut output,
            lane,
            lane32(right, ((selector >> (2 * lane)) & 3) as usize),
        );
    }
    cpu.sse.xmm[destination] = output;
    Ok(())
}

fn shufpd_instr<M: PagedMemory>(cpu: &mut Cpu, instr: &SseInstr, mem: &mut M) -> Result<()> {
    let destination = xmm_operand(instr, instr.operands.dst)?;
    let left = cpu.sse.xmm[destination];
    let right = source_value(cpu, instr, mem, 16)?;
    let selector = instr.operands.imm8;
    let mut output = 0u128;
    put64(&mut output, 0, lane64(left, (selector & 1) as usize));
    put64(
        &mut output,
        1,
        lane64(right, ((selector >> 1) & 1) as usize),
    );
    cpu.sse.xmm[destination] = output;
    Ok(())
}

fn pshuffle_instr<M: PagedMemory>(cpu: &mut Cpu, instr: &SseInstr, mem: &mut M) -> Result<()> {
    let bytes = if instr.op == SseOp::Pshufw { 8 } else { 16 };
    let input = source_value(cpu, instr, mem, bytes)?;
    let immediate = instr.operands.imm8;
    // A runtime immediate selects each lane, so this stays a portable
    // lane-select on every host: SSE2 offers no variable byte shuffle, and the
    // PSHUFB form that would need SSSE3 is not built speculatively (HLD D9).
    let selector = |lane: usize| usize::from((immediate >> (lane * 2)) & 3);
    let output = match instr.op {
        SseOp::Pshufd => {
            let lanes = split32(input);
            let mut out = [0u32; 4];
            for lane in 0..4 {
                out[lane] = lanes[selector(lane)];
            }
            join32(out)
        }
        SseOp::Pshufw => {
            let lanes = split16(input);
            let mut out = [0u16; 8];
            for lane in 0..4 {
                out[lane] = lanes[selector(lane)];
            }
            join16(out)
        }
        SseOp::Pshuflw => {
            let lanes = split16(input);
            let mut out = lanes;
            for lane in 0..4 {
                out[lane] = lanes[selector(lane)];
            }
            join16(out)
        }
        // PSHUFHW: the upper four words, selected from the upper four.
        _ => {
            let lanes = split16(input);
            let mut out = lanes;
            for lane in 0..4 {
                out[4 + lane] = lanes[4 + selector(lane)];
            }
            join16(out)
        }
    };
    write_destination(cpu, instr, mem, output, bytes)
}

fn pmaddwd_instr<M: PagedMemory>(cpu: &mut Cpu, instr: &SseInstr, mem: &mut M) -> Result<()> {
    let destination = xmm_operand(instr, instr.operands.dst)?;
    let left = cpu.sse.xmm[destination];
    let right = source_value(cpu, instr, mem, 16)?;
    cpu.sse.xmm[destination] = multiply_add_words(left, right);
    Ok(())
}

fn pmuludq_instr<M: PagedMemory>(cpu: &mut Cpu, instr: &SseInstr, mem: &mut M) -> Result<()> {
    let (left, bytes) = destination_register(cpu, instr)?;
    let right = source_value(cpu, instr, mem, bytes)?;
    let left = split32(left);
    let right = split32(right);
    let low = u64::from(left[0]) * u64::from(right[0]);
    if bytes == 8 {
        return write_destination(cpu, instr, mem, u128::from(low), 8);
    }
    let high = u64::from(left[2]) * u64::from(right[2]);
    write_destination(
        cpu,
        instr,
        mem,
        u128::from(low) | (u128::from(high) << 64),
        16,
    )
}

fn psadbw_instr<M: PagedMemory>(cpu: &mut Cpu, instr: &SseInstr, mem: &mut M) -> Result<()> {
    let (left, bytes) = destination_register(cpu, instr)?;
    let right = source_value(cpu, instr, mem, bytes)?;
    // The MM form arrives zero-extended in both operands, so the upper
    // 8-byte half contributes a zero sum that the 8-byte write discards.
    write_destination(
        cpu,
        instr,
        mem,
        sum_absolute_byte_differences(left, right),
        bytes,
    )
}

fn move_mask_instr(cpu: &mut Cpu, instr: &SseInstr) -> Result<()> {
    let mmx_source = matches!(instr.operands.src, SseOperand::Mm(_));
    let input = if mmx_source {
        u128::from(
            cpu.fpu
                .read_mmx_register_committed(mm_operand(instr, instr.operands.src)?),
        )
    } else {
        cpu.sse.xmm[xmm_operand(instr, instr.operands.src)?]
    };
    let result = match instr.op {
        SseOp::Movmskps => (0..4).fold(0, |mask, lane| {
            mask | (((lane32(input, lane) >> 31) & 1) << lane)
        }),
        SseOp::Movmskpd => (0..2).fold(0, |mask, lane| {
            mask | (((lane64(input, lane) >> 63) as u32 & 1) << lane)
        }),
        // PMOVMSKB. The MM form's input is zero-extended into the upper half,
        // whose sign bits are therefore clear, so the same 16-byte gather
        // yields the 8-bit result the MM form defines.
        _ => byte_sign_mask(input),
    };
    write_gpr32(cpu, gpr_operand(instr, instr.operands.dst)?, result);
    if mmx_source {
        cpu.fpu.enter_mmx_state();
    }
    Ok(())
}

fn pinsrw_instr<M: PagedMemory>(cpu: &mut Cpu, instr: &SseInstr, mem: &mut M) -> Result<()> {
    let value = match instr.operands.src {
        SseOperand::Gpr(index) => read_gpr32(cpu, usize::from(index)) as u16,
        _ => source_value(cpu, instr, mem, 2)? as u16,
    };
    let (old, bytes) = destination_register(cpu, instr)?;
    let lane = usize::from(instr.operands.imm8 & if bytes == 8 { 3 } else { 7 });
    let result = (old & !(0xffffu128 << (lane * 16))) | (u128::from(value) << (lane * 16));
    write_destination(cpu, instr, mem, result, bytes)
}

fn pextrw_instr(cpu: &mut Cpu, instr: &SseInstr) -> Result<()> {
    let mmx_source = matches!(instr.operands.src, SseOperand::Mm(_));
    let input = if mmx_source {
        u128::from(
            cpu.fpu
                .read_mmx_register_committed(mm_operand(instr, instr.operands.src)?),
        )
    } else {
        cpu.sse.xmm[xmm_operand(instr, instr.operands.src)?]
    };
    let lane = usize::from(instr.operands.imm8 & if mmx_source { 3 } else { 7 });
    write_gpr32(
        cpu,
        gpr_operand(instr, instr.operands.dst)?,
        ((input >> (lane * 16)) & 0xffff) as u32,
    );
    if mmx_source {
        cpu.fpu.enter_mmx_state();
    }
    Ok(())
}

/// Per-lane shift. A count at or past the lane width clears the lane, or
/// fills it with the sign for an arithmetic right shift.
fn shift_instr(
    cpu: &mut Cpu,
    instr: &SseInstr,
    count: u64,
    width: usize,
    left: bool,
    arithmetic: bool,
) -> Result<()> {
    let destination = xmm_operand(instr, instr.operands.dst)?;
    let value = cpu.sse.xmm[destination];
    debug_assert!(matches!(width, 16 | 32 | 64), "shift lane width");
    if count >= width as u64 && !arithmetic {
        // Every lane is shifted clean out.
        cpu.sse.xmm[destination] = 0;
        return Ok(());
    }
    // An arithmetic right shift at or past the lane width replicates the sign,
    // which is what shifting by width-1 does.
    let count = count.min(width as u64 - 1);
    // Each lane shifts in isolation: the lane type's own `<<`/`>>` discards the
    // bits carried out of that lane, so nothing bleeds into its neighbour.
    macro_rules! shift_lanes {
        ($split:ident, $join:ident, $lanes:expr, $unsigned:ty, $signed:ty) => {{
            let lanes = $split(value);
            let mut out = [0; $lanes];
            for lane in 0..$lanes {
                out[lane] = if left {
                    lanes[lane] << count
                } else if arithmetic {
                    ((lanes[lane] as $signed) >> count) as $unsigned
                } else {
                    lanes[lane] >> count
                };
            }
            $join(out)
        }};
    }
    cpu.sse.xmm[destination] = match width {
        16 => shift_lanes!(split16, join16, 8, u16, i16),
        32 => shift_lanes!(split32, join32, 4, u32, i32),
        _ => shift_lanes!(split64, join64, 2, u64, i64),
    };
    Ok(())
}

/// Whole-register byte shift (PSLLDQ/PSRLDQ), which has no lanes.
fn shift_bytes_instr(cpu: &mut Cpu, instr: &SseInstr, count: u64, left: bool) -> Result<()> {
    let destination = xmm_operand(instr, instr.operands.dst)?;
    let value = cpu.sse.xmm[destination];
    cpu.sse.xmm[destination] = if count >= 16 {
        0
    } else if left {
        value << (count * 8)
    } else {
        value >> (count * 8)
    };
    Ok(())
}

fn convert_instr<M: PagedMemory>(cpu: &mut Cpu, instr: &SseInstr, mem: &mut M) -> Result<()> {
    use SseOp as O;
    match instr.op {
        O::Cvtpi2ps => {
            let mmx_source = matches!(instr.operands.src, SseOperand::Mm(_));
            let input = source_value(cpu, instr, mem, 8)?;
            let destination = xmm_operand(instr, instr.operands.dst)?;
            let mut output = cpu.sse.xmm[destination];
            let mut status = Status::OK;
            for lane in 0..2 {
                let result = Single::from_i128_r(
                    lane32(input, lane) as i32 as i128,
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
        O::Cvtpi2pd => {
            let mmx_source = matches!(instr.operands.src, SseOperand::Mm(_));
            let input = source_value(cpu, instr, mem, 8)?;
            let destination = xmm_operand(instr, instr.operands.dst)?;
            let mut output = 0u128;
            for lane in 0..2 {
                put64(
                    &mut output,
                    lane,
                    (lane32(input, lane) as i32 as f64).to_bits(),
                );
            }
            cpu.sse.xmm[destination] = output;
            if mmx_source {
                cpu.fpu.enter_mmx_state();
            }
            Ok(())
        }
        O::Cvtps2pi | O::Cvttps2pi => {
            let input = source_value(cpu, instr, mem, 8)?;
            let truncate = instr.op == O::Cvttps2pi;
            let mut output = 0u64;
            let mut status = Status::OK;
            for lane in 0..2 {
                let (value, lane_status) = single_to_i32(
                    lane32(input, lane),
                    truncate,
                    cpu.sse.rounding(),
                    cpu.sse.mxcsr & (1 << 6) != 0,
                );
                status |= lane_status;
                output |= u64::from(value as u32) << (lane * 32);
            }
            publish_fp_status(cpu, status, false)?;
            cpu.fpu.enter_mmx_state();
            cpu.fpu
                .write_mmx_register(mm_operand(instr, instr.operands.dst)?, output);
            Ok(())
        }
        O::Cvtpd2pi | O::Cvttpd2pi => {
            let input = source_value(cpu, instr, mem, 16)?;
            let truncate = instr.op == O::Cvttpd2pi;
            let mut output = 0u64;
            let mut status = Status::OK;
            for lane in 0..2 {
                let (value, lane_status) = double_to_i32(
                    lane64(input, lane),
                    truncate,
                    cpu.sse.rounding(),
                    cpu.sse.mxcsr & (1 << 6) != 0,
                );
                status |= lane_status;
                output |= u64::from(value as u32) << (lane * 32);
            }
            publish_fp_status(cpu, status, false)?;
            cpu.fpu.enter_mmx_state();
            cpu.fpu
                .write_mmx_register(mm_operand(instr, instr.operands.dst)?, output);
            Ok(())
        }
        O::Cvtdq2ps => {
            let input = source_value(cpu, instr, mem, 16)?;
            let destination = xmm_operand(instr, instr.operands.dst)?;
            let mut output = 0u128;
            let mut status = Status::OK;
            for lane in 0..4 {
                let result = Single::from_i128_r(
                    lane32(input, lane) as i32 as i128,
                    apfloat_round(cpu.sse.rounding()),
                );
                status |= result.status;
                put32(&mut output, lane, result.value.to_bits() as u32);
            }
            publish_fp_status(cpu, status, false)?;
            cpu.sse.xmm[destination] = output;
            Ok(())
        }
        O::Cvtps2dq | O::Cvttps2dq => {
            let input = source_value(cpu, instr, mem, 16)?;
            let destination = xmm_operand(instr, instr.operands.dst)?;
            let (output, flags) =
                packed_to_int32_values(input, instr.op == O::Cvttps2dq, cpu.sse.mxcsr);
            publish_fp_status_raw(cpu, flags)?;
            cpu.sse.xmm[destination] = output;
            Ok(())
        }
        O::Cvtps2pd => {
            let input = source_value(cpu, instr, mem, 8)?;
            let destination = xmm_operand(instr, instr.operands.dst)?;
            let mut output = 0u128;
            let mut status = Status::OK;
            let mut denormal = false;
            let daz = cpu.sse.mxcsr & (1 << 6) != 0;
            for lane in 0..2 {
                let (bits, lane_denormal) = daz32(lane32(input, lane), daz);
                denormal |= lane_denormal;
                let mut loses_info = false;
                let result: rustc_apfloat::StatusAnd<Double> = Single::from_bits(u128::from(bits))
                    .convert_r(apfloat_round(cpu.sse.rounding()), &mut loses_info);
                status |= result.status;
                put64(&mut output, lane, result.value.to_bits() as u64);
            }
            publish_fp_status(cpu, status, denormal)?;
            cpu.sse.xmm[destination] = output;
            Ok(())
        }
        O::Cvtpd2ps => {
            let input = source_value(cpu, instr, mem, 16)?;
            let destination = xmm_operand(instr, instr.operands.dst)?;
            let mut output = 0u128;
            let mut status = Status::OK;
            let mut denormal = false;
            let daz = cpu.sse.mxcsr & (1 << 6) != 0;
            let ftz = cpu.sse.mxcsr & (1 << 15) != 0;
            for lane in 0..2 {
                let (bits, lane_denormal) = daz64(lane64(input, lane), daz);
                denormal |= lane_denormal;
                let mut loses_info = false;
                let result: rustc_apfloat::StatusAnd<Single> = Double::from_bits(u128::from(bits))
                    .convert_r(apfloat_round(cpu.sse.rounding()), &mut loses_info);
                status |= result.status;
                put32(
                    &mut output,
                    lane,
                    ftz32(result.value.to_bits() as u32, result.status, ftz),
                );
            }
            // A double narrowed to single can land tiny and exact, which the
            // masked rule does not report (MDD-BUG-CRUCIBLE-00774).
            let flags = status_bits(status, denormal)
                | unmasked_underflow_bit(output, 2, false, cpu.sse.mxcsr);
            publish_fp_status_raw(cpu, flags)?;
            cpu.sse.xmm[destination] = output;
            Ok(())
        }
        O::Cvtdq2pd => {
            let input = source_value(cpu, instr, mem, 8)?;
            let destination = xmm_operand(instr, instr.operands.dst)?;
            let mut output = 0u128;
            for lane in 0..2 {
                put64(
                    &mut output,
                    lane,
                    (lane32(input, lane) as i32 as f64).to_bits(),
                );
            }
            cpu.sse.xmm[destination] = output;
            Ok(())
        }
        O::Cvtpd2dq | O::Cvttpd2dq => {
            let input = source_value(cpu, instr, mem, 16)?;
            let destination = xmm_operand(instr, instr.operands.dst)?;
            let mut output = 0u128;
            let mut status = Status::OK;
            for lane in 0..2 {
                let (value, lane_status) = double_to_i32(
                    lane64(input, lane),
                    instr.op == O::Cvttpd2dq,
                    cpu.sse.rounding(),
                    cpu.sse.mxcsr & (1 << 6) != 0,
                );
                status |= lane_status;
                put32(&mut output, lane, value as u32);
            }
            publish_fp_status(cpu, status, false)?;
            cpu.sse.xmm[destination] = output;
            Ok(())
        }
        O::Cvtsi2ss | O::Cvtsi2sd => {
            let value = match instr.operands.src {
                SseOperand::Gpr(index) => read_gpr32(cpu, usize::from(index)) as i32,
                _ => source_value(cpu, instr, mem, 4)? as i32,
            };
            let destination = xmm_operand(instr, instr.operands.dst)?;
            let (output, flags) = int32_to_float_values(
                cpu.sse.xmm[destination],
                value,
                instr.op == O::Cvtsi2sd,
                cpu.sse.mxcsr,
            );
            publish_fp_status_raw(cpu, flags)?;
            cpu.sse.xmm[destination] = output;
            Ok(())
        }
        O::Cvtss2si | O::Cvttss2si => {
            let bits = match instr.operands.src {
                SseOperand::Xmm(index) => lane32(cpu.sse.xmm[usize::from(index)], 0),
                _ => source_value(cpu, instr, mem, 4)? as u32,
            };
            let (value, flags) =
                single_to_int32_values(bits, instr.op == O::Cvttss2si, cpu.sse.mxcsr);
            publish_fp_status_raw(cpu, flags)?;
            write_gpr32(cpu, gpr_operand(instr, instr.operands.dst)?, value as u32);
            Ok(())
        }
        O::Cvtsd2si | O::Cvttsd2si => {
            let bits = match instr.operands.src {
                SseOperand::Xmm(index) => lane64(cpu.sse.xmm[usize::from(index)], 0),
                _ => source_value(cpu, instr, mem, 8)? as u64,
            };
            let (value, flags) =
                double_to_int32_values(bits, instr.op == O::Cvttsd2si, cpu.sse.mxcsr);
            publish_fp_status_raw(cpu, flags)?;
            write_gpr32(cpu, gpr_operand(instr, instr.operands.dst)?, value as u32);
            Ok(())
        }
        O::Cvtss2sd => {
            let input = source_value(cpu, instr, mem, 4)?;
            let destination = xmm_operand(instr, instr.operands.dst)?;
            let (bits, denormal) = daz32(input as u32, cpu.sse.mxcsr & (1 << 6) != 0);
            let mut loses_info = false;
            let result: rustc_apfloat::StatusAnd<Double> = Single::from_bits(u128::from(bits))
                .convert_r(apfloat_round(cpu.sse.rounding()), &mut loses_info);
            publish_fp_status(cpu, result.status, denormal)?;
            put64(
                &mut cpu.sse.xmm[destination],
                0,
                result.value.to_bits() as u64,
            );
            Ok(())
        }
        // CVTSD2SS: the only remaining conversion form.
        _ => {
            let input = source_value(cpu, instr, mem, 8)?;
            let destination = xmm_operand(instr, instr.operands.dst)?;
            let (bits, denormal) = daz64(input as u64, cpu.sse.mxcsr & (1 << 6) != 0);
            let mut loses_info = false;
            let result: rustc_apfloat::StatusAnd<Single> = Double::from_bits(u128::from(bits))
                .convert_r(apfloat_round(cpu.sse.rounding()), &mut loses_info);
            publish_fp_status(cpu, result.status, denormal)?;
            put32(
                &mut cpu.sse.xmm[destination],
                0,
                ftz32(
                    result.value.to_bits() as u32,
                    result.status,
                    cpu.sse.mxcsr & (1 << 15) != 0,
                ),
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sse_decode::tests::CANONICAL;
    use crate::sse_decode::{self, SseDecode};
    use crate::sse_exec_reference::{self, decode_uncached};
    use crate::{Flags, MemoryAccess};

    /// MDD-BUG-CRUCIBLE-00774: with UM unmasked, an EXACT tiny result still
    /// signals Underflow.
    ///
    /// x86 has two underflow rules: masked, Underflow means tiny AND inexact;
    /// unmasked, it means tiny, full stop. `rustc_apfloat` implements only the
    /// masked one, so `2^-100 * 2^-49` — exactly representable as a single
    /// subnormal, and the record's own example — carried no UE and a guest
    /// that unmasked UE never took the #XM silicon raises.
    /// MDD-BUG-CRUCIBLE-00784: a denormal square root whose answer is exactly
    /// representable stays exact, in every rounding mode.
    ///
    /// `ieee_apsqrt` reports a denormal root as inexact even when it is not,
    /// so a directed mode then moved it one ulp. Round-to-nearest hid it -
    /// the record quotes the wrong value without naming the mode, and under
    /// the default mode the shipped code looks correct.
    ///
    /// The exact cases: `sqrt(2^-148) = 2^-74` and `sqrt(2^-1074 * 2) =
    /// 2^-537`, both the record's own, and both measured against this box's
    /// silicon, which answers them with DE and no PE in all four modes.
    #[test]
    fn a_denormal_square_root_that_is_exact_stays_exact_in_every_rounding_mode() {
        const PE: u32 = 1 << 5;
        const DE: u32 = 1 << 1;
        for (name, mode) in [("nearest", 0_u32), ("down", 1), ("up", 2), ("zero", 3)] {
            let mxcsr = 0x1f80 | (mode << 13);

            let mut input = 0_u128;
            put32(&mut input, 0, 0x0000_0002);
            let (value, flags) = soft_square_root(0, input, 1, false, mxcsr);
            assert_eq!(
                lane32(value, 0),
                0x1a80_0000,
                "{name}: sqrt(2^-148) is exactly 2^-74"
            );
            assert_eq!(flags & PE, 0, "{name}: an exact root raises no Precision");
            assert_eq!(
                flags & DE,
                DE,
                "{name}: the denormal operand still raises DE"
            );

            let mut input = 0_u128;
            put64(&mut input, 0, 0x0000_0000_0000_0001);
            let (value, flags) = soft_square_root(0, input, 1, true, mxcsr);
            assert_eq!(
                lane64(value, 0),
                0x1e60_0000_0000_0000,
                "{name}: sqrt(2^-1074) is exactly 2^-537"
            );
            assert_eq!(flags & PE, 0, "{name}: an exact root raises no Precision");
            assert_eq!(
                flags & DE,
                DE,
                "{name}: the denormal operand still raises DE"
            );
        }
    }

    /// The whole denormal range against this box's silicon, in every rounding
    /// mode (MDD-BUG-CRUCIBLE-00784).
    ///
    /// The two exact cases above are the ones that were wrong, but the repair
    /// re-scales EVERY denormal, so every denormal is what has to be checked -
    /// an inexact root must still round the way the hardware rounds it. The
    /// oracle is the crate's own host kernel taken below the refusal in
    /// `square_root`, which sends this very input class to the software model.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn every_denormal_square_root_matches_this_host() {
        for mode in 0_u32..4 {
            let mxcsr = 0x1f80 | (mode << 13);

            let mut singles = vec![1_u32, 2, 3, 4, 5, 8, 9, 0x7f_ffff, 0x7f_fffe, 0x40_0000];
            singles.extend((1..=32).map(|k| 1_u32 << (k % 23)));
            singles.extend((1..=64).map(|k: u32| k.wrapping_mul(0x0001_3579) & 0x007f_ffff));
            for bits in singles {
                if !is_denormal32(bits) {
                    continue;
                }
                let mut input = 0_u128;
                put32(&mut input, 0, bits);
                let (value, _) = soft_square_root(0, input, 1, false, mxcsr);
                let (host, _) =
                    crate::sse_host_kernels::tests::host_sqrt_scalar_for_test(input, false, mxcsr);
                assert_eq!(
                    lane32(value, 0),
                    lane32(host, 0),
                    "single sqrt({bits:#010x}) in rounding mode {mode}"
                );
            }

            let mut doubles = vec![1_u64, 2, 3, 4, 5, 0x000f_ffff_ffff_ffff];
            doubles.extend((1..=52).map(|k| 1_u64 << k));
            doubles.extend(
                (1..=64)
                    .map(|k: u64| k.wrapping_mul(0x0001_3579_0002_468a) & 0x000f_ffff_ffff_ffff),
            );
            for bits in doubles {
                if !is_denormal64(bits) {
                    continue;
                }
                let mut input = 0_u128;
                put64(&mut input, 0, bits);
                let (value, _) = soft_square_root(0, input, 1, true, mxcsr);
                let (host, _) =
                    crate::sse_host_kernels::tests::host_sqrt_scalar_for_test(input, true, mxcsr);
                assert_eq!(
                    lane64(value, 0),
                    lane64(host, 0),
                    "double sqrt({bits:#018x}) in rounding mode {mode}"
                );
            }
        }
    }

    /// MDD-BUG-CRUCIBLE-00785: an overflow that CLAMPS still raises Overflow.
    ///
    /// Under a mode that rounds toward zero for the result's sign, an
    /// overflowing operation delivers the largest finite rather than an
    /// infinity. apfloat reported only Inexact for that, so MXCSR.OE never
    /// went sticky and an unmasked OE never faulted.
    ///
    /// The corpus deliberately includes the TRAP: `max + tiny` also delivers
    /// the largest finite, also inexact, in the same mode - and is NOT an
    /// overflow. A rule keyed on the delivered value alone passes the
    /// record's own case and fails this one, which is how the first attempt
    /// at this fix was caught.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn a_clamped_overflow_raises_overflow_exactly_when_this_host_does() {
        const OE: u32 = 1 << 3;
        const MAX32: u32 = 0x7f7f_ffff;
        const MAX64: u64 = 0x7fef_ffff_ffff_ffff;
        const TINY32: u32 = 0x0000_0001;
        const TINY64: u64 = 0x0000_0000_0000_0001;
        const ONE32: u32 = 0x3f80_0000;
        const ONE64: u64 = 0x3ff0_0000_0000_0000;
        const HALF32: u32 = 0x3f00_0000;
        const HALF64: u64 = 0x3fe0_0000_0000_0000;

        let singles: [(u32, u32); 8] = [
            (MAX32, MAX32),
            (MAX32, TINY32),
            (MAX32 | 0x8000_0000, MAX32 | 0x8000_0000),
            (MAX32 | 0x8000_0000, TINY32 | 0x8000_0000),
            (MAX32, ONE32),
            (MAX32, MAX32 | 0x8000_0000),
            (MAX32, HALF32),
            (0x7f00_0000, 0x7f00_0000),
        ];
        let doubles: [(u64, u64); 8] = [
            (MAX64, MAX64),
            (MAX64, TINY64),
            (MAX64 | 0x8000_0000_0000_0000, MAX64 | 0x8000_0000_0000_0000),
            (
                MAX64 | 0x8000_0000_0000_0000,
                TINY64 | 0x8000_0000_0000_0000,
            ),
            (MAX64, ONE64),
            (MAX64, MAX64 | 0x8000_0000_0000_0000),
            (MAX64, HALF64),
            (0x7fe0_0000_0000_0000, 0x7fe0_0000_0000_0000),
        ];
        let operations = [
            FloatOperation::Add,
            FloatOperation::Subtract,
            FloatOperation::Multiply,
            FloatOperation::Divide,
        ];

        let mut saw_overflow = 0;
        let mut saw_clamped_without_overflow = 0;
        for mode in 0_u32..4 {
            let mxcsr = 0x1f80 | (mode << 13);
            for operation in operations {
                for (a, b) in singles {
                    let mut left = 0_u128;
                    let mut right = 0_u128;
                    put32(&mut left, 0, a);
                    put32(&mut right, 0, b);
                    let (value, flags) =
                        float_binary_values(left, right, 1, false, operation, mxcsr);
                    let (host, host_flags) =
                        crate::sse_host_kernels::tests::host_binary_scalar_for_test(
                            left, right, false, mxcsr, operation,
                        );
                    assert_eq!(
                        lane32(value, 0),
                        lane32(host, 0),
                        "single {operation:?}({a:#010x}, {b:#010x}) mode {mode} value"
                    );
                    assert_eq!(
                        flags & OE,
                        host_flags & OE,
                        "single {operation:?}({a:#010x}, {b:#010x}) mode {mode} Overflow"
                    );
                    if host_flags & OE != 0 {
                        saw_overflow += 1;
                        if lane32(value, 0) & 0x7fff_ffff == MAX32 {
                            saw_clamped_without_overflow += 1;
                        }
                    }
                }
                for (a, b) in doubles {
                    let mut left = 0_u128;
                    let mut right = 0_u128;
                    put64(&mut left, 0, a);
                    put64(&mut right, 0, b);
                    let (value, flags) =
                        float_binary_values(left, right, 1, true, operation, mxcsr);
                    let (host, host_flags) =
                        crate::sse_host_kernels::tests::host_binary_scalar_for_test(
                            left, right, true, mxcsr, operation,
                        );
                    assert_eq!(
                        lane64(value, 0),
                        lane64(host, 0),
                        "double {operation:?}({a:#018x}, {b:#018x}) mode {mode} value"
                    );
                    assert_eq!(
                        flags & OE,
                        host_flags & OE,
                        "double {operation:?}({a:#018x}, {b:#018x}) mode {mode} Overflow"
                    );
                }
            }
        }
        // The corpus has to contain the behaviour it claims to pin: overflows
        // that were delivered as the largest finite rather than an infinity.
        assert!(saw_overflow > 0, "the corpus raised no Overflow at all");
        assert!(
            saw_clamped_without_overflow > 0,
            "the corpus never reached a CLAMPED overflow, so it proves nothing \
             about the rule this record is for"
        );
    }

    /// MDD-BUG-CRUCIBLE-00786: a short-circuited lane raises no Denormal.
    ///
    /// DE was derived from operand inspection alone, so an operation whose
    /// result a NaN or a divide-by-zero had already settled still reported the
    /// denormal it never used - a spurious sticky DE, and a spurious #XM
    /// whenever DM was unmasked.
    ///
    /// Half this corpus is the cases that must KEEP their DE. `den + inf`,
    /// `den + 0`, `den * one`, `0 / den` and `inf / den` all pair a denormal
    /// with something eye-catching and are not short circuits, so a rule that
    /// suppressed on "anything unusual" would fail here rather than silently
    /// dropping a real exception.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn a_short_circuited_lane_raises_denormal_exactly_when_this_host_does() {
        const DE: u32 = 1 << 1;
        const SUPPRESSED: u32 = 0;

        // (name, left, right, operation, DE expected of silicon)
        let singles: [(&str, u32, u32, FloatOperation, u32); 11] = [
            (
                "den + qnan",
                0x0000_0001,
                0x7fc0_0000,
                FloatOperation::Add,
                SUPPRESSED,
            ),
            (
                "qnan + den",
                0x7fc0_0000,
                0x0000_0001,
                FloatOperation::Add,
                SUPPRESSED,
            ),
            (
                "den + snan",
                0x0000_0001,
                0x7f80_0001,
                FloatOperation::Add,
                SUPPRESSED,
            ),
            (
                "den * qnan",
                0x0000_0001,
                0x7fc0_0000,
                FloatOperation::Multiply,
                SUPPRESSED,
            ),
            (
                "den / qnan",
                0x0000_0001,
                0x7fc0_0000,
                FloatOperation::Divide,
                SUPPRESSED,
            ),
            (
                "den / zero",
                0x0000_0001,
                0x0000_0000,
                FloatOperation::Divide,
                SUPPRESSED,
            ),
            (
                "den + inf",
                0x0000_0001,
                0x7f80_0000,
                FloatOperation::Add,
                DE,
            ),
            (
                "den + zero",
                0x0000_0001,
                0x0000_0000,
                FloatOperation::Add,
                DE,
            ),
            (
                "den * one",
                0x0000_0001,
                0x3f80_0000,
                FloatOperation::Multiply,
                DE,
            ),
            (
                "zero / den",
                0x0000_0000,
                0x0000_0001,
                FloatOperation::Divide,
                DE,
            ),
            (
                "inf / den",
                0x7f80_0000,
                0x0000_0001,
                FloatOperation::Divide,
                DE,
            ),
        ];
        let doubles: [(&str, u64, u64, FloatOperation, u32); 11] = [
            (
                "den + qnan",
                1,
                0x7ff8_0000_0000_0000,
                FloatOperation::Add,
                SUPPRESSED,
            ),
            (
                "qnan + den",
                0x7ff8_0000_0000_0000,
                1,
                FloatOperation::Add,
                SUPPRESSED,
            ),
            (
                "den + snan",
                1,
                0x7ff0_0000_0000_0001,
                FloatOperation::Add,
                SUPPRESSED,
            ),
            (
                "den * qnan",
                1,
                0x7ff8_0000_0000_0000,
                FloatOperation::Multiply,
                SUPPRESSED,
            ),
            (
                "den / qnan",
                1,
                0x7ff8_0000_0000_0000,
                FloatOperation::Divide,
                SUPPRESSED,
            ),
            ("den / zero", 1, 0, FloatOperation::Divide, SUPPRESSED),
            (
                "den + inf",
                1,
                0x7ff0_0000_0000_0000,
                FloatOperation::Add,
                DE,
            ),
            ("den + zero", 1, 0, FloatOperation::Add, DE),
            (
                "den * one",
                1,
                0x3ff0_0000_0000_0000,
                FloatOperation::Multiply,
                DE,
            ),
            ("zero / den", 0, 1, FloatOperation::Divide, DE),
            (
                "inf / den",
                0x7ff0_0000_0000_0000,
                1,
                FloatOperation::Divide,
                DE,
            ),
        ];

        let mut suppressions_seen = 0;
        let mut retentions_seen = 0;
        for mode in 0_u32..4 {
            let mxcsr = 0x1f80 | (mode << 13);
            for (name, a, b, operation, expected) in singles {
                let mut left = 0_u128;
                let mut right = 0_u128;
                put32(&mut left, 0, a);
                put32(&mut right, 0, b);
                let (_, flags) = float_binary_values(left, right, 1, false, operation, mxcsr);
                let (_, host) = crate::sse_host_kernels::tests::host_binary_scalar_for_test(
                    left, right, false, mxcsr, operation,
                );
                assert_eq!(
                    host & DE,
                    expected,
                    "single {name} mode {mode}: this host disagrees with the recorded rule"
                );
                assert_eq!(flags & DE, host & DE, "single {name} mode {mode} Denormal");
                if expected == SUPPRESSED {
                    suppressions_seen += 1;
                } else {
                    retentions_seen += 1;
                }
            }
            for (name, a, b, operation, expected) in doubles {
                let mut left = 0_u128;
                let mut right = 0_u128;
                put64(&mut left, 0, a);
                put64(&mut right, 0, b);
                let (_, flags) = float_binary_values(left, right, 1, true, operation, mxcsr);
                let (_, host) = crate::sse_host_kernels::tests::host_binary_scalar_for_test(
                    left, right, true, mxcsr, operation,
                );
                assert_eq!(
                    host & DE,
                    expected,
                    "double {name} mode {mode}: this host disagrees with the recorded rule"
                );
                assert_eq!(flags & DE, host & DE, "double {name} mode {mode} Denormal");
            }
        }
        // Both halves have to be present, or the corpus stops proving the rule
        // and starts proving only one side of it.
        assert!(suppressions_seen > 0 && retentions_seen > 0);
    }

    /// MDD-BUG-CRUCIBLE-00787: FTZ flushes ANY tiny result, not only one that
    /// was also inexact.
    ///
    /// `ftz32`/`ftz64` gated on apfloat's `UNDERFLOW`, which means tiny AND
    /// inexact, so a tiny-but-exact result kept its denormal and raised
    /// nothing where hardware delivers a signed zero with UE|PE.
    ///
    /// Half the corpus is the tiny-and-INEXACT cases that already agreed, and
    /// half the results that must NOT be flushed at all. Both halves matter:
    /// this defect is a rule that was too narrow, and the obvious overshoot -
    /// flushing whenever FTZ is on - would break the normal and zero results.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn ftz_flushes_a_tiny_result_exactly_when_this_host_does() {
        // (name, a, b, op, tiny)
        let cases: [(&str, u32, u32, FloatOperation, bool); 9] = [
            (
                "tiny exact, the record's own",
                0x0080_0001,
                0x8080_0000,
                FloatOperation::Add,
                true,
            ),
            (
                "tiny exact, negative",
                0x0080_0000,
                0x8080_0001,
                FloatOperation::Add,
                true,
            ),
            (
                "tiny exact, 2^-126 * 0.5",
                0x0080_0000,
                0x3f00_0000,
                FloatOperation::Multiply,
                true,
            ),
            (
                "tiny exact, quarter / 2^126",
                0x3e80_0000,
                0x7e80_0000,
                FloatOperation::Divide,
                true,
            ),
            (
                "tiny exact, denormal passthrough",
                0x0000_0001,
                0x0000_0000,
                FloatOperation::Add,
                true,
            ),
            (
                "tiny inexact, halved denormal",
                0x0000_0001,
                0x3f00_0000,
                FloatOperation::Multiply,
                true,
            ),
            (
                "tiny inexact, denormal / 3",
                0x0000_0007,
                0x4040_0000,
                FloatOperation::Divide,
                true,
            ),
            (
                "normal result",
                0x3f80_0000,
                0x3f80_0000,
                FloatOperation::Add,
                false,
            ),
            (
                "exact zero",
                0x3f80_0000,
                0xbf80_0000,
                FloatOperation::Add,
                false,
            ),
        ];
        let mut flushed_seen = 0;
        let mut untouched_seen = 0;
        for ftz in [false, true] {
            for (name, a, b, operation, tiny) in cases {
                let mxcsr = 0x1f80_u32 | if ftz { 1 << 15 } else { 0 };
                let mut left = 0_u128;
                let mut right = 0_u128;
                put32(&mut left, 0, a);
                put32(&mut right, 0, b);
                let (value, flags) = float_binary_values(left, right, 1, false, operation, mxcsr);
                let (host, host_flags) =
                    crate::sse_host_kernels::tests::host_binary_scalar_for_test(
                        left, right, false, mxcsr, operation,
                    );
                assert_eq!(
                    lane32(value, 0),
                    lane32(host, 0),
                    "{name} (ftz={ftz}) value"
                );
                assert_eq!(flags & 0x3f, host_flags & 0x3f, "{name} (ftz={ftz}) flags");
                // The software model is what this fix changes, and the host
                // kernel answers every case with no denormal in it - so
                // comparing only the dispatched result would leave the
                // software path untested exactly where it must not flush.
                let (soft, soft_flags) = soft_float_binary(left, right, 1, false, operation, mxcsr);
                assert_eq!(
                    lane32(soft, 0),
                    lane32(host, 0),
                    "{name} (ftz={ftz}) software value"
                );
                assert_eq!(
                    soft_flags & 0x3f,
                    host_flags & 0x3f,
                    "{name} (ftz={ftz}) software flags"
                );
                if ftz && tiny {
                    assert_eq!(
                        lane32(host, 0) & 0x7fff_ffff,
                        0,
                        "{name}: this host did not flush a tiny result under FTZ"
                    );
                    flushed_seen += 1;
                } else if ftz {
                    untouched_seen += 1;
                }
            }
        }
        assert!(flushed_seen > 0 && untouched_seen > 0);
    }

    #[test]
    fn an_exact_tiny_result_signals_underflow_only_when_um_is_unmasked() {
        const UE: u32 = 1 << 4;
        const PE: u32 = 1 << 5;
        const ALL_MASKED: u32 = 0x1f80; // every exception masked, round nearest.
        const UM: u32 = 1 << 11;
        // 2^-100 and 2^-49: the product is 2^-149, the smallest single
        // subnormal, and it is EXACT — no rounding happens at all.
        let two_pow = |exponent: i32| ((exponent + 127) as u32) << 23;
        let left = u128::from(two_pow(-100));
        let right = u128::from(two_pow(-49));

        let (masked_value, masked_flags) =
            float_binary_values(left, right, 4, false, FloatOperation::Multiply, ALL_MASKED);
        let (unmasked_value, unmasked_flags) = float_binary_values(
            left,
            right,
            4,
            false,
            FloatOperation::Multiply,
            ALL_MASKED & !UM,
        );

        assert_eq!(
            lane32(masked_value, 0),
            1,
            "the product is the smallest single subnormal"
        );
        assert_eq!(unmasked_value, masked_value, "the value must not change");
        assert_eq!(
            masked_flags & UE,
            0,
            "masked: tiny but exact raises nothing — this half was already right"
        );
        assert_eq!(
            unmasked_flags & UE,
            UE,
            "unmasked: tiny alone signals Underflow"
        );
        assert_eq!(
            unmasked_flags & PE,
            0,
            "the result is exact, so Precision must stay clear either way"
        );

        // An ordinary normal result must not acquire UE just because UM is
        // unmasked, or the rule would be "signal always".
        let one = u128::from(two_pow(0));
        let (_, normal_flags) = float_binary_values(
            one,
            one,
            4,
            false,
            FloatOperation::Multiply,
            ALL_MASKED & !UM,
        );
        assert_eq!(normal_flags & UE, 0, "1.0 * 1.0 is not tiny");

        // Whichever backend answered, it reported the masked rule and nothing
        // more — that is the limitation this fix compensates for, and it holds
        // for the software model and the host kernel alike.
        let (backend_value, backend_flags) = float_binary_backend(
            left,
            right,
            4,
            false,
            FloatOperation::Multiply,
            ALL_MASKED & !UM,
        );
        assert_eq!(backend_value, masked_value);
        assert_eq!(
            backend_flags & UE,
            0,
            "the backend itself never reports the unmasked rule"
        );
    }

    /// MDD-BUG-CRUCIBLE-00772: MIN/MAX signal Invalid on a QUIET NaN, not
    /// only a signaling one.
    ///
    /// Adjudicated against the SDM and host silicon: MINPS/MAXPS/MINSS/MINPD/MAXSD all
    /// set MXCSR.IE for a quiet-NaN operand in either position, while ADDPS
    /// with the same operand leaves IE clear. We raised IE only for a
    /// signaling NaN, so a guest with IM unmasked never took the #XM real
    /// hardware raises.
    #[test]
    fn min_max_signal_invalid_on_a_quiet_nan_operand() {
        const IE: u32 = 1 << 0;
        const QNAN32: u32 = 0x7FC0_0000;
        const SNAN32: u32 = 0x7F80_0001;
        const QNAN64: u64 = 0x7FF8_0000_0000_0000;
        const ONE32: u32 = 0x3F80_0000;
        const ONE64: u64 = 0x3FF0_0000_0000_0000;

        let packed32 = |bits: u32| u128::from(bits) * 0x0000_0001_0000_0001_0000_0001_0000_0001;
        let packed64 = |bits: u64| u128::from(bits) * 0x0000_0000_0000_0001_0000_0000_0000_0001;

        for maximum in [false, true] {
            // A quiet NaN in either operand raises Invalid, single and double.
            let (_, flags) = soft_min_max(packed32(ONE32), packed32(QNAN32), 4, false, maximum, 0);
            assert_eq!(flags & IE, IE, "single-precision quiet NaN source");
            let (_, flags) = soft_min_max(packed32(QNAN32), packed32(ONE32), 4, false, maximum, 0);
            assert_eq!(flags & IE, IE, "single-precision quiet NaN destination");
            let (_, flags) = soft_min_max(packed64(ONE64), packed64(QNAN64), 2, true, maximum, 0);
            assert_eq!(flags & IE, IE, "double-precision quiet NaN source");

            // A signaling NaN still does, and two ordinary operands still
            // do not — so this is not "IE for everything".
            let (_, flags) = soft_min_max(packed32(ONE32), packed32(SNAN32), 4, false, maximum, 0);
            assert_eq!(flags & IE, IE, "signaling NaN still signals");
            let (_, flags) = soft_min_max(packed32(ONE32), packed32(ONE32), 4, false, maximum, 0);
            assert_eq!(flags & IE, 0, "two numbers must not signal");
        }

        // The VALUE is unchanged: with a NaN operand the second operand is
        // returned, as the silicon probe showed.
        let (value, _) = soft_min_max(packed32(ONE32), packed32(QNAN32), 4, false, false, 0);
        assert_eq!(value, packed32(QNAN32), "min(1.0, QNaN) returns the source");
        let (value, _) = soft_min_max(packed32(QNAN32), packed32(ONE32), 4, false, false, 0);
        assert_eq!(value, packed32(ONE32), "min(QNaN, 1.0) returns the source");
    }

    /// MDD-BUG-CRUCIBLE-00782: PSLLW/PSLLD/PSLLQ shift each lane on its own.
    ///
    /// Both executors used to OR an untruncated `lane << count` back into
    /// place, which is algebraically one whole-register 128-bit shift: a bit
    /// pushed off the top of a lane reappeared in the next lane's low bits.
    /// Silicon discards it. The operand below sets every lane's top bit, so a
    /// carried-out bit is visible in every higher lane.
    #[test]
    fn a_packed_left_shift_never_carries_bits_across_a_lane_boundary() {
        const OPERAND: u128 = 0x8001_8001_8001_8001_8001_8001_8001_8001;
        // Shift by one. Per lane the top bit is discarded, so `0x8001`
        // becomes `0x0002` at 16 bits, `0x8001_8001` becomes `0x0003_0002` at
        // 32, and so on. A whole-register shift would instead leave `...0003`
        // in every lane above the lowest.
        let cases: [(&str, [u8; 5], u128); 3] = [
            (
                "psllw",
                [0x66, 0x0F, 0x71, 0xF0, 0x01],
                0x0002_0002_0002_0002_0002_0002_0002_0002,
            ),
            (
                "pslld",
                [0x66, 0x0F, 0x72, 0xF0, 0x01],
                0x0003_0002_0003_0002_0003_0002_0003_0002,
            ),
            (
                "psllq",
                [0x66, 0x0F, 0x73, 0xF0, 0x01],
                0x0003_0003_0003_0002_0003_0003_0003_0002,
            ),
        ];
        for (name, bytes, expected) in cases {
            let decoded = match sse_decode::decode(&bytes, 32) {
                SseDecode::Decoded(instr) => instr,
                other => panic!("{name} decoded as {other:?}"),
            };
            let mut cpu = Cpu::new();
            let mut mem = Ram::new();
            cpu.sse.xmm[0] = OPERAND;
            execute(&mut cpu, &decoded, &mut mem).expect("shift executes");
            assert_eq!(
                cpu.sse.xmm[0], expected,
                "{name}: a bit crossed a lane boundary in the executor"
            );

            // The frozen oracle carried the same defect, and the differential
            // holds the two models in lockstep, so it has to move as well.
            let reference = decode_uncached(&bytes, 32);
            let mut cpu = Cpu::new();
            let mut mem = Ram::new();
            cpu.sse.xmm[0] = OPERAND;
            sse_exec_reference::execute(&mut cpu, &reference, &mut mem).expect("shift executes");
            assert_eq!(
                cpu.sse.xmm[0], expected,
                "{name}: a bit crossed a lane boundary in the frozen oracle"
            );
        }
    }

    // ------------------------------------------------------------------
    // A flat real-mode machine
    // ------------------------------------------------------------------

    const RAM_SIZE: usize = 0x8000;

    /// Real-mode selectors, deliberately all different: a segment resolved to
    /// the wrong register lands on different bytes, so the differential can
    /// see it.
    const SEG_DS: u16 = 0x0000;
    const SEG_SS: u16 = 0x0040;
    const SEG_ES: u16 = 0x0100;
    const SEG_CS: u16 = 0x0140;
    const SEG_FS: u16 = 0x0080;
    const SEG_GS: u16 = 0x00C0;

    struct Ram {
        data: Vec<u8>,
    }

    impl Ram {
        fn new() -> Self {
            Self {
                data: vec![0; RAM_SIZE],
            }
        }
    }

    impl MemoryAccess for Ram {
        fn read_u8(&self, addr: usize) -> u8 {
            self.data.get(addr).copied().unwrap_or(0)
        }
        fn read_u16(&self, addr: usize) -> u16 {
            u16::from_le_bytes([self.read_u8(addr), self.read_u8(addr + 1)])
        }
        fn read_u32(&self, addr: usize) -> u32 {
            u32::from_le_bytes([
                self.read_u8(addr),
                self.read_u8(addr + 1),
                self.read_u8(addr + 2),
                self.read_u8(addr + 3),
            ])
        }
        fn read_u64(&self, addr: usize) -> u64 {
            u64::from(self.read_u32(addr)) | (u64::from(self.read_u32(addr + 4)) << 32)
        }
        fn write_u8(&mut self, addr: usize, value: u8) {
            if let Some(slot) = self.data.get_mut(addr) {
                *slot = value;
            }
        }
        fn write_u16(&mut self, addr: usize, value: u16) {
            for (offset, byte) in value.to_le_bytes().into_iter().enumerate() {
                self.write_u8(addr + offset, byte);
            }
        }
        fn write_u32(&mut self, addr: usize, value: u32) {
            for (offset, byte) in value.to_le_bytes().into_iter().enumerate() {
                self.write_u8(addr + offset, byte);
            }
        }
        fn write_u64(&mut self, addr: usize, value: u64) {
            for (offset, byte) in value.to_le_bytes().into_iter().enumerate() {
                self.write_u8(addr + offset, byte);
            }
        }
    }

    impl PagedMemory for Ram {
        fn map_linear_physical(&self, _: u32) -> Option<usize> {
            None
        }
        fn tlb_lookup(&self, _: u32) -> Option<(u32, bool, bool, bool)> {
            None
        }
        fn tlb_insert(&mut self, _: u32, _: u32, _: bool, _: bool, _: bool) {}
        fn read_u32_physical(&self, addr: usize) -> u32 {
            self.read_u32(addr)
        }
        fn read_u8_physical_checked(&self, addr: usize) -> Option<u8> {
            self.data.get(addr).copied()
        }
        fn write_u32_physical(&mut self, addr: usize, value: u32) {
            self.write_u32(addr, value);
        }
        fn set_page_fault(&mut self, _: u32, _: u32) {}
        fn clear_page_fault(&mut self) {}
        fn page_fault_pending(&self) -> bool {
            false
        }
        fn page_fault_address(&self) -> u32 {
            0
        }
        fn page_fault_error_code(&self) -> u32 {
            0
        }
        fn invalidate_tlb(&mut self) {}
        fn invalidate_tlb_page(&mut self, _: u32) {}
    }

    // ------------------------------------------------------------------
    // Deterministic state generation
    // ------------------------------------------------------------------

    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
        }
        fn next_u64(&mut self) -> u64 {
            // SplitMix64.
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn next_u32(&mut self) -> u32 {
            self.next_u64() as u32
        }
        fn below(&mut self, bound: usize) -> usize {
            (self.next_u64() % bound as u64) as usize
        }
    }

    /// Single-precision values whose classes the executor treats specially.
    const SPECIAL32: &[u32] = &[
        0x0000_0000,
        0x8000_0000,
        0x0000_0001,
        0x807F_FFFF,
        0x0080_0000,
        0x3F80_0000,
        0xBF80_0000,
        0x4B00_0000,
        0x7F7F_FFFF,
        0xFF7F_FFFF,
        0x7F80_0000,
        0xFF80_0000,
        0x7FC0_0000,
        0x7FA0_0000,
        0xFFC0_0000,
        0x7F80_0001,
        0x4049_0FDB,
        0xCF00_0000,
        0x4F00_0000,
        0x0000_00FF,
    ];

    /// Double-precision counterparts.
    const SPECIAL64: &[u64] = &[
        0x0000_0000_0000_0000,
        0x8000_0000_0000_0000,
        0x0000_0000_0000_0001,
        0x800F_FFFF_FFFF_FFFF,
        0x0010_0000_0000_0000,
        0x3FF0_0000_0000_0000,
        0xBFF0_0000_0000_0000,
        0x4330_0000_0000_0000,
        0x7FEF_FFFF_FFFF_FFFF,
        0x7FF0_0000_0000_0000,
        0xFFF0_0000_0000_0000,
        0x7FF8_0000_0000_0000,
        0x7FF4_0000_0000_0000,
        0x7FF0_0000_0000_0001,
        0x4009_21FB_5444_2D18,
        0xC1E0_0000_0000_0000,
    ];

    /// The byte-MSB pattern MASKMOV*/PMOVMSKB select on.
    const BYTE_SIGNS: u128 = 0x8080_8080_8080_8080_8080_8080_8080_8080;

    fn random_lane_value(rng: &mut Rng) -> u128 {
        match rng.below(6) {
            // Every byte sign clear, then every byte sign set: without these
            // the mask registers of MASKMOVQ/MASKMOVDQU are never uniformly
            // off or uniformly on, and the store-nothing path goes untested.
            4 => (u128::from(rng.next_u64()) | (u128::from(rng.next_u64()) << 64)) & !BYTE_SIGNS,
            5 => (u128::from(rng.next_u64()) | (u128::from(rng.next_u64()) << 64)) | BYTE_SIGNS,
            other => random_value_class(rng, other),
        }
    }

    fn random_value_class(rng: &mut Rng, class: usize) -> u128 {
        match class {
            0 => {
                // Four single-precision lanes.
                let mut value = 0u128;
                for lane in 0..4 {
                    let bits = if rng.below(3) == 0 {
                        rng.next_u32()
                    } else {
                        SPECIAL32[rng.below(SPECIAL32.len())]
                    };
                    value |= u128::from(bits) << (lane * 32);
                }
                value
            }
            1 => {
                // Two double-precision lanes.
                let mut value = 0u128;
                for lane in 0..2 {
                    let bits = if rng.below(3) == 0 {
                        rng.next_u64()
                    } else {
                        SPECIAL64[rng.below(SPECIAL64.len())]
                    };
                    value |= u128::from(bits) << (lane * 64);
                }
                value
            }
            2 => u128::from(rng.next_u64()) | (u128::from(rng.next_u64()) << 64),
            // Byte/word patterns that make packed-integer saturation and
            // averaging boundaries likely.
            _ => {
                let mut value = 0u128;
                for byte in 0..16 {
                    let bits = match rng.below(4) {
                        0 => 0x00,
                        1 => 0xFF,
                        2 => 0x80,
                        _ => rng.next_u32() as u8,
                    };
                    value |= u128::from(bits) << (byte * 8);
                }
                value
            }
        }
    }

    /// Address-register values that put every generated effective address
    /// inside the RAM window.
    const TAME_GPRS: [u32; 8] = [
        0x0000_2100, // EAX  - SIB base
        0x0000_2000, // ECX  - [ECX]
        0x0000_0008, // EDX  - SIB index, scaled by 4
        0x0000_1000, // EBX  - BX in 16-bit forms
        0x0000_2010, // ESP
        0x0000_1040, // EBP  - BP in 16-bit forms (SS-relative)
        0x0000_2020, // ESI  - SI in 16-bit forms
        0x0000_0030, // EDI  - DI, and MASKMOV*'s implicit offset
    ];

    struct GuestState {
        xmm: [u128; 8],
        mxcsr: u32,
        daz_supported: bool,
        gprs: [u32; 8],
        flags: u16,
        cr4: u32,
        fpu_regs: [[u8; 10]; 8],
        fpu_status: u16,
        fpu_control: u16,
        fpu_tag: u16,
        fpu_opcode: u16,
        fpu_ip_offset: u32,
        fpu_ip_selector: u16,
        fpu_operand_offset: u32,
        fpu_operand_selector: u16,
        ram: Vec<u8>,
    }

    fn guest_state(seed: u64, tame_addresses: bool) -> GuestState {
        let mut rng = Rng::new(seed);
        let mut xmm = [0u128; 8];
        for register in &mut xmm {
            *register = random_lane_value(&mut rng);
        }
        // Half of the out-of-range-address states clear every byte sign, so
        // that MASKMOV*'s "no byte is stored" case coincides with a faulting
        // operand often enough to pin that the store still faults.
        if !tame_addresses && rng.below(2) == 0 {
            for register in &mut xmm {
                *register &= !BYTE_SIGNS;
            }
        }
        let daz_supported = rng.below(2) == 0;
        let mask = if daz_supported { 0xFFFF } else { 0xFFBF };
        // Rounding control, DAZ, FTZ, and a varying exception mask, so the
        // unmasked path (#XM / #UD by CR4.OSXMMEXCPT) is reached too.
        let mut mxcsr = (rng.next_u32() & 0x6000) | (0x3F << 7);
        if rng.below(2) == 0 {
            mxcsr &= !(u32::from(rng.next_u32() as u8 & 0x3F) << 7);
        }
        if daz_supported && rng.below(2) == 0 {
            mxcsr |= 1 << 6;
        }
        if rng.below(2) == 0 {
            mxcsr |= 1 << 15;
        }
        // Sticky status bits start set sometimes, so "already set" cannot mask
        // a missing update.
        mxcsr |= u32::from(rng.next_u32() as u8 & 0x3F) * u32::from(rng.below(2) == 0);
        mxcsr &= mask;

        let mut gprs = TAME_GPRS;
        if !tame_addresses {
            for register in &mut gprs {
                *register = rng.next_u32();
            }
        }

        let mut fpu_regs = [[0u8; 10]; 8];
        for register in &mut fpu_regs {
            for byte in register.iter_mut() {
                *byte = rng.next_u32() as u8;
            }
        }

        let mut ram = vec![0u8; RAM_SIZE];
        for byte in &mut ram {
            *byte = rng.next_u32() as u8;
        }

        GuestState {
            xmm,
            mxcsr,
            daz_supported,
            gprs,
            flags: rng.next_u32() as u16,
            cr4: if rng.below(2) == 0 {
                crate::CR4_OSXMMEXCPT
            } else {
                0
            },
            fpu_regs,
            fpu_status: rng.next_u32() as u16,
            fpu_control: rng.next_u32() as u16,
            fpu_tag: rng.next_u32() as u16,
            fpu_opcode: rng.next_u32() as u16,
            fpu_ip_offset: rng.next_u32(),
            fpu_ip_selector: rng.next_u32() as u16,
            fpu_operand_offset: rng.next_u32(),
            fpu_operand_selector: rng.next_u32() as u16,
            ram,
        }
    }

    fn apply_state(cpu: &mut Cpu, mem: &mut Ram, state: &GuestState) {
        cpu.regs.cr0 = 0;
        cpu.regs.cr4 = state.cr4;
        cpu.regs.ds = SEG_DS;
        cpu.regs.ss = SEG_SS;
        cpu.regs.es = SEG_ES;
        cpu.regs.cs = SEG_CS;
        cpu.regs.fs = SEG_FS;
        cpu.regs.gs = SEG_GS;
        cpu.regs.ds_cache.base = u32::from(SEG_DS) << 4;
        cpu.regs.ss_cache.base = u32::from(SEG_SS) << 4;
        cpu.regs.es_cache.base = u32::from(SEG_ES) << 4;
        cpu.regs.cs_cache.base = u32::from(SEG_CS) << 4;
        cpu.regs.fs_cache.base = u32::from(SEG_FS) << 4;
        cpu.regs.gs_cache.base = u32::from(SEG_GS) << 4;

        cpu.regs.eax = state.gprs[0];
        cpu.regs.ecx = state.gprs[1];
        cpu.regs.edx = state.gprs[2];
        cpu.regs.ebx = state.gprs[3];
        cpu.regs.esp = state.gprs[4];
        cpu.regs.ebp = state.gprs[5];
        cpu.regs.esi = state.gprs[6];
        cpu.regs.edi = state.gprs[7];
        cpu.regs.flags = Flags::from_bits_truncate(state.flags);

        cpu.sse.set_daz_supported(state.daz_supported);
        cpu.sse.xmm = state.xmm;
        cpu.sse.mxcsr = state.mxcsr;

        for (register, bytes) in cpu.fpu.regs.iter_mut().zip(state.fpu_regs.iter()) {
            register.0 = *bytes;
        }
        cpu.fpu.status_word = state.fpu_status;
        cpu.fpu.control_word = state.fpu_control;
        cpu.fpu.tag_word = state.fpu_tag;
        cpu.fpu.opcode = state.fpu_opcode;
        cpu.fpu.ip_offset = state.fpu_ip_offset;
        cpu.fpu.ip_selector = state.fpu_ip_selector;
        cpu.fpu.operand_offset = state.fpu_operand_offset;
        cpu.fpu.operand_selector = state.fpu_operand_selector;

        mem.data.copy_from_slice(&state.ram);
    }

    #[derive(PartialEq, Eq)]
    struct Snapshot {
        xmm: [u128; 8],
        mxcsr: u32,
        gprs: [u32; 8],
        flags: u16,
        fpu_regs: [[u8; 10]; 8],
        fpu_status: u16,
        fpu_control: u16,
        fpu_tag: u16,
        fpu_opcode: u16,
        fpu_ip_offset: u32,
        fpu_ip_selector: u16,
        fpu_operand_offset: u32,
        fpu_operand_selector: u16,
        ram: Vec<u8>,
        error: Option<String>,
    }

    impl std::fmt::Debug for Snapshot {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "xmm={:032X?} mxcsr={:08X} gprs={:08X?} flags={:04X} fsw={:04X} ftw={:04X} \
                 fcw={:04X} fpu_regs={:02X?} error={:?}",
                self.xmm,
                self.mxcsr,
                self.gprs,
                self.flags,
                self.fpu_status,
                self.fpu_tag,
                self.fpu_control,
                self.fpu_regs,
                self.error
            )
        }
    }

    /// `Unimplemented` carries a formatted instruction, which the two paths
    /// necessarily render differently — collapse it, and assert separately
    /// that neither path ever produces one.
    fn error_kind(error: &CpuError) -> String {
        match error {
            CpuError::Unimplemented(_) => "Unimplemented".to_string(),
            other => format!("{other:?}"),
        }
    }

    fn snapshot(cpu: &Cpu, mem: &Ram, outcome: &Result<()>) -> Snapshot {
        let mut fpu_regs = [[0u8; 10]; 8];
        for (destination, register) in fpu_regs.iter_mut().zip(cpu.fpu.regs.iter()) {
            *destination = register.0;
        }
        Snapshot {
            xmm: cpu.sse.xmm,
            mxcsr: cpu.sse.mxcsr,
            gprs: [
                cpu.regs.eax,
                cpu.regs.ecx,
                cpu.regs.edx,
                cpu.regs.ebx,
                cpu.regs.esp,
                cpu.regs.ebp,
                cpu.regs.esi,
                cpu.regs.edi,
            ],
            flags: cpu.regs.flags.bits(),
            fpu_regs,
            fpu_status: cpu.fpu.status_word,
            fpu_control: cpu.fpu.control_word,
            fpu_tag: cpu.fpu.tag_word,
            fpu_opcode: cpu.fpu.opcode,
            fpu_ip_offset: cpu.fpu.ip_offset,
            fpu_ip_selector: cpu.fpu.ip_selector,
            fpu_operand_offset: cpu.fpu.operand_offset,
            fpu_operand_selector: cpu.fpu.operand_selector,
            ram: mem.data.clone(),
            error: outcome.as_ref().err().map(error_kind),
        }
    }

    // ------------------------------------------------------------------
    // Encoding corpus
    // ------------------------------------------------------------------

    /// `(mod/rm byte template, trailing address bytes)` shapes to try beside
    /// each canonical encoding. The ModR/M reg field is supplied by the
    /// canonical encoding, so group-encoded forms keep their sub-opcode.
    fn address_shapes(bitness: u8) -> Vec<(u8, Vec<u8>)> {
        if bitness == 32 {
            vec![
                (0x01, vec![]),                       // [ECX]
                (0x43, vec![0x10]),                   // [EBX+0x10]
                (0x86, vec![0x30, 0x00, 0x00, 0x00]), // [ESI+0x30]
                (0x04, vec![0x90]),                   // [EAX+EDX*4]
                (0x05, vec![0x80, 0x20, 0x00, 0x00]), // [0x2080]
            ]
        } else {
            vec![
                (0x00, vec![]),           // [BX+SI]
                (0x46, vec![0x08]),       // [BP+8], SS-relative
                (0x84, vec![0x60, 0x10]), // [SI+0x1060]
                (0x06, vec![0x80, 0x10]), // [0x1080]
            ]
        }
    }

    /// Segment-override prefixes, plus no override.
    const SEGMENT_PREFIXES: &[Option<u8>] = &[None, Some(0x64), Some(0x36), Some(0x2E)];

    struct Encoding {
        bytes: Vec<u8>,
        bitness: u8,
    }

    /// Every encoding of `op` the sweep drives, filtered to those our decoder
    /// really produces for that form.
    fn encodings_for(op: SseOp, immediates: &[u8], extended: bool) -> Vec<Encoding> {
        let (_, prefix, opcode, canonical_modrm) = *CANONICAL
            .iter()
            .find(|(candidate, ..)| *candidate == op)
            .expect("every variant has a canonical encoding");
        let reg_field = (canonical_modrm >> 3) & 7;
        let mut out = Vec::new();

        for &bitness in &[32u8, 16u8] {
            let mut modrm_shapes: Vec<(u8, Vec<u8>)> = vec![(canonical_modrm, vec![])];
            // Other register pairings.
            for rm in [0u8, 3, 7] {
                modrm_shapes.push((0xC0 | (reg_field << 3) | rm, vec![]));
            }
            for (template, tail) in address_shapes(bitness) {
                modrm_shapes.push(((template & 0xC7) | (reg_field << 3), tail));
            }
            if extended {
                // A second reg field, where the form allows one.
                for rm in [1u8, 5] {
                    modrm_shapes.push((0xC0 | (2 << 3) | rm, vec![]));
                }
            }

            for (modrm, tail) in modrm_shapes {
                // A segment override only changes behaviour when the encoding
                // has a memory operand. MASKMOVQ/MASKMOVDQU are the case a
                // ModR/M test misses: their operand is the implicit DS:rDI,
                // so a register-form encoding still honours the override.
                let has_memory = modrm < 0xC0 || matches!(op, SseOp::Maskmovq | SseOp::Maskmovdqu);
                let segments: &[Option<u8>] = if !has_memory {
                    &SEGMENT_PREFIXES[..1]
                } else if extended {
                    SEGMENT_PREFIXES
                } else {
                    &SEGMENT_PREFIXES[..2]
                };
                for segment in segments {
                    for &immediate in immediates {
                        let mut bytes = Vec::with_capacity(8);
                        if let Some(segment) = segment {
                            bytes.push(*segment);
                        }
                        bytes.extend_from_slice(prefix);
                        bytes.push(0x0F);
                        bytes.push(opcode);
                        bytes.push(modrm);
                        bytes.extend_from_slice(&tail);
                        if op.has_imm8() {
                            bytes.push(immediate);
                        }
                        let decoded = match sse_decode::decode(&bytes, bitness) {
                            SseDecode::Decoded(instr) => instr,
                            _ => continue,
                        };
                        if decoded.op != op || usize::from(decoded.len) != bytes.len() {
                            continue;
                        }
                        out.push(Encoding { bytes, bitness });
                        if !op.has_imm8() {
                            break;
                        }
                    }
                }
            }
        }
        out
    }

    // ------------------------------------------------------------------
    // The migration oracle: old iced-driven executor vs the new one
    // ------------------------------------------------------------------

    struct Differential {
        old_cpu: Cpu,
        new_cpu: Cpu,
        old_mem: Ram,
        new_mem: Ram,
        cases: usize,
        /// Per-op `(register-form cases, memory-form cases, faulting cases)`,
        /// so a corpus that silently stopped covering a shape is visible.
        coverage: std::collections::BTreeMap<&'static str, [usize; 3]>,
    }

    impl Differential {
        fn new() -> Self {
            Self {
                old_cpu: Cpu::new(),
                new_cpu: Cpu::new(),
                old_mem: Ram::new(),
                new_mem: Ram::new(),
                cases: 0,
                coverage: std::collections::BTreeMap::new(),
            }
        }

        fn run(&mut self, encoding: &Encoding, state: &GuestState) {
            let instruction = decode_uncached(&encoding.bytes, encoding.bitness);
            let decoded = match sse_decode::decode(&encoding.bytes, encoding.bitness) {
                SseDecode::Decoded(instr) => instr,
                other => panic!("{:02X?} decoded as {other:?}", encoding.bytes),
            };
            assert_eq!(
                instruction.len(),
                usize::from(decoded.len),
                "EIP advance disagrees for {:02X?} @{}",
                encoding.bytes,
                encoding.bitness
            );

            apply_state(&mut self.old_cpu, &mut self.old_mem, state);
            apply_state(&mut self.new_cpu, &mut self.new_mem, state);

            let old_outcome =
                sse_exec_reference::execute(&mut self.old_cpu, &instruction, &mut self.old_mem);
            let new_outcome = execute(&mut self.new_cpu, &decoded, &mut self.new_mem);

            for outcome in [&old_outcome, &new_outcome] {
                if let Err(CpuError::Unimplemented(message)) = outcome {
                    panic!(
                        "{} reached an unimplemented arm ({message}) for {:02X?}",
                        decoded.op.name(),
                        encoding.bytes
                    );
                }
            }

            let old = snapshot(&self.old_cpu, &self.old_mem, &old_outcome);
            let new = snapshot(&self.new_cpu, &self.new_mem, &new_outcome);
            assert!(
                old == new,
                "{} diverged for {:02X?} @{}\n  old: {old:?}\n  new: {new:?}\n  ram differs: {}",
                decoded.op.name(),
                encoding.bytes,
                encoding.bitness,
                old.ram != new.ram
            );
            let counts = self.coverage.entry(decoded.op.name()).or_default();
            if decoded.operands.memory_operand().is_some() {
                counts[1] += 1;
            } else {
                counts[0] += 1;
            }
            if new.error.is_some() {
                counts[2] += 1;
            }
            self.cases += 1;
        }
    }

    fn run_migration_differential(
        states: usize,
        immediates: &[u8],
        extended: bool,
    ) -> Differential {
        let mut differential = Differential::new();
        let mut seed = 0x5EED_0000_0000_0001u64;
        let mut chooser = Rng::new(0x00C0_FFEE);
        for (op, ..) in CANONICAL {
            for encoding in encodings_for(*op, immediates, extended) {
                for _ in 0..states {
                    seed = seed.wrapping_add(0x1234_5678_9ABC_DEF1);
                    // One state in four uses out-of-range address registers,
                    // so fault parity is exercised alongside value parity.
                    // The choice comes from its own mixed stream: the case
                    // seed advances by a constant, so any bit of it taken
                    // directly stays constant across long runs of cases.
                    let state = guest_state(seed, chooser.below(4) != 0);
                    differential.run(&encoding, &state);
                }
            }
        }
        // Every encoding form must have been driven at least once; a decode
        // change that silently stopped producing one would otherwise leave
        // that form untested and the differential still green.
        assert_eq!(
            differential.coverage.len(),
            SseOp::COUNT,
            "corpus missed some encoding forms"
        );
        differential
    }

    #[test]
    fn new_executor_matches_the_iced_executor() {
        // The immediates must span both compare-predicate classes: 1 and 5
        // signal on a quiet NaN, 3 and 7 do not. A set of only quiet
        // predicates leaves the signalling half of the matrix untested.
        let differential = run_migration_differential(2, &[0x01, 0x03, 0x05, 0xFF], false);
        // Floors just under what the corpus actually produces (12,592 cases,
        // 8,368 of them with a memory operand and 1,535 faulting), so a
        // generator change that quietly stops covering a shape trips here
        // rather than leaving the differential green over a thin corpus.
        assert!(
            differential.cases > 12_000,
            "corpus collapsed to {} cases",
            differential.cases
        );
        let memory_cases: usize = differential.coverage.values().map(|counts| counts[1]).sum();
        let faults: usize = differential.coverage.values().map(|counts| counts[2]).sum();
        assert!(
            memory_cases > 8_000,
            "corpus lost its memory operands ({memory_cases})"
        );
        assert!(
            faults > 1_400,
            "corpus stopped exercising faults ({faults})"
        );
    }

    #[test]
    #[ignore = "extended differential; full_test (--include-ignored)"]
    fn new_executor_matches_the_iced_executor_extended() {
        let differential = run_migration_differential(
            8,
            &[0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0xFF],
            true,
        );
        assert!(
            differential.cases > 110_000,
            "corpus collapsed to {} cases",
            differential.cases
        );
    }

    // ------------------------------------------------------------------
    // Decode -> execute smoke sweep
    // ------------------------------------------------------------------

    #[test]
    fn every_variant_executes_from_its_canonical_encoding() {
        let mut cpu = Cpu::new();
        let mut mem = Ram::new();
        let state = guest_state(7, true);
        let mut seen = 0;
        for (op, ..) in CANONICAL {
            let bytes = crate::sse_decode::tests::canonical_encoding(*op);
            let decoded = match sse_decode::decode(&bytes, 32) {
                SseDecode::Decoded(instr) => instr,
                other => panic!("{op:?} canonical encoding decoded as {other:?}"),
            };
            assert_eq!(decoded.op, *op);
            apply_state(&mut cpu, &mut mem, &state);
            if let Err(CpuError::Unimplemented(message)) = execute(&mut cpu, &decoded, &mut mem) {
                panic!("{op:?} has no executor arm: {message}");
            }
            seen += 1;
        }
        assert_eq!(seen, SseOp::COUNT, "every variant must be swept");
    }

    // ------------------------------------------------------------------
    // Quirk tests
    // ------------------------------------------------------------------

    /// Build one instruction from raw bytes.
    /// MDD-BUG-FLU-02382: every SSE float-to-int32 conversion reports
    /// Precision only for a denormal source, never Denormal. Silicon probe
    /// (MXCSR 0x1F80, black_box inputs, unoptimised; Intel i7-14700KF and
    /// Core Ultra 7 270K Plus, AMD Ryzen 9 9950X3D, all identical): a
    /// denormal lane converts to 0 and MXCSR becomes 0x1FA0. With DAZ set the
    /// source is flushed first, so the conversion is exact and sets nothing.
    #[test]
    fn float_to_int32_conversions_never_report_denormal() {
        const PE: u32 = 1 << 5;
        const DAZ: u32 = 1 << 6;
        // Lane 0 is the smallest positive denormal; the other lanes are 2.0.
        let singles: u128 = 0x4000_0000_4000_0000_4000_0000_0000_0001;
        let doubles: u128 = 0x4000_0000_0000_0000_0000_0000_0000_0001;
        let cases: &[(&str, &[u8], u128, u64)] = &[
            ("cvtss2si", &[0xF3, 0x0F, 0x2D, 0xC1], singles, 0),
            ("cvttss2si", &[0xF3, 0x0F, 0x2C, 0xC1], singles, 0),
            ("cvtsd2si", &[0xF2, 0x0F, 0x2D, 0xC1], doubles, 0),
            ("cvttsd2si", &[0xF2, 0x0F, 0x2C, 0xC1], doubles, 0),
            ("cvtps2pi", &[0x0F, 0x2D, 0xC1], singles, 0x2_0000_0000),
            ("cvttps2pi", &[0x0F, 0x2C, 0xC1], singles, 0x2_0000_0000),
            (
                "cvtpd2pi",
                &[0x66, 0x0F, 0x2D, 0xC1],
                doubles,
                0x2_0000_0000,
            ),
            (
                "cvttpd2pi",
                &[0x66, 0x0F, 0x2C, 0xC1],
                doubles,
                0x2_0000_0000,
            ),
            (
                "cvtps2dq",
                &[0x66, 0x0F, 0x5B, 0xC1],
                singles,
                0x2_0000_0000,
            ),
            (
                "cvttps2dq",
                &[0xF3, 0x0F, 0x5B, 0xC1],
                singles,
                0x2_0000_0000,
            ),
            (
                "cvtpd2dq",
                &[0xF2, 0x0F, 0xE6, 0xC1],
                doubles,
                0x2_0000_0000,
            ),
            (
                "cvttpd2dq",
                &[0x66, 0x0F, 0xE6, 0xC1],
                doubles,
                0x2_0000_0000,
            ),
        ];
        for &(name, bytes, source, low64) in cases {
            for (mxcsr, want) in [(0x1F80, 0x1F80 | PE), (0x1F80 | DAZ, 0x1F80 | DAZ)] {
                let mut cpu = Cpu::new();
                let mut mem = Ram::new();
                cpu.sse.mxcsr = mxcsr;
                cpu.sse.mxcsr_mask = 0xFFFF;
                cpu.sse.xmm[1] = source;
                cpu.regs.eax = 0xDEAD_BEEF;
                execute(&mut cpu, &decoded_bytes(bytes), &mut mem).expect(name);
                assert_eq!(cpu.sse.mxcsr, want, "{name} mxcsr {mxcsr:#x}");
                let result = if name.ends_with("si") {
                    u64::from(cpu.regs.eax)
                } else if name.ends_with("pi") {
                    cpu.fpu.read_mmx_register_committed(0)
                } else {
                    cpu.sse.xmm[0] as u64
                };
                assert_eq!(result, low64, "{name} result mxcsr {mxcsr:#x}");
            }
        }
    }

    fn decoded_bytes(bytes: &[u8]) -> SseInstr {
        match sse_decode::decode(bytes, 32) {
            SseDecode::Decoded(instr) => instr,
            other => panic!("{bytes:02X?} decoded as {other:?}"),
        }
    }

    struct Machine {
        cpu: Cpu,
        mem: Ram,
    }

    impl Machine {
        /// A quiet machine: real mode, flat DS, default MXCSR, zeroed state.
        fn new() -> Self {
            let mut cpu = Cpu::new();
            cpu.regs.cr0 = 0;
            cpu.regs.cr4 = crate::CR4_OSXMMEXCPT;
            for (selector, cache) in [
                (SEG_DS, &mut cpu.regs.ds_cache),
                (SEG_SS, &mut cpu.regs.ss_cache),
            ] {
                cache.base = u32::from(selector) << 4;
            }
            cpu.regs.ds = SEG_DS;
            cpu.regs.ss = SEG_SS;
            cpu.regs.ecx = 0x2000;
            cpu.sse.set_daz_supported(true);
            cpu.sse.mxcsr = 0x1F80;
            cpu.sse.xmm = [0; 8];
            Self {
                cpu,
                mem: Ram::new(),
            }
        }

        fn execute(&mut self, bytes: &[u8]) -> Result<()> {
            execute(&mut self.cpu, &decoded_bytes(bytes), &mut self.mem)
        }
    }

    const QNAN32: u32 = 0x7FC0_0000;
    const SNAN32: u32 = 0x7F80_0001;

    #[test]
    fn minps_takes_the_second_operand_on_any_nan() {
        let mut machine = Machine::new();
        machine.cpu.sse.xmm[0] = u128::from(QNAN32);
        machine.cpu.sse.xmm[1] = u128::from(0x3F80_0000u32);
        // MINPS xmm0, xmm1
        machine.execute(&[0x0F, 0x5D, 0xC1]).expect("minps");
        assert_eq!(lane32(machine.cpu.sse.xmm[0], 0), 0x3F80_0000);
        // With the NaN second, the NaN is what gets taken.
        machine.cpu.sse.xmm[0] = u128::from(0x3F80_0000u32);
        machine.cpu.sse.xmm[1] = u128::from(QNAN32);
        machine.execute(&[0x0F, 0x5D, 0xC1]).expect("minps");
        assert_eq!(lane32(machine.cpu.sse.xmm[0], 0), QNAN32);
        // EVERY NaN raises invalid-operation, quiet included — MIN/MAX are
        // unusual that way, and both the SDM and this host's silicon say so
        // (MDD-BUG-CRUCIBLE-00772). This assertion read `0` for the quiet
        // case until then, which is the defect it now guards.
        assert_eq!(machine.cpu.sse.mxcsr & 1, 1);
        machine.cpu.sse.mxcsr &= !1;
        machine.cpu.sse.xmm[1] = u128::from(SNAN32);
        machine.execute(&[0x0F, 0x5D, 0xC1]).expect("minps");
        assert_eq!(machine.cpu.sse.mxcsr & 1, 1);
    }

    #[test]
    fn maxps_takes_the_second_operand_on_any_nan() {
        let mut machine = Machine::new();
        machine.cpu.sse.xmm[0] = u128::from(QNAN32);
        machine.cpu.sse.xmm[1] = u128::from(0xBF80_0000u32);
        // MAXPS xmm0, xmm1 — the NaN loses even though it is "larger".
        machine.execute(&[0x0F, 0x5F, 0xC1]).expect("maxps");
        assert_eq!(lane32(machine.cpu.sse.xmm[0], 0), 0xBF80_0000);
    }

    #[test]
    fn cmpps_quiets_only_the_ordering_predicates() {
        // Predicates 1 (LT), 2 (LE), 5 (NLT), 6 (NLE) signal on a quiet NaN;
        // 0 (EQ), 3 (UNORD), 4 (NEQ), 7 (ORD) do not.
        for predicate in 0u8..8 {
            let mut machine = Machine::new();
            machine.cpu.sse.xmm[0] = u128::from(QNAN32);
            machine.cpu.sse.xmm[1] = 0;
            machine
                .execute(&[0x0F, 0xC2, 0xC1, predicate])
                .expect("cmpps");
            let signalled = machine.cpu.sse.mxcsr & 1 != 0;
            assert_eq!(
                signalled,
                matches!(predicate, 1 | 2 | 5 | 6),
                "predicate {predicate} invalid-operation flag"
            );
            let lane = lane32(machine.cpu.sse.xmm[0], 0);
            // UNORD, NEQ, NLT and NLE all hold when a comparison is unordered.
            let expected = matches!(predicate, 3..=6);
            assert_eq!(
                lane == u32::MAX,
                expected,
                "predicate {predicate} result lane {lane:08X}"
            );
        }
    }

    #[test]
    fn daz_suppresses_the_denormal_flag() {
        let denormal = 0x0000_0001u32;
        let mut machine = Machine::new();
        machine.cpu.sse.xmm[0] = u128::from(0x3F80_0000u32);
        machine.cpu.sse.xmm[1] = u128::from(denormal);
        machine.execute(&[0x0F, 0x58, 0xC1]).expect("addps");
        assert_eq!(machine.cpu.sse.mxcsr & 2, 2, "DE without DAZ");

        let mut machine = Machine::new();
        machine.cpu.sse.mxcsr = 0x1F80 | (1 << 6);
        machine.cpu.sse.xmm[0] = u128::from(0x3F80_0000u32);
        machine.cpu.sse.xmm[1] = u128::from(denormal);
        machine.execute(&[0x0F, 0x58, 0xC1]).expect("addps");
        assert_eq!(machine.cpu.sse.mxcsr & 2, 0, "DAZ must suppress DE");
        assert_eq!(lane32(machine.cpu.sse.xmm[0], 0), 0x3F80_0000);
    }

    #[test]
    fn ftz_flushes_any_tiny_result_not_only_an_inexact_one() {
        // (2^-126 + 1ulp) * 0.5 is tiny AND inexact; FTZ replaces the denormal
        // result by a signed zero. This half was always right.
        let mut machine = Machine::new();
        machine.cpu.sse.mxcsr = 0x1F80 | (1 << 15);
        machine.cpu.sse.xmm[0] = u128::from(0x0080_0001u32);
        machine.cpu.sse.xmm[1] = u128::from(0x3F00_0000u32);
        machine.execute(&[0x0F, 0x59, 0xC1]).expect("mulps");
        assert_eq!(lane32(machine.cpu.sse.xmm[0], 0), 0);
        assert_eq!(machine.cpu.sse.mxcsr & (1 << 4), 1 << 4, "UE is recorded");

        // A tiny-but-EXACT result is flushed just the same. This assertion
        // used to expect the denormal to survive, which pinned
        // MDD-BUG-CRUCIBLE-00787 as if it were intent. Re-measured on this
        // box: `MULSS 2^-126, 0.5` delivers +0 with UE|PE, not 0x00400000.
        let mut machine = Machine::new();
        machine.cpu.sse.mxcsr = 0x1F80 | (1 << 15);
        machine.cpu.sse.xmm[0] = u128::from(0x0080_0000u32);
        machine.cpu.sse.xmm[1] = u128::from(0x3F00_0000u32);
        machine.execute(&[0x0F, 0x59, 0xC1]).expect("mulps");
        assert_eq!(lane32(machine.cpu.sse.xmm[0], 0), 0);
        assert_eq!(machine.cpu.sse.mxcsr & (1 << 4), 1 << 4, "UE is recorded");

        // And a denormal that merely passes through is a tiny RESULT, so it
        // is flushed too - again measured, `ADDSS denormal, 0` gives +0 with
        // UE|PE|DE where this expected the denormal back.
        let mut machine = Machine::new();
        machine.cpu.sse.mxcsr = 0x1F80 | (1 << 15);
        machine.cpu.sse.xmm[0] = u128::from(0x0000_0001u32);
        machine.cpu.sse.xmm[1] = 0;
        machine.execute(&[0x0F, 0x58, 0xC1]).expect("addps");
        assert_eq!(lane32(machine.cpu.sse.xmm[0], 0), 0);
        assert_eq!(machine.cpu.sse.mxcsr & (1 << 4), 1 << 4, "UE is recorded");
    }

    #[test]
    fn out_of_range_conversion_yields_the_integer_indefinite() {
        let mut machine = Machine::new();
        // 2^31 as f32 does not fit a signed 32-bit integer.
        machine.cpu.sse.xmm[1] = u128::from(0x4F00_0000u32);
        // CVTTSS2SI eax, xmm1
        machine
            .execute(&[0xF3, 0x0F, 0x2C, 0xC1])
            .expect("cvttss2si");
        assert_eq!(machine.cpu.regs.eax, 0x8000_0000);
        assert_eq!(machine.cpu.sse.mxcsr & 1, 1, "invalid-operation is sticky");
    }

    #[test]
    fn movss_merges_from_a_register_and_replaces_from_memory() {
        let mut machine = Machine::new();
        machine.cpu.sse.xmm[0] = u128::MAX;
        machine.cpu.sse.xmm[1] = u128::from(0x1234_5678u32);
        // MOVSS xmm0, xmm1 — only the low doubleword changes.
        machine.execute(&[0xF3, 0x0F, 0x10, 0xC1]).expect("movss");
        assert_eq!(machine.cpu.sse.xmm[0], (u128::MAX << 32) | 0x1234_5678);

        // MOVSS xmm0, [ECX] — the upper 96 bits are zeroed.
        machine.cpu.sse.xmm[0] = u128::MAX;
        machine.mem.data[0x2000..0x2004].copy_from_slice(&0xAABB_CCDDu32.to_le_bytes());
        machine.execute(&[0xF3, 0x0F, 0x10, 0x01]).expect("movss");
        assert_eq!(machine.cpu.sse.xmm[0], 0xAABB_CCDD);
    }

    #[test]
    fn movd_zero_extends_into_the_whole_register() {
        let mut machine = Machine::new();
        machine.cpu.sse.xmm[0] = u128::MAX;
        machine.cpu.regs.ecx = 0x8899_AABB;
        // MOVD xmm0, ecx
        machine.execute(&[0x66, 0x0F, 0x6E, 0xC1]).expect("movd");
        assert_eq!(machine.cpu.sse.xmm[0], 0x8899_AABB);
    }

    #[test]
    fn an_unmasked_exception_leaves_the_destination_and_sets_the_sticky_bit() {
        let mut machine = Machine::new();
        // Unmask the invalid-operation exception only.
        machine.cpu.sse.mxcsr = 0x1F80 & !(1 << 7);
        machine.cpu.sse.xmm[0] = u128::from(SNAN32);
        machine.cpu.sse.xmm[1] = u128::from(0x3F80_0000u32);
        let before = machine.cpu.sse.xmm[0];
        let outcome = machine.execute(&[0x0F, 0x58, 0xC1]);
        assert!(
            matches!(outcome, Err(CpuError::Interrupt(19))),
            "CR4.OSXMMEXCPT selects #XM, got {outcome:?}"
        );
        assert_eq!(
            machine.cpu.sse.xmm[0], before,
            "destination must not commit"
        );
        assert_eq!(machine.cpu.sse.mxcsr & 1, 1, "IE is still recorded");

        // Without CR4.OSXMMEXCPT the same condition raises #UD.
        machine.cpu.regs.cr4 = 0;
        machine.cpu.sse.mxcsr = 0x1F80 & !(1 << 7);
        let outcome = machine.execute(&[0x0F, 0x58, 0xC1]);
        assert!(
            matches!(outcome, Err(CpuError::Interrupt(6))),
            "{outcome:?}"
        );
    }

    #[test]
    fn comiss_sets_the_eflags_the_architecture_defines() {
        let cases: &[(u32, u32, u16, u16)] = &[
            // (left, right, set flags, cleared flags) — ZF|PF|CF = 0x45.
            (0x3F80_0000, 0x4000_0000, 0x0001, 0x0044), // 1.0 <  2.0 -> CF
            (0x4000_0000, 0x3F80_0000, 0x0000, 0x0045), // 2.0 >  1.0 -> none
            (0x3F80_0000, 0x3F80_0000, 0x0040, 0x0005), // 1.0 == 1.0 -> ZF
            (QNAN32, 0x3F80_0000, 0x0045, 0x0000),      // unordered -> ZF|PF|CF
        ];
        for &(left, right, set, cleared) in cases {
            let mut machine = Machine::new();
            // Mask everything so an ordered compare against a NaN still
            // completes and publishes flags.
            machine.cpu.sse.mxcsr = 0x1F80;
            machine.cpu.sse.xmm[0] = u128::from(left);
            machine.cpu.sse.xmm[1] = u128::from(right);
            machine.cpu.regs.flags = Flags::from_bits_truncate(0x08D5);
            machine.execute(&[0x0F, 0x2F, 0xC1]).expect("comiss");
            let flags = machine.cpu.regs.flags.bits();
            assert_eq!(flags & set, set, "{left:08X} vs {right:08X}: set bits");
            assert_eq!(
                flags & cleared,
                0,
                "{left:08X} vs {right:08X}: cleared bits"
            );
            // OF, SF and AF are always cleared.
            assert_eq!(flags & 0x0890, 0);
        }
    }

    #[test]
    fn maskmovdqu_faults_on_a_masked_off_byte() {
        let mut machine = Machine::new();
        // Every mask byte clear, so no byte is architecturally stored — the
        // write permission of the whole operand is still checked.
        machine.cpu.sse.xmm[0] = u128::MAX;
        machine.cpu.sse.xmm[1] = 0;
        // Put EDI one byte below the 64 KB real-mode segment limit.
        machine.cpu.regs.edi = 0xFFFF;
        let outcome = machine.execute(&[0x66, 0x0F, 0xF7, 0xC1]);
        assert!(
            matches!(outcome, Err(CpuError::GeneralProtectionFault)),
            "a masked-off MASKMOVDQU still faults as a write, got {outcome:?}"
        );
    }

    #[test]
    fn mmx_source_sse_instructions_enter_mmx_state_on_retirement() {
        // Every MMX-source SSE instruction transitions the x87 register file
        // only after its result has committed.  Keep a non-zero TOP/tag here
        // so a missing transition is observable.
        let cases: &[(&[u8], u32)] = &[
            (&[0x0F, 0xE7, 0x01], 0x2000),       // MOVNTQ [ECX], MM0
            (&[0xF3, 0x0F, 0xD6, 0xC1], 0x2000), // MOVQ2DQ XMM0, MM1
            (&[0x0F, 0xD7, 0xC1], 0x2000),       // PMOVMSKB EAX, MM1
            (&[0x0F, 0xC5, 0xC1, 0x02], 0x2000), // PEXTRW EAX, MM1, 2
            (&[0x0F, 0xF7, 0xC1], 0x3000),       // MASKMOVQ MM0, MM1
            (&[0x0F, 0x2A, 0xC1], 0x2000),       // CVTPI2PS XMM0, MM1
            (&[0x66, 0x0F, 0x2A, 0xC1], 0x2000), // CVTPI2PD XMM0, MM1
        ];
        for &(bytes, address) in cases {
            let mut machine = Machine::new();
            machine.cpu.regs.ecx = address;
            machine.cpu.regs.edi = address;
            machine.cpu.fpu.write_mmx_register(0, 0x8877_6655_4433_2211);
            machine.cpu.fpu.write_mmx_register(1, 0x8001_0002_0003_0004);
            machine.cpu.fpu.set_top(5);
            machine.cpu.fpu.tag_word = 0x5A5A;
            machine.execute(bytes).expect("MMX-source SSE instruction");
            assert_eq!(machine.cpu.fpu.get_top(), 0, "{bytes:02X?}: TOP");
            assert_eq!(machine.cpu.fpu.tag_word, 0, "{bytes:02X?}: tag word");
        }
    }

    #[test]
    fn movntq_fault_does_not_enter_mmx_state() {
        let mut machine = Machine::new();
        machine.cpu.regs.edi = 0xFFFF;
        machine.cpu.fpu.write_mmx_register(0, 0x8877_6655_4433_2211);
        machine.cpu.fpu.set_top(5);
        machine.cpu.fpu.tag_word = 0x5A5A;
        let outcome = machine.execute(&[0x0F, 0xE7, 0x07]); // MOVNTQ [EDI], MM0
        assert!(
            matches!(outcome, Err(CpuError::GeneralProtectionFault)),
            "MOVNTQ must fault before its store, got {outcome:?}"
        );
        assert_eq!(machine.cpu.fpu.get_top(), 5);
        assert_eq!(machine.cpu.fpu.tag_word, 0x5A5A);
    }

    #[test]
    fn only_the_aligned_move_class_checks_alignment() {
        // MOVAPS xmm0, [ECX+1] faults; MOVUPS at the same address does not.
        let mut machine = Machine::new();
        machine.cpu.regs.ecx = 0x2001;
        assert!(matches!(
            machine.execute(&[0x0F, 0x28, 0x01]),
            Err(CpuError::GeneralProtectionFault)
        ));
        machine.execute(&[0x0F, 0x10, 0x01]).expect("movups");
        // MOVDQA and the non-temporal stores are in the class; ADDPS is not.
        assert!(matches!(
            machine.execute(&[0x66, 0x0F, 0x6F, 0x01]),
            Err(CpuError::GeneralProtectionFault)
        ));
        assert!(matches!(
            machine.execute(&[0x0F, 0x2B, 0x01]),
            Err(CpuError::GeneralProtectionFault)
        ));
        machine.execute(&[0x0F, 0x58, 0x01]).expect("addps");
        // FXSAVE/FXRSTOR are checked too.
        assert!(matches!(
            machine.execute(&[0x0F, 0xAE, 0x01]),
            Err(CpuError::GeneralProtectionFault)
        ));
        assert!(matches!(
            machine.execute(&[0x0F, 0xAE, 0x09]),
            Err(CpuError::GeneralProtectionFault)
        ));
    }

    #[test]
    fn ldmxcsr_rejects_reserved_bits() {
        let mut machine = Machine::new();
        machine.cpu.sse.set_daz_supported(false);
        machine.mem.data[0x2000..0x2004].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        assert!(matches!(
            machine.execute(&[0x0F, 0xAE, 0x11]),
            Err(CpuError::GeneralProtectionFault)
        ));
        // Bit 6 (DAZ) is reserved on an identity that does not support it.
        machine.mem.data[0x2000..0x2004].copy_from_slice(&0x0000_1FC0u32.to_le_bytes());
        assert!(matches!(
            machine.execute(&[0x0F, 0xAE, 0x11]),
            Err(CpuError::GeneralProtectionFault)
        ));
        machine.mem.data[0x2000..0x2004].copy_from_slice(&0x0000_3F80u32.to_le_bytes());
        machine.execute(&[0x0F, 0xAE, 0x11]).expect("ldmxcsr");
        assert_eq!(machine.cpu.sse.mxcsr, 0x3F80);
    }

    #[test]
    fn fxsave_and_fxrstor_round_trip_the_whole_image() {
        let mut machine = Machine::new();
        machine.cpu.sse.set_daz_supported(true);
        machine.cpu.sse.mxcsr = 0x0000_9F40;
        for (index, register) in machine.cpu.sse.xmm.iter_mut().enumerate() {
            *register = u128::from(index as u64 + 1) * 0x0101_0101_0101_0101;
        }
        for (index, register) in machine.cpu.fpu.regs.iter_mut().enumerate() {
            register.0 = [index as u8 + 0x11; 10];
            // A valid, normal 80-bit significand so the restored tag is 0.
            register.0[7] = 0x80;
            register.0[8] = 0x01;
            register.0[9] = 0x40;
        }
        machine.cpu.fpu.control_word = 0x037A;
        machine.cpu.fpu.status_word = 0x1234;
        machine.cpu.fpu.tag_word = 0xF0F0;
        machine.cpu.fpu.opcode = 0x07AB;
        machine.cpu.fpu.ip_offset = 0xDEAD_BEEF;
        machine.cpu.fpu.ip_selector = 0x1357;
        machine.cpu.fpu.operand_offset = 0xFEED_FACE;
        machine.cpu.fpu.operand_selector = 0x2468;
        let saved_xmm = machine.cpu.sse.xmm;
        let saved_regs: Vec<[u8; 10]> = machine.cpu.fpu.regs.iter().map(|r| r.0).collect();

        machine.execute(&[0x0F, 0xAE, 0x01]).expect("fxsave");
        // The abridged tag word records only which registers are non-empty:
        // 0xF0F0 tags R2, R3, R6 and R7 empty, so their bits are clear.
        assert_eq!(machine.mem.data[0x2004], 0x33);

        machine.cpu.sse.xmm = [0; 8];
        machine.cpu.sse.mxcsr = 0x1F80;
        machine.cpu.fpu.tag_word = 0;
        for register in machine.cpu.fpu.regs.iter_mut() {
            register.0 = [0; 10];
        }
        machine.execute(&[0x0F, 0xAE, 0x09]).expect("fxrstor");

        assert_eq!(machine.cpu.sse.xmm, saved_xmm);
        assert_eq!(machine.cpu.sse.mxcsr, 0x0000_9F40);
        assert_eq!(machine.cpu.fpu.control_word, 0x037A);
        assert_eq!(machine.cpu.fpu.status_word, 0x1234);
        assert_eq!(machine.cpu.fpu.opcode, 0x07AB);
        assert_eq!(machine.cpu.fpu.ip_offset, 0xDEAD_BEEF);
        assert_eq!(machine.cpu.fpu.operand_offset, 0xFEED_FACE);
        for (register, saved) in machine.cpu.fpu.regs.iter().zip(saved_regs) {
            assert_eq!(register.0, saved);
        }
        // The four registers the abridged word marked empty come back tagged
        // empty; the rest are re-classified from their contents as valid, so
        // this particular tag word survives the round trip exactly.
        assert_eq!(machine.cpu.fpu.tag_word, 0xF0F0);
    }

    /// MDD-BUG-TEM-00163: the FXSAVE register area is ST-relative — slot i is
    /// ST(i), the physical register (TOP + i) & 7 (SDM Vol 1 Fig 10-2) — and
    /// FXRSTOR reads it back the same way. Both used physical order, which a
    /// round trip cannot see and a guest reading the image can. The sequence
    /// is the host probe's (`x87_oracle.rs`
    /// `host_fnsave_register_area_is_st_relative_not_physical`): after FNINIT;
    /// FLD1; FLDZ; FLDPI, TOP is 5 and pi (ST0, R5) belongs in slot 0.
    #[test]
    fn fxsave_and_fxrstor_register_area_is_st_relative() {
        use crate::x87_ops::F80;

        let mut machine = Machine::new();
        machine.cpu.fpu.push(F80::from_f64(1.0));
        machine.cpu.fpu.push(F80::from_f64(0.0));
        machine.cpu.fpu.push(F80::from_f64(std::f64::consts::PI));
        assert_eq!(machine.cpu.fpu.get_top(), 5, "the fixture needs TOP != 0");

        machine.execute(&[0x0F, 0xAE, 0x01]).expect("fxsave");
        let slot = |st: usize| {
            let offset = 0x2000 + 32 + st * 16;
            machine.mem.data[offset..offset + 10].to_vec()
        };
        assert_eq!(
            slot(0),
            F80::from_f64(std::f64::consts::PI).0,
            "slot 0 is ST(0)"
        );
        assert_eq!(slot(1), F80::from_f64(0.0).0, "slot 1 is ST(1)");
        assert_eq!(slot(2), F80::from_f64(1.0).0, "slot 2 is ST(2)");
        // The abridged tag byte stays physical: R5, R6 and R7 are valid.
        assert_eq!(machine.mem.data[0x2004], 0xE0);

        // FXRSTOR under the image's TOP of 5 puts slot i back in ST(i), and
        // tags from the abridged byte by PHYSICAL register: the value written
        // into slot 3 lands in R0, which bit 0 of 0xE1 says is valid.
        let offset = 0x2000 + 32 + 3 * 16;
        machine.mem.data[offset..offset + 10].copy_from_slice(&F80::from_f64(2.5).0);
        machine.mem.data[0x2004] = 0xE1;
        for register in machine.cpu.fpu.regs.iter_mut() {
            register.0 = [0; 10];
        }
        machine.cpu.fpu.tag_word = 0xFFFF;
        machine.execute(&[0x0F, 0xAE, 0x09]).expect("fxrstor");

        assert_eq!(machine.cpu.fpu.get_top(), 5);
        assert_eq!(
            machine.cpu.fpu.get_st(0),
            F80::from_f64(std::f64::consts::PI)
        );
        assert_eq!(machine.cpu.fpu.get_st(1), F80::from_f64(0.0));
        assert_eq!(machine.cpu.fpu.get_st(2), F80::from_f64(1.0));
        assert_eq!(
            machine.cpu.fpu.get_st(3),
            F80::from_f64(2.5),
            "slot 3 is ST(3) = R0"
        );
        assert_eq!(machine.cpu.fpu.regs[0], F80::from_f64(2.5));
        // R0 valid (00), R1-R4 empty (11), R5 valid (00), R6 zero (01), R7 valid (00).
        assert_eq!(machine.cpu.fpu.tag_word, 0x13FC);
    }

    /// MDD-BUG-KIL-00762: FXSAVE built its image straight from `cpu.fpu.regs`.
    ///
    /// A deferred binary64 update can leave the F80 mirrors stale. FXSAVE
    /// must materialize them first so a later FXRSTOR restores the current
    /// numeric state rather than the pre-operation bytes.
    #[test]
    fn fxsave_materializes_a_reverse_stale_drc_slot_before_saving() {
        use crate::x87_ops::F80;

        let mut machine = Machine::new();
        machine.cpu.fpu.push(F80::from_f64(9.0));
        let physical = machine.cpu.fpu.get_st_idx(0);

        // The F80 mirror still reads 9.0 while the live binary64 value is
        // 1.25, pending materialization.
        machine.cpu.fpu.seed_drc_reverse_stale_f64(physical, 1.25);
        assert_eq!(
            machine.cpu.fpu.regs[physical],
            F80::from_f64(9.0),
            "the fixture must start with a STALE mirror, or it proves nothing"
        );

        machine.execute(&[0x0F, 0xAE, 0x01]).expect("fxsave");

        // The pushed value is ST(0), and the image's register area is
        // ST-relative (MDD-BUG-TEM-00163), so it is saved in slot 0.
        let offset = 0x2000 + 32;
        let saved = &machine.mem.data[offset..offset + 10];
        assert_eq!(
            saved,
            &F80::from_f64(1.25).0[..],
            "FXSAVE must save the committed value, not the pre-operation mirror"
        );
        assert_ne!(
            saved,
            &F80::from_f64(9.0).0[..],
            "saving 9.0 is the defect: the stale mirror reached the image"
        );
    }

    #[test]
    fn fxrstor_invalidates_old_drc_numeric_shadow_proofs() {
        use crate::x87_ops::F80;

        let mut machine = Machine::new();
        machine.cpu.fpu.push(F80::from_f64(1.0));
        machine.cpu.fpu.enter_drc_binary64_mode();
        assert_eq!(machine.cpu.fpu.exact_binary64_st(0), Some(1.0));

        machine.execute(&[0x0F, 0xAE, 0x01]).expect("fxsave");
        // Slot 0 of the ST-relative register area is ST(0).
        let offset = 0x2000 + 32;
        machine.mem.data[offset..offset + 10].copy_from_slice(&F80::from_f64(3.0).0);

        machine.execute(&[0x0F, 0xAE, 0x09]).expect("fxrstor");

        assert_eq!(machine.cpu.fpu.exact_binary64_st(0), None);
        machine.cpu.fpu.commit_drc_binary64_register_file();
        assert_eq!(machine.cpu.fpu.get_st(0), F80::from_f64(3.0));
    }

    // ------------------------------------------------------------------
    // Host-vs-soft differential (HLD 3.6.3) — the permanent oracle for the
    // promoted host float kernels.
    //
    // The software model is authoritative (D9). Wherever a kernel accepts an
    // input, its value, its six status bits, and the fault it leads to must be
    // the model's. A kernel that declines is not tested here — the executor
    // runs the model for it, so there is nothing to compare.
    // ------------------------------------------------------------------
    #[cfg(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", target_endian = "little")
    ))]
    mod host_soft {
        use super::*;
        use crate::sse_host_kernels;

        #[derive(Clone, Copy)]
        enum Kind {
            Binary(FloatOperation),
            SquareRoot,
            MinMax(bool),
            /// CVT(T)PS2DQ, CVT(T)SS2SI, CVT(T)SD2SI: the flag is truncation.
            /// The source is the second operand; the first is the destination
            /// whose surviving lanes, where there are any, must be carried.
            ToInt32(bool),
            /// CVTSI2SS / CVTSI2SD; the integer is the second operand.
            FromInt32,
        }

        #[derive(Clone, Copy)]
        struct Form {
            name: &'static str,
            double: bool,
            lanes: usize,
            kind: Kind,
        }

        const FORMS: &[Form] = &[
            Form {
                name: "addps",
                double: false,
                lanes: 4,
                kind: Kind::Binary(FloatOperation::Add),
            },
            Form {
                name: "addss",
                double: false,
                lanes: 1,
                kind: Kind::Binary(FloatOperation::Add),
            },
            Form {
                name: "addpd",
                double: true,
                lanes: 2,
                kind: Kind::Binary(FloatOperation::Add),
            },
            Form {
                name: "addsd",
                double: true,
                lanes: 1,
                kind: Kind::Binary(FloatOperation::Add),
            },
            Form {
                name: "subps",
                double: false,
                lanes: 4,
                kind: Kind::Binary(FloatOperation::Subtract),
            },
            Form {
                name: "subss",
                double: false,
                lanes: 1,
                kind: Kind::Binary(FloatOperation::Subtract),
            },
            Form {
                name: "subpd",
                double: true,
                lanes: 2,
                kind: Kind::Binary(FloatOperation::Subtract),
            },
            Form {
                name: "subsd",
                double: true,
                lanes: 1,
                kind: Kind::Binary(FloatOperation::Subtract),
            },
            Form {
                name: "mulps",
                double: false,
                lanes: 4,
                kind: Kind::Binary(FloatOperation::Multiply),
            },
            Form {
                name: "mulss",
                double: false,
                lanes: 1,
                kind: Kind::Binary(FloatOperation::Multiply),
            },
            Form {
                name: "mulpd",
                double: true,
                lanes: 2,
                kind: Kind::Binary(FloatOperation::Multiply),
            },
            Form {
                name: "mulsd",
                double: true,
                lanes: 1,
                kind: Kind::Binary(FloatOperation::Multiply),
            },
            Form {
                name: "divps",
                double: false,
                lanes: 4,
                kind: Kind::Binary(FloatOperation::Divide),
            },
            Form {
                name: "divss",
                double: false,
                lanes: 1,
                kind: Kind::Binary(FloatOperation::Divide),
            },
            Form {
                name: "divpd",
                double: true,
                lanes: 2,
                kind: Kind::Binary(FloatOperation::Divide),
            },
            Form {
                name: "divsd",
                double: true,
                lanes: 1,
                kind: Kind::Binary(FloatOperation::Divide),
            },
            Form {
                name: "sqrtps",
                double: false,
                lanes: 4,
                kind: Kind::SquareRoot,
            },
            Form {
                name: "sqrtss",
                double: false,
                lanes: 1,
                kind: Kind::SquareRoot,
            },
            Form {
                name: "sqrtpd",
                double: true,
                lanes: 2,
                kind: Kind::SquareRoot,
            },
            Form {
                name: "sqrtsd",
                double: true,
                lanes: 1,
                kind: Kind::SquareRoot,
            },
            Form {
                name: "minps",
                double: false,
                lanes: 4,
                kind: Kind::MinMax(false),
            },
            Form {
                name: "minss",
                double: false,
                lanes: 1,
                kind: Kind::MinMax(false),
            },
            Form {
                name: "minpd",
                double: true,
                lanes: 2,
                kind: Kind::MinMax(false),
            },
            Form {
                name: "minsd",
                double: true,
                lanes: 1,
                kind: Kind::MinMax(false),
            },
            Form {
                name: "maxps",
                double: false,
                lanes: 4,
                kind: Kind::MinMax(true),
            },
            Form {
                name: "maxss",
                double: false,
                lanes: 1,
                kind: Kind::MinMax(true),
            },
            Form {
                name: "maxpd",
                double: true,
                lanes: 2,
                kind: Kind::MinMax(true),
            },
            Form {
                name: "maxsd",
                double: true,
                lanes: 1,
                kind: Kind::MinMax(true),
            },
            Form {
                name: "cvtps2dq",
                double: false,
                lanes: 4,
                kind: Kind::ToInt32(false),
            },
            Form {
                name: "cvttps2dq",
                double: false,
                lanes: 4,
                kind: Kind::ToInt32(true),
            },
            Form {
                name: "cvtss2si",
                double: false,
                lanes: 1,
                kind: Kind::ToInt32(false),
            },
            Form {
                name: "cvttss2si",
                double: false,
                lanes: 1,
                kind: Kind::ToInt32(true),
            },
            Form {
                name: "cvtsd2si",
                double: true,
                lanes: 1,
                kind: Kind::ToInt32(false),
            },
            Form {
                name: "cvttsd2si",
                double: true,
                lanes: 1,
                kind: Kind::ToInt32(true),
            },
            Form {
                name: "cvtsi2ss",
                double: false,
                lanes: 1,
                kind: Kind::FromInt32,
            },
            Form {
                name: "cvtsi2sd",
                double: true,
                lanes: 1,
                kind: Kind::FromInt32,
            },
        ];

        /// Integer seeds for CVTSI2SS/SD: the boundaries where a 32-bit integer
        /// stops being exactly representable in each format.
        const INTEGER_SEEDS: &[i32] = &[
            0,
            1,
            -1,
            2,
            -2,
            0x00FF_FFFF,
            0x0100_0000, // 2^24, the last exact single
            0x0100_0001,
            0x0100_0003,
            0x7FFF_FFFF,
            -0x8000_0000,
            -0x0100_0001,
            0x0080_0000,
            0x1234_5678,
            -0x1234_5679,
        ];

        /// Single-precision seeds: the executor's own special list plus the
        /// magnitudes that put a product or quotient on an overflow or
        /// underflow boundary, which the general list does not reach.
        fn seeds32() -> Vec<u32> {
            let mut values = SPECIAL32.to_vec();
            values.extend_from_slice(&[
                0x0000_0002, // two ulp of the smallest denormal
                0x0000_0400, // mid-range denormal
                0x0080_0001, // just above the smallest normal
                0x0C00_0000, // 2^-102: squaring underflows, halving is exact
                0x0BFF_FFFF, // the inexact neighbour of the same
                0x7E80_0000, // 2^126: doubling overflows
                0x4180_0000, // 16.0, an exact square
                0x3E80_0000, // 0.25, an exact square
                0x8080_0000, // negative smallest normal
                // Quiet NaNs of several payloads and both signs. The binary
                // family is promoted with NaN operands unguarded, so which NaN
                // survives an operation — and with which payload — has to be
                // pinned rather than assumed.
                0x7FC0_1234,
                0xFFD5_5555,
                0x7FFF_FFFF,
                0xFFC0_0001,
                0x7FBF_FFFF, // signalling, largest payload
            ]);
            values
        }

        /// Double-precision counterparts.
        fn seeds64() -> Vec<u64> {
            let mut values = SPECIAL64.to_vec();
            values.extend_from_slice(&[
                0x0000_0000_0000_0002,
                0x0000_0000_0000_0400,
                0x0010_0000_0000_0001,
                0x0180_0000_0000_0000, // 2^-999-ish: squaring underflows
                0x017F_FFFF_FFFF_FFFF,
                0x7FD0_0000_0000_0000, // doubling overflows
                0x4030_0000_0000_0000, // 16.0
                0x3FD0_0000_0000_0000, // 0.25
                0x8010_0000_0000_0000,
                0x7FF8_0000_1234_5678,
                0xFFFA_AAAA_AAAA_AAAA,
                0xFFF8_0000_0000_0001,
                0x7FF7_FFFF_FFFF_FFFF, // signalling, largest payload
            ]);
            values
        }

        /// Control-word states: every rounding mode crossed with DAZ and FTZ.
        ///
        /// Only these four fields can change a value or a status bit; the
        /// exception masks change the *fault*, and are crossed separately in
        /// `MASK_PATTERNS` so the expensive dimension stays small.
        fn control_states() -> Vec<u32> {
            let mut states = Vec::new();
            for rounding in 0..4u32 {
                for daz in [0u32, 1 << 6] {
                    for ftz in [0u32, 1 << 15] {
                        states.push((rounding << 13) | daz | ftz);
                    }
                }
            }
            states
        }

        /// Exception-mask patterns, including all-unmasked so the `#XM` / `#UD`
        /// decision is exercised, and singles so one unmasked bit at a time is
        /// the only thing that can fault.
        const MASK_PATTERNS: &[u32] = &[0x3F << 7, 0, 0x3E << 7, 0x2F << 7, 0x1F << 7, 0x15 << 7];

        fn packed_input(rng: &mut Rng, double: bool, seeds32: &[u32], seeds64: &[u64]) -> u128 {
            let mut value = 0u128;
            if double {
                for lane in 0..2 {
                    let bits = if rng.below(4) == 0 {
                        rng.next_u64()
                    } else {
                        seeds64[rng.below(seeds64.len())]
                    };
                    value |= u128::from(bits) << (lane * 64);
                }
            } else {
                for lane in 0..4 {
                    let bits = if rng.below(4) == 0 {
                        rng.next_u32()
                    } else {
                        seeds32[rng.below(seeds32.len())]
                    };
                    value |= u128::from(bits) << (lane * 32);
                }
            }
            value
        }

        fn host(form: Form, left: u128, right: u128, mxcsr: u32) -> Option<(u128, u32)> {
            match form.kind {
                Kind::Binary(operation) => sse_host_kernels::float_binary(
                    left,
                    right,
                    form.lanes,
                    form.double,
                    operation,
                    mxcsr,
                ),
                Kind::SquareRoot => {
                    sse_host_kernels::square_root(left, right, form.lanes, form.double, mxcsr)
                }
                Kind::MinMax(maximum) => {
                    sse_host_kernels::min_max(left, right, form.lanes, form.double, maximum, mxcsr)
                }
                Kind::ToInt32(truncating) => match (form.lanes, form.double) {
                    (4, _) => sse_host_kernels::packed_to_int32(right, truncating, mxcsr),
                    (_, false) => {
                        sse_host_kernels::single_to_int32(right as u32, truncating, mxcsr)
                            .map(|(value, status)| (u128::from(value as u32), status))
                    }
                    (_, true) => sse_host_kernels::double_to_int32(right as u64, truncating, mxcsr)
                        .map(|(value, status)| (u128::from(value as u32), status)),
                },
                Kind::FromInt32 => {
                    sse_host_kernels::int32_to_float(left, right as i32, form.double, mxcsr)
                }
            }
        }

        fn soft(form: Form, left: u128, right: u128, mxcsr: u32) -> (u128, u32) {
            match form.kind {
                Kind::Binary(operation) => {
                    soft_float_binary(left, right, form.lanes, form.double, operation, mxcsr)
                }
                Kind::SquareRoot => soft_square_root(left, right, form.lanes, form.double, mxcsr),
                Kind::MinMax(maximum) => {
                    soft_min_max(left, right, form.lanes, form.double, maximum, mxcsr)
                }
                Kind::ToInt32(truncating) => match (form.lanes, form.double) {
                    (4, _) => soft_packed_to_int32(right, truncating, mxcsr),
                    (_, false) => {
                        let (value, flags) = soft_single_to_int32(right as u32, truncating, mxcsr);
                        (u128::from(value as u32), flags)
                    }
                    (_, true) => {
                        let (value, flags) = soft_double_to_int32(right as u64, truncating, mxcsr);
                        (u128::from(value as u32), flags)
                    }
                },
                Kind::FromInt32 => soft_int32_to_float(left, right as i32, form.double, mxcsr),
            }
        }

        /// What `publish_fp_status_raw` does with one status word: the fault it
        /// raises, if any, and the MXCSR it leaves behind.
        fn fault_of(cpu: &mut Cpu, mxcsr: u32, cr4: u32, flags: u32) -> (String, u32) {
            cpu.sse.mxcsr_mask = 0xFFFF;
            cpu.sse.mxcsr = mxcsr;
            cpu.regs.cr4 = cr4;
            let outcome = match publish_fp_status_raw(cpu, flags) {
                Ok(()) => "ok".to_string(),
                Err(error) => format!("{error:?}"),
            };
            (outcome, cpu.sse.mxcsr)
        }

        /// Which forms this host's backend is expected to promote. The
        /// x86-64 backend answers for everything; the aarch64 backend's set
        /// is the measured-winner set (its module doc carries the numbers).
        /// Both directions are enforced: an expected form that never
        /// promotes fails (silent kernel), and an unexpected form that
        /// promotes fails too (someone widened the backend without updating
        /// the expectation — and the measurement duty that goes with it).
        #[cfg(target_arch = "x86_64")]
        fn expected_to_promote(form: Form) -> bool {
            let _ = form;
            true
        }

        #[cfg(all(target_arch = "aarch64", target_endian = "little"))]
        fn expected_to_promote(form: Form) -> bool {
            match form.kind {
                Kind::Binary(operation) => {
                    form.lanes > 1 || matches!(operation, FloatOperation::Divide)
                }
                Kind::SquareRoot => true,
                Kind::MinMax(_) => form.lanes > 1,
                Kind::ToInt32(_) | Kind::FromInt32 => false,
            }
        }

        fn expected_exact_ftz_add_to_promote(form: Form) -> bool {
            #[cfg(target_arch = "x86_64")]
            {
                let _ = form;
                // MDD-BUG-CRUCIBLE-00787: host UE is not yet proven for the
                // exact tiny-denormal result, so the x86 kernel must decline.
                false
            }
            #[cfg(all(target_arch = "aarch64", target_endian = "little"))]
            {
                expected_to_promote(form)
            }
        }

        /// Divergences grouped by kind, because the useful failure output is
        /// "which classes of input escaped the guards", not the first two dozen
        /// rows of a sweep that may have thousands.
        struct Report {
            classes: std::collections::BTreeMap<String, (u64, String)>,
            total: u64,
            promoted: u64,
            /// `(promoted, total)` per form. A form that never promotes is
            /// compared against nothing, so its green is worthless — the test
            /// says so rather than passing — and the printed ratios are the
            /// journal's promotion-coverage evidence (a one-in-thousands
            /// ratio passes the zero floor but should be visible, not
            /// silent).
            per_form: std::collections::BTreeMap<&'static str, (u64, u64)>,
        }

        impl Report {
            fn new() -> Self {
                Self {
                    classes: std::collections::BTreeMap::new(),
                    total: 0,
                    promoted: 0,
                    per_form: std::collections::BTreeMap::new(),
                }
            }

            fn record(&mut self, key: String, detail: impl FnOnce() -> String) {
                let entry = self.classes.entry(key).or_insert_with(|| (0, detail()));
                entry.0 += 1;
            }

            fn finish(self, label: &str) {
                let promoted_of = |name: &str| self.per_form.get(name).map_or(0, |c| c.0);
                let silent: Vec<&str> = FORMS
                    .iter()
                    .filter(|form| expected_to_promote(**form))
                    .map(|form| form.name)
                    .filter(|name| promoted_of(name) == 0)
                    .collect();
                assert!(
                    silent.is_empty(),
                    "{label}: these kernels accepted no input at all, so their green \
                     proves nothing — widen the seeds or check the guard: {silent:?}"
                );
                let unexpected: Vec<&str> = FORMS
                    .iter()
                    .filter(|form| !expected_to_promote(**form))
                    .map(|form| form.name)
                    .filter(|name| promoted_of(name) > 0)
                    .collect();
                assert!(
                    unexpected.is_empty(),
                    "{label}: these forms promoted but the backend declares them \
                     soft-only — update expected_to_promote (and re-measure): \
                     {unexpected:?}"
                );
                if !self.classes.is_empty() {
                    let mut text = String::new();
                    for (key, (count, example)) in &self.classes {
                        text.push_str(&format!("\n[{count} cases] {key}\n{example}"));
                    }
                    panic!(
                        "{label}: {} divergence classes over {} cases ({} promoted).\
                         A promoted kernel must match the software model exactly; \
                         guard the class or leave the op soft (HLD D9) — never relax \
                         this test.{text}",
                        self.classes.len(),
                        self.total,
                        self.promoted,
                    );
                }
                println!(
                    "{label}: {} promoted of {} cases",
                    self.promoted, self.total
                );
                for (name, (promoted, total)) in &self.per_form {
                    println!("{label}:   {name}: {promoted}/{total}");
                }
            }
        }

        fn compare(
            report: &mut Report,
            cpu: &mut Cpu,
            form: Form,
            left: u128,
            right: u128,
            mxcsr: u32,
        ) {
            report.total += 1;
            report.per_form.entry(form.name).or_default().1 += 1;
            let Some((host_value, host_flags)) = host(form, left, right, mxcsr) else {
                return;
            };
            report.promoted += 1;
            report.per_form.entry(form.name).or_default().0 += 1;
            let (soft_value, soft_flags) = soft(form, left, right, mxcsr);
            if host_value != soft_value || host_flags != soft_flags {
                let key = format!(
                    "{} value={} host-only-status={:#04x} soft-only-status={:#04x}",
                    form.name,
                    if host_value == soft_value {
                        "equal"
                    } else {
                        "DIFFERENT"
                    },
                    host_flags & !soft_flags,
                    soft_flags & !host_flags,
                );
                report.record(key, || {
                    format!(
                        "  mxcsr={mxcsr:#06x} a={left:#034x} b={right:#034x}\n\
                         host value={host_value:#034x} status={host_flags:#04x}\n\
                         soft value={soft_value:#034x} status={soft_flags:#04x}"
                    )
                });
                return;
            }
            // Equal status bits must also mean an equal architectural outcome.
            // Driving both through the shared publisher proves the promoted
            // path cannot fault differently from the model it replaced.
            for &masks in MASK_PATTERNS {
                for cr4 in [0, crate::CR4_OSXMMEXCPT] {
                    let word = (mxcsr & !0x1F80) | masks;
                    let host_outcome = fault_of(cpu, word, cr4, host_flags);
                    let soft_outcome = fault_of(cpu, word, cr4, soft_flags);
                    if host_outcome != soft_outcome {
                        report.record(format!("{} fault decision", form.name), || {
                            format!(
                                "  mxcsr={word:#06x} cr4={cr4:#x} \
                                 host={host_outcome:?} soft={soft_outcome:?}"
                            )
                        });
                    }
                }
            }
        }

        fn run(label: &str, scalar_randoms: usize, packed_pairs: usize, seed: u64) {
            let seeds32 = seeds32();
            let seeds64 = seeds64();
            let states = control_states();
            let mut cpu = Cpu::new();
            let mut report = Report::new();

            for &form in FORMS {
                let mut rng =
                    Rng::new(seed ^ (form.name.len() as u64) ^ u64::from(form.lanes as u32));
                // Scalar forms take the whole special cross: at one lane the
                // domain is small enough to enumerate, which is what makes the
                // NaN, infinity and denormal corners pinned rather than sampled.
                let pairs: Vec<(u128, u128)> = if matches!(form.kind, Kind::FromInt32) {
                    // The integer is the source; the destination only has to
                    // vary enough to prove its upper lanes are carried.
                    let mut pairs = Vec::new();
                    for &integer in INTEGER_SEEDS {
                        for destination in [0u128, u128::MAX, 0x1234_5678_9abc_def0] {
                            pairs.push((destination, u128::from(integer as u32)));
                        }
                    }
                    for _ in 0..scalar_randoms {
                        pairs.push((
                            packed_input(&mut rng, false, &seeds32, &seeds64),
                            u128::from(rng.next_u32()),
                        ));
                    }
                    pairs
                } else if form.lanes == 1 {
                    let mut pairs = Vec::new();
                    if form.double {
                        for &a in &seeds64 {
                            for &b in &seeds64 {
                                pairs.push((u128::from(a), u128::from(b)));
                            }
                        }
                    } else {
                        for &a in &seeds32 {
                            for &b in &seeds32 {
                                pairs.push((u128::from(a), u128::from(b)));
                            }
                        }
                    }
                    for _ in 0..scalar_randoms {
                        pairs.push((
                            packed_input(&mut rng, form.double, &seeds32, &seeds64),
                            packed_input(&mut rng, form.double, &seeds32, &seeds64),
                        ));
                    }
                    pairs
                } else {
                    (0..packed_pairs)
                        .map(|_| {
                            (
                                packed_input(&mut rng, form.double, &seeds32, &seeds64),
                                packed_input(&mut rng, form.double, &seeds32, &seeds64),
                            )
                        })
                        .collect()
                };

                for &(left, right) in &pairs {
                    for &control in &states {
                        let mxcsr = control | (0x3F << 7);
                        compare(&mut report, &mut cpu, form, left, right, mxcsr);
                    }
                }
            }

            report.finish(label);
        }

        fn packed32(values: [f32; 4]) -> u128 {
            values.iter().enumerate().fold(0u128, |acc, (lane, value)| {
                acc | (u128::from(value.to_bits()) << (lane * 32))
            })
        }

        fn packed64(values: [f64; 2]) -> u128 {
            values.iter().enumerate().fold(0u128, |acc, (lane, value)| {
                acc | (u128::from(value.to_bits()) << (lane * 64))
            })
        }

        /// Ordinary finite operands every backend must be able to answer for:
        /// round-nearest, no NaN, no denormal, nothing near a range boundary.
        fn ordinary_pair(form: Form) -> (u128, u128) {
            match form.kind {
                Kind::Binary(_) | Kind::MinMax(_) => {
                    if form.double {
                        (packed64([1.5, -3.25]), packed64([2.5, 4.0]))
                    } else {
                        (
                            packed32([1.5, -3.25, 100.0, 0.375]),
                            packed32([2.5, 4.0, -7.5, 3.0]),
                        )
                    }
                }
                Kind::SquareRoot => {
                    if form.double {
                        (packed64([9.0, 8.0]), packed64([2.25, 16.0]))
                    } else {
                        (
                            packed32([9.0, 8.0, 7.0, 6.0]),
                            packed32([2.25, 16.0, 1.0, 9.0]),
                        )
                    }
                }
                Kind::ToInt32(_) => match (form.lanes, form.double) {
                    (4, _) => (0, packed32([1.5, -2.5, 3.0, 100.0])),
                    (_, false) => (0, u128::from(1.5f32.to_bits())),
                    (_, true) => (0, u128::from(1.5f64.to_bits())),
                },
                Kind::FromInt32 => (packed32([9.0, 8.0, 7.0, 6.0]), 3),
            }
        }

        /// A guard tightened into uselessness cannot hide behind the random
        /// corpus: every form must promote — and match the model on — a
        /// deterministic ordinary input (HLD 2026.08.20 §3.5.1).
        #[test]
        fn every_form_promotes_the_ordinary_case() {
            let mut cpu = Cpu::new();
            let mut report = Report::new();
            for &form in FORMS {
                let (left, right) = ordinary_pair(form);
                compare(&mut report, &mut cpu, form, left, right, 0x1F80);
            }
            for &form in FORMS {
                let expected = u64::from(expected_to_promote(form));
                assert_eq!(
                    report.per_form.get(form.name).map_or(0, |counts| counts.0),
                    expected,
                    "{} {} its ordinary case",
                    form.name,
                    if expected == 1 {
                        "declined"
                    } else {
                        "promoted (declared soft-only)"
                    }
                );
            }
            report.finish("must-promote");
        }

        /// Deterministic corners the random corpus cannot be trusted to hit:
        /// the same-lane signed-zero pair (the min/max take-second rule), and
        /// an exact-denormal sum under guest FTZ (the flush seam both
        /// backends must agree with the model on — promoted or declined,
        /// never divergent).
        #[test]
        fn deterministic_corner_cases_never_diverge() {
            let mut cpu = Cpu::new();
            let mut report = Report::new();
            // Vacuity guard: `compare` records nothing for a declined input,
            // so without this accounting the test would stay green if every
            // corner silently stopped promoting.
            let mut expected_promotions = 0u64;
            let plus32 = packed32([0.0; 4]);
            let minus32 = packed32([-0.0; 4]);
            let plus64 = packed64([0.0; 2]);
            let minus64 = packed64([-0.0; 2]);
            // 1.5 * min_normal + (-min_normal) = an exact denormal.
            let tiny32_a = packed32([f32::from_bits(0x00C0_0000); 4]);
            let tiny32_b = packed32([f32::from_bits(0x8080_0000); 4]);
            let tiny64_a = packed64([f64::from_bits(0x0018_0000_0000_0000); 2]);
            let tiny64_b = packed64([f64::from_bits(0x8010_0000_0000_0000); 2]);
            for &form in FORMS {
                match form.kind {
                    Kind::MinMax(_) => {
                        let (plus, minus) = if form.double {
                            (plus64, minus64)
                        } else {
                            (plus32, minus32)
                        };
                        if expected_to_promote(form) {
                            expected_promotions += 2;
                        }
                        compare(&mut report, &mut cpu, form, plus, minus, 0x1F80);
                        compare(&mut report, &mut cpu, form, minus, plus, 0x1F80);
                    }
                    Kind::Binary(FloatOperation::Add) => {
                        let (a, b) = if form.double {
                            (tiny64_a, tiny64_b)
                        } else {
                            (tiny32_a, tiny32_b)
                        };
                        if expected_exact_ftz_add_to_promote(form) {
                            expected_promotions += 1;
                        }
                        // Guest FTZ on; DAZ off; round-nearest, all masked.
                        compare(&mut report, &mut cpu, form, a, b, 0x1F80 | 0x8000);
                    }
                    _ => {}
                }
            }
            assert!(
                report.classes.is_empty(),
                "corner cases diverged: {:?}",
                report.classes.keys().collect::<Vec<_>>()
            );
            assert_eq!(
                report.promoted, expected_promotions,
                "a corner case this backend should answer for silently declined"
            );
        }

        /// The production-engagement witness (HLD 2026.08.20 §3.5.2): one
        /// promotable instruction per float family, decoded from raw bytes
        /// and run through the production `execute` entry, must advance this
        /// thread's promotion counter. The differentials compare values, so
        /// a call-site cfg reverted to x86-only would silently run the
        /// (equally correct) soft path and stay green there — this is the
        /// test that fails instead.
        #[test]
        fn the_production_executor_reaches_the_host_backend() {
            use crate::sse_host_common::PROMOTIONS;
            let mut cpu = Cpu::new();
            let mut mem = Ram::new();
            // One promotable instruction per family the host backend
            // declares (expected_to_promote): the x86-64 backend answers for
            // all six families; the aarch64 one for four (conversions are
            // measured-decline there, and its scalar-binary witness is the
            // promoted divide).
            #[cfg(target_arch = "x86_64")]
            let cases: &[(&str, &[u8])] = &[
                ("addps", &[0x0F, 0x58, 0xC1]),
                ("sqrtps", &[0x0F, 0x51, 0xC1]),
                ("minps", &[0x0F, 0x5D, 0xC1]),
                ("cvtps2dq", &[0x66, 0x0F, 0x5B, 0xC1]),
                ("cvtss2si", &[0xF3, 0x0F, 0x2D, 0xC1]),
                ("cvtsi2ss", &[0xF3, 0x0F, 0x2A, 0xC1]),
            ];
            #[cfg(all(target_arch = "aarch64", target_endian = "little"))]
            let cases: &[(&str, &[u8])] = &[
                ("addps", &[0x0F, 0x58, 0xC1]),
                ("divss", &[0xF3, 0x0F, 0x5E, 0xC1]),
                ("sqrtps", &[0x0F, 0x51, 0xC1]),
                ("minps", &[0x0F, 0x5D, 0xC1]),
            ];
            for &(name, bytes) in cases {
                cpu.sse.mxcsr = 0x1F80;
                cpu.sse.mxcsr_mask = 0xFFFF;
                cpu.sse.xmm[0] = packed32([1.5, 2.5, 3.5, 4.5]);
                cpu.sse.xmm[1] = packed32([2.0, 4.0, 8.0, 16.0]);
                cpu.regs.ecx = 3;
                let instr = decoded_bytes(bytes);
                let before = PROMOTIONS.with(|count| count.get());
                execute(&mut cpu, &instr, &mut mem).expect(name);
                let after = PROMOTIONS.with(|count| count.get());
                assert!(
                    after > before,
                    "{name} executed without reaching the host backend"
                );
            }
        }

        #[test]
        fn promoted_host_kernels_match_the_software_model() {
            run("host-vs-soft", 64, 192, 0x5EED_0001);
        }

        #[test]
        #[ignore = "extended host-vs-soft differential; full_test (--include-ignored)"]
        fn promoted_host_kernels_match_the_software_model_extended() {
            run("host-vs-soft (extended)", 2048, 4096, 0x5EED_0002);
        }
    }
}
