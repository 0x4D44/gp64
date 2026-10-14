//! Arch-neutral host-float x87 fast-path cores (U7 fast path + U3 shadow).
//!
//! When the x87 control word narrows precision to single (PC=00) or double
//! (PC=10) and both operands are zero/normal values that the host `f32`/`f64`
//! represents exactly, the basic arithmetic ops can run directly in the host
//! type. `PE`/`C1` are derived from an error-free transform of the host
//! operation (TwoSum for add/sub, an FMA residual for mul, a back-substitution
//! FMA residual for div); directed rounding modes are produced from the
//! nearest-even host result by one-ulp stepping on the exact residual sign.
//! Every core returns `None` on any doubt so its caller falls back to the
//! authoritative pure path.
//!
//! Two consumers share these cores (single numerics source, HLD 2026.07.29
//! D4): `x87_soft.rs` wraps them behind F80 operand decode/encode for the
//! per-op fast path (production on non-x86_64 hosts), and `fpu.rs`'s
//! `NumericShadow` arithmetic calls them directly with proof-carried operands
//! on every host arch. The differential suites in `x87_soft.rs` pin both arms
//! bit-for-bit against the pure `rustc_apfloat` path.

#[cfg(any(test, feature = "interp-attrib"))]
std::thread_local! {
    static FAST_PATH_HITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Test-only instrumentation: counts fast-path engagements so differential
/// suites can prove the fast path actually fires on a healthy fraction of
/// eligible cases, rather than silently passing by always falling back.
#[cfg(any(test, feature = "interp-attrib"))]
pub(crate) fn record_fast_path_hit() {
    FAST_PATH_HITS.with(|c| c.set(c.get() + 1));
}

#[cfg(not(any(test, feature = "interp-attrib")))]
#[inline(always)]
pub(crate) fn record_fast_path_hit() {}

/// Read this thread's engagement count (test/attrib instrumentation).
#[cfg(any(test, feature = "interp-attrib"))]
pub(crate) fn fast_path_hits() -> u64 {
    FAST_PATH_HITS.with(std::cell::Cell::get)
}

/// Reset this thread's engagement count (test instrumentation).
#[cfg(test)]
pub(crate) fn reset_fast_path_hits() {
    FAST_PATH_HITS.with(|c| c.set(0));
}

/// TwoSum (Knuth/Møller error-free transform): returns `(sum, error)` such
/// that `a + b == sum + error` exactly in the reals, for IEEE754
/// round-to-nearest-even addition with no overflow.
#[inline]
pub(crate) fn two_sum_f64(a: f64, b: f64) -> (f64, f64) {
    let s = a + b;
    let bb = s - a;
    let err = (a - (s - bb)) + (b - bb);
    (s, err)
}

/// `two_sum_f64`'s `f32` sibling, for the PC=24 (single precision) fast path.
#[inline]
pub(crate) fn two_sum_f32(a: f32, b: f32) -> (f32, f32) {
    let s = a + b;
    let bb = s - a;
    let err = (a - (s - bb)) + (b - bb);
    (s, err)
}

/// x87 C1 (round-up) under round-to-nearest, derived from an error-free-
/// transform residual: the residual is (true value − rounded value).
/// Rounding increased the magnitude (away from zero, C1=1) exactly when the
/// residual is nonzero and its sign is opposite the rounded result's — adding
/// the residual back would move the result further from zero, meaning the
/// rounded value already overshot.
#[inline]
pub(crate) fn c1_from_residual_signs(
    residual_zero: bool,
    residual_negative: bool,
    rounded_negative: bool,
) -> bool {
    !residual_zero && residual_negative != rounded_negative
}

/// Pack a fast-path arithmetic status word: bit5 (PE) and bit9 (C1), the only
/// bits the fast path ever sets — every exceptional case (IE/DE/ZE/OE/UE) is
/// excluded by eligibility and falls back to the pure path instead.
#[inline]
pub(crate) fn fast_arith_sw(pe: bool, c1: bool) -> u16 {
    if !pe {
        return 0;
    }
    if c1 {
        0x0220
    } else {
        0x0020
    }
}

/// Sign of an exact residual as -1/0/+1. The exponent floors on the callers
/// guarantee a nonzero residual never underflows, so the sign is exact.
#[inline]
pub(crate) fn residual_sign_f64(r: f64) -> i8 {
    if r > 0.0 {
        1
    } else if r < 0.0 {
        -1
    } else {
        0
    }
}

#[inline]
pub(crate) fn residual_sign_f32(r: f32) -> i8 {
    if r > 0.0 {
        1
    } else if r < 0.0 {
        -1
    } else {
        0
    }
}

/// C1 for a fast-path result, matching the pure path's conventions exactly:
/// directed modes use the final result's sign, truncation never rounds up,
/// and nearest uses the residual-derived magnitude rule.
#[inline]
pub(crate) fn fast_c1(pe: bool, rc: u16, result_negative: bool, nearest_c1: bool) -> bool {
    if !pe {
        return false;
    }
    match rc {
        0b01 => result_negative,  // toward -infinity
        0b10 => !result_negative, // toward +infinity
        0b11 => false,            // toward zero
        _ => nearest_c1,
    }
}

/// Directed-rounding adjustment: convert the host's round-to-nearest-even
/// result `s` into the control word's rounding mode using the exact residual
/// sign (sign of `true_value - s`; 0 when exact). `next_up`/`next_down` step
/// exactly one ulp on the host grid, which IS the PC-precision grid for the
/// fast path's eligible range. Returns None when the adjusted value leaves
/// the zero-or-normal range — the caller falls back to the pure path.
macro_rules! define_apply_rc {
    ($name:ident, $t:ty) => {
        #[inline]
        pub(crate) fn $name(s: $t, residual_sign: i8, rc: u16) -> Option<$t> {
            if residual_sign == 0 || rc == 0b00 {
                return Some(s);
            }
            let adjusted = match rc {
                // Toward zero: nearest overshot in magnitude exactly when the
                // residual points back toward zero (opposite the result's
                // sign); truncation is then one ulp toward zero.
                0b11 => {
                    if (residual_sign > 0) != s.is_sign_negative() {
                        return Some(s);
                    }
                    if s > 0.0 {
                        s.next_down()
                    } else {
                        s.next_up()
                    }
                }
                // Toward -inf: `s` already lies at or below the true value
                // when the residual is positive; otherwise step down.
                0b01 => {
                    if residual_sign > 0 {
                        return Some(s);
                    }
                    s.next_down()
                }
                // Toward +inf, mirror image.
                _ => {
                    if residual_sign < 0 {
                        return Some(s);
                    }
                    s.next_up()
                }
            };
            if adjusted == 0.0 || adjusted.is_normal() {
                Some(adjusted)
            } else {
                None // crossed into subnormal/infinite: pure path owns it
            }
        }
    };
}
define_apply_rc!(apply_rc_f64, f64);
define_apply_rc!(apply_rc_f32, f32);

/// IEEE-754 zero-sign rule for exact-zero sums: under roundTowardNegative an
/// exact zero sum/difference is -0 unless both effective addends are +0. The
/// host computes in round-to-nearest (where such zeros are +0), so the fast
/// add/sub arms re-sign here. Multiplication/division zero signs are mode-
/// independent (operand sign XOR) and need no fixup.
macro_rules! define_fix_exact_zero_sign {
    ($name:ident, $t:ty) => {
        #[inline]
        pub(crate) fn $name(s: $t, x: $t, y: $t, residual_sign: i8, rc: u16) -> $t {
            if s == 0.0
                && residual_sign == 0
                && rc == 0b01
                && s.is_sign_positive()
                && !(x.is_sign_positive() && y.is_sign_positive())
            {
                -0.0
            } else {
                s
            }
        }
    };
}
define_fix_exact_zero_sign!(fix_exact_zero_sign_f64, f64);
define_fix_exact_zero_sign!(fix_exact_zero_sign_f32, f32);

/// The add/mul/div cores over exactness-proven host-float operands, one set
/// per host width. Subtraction is `add_core(fa, -fb)` (the callers negate).
/// Each returns the rounding-adjusted host result plus the modeled PE/C1
/// status bits, or `None` when the pure path must own the case. The mul/div
/// magnitude floors keep the FMA residual's exponent above the subnormal
/// flush zone so an inexact result can never read back as exact (values per
/// width: ~2^-100 for f32, ~2^-960 for f64).
macro_rules! define_fast_cores {
    ($t:ty, $add_core:ident, $mul_core:ident, $div_core:ident,
     $two_sum:ident, $residual_sign:ident, $apply_rc:ident,
     $fix_zero:ident, $floor:expr) => {
        #[inline]
        pub(crate) fn $add_core(fa: $t, fb: $t, rc: u16) -> Option<($t, u16)> {
            let (s, e) = $two_sum(fa, fb);
            if !(s == 0.0 || s.is_normal()) {
                return None;
            }
            let res_sign = $residual_sign(e);
            let s = $fix_zero(s, fa, fb, res_sign, rc);
            let s_adj = $apply_rc(s, res_sign, rc)?;
            let pe = res_sign != 0;
            let nearest_c1 =
                c1_from_residual_signs(!pe, e.is_sign_negative(), s.is_sign_negative());
            let c1 = fast_c1(pe, rc, s_adj.is_sign_negative(), nearest_c1);
            record_fast_path_hit();
            Some((s_adj, fast_arith_sw(pe, c1)))
        }

        #[inline]
        pub(crate) fn $mul_core(fa: $t, fb: $t, rc: u16) -> Option<($t, u16)> {
            let p = fa * fb;
            if p == 0.0 {
                // Zero product from nonzero operands is a host underflow the
                // pure path owns. Operands are exact, so `== 0.0` is the F80
                // zero test.
                if !(fa == 0.0 || fb == 0.0) {
                    return None;
                }
            } else if !p.is_normal() || p.abs() < $floor {
                return None;
            }
            let e = fa.mul_add(fb, -p);
            let res_sign = $residual_sign(e);
            let p_adj = $apply_rc(p, res_sign, rc)?;
            let pe = res_sign != 0;
            let nearest_c1 =
                c1_from_residual_signs(!pe, e.is_sign_negative(), p.is_sign_negative());
            let c1 = fast_c1(pe, rc, p_adj.is_sign_negative(), nearest_c1);
            record_fast_path_hit();
            Some((p_adj, fast_arith_sw(pe, c1)))
        }

        #[inline]
        pub(crate) fn $div_core(fa: $t, fb: $t, rc: u16) -> Option<($t, u16)> {
            if fb == 0.0 {
                return None; // pure path owns #ZE and the infinity result
            }
            let q = fa / fb;
            if q == 0.0 {
                if fa != 0.0 {
                    return None;
                }
            } else if !q.is_normal() || q.abs() < $floor || fa.abs() < $floor {
                // Floors on quotient AND dividend: the back-substitution
                // residual q*b - a is a multiple of the dividend's ulp scale.
                return None;
            }
            let r = q.mul_add(fb, -fa); // back-substitution residual: q*b - a
            let delta = -(r / fb); // true_quotient - q
            let res_sign = $residual_sign(delta);
            let q_adj = $apply_rc(q, res_sign, rc)?;
            let pe = res_sign != 0;
            let nearest_c1 =
                c1_from_residual_signs(!pe, delta.is_sign_negative(), q.is_sign_negative());
            let c1 = fast_c1(pe, rc, q_adj.is_sign_negative(), nearest_c1);
            record_fast_path_hit();
            Some((q_adj, fast_arith_sw(pe, c1)))
        }
    };
}
define_fast_cores!(
    f32,
    fast_add_f32_core,
    fast_mul_f32_core,
    fast_div_f32_core,
    two_sum_f32,
    residual_sign_f32,
    apply_rc_f32,
    fix_exact_zero_sign_f32,
    1e-30
);
define_fast_cores!(
    f64,
    fast_add_f64_core,
    fast_mul_f64_core,
    fast_div_f64_core,
    two_sum_f64,
    residual_sign_f64,
    apply_rc_f64,
    fix_exact_zero_sign_f64,
    1e-289
);

/// FSQRT core over an exactness-proven host-double operand (the dynrec-drc
/// binary64 contract's unary sibling of the cores above — the reference emulator's double
/// register file computes sqrt as a host-double op). Host `f64::sqrt` is
/// IEEE-correctly-rounded to nearest-even; PE and the residual sign come from
/// the back-substitution FMA residual `q*q - a` (the div core's technique),
/// and directed modes step one ulp off the nearest result. The operand floor
/// keeps the residual's quantum (`2*ulp-exponent of q`, at least `2^-1064`
/// for operands above it) clear of the flush-to-zero boundary so a nonzero
/// residual can never read back as exact. Returns `None` on any doubt —
/// negative, denormal, NaN, infinite, or sub-floor operands — so the caller
/// falls back to the authoritative pure path (which owns IE/DE).
#[inline]
pub(crate) fn fast_sqrt_f64_core(fa: f64, rc: u16) -> Option<(f64, u16)> {
    if fa == 0.0 {
        // sqrt(+-0) = +-0: exact in every rounding mode, no flags.
        record_fast_path_hit();
        return Some((fa, 0));
    }
    if !fa.is_normal() || fa.is_sign_negative() {
        return None;
    }
    if fa < 1e-289 {
        return None;
    }
    let q = fa.sqrt();
    // sqrt maps the admitted normal-positive range into the normal range, so
    // q needs no range check. Residual r = fl(q*q - fa); its sign equals the
    // exact sign, and r == 0 exactly when q is the exact square root, so the
    // true-minus-rounded residual sign is -sign(r).
    let r = q.mul_add(q, -fa);
    let res_sign = -residual_sign_f64(r);
    let q_adj = apply_rc_f64(q, res_sign, rc)?;
    let pe = res_sign != 0;
    let nearest_c1 = c1_from_residual_signs(!pe, res_sign < 0, false);
    let c1 = fast_c1(pe, rc, false, nearest_c1);
    record_fast_path_hit();
    Some((q_adj, fast_arith_sw(pe, c1)))
}

#[cfg(test)]
mod sqrt_core_tests {
    use super::fast_sqrt_f64_core;
    use rustc_apfloat::ieee::{Double, Quad};
    use rustc_apfloat::{Float, FloatConvert, Round};

    /// Widen an f64 to Quad — exact by construction.
    fn quad(x: f64) -> Quad {
        let mut loses = false;
        Double::from_bits(u128::from(x.to_bits()))
            .convert_r(Round::NearestTiesToEven, &mut loses)
            .value
    }

    /// `x*x` in Quad: a 53-bit square needs 106 bits, so the 113-bit Quad
    /// holds it exactly.
    fn sq(x: f64) -> Quad {
        let q = quad(x);
        q.mul_r(q, Round::NearestTiesToEven).value
    }

    /// The square of the midpoint between two adjacent f64s: the midpoint is
    /// a 54-bit value (exact in Quad), its square needs 108 bits — exact.
    fn mid_sq(a: f64, b: f64) -> Quad {
        let mid = quad(a)
            .add_r(quad(b), Round::NearestTiesToEven)
            .value
            .scalbn(-1);
        mid.mul_r(mid, Round::NearestTiesToEven).value
    }

    /// Ground-truth differential with EXACT Quad arithmetic — deliberately
    /// not `ieee_apsqrt` as the judge, whose directed-rounding results are
    /// only faithful (it bumps the exact root of 62500 under toward-positive;
    /// `x87_soft` likewise only ever consumes it in toward-zero). For a
    /// candidate root `c` of `fa`, correctness per rounding mode is a pure
    /// bracket statement about `c^2` vs `fa`, and every square involved fits
    /// Quad exactly. A tie against a rounding boundary is impossible: a
    /// midpoint has a 54-bit odd significand, so its square has a 107-bit odd
    /// significand and can never equal a 53-bit `fa`.
    #[test]
    fn fast_sqrt_f64_core_is_correctly_rounded_in_every_mode() {
        let mut operands: Vec<f64> = vec![
            0.0,
            -0.0,
            1.0,
            4.0,
            0.25,
            6.25e4, // exact roots
            2.0,
            3.0,
            1.5,
            0.5,
            10.0,
            std::f64::consts::PI,
            1e300,
            2e-289, // just above the eligibility floor
            f64::MAX,
        ];
        // Deterministic xorshift sweep over positive normals.
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        for _ in 0..4000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let value = f64::from_bits(state & 0x7FEF_FFFF_FFFF_FFFF);
            if value.is_normal() {
                operands.push(value);
            }
        }
        let mut engaged = 0u32;
        for &fa in &operands {
            for rc in 0..4u16 {
                let Some((c, sw)) = fast_sqrt_f64_core(fa, rc) else {
                    continue;
                };
                engaged += 1;
                let pe = sw & 0x20 != 0;
                let c1 = sw & 0x200 != 0;
                let target = quad(fa);
                if !pe {
                    assert!(
                        sq(c) == target,
                        "PE clear must mean an exact root: sqrt({fa:e}) rc={rc} gave {c:e}"
                    );
                    assert!(!c1, "an exact root must clear C1: sqrt({fa:e}) rc={rc}");
                    continue;
                }
                assert!(
                    sq(c) != target,
                    "PE set on an exact root: sqrt({fa:e}) rc={rc} gave {c:e}"
                );
                // Mode bracket: c must be the correctly-rounded root.
                match rc {
                    0b01 | 0b11 => {
                        // Toward -inf / toward zero on a positive root: the
                        // largest f64 whose square is below fa.
                        assert!(
                            sq(c) < target && sq(c.next_up()) > target,
                            "truncation bracket failed: sqrt({fa:e}) rc={rc} gave {c:e}"
                        );
                    }
                    0b10 => {
                        assert!(
                            sq(c) > target && sq(c.next_down()) < target,
                            "round-up bracket failed: sqrt({fa:e}) rc={rc} gave {c:e}"
                        );
                    }
                    _ => {
                        assert!(
                            mid_sq(c.next_down(), c) < target && mid_sq(c, c.next_up()) > target,
                            "nearest bracket failed: sqrt({fa:e}) gave {c:e}"
                        );
                    }
                }
                // C1 is 'rounded up in magnitude': for a positive root,
                // exactly c^2 > fa.
                assert_eq!(
                    c1,
                    sq(c) > target,
                    "C1 must mean 'rounded up in magnitude': sqrt({fa:e}) rc={rc}"
                );
            }
        }
        assert!(
            engaged > 8000,
            "the sweep must actually exercise the fast path (engaged only {engaged} times)"
        );
    }

    /// The refusal set: everything the pure path owns must return None.
    #[test]
    fn fast_sqrt_f64_core_refuses_what_the_pure_path_owns() {
        for rc in 0..4u16 {
            assert!(fast_sqrt_f64_core(-1.0, rc).is_none(), "negative");
            assert!(fast_sqrt_f64_core(f64::NAN, rc).is_none(), "NaN");
            assert!(fast_sqrt_f64_core(f64::INFINITY, rc).is_none(), "+inf");
            assert!(fast_sqrt_f64_core(f64::NEG_INFINITY, rc).is_none(), "-inf");
            assert!(
                fast_sqrt_f64_core(f64::MIN_POSITIVE / 2.0, rc).is_none(),
                "denormal"
            );
            assert!(fast_sqrt_f64_core(1e-300, rc).is_none(), "below the floor");
        }
        // sqrt(+-0) = +-0 exactly, sign preserved, no flags.
        for rc in 0..4u16 {
            assert_eq!(fast_sqrt_f64_core(0.0, rc), Some((0.0, 0)));
            let (negative_zero, sw) = fast_sqrt_f64_core(-0.0, rc).unwrap();
            assert!(negative_zero == 0.0 && negative_zero.is_sign_negative());
            assert_eq!(sw, 0);
        }
    }
}
