//! V.21 FSK modulator and demodulator for 300-baud data exchange.
//!
//! Used during V.8 negotiation and as a standalone low-speed modem mode.
//!
//! V.21 defines two channels with frequency-shift keying:
//!
//! | Channel    | Mark (1)  | Space (0) |
//! |------------|-----------|-----------|
//! | Originate  |  980 Hz   | 1180 Hz   |
//! | Answer     | 1650 Hz   | 1850 Hz   |
//!
//! Sample rate: 8000 Hz, baud rate: 300, samples per bit: 26.667.

use std::collections::VecDeque;

use crate::dsp::goertzel::Goertzel;
use crate::dsp::oscillator::Oscillator;

// V.21 channel frequencies
const ORIGINATE_MARK_HZ: f64 = 980.0;
const ORIGINATE_SPACE_HZ: f64 = 1180.0;
const ANSWER_MARK_HZ: f64 = 1650.0;
const ANSWER_SPACE_HZ: f64 = 1850.0;

const SAMPLE_RATE: u32 = 8000;
const BAUD_RATE: f64 = 300.0;
const SAMPLES_PER_BIT: f64 = SAMPLE_RATE as f64 / BAUD_RATE; // 26.667

/// Amplitude for generated PCM (well below i16::MAX to avoid clipping).
const AMPLITUDE: f64 = 16000.0;

// ---------------------------------------------------------------------------
// FSK Modulator
// ---------------------------------------------------------------------------

/// V.21 FSK modulator. Generates PCM audio from a bitstream.
pub struct FskModulator {
    osc: Oscillator,
    mark_freq: f64,
    space_freq: f64,
    samples_per_bit: f64,
    /// Fractional carry across bit boundaries. Holds the not-yet-emitted part
    /// of the ideal bit period, so 26.667 samples per bit averages exactly.
    sample_debt: f64,
    current_bit: bool, // true = mark (1), false = space (0)
}

impl FskModulator {
    /// Create a modulator for the originate (caller) channel.
    pub fn new_originate() -> Self {
        Self::new(ORIGINATE_MARK_HZ, ORIGINATE_SPACE_HZ)
    }

    /// Create a modulator for the answer channel.
    pub fn new_answer() -> Self {
        Self::new(ANSWER_MARK_HZ, ANSWER_SPACE_HZ)
    }

    fn new(mark_freq: f64, space_freq: f64) -> Self {
        Self {
            osc: Oscillator::new(mark_freq, SAMPLE_RATE),
            mark_freq,
            space_freq,
            samples_per_bit: SAMPLES_PER_BIT,
            sample_debt: 0.0,
            current_bit: true, // idle = mark
        }
    }

    /// Queue a byte for transmission (LSB first, with start/stop bits).
    ///
    /// Async serial format: 1 start bit (space) + 8 data bits LSB-first + 1 stop bit (mark).
    /// Appends the generated PCM samples to `output`.
    pub fn modulate_byte(&mut self, byte: u8, output: &mut Vec<i16>) {
        // Start bit (space = 0)
        self.send_bit(false, output);

        // 8 data bits, LSB first
        for i in 0..8 {
            let bit = (byte >> i) & 1 != 0;
            self.send_bit(bit, output);
        }

        // Stop bit (mark = 1)
        self.send_bit(true, output);
    }

    /// Generate a single sample of the current bit.
    pub fn next_sample(&mut self) -> i16 {
        let sample = self.osc.next_sample() * AMPLITUDE;
        sample as i16
    }

    /// Set the current bit (mark or space) for continuous tone.
    pub fn set_bit(&mut self, mark: bool) {
        self.current_bit = mark;
        let freq = if mark {
            self.mark_freq
        } else {
            self.space_freq
        };
        self.osc.set_frequency(freq, SAMPLE_RATE);
    }

    /// Generate idle (continuous mark) samples.
    pub fn generate_idle(&mut self, samples: usize, output: &mut Vec<i16>) {
        self.set_bit(true);
        for _ in 0..samples {
            output.push(self.next_sample());
        }
    }

    /// Send a single bit as PCM samples appended to `output`.
    fn send_bit(&mut self, mark: bool, output: &mut Vec<i16>) {
        self.set_bit(mark);

        // V.21's 26.667 samples per bit is not an integer. Zeroing a counter
        // per bit and emitting while it was below the period produced 27
        // samples for EVERY bit -- 8000/27 = 296.296 baud, not 300
        // (MDD-BUG-CRUCIBLE-00519). The comment here used to claim fractional
        // tracking; the per-bit reset is exactly what defeated it.
        //
        // Instead accumulate the ideal period as a debt and pay out whole
        // samples, carrying the remainder into the next bit: periods then run
        // 27, 27, 26 (80 samples per 3 bits) and average exactly 8000/300.
        self.sample_debt += self.samples_per_bit;
        while self.sample_debt >= 1.0 {
            output.push(self.next_sample());
            self.sample_debt -= 1.0;
        }
    }
}

// ---------------------------------------------------------------------------
// FSK Demodulator
// ---------------------------------------------------------------------------

/// State machine for async serial framing within the demodulator.
#[derive(Debug, Clone)]
enum FskDemodState {
    /// Waiting for start bit (transition from mark idle to space).
    WaitingForStart,
    /// A mark->space flip was seen at `flip_idx`; collecting enough further
    /// samples that the whole start bit is in history and its edge can be
    /// located to the sample.
    LocatingEdge { flip_idx: u64 },
    /// Sampling a bit (start, data, or stop).
    SamplingBit { bit_index: u8, byte_acc: u8 },
}

/// V.21 FSK demodulator. Recovers bytes from PCM audio.
pub struct FskDemodulator {
    mark_det: Goertzel,
    space_det: Goertzel,
    samples_per_bit: f64,
    /// Accumulated samples for the current bit period.
    bit_buffer: Vec<i16>,
    /// Trailing one-bit window used to find the start edge while idle. It
    /// slides one SAMPLE at a time, which is what makes the recovered frame
    /// phase sample-accurate instead of quantised to a whole bit period.
    idle_window: Vec<i16>,
    /// Recent samples, kept so the start edge can be located by searching
    /// BACKWARDS once a flip is seen. Retains a few bit periods.
    hist: Vec<i16>,
    /// Absolute sample index of `hist[0]`.
    hist_start_abs: u64,
    /// State machine for async serial framing.
    state: FskDemodState,
    /// Decoded bytes ready for retrieval.
    decoded: VecDeque<u8>,
    /// Running detection: was the previous sample-window a mark?
    last_was_mark: bool,
    /// Index of the next sample to be processed, counted from construction.
    abs_index: u64,
    /// Absolute sample index at which the current bit's window opens.
    bit_start: f64,
    /// Absolute sample index at which the current bit's window closes. Held
    /// as f64 so successive bits step by the FRACTIONAL period and the frame
    /// never drifts against a standards-rate peer.
    bit_end: f64,
}

impl FskDemodulator {
    /// Create a demodulator for the originate channel.
    pub fn new_originate() -> Self {
        Self::new(ORIGINATE_MARK_HZ, ORIGINATE_SPACE_HZ)
    }

    /// Create a demodulator for the answer channel.
    pub fn new_answer() -> Self {
        Self::new(ANSWER_MARK_HZ, ANSWER_SPACE_HZ)
    }

    fn new(mark_freq: f64, space_freq: f64) -> Self {
        Self {
            mark_det: Goertzel::new(mark_freq, SAMPLE_RATE),
            space_det: Goertzel::new(space_freq, SAMPLE_RATE),
            samples_per_bit: SAMPLES_PER_BIT,
            bit_buffer: Vec::with_capacity(32),
            idle_window: Vec::with_capacity(32),
            hist: Vec::with_capacity(256),
            hist_start_abs: 0,
            state: FskDemodState::WaitingForStart,
            decoded: VecDeque::new(),
            last_was_mark: true, // idle = mark
            abs_index: 0,
            bit_start: 0.0,
            bit_end: 0.0,
        }
    }

    /// Feed PCM samples. Any decoded bytes become available via `take_byte()`.
    pub fn demodulate(&mut self, samples: &[i16]) {
        for &sample in samples {
            self.process_sample(sample);
        }
    }

    /// Take the next decoded byte (if available).
    pub fn take_byte(&mut self) -> Option<u8> {
        self.decoded.pop_front()
    }

    /// Whether decoded bytes are available.
    pub fn has_data(&self) -> bool {
        !self.decoded.is_empty()
    }

    /// Reset state.
    pub fn reset(&mut self) {
        self.bit_buffer.clear();
        self.idle_window.clear();
        self.hist.clear();
        self.hist_start_abs = 0;
        self.state = FskDemodState::WaitingForStart;
        self.decoded.clear();
        self.last_was_mark = true;
        self.abs_index = 0;
        self.bit_start = 0.0;
        self.bit_end = 0.0;
    }

    /// Process a single PCM sample through the demodulator.
    ///
    /// Framing is recovered in two stages, which is what lets the receiver
    /// track a true 300-baud peer (MDD-BUG-CRUCIBLE-00519):
    ///
    /// 1. While idle, a one-bit window slides a SAMPLE at a time looking for
    ///    the mark-to-space start edge, whose position is then pinned by
    ///    correlation. The previous code consumed a whole window per
    ///    detection, so the recovered phase was quantised to a bit period and
    ///    could sit a full bit from the true edge.
    /// 2. Every following bit window is stepped by the FRACTIONAL period from
    ///    that edge, so window lengths alternate 26/27 to match the
    ///    transmitter instead of being fixed at a rounded 27.
    ///
    /// The two stages are inseparable: a rounded receiver only ever worked
    /// because the transmitter shared the same wrong integer period.
    fn process_sample(&mut self, sample: i16) {
        let idx = self.abs_index;
        self.abs_index += 1;
        self.push_hist(sample);

        match self.state.clone() {
            FskDemodState::WaitingForStart => {
                // Trailing one-bit window, advanced one sample at a time.
                let width = self.samples_per_bit.round() as usize;
                self.idle_window.push(sample);
                if self.idle_window.len() > width {
                    self.idle_window.remove(0);
                }
                if self.idle_window.len() < width {
                    return;
                }

                let is_mark = self.detect_bit(&self.idle_window);
                if !is_mark && self.last_was_mark {
                    // A flip only BOUNDS the edge, it does not locate it: the
                    // window tips to space somewhere around half overlap. Wait
                    // until the whole start bit is in history, then correlate.
                    self.state = FskDemodState::LocatingEdge { flip_idx: idx };
                }
                self.last_was_mark = is_mark;
            }
            FskDemodState::LocatingEdge { flip_idx } => {
                let width = self.samples_per_bit.round() as u64;
                if idx < flip_idx + width {
                    return; // still filling; the start bit is not fully held yet
                }

                let edge = self.locate_start_edge(flip_idx);
                // The start bit occupies [edge, edge + 1 bit); the first DATA
                // bit is the one after it.
                self.bit_start = edge + self.samples_per_bit;
                self.bit_end = edge + 2.0 * self.samples_per_bit;
                self.idle_window.clear();
                self.bit_buffer.clear();
                self.state = FskDemodState::SamplingBit {
                    bit_index: 0,
                    byte_acc: 0,
                };
                // Locating the edge costs latency, so part of the frame is
                // already behind us. Every bit is evaluated from history, so
                // the decoder simply catches up.
                self.pump_bits(idx);
            }
            FskDemodState::SamplingBit { .. } => {
                self.pump_bits(idx);
            }
        }
    }

    /// Evaluate every bit whose window has fully arrived, reading from
    /// history rather than from live samples.
    ///
    /// Driving the frame off absolute time is what lets the decoder tolerate
    /// the latency of locating the start edge: bits that elapsed while we were
    /// searching are simply replayed out of `hist`.
    fn pump_bits(&mut self, upto_abs: u64) {
        loop {
            let (bit_index, byte_acc) = match self.state {
                FskDemodState::SamplingBit {
                    bit_index,
                    byte_acc,
                } => (bit_index, byte_acc),
                _ => return,
            };
            if self.bit_end > (upto_abs + 1) as f64 {
                return; // window still open
            }
            let Some(win) = self.window_from_hist(self.bit_start, self.bit_end) else {
                // History no longer covers the window; resynchronise rather
                // than emit a byte assembled from nothing.
                self.idle_window.clear();
                self.state = FskDemodState::WaitingForStart;
                return;
            };
            let is_mark = if win.is_empty() {
                true
            } else {
                self.detect_bit(&win)
            };

            // Step by the FRACTIONAL period so the ten bits of a frame
            // accumulate no rounding error against a standards-rate peer.
            self.bit_start = self.bit_end;
            self.bit_end += self.samples_per_bit;

            if bit_index < 8 {
                // Data bit (LSB first)
                let new_acc = if is_mark {
                    byte_acc | (1 << bit_index)
                } else {
                    byte_acc
                };
                self.state = FskDemodState::SamplingBit {
                    bit_index: bit_index + 1,
                    byte_acc: new_acc,
                };
            } else {
                // This was the stop bit (bit_index == 8). Emit the byte. We
                // could validate is_mark here, but real modems are lenient.
                self.decoded.push_back(byte_acc);
                self.last_was_mark = is_mark;
                self.idle_window.clear();
                self.state = FskDemodState::WaitingForStart;
                return;
            }
        }
    }

    /// The samples spanning `[start, end)`, if history still covers them.
    fn window_from_hist(&self, start: f64, end: f64) -> Option<Vec<i16>> {
        let s = start.round().max(0.0) as u64;
        let e = end.round().max(0.0) as u64;
        if e <= s || s < self.hist_start_abs {
            return None;
        }
        let js = (s - self.hist_start_abs) as usize;
        let je = (e - self.hist_start_abs) as usize;
        if je > self.hist.len() {
            return None;
        }
        Some(self.hist[js..je].to_vec())
    }

    /// Retain the most recent few bit periods so the start edge can be found
    /// by looking backwards, and so elapsed bits can be replayed.
    fn push_hist(&mut self, sample: i16) {
        let cap = (self.samples_per_bit.round() as usize) * 8;
        self.hist.push(sample);
        if self.hist.len() > cap {
            let drop = self.hist.len() - cap;
            self.hist.drain(..drop);
            self.hist_start_abs += drop as u64;
        }
    }

    /// Find the start bit's leading edge, to the sample.
    ///
    /// Scores each candidate alignment on the mark-to-space TRANSITION: space
    /// over the bit starting at `e`, and mark over the half-bit before it.
    ///
    /// Scoring space alone is not enough, and the failure is not subtle. A
    /// start bit whose first data bits are also space gives one long space
    /// run, every alignment inside it scores about the same, and the search
    /// drifts LATE -- enough to pull the following bit into the window and
    /// drop a one bit (0x4C decoded as 0x0C). Requiring mark immediately
    /// before the candidate anchors the search on the edge itself, which is
    /// the only feature that is actually unique.
    fn locate_start_edge(&self, flip_idx: u64) -> f64 {
        let width = self.samples_per_bit.round() as usize;
        let guard = width / 2; // preceding mark evidence
        let span = width as u64 + width as u64 / 2;
        let lo = flip_idx
            .saturating_sub(span)
            .max(self.hist_start_abs + guard as u64);

        let mut best = flip_idx as f64 - self.samples_per_bit / 2.0;
        let mut best_score = f64::NEG_INFINITY;
        for e in lo..=flip_idx {
            let j = (e - self.hist_start_abs) as usize;
            if j + width > self.hist.len() || j < guard {
                continue;
            }
            let bit = &self.hist[j..j + width];
            let before = &self.hist[j - guard..j];
            let space_here = self.space_det.energy(bit) - self.mark_det.energy(bit);
            let mark_before = self.mark_det.energy(before) - self.space_det.energy(before);
            let score = space_here + mark_before;
            if score > best_score {
                best_score = score;
                best = e as f64;
            }
        }
        best
    }

    /// Detect whether a block of samples represents mark (true) or space (false).
    ///
    /// If neither frequency has significant energy (signal is off-channel),
    /// returns true (mark = idle) to avoid false start-bit detection.
    fn detect_bit(&self, samples: &[i16]) -> bool {
        let mark_energy = self.mark_det.energy(samples);
        let space_energy = self.space_det.energy(samples);

        // Require meaningful energy at one of the two target frequencies.
        // Total signal energy provides a baseline for comparison.
        let total_energy: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
        let combined_target = mark_energy + space_energy;

        // If less than 10% of total signal energy is at our two target
        // frequencies, this signal is not meant for us — treat as idle (mark).
        let n = samples.len() as f64;
        if total_energy > 0.0 && combined_target < total_energy * n * 0.05 {
            return true; // idle
        }

        mark_energy > space_energy
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn test_fsk_modulate_byte() {
        let mut mod_orig = FskModulator::new_originate();
        let mut output = Vec::new();
        mod_orig.modulate_byte(0x55, &mut output);

        // 10 bits * 26.67 samples/bit ≈ 267 samples
        let expected_min = (10.0 * SAMPLES_PER_BIT).floor() as usize;
        let expected_max = (10.0 * SAMPLES_PER_BIT).ceil() as usize + 10; // allow rounding
        assert!(
            output.len() >= expected_min && output.len() <= expected_max,
            "Expected ~267 samples for 10 bits, got {}",
            output.len()
        );

        // Verify no silence — every sample should have some signal
        let max_abs = output
            .iter()
            .map(|s| s.unsigned_abs() as u32)
            .max()
            .unwrap();
        assert!(
            max_abs > 1000,
            "Signal should have significant amplitude, max={max_abs}"
        );
    }

    #[test]
    fn test_fsk_roundtrip_originate() {
        let mut modulator = FskModulator::new_originate();
        let mut demodulator = FskDemodulator::new_originate();

        let test_byte: u8 = 0xA5;

        // Generate idle preamble so demodulator sees mark state
        let mut pcm = Vec::new();
        modulator.generate_idle(200, &mut pcm);
        modulator.modulate_byte(test_byte, &mut pcm);
        // Trailing idle so the stop bit completes
        modulator.generate_idle(100, &mut pcm);

        demodulator.demodulate(&pcm);

        let decoded = demodulator.take_byte();
        assert_eq!(
            decoded,
            Some(test_byte),
            "Roundtrip originate failed: expected 0x{test_byte:02X}, got {decoded:?}"
        );
    }

    #[test]
    fn test_fsk_roundtrip_answer() {
        let mut modulator = FskModulator::new_answer();
        let mut demodulator = FskDemodulator::new_answer();

        let test_byte: u8 = 0x42;

        let mut pcm = Vec::new();
        modulator.generate_idle(200, &mut pcm);
        modulator.modulate_byte(test_byte, &mut pcm);
        modulator.generate_idle(100, &mut pcm);

        demodulator.demodulate(&pcm);

        let decoded = demodulator.take_byte();
        assert_eq!(
            decoded,
            Some(test_byte),
            "Roundtrip answer failed: expected 0x{test_byte:02X}, got {decoded:?}"
        );
    }

    #[test]
    fn test_fsk_roundtrip_multiple_bytes() {
        let mut modulator = FskModulator::new_originate();
        let mut demodulator = FskDemodulator::new_originate();

        let message = b"HELLO";

        let mut pcm = Vec::new();
        modulator.generate_idle(200, &mut pcm);
        for &byte in message {
            modulator.modulate_byte(byte, &mut pcm);
        }
        modulator.generate_idle(100, &mut pcm);

        demodulator.demodulate(&pcm);

        for (i, &expected) in message.iter().enumerate() {
            let decoded = demodulator.take_byte();
            assert_eq!(
                decoded,
                Some(expected),
                "Byte {i} mismatch: expected 0x{expected:02X} ('{}'), got {decoded:?}",
                expected as char
            );
        }

        // No extra bytes
        assert!(
            !demodulator.has_data(),
            "Unexpected extra bytes in decoded output"
        );
    }

    #[test]
    fn test_fsk_idle_is_mark() {
        let mut modulator = FskModulator::new_originate();
        let mut pcm = Vec::new();
        modulator.generate_idle(200, &mut pcm);

        // Verify the idle signal is at mark frequency (980 Hz for originate)
        let mark_det = Goertzel::new(ORIGINATE_MARK_HZ, SAMPLE_RATE);
        let space_det = Goertzel::new(ORIGINATE_SPACE_HZ, SAMPLE_RATE);

        let mark_energy = mark_det.energy(&pcm);
        let space_energy = space_det.energy(&pcm);

        assert!(
            mark_energy > space_energy * 10.0,
            "Idle should be mark frequency: mark_energy={mark_energy}, space_energy={space_energy}"
        );
    }

    #[test]
    fn test_fsk_channels_independent() {
        // Modulate on originate channel
        let mut mod_orig = FskModulator::new_originate();
        let mut pcm = Vec::new();
        mod_orig.generate_idle(200, &mut pcm);
        mod_orig.modulate_byte(0x55, &mut pcm);
        mod_orig.generate_idle(100, &mut pcm);

        // Try to demodulate on the answer channel — should NOT decode any data
        let mut demod_answer = FskDemodulator::new_answer();
        demod_answer.demodulate(&pcm);
        assert!(
            !demod_answer.has_data(),
            "Answer demodulator should not decode originate-channel data"
        );

        // Conversely, originate demodulator should decode it
        let mut demod_orig = FskDemodulator::new_originate();
        demod_orig.demodulate(&pcm);
        assert_eq!(
            demod_orig.take_byte(),
            Some(0x55),
            "Originate demodulator should decode originate-channel data"
        );
    }

    #[test]
    fn test_fsk_start_stop_bits() {
        // This test used to assert 10 * ceil(26.667) = 270 samples per byte,
        // which pinned MDD-BUG-CRUCIBLE-00519 rather than the standard: a flat
        // 27 samples per bit IS the 296.296-baud defect. The property actually
        // worth pinning is that a byte costs ten bit periods of the REAL rate,
        // so its length is 266 or 267 depending where the fractional carry
        // stands -- never a constant 270.
        let mut modulator = FskModulator::new_originate();

        let mut output = Vec::new();
        modulator.modulate_byte(0x00, &mut output);
        let len_00 = output.len();

        output.clear();
        modulator.modulate_byte(0xFF, &mut output);
        let len_ff = output.len();

        // Length is independent of the DATA (ten bits either way) even though
        // it depends on the carry phase.
        for (label, len) in [("0x00", len_00), ("0xFF", len_ff)] {
            assert!(
                len == 266 || len == 267,
                "{label} produced {len} samples; ten bits at 8000/300 is 266.67, \
                 so each byte must be 266 or 267. 270 would mean the old flat \
                 27-samples-per-bit scheduler is back (MDD-BUG-CRUCIBLE-00519)."
            );
        }

        let ideal = 20.0 * SAMPLES_PER_BIT; // two bytes, ten bits each
        let actual = (len_00 + len_ff) as f64;
        assert!(
            (actual - ideal).abs() <= 1.0,
            "two bytes ran {actual} samples against an ideal {ideal}; the carry \
             must not accumulate error across bytes"
        );
    }

    /// The cumulative oracle MDD-BUG-CRUCIBLE-00519 asks for: over many bytes
    /// the transmitter must average exactly 300 baud, not 296.296.
    #[test]
    fn v21_transmit_averages_exactly_300_baud() {
        let mut modulator = FskModulator::new_originate();
        let mut pcm = Vec::new();

        const BYTES: usize = 200;
        for i in 0..BYTES {
            modulator.modulate_byte(i as u8, &mut pcm);
        }

        let bits = (BYTES * 10) as f64;
        let ideal = bits * SAMPLES_PER_BIT;
        let actual = pcm.len() as f64;

        // Error must stay bounded by the single unpaid sample of carry, NOT
        // grow with the message.
        assert!(
            (actual - ideal).abs() <= 1.0,
            "{BYTES} bytes ran {actual} samples against an ideal {ideal}"
        );

        let effective_baud = SAMPLE_RATE as f64 * bits / actual;
        assert!(
            (effective_baud - BAUD_RATE).abs() < 0.01,
            "effective baud {effective_baud} must be 300, not the 296.296 the \
             flat 27-sample scheduler produced"
        );

        // Anti-regression on the exact defect: a flat 27 per bit would run
        // 2000 samples longer over this message.
        assert_ne!(
            pcm.len(),
            BYTES * 10 * 27,
            "transmitter is back to a flat 27 samples per bit"
        );
    }

    /// Receive regression generated at exact 300-baud timing, as the entry
    /// asks. The bit periods are synthesised independently of `FskModulator`,
    /// so this pins the DEMODULATOR against the standard rate rather than
    /// against whatever our own transmitter happens to do.
    #[test]
    fn v21_receive_tracks_a_standards_rate_peer() {
        let mut osc = Oscillator::new(ORIGINATE_MARK_HZ, SAMPLE_RATE);
        let mut pcm = Vec::new();
        let mut debt = 0.0f64;

        let emit = |osc: &mut Oscillator, pcm: &mut Vec<i16>, debt: &mut f64, mark: bool| {
            osc.set_frequency(
                if mark {
                    ORIGINATE_MARK_HZ
                } else {
                    ORIGINATE_SPACE_HZ
                },
                SAMPLE_RATE,
            );
            *debt += SAMPLES_PER_BIT;
            while *debt >= 1.0 {
                pcm.push((osc.next_sample() * AMPLITUDE) as i16);
                *debt -= 1.0;
            }
        };

        // Idle preamble so the receiver has a mark reference.
        for _ in 0..200 {
            osc.set_frequency(ORIGINATE_MARK_HZ, SAMPLE_RATE);
            pcm.push((osc.next_sample() * AMPLITUDE) as i16);
        }

        // Long enough that 0.333 samples per bit of slip would accumulate past
        // a whole bit period many times over.
        let message: Vec<u8> = (0..64u16).map(|i| (i as u8).wrapping_mul(7)).collect();
        for &byte in &message {
            emit(&mut osc, &mut pcm, &mut debt, false); // start
            for i in 0..8 {
                emit(&mut osc, &mut pcm, &mut debt, (byte >> i) & 1 != 0);
            }
            emit(&mut osc, &mut pcm, &mut debt, true); // stop
        }
        for _ in 0..200 {
            osc.set_frequency(ORIGINATE_MARK_HZ, SAMPLE_RATE);
            pcm.push((osc.next_sample() * AMPLITUDE) as i16);
        }

        let mut demodulator = FskDemodulator::new_originate();
        demodulator.demodulate(&pcm);

        let mut got = Vec::new();
        while let Some(b) = demodulator.take_byte() {
            got.push(b);
        }
        assert_eq!(
            got, message,
            "demodulator must track a true 300-baud peer across a long message"
        );
    }

    #[test]
    fn test_fsk_demodulator_reset() {
        let mut demod = FskDemodulator::new_originate();
        let mut mod_orig = FskModulator::new_originate();

        // Partially demodulate something
        let mut pcm = Vec::new();
        mod_orig.generate_idle(100, &mut pcm);
        demod.demodulate(&pcm);

        // Reset and verify clean state
        demod.reset();
        assert!(!demod.has_data());
        assert!(demod.take_byte().is_none());
    }

    #[test]
    fn test_fsk_roundtrip_all_byte_values() {
        // Test representative byte values through modulate/demodulate roundtrip
        let test_bytes: Vec<u8> = vec![0x00, 0x01, 0x55, 0x7F, 0x80, 0xAA, 0xFE, 0xFF];

        for &test_byte in &test_bytes {
            let mut modulator = FskModulator::new_originate();
            let mut demodulator = FskDemodulator::new_originate();

            let mut pcm = Vec::new();
            modulator.generate_idle(200, &mut pcm);
            modulator.modulate_byte(test_byte, &mut pcm);
            modulator.generate_idle(100, &mut pcm);

            demodulator.demodulate(&pcm);

            let decoded = demodulator.take_byte();
            assert_eq!(
                decoded,
                Some(test_byte),
                "Roundtrip failed for byte 0x{test_byte:02X}: got {decoded:?}"
            );
        }
    }

    #[test]
    fn test_fsk_has_data_and_take_byte_consistent() {
        let mut demod = FskDemodulator::new_originate();
        // Before any data, has_data should be false
        assert!(!demod.has_data());
        assert!(demod.take_byte().is_none());

        // After modulating and demodulating, they should be consistent
        let mut modulator = FskModulator::new_originate();
        let mut pcm = Vec::new();
        modulator.generate_idle(200, &mut pcm);
        modulator.modulate_byte(0x42, &mut pcm);
        modulator.generate_idle(100, &mut pcm);
        demod.demodulate(&pcm);

        if demod.has_data() {
            assert!(demod.take_byte().is_some());
        }
    }

    #[test]
    fn test_fsk_answer_channel_frequencies() {
        // Verify the answer channel uses higher frequencies (1650/1850 Hz)
        let mut mod_ans = FskModulator::new_answer();
        let mut pcm = Vec::new();
        mod_ans.generate_idle(200, &mut pcm);

        // Mark frequency should be 1650 Hz for answer channel
        let mark_det = Goertzel::new(ANSWER_MARK_HZ, SAMPLE_RATE);
        let space_det = Goertzel::new(ANSWER_SPACE_HZ, SAMPLE_RATE);
        let orig_mark_det = Goertzel::new(ORIGINATE_MARK_HZ, SAMPLE_RATE);

        let mark_energy = mark_det.energy(&pcm);
        let space_energy = space_det.energy(&pcm);
        let orig_energy = orig_mark_det.energy(&pcm);

        assert!(
            mark_energy > space_energy * 10.0,
            "Answer idle should be at 1650 Hz mark"
        );
        assert!(
            mark_energy > orig_energy * 10.0,
            "Answer channel should not activate originate detector"
        );
    }

    #[test]
    fn test_fsk_set_bit_changes_frequency() {
        let mut modulator = FskModulator::new_originate();

        // Generate mark tone
        modulator.set_bit(true);
        let mut mark_pcm = Vec::new();
        for _ in 0..200 {
            mark_pcm.push(modulator.next_sample());
        }

        // Generate space tone
        modulator.set_bit(false);
        let mut space_pcm = Vec::new();
        for _ in 0..200 {
            space_pcm.push(modulator.next_sample());
        }

        let mark_det = Goertzel::new(ORIGINATE_MARK_HZ, SAMPLE_RATE);
        let space_det = Goertzel::new(ORIGINATE_SPACE_HZ, SAMPLE_RATE);

        let mark_in_mark = mark_det.energy(&mark_pcm);
        let space_in_mark = space_det.energy(&mark_pcm);
        assert!(mark_in_mark > space_in_mark * 5.0, "Mark tone mismatch");

        let mark_in_space = mark_det.energy(&space_pcm);
        let space_in_space = space_det.energy(&space_pcm);
        assert!(space_in_space > mark_in_space * 5.0, "Space tone mismatch");
    }
}
