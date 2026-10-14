//! Host SSE float kernels for little-endian aarch64: guest arithmetic run on
//! the host's NEON unit in its **default** floating-point environment.
//!
//! Unlike the x86-64 backend, nothing is ever installed in a control
//! register: FPCR is read, never written. A kernel is only *promoted* on
//! inputs where the default environment (round-nearest, no flush, no
//! alternate-FP behaviour) computes the guest's exact answer — the dispatcher
//! declines to the software model for everything else, including any run
//! where the ambient FPCR is not the default word (HLD 2026.08.20 N2/N10).
//! Rounding-sensitive ops are gated on guest RC == round-nearest.
//!
//! The promoted set is the measured-winner set, not the x86-64 mirror
//! (same-binary timing probe, journal 2026.08.20): packed binary arithmetic,
//! the scalar divides, every sqrt form (the software `sqrt_accurate` costs
//! 500-1030 ns where the kernel costs ~35-47), and packed min/max. Scalar
//! add/sub/mul, scalar min/max, and ALL the conversions decline — the
//! ~30 ns FPSR-bracket floor loses to their 5-29 ns software paths.
//!
//! Each flag-bearing kernel is one `asm!` block — save FPSR, clear it, run
//! the one operation, capture FPSR, restore the saved word — so LLVM cannot
//! schedule its own floating-point work into the measured window, and host
//! sticky status survives exactly as the x86-64 MXCSR discipline demands.
//! The captured raw FPSR word (exception bits 4:0 plus IDC at bit 7 — *not*
//! a contiguous field) is remapped into MXCSR layout before the shared
//! `STATUS_BITS` mask may touch it.
//!
//! Promotion is per-op and evidence-led (HLD D9): the software model in
//! `sse_exec` stays authoritative, and the host-vs-soft differential is the
//! gate. IE is unproven here — where x86 writes the integer indefinite for an
//! out-of-range conversion, AArch64 saturates, so any IOC-raising answer must
//! fall back to the model. UE is likewise permanently unprovable: AArch64
//! (AH=0) detects tininess before rounding, x86 after, so UFC is a strict
//! superset of the guest's UE on the boundary class.

use core::arch::aarch64::{
    vbslq_u32, vbslq_u64, vcgtq_f32, vcgtq_f64, vcltq_f32, vcltq_f64, vreinterpretq_f32_u8,
    vreinterpretq_f64_u8, vreinterpretq_u32_u8, vreinterpretq_u64_u8, vreinterpretq_u8_u32,
    vreinterpretq_u8_u64,
};
use core::arch::asm;

use crate::sse_exec::{from_vq as from_v, to_vq as to_v, FloatOperation};
use crate::sse_host_common::{classify, promote, DE, IE, OE, PE, UE, ZE};

/// The status bits whose appearance takes an answer outside the proven
/// domain, for every family on this backend (see the module doc for why IE
/// and UE can never join the proven set here).
const A64_UNPROVEN: u32 = IE | OE | UE;

/// The ambient-environment gate (HLD N10). Every field of a non-default FPCR
/// is hazardous or unargued here — RMode would silently mis-round, FZ/FIZ
/// change flush behaviour, DN rewrites NaN results, AH moves tininess
/// detection, NEP makes scalar ops merge, and the trap-enable bits would
/// SIGFPE our own process — so the one sound predicate is "exactly the
/// default word". A non-default environment is a *supported* state that
/// declines to the software model; it is not a bug, and nothing asserts on
/// it.
#[inline]
fn host_fp_env_is_default(fpcr: u64) -> bool {
    fpcr == 0
}

/// Read the host FPCR. Checked by the dispatchers *before* any kernel runs,
/// so a trap-enabled ambient environment declines before a single host FP
/// instruction executes. Unlike FPSR it is not mutated by ambient arithmetic,
/// so reading it outside the kernels' asm blocks is sound.
#[inline]
fn read_fpcr() -> u64 {
    let value: u64;
    // SAFETY: `MRS` from FPCR reads a per-thread system register and touches
    // nothing else.
    unsafe {
        // x87-inventory-exempt: AArch64-only FPSR/FPCR system-register access — no host x87 exists on this architecture, so the native compiled residency tripwire this guard demands would report a violation that cannot occur (drc_runtime cntvct/cntfrq precedent, 2026-08-16).
        asm!("mrs {v}, fpcr", v = out(reg) value, options(nostack, nomem));
    }
    value
}

/// True when every dispatcher may consider promoting at all: the ambient
/// environment is the default one the kernels were argued against.
#[inline]
pub(crate) fn env_ok() -> bool {
    host_fp_env_is_default(read_fpcr())
}

/// Guest RC field is round-nearest. Required by every kernel whose rounding
/// comes from the environment (arithmetic, sqrt, int→float); the
/// rounding-embedded conversions do not consult it.
#[inline]
fn rc_is_nearest(mxcsr: u32) -> bool {
    mxcsr & (0b11 << 13) == 0
}

/// Remap a raw FPSR word into the MXCSR-layout six bits the executor
/// publishes. The raw exception bits are IOC(0) DZC(1) OFC(2) UFC(3) IXC(4)
/// IDC(7); everything else (QC included) is dropped here, which is what makes
/// the shared MXCSR-layout `STATUS_BITS` mask safe downstream.
#[inline]
fn remap_fpsr(raw: u64) -> u32 {
    let raw = raw as u32;
    let mut status = 0;
    if raw & (1 << 0) != 0 {
        status |= IE;
    }
    if raw & (1 << 1) != 0 {
        status |= ZE;
    }
    if raw & (1 << 2) != 0 {
        status |= OE;
    }
    if raw & (1 << 3) != 0 {
        status |= UE;
    }
    if raw & (1 << 4) != 0 {
        status |= PE;
    }
    if raw & (1 << 7) != 0 {
        status |= DE;
    }
    status
}

/// Generate one FPSR-bracketed kernel: save, clear, one operation, capture,
/// restore — a single asm block, so no compiler-scheduled FP work can land
/// inside the measured window, and the restored word is the *saved* one,
/// never a constant, so host sticky status survives.
///
/// `{d}` is the destination register (read-modify-write; the packed forms
/// overwrite it, the scalar forms write their lane and zero the rest, and the
/// dispatcher merges), `{s}` the source.
macro_rules! bracketed_kernel {
    ($(#[$doc:meta])* $name:ident, $op:literal) => {
        $(#[$doc])*
        #[inline]
        fn $name(destination: u128, source: u128) -> (u128, u32) {
            let mut d = to_v(destination);
            let s = to_v(source);
            let raw_status: u64;

            // SAFETY: MRS/MSR on FPSR touch only the per-thread status
            // register, and the vector op reads/writes only its named
            // registers. Every exception is architecturally non-trapping
            // under the default FPCR the dispatcher has already verified, so
            // the operation cannot fault. `nomem`: nothing here touches
            // memory.
            unsafe {
                // x87-inventory-exempt: AArch64-only FPSR/FPCR system-register access — no host x87 exists on this architecture, so the native compiled residency tripwire this guard demands would report a violation that cannot occur (drc_runtime cntvct/cntfrq precedent, 2026-08-16).
                asm!(
                    "mrs {saved}, fpsr",
                    "msr fpsr, xzr",
                    $op,
                    "mrs {status}, fpsr",
                    "msr fpsr, {saved}",
                    saved = out(reg) _,
                    status = out(reg) raw_status,
                    d = inout(vreg) d,
                    s = in(vreg) s,
                    options(nostack, nomem),
                );
            }

            (from_v(d), remap_fpsr(raw_status))
        }
    };
}

bracketed_kernel!(addps, "fadd {d:v}.4s, {d:v}.4s, {s:v}.4s");
bracketed_kernel!(addpd, "fadd {d:v}.2d, {d:v}.2d, {s:v}.2d");

bracketed_kernel!(subps, "fsub {d:v}.4s, {d:v}.4s, {s:v}.4s");
bracketed_kernel!(subpd, "fsub {d:v}.2d, {d:v}.2d, {s:v}.2d");

bracketed_kernel!(mulps, "fmul {d:v}.4s, {d:v}.4s, {s:v}.4s");
bracketed_kernel!(mulpd, "fmul {d:v}.2d, {d:v}.2d, {s:v}.2d");

bracketed_kernel!(divps, "fdiv {d:v}.4s, {d:v}.4s, {s:v}.4s");
bracketed_kernel!(divpd, "fdiv {d:v}.2d, {d:v}.2d, {s:v}.2d");
bracketed_kernel!(divss, "fdiv {d:s}, {d:s}, {s:s}");
bracketed_kernel!(divsd, "fdiv {d:d}, {d:d}, {s:d}");

// The square roots ignore {d}'s incoming value (the packed forms overwrite
// it whole, the scalar forms zero-extend); it still rides the same template
// so the dispatcher's merge is uniform.
bracketed_kernel!(sqrtps, "fsqrt {d:v}.4s, {s:v}.4s");
bracketed_kernel!(sqrtpd, "fsqrt {d:v}.2d, {s:v}.2d");
bracketed_kernel!(sqrtss, "fsqrt {d:s}, {s:s}");
bracketed_kernel!(sqrtsd, "fsqrt {d:d}, {s:d}");

/// Merge a scalar kernel's lane back into the surviving destination lanes.
/// This extraction is the *relied-on* mechanism (HLD §3.2): the kernels'
/// upper output lanes are never trusted, so FEAT_AFP's NEP merging could not
/// corrupt a result even if the environment check were somehow bypassed.
#[inline]
fn merge_lane0(destination: u128, result: u128, double: bool) -> u128 {
    let mask = if double {
        u128::from(u64::MAX)
    } else {
        u128::from(u32::MAX)
    };
    (destination & !mask) | (result & mask)
}

/// ADD/SUB/MUL/DIV — the measured-winner forms only.
///
/// Packed forms promote (probe: addps 49.3 ns vs 75.8 soft, addpd 29.8 vs
/// 32.7), and so do the scalar divides (divss 31.7 vs 36.5 — software
/// division is dear enough to carry the bracket). Scalar add/sub/mul
/// decline before anything else runs: the ~30 ns bracket path loses to the
/// ~22 ns single-lane software model, measured same-binary.
///
/// Guards (HLD §3.3): default ambient environment; guest round-nearest (the
/// rounding would otherwise come from an FPCR we refuse to write); no NaN
/// operand (kept so correctness never leans on the DN/NaN-priority analysis —
/// see N4); no denormal operand in *either* DAZ state (the default
/// environment cannot flush inputs). The post-check declines IE/OE/UE.
pub(crate) fn float_binary(
    left: u128,
    right: u128,
    lanes: usize,
    double: bool,
    operation: FloatOperation,
    mxcsr: u32,
) -> Option<(u128, u32)> {
    use FloatOperation as F;
    if lanes == 1 && !matches!(operation, F::Divide) {
        return None;
    }
    if !rc_is_nearest(mxcsr) || !env_ok() {
        return None;
    }
    let first = classify(left, lanes, double);
    let second = classify(right, lanes, double);
    if first.nan || second.nan || first.denormal || second.denormal {
        return None;
    }
    let (value, status) = match (double, lanes, operation) {
        (false, 4, F::Add) => addps(left, right),
        (false, 4, F::Subtract) => subps(left, right),
        (false, 4, F::Multiply) => mulps(left, right),
        (false, 4, F::Divide) => divps(left, right),
        (false, 1, F::Divide) => divss(left, right),
        (true, 2, F::Add) => addpd(left, right),
        (true, 2, F::Subtract) => subpd(left, right),
        (true, 2, F::Multiply) => mulpd(left, right),
        (true, 2, F::Divide) => divpd(left, right),
        (true, 1, F::Divide) => divsd(left, right),
        _ => return None,
    };
    let mut value = if lanes == 1 {
        merge_lane0(left, value, double)
    } else {
        value
    };
    let mut flushed = false;
    if mxcsr & (1 << 15) != 0 {
        for lane in 0..lanes {
            let (shift, mask, exponent, magnitude, sign) = if double {
                (
                    lane * 64,
                    u128::from(u64::MAX),
                    0x7ff0_0000_0000_0000,
                    0x7fff_ffff_ffff_ffff,
                    0x8000_0000_0000_0000,
                )
            } else {
                (
                    lane * 32,
                    u128::from(u32::MAX),
                    0x7f80_0000,
                    0x7fff_ffff,
                    0x8000_0000,
                )
            };
            let bits = (value >> shift) & mask;
            if bits & exponent == 0 && bits & magnitude != 0 {
                // Intel SDM Vol. 1, 10.2.3.3: masked FTZ returns signed zero
                // and sets UE|PE even when the tiny result was exact. The
                // default ARM environment leaves exact subnormals intact.
                // Unmasked underflow keeps the existing software trap path.
                if mxcsr & (1 << 11) == 0 {
                    return None;
                }
                value = (value & !(mask << shift)) | ((bits & sign) << shift);
                flushed = true;
            }
        }
    }
    // Reject unproven host exceptions before adding the guest FTZ flags.
    let (value, status) = promote(value, status, A64_UNPROVEN)?;
    Some((value, status | if flushed { UE | PE } else { 0 }))
}

/// SQRT, packed and scalar, single and double — every form promotes, and
/// this family is the backend's real prize: the software `sqrt_accurate`
/// path measures 1031.8 ns (sqrtps) / 504.6 ns (sqrtss) against 46.9 / 34.9
/// for the kernels.
///
/// The x86-64 refusals (NaN, negative, denormal) apply unchanged — negative
/// because the host's QNaN-indefinite differs from `sqrt_accurate`'s answer —
/// plus the environment and round-nearest gates.
pub(crate) fn square_root(
    destination: u128,
    input: u128,
    lanes: usize,
    double: bool,
    mxcsr: u32,
) -> Option<(u128, u32)> {
    if !rc_is_nearest(mxcsr) || !env_ok() {
        return None;
    }
    let operand = classify(input, lanes, double);
    if operand.nan || operand.negative || operand.denormal {
        return None;
    }
    let (value, status) = match (double, lanes) {
        (false, 4) => sqrtps(destination, input),
        (false, _) => sqrtss(destination, input),
        (true, 2) => sqrtpd(destination, input),
        (true, _) => sqrtsd(destination, input),
    };
    let value = if lanes == 1 {
        merge_lane0(destination, value, double)
    } else {
        value
    };
    promote(value, status, A64_UNPROVEN)
}

/// MIN/MAX — packed forms only (probe: minps 13.0 ns vs 14.5 soft; the
/// scalar forms lose, 7.4 vs 5.0, because the soft path is already a bare
/// compare). No FPSR bracket and no environment gate: on the guarded domain
/// nothing here can raise a flag or consult the environment (HLD N6).
///
/// ARM's native FMIN/FMAX is semantically wrong for SSE (−0 < +0 ordering,
/// NaN propagation), so the guest rule is encoded directly: take src1 iff
/// src1<src2 (min) / src1>src2 (max), else src2 — min(+0,−0)=src2 included.
/// AArch64's ordered vector compares are *signalling* (IOC on any NaN, quiet
/// included), which is why the NaN guard covers every lane.
pub(crate) fn min_max(
    left: u128,
    right: u128,
    lanes: usize,
    double: bool,
    maximum: bool,
    mxcsr: u32,
) -> Option<(u128, u32)> {
    let _ = mxcsr; // compares neither round nor flush; no environment gate
    if lanes == 1 {
        return None;
    }
    let first = classify(left, lanes, double);
    let second = classify(right, lanes, double);
    if first.nan || second.nan || first.denormal || second.denormal {
        return None;
    }
    // One semantic body for both widths — the compare direction and the
    // bit-select argument order are the load-bearing parts, and a fix to
    // either must not have a second copy to miss.
    macro_rules! take_second_select {
        ($gt:path, $lt:path, $bsl:path, $lanes_of:path, $bits_of:path, $bits_back:path) => {{
            let a = to_v(left);
            let b = to_v(right);
            // SAFETY: NEON is baseline on aarch64; every lane is
            // guard-verified NaN-free, so the signalling compare is silent.
            unsafe {
                let fa = $lanes_of(a);
                let fb = $lanes_of(b);
                let take_first = if maximum { $gt(fa, fb) } else { $lt(fa, fb) };
                from_v($bits_back($bsl(take_first, $bits_of(a), $bits_of(b))))
            }
        }};
    }
    let value = if double {
        take_second_select!(
            vcgtq_f64,
            vcltq_f64,
            vbslq_u64,
            vreinterpretq_f64_u8,
            vreinterpretq_u64_u8,
            vreinterpretq_u8_u64
        )
    } else {
        take_second_select!(
            vcgtq_f32,
            vcltq_f32,
            vbslq_u32,
            vreinterpretq_f32_u8,
            vreinterpretq_u32_u8,
            vreinterpretq_u8_u32
        )
    };
    promote(value, 0, A64_UNPROVEN)
}

/// The conversions never promote on this backend — measured, not assumed
/// (probe: soft cvtss2si 8.4 ns, cvtsi2ss 4.9 ns, cvtps2dq 29.3 ns against a
/// ~30 ns bracketed-FCVT floor). The seam functions remain so the executor's
/// call sites stay arch-uniform; each declines unconditionally, and the
/// host_soft expectation table plus the measured-decline contract test pin
/// this so a re-promotion is a deliberate, re-measured decision.
pub(crate) fn packed_to_int32(_input: u128, _truncating: bool, _mxcsr: u32) -> Option<(u128, u32)> {
    None
}

/// See `packed_to_int32`.
pub(crate) fn single_to_int32(_bits: u32, _truncating: bool, _mxcsr: u32) -> Option<(i32, u32)> {
    None
}

/// See `packed_to_int32`.
pub(crate) fn double_to_int32(_bits: u64, _truncating: bool, _mxcsr: u32) -> Option<(i32, u32)> {
    None
}

/// See `packed_to_int32`.
pub(crate) fn int32_to_float(
    _destination: u128,
    _value: i32,
    _double: bool,
    _mxcsr: u32,
) -> Option<(u128, u32)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const RN_MASKED: u32 = 0x0000_1F80;

    /// Read the host FPSR (production capture happens inside the kernels'
    /// own asm blocks; the tests read it around them).
    fn read_fpsr() -> u64 {
        let value: u64;
        // SAFETY: as `read_fpcr`, for the status register.
        unsafe {
            // x87-inventory-exempt: AArch64-only FPSR/FPCR system-register access — no host x87 exists on this architecture, so the native compiled residency tripwire this guard demands would report a violation that cannot occur (drc_runtime cntvct/cntfrq precedent, 2026-08-16).
            asm!("mrs {v}, fpsr", v = out(reg) value, options(nostack, nomem));
        }
        value
    }

    fn write_fpsr(value: u64) {
        // SAFETY: FPSR is per-thread; tests seed sentinels and restore.
        unsafe {
            // x87-inventory-exempt: AArch64-only FPSR/FPCR system-register access — no host x87 exists on this architecture, so the native compiled residency tripwire this guard demands would report a violation that cannot occur (drc_runtime cntvct/cntfrq precedent, 2026-08-16).
            asm!("msr fpsr, {v}", v = in(reg) value, options(nostack, nomem));
        }
    }

    fn write_fpcr(value: u64) {
        // SAFETY: FPCR is per-thread; tests set a non-default word to prove
        // the dispatchers decline, and restore the entry word immediately
        // after.
        unsafe {
            // x87-inventory-exempt: AArch64-only FPSR/FPCR system-register access — no host x87 exists on this architecture, so the native compiled residency tripwire this guard demands would report a violation that cannot occur (drc_runtime cntvct/cntfrq precedent, 2026-08-16).
            asm!("msr fpcr, {v}", v = in(reg) value, options(nostack, nomem));
        }
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

    /// The remap is pinned one bit at a time: an aggregate all-bits case
    /// cannot see a swapped mapping (every permutation of a full house still
    /// yields the full house), so each source bit is driven alone.
    #[test]
    fn the_fpsr_remap_is_pinned_one_hot() {
        const CASES: &[(u64, u32)] = &[
            (0, 0),
            (1 << 0, IE),                        // IOC
            (1 << 1, ZE),                        // DZC
            (1 << 2, OE),                        // OFC
            (1 << 3, UE),                        // UFC
            (1 << 4, PE),                        // IXC
            (1 << 7, DE),                        // IDC
            (1 << 27, 0),                        // QC is not a guest-visible exception
            (1 << 30, 0),                        // Z (AArch32 compare flags) likewise
            (0x9F, IE | ZE | OE | UE | PE | DE), // aggregate, secondary only
        ];
        for &(raw, expected) in CASES {
            assert_eq!(remap_fpsr(raw), expected, "raw {raw:#010x}");
        }
    }

    /// The environment predicate refuses every FPCR field independently — an
    /// implementation that forgot a field would pass an RMode-only test.
    #[test]
    fn the_environment_predicate_refuses_every_field() {
        assert!(host_fp_env_is_default(0));
        for (bit, name) in [
            (0u64, "FIZ"),
            (1, "AH"),
            (2, "NEP"),
            (8, "IOE trap"),
            (9, "DZE trap"),
            (10, "OFE trap"),
            (11, "UFE trap"),
            (12, "IXE trap"),
            (15, "IDE trap"),
            (22, "RMode low"),
            (23, "RMode high"),
            (24, "FZ"),
            (25, "DN"),
        ] {
            assert!(
                !host_fp_env_is_default(1 << bit),
                "{name} (bit {bit}) accepted"
            );
        }
    }

    /// A bracketed kernel leaves the host FPSR exactly as it found it, sticky
    /// status included.
    #[test]
    fn a_kernel_restores_the_incoming_host_fpsr() {
        let entry = read_fpsr();
        // Seed a sentinel: QC plus a stale IXC.
        write_fpsr((1 << 27) | (1 << 4));
        // An inexact divide, so the kernel demonstrably raises status of its
        // own inside the bracket.
        let promoted = float_binary(
            0x3f80_0000,
            0x4040_0000,
            1,
            false,
            FloatOperation::Divide,
            RN_MASKED,
        );
        assert!(promoted.is_some(), "1.0/3.0 must promote");
        assert_eq!(read_fpsr(), (1 << 27) | (1 << 4), "kernel leaked FPSR");
        write_fpsr(entry);
    }

    /// The captured word describes this instruction, not the host's history.
    #[test]
    fn captured_status_is_not_sticky_across_kernels() {
        let (_, inexact) = divss(0x3f80_0000, 0x4040_0000);
        assert_ne!(inexact & PE, 0, "expected PE from an inexact divide");
        // 1.0 / 2.0 is exact.
        let (value, exact) = divss(0x3f80_0000, 0x4000_0000);
        assert_eq!(value as u32, 0x3f00_0000);
        assert_eq!(exact, 0, "a previous instruction's status leaked in");
    }

    /// A non-default ambient environment declines cleanly — no panic, no
    /// promotion — and promotion resumes once the environment is restored.
    #[test]
    fn a_non_default_environment_declines_to_soft() {
        let entry = read_fpcr();
        assert_eq!(entry, 0, "test premise: this thread runs the default env");
        let ones = packed32([1.0; 4]);
        // Round-toward-minus in FPCR.RMode.
        write_fpcr(0b10 << 22);
        let declined = float_binary(ones, ones, 4, false, FloatOperation::Add, RN_MASKED);
        write_fpcr(entry);
        assert!(declined.is_none(), "non-default FPCR must decline");
        let promoted = float_binary(ones, ones, 4, false, FloatOperation::Add, RN_MASKED);
        assert_eq!(promoted, Some((packed32([2.0; 4]), 0)));
    }

    /// A guest rounding mode other than nearest declines every env-rounded
    /// family.
    #[test]
    fn guest_directed_rounding_declines_env_rounded_kernels() {
        let toward_zero = RN_MASKED | (0b11 << 13);
        let ones = packed32([1.0; 4]);
        assert!(float_binary(ones, ones, 4, false, FloatOperation::Add, toward_zero).is_none());
        assert!(square_root(0, packed32([4.0; 4]), 4, false, toward_zero).is_none());
    }

    /// The measured-decline contract: the forms the probe showed losing to
    /// the software model return None even on ordinary round-nearest inputs.
    /// Re-promoting one is a deliberate decision that starts by re-measuring
    /// and updating this test plus the host_soft expectation table together.
    #[test]
    fn measured_decline_forms_stay_soft() {
        let ones = packed32([1.0; 4]);
        for operation in [
            FloatOperation::Add,
            FloatOperation::Subtract,
            FloatOperation::Multiply,
        ] {
            assert!(float_binary(ones, ones, 1, false, operation, RN_MASKED).is_none());
            assert!(float_binary(ones, ones, 1, true, operation, RN_MASKED).is_none());
        }
        assert!(min_max(ones, ones, 1, false, false, RN_MASKED).is_none());
        assert!(min_max(ones, ones, 1, true, true, RN_MASKED).is_none());
        assert!(packed_to_int32(ones, false, RN_MASKED).is_none());
        assert!(single_to_int32(0x3fc0_0000, false, RN_MASKED).is_none());
        assert!(double_to_int32(0x3ff8_0000_0000_0000, true, RN_MASKED).is_none());
        assert!(int32_to_float(0, 3, false, RN_MASKED).is_none());
        assert!(int32_to_float(0, 3, true, RN_MASKED).is_none());
    }

    /// Scalar kernels merge via lane-0 extraction: the destination's upper
    /// lanes survive byte-for-byte.
    #[test]
    fn a_scalar_kernel_merges_into_the_destination() {
        let destination = 0xdead_beef_cafe_f00d_1234_5678_3f80_0000u128;
        let promoted = float_binary(
            destination,
            u128::from(0x4000_0000u32),
            1,
            false,
            FloatOperation::Divide,
            RN_MASKED,
        );
        let (value, _) = promoted.expect("1.0/2.0 must promote");
        assert_eq!(value >> 32, destination >> 32, "upper lanes were disturbed");
        assert_eq!(value as u32, 0x3f00_0000);
    }

    /// Masked FTZ flushes exact tiny results with their sign and records both
    /// exception flags, while scalar forms preserve inactive destination lanes.
    #[test]
    fn masked_ftz_preserves_sign_flags_and_inactive_lanes() {
        let ftz = RN_MASKED | (1 << 15);
        let cases = [
            (false, 0x0080_0000u128, 0x4000_0000u128, 0x8000_0000u128, 32),
            (
                true,
                0x0010_0000_0000_0000u128,
                0x4000_0000_0000_0000u128,
                0x8000_0000_0000_0000u128,
                64,
            ),
        ];
        for (double, minimum_normal, two, sign, width) in cases {
            // The inactive upper lanes contain subnormal bit patterns too.
            let inactive = if double { 1u128 << 64 } else { 1u128 << 32 };
            for result_sign in [0, sign] {
                let destination = inactive | minimum_normal | result_sign;
                assert_eq!(
                    float_binary(destination, two, 1, double, FloatOperation::Divide, ftz),
                    Some((inactive | result_sign, UE | PE)),
                    "scalar width={width} sign={result_sign:#x}"
                );
                assert!(
                    float_binary(
                        destination,
                        two,
                        1,
                        double,
                        FloatOperation::Divide,
                        ftz & !(1 << 11)
                    )
                    .is_none(),
                    "unmasked tiny result must use the software trap path"
                );
                assert_eq!(
                    float_binary(
                        destination,
                        two,
                        1,
                        double,
                        FloatOperation::Divide,
                        RN_MASKED
                    ),
                    Some((inactive | (minimum_normal >> 1) | result_sign, 0)),
                    "FTZ disabled preserves an exact subnormal"
                );
            }
            let lanes = if double { 2 } else { 4 };
            let mut numerators = 0;
            let mut divisors = 0;
            let mut expected = 0;
            for lane in 0..lanes {
                let lane_sign = if lane % 2 == 0 { 0 } else { sign };
                numerators |= (minimum_normal | lane_sign) << (lane * width);
                divisors |= two << (lane * width);
                expected |= lane_sign << (lane * width);
            }
            assert_eq!(
                float_binary(
                    numerators,
                    divisors,
                    lanes,
                    double,
                    FloatOperation::Divide,
                    ftz
                ),
                Some((expected, UE | PE)),
                "packed width={width}"
            );
        }
    }

    /// The signed-zero ordering is the guest's — min(+0,−0) and max(−0,+0)
    /// both take src2, whatever ARM's native FMIN/FMAX would have said — and
    /// the flag-free claim is proven, not trusted: a sentinel FPSR survives.
    #[test]
    fn packed_min_max_signed_zero_takes_the_second_source() {
        let entry = read_fpsr();
        write_fpsr(0);
        let plus32 = packed32([0.0; 4]);
        let minus32 = packed32([-0.0; 4]);
        let plus64 = packed64([0.0; 2]);
        let minus64 = packed64([-0.0; 2]);
        for maximum in [false, true] {
            for (a, b) in [(plus32, minus32), (minus32, plus32)] {
                assert_eq!(
                    min_max(a, b, 4, false, maximum, RN_MASKED),
                    Some((b, 0)),
                    "single maximum={maximum}"
                );
            }
            for (a, b) in [(plus64, minus64), (minus64, plus64)] {
                assert_eq!(
                    min_max(a, b, 2, true, maximum, RN_MASKED),
                    Some((b, 0)),
                    "double maximum={maximum}"
                );
            }
        }
        assert_eq!(read_fpsr(), 0, "packed min/max touched FPSR");
        write_fpsr(entry);
    }
}
