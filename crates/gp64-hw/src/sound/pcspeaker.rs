//! PC Speaker emulation
//!
//! Supports two sound generation modes:
//! 1. PIT-based: Timer channel 2 generates frequency, bits 0+1 enable output
//! 2. Direct toggle: Program rapidly toggles bit 1 to create waveform

use gp64_types::FixedRing;

/// Absolute floor on retained PIT reprogrammings before PCM mode can engage.
///
/// This is a floor, not the criterion — see [`PCM_DETECTION_RATE_PER_MS`]. It
/// stops a very short render window from certifying a high rate off one or two
/// events.
const PCM_DETECTION_THRESHOLD: usize = 32;

/// Absolute floor on retained toggle events before direct mode can engage.
///
/// Must be high enough to avoid false positives from normal PIT note changes
/// (which do disable→enable = 2 events). This is a floor, not the criterion —
/// see [`DIRECT_TOGGLE_RATE_PER_MS`].
const DIRECT_TOGGLE_THRESHOLD: usize = 16;

/// PIT reprogrammings per millisecond of guest time that mean "the guest is
/// using divisor values as PCM sample amplitudes".
///
/// Classification must describe the GUEST, so it is a rate, not a per-call
/// count: the render window is caller-chosen and varies by an order of
/// magnitude between the two production mixers (a ~1 ms synchronous tick in
/// `Emulator::complete_synchronous_audio_tick`, a whole video frame in
/// `Emulator::generate_audio_frame_with_stems`). A per-call count let the
/// caller's chunking, rather than guest behaviour, pick the mode
/// (MDD-BUG-ANVIL-00111).
///
/// The value is the historical count read as a per-millisecond figure, which
/// keeps the 1 ms synchronous tick behaving as it did. Divisor-as-amplitude
/// players reprogram at their sample rate (~7 kHz upward, i.e. ≥7/ms and
/// typically far more), while changing musical notes reprograms a
/// handful of times per second, so the two populations sit decades apart and
/// the exact figure inside that gap is not critical.
const PCM_DETECTION_RATE_PER_MS: f32 = 32.0;

/// Port 61h toggles per millisecond of guest time that mean "the guest is
/// bit-banging a waveform" rather than gating a PIT square wave.
///
/// A rate for the same reason as [`PCM_DETECTION_RATE_PER_MS`], and likewise
/// the historical count read as a per-millisecond figure. Direct-mode speaker
/// PCM runs at ~7 kHz upward, so it clears this
/// comfortably; a beep that gates the speaker a few times a second does not
/// come close.
const DIRECT_TOGGLE_RATE_PER_MS: f32 = 16.0;

/// PIT base frequency in Hz (1.193182 MHz crystal / 1)
const PIT_FREQUENCY: f64 = 1_193_182.0;

/// A complete ordinary-PIT output state, independent of live I/O state.
#[derive(Clone, Copy, Default)]
struct PitSoundState {
    sounding: bool,
    divisor: u16,
    effective_divisor: u64,
}

/// PC Speaker emulation
#[derive(Clone)]
pub struct PcSpeaker {
    /// Speaker enabled (via port 61h bit 1)
    enabled: bool,
    /// Timer 2 gate (port 61h bit 0)
    timer_gate: bool,
    /// Raw PIT channel 2 reload encoding, retained for PCM and diagnostics.
    divisor: u16,
    /// Loaded count in binary ticks; None until the channel is programmed.
    effective_divisor: Option<u64>,
    /// Speaker output state
    output: bool,
    /// Phase counter for audio generation (0.0 to 1.0)
    phase: f64,
    /// Toggle events for direct speaker manipulation (cycle count, state)
    toggle_events: FixedRing<(u64, bool), 128>,
    /// Current CPU cycle count (updated by emulator)
    current_cycle: u64,
    /// Cycles per audio frame (for timing conversion)
    cycles_per_frame: u64,
    /// Master volume (0.0 to 1.0)
    volume: f32,
    /// PIT reprogramming events for PCM detection (cycle count, divisor)
    pit_pcm_events: FixedRing<(u64, u16), 128>,
    /// Last state consumed by the explicit-window direct-toggle renderer.
    /// This is separate from `enabled`, which can already reflect a write in a
    /// later guest-time window.
    direct_window_state: bool,
    /// Once direct-toggle PCM is detected, retain that mode while an event at
    /// a window end waits for the following half-open window.
    direct_window_mode_active: bool,
    /// Last amplitude consumed by the explicit-window PIT-PCM renderer.
    pcm_window_amplitude: f32,
    /// Equivalent carry state for rapid PIT-reprogramming PCM.
    pcm_window_mode_active: bool,
    /// PIT channel 2 output state (exposed on port 61h bit 4)
    timer2_output: bool,
    /// Sparse audible transitions, retained even after the guest gates off.
    pit_sound_events: FixedRing<(u64, PitSoundState), 128>,
    pit_window_state: PitSoundState,
    /// Pure mixer synchronization may seed a new speaker, never queued history.
    pit_history_seen: bool,
}

impl PcSpeaker {
    pub fn new() -> Self {
        Self {
            enabled: false,
            timer_gate: false,
            divisor: 0,
            effective_divisor: None,
            output: false,
            phase: 0.0,
            toggle_events: FixedRing::new(),
            current_cycle: 0,
            // Default: ~4.77 MHz CPU, 60 fps -> ~79500 cycles/frame
            cycles_per_frame: 79500,
            // Default volume: 30% (PC speaker is typically quiet compared to other audio)
            volume: 0.3,
            pit_pcm_events: FixedRing::new(),
            direct_window_state: false,
            direct_window_mode_active: false,
            pcm_window_amplitude: 0.0,
            pcm_window_mode_active: false,
            // PIT channel 2 OUT starts HIGH in the default mode (mode 2/3)
            timer2_output: true,
            pit_sound_events: FixedRing::new(),
            pit_window_state: PitSoundState::default(),
            pit_history_seen: false,
        }
    }

    /// Read port 61h (PPI Port B) — bits 0-1 plus bit 4 (Timer 2 output).
    /// IoBus merges bits 0-3 from here with upper bits from keyboard/PIT.
    pub fn read_control(&self) -> u8 {
        let mut value = 0u8;
        if self.timer_gate {
            value |= 0x01; // Bit 0: Timer 2 gate
        }
        if self.enabled {
            value |= 0x02; // Bit 1: Speaker enable
        }
        if self.timer2_output {
            value |= 0x10; // Bit 4: Timer 2 output
        }
        value
    }

    /// Write port 61h (PPI Port B)
    pub fn write_control(&mut self, value: u8) {
        let previous = self.pit_sound_state();
        let new_gate = (value & 0x01) != 0;
        let new_enabled = (value & 0x02) != 0;

        // Track toggle events for direct speaker manipulation
        if new_enabled != self.enabled {
            // Record the toggle event with current cycle
            if !self.toggle_events.is_full() {
                self.toggle_events
                    .push_back((self.current_cycle, new_enabled));
            }
        }

        self.timer_gate = new_gate;
        self.enabled = new_enabled;
        self.record_pit_sound_change(previous);
    }

    /// Update current CPU cycle count (call this regularly from emulator)
    pub fn set_cycle(&mut self, cycle: u64) {
        self.current_cycle = cycle;
    }

    /// Set cycles per frame for timing calculations
    pub fn set_cycles_per_frame(&mut self, cycles: u64) {
        self.cycles_per_frame = cycles;
    }

    /// Set speaker frequency from PIT channel 2 divisor.
    ///
    /// Decodes a binary reload for precise phase computation (avoids integer
    /// truncation of `1193182 / divisor`). Also records the raw value for PCM
    /// detection — rapid PIT channel 2 reprogramming uses the
    /// divisor values as PCM sample amplitudes.
    pub fn set_frequency(&mut self, divisor: u16) {
        self.update_pit_divisor(divisor);

        // Record for PCM detection
        if !self.pit_pcm_events.is_full() {
            self.pit_pcm_events.push_back((self.current_cycle, divisor));
        }
    }

    /// Sync the divisor from PIT channel 2 without recording a PCM event.
    /// Used by the per-tick audio mixer to keep the frequency current without
    /// polluting the PCM event stream.
    pub fn sync_divisor(&mut self, divisor: u16) {
        self.divisor = divisor;
        self.effective_divisor = Some(Self::binary_divisor(divisor));
        if !self.pit_history_seen {
            self.pit_window_state = self.pit_sound_state();
        }
    }

    /// A completed I/O reload updates ordinary PIT state even with the gate off,
    /// without admitting silent programming to the PCM classifier.
    pub fn update_pit_divisor(&mut self, divisor: u16) {
        self.update_pit_state(divisor, Some(Self::binary_divisor(divisor)));
    }

    /// Update from the PIT's loaded state and effective binary/BCD count.
    /// Raw reloads remain separate because PCM uses their encoded amplitudes.
    pub fn update_pit_state(&mut self, divisor: u16, effective_divisor: Option<u64>) {
        let previous = self.pit_sound_state();
        self.divisor = divisor;
        self.effective_divisor = effective_divisor;
        self.record_pit_sound_change(previous);
    }

    fn binary_divisor(divisor: u16) -> u64 {
        if divisor == 0 {
            65_536
        } else {
            u64::from(divisor)
        }
    }

    fn pit_sound_state(&self) -> PitSoundState {
        PitSoundState {
            sounding: self.is_active(),
            divisor: self.divisor,
            effective_divisor: self.effective_divisor.unwrap_or(0),
        }
    }

    fn record_pit_sound_change(&mut self, previous: PitSoundState) {
        let state = self.pit_sound_state();
        if previous.sounding == state.sounding
            && (!state.sounding
                || (previous.divisor == state.divisor
                    && previous.effective_divisor == state.effective_divisor))
        {
            return;
        }
        self.pit_history_seen = true;
        if self.pit_sound_events.is_full() {
            // Keep complete recent snapshots, especially the terminal gate-off.
            // Lost older snapshots are never applied ahead of their timestamp.
            let _ = self.pit_sound_events.pop_front();
        }
        self.pit_sound_events.push_back((self.current_cycle, state));
    }

    fn consume_pit_sound_events_before(&mut self, end_cycle: u64) {
        while matches!(self.pit_sound_events.front(), Some((cycle, _)) if *cycle < end_cycle) {
            self.pit_window_state = self
                .pit_sound_events
                .pop_front()
                .expect("retained PIT state")
                .1;
        }
    }

    fn reset_pit_sound_history(&mut self) {
        self.pit_sound_events.clear();
        self.pit_window_state = self.pit_sound_state();
        self.pit_history_seen = false;
    }

    /// Set PIT channel 2 output state (exposed as bit 4 of port 61h)
    pub fn set_timer2_output(&mut self, high: bool) {
        self.timer2_output = high;
    }

    /// Get PIT channel 2 output state
    pub fn timer2_output(&self) -> bool {
        self.timer2_output
    }

    /// Toggle output (called by timer)
    pub fn toggle(&mut self) {
        if self.enabled && self.timer_gate {
            self.output = !self.output;
        }
    }

    /// Check if speaker is producing sound (PIT mode)
    pub fn is_active(&self) -> bool {
        self.enabled && self.timer_gate && self.effective_divisor.is_some()
    }

    /// Get current frequency in Hz (integer approximation for diagnostics)
    pub fn get_frequency(&self) -> u32 {
        if let Some(divisor) = self.effective_divisor {
            ((PIT_FREQUENCY as u64) / divisor) as u32
        } else {
            0
        }
    }

    /// Get current PIT divisor
    pub fn get_divisor(&self) -> u16 {
        self.divisor
    }

    /// Set master volume (0.0 to 1.0)
    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
    }

    /// Get current volume
    pub fn get_volume(&self) -> f32 {
        self.volume
    }

    /// Check if there are pending toggle events
    pub fn has_toggle_events(&self) -> bool {
        !self.toggle_events.is_empty()
    }

    /// Check if PCM mode is detected (rapid PIT reprogramming)
    pub fn has_pcm_events(&self) -> bool {
        self.pit_pcm_events.len() >= PCM_DETECTION_THRESHOLD
    }

    /// Check if any queued PCM state needs the next render call to consume or clear it.
    pub fn has_pcm_maintenance(&self) -> bool {
        !self.pit_pcm_events.is_empty()
    }

    /// Check if the next audio render can produce or consume speaker output state.
    pub fn has_pending_output(&self) -> bool {
        self.is_active()
            || self.has_toggle_events()
            || self.has_pcm_maintenance()
            || !self.pit_sound_events.is_empty()
            || self.pit_window_state.sounding
    }

    /// Guest cycles per millisecond, derived from the render request itself.
    ///
    /// The call renders `count` samples at `sample_rate` — that is how much
    /// real time it covers — while spanning `window_cycles` of guest time. The
    /// ratio converts event timestamps into milliseconds, so the classifier
    /// needs no extra plumbing from the mixer. `None` when the request carries
    /// no usable scale.
    fn cycles_per_ms(window_cycles: u64, count: usize, sample_rate: u32) -> Option<f32> {
        if sample_rate == 0 || count == 0 || window_cycles == 0 {
            return None;
        }
        let window_ms = (count as f32 * 1000.0) / sample_rate as f32;
        if window_ms <= 0.0 {
            return None;
        }
        Some(window_cycles as f32 / window_ms)
    }

    /// Does the retained toggle stream represent a direct-mode bit-bang rate?
    fn direct_toggle_rate_met(&self, cycles_per_ms: Option<f32>) -> bool {
        Self::rate_met(
            self.toggle_events.front().map(|&(cycle, _)| cycle),
            self.toggle_events.back().map(|&(cycle, _)| cycle),
            self.toggle_events.len(),
            DIRECT_TOGGLE_THRESHOLD,
            DIRECT_TOGGLE_RATE_PER_MS,
            cycles_per_ms,
        )
    }

    /// Does the retained PIT reprogramming stream represent a PCM rate?
    fn pcm_rate_met(&self, cycles_per_ms: Option<f32>) -> bool {
        Self::rate_met(
            self.pit_pcm_events.front().map(|&(cycle, _)| cycle),
            self.pit_pcm_events.back().map(|&(cycle, _)| cycle),
            self.pit_pcm_events.len(),
            PCM_DETECTION_THRESHOLD,
            PCM_DETECTION_RATE_PER_MS,
            cycles_per_ms,
        )
    }

    /// Is this event stream dense enough, in guest time, to be sample-rate
    /// audio rather than ordinary device programming?
    ///
    /// The denominator is the span of the EVENTS, not of the render call.
    /// Speaker PCM is inherently bursty — a sound effect occupies a couple of
    /// milliseconds inside a 16 ms video frame — so dividing by the call's
    /// window would average a genuine burst down to nothing and misclassify it
    /// as PIT programming. Event timestamps are also independent of how the
    /// caller chunks its renders, which is the property this whole change
    /// exists to establish (MDD-BUG-ANVIL-00111).
    ///
    /// Taking the span from the retained events also makes the fixed-capacity
    /// rings harmless: dropping events off the tail leaves the surviving
    /// prefix's rate unchanged, so a long render can still reach the rate.
    fn rate_met(
        first_cycle: Option<u64>,
        last_cycle: Option<u64>,
        len: usize,
        count_floor: usize,
        rate_per_ms: f32,
        cycles_per_ms: Option<f32>,
    ) -> bool {
        if len < count_floor {
            return false;
        }
        let (Some(first), Some(last), Some(cycles_per_ms)) =
            (first_cycle, last_cycle, cycles_per_ms)
        else {
            // No usable scale: fall back to the count floor already met above.
            return true;
        };
        let span_cycles = last.saturating_sub(first);
        if span_cycles == 0 || cycles_per_ms <= 0.0 {
            // Every event at one instant — an unbounded rate.
            return true;
        }
        let span_ms = span_cycles as f32 / cycles_per_ms;
        // `len - 1` intervals span `len` events; `len` is at least the floor.
        (len - 1) as f32 >= rate_per_ms * span_ms
    }

    /// Clear PIT PCM events (call at end of frame)
    pub fn clear_pcm_events(&mut self) {
        self.pit_pcm_events.clear();
        self.pcm_window_amplitude = 0.0;
        self.pcm_window_mode_active = false;
    }

    /// Generate audio samples
    pub fn generate_samples(&mut self, count: usize, sample_rate: u32, output: &mut [f32]) {
        // This compatibility API renders live state; never replay its consumed
        // interval later through the explicit-window API.
        self.reset_pit_sound_history();
        let volume = self.volume;
        let speaker_gated = self.enabled && self.timer_gate;
        let cycles_per_ms = Self::cycles_per_ms(self.cycles_per_frame, count, sample_rate);

        // Check for PCM mode first (rapid PIT reprogramming).
        // PCM events are only recorded when the speaker is active, but discard
        // any stale events if the speaker has since been turned off.
        if self.pcm_rate_met(cycles_per_ms) {
            if speaker_gated {
                self.generate_pcm_samples(count, sample_rate, output, volume);
            } else {
                self.pit_pcm_events.clear();
            }
            return;
        }
        // Clear insufficient PCM events
        self.pit_pcm_events.clear();

        // Check if we have enough direct toggle events to indicate true
        // bitbang audio (programs rapidly toggling port 61h bit 1).
        // Normal PIT note changes (disable → reprogram → enable) only produce
        // 2-3 events per change, which must NOT override PIT square wave mode.
        // Direct toggle mode is exempt from the gate check — the toggle events
        // themselves encode the speaker on/off state.
        if self.direct_toggle_rate_met(cycles_per_ms) {
            self.generate_direct_samples(count, sample_rate, output, volume);
            return;
        }

        // Not enough toggles for direct mode — clear them and use PIT mode.
        self.toggle_events.clear();

        // PIT-based mode: generate square wave at configured frequency.
        // Compute phase increment directly from the divisor in f64 to avoid
        // the precision loss of integer Hz truncation (1193182 / divisor).
        let Some(divisor) = self.effective_divisor.filter(|_| speaker_gated) else {
            return;
        };

        let phase_inc = PIT_FREQUENCY / (divisor as f64 * sample_rate as f64);

        for i in 0..count {
            self.phase += phase_inc;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }

            let sample = if self.phase < 0.5 { volume } else { -volume };

            // Add to mix (assuming stereo)
            output[i * 2] += sample;
            output[i * 2 + 1] += sample;
        }
    }

    /// Generate samples for an exact half-open guest-cycle window.
    ///
    /// Events at `start_cycle` affect the first sample. Events at `end_cycle`
    /// are retained for the next window, so no renderer has to infer a start
    /// from the last CPU cycle or from the first event it happened to see.
    pub fn generate_samples_in_window(
        &mut self,
        start_cycle: u64,
        end_cycle: u64,
        count: usize,
        sample_rate: u32,
        output: &mut [f32],
    ) {
        assert!(
            start_cycle <= end_cycle,
            "PC speaker render window must be half-open [start, end)"
        );
        if count == 0 || start_cycle == end_cycle {
            return;
        }

        self.render_samples_in_window(start_cycle, end_cycle, count, sample_rate, output);
        // Consume tail events even when PCM/direct rendering took precedence,
        // including writes after the final sample but before the window end.
        self.consume_pit_sound_events_before(end_cycle);
    }

    fn render_samples_in_window(
        &mut self,
        start_cycle: u64,
        end_cycle: u64,
        count: usize,
        sample_rate: u32,
        output: &mut [f32],
    ) {
        let volume = self.volume;
        let speaker_gated = self.enabled && self.timer_gate;
        let cycles_per_ms =
            Self::cycles_per_ms(end_cycle.saturating_sub(start_cycle), count, sample_rate);

        if self.pcm_rate_met(cycles_per_ms) {
            self.pcm_window_mode_active = true;
        }
        if self.pcm_window_mode_active && !self.pit_pcm_events.is_empty() {
            if speaker_gated {
                self.generate_pcm_samples_in_window(
                    start_cycle,
                    end_cycle,
                    count,
                    sample_rate,
                    output,
                    volume,
                );
            } else {
                self.pit_pcm_events.clear();
                self.pcm_window_mode_active = false;
                self.pcm_window_amplitude = 0.0;
            }
            return;
        }
        self.discard_pcm_events_before(end_cycle);

        if self.direct_toggle_rate_met(cycles_per_ms) {
            self.direct_window_mode_active = true;
        }
        if self.direct_window_mode_active && !self.toggle_events.is_empty() {
            self.generate_direct_samples_in_window(
                start_cycle,
                end_cycle,
                count,
                sample_rate,
                output,
                volume,
            );
            return;
        }
        self.discard_toggle_events_before(end_cycle);

        let phase_increment = |state: PitSoundState| {
            if state.sounding {
                PIT_FREQUENCY / (state.effective_divisor as f64 * sample_rate as f64)
            } else {
                0.0
            }
        };
        let mut phase_inc = phase_increment(self.pit_window_state);
        for i in 0..count {
            let cycle = Self::sample_cycle_in_window(start_cycle, end_cycle, i, count);
            while matches!(self.pit_sound_events.front(), Some((at, _)) if *at <= cycle) {
                self.pit_window_state = self
                    .pit_sound_events
                    .pop_front()
                    .expect("retained PIT state")
                    .1;
                phase_inc = phase_increment(self.pit_window_state);
            }
            if !self.pit_window_state.sounding {
                continue;
            }
            self.phase += phase_inc;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
            let sample = if self.phase < 0.5 { volume } else { -volume };
            output[i * 2] += sample;
            output[i * 2 + 1] += sample;
        }
    }

    /// Generate samples from rapid PIT reprogramming (PCM mode).
    ///
    /// When PIT channel 2 is rapidly reprogrammed with varying divisor values,
    /// they are effectively using the divisor as a PCM sample amplitude.
    /// The divisor value represents a counter reload — lower values mean
    /// higher frequency, but for PCM the value itself encodes amplitude.
    /// We normalize divisor values to [-1.0, 1.0] and resample to output rate.
    fn generate_pcm_samples(
        &mut self,
        count: usize,
        _sample_rate: u32,
        output: &mut [f32],
        volume: f32,
    ) {
        let events: Vec<(u64, u16)> = self.pit_pcm_events.drain().collect();

        if events.is_empty() {
            return;
        }

        let cycles_per_sample = self.cycles_per_frame as f32 / count as f32;

        // Estimate the frame start cycle
        let frame_start_cycle = if let Some(&(first_cycle, _)) = events.first() {
            first_cycle
        } else {
            self.current_cycle.saturating_sub(self.cycles_per_frame)
        };

        let mut event_idx = 0;
        // Track current PCM amplitude from divisor values.
        // Typical PCM range: divisors ~1-255 for 8-bit audio.
        // Normalize to [-1.0, 1.0]: center at 128, range 0-255.
        let mut current_amplitude: f32 = 0.0;

        for i in 0..count {
            let sample_cycle = frame_start_cycle + (i as f32 * cycles_per_sample) as u64;

            // Process any events up to this sample time
            while event_idx < events.len() && events[event_idx].0 <= sample_cycle {
                let divisor = events[event_idx].1;
                // Clamp divisor to 8-bit PCM range and normalize to [-1.0, 1.0]
                let clamped = (divisor as f32).min(255.0);
                current_amplitude = (clamped - 128.0) / 128.0;
                event_idx += 1;
            }

            let sample = current_amplitude * volume;
            output[i * 2] += sample;
            output[i * 2 + 1] += sample;
        }
    }

    /// Generate samples from direct toggle events
    fn generate_direct_samples(
        &mut self,
        count: usize,
        _sample_rate: u32,
        output: &mut [f32],
        volume: f32,
    ) {
        // The render window STARTS at the first event and runs forward for one
        // frame. It previously started one whole frame EARLIER
        // (`first_cycle.saturating_sub(self.cycles_per_frame)`), so the window
        // ended exactly at `first_cycle`: every sample cycle was strictly less
        // than the first event, the `<=` guard below never fired, `speaker_on`
        // kept its `false` initialiser for the whole buffer, and direct mode
        // rendered a constant -volume — silence, on every frame
        // (MDD-BUG-FLUX-00134). The bug was not a misplaced waveform; no toggle
        // event was ever applied at all.
        let frame_start_cycle = if let Some(&(first_cycle, _)) = self.toggle_events.front() {
            first_cycle
        } else {
            self.current_cycle.saturating_sub(self.cycles_per_frame)
        };
        let frame_end_cycle = frame_start_cycle.saturating_add(self.cycles_per_frame);

        // Track current speaker state
        let mut speaker_on = false;
        let mut event_idx = 0;
        let events: Vec<(u64, bool)> = self.toggle_events.drain().collect();

        for i in 0..count {
            // Exact integer placement, shared with the windowed renderer. The
            // old `frame_start + (i as f32 * cycles_per_sample) as u64` lost
            // precision in f32 and could land an event on a different sample
            // than the windowed path does for the identical window.
            let sample_cycle =
                Self::sample_cycle_in_window(frame_start_cycle, frame_end_cycle, i, count);

            // Process any events that occurred before this sample
            while event_idx < events.len() && events[event_idx].0 <= sample_cycle {
                speaker_on = events[event_idx].1;
                event_idx += 1;
            }

            // Generate sample based on current speaker state
            let sample = if speaker_on { volume } else { -volume };

            output[i * 2] += sample;
            output[i * 2 + 1] += sample;
        }

        // Keep final state for continuity
        if let Some(&(_, state)) = events.last() {
            self.enabled = state;
        }
    }

    fn generate_pcm_samples_in_window(
        &mut self,
        start_cycle: u64,
        end_cycle: u64,
        count: usize,
        _sample_rate: u32,
        output: &mut [f32],
        volume: f32,
    ) {
        let mut events = Vec::new();
        while let Some(&(cycle, _)) = self.pit_pcm_events.front() {
            if cycle >= end_cycle {
                break;
            }
            events.push(
                self.pit_pcm_events
                    .pop_front()
                    .expect("front event must be removable"),
            );
        }

        let mut event_idx = 0;
        while event_idx < events.len() && events[event_idx].0 < start_cycle {
            self.pcm_window_amplitude = Self::pcm_amplitude(events[event_idx].1);
            event_idx += 1;
        }

        for i in 0..count {
            let sample_cycle = Self::sample_cycle_in_window(start_cycle, end_cycle, i, count);
            while event_idx < events.len() && events[event_idx].0 <= sample_cycle {
                self.pcm_window_amplitude = Self::pcm_amplitude(events[event_idx].1);
                event_idx += 1;
            }
            let sample = self.pcm_window_amplitude * volume;
            output[i * 2] += sample;
            output[i * 2 + 1] += sample;
        }

        if self.pit_pcm_events.is_empty() {
            self.pcm_window_mode_active = false;
        }
    }

    fn generate_direct_samples_in_window(
        &mut self,
        start_cycle: u64,
        end_cycle: u64,
        count: usize,
        _sample_rate: u32,
        output: &mut [f32],
        volume: f32,
    ) {
        let mut events = Vec::new();
        while let Some(&(cycle, _)) = self.toggle_events.front() {
            if cycle >= end_cycle {
                break;
            }
            events.push(
                self.toggle_events
                    .pop_front()
                    .expect("front event must be removable"),
            );
        }

        let mut event_idx = 0;
        while event_idx < events.len() && events[event_idx].0 < start_cycle {
            self.direct_window_state = events[event_idx].1;
            event_idx += 1;
        }

        for i in 0..count {
            let sample_cycle = Self::sample_cycle_in_window(start_cycle, end_cycle, i, count);
            while event_idx < events.len() && events[event_idx].0 <= sample_cycle {
                self.direct_window_state = events[event_idx].1;
                event_idx += 1;
            }
            let sample = if self.direct_window_state {
                volume
            } else {
                -volume
            };
            output[i * 2] += sample;
            output[i * 2 + 1] += sample;
        }

        if self.toggle_events.is_empty() {
            self.direct_window_mode_active = false;
        }
    }

    fn sample_cycle_in_window(start_cycle: u64, end_cycle: u64, index: usize, count: usize) -> u64 {
        let offset = (index as u128 * (end_cycle - start_cycle) as u128) / count as u128;
        start_cycle + offset as u64
    }

    fn pcm_amplitude(divisor: u16) -> f32 {
        ((divisor as f32).min(255.0) - 128.0) / 128.0
    }

    fn discard_pcm_events_before(&mut self, end_cycle: u64) {
        while matches!(self.pit_pcm_events.front(), Some((cycle, _)) if *cycle < end_cycle) {
            let _ = self.pit_pcm_events.pop_front();
        }
    }

    fn discard_toggle_events_before(&mut self, end_cycle: u64) {
        while matches!(self.toggle_events.front(), Some((cycle, _)) if *cycle < end_cycle) {
            let _ = self.toggle_events.pop_front();
        }
    }

    /// Clear toggle events (call at end of frame)
    pub fn clear_toggle_events(&mut self) {
        self.reset_pit_sound_history();
        self.toggle_events.clear();
        self.pit_pcm_events.clear();
        self.direct_window_state = false;
        self.direct_window_mode_active = false;
        self.pcm_window_amplitude = 0.0;
        self.pcm_window_mode_active = false;
    }
}

impl Default for PcSpeaker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// Capacity of the toggle event ring buffer (must match FixedRing<_, N>)
    const TOGGLE_EVENT_CAPACITY: usize = 128;

    #[test]
    fn test_new() {
        let speaker = PcSpeaker::new();
        assert!(!speaker.enabled);
        assert!(!speaker.timer_gate);
        assert_eq!(speaker.divisor, 0);
        assert!(!speaker.output);
        assert_eq!(speaker.phase, 0.0);
    }

    #[test]
    fn test_default() {
        let speaker = PcSpeaker::default();
        assert!(!speaker.enabled);
    }

    #[test]
    fn test_write_control_enable() {
        let mut speaker = PcSpeaker::new();

        // Enable speaker only (bit 1)
        speaker.write_control(0x02);
        assert!(speaker.enabled);
        assert!(!speaker.timer_gate);

        // Enable timer gate only (bit 0)
        speaker.write_control(0x01);
        assert!(!speaker.enabled);
        assert!(speaker.timer_gate);

        // Enable both
        speaker.write_control(0x03);
        assert!(speaker.enabled);
        assert!(speaker.timer_gate);
    }

    #[test]
    fn test_write_control_disable() {
        let mut speaker = PcSpeaker::new();

        speaker.write_control(0x03);
        assert!(speaker.enabled);
        assert!(speaker.timer_gate);

        speaker.write_control(0x00);
        assert!(!speaker.enabled);
        assert!(!speaker.timer_gate);
    }

    #[test]
    fn test_read_control() {
        let mut speaker = PcSpeaker::new();

        // Nothing enabled
        let val = speaker.read_control();
        assert_eq!(val & 0x03, 0x00);
        assert_eq!(val & 0x10, 0x10); // Timer 2 output high

        // Enable speaker
        speaker.write_control(0x02);
        let val = speaker.read_control();
        assert_eq!(val & 0x02, 0x02);

        // Enable timer gate
        speaker.write_control(0x03);
        let val = speaker.read_control();
        assert_eq!(val & 0x03, 0x03);
    }

    #[test]
    fn test_set_frequency() {
        let mut speaker = PcSpeaker::new();

        // Set to 1000 Hz approx (divisor ~1193)
        speaker.set_frequency(1193);
        assert_eq!(speaker.divisor, 1193);
        assert_eq!(speaker.get_frequency(), 1193182 / 1193);

        // Set to 440 Hz (A4) approx (divisor ~2711)
        speaker.set_frequency(2711);
        assert_eq!(speaker.divisor, 2711);
        assert_eq!(speaker.get_frequency(), 1193182 / 2711);
    }

    #[test]
    fn test_set_frequency_zero() {
        let mut speaker = PcSpeaker::new();

        speaker.set_frequency(1193);
        assert!(speaker.divisor > 0);

        // Zero divisor should set divisor to 0
        speaker.set_frequency(0);
        assert_eq!(speaker.divisor, 0);
    }

    #[test]
    fn test_get_frequency() {
        let mut speaker = PcSpeaker::new();

        assert_eq!(speaker.get_frequency(), 0);

        speaker.set_frequency(1000);
        assert_eq!(speaker.get_frequency(), 1193182 / 1000);
    }

    #[test]
    fn test_sync_divisor_no_pcm_event() {
        let mut speaker = PcSpeaker::new();

        // sync_divisor should update divisor without recording PCM events
        speaker.sync_divisor(1000);
        assert_eq!(speaker.divisor, 1000);
        assert!(speaker.pit_pcm_events.is_empty());

        // set_frequency SHOULD record PCM events
        speaker.set_frequency(2000);
        assert_eq!(speaker.divisor, 2000);
        assert_eq!(speaker.pit_pcm_events.len(), 1);
    }

    #[test]
    fn programmed_zero_reload_renders_live_and_window_square_wave() {
        for synchronize in [false, true] {
            let mut speaker = PcSpeaker::new();
            speaker.write_control(3);
            assert!(!speaker.is_active(), "unprogrammed speaker remains silent");
            if synchronize {
                speaker.sync_divisor(0);
            } else {
                speaker.set_frequency(0);
            }
            let mut live = vec![0.0; 8_820];
            let mut window = live.clone();
            let mut historical = speaker.clone();
            speaker.generate_samples(4_410, 44_100, &mut live);
            historical.generate_samples_in_window(0, 100_000, 4_410, 44_100, &mut window);
            for samples in [&live, &window] {
                assert!(
                    samples.iter().any(|sample| *sample > 0.0),
                    "zero reload must render positive polarity"
                );
                assert!(
                    samples.iter().any(|sample| *sample < 0.0),
                    "zero reload must render negative polarity"
                );
            }
            assert_eq!(speaker.get_divisor(), 0);
            assert_eq!(speaker.get_frequency(), 18);
        }
    }

    #[test]
    fn test_is_active() {
        let mut speaker = PcSpeaker::new();

        // Initially not active
        assert!(!speaker.is_active());

        // Set frequency but nothing enabled
        speaker.set_frequency(1193);
        assert!(!speaker.is_active());

        // Enable speaker only
        speaker.write_control(0x02);
        assert!(!speaker.is_active());

        // Enable timer gate (both now enabled)
        speaker.write_control(0x03);
        assert!(speaker.is_active());

        // A completed binary zero reload means 65,536 ticks.
        speaker.set_frequency(0);
        assert!(speaker.is_active());
    }

    #[test]
    fn test_toggle_when_disabled() {
        let mut speaker = PcSpeaker::new();

        assert!(!speaker.output);
        speaker.toggle();
        assert!(!speaker.output); // Should not toggle when disabled
    }

    #[test]
    fn test_toggle_when_timer_gate_disabled() {
        let mut speaker = PcSpeaker::new();

        speaker.write_control(0x02); // Enable speaker only
        assert!(!speaker.output);
        speaker.toggle();
        assert!(!speaker.output);
    }

    #[test]
    fn test_toggle_when_enabled() {
        let mut speaker = PcSpeaker::new();

        speaker.write_control(0x03); // Enable both
        speaker.set_frequency(1000);

        assert!(!speaker.output);
        speaker.toggle();
        assert!(speaker.output);
        speaker.toggle();
        assert!(!speaker.output);
    }

    #[test]
    fn test_generate_samples_disabled() {
        let mut speaker = PcSpeaker::new();
        let mut output = [0.0f32; 200];

        // Should not generate samples when disabled
        speaker.generate_samples(100, 44100, &mut output);

        // Output should remain unchanged (all zeros)
        for sample in output.iter() {
            assert_eq!(*sample, 0.0);
        }
    }

    #[test]
    fn test_generate_samples_no_frequency() {
        let mut speaker = PcSpeaker::new();
        speaker.write_control(0x03);
        // frequency is 0

        let mut output = [0.0f32; 200];
        speaker.generate_samples(100, 44100, &mut output);

        // Output should remain unchanged
        for sample in output.iter() {
            assert_eq!(*sample, 0.0);
        }
    }

    #[test]
    fn test_generate_samples_gate_disabled() {
        let mut speaker = PcSpeaker::new();
        speaker.write_control(0x02); // Enable speaker only
        speaker.set_frequency(1000);

        let mut output = [0.0f32; 200];
        speaker.generate_samples(100, 44100, &mut output);

        for sample in output.iter() {
            assert_eq!(*sample, 0.0);
        }
    }

    #[test]
    fn test_generate_samples_active() {
        let mut speaker = PcSpeaker::new();
        speaker.write_control(0x03);
        speaker.set_frequency(1000); // ~1 kHz

        let mut output = [0.0f32; 200];
        speaker.generate_samples(100, 44100, &mut output);

        // Should have generated some non-zero samples
        let has_positive = output.iter().any(|&s| s > 0.0);
        let has_negative = output.iter().any(|&s| s < 0.0);

        assert!(has_positive, "Expected positive samples");
        assert!(has_negative, "Expected negative samples");
    }

    #[test]
    fn test_generate_samples_square_wave() {
        let mut speaker = PcSpeaker::new();
        speaker.write_control(0x03);
        // Use divisor 270 to get ~4400 Hz (1193182 / 270 = 4418 Hz)
        // At 44100 Hz sample rate, that's about 10 samples per cycle
        speaker.set_frequency(270);

        let mut output = [0.0f32; 200];
        speaker.generate_samples(100, 44100, &mut output);

        // Should produce a square wave pattern
        let volume = speaker.get_volume(); // Use actual volume setting

        // Check that we have both positive and negative samples
        let positives: Vec<f32> = output.iter().cloned().filter(|&s| s > 0.0).collect();
        let negatives: Vec<f32> = output.iter().cloned().filter(|&s| s < 0.0).collect();

        assert!(!positives.is_empty(), "Expected positive samples");
        assert!(!negatives.is_empty(), "Expected negative samples");

        // All positive samples should be approximately +volume
        for &p in &positives {
            assert!(
                (p - volume).abs() < 0.01,
                "Positive sample {p} not ~{volume}"
            );
        }

        // All negative samples should be approximately -volume
        for &n in &negatives {
            assert!(
                (n + volume).abs() < 0.01,
                "Negative sample {} not ~{}",
                n,
                -volume
            );
        }
    }

    #[test]
    fn test_generate_samples_stereo() {
        let mut speaker = PcSpeaker::new();
        speaker.write_control(0x03);
        speaker.set_frequency(1000);

        let mut output = [0.0f32; 200];
        speaker.generate_samples(100, 44100, &mut output);

        // Left and right channels should be identical (mono to stereo)
        for i in 0..100 {
            assert_eq!(output[i * 2], output[i * 2 + 1]);
        }
    }

    #[test]
    fn test_phase_wrapping() {
        let mut speaker = PcSpeaker::new();
        speaker.write_control(0x03);
        speaker.set_frequency(44100); // 1 sample per cycle

        let mut output = [0.0f32; 2000];
        speaker.generate_samples(1000, 44100, &mut output);

        // Phase should stay in [0, 1)
        let in_range = (speaker.phase >= 0.0) & (speaker.phase < 1.0);
        assert!(in_range);
    }

    #[test]
    fn test_direct_toggle_records_events() {
        let mut speaker = PcSpeaker::new();
        speaker.set_cycle(1000);

        // Toggle speaker on
        speaker.write_control(0x02);
        assert!(speaker.has_toggle_events());

        // Toggle speaker off
        speaker.set_cycle(2000);
        speaker.write_control(0x00);

        // Should have 2 events
        assert_eq!(speaker.toggle_events.len(), 2);
    }

    #[test]
    fn test_toggle_event_buffer_full_drops_events() {
        let mut speaker = PcSpeaker::new();

        for i in 0..TOGGLE_EVENT_CAPACITY {
            let value = if i % 2 == 0 { 0x02 } else { 0x00 };
            speaker.write_control(value);
        }

        assert_eq!(speaker.toggle_events.len(), TOGGLE_EVENT_CAPACITY);

        // One more toggle should not exceed capacity.
        speaker.write_control(0x02);
        assert_eq!(speaker.toggle_events.len(), TOGGLE_EVENT_CAPACITY);
    }

    #[test]
    fn test_generate_direct_samples_with_empty_events() {
        let mut speaker = PcSpeaker::new();
        speaker.set_cycles_per_frame(1000);
        speaker.set_cycle(5000);

        let volume = speaker.get_volume();
        let mut output = [0.0f32; 8];
        speaker.generate_direct_samples(4, 44100, &mut output, volume);

        assert!(output.iter().all(|&s| s <= 0.0));
        assert!(!speaker.enabled);
    }

    #[test]
    fn test_direct_toggle_generates_samples() {
        let mut speaker = PcSpeaker::new();
        speaker.set_cycles_per_frame(10000);

        // Simulate a genuine direct-toggle RATE. The 100-sample render below
        // covers ~2.27 ms, so 64 toggles is ~28/ms — well clear of
        // DIRECT_TOGGLE_RATE_PER_MS. (Before MDD-BUG-ANVIL-00111 the classifier
        // was a per-call count, so any 16 events would do regardless of how much
        // guest time the call covered.)
        for i in 0..64 {
            speaker.set_cycle(i * 156);
            speaker.write_control(if i % 2 == 0 { 0x02 } else { 0x00 });
        }

        let mut output = [0.0f32; 200];
        speaker.generate_samples(100, 44100, &mut output);

        // Direct mode fills every slot with +volume or -volume, so
        // `has_positive | has_negative` was a tautology: it held for a buffer
        // that was uniformly -volume, which is exactly what MDD-BUG-FLUX-00134
        // produced. Both polarities must be present for the toggles to have
        // been rendered at all.
        let has_positive = output.iter().any(|&s| s > 0.0);
        let has_negative = output.iter().any(|&s| s < 0.0);
        assert!(
            has_positive && has_negative,
            "direct toggle mode must render a toggling waveform, not a constant level"
        );
    }

    /// MDD-BUG-FLUX-00134 — direct mode rendered silence on every frame.
    ///
    /// `generate_direct_samples` placed its window origin one whole frame
    /// BEFORE the first event, so the window ended exactly at that event. Every
    /// sample cycle was strictly below it, no toggle was ever applied, and the
    /// buffer came out at a constant -volume.
    #[test]
    fn direct_mode_renders_the_toggles_instead_of_a_constant_level() {
        let mut speaker = PcSpeaker::new();
        speaker.set_cycles_per_frame(10_000);

        // The first event must sit at least one frame into the run, or
        // `saturating_sub` clamps the bad origin to 0 and accidentally lands on
        // the correct window — which is precisely why the pre-existing
        // `test_direct_toggle_generates_samples` (first event at cycle 0) never
        // caught this. Start well past one frame so the subtraction really bites.
        const FIRST: u64 = 50_000;

        // 64 alternating toggles — every write is a state change, so all 64
        // record. Over the ~2.27 ms this render covers that is ~28 toggles/ms,
        // comfortably over DIRECT_TOGGLE_RATE_PER_MS.
        for i in 0..64u64 {
            speaker.set_cycle(FIRST + i * 156);
            speaker.write_control(if i % 2 == 0 { 0x02 } else { 0x00 });
        }

        let mut output = [0.0f32; 200];
        speaker.generate_samples(100, 44100, &mut output);

        let positives = output.iter().filter(|&&s| s > 0.0).count();
        let negatives = output.iter().filter(|&&s| s < 0.0).count();
        assert!(
            positives > 0,
            "no sample carries the speaker-ON state: every toggle was dropped, \
             so the frame is silent (positives={positives}, negatives={negatives})"
        );
        assert!(
            negatives > 0,
            "no sample carries the speaker-OFF state (positives={positives}, \
             negatives={negatives})"
        );
    }

    /// The legacy inferred-window renderer must agree, sample for sample, with
    /// the explicit-window renderer given the equivalent window.
    ///
    /// This is the oracle MDD-BUG-FLUX-00134 asked for. "No longer uniformly
    /// silent" is far too weak a bar — it would accept a window placed at the
    /// wrong origin, or events landing on the wrong samples. Instead the fix is
    /// pinned against a second, independent implementation of the same
    /// semantics that already existed and was already correct
    /// (`generate_direct_samples_in_window`, exercised by
    /// `explicit_window_keeps_end_event_for_next_direct_render`).
    #[test]
    fn direct_mode_matches_the_explicit_window_renderer_sample_for_sample() {
        const FRAME: u64 = 10_000;
        const FIRST: u64 = 4_242; // deliberately not zero, to catch origin errors
        const COUNT: usize = 64;

        // Identical event streams into two speakers.
        let build = || {
            let mut speaker = PcSpeaker::new();
            speaker.set_cycles_per_frame(FRAME);
            // Alternating, so every write is a state CHANGE and records an
            // event: 32 events over the ~1.45 ms this render covers, i.e.
            // ~22 toggles/ms, comfortably over DIRECT_TOGGLE_RATE_PER_MS.
            // (A pattern that repeats a level records nothing for the repeat
            // and can silently drop below the rate, taking the PIT path
            // instead and rendering an all-zero buffer.)
            for i in 0..32u64 {
                speaker.set_cycle(FIRST + i * 137);
                speaker.write_control(if i % 2 == 0 { 0x02 } else { 0x00 });
            }
            speaker
        };

        let mut legacy = build();
        let mut windowed = build();

        let mut legacy_out = [0.0f32; COUNT * 2];
        legacy.generate_samples(COUNT, 44100, &mut legacy_out);

        // The window the legacy path infers: one frame forward from the first
        // event.
        let mut windowed_out = [0.0f32; COUNT * 2];
        windowed.generate_samples_in_window(FIRST, FIRST + FRAME, COUNT, 44100, &mut windowed_out);

        assert_eq!(
            legacy_out, windowed_out,
            "the inferred-window renderer disagrees with the explicit-window \
             renderer for the same events over the same window"
        );

        // And the agreed-on output is a real waveform, so the comparison is not
        // two implementations agreeing on silence.
        assert!(legacy_out.iter().any(|&s| s > 0.0), "no ON samples");
        assert!(legacy_out.iter().any(|&s| s < 0.0), "no OFF samples");
    }

    #[test]
    fn normal_pit_gate_only_boundaries_and_zero_windows() {
        let mut speaker = PcSpeaker::new();
        speaker.update_pit_divisor(1594);
        speaker.write_control(0x02); // Enable without the timer gate.
        speaker.set_cycle(100);
        speaker.write_control(0x03); // Gate-only change must queue output.
        speaker.set_cycle(200);
        speaker.write_control(0x02);
        assert!(
            speaker.has_pending_output(),
            "completed gate-only pulse is pending"
        );
        let mut unchanged = [0.0; 8];
        speaker.generate_samples_in_window(100, 100, 4, 10_000, &mut unchanged);
        speaker.generate_samples_in_window(100, 200, 0, 10_000, &mut unchanged);
        assert_eq!(
            speaker.pit_sound_events.len(),
            2,
            "zero windows retain history"
        );
        let mut before = [0.0; 8];
        speaker.generate_samples_in_window(0, 100, 4, 10_000, &mut before);
        assert_eq!(before, [0.0; 8], "end event does not affect this window");
        let mut during = [0.0; 8];
        speaker.generate_samples_in_window(100, 200, 4, 10_000, &mut during);
        assert!(during.iter().all(|sample| sample.abs() == speaker.volume));
        let mut after = [0.0; 8];
        speaker.generate_samples_in_window(200, 300, 4, 10_000, &mut after);
        assert_eq!(after, [0.0; 8]);
        assert!(!speaker.has_pending_output());
    }

    #[test]
    fn normal_pit_off_grid_split_preserves_phase_and_tail_off() {
        let build = || {
            let mut speaker = PcSpeaker::new();
            speaker.update_pit_divisor(1594);
            speaker.set_cycle(51);
            speaker.write_control(0x03);
            speaker.set_cycle(251);
            speaker.update_pit_divisor(2390);
            speaker.set_cycle(499); // After the final sample, before window end.
            speaker.write_control(0x00);
            speaker
        };
        let mut whole = build();
        let mut split = build();
        let mut whole_out = [0.0; 20];
        let mut split_out = [0.0; 20];
        whole.generate_samples_in_window(0, 500, 10, 10_000, &mut whole_out);
        split.generate_samples_in_window(0, 250, 5, 10_000, &mut split_out[..10]);
        split.generate_samples_in_window(250, 500, 5, 10_000, &mut split_out[10..]);
        assert!(whole_out[4..].iter().any(|sample| *sample != 0.0));
        assert_eq!(
            whole_out[..4],
            [0.0; 4],
            "off-grid on starts at the next sample"
        );
        assert_eq!(
            whole_out, split_out,
            "chunk boundaries preserve sample phase"
        );
        assert_eq!(whole.phase, split.phase);
        assert!(!whole.pit_window_state.sounding, "tail off updates carry");
        let mut after = [0.0; 4];
        split.generate_samples_in_window(500, 600, 2, 10_000, &mut after);
        assert_eq!(after, [0.0; 4]);
    }

    #[test]
    fn normal_pit_overflow_keeps_terminal_off_without_backdating() {
        let mut speaker = PcSpeaker::new();
        speaker.update_pit_divisor(1594);
        speaker.write_control(0x02);
        for i in 0..140 {
            speaker.set_cycle(100 + i * 10);
            speaker.write_control(if i % 2 == 0 { 0x03 } else { 0x02 });
        }
        assert_eq!(speaker.pit_sound_events.len(), 128);
        let mut early = [0.0; 20];
        speaker.generate_samples_in_window(0, 200, 10, 10_000, &mut early);
        assert_eq!(
            early, [0.0; 20],
            "evicted history is not a retroactive baseline"
        );
        let mut retained = [0.0; 280];
        speaker.generate_samples_in_window(200, 1_600, 140, 10_000, &mut retained);
        assert!(retained.iter().any(|sample| *sample != 0.0));
        assert!(
            !speaker.pit_window_state.sounding,
            "newest gate off survives overflow"
        );
        let mut after = [0.0; 20];
        speaker.generate_samples_in_window(1_600, 1_700, 10, 10_000, &mut after);
        assert_eq!(after, [0.0; 20]);
    }

    #[test]
    fn normal_pit_dense_modes_consume_tail_before_ordinary_handoff() {
        for pcm in [false, true] {
            let mut speaker = PcSpeaker::new();
            speaker.update_pit_divisor(1594);
            if pcm {
                speaker.write_control(0x03);
                for i in 0..40 {
                    speaker.set_cycle(i * 2);
                    speaker.set_frequency(100 + i as u16);
                }
            } else {
                for i in 0..32 {
                    speaker.set_cycle(i * 2);
                    speaker.write_control(if i % 2 == 0 { 0x03 } else { 0x00 });
                }
            }
            speaker.set_cycle(90);
            speaker.write_control(0x03);
            speaker.set_cycle(99);
            speaker.write_control(0x00);
            speaker.set_cycle(200);
            speaker.write_control(0x03); // Future live state must not leak backward.
            let mut dense = [0.0; 8];
            speaker.generate_samples_in_window(0, 100, 4, 44_100, &mut dense);
            assert!(dense.iter().any(|sample| *sample != 0.0));
            assert!(
                !speaker.pit_window_state.sounding,
                "dense path consumes tail off (pcm={pcm})"
            );
            assert_eq!(
                speaker.pit_sound_events.front().map(|event| event.0),
                Some(200)
            );
            // Drop the classifier carry deliberately to inspect ordinary handoff.
            speaker.direct_window_mode_active = false;
            speaker.pcm_window_mode_active = false;
            let mut gap = [0.0; 8];
            speaker.generate_samples_in_window(100, 200, 4, 44_100, &mut gap);
            assert_eq!(
                gap, [0.0; 8],
                "no stale ordinary tone between dense/future output"
            );
        }
    }

    #[test]
    fn normal_pit_sync_cannot_overwrite_queued_history_and_legacy_consumes_it() {
        let mut speaker = PcSpeaker::new();
        speaker.update_pit_divisor(1594);
        speaker.set_cycle(100);
        speaker.write_control(0x03);
        speaker.set_cycle(200);
        speaker.write_control(0x00);
        speaker.sync_divisor(0x1212);
        assert_eq!(speaker.pit_sound_events.len(), 2);
        let mut before = [0.0; 8];
        speaker.generate_samples_in_window(0, 100, 4, 10_000, &mut before);
        assert_eq!(before, [0.0; 8]);
        let mut during = [0.0; 8];
        speaker.generate_samples_in_window(100, 200, 4, 10_000, &mut during);
        assert!(
            during.iter().any(|sample| *sample != 0.0),
            "sync preserves historical tone"
        );
        assert_eq!(speaker.pit_window_state.divisor, 1594);
        speaker.generate_samples(4, 10_000, &mut [0.0; 8]);
        assert!(
            speaker.pit_sound_events.is_empty(),
            "legacy renderer consumes its history"
        );
        assert!(!speaker.pit_window_state.sounding);
    }

    #[test]
    fn normal_pit_silent_programming_has_no_audio_activity() {
        let mut speaker = PcSpeaker::new();
        for divisor in 1..100 {
            speaker.set_cycle(u64::from(divisor));
            speaker.update_pit_divisor(divisor);
        }
        assert_eq!(speaker.get_divisor(), 99);
        assert!(!speaker.has_pending_output());
        assert!(!speaker.has_pcm_maintenance());
        assert!(speaker.pit_sound_events.is_empty());
        speaker.write_control(0x03);
        assert_eq!(
            speaker
                .pit_sound_events
                .front()
                .map(|event| event.1.divisor),
            Some(99)
        );
    }

    #[test]
    fn explicit_window_preserves_pit_beep_completed_before_mixing() {
        let mut speaker = PcSpeaker::new();
        speaker.set_cycle(2_000);
        speaker.set_frequency(2048); // Synthetic power-of-two divisor.
        speaker.write_control(0x03);
        speaker.set_cycle(4_000);
        speaker.write_control(0x00);
        // Change the idle channel setting before mixing the buffered beep.
        speaker.set_frequency(0x1000);

        let mut output = [0.0f32; 200];
        speaker.generate_samples_in_window(0, 10_000, 100, 10_000, &mut output);
        assert!(output[..40].iter().all(|&sample| sample == 0.0));
        assert!(
            output[40..80]
                .iter()
                .all(|&sample| sample.abs() == speaker.volume),
            "the complete PIT beep vanished because the speaker was off when mixing ran"
        );
        assert!(output[40..80].iter().any(|&sample| sample > 0.0));
        assert!(output[40..80].iter().any(|&sample| sample < 0.0));
        assert!(output[80..].iter().all(|&sample| sample == 0.0));

        let mut following = [0.0f32; 200];
        speaker.generate_samples_in_window(10_000, 20_000, 100, 10_000, &mut following);
        assert!(following.iter().all(|&sample| sample == 0.0));
    }

    #[test]
    fn explicit_window_preserves_pit_divisor_changes_before_mixing() {
        let mut speaker = PcSpeaker::new();
        speaker.set_cycle(2_000);
        speaker.set_frequency(2048);
        speaker.write_control(0x03);
        speaker.set_cycle(3_000);
        speaker.set_frequency(4096);
        speaker.set_cycle(4_000);
        speaker.write_control(0x00);
        speaker.set_frequency(0x1000);

        let mut output = [0.0f32; 200];
        speaker.generate_samples_in_window(0, 10_000, 100, 10_000, &mut output);
        assert!(output[..40].iter().all(|&sample| sample == 0.0));
        let mut phase = 0.0;
        for i in 20..40 {
            let divisor = if i < 30 { 2048 } else { 4096 };
            phase += PIT_FREQUENCY / (f64::from(divisor) * 10_000.0);
            if phase >= 1.0 {
                phase -= 1.0;
            }
            let expected = if phase < 0.5 {
                speaker.volume
            } else {
                -speaker.volume
            };
            assert_eq!(
                &output[i * 2..i * 2 + 2],
                &[expected, expected],
                "sample {i} must use the divisor programmed at that guest cycle"
            );
        }
        assert!(output[80..].iter().all(|&sample| sample == 0.0));
    }

    #[test]
    fn explicit_window_keeps_end_event_for_next_direct_render() {
        let mut speaker = PcSpeaker::new();

        // Establish direct-toggle mode with thirteen earlier state changes.
        // The final earlier event leaves the rendered state on.
        for i in 0..13 {
            speaker.set_cycle(i * 7);
            speaker.write_control(if i % 2 == 0 { 0x02 } else { 0x00 });
        }
        speaker.set_cycle(100);
        speaker.write_control(0x00); // Window start: first sample is off.
        speaker.set_cycle(150);
        speaker.write_control(0x02); // Exact interior sample boundary: on.
        speaker.set_cycle(200);
        speaker.write_control(0x00); // Window end: belongs to the next window.
        assert_eq!(speaker.toggle_events.len(), DIRECT_TOGGLE_THRESHOLD);

        let mut first = [0.0f32; 8];
        speaker.generate_samples_in_window(100, 200, 4, 44100, &mut first);
        assert!(first[0] < 0.0 && first[2] < 0.0, "start event is visible");
        assert!(
            first[4] > 0.0 && first[6] > 0.0,
            "interior-boundary event is visible at its boundary"
        );
        assert_eq!(speaker.toggle_events.front(), Some(&(200, false)));

        let mut second = [0.0f32; 8];
        speaker.generate_samples_in_window(200, 300, 4, 44100, &mut second);
        assert!(
            second.iter().all(|sample| *sample < 0.0),
            "end event must first affect the following half-open window"
        );
    }

    #[test]
    fn test_few_toggles_use_pit_mode_not_direct() {
        // 2-3 toggle events (e.g. from note change: disable→enable) should NOT
        // trigger direct toggle mode. PIT square wave should be used instead.
        let mut speaker = PcSpeaker::new();
        speaker.set_cycles_per_frame(10000);
        speaker.set_frequency(1193); // ~1000 Hz

        // Simulate a note change: disable then re-enable
        speaker.set_cycle(0);
        speaker.write_control(0x03); // enable (1 event)
        speaker.set_cycle(100);
        speaker.write_control(0x00); // disable (2 events)
        speaker.set_cycle(200);
        speaker.write_control(0x03); // re-enable (3 events)

        assert_eq!(speaker.toggle_events.len(), 3);

        let mut output = [0.0f32; 200];
        speaker.generate_samples(100, 44100, &mut output);

        // Should have used PIT mode, not direct toggle.
        // PIT mode produces a proper square wave with both + and - values.
        let has_positive = output.iter().any(|&s| s > 0.0);
        let has_negative = output.iter().any(|&s| s < 0.0);
        assert!(has_positive, "PIT mode should produce positive samples");
        assert!(has_negative, "PIT mode should produce negative samples");

        // Toggle events should have been cleared
        assert!(!speaker.has_toggle_events());
    }

    #[test]
    fn test_volume_default() {
        let speaker = PcSpeaker::new();
        assert_eq!(speaker.get_volume(), 0.3);
    }

    #[test]
    fn test_set_volume() {
        let mut speaker = PcSpeaker::new();

        speaker.set_volume(0.5);
        assert_eq!(speaker.get_volume(), 0.5);

        speaker.set_volume(1.0);
        assert_eq!(speaker.get_volume(), 1.0);

        speaker.set_volume(0.0);
        assert_eq!(speaker.get_volume(), 0.0);
    }

    #[test]
    fn test_set_volume_clamps() {
        let mut speaker = PcSpeaker::new();

        // Values > 1.0 should clamp to 1.0
        speaker.set_volume(1.5);
        assert_eq!(speaker.get_volume(), 1.0);

        // Values < 0.0 should clamp to 0.0
        speaker.set_volume(-0.5);
        assert_eq!(speaker.get_volume(), 0.0);
    }

    #[test]
    fn test_volume_affects_output() {
        // Test at low volume
        let mut speaker_low = PcSpeaker::new();
        speaker_low.write_control(0x03);
        speaker_low.set_frequency(1000);
        speaker_low.set_volume(0.1);

        let mut output_low = [0.0f32; 200];
        speaker_low.generate_samples(100, 44100, &mut output_low);
        let max_low: f32 = output_low.iter().map(|s| s.abs()).fold(0.0, f32::max);

        // Test at high volume
        let mut speaker_high = PcSpeaker::new();
        speaker_high.write_control(0x03);
        speaker_high.set_frequency(1000);
        speaker_high.set_volume(0.8);

        let mut output_high = [0.0f32; 200];
        speaker_high.generate_samples(100, 44100, &mut output_high);
        let max_high: f32 = output_high.iter().map(|s| s.abs()).fold(0.0, f32::max);

        // Higher volume should produce larger amplitude
        assert!(
            max_high > max_low * 2.0,
            "High volume ({max_high}) should be much larger than low volume ({max_low})"
        );
    }

    #[test]
    fn test_clear_toggle_events() {
        let mut speaker = PcSpeaker::new();
        speaker.write_control(0x02); // Add an event
        assert!(speaker.has_toggle_events());

        speaker.clear_toggle_events();
        assert!(!speaker.has_toggle_events());
    }

    #[test]
    fn test_generate_samples_clears_single_event() {
        let mut speaker = PcSpeaker::new();
        speaker.write_control(0x02); // Add 1 event
        assert!(speaker.has_toggle_events());
        assert_eq!(speaker.toggle_events.len(), 1);

        let mut output = [0.0f32; 10];
        // Sub-threshold events should be cleared and PIT mode used (silence if PIT not active)
        speaker.generate_samples(5, 44100, &mut output);

        assert!(!speaker.has_toggle_events());
        // Verify output is silence (PIT not active)
        assert!(output.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn test_generate_samples_clears_sub_threshold_toggle_events() {
        let mut speaker = PcSpeaker::new();
        speaker.set_cycle(100);
        speaker.write_control(0x02);
        speaker.set_cycle(200);
        speaker.write_control(0x00);

        assert_eq!(speaker.toggle_events.len(), 2);

        let mut output = [0.0f32; 10];
        // 2 events is below DIRECT_TOGGLE_THRESHOLD — should be cleared, not consumed
        speaker.generate_samples(5, 44100, &mut output);

        assert!(!speaker.has_toggle_events());
    }

    // =========================================================================
    // SND-16: PC Speaker PCM mode tests
    // =========================================================================

    #[test]
    fn test_pcm_detection_below_threshold() {
        let mut speaker = PcSpeaker::new();

        // Fewer than PCM_DETECTION_THRESHOLD reprogram events
        for i in 0..10 {
            speaker.set_cycle(i * 100);
            speaker.set_frequency(100 + i as u16);
        }

        assert!(!speaker.has_pcm_events());
        assert!(speaker.has_pcm_maintenance());
        assert!(speaker.has_pending_output());
    }

    #[test]
    fn test_pcm_detection_above_threshold() {
        let mut speaker = PcSpeaker::new();

        // At least PCM_DETECTION_THRESHOLD reprogram events
        for i in 0..PCM_DETECTION_THRESHOLD + 10 {
            speaker.set_cycle(i as u64 * 100);
            speaker.set_frequency(50 + (i % 200) as u16);
        }

        assert!(speaker.has_pcm_events());
    }

    #[test]
    fn test_pcm_generates_samples() {
        let mut speaker = PcSpeaker::new();
        speaker.set_cycles_per_frame(10000);
        speaker.write_control(0x03); // Enable speaker + gate (PCM requires gated output)

        // Generate enough PIT reprogramming events for PCM detection
        for i in 0..64 {
            speaker.set_cycle(i * 150);
            // Vary divisor around 128 center to get varying amplitude
            let divisor = if i % 2 == 0 { 200 } else { 50 };
            speaker.set_frequency(divisor);
        }

        assert!(speaker.has_pcm_events());

        let mut output = [0.0f32; 200];
        speaker.generate_samples(100, 44100, &mut output);

        // Should have produced varying samples
        let has_positive = output.iter().any(|&s| s > 0.0);
        let has_negative = output.iter().any(|&s| s < 0.0);
        assert!(
            has_positive || has_negative,
            "PCM mode should produce non-zero samples"
        );
    }

    #[test]
    fn test_pcm_clears_after_generate() {
        let mut speaker = PcSpeaker::new();
        speaker.set_cycles_per_frame(10000);

        for i in 0..64 {
            speaker.set_cycle(i * 100);
            speaker.set_frequency(100);
        }

        assert!(speaker.has_pcm_events());

        let mut output = [0.0f32; 200];
        speaker.generate_samples(100, 44100, &mut output);

        // PCM events should be consumed after generate
        assert!(!speaker.has_pcm_events());
    }

    #[test]
    fn test_pcm_takes_priority_over_direct_toggle() {
        let mut speaker = PcSpeaker::new();
        speaker.set_cycles_per_frame(10000);

        // Add both toggle events and PCM events
        for i in 0..10 {
            speaker.set_cycle(i * 100);
            speaker.write_control(if i % 2 == 0 { 0x02 } else { 0x00 });
        }
        for i in 0..64 {
            speaker.set_cycle(i * 100);
            speaker.set_frequency(100 + (i % 150) as u16);
        }

        assert!(speaker.has_pcm_events());
        assert!(speaker.has_toggle_events());

        let mut output = [0.0f32; 200];
        speaker.generate_samples(100, 44100, &mut output);

        // PCM should have been consumed (it takes priority)
        assert!(!speaker.has_pcm_events());
    }

    #[test]
    fn test_clear_toggle_events_also_clears_pcm() {
        let mut speaker = PcSpeaker::new();

        for i in 0..64 {
            speaker.set_cycle(i * 100);
            speaker.set_frequency(100);
        }

        assert!(speaker.has_pcm_events());

        speaker.clear_toggle_events();
        assert!(!speaker.has_pcm_events());
    }

    #[test]
    fn test_clear_pcm_events() {
        let mut speaker = PcSpeaker::new();

        for i in 0..64 {
            speaker.set_cycle(i * 100);
            speaker.set_frequency(100);
        }

        assert!(speaker.has_pcm_events());
        speaker.clear_pcm_events();
        assert!(!speaker.has_pcm_events());
    }

    #[test]
    fn test_pcm_volume_scaling() {
        let mut speaker = PcSpeaker::new();
        speaker.set_cycles_per_frame(10000);
        speaker.set_volume(0.5);
        speaker.write_control(0x03); // Enable speaker + gate

        // All divisor=255 → amplitude = (255-128)/128 ≈ 0.992
        // Scaled by volume 0.5 → ~0.496
        for i in 0..64 {
            speaker.set_cycle(i * 100);
            speaker.set_frequency(255);
        }

        let mut output = [0.0f32; 200];
        speaker.generate_samples(100, 44100, &mut output);

        let max_sample: f32 = output.iter().map(|s| s.abs()).fold(0.0, f32::max);
        assert!(
            max_sample < 0.6,
            "PCM at volume 0.5 should cap around 0.5, got {max_sample}"
        );
        assert!(
            max_sample > 0.3,
            "PCM at volume 0.5 should produce audible output, got {max_sample}"
        );
    }

    #[test]
    fn test_pcm_discarded_when_speaker_off() {
        // PCM events that accumulated while the speaker was on should be
        // discarded if the speaker is turned off before the mixer runs.
        let mut speaker = PcSpeaker::new();
        speaker.set_cycles_per_frame(10000);
        speaker.write_control(0x03); // Enable speaker

        for i in 0..64 {
            speaker.set_cycle(i * 100);
            speaker.set_frequency(100 + (i % 150) as u16);
        }
        assert!(speaker.has_pcm_events());

        // Turn speaker off before generating samples
        speaker.write_control(0x00);

        let mut output = [0.0f32; 200];
        speaker.generate_samples(100, 44100, &mut output);

        // All samples should be zero (silence)
        assert!(
            output.iter().all(|&s| s == 0.0),
            "PCM events should be discarded when speaker is off"
        );
        // Events should be cleared
        assert!(!speaker.has_pcm_events());
    }

    /// Drive `toggles_per_ms` port-61h toggles across `total_ms` of guest time
    /// and report whether the renderer classified them as direct-toggle audio.
    ///
    /// `chunk_ms` is the CALLER's render granularity — the whole span in one
    /// call, or one call per millisecond. The guest does exactly the same thing
    /// either way, so the verdict must be the same either way.
    ///
    /// The verdict is read off the output: with the divisor left at 0 and the
    /// timer gate closed, PIT mode renders silence and returns early, while
    /// direct mode is exempt from the gate check and renders the toggle
    /// waveform. So any non-zero sample means direct mode ran.
    fn direct_mode_verdict(toggles_per_ms: usize, total_ms: usize, chunk_ms: usize) -> bool {
        const SAMPLE_RATE: u32 = 44100;
        const CYCLES_PER_MS: u64 = 1000;

        let mut speaker = PcSpeaker::new();
        speaker.set_cycles_per_frame(CYCLES_PER_MS * chunk_ms as u64);

        let samples_per_chunk = (SAMPLE_RATE as usize * chunk_ms) / 1000;
        let toggles_per_chunk = toggles_per_ms * chunk_ms;
        let chunk_cycles = CYCLES_PER_MS * chunk_ms as u64;
        let mut rendered_any = false;

        for chunk in 0..(total_ms / chunk_ms) {
            let chunk_start = chunk as u64 * chunk_cycles;
            for step in 0..toggles_per_chunk {
                speaker.set_cycle(
                    chunk_start + (step as u64 * chunk_cycles) / toggles_per_chunk as u64,
                );
                speaker.write_control(if step % 2 == 0 { 0x02 } else { 0x00 });
            }
            speaker.set_cycle(chunk_start + chunk_cycles);

            let mut output = vec![0.0f32; samples_per_chunk * 2];
            speaker.generate_samples(samples_per_chunk, SAMPLE_RATE, &mut output);
            rendered_any |= output.iter().any(|&s| s != 0.0);
        }

        rendered_any
    }

    #[test]
    fn direct_mode_classification_does_not_depend_on_render_chunk_size() {
        // 4 toggles/ms is an order of magnitude below bit-banged speaker PCM
        // (~7 kHz upward), so it is PIT-gating behaviour and must read that way
        // however the caller chunks the render. Before MDD-BUG-ANVIL-00111 the
        // threshold was a per-CALL count, so rendering the same 8 ms in one
        // call cleared 16 events and flipped it to direct mode.
        let per_ms = direct_mode_verdict(4, 8, 1);
        let one_call = direct_mode_verdict(4, 8, 8);

        assert!(!per_ms, "4 toggles/ms is not bit-banged direct-mode audio");
        assert_eq!(
            one_call, per_ms,
            "render chunk size, not guest behaviour, decided the speaker mode"
        );
    }

    #[test]
    fn a_genuine_bitbang_rate_reads_as_direct_at_every_chunk_size() {
        // The converse guard: rate-normalising must not make a REAL direct
        // stream unreachable at a long render window. 20 toggles/ms over 8 ms
        // is 160 events against a 128-entry ring, so this also pins that
        // dropping events off the tail leaves the retained prefix's rate — and
        // therefore the verdict — intact.
        let per_ms = direct_mode_verdict(20, 8, 1);
        let one_call = direct_mode_verdict(20, 8, 8);

        assert!(per_ms, "20 toggles/ms is bit-banged direct-mode audio");
        assert_eq!(
            one_call, per_ms,
            "render chunk size, not guest behaviour, decided the speaker mode"
        );
    }
}
