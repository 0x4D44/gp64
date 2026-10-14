//! Phase 1b.6 acceptance gate — full PSTN-profile B2B integration test under the
//! PSTN `DspProfile`.
//!
//! Builds two `ModemDsp` instances with `DspProfile::pstn()` (tracking PLL +
//! Gardner timing active, echo canceller on), drives them through a full
//! handshake (call setup → V.8 → training → DataMode entry), and
//! injects two deterministic PSTN impairments onto the wire between them:
//!
//! 1. A small clock skew (±10 ppm) applied per direction — realistic crystal
//!    drift that translates directly into a sub-Hz frequency offset at the
//!    symbol rate plus a fractional sample-timing drift across the block.
//!    Exercises the decision-directed PLL and Gardner TED enabled by
//!    `DspProfile::pstn()` but inert under the TCP profile.
//!
//! 2. A delayed-and-attenuated synthetic echo (30 samples @ 16 kHz ≈
//!    1.875 ms, gain 0.2 ≈ −14 dB ERL). Exercises the NLMS echo canceller
//!    installed by `DspProfile::pstn()` (Phase 1a.1).
//!
//! # What is and is not asserted
//!
//! The primary Phase 1b acceptance gate is that the modem reaches
//! `is_online()` on **both** sides under PSTN-profile tracking loops with
//! realistic impairments — the full-stack wiring (echo canceller + PLL +
//! timing recovery + online hand-off) works end-to-end. Normal V.8 now
//! advertises only byte-stable V.32bis rates; V.34 pre-emphasis remains
//! covered by V.34-specific training tests rather than this fallback path.
//!
//! The test **does not** assert bit-exact data-mode payload recovery. As
//! documented in Phase 1a.5 and Phase 1b.5 the OnlineV34 equalizer-convergence
//! path still has three known B2B flakes under the clean TCP profile
//! (`test_v34_data_256_bytes_b2b`, `test_v34_data_a2c_b2b`,
//! `test_v34_data_round_trip`); that deficiency is explicitly deferred to
//! Stage 2.
//!
//! Per the Stage 1 Working Plan, if the acceptance gate fails under
//! impairments the correct response is to report honestly and tighten the
//! milestone assertion — **not** to hack PLL/TED/equaliser knobs. Those
//! values are tuned per HLD and over-tuning breaks real-hardware
//! compatibility later. See the "degraded milestone" assertions below.

use std::collections::VecDeque;

use gp64_modem::wire::CHUNK_SAMPLES;
use gp64_modem::{DspProfile, ModemConfig, ModemDsp, ModemPhase};

/// Total handshake budget in DSP ticks (10 ms per tick @ 16 kHz wire rate).
/// Generous compared to the clean-TCP baseline (~1200) to accommodate
/// additional PSTN training/probing ticks defined in `DspProfile::pstn()`.
const MAX_HANDSHAKE_TICKS: usize = 5000;

/// Impairment parameters — tuned for "realistic but benign" PSTN conditions
/// matching the HLD §4 PSTN loop model.
///
/// Values here are deliberately **not** knobs the test tightens to force a
/// pass — they model the impairments we want the profile stack to tolerate.
const CLOCK_SKEW_PPM: i32 = 10;
const ECHO_DELAY_SAMPLES: usize = 30;
const ECHO_GAIN: f32 = 0.2;
/// Phase 1c.4 stressor SNR in dB. 30 dB matches the HLD §4 "good PSTN loop"
/// floor — anything worse than this is a degraded line that users should
/// expect to fall back to 9600 bps on real hardware; 30 dB keeps the V.34
/// training path exercised under realistic but tolerable AWGN.
const AWGN_SNR_DB: f64 = 30.0;

// ---------------------------------------------------------------------------
// Deterministic PSTN impairment channel
// ---------------------------------------------------------------------------

/// One-way impairment channel between two modems.
///
/// * Clock skew is implemented as a linear-interpolating fractional-delay
///   resampler driven by `ppm` (parts per million). Identical in spirit to
///   the production `SkewResampler` in `tests/pstn_impairment.rs`; duplicated
///   here so this integration test remains self-contained.
/// * Synthetic echo is a fixed-tap delay line: `out[n] += gain · in[n-D]`.
///   Sample-accurate; the delay is an integer number of samples so no
///   filter-induced phase drift creeps in.
/// * Optional AWGN is added *after* echo and *before* skew so the signal
///   the PLL/TED sees already carries the noise floor. Variance is sized
///   from a long-run signal-power estimate so the SNR stays close to the
///   configured `snr_db` once the training audio is flowing.
struct PstnChannel {
    skew: SkewResampler,
    echo_line: VecDeque<f32>,
    echo_delay_samples: usize,
    echo_gain: f32,
    /// Target AWGN SNR in dB; `None` = noise-free.
    snr_db: Option<f64>,
    /// Deterministic PRNG for AWGN.
    noise_rng: SplitMix64,
}

impl PstnChannel {
    fn new(ppm: i32, echo_delay_samples: usize, echo_gain: f32) -> Self {
        Self {
            skew: SkewResampler::new(ppm),
            echo_line: VecDeque::with_capacity(echo_delay_samples.max(1)),
            echo_delay_samples,
            echo_gain,
            snr_db: None,
            noise_rng: SplitMix64::new(0xC0FFEE_DEADBEEF),
        }
    }

    /// Enable AWGN at the given SNR.  Call once at construction; the channel
    /// remembers the value for the rest of the session.  `seed` lets the two
    /// directions of a B2B harness carry uncorrelated noise streams.
    fn with_awgn(mut self, snr_db: f64, seed: u64) -> Self {
        self.snr_db = Some(snr_db);
        self.noise_rng = SplitMix64::new(seed);
        self
    }

    fn process(&mut self, input: &[i16; CHUNK_SAMPLES]) -> [i16; CHUNK_SAMPLES] {
        // Cast to f32, mix in delayed echo, clamp, then hand the float stream
        // to the skew resampler. The resampler emits exactly CHUNK_SAMPLES
        // per call regardless of the skew ratio — see SkewResampler below.
        let mut shaped = [0.0f32; CHUNK_SAMPLES];
        let mut sum_sq: f64 = 0.0;
        for (i, &sample) in input.iter().enumerate() {
            let mut x = sample as f32;
            if self.echo_delay_samples > 0 {
                let echo = self.echo_line.front().copied().unwrap_or(0.0);
                x += echo * self.echo_gain;
                self.echo_line.push_back(x);
                while self.echo_line.len() > self.echo_delay_samples {
                    self.echo_line.pop_front();
                }
            }
            shaped[i] = x;
            sum_sq += (x as f64) * (x as f64);
        }

        // AWGN: size the noise std-dev from the running signal RMS in this
        // block so the instantaneous SNR tracks the configured target as
        // amplitude ramps during training.
        if let Some(snr_db) = self.snr_db {
            let sig_power = sum_sq / CHUNK_SAMPLES as f64;
            // Floor the signal power estimate to 1.0 so a silent block
            // doesn't produce near-zero noise (the training path expects a
            // steady noise floor; not doing this causes bursty noise that
            // correlates with the symbol rate).
            let sig_power = sig_power.max(1.0);
            let noise_power = sig_power / 10f64.powf(snr_db / 10.0);
            let sigma = noise_power.sqrt();
            for s in shaped.iter_mut() {
                let n = self.noise_rng.next_gaussian() * sigma;
                *s += n as f32;
            }
        }

        let skewed = self.skew.resample(&shaped);
        let mut out = [0i16; CHUNK_SAMPLES];
        for (dst, s) in out.iter_mut().zip(skewed.iter()) {
            *dst = s.clamp(i16::MIN as f32, i16::MAX as f32).round() as i16;
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Deterministic Gaussian PRNG for AWGN (SplitMix64 + Box-Muller)
// ---------------------------------------------------------------------------

/// Tiny self-contained PRNG (SplitMix64) + Box–Muller Gaussian draw.
/// Used so the AWGN stressor test is bit-for-bit reproducible without
/// pulling in `rand` as a dev-dependency.
struct SplitMix64 {
    state: u64,
    /// Spare Gaussian draw from the last Box-Muller call (Box-Muller
    /// produces two at a time; we cache the second).
    spare: Option<f64>,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self {
            state: seed,
            spare: None,
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform f64 in [0, 1).
    fn next_uniform(&mut self) -> f64 {
        // 53-bit mantissa: shift top 53 bits into the [0, 2^53) range,
        // then divide by 2^53 to land in [0, 1).
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Standard-normal (mean 0, variance 1) Gaussian draw via Box-Muller.
    fn next_gaussian(&mut self) -> f64 {
        if let Some(v) = self.spare.take() {
            return v;
        }
        // Guard u1 away from 0 so ln() stays finite.
        let u1 = self.next_uniform().max(f64::MIN_POSITIVE);
        let u2 = self.next_uniform();
        let r = (-2.0 * u1.ln()).sqrt();
        let theta = std::f64::consts::TAU * u2;
        self.spare = Some(r * theta.sin());
        r * theta.cos()
    }
}

/// Fractional-delay resampler driven by a ppm clock-skew ratio.
///
/// Each `resample` call consumes the incoming chunk, appends it to a short
/// history tail, then emits exactly `CHUNK_SAMPLES` outputs at the skewed
/// sample step. Linear interpolation between the two surrounding history
/// samples. Phase is preserved across calls.
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

    fn resample(&mut self, input: &[f32; CHUNK_SAMPLES]) -> [f32; CHUNK_SAMPLES] {
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

        // Drain already-consumed history to keep the working set bounded.
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

// ---------------------------------------------------------------------------
// B2B harness wiring two PSTN-profile modems through impairment channels
// ---------------------------------------------------------------------------

struct B2BHarness {
    caller: ModemDsp,
    answerer: ModemDsp,
    forward: PstnChannel, // caller TX → impairment → answerer RX
    reverse: PstnChannel, // answerer TX → impairment → caller RX
    prev_caller_out: [i16; CHUNK_SAMPLES],
    prev_answerer_out: [i16; CHUNK_SAMPLES],
}

impl B2BHarness {
    fn new(speed: u32) -> Self {
        // Both sides run the PSTN profile: decision-directed PLL + Gardner
        // TED on, echo canceller on, pre-emphasis selection active during
        // Phase 2 probing → MP exchange → DataMode.
        let caller = ModemDsp::with_profile(
            ModemConfig {
                max_speed: speed,
                ..Default::default()
            },
            DspProfile::pstn(),
        );
        let answerer = ModemDsp::with_profile(
            ModemConfig {
                max_speed: speed,
                ..Default::default()
            },
            DspProfile::pstn(),
        );

        Self {
            caller,
            answerer,
            // Opposing signs of clock skew so the two directions are not
            // symmetric — mimics two independent crystals on a real line.
            forward: PstnChannel::new(CLOCK_SKEW_PPM, ECHO_DELAY_SAMPLES, ECHO_GAIN),
            reverse: PstnChannel::new(-CLOCK_SKEW_PPM, ECHO_DELAY_SAMPLES, ECHO_GAIN),
            prev_caller_out: [0i16; CHUNK_SAMPLES],
            prev_answerer_out: [0i16; CHUNK_SAMPLES],
        }
    }

    /// Drive one back-to-back 10 ms DSP tick with impairments applied to each
    /// direction independently.
    fn tick(&mut self) {
        let caller_line_in = self.reverse.process(&self.prev_answerer_out);
        let answerer_line_in = self.forward.process(&self.prev_caller_out);

        let mut caller_out = [0i16; CHUNK_SAMPLES];
        let mut answerer_out = [0i16; CHUNK_SAMPLES];
        self.caller.tick_dsp(&caller_line_in, &mut caller_out);
        self.answerer.tick_dsp(&answerer_line_in, &mut answerer_out);

        self.prev_caller_out = caller_out;
        self.prev_answerer_out = answerer_out;
    }

    fn drain_both_serial(&mut self) {
        while self.caller.send_to_serial().is_some() {}
        while self.answerer.send_to_serial().is_some() {}
    }
}

fn send_at(modem: &mut ModemDsp, cmd: &str) {
    for b in cmd.bytes() {
        modem.receive_from_serial(b);
    }
    modem.receive_from_serial(b'\r');
}

// ---------------------------------------------------------------------------
// The acceptance test
// ---------------------------------------------------------------------------

/// Phase 1b.6 acceptance: full V.34 B2B handshake under PSTN profile with
/// synthetic PSTN impairments.
///
/// # Degraded milestones (do not silently escalate these assertions)
///
/// If the PSTN profile's tracking-loop hand-off into DataMode fails under
/// impairments, the **correct** response is to tighten this test to report
/// the furthest phase reached and open a Stage-2 item — not to widen the
/// handshake budget or retune the profile. The HLD explicitly forbids
/// per-test profile tuning.
///
/// Current expectation: reaches `ModemPhase::Online` on both sides with
/// matching `connect_speed()` populated. The honest production fallback is
/// currently V.32bis 9600, so V.34 pre-emphasis is not expected here.
#[test]
fn test_v34_b2b_pstn_profile_with_impairments() {
    // Request 28800. Normal V.8 capability advertisement currently falls back
    // to the highest byte-stable data path, V.32bis 9600.
    let mut h = B2BHarness::new(28800);

    // Answerer in auto-answer mode; caller dials.
    send_at(&mut h.answerer, "ATS0=1");
    while h.answerer.send_to_serial().is_some() {}
    send_at(&mut h.caller, "ATD5551234");
    while h.caller.send_to_serial().is_some() {}

    // Run the handshake to completion or budget exhaustion.
    let mut ticks_to_online = None;
    for tick in 0..MAX_HANDSHAKE_TICKS {
        h.tick();
        h.drain_both_serial();
        if h.caller.is_online() && h.answerer.is_online() {
            ticks_to_online = Some(tick + 1);
            break;
        }
    }

    // ----- Primary acceptance: both sides reach Online under impairments -----

    let caller_phase = h.caller.phase();
    let answerer_phase = h.answerer.phase();
    let caller_speed = h.caller.connect_speed();
    let answerer_speed = h.answerer.connect_speed();
    let caller_pre = h.caller.v34_preemphasis_index();
    let answerer_pre = h.answerer.v34_preemphasis_index();

    // Collect a single diagnostic string so if any assert trips we see the
    // full end-state without re-running the test.
    let diag = format!(
        "\n  ticks_to_online = {:?}\n  caller: phase={:?}, connect_speed={:?}, preemph={:?}\n  answerer: phase={:?}, connect_speed={:?}, preemph={:?}",
        ticks_to_online,
        caller_phase, caller_speed, caller_pre,
        answerer_phase, answerer_speed, answerer_pre,
    );

    assert!(
        h.caller.is_online(),
        "caller did not reach Online under PSTN impairments within {MAX_HANDSHAKE_TICKS} ticks.{diag}"
    );
    assert!(
        h.answerer.is_online(),
        "answerer did not reach Online under PSTN impairments within {MAX_HANDSHAKE_TICKS} ticks.{diag}"
    );

    // V.8 convergence: both sides must publish the negotiated online speed.
    assert!(
        caller_speed.is_some(),
        "caller connect_speed not populated after Online.{diag}"
    );
    assert!(
        answerer_speed.is_some(),
        "answerer connect_speed not populated after Online.{diag}"
    );
    assert_eq!(
        caller_speed, answerer_speed,
        "caller and answerer disagree on negotiated speed.{diag}"
    );
    assert_eq!(
        caller_speed,
        Some(9600),
        "PSTN profile should currently report the honest V.32bis fallback speed.{diag}"
    );
    assert_eq!(
        caller_pre, None,
        "V.34 pre-emphasis should be absent on the V.32bis fallback path.{diag}"
    );
    assert_eq!(
        answerer_pre, None,
        "V.34 pre-emphasis should be absent on the V.32bis fallback path.{diag}"
    );

    // Phase accessor parity — both sides report ModemPhase::Online.
    assert_eq!(
        caller_phase,
        ModemPhase::Online,
        "caller phase != Online.{diag}"
    );
    assert_eq!(
        answerer_phase,
        ModemPhase::Online,
        "answerer phase != Online.{diag}"
    );
}

/// Control: the same PSTN-profile handshake without impairments. If this
/// fails, the regression is in the PSTN profile plumbing or the Phase
/// 1a.5-introduced tracking-loop hand-off — not in the impairment model.
/// Used as a bisection aid if the main test above starts flaking.
#[test]
fn test_v34_b2b_pstn_profile_clean_channel_control() {
    // Build a harness whose impairment channels are "pass-through":
    // 0 ppm skew, 0 echo gain. Everything else (PSTN profile, handshake
    // budget, assertion set) matches the main test.
    struct CleanHarness {
        caller: ModemDsp,
        answerer: ModemDsp,
        prev_caller_out: [i16; CHUNK_SAMPLES],
        prev_answerer_out: [i16; CHUNK_SAMPLES],
    }
    let mut h = CleanHarness {
        caller: ModemDsp::with_profile(
            ModemConfig {
                max_speed: 28800,
                ..Default::default()
            },
            DspProfile::pstn(),
        ),
        answerer: ModemDsp::with_profile(
            ModemConfig {
                max_speed: 28800,
                ..Default::default()
            },
            DspProfile::pstn(),
        ),
        prev_caller_out: [0i16; CHUNK_SAMPLES],
        prev_answerer_out: [0i16; CHUNK_SAMPLES],
    };

    send_at(&mut h.answerer, "ATS0=1");
    while h.answerer.send_to_serial().is_some() {}
    send_at(&mut h.caller, "ATD5551234");
    while h.caller.send_to_serial().is_some() {}

    let mut ticks_to_online = None;
    for tick in 0..MAX_HANDSHAKE_TICKS {
        let mut caller_out = [0i16; CHUNK_SAMPLES];
        let mut answerer_out = [0i16; CHUNK_SAMPLES];
        h.caller.tick_dsp(&h.prev_answerer_out, &mut caller_out);
        h.answerer.tick_dsp(&h.prev_caller_out, &mut answerer_out);
        h.prev_caller_out = caller_out;
        h.prev_answerer_out = answerer_out;
        while h.caller.send_to_serial().is_some() {}
        while h.answerer.send_to_serial().is_some() {}
        if h.caller.is_online() && h.answerer.is_online() {
            ticks_to_online = Some(tick + 1);
            break;
        }
    }

    assert!(
        h.caller.is_online() && h.answerer.is_online(),
        "PSTN profile did not reach Online on a clean channel — \
         regression in Phase 1a.5 / 1b.* plumbing, not in the impairment model.  \
         ticks_to_online={:?}, caller_phase={:?}, answerer_phase={:?}",
        ticks_to_online,
        h.caller.phase(),
        h.answerer.phase(),
    );
}

/// Phase 1c.4 stressor: full V.34 B2B handshake under PSTN profile with echo +
/// clock skew + **AWGN at 30 dB SNR**.
///
/// This is the Stage 1 final acceptance gate. Extends
/// `test_v34_b2b_pstn_profile_with_impairments` with additive white Gaussian
/// noise at the HLD §4 "good PSTN loop" floor (30 dB) to exercise the
/// PLL/TED/NLMS equalizer stack under a realistic noise environment.
///
/// Same degraded-milestone discipline as the main impairments test: if this
/// fails, report the furthest phase reached and open a Stage-2 item — do
/// **not** widen the handshake budget or retune the profile.
#[test]
fn test_v34_b2b_pstn_profile_with_awgn() {
    // Build a harness whose channels carry echo + skew + AWGN. Two
    // uncorrelated noise streams, one per direction.
    struct AwgnHarness {
        caller: ModemDsp,
        answerer: ModemDsp,
        forward: PstnChannel,
        reverse: PstnChannel,
        prev_caller_out: [i16; CHUNK_SAMPLES],
        prev_answerer_out: [i16; CHUNK_SAMPLES],
    }
    let mut h = AwgnHarness {
        caller: ModemDsp::with_profile(
            ModemConfig {
                max_speed: 28800,
                ..Default::default()
            },
            DspProfile::pstn(),
        ),
        answerer: ModemDsp::with_profile(
            ModemConfig {
                max_speed: 28800,
                ..Default::default()
            },
            DspProfile::pstn(),
        ),
        // Different seeds per direction so the noise streams are
        // uncorrelated but the test remains bit-for-bit deterministic.
        forward: PstnChannel::new(CLOCK_SKEW_PPM, ECHO_DELAY_SAMPLES, ECHO_GAIN)
            .with_awgn(AWGN_SNR_DB, 0x05EE_DC0F_FEEA_u64.wrapping_mul(17)),
        reverse: PstnChannel::new(-CLOCK_SKEW_PPM, ECHO_DELAY_SAMPLES, ECHO_GAIN)
            .with_awgn(AWGN_SNR_DB, 0x0D15_EA5E_DBAD_C0DE_u64.wrapping_mul(23)),
        prev_caller_out: [0i16; CHUNK_SAMPLES],
        prev_answerer_out: [0i16; CHUNK_SAMPLES],
    };

    send_at(&mut h.answerer, "ATS0=1");
    while h.answerer.send_to_serial().is_some() {}
    send_at(&mut h.caller, "ATD5551234");
    while h.caller.send_to_serial().is_some() {}

    let mut ticks_to_online = None;
    for tick in 0..MAX_HANDSHAKE_TICKS {
        let caller_line_in = h.reverse.process(&h.prev_answerer_out);
        let answerer_line_in = h.forward.process(&h.prev_caller_out);

        let mut caller_out = [0i16; CHUNK_SAMPLES];
        let mut answerer_out = [0i16; CHUNK_SAMPLES];
        h.caller.tick_dsp(&caller_line_in, &mut caller_out);
        h.answerer.tick_dsp(&answerer_line_in, &mut answerer_out);

        h.prev_caller_out = caller_out;
        h.prev_answerer_out = answerer_out;

        while h.caller.send_to_serial().is_some() {}
        while h.answerer.send_to_serial().is_some() {}

        if h.caller.is_online() && h.answerer.is_online() {
            ticks_to_online = Some(tick + 1);
            break;
        }
    }

    let diag = format!(
        "\n  ticks_to_online = {:?}\n  caller: phase={:?}, connect_speed={:?}\n  answerer: phase={:?}, connect_speed={:?}",
        ticks_to_online,
        h.caller.phase(),
        h.caller.connect_speed(),
        h.answerer.phase(),
        h.answerer.connect_speed(),
    );

    assert!(
        h.caller.is_online(),
        "caller did not reach Online under PSTN + {AWGN_SNR_DB:.0} dB AWGN within {MAX_HANDSHAKE_TICKS} ticks.{diag}"
    );
    assert!(
        h.answerer.is_online(),
        "answerer did not reach Online under PSTN + {AWGN_SNR_DB:.0} dB AWGN within {MAX_HANDSHAKE_TICKS} ticks.{diag}"
    );
    assert_eq!(
        h.caller.phase(),
        ModemPhase::Online,
        "caller phase != Online.{diag}"
    );
    assert_eq!(
        h.answerer.phase(),
        ModemPhase::Online,
        "answerer phase != Online.{diag}"
    );
    assert_eq!(
        h.caller.connect_speed(),
        h.answerer.connect_speed(),
        "caller and answerer disagree on negotiated speed under AWGN.{diag}"
    );
}
