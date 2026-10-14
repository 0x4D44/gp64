//! Arch-neutral pieces of the host SSE float kernels: the input classifier
//! that bounds every kernel's promotion domain, the promotion post-check, and
//! the MXCSR-layout status bits both backends report in.
//!
//! The guard *policies* that consume these — which inputs each backend
//! refuses, which status bits are unproven — are genuinely per-arch and live
//! in the backend modules (`sse_host_kernels.rs` on x86-64,
//! `sse_host_kernels_a64.rs` on little-endian aarch64). Only the stable bit
//! inspection is shared.

/// The six sticky status bits, MXCSR 5:0. Every kernel reports in this
/// layout — the aarch64 backend remaps raw FPSR bits (4:0 plus IDC at 7, not
/// a contiguous field) and masks *during* that remap, so only the x86-64
/// backend consumes this mask directly.
#[cfg(target_arch = "x86_64")]
pub(crate) const STATUS_BITS: u32 = 0x0000_003F;
/// Invalid operation.
pub(crate) const IE: u32 = 1 << 0;
/// Denormal operand. Consumed by the aarch64 FPSR remap only — the x86-64
/// backend names just IE/OE/UE in its post-check masks, so this is
/// deliberately allowed dead there rather than cfg-scattered.
#[cfg_attr(target_arch = "x86_64", allow(dead_code))]
pub(crate) const DE: u32 = 1 << 1;
/// Divide by zero. Consumed by the aarch64 FPSR remap only — the x86-64
/// backend names just IE/OE/UE in its post-check masks, so this is
/// deliberately allowed dead there rather than cfg-scattered.
#[cfg_attr(target_arch = "x86_64", allow(dead_code))]
pub(crate) const ZE: u32 = 1 << 2;
/// Overflow.
pub(crate) const OE: u32 = 1 << 3;
/// Underflow.
pub(crate) const UE: u32 = 1 << 4;
/// Precision (inexact). Consumed by the aarch64 FPSR remap only — the x86-64
/// backend names just IE/OE/UE in its post-check masks, so this is
/// deliberately allowed dead there rather than cfg-scattered.
#[cfg_attr(target_arch = "x86_64", allow(dead_code))]
pub(crate) const PE: u32 = 1 << 5;

// Count of kernel answers the post-check accepted on *this thread*, for the
// production-engagement witness: a test executes a promotable instruction
// through the full executor and asserts this advanced, which is what proves
// the arch-gated call sites in `sse_exec.rs` actually reach the backend
// rather than silently running the (equally correct) soft path.
// Thread-local on purpose: the test harness runs tests on parallel threads,
// and a process-global counter would let another test's promotions mask a
// non-engaging op in the witness's before/after window. (Plain comment: a
// doc comment on a macro invocation is discarded and warns. The cfg rides
// inside the macro so the iced-free guard's one-top-level-`#[cfg(test)]`
// premise holds and this file's production half stays fully scanned.)
thread_local! {
    #[cfg(test)]
    pub(crate) static PROMOTIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[inline]
fn record_promotion() {
    #[cfg(test)]
    PROMOTIONS.with(|count| count.set(count.get() + 1));
}

/// The promotion post-check: a kernel's answer is adopted only if it raised
/// none of the status bits its backend declares unproven (HLD D9).
#[inline]
pub(crate) fn promote(value: u128, status: u32, unproven: u32) -> Option<(u128, u32)> {
    if status & unproven == 0 {
        record_promotion();
        Some((value, status))
    } else {
        None
    }
}

/// `promote` for the kernels whose result is a GPR rather than a register
/// pair (the scalar float→int32 conversions — x86-64 only: the aarch64
/// backend's conversions are measured-decline and never reach a post-check).
#[cfg(target_arch = "x86_64")]
#[inline]
pub(crate) fn promote_scalar(value: i32, status: u32, unproven: u32) -> Option<(i32, u32)> {
    if status & unproven == 0 {
        record_promotion();
        Some((value, status))
    } else {
        None
    }
}

/// What the guards need to know about one operand's lanes.
#[derive(Clone, Copy, Default)]
pub(crate) struct Operand {
    /// Any lane is a NaN, quiet or signalling.
    pub(crate) nan: bool,
    /// Any lane is a denormal.
    pub(crate) denormal: bool,
    /// Any lane is negative and not a zero.
    pub(crate) negative: bool,
}

impl Operand {
    /// A denormal operand is only a hazard while DAZ is off. With DAZ set both
    /// backends substitute a zero and report nothing, so they agree. The
    /// aarch64 backend cannot flush inputs at all and refuses denormals in
    /// both DAZ states, so this predicate is the x86-64 guard alone.
    #[cfg(target_arch = "x86_64")]
    #[inline]
    pub(crate) fn denormal_hazard(self, mxcsr: u32) -> bool {
        self.denormal && mxcsr & (1 << 6) == 0
    }
}

/// A plain lane loop. A branchless SSE2 rewrite of the four-lane case was
/// built and measured against this one on the `sse_addps` bench: 500.98 µs
/// against 495.21 µs at 10k iterations, p = 0.74 — no effect, because the
/// per-instruction interpreter overhead around the kernel dwarfs the guard.
/// The simpler form is therefore the one that ships.
#[inline]
pub(crate) fn classify(value: u128, lanes: usize, double: bool) -> Operand {
    let mut seen = Operand::default();
    if double {
        for lane in 0..lanes {
            let bits = (value >> (lane * 64)) as u64;
            let exponent = bits & 0x7ff0_0000_0000_0000;
            let magnitude = bits & 0x7fff_ffff_ffff_ffff;
            seen.nan |= magnitude > 0x7ff0_0000_0000_0000;
            seen.denormal |= exponent == 0 && magnitude != 0;
            seen.negative |= bits & 0x8000_0000_0000_0000 != 0 && magnitude != 0;
        }
    } else {
        for lane in 0..lanes {
            let bits = (value >> (lane * 32)) as u32;
            let exponent = bits & 0x7f80_0000;
            let magnitude = bits & 0x7fff_ffff;
            seen.nan |= magnitude > 0x7f80_0000;
            seen.denormal |= exponent == 0 && magnitude != 0;
            seen.negative |= bits & 0x8000_0000 != 0 && magnitude != 0;
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classifier decides which inputs a kernel may answer for, so a lane
    /// it misreads is a promotion the differential never sees. This pins each
    /// predicate against the definition spelled out per class.
    #[test]
    fn the_classifier_reads_every_lane_class() {
        // (bits, nan, denormal, negative)
        const EXPECTED: &[(u32, bool, bool, bool)] = &[
            (0x0000_0000, false, false, false), // +0 is not negative
            (0x8000_0000, false, false, false), // and neither is -0
            (0x0000_0001, false, true, false),  // smallest denormal
            (0x807f_ffff, false, true, true),   // largest negative denormal
            (0x0080_0000, false, false, false), // the smallest normal is not denormal
            (0x3f80_0000, false, false, false), // 1.0
            (0xbf80_0000, false, false, true),  // -1.0
            (0x7f7f_ffff, false, false, false), // largest finite
            (0x7f80_0000, false, false, false), // an infinity is not a NaN
            (0xff80_0000, false, false, true),  // -inf
            (0x7fc0_0000, true, false, false),  // QNaN
            (0x7f80_0001, true, false, false),  // signalling, smallest payload
            (0xffff_ffff, true, false, true),   // NaN, every bit set
        ];

        for &(bits, nan, denormal, negative) in EXPECTED {
            let scalar = classify(u128::from(bits), 1, false);
            assert_eq!(
                (scalar.nan, scalar.denormal, scalar.negative),
                (nan, denormal, negative),
                "lane 0 of {bits:#010x}"
            );
            // A packed form must see the same class in every lane, and a
            // scalar form must ignore lanes 1..3 entirely.
            let upper_only = u128::from(bits) << 32;
            let quiet = classify(upper_only, 1, false);
            assert_eq!(
                (quiet.nan, quiet.denormal, quiet.negative),
                (false, false, false),
                "a scalar form looked past lane 0 of {bits:#010x}"
            );
            let packed = classify(upper_only, 4, false);
            assert_eq!(
                (packed.nan, packed.denormal, packed.negative),
                (nan, denormal, negative),
                "a packed form missed lane 1 of {bits:#010x}"
            );
        }
    }

    /// The post-check refuses exactly the declared-unproven bits and passes
    /// everything else through untouched.
    #[test]
    fn promote_declines_unproven_bits_and_only_those() {
        let unproven = IE | OE | UE;
        assert_eq!(promote(7, 0, unproven), Some((7, 0)));
        assert_eq!(promote(7, DE | ZE | PE, unproven), Some((7, DE | ZE | PE)));
        for bit in [IE, OE, UE] {
            assert_eq!(promote(7, bit, unproven), None, "bit {bit:#04x}");
            assert_eq!(promote(7, bit | PE, unproven), None, "bit {bit:#04x}|PE");
        }
        #[cfg(target_arch = "x86_64")]
        {
            assert_eq!(promote_scalar(-1, PE, OE | UE), Some((-1, PE)));
            assert_eq!(promote_scalar(-1, UE, OE | UE), None);
        }
    }
}
