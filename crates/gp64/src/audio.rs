//! Chip-rate mixing and a streaming area resampler. Integer phases preserve
//! duration across arbitrary slice boundaries, including notes shorter than a callback.
use gp64_machine::AudioSample;
use gp64_opl2::SAMPLE_RATE;

pub struct Mixer {
    resampler: Resampler,
    last_cycle: Option<u64>,
}
impl Mixer {
    pub fn new(host_rate: u32) -> Self {
        Self {
            resampler: Resampler::new(SAMPLE_RATE, host_rate),
            last_cycle: None,
        }
    }
    #[cfg(test)]
    pub fn render(&mut self, samples: &[AudioSample]) -> Vec<f32> {
        let mut output = Vec::new();
        self.render_into(samples, &mut output);
        output
    }
    /// Replaces `output` with the host-rate samples, reusing its allocation.
    pub fn render_into(&mut self, samples: &[AudioSample], output: &mut Vec<f32>) {
        output.clear();
        for sample in samples {
            // Taps are already aligned at chip-rate cycle deadlines. Do not mix
            // a duplicate timestamp twice if a caller repeats a drained chunk.
            if self.last_cycle.is_some_and(|cycle| sample.cycle <= cycle) {
                continue;
            }
            self.last_cycle = Some(sample.cycle);
            let mixed = gp64::recording::mixed_sample(sample);
            self.resampler.push(mixed, output);
        }
    }
}
/// Each input sample covers `output_rate` ticks; each output sample averages
/// `input_rate` ticks. This retains even a pulse shorter than an output sample.
struct Resampler {
    input_rate: u32,
    output_rate: u32,
    filled: u32,
    sum: f64,
}
impl Resampler {
    fn new(input_rate: u32, output_rate: u32) -> Self {
        assert!(input_rate > 0 && output_rate > 0);
        Self {
            input_rate,
            output_rate,
            filled: 0,
            sum: 0.0,
        }
    }
    fn push(&mut self, sample: f32, output: &mut Vec<f32>) {
        let mut remaining = self.output_rate;
        while remaining > 0 {
            let take = remaining.min(self.input_rate - self.filled);
            self.sum += f64::from(sample) * f64::from(take);
            self.filled += take;
            remaining -= take;
            if self.filled == self.input_rate {
                output.push((self.sum / f64::from(self.input_rate)) as f32);
                self.sum = 0.0;
                self.filled = 0;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn tone(n: u32) -> Vec<AudioSample> {
        (0..n)
            .map(|i| AudioSample {
                cycle: u64::from(i + 1) * 400,
                speaker: if i < 50 {
                    if i % 20 < 10 {
                        0.75
                    } else {
                        -0.75
                    }
                } else {
                    0.0
                },
                opl2: if i < 50 {
                    if i % 20 < 10 {
                        4096
                    } else {
                        -4096
                    }
                } else {
                    0
                },
            })
            .collect()
    }
    #[test]
    fn short_tone_is_audible_at_each_host_rate() {
        for rate in [44_100, 48_000] {
            let output = Mixer::new(rate).render(&tone(SAMPLE_RATE / 25));
            assert!(output.iter().any(|&s| s > 0.4));
            assert!(output.iter().map(|s| s.abs()).sum::<f32>() > 10.0);
            let (mut producer, mut consumer) = crate::ring::ring(rate as usize * 400 / 1000);
            assert_eq!(producer.push(&output), output.len());
            assert!((0..rate / 25 + 1).any(|_| consumer.pop().unwrap().abs() > 0.4));
        }
        let mut resampler = Resampler::new(49_716, 8000);
        let mut output = Vec::new();
        for i in 0..10 {
            resampler.push(if i == 0 { 1.0 } else { 0.0 }, &mut output);
        }
        assert!(output[0] > 0.0);
    }
    #[test]
    fn equal_duration_and_streaming_chunk_boundaries() {
        let samples = tone(SAMPLE_RATE);
        for rate in [44_100, 48_000] {
            let expected = Mixer::new(rate).render(&samples);
            assert_eq!(expected.len(), rate as usize);
            let mut mixer = Mixer::new(rate);
            let chunked: Vec<_> = samples
                .chunks(31)
                .flat_map(|part| mixer.render(part))
                .collect();
            assert_eq!(chunked, expected);
            assert!(mixer.render(&samples).is_empty());
            let partial = Mixer::new(rate).render(&samples[..12345]);
            let duration_samples = 12345.0 / f64::from(SAMPLE_RATE) * f64::from(rate);
            assert!((partial.len() as f64 - duration_samples).abs() < 1.0);
        }
    }
}
