//! V.34 TX pre-emphasis filter (ITU-T V.34 Table 4).
//!
//! Pre-distorts the transmitted signal spectrum to compensate for the
//! frequency-dependent attenuation of a typical PSTN line (roughly
//! 3 dB/kHz roll-off above 1 kHz). Applied in the TX path **before** the
//! RRC pulse-shaping filter so the signal arrives approximately flat at
//! the far end and the receive-side equalizer only needs to correct
//! residual channel non-idealities.
//!
//! V.34 defines ten pre-emphasis profiles, indices 0..=9:
//! - Index 0 is flat (no pre-emphasis).
//! - Indices 1..=9 apply monotonically increasing high-frequency boost.
//!
//! The index is negotiated during the V.34 training handshake (see
//! `v34_training.rs`): each side measures per-tone SNR during Phase 2
//! line probing, recommends an index in INFO1, and the agreed index is
//! confirmed in the Phase 4 MP message. This module only implements the
//! filter itself; selection and negotiation live elsewhere.
//!
//! ## Sample rate
//!
//! The filter runs at the V.34 wire sample rate of **16 kHz** (see
//! `wire::WIRE_SAMPLE_RATE`). All coefficients in `PROFILES` are designed
//! at this rate; changing the wire rate requires redesigning the table.
//!
//! ## Structure
//!
//! Each profile is realized as a single biquad (second-order IIR)
//! section in transposed direct form II:
//!
//! ```text
//! y[n] = b0*x[n] + s1
//! s1   = b1*x[n] - a1*y[n] + s2
//! s2   = b2*x[n] - a2*y[n]
//! ```
//!
//! Coefficients are normalised so `a[0] = 1` and `b[0] = 1`. Index 0
//! is `b = [1, 0, 0], a = [0, 0]` (identity).
//!
//! A single biquad is sufficient: V.34 Table 4 profiles are first- or
//! second-order responses (one or two zeros and poles), well within a
//! biquad's expressive range. Today the placeholder table uses first- or
//! second-order FIR biquads (all `a` coefficients zero; `PROFILES[1..=5]`
//! are 2-tap / first-order, `PROFILES[6..=9]` are 3-tap / second-order),
//! but the structure is ready for IIR poles when the spandsp values are
//! imported.
//!
//! ## Coefficient source
//!
//! ITU-T V.34 Table 4 is paywalled. A comparison reference is
//! spandsp's `src/v34tx.c` and its pre-emphasis filters
//! (Steve Underwood, LGPL-2.1; see
//! `https://github.com/freeswitch/spandsp`).
//!
//! **TODO (Phase 1a.3 follow-up):** the coefficients below are
//! placeholder high-shelf biquads designed for monotonic HF boost at
//! 16 kHz; they have NOT yet been cross-checked against spandsp's
//! `v34_pre_emphasis[]` table. They produce the correct qualitative
//! behaviour (flat at DC, boosted above ~1 kHz, monotonically stronger
//! with index) but the magnitude response is not guaranteed to match
//! V.34 Table 4 within spec tolerance. This is accepted risk for Phase
//! 1a.3; the final coefficients must be filled in before Phase 1c
//! integration testing against real hardware. SoftModem is the
//! secondary reference if spandsp values look suspicious.

/// V.34 wire sample rate (Hz). Pre-emphasis coefficients are designed
/// at this rate. Pinned to the canonical `wire::WIRE_SAMPLE_RATE` so
/// that the two cannot drift silently.
pub const SAMPLE_RATE: u32 = crate::wire::WIRE_SAMPLE_RATE;

/// Number of defined V.34 pre-emphasis profiles (indices 0..=9).
pub const NUM_PROFILES: usize = 10;

/// Flip to `true` in the spandsp-import PR that replaces the
/// placeholder `PROFILES` values with verified V.34 Table 4
/// coefficients. While `false`, constructing a non-flat filter emits
/// a one-shot `tracing::warn!`. Belt-and-braces alongside the
/// `debug_assert!` in `new` and the ignored `test_matches_spandsp_table`.
const COEFFS_VERIFIED: bool = false;

/// One-shot guard so the `COEFFS_VERIFIED` warning fires only once per
/// process regardless of how many filters get constructed.
static COEFFS_WARN_ONCE: std::sync::Once = std::sync::Once::new();

/// One biquad profile. `b` = feed-forward (numerator), `a` = feedback
/// (denominator). `a[0]` is implicitly 1.0 — only `a[1]` and `a[2]` are
/// stored, so the array is `[a1, a2]`.
#[derive(Debug, Clone, Copy)]
struct BiquadProfile {
    b: [f64; 3],
    a: [f64; 2], // a1, a2 (a0 normalised to 1.0)
}

/// V.34 pre-emphasis profiles, indexed 0..=9.
///
/// Index 0 is the identity filter (flat). Indices 1..=9 are FIR high-boost
/// biquads (all `a` coefficients zero) with progressively more aggressive
/// high-frequency boost.
///
/// Each profile is a first-order FIR of the form
/// `H(z) = 1 - α z^-1 + β z^-2` where α ≥ 0 increases with index to
/// give monotonic HF boost across the 0–4 kHz voiceband, and β provides
/// a small second-tap correction for the higher-boost indices. `b[0]`
/// is fixed at 1.0 so the first sample through a freshly reset filter
/// equals the input (a property the state-machine reset test relies on).
///
/// See module-level TODO — these are placeholder coefficients awaiting
/// cross-check against spandsp's `v34_pre_emphasis[]` table. They
/// produce the correct qualitative response (flat at DC-ish, boosted at
/// HF, monotonic with index, stable, short impulse tail) but exact dB
/// values are not verified against V.34 Table 4.
const PROFILES: [BiquadProfile; NUM_PROFILES] = [
    // Index 0: flat (identity).
    BiquadProfile {
        b: [1.0, 0.0, 0.0],
        a: [0.0, 0.0],
    },
    // Indices 1..=5: pure two-tap FIR with α ramping from 0.05 → 0.25.
    BiquadProfile {
        b: [1.0, -0.05, 0.0],
        a: [0.0, 0.0],
    },
    BiquadProfile {
        b: [1.0, -0.10, 0.0],
        a: [0.0, 0.0],
    },
    BiquadProfile {
        b: [1.0, -0.15, 0.0],
        a: [0.0, 0.0],
    },
    BiquadProfile {
        b: [1.0, -0.20, 0.0],
        a: [0.0, 0.0],
    },
    BiquadProfile {
        b: [1.0, -0.25, 0.0],
        a: [0.0, 0.0],
    },
    // Indices 6..=9: three-tap FIR with a small b2 correction so the HF
    // boost continues to rise without the two-tap response flattening.
    BiquadProfile {
        b: [1.0, -0.30, 0.005],
        a: [0.0, 0.0],
    },
    BiquadProfile {
        b: [1.0, -0.35, 0.010],
        a: [0.0, 0.0],
    },
    BiquadProfile {
        b: [1.0, -0.40, 0.015],
        a: [0.0, 0.0],
    },
    BiquadProfile {
        b: [1.0, -0.45, 0.020],
        a: [0.0, 0.0],
    },
];

/// V.34 TX pre-emphasis filter.
///
/// Stateful single-biquad filter in transposed direct form II.
/// Construct with the negotiated V.34 pre-emphasis index (0..=9),
/// feed PCM samples one at a time through `process`, and `reset`
/// at call teardown or before reusing the filter on a new session.
///
/// # Invariants
///
/// Every profile in `PROFILES` is normalised so `b[0] == 1.0`. This
/// guarantees that the first sample through a freshly constructed or
/// freshly `reset`-ed filter equals the input sample (because the
/// delay line is zero, `y = b[0] * x + 0 = x`). The downstream reset
/// test in `state_machine.rs` (see
/// `test_transition_to_online_v34_uses_negotiated_preemphasis_index`)
/// relies on this property: it calls `process(1.0)` after a reset and
/// asserts the output is `1.0 ± 1e-12`. The invariant is enforced at
/// filter construction via a `debug_assert_eq!` in `new`, so a profile
/// table with a non-unit `b[0]` fails fast here rather than via that
/// distant test.
pub struct PreEmphasisFilter {
    profile: BiquadProfile,
    /// Transposed direct form II delay states [s1, s2].
    state: [f64; 2],
    filter_index: u8,
}

impl PreEmphasisFilter {
    /// Create a filter for the given V.34 pre-emphasis index.
    ///
    /// Index 0 is flat (identity). Indices 1..=9 apply increasing
    /// amounts of high-frequency boost per V.34 Table 4.
    ///
    /// # Panics
    ///
    /// Panics if `index > 9`.
    pub fn new(index: u8) -> Self {
        assert!(
            (index as usize) < NUM_PROFILES,
            "V.34 pre-emphasis index must be 0-9, got {index}"
        );
        let profile = PROFILES[index as usize];
        debug_assert_eq!(
            profile.b[0], 1.0,
            "pre-emphasis profile {index} must have b[0] == 1.0 (assumed by state_machine reset test)",
        );
        if !COEFFS_VERIFIED && index != 0 {
            COEFFS_WARN_ONCE.call_once(|| {
                tracing::warn!(
                    target: "gp64::modem::preemphasis",
                    "V.34 pre-emphasis profile {index} in use; PROFILES[1..=9] are \
                     placeholder coefficients not yet cross-checked against spandsp \
                     V.34 Table 4 (see test_matches_spandsp_table)",
                );
            });
        }
        Self {
            profile,
            state: [0.0; 2],
            filter_index: index,
        }
    }

    /// Filter one 16 kHz sample through the biquad.
    ///
    /// Transposed direct form II — numerically well-behaved for audio
    /// biquads and keeps the delay line to two elements regardless of
    /// the filter order.
    #[inline]
    pub fn process(&mut self, sample: f64) -> f64 {
        let BiquadProfile { b, a } = self.profile;
        let [s1, s2] = self.state;

        let y = b[0] * sample + s1;
        let new_s1 = b[1] * sample - a[0] * y + s2;
        let new_s2 = b[2] * sample - a[1] * y;

        self.state = [new_s1, new_s2];
        y
    }

    /// Reset the filter state (clears the delay line).
    ///
    /// Call between sessions. Does not change the selected profile;
    /// construct a new filter if the negotiated index changes.
    pub fn reset(&mut self) {
        self.state = [0.0; 2];
    }

    /// The V.34 pre-emphasis index this filter was constructed with.
    pub fn index(&self) -> u8 {
        self.filter_index
    }

    /// Test-only constructor accepting an arbitrary biquad profile.
    ///
    /// Used to exercise the IIR (non-zero `a[]`) code path in unit
    /// tests, since the shipping `PROFILES` table is currently
    /// FIR-only. Not exposed on the public API — production code must
    /// go through `new(index)` so a defined V.34 profile is used.
    #[cfg(test)]
    fn from_profile(profile: BiquadProfile) -> Self {
        Self {
            profile,
            state: [0.0; 2],
            filter_index: u8::MAX,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    /// Evaluate the biquad's complex frequency response H(e^{jω}) at
    /// the given frequency (Hz). Used by tests for analytical
    /// magnitude-response checks without the transient artifacts of
    /// feeding a tone through the filter.
    fn freq_response(profile: &BiquadProfile, freq_hz: f64) -> (f64, f64) {
        // ω = 2π * f / fs
        let w = 2.0 * PI * freq_hz / SAMPLE_RATE as f64;
        // Numerator: b0 + b1*e^{-jω} + b2*e^{-j2ω}
        let num_re = profile.b[0] + profile.b[1] * (-w).cos() + profile.b[2] * (-2.0 * w).cos();
        let num_im = profile.b[1] * (-w).sin() + profile.b[2] * (-2.0 * w).sin();
        // Denominator: 1 + a1*e^{-jω} + a2*e^{-j2ω}
        let den_re = 1.0 + profile.a[0] * (-w).cos() + profile.a[1] * (-2.0 * w).cos();
        let den_im = profile.a[0] * (-w).sin() + profile.a[1] * (-2.0 * w).sin();
        // H = num / den (complex division)
        let den_mag2 = den_re * den_re + den_im * den_im;
        let re = (num_re * den_re + num_im * den_im) / den_mag2;
        let im = (num_im * den_re - num_re * den_im) / den_mag2;
        (re, im)
    }

    fn magnitude(profile: &BiquadProfile, freq_hz: f64) -> f64 {
        let (re, im) = freq_response(profile, freq_hz);
        (re * re + im * im).sqrt()
    }

    /// Index 0 must be a pure passthrough: feed an impulse, a tone, and
    /// arbitrary samples — output must equal input to within fp epsilon.
    #[test]
    fn test_index_0_is_flat() {
        let mut f = PreEmphasisFilter::new(0);

        // Test 1: impulse
        assert!((f.process(1.0) - 1.0).abs() < 1e-15);
        for _ in 0..32 {
            assert!(f.process(0.0).abs() < 1e-15);
        }

        // Test 2: tone + arbitrary waveform
        f.reset();
        let input = [1.0, -0.5, 0.3, 0.0, -1.0, 0.7, 0.123, -0.987];
        for &x in &input {
            let y = f.process(x);
            assert!(
                (y - x).abs() < 1e-15,
                "Index 0 must be passthrough: input={x}, output={y}"
            );
        }

        // Test 3: analytical — magnitude response is exactly 1 everywhere
        for &freq in &[0.0, 500.0, 1000.0, 2000.0, 4000.0, 7000.0] {
            let mag = magnitude(&PROFILES[0], freq);
            assert!(
                (mag - 1.0).abs() < 1e-15,
                "Index 0 |H({freq})| = {mag}, expected 1.0"
            );
        }
    }

    /// Check spec-level properties of the magnitude response across the
    /// voiceband. Until the coefficients are cross-checked against
    /// spandsp we assert the qualitative shape rather than exact dB
    /// values:
    ///
    ///   - magnitude finite and positive across 0–4 kHz for all indices
    ///   - HF boost at 3 kHz (relative to 1 kHz) is monotonically
    ///     non-decreasing with index
    ///   - Index 0 is exactly flat everywhere
    ///
    /// See module-level TODO: this test should tighten to the spandsp
    /// reference values with a 0.5 dB tolerance once those are imported.
    #[test]
    fn test_magnitude_response_matches_spec() {
        let probe_freqs = [500.0, 1000.0, 1500.0, 2000.0, 2500.0, 3000.0, 3400.0];

        // Index 0: exactly unity magnitude at every probe frequency.
        for &f in &probe_freqs {
            let m = magnitude(&PROFILES[0], f);
            assert!((m - 1.0).abs() < 1e-15, "Index 0 @ {f} Hz: |H| = {m}");
        }

        // Finite and positive response across the whole voiceband.
        for idx in 0..NUM_PROFILES {
            for &f in &probe_freqs {
                let m = magnitude(&PROFILES[idx], f);
                assert!(
                    m.is_finite() && m > 0.0,
                    "Index {idx} @ {f} Hz: non-finite magnitude {m}"
                );
            }
        }

        // HF boost (|H(3k)|/|H(1k)|) must rise STRICTLY monotonically
        // with index. Strict `>` (not `>=`) means two profiles with
        // identical HF boost — e.g. if someone copy-pastes a row and
        // forgets to edit — fail this test immediately.
        let mut prev_ratio_db = f64::NEG_INFINITY;
        for idx in 0..NUM_PROFILES {
            let mag_1k = magnitude(&PROFILES[idx], 1000.0);
            let mag_3k = magnitude(&PROFILES[idx], 3000.0);
            let ratio_db = 20.0 * (mag_3k / mag_1k).log10();
            assert!(
                ratio_db > prev_ratio_db,
                "Index {idx}: HF/LF ratio {ratio_db:.3} dB not > index {} ({prev_ratio_db:.3} dB)",
                idx.saturating_sub(1)
            );
            prev_ratio_db = ratio_db;
        }
    }

    /// All profiles in the `PROFILES` table must have distinct
    /// coefficients. Catches accidental copy-paste duplicates.
    #[test]
    fn test_profiles_are_distinct() {
        for i in 0..NUM_PROFILES {
            for j in (i + 1)..NUM_PROFILES {
                assert!(
                    PROFILES[i].b != PROFILES[j].b || PROFILES[i].a != PROFILES[j].a,
                    "Profiles {i} and {j} are identical: b={:?} a={:?}",
                    PROFILES[i].b,
                    PROFILES[i].a
                );
            }
        }
    }

    /// Feeding a signal, then resetting, then feeding the same signal
    /// must produce bitwise-identical output. Catches state leakage.
    #[test]
    fn test_reset_clears_state() {
        for idx in 0..=9u8 {
            let mut f = PreEmphasisFilter::new(idx);

            // Prime the filter.
            let priming: Vec<f64> = (0..32).map(|i| (i as f64 * 0.37).sin()).collect();
            for &x in &priming {
                f.process(x);
            }
            f.reset();
            let first_run: Vec<f64> = priming.iter().map(|&x| f.process(x)).collect();

            // Reset and run again — outputs must match exactly.
            f.reset();
            let second_run: Vec<f64> = priming.iter().map(|&x| f.process(x)).collect();

            for (i, (a, b)) in first_run.iter().zip(second_run.iter()).enumerate() {
                assert_eq!(
                    a, b,
                    "Index {idx}, sample {i}: reset did not restore state ({a} vs {b})"
                );
            }
        }
    }

    /// The impulse response must decay: a V.34 pre-emphasis filter
    /// should not ring for more than a few ms. 64 samples at 16 kHz = 4 ms.
    /// This rules out unstable poles (|root| ≥ 1) in the placeholder
    /// coefficients. Protects the IIR code path that becomes live when
    /// spandsp coefficients introduce non-zero `a[]`.
    #[test]
    fn test_impulse_response_length_bounded() {
        const N: usize = 64;
        const THRESHOLD: f64 = 0.001; // 0.1% of peak

        for idx in 0..=9u8 {
            let mut f = PreEmphasisFilter::new(idx);
            let mut ir = [0.0; N];
            ir[0] = f.process(1.0);
            for i in 1..N {
                ir[i] = f.process(0.0);
            }

            let peak = ir.iter().fold(0.0f64, |a, &b| a.max(b.abs()));
            assert!(peak > 0.0, "Index {idx}: zero impulse response");

            // Index 0 is a pure impulse — after sample 0 everything is zero.
            // For indices 1..=9 the tail must decay below THRESHOLD * peak
            // by the end of the window.
            let tail_peak = ir[N - 8..].iter().fold(0.0f64, |a, &b| a.max(b.abs()));
            assert!(
                tail_peak < THRESHOLD * peak,
                "Index {idx}: impulse response tail {tail_peak:.6} ≥ {:.6} (threshold = 0.1% of peak {peak:.6}) — filter may be unstable",
                THRESHOLD * peak
            );
        }
    }

    /// Pre-emphasis must boost high frequencies relative to low. If
    /// coefficient signs are swapped we would see attenuation instead,
    /// so this guards against that class of bug.
    #[test]
    fn test_monotonic_hf_gain() {
        for idx in 1..=9u8 {
            let profile = &PROFILES[idx as usize];
            let mag_1k = magnitude(profile, 1000.0);
            let mag_3k = magnitude(profile, 3000.0);
            assert!(
                mag_3k >= mag_1k,
                "Index {idx}: |H(3 kHz)| = {mag_3k:.4} < |H(1 kHz)| = {mag_1k:.4} — coefficient sign inverted?"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Additional tests retained from the pre-biquad stub
    // -----------------------------------------------------------------------

    /// All ten indices must construct without panic and report their index.
    #[test]
    fn test_all_indices_valid() {
        for idx in 0..=9u8 {
            let f = PreEmphasisFilter::new(idx);
            assert_eq!(f.index(), idx);
        }
    }

    /// Index 10 must panic.
    #[test]
    #[should_panic(expected = "V.34 pre-emphasis index must be 0-9")]
    fn test_index_out_of_range_panics() {
        let _ = PreEmphasisFilter::new(10);
    }

    // -----------------------------------------------------------------------
    // Forcing-function test: fails by design until spandsp coefficients land
    // -----------------------------------------------------------------------

    #[test]
    #[ignore = "Enable when spandsp V.34 Table 4 coefficients are imported. Remove #[ignore] and replace body with actual value assertions."]
    fn test_matches_spandsp_table() {
        // Forcing-function checklist item: this test exists to remind us
        // that the coefficients in `PROFILES[1..=9]` are placeholder
        // values and must be replaced with spandsp's V.34 pre-emphasis
        // table before Stage 2 (real-hardware interop).
        //
        // When importing, replace this body with per-index assertions
        // that each profile's coefficients match spandsp
        // `v34_tx.c v34_pre_emphasis[]` within quantisation tolerance.
        eprintln!(
            "pending external oracle: import spandsp V.34 Table 4 coefficients \
             and add per-index checks"
        );
    }

    // -----------------------------------------------------------------------
    // IIR code-path tests: use a synthetic stable pole profile to exercise
    // the `a[]` feedback paths that the shipping FIR-only `PROFILES` do
    // not reach. Catches sign bugs in the transposed-DF2 feedback terms
    // before real IIR coefficients land.
    // -----------------------------------------------------------------------

    /// A first-order lowpass `H(z) = (0.5 + 0.5 z^-1) / (1 - 0.3 z^-1)`
    /// expressed as a biquad. Pole at z = 0.3 (inside unit circle →
    /// stable); DC gain = (0.5 + 0.5) / (1 - 0.3) = 1.0 / 0.7 ≈ 1.4286.
    ///
    /// Note: our storage convention is `a = [a1, a2]` where the full
    /// denominator is `1 + a1 z^-1 + a2 z^-2`, so a pole at z = +0.3
    /// means a1 = -0.3.
    const IIR_TEST_PROFILE: BiquadProfile = BiquadProfile {
        b: [0.5, 0.5, 0.0],
        a: [-0.3, 0.0],
    };

    /// IIR test profile: impulse response must decay (stability check).
    /// With a pole at z = 0.3, the tail decays as 0.3^n which falls
    /// below 1e-5 by sample ~10, easily within the 64-sample window.
    #[test]
    fn test_iir_profile_impulse_response_bounded() {
        const N: usize = 64;
        const THRESHOLD: f64 = 0.001;

        let mut f = PreEmphasisFilter::from_profile(IIR_TEST_PROFILE);
        let mut ir = [0.0; N];
        ir[0] = f.process(1.0);
        for i in 1..N {
            ir[i] = f.process(0.0);
        }

        let peak = ir.iter().fold(0.0f64, |a, &b| a.max(b.abs()));
        assert!(peak > 0.0, "IIR test profile: zero impulse response");

        let tail_peak = ir[N - 8..].iter().fold(0.0f64, |a, &b| a.max(b.abs()));
        assert!(
            tail_peak < THRESHOLD * peak,
            "IIR test profile: impulse response tail {tail_peak:.6} ≥ {:.6} — \
             feedback path may have a sign bug",
            THRESHOLD * peak
        );

        // Additional sanity check: analytical DC gain.
        // H(1) = (b0 + b1 + b2) / (1 + a1 + a2) = 1.0 / 0.7
        let dc_gain = magnitude(&IIR_TEST_PROFILE, 0.0);
        let expected = 1.0 / 0.7;
        assert!(
            (dc_gain - expected).abs() < 1e-12,
            "IIR test profile DC gain {dc_gain} != expected {expected}"
        );
    }

    /// IIR test profile: reset must bring the filter back to a clean
    /// initial state, same as for FIR profiles.
    #[test]
    fn test_iir_profile_reset_clears_state() {
        let mut f = PreEmphasisFilter::from_profile(IIR_TEST_PROFILE);

        let priming: Vec<f64> = (0..32).map(|i| (i as f64 * 0.37).sin()).collect();
        for &x in &priming {
            f.process(x);
        }
        f.reset();
        let first_run: Vec<f64> = priming.iter().map(|&x| f.process(x)).collect();

        f.reset();
        let second_run: Vec<f64> = priming.iter().map(|&x| f.process(x)).collect();

        for (i, (a, b)) in first_run.iter().zip(second_run.iter()).enumerate() {
            assert_eq!(
                a, b,
                "IIR test profile, sample {i}: reset did not restore state ({a} vs {b})"
            );
        }
    }
}
