//! Linear interpolation audio resampler.

/// Linear interpolation resampler for converting between sample rates.
///
/// Designed for resampling interleaved stereo audio from 44100 Hz to 48000 Hz,
/// but works for any input/output rate combination.
pub struct LinearResampler {
    /// Ratio of output_rate / input_rate.
    ratio: f64,
    /// Position of the next output sample, in the CURRENT chunk's frame
    /// coordinates. A negative value means the sample falls between the
    /// previous chunk's final frame — index `-1` — and this chunk's first.
    position: f64,
    /// The previous chunk's final frame, which sits at index `-1`.
    ///
    /// `None` until the first chunk has been processed: at the very start of a
    /// stream there is no earlier frame to interpolate from.
    last: Option<(f32, f32)>,
}

impl LinearResampler {
    /// Create a new resampler for the given input and output sample rates.
    pub fn new(input_rate: u32, output_rate: u32) -> Self {
        Self {
            ratio: f64::from(output_rate) / f64::from(input_rate),
            position: 0.0,
            last: None,
        }
    }

    /// Resample interleaved stereo f32 samples.
    ///
    /// Input: `&[f32]` interleaved stereo at `input_rate` (pairs of [L, R, L, R, ...]).
    /// Output: `Vec<f32>` interleaved stereo at `output_rate`.
    ///
    /// # Chunking
    ///
    /// The stream is continuous across calls: this chunk's frame `-1` is the
    /// previous chunk's final frame, so splitting a stream into chunks produces
    /// the same samples as resampling it whole.
    ///
    /// MDD-BUG-CRU-00809: the position-zero case used to take its LEFT
    /// interpolation sample from the previous chunk's tail instead of this
    /// chunk's first frame. At a stream's start that emitted a zero; at every
    /// later chunk boundary it re-emitted the previous tail and dropped the new
    /// chunk's first frame. Even the identity 48 kHz path used by the encoder
    /// was corrupted, one stereo frame per chunk.
    ///
    /// A sample whose right-hand neighbour has not arrived yet cannot be
    /// interpolated, so it is held until the next chunk supplies it. That gives
    /// the stream a ONE-FRAME latency and leaves the final frame of a stream
    /// unemitted — inherent to streaming interpolation, not a defect, and below
    /// the encoder's resolution either way: Opus packetises 960 frames at a
    /// time and already discards a sub-packet remainder.
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        // Input must be stereo pairs
        let input_frames = input.len() / 2;
        if input_frames == 0 {
            return Vec::new();
        }

        let step = 1.0 / self.ratio;
        let mut output =
            Vec::with_capacity((((input_frames as f64) * self.ratio).ceil() as usize) * 2);

        // The last position whose right-hand neighbour is still inside this
        // chunk. Beyond it the interpolation needs the next chunk's first
        // frame, so the sample waits rather than being held or invented.
        let limit = (input_frames - 1) as f64;

        while self.position < limit {
            let idx = self.position.floor() as i64;
            let frac = (self.position - idx as f64) as f32;

            let (l0, r0) = if idx < 0 {
                // Only reachable once a previous chunk has carried a negative
                // position in, which also set `last`. The fallback keeps a
                // malformed state from panicking in a capture thread.
                self.last.unwrap_or((input[0], input[1]))
            } else {
                let base = idx as usize * 2;
                (input[base], input[base + 1])
            };

            if frac == 0.0 {
                // Exactly on a frame. Emitting it verbatim is what makes an
                // identity-rate stream BIT-exact: `l0 + (l1 - l0) * 0.0` is not
                // the identity function, because it turns a -0.0 sample into
                // +0.0.
                output.push(l0);
                output.push(r0);
            } else {
                // `idx + 1` is in range: the loop bound guarantees it.
                let base = (idx + 1) as usize * 2;
                let (l1, r1) = (input[base], input[base + 1]);
                output.push(l0 + (l1 - l0) * frac);
                output.push(r0 + (r1 - r0) * frac);
            }

            self.position += step;
        }

        // This chunk's final frame becomes the next chunk's frame -1, which is
        // why the carry is a whole `input_frames` and leaves `position >= -1`.
        self.last = Some((
            input[(input_frames - 1) * 2],
            input[(input_frames - 1) * 2 + 1],
        ));
        self.position -= input_frames as f64;

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_length_ratio() {
        let mut resampler = LinearResampler::new(44100, 48000);
        // 44100 stereo samples = 88200 floats
        let input = vec![0.0f32; 44100 * 2];
        let output = resampler.process(&input);
        // Output should be approximately 48000 stereo samples
        let output_frames = output.len() / 2;
        // Allow a tolerance of +/- 2 frames
        assert!(
            (output_frames as i64 - 48000).unsigned_abs() <= 2,
            "Expected ~48000 output frames, got {output_frames}"
        );
    }

    #[test]
    fn silence_stays_silent() {
        let mut resampler = LinearResampler::new(44100, 48000);
        let input = vec![0.0f32; 1000 * 2];
        let output = resampler.process(&input);
        for sample in &output {
            assert_eq!(*sample, 0.0, "Silence should remain silent");
        }
    }

    /// A fixture whose first frame is ZERO cannot see MDD-BUG-CRU-00809: the
    /// defect substituted zero there, which such a fixture agrees with. So this
    /// one starts away from zero, never repeats a frame, and carries a -0.0 to
    /// pin bit-exactness rather than near-equality.
    fn distinct_frames(count: usize) -> Vec<f32> {
        (0..count)
            .flat_map(|i| {
                if i == 3 {
                    [-0.0f32, 0.0f32]
                } else {
                    [(i as f32) + 1.5, -((i as f32) + 2.25)]
                }
            })
            .collect()
    }

    #[test]
    fn identity_ratio() {
        let mut resampler = LinearResampler::new(48000, 48000);
        let input = distinct_frames(100);
        let output = resampler.process(&input);

        // One frame of the chunk is held back: its right-hand neighbour has not
        // arrived, and interpolation cannot invent it. Everything emitted is
        // the input verbatim, to the bit.
        assert_eq!(output.len(), input.len() - 2);
        for (i, (a, b)) in output.iter().zip(input.iter()).enumerate() {
            assert_eq!(
                a.to_bits(),
                b.to_bits(),
                "1:1 resampling must preserve sample {i} exactly: got {a}, want {b}"
            );
        }
    }

    #[test]
    fn identity_rate_is_bit_exact_across_chunk_boundaries() {
        // MDD-BUG-CRU-00809: every chunk boundary used to overwrite the new
        // chunk's first frame with the previous chunk's last, silently mangling
        // one stereo frame per submitted chunk on the encoder's own 48 kHz path.
        let input = distinct_frames(64);

        let mut resampler = LinearResampler::new(48000, 48000);
        let mut output = Vec::new();
        let mut offset = 0usize;
        for frames in [7usize, 1, 13, 1, 29, 13] {
            let end = offset + frames * 2;
            output.extend_from_slice(&resampler.process(&input[offset..end]));
            offset = end;
        }
        assert_eq!(
            offset,
            input.len(),
            "the chunk sizes must cover the fixture"
        );

        // Chunking is invisible: the stream comes back verbatim, less the one
        // frame still awaiting its neighbour.
        assert_eq!(
            output.len(),
            input.len() - 2,
            "chunked identity resampling must neither drop nor duplicate frames"
        );
        for (i, (a, b)) in output.iter().zip(input.iter()).enumerate() {
            assert_eq!(
                a.to_bits(),
                b.to_bits(),
                "sample {i} was altered by a chunk boundary: got {a}, want {b}"
            );
        }
    }

    #[test]
    fn split_input_matches_whole_stream_resampling() {
        // The genuinely resampling path, on input that changes every frame so a
        // boundary error cannot hide. Where the chunks fall must not matter.
        let input: Vec<f32> = (0..500)
            .flat_map(|i| {
                let t = f64::from(i) / 44100.0;
                let l = (2.0 * std::f64::consts::PI * 440.0 * t).sin() as f32;
                let r = (2.0 * std::f64::consts::PI * 997.0 * t).sin() as f32;
                [l, r]
            })
            .collect();

        let whole = LinearResampler::new(44100, 48000).process(&input);

        let mut split_resampler = LinearResampler::new(44100, 48000);
        let mut split = Vec::new();
        let mut offset = 0usize;
        for frames in [1usize, 100, 37, 2, 200, 160] {
            let end = offset + frames * 2;
            split.extend_from_slice(&split_resampler.process(&input[offset..end]));
            offset = end;
        }
        assert_eq!(
            offset,
            input.len(),
            "the chunk sizes must cover the fixture"
        );

        assert_eq!(
            split.len(),
            whole.len(),
            "chunking changed the output length"
        );
        for (i, (a, b)) in split.iter().zip(whole.iter()).enumerate() {
            assert!(
                (a - b).abs() < 1e-6,
                "sample {i} differs between split and whole-stream resampling: {a} vs {b}"
            );
        }
    }

    #[test]
    fn empty_input() {
        let mut resampler = LinearResampler::new(44100, 48000);
        let output = resampler.process(&[]);
        assert!(output.is_empty());
    }

    #[test]
    fn sine_wave_preservation() {
        // Generate a 440 Hz sine wave at 44100 Hz, resample to 48000 Hz,
        // and verify the output is still a plausible sine wave (no NaN, no huge spikes).
        let mut resampler = LinearResampler::new(44100, 48000);
        let freq = 440.0f64;
        let input_rate = 44100.0f64;
        let num_samples = 4410; // 100ms

        let input: Vec<f32> = (0..num_samples)
            .flat_map(|i| {
                let t = i as f64 / input_rate;
                let val = (2.0 * std::f64::consts::PI * freq * t).sin() as f32;
                [val, val] // stereo
            })
            .collect();

        let output = resampler.process(&input);

        // Verify no NaN or infinity
        for sample in &output {
            assert!(sample.is_finite(), "Output contains non-finite value");
        }

        // Verify output is in [-1, 1] range (sine wave should not clip)
        for sample in &output {
            assert!(
                *sample >= -1.01 && *sample <= 1.01,
                "Output sample {sample} out of range"
            );
        }

        // Verify output length is approximately correct
        let expected_frames = (num_samples as f64 * 48000.0 / 44100.0).ceil() as usize;
        let output_frames = output.len() / 2;
        assert!(
            (output_frames as i64 - expected_frames as i64).unsigned_abs() <= 2,
            "Expected ~{expected_frames} output frames, got {output_frames}"
        );
    }

    #[test]
    fn multiple_chunks_consistent() {
        // Feeding data in chunks should produce the same total output length
        // as feeding it all at once (within +/- 1 frame).
        let mut resampler_whole = LinearResampler::new(44100, 48000);
        let mut resampler_chunks = LinearResampler::new(44100, 48000);

        let full_input = vec![0.5f32; 8820 * 2]; // 200ms at 44100

        let whole_output = resampler_whole.process(&full_input);

        let chunk_size = 441 * 2; // 10ms chunks
        let mut chunked_total = 0;
        for chunk in full_input.chunks(chunk_size) {
            let out = resampler_chunks.process(chunk);
            chunked_total += out.len();
        }

        let whole_frames = whole_output.len() / 2;
        let chunk_frames = chunked_total / 2;
        assert!(
            (whole_frames as i64 - chunk_frames as i64).unsigned_abs() <= 2,
            "Whole ({whole_frames}) vs chunked ({chunk_frames}) frame count mismatch"
        );
    }
}
