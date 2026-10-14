//! Callback-side playback state: a prebuffer gate, underrun concealment and
//! high-fill recovery. Runs inside the device callback, so it never locks,
//! allocates or logs.
use crate::ring::Consumer;

/// Re-arm level after an underrun: short, so the gap stays short.
const PREBUFFER_MS: usize = 20;
/// Fill the pacer holds; the first start waits for it.
pub const TARGET_FILL_MS: usize = 50;
/// Real audio before a known gap ramps down over this span.
const FADE_OUT_MS: usize = 3;
/// Resumed audio ramps up over this many frames.
const FADE_IN_FRAMES: usize = 128;
/// Per-frame decay of the last played sample while no data is available;
/// reaches silence in a few milliseconds instead of stepping to zero.
const UNDERRUN_DECAY: f32 = 0.995;
/// Above this fill the backlog is discarded back to the target.
const HIGH_FILL_MS: usize = 300;
/// The high-fill episode ends below this fill (hysteresis).
const OK_FILL_MS: usize = 200;

pub fn frames_for_ms(rate: u32, ms: usize) -> usize {
    rate as usize * ms / 1000
}

pub struct OutputState {
    last: f32,
    waiting: bool,
    played: bool,
    prebuffer: usize,
    target: usize,
    fade_out: usize,
    fade_in_remaining: usize,
    high_fill: usize,
    ok_fill: usize,
    in_high_fill: bool,
}
impl OutputState {
    pub fn new(rate: u32) -> Self {
        Self {
            last: 0.0,
            waiting: true,
            played: false,
            prebuffer: frames_for_ms(rate, PREBUFFER_MS),
            target: frames_for_ms(rate, TARGET_FILL_MS),
            fade_out: frames_for_ms(rate, FADE_OUT_MS),
            fade_in_remaining: 0,
            high_fill: frames_for_ms(rate, HIGH_FILL_MS),
            ok_fill: frames_for_ms(rate, OK_FILL_MS),
            in_high_fill: false,
        }
    }
    /// Produces exactly `frames` mono samples through `write(index, sample)`.
    pub fn render(
        &mut self,
        consumer: &mut Consumer,
        frames: usize,
        mut write: impl FnMut(usize, f32),
    ) {
        let queued = consumer.len();
        if !self.in_high_fill && queued > self.high_fill {
            // Producer and consumer return to the same long-run rate, so a
            // backlog never drains by itself. Keep the target plus this
            // callback and let the gate and fade-in restart playback.
            self.in_high_fill = true;
            let keep = (self.prebuffer.max(self.target) + frames).min(consumer.capacity());
            if consumer.skip(queued.saturating_sub(keep)) > 0 {
                self.waiting = true;
            }
        } else if self.in_high_fill && queued < self.ok_fill {
            self.in_high_fill = false;
        }
        if self.waiting {
            // The first start waits for the target; a re-arm after an
            // underrun waits only for the prebuffer and the pacer refills.
            let gate = if self.played {
                self.prebuffer
            } else {
                self.prebuffer.max(self.target)
            };
            if consumer.len() >= gate.max(frames) {
                self.waiting = false;
                self.played = true;
                self.fade_in_remaining = FADE_IN_FRAMES;
                consumer.mark_started();
            } else {
                for index in 0..frames {
                    write(index, self.decay());
                }
                return;
            }
        }
        // A callback holding fewer frames than it plays knows where the gap
        // falls, so the audio before it can fade out. Data that arrives
        // during the callback belongs to the next refill.
        let playable = (self.fade_out > 0)
            .then(|| consumer.len())
            .filter(|&queued| queued < frames);
        let fade_start = playable.map(|playable| playable.saturating_sub(self.fade_out));
        let mut dry = false;
        for index in 0..frames {
            if !dry && playable == Some(index) {
                dry = true;
            }
            let sample = if dry { None } else { consumer.pop() };
            let Some(mut sample) = sample else {
                dry = true;
                self.waiting = true;
                self.fade_in_remaining = 0;
                write(index, self.decay());
                continue;
            };
            if self.fade_in_remaining > 0 {
                let t = 1.0 - self.fade_in_remaining as f32 / FADE_IN_FRAMES as f32;
                sample *= t * t;
                self.fade_in_remaining -= 1;
            }
            if let (Some(playable), Some(start)) = (playable, fade_start) {
                if index >= start {
                    let t = (playable - index) as f32 / (playable - start) as f32;
                    sample *= t * t;
                }
            }
            // The decay tail continues from what was actually played.
            self.last = sample;
            write(index, sample);
        }
    }
    fn decay(&mut self) -> f32 {
        self.last *= UNDERRUN_DECAY;
        self.last
    }
}

/// Mute and unmute ramp over this span instead of stepping.
const MUTE_RAMP_MS: usize = 5;

/// Host-side mute applied after [`OutputState::render`], so the ring keeps
/// draining at the device rate and pacing feedback is unchanged.
pub struct MuteGain {
    gain: f32,
    step: f32,
}
impl MuteGain {
    pub fn new(rate: u32, muted: bool) -> Self {
        Self {
            gain: if muted { 0.0 } else { 1.0 },
            step: 1.0 / frames_for_ms(rate, MUTE_RAMP_MS).max(1) as f32,
        }
    }
    pub fn apply(&mut self, muted: bool, sample: f32) -> f32 {
        self.gain = if muted {
            (self.gain - self.step).max(0.0)
        } else {
            (self.gain + self.step).min(1.0)
        };
        sample * self.gain
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring::{ring, Producer};

    const RATE: u32 = 48_000;
    const CALLBACK: usize = 512;

    fn setup() -> (Producer, Consumer, OutputState) {
        let (producer, consumer) = ring(frames_for_ms(RATE, 400));
        (producer, consumer, OutputState::new(RATE))
    }
    fn play(state: &mut OutputState, consumer: &mut Consumer, frames: usize) -> Vec<f32> {
        let mut out = vec![f32::NAN; frames];
        state.render(consumer, frames, |index, sample| out[index] = sample);
        assert!(out.iter().all(|sample| sample.is_finite()));
        out
    }
    /// Largest jump between adjacent samples. A step into silence from the
    /// full-scale test signal is 1.0; the shortest fade here (a 96-frame
    /// fade-out when only that much was queued) peaks near 0.021.
    fn max_step(samples: &[f32]) -> f32 {
        samples
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0, f32::max)
    }

    #[test]
    fn first_start_waits_for_target_then_fades_in() {
        let (mut producer, mut consumer, mut state) = setup();
        let target = frames_for_ms(RATE, TARGET_FILL_MS);
        producer.push(&vec![1.0; target - 1]);
        assert!(play(&mut state, &mut consumer, CALLBACK)
            .iter()
            .all(|&sample| sample == 0.0));
        assert_eq!(consumer.len(), target - 1);
        assert!(!producer.started());
        producer.push(&[1.0]);
        let out = play(&mut state, &mut consumer, CALLBACK);
        assert!(producer.started());
        assert_eq!(consumer.len(), target - CALLBACK);
        assert_eq!(out[0], 0.0);
        assert!(out.windows(2).all(|pair| pair[1] >= pair[0]));
        assert!(out[FADE_IN_FRAMES - 1] < 1.0);
        assert!(out[FADE_IN_FRAMES..].iter().all(|&sample| sample == 1.0));
    }

    #[test]
    fn underrun_fades_and_decays_then_rearms_at_prebuffer() {
        let (mut producer, mut consumer, mut state) = setup();
        let target = frames_for_ms(RATE, TARGET_FILL_MS);
        // A constant full-scale signal makes any step into silence obvious.
        producer.push(&vec![1.0; target + CALLBACK / 2]);
        let mut out = Vec::new();
        for _ in 0..12 {
            out.extend(play(&mut state, &mut consumer, CALLBACK));
        }
        assert_eq!(consumer.len(), 0);
        assert!(max_step(&out) < 0.05, "step {}", max_step(&out));
        assert!(out.last().unwrap().abs() < 0.01);
        // Underrun at an exact callback boundary: the tail still decays.
        let (mut producer, mut consumer, mut state) = setup();
        producer.push(&vec![1.0; CALLBACK * 6]);
        assert!(CALLBACK * 6 >= target);
        let mut out = Vec::new();
        for _ in 0..8 {
            out.extend(play(&mut state, &mut consumer, CALLBACK));
        }
        assert!(max_step(&out) < 0.05, "step {}", max_step(&out));
        // Re-arm waits for the 20 ms prebuffer, not the 50 ms target.
        let prebuffer = frames_for_ms(RATE, PREBUFFER_MS);
        producer.push(&vec![1.0; prebuffer - 1]);
        out.extend(play(&mut state, &mut consumer, CALLBACK));
        assert_eq!(consumer.len(), prebuffer - 1);
        producer.push(&[1.0]);
        let resumed = play(&mut state, &mut consumer, CALLBACK);
        assert_eq!(consumer.len(), prebuffer - CALLBACK);
        out.extend(resumed);
        assert!(max_step(&out) < 0.05, "step {}", max_step(&out));
        assert_eq!(*out.last().unwrap(), 1.0);
    }

    #[test]
    fn mute_ramps_to_silence_and_back_while_the_ring_drains() {
        let (mut producer, mut consumer, mut state) = setup();
        let mut gain = MuteGain::new(RATE, false);
        let target = frames_for_ms(RATE, TARGET_FILL_MS);
        producer.push(&vec![1.0; target + CALLBACK * 8]);
        let mut render = |muted: bool, consumer: &mut Consumer| {
            let mut out = vec![f32::NAN; CALLBACK];
            state.render(consumer, CALLBACK, |index, sample| {
                out[index] = gain.apply(muted, sample);
            });
            out
        };
        let mut out = Vec::new();
        out.extend(render(false, &mut consumer));
        let queued = consumer.len();
        let muted = render(true, &mut consumer);
        // Muting still consumes, so the pacer sees the same drain rate.
        assert_eq!(consumer.len(), queued - CALLBACK);
        out.extend(&muted);
        let silent = render(true, &mut consumer);
        assert!(silent.iter().all(|&sample| sample == 0.0));
        out.extend(silent);
        out.extend(render(false, &mut consumer));
        assert_eq!(*out.last().unwrap(), 1.0);
        assert!(max_step(&out) < 0.05, "step {}", max_step(&out));
    }

    #[test]
    fn high_fill_discards_backlog_back_to_target() {
        let (mut producer, mut consumer, mut state) = setup();
        let target = frames_for_ms(RATE, TARGET_FILL_MS);
        producer.push(&vec![0.5; target]);
        play(&mut state, &mut consumer, CALLBACK);
        producer.push(&vec![0.5; frames_for_ms(RATE, 300)]);
        assert!(consumer.len() > frames_for_ms(RATE, HIGH_FILL_MS));
        play(&mut state, &mut consumer, CALLBACK);
        assert_eq!(consumer.len(), target);
        // Hysteresis: until the fill falls below the low mark, a new
        // backlog is left to play.
        producer.push(&vec![0.5; frames_for_ms(RATE, 300)]);
        let queued = consumer.len();
        assert!(queued > frames_for_ms(RATE, HIGH_FILL_MS));
        play(&mut state, &mut consumer, CALLBACK);
        assert_eq!(consumer.len(), queued - CALLBACK);
    }
}
