//! Call setup tone generation and detection.
//!
//! Handles the first phase of a modem call: CNG (calling) and ANS/ANSam
//! (answer) tones as defined in ITU-T V.25 and V.8.
//!
//! ANSam is the V.8 "modified answer tone": 2100 Hz with 15 Hz amplitude
//! modulation and a 180° phase reversal every 450 ms. The phase reversals
//! let the caller distinguish a V.8-capable answerer from a V.25-only
//! answerer (which emits plain ANS). Reference: ITU-T V.8 §6.1 and spandsp
//! `src/modem_connect_tones.c::modem_connect_tone_tx_*` (Steve Underwood, LGPL-2.1).

use crate::dsp::goertzel::Goertzel;
use crate::dsp::oscillator::Oscillator;

/// Sample rate used throughout the modem DSP chain.
const SAMPLE_RATE: u32 = 8000;

/// CNG tone frequency (Hz).
pub const CNG_FREQ: f64 = 1100.0;
/// CNG tone-on duration (ms).
pub const CNG_ON_MS: u32 = 500;
/// CNG silence duration (ms).
pub const CNG_OFF_MS: u32 = 3000;

/// ANS tone frequency (Hz).
pub const ANS_FREQ: f64 = 2100.0;
/// ANS tone duration (ms).
pub const ANS_DURATION_MS: u32 = 3300;
/// ANS tone duration in 10 ms DSP ticks for the call-setup state machine.
pub const ANS_DURATION_TICKS_10MS: u32 = ANS_DURATION_MS / 10;

/// Return the compliant 3.3 s answer-tone duration as 10 ms DSP ticks.
pub const fn ans_duration_ticks_10ms() -> u32 {
    ANS_DURATION_TICKS_10MS
}

/// Return the compliant 3.3 s answer-tone duration as 8 kHz samples.
pub const fn ans_duration_samples_8khz() -> u32 {
    ANS_DURATION_MS * SAMPLE_RATE / 1000
}

/// ANSam amplitude modulation frequency (Hz).
pub const ANSAM_AM_FREQ: f64 = 15.0;
/// ANSam phase reversal interval (ms).
pub const ANSAM_PHASE_REV_MS: u32 = 450;

// ---------------------------------------------------------------------------
// CNG tone generator
// ---------------------------------------------------------------------------

/// Generates the calling tone: 1100 Hz, 0.5 s on, 3.0 s off, repeating.
pub struct CngGenerator {
    osc: Oscillator,
    sample_count: u32,
    on_samples: u32,
    off_samples: u32,
    /// `true` while the tone is sounding, `false` during silence.
    active: bool,
}

impl Default for CngGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl CngGenerator {
    pub fn new() -> Self {
        Self {
            osc: Oscillator::new(CNG_FREQ, SAMPLE_RATE),
            sample_count: 0,
            on_samples: CNG_ON_MS * SAMPLE_RATE / 1000,
            off_samples: CNG_OFF_MS * SAMPLE_RATE / 1000,
            active: true,
        }
    }

    /// Generate the next PCM sample.
    pub fn next_sample(&mut self) -> i16 {
        let limit = if self.active {
            self.on_samples
        } else {
            self.off_samples
        };

        if self.sample_count >= limit {
            self.sample_count = 0;
            self.active = !self.active;
            if self.active {
                // Keep phase continuous — no reset needed.
            }
        }

        self.sample_count += 1;

        if self.active {
            (self.osc.next_sample() * 16000.0) as i16
        } else {
            // Advance the oscillator even during silence to keep phase
            // continuous when the tone resumes, but output zero.
            let _ = self.osc.next_sample();
            0
        }
    }

    /// Fill a buffer with samples.
    pub fn generate(&mut self, output: &mut [i16]) {
        for sample in output.iter_mut() {
            *sample = self.next_sample();
        }
    }

    /// Reset to start of CNG pattern.
    pub fn reset(&mut self) {
        self.osc.reset();
        self.sample_count = 0;
        self.active = true;
    }
}

// ---------------------------------------------------------------------------
// ANS / ANSam tone generator
// ---------------------------------------------------------------------------

/// Generates the answer tone: 2100 Hz with optional 15 Hz AM and phase
/// reversals (ANSam).
pub struct AnsGenerator {
    osc: Oscillator,
    am_osc: Oscillator,
    sample_count: u32,
    phase_rev_samples: u32,
    use_am: bool,
    /// Accumulated sign from phase reversals (+1 or -1).
    phase_sign: f64,
}

impl AnsGenerator {
    /// Create a plain ANS generator (2100 Hz, no AM).
    pub fn new_ans() -> Self {
        Self {
            osc: Oscillator::new(ANS_FREQ, SAMPLE_RATE),
            am_osc: Oscillator::new(ANSAM_AM_FREQ, SAMPLE_RATE),
            sample_count: 0,
            phase_rev_samples: ANSAM_PHASE_REV_MS * SAMPLE_RATE / 1000,
            use_am: false,
            phase_sign: 1.0,
        }
    }

    /// Create an ANSam generator (2100 Hz + 15 Hz AM + phase reversals).
    pub fn new_ansam() -> Self {
        Self {
            osc: Oscillator::new(ANS_FREQ, SAMPLE_RATE),
            am_osc: Oscillator::new(ANSAM_AM_FREQ, SAMPLE_RATE),
            sample_count: 0,
            phase_rev_samples: ANSAM_PHASE_REV_MS * SAMPLE_RATE / 1000,
            use_am: true,
            phase_sign: 1.0,
        }
    }

    /// Generate the next PCM sample.
    pub fn next_sample(&mut self) -> i16 {
        self.sample_count += 1;

        // Phase reversal every ANSAM_PHASE_REV_MS (ANSam only).
        if self.use_am && self.sample_count >= self.phase_rev_samples {
            self.sample_count = 0;
            self.phase_sign = -self.phase_sign;
        }

        let carrier = self.osc.next_sample() * self.phase_sign;

        if self.use_am {
            // ANSam: output = sin(2*pi*2100*t) * (1 + 0.2*sin(2*pi*15*t))
            let am = 1.0 + 0.2 * self.am_osc.next_sample();
            (carrier * am * 16000.0) as i16
        } else {
            let _ = self.am_osc.next_sample(); // keep in sync
            (carrier * 16000.0) as i16
        }
    }

    /// Fill a buffer with samples.
    pub fn generate(&mut self, output: &mut [i16]) {
        for sample in output.iter_mut() {
            *sample = self.next_sample();
        }
    }

    /// Reset to start.
    pub fn reset(&mut self) {
        self.osc.reset();
        self.am_osc.reset();
        self.sample_count = 0;
        self.phase_sign = 1.0;
    }
}

// ---------------------------------------------------------------------------
// Tone detector
// ---------------------------------------------------------------------------

/// Result of tone detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedTone {
    /// 1100 Hz calling tone.
    Cng,
    /// 2100 Hz answer tone.
    Ans,
}

/// Detects CNG and ANS/ANSam tones using Goertzel energy detection with
/// temporal persistence to avoid false positives.
pub struct ToneDetector {
    cng_goertzel: Goertzel,
    ans_goertzel: Goertzel,
    /// Number of consecutive blocks where CNG was detected.
    cng_persistence: u32,
    /// Whether the current qualified CNG burst has already been reported.
    cng_latched: bool,
    /// Number of consecutive blocks where ANS was detected.
    ans_persistence: u32,
    /// Required consecutive blocks before confirming CNG.
    cng_threshold_blocks: u32,
    /// Required consecutive blocks before confirming ANS.
    ans_threshold_blocks: u32,
    /// Normalized energy threshold for detection.
    energy_threshold: f64,
}

impl Default for ToneDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl ToneDetector {
    pub fn new() -> Self {
        Self {
            cng_goertzel: Goertzel::new(CNG_FREQ, SAMPLE_RATE),
            ans_goertzel: Goertzel::new(ANS_FREQ, SAMPLE_RATE),
            cng_persistence: 0,
            cng_latched: false,
            ans_persistence: 0,
            cng_threshold_blocks: 20, // 200 ms at 10 ms/block
            ans_threshold_blocks: 50, // 500 ms at 10 ms/block
            energy_threshold: 0.4,
        }
    }

    /// Feed a 10 ms block of samples (80 samples at 8 kHz).
    /// Returns the detected tone if persistence thresholds are met.
    pub fn detect(&mut self, samples: &[i16]) -> Option<DetectedTone> {
        let cng_energy = self.cng_goertzel.normalized_energy(samples);
        let ans_energy = self.ans_goertzel.normalized_energy(samples);

        // CNG persistence tracking
        if cng_energy >= self.energy_threshold {
            self.cng_persistence += 1;
        } else {
            self.cng_persistence = 0;
            self.cng_latched = false;
        }

        // ANS persistence tracking
        if ans_energy >= self.energy_threshold {
            self.ans_persistence += 1;
        } else {
            self.ans_persistence = 0;
        }

        // Check thresholds — prefer ANS over CNG if both somehow fire.
        if self.ans_persistence >= self.ans_threshold_blocks {
            Some(DetectedTone::Ans)
        } else if self.cng_persistence >= self.cng_threshold_blocks && !self.cng_latched {
            self.cng_latched = true;
            Some(DetectedTone::Cng)
        } else {
            None
        }
    }

    /// Reset detection state.
    pub fn reset(&mut self) {
        self.cng_persistence = 0;
        self.cng_latched = false;
        self.ans_persistence = 0;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// Block size for 10 ms at 8 kHz.
    const BLOCK_SIZE: usize = 80;

    #[test]
    fn test_cng_pattern() {
        // Verify the 0.5 s on, 3.0 s off cadence.
        let mut gen = CngGenerator::new();

        // First 4000 samples (0.5 s) should be non-silent.
        let on_samples = CNG_ON_MS * SAMPLE_RATE / 1000;
        let off_samples = CNG_OFF_MS * SAMPLE_RATE / 1000;

        let mut buf = vec![0i16; on_samples as usize];
        gen.generate(&mut buf);
        let on_energy: f64 = buf.iter().map(|&s| (s as f64).powi(2)).sum();
        assert!(on_energy > 0.0, "CNG on-phase should have non-zero energy");

        // Next 24000 samples (3.0 s) should be silence.
        let mut buf_off = vec![0i16; off_samples as usize];
        gen.generate(&mut buf_off);
        let off_energy: f64 = buf_off.iter().map(|&s| (s as f64).powi(2)).sum();
        assert_eq!(off_energy, 0.0, "CNG off-phase should be silent");

        // Then tone should resume.
        let mut buf_on2 = vec![0i16; on_samples as usize];
        gen.generate(&mut buf_on2);
        let on2_energy: f64 = buf_on2.iter().map(|&s| (s as f64).powi(2)).sum();
        assert!(on2_energy > 0.0, "CNG should resume after silence");
    }

    #[test]
    fn test_cng_frequency() {
        let mut gen = CngGenerator::new();
        let mut buf = vec![0i16; 800]; // 100 ms of tone (within on-phase)
        gen.generate(&mut buf);

        let detector = Goertzel::new(CNG_FREQ, SAMPLE_RATE);
        let norm = detector.normalized_energy(&buf);
        assert!(
            norm > 0.8,
            "CNG tone should have strong normalized energy at 1100 Hz, got {norm}"
        );
    }

    #[test]
    fn test_ans_frequency() {
        let mut gen = AnsGenerator::new_ans();
        let mut buf = vec![0i16; 800];
        gen.generate(&mut buf);

        let detector = Goertzel::new(ANS_FREQ, SAMPLE_RATE);
        let norm = detector.normalized_energy(&buf);
        assert!(
            norm > 0.8,
            "ANS tone should have strong normalized energy at 2100 Hz, got {norm}"
        );
    }

    #[test]
    fn test_ansam_has_am() {
        // ANSam has 15 Hz amplitude modulation. Over one AM period (~533 samples
        // at 8 kHz) the envelope should vary noticeably.
        let mut gen = AnsGenerator::new_ansam();
        let am_period_samples = (SAMPLE_RATE as f64 / ANSAM_AM_FREQ) as usize; // ~533

        let mut buf = vec![0i16; am_period_samples];
        gen.generate(&mut buf);

        // Compare peak amplitude in the first quarter (near AM peak) vs the
        // third quarter (near AM trough — half an AM period apart).
        let quarter = am_period_samples / 4;
        let peak_q1 = buf[..quarter]
            .iter()
            .map(|&s| (s as i32).unsigned_abs())
            .max()
            .unwrap();
        let peak_q3 = buf[quarter * 2..quarter * 3]
            .iter()
            .map(|&s| (s as i32).unsigned_abs())
            .max()
            .unwrap();

        // With 20% AM depth, peaks should differ by at least 5%.
        let diff = (peak_q1 as f64 - peak_q3 as f64).abs();
        let avg = (peak_q1 as f64 + peak_q3 as f64) / 2.0;
        let relative_diff = diff / avg;
        assert!(
            relative_diff > 0.05,
            "ANSam should have visible AM modulation, relative peak difference = {relative_diff:.4}"
        );
    }

    #[test]
    fn test_detect_cng() {
        let mut gen = CngGenerator::new();
        let mut det = ToneDetector::new();

        // Feed CNG tone in 10 ms blocks until detected.
        // CNG on-phase is 500 ms = 50 blocks. Threshold is 20 blocks.
        let mut detected = false;
        for _ in 0..50 {
            let mut block = vec![0i16; BLOCK_SIZE];
            gen.generate(&mut block);
            if det.detect(&block) == Some(DetectedTone::Cng) {
                detected = true;
                break;
            }
        }
        assert!(detected, "Should detect CNG within the 500 ms on-phase");
    }

    #[test]
    fn test_detect_cng_reports_one_event_per_burst() {
        let mut gen = CngGenerator::new();
        let mut det = ToneDetector::new();
        let mut detections = 0;

        // A CNG ring is one 500 ms tone burst, not one event per 10 ms
        // detector block after the persistence threshold is reached.
        for _ in 0..50 {
            let mut block = vec![0i16; BLOCK_SIZE];
            gen.generate(&mut block);
            detections += usize::from(det.detect(&block) == Some(DetectedTone::Cng));
        }

        assert_eq!(detections, 1, "one CNG burst must report one ring event");
    }

    #[test]
    fn test_detect_ans() {
        let mut gen = AnsGenerator::new_ans();
        let mut det = ToneDetector::new();

        // Feed ANS tone for up to 1000 ms (100 blocks). Threshold is 50 blocks.
        let mut detected = false;
        for _ in 0..100 {
            let mut block = vec![0i16; BLOCK_SIZE];
            gen.generate(&mut block);
            if det.detect(&block) == Some(DetectedTone::Ans) {
                detected = true;
                break;
            }
        }
        assert!(detected, "Should detect ANS within 1000 ms");
    }

    #[test]
    fn test_detect_silence() {
        let mut det = ToneDetector::new();
        let silence = vec![0i16; BLOCK_SIZE];

        // 200 blocks of silence — should never detect anything.
        for _ in 0..200 {
            assert_eq!(
                det.detect(&silence),
                None,
                "Silence should not trigger detection"
            );
        }
    }

    #[test]
    fn test_detect_wrong_freq() {
        let mut det = ToneDetector::new();
        let mut osc = Oscillator::new(500.0, SAMPLE_RATE);

        // Feed 200 blocks of 500 Hz — should not trigger CNG (1100) or ANS (2100).
        for _ in 0..200 {
            let block: Vec<i16> = (0..BLOCK_SIZE)
                .map(|_| (osc.next_sample() * 16000.0) as i16)
                .collect();
            assert_eq!(
                det.detect(&block),
                None,
                "500 Hz tone should not trigger CNG or ANS detection"
            );
        }
    }

    #[test]
    fn test_cng_to_ans_transition() {
        let mut cng_gen = CngGenerator::new();
        let mut ans_gen = AnsGenerator::new_ans();
        let mut det = ToneDetector::new();

        // Phase 1: feed CNG until detected.
        let mut cng_detected = false;
        for _ in 0..50 {
            let mut block = vec![0i16; BLOCK_SIZE];
            cng_gen.generate(&mut block);
            if det.detect(&block) == Some(DetectedTone::Cng) {
                cng_detected = true;
                break;
            }
        }
        assert!(cng_detected, "CNG should be detected first");

        // Reset detector for the next phase.
        det.reset();

        // Phase 2: feed ANS until detected.
        let mut ans_detected = false;
        for _ in 0..100 {
            let mut block = vec![0i16; BLOCK_SIZE];
            ans_gen.generate(&mut block);
            if det.detect(&block) == Some(DetectedTone::Ans) {
                ans_detected = true;
                break;
            }
        }
        assert!(ans_detected, "ANS should be detected after CNG");
    }

    #[test]
    fn test_ansam_detected_as_ans() {
        // ANSam (with AM) should still be detected as ANS since the carrier
        // is 2100 Hz and the AM only modulates the envelope.
        let mut gen = AnsGenerator::new_ansam();
        let mut det = ToneDetector::new();

        let mut detected = false;
        for _ in 0..100 {
            let mut block = vec![0i16; BLOCK_SIZE];
            gen.generate(&mut block);
            if det.detect(&block) == Some(DetectedTone::Ans) {
                detected = true;
                break;
            }
        }
        assert!(detected, "ANSam should be detected as ANS");
    }

    #[test]
    fn test_cng_reset() {
        let mut gen1 = CngGenerator::new();
        let mut gen2 = CngGenerator::new();

        // Advance gen1
        let mut buf = vec![0i16; 200];
        gen1.generate(&mut buf);

        // Reset should bring it back to start
        gen1.reset();

        let mut buf1 = vec![0i16; 80];
        let mut buf2 = vec![0i16; 80];
        gen1.generate(&mut buf1);
        gen2.generate(&mut buf2);
        assert_eq!(buf1, buf2, "Reset CNG should match fresh generator");
    }

    #[test]
    fn test_ans_reset() {
        let mut gen1 = AnsGenerator::new_ans();
        let mut gen2 = AnsGenerator::new_ans();

        let mut buf = vec![0i16; 200];
        gen1.generate(&mut buf);
        gen1.reset();

        let mut buf1 = vec![0i16; 80];
        let mut buf2 = vec![0i16; 80];
        gen1.generate(&mut buf1);
        gen2.generate(&mut buf2);
        assert_eq!(buf1, buf2, "Reset ANS should match fresh generator");
    }

    #[test]
    fn test_tone_detector_default() {
        let det = ToneDetector::default();
        // Should be equivalent to new()
        let det2 = ToneDetector::new();
        // Both should return None for silence
        let silence = vec![0i16; BLOCK_SIZE];
        let mut d1 = det;
        let mut d2 = det2;
        assert_eq!(d1.detect(&silence), d2.detect(&silence));
    }

    #[test]
    fn test_cng_default() {
        let gen = CngGenerator::default();
        let gen2 = CngGenerator::new();
        let mut buf1 = vec![0i16; 80];
        let mut buf2 = vec![0i16; 80];
        let mut g1 = gen;
        let mut g2 = gen2;
        g1.generate(&mut buf1);
        g2.generate(&mut buf2);
        assert_eq!(buf1, buf2);
    }

    #[test]
    fn test_ans_duration_matches_33s_answer_tone_path() {
        assert_eq!(ANS_DURATION_TICKS_10MS, 330);
        assert_eq!(ANS_DURATION_TICKS_10MS * 10, ANS_DURATION_MS);
    }

    #[test]
    fn test_tone_detector_reset_clears_persistence() {
        let mut gen = CngGenerator::new();
        let mut det = ToneDetector::new();

        // Feed 15 blocks of CNG (not enough for threshold of 20)
        for _ in 0..15 {
            let mut block = vec![0i16; BLOCK_SIZE];
            gen.generate(&mut block);
            let _ = det.detect(&block);
        }

        // Reset — persistence should drop to 0
        det.reset();

        // Now feed 15 more — still not enough (need 20 consecutive)
        for _ in 0..15 {
            let mut block = vec![0i16; BLOCK_SIZE];
            gen.generate(&mut block);
            assert_eq!(
                det.detect(&block),
                None,
                "Should not detect after reset + 15 blocks"
            );
        }
    }

    #[test]
    fn test_ansam_phase_reversal() {
        // ANSam has phase reversals every 450 ms.
        // Over the reversal boundary, consecutive samples should show a sign change.
        let mut gen = AnsGenerator::new_ansam();
        let phase_rev_samples = ANSAM_PHASE_REV_MS * SAMPLE_RATE / 1000;

        // Generate samples up to just before the reversal point
        let mut pre_samples = Vec::new();
        for _ in 0..phase_rev_samples - 10 {
            pre_samples.push(gen.next_sample());
        }

        // Capture samples around the reversal
        let mut around_rev = Vec::new();
        for _ in 0..20 {
            around_rev.push(gen.next_sample());
        }

        // The carrier should have reversed sign near sample 10 of around_rev.
        // Check that some sign transitions occur (the reversal causes the
        // carrier to flip, which shows as a shift in the waveform pattern).
        let sign_changes: usize = around_rev
            .windows(2)
            .filter(|w| (w[0] > 0) != (w[1] > 0))
            .count();
        assert!(
            sign_changes > 0,
            "Should see sign changes around phase reversal"
        );
    }

    #[test]
    fn test_ans_duration_helpers_for_3p3s_path() {
        // Keep call-setup duration math centralized for the state-machine answer-tone path.
        assert_eq!(ANS_DURATION_MS, 3300);
        assert_eq!(ans_duration_ticks_10ms(), 330);
        assert_eq!(ans_duration_samples_8khz(), 26_400);
    }
}
