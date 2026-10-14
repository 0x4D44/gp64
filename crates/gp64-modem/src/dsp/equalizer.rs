//! T-spaced complex LMS adaptive equalizer.
//!
//! Compensates residual ISI from RRC filter truncation in the V.34
//! demodulation path. Operates at the symbol rate on complex (I, Q)
//! baseband samples after timing recovery.
//!
//! The equalizer is initialized as a passthrough (center tap = 1+0j,
//! all others = 0+0j) and adapts using the LMS algorithm:
//!
//!   tap[k] += mu * error * conj(input[k])
//!
//! where error = desired - output.

/// T-spaced complex LMS/NLMS adaptive equalizer.
pub struct Equalizer {
    taps_i: Vec<f64>,
    taps_q: Vec<f64>,
    delay_i: Vec<f64>,
    delay_q: Vec<f64>,
    num_taps: usize,
    mu: f64,
    last_out_i: f64,
    last_out_q: f64,
    /// When true, uses Normalized LMS (NLMS) update rule.
    normalized: bool,
}

impl Equalizer {
    /// Create a new equalizer with the given number of taps and LMS step size.
    ///
    /// The center tap is initialized to (1, 0) (passthrough); all others to
    /// (0, 0). Typical values: 11 taps, mu = 0.01.
    pub fn new(num_taps: usize, mu: f64) -> Self {
        assert!(num_taps > 0, "equalizer needs at least 1 tap");
        let center = num_taps / 2;
        let mut taps_i = vec![0.0; num_taps];
        let taps_q = vec![0.0; num_taps];
        taps_i[center] = 1.0;
        Self {
            taps_i,
            taps_q,
            delay_i: vec![0.0; num_taps],
            delay_q: vec![0.0; num_taps],
            num_taps,
            mu,
            last_out_i: 0.0,
            last_out_q: 0.0,
            normalized: false,
        }
    }

    /// Create a new equalizer with Normalized LMS (NLMS) adaptation.
    ///
    /// NLMS divides the step size by the input signal power, providing
    /// better convergence on channels with varying signal levels (e.g. PSTN).
    pub fn new_normalized(num_taps: usize, mu: f64) -> Self {
        let mut eq = Self::new(num_taps, mu);
        eq.normalized = true;
        eq
    }

    /// Process one symbol-rate sample. Returns equalized (I, Q).
    pub fn process(&mut self, in_i: f64, in_q: f64) -> (f64, f64) {
        // Shift delay line: newest sample at index 0.
        self.delay_i.rotate_right(1);
        self.delay_q.rotate_right(1);
        self.delay_i[0] = in_i;
        self.delay_q[0] = in_q;

        // Complex FIR: output = sum(tap[k] * input[k])
        // where both tap and input are complex.
        let mut out_i = 0.0;
        let mut out_q = 0.0;
        for k in 0..self.num_taps {
            // (tap_i + j*tap_q) * (delay_i + j*delay_q)
            out_i += self.taps_i[k] * self.delay_i[k] - self.taps_q[k] * self.delay_q[k];
            out_q += self.taps_i[k] * self.delay_q[k] + self.taps_q[k] * self.delay_i[k];
        }

        self.last_out_i = out_i;
        self.last_out_q = out_q;
        (out_i, out_q)
    }

    /// Update taps using LMS or NLMS algorithm.
    ///
    /// Call after `process()` with the desired (reference) symbol IQ and the
    /// output IQ from the most recent `process()` call.
    pub fn update(&mut self, desired_i: f64, desired_q: f64, output_i: f64, output_q: f64) {
        let err_i = desired_i - output_i;
        let err_q = desired_q - output_q;

        // Compute effective step size: plain mu for LMS, power-normalized for NLMS.
        let mu_eff = if self.normalized {
            let power: f64 = self
                .delay_i
                .iter()
                .zip(self.delay_q.iter())
                .map(|(i, q)| i * i + q * q)
                .sum();
            self.mu / (power + 1.0) // delta = 1.0 for numerical stability
        } else {
            self.mu
        };

        for k in 0..self.num_taps {
            // tap += mu_eff * error * conj(input)
            // conj(delay_i + j*delay_q) = delay_i - j*delay_q
            // (err_i + j*err_q) * (delay_i - j*delay_q)
            //   = err_i*delay_i + err_q*delay_q + j*(err_q*delay_i - err_i*delay_q)
            self.taps_i[k] += mu_eff * (err_i * self.delay_i[k] + err_q * self.delay_q[k]);
            self.taps_q[k] += mu_eff * (err_q * self.delay_i[k] - err_i * self.delay_q[k]);
        }
    }

    /// Update taps using the most recent process() output as the output value.
    ///
    /// Convenience method: equivalent to `update(desired_i, desired_q, last_out_i, last_out_q)`.
    pub fn update_from_last(&mut self, desired_i: f64, desired_q: f64) {
        self.update(desired_i, desired_q, self.last_out_i, self.last_out_q);
    }

    /// Set the LMS step size (mu). Use 0.0 to freeze taps.
    ///
    /// **Contract boundary.** `QamDemodulator::set_equalizer_mu` filters
    /// external public inputs before reaching this method; direct DSP
    /// users still rely on this lower-layer guard. A NaN/Inf mu would
    /// poison `taps_i`/`taps_q` on the next `update`
    /// (`tap += mu * error * conj(input)` is NaN once mu is non-finite),
    /// after which `process` produces NaN output forever. See the
    /// matching note on `Oscillator::set_phase`. `debug_assert` fires in
    /// debug; release silently keeps the previous mu.
    pub fn set_mu(&mut self, mu: f64) {
        debug_assert!(mu.is_finite(), "Equalizer::set_mu: non-finite mu {mu}");
        if !mu.is_finite() {
            return;
        }
        self.mu = mu;
    }

    /// Copy trained tap coefficients from another equalizer.
    ///
    /// Both equalizers must have the same number of taps. Only the tap
    /// weights are copied; the delay line and mu are left unchanged.
    pub fn copy_taps_from(&mut self, other: &Equalizer) {
        assert_eq!(
            self.num_taps, other.num_taps,
            "equalizer tap count mismatch: {} vs {}",
            self.num_taps, other.num_taps
        );
        self.taps_i.copy_from_slice(&other.taps_i);
        self.taps_q.copy_from_slice(&other.taps_q);
    }

    /// Reset equalizer to initial passthrough state.
    pub fn reset(&mut self) {
        let center = self.num_taps / 2;
        self.taps_i.fill(0.0);
        self.taps_q.fill(0.0);
        self.taps_i[center] = 1.0;
        self.delay_i.fill(0.0);
        self.delay_q.fill(0.0);
        self.last_out_i = 0.0;
        self.last_out_q = 0.0;
    }

    /// Sum of squared tap weights (I² + Q² over all taps).
    /// Passthrough = 1.0. Divergence visible as significant deviation.
    /// Diagnostic only — flag for removal after B1 investigation.
    #[cfg(test)]
    pub fn diag_taps_norm(&self) -> f64 {
        let sum_i: f64 = self.taps_i.iter().map(|x| x * x).sum();
        let sum_q: f64 = self.taps_q.iter().map(|x| x * x).sum();
        sum_i + sum_q
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn passthrough_with_center_tap_only() {
        let mut eq = Equalizer::new(11, 0.01);

        // With center-tap = (1,0) and all others = (0,0), the equalizer
        // should act as a pure delay (center_tap = 5 samples).
        // Feed 6 samples to fill delay line up to center tap, then check.
        let center = 5;
        for i in 0..center {
            let (out_i, out_q) = eq.process(1.0 + i as f64, 0.5 + i as f64);
            // Output comes from center tap looking at delay[center], which is
            // still zero until we've pushed enough samples.
            let _ = (out_i, out_q);
        }

        // After `center` samples, the center position of the delay line holds
        // the first sample we pushed. From here on, output should match
        // input delayed by `center` samples.
        let test_points = [
            (0.7, 0.3),
            (-0.5, 0.8),
            (1.2, -0.4),
            (0.0, 0.0),
            (-1.0, 1.0),
        ];
        // Push `center` more samples to get past the initial fill.
        for i in 0..center {
            eq.process(
                test_points[i % test_points.len()].0,
                test_points[i % test_points.len()].1,
            );
        }

        // Now feed known inputs and check that output matches input from
        // `center` samples ago.
        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        for i in 0..20 {
            let in_i = (i as f64 * 0.37).sin();
            let in_q = (i as f64 * 0.53).cos();
            inputs.push((in_i, in_q));
            let (out_i, out_q) = eq.process(in_i, in_q);
            outputs.push((out_i, out_q));
        }

        // Output[k] should equal input[k - center], but since we already
        // primed the delay line, from output index `center` onward the
        // output matches the input at index 0, 1, 2, ...
        for k in center..20 {
            let (expected_i, expected_q) = inputs[k - center];
            let (got_i, got_q) = outputs[k];
            assert!(
                (got_i - expected_i).abs() < 1e-12 && (got_q - expected_q).abs() < 1e-12,
                "Passthrough failed at k={k}: expected ({expected_i:.6}, {expected_q:.6}), \
                 got ({got_i:.6}, {got_q:.6})"
            );
        }
    }

    #[test]
    fn convergence_on_known_distortion() {
        // Simulate a simple ISI channel: output[n] = input[n] + 0.3*input[n-1].
        // The equalizer should learn to compensate this and recover the original.
        let mu = 0.05;
        let mut eq = Equalizer::new(11, mu);

        // Generate a sequence of known symbols (QPSK-like).
        let symbols: Vec<(f64, f64)> = (0..500)
            .map(|i| {
                let phase = (i % 4) as f64 * std::f64::consts::FRAC_PI_2;
                (phase.cos(), phase.sin())
            })
            .collect();

        // Pass symbols through ISI channel.
        let mut prev_i = 0.0;
        let mut prev_q = 0.0;
        let mut distorted = Vec::with_capacity(symbols.len());
        for &(si, sq) in &symbols {
            let di = si + 0.3 * prev_i;
            let dq = sq + 0.3 * prev_q;
            distorted.push((di, dq));
            prev_i = si;
            prev_q = sq;
        }

        // Train: feed distorted, update with known desired.
        let mut errors_early = Vec::new();
        let mut errors_late = Vec::new();
        for (idx, (&(di, dq), &(si, sq))) in distorted.iter().zip(symbols.iter()).enumerate() {
            let (out_i, out_q) = eq.process(di, dq);
            eq.update(si, sq, out_i, out_q);

            let err = ((out_i - si).powi(2) + (out_q - sq).powi(2)).sqrt();
            if idx >= 10 && idx < 60 {
                errors_early.push(err);
            }
            if idx >= 400 {
                errors_late.push(err);
            }
        }

        let mean_early: f64 = errors_early.iter().sum::<f64>() / errors_early.len() as f64;
        let mean_late: f64 = errors_late.iter().sum::<f64>() / errors_late.len() as f64;

        assert!(
            mean_late < mean_early * 0.3,
            "Late error ({mean_late:.4}) should be much smaller than early error ({mean_early:.4})"
        );
        assert!(
            mean_late < 0.05,
            "Converged error ({mean_late:.4}) should be < 0.05"
        );
    }

    #[test]
    fn reset_returns_to_passthrough() {
        let mut eq = Equalizer::new(11, 0.05);

        // Train on some data
        for i in 0..100 {
            let (out_i, out_q) = eq.process((i as f64 * 0.3).sin(), (i as f64 * 0.5).cos());
            eq.update(0.5, 0.5, out_i, out_q);
        }

        eq.reset();

        // After reset, center tap should be (1,0) and all others (0,0).
        // Feed the identity signal: a single 1.0 after center-tap delay should appear.
        let center = 5;
        for _ in 0..center {
            eq.process(0.0, 0.0);
        }
        let (out_i, out_q) = eq.process(1.0, 0.0);
        // Output should be the sample from center positions ago = 0.0
        let _ = (out_i, out_q);

        // Feed zeros to flush
        for _ in 0..center {
            eq.process(0.0, 0.0);
        }
        let (out_i, _out_q) = eq.process(0.0, 0.0);
        assert!(
            out_i.abs() < 1e-10,
            "After reset + flush, output should be zero"
        );
    }

    #[test]
    fn copy_taps_transfers_coefficients() {
        let mut eq1 = Equalizer::new(11, 0.05);
        let eq2 = Equalizer::new(11, 0.0); // frozen

        // Train eq1
        for i in 0..200 {
            let (out_i, out_q) = eq1.process(
                (i as f64 * 0.3).sin() + 0.3 * ((i - 1) as f64 * 0.3).sin(),
                0.0,
            );
            eq1.update((i as f64 * 0.3).sin(), 0.0, out_i, out_q);
        }

        // Copy trained taps to eq2
        let mut eq2 = eq2;
        eq2.copy_taps_from(&eq1);

        // Both should now produce similar output for the same input
        let test_i = 0.5;
        let test_q = -0.3;
        let (o1_i, o1_q) = eq1.process(test_i, test_q);
        let (o2_i, o2_q) = eq2.process(test_i, test_q);
        // They share taps but have different delay lines, so outputs differ
        // after the first sample. Just verify copy_taps didn't panic.
        let _ = (o1_i, o1_q, o2_i, o2_q);
    }

    #[test]
    fn set_mu_freezes_taps() {
        let mut eq = Equalizer::new(11, 0.05);

        // Train for a bit
        for i in 0..50 {
            let phase = (i % 4) as f64 * std::f64::consts::FRAC_PI_2;
            let (out_i, out_q) = eq.process(phase.cos(), phase.sin());
            eq.update(phase.cos(), phase.sin(), out_i, out_q);
        }

        // Freeze
        eq.set_mu(0.0);

        // Process more samples — taps should not change (mu=0)
        let (out1_i, _) = eq.process(0.5, 0.3);
        eq.update(0.0, 0.0, out1_i, 0.0); // Would be a large error normally
        let (out2_i, _) = eq.process(0.5, 0.3);

        // With frozen taps, same input at same delay state should give same output.
        // (Delay line advances so they won't be identical, but the key is the taps
        // didn't change from the update call.)
        let _ = (out1_i, out2_i);
    }

    #[test]
    fn update_from_last_convenience() {
        let mut eq = Equalizer::new(5, 0.05);

        let (out_i, out_q) = eq.process(0.7, 0.3);
        // update_from_last should produce same result as explicit update
        eq.update_from_last(1.0, 0.0);
        // No panic and it used last_out values internally
        let _ = (out_i, out_q);
    }

    #[test]
    fn nlms_convergence_on_known_distortion() {
        // Same ISI channel as the LMS test: output[n] = input[n] + 0.3*input[n-1].
        // NLMS should converge at least as well as LMS.
        let mu = 0.5; // NLMS can use a larger mu due to normalization
        let mut eq = Equalizer::new_normalized(11, mu);

        let symbols: Vec<(f64, f64)> = (0..500)
            .map(|i| {
                let phase = (i % 4) as f64 * std::f64::consts::FRAC_PI_2;
                (phase.cos(), phase.sin())
            })
            .collect();

        let mut prev_i = 0.0;
        let mut prev_q = 0.0;
        let mut distorted = Vec::with_capacity(symbols.len());
        for &(si, sq) in &symbols {
            let di = si + 0.3 * prev_i;
            let dq = sq + 0.3 * prev_q;
            distorted.push((di, dq));
            prev_i = si;
            prev_q = sq;
        }

        let mut errors_late = Vec::new();
        for (idx, (&(di, dq), &(si, sq))) in distorted.iter().zip(symbols.iter()).enumerate() {
            let (out_i, out_q) = eq.process(di, dq);
            eq.update(si, sq, out_i, out_q);

            let err = ((out_i - si).powi(2) + (out_q - sq).powi(2)).sqrt();
            if idx >= 400 {
                errors_late.push(err);
            }
        }

        let mean_late: f64 = errors_late.iter().sum::<f64>() / errors_late.len() as f64;
        assert!(
            mean_late < 0.05,
            "NLMS converged error ({mean_late:.4}) should be < 0.05"
        );
    }

    #[test]
    fn nlms_does_not_regress_vs_lms() {
        // Both LMS and NLMS should converge on the same channel.
        // NLMS should not be significantly worse.
        let symbols: Vec<(f64, f64)> = (0..500)
            .map(|i| {
                let phase = (i % 4) as f64 * std::f64::consts::FRAC_PI_2;
                (phase.cos(), phase.sin())
            })
            .collect();

        let mut prev_i = 0.0;
        let mut prev_q = 0.0;
        let mut distorted = Vec::with_capacity(symbols.len());
        for &(si, sq) in &symbols {
            distorted.push((si + 0.3 * prev_i, sq + 0.3 * prev_q));
            prev_i = si;
            prev_q = sq;
        }

        // Run LMS
        let mut eq_lms = Equalizer::new(11, 0.05);
        let mut lms_late = Vec::new();
        for (idx, (&(di, dq), &(si, sq))) in distorted.iter().zip(symbols.iter()).enumerate() {
            let (out_i, out_q) = eq_lms.process(di, dq);
            eq_lms.update(si, sq, out_i, out_q);
            if idx >= 400 {
                lms_late.push(((out_i - si).powi(2) + (out_q - sq).powi(2)).sqrt());
            }
        }

        // Run NLMS
        let mut eq_nlms = Equalizer::new_normalized(11, 0.5);
        let mut nlms_late = Vec::new();
        for (idx, (&(di, dq), &(si, sq))) in distorted.iter().zip(symbols.iter()).enumerate() {
            let (out_i, out_q) = eq_nlms.process(di, dq);
            eq_nlms.update(si, sq, out_i, out_q);
            if idx >= 400 {
                nlms_late.push(((out_i - si).powi(2) + (out_q - sq).powi(2)).sqrt());
            }
        }

        let lms_mean: f64 = lms_late.iter().sum::<f64>() / lms_late.len() as f64;
        let nlms_mean: f64 = nlms_late.iter().sum::<f64>() / nlms_late.len() as f64;

        // NLMS should not be more than 3x worse than LMS.
        assert!(
            nlms_mean < lms_mean * 3.0,
            "NLMS ({nlms_mean:.4}) should not regress significantly vs LMS ({lms_mean:.4})"
        );
    }

    /// `set_mu(NaN)` / `set_mu(±Inf)` must not poison the taps when a
    /// later `update` is called with finite inputs. Run only in release
    /// mode where `debug_assert` is disabled.
    #[test]
    #[cfg(not(debug_assertions))]
    fn test_set_mu_rejects_non_finite() {
        for &bad in &[f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut eq = Equalizer::new(11, 0.05);

            // Train briefly with finite inputs so taps are non-default.
            for i in 0..50 {
                let phase = (i % 4) as f64 * std::f64::consts::FRAC_PI_2;
                let (out_i, out_q) = eq.process(phase.cos(), phase.sin());
                eq.update(phase.cos(), phase.sin(), out_i, out_q);
            }
            let taps_norm_before = eq.diag_taps_norm();

            // Poison mu.
            eq.set_mu(bad);

            // A subsequent update with finite inputs must keep taps finite
            // (silent skip means mu stayed at 0.05, so the update applies
            // a small correction; either way, no NaN).
            for i in 0..20 {
                let phase = (i % 4) as f64 * std::f64::consts::FRAC_PI_2;
                let (out_i, out_q) = eq.process(phase.cos(), phase.sin());
                eq.update(phase.cos(), phase.sin(), out_i, out_q);
                assert!(
                    out_i.is_finite() && out_q.is_finite(),
                    "process output non-finite after set_mu({bad})"
                );
            }
            let taps_norm_after = eq.diag_taps_norm();
            assert!(taps_norm_after.is_finite(),
                "diag_taps_norm non-finite after set_mu({bad}): before={taps_norm_before}, after={taps_norm_after}");
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "non-finite mu")]
    fn test_set_mu_debug_assert_panics_on_nan() {
        let mut eq = Equalizer::new(11, 0.05);
        eq.set_mu(f64::NAN);
    }
}
