//! Root-raised-cosine FIR filter for V.32bis pulse shaping.

use std::f64::consts::PI;

/// Precomputed RRC FIR filter coefficients.
pub struct RrcFilter {
    taps: Vec<f64>,
    delay_line: Vec<f64>,
    index: usize,
}

impl RrcFilter {
    /// Create an RRC filter with the given parameters.
    /// `rolloff`: excess bandwidth factor (0.15 for V.32bis)
    /// `symbol_rate`: symbols per second (2400 for V.32bis)
    /// `sample_rate`: samples per second (8000)
    /// `num_taps`: filter length (48)
    pub fn new(rolloff: f64, symbol_rate: f64, sample_rate: f64, num_taps: usize) -> Self {
        let taps = compute_rrc_taps(rolloff, symbol_rate, sample_rate, num_taps);
        Self {
            delay_line: vec![0.0; taps.len()],
            taps,
            index: 0,
        }
    }

    /// Filter a single sample (FIR convolution).
    pub fn filter(&mut self, sample: f64) -> f64 {
        let n = self.taps.len();

        // Write new sample into circular buffer at current index.
        self.delay_line[self.index] = sample;

        // Convolve: tap[0] is multiplied by the most recent sample, etc.
        let mut output = 0.0;
        for k in 0..n {
            // Oldest sample first: walk backwards from current index.
            let buf_idx = (self.index + n - k) % n;
            output += self.taps[k] * self.delay_line[buf_idx];
        }

        // Advance circular index.
        self.index = (self.index + 1) % n;

        output
    }

    /// Filter a block of samples in-place.
    pub fn filter_block(&mut self, samples: &mut [f64]) {
        for i in 0..samples.len() {
            samples[i] = self.filter(samples[i]);
        }
    }

    /// Reset the delay line to zeros.
    pub fn reset(&mut self) {
        self.delay_line.fill(0.0);
        self.index = 0;
    }

    /// Access the filter coefficients (for testing).
    pub fn taps(&self) -> &[f64] {
        &self.taps
    }
}

/// Compute RRC impulse response coefficients, centered at tap `num_taps/2`.
fn compute_rrc_taps(rolloff: f64, symbol_rate: f64, sample_rate: f64, num_taps: usize) -> Vec<f64> {
    let t_sym = 1.0 / symbol_rate; // Symbol period T
    let center = (num_taps - 1) as f64 / 2.0;
    let alpha = rolloff;

    let mut taps = Vec::with_capacity(num_taps);

    for i in 0..num_taps {
        let t = (i as f64 - center) / sample_rate; // Time offset in seconds
        let t_norm = t / t_sym; // Time normalized by symbol period (t/T)

        let h = if t.abs() < 1e-12 {
            // t = 0 special case
            (1.0 - alpha + 4.0 * alpha / PI) / t_sym
        } else if (4.0 * alpha * t_norm).abs() > 1.0 - 1e-10
            && (4.0 * alpha * t_norm).abs() < 1.0 + 1e-10
        {
            // t = ±T/(4α) special case: denominator (1 - (4αt/T)²) → 0
            alpha / (t_sym * 2.0_f64.sqrt())
                * ((1.0 + 2.0 / PI) * (PI / (4.0 * alpha)).sin()
                    + (1.0 - 2.0 / PI) * (PI / (4.0 * alpha)).cos())
        } else {
            // General case
            let num = (PI * t_norm * (1.0 - alpha)).sin()
                + 4.0 * alpha * t_norm * (PI * t_norm * (1.0 + alpha)).cos();
            let den = PI * t_norm * (1.0 - (4.0 * alpha * t_norm).powi(2));
            num / (den * t_sym)
        };

        taps.push(h);
    }

    // Normalize so the cascaded TX*RX response preserves signal amplitude.
    // Using DC gain normalization (sum of taps = constant) ensures that all
    // RRC filters — regardless of num_taps or sample_rate — have the same
    // passband gain, making the cascaded response a properly-scaled raised
    // cosine even when TX and RX filters are designed at different rates.
    let tap_sum: f64 = taps.iter().sum();
    if tap_sum.abs() > 1e-12 {
        for tap in &mut taps {
            *tap /= tap_sum;
        }
    }

    taps
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    const ROLLOFF: f64 = 0.15;
    const SYMBOL_RATE: f64 = 2400.0;
    const SAMPLE_RATE: f64 = 8000.0;
    const NUM_TAPS: usize = 48;

    #[test]
    fn test_rrc_tap_count() {
        let filter = RrcFilter::new(ROLLOFF, SYMBOL_RATE, SAMPLE_RATE, NUM_TAPS);
        assert_eq!(
            filter.taps().len(),
            NUM_TAPS,
            "Filter should have exactly {NUM_TAPS} taps"
        );
    }

    #[test]
    fn test_rrc_symmetry() {
        let filter = RrcFilter::new(ROLLOFF, SYMBOL_RATE, SAMPLE_RATE, NUM_TAPS);
        let taps = filter.taps();

        for i in 0..NUM_TAPS / 2 {
            let diff = (taps[i] - taps[NUM_TAPS - 1 - i]).abs();
            assert!(
                diff < 1e-12,
                "Tap [{i}] ({}) should equal tap [{}] ({}) for linear-phase symmetry, diff={diff}",
                taps[i],
                NUM_TAPS - 1 - i,
                taps[NUM_TAPS - 1 - i]
            );
        }
    }

    #[test]
    fn test_rrc_impulse_response() {
        let mut filter = RrcFilter::new(ROLLOFF, SYMBOL_RATE, SAMPLE_RATE, NUM_TAPS);
        let taps = filter.taps().to_vec();

        // Feed an impulse (1.0 followed by zeros).
        let mut output = Vec::with_capacity(NUM_TAPS);
        output.push(filter.filter(1.0));
        for _ in 1..NUM_TAPS {
            output.push(filter.filter(0.0));
        }

        // Output should match the taps (the FIR impulse response IS the taps).
        for i in 0..NUM_TAPS {
            let diff = (output[i] - taps[i]).abs();
            assert!(
                diff < 1e-12,
                "Impulse response at [{i}]: got {}, expected {}, diff={diff}",
                output[i],
                taps[i]
            );
        }
    }

    #[test]
    fn test_rrc_cascade_zero_isi() {
        // TX RRC * RX RRC = raised cosine, which has zero ISI at symbol intervals.
        // Generate an impulse, filter through TX then RX, verify zero crossings at
        // symbol-rate sample points.
        let samples_per_symbol = SAMPLE_RATE / SYMBOL_RATE; // ~3.333

        let mut tx = RrcFilter::new(ROLLOFF, SYMBOL_RATE, SAMPLE_RATE, NUM_TAPS);
        let mut rx = RrcFilter::new(ROLLOFF, SYMBOL_RATE, SAMPLE_RATE, NUM_TAPS);

        // Length must cover the full filter response of both stages.
        let total_len = NUM_TAPS * 3;

        // Impulse at sample 0
        let mut signal = vec![0.0; total_len];
        signal[0] = 1.0;

        // Pass through TX filter
        tx.filter_block(&mut signal);

        // Pass through RX filter
        rx.filter_block(&mut signal);

        // The cascade impulse response peaks at sample = 2*(NUM_TAPS-1)/2 = NUM_TAPS-1
        // (group delay of two filters concatenated).
        let peak_sample = NUM_TAPS - 1;

        // Find the peak value
        let peak_val = signal[peak_sample];
        assert!(
            peak_val.abs() > 1e-6,
            "Peak of cascaded RRC should be nonzero, got {peak_val}"
        );

        // Check that at every symbol interval offset from the peak, the value is ~zero.
        // We check integer symbol offsets: peak ± k * samples_per_symbol
        for k in 1..8 {
            let offset = (k as f64) * samples_per_symbol;
            let idx_before = peak_sample as f64 + offset;
            let idx_after = peak_sample as f64 - offset;

            for idx_f in [idx_before, idx_after] {
                if idx_f < 0.0 || idx_f >= total_len as f64 - 1.0 {
                    continue;
                }
                // Linear interpolation for fractional sample index
                let idx_lo = idx_f.floor() as usize;
                let frac = idx_f - idx_lo as f64;
                let val = signal[idx_lo] * (1.0 - frac) + signal[idx_lo + 1] * frac;

                let normalized = val / peak_val;
                assert!(
                    normalized.abs() < 0.05,
                    "Zero ISI violated at symbol offset {k}: normalized value {normalized} \
                     (raw {val}) at sample index {idx_f:.2}"
                );
            }
        }
    }

    #[test]
    fn test_rrc_dc_gain_normalization() {
        let filter = RrcFilter::new(ROLLOFF, SYMBOL_RATE, SAMPLE_RATE, NUM_TAPS);
        let taps = filter.taps();

        // After DC-gain normalization, sum of taps should be 1.0.
        let tap_sum: f64 = taps.iter().sum();
        assert!(
            (tap_sum - 1.0).abs() < 1e-10,
            "DC gain (sum of taps) should be 1.0, got {tap_sum}"
        );

        // Verify DC-gain normalization is consistent across different configurations.
        // All RRC filters should have the same DC gain regardless of sample rate or taps.
        let filter_16k = RrcFilter::new(ROLLOFF, SYMBOL_RATE, 16000.0, 96);
        let sum_16k: f64 = filter_16k.taps().iter().sum();
        assert!(
            (sum_16k - 1.0).abs() < 1e-10,
            "16kHz filter DC gain should be 1.0, got {sum_16k}"
        );
    }

    #[test]
    fn test_rrc_reset() {
        let mut filter = RrcFilter::new(ROLLOFF, SYMBOL_RATE, SAMPLE_RATE, NUM_TAPS);

        // Feed some data
        for i in 0..NUM_TAPS {
            filter.filter(i as f64);
        }

        // Reset and verify zeros out
        filter.reset();
        let output = filter.filter(0.0);
        assert!(
            output.abs() < 1e-10,
            "After reset, zero input should give zero output, got {output}"
        );
    }

    #[test]
    fn test_rrc_center_tap_is_largest() {
        let filter = RrcFilter::new(ROLLOFF, SYMBOL_RATE, SAMPLE_RATE, NUM_TAPS);
        let taps = filter.taps();
        let center = NUM_TAPS / 2;
        let center_val = taps[center].abs();

        for (i, &tap) in taps.iter().enumerate() {
            if i != center {
                assert!(
                    tap.abs() <= center_val + 1e-12,
                    "Center tap should be the largest: tap[{i}]={} > center tap={center_val}",
                    tap.abs()
                );
            }
        }
    }

    #[test]
    fn test_rrc_different_rolloff() {
        // Different rolloff values should produce different taps
        let filter1 = RrcFilter::new(0.1, SYMBOL_RATE, SAMPLE_RATE, NUM_TAPS);
        let filter2 = RrcFilter::new(0.3, SYMBOL_RATE, SAMPLE_RATE, NUM_TAPS);

        let taps1 = filter1.taps();
        let taps2 = filter2.taps();

        let any_different = taps1
            .iter()
            .zip(taps2.iter())
            .any(|(&a, &b)| (a - b).abs() > 1e-10);
        assert!(
            any_different,
            "Different rolloff should produce different taps"
        );
    }

    #[test]
    fn test_rrc_filter_block() {
        let mut filter1 = RrcFilter::new(ROLLOFF, SYMBOL_RATE, SAMPLE_RATE, NUM_TAPS);
        let mut filter2 = RrcFilter::new(ROLLOFF, SYMBOL_RATE, SAMPLE_RATE, NUM_TAPS);

        // filter_block should produce same result as individual filter calls
        let input: Vec<f64> = (0..20).map(|i| (i as f64 * 0.3).sin()).collect();
        let mut block = input.clone();
        filter1.filter_block(&mut block);

        let individual: Vec<f64> = input.iter().map(|&s| filter2.filter(s)).collect();

        for (i, (&b, &ind)) in block.iter().zip(individual.iter()).enumerate() {
            assert!(
                (b - ind).abs() < 1e-12,
                "filter_block vs filter mismatch at {i}: {b} vs {ind}"
            );
        }
    }
}
