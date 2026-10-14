//! Host SSE float kernels: guest arithmetic run by the host FPU itself.
//!
//! Each kernel installs the guest's MXCSR *control* bits with every exception
//! masked, executes one SSE instruction, captures the six status bits that
//! instruction raised, and restores the incoming host word. The whole sequence
//! lives in a single `asm!` block, so LLVM cannot hoist one of its own
//! floating-point operations into the window where the environment is the
//! guest's.
//!
//! Promotion is per-op and evidence-led (HLD D9): the software model in
//! `sse_exec` stays authoritative, and a kernel's answer is adopted only over
//! the input domain the host-vs-soft differential proves bit-identical in both
//! value and status. Each dispatcher below therefore carries the guard that
//! keeps it inside that proven domain and hands everything else back to the
//! soft path.
//!
//! Two instruction encodings are emitted from one template (HLD D10). The
//! Building with `+fma` implies AVX, so compiled code
//! around these kernels is VEX-encoded and a legacy-SSE kernel would pay the
//! AVX/SSE transition penalty; a baseline x86-64 build has no AVX at all and a
//! VEX kernel would be an illegal instruction. `target_feature = "avx"` picks
//! the arm.

use core::arch::asm;

use crate::sse_exec::{from_m128i, to_m128i, FloatOperation};

/// Bits a guest can set in MXCSR. Everything above is reserved, and `LDMXCSR`
/// raises `#GP` if any reserved bit is set.
const GUEST_REACHABLE: u32 = 0x0000_FFFF;
/// The six exception-mask bits, MXCSR 12:7.
const ALL_MASKED: u32 = 0x0000_1F80;
/// Guest control bits a kernel must honour: FTZ (15), RC (14:13), DAZ (6).
const GUEST_CONTROL: u32 = 0x8000 | 0x6000 | 0x0040;

use crate::sse_host_common::{classify, promote, promote_scalar, IE, OE, STATUS_BITS, UE};

/// The control word a kernel installs for one guest operation.
///
/// The guest's rounding, flush-to-zero and denormals-are-zero selections are
/// exactly what the instruction must see. Every exception is masked, because
/// an unmasked host exception here would be a real `#XM` in *our* process
/// rather than a guest fault — the guest's own masking is applied afterwards,
/// by `publish_fp_status_raw`, against the captured status bits. Status starts
/// clear so the word captured after the instruction describes that instruction
/// alone rather than whatever was sticky before it.
///
/// The result cannot fault `LDMXCSR`: the three retained fields all live inside
/// `GUEST_REACHABLE`, so no reserved bit survives whatever the caller passes.
#[inline]
pub(crate) fn prepared_mxcsr(guest: u32) -> u32 {
    ((guest & GUEST_CONTROL) | ALL_MASKED) & GUEST_REACHABLE
}

/// Read the host MXCSR.
///
/// Used by the debug leak tripwire and by the tests. `core::arch::_mm_getcsr`
/// would do the same job but is deprecated, and reading it here keeps the
/// module's only MXCSR access in one place. Not debug-gated: the public
/// `crate::host_mxcsr` (native SSE) needs it in release builds too.
#[inline]
pub(crate) fn read_mxcsr() -> u32 {
    let mut value = 0u32;
    // SAFETY: `STMXCSR` writes four bytes through the pointer, which addresses
    // a live, correctly aligned local. It touches no other state.
    unsafe {
        // x87-inventory-exempt: x86-64 SSE kernel MXCSR bracket — reads and writes only XMM registers and MXCSR, never the x87/MMX register file (iced-free SSE HLD D8), so the native compiled x87 residency tripwire does not apply and arming it here would restore a control word nothing changed.
        asm!(
            "stmxcsr dword ptr [{slot}]",
            slot = in(reg) core::ptr::addr_of_mut!(value),
            options(nostack),
        );
    }
    value
}

/// Generate one kernel from the shared five-instruction template.
///
/// `$vex` and `$sse` are the same operation in the two encodings; `{d}` is the
/// destination (read-modify-write, because every scalar form merges into the
/// upper lanes it does not compute) and `{s}` the source.
macro_rules! float_kernel {
    ($(#[$doc:meta])* $name:ident, $vex:literal, $sse:literal) => {
        $(#[$doc])*
        #[inline]
        fn $name(destination: u128, source: u128, guest_mxcsr: u32) -> (u128, u32) {
            #[cfg(debug_assertions)]
            let entry = read_mxcsr();

            let mut saved = 0u32;
            let prepared = prepared_mxcsr(guest_mxcsr);
            let mut status = 0u32;
            let mut d = to_m128i(destination);
            let s = to_m128i(source);

            // SAFETY: the three pointers address live, correctly aligned `u32`
            // locals, and `STMXCSR`/`LDMXCSR` touch exactly four bytes each.
            // The block saves the incoming MXCSR before changing it and
            // restores that saved word — never a constant — so third-party
            // host control bits survive. Every exception is masked in
            // `prepared`, so the arithmetic cannot raise a host fault. No
            // `preserves_flags`: clobbering the MXCSR status bits is the
            // point of the sequence.
            unsafe {
                #[cfg(target_feature = "avx")]
                // x87-inventory-exempt: x86-64 SSE kernel MXCSR bracket — reads and writes only XMM registers and MXCSR, never the x87/MMX register file (iced-free SSE HLD D8), so the native compiled x87 residency tripwire does not apply and arming it here would restore a control word nothing changed.
                asm!(
                    "stmxcsr dword ptr [{saved}]",
                    "ldmxcsr dword ptr [{prepared}]",
                    $vex,
                    "stmxcsr dword ptr [{status}]",
                    "ldmxcsr dword ptr [{saved}]",
                    saved = in(reg) core::ptr::addr_of_mut!(saved),
                    prepared = in(reg) core::ptr::addr_of!(prepared),
                    status = in(reg) core::ptr::addr_of_mut!(status),
                    d = inout(xmm_reg) d,
                    s = in(xmm_reg) s,
                    options(nostack),
                );
                #[cfg(not(target_feature = "avx"))]
                // x87-inventory-exempt: x86-64 SSE kernel MXCSR bracket — reads and writes only XMM registers and MXCSR, never the x87/MMX register file (iced-free SSE HLD D8), so the native compiled x87 residency tripwire does not apply and arming it here would restore a control word nothing changed.
                asm!(
                    "stmxcsr dword ptr [{saved}]",
                    "ldmxcsr dword ptr [{prepared}]",
                    $sse,
                    "stmxcsr dword ptr [{status}]",
                    "ldmxcsr dword ptr [{saved}]",
                    saved = in(reg) core::ptr::addr_of_mut!(saved),
                    prepared = in(reg) core::ptr::addr_of!(prepared),
                    status = in(reg) core::ptr::addr_of_mut!(status),
                    d = inout(xmm_reg) d,
                    s = in(xmm_reg) s,
                    options(nostack),
                );
            }

            // The bracket is only sound if it is airtight, and the native compiled
            // FNSAVE verify-bracket cannot see MXCSR (HLD §7). Debug builds
            // therefore prove it per call.
            #[cfg(debug_assertions)]
            debug_assert_eq!(
                read_mxcsr(),
                entry,
                concat!("host SSE kernel ", stringify!($name), " leaked MXCSR"),
            );

            (from_m128i(d), status & STATUS_BITS)
        }
    };
}

float_kernel!(addps, "vaddps {d}, {d}, {s}", "addps {d}, {s}");
float_kernel!(addpd, "vaddpd {d}, {d}, {s}", "addpd {d}, {s}");
float_kernel!(addss, "vaddss {d}, {d}, {s}", "addss {d}, {s}");
float_kernel!(addsd, "vaddsd {d}, {d}, {s}", "addsd {d}, {s}");

float_kernel!(subps, "vsubps {d}, {d}, {s}", "subps {d}, {s}");
float_kernel!(subpd, "vsubpd {d}, {d}, {s}", "subpd {d}, {s}");
float_kernel!(subss, "vsubss {d}, {d}, {s}", "subss {d}, {s}");
float_kernel!(subsd, "vsubsd {d}, {d}, {s}", "subsd {d}, {s}");

float_kernel!(mulps, "vmulps {d}, {d}, {s}", "mulps {d}, {s}");
float_kernel!(mulpd, "vmulpd {d}, {d}, {s}", "mulpd {d}, {s}");
float_kernel!(mulss, "vmulss {d}, {d}, {s}", "mulss {d}, {s}");
float_kernel!(mulsd, "vmulsd {d}, {d}, {s}", "mulsd {d}, {s}");

float_kernel!(divps, "vdivps {d}, {d}, {s}", "divps {d}, {s}");
float_kernel!(divpd, "vdivpd {d}, {d}, {s}", "divpd {d}, {s}");
float_kernel!(divss, "vdivss {d}, {d}, {s}", "divss {d}, {s}");
float_kernel!(divsd, "vdivsd {d}, {d}, {s}", "divsd {d}, {s}");

// The packed square roots overwrite the destination outright, so their VEX
// form is two-operand; the scalar ones merge, so theirs is three.
float_kernel!(sqrtps, "vsqrtps {d}, {s}", "sqrtps {d}, {s}");
float_kernel!(sqrtpd, "vsqrtpd {d}, {s}", "sqrtpd {d}, {s}");
float_kernel!(sqrtss, "vsqrtss {d}, {d}, {s}", "sqrtss {d}, {s}");
float_kernel!(sqrtsd, "vsqrtsd {d}, {d}, {s}", "sqrtsd {d}, {s}");

float_kernel!(minps, "vminps {d}, {d}, {s}", "minps {d}, {s}");
float_kernel!(minpd, "vminpd {d}, {d}, {s}", "minpd {d}, {s}");
float_kernel!(minss, "vminss {d}, {d}, {s}", "minss {d}, {s}");
float_kernel!(minsd, "vminsd {d}, {d}, {s}", "minsd {d}, {s}");

float_kernel!(maxps, "vmaxps {d}, {d}, {s}", "maxps {d}, {s}");
float_kernel!(maxpd, "vmaxpd {d}, {d}, {s}", "maxpd {d}, {s}");
float_kernel!(maxss, "vmaxss {d}, {d}, {s}", "maxss {d}, {s}");
float_kernel!(maxsd, "vmaxsd {d}, {d}, {s}", "maxsd {d}, {s}");

/// Generate a float-to-integer kernel: XMM source, 32-bit GPR destination.
///
/// Same five-instruction bracket as `float_kernel`; only the operand shape
/// differs, and the destination is written outright rather than merged.
macro_rules! to_integer_kernel {
    ($name:ident, $vex:literal, $sse:literal) => {
        #[inline]
        fn $name(source: u128, guest_mxcsr: u32) -> (i32, u32) {
            #[cfg(debug_assertions)]
            let entry = read_mxcsr();

            let mut saved = 0u32;
            let prepared = prepared_mxcsr(guest_mxcsr);
            let mut status = 0u32;
            let mut out: i32;
            let s = to_m128i(source);

            // SAFETY: as `float_kernel` — live aligned `u32` slots, the saved
            // word restored rather than a constant, every exception masked.
            unsafe {
                #[cfg(target_feature = "avx")]
                // x87-inventory-exempt: x86-64 SSE kernel MXCSR bracket — reads and writes only XMM registers and MXCSR, never the x87/MMX register file (iced-free SSE HLD D8), so the native compiled x87 residency tripwire does not apply and arming it here would restore a control word nothing changed.
                asm!(
                    "stmxcsr dword ptr [{saved}]",
                    "ldmxcsr dword ptr [{prepared}]",
                    $vex,
                    "stmxcsr dword ptr [{status}]",
                    "ldmxcsr dword ptr [{saved}]",
                    saved = in(reg) core::ptr::addr_of_mut!(saved),
                    prepared = in(reg) core::ptr::addr_of!(prepared),
                    status = in(reg) core::ptr::addr_of_mut!(status),
                    out = out(reg) out,
                    s = in(xmm_reg) s,
                    options(nostack),
                );
                #[cfg(not(target_feature = "avx"))]
                // x87-inventory-exempt: x86-64 SSE kernel MXCSR bracket — reads and writes only XMM registers and MXCSR, never the x87/MMX register file (iced-free SSE HLD D8), so the native compiled x87 residency tripwire does not apply and arming it here would restore a control word nothing changed.
                asm!(
                    "stmxcsr dword ptr [{saved}]",
                    "ldmxcsr dword ptr [{prepared}]",
                    $sse,
                    "stmxcsr dword ptr [{status}]",
                    "ldmxcsr dword ptr [{saved}]",
                    saved = in(reg) core::ptr::addr_of_mut!(saved),
                    prepared = in(reg) core::ptr::addr_of!(prepared),
                    status = in(reg) core::ptr::addr_of_mut!(status),
                    out = out(reg) out,
                    s = in(xmm_reg) s,
                    options(nostack),
                );
            }

            #[cfg(debug_assertions)]
            debug_assert_eq!(
                read_mxcsr(),
                entry,
                concat!("host SSE kernel ", stringify!($name), " leaked MXCSR"),
            );

            (out, status & STATUS_BITS)
        }
    };
}

/// Generate an integer-to-float kernel: 32-bit GPR source, XMM destination.
///
/// The destination merges — `CVTSI2SS` writes lane 0 and leaves the rest — so
/// it is read-modify-write here exactly as in the scalar arithmetic kernels.
macro_rules! from_integer_kernel {
    ($name:ident, $vex:literal, $sse:literal) => {
        #[inline]
        fn $name(destination: u128, value: i32, guest_mxcsr: u32) -> (u128, u32) {
            #[cfg(debug_assertions)]
            let entry = read_mxcsr();

            let mut saved = 0u32;
            let prepared = prepared_mxcsr(guest_mxcsr);
            let mut status = 0u32;
            let mut d = to_m128i(destination);

            // SAFETY: as `float_kernel`.
            unsafe {
                #[cfg(target_feature = "avx")]
                // x87-inventory-exempt: x86-64 SSE kernel MXCSR bracket — reads and writes only XMM registers and MXCSR, never the x87/MMX register file (iced-free SSE HLD D8), so the native compiled x87 residency tripwire does not apply and arming it here would restore a control word nothing changed.
                asm!(
                    "stmxcsr dword ptr [{saved}]",
                    "ldmxcsr dword ptr [{prepared}]",
                    $vex,
                    "stmxcsr dword ptr [{status}]",
                    "ldmxcsr dword ptr [{saved}]",
                    saved = in(reg) core::ptr::addr_of_mut!(saved),
                    prepared = in(reg) core::ptr::addr_of!(prepared),
                    status = in(reg) core::ptr::addr_of_mut!(status),
                    d = inout(xmm_reg) d,
                    v = in(reg) value,
                    options(nostack),
                );
                #[cfg(not(target_feature = "avx"))]
                // x87-inventory-exempt: x86-64 SSE kernel MXCSR bracket — reads and writes only XMM registers and MXCSR, never the x87/MMX register file (iced-free SSE HLD D8), so the native compiled x87 residency tripwire does not apply and arming it here would restore a control word nothing changed.
                asm!(
                    "stmxcsr dword ptr [{saved}]",
                    "ldmxcsr dword ptr [{prepared}]",
                    $sse,
                    "stmxcsr dword ptr [{status}]",
                    "ldmxcsr dword ptr [{saved}]",
                    saved = in(reg) core::ptr::addr_of_mut!(saved),
                    prepared = in(reg) core::ptr::addr_of!(prepared),
                    status = in(reg) core::ptr::addr_of_mut!(status),
                    d = inout(xmm_reg) d,
                    v = in(reg) value,
                    options(nostack),
                );
            }

            #[cfg(debug_assertions)]
            debug_assert_eq!(
                read_mxcsr(),
                entry,
                concat!("host SSE kernel ", stringify!($name), " leaked MXCSR"),
            );

            (from_m128i(d), status & STATUS_BITS)
        }
    };
}

float_kernel!(cvtps2dq, "vcvtps2dq {d}, {s}", "cvtps2dq {d}, {s}");
float_kernel!(cvttps2dq, "vcvttps2dq {d}, {s}", "cvttps2dq {d}, {s}");

to_integer_kernel!(cvtss2si, "vcvtss2si {out:e}, {s}", "cvtss2si {out:e}, {s}");
to_integer_kernel!(
    cvttss2si,
    "vcvttss2si {out:e}, {s}",
    "cvttss2si {out:e}, {s}"
);
to_integer_kernel!(cvtsd2si, "vcvtsd2si {out:e}, {s}", "cvtsd2si {out:e}, {s}");
to_integer_kernel!(
    cvttsd2si,
    "vcvttsd2si {out:e}, {s}",
    "cvttsd2si {out:e}, {s}"
);

from_integer_kernel!(cvtsi2ss, "vcvtsi2ss {d}, {d}, {v:e}", "cvtsi2ss {d}, {v:e}");
from_integer_kernel!(cvtsi2sd, "vcvtsi2sd {d}, {d}, {v:e}", "cvtsi2sd {d}, {v:e}");

/// The status bits whose appearance takes an answer outside the proven domain.
///
/// `IE`: the host's invalid-operation answer is the negative QNaN indefinite
/// (`0xFFC0_0000` in single precision) while the software model returns
/// apfloat's positive default NaN, so every invalid result differs in the sign
/// bit alone.
///
/// `OE`: under a directed rounding mode an overflow that rounds back to the
/// largest finite value raises `OE` on hardware and does not in apfloat, which
/// treats a finite result as no overflow. The values agree; only the flag does.
///
/// `UE`: underflow remains outside this backend's proven promotion domain.
/// Arithmetic flushes every tiny result under masked FTZ and records `UE|PE`;
/// the software square-root and conversion paths have separate underflow rules.
///
/// All three are rare enough that recomputing them in software costs nothing
/// measurable, and all three are ledger material rather than something this
/// task adjudicates (HLD D7/D9).
const UNPROVEN: u32 = IE | OE | UE;

/// The conversions keep `IE` *inside* the proven domain. Hardware's answer for
/// an out-of-range or NaN source is the integer indefinite `0x8000_0000` with
/// `IE` and nothing else, and that is exactly what the software model
/// substitutes — measured, not assumed: with `IE` dropped from the guard the
/// differential's only divergence classes are arithmetic ones. Neither
/// overflow nor underflow can arise converting to or from a 32-bit integer, so
/// the remaining two bits are a formality that costs nothing.
const CONVERSION_UNPROVEN: u32 = OE | UE;

/// ADD/SUB/MUL/DIV, packed and scalar, single and double.
///
/// A denormal operand under DAZ-off is refused: hardware suppresses the
/// denormal-operand flag when the operation short-circuits on a NaN operand or
/// a divide by zero, while the software model reports `DE` from the inputs
/// alone and so over-reports it there. NaN operands themselves are *not*
/// refused — the differential drives quiet NaNs of several payloads through
/// every form and both backends propagate the first NaN operand identically,
/// and a signalling one raises `IE`, which the post-check already declines.
pub(crate) fn float_binary(
    left: u128,
    right: u128,
    lanes: usize,
    double: bool,
    operation: FloatOperation,
    mxcsr: u32,
) -> Option<(u128, u32)> {
    use FloatOperation as F;
    if classify(left, lanes, double).denormal_hazard(mxcsr)
        || classify(right, lanes, double).denormal_hazard(mxcsr)
    {
        return None;
    }
    let (value, status) = match (double, lanes, operation) {
        (false, 4, F::Add) => addps(left, right, mxcsr),
        (false, 4, F::Subtract) => subps(left, right, mxcsr),
        (false, 4, F::Multiply) => mulps(left, right, mxcsr),
        (false, 4, F::Divide) => divps(left, right, mxcsr),
        (false, 1, F::Add) => addss(left, right, mxcsr),
        (false, 1, F::Subtract) => subss(left, right, mxcsr),
        (false, 1, F::Multiply) => mulss(left, right, mxcsr),
        (false, 1, F::Divide) => divss(left, right, mxcsr),
        (true, 2, F::Add) => addpd(left, right, mxcsr),
        (true, 2, F::Subtract) => subpd(left, right, mxcsr),
        (true, 2, F::Multiply) => mulpd(left, right, mxcsr),
        (true, 2, F::Divide) => divpd(left, right, mxcsr),
        (true, 1, F::Add) => addsd(left, right, mxcsr),
        (true, 1, F::Subtract) => subsd(left, right, mxcsr),
        (true, 1, F::Multiply) => mulsd(left, right, mxcsr),
        (true, 1, F::Divide) => divsd(left, right, mxcsr),
        _ => return None,
    };
    promote(value, status, UNPROVEN)
}

/// SQRT, packed and scalar, single and double.
///
/// Three input classes are refused. A negative operand, because the host
/// returns the QNaN indefinite where the software model returns whatever
/// `sqrt_accurate` produces. A NaN, because a quiet one propagates without
/// raising anything the post-check could see. And a denormal under DAZ-off,
/// because `sqrt_accurate` gets those wrong outright: it reports `sqrt(2^-148)`
/// as inexact and one ulp high when the exact answer `2^-74` is representable.
pub(crate) fn square_root(
    destination: u128,
    input: u128,
    lanes: usize,
    double: bool,
    mxcsr: u32,
) -> Option<(u128, u32)> {
    let operand = classify(input, lanes, double);
    if operand.nan || operand.negative || operand.denormal_hazard(mxcsr) {
        return None;
    }
    let (value, status) = match (double, lanes) {
        (false, 4) => sqrtps(destination, input, mxcsr),
        (false, _) => sqrtss(destination, input, mxcsr),
        (true, 2) => sqrtpd(destination, input, mxcsr),
        (true, _) => sqrtsd(destination, input, mxcsr),
    };
    promote(value, status, UNPROVEN)
}

/// MIN/MAX, packed and scalar, single and double.
///
/// NaN operands are refused up front. Real hardware raises `IE` for *any* NaN
/// operand of a MIN/MAX, while the software model raises it only for a
/// signalling one (`MDD-BUG-CRUCIBLE-00772`); the selected value agrees either
/// way, but the status word does not, and the post-check cannot tell a
/// legitimate host `IE` from that modelling gap. Everything else — including
/// the `min(+0.0, -0.0) == src2` rule both sides implement — is promoted.
pub(crate) fn min_max(
    left: u128,
    right: u128,
    lanes: usize,
    double: bool,
    maximum: bool,
    mxcsr: u32,
) -> Option<(u128, u32)> {
    let first = classify(left, lanes, double);
    let second = classify(right, lanes, double);
    if first.nan || second.nan || first.denormal_hazard(mxcsr) || second.denormal_hazard(mxcsr) {
        return None;
    }
    let (value, status) = match (double, lanes, maximum) {
        (false, 4, false) => minps(left, right, mxcsr),
        (false, 4, true) => maxps(left, right, mxcsr),
        (false, _, false) => minss(left, right, mxcsr),
        (false, _, true) => maxss(left, right, mxcsr),
        (true, 2, false) => minpd(left, right, mxcsr),
        (true, 2, true) => maxpd(left, right, mxcsr),
        (true, _, false) => minsd(left, right, mxcsr),
        (true, _, true) => maxsd(left, right, mxcsr),
    };
    promote(value, status, UNPROVEN)
}

/// CVTPS2DQ / CVTTPS2DQ — four single-precision lanes to four `i32` lanes.
pub(crate) fn packed_to_int32(input: u128, truncating: bool, mxcsr: u32) -> Option<(u128, u32)> {
    if classify(input, 4, false).denormal_hazard(mxcsr) {
        return None;
    }
    let (value, status) = if truncating {
        cvttps2dq(0, input, mxcsr)
    } else {
        cvtps2dq(0, input, mxcsr)
    };
    promote(value, status, CONVERSION_UNPROVEN)
}

/// CVTSS2SI / CVTTSS2SI.
pub(crate) fn single_to_int32(bits: u32, truncating: bool, mxcsr: u32) -> Option<(i32, u32)> {
    if classify(u128::from(bits), 1, false).denormal_hazard(mxcsr) {
        return None;
    }
    let (value, status) = if truncating {
        cvttss2si(u128::from(bits), mxcsr)
    } else {
        cvtss2si(u128::from(bits), mxcsr)
    };
    promote_scalar(value, status, CONVERSION_UNPROVEN)
}

/// CVTSD2SI / CVTTSD2SI.
pub(crate) fn double_to_int32(bits: u64, truncating: bool, mxcsr: u32) -> Option<(i32, u32)> {
    if classify(u128::from(bits), 1, true).denormal_hazard(mxcsr) {
        return None;
    }
    let (value, status) = if truncating {
        cvttsd2si(u128::from(bits), mxcsr)
    } else {
        cvtsd2si(u128::from(bits), mxcsr)
    };
    promote_scalar(value, status, CONVERSION_UNPROVEN)
}

/// CVTSI2SS / CVTSI2SD. No operand can be a NaN or a denormal, so the
/// post-check is the whole guard.
pub(crate) fn int32_to_float(
    destination: u128,
    value: i32,
    double: bool,
    mxcsr: u32,
) -> Option<(u128, u32)> {
    let (result, status) = if double {
        cvtsi2sd(destination, value, mxcsr)
    } else {
        cvtsi2ss(destination, value, mxcsr)
    };
    promote(result, status, CONVERSION_UNPROVEN)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The raw scalar square-root kernels, BELOW the refusals in
    /// [`square_root`], so a test can use real silicon as an oracle for an
    /// input class that guard sends to the software model instead
    /// (MDD-BUG-CRUCIBLE-00784).
    /// One scalar arithmetic form on real silicon, for tests that need
    /// hardware's own exception flags as the oracle (MDD-BUG-CRUCIBLE-00785).
    pub(crate) fn host_binary_scalar_for_test(
        left: u128,
        right: u128,
        double: bool,
        mxcsr: u32,
        operation: crate::sse_exec::FloatOperation,
    ) -> (u128, u32) {
        use crate::sse_exec::FloatOperation as Op;
        match (double, operation) {
            (false, Op::Add) => addss(left, right, mxcsr),
            (false, Op::Subtract) => subss(left, right, mxcsr),
            (false, Op::Multiply) => mulss(left, right, mxcsr),
            (false, Op::Divide) => divss(left, right, mxcsr),
            (true, Op::Add) => addsd(left, right, mxcsr),
            (true, Op::Subtract) => subsd(left, right, mxcsr),
            (true, Op::Multiply) => mulsd(left, right, mxcsr),
            (true, Op::Divide) => divsd(left, right, mxcsr),
        }
    }

    pub(crate) fn host_sqrt_scalar_for_test(input: u128, double: bool, mxcsr: u32) -> (u128, u32) {
        if double {
            sqrtsd(input, input, mxcsr)
        } else {
            sqrtss(input, input, mxcsr)
        }
    }

    /// The word a kernel installs can never fault `LDMXCSR`, whatever the
    /// guest wrote — including the reserved bits an `LDMXCSR` emulation would
    /// have rejected but a save-state restore might not.
    #[test]
    fn the_prepared_word_is_always_loadable() {
        for guest in [
            0x0000_1F80,
            0x0000_FFFF,
            0xFFFF_FFFF,
            0x0000_8040,
            0x0000_6000,
            0x0000_003F,
        ] {
            let prepared = prepared_mxcsr(guest);
            assert_eq!(
                prepared & !GUEST_REACHABLE,
                0,
                "reserved bit reached LDMXCSR"
            );
            assert_eq!(
                prepared & ALL_MASKED,
                ALL_MASKED,
                "an exception was left unmasked"
            );
            assert_eq!(
                prepared & STATUS_BITS,
                0,
                "a stale status bit was installed"
            );
            assert_eq!(
                prepared & GUEST_CONTROL,
                guest & GUEST_CONTROL,
                "guest rounding/FTZ/DAZ was not carried through"
            );
        }
    }

    /// The tripwire's own premise: a kernel leaves the host MXCSR exactly as it
    /// found it, sticky status included.
    #[test]
    fn a_kernel_restores_the_incoming_host_word() {
        let entry = read_mxcsr();
        // Round-toward-negative, FTZ and DAZ set: the guest word differs from
        // the host's in every field a kernel installs.
        let (_, status) = divps(0x3f80_0000, 0x4000_0000, 0x0000_c040);
        assert_eq!(status & !STATUS_BITS, 0);
        assert_eq!(read_mxcsr(), entry, "kernel leaked MXCSR");
    }

    /// The captured word describes this instruction, not the host's history:
    /// a kernel run after another kernel has raised PE reports no PE of its own.
    #[test]
    fn captured_status_is_not_sticky_across_kernels() {
        // 1.0 / 3.0 is inexact.
        let (_, inexact) = divss(0x3f80_0000, 0x4040_0000, 0x0000_1f80);
        assert_ne!(inexact & (1 << 5), 0, "expected PE from an inexact divide");
        // 1.0 + 1.0 is exact.
        let (value, exact) = addss(0x3f80_0000, 0x3f80_0000, 0x0000_1f80);
        assert_eq!(value, 0x4000_0000);
        assert_eq!(exact, 0, "a previous instruction's status leaked in");
    }

    /// Guest rounding control reaches the instruction.
    #[test]
    fn rounding_control_is_the_guests() {
        // 1.0 / 3.0 rounds down to 0x3EAAAAAA and up to 0x3EAAAAAB.
        let (down, _) = divss(0x3f80_0000, 0x4040_0000, 0x0000_1f80 | 0x2000);
        let (up, _) = divss(0x3f80_0000, 0x4040_0000, 0x0000_1f80 | 0x4000);
        assert_eq!(down, 0x3eaa_aaaa);
        assert_eq!(up, 0x3eaa_aaab);
    }

    /// Scalar forms merge: the upper lanes of the destination survive.
    #[test]
    fn a_scalar_kernel_merges_into_the_destination() {
        let destination = 0xdead_beef_cafe_f00d_1234_5678_3f80_0000u128;
        let (value, _) = addss(destination, 0x3f80_0000, 0x0000_1f80);
        assert_eq!(value >> 32, destination >> 32, "upper lanes were disturbed");
        assert_eq!(value as u32, 0x4000_0000);
    }

    /// Which encoding arm this build compiled, reported so a run of the test
    /// cluster under `RUSTFLAGS=''` is visibly a different arm.
    #[test]
    fn the_compiled_encoding_arm_is_reported() {
        let arm = if cfg!(target_feature = "avx") {
            "VEX"
        } else {
            "legacy-SSE"
        };
        // Both arms must compute; the assertion is the same either way.
        let (value, status) = mulps(0x4000_0000, 0x4040_0000, 0x0000_1f80);
        assert_eq!(value, 0x40c0_0000, "{arm} arm produced the wrong product");
        assert_eq!(status, 0, "{arm} arm raised an unexpected exception");
        println!("host SSE kernels compiled the {arm} arm");
    }
}
