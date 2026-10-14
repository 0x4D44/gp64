//! Goertzel algorithm for efficient single-frequency energy detection.
//!
//! This module provides two layers:
//!
//! 1. [`Goertzel`] — single-bin detector. Given a block of samples,
//!    returns the magnitude-squared at the target frequency, plus a
//!    normalised "tone-ness" metric bounded to `[0, 1]`.
//!
//! 2. [`MultiToneAnalyzer`] — a bank of `Goertzel`s evaluated in one
//!    pass over the input block. Used by V.34 line probing to recover
//!    the per-tone magnitude spectrum of the simultaneously-transmitted
//!    probing signal (see `v34_training.rs` Phase 2).
//!
//! Goertzel is the right tool here instead of FFT: we only care about
//! 21 known bins, the block length is not a power of two, and the per-
//! bin cost is `O(N)` regardless of bin placement. For 21 bins × 80
//! samples per 10 ms tick this is trivial (one multiply-add per sample
//! per bin on the hot path).

use std::f64::consts::TAU;

/// Goertzel filter for detecting a specific frequency in a block of samples.
pub struct Goertzel {
    /// Precomputed coefficient: 2 * cos(2*pi * freq / sample_rate).
    coeff: f64,
    /// Target frequency in Hz (stored for diagnostics).
    #[allow(dead_code)]
    target_freq: f64,
    /// Sample rate in Hz (stored for diagnostics).
    #[allow(dead_code)]
    sample_rate: u32,
}

impl Goertzel {
    /// Create a Goertzel detector for the given frequency.
    pub fn new(frequency: f64, sample_rate: u32) -> Self {
        let normalized_freq = frequency / sample_rate as f64;
        Self {
            coeff: 2.0 * (TAU * normalized_freq).cos(),
            target_freq: frequency,
            sample_rate,
        }
    }

    /// Compute the energy at the target frequency for a block of samples.
    /// Returns the magnitude squared (proportional to energy).
    pub fn energy(&self, samples: &[i16]) -> f64 {
        let mut s1: f64 = 0.0;
        let mut s2: f64 = 0.0;

        for &sample in samples {
            let s0 = self.coeff * s1 - s2 + sample as f64;
            s2 = s1;
            s1 = s0;
        }

        // Magnitude squared from final state
        s1 * s1 + s2 * s2 - self.coeff * s1 * s2
    }

    /// Compute the normalized energy: ratio of target frequency energy to
    /// total signal energy. Range [0.0, 1.0]. Values > 0.5 strongly
    /// indicate a pure tone at the target frequency.
    pub fn normalized_energy(&self, samples: &[i16]) -> f64 {
        if samples.is_empty() {
            return 0.0;
        }

        let total_energy: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
        if total_energy == 0.0 {
            return 0.0;
        }

        let goertzel_energy = self.energy(samples);

        // For a pure tone of amplitude A over N samples:
        //   Goertzel energy ≈ N^2 * A^2 / 4
        //   Total energy (sum of squares) ≈ N * A^2 / 2
        // So the ratio goertzel / (total * N/2) ≈ 1.0 for a pure tone.
        let n = samples.len() as f64;
        let normalized = goertzel_energy / (total_energy * n / 2.0);

        // Clamp to [0, 1] to handle floating-point edge cases
        normalized.clamp(0.0, 1.0)
    }
}

// ---------------------------------------------------------------------------
// Multi-tone analyzer
// ---------------------------------------------------------------------------

/// A bank of [`Goertzel`] detectors evaluated in a single pass over the
/// input block.
///
/// Intended use: V.34 Phase 2 line-probing RX, where the caller
/// transmits N tones simultaneously (see V.34 Table 2 / spandsp
/// `src/v34.c`). The answerer measures per-tone magnitude to estimate
/// the channel response and picks a pre-emphasis index from it.
///
/// ## API
///
/// - `new(frequencies, sample_rate)` — precomputes one Goertzel
///   coefficient per tone.
/// - `magnitudes(samples)` — returns magnitude per tone, in the same
///   order as `frequencies` passed to `new`. Magnitudes are
///   `sqrt(energy)` in the same units as the input samples, i.e. for a
///   block of N samples of a unit-amplitude sinusoid the magnitude is
///   approximately `N/2`.
/// - `normalized_magnitudes(samples)` — returns each tone's normalised
///   energy ratio in `[0, 1]` (reuses [`Goertzel::normalized_energy`]).
///   This is the form consumed by the existing pre-emphasis selector.
/// - `frequencies()` — accessor for the tone frequencies, useful for
///   diagnostics and downstream logic that wants to pair a magnitude
///   with its bin.
///
/// ## Performance
///
/// `magnitudes` is O(N × T) where N = block length and T = tone count.
/// For V.34 probing (80-sample ticks × 21 tones) this is ~1.7 k
/// multiply-adds per tick — negligible compared to the QAM path.
pub struct MultiToneAnalyzer {
    detectors: Vec<Goertzel>,
    frequencies: Vec<f64>,
}

impl MultiToneAnalyzer {
    /// Build an analyzer for the given tone frequencies.
    pub fn new(frequencies: &[f64], sample_rate: u32) -> Self {
        let detectors = frequencies
            .iter()
            .map(|&freq| Goertzel::new(freq, sample_rate))
            .collect();
        Self {
            detectors,
            frequencies: frequencies.to_vec(),
        }
    }

    /// Number of tone bins in the bank.
    pub fn len(&self) -> usize {
        self.detectors.len()
    }

    /// True if the analyzer has no bins (should not happen in practice).
    pub fn is_empty(&self) -> bool {
        self.detectors.is_empty()
    }

    /// The tone frequencies in bin order.
    pub fn frequencies(&self) -> &[f64] {
        &self.frequencies
    }

    /// Compute the magnitude at each tone frequency for the given block.
    ///
    /// Magnitude is the square root of Goertzel energy — i.e. it scales
    /// linearly with input amplitude. Returned in the same order as the
    /// frequencies passed to `new`.
    pub fn magnitudes(&self, samples: &[i16]) -> Vec<f64> {
        self.detectors
            .iter()
            .map(|det| det.energy(samples).max(0.0).sqrt())
            .collect()
    }

    /// Compute the normalised (`[0, 1]`) tone-ness at each frequency.
    ///
    /// Equivalent to calling `normalized_energy` on each detector. A
    /// value near 1.0 means the bin's frequency dominates the block;
    /// near 0.0 means it's absent. Used by V.34 probing to decide how
    /// many of the expected tones were present above the noise floor.
    pub fn normalized_magnitudes(&self, samples: &[i16]) -> Vec<f64> {
        self.detectors
            .iter()
            .map(|det| det.normalized_energy(samples))
            .collect()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    const SAMPLE_RATE: u32 = 8000;

    /// Generate a pure tone as i16 samples.
    fn generate_tone(
        frequency: f64,
        sample_rate: u32,
        num_samples: usize,
        amplitude: f64,
    ) -> Vec<i16> {
        (0..num_samples)
            .map(|i| {
                let t = i as f64 / sample_rate as f64;
                (amplitude * (TAU * frequency * t).sin()) as i16
            })
            .collect()
    }

    #[test]
    fn test_goertzel_detects_pure_tone() {
        let detector = Goertzel::new(1100.0, SAMPLE_RATE);
        let samples = generate_tone(1100.0, SAMPLE_RATE, 800, 16000.0);
        let energy = detector.energy(&samples);
        assert!(
            energy > 1e6,
            "Expected high energy for matching tone, got {energy}"
        );
    }

    #[test]
    fn test_goertzel_rejects_wrong_frequency() {
        let detector = Goertzel::new(1100.0, SAMPLE_RATE);

        // Energy at 1100 Hz for a 2100 Hz tone
        let samples = generate_tone(2100.0, SAMPLE_RATE, 800, 16000.0);
        let energy_wrong = detector.energy(&samples);

        // Energy at 1100 Hz for a 1100 Hz tone (reference)
        let samples_right = generate_tone(1100.0, SAMPLE_RATE, 800, 16000.0);
        let energy_right = detector.energy(&samples_right);

        // The wrong-frequency energy should be much lower
        assert!(
            energy_wrong < energy_right * 0.01,
            "Wrong frequency energy ({energy_wrong}) should be <<< matching energy ({energy_right})"
        );
    }

    #[test]
    fn test_goertzel_silence_zero_energy() {
        let detector = Goertzel::new(1100.0, SAMPLE_RATE);
        let samples = vec![0i16; 800];
        let energy = detector.energy(&samples);
        assert!(
            energy.abs() < 1e-10,
            "Silence should produce zero energy, got {energy}"
        );
    }

    #[test]
    fn test_goertzel_normalized_pure_tone() {
        let detector = Goertzel::new(1100.0, SAMPLE_RATE);
        let samples = generate_tone(1100.0, SAMPLE_RATE, 800, 16000.0);
        let norm = detector.normalized_energy(&samples);
        assert!(
            norm > 0.8,
            "Normalized energy for pure tone should be > 0.8, got {norm}"
        );
    }

    #[test]
    fn test_goertzel_normalized_noise() {
        let detector = Goertzel::new(1100.0, SAMPLE_RATE);

        // Generate pseudo-random noise using a simple LCG
        let mut rng_state: u32 = 12345;
        let samples: Vec<i16> = (0..800)
            .map(|_| {
                rng_state = rng_state.wrapping_mul(1103515245).wrapping_add(12345);
                ((rng_state >> 16) as i16).wrapping_mul(2) // Spread the range
            })
            .collect();

        let norm = detector.normalized_energy(&samples);
        assert!(
            norm < 0.2,
            "Normalized energy for noise should be < 0.2, got {norm}"
        );
    }

    #[test]
    fn test_goertzel_detects_in_mix() {
        let detector = Goertzel::new(1100.0, SAMPLE_RATE);
        let n = 800;

        // Mix 1100 Hz + 500 Hz at equal amplitudes
        let mixed: Vec<i16> = (0..n)
            .map(|i| {
                let t = i as f64 / SAMPLE_RATE as f64;
                let s1 = 10000.0 * (TAU * 1100.0 * t).sin();
                let s2 = 10000.0 * (TAU * 500.0 * t).sin();
                (s1 + s2) as i16
            })
            .collect();

        // Should still detect significant energy at 1100 Hz
        let energy_mixed = detector.energy(&mixed);
        let energy_pure = detector.energy(&generate_tone(1100.0, SAMPLE_RATE, n, 10000.0));

        // The mixed signal should have similar 1100 Hz energy to the pure tone
        // (within an order of magnitude — the 500 Hz component doesn't affect 1100 Hz much)
        assert!(
            energy_mixed > energy_pure * 0.5,
            "1100 Hz energy in mix ({energy_mixed}) should be close to pure ({energy_pure})"
        );
    }

    #[test]
    fn test_goertzel_normalized_empty_samples() {
        let detector = Goertzel::new(1100.0, SAMPLE_RATE);
        let norm = detector.normalized_energy(&[]);
        assert_eq!(norm, 0.0, "Empty samples should return 0.0");
    }

    #[test]
    fn test_goertzel_normalized_silence() {
        let detector = Goertzel::new(1100.0, SAMPLE_RATE);
        let samples = vec![0i16; 800];
        let norm = detector.normalized_energy(&samples);
        assert_eq!(norm, 0.0, "Silence should return 0.0 normalized energy");
    }

    #[test]
    fn test_goertzel_energy_scales_with_amplitude() {
        let detector = Goertzel::new(1100.0, SAMPLE_RATE);
        let samples_low = generate_tone(1100.0, SAMPLE_RATE, 800, 1000.0);
        let samples_high = generate_tone(1100.0, SAMPLE_RATE, 800, 10000.0);
        let energy_low = detector.energy(&samples_low);
        let energy_high = detector.energy(&samples_high);
        // Energy scales as amplitude^2, so 10x amplitude -> 100x energy
        let ratio = energy_high / energy_low;
        assert!(
            ratio > 50.0 && ratio < 150.0,
            "Energy should scale ~100x for 10x amplitude, got {ratio:.1}x"
        );
    }

    #[test]
    fn test_goertzel_normalized_clamped() {
        let detector = Goertzel::new(1100.0, SAMPLE_RATE);
        let samples = generate_tone(1100.0, SAMPLE_RATE, 800, 16000.0);
        let norm = detector.normalized_energy(&samples);
        assert!(
            norm >= 0.0 && norm <= 1.0,
            "Normalized energy should be in [0,1], got {norm}"
        );
    }

    #[test]
    fn test_goertzel_different_frequencies() {
        // Verify detection at various frequencies
        for freq in [500.0, 1000.0, 1500.0, 2000.0, 3000.0] {
            let detector = Goertzel::new(freq, SAMPLE_RATE);
            let samples = generate_tone(freq, SAMPLE_RATE, 800, 16000.0);
            let norm = detector.normalized_energy(&samples);
            assert!(
                norm > 0.7,
                "Should detect {freq} Hz tone, normalized={norm}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // MultiToneAnalyzer tests
    // -----------------------------------------------------------------------

    /// Feed a known sum of sinusoids with unequal amplitudes into the
    /// analyzer. Each bin's recovered magnitude must be proportional to
    /// its input amplitude (within ~5%).
    #[test]
    fn test_multi_tone_analyzer_recovers_known_amplitudes() {
        let frequencies = [500.0, 1000.0, 1500.0, 2000.0, 2500.0];
        let amplitudes = [1000.0, 2000.0, 3000.0, 4000.0, 5000.0];
        let n = 1600_usize; // 200 ms at 8 kHz — enough for stable bins

        let samples: Vec<i16> = (0..n)
            .map(|i| {
                let t = i as f64 / SAMPLE_RATE as f64;
                let sum: f64 = frequencies
                    .iter()
                    .zip(amplitudes.iter())
                    .map(|(&f, &a)| a * (TAU * f * t).sin())
                    .sum();
                sum as i16
            })
            .collect();

        let analyzer = MultiToneAnalyzer::new(&frequencies, SAMPLE_RATE);
        let mags = analyzer.magnitudes(&samples);

        assert_eq!(mags.len(), frequencies.len());

        // Normalise against amplitude[0] so we test ratios, not absolute
        // Goertzel units. Expected ratios are amplitudes[k] / amplitudes[0].
        let reference_ratio = mags[0] / amplitudes[0];
        for (i, &amp) in amplitudes.iter().enumerate() {
            let expected = amp * reference_ratio;
            let err = ((mags[i] - expected) / expected).abs();
            assert!(
                err < 0.05,
                "bin {i} (f={} Hz, amp={amp}): magnitude {:.1} vs expected {:.1} ({:.1}% error)",
                frequencies[i],
                mags[i],
                expected,
                err * 100.0
            );
        }
    }

    /// Feed a pure tone at a frequency NOT in the probe list. Every
    /// probe-bin magnitude must be small compared to the magnitude
    /// that bin would have shown if its own frequency had been injected
    /// at the same amplitude (frequency isolation / no leakage).
    #[test]
    fn test_multi_tone_analyzer_rejects_out_of_band() {
        // Probe bins at 600, 1200, 1800, 2400 Hz. Inject 3300 Hz — well
        // clear of every bin (≥ 900 Hz away from nearest).
        let frequencies = [600.0, 1200.0, 1800.0, 2400.0];
        let analyzer = MultiToneAnalyzer::new(&frequencies, SAMPLE_RATE);

        let amplitude = 10000.0;
        let n = 1600;

        let out_of_band = 3300.0;
        let samples = generate_tone(out_of_band, SAMPLE_RATE, n, amplitude);
        let rejected = analyzer.magnitudes(&samples);

        // Sanity: the magnitude we'd see at bin `i` if its OWN frequency
        // were driven at the same amplitude — this is the "full-scale"
        // reference. Each rejected magnitude must be a small fraction
        // of this.
        for (i, &freq) in frequencies.iter().enumerate() {
            let in_band = generate_tone(freq, SAMPLE_RATE, n, amplitude);
            let in_band_mag = analyzer.magnitudes(&in_band)[i];
            assert!(
                in_band_mag > 0.0,
                "bin {i} ({freq} Hz) produced zero magnitude for a matching tone — test broken"
            );
            let ratio = rejected[i] / in_band_mag;
            assert!(
                ratio < 0.05,
                "bin {i} ({freq} Hz): out-of-band leakage {:.1} vs in-band reference {:.1} (ratio {:.4})",
                rejected[i],
                in_band_mag,
                ratio
            );
        }
    }

    /// Adjacent-bin leakage: the tight case for rectangular-window
    /// Goertzel. Inject a signal at 3225 Hz — halfway between the V.34
    /// probe tones at 3150 Hz and 3300 Hz (the latter is *outside* the
    /// probe grid, but the physical risk is the same: an off-grid tone
    /// bleeding into the nearest probe bin). 3150 Hz is the highest
    /// probe frequency, so leakage from anything just above it is the
    /// worst case the analyzer needs to tolerate.
    ///
    /// Block length choice: N = 80 samples (10 ms at 8 kHz). This
    /// matches the V.34 analyzer's per-tick granularity and gives
    /// Δf · N / fs = 0.75 at 75 Hz offset — squarely in the first
    /// sidelobe region where leakage is interesting. Longer blocks
    /// push the adjacent bin deep into a sinc null (ratio ≈ 0), which
    /// is trivially true and tests nothing.
    ///
    /// Expectations:
    /// - The 3150 Hz bin, 75 Hz off-axis, gets some of the injected
    ///   energy via the sinc envelope; empirically ≤ 30 % of what a
    ///   matching in-band tone produces. Bound set just above the
    ///   measured value.
    /// - Everything below 3000 Hz (≥ 225 Hz away) must be in the sinc
    ///   far-out tail — bound < 5 % of the matched-tone reference.
    #[test]
    fn test_multi_tone_analyzer_adjacent_bin_leakage() {
        // Real V.34 probe grid: 150 Hz .. 3150 Hz, 150 Hz spacing.
        let frequencies: Vec<f64> = (1..=21).map(|k| 150.0 * k as f64).collect();
        let analyzer = MultiToneAnalyzer::new(&frequencies, SAMPLE_RATE);

        let amplitude = 10000.0;
        // 80 samples = 10 ms at 8 kHz — matches the V.34 probing
        // analyzer's per-tick block size (module doc). At this length,
        // 75 Hz offset lands in the first sidelobe of the rectangular
        // window's sinc envelope, exposing Goertzel leakage.
        let n = 80;

        // Off-grid injection at 3225 Hz: midway between the highest
        // probe (3150 Hz) and the next harmonic we'd use if we extended
        // the grid (3300 Hz). 75 Hz from the 3150 Hz bin.
        let off_grid = 3225.0;
        let samples = generate_tone(off_grid, SAMPLE_RATE, n, amplitude);
        let leaked = analyzer.magnitudes(&samples);

        // Compute the in-band reference for the 3150 Hz bin (index 20):
        // what its magnitude would be if 3150 Hz were injected directly
        // at the same amplitude. All leakage ratios are normalised to
        // this.
        let top_bin = 20; // 150 * 21 = 3150 Hz
        assert!((frequencies[top_bin] - 3150.0).abs() < 1e-9);
        let in_band_top = generate_tone(3150.0, SAMPLE_RATE, n, amplitude);
        let in_band_top_mag = analyzer.magnitudes(&in_band_top)[top_bin];
        assert!(
            in_band_top_mag > 0.0,
            "3150 Hz bin produced zero magnitude for matching tone — test broken"
        );

        // Adjacent-bin leakage at 3150 Hz. Measured empirically at
        // ~0.309 (sinc envelope at 75 Hz offset for N=80, fs=8 kHz).
        // Bound set just above the measurement with a small margin for
        // floating-point noise.
        let adjacent_ratio = leaked[top_bin] / in_band_top_mag;
        assert!(
            adjacent_ratio < 0.35,
            "adjacent-bin leakage at 3150 Hz (75 Hz off): ratio {adjacent_ratio:.4} \
             (leak {:.1}, in-band ref {:.1}) — expected < 0.35 (measured ~0.309)",
            leaked[top_bin],
            in_band_top_mag
        );

        // Far out-of-band: at N=80 the sinc sidelobe decay is gentle
        // (≈1/x envelope). Bins < 2700 Hz (≥ 525 Hz / ≥ 5.25 bin-widths
        // from 3225 Hz) must drop below 5 % of the matched-tone
        // reference. Bins in the 2700..3000 Hz range are still within
        // the near sidelobes (one bin-width ≈ 100 Hz at N=80) — a
        // separate, looser assertion at 15 % guards those without
        // being vacuous.
        for (i, &freq) in frequencies.iter().enumerate() {
            if freq > 3000.0 {
                continue;
            }
            let in_band = generate_tone(freq, SAMPLE_RATE, n, amplitude);
            let in_band_mag = analyzer.magnitudes(&in_band)[i];
            assert!(
                in_band_mag > 0.0,
                "bin {i} ({freq} Hz) produced zero magnitude for matching tone — test broken"
            );
            let ratio = leaked[i] / in_band_mag;
            let bound = if freq <= 2700.0 { 0.05 } else { 0.15 };
            assert!(
                ratio < bound,
                "bin {i} ({freq} Hz): far-OOB leakage ratio {ratio:.4} (leak {:.1}, \
                 in-band ref {:.1}) — expected < {bound}",
                leaked[i],
                in_band_mag
            );
        }
    }

    #[test]
    fn test_multi_tone_analyzer_normalized_magnitudes_in_range() {
        let frequencies = [800.0, 1600.0, 2400.0];
        let analyzer = MultiToneAnalyzer::new(&frequencies, SAMPLE_RATE);
        let samples = generate_tone(1600.0, SAMPLE_RATE, 800, 10000.0);

        let norms = analyzer.normalized_magnitudes(&samples);
        for &n in &norms {
            assert!(
                (0.0..=1.0).contains(&n),
                "normalised magnitude {n} out of [0,1]"
            );
        }
        // The 1600 Hz bin should dominate.
        assert!(norms[1] > 0.5, "1600 Hz bin should light up, got {norms:?}");
        assert!(norms[0] < 0.1, "800 Hz bin should be quiet, got {norms:?}");
        assert!(norms[2] < 0.1, "2400 Hz bin should be quiet, got {norms:?}");
    }

    #[test]
    fn test_multi_tone_analyzer_preserves_bin_order() {
        let frequencies = [1234.0, 567.0, 2100.0];
        let analyzer = MultiToneAnalyzer::new(&frequencies, SAMPLE_RATE);
        assert_eq!(analyzer.frequencies(), &frequencies);
        assert_eq!(analyzer.len(), 3);
        assert!(!analyzer.is_empty());
    }
}
