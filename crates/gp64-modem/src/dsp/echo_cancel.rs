//! NLMS (Normalized Least Mean Squares) adaptive echo canceller.
//!
//! Cancels line echo in the modem receive path by estimating the echo
//! impulse response with an adaptive FIR filter. A Geigel double-talk
//! detector (DTD) freezes tap adaptation when far-end signal is present,
//! preventing the filter from diverging during double-talk.
//!
//! ## Parameter rationale
//!
//! All default parameters are fixed by the V.34 PSTN interop HLD V4
//! Change 6. They should not be tuned without an HLD revision.
//!
//! - **Sample rate: 16 kHz.** The V.34 DSP runs at 16 kHz throughout.
//! - **Tail length: 2048 taps (128 ms).** HLD V4 Change 6 upgraded this
//!   from the earlier 8 kHz / 1024-tap draft to cover worst-case PSTN
//!   echo paths including VoIP-bridged routes where round-trip delay can
//!   exceed 100 ms.
//! - **NLMS step size mu = 0.2.** A conservative value that trades
//!   convergence speed for stability against noisy PSTN inputs.
//! - **NLMS regularization delta = 1.0.** Prevents division by zero and
//!   stabilises adaptation against quiet-TX intervals.
//! - **Geigel DTD threshold alpha = 0.5.** ITU-T G.168 recommends 0.71
//!   (-3 dB); we use 0.5 per HLD. This may be retuned in Stage 2 against
//!   real hardware.
//! - **DTD hangover: 30 ms (480 samples).** G.168 typical is 100-300 ms;
//!   we follow the HLD's shorter value and may retune in Stage 2.
//!
//! ## NLMS algorithm (per sample)
//!
//! ```text
//! insert tx_sample into tx_history
//! echo_estimate = dot(taps, tx_history)
//! error = rx_sample - echo_estimate
//! if not dtd_frozen:
//!     power = dot(tx_history, tx_history)
//!     norm = mu / (power + delta)
//!     for each tap: tap += norm * error * tx_history[i]
//! return error  // echo-cancelled signal
//! ```
//!
//! ## Known limitation: full-duplex data mode
//!
//! With a plain Geigel DTD the canceller is effectively frozen throughout
//! V.34 full-duplex data mode: both TX and RX are continuous, in-band
//! signals, so the Geigel ratio rx_peak/tx_peak rarely falls below alpha
//! once data transmission begins. In practice this means tap adaptation
//! runs during the half-duplex training phases (phase 2 / phase 3) and
//! then stays frozen for the remainder of the call. This matches the HLD
//! "Known Limitations (Deferred)" section and is adequate for the stable
//! PSTN echo paths we are targeting in Phase 1. A correlation-based DTD
//! is the follow-up path if Stage 2 testing reveals problems.
//!
//! ## TODO: DSP chain ordering
//!
//! The signal-chain ordering of AGC / echo canceller / equalizer is not
//! yet fixed by the modem state machine. This will be settled in
//! Phase 1c — see the 2026.04.23 supervisor journal.

/// Default NLMS step size.
const DEFAULT_MU: f64 = 0.2;

/// Default regularization to prevent division by zero in NLMS normalization.
const DEFAULT_DELTA: f64 = 1.0;

/// Default Geigel DTD threshold: freeze when rx_peak > threshold * tx_peak.
const DEFAULT_DTD_THRESHOLD: f64 = 0.5;

/// Default Geigel DTD sliding window size in samples (10ms @ 16kHz).
const DEFAULT_DTD_WINDOW: usize = 160;

/// Default DTD hangover duration in samples (30ms @ 16kHz).
const DEFAULT_DTD_HANGOVER: usize = 480;

/// NLMS adaptive echo canceller with Geigel double-talk detection.
pub struct EchoCanceller {
    /// Adaptive filter tap coefficients.
    taps: Vec<f64>,
    /// Transmit signal delay line (circular buffer, newest at `tx_head`).
    tx_history: Vec<f64>,
    /// Write position (head) in the tx_history circular buffer.
    tx_head: usize,
    /// Tail length (number of taps).
    tail_length: usize,
    /// NLMS step size.
    mu: f64,
    /// Regularization to prevent division by zero in NLMS normalization.
    delta: f64,
    /// Geigel DTD threshold: freeze when rx_peak > threshold * tx_peak.
    dtd_threshold: f64,
    /// Geigel DTD sliding window size in samples.
    dtd_window: usize,
    /// DTD hangover duration in samples.
    dtd_hangover: usize,
    /// Sliding window of recent |rx| values for DTD.
    rx_window: Vec<f64>,
    /// Write position in the rx_window circular buffer.
    rx_window_pos: usize,
    /// Whether DTD is currently frozen.
    dtd_frozen: bool,
    /// Hangover countdown (samples remaining).
    hangover_remaining: usize,
    /// Cached peak of |tx_history| for incremental DTD tracking.
    tx_peak_cached: f64,
    /// Age counter for tx_peak_cached (samples since last full rescan).
    tx_peak_age: usize,
    /// Cached peak of |rx_window| for incremental DTD tracking.
    rx_peak_cached: f64,
    /// Age counter for rx_peak_cached (samples since last full rescan).
    rx_peak_age: usize,
}

// Counts samples pushed through `EchoCanceller::process`, so a test can prove
// an idle line performs NO adaptive-filter work (MDD-BUG-CRUCIBLE-00512).
// Thread-local, so parallel tests never see each other's counts.
#[cfg(test)]
thread_local! {
    pub(crate) static ADAPTIVE_FILTER_SAMPLES: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

impl EchoCanceller {
    /// Create with specified tail length in samples (at 16kHz), using default
    /// NLMS and DTD parameters.
    pub fn new(tail_length: usize) -> Self {
        Self::new_with_params(
            tail_length,
            DEFAULT_MU,
            DEFAULT_DELTA,
            DEFAULT_DTD_THRESHOLD,
            DEFAULT_DTD_WINDOW,
            DEFAULT_DTD_HANGOVER,
        )
    }

    /// Create with fully configurable parameters.
    ///
    /// - `tail_length`: Number of adaptive filter taps (samples).
    /// - `mu`: NLMS step size (learning rate).
    /// - `delta`: NLMS regularization (prevents division by zero).
    /// - `dtd_threshold`: Geigel DTD threshold (freeze when rx_peak > threshold * tx_peak).
    /// - `dtd_window`: DTD sliding window size in samples.
    /// - `dtd_hangover`: DTD hangover duration in samples.
    pub fn new_with_params(
        tail_length: usize,
        mu: f64,
        delta: f64,
        dtd_threshold: f64,
        dtd_window: usize,
        dtd_hangover: usize,
    ) -> Self {
        assert!(tail_length > 0, "echo canceller needs at least 1 tap");
        assert!(dtd_window > 0, "DTD window must be at least 1 sample");
        Self {
            taps: vec![0.0; tail_length],
            tx_history: vec![0.0; tail_length],
            tx_head: 0,
            tail_length,
            mu,
            delta,
            dtd_threshold,
            dtd_window,
            dtd_hangover,
            rx_window: vec![0.0; dtd_window],
            rx_window_pos: 0,
            dtd_frozen: false,
            hangover_remaining: 0,
            tx_peak_cached: 0.0,
            tx_peak_age: 0,
            rx_peak_cached: 0.0,
            rx_peak_age: 0,
        }
    }

    /// Process one sample. Given the transmitted sample and received sample,
    /// returns the echo-cancelled received sample.
    pub fn process(&mut self, tx_sample: f64, rx_sample: f64) -> f64 {
        // Adaptive-filter work oracle (MDD-BUG-CRUCIBLE-00512): every call here
        // walks the tap array up to three times, so a caller that runs this on
        // an idle line is doing millions of iterations over zero histories.
        #[cfg(test)]
        ADAPTIVE_FILTER_SAMPLES.with(|n| n.set(n.get() + 1));

        // Insert new sample into tx_history circular buffer *before* the
        // echo estimate so that taps[k] aligns with delay k (no off-by-one).
        self.tx_head = if self.tx_head == 0 {
            self.tx_history.len() - 1
        } else {
            self.tx_head - 1
        };
        self.tx_history[self.tx_head] = tx_sample;

        // Compute echo estimate: dot(taps, tx_history).
        let echo_estimate = self.dot_taps_tx();
        let error = rx_sample - echo_estimate;

        // Update DTD state before tap adaptation.
        self.update_dtd(rx_sample);

        // NLMS tap adaptation (only when DTD is not frozen).
        if !self.dtd_frozen {
            let power = self.dot_tx_tx();
            let norm = self.mu / (power + self.delta);
            let len = self.tx_history.len();
            for i in 0..self.tail_length {
                self.taps[i] += norm * error * self.tx_history[(self.tx_head + i) % len];
            }
        }

        error
    }

    /// Test-only: are all adaptive taps still at their reset value?
    ///
    /// Used to prove the call-lifecycle reset actually drops the impulse
    /// response adapted to the previous call (MDD-BUG-CRUCIBLE-00512).
    #[cfg(test)]
    pub(crate) fn taps_are_zero(&self) -> bool {
        self.taps.iter().all(|t| *t == 0.0)
    }

    /// Whether the DTD is currently frozen (for diagnostics).
    pub fn is_frozen(&self) -> bool {
        self.dtd_frozen
    }

    /// Reset all state (taps, delay lines, DTD).
    pub fn reset(&mut self) {
        self.taps.fill(0.0);
        self.tx_history.fill(0.0);
        self.tx_head = 0;
        self.rx_window.fill(0.0);
        self.rx_window_pos = 0;
        self.dtd_frozen = false;
        self.hangover_remaining = 0;
        self.tx_peak_cached = 0.0;
        self.tx_peak_age = 0;
        self.rx_peak_cached = 0.0;
        self.rx_peak_age = 0;
    }

    /// Dot product of taps and tx_history (circular buffer).
    fn dot_taps_tx(&self) -> f64 {
        let mut sum = 0.0;
        let len = self.tx_history.len();
        for i in 0..self.tail_length {
            sum += self.taps[i] * self.tx_history[(self.tx_head + i) % len];
        }
        sum
    }

    /// Dot product of tx_history with itself (signal power, circular buffer).
    fn dot_tx_tx(&self) -> f64 {
        let mut sum = 0.0;
        let len = self.tx_history.len();
        for i in 0..self.tail_length {
            let v = self.tx_history[(self.tx_head + i) % len];
            sum += v * v;
        }
        sum
    }

    /// Update Geigel double-talk detector state.
    ///
    /// Uses incremental peak tracking: new samples are checked against the
    /// cached peak in O(1). A full rescan is performed only when the cached
    /// peak ages out (every 256 samples for tx, every 64 for rx), amortizing
    /// the scan cost.
    fn update_dtd(&mut self, rx_sample: f64) {
        // Update rx_window circular buffer.
        let rx_abs = rx_sample.abs();
        self.rx_window[self.rx_window_pos] = rx_abs;
        self.rx_window_pos = (self.rx_window_pos + 1) % self.dtd_window;

        // Incremental rx_peak tracking.
        //
        // `rx_peak_age` counts samples since the last *full rescan* — not
        // since the cache was last updated. A new sample that meets or exceeds
        // the cached peak updates the cache (cheap O(1)) but does NOT reset
        // the age: otherwise a non-decreasing input would latch the cache
        // indefinitely and the 64-sample rescan budget would never fire.
        self.rx_peak_age += 1;
        if rx_abs >= self.rx_peak_cached {
            self.rx_peak_cached = rx_abs;
        }
        if self.rx_peak_age > 64 {
            // Full rescan of rx_window.
            self.rx_peak_cached = self.rx_window.iter().copied().fold(0.0_f64, f64::max);
            self.rx_peak_age = 0;
        }

        // Incremental tx_peak tracking (over the entire tx_history tail).
        // The newest tx_sample was already inserted at tx_head before this
        // call, so tx_history[tx_head] is the current sample. Same latch-bug
        // fix as above: `tx_peak_age` counts samples since the last full
        // rescan. Note the tx path is additionally self-limiting because
        // tx_history wraps at the tap-buffer length, but we keep the logic
        // consistent with the rx path.
        let tx_abs = self.tx_history[self.tx_head].abs();
        self.tx_peak_age += 1;
        if tx_abs >= self.tx_peak_cached {
            self.tx_peak_cached = tx_abs;
        }
        if self.tx_peak_age > 256 {
            // Full rescan of tx_history.
            self.tx_peak_cached = self
                .tx_history
                .iter()
                .copied()
                .fold(0.0_f64, |acc, v| acc.max(v.abs()));
            self.tx_peak_age = 0;
        }

        let rx_peak = self.rx_peak_cached;
        let tx_peak = self.tx_peak_cached;

        if rx_peak > self.dtd_threshold * tx_peak {
            self.dtd_frozen = true;
            self.hangover_remaining = self.dtd_hangover;
        } else if self.hangover_remaining > 0 {
            self.hangover_remaining -= 1;
            self.dtd_frozen = true;
        } else {
            self.dtd_frozen = false;
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// Deterministic LCG producing samples in [-1.0, 1.0) — seeded per test
    /// so runs are reproducible (no proptest, no wall clock).
    fn white_noise(seed: u64, n: usize) -> Vec<f64> {
        let mut out = vec![0.0_f64; n];
        let mut s = seed;
        for v in out.iter_mut() {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *v = (s >> 33) as f64 / (1u64 << 31) as f64 - 1.0;
        }
        out
    }

    /// L2 norm of a slice.
    fn l2_norm(xs: &[f64]) -> f64 {
        xs.iter().map(|x| x * x).sum::<f64>().sqrt()
    }

    /// L2 distance between two equal-length slices.
    fn l2_distance(a: &[f64], b: &[f64]) -> f64 {
        assert_eq!(a.len(), b.len());
        a.iter()
            .zip(b.iter())
            .map(|(x, y)| (x - y) * (x - y))
            .sum::<f64>()
            .sqrt()
    }

    /// RMS of a slice (sqrt of mean square).
    fn rms(xs: &[f64]) -> f64 {
        if xs.is_empty() {
            return 0.0;
        }
        (xs.iter().map(|x| x * x).sum::<f64>() / xs.len() as f64).sqrt()
    }

    /// Simple one-pole IIR low-pass filter (y[n] = a*y[n-1] + (1-a)*x[n]).
    /// Used to colour white-noise TX so convergence tests are not operating
    /// on the degenerate flat-spectrum input that LMS handles trivially.
    fn low_pass(xs: &[f64], alpha: f64) -> Vec<f64> {
        let mut out = vec![0.0_f64; xs.len()];
        let mut y = 0.0_f64;
        for (i, &x) in xs.iter().enumerate() {
            y = alpha * y + (1.0 - alpha) * x;
            out[i] = y;
        }
        out
    }

    /// Test 1 (per working plan / HLD V4 Change 6):
    /// Realistic multi-tap colored echo path. TX is low-pass-filtered white
    /// noise (modem-band-like spectrum), RX is the convolution of TX with a
    /// multi-tap impulse response whose taps are scattered across the delay
    /// range (5, 12, 18, 25 samples at our 16 kHz effective test rate),
    /// with exponentially decaying magnitudes. This exercises the adaptive
    /// filter on something closer to a real PSTN echo response than a
    /// single-tap delay.
    ///
    /// After a convergence window, residual RMS must be at least 20 dB below
    /// the echo RMS (ERLE >= 20 dB).
    #[test]
    fn test_converges_on_synthetic_echo() {
        // Taps must be long enough to cover the deepest echo tap (25) with
        // margin for the FIR to settle.
        let tail = 64;
        let n = 16_000;

        // Multi-tap coloured impulse response. (delay_samples, gain).
        // Total L2 magnitude sqrt(0.09+0.0144+0.0036+0.0009) ~= 0.346, and
        // the peak single tap is 0.30 — well below the Geigel DTD threshold
        // of 0.5 so the DTD does not falsely fire on this pure-echo scenario.
        let echo_taps: &[(usize, f64)] = &[(5, 0.30), (12, 0.12), (18, 0.06), (25, 0.03)];

        // TX: low-pass-filtered white noise so the input is not spectrally
        // flat. alpha = 0.6 gives a gentle roll-off reminiscent of a
        // band-limited modem signal.
        let raw = white_noise(0xC0FFEE_u64, n);
        let tx = low_pass(&raw, 0.6);

        // RX: convolution of TX with the echo response. No near-end signal.
        let max_delay = echo_taps.iter().map(|(d, _)| *d).max().unwrap();
        let mut rx = vec![0.0_f64; n];
        for i in max_delay..n {
            let mut s = 0.0_f64;
            for &(d, g) in echo_taps {
                s += g * tx[i - d];
            }
            rx[i] = s;
        }

        let mut ec = EchoCanceller::new(tail);
        let mut out = vec![0.0_f64; n];
        for i in 0..n {
            out[i] = ec.process(tx[i], rx[i]);
        }

        // Measure the last 2000 samples (after convergence). Coloured inputs
        // need a larger convergence window than flat white noise.
        let measure_start = n - 2000;
        let echo_rms = rms(&rx[measure_start..]);
        let residual_rms = rms(&out[measure_start..]);
        let erle_db = 20.0 * (echo_rms / residual_rms.max(1e-30)).log10();

        assert!(
            erle_db >= 20.0,
            "ERLE should be >= 20 dB on multi-tap coloured echo, got {erle_db:.1} dB (echo_rms={echo_rms:.6}, residual_rms={residual_rms:.6})"
        );
    }

    /// Test 2: snapshot taps after convergence, then inject strong near-end
    /// speech at a non-echo frequency. The DTD must freeze adaptation so
    /// taps do not drift significantly during double-talk.
    ///
    /// Strategy: run two canceller instances in parallel — one with the DTD
    /// enabled and one with the DTD effectively disabled (threshold=inf). Both
    /// see the same training and double-talk inputs. The DTD-enabled
    /// instance's tap drift must be far smaller than the DTD-disabled
    /// instance's. This directly exercises "the DTD prevents tap
    /// destruction" rather than depending on a brittle absolute drift bound
    /// (which depends on the DTD's rx_peak latch latency, a few samples).
    #[test]
    fn test_dtd_freezes_adaptation() {
        let tail = 64;
        let echo_delay = 8;
        let echo_gain = 0.3; // well below DTD threshold (0.5)
        let n_train = 6000;

        let mut ec_dtd = EchoCanceller::new(tail);
        let mut ec_ref = EchoCanceller::new_with_params(
            tail,
            DEFAULT_MU,
            DEFAULT_DELTA,
            f64::INFINITY, // DTD effectively disabled in the reference
            DEFAULT_DTD_WINDOW,
            DEFAULT_DTD_HANGOVER,
        );

        // Phase 1: train on pure echo until taps converge (both cancellers).
        let tx_train = white_noise(0x1234_u64, n_train);
        let mut rx_train = vec![0.0_f64; n_train];
        for i in echo_delay..n_train {
            rx_train[i] = echo_gain * tx_train[i - echo_delay];
        }
        for i in 0..n_train {
            ec_dtd.process(tx_train[i], rx_train[i]);
            ec_ref.process(tx_train[i], rx_train[i]);
        }
        let taps_converged: Vec<f64> = ec_dtd.taps.clone();
        let converged_norm = l2_norm(&taps_converged);
        assert!(
            converged_norm > 0.1,
            "taps should have non-trivial energy after training (got ||taps||={converged_norm:.4})"
        );
        // Both instances should have converged to similar taps (sanity).
        let conv_diff = l2_distance(&ec_dtd.taps, &ec_ref.taps) / converged_norm;
        assert!(
            conv_diff < 0.05,
            "DTD-on and DTD-off cancellers should converge to the same taps on pure-echo training, got {:.2}% difference",
            conv_diff * 100.0
        );

        // Phase 2: pulsed speech-like double-talk. Sustained-tone near-end
        // triggers Geigel once and never re-evaluates (rx_peak latches); we
        // want to exercise the hangover path explicitly, so the near-end is
        // bursted 100 ms on / 100 ms off (1600 samples on, 1600 samples off
        // at 16 kHz) across at least 3 on/off cycles. Each burst must
        // re-trigger the DTD; hangover holds through each gap.
        let burst_len = 1600; // 100 ms @ 16 kHz
        let gap_len = 1600; // 100 ms @ 16 kHz
        let n_cycles = 4; // > 3 on/off cycles as required
        let n_dt = n_cycles * (burst_len + gap_len);

        let tx_dt = white_noise(0x5678_u64, n_dt);
        let tail_tx: Vec<f64> = tx_train[n_train - echo_delay..].to_vec();

        // Track that the DTD fires during at least one burst and holds
        // through at least one gap (not just at the very end).
        let mut dtd_seen_frozen_in_burst = false;
        let mut dtd_seen_frozen_in_gap = false;

        for (i, &tx_s) in tx_dt.iter().enumerate() {
            let echo_src = if i < echo_delay {
                tail_tx[i]
            } else {
                tx_dt[i - echo_delay]
            };
            let echo_component = echo_gain * echo_src;

            // Burst/gap position within the cycle.
            let phase = i % (burst_len + gap_len);
            let in_burst = phase < burst_len;
            let near_end = if in_burst {
                // Speech-like near-end: colored noise at substantial level.
                // Use a fresh LCG draw per sample so it is not a pure tone.
                let s = (i as u64)
                    .wrapping_mul(2862933555777941757)
                    .wrapping_add(3037000493);
                let raw = (s >> 33) as f64 / (1u64 << 31) as f64 - 1.0;
                0.8 * raw
            } else {
                0.0
            };
            let rx_s = echo_component + near_end;
            ec_dtd.process(tx_s, rx_s);
            ec_ref.process(tx_s, rx_s);

            // Sample the DTD state mid-burst, and *inside hangover* during
            // the gap (hangover is 480 samples; sample at burst_len + 200
            // which is well inside hangover and beyond any latched-peak
            // transient). Skip the very first cycle to let the peak
            // tracker settle.
            if i > burst_len + gap_len {
                if in_burst && phase == burst_len / 2 && ec_dtd.is_frozen() {
                    dtd_seen_frozen_in_burst = true;
                }
                if !in_burst && phase == burst_len + 200 && ec_dtd.is_frozen() {
                    dtd_seen_frozen_in_gap = true;
                }
            }
        }

        assert!(
            dtd_seen_frozen_in_burst,
            "DTD must be frozen during near-end bursts"
        );
        assert!(
            dtd_seen_frozen_in_gap,
            "DTD hangover must keep the canceller frozen through inter-burst gaps"
        );
        assert!(
            !ec_ref.is_frozen(),
            "reference (DTD-disabled) must not be frozen"
        );

        let drift_dtd = l2_distance(&ec_dtd.taps, &taps_converged);
        let drift_ref = l2_distance(&ec_ref.taps, &taps_converged);

        // The reference (no DTD) should have drifted meaningfully — the
        // filter is being corrupted by the near-end signal. With pulsed
        // near-end (50% duty cycle) the drift is smaller than a sustained
        // near-end would produce, but still needs to be non-trivial for
        // the DTD-vs-no-DTD ratio assertion below to be meaningful.
        assert!(
            drift_ref / converged_norm > 0.10,
            "reference drift should be non-trivial without DTD (got {:.2}%)",
            100.0 * drift_ref / converged_norm
        );

        // The DTD instance's drift must be at least 5x smaller than the
        // reference — this is the real thing we want to verify.
        assert!(
            drift_dtd * 5.0 < drift_ref,
            "DTD must reduce tap drift by >=5x vs no-DTD reference: drift_dtd={drift_dtd:.4}, drift_ref={drift_ref:.4} (ratio {:.2}x)",
            drift_ref / drift_dtd.max(1e-30)
        );
    }

    /// Test 3: no echo path — TX and RX are uncorrelated signals (different
    /// seeds, independent generation). A correct canceller should not be
    /// able to model RX from TX, so taps should stay small and the output
    /// should closely track RX (not diverge trying to predict an
    /// independent signal).
    ///
    /// This is a stronger claim than "TX=0 -> output=RX", which is a
    /// trivial arithmetic no-op that exercises nothing in the algorithm.
    #[test]
    fn test_no_echo_passthrough() {
        let n = 4000;
        let mut ec = EchoCanceller::new(64);

        // Modem-like TX: low-pass-filtered white noise (band-limited).
        let tx = low_pass(&white_noise(0x1111_u64, n), 0.6);
        // Independent near-end RX: different seed, also low-passed so it
        // has a realistic spectrum rather than a flat one.
        let rx = low_pass(&white_noise(0x2222_u64, n), 0.6);

        let mut out = vec![0.0_f64; n];
        for i in 0..n {
            out[i] = ec.process(tx[i], rx[i]);
        }

        // Taps should remain small — there is no echo structure to learn.
        // NLMS with uncorrelated TX/RX still produces gradient noise, but
        // the tap norm should stay bounded and well below the echo-case
        // norm (converged echo-path norm >= 0.1, see
        // test_dtd_freezes_adaptation).
        let tap_norm = l2_norm(&ec.taps);
        assert!(
            tap_norm < 0.1,
            "taps must remain small with uncorrelated TX/RX, got ||taps||={tap_norm:.4}"
        );

        // Output should closely track RX. Quantify as the ratio of residual
        // to input RMS: since there is nothing to subtract, out[i] ~ rx[i].
        let residual_rms = rms(&out
            .iter()
            .zip(rx.iter())
            .map(|(o, r)| o - r)
            .collect::<Vec<_>>());
        let rx_rms = rms(&rx);
        assert!(
            residual_rms / rx_rms.max(1e-30) < 0.2,
            "output must closely track RX (residual/rx RMS = {:.4})",
            residual_rms / rx_rms.max(1e-30)
        );
    }

    /// Test 4: after DTD fires, taps must remain frozen for the configured
    /// hangover period — even once the near-end signal has ended. We verify
    /// by snapshotting taps at hangover start and again halfway through.
    #[test]
    fn test_hangover_keeps_taps_frozen() {
        let tail = 64;
        let echo_delay = 8;
        let echo_gain = 0.3;
        let n_train = 6000;

        let mut ec = EchoCanceller::new(tail);

        // Train on echo so taps are non-trivial.
        let tx_train = white_noise(0xDEAD_u64, n_train);
        let mut rx_train = vec![0.0_f64; n_train];
        for i in echo_delay..n_train {
            rx_train[i] = echo_gain * tx_train[i - echo_delay];
        }
        for i in 0..n_train {
            ec.process(tx_train[i], rx_train[i]);
        }

        // Trigger DTD with a strong near-end burst, then stop the near-end.
        for i in 0..DEFAULT_DTD_WINDOW {
            let tx = 0.5 * (i as f64 * 0.2).sin();
            let rx = 0.9 * (i as f64 * 0.37).cos();
            ec.process(tx, rx);
        }
        assert!(
            ec.is_frozen(),
            "DTD must be frozen at end of near-end burst"
        );

        // Snapshot taps at the start of the hangover.
        let taps_at_hangover_start: Vec<f64> = ec.taps.clone();

        // Feed TX only (no near-end). Walk half the hangover.
        let half = DEFAULT_DTD_HANGOVER / 2;
        for i in 0..half {
            let tx = 0.5 * ((1000 + i) as f64 * 0.2).sin();
            ec.process(tx, 0.0);
            assert!(
                ec.is_frozen(),
                "DTD must still be frozen inside the hangover period at i={i}"
            );
        }
        // During hangover, dtd_frozen == true means the tap-update branch
        // is not executed at all — so drift must be *algorithmically* zero,
        // not merely "small". Any non-zero drift indicates a bug (e.g. the
        // update path being entered when it should be skipped).
        let drift_midway = l2_distance(&ec.taps, &taps_at_hangover_start);
        assert_eq!(
            drift_midway, 0.0,
            "taps must not drift at all during hangover (got {drift_midway:.6e})"
        );

        // Continue through the rest of the hangover, plus enough time for
        // the rx_window (DTD_WINDOW samples) to refill with zeros and for
        // at least one rescan (up-to-64-sample budget) to pick up the
        // now-zero peak. The rx_peak_age latch-bug fix doesn't remove this
        // requirement: it corrects cache-staleness on non-decreasing
        // inputs, not the fundamental latency of flushing the sliding
        // window after a burst ends.
        for i in 0..(DEFAULT_DTD_HANGOVER - half + DEFAULT_DTD_WINDOW + 64) {
            let tx = 0.5 * ((2000 + i) as f64 * 0.2).sin();
            ec.process(tx, 0.0);
        }
        assert!(
            !ec.is_frozen(),
            "DTD must unfreeze after hangover + rx_window flush"
        );
    }

    // ---- Extra coverage (not in the required list but useful). ---------

    #[test]
    fn test_reset_clears_state() {
        let mut ec = EchoCanceller::new(64);
        for i in 0..500 {
            let tx = 0.5 * (i as f64 * 0.1).sin();
            let rx = 0.3 * (i as f64 * 0.1).sin();
            ec.process(tx, rx);
        }
        ec.reset();
        assert!(!ec.is_frozen());
        let out = ec.process(1.0, 0.0);
        assert!(out.abs() < 1e-12);
    }

    #[test]
    fn test_tap_alignment_no_offset() {
        // Guards against the off-by-one the implementation previously had:
        // on a single-tap echo rx[n] = g*tx[n-D], learned energy must land
        // at taps[D], not taps[D+1].
        let delay = 5;
        let gain = 0.4;
        let tail = 32;
        let num_samples = 8000;

        let mut ec = EchoCanceller::new_with_params(
            tail,
            DEFAULT_MU,
            DEFAULT_DELTA,
            100.0, // disable DTD
            DEFAULT_DTD_WINDOW,
            DEFAULT_DTD_HANGOVER,
        );

        let tx_signal = white_noise(0x98765_u64, num_samples);
        let mut rx_signal = vec![0.0_f64; num_samples];
        for i in delay..num_samples {
            rx_signal[i] = gain * tx_signal[i - delay];
        }
        for i in 0..num_samples {
            ec.process(tx_signal[i], rx_signal[i]);
        }

        let dominant_tap = ec.taps[delay];
        assert!(
            (dominant_tap - gain).abs() < 0.05,
            "taps[{delay}] should be ~= {gain}, got {dominant_tap:.6}"
        );
        for (i, &tap) in ec.taps.iter().enumerate() {
            if i == delay {
                continue;
            }
            assert!(tap.abs() < 0.05, "taps[{i}] should be ~= 0, got {tap:.6}");
        }
    }

    #[test]
    fn test_dtd_not_triggered_by_echo_only() {
        // Echo at 0.3 gain is below the 0.5 Geigel threshold — DTD must not
        // falsely freeze on a pure-echo scenario.
        let mut ec = EchoCanceller::new(64);
        let echo_delay = 5;
        let echo_gain = 0.3;
        for i in 0..500 {
            let tx = 0.8 * (i as f64 * 0.1).sin();
            let rx = if i >= echo_delay {
                echo_gain * 0.8 * ((i - echo_delay) as f64 * 0.1).sin()
            } else {
                0.0
            };
            ec.process(tx, rx);
        }
        assert!(!ec.is_frozen());
    }
}
