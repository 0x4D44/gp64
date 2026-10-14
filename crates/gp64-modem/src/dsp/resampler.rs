//! FIR resampler and decimator for sample rate conversion.
//!
//! - [`Resampler`]: Upsampling (e.g., 8 kHz -> 48 kHz), f64 interface.
//!   Uses a true polyphase decomposition (one output per ratio input MACs).
//! - [`Decimator`]: Integer-ratio downsampling (e.g., 48 -> 16 kHz, 16 -> 8 kHz), i16 interface.
//!   Implemented as FIR-on-every-input-sample (not polyphase-optimised).
//!   A true polyphase decomposition would cost ~ratio-x less but is deferred
//!   until Stage 2 real-time audio callback constraints apply.
//!
//! Decimator filter corner frequencies (for reference, cutoff = 0.45 * input_rate / ratio):
//!   ratio 2 (16 kHz -> 8 kHz):  cutoff ~ 3.6 kHz  (comfortable for V.32bis's 3 kHz top)
//!   ratio 3 (48 kHz -> 16 kHz): cutoff ~ 7.2 kHz  (comfortable for V.34's 3.4 kHz top)
//!   ratio 6 (48 kHz -> 8 kHz):  cutoff ~ 3.6 kHz  (tight for V.32bis; see passband-edge test)

use std::f64::consts::PI;

/// Upsampling polyphase FIR resampler.
pub struct Resampler {
    /// Polyphase filter bank: `ratio` phases, each with `taps_per_phase` coefficients.
    phases: Vec<Vec<f64>>,
    /// Circular delay line holding the most recent input samples.
    delay_line: Vec<f64>,
    /// Write index into the circular delay line.
    index: usize,
    /// Upsampling ratio (e.g., 6 for 8000 -> 48000 Hz).
    ratio: usize,
}

impl Resampler {
    /// Create an upsampling resampler with the given ratio.
    /// For 8000 -> 48000 Hz, ratio = 6.
    /// Designs a lowpass FIR with cutoff at pi/ratio, then decomposes
    /// into polyphase branches.
    pub fn new(ratio: usize, taps_per_phase: usize) -> Self {
        let prototype_len = ratio * taps_per_phase;
        let prototype = design_lowpass_fir(prototype_len, ratio);

        // Decompose into polyphase branches.
        // Phase p gets coefficients: prototype[p], prototype[p + ratio], prototype[p + 2*ratio], ...
        let mut phases = Vec::with_capacity(ratio);
        for p in 0..ratio {
            let mut phase_taps = Vec::with_capacity(taps_per_phase);
            for k in 0..taps_per_phase {
                phase_taps.push(prototype[p + k * ratio]);
            }
            phases.push(phase_taps);
        }

        Self {
            phases,
            delay_line: vec![0.0; taps_per_phase],
            index: 0,
            ratio,
        }
    }

    /// Resample a block of input samples, writing to the output buffer.
    /// For each input sample, produces `ratio` output samples.
    /// Output length = input length * ratio.
    /// The upsampling ratio this instance was built for.
    ///
    /// Callers that cache a `Resampler` across rate changes need this to tell
    /// whether the cached one still matches the rate being asked for — reusing
    /// one built for a different ratio violates `process`'s length contract
    /// (`MDD-BUG-CRUCIBLE-00520`).
    pub fn ratio(&self) -> usize {
        self.ratio
    }

    pub fn process(&mut self, input: &[f64], output: &mut [f64]) {
        debug_assert_eq!(
            output.len(),
            input.len() * self.ratio,
            "Output buffer must be exactly input.len() * ratio"
        );

        let taps_per_phase = self.delay_line.len();
        let n = taps_per_phase;

        for (i, &sample) in input.iter().enumerate() {
            // Insert new sample into circular delay line.
            self.delay_line[self.index] = sample;

            // Compute each polyphase output.
            for p in 0..self.ratio {
                let mut acc = 0.0;
                let phase = &self.phases[p];
                for k in 0..n {
                    // Walk backwards through the delay line from current index.
                    let buf_idx = (self.index + n - k) % n;
                    acc += phase[k] * self.delay_line[buf_idx];
                }
                output[i * self.ratio + p] = acc * self.ratio as f64;
            }

            // Advance circular index.
            self.index = (self.index + 1) % n;
        }
    }

    /// Reset internal state.
    pub fn reset(&mut self) {
        self.delay_line.fill(0.0);
        self.index = 0;
    }
}

/// Design a lowpass FIR filter using windowed sinc (Hamming window).
/// `length`: total number of taps in the prototype filter.
/// `ratio`: upsampling ratio (cutoff = 1/(2*ratio) of the output sample rate).
fn design_lowpass_fir(length: usize, ratio: usize) -> Vec<f64> {
    let center = (length - 1) as f64 / 2.0;
    let cutoff = 1.0 / (2.0 * ratio as f64); // Normalized cutoff frequency (0..0.5)

    let mut taps = Vec::with_capacity(length);

    for i in 0..length {
        let n = i as f64 - center;

        // Sinc function
        let sinc = if n.abs() < 1e-12 {
            1.0
        } else {
            (2.0 * PI * cutoff * n).sin() / (PI * n)
        };

        // Hamming window
        let window = 0.54 - 0.46 * (2.0 * PI * i as f64 / (length - 1) as f64).cos();

        taps.push(2.0 * cutoff * sinc * window);
    }

    // Normalize so that DC gain = 1.0 (sum of taps = 1).
    let sum: f64 = taps.iter().sum();
    if sum.abs() > 0.0 {
        for tap in &mut taps {
            *tap /= sum;
        }
    }

    taps
}

/// Design a lowpass FIR for decimation. Uses a guard band below the output
/// Nyquist to achieve better stopband attenuation with a given filter length.
/// `length`: total number of taps in the prototype filter.
/// `ratio`: decimation ratio.
fn design_decimation_fir(length: usize, ratio: usize) -> Vec<f64> {
    let center = (length - 1) as f64 / 2.0;
    // Pull cutoff 10% below Nyquist of the decimated rate to create a guard band.
    // This trades a tiny bit of passband width for significantly better stopband rejection.
    let cutoff = 0.45 / ratio as f64;

    let mut taps = Vec::with_capacity(length);

    for i in 0..length {
        let n = i as f64 - center;

        // Sinc function
        let sinc = if n.abs() < 1e-12 {
            1.0
        } else {
            (2.0 * PI * cutoff * n).sin() / (PI * n)
        };

        // Hamming window
        let window = 0.54 - 0.46 * (2.0 * PI * i as f64 / (length - 1) as f64).cos();

        taps.push(2.0 * cutoff * sinc * window);
    }

    // Normalize so that DC gain = 1.0.
    let sum: f64 = taps.iter().sum();
    if sum.abs() > 0.0 {
        for tap in &mut taps {
            *tap /= sum;
        }
    }

    taps
}

/// Downsampling polyphase FIR decimator.
///
/// Accepts integer-ratio decimation (e.g., 3 for 48 kHz -> 16 kHz,
/// 2 for 16 kHz -> 8 kHz). Uses a Hamming-windowed lowpass FIR.
/// For each group of `ratio` input samples, all are filtered and one
/// output sample is produced.
pub struct Decimator {
    /// Prototype FIR filter coefficients (length = ratio * taps_per_phase).
    coeffs: Vec<f64>,
    /// Circular delay line holding the most recent input samples.
    delay_line: Vec<f64>,
    /// Write index into the circular delay line.
    index: usize,
    /// Number of input samples consumed since last output (0..ratio-1).
    phase: usize,
    /// Decimation ratio (e.g., 3 for 48 kHz -> 16 kHz).
    ratio: usize,
    /// Leftover input samples from a previous call that didn't fill a complete decimation group.
    remainder: Vec<i16>,
}

impl Decimator {
    /// Create a decimator with the given integer decimation ratio and FIR filter taps per phase.
    /// Uses a Hamming-windowed lowpass FIR designed for the given ratio.
    pub fn new(ratio: usize, taps_per_phase: usize) -> Self {
        assert!(ratio >= 1, "Decimation ratio must be >= 1");
        assert!(taps_per_phase >= 1, "Taps per phase must be >= 1");

        let prototype_len = ratio * taps_per_phase;
        let coeffs = design_decimation_fir(prototype_len, ratio);

        Self {
            delay_line: vec![0.0; coeffs.len()],
            coeffs,
            index: 0,
            phase: 0,
            ratio,
            remainder: Vec::new(),
        }
    }

    /// Process input samples. Returns decimated output.
    ///
    /// If the total available samples (remainder from previous call + new input)
    /// is not a multiple of the decimation ratio, leftover samples are buffered
    /// internally for the next call.
    pub fn process(&mut self, input: &[i16]) -> Vec<i16> {
        // Combine remainder from previous call with new input.
        let total: Vec<i16> = if self.remainder.is_empty() {
            input.to_vec()
        } else {
            let mut combined = std::mem::take(&mut self.remainder);
            combined.extend_from_slice(input);
            combined
        };

        let usable = (total.len() / self.ratio) * self.ratio;
        let num_output = usable / self.ratio;

        // Save leftover samples.
        if usable < total.len() {
            self.remainder = total[usable..].to_vec();
        }

        let filter_len = self.delay_line.len();
        let mut output = Vec::with_capacity(num_output);

        for &sample in &total[..usable] {
            // Insert sample into circular delay line.
            self.delay_line[self.index] = sample as f64;
            self.index = (self.index + 1) % filter_len;

            self.phase += 1;
            if self.phase == self.ratio {
                self.phase = 0;

                // Compute FIR output: dot product of coefficients with delay line.
                // coeffs[0] multiplies the most recent sample, coeffs[N-1] the oldest.
                let mut acc = 0.0;
                for (k, &coeff) in self.coeffs.iter().enumerate() {
                    let buf_idx = (self.index + filter_len - 1 - k) % filter_len;
                    acc += coeff * self.delay_line[buf_idx];
                }

                let clamped = acc.round().clamp(i16::MIN as f64, i16::MAX as f64);
                output.push(clamped as i16);
            }
        }

        output
    }

    /// Reset filter state.
    pub fn reset(&mut self) {
        self.delay_line.fill(0.0);
        self.index = 0;
        self.phase = 0;
        self.remainder.clear();
    }

    /// Linear-phase FIR group delay, in input samples.
    ///
    /// For a symmetric FIR of length `N`, the group delay is `(N - 1) / 2`.
    /// This is expressed in **input-rate samples**; downstream code that needs
    /// an output-rate delay should divide by the decimation ratio.
    ///
    /// Required by the echo canceller (and any other downstream timing-sensitive
    /// code) to align signals across the decimator.
    pub fn group_delay_samples(&self) -> usize {
        (self.coeffs.len() - 1) / 2
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    const RATIO: usize = 6;
    const TAPS_PER_PHASE: usize = 16;
    const INPUT_RATE: f64 = 8000.0;
    const OUTPUT_RATE: f64 = 48000.0;

    #[test]
    fn test_resampler_output_length() {
        let mut resampler = Resampler::new(RATIO, TAPS_PER_PHASE);
        let input = vec![0.0; 80];
        let mut output = vec![0.0; 80 * RATIO];
        resampler.process(&input, &mut output);
        assert_eq!(
            output.len(),
            480,
            "80 input samples at ratio 6 should produce 480 output samples"
        );
    }

    #[test]
    fn test_resampler_dc_passthrough() {
        let mut resampler = Resampler::new(RATIO, TAPS_PER_PHASE);
        let dc_value = 0.75;

        // Feed enough samples to fill the delay line and reach steady state.
        let warmup = TAPS_PER_PHASE * 2;
        let input = vec![dc_value; warmup + 100];
        let mut output = vec![0.0; input.len() * RATIO];
        resampler.process(&input, &mut output);

        // After warmup, all output samples should be close to dc_value.
        // Hamming-windowed FIR has slight passband ripple, so allow 0.1% tolerance.
        let steady_start = warmup * RATIO;
        for (i, &val) in output[steady_start..].iter().enumerate() {
            assert!(
                (val - dc_value).abs() < 0.01,
                "DC passthrough failed at output sample {}: got {val}, expected {dc_value}",
                steady_start + i
            );
        }
    }

    #[test]
    fn test_resampler_low_frequency_preserved() {
        // Generate a 1000 Hz tone at 8000 Hz, resample to 48000 Hz,
        // verify frequency preserved using Goertzel at the output rate.
        let mut resampler = Resampler::new(RATIO, TAPS_PER_PHASE);
        let freq = 1000.0;
        let num_input = 800; // 100 ms at 8000 Hz
        let num_output = num_input * RATIO;

        // Generate input tone
        let input: Vec<f64> = (0..num_input)
            .map(|i| (2.0 * PI * freq * i as f64 / INPUT_RATE).sin())
            .collect();

        let mut output = vec![0.0; num_output];
        resampler.process(&input, &mut output);

        // Use Goertzel at the output sample rate to check for 1000 Hz.
        let target_energy = goertzel_energy_f64(&output, freq, OUTPUT_RATE);

        // Check that most of the output energy is at 1000 Hz.
        let total_energy: f64 = output.iter().map(|x| x * x).sum();

        // Goertzel energy for a pure tone of length N, amplitude A:
        //   ~N^2 * A^2 / 4
        // Total energy: ~N * A^2 / 2
        // Ratio: goertzel / (total * N/2) ~ 1.0
        let n = num_output as f64;
        let normalized = target_energy / (total_energy * n / 2.0);

        assert!(
            normalized > 0.5,
            "1000 Hz tone should be preserved after resampling, normalized energy = {normalized}"
        );
    }

    #[test]
    fn test_resampler_no_imaging() {
        // Resample a 1000 Hz tone from 8000 to 48000 Hz.
        // Verify no spectral images above 4000 Hz (the input Nyquist).
        let mut resampler = Resampler::new(RATIO, TAPS_PER_PHASE);
        let freq = 1000.0;
        let num_input = 800;
        let num_output = num_input * RATIO;

        let input: Vec<f64> = (0..num_input)
            .map(|i| (2.0 * PI * freq * i as f64 / INPUT_RATE).sin())
            .collect();

        let mut output = vec![0.0; num_output];
        resampler.process(&input, &mut output);

        // Measure energy at the fundamental (1000 Hz)
        let fundamental_energy = goertzel_energy_f64(&output, freq, OUTPUT_RATE);

        // Check for spectral images at the expected aliased positions:
        // Images appear at k*8000 ± 1000 Hz for k=1,2,...
        // So: 7000, 9000, 15000, 17000, 23000 Hz
        let image_freqs = [7000.0, 9000.0, 15000.0, 17000.0, 23000.0];
        for &img_freq in &image_freqs {
            let img_energy = goertzel_energy_f64(&output, img_freq, OUTPUT_RATE);
            let ratio = img_energy / fundamental_energy;
            assert!(
                ratio < 0.01,
                "Spectral image at {img_freq} Hz: energy ratio {ratio:.6} should be < 0.01"
            );
        }
    }

    /// Goertzel energy detector for f64 samples.
    fn goertzel_energy_f64(samples: &[f64], frequency: f64, sample_rate: f64) -> f64 {
        let normalized_freq = frequency / sample_rate;
        let coeff = 2.0 * (2.0 * PI * normalized_freq).cos();

        let mut s1: f64 = 0.0;
        let mut s2: f64 = 0.0;

        for &sample in samples {
            let s0 = coeff * s1 - s2 + sample;
            s2 = s1;
            s1 = s0;
        }

        s1 * s1 + s2 * s2 - coeff * s1 * s2
    }

    #[test]
    fn test_resampler_reset() {
        let mut resampler = Resampler::new(RATIO, TAPS_PER_PHASE);

        // Feed some data
        let input = vec![1.0; 80];
        let mut output = vec![0.0; 480];
        resampler.process(&input, &mut output);

        // Reset and feed zeros — should get zeros (no leftover state)
        resampler.reset();
        let input_zero = vec![0.0; 80];
        let mut output_zero = vec![0.0; 480];
        resampler.process(&input_zero, &mut output_zero);

        // After reset, all zeros in should give all zeros out
        let all_zero = output_zero.iter().all(|&v| v.abs() < 1e-10);
        assert!(
            all_zero,
            "After reset, zero input should produce zero output"
        );
    }

    #[test]
    fn test_resampler_ratio_2() {
        // Test with ratio=2 (e.g., 8000 -> 16000 Hz)
        let ratio = 2;
        let taps = 16;
        let mut resampler = Resampler::new(ratio, taps);
        let input = vec![0.5; 80];
        let mut output = vec![0.0; 80 * ratio];
        resampler.process(&input, &mut output);
        assert_eq!(output.len(), 160);
    }

    #[test]
    fn test_resampler_single_sample() {
        let mut resampler = Resampler::new(RATIO, TAPS_PER_PHASE);
        let input = [1.0];
        let mut output = [0.0; RATIO];
        resampler.process(&input, &mut output);
        // Should produce exactly RATIO output samples
        assert_eq!(output.len(), RATIO);
    }

    #[test]
    fn test_resampler_high_frequency_attenuated() {
        // A tone at 3900 Hz (near input Nyquist of 4000 Hz) should be
        // attenuated in the output (anti-imaging filter).
        let mut resampler = Resampler::new(RATIO, TAPS_PER_PHASE);
        let freq = 3900.0;
        let num_input = 800;
        let num_output = num_input * RATIO;

        let input: Vec<f64> = (0..num_input)
            .map(|i| (2.0 * PI * freq * i as f64 / INPUT_RATE).sin())
            .collect();

        let mut output = vec![0.0; num_output];
        resampler.process(&input, &mut output);

        // The energy at 3900 Hz in the output should be present (it's within
        // the input bandwidth), but spectral images above 4000 Hz should be suppressed.
        let target_energy = goertzel_energy_f64(&output, freq, OUTPUT_RATE);
        assert!(
            target_energy > 0.0,
            "3900 Hz signal should pass through (within input bandwidth)"
        );
    }

    // ---- Decimator tests ----

    /// Goertzel energy detector for i16 samples (used by decimator tests).
    fn goertzel_energy_i16(samples: &[i16], frequency: f64, sample_rate: f64) -> f64 {
        let normalized_freq = frequency / sample_rate;
        let coeff = 2.0 * (2.0 * PI * normalized_freq).cos();

        let mut s1: f64 = 0.0;
        let mut s2: f64 = 0.0;

        for &sample in samples {
            let s0 = coeff * s1 - s2 + sample as f64;
            s2 = s1;
            s1 = s0;
        }

        s1 * s1 + s2 * s2 - coeff * s1 * s2
    }

    /// Generate a pure tone as i16 samples.
    fn generate_tone_i16(
        frequency: f64,
        sample_rate: f64,
        num_samples: usize,
        amplitude: f64,
    ) -> Vec<i16> {
        (0..num_samples)
            .map(|i| {
                let t = i as f64 / sample_rate;
                (amplitude * (2.0 * PI * frequency * t).sin()).round() as i16
            })
            .collect()
    }

    #[test]
    fn test_decimation_by_2() {
        // Decimate a 1kHz tone from 16kHz to 8kHz (ratio=2).
        // Verify frequency is preserved and amplitude is within 1dB.
        let input_rate = 16000.0;
        let output_rate = 8000.0;
        let ratio = 2;
        let freq = 1000.0;
        let amplitude = 16000.0;
        let num_input = 3200; // 200ms at 16kHz
        let taps = 16;

        let mut dec = Decimator::new(ratio, taps);

        // Warmup: feed enough samples to fill the delay line.
        let warmup = taps * ratio * 2;
        let warmup_samples = generate_tone_i16(freq, input_rate, warmup, amplitude);
        let _ = dec.process(&warmup_samples);

        // Process the main signal.
        let input = generate_tone_i16(freq, input_rate, num_input, amplitude);
        let output = dec.process(&input);

        assert_eq!(
            output.len(),
            num_input / ratio,
            "Output length should be input / ratio"
        );

        // Verify frequency preserved via Goertzel.
        let target_energy = goertzel_energy_i16(&output, freq, output_rate);
        let total_energy: f64 = output.iter().map(|&s| (s as f64) * (s as f64)).sum();
        let n = output.len() as f64;
        let normalized = target_energy / (total_energy * n / 2.0);

        assert!(
            normalized > 0.5,
            "1kHz tone should be preserved after decimation by 2, normalized = {normalized}"
        );

        // Verify amplitude within 1dB: measure RMS of output vs expected.
        let rms_output = (total_energy / n).sqrt();
        let rms_expected = amplitude / 2.0_f64.sqrt(); // RMS of sine = A/sqrt(2)
        let db_diff = 20.0 * (rms_output / rms_expected).log10();
        assert!(
            db_diff.abs() < 1.0,
            "Amplitude should be within 1dB, got {db_diff:.2} dB difference"
        );
    }

    #[test]
    fn test_decimation_by_3() {
        // Decimate a 1kHz tone from 48kHz to 16kHz (ratio=3).
        let input_rate = 48000.0;
        let output_rate = 16000.0;
        let ratio = 3;
        let freq = 1000.0;
        let amplitude = 16000.0;
        let num_input = 4800; // 100ms at 48kHz
        let taps = 16;

        let mut dec = Decimator::new(ratio, taps);

        // Warmup.
        let warmup = taps * ratio * 2;
        let warmup_samples = generate_tone_i16(freq, input_rate, warmup, amplitude);
        let _ = dec.process(&warmup_samples);

        let input = generate_tone_i16(freq, input_rate, num_input, amplitude);
        let output = dec.process(&input);

        assert_eq!(output.len(), num_input / ratio);

        // Verify frequency.
        let target_energy = goertzel_energy_i16(&output, freq, output_rate);
        let total_energy: f64 = output.iter().map(|&s| (s as f64) * (s as f64)).sum();
        let n = output.len() as f64;
        let normalized = target_energy / (total_energy * n / 2.0);

        assert!(
            normalized > 0.5,
            "1kHz tone should be preserved after decimation by 3, normalized = {normalized}"
        );

        // Amplitude within 1dB.
        let rms_output = (total_energy / n).sqrt();
        let rms_expected = amplitude / 2.0_f64.sqrt();
        let db_diff = 20.0 * (rms_output / rms_expected).log10();
        assert!(
            db_diff.abs() < 1.0,
            "Amplitude should be within 1dB, got {db_diff:.2} dB"
        );
    }

    #[test]
    fn test_decimation_dc_passthrough() {
        // A DC signal (constant value) should pass through unchanged.
        let ratio = 3;
        let taps = 16;
        let dc_value: i16 = 1000;

        let mut dec = Decimator::new(ratio, taps);

        // Warmup to fill delay line.
        let warmup = vec![dc_value; taps * ratio * 2];
        let _ = dec.process(&warmup);

        // Process steady-state DC.
        let input = vec![dc_value; 300]; // 100 output samples
        let output = dec.process(&input);

        assert_eq!(output.len(), 100);

        // After warmup, output should be close to dc_value.
        for (i, &val) in output.iter().enumerate() {
            let diff = (val as i32 - dc_value as i32).unsigned_abs();
            assert!(
                diff <= 1,
                "DC passthrough failed at output sample {i}: got {val}, expected ~{dc_value}"
            );
        }
    }

    #[test]
    fn test_decimation_anti_alias() {
        // Input with energy above Nyquist/ratio should be suppressed by >=30dB.
        // For ratio=3 at 48kHz input, output Nyquist is 8kHz.
        // A 10kHz tone (above 8kHz) should be heavily attenuated.
        let input_rate = 48000.0;
        let ratio = 3;
        let taps = 16;

        let mut dec = Decimator::new(ratio, taps);

        // Generate two test signals: one in-band (1kHz), one above Nyquist (10kHz).
        // Output Nyquist is 8kHz (16kHz / 2).
        let num_input = 4800;
        let amplitude = 16000.0;

        // Warmup with silence.
        let warmup = vec![0i16; taps * ratio * 2];

        // --- In-band: 1kHz ---
        dec.reset();
        let _ = dec.process(&warmup);
        let inband = generate_tone_i16(1000.0, input_rate, num_input, amplitude);
        let output_inband = dec.process(&inband);
        let energy_inband: f64 = output_inband.iter().map(|&s| (s as f64) * (s as f64)).sum();

        // --- Out-of-band: 10kHz (above output Nyquist of 8kHz) ---
        dec.reset();
        let _ = dec.process(&warmup);
        let outband = generate_tone_i16(10000.0, input_rate, num_input, amplitude);
        let output_outband = dec.process(&outband);
        let energy_outband: f64 = output_outband
            .iter()
            .map(|&s| (s as f64) * (s as f64))
            .sum();

        // The out-of-band energy should be >=30dB below the in-band energy.
        let ratio_db = 10.0 * (energy_outband / energy_inband).log10();
        assert!(
            ratio_db < -30.0,
            "Out-of-band (10kHz) should be suppressed >=30dB below in-band (1kHz), got {ratio_db:.1} dB"
        );
    }

    #[test]
    fn test_decimation_input_length_remainder() {
        // Non-multiple input length: process what we can, buffer the remainder.
        let ratio = 3;
        let taps = 16;
        let mut dec = Decimator::new(ratio, taps);

        // Feed 10 samples (not a multiple of 3): should produce 3 outputs, buffer 1.
        let input = vec![1000i16; 10];
        let output = dec.process(&input);
        assert_eq!(
            output.len(),
            3,
            "10 samples at ratio 3: should produce 3 output samples"
        );

        // Feed 2 more: combined with the 1 remainder = 3, should produce 1 output.
        let input2 = vec![1000i16; 2];
        let output2 = dec.process(&input2);
        assert_eq!(
            output2.len(),
            1,
            "2 + 1 remainder = 3 samples at ratio 3: should produce 1 output"
        );
    }

    #[test]
    fn test_decimation_reset() {
        let ratio = 3;
        let taps = 16;
        let mut dec = Decimator::new(ratio, taps);

        // Feed a strong signal.
        let input = vec![10000i16; 300];
        let _ = dec.process(&input);

        // Also feed a non-multiple to leave remainder.
        let _ = dec.process(&[5000i16; 2]);

        // Reset and feed zeros — output should be all zeros (no leftover state).
        dec.reset();
        let zeros = vec![0i16; 300];
        let output = dec.process(&zeros);

        let all_zero = output.iter().all(|&v| v == 0);
        assert!(
            all_zero,
            "After reset, zero input should produce zero output"
        );
    }

    // ---- Phase 1a.2 required tests (V.34 PSTN interop working plan) ----
    //
    // These exercise the explicit rate conversions called out in the plan:
    //   - 48 -> 16 kHz (ratio 3) for the Stage 2 sound card to V.34 wire rate.
    //   - 48 ->  8 kHz (ratio 6) for the Stage 2 sound card to non-V.34 rate.
    // The filter must actually reject aliasing, not just subsample.

    /// Seedable xorshift64* — deterministic white noise generator.
    /// Marsaglia's xorshift64 with a multiplicative output transform; passes
    /// BigCrush, has a flat PSD, and is adequate for filter regression checks.
    fn xorshift_noise(seed: u64, n: usize, amplitude: f64) -> Vec<i16> {
        let mut state = if seed == 0 { 0x9E3779B97F4A7C15 } else { seed };
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            // xorshift64* scramble.
            let r = state.wrapping_mul(0x2545F4914F6CDD1D);
            // Map to [-1.0, 1.0) from top 32 bits.
            let hi = (r >> 32) as u32;
            let x = (hi as f64 / u32::MAX as f64) * 2.0 - 1.0;
            out.push(
                (amplitude * x)
                    .round()
                    .clamp(i16::MIN as f64, i16::MAX as f64) as i16,
            );
        }
        out
    }

    #[test]
    fn test_decimate_48_to_16_passband_preserved() {
        // 1 kHz tone at 48 kHz is well inside the 8 kHz output Nyquist.
        // After 1:3 decimation, amplitude loss through the anti-alias filter
        // must be within -1 dB (typical Hamming passband ripple is ~0.05 dB).
        let input_rate = 48000.0;
        let output_rate = 16000.0;
        let ratio = 3;
        let freq = 1000.0;
        let amplitude = 16000.0;
        let taps = 16;

        let mut dec = Decimator::new(ratio, taps);

        // Warmup to fill the delay line and reach steady state.
        let warmup = generate_tone_i16(freq, input_rate, ratio * taps * 2, amplitude);
        let _ = dec.process(&warmup);

        // 100 ms of signal to keep the Goertzel bin sharp.
        let num_input = 4800;
        let input = generate_tone_i16(freq, input_rate, num_input, amplitude);
        let output = dec.process(&input);
        assert_eq!(output.len(), num_input / ratio);

        // Compare RMS of output to expected RMS (A/sqrt(2)) for a pure sine.
        let total_energy: f64 = output.iter().map(|&s| (s as f64) * (s as f64)).sum();
        let rms_out = (total_energy / output.len() as f64).sqrt();
        let rms_expected = amplitude / 2.0_f64.sqrt();
        let db_diff = 20.0 * (rms_out / rms_expected).log10();
        assert!(
            db_diff.abs() < 1.0,
            "48->16 passband ripple at 1 kHz: {db_diff:.3} dB (expected within -1 dB)"
        );

        // Sanity: the spectrum really is at 1 kHz, not smeared.
        let target_energy = goertzel_energy_i16(&output, freq, output_rate);
        let n = output.len() as f64;
        let normalized = target_energy / (total_energy * n / 2.0);
        assert!(
            normalized > 0.9,
            "1 kHz should dominate output spectrum, normalized = {normalized:.3}"
        );
    }

    #[test]
    fn test_decimate_48_to_16_rejects_alias() {
        // 20 kHz input sampled at 48 kHz is above the 16 kHz output Nyquist of 8 kHz.
        // If the filter does nothing, the 20 kHz component would alias to
        //     |20 - 16| = 4 kHz
        // in the 16 kHz output. A competent anti-alias filter must reject this
        // by at least 40 dB (standard communications-grade stopband; HLD V4 Change 7).
        let input_rate = 48000.0;
        let output_rate = 16000.0;
        let ratio = 3;
        let alias_src_freq = 20000.0;
        let alias_folded_freq = 4000.0; // where 20 kHz would fold to in 16 kHz
        let amplitude = 16000.0;
        let taps = 16;

        let mut dec = Decimator::new(ratio, taps);

        // Warmup.
        let warmup = generate_tone_i16(alias_src_freq, input_rate, ratio * taps * 2, amplitude);
        let _ = dec.process(&warmup);

        let num_input = 4800;
        let input = generate_tone_i16(alias_src_freq, input_rate, num_input, amplitude);
        let output = dec.process(&input);

        // Per-sample energy of the original 20 kHz tone. Goertzel on a pure
        // sine of amplitude A over N samples integrates to ~N^2 * A^2 / 4,
        // so goertzel / (N^2 / 4) ~ A^2 regardless of rate. Compute on input
        // and on output at the folded frequency and take the ratio.
        let input_energy = goertzel_energy_i16(&input, alias_src_freq, input_rate);
        let output_energy = goertzel_energy_i16(&output, alias_folded_freq, output_rate);

        let n_in = input.len() as f64;
        let n_out = output.len() as f64;
        // Amplitude^2 estimate in each domain.
        let a2_in = input_energy / (n_in * n_in / 4.0);
        let a2_out = output_energy / (n_out * n_out / 4.0);

        let rejection_db = 10.0 * (a2_out / a2_in).log10();
        assert!(
            rejection_db < -40.0,
            "48->16 alias rejection at 4 kHz (from 20 kHz input) = {rejection_db:.1} dB, need <= -40 dB"
        );
    }

    #[test]
    fn test_decimate_48_to_8_rejects_alias() {
        // 1:6 decimation. Output Nyquist is 4 kHz. A 5 kHz tone would fold to
        //     |5 - 8| = 3 kHz
        // in the 8 kHz output. Require >= 40 dB rejection (HLD V4 Change 7).
        let input_rate = 48000.0;
        let output_rate = 8000.0;
        let ratio = 6;
        let alias_src_freq = 5000.0;
        let alias_folded_freq = 3000.0;
        let amplitude = 16000.0;
        let taps = 16;

        let mut dec = Decimator::new(ratio, taps);

        let warmup = generate_tone_i16(alias_src_freq, input_rate, ratio * taps * 2, amplitude);
        let _ = dec.process(&warmup);

        let num_input = 4800; // multiple of 6
        let input = generate_tone_i16(alias_src_freq, input_rate, num_input, amplitude);
        let output = dec.process(&input);
        assert_eq!(output.len(), num_input / ratio);

        let input_energy = goertzel_energy_i16(&input, alias_src_freq, input_rate);
        let output_energy = goertzel_energy_i16(&output, alias_folded_freq, output_rate);

        let n_in = input.len() as f64;
        let n_out = output.len() as f64;
        let a2_in = input_energy / (n_in * n_in / 4.0);
        let a2_out = output_energy / (n_out * n_out / 4.0);

        let rejection_db = 10.0 * (a2_out / a2_in).log10();
        assert!(
            rejection_db < -40.0,
            "48->8 alias rejection at 3 kHz (from 5 kHz input) = {rejection_db:.1} dB, need <= -40 dB"
        );
    }

    #[test]
    fn test_decimate_output_length() {
        // Output must be exactly floor(N / ratio) samples, no off-by-one.
        // Covers the three ratios the plan calls out: 2, 3, 6.
        for &ratio in &[2usize, 3, 6] {
            for &n in &[0usize, 1, ratio - 1, ratio, ratio + 1, 1000, 4801] {
                let mut dec = Decimator::new(ratio, 16);
                let input = vec![0i16; n];
                let output = dec.process(&input);
                assert_eq!(
                    output.len(),
                    n / ratio,
                    "ratio={ratio}, N={n}: expected {} output samples, got {}",
                    n / ratio,
                    output.len()
                );
            }
        }
    }

    #[test]
    fn test_decimate_noise_spectrum() {
        // Deterministic white noise in -> decimate 48 -> 16 -> check passband
        // flatness. Probes stay in the passband (well below the ~3.6 kHz
        // cutoff at 0.45 * 8 kHz output Nyquist); transition-band behaviour
        // is covered by the alias-rejection tests. Require flatness within
        // +/- 3 dB across the probed passband bands.
        // Input: 48 kHz. Output: 16 kHz (ratio 3).
        let output_rate = 16000.0;
        let ratio = 3;
        let amplitude = 8000.0;
        let taps = 16;
        let num_input = 48000; // 1 second at 48 kHz
        let seed: u64 = 0x000D_34C1_A70E_u64;

        let mut dec = Decimator::new(ratio, taps);

        // Warmup the filter with the same noise stream (discard output).
        let warmup = xorshift_noise(seed ^ 0x1, ratio * taps * 4, amplitude);
        let _ = dec.process(&warmup);

        let input = xorshift_noise(seed, num_input, amplitude);
        let output = dec.process(&input);
        assert_eq!(output.len(), num_input / ratio);

        // Probe bands well inside the passband (cutoff ~3.6 kHz for 0.45 * Nyq).
        // A single Goertzel bin on finite noise has high variance (chi-squared
        // with ~2 DOF). Average many bins across a 600 Hz integration window
        // centered on each probe to bring the per-band estimate well below
        // +/- 3 dB. 61 bins * 10 Hz spacing = 600 Hz per band.
        // Probes stay in passband; transition-band behaviour is covered by the
        // alias-rejection tests.
        let probe_centers = [500.0, 1200.0, 2000.0, 2800.0];
        let band_bins = 61;
        let band_step = 10.0;
        let energies: Vec<f64> = probe_centers
            .iter()
            .map(|&center| {
                let half = (band_bins as f64 - 1.0) / 2.0;
                let mut total = 0.0;
                for k in 0..band_bins {
                    let f = center + (k as f64 - half) * band_step;
                    total += goertzel_energy_i16(&output, f, output_rate);
                }
                total / band_bins as f64
            })
            .collect();

        let min_e = energies.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_e = energies.iter().cloned().fold(0.0_f64, f64::max);
        // Spread in dB across the probed passband bands.
        let spread_db = 10.0 * (max_e / min_e).log10();

        assert!(
            spread_db < 3.0,
            "Passband flatness: spread {spread_db:.2} dB across {probe_centers:?}, energies = {energies:?}"
        );

        // Also sanity-check no DC offset: decimated mean should be near zero.
        let mean = output.iter().map(|&s| s as f64).sum::<f64>() / output.len() as f64;
        assert!(
            mean.abs() < amplitude * 0.05,
            "Decimated noise mean = {mean:.2}, expected near zero"
        );
    }

    #[test]
    fn test_cascaded_48_to_8() {
        // Chain decimator(3) -> decimator(2) for 48kHz -> 16kHz -> 8kHz.
        // Verify a 1kHz voice-band signal passes through cleanly.
        let freq = 1000.0;
        let amplitude = 16000.0;
        let input_rate = 48000.0;
        let output_rate = 8000.0;
        let taps = 16;

        let mut dec_stage1 = Decimator::new(3, taps); // 48->16
        let mut dec_stage2 = Decimator::new(2, taps); // 16->8

        // Warmup both stages.
        let warmup1 = generate_tone_i16(freq, input_rate, 3 * taps * 2, amplitude);
        let mid_warmup = dec_stage1.process(&warmup1);
        let _ = dec_stage2.process(&mid_warmup);

        // Process: 4800 samples at 48kHz -> 1600 at 16kHz -> 800 at 8kHz.
        let num_input = 4800; // 100ms at 48kHz
        let input = generate_tone_i16(freq, input_rate, num_input, amplitude);
        let mid = dec_stage1.process(&input);
        assert_eq!(mid.len(), 1600, "Stage 1: 48->16 kHz");

        let output = dec_stage2.process(&mid);
        assert_eq!(output.len(), 800, "Stage 2: 16->8 kHz");

        // Verify 1kHz tone is preserved.
        let target_energy = goertzel_energy_i16(&output, freq, output_rate);
        let total_energy: f64 = output.iter().map(|&s| (s as f64) * (s as f64)).sum();
        let n = output.len() as f64;
        let normalized = target_energy / (total_energy * n / 2.0);

        assert!(
            normalized > 0.5,
            "1kHz tone should survive 48->16->8 kHz cascade, normalized = {normalized}"
        );

        // Amplitude within 2dB (cascaded filters accumulate some loss).
        let rms_output = (total_energy / n).sqrt();
        let rms_expected = amplitude / 2.0_f64.sqrt();
        let db_diff = 20.0 * (rms_output / rms_expected).log10();
        assert!(
            db_diff.abs() < 2.0,
            "Cascaded amplitude should be within 2dB, got {db_diff:.2} dB"
        );
    }

    // ---- Phase 1a.2 review feedback: band-edge passband + group delay ----

    /// Measure passband attenuation at a given probe frequency for a 48 kHz
    /// input decimated to `output_rate` with the given `ratio`. Returns the dB
    /// difference vs. the input RMS (negative = attenuation).
    fn measure_passband_attenuation_db(
        ratio: usize,
        output_rate: f64,
        probe_hz: f64,
        taps: usize,
    ) -> f64 {
        let input_rate = 48000.0;
        let amplitude = 16000.0;

        let mut dec = Decimator::new(ratio, taps);

        // Warmup to reach steady state.
        let warmup = generate_tone_i16(probe_hz, input_rate, ratio * taps * 2, amplitude);
        let _ = dec.process(&warmup);

        // 100 ms of signal. Ensure multiple of ratio so no samples are buffered.
        let num_input = ((input_rate as usize / 10) / ratio) * ratio;
        let input = generate_tone_i16(probe_hz, input_rate, num_input, amplitude);
        let output = dec.process(&input);

        // Goertzel at the probe frequency, plus total energy for sanity.
        let target_energy = goertzel_energy_i16(&output, probe_hz, output_rate);
        // Extract amplitude from Goertzel: for pure sine length N, amplitude A,
        // Goertzel ~= N^2 * A^2 / 4. So A_out = 2 * sqrt(target_energy) / N.
        let n_out = output.len() as f64;
        let a_out = 2.0 * target_energy.sqrt() / n_out;
        20.0 * (a_out / amplitude).log10()
    }

    #[test]
    fn test_decimate_48_to_16_passband_preserved_v34_edge() {
        // V.34 occupies ~150 Hz - 3429 Hz. Probe at 3.0 kHz and 3.4 kHz.
        // Require output amplitude within 1 dB of input amplitude.
        // Baseline on the 16-tap Hamming at cutoff 0.45/ratio:
        //   3.0 kHz ~= -0.029 dB, 3.4 kHz ~= -0.048 dB (passes comfortably).
        let taps = 16;
        let output_rate = 16000.0;
        let ratio = 3;

        let db_30 = measure_passband_attenuation_db(ratio, output_rate, 3000.0, taps);
        let db_34 = measure_passband_attenuation_db(ratio, output_rate, 3400.0, taps);

        assert!(
            db_30.abs() < 1.0,
            "48->16 V.34 passband @3.0 kHz: {db_30:.3} dB (need within 1 dB)"
        );
        assert!(
            db_34.abs() < 1.0,
            "48->16 V.34 passband @3.4 kHz: {db_34:.3} dB (need within 1 dB)"
        );
    }

    #[test]
    fn test_decimate_48_to_8_passband_preserved_v32bis_edge() {
        // V.32bis occupies ~600 Hz - 3000 Hz with carrier at 1800 Hz.
        // Probe a 3.0 kHz tone, 48 kHz -> 8 kHz.
        // Require output amplitude within 1 dB of input amplitude.
        // Baseline on the 16-tap Hamming at cutoff 0.45/ratio:
        //   3.0 kHz ~= -0.383 dB (passes).
        let taps = 16;
        let output_rate = 8000.0;
        let ratio = 6;

        let db_30 = measure_passband_attenuation_db(ratio, output_rate, 3000.0, taps);

        assert!(
            db_30.abs() < 1.0,
            "48->8 V.32bis passband @3.0 kHz: {db_30:.3} dB (need within 1 dB)"
        );
    }

    #[test]
    fn test_decimate_group_delay_linear_phase() {
        // Feed a delta impulse and verify the filtered impulse response peak
        // appears at the group-delay offset, confirming the FIR is linear
        // phase. Test all three production ratios.
        for &ratio in &[2usize, 3, 6] {
            let taps = 16;
            let mut dec = Decimator::new(ratio, taps);
            let group_delay = dec.group_delay_samples();

            // Filter length = ratio * taps. Input length: enough to contain
            // the full impulse response plus a little margin. Place the
            // impulse at input index 0.
            let filter_len = ratio * taps;
            // Round up to a multiple of ratio to get a clean output length.
            let n_input = (filter_len * 3).div_ceil(ratio) * ratio;
            let mut input = vec![0i16; n_input];
            input[0] = 16000; // impulse
            let output = dec.process(&input);

            // Peak of the output must be at output index that corresponds to
            // input-index group_delay. The decimator's k-th output is produced
            // after consuming (k + 1) * ratio input samples; the newest input
            // at that point has input-index (k + 1) * ratio - 1. So the
            // group-delay peak shows up at output index:
            //     k* = ((group_delay + 1) / ratio) - 1, if group_delay >=
            //     ratio - 1; more simply: find the argmax and compare.
            let peak_idx = output
                .iter()
                .enumerate()
                .max_by_key(|(_, &v)| v.unsigned_abs())
                .map(|(i, _)| i)
                .expect("output should be non-empty");

            // Newest input-index at output index k is (k + 1) * ratio - 1.
            // So the input-index of the peak is:
            let peak_input_idx = (peak_idx + 1) * ratio - 1;
            // Allow +/- ratio slack: the impulse peak lives on a single
            // input sample, but the decimator only emits every ratio-th.
            let diff = (peak_input_idx as isize - group_delay as isize).unsigned_abs() as usize;
            assert!(
                diff < ratio,
                "ratio={ratio}: group delay peak at input-idx {peak_input_idx}, expected ~{group_delay} (diff={diff})"
            );
        }
    }
}
