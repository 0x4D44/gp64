//! Symbol timing recovery for QAM demodulation.

/// Gardner timing error detector with interpolation.
///
/// Uses 4-point Catmull-Rom for >= 3 sps, or 6-point windowed sinc for < 3 sps.
/// The sinc interpolator provides better accuracy at low oversampling ratios
/// (e.g., V.34 at 2.5 sps) where Catmull-Rom introduces significant ISI.
pub struct TimingRecovery {
    /// Fractional symbol timing offset (0.0-1.0).
    mu: f64,
    /// Samples per symbol (e.g., 3.333 for 2400 baud at 8000 Hz).
    samples_per_symbol: f64,
    /// Timing loop gain.
    loop_gain: f64,
    /// Fractional sample counter within the current symbol period.
    sample_count: f64,
    /// Previous symbol decision point value (I channel).
    prev_symbol: f64,
    /// Previous symbol decision point value (Q channel).
    prev_symbol_q: f64,
    /// 4-sample I history: [n-3, n-2, n-1, n] (shift register).
    hist_i: [f64; 4],
    /// 4-sample Q history.
    hist_q: [f64; 4],
    /// 6-sample I history for sinc interpolation (when use_sinc is true).
    hist6_i: [f64; 6],
    /// 6-sample Q history for sinc interpolation.
    hist6_q: [f64; 6],
    /// Number of samples fed (for warmup).
    samples_fed: u32,
    /// Pending interpolation: frac value waiting for next sample's context.
    pending_frac: Option<f64>,
    /// Use 6-point sinc interpolation (auto-enabled when sps < 3.0).
    use_sinc: bool,
}

/// Catmull-Rom cubic interpolation at position `t` within [p1, p2].
/// `t=0` gives p1, `t=1` gives p2.
fn catmull_rom(p0: f64, p1: f64, p2: f64, p3: f64, t: f64) -> f64 {
    let t2 = t * t;
    let t3 = t2 * t;
    0.5 * ((2.0 * p1)
        + (-p0 + p2) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3)
}

/// 6-point Hann-windowed sinc interpolation.
///
/// Interpolates at fractional position `frac` within [h[3], h[4]], using all 6
/// points. The window is asymmetric (3 pre-center, 1 post-center) to work with
/// 1-sample pending at low oversampling ratios.
fn sinc_interp_6(h: &[f64; 6], frac: f64) -> f64 {
    let mut sum = 0.0;
    let mut wsum = 0.0;
    for k in 0..6 {
        let d = (k as f64 - 3.0) - frac;
        let w = if d.abs() < 1e-12 {
            1.0
        } else {
            let sinc = (std::f64::consts::PI * d).sin() / (std::f64::consts::PI * d);
            let hann = 0.5 * (1.0 + (std::f64::consts::PI * d / 3.5).cos());
            sinc * hann
        };
        sum += h[k] * w;
        wsum += w;
    }
    if wsum.abs() > 1e-24 {
        sum / wsum
    } else {
        sum
    }
}

impl TimingRecovery {
    /// Create timing recovery for the given symbol rate and sample rate.
    pub fn new(symbol_rate: f64, sample_rate: f64, loop_gain: f64) -> Self {
        let sps = sample_rate / symbol_rate;
        Self {
            mu: 0.0,
            samples_per_symbol: sps,
            loop_gain,
            sample_count: 0.0,
            prev_symbol: 0.0,
            prev_symbol_q: 0.0,
            hist_i: [0.0; 4],
            hist_q: [0.0; 4],
            hist6_i: [0.0; 6],
            hist6_q: [0.0; 6],
            samples_fed: 0,
            pending_frac: None,
            use_sinc: sps < 3.0,
        }
    }

    /// Set the initial sample counter to pre-align decimation with a known
    /// group delay. For example, when TX and RX RRC filters contribute a
    /// combined group delay of `G` samples, set `initial_count = G % sps`
    /// so the first symbol boundary falls at the correct sampling instant.
    ///
    /// Non-finite `count` is silently skipped (with a debug-build assert).
    /// Same NaN-poison rationale as `Oscillator::set_phase`: `sample_count`
    /// participates in modulo arithmetic that has no protection against NaN
    /// flooding the timing loop.
    pub fn set_initial_sample_count(&mut self, count: f64) {
        debug_assert!(
            count.is_finite(),
            "TimingRecovery::set_initial_sample_count: non-finite count {count}"
        );
        if !count.is_finite() {
            return;
        }
        self.sample_count = count;
    }

    /// Feed a new sample. Returns `Some(interpolated_value)` when a symbol
    /// decision point is reached, `None` otherwise.
    pub fn feed(&mut self, sample: f64) -> Option<f64> {
        self.feed_iq(sample, 0.0).map(|(i, _)| i)
    }

    /// Feed new I and Q samples. Returns `Some((interp_i, interp_q))` when a
    /// symbol decision point is reached, interpolating both channels to the
    /// same fractional sample instant using Catmull-Rom cubic interpolation.
    pub fn feed_iq(&mut self, sample_i: f64, sample_q: f64) -> Option<(f64, f64)> {
        // Shift 4-point history (always maintained for Catmull-Rom + Gardner TED).
        self.hist_i[0] = self.hist_i[1];
        self.hist_i[1] = self.hist_i[2];
        self.hist_i[2] = self.hist_i[3];
        self.hist_i[3] = sample_i;
        self.hist_q[0] = self.hist_q[1];
        self.hist_q[1] = self.hist_q[2];
        self.hist_q[2] = self.hist_q[3];
        self.hist_q[3] = sample_q;

        // Shift 6-point history for sinc interpolation.
        if self.use_sinc {
            for k in 0..5 {
                self.hist6_i[k] = self.hist6_i[k + 1];
                self.hist6_q[k] = self.hist6_q[k + 1];
            }
            self.hist6_i[5] = sample_i;
            self.hist6_q[5] = sample_q;
        }

        self.samples_fed += 1;
        self.sample_count += 1.0;

        let min_warmup = if self.use_sinc { 6 } else { 4 };
        if self.samples_fed < min_warmup {
            return None;
        }

        // Check if a pending symbol needs to be emitted now that we have context
        if let Some(frac) = self.pending_frac.take() {
            let (sym_i, sym_q) = if self.use_sinc {
                // 6-point sinc: hist6 = [N-4, N-3, N-2, N-1, N, N+1]
                // Decision point between hist6[3]=N-1 and hist6[4]=N.
                (
                    sinc_interp_6(&self.hist6_i, frac),
                    sinc_interp_6(&self.hist6_q, frac),
                )
            } else {
                // 4-point Catmull-Rom: hist = [N-2, N-1, N, N+1]
                (
                    catmull_rom(
                        self.hist_i[0],
                        self.hist_i[1],
                        self.hist_i[2],
                        self.hist_i[3],
                        frac,
                    ),
                    catmull_rom(
                        self.hist_q[0],
                        self.hist_q[1],
                        self.hist_q[2],
                        self.hist_q[3],
                        frac,
                    ),
                )
            };

            // Gardner TED: dual-channel (I+Q) for proper QAM tracking
            let mid_i = self.hist_i[1];
            let mid_q = self.hist_q[1];
            let error =
                (mid_i * (self.prev_symbol - sym_i) + mid_q * (self.prev_symbol_q - sym_q)) * 0.5;
            self.mu -= self.loop_gain * error;
            self.mu = self.mu.clamp(-0.5, 0.5);

            self.prev_symbol = sym_i;
            self.prev_symbol_q = sym_q;

            // DON'T check for another boundary this sample — the pending consumed it.
            return Some((sym_i, sym_q));
        }

        // Check for symbol boundary crossing
        let threshold = self.samples_per_symbol + self.mu;

        if self.sample_count >= threshold {
            let overshoot = self.sample_count - threshold;
            let frac = (1.0 - overshoot).clamp(0.0, 1.0);

            // Set pending — wait for next sample to get Catmull-Rom context
            self.pending_frac = Some(frac);
            self.sample_count = overshoot;

            None // Symbol will be emitted on the NEXT feed_iq() call
        } else {
            None
        }
    }

    /// Reset timing state.
    pub fn reset(&mut self) {
        self.mu = 0.0;
        self.sample_count = 0.0;
        self.prev_symbol = 0.0;
        self.prev_symbol_q = 0.0;
        self.hist_i = [0.0; 4];
        self.hist_q = [0.0; 4];
        self.hist6_i = [0.0; 6];
        self.hist6_q = [0.0; 6];
        self.samples_fed = 0;
        self.pending_frac = None;
    }

    /// Update the timing loop gain (bandwidth) at runtime.
    /// Set to 0.0 to freeze timing at its current converged position.
    ///
    /// Non-finite `gain` is silently skipped (with a debug-build assert).
    /// Same NaN-poison rationale as `Oscillator::set_phase`.
    pub fn set_loop_gain(&mut self, gain: f64) {
        debug_assert!(
            gain.is_finite(),
            "TimingRecovery::set_loop_gain: non-finite gain {gain}"
        );
        if !gain.is_finite() {
            return;
        }
        self.loop_gain = gain;
    }

    /// Current fractional timing offset.
    pub fn mu(&self) -> f64 {
        self.mu
    }

    /// Current timing loop gain (bandwidth). Test-only.
    #[cfg(test)]
    pub fn loop_gain(&self) -> f64 {
        self.loop_gain
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const SYMBOL_RATE: f64 = 2400.0;
    const SAMPLE_RATE: f64 = 8000.0;
    const LOOP_GAIN: f64 = 0.01;

    #[test]
    fn test_timing_outputs_at_symbol_rate() {
        // Feed 80 samples (10ms at 8000 Hz), expect ~24 symbol outputs (2400 baud * 10ms).
        let mut tr = TimingRecovery::new(SYMBOL_RATE, SAMPLE_RATE, LOOP_GAIN);

        // Generate a simple test signal: 1200 Hz tone (half the symbol rate).
        let num_samples = 80;
        let mut symbol_count = 0;
        for i in 0..num_samples {
            let t = i as f64 / SAMPLE_RATE;
            let sample = (TAU * 1200.0 * t).sin();
            if tr.feed(sample).is_some() {
                symbol_count += 1;
            }
        }

        // Expected: 80 samples / (8000/2400) ≈ 24 symbols.
        // Allow ±1 for edge effects at block boundaries.
        assert!(
            (23..=25).contains(&symbol_count),
            "Expected ~24 symbol outputs from 80 samples at 2400 baud, got {symbol_count}"
        );
    }

    #[test]
    fn test_timing_interpolation_quality() {
        // Feed a known sinusoid at the symbol rate frequency, verify
        // interpolated values are plausible sine values (magnitude <= 1).
        let mut tr = TimingRecovery::new(SYMBOL_RATE, SAMPLE_RATE, LOOP_GAIN);
        let freq = 600.0; // Well below Nyquist for clean interpolation.

        let mut symbols = Vec::new();
        for i in 0..800 {
            let t = i as f64 / SAMPLE_RATE;
            let sample = (TAU * freq * t).sin();
            if let Some(val) = tr.feed(sample) {
                symbols.push(val);
            }
        }

        // All interpolated values should have magnitude <= 1.0 (with small
        // tolerance for interpolation artifacts).
        for (i, &val) in symbols.iter().enumerate() {
            assert!(
                val.abs() < 1.1,
                "Symbol {i} has value {val} which exceeds expected magnitude for unit sine"
            );
        }

        // Should have produced a reasonable number of symbols.
        assert!(
            symbols.len() > 200,
            "Expected > 200 symbols from 800 samples, got {}",
            symbols.len()
        );
    }

    #[test]
    fn test_timing_reset() {
        let mut tr = TimingRecovery::new(SYMBOL_RATE, SAMPLE_RATE, LOOP_GAIN);

        // Feed some samples to change internal state.
        for i in 0..50 {
            let t = i as f64 / SAMPLE_RATE;
            tr.feed((TAU * 1200.0 * t).sin());
        }

        // Reset and verify state is clean.
        tr.reset();
        assert_eq!(tr.mu(), 0.0, "mu should be 0.0 after reset");

        // After reset, the first few samples should behave identically to
        // a fresh instance.
        let mut fresh = TimingRecovery::new(SYMBOL_RATE, SAMPLE_RATE, LOOP_GAIN);
        for i in 0..20 {
            let t = i as f64 / SAMPLE_RATE;
            let sample = (TAU * 1200.0 * t).sin();
            let result_reset = tr.feed(sample);
            let result_fresh = fresh.feed(sample);
            assert_eq!(
                result_reset.is_some(),
                result_fresh.is_some(),
                "Reset instance should behave identically to fresh at sample {i}"
            );
        }
    }

    #[test]
    fn test_timing_iq_output() {
        // feed_iq should produce symbols with both I and Q components
        let mut tr = TimingRecovery::new(SYMBOL_RATE, SAMPLE_RATE, 0.0);
        let freq = 600.0;

        let mut symbols = Vec::new();
        for i in 0..800 {
            let t = i as f64 / SAMPLE_RATE;
            let sample_i = (TAU * freq * t).sin();
            let sample_q = (TAU * freq * t).cos();
            if let Some((si, sq)) = tr.feed_iq(sample_i, sample_q) {
                symbols.push((si, sq));
            }
        }

        assert!(symbols.len() > 200, "Should produce symbols");
        // At least some Q values should be non-zero
        let q_nonzero = symbols.iter().any(|(_, q)| q.abs() > 0.01);
        assert!(q_nonzero, "Q channel should have non-zero values");
    }

    #[test]
    fn test_timing_set_initial_sample_count() {
        let mut tr = TimingRecovery::new(SYMBOL_RATE, SAMPLE_RATE, LOOP_GAIN);
        tr.set_initial_sample_count(1.5);
        // Just verify it doesn't panic and the first symbols still come out
        let mut count = 0;
        for i in 0..80 {
            let t = i as f64 / SAMPLE_RATE;
            if tr.feed((TAU * 1200.0 * t).sin()).is_some() {
                count += 1;
            }
        }
        assert!(count > 0, "Should produce at least one symbol");
    }

    #[test]
    fn test_timing_zero_loop_gain() {
        // With zero loop gain, mu should stay at 0 (no timing adjustment)
        let mut tr = TimingRecovery::new(SYMBOL_RATE, SAMPLE_RATE, 0.0);

        for i in 0..800 {
            let t = i as f64 / SAMPLE_RATE;
            tr.feed((TAU * 1200.0 * t).sin());
        }

        assert_eq!(
            tr.mu(),
            0.0,
            "Zero loop gain should keep mu at 0, got {}",
            tr.mu()
        );
    }

    #[test]
    fn test_timing_v34_rate() {
        // V.34 at 3200 baud, 8000 Hz → 2.5 sps (should use sinc interpolation)
        let mut tr = TimingRecovery::new(3200.0, 8000.0, 0.0);
        let freq = 800.0;

        let mut symbol_count = 0;
        for i in 0..80 {
            let t = i as f64 / 8000.0;
            if tr.feed((TAU * freq * t).sin()).is_some() {
                symbol_count += 1;
            }
        }
        // 80 samples / 2.5 sps = 32 symbols expected
        assert!(
            (30..=34).contains(&symbol_count),
            "Expected ~32 symbols at 3200 baud, got {symbol_count}"
        );
    }

    /// `set_initial_sample_count(NaN)` and `set_initial_sample_count(±Inf)`
    /// must not poison the timing loop. Backlog #43 — same NaN-poison
    /// shape as `Oscillator::set_phase`. Release-only because debug
    /// builds correctly panic at the `debug_assert`.
    #[test]
    #[cfg(not(debug_assertions))]
    fn test_set_initial_sample_count_rejects_non_finite() {
        let mut tr = TimingRecovery::new(SAMPLE_RATE, SYMBOL_RATE, LOOP_GAIN);
        tr.set_initial_sample_count(f64::NAN);
        tr.set_initial_sample_count(f64::INFINITY);
        tr.set_initial_sample_count(f64::NEG_INFINITY);
        // Sanity: feed a sine and confirm finite outputs.
        for k in 0..400u32 {
            let t = k as f64 / SAMPLE_RATE;
            if let Some(s) = tr.feed((TAU * 1200.0 * t).sin()) {
                assert!(
                    s.is_finite(),
                    "TR symbol[{k}] non-finite after non-finite set_initial_sample_count"
                );
            }
        }
    }

    /// Debug-build counterpart: `set_initial_sample_count(NaN)` panics
    /// via `debug_assert`, confirming the assertion is wired up.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "non-finite count")]
    fn test_set_initial_sample_count_debug_panics_on_nan() {
        let mut tr = TimingRecovery::new(SAMPLE_RATE, SYMBOL_RATE, LOOP_GAIN);
        tr.set_initial_sample_count(f64::NAN);
    }

    /// `set_loop_gain(NaN)` and `set_loop_gain(±Inf)` must not poison
    /// the timing loop. Backlog #43.
    #[test]
    #[cfg(not(debug_assertions))]
    fn test_set_loop_gain_rejects_non_finite() {
        let mut tr = TimingRecovery::new(SAMPLE_RATE, SYMBOL_RATE, LOOP_GAIN);
        tr.set_loop_gain(f64::NAN);
        tr.set_loop_gain(f64::INFINITY);
        for k in 0..400u32 {
            let t = k as f64 / SAMPLE_RATE;
            if let Some(s) = tr.feed((TAU * 1200.0 * t).sin()) {
                assert!(
                    s.is_finite(),
                    "TR symbol[{k}] non-finite after non-finite set_loop_gain"
                );
            }
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "non-finite gain")]
    fn test_set_loop_gain_debug_panics_on_nan() {
        let mut tr = TimingRecovery::new(SAMPLE_RATE, SYMBOL_RATE, LOOP_GAIN);
        tr.set_loop_gain(f64::NAN);
    }
}
