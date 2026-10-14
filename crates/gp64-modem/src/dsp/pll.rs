//! Second-order PLL for carrier frequency/phase recovery.

use std::f64::consts::PI;

/// Decision-directed carrier recovery PLL.
pub struct Pll {
    /// Estimated carrier phase (radians).
    phase: f64,
    /// Estimated carrier frequency offset (radians/sample).
    freq: f64,
    /// Proportional gain.
    kp: f64,
    /// Integral gain.
    ki: f64,
    /// Nominal loop bandwidth in Hz last applied via `with_bandwidth` or
    /// `set_bandwidth`. Test-only; not used by the loop update.
    #[cfg(test)]
    nominal_bandwidth_hz: f64,
}

impl Pll {
    /// Create a PLL with the given loop gains.
    /// For V.32bis: carrier at 1800 Hz, 8000 Hz sample rate.
    /// Typical: kp=0.01, ki=0.0001 for ~50 Hz loop bandwidth.
    pub fn new(kp: f64, ki: f64) -> Self {
        debug_assert!(kp.is_finite(), "Pll::new: non-finite kp {kp}");
        debug_assert!(ki.is_finite(), "Pll::new: non-finite ki {ki}");
        // Substitute 0.0 (inactive PLL) for non-finite gains rather than
        // poisoning the loop on first `update()`. Same NaN-poison
        // rationale as `Oscillator::new`'s frequency clamp.
        let kp = if kp.is_finite() { kp } else { 0.0 };
        let ki = if ki.is_finite() { ki } else { 0.0 };
        Self {
            phase: 0.0,
            freq: 0.0,
            kp,
            ki,
            #[cfg(test)]
            nominal_bandwidth_hz: 0.0,
        }
    }

    /// Create a PLL with a target loop bandwidth.
    /// Automatically computes kp/ki for the given bandwidth and damping.
    ///
    /// Uses the standard 2nd-order PLL design equations:
    ///   omega_n = 2 * pi * bandwidth / (damping + 1/(4*damping))
    ///   kp = 2 * damping * omega_n
    ///   ki = omega_n^2
    pub fn with_bandwidth(bandwidth_hz: f64, sample_rate: f64, damping: f64) -> Self {
        debug_assert!(
            bandwidth_hz.is_finite(),
            "Pll::with_bandwidth: non-finite bandwidth_hz {bandwidth_hz}"
        );
        debug_assert!(
            sample_rate.is_finite(),
            "Pll::with_bandwidth: non-finite sample_rate {sample_rate}"
        );
        debug_assert!(
            damping.is_finite(),
            "Pll::with_bandwidth: non-finite damping {damping}"
        );
        // Substitute zero gains (inactive PLL) on any non-finite input.
        if !bandwidth_hz.is_finite() || !sample_rate.is_finite() || !damping.is_finite() {
            return Self {
                phase: 0.0,
                freq: 0.0,
                kp: 0.0,
                ki: 0.0,
                #[cfg(test)]
                nominal_bandwidth_hz: 0.0,
            };
        }
        // Natural frequency of the loop.
        let omega_n = 2.0 * PI * bandwidth_hz / (damping + 1.0 / (4.0 * damping)) / sample_rate;
        let kp = 2.0 * damping * omega_n;
        let ki = omega_n * omega_n;

        Self {
            phase: 0.0,
            freq: 0.0,
            kp,
            ki,
            #[cfg(test)]
            nominal_bandwidth_hz: bandwidth_hz,
        }
    }

    /// Update the PLL with a phase error measurement.
    /// Returns the current phase estimate.
    ///
    /// Fails closed (MDD-BUG-CRU-01208): a non-finite `phase_error` is
    /// ignored, and an update that overflows the loop state to a non-finite
    /// value resets it rather than carrying infinity forward.
    pub fn update(&mut self, phase_error: f64) -> f64 {
        if !phase_error.is_finite() {
            return self.phase;
        }
        // Standard 2nd-order PLL update.
        self.freq += self.ki * phase_error;
        self.phase += self.freq + self.kp * phase_error;
        if !self.freq.is_finite() || !self.phase.is_finite() {
            self.reset();
        }

        // Wrap phase to [-pi, pi).
        self.phase = wrap_phase(self.phase);

        self.phase
    }

    /// Get the current phase estimate (radians).
    pub fn phase(&self) -> f64 {
        self.phase
    }

    /// Get the current frequency estimate (radians/sample).
    pub fn frequency(&self) -> f64 {
        self.freq
    }

    /// Change PLL loop bandwidth at runtime (e.g., wide during training, narrow for data mode).
    /// Preserves the accumulated frequency estimate.
    pub fn set_bandwidth(&mut self, bandwidth_hz: f64, sample_rate: f64, damping: f64) {
        debug_assert!(
            bandwidth_hz.is_finite(),
            "Pll::set_bandwidth: non-finite bandwidth_hz {bandwidth_hz}"
        );
        debug_assert!(
            sample_rate.is_finite(),
            "Pll::set_bandwidth: non-finite sample_rate {sample_rate}"
        );
        debug_assert!(
            damping.is_finite(),
            "Pll::set_bandwidth: non-finite damping {damping}"
        );
        if !bandwidth_hz.is_finite() || !sample_rate.is_finite() || !damping.is_finite() {
            return;
        }
        let omega_n = 2.0 * PI * bandwidth_hz / (damping + 1.0 / (4.0 * damping)) / sample_rate;
        self.kp = 2.0 * damping * omega_n;
        self.ki = omega_n * omega_n;
        // self.freq and self.phase are intentionally preserved
        #[cfg(test)]
        {
            self.nominal_bandwidth_hz = bandwidth_hz;
        }
    }

    /// Nominal loop bandwidth in Hz as passed to `with_bandwidth` /
    /// `set_bandwidth`. Test-only.
    #[cfg(test)]
    pub fn bandwidth_hz(&self) -> f64 {
        self.nominal_bandwidth_hz
    }

    /// Reset PLL state.
    pub fn reset(&mut self) {
        self.phase = 0.0;
        self.freq = 0.0;
    }
}

/// Wrap a phase angle to [-pi, pi) in constant time.
///
/// The loop update moves the phase by far less than a turn, so the one-turn
/// correction below is the path that runs, and it rounds exactly as the old
/// repeated subtraction did. Anything farther out takes a remainder: the old
/// loop took time proportional to the magnitude, and never finished once tau
/// fell below one ULP of the value, or for infinity (MDD-BUG-CRU-01208).
/// A non-finite phase has no angle and wraps to 0.
fn wrap_phase(phase: f64) -> f64 {
    let tau = 2.0 * PI;
    if !phase.is_finite() {
        return 0.0;
    }
    if (-3.0 * PI..3.0 * PI).contains(&phase) {
        if phase >= PI {
            return phase - tau;
        }
        if phase < -PI {
            return phase + tau;
        }
        return phase;
    }
    let wrapped = (phase + PI).rem_euclid(tau) - PI;
    // `rem_euclid` may round up to `tau` itself; that is the angle -PI.
    if wrapped >= PI {
        -PI
    } else {
        wrapped
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// Run `f` on a watchdog thread: the pre-fix wrap loop never returns, and
    /// a regression must fail rather than hang the test runner.
    fn within_a_second<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(std::time::Duration::from_secs(1))
            .expect("the PLL call did not return: the phase wrap wedged")
    }

    /// MDD-BUG-CRU-01208: wrapping is bounded for extreme and non-finite
    /// phases and always lands in [-pi, pi).
    #[test]
    fn wrap_phase_is_bounded_for_extreme_and_non_finite_phases() {
        for phase in [1e300, -1e300, f64::MAX, f64::MIN, 1e17, -1e17] {
            let wrapped = within_a_second(move || wrap_phase(phase));
            assert!(
                (-PI..PI).contains(&wrapped),
                "{phase:e} wrapped to {wrapped}"
            );
        }
        for phase in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
            assert_eq!(within_a_second(move || wrap_phase(phase)), 0.0, "{phase}");
        }
        // The one-turn path is unchanged.
        assert_eq!(wrap_phase(PI + 0.5), PI + 0.5 - 2.0 * PI);
        assert_eq!(wrap_phase(-PI - 0.5), -PI - 0.5 + 2.0 * PI);
    }

    /// MDD-BUG-CRU-01208: `update` returns for any input and keeps a finite
    /// state: a non-finite error is ignored, an overflow resets the loop.
    #[test]
    fn update_fails_closed_on_non_finite_and_extreme_errors() {
        let (phase, freq) = within_a_second(|| {
            let mut pll = Pll::new(0.5, 0.1);
            pll.update(0.25);
            let before = (pll.phase(), pll.frequency());
            for error in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                assert_eq!(pll.update(error), before.0);
                assert_eq!((pll.phase(), pll.frequency()), before);
            }
            pll.update(1e300);
            pll.update(f64::MAX);
            (pll.phase(), pll.frequency())
        });
        assert!((-PI..PI).contains(&phase), "phase {phase}");
        assert!(freq.is_finite(), "frequency {freq}");
    }

    #[test]
    fn test_pll_tracks_constant_phase() {
        // Feed constant phase error = 0 → phase should stay stable near 0.
        let mut pll = Pll::new(0.01, 0.0001);
        for _ in 0..1000 {
            pll.update(0.0);
        }

        assert!(
            pll.phase().abs() < 1e-10,
            "Phase should remain near zero with zero error, got {}",
            pll.phase()
        );
        assert!(
            pll.frequency().abs() < 1e-10,
            "Frequency should remain near zero with zero error, got {}",
            pll.frequency()
        );
    }

    #[test]
    fn test_pll_corrects_phase_offset() {
        // Start feeding a constant phase error to simulate an initial offset,
        // then feed the difference from a target. The PLL should converge.
        let mut pll = Pll::new(0.01, 0.0001);
        let target_phase = 1.0; // radians

        // Simulate: the "error" is (target - estimated_phase).
        // Over many iterations the PLL phase should approach the target.
        for _ in 0..5000 {
            let error = target_phase - pll.phase();
            pll.update(error);
        }

        let phase_error = (pll.phase() - target_phase).abs();
        assert!(
            phase_error < 0.01,
            "PLL should converge to target phase {target_phase}, got {} (error={phase_error})",
            pll.phase()
        );
    }

    #[test]
    fn test_pll_tracks_frequency_offset() {
        // Feed a linearly increasing phase (= constant frequency offset).
        // The PLL frequency estimate should adapt to match.
        let freq_offset = 0.05; // radians/sample
        let mut pll = Pll::new(0.01, 0.0001);
        let mut true_phase = 0.0;

        for _ in 0..10000 {
            true_phase += freq_offset;
            true_phase = wrap_phase(true_phase);
            let error = true_phase - pll.phase();
            // Wrap the error to [-pi, pi) for correct feedback.
            let error = wrap_phase(error);
            pll.update(error);
        }

        // The PLL frequency should have adapted toward the offset.
        let freq_error = (pll.frequency() - freq_offset).abs();
        assert!(
            freq_error < 0.01,
            "PLL frequency should converge to {freq_offset}, got {} (error={freq_error})",
            pll.frequency()
        );
    }

    #[test]
    fn test_pll_with_bandwidth() {
        // Verify the factory method produces reasonable kp/ki.
        let pll = Pll::with_bandwidth(50.0, 8000.0, 0.707);

        // kp and ki should be positive and small.
        assert!(pll.kp > 0.0, "kp should be positive, got {}", pll.kp);
        assert!(pll.ki > 0.0, "ki should be positive, got {}", pll.ki);
        assert!(
            pll.kp < 1.0,
            "kp should be < 1.0 for a 50 Hz bandwidth at 8000 Hz, got {}",
            pll.kp
        );
        assert!(
            pll.ki < pll.kp,
            "ki ({}) should be smaller than kp ({}) for a well-damped loop",
            pll.ki,
            pll.kp
        );

        // Phase and frequency should start at zero.
        assert_eq!(pll.phase(), 0.0);
        assert_eq!(pll.frequency(), 0.0);
    }

    #[test]
    fn test_pll_reset() {
        let mut pll = Pll::new(0.01, 0.0001);
        // Drive PLL to a non-zero state
        for _ in 0..100 {
            pll.update(0.5);
        }
        assert!(pll.phase().abs() > 0.01, "Phase should have moved");
        assert!(pll.frequency().abs() > 0.0, "Frequency should have moved");

        pll.reset();
        assert_eq!(pll.phase(), 0.0, "Phase should be 0 after reset");
        assert_eq!(pll.frequency(), 0.0, "Frequency should be 0 after reset");
    }

    #[test]
    fn test_pll_phase_wrapping() {
        // Large error should not cause phase to blow up past [-pi, pi)
        let mut pll = Pll::new(0.5, 0.01);
        for _ in 0..1000 {
            pll.update(1.0);
        }
        assert!(
            pll.phase().abs() <= PI,
            "Phase should stay in [-pi, pi), got {}",
            pll.phase()
        );
    }

    #[test]
    fn test_pll_negative_phase_error() {
        let mut pll = Pll::new(0.01, 0.0001);
        let target_phase = -1.0;

        for _ in 0..5000 {
            let error = target_phase - pll.phase();
            pll.update(error);
        }

        let phase_error = (pll.phase() - target_phase).abs();
        assert!(
            phase_error < 0.01,
            "PLL should converge to negative target {target_phase}, got {} (error={phase_error})",
            pll.phase()
        );
    }

    #[test]
    fn test_pll_with_bandwidth_different_rates() {
        // Higher bandwidth should mean larger kp
        let pll_narrow = Pll::with_bandwidth(10.0, 8000.0, 0.707);
        let pll_wide = Pll::with_bandwidth(100.0, 8000.0, 0.707);
        assert!(
            pll_wide.kp > pll_narrow.kp,
            "Wider bandwidth should have larger kp: {} vs {}",
            pll_wide.kp,
            pll_narrow.kp
        );
    }

    #[test]
    fn test_set_bandwidth_preserves_freq() {
        // Acquire a frequency offset, then switch bandwidth — freq must be preserved.
        let freq_offset = 0.03; // radians/sample
        let mut pll = Pll::with_bandwidth(50.0, 8000.0, 0.707);
        let mut true_phase = 0.0;

        // Drive PLL until it acquires the frequency offset.
        for _ in 0..8000 {
            true_phase += freq_offset;
            true_phase = wrap_phase(true_phase);
            let error = wrap_phase(true_phase - pll.phase());
            pll.update(error);
        }

        let freq_before = pll.frequency();
        assert!(
            (freq_before - freq_offset).abs() < 0.005,
            "PLL should have acquired freq offset {freq_offset}, got {freq_before}"
        );

        // Switch to narrow bandwidth — freq must survive.
        pll.set_bandwidth(10.0, 8000.0, 0.707);
        let freq_after = pll.frequency();
        assert_eq!(
            freq_before, freq_after,
            "set_bandwidth must preserve freq: before={freq_before}, after={freq_after}"
        );
    }

    #[test]
    fn test_pll_locks_at_pstn_bandwidth_50hz_offset() {
        // PLL regression guard (NOT a plumbing test, despite the old name).
        // This replaces the earlier mislabelled
        // `test_pstn_profile_converges_with_frequency_offset`, which was
        // really just a bare-PLL test wearing a profile-shaped hat. This
        // version takes the loop bandwidth directly from
        // `DspProfile::pstn().pll_bandwidth_hz` so the provenance is
        // explicit: if the profile's PLL bandwidth ever changes, this test
        // follows without editing.
        //
        // For the plumbing check that the PSTN profile actually flows into
        // the QamDemodulator's PLL, see
        // `state_machine::tests::test_pstn_profile_plumbing_enables_tracking_loops`.
        // For full closed-loop convergence on a V.34 channel, see the
        // Phase 1b B2B harness.
        //
        // Offset translation: 50 Hz at an 8000 Hz sample rate corresponds
        // to 2*pi*50/8000 ≈ 0.0393 rad/sample of phase advance.
        let sample_rate = 8000.0;
        let offset_hz = 50.0;
        let freq_offset = 2.0 * PI * offset_hz / sample_rate;

        let pstn_bw = crate::DspProfile::pstn().pll_bandwidth_hz;
        let mut pll = Pll::with_bandwidth(pstn_bw, sample_rate, 0.707);
        let mut true_phase = 0.0;

        // Drive the loop for 1 second of samples — plenty for a 50 Hz
        // bandwidth loop to lock.
        for _ in 0..sample_rate as u32 {
            true_phase += freq_offset;
            true_phase = wrap_phase(true_phase);
            let error = wrap_phase(true_phase - pll.phase());
            pll.update(error);
        }

        let freq_err = (pll.frequency() - freq_offset).abs();
        assert!(
            freq_err < freq_offset * 0.1,
            "PSTN-bandwidth PLL must converge to a 50 Hz offset within 10%: \
             target={freq_offset}, got={}, err={freq_err}",
            pll.frequency()
        );
    }

    #[test]
    fn test_tcp_profile_frozen_loop_does_not_track() {
        // Contrapositive guard: a 0 Hz bandwidth loop must NOT acquire the
        // frequency offset — this is the TCP profile's explicit design.
        // Confirms that disabling the tracking loop actually freezes it.
        let sample_rate = 8000.0;
        let freq_offset = 2.0 * PI * 50.0 / sample_rate;
        let mut pll = Pll::with_bandwidth(0.0, sample_rate, 0.707);
        let mut true_phase = 0.0;

        for _ in 0..sample_rate as u32 {
            true_phase += freq_offset;
            true_phase = wrap_phase(true_phase);
            let error = wrap_phase(true_phase - pll.phase());
            pll.update(error);
        }

        assert!(
            pll.frequency().abs() < 1e-9,
            "Zero-bandwidth (TCP-profile) PLL must remain frozen, got freq={}",
            pll.frequency()
        );
    }

    #[test]
    fn test_pll_set_bandwidth_preserves_phase() {
        // Lock the PLL to a known signal (constant frequency offset),
        // capture phase estimate, call set_bandwidth, and verify the
        // phase estimate hasn't jumped — only tracking dynamics change.
        let freq_offset = 0.02; // radians/sample
        let mut pll = Pll::with_bandwidth(50.0, 8000.0, 0.707);
        let mut true_phase = 0.0;

        // Drive to a steady-state lock.
        for _ in 0..6000 {
            true_phase += freq_offset;
            true_phase = wrap_phase(true_phase);
            let error = wrap_phase(true_phase - pll.phase());
            pll.update(error);
        }

        let phase_before = pll.phase();
        let freq_before = pll.frequency();

        // Change bandwidth — only coefficients should move.
        pll.set_bandwidth(10.0, 8000.0, 0.707);

        let phase_after = pll.phase();
        let freq_after = pll.frequency();

        assert_eq!(
            phase_before, phase_after,
            "set_bandwidth must not perturb phase: before={phase_before}, after={phase_after}"
        );
        assert_eq!(
            freq_before, freq_after,
            "set_bandwidth must not perturb freq: before={freq_before}, after={freq_after}"
        );
    }

    #[test]
    fn test_set_bandwidth_changes_gains() {
        let mut pll = Pll::with_bandwidth(50.0, 8000.0, 0.707);
        let kp_before = pll.kp;
        let ki_before = pll.ki;

        pll.set_bandwidth(10.0, 8000.0, 0.707);
        assert_ne!(pll.kp, kp_before, "kp should change after set_bandwidth");
        assert_ne!(pll.ki, ki_before, "ki should change after set_bandwidth");

        // Narrower bandwidth should produce smaller gains.
        assert!(
            pll.kp < kp_before,
            "Narrower bandwidth should reduce kp: {} vs {}",
            pll.kp,
            kp_before
        );
        assert!(
            pll.ki < ki_before,
            "Narrower bandwidth should reduce ki: {} vs {}",
            pll.ki,
            ki_before
        );
    }

    // ----- Non-finite-input rejection (Backlog #43) -----
    //
    // Same NaN-poison shape as `Oscillator::set_phase`: feeding NaN
    // through any of {kp, ki, bandwidth_hz, sample_rate, damping}
    // would silently corrupt `freq`/`phase` and propagate via
    // `update()`. Release-mode tests verify silent skip; debug-mode
    // tests verify the `debug_assert` fires.

    #[test]
    #[cfg(not(debug_assertions))]
    fn test_new_rejects_non_finite() {
        let pll = Pll::new(f64::NAN, f64::NAN);
        // Substituted with 0.0 → inactive PLL, but no NaN.
        assert!(pll.kp.is_finite() && pll.ki.is_finite());
        // Update with finite phase_error must yield finite phase.
        let mut pll = Pll::new(f64::INFINITY, 0.5);
        for _ in 0..50 {
            let p = pll.update(0.1);
            assert!(p.is_finite(), "phase non-finite after Pll::new(Inf, 0.5)");
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "non-finite kp")]
    fn test_new_debug_panics_on_nan_kp() {
        let _ = Pll::new(f64::NAN, 0.5);
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn test_with_bandwidth_rejects_non_finite() {
        let pll = Pll::with_bandwidth(f64::NAN, 8000.0, 0.707);
        assert!(pll.kp.is_finite() && pll.ki.is_finite());
        let mut pll = Pll::with_bandwidth(100.0, f64::INFINITY, 0.707);
        for _ in 0..50 {
            let p = pll.update(0.1);
            assert!(p.is_finite());
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "non-finite bandwidth_hz")]
    fn test_with_bandwidth_debug_panics_on_nan() {
        let _ = Pll::with_bandwidth(f64::NAN, 8000.0, 0.707);
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn test_set_bandwidth_rejects_non_finite() {
        let mut pll = Pll::with_bandwidth(100.0, 8000.0, 0.707);
        let kp_before = pll.kp;
        let ki_before = pll.ki;
        pll.set_bandwidth(f64::NAN, 8000.0, 0.707);
        // kp/ki must be unchanged on rejection.
        assert_eq!(pll.kp, kp_before);
        assert_eq!(pll.ki, ki_before);
        pll.set_bandwidth(100.0, f64::INFINITY, 0.707);
        assert_eq!(pll.kp, kp_before);
        assert_eq!(pll.ki, ki_before);
        // Phase must remain finite after subsequent updates.
        for _ in 0..50 {
            assert!(pll.update(0.1).is_finite());
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "non-finite damping")]
    fn test_set_bandwidth_debug_panics_on_nan_damping() {
        let mut pll = Pll::with_bandwidth(100.0, 8000.0, 0.707);
        pll.set_bandwidth(100.0, 8000.0, f64::NAN);
    }
}
