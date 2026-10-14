//! Automatic gain control for received signal normalization.

/// RMS-based automatic gain control with separate attack/decay rates.
///
/// Attack (level increasing) uses a fast alpha for quick response to
/// prevent clipping. Decay (level decreasing) uses a slow alpha to
/// avoid gain pumping on fading signals.
pub struct Agc {
    /// Desired output RMS level.
    target_rms: f64,
    /// Current gain factor.
    gain: f64,
    /// Fast smoothing factor for level increases (attack).
    attack_alpha: f64,
    /// Slow smoothing factor for level decreases (decay).
    decay_alpha: f64,
}

impl Agc {
    /// Create an AGC with separate attack and decay rates.
    ///
    /// `attack_alpha`: fast response to level increase (e.g. 0.01)
    /// `decay_alpha`: slow response to level decrease (e.g. 0.001)
    pub fn with_asymmetric(target_rms: f64, attack_alpha: f64, decay_alpha: f64) -> Self {
        Self {
            target_rms,
            gain: 1.0,
            attack_alpha,
            decay_alpha,
        }
    }

    /// Create an AGC with equal attack and decay rates (backward compatible).
    ///
    /// `alpha`: adaptation rate (0.01 = slow/stable, 0.1 = fast/responsive)
    pub fn new(target_rms: f64, alpha: f64) -> Self {
        Self::with_asymmetric(target_rms, alpha, alpha)
    }

    /// Process a block of samples in-place, adjusting gain.
    pub fn process(&mut self, samples: &mut [f64]) {
        if samples.is_empty() {
            return;
        }

        // Compute block RMS.
        let sum_sq: f64 = samples.iter().map(|&s| s * s).sum();
        let measured_rms = (sum_sq / samples.len() as f64).sqrt();

        // Update gain with exponential smoothing, but only if the signal
        // has measurable energy. For silence, hold the current gain to
        // avoid blowing it up toward infinity.
        if measured_rms > 1e-12 {
            let ideal_gain = self.target_rms / measured_rms;
            // Select alpha based on whether we need to reduce gain (attack —
            // signal got louder) or increase gain (decay — signal got quieter).
            let alpha = if measured_rms > self.target_rms {
                self.attack_alpha // fast: prevent clipping
            } else {
                self.decay_alpha // slow: avoid pumping
            };
            self.gain = self.gain * (1.0 - alpha) + ideal_gain * alpha;
        }

        // Apply gain to all samples.
        for s in samples.iter_mut() {
            *s *= self.gain;
        }
    }

    /// Current gain factor.
    pub fn gain(&self) -> f64 {
        self.gain
    }

    /// Set the gain factor directly (e.g., to carry a converged value
    /// from training into data mode).
    ///
    /// **Contract boundary.** `QamDemodulator::set_agc_gain` filters
    /// external public inputs before reaching this method; direct DSP
    /// users still rely on this lower-layer guard. A NaN/Inf gain would
    /// poison the AGC's exponential-smoothing state permanently —
    /// `gain * (1.0 - alpha) + ideal_gain * alpha` is NaN forever once
    /// either operand is NaN, and `*s *= self.gain` then propagates NaN
    /// into the sample stream. See the matching note on
    /// `Oscillator::set_phase`. `debug_assert` fires in debug; release
    /// silently keeps the previous gain.
    pub fn set_gain(&mut self, gain: f64) {
        debug_assert!(gain.is_finite(), "Agc::set_gain: non-finite gain {gain}");
        if !gain.is_finite() {
            return;
        }
        self.gain = gain;
    }

    /// Reset to unity gain.
    pub fn reset(&mut self) {
        self.gain = 1.0;
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    const TARGET_RMS: f64 = 1.0;
    const ALPHA: f64 = 0.1;

    /// Generate a constant-amplitude block with the given RMS level.
    fn constant_block(rms: f64, len: usize) -> Vec<f64> {
        // A constant value `v` has RMS = |v|.
        vec![rms; len]
    }

    #[test]
    fn test_agc_normalizes_loud() {
        // Input at 2x target → gain should reduce below 1.0.
        let mut agc = Agc::new(TARGET_RMS, ALPHA);
        let mut block = constant_block(2.0, 128);
        agc.process(&mut block);

        assert!(
            agc.gain() < 1.0,
            "Gain should decrease for loud input, got {}",
            agc.gain()
        );
    }

    #[test]
    fn test_agc_normalizes_quiet() {
        // Input at 0.5x target → gain should increase above 1.0.
        let mut agc = Agc::new(TARGET_RMS, ALPHA);
        let mut block = constant_block(0.5, 128);
        agc.process(&mut block);

        assert!(
            agc.gain() > 1.0,
            "Gain should increase for quiet input, got {}",
            agc.gain()
        );
    }

    #[test]
    fn test_agc_silence_stable() {
        // All-zero input should not cause infinite gain.
        let mut agc = Agc::new(TARGET_RMS, ALPHA);

        // First feed a real signal to move gain away from 1.0.
        let mut real_block = constant_block(0.5, 128);
        agc.process(&mut real_block);
        let gain_before_silence = agc.gain();

        // Now feed silence — gain should be held, not blown up.
        let mut silence = vec![0.0; 128];
        agc.process(&mut silence);

        assert!(
            agc.gain().is_finite(),
            "Gain should remain finite after silence, got {}",
            agc.gain()
        );
        assert!(
            (agc.gain() - gain_before_silence).abs() < 1e-10,
            "Gain should not change during silence: before={gain_before_silence}, after={}",
            agc.gain()
        );
    }

    #[test]
    fn test_agc_steady_state() {
        // After many blocks at a constant level, gain should stabilize
        // at target_rms / input_rms.
        let input_rms = 0.5;
        let expected_gain = TARGET_RMS / input_rms; // 2.0
        let mut agc = Agc::new(TARGET_RMS, ALPHA);

        for _ in 0..200 {
            let mut block = constant_block(input_rms, 128);
            agc.process(&mut block);
        }

        let diff = (agc.gain() - expected_gain).abs();
        assert!(
            diff < 0.01,
            "Gain should converge to {expected_gain}, got {} (diff={diff})",
            agc.gain()
        );
    }

    #[test]
    fn test_agc_reset() {
        let mut agc = Agc::new(TARGET_RMS, ALPHA);
        let mut block = constant_block(0.5, 128);
        agc.process(&mut block);
        assert_ne!(agc.gain(), 1.0);

        agc.reset();
        assert_eq!(agc.gain(), 1.0, "Gain should be 1.0 after reset");
    }

    #[test]
    fn test_agc_set_gain() {
        let mut agc = Agc::new(TARGET_RMS, ALPHA);
        agc.set_gain(3.5);
        assert_eq!(agc.gain(), 3.5, "set_gain should set the gain directly");
    }

    #[test]
    fn test_agc_empty_block() {
        let mut agc = Agc::new(TARGET_RMS, ALPHA);
        let mut empty: Vec<f64> = Vec::new();
        agc.process(&mut empty);
        assert_eq!(agc.gain(), 1.0, "Empty block should not change gain");
    }

    #[test]
    fn test_agc_single_sample() {
        let mut agc = Agc::new(TARGET_RMS, ALPHA);
        let mut block = vec![2.0];
        agc.process(&mut block);
        // Gain should move toward target_rms / input_rms = 1.0 / 2.0 = 0.5
        assert!(agc.gain() < 1.0, "Single loud sample should decrease gain");
    }

    #[test]
    fn test_agc_output_amplitude() {
        // After convergence, output RMS should be close to target
        let mut agc = Agc::new(TARGET_RMS, ALPHA);
        let input_rms = 3.0;

        // Converge
        for _ in 0..500 {
            let mut block = constant_block(input_rms, 128);
            agc.process(&mut block);
        }

        // Check that output matches target
        let mut block = constant_block(input_rms, 128);
        agc.process(&mut block);
        let output_rms: f64 =
            (block.iter().map(|s| s * s).sum::<f64>() / block.len() as f64).sqrt();
        assert!(
            (output_rms - TARGET_RMS).abs() < 0.05,
            "Output RMS should be ~{TARGET_RMS}, got {output_rms}"
        );
    }

    #[test]
    fn test_agc_attack_decay_asymmetry() {
        // Fast attack (0.1) should respond quicker to level increase than
        // slow decay (0.01) responds to level decrease.
        let attack = 0.1;
        let decay = 0.01;
        let mut agc = Agc::with_asymmetric(TARGET_RMS, attack, decay);

        // Phase 1: Converge at target level (gain ~1.0)
        for _ in 0..200 {
            let mut block = constant_block(1.0, 128);
            agc.process(&mut block);
        }
        let gain_before = agc.gain();

        // Phase 2: Sudden loud signal (3x target) — attack path
        let mut agc_attack = Agc::with_asymmetric(TARGET_RMS, attack, decay);
        agc_attack.set_gain(gain_before);
        let mut block = constant_block(3.0, 128);
        agc_attack.process(&mut block);
        let attack_change = (agc_attack.gain() - gain_before).abs();

        // Phase 3: Sudden quiet signal (0.3x target) — decay path
        let mut agc_decay = Agc::with_asymmetric(TARGET_RMS, attack, decay);
        agc_decay.set_gain(gain_before);
        let mut block = constant_block(0.3, 128);
        agc_decay.process(&mut block);
        let decay_change = (agc_decay.gain() - gain_before).abs();

        // Attack should produce a larger gain change than decay
        assert!(
            attack_change > decay_change,
            "Attack change ({attack_change:.6}) should be > decay change ({decay_change:.6})"
        );
    }

    #[test]
    fn test_agc_symmetric_backward_compat() {
        // Agc::new() with a single alpha should behave identically to
        // with_asymmetric() where attack==decay.
        let alpha = 0.05;
        let mut agc_new = Agc::new(TARGET_RMS, alpha);
        let mut agc_asym = Agc::with_asymmetric(TARGET_RMS, alpha, alpha);

        for _ in 0..100 {
            let mut block1 = constant_block(2.0, 64);
            let mut block2 = constant_block(2.0, 64);
            agc_new.process(&mut block1);
            agc_asym.process(&mut block2);
        }

        assert!(
            (agc_new.gain() - agc_asym.gain()).abs() < 1e-10,
            "Symmetric should match: new={}, asym={}",
            agc_new.gain(),
            agc_asym.gain()
        );
    }

    /// `set_gain(NaN)` and `set_gain(±Inf)` must not poison subsequent
    /// `process` output. Run only in release mode where `debug_assert`
    /// is disabled.
    #[test]
    #[cfg(not(debug_assertions))]
    fn test_set_gain_rejects_non_finite() {
        for &bad in &[f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut agc = Agc::new(TARGET_RMS, ALPHA);
            // Drive gain away from the default so we can tell the silent
            // skip preserved a non-default value.
            agc.set_gain(2.5);
            agc.set_gain(bad);
            assert_eq!(agc.gain(), 2.5, "set_gain({bad}) must be a no-op");
            // process() must produce only finite output.
            let mut block = vec![0.5_f64; 64];
            agc.process(&mut block);
            for (k, &s) in block.iter().enumerate() {
                assert!(
                    s.is_finite(),
                    "sample[{k}] non-finite after set_gain({bad}): {s}"
                );
            }
            assert!(agc.gain().is_finite(), "gain non-finite after process");
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "non-finite gain")]
    fn test_set_gain_debug_assert_panics_on_nan() {
        let mut agc = Agc::new(TARGET_RMS, ALPHA);
        agc.set_gain(f64::NAN);
    }
}
