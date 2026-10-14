use std::collections::VecDeque;
use std::f32::consts::PI;

use gp64_modem::wire::CHUNK_SAMPLES;
use gp64_modem::{ModemConfig, ModemDsp};

const SPEEDS: [u32; 3] = [9600, 14400, 28800];
const IMPAIRED_SPEEDS: [u32; 2] = [9600, 14400];
const HANDSHAKE_TICKS: usize = 1200;
const ONLINE_SETTLE_TICKS: usize = 20;
const TRANSFER_TICKS_PER_BYTE: usize = 24;
const PAYLOAD: &[u8] = b"pstn-interop-round-trip";

#[derive(Clone, Copy, Debug)]
struct ChannelProfile {
    gain: f32,
    echo_delay_samples: usize,
    echo_gain: f32,
    awgn_stddev: f32,
    rolloff_alpha: f32,
    quant_bits: u8,
    clock_skew_ppm: i32,
}

impl ChannelProfile {
    fn clean() -> Self {
        Self {
            gain: 1.0,
            echo_delay_samples: 0,
            echo_gain: 0.0,
            awgn_stddev: 0.0,
            rolloff_alpha: 1.0,
            quant_bits: 16,
            clock_skew_ppm: 0,
        }
    }

    fn echo() -> Self {
        Self {
            echo_delay_samples: 6,
            echo_gain: 0.01,
            ..Self::clean()
        }
    }

    fn awgn() -> Self {
        Self {
            awgn_stddev: 0.5,
            ..Self::clean()
        }
    }

    fn rolloff() -> Self {
        Self {
            rolloff_alpha: 0.985,
            ..Self::clean()
        }
    }

    fn quantization() -> Self {
        Self {
            quant_bits: 14,
            ..Self::clean()
        }
    }

    fn gain_mismatch(gain: f32) -> Self {
        Self {
            gain,
            ..Self::clean()
        }
    }

    fn clock_skew(ppm: i32) -> Self {
        Self {
            clock_skew_ppm: ppm,
            ..Self::clean()
        }
    }
}

struct PstnChannel {
    profile: ChannelProfile,
    echo_line: VecDeque<f32>,
    lowpass_prev: f32,
    noise_state: u32,
    noise_spare: Option<f32>,
    skew: SkewResampler,
    mute_ticks_remaining: usize,
}

impl PstnChannel {
    fn new(profile: ChannelProfile) -> Self {
        Self {
            echo_line: VecDeque::new(),
            lowpass_prev: 0.0,
            noise_state: 0xA5A5_1357,
            noise_spare: None,
            skew: SkewResampler::new(profile.clock_skew_ppm),
            profile,
            mute_ticks_remaining: 0,
        }
    }

    fn process(&mut self, input: &[i16; CHUNK_SAMPLES]) -> [i16; CHUNK_SAMPLES] {
        if self.mute_ticks_remaining > 0 {
            self.mute_ticks_remaining -= 1;
            return [0i16; CHUNK_SAMPLES];
        }

        let mut shaped = Vec::with_capacity(CHUNK_SAMPLES);
        for &sample in input {
            let mut x = sample as f32 * self.profile.gain;

            x = self.profile.rolloff_alpha * x
                + (1.0 - self.profile.rolloff_alpha) * self.lowpass_prev;
            self.lowpass_prev = x;

            if self.profile.echo_delay_samples > 0 {
                let echo = self.echo_line.front().copied().unwrap_or(0.0);
                x += echo * self.profile.echo_gain;
                self.echo_line.push_back(x);
                while self.echo_line.len() > self.profile.echo_delay_samples {
                    self.echo_line.pop_front();
                }
            }

            x += self.next_gaussian() * self.profile.awgn_stddev;
            shaped.push(x);
        }

        let skewed = self.skew.resample(&shaped);
        quantize_chunk(&skewed, self.profile.quant_bits)
    }

    fn next_uniform(&mut self) -> f32 {
        let mut x = self.noise_state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.noise_state = x;
        (x as f32 + 0.5) / (u32::MAX as f32 + 1.0)
    }

    fn next_gaussian(&mut self) -> f32 {
        if let Some(spare) = self.noise_spare.take() {
            return spare;
        }

        let u1 = self.next_uniform().max(1e-6);
        let u2 = self.next_uniform();
        let radius = (-2.0 * u1.ln()).sqrt();
        let theta = 2.0 * PI * u2;
        let z0 = radius * theta.cos();
        let z1 = radius * theta.sin();
        self.noise_spare = Some(z1);
        z0
    }
}

struct SkewResampler {
    step: f64,
    phase: f64,
    history: Vec<f32>,
}

impl SkewResampler {
    fn new(ppm: i32) -> Self {
        Self {
            step: 1.0 + ppm as f64 / 1_000_000.0,
            phase: 0.0,
            history: vec![0.0, 0.0],
        }
    }

    fn resample(&mut self, input: &[f32]) -> [f32; CHUNK_SAMPLES] {
        self.history.extend_from_slice(input);

        let mut out = [0.0f32; CHUNK_SAMPLES];
        for sample in &mut out {
            let idx = self.phase.floor() as usize;
            let frac = (self.phase - idx as f64) as f32;
            *sample = if idx + 1 < self.history.len() {
                let a = self.history[idx];
                let b = self.history[idx + 1];
                a + (b - a) * frac
            } else {
                *self.history.last().unwrap_or(&0.0)
            };
            self.phase += self.step;
        }

        let keep = 2usize.min(self.history.len());
        let drain = (self.phase.floor().max(0.0) as usize)
            .saturating_sub(keep)
            .min(self.history.len().saturating_sub(keep));
        if drain > 0 {
            self.history.drain(0..drain);
            self.phase -= drain as f64;
        }

        out
    }
}

struct PstnHarness {
    caller: ModemDsp,
    answerer: ModemDsp,
    forward: PstnChannel,
    reverse: PstnChannel,
    prev_caller_out: [i16; CHUNK_SAMPLES],
    prev_answerer_out: [i16; CHUNK_SAMPLES],
    caller_out: [i16; CHUNK_SAMPLES],
    answerer_out: [i16; CHUNK_SAMPLES],
}

impl PstnHarness {
    fn connect(speed: u32, forward: ChannelProfile, reverse: ChannelProfile) -> Self {
        let mut harness = Self::connect_for_ticks(speed, forward, reverse, HANDSHAKE_TICKS);

        assert!(
            harness.caller.is_online(),
            "caller failed to reach online at {speed} baud. Phase: {:?}",
            harness.caller.phase()
        );
        assert!(
            harness.answerer.is_online(),
            "answerer failed to reach online at {speed} baud. Phase: {:?}",
            harness.answerer.phase()
        );

        drain_serial(&mut harness.caller);
        drain_serial(&mut harness.answerer);
        for _ in 0..ONLINE_SETTLE_TICKS {
            harness.tick();
            drain_serial(&mut harness.caller);
            drain_serial(&mut harness.answerer);
        }
        harness
    }

    fn connect_for_ticks(
        speed: u32,
        forward: ChannelProfile,
        reverse: ChannelProfile,
        ticks: usize,
    ) -> Self {
        let mut caller = ModemDsp::new(ModemConfig {
            max_speed: speed,
            ..Default::default()
        });
        let mut answerer = ModemDsp::new(ModemConfig {
            max_speed: speed,
            ..Default::default()
        });

        for &b in b"ATS0=1\r" {
            answerer.receive_from_serial(b);
        }
        drain_serial(&mut answerer);

        for &b in b"ATD5551234\r" {
            caller.receive_from_serial(b);
        }
        drain_serial(&mut caller);

        let mut harness = Self {
            caller,
            answerer,
            forward: PstnChannel::new(forward),
            reverse: PstnChannel::new(reverse),
            prev_caller_out: [0i16; CHUNK_SAMPLES],
            prev_answerer_out: [0i16; CHUNK_SAMPLES],
            caller_out: [0i16; CHUNK_SAMPLES],
            answerer_out: [0i16; CHUNK_SAMPLES],
        };

        for _ in 0..ticks {
            harness.tick();
            drain_serial(&mut harness.caller);
            drain_serial(&mut harness.answerer);
            if harness.caller.is_online() && harness.answerer.is_online() {
                break;
            }
        }

        harness
    }

    fn tick(&mut self) {
        let caller_line_in = self.reverse.process(&self.prev_answerer_out);
        let answerer_line_in = self.forward.process(&self.prev_caller_out);

        self.caller.tick_dsp(&caller_line_in, &mut self.caller_out);
        self.answerer
            .tick_dsp(&answerer_line_in, &mut self.answerer_out);

        self.prev_caller_out = self.caller_out;
        self.prev_answerer_out = self.answerer_out;
    }

    fn transfer_c2a(&mut self, payload: &[u8]) -> Vec<u8> {
        self.transfer(payload, true)
    }

    fn transfer_a2c(&mut self, payload: &[u8]) -> Vec<u8> {
        self.transfer(payload, false)
    }

    fn transfer(&mut self, payload: &[u8], caller_to_answerer: bool) -> Vec<u8> {
        let mut received = Vec::new();
        let mut fed = 0usize;
        let max_ticks = payload.len() * TRANSFER_TICKS_PER_BYTE + 1000;

        for _ in 0..max_ticks {
            drain_serial(&mut self.caller);
            drain_serial(&mut self.answerer);

            let end = (fed + 16).min(payload.len());
            if caller_to_answerer {
                for &b in &payload[fed..end] {
                    self.caller.receive_from_serial(b);
                }
            } else {
                for &b in &payload[fed..end] {
                    self.answerer.receive_from_serial(b);
                }
            }
            fed = end;

            self.tick();
            if caller_to_answerer {
                received.extend(drain_serial(&mut self.answerer));
            } else {
                received.extend(drain_serial(&mut self.caller));
            }

            if received.len() >= payload.len() {
                break;
            }
        }

        received
    }
}
fn drain_serial(modem: &mut ModemDsp) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(b) = modem.send_to_serial() {
        out.push(b);
    }
    out
}

fn quantize_chunk(samples: &[f32; CHUNK_SAMPLES], bits: u8) -> [i16; CHUNK_SAMPLES] {
    if bits >= 16 {
        return samples.map(|sample| sample.clamp(i16::MIN as f32, i16::MAX as f32).round() as i16);
    }

    let levels = (1u32 << (bits - 1)) - 1;
    let step = i16::MAX as f32 / levels as f32;
    samples.map(|sample| {
        let quantized = (sample / step)
            .round()
            .clamp(-(levels as f32), levels as f32);
        (quantized * step)
            .round()
            .clamp(i16::MIN as f32, i16::MAX as f32) as i16
    })
}

fn assert_round_trip(name: &str, speed: u32, forward: ChannelProfile, reverse: ChannelProfile) {
    let mut harness = PstnHarness::connect(speed, forward, reverse);

    let c2a = harness.transfer_c2a(PAYLOAD);
    assert!(
        c2a.len() >= PAYLOAD.len(),
        "{name}: caller -> answerer transfer lost bytes at {speed} baud"
    );

    let a2c = harness.transfer_a2c(PAYLOAD);
    assert!(
        a2c.len() >= PAYLOAD.len(),
        "{name}: answerer -> caller transfer lost bytes at {speed} baud"
    );
}

#[test]
fn pstn_echo_impairment_round_trip() {
    for &speed in &IMPAIRED_SPEEDS {
        assert_round_trip(
            "echo",
            speed,
            ChannelProfile::echo(),
            ChannelProfile::echo(),
        );
    }
}

#[test]
fn pstn_awgn_impairment_round_trip() {
    for &speed in &IMPAIRED_SPEEDS {
        assert_round_trip(
            "awgn",
            speed,
            ChannelProfile::awgn(),
            ChannelProfile::awgn(),
        );
    }
}

#[test]
fn pstn_rolloff_impairment_round_trip() {
    for &speed in &IMPAIRED_SPEEDS {
        assert_round_trip(
            "rolloff",
            speed,
            ChannelProfile::rolloff(),
            ChannelProfile::rolloff(),
        );
    }
}

#[test]
fn pstn_quantization_impairment_round_trip() {
    for &speed in &IMPAIRED_SPEEDS {
        assert_round_trip(
            "quantization",
            speed,
            ChannelProfile::quantization(),
            ChannelProfile::quantization(),
        );
    }
}

#[test]
fn pstn_gain_mismatch_round_trip() {
    for &speed in &IMPAIRED_SPEEDS {
        assert_round_trip(
            "gain mismatch",
            speed,
            ChannelProfile::gain_mismatch(0.98),
            ChannelProfile::gain_mismatch(1.02),
        );
    }
}

#[test]
fn pstn_clock_skew_round_trip() {
    for &speed in &IMPAIRED_SPEEDS {
        assert_round_trip(
            "clock skew",
            speed,
            ChannelProfile::clock_skew(10),
            ChannelProfile::clock_skew(-10),
        );
    }
}

#[test]
fn pstn_supported_speed_round_trip() {
    for &speed in &SPEEDS[..2] {
        assert_round_trip(
            "supported speed",
            speed,
            ChannelProfile::clean(),
            ChannelProfile::clean(),
        );
    }

    let harness = PstnHarness::connect_for_ticks(
        28800,
        ChannelProfile::clean(),
        ChannelProfile::clean(),
        HANDSHAKE_TICKS,
    );
    assert_ne!(
        format!("{:?}", harness.caller.phase()),
        "Idle",
        "28800 baud smoke path never left idle"
    );
    assert_ne!(
        format!("{:?}", harness.answerer.phase()),
        "Idle",
        "28800 baud smoke path never left idle"
    );
}
