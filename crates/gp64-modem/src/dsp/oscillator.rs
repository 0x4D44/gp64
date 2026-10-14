//! Phase-accumulator oscillator for tone and carrier generation.

use std::f64::consts::TAU;

/// Sine/cosine oscillator with sub-sample phase accuracy.
pub struct Oscillator {
    /// Current phase in radians [0, 2*pi).
    phase: f64,
    /// Phase increment per sample.
    phase_inc: f64,
}

impl Oscillator {
    /// Create an oscillator at the given frequency and sample rate.
    ///
    /// Non-finite `frequency` is rejected: `debug_assert` fires in debug
    /// builds; in release the oscillator is constructed at 0 Hz so the
    /// rest of the DSP chain stays finite. See `set_phase` for the
    /// NaN-poison rationale that motivates these guards.
    pub fn new(frequency: f64, sample_rate: u32) -> Self {
        debug_assert!(
            frequency.is_finite(),
            "Oscillator::new: non-finite frequency {frequency}"
        );
        let freq = if frequency.is_finite() {
            frequency
        } else {
            0.0
        };
        Self {
            phase: 0.0,
            phase_inc: TAU * freq / sample_rate as f64,
        }
    }

    /// Create an oscillator at the given frequency with an initial phase offset (radians).
    ///
    /// Non-finite `frequency` or `initial_phase` are rejected
    /// (`debug_assert` in debug, replaced with `0.0` in release) —
    /// see `set_phase` for the NaN-poison rationale.
    pub fn with_phase(frequency: f64, sample_rate: u32, initial_phase: f64) -> Self {
        debug_assert!(
            frequency.is_finite(),
            "Oscillator::with_phase: non-finite frequency {frequency}"
        );
        debug_assert!(
            initial_phase.is_finite(),
            "Oscillator::with_phase: non-finite initial_phase {initial_phase}"
        );
        let freq = if frequency.is_finite() {
            frequency
        } else {
            0.0
        };
        let phase = if initial_phase.is_finite() {
            initial_phase
        } else {
            0.0
        };
        Self {
            phase,
            phase_inc: TAU * freq / sample_rate as f64,
        }
    }

    /// Set the frequency (updates phase increment, preserves phase continuity).
    ///
    /// Non-finite `frequency` is silently ignored in release builds
    /// (`debug_assert` in debug). A NaN/Inf phase increment would poison
    /// `Oscillator::phase` permanently because the wrap step in `advance`
    /// (`if self.phase >= TAU { ... }`) is a no-op for NaN — see
    /// `set_phase`.
    pub fn set_frequency(&mut self, frequency: f64, sample_rate: u32) {
        debug_assert!(
            frequency.is_finite(),
            "Oscillator::set_frequency: non-finite frequency {frequency}"
        );
        if !frequency.is_finite() {
            return;
        }
        self.phase_inc = TAU * frequency / sample_rate as f64;
    }

    /// Generate the next sample (returns sine value).
    pub fn next_sample(&mut self) -> f64 {
        let value = self.phase.sin();
        self.advance();
        value
    }

    /// Generate the next sample as a complex pair (cos, sin) for QAM.
    pub fn next_iq(&mut self) -> (f64, f64) {
        let i = self.phase.cos();
        let q = self.phase.sin();
        self.advance();
        (i, q)
    }

    /// Current phase in radians.
    pub fn phase(&self) -> f64 {
        self.phase
    }

    /// Set the phase to a specific value (in radians).
    ///
    /// **NaN-poison invariant.** Real V.32bis hardware has no concept of
    /// NaN: carrier phase is an analog quantity, always finite, modulo
    /// 2π. A NaN phase here is unrecoverable in this oscillator design
    /// because the wrap step in `advance` (`if self.phase >= TAU { ... }`)
    /// is a no-op for NaN (any comparison with NaN is false), and every
    /// downstream `cos`/`sin` of NaN is also NaN. Once poisoned, every
    /// subsequent `next_iq` would emit `(NaN, NaN)`. The same applies to
    /// ±Inf because `cos(±Inf) = sin(±Inf) = NaN` per IEEE 754.
    ///
    /// We therefore reject non-finite input at this boundary: the
    /// `debug_assert` makes the violation loud in debug/test builds (the
    /// fuzzer surfaces it immediately); release builds silently keep the
    /// previous phase so a corrupted upstream caller cannot break the
    /// modem session.
    pub fn set_phase(&mut self, phase: f64) {
        debug_assert!(
            phase.is_finite(),
            "Oscillator::set_phase: non-finite phase {phase}"
        );
        if !phase.is_finite() {
            return;
        }
        self.phase = phase;
    }

    /// Reset phase to zero.
    pub fn reset(&mut self) {
        self.phase = 0.0;
    }

    /// Advance phase by one sample and wrap to [0, 2*pi).
    fn advance(&mut self) {
        self.phase += self.phase_inc;
        if self.phase >= TAU {
            self.phase -= TAU;
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// Count zero crossings (positive-going) in a buffer of samples.
    fn count_positive_zero_crossings(samples: &[f64]) -> usize {
        let mut count = 0;
        for i in 1..samples.len() {
            if samples[i - 1] < 0.0 && samples[i] >= 0.0 {
                count += 1;
            }
        }
        count
    }

    #[test]
    fn test_oscillator_frequency() {
        // 1000 Hz at 8000 Hz sample rate → 1000 cycles per second.
        // In 1000 samples (0.125 seconds) we expect 125 positive zero crossings.
        let mut osc = Oscillator::new(1000.0, 8000);
        let samples: Vec<f64> = (0..1000).map(|_| osc.next_sample()).collect();
        let crossings = count_positive_zero_crossings(&samples);
        // Allow ±1 for edge effects
        assert!(
            (124..=126).contains(&crossings),
            "Expected ~125 zero crossings for 1000 Hz @ 8000 Hz over 1000 samples, got {crossings}"
        );
    }

    #[test]
    fn test_oscillator_phase_continuity() {
        let mut osc = Oscillator::new(1000.0, 8000);

        // Generate some samples at 1000 Hz
        for _ in 0..100 {
            osc.next_sample();
        }
        let phase_before = osc.phase();
        let sample_before = phase_before.sin();

        // Change frequency — phase should not jump
        osc.set_frequency(2000.0, 8000);
        let phase_after = osc.phase();
        let sample_after = phase_after.sin();

        assert!(
            (phase_before - phase_after).abs() < 1e-10,
            "Phase should not change on frequency update: before={phase_before}, after={phase_after}"
        );
        assert!(
            (sample_before - sample_after).abs() < 1e-10,
            "Signal value should be continuous across frequency change"
        );
    }

    #[test]
    fn test_oscillator_iq_quadrature() {
        // cos and sin should be 90 degrees apart: cos^2 + sin^2 = 1
        let mut osc = Oscillator::new(1800.0, 8000);
        for _ in 0..500 {
            let (i, q) = osc.next_iq();
            let magnitude_sq = i * i + q * q;
            assert!(
                (magnitude_sq - 1.0).abs() < 1e-10,
                "IQ magnitude should be 1.0, got {magnitude_sq} (i={i}, q={q})"
            );
        }
    }

    #[test]
    fn test_oscillator_reset() {
        let mut osc = Oscillator::new(1000.0, 8000);

        // Advance phase
        for _ in 0..50 {
            osc.next_sample();
        }
        assert!(osc.phase() > 0.0, "Phase should have advanced");

        // Reset
        osc.reset();
        assert_eq!(osc.phase(), 0.0, "Phase should be zero after reset");

        // First sample after reset should be sin(0) = 0
        let sample = osc.next_sample();
        assert!(
            sample.abs() < 1e-10,
            "First sample after reset should be ~0.0, got {sample}"
        );
    }

    #[test]
    fn test_oscillator_dc_at_zero_hz() {
        let mut osc = Oscillator::new(0.0, 8000);
        for _ in 0..100 {
            let sample = osc.next_sample();
            assert!(
                sample.abs() < 1e-10,
                "0 Hz oscillator sin should always be 0.0, got {sample}"
            );
        }

        // IQ at 0 Hz: cos(0)=1, sin(0)=0
        let mut osc2 = Oscillator::new(0.0, 8000);
        for _ in 0..100 {
            let (i, q) = osc2.next_iq();
            assert!(
                (i - 1.0).abs() < 1e-10,
                "0 Hz cos should always be 1.0, got {i}"
            );
            assert!(q.abs() < 1e-10, "0 Hz sin should always be 0.0, got {q}");
        }
    }

    #[test]
    fn test_oscillator_set_phase() {
        let mut osc = Oscillator::new(1000.0, 8000);
        osc.set_phase(std::f64::consts::FRAC_PI_2);
        // sin(pi/2) = 1.0
        let sample = osc.next_sample();
        assert!(
            (sample - 1.0).abs() < 1e-10,
            "sin(pi/2) should be 1.0, got {sample}"
        );
    }

    #[test]
    fn test_oscillator_with_phase() {
        // with_phase at pi/2 should produce the same first sample as set_phase
        let mut osc = Oscillator::with_phase(1000.0, 8000, std::f64::consts::FRAC_PI_2);
        let sample = osc.next_sample();
        assert!(
            (sample - 1.0).abs() < 1e-10,
            "with_phase(pi/2) first sample should be sin(pi/2) = 1.0, got {sample}"
        );
    }

    #[test]
    fn test_oscillator_nyquist() {
        // At Nyquist frequency (half sample rate), each sample alternates
        // between sin(0), sin(pi), sin(2pi), sin(3pi) = 0, 0, 0, 0
        // because sin(n*pi) = 0 for integer n.
        let mut osc = Oscillator::new(4000.0, 8000);
        for _ in 0..10 {
            let sample = osc.next_sample();
            assert!(
                sample.abs() < 1e-10,
                "Nyquist frequency sin samples should be ~0, got {sample}"
            );
        }
    }

    #[test]
    fn test_oscillator_half_sample_rate() {
        // 2000 Hz at 8000 Hz sample rate = 4 samples per cycle.
        // sin(0), sin(pi/2), sin(pi), sin(3pi/2) = 0, 1, 0, -1
        let mut osc = Oscillator::new(2000.0, 8000);
        let s0 = osc.next_sample();
        let s1 = osc.next_sample();
        let s2 = osc.next_sample();
        let s3 = osc.next_sample();

        assert!(s0.abs() < 1e-10, "Sample 0 should be ~0, got {s0}");
        assert!((s1 - 1.0).abs() < 1e-10, "Sample 1 should be ~1, got {s1}");
        assert!(s2.abs() < 1e-10, "Sample 2 should be ~0, got {s2}");
        assert!((s3 + 1.0).abs() < 1e-10, "Sample 3 should be ~-1, got {s3}");
    }

    #[test]
    fn test_oscillator_amplitude_bounded() {
        // All samples should be in [-1, 1]
        let mut osc = Oscillator::new(1234.5, 8000);
        for i in 0..10000 {
            let sample = osc.next_sample();
            assert!(
                sample >= -1.0 && sample <= 1.0,
                "Sample {i} out of bounds: {sample}"
            );
        }
    }

    /// Helper for the NaN-poison guards: build a release-mode oscillator
    /// (so `debug_assert` does not fire), poison `set_phase` with `bad`,
    /// pull 100 IQ pairs and assert every one is finite. The release-time
    /// silent-skip behaviour means the poisoned call is a no-op and the
    /// oscillator continues from its prior phase.
    #[cfg(not(debug_assertions))]
    fn assert_set_phase_rejected(bad: f64) {
        // Drive the oscillator forward once so the prior phase is non-zero.
        let mut osc = Oscillator::new(1800.0, 8000);
        let _ = osc.next_iq();
        let phase_before = osc.phase();

        osc.set_phase(bad);
        // Silent skip in release: phase must be unchanged.
        assert_eq!(
            osc.phase(),
            phase_before,
            "set_phase({bad}) must be a no-op in release"
        );

        // 100 IQ pairs after the poisoned set_phase must all be finite.
        for k in 0..100 {
            let (i, q) = osc.next_iq();
            assert!(
                i.is_finite(),
                "IQ[{k}].i non-finite after set_phase({bad}): {i}"
            );
            assert!(
                q.is_finite(),
                "IQ[{k}].q non-finite after set_phase({bad}): {q}"
            );
        }
    }

    /// `set_phase(NaN)` and `set_phase(±Inf)` must not poison the
    /// oscillator. Run only in release mode where `debug_assert` is
    /// disabled; debug builds correctly panic at the `debug_assert`.
    #[test]
    #[cfg(not(debug_assertions))]
    fn test_set_phase_rejects_non_finite() {
        assert_set_phase_rejected(f64::NAN);
        assert_set_phase_rejected(f64::INFINITY);
        assert_set_phase_rejected(f64::NEG_INFINITY);
    }

    /// `set_frequency` with non-finite input must not poison `phase_inc`.
    #[test]
    #[cfg(not(debug_assertions))]
    fn test_set_frequency_rejects_non_finite() {
        let mut osc = Oscillator::new(1800.0, 8000);
        // Finite output before
        for _ in 0..10 {
            let (i, q) = osc.next_iq();
            assert!(i.is_finite() && q.is_finite());
        }
        osc.set_frequency(f64::NAN, 8000);
        osc.set_frequency(f64::INFINITY, 8000);
        // Finite output after
        for k in 0..100 {
            let (i, q) = osc.next_iq();
            assert!(
                i.is_finite() && q.is_finite(),
                "IQ[{k}] non-finite after non-finite set_frequency"
            );
        }
    }

    /// `with_phase` constructed with non-finite frequency or initial_phase
    /// must yield an oscillator that emits only finite samples.
    #[test]
    #[cfg(not(debug_assertions))]
    fn test_with_phase_rejects_non_finite() {
        let osc1 = Oscillator::with_phase(f64::NAN, 8000, 0.0);
        let osc2 = Oscillator::with_phase(1800.0, 8000, f64::NAN);
        let osc3 = Oscillator::with_phase(f64::INFINITY, 8000, f64::NEG_INFINITY);
        for mut osc in [osc1, osc2, osc3] {
            for _ in 0..50 {
                let (i, q) = osc.next_iq();
                assert!(i.is_finite() && q.is_finite());
            }
        }
    }

    /// Debug-build counterpart: `set_phase(NaN)` panics via `debug_assert`,
    /// confirming the assertion is wired up. Without this, a release-only
    /// silent-skip change could regress the loud-in-debug behaviour
    /// without anyone noticing.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "non-finite phase")]
    fn test_set_phase_debug_assert_panics_on_nan() {
        let mut osc = Oscillator::new(1800.0, 8000);
        osc.set_phase(f64::NAN);
    }
}
