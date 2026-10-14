//! V.32bis training sequence state machine.
//!
//! After V.8 selects V.32bis, the training sequence proceeds:
//!
//! 1. **Answerer** sends AA pattern (alternating 4-QAM symbols) at 2400 baud
//!    on 1800 Hz carrier for ~200ms (480 symbols).
//! 2. **Caller** detects AA (carrier energy at 1800 Hz), sends AC pattern
//!    (different alternating symbols) for ~200ms.
//! 3. **Answerer** detects AC (caller's carrier), sends S and S-bar
//!    conditioning on the 1800 Hz carrier, then TRN (scrambled symbols)
//!    for equalizer training ~500ms.
//! 4. **Caller** sends its own S and S-bar conditioning, then TRN.
//! 5. **Rate signal exchange**: half-duplex, answerer transmits first while
//!    caller listens, then they swap. Differential 4-QAM encoding with
//!    constant step per speed for rotation invariance and alignment freedom.
//! 6. Both enter data mode at the agreed rate.
//!
//! Both sides use the same 1800 Hz carrier frequency, so rate signals must
//! be transmitted half-duplex to avoid interference.
//!
//! ## Rate Signal Encoding
//!
//! QAM demodulators commonly exhibit a fixed phase rotation that maps all
//! constellation points consistently. We use **differential encoding**: the
//! speed is encoded as a constant *step* between consecutive symbols
//! modulo 4, which is invariant under any fixed rotation and requires no
//! block alignment:
//!
//! - 4800:  step 0 (same symbol repeated)
//! - 7200:  step 1 (rotate +90 degrees each symbol)
//! - 9600:  step 2 (rotate +180 degrees each symbol)
//! - 12000: step 3 (rotate +270 degrees each symbol)
//! - 14400: alternating steps 1, 3 (unique pattern distinguishable from
//!   constant-step speeds by checking vote distribution)

use crate::qam::{Constellation, QamDemodulator, QamModulator};
use crate::scrambler::Scrambler;

/// 10 ms tick = 80 samples at 8000 Hz.
const TICK_SAMPLES: usize = 80;

/// Symbols per tick at 2400 baud, 10 ms tick: 2400 * 0.010 = 24.
const SYMBOLS_PER_TICK: u32 = 24;

/// AA/AC pattern duration: ~200 ms = 480 symbols = 20 ticks.
const AA_AC_SYMBOLS: u32 = 480;

/// TRN duration: ~500 ms = 1200 symbols = 50 ticks.
const TRN_SYMBOLS: u32 = 1200;

/// Duration of each half of the rate signal exchange (ticks).
/// Answerer transmits for this many ticks, then caller transmits for this many.
const RATE_HALF_TICKS: u32 = 15;

/// Ticks to skip at the start of each rate signal half before collecting
/// symbols, allowing the demodulator to adapt to the new pattern.
const RATE_GUARD_TICKS: u32 = 3;

/// Duration of S conditioning phase before TRN: 64 symbols ≈ 27 ms at 2400 baud.
///
/// V.32bis §7.8: S is a periodic alternating pattern on the 1800 Hz carrier
/// that initialises the receiver's timing recovery before the scrambled TRN.
const S_SYMBOLS: u32 = 64;

/// Duration of S-bar conditioning phase before TRN: 64 symbols ≈ 27 ms.
///
/// S-bar is the phase-reversed S pattern, signalling the transition to TRN.
const S_BAR_SYMBOLS: u32 = 64;

/// Ticks required for the S conditioning phase (ceiling division).
///
/// 64 symbols / 24 symbols/tick = 2.67 → 3 ticks (72 symbols sent; the
/// 8-symbol overshoot is within the ±1 tick receiver tolerance of V.32bis).
#[cfg(test)]
const S_TICKS: u32 = S_SYMBOLS.div_ceil(SYMBOLS_PER_TICK);

/// Ticks required for the S-bar conditioning phase (ceiling division).
///
/// Same arithmetic as S_TICKS: 64 / 24 → 3 ticks.
#[cfg(test)]
const S_BAR_TICKS: u32 = S_BAR_SYMBOLS.div_ceil(SYMBOLS_PER_TICK);

/// Ticks required for the scrambled TRN main sequence.
///
/// 1200 symbols / 24 symbols/tick = 50 ticks (exact).
#[cfg(test)]
const TRN_MAIN_TICKS: u32 = TRN_SYMBOLS / SYMBOLS_PER_TICK;

/// Total ticks for the full TRN phase: S conditioning + S-bar + scrambled TRN.
///
/// 3 + 3 + 50 = 56 ticks = 560 ms at the 10 ms tick rate.
#[cfg(test)]
const TRN_TOTAL_TICKS: u32 = S_TICKS + S_BAR_TICKS + TRN_MAIN_TICKS;

/// Number of consecutive ticks with carrier energy above threshold needed
/// for carrier detection.
const CARRIER_DETECT_TICKS: u32 = 3;

/// RMS threshold for carrier detection. On a clean channel, modulated QAM
/// symbols produce I/Q magnitudes around 0.3-1.0 after demodulator AGC
/// convergence. We set the threshold low enough that even partially-converged
/// signals are detected.
const CARRIER_RMS_THRESHOLD: f64 = 0.05;

/// Timeout in ticks (5 seconds = 500 ticks at 10 ms each).
const TIMEOUT_TICKS: u32 = 500;

/// Training role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrainingRole {
    Caller,
    Answerer,
}

/// Internal sub-phase within `SendingTRN`.
///
/// Keeps the public `TrainingState` surface stable while adding S/S-bar
/// conditioning before the scrambled TRN sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrnSubPhase {
    /// Sending S conditioning symbols (alternating 0/2, 1800 Hz carrier).
    S,
    /// Sending S-bar conditioning symbols (phase-reversed: alternating 2/0).
    SBar,
    /// Sending scrambled TRN symbols.
    Main,
}

/// Training state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrainingState {
    /// Answerer sending AA, caller waiting to detect carrier.
    SendingAA,
    /// Caller detected carrier, both exchanging AC.
    SendingAC,
    /// Sending TRN (scrambled training sequence).
    SendingTRN,
    /// Sending rate signal.
    SendingRateSignal,
    /// Training complete, ready for data mode.
    Complete,
    /// Failed (timeout).
    Failed,
}

/// V.32bis training negotiator.
pub struct Training {
    role: TrainingRole,
    state: TrainingState,
    modulator: QamModulator,
    demodulator: QamDemodulator,
    tick_count: u32,
    symbol_count: u32,
    /// Negotiated speed in bps.
    negotiated_speed: Option<u32>,
    /// Our maximum speed.
    our_max_speed: u32,
    /// Carrier detected (for pattern recognition).
    carrier_detected: bool,
    /// Energy detector for carrier presence.
    #[allow(dead_code)]
    carrier_energy_accum: f64,
    carrier_energy_count: u32,
    /// Scrambler for TRN generation.
    scrambler: Scrambler,
    /// Accumulated hard decisions from demodulator during rate signal phase.
    rate_symbols: Vec<u8>,
    /// Ticks spent in the SendingRateSignal state.
    rate_ticks: u32,
    /// Last symbol index transmitted in the rate signal.
    rate_tx_last_sym: u8,
    /// Counter for alternating step pattern (14400 encoding).
    rate_tx_step_toggle: bool,
    /// Sub-phase within `SendingTRN` (S → S-bar → scrambled TRN).
    trn_sub: TrnSubPhase,
    /// Symbol counter for the S and S-bar sub-phases.
    s_sym_count: u32,
}

impl Training {
    /// Create a new V.32bis training negotiator.
    ///
    /// - `role`: Caller or Answerer
    /// - `max_speed`: our maximum data rate in bps (4800, 7200, 9600, 12000, or 14400)
    pub fn new(role: TrainingRole, max_speed: u32) -> Self {
        Self {
            role,
            state: TrainingState::SendingAA,
            modulator: QamModulator::new(Constellation::Qam4),
            demodulator: QamDemodulator::new(Constellation::Qam4),
            tick_count: 0,
            symbol_count: 0,
            negotiated_speed: None,
            our_max_speed: max_speed,
            carrier_detected: false,
            carrier_energy_accum: 0.0,
            carrier_energy_count: 0,
            scrambler: Scrambler::new(),
            rate_symbols: Vec::new(),
            rate_ticks: 0,
            rate_tx_last_sym: 0,
            rate_tx_step_toggle: false,
            trn_sub: TrnSubPhase::S,
            s_sym_count: 0,
        }
    }

    /// Process one 10 ms tick of training.
    /// Returns current state.
    pub fn tick(
        &mut self,
        line_in: &[i16; TICK_SAMPLES],
        line_out: &mut [i16; TICK_SAMPLES],
    ) -> TrainingState {
        // Terminal states produce silence.
        if self.state == TrainingState::Complete || self.state == TrainingState::Failed {
            line_out.fill(0);
            return self.state;
        }

        self.tick_count += 1;

        // Check timeout.
        if self.tick_count >= TIMEOUT_TICKS {
            self.state = TrainingState::Failed;
            line_out.fill(0);
            return self.state;
        }

        // Demodulate incoming audio and check for carrier/rate signals.
        let demod_symbols = self.demodulator.demodulate(line_in);
        self.update_carrier_detection(&demod_symbols);

        // Run role-specific state machine.
        match self.role {
            TrainingRole::Caller => self.tick_caller(&demod_symbols),
            TrainingRole::Answerer => self.tick_answerer(&demod_symbols),
        }

        // Generate output audio from the modulator.
        for sample in line_out.iter_mut() {
            *sample = self.modulator.next_sample();
        }

        self.state
    }

    /// Negotiated speed (available when state == Complete).
    pub fn negotiated_speed(&self) -> Option<u32> {
        self.negotiated_speed
    }

    /// The maximum speed this training was configured for (from V.8 negotiation).
    /// This is the authoritative speed agreed by both sides during V.8.
    pub fn configured_max_speed(&self) -> u32 {
        self.our_max_speed
    }

    /// Current state.
    pub fn state(&self) -> TrainingState {
        self.state
    }

    /// Tick counter (10 ms per tick).  Read-only — exposed so failure-path
    /// WARN logging in `state_machine.rs` can record how many ticks the
    /// trainer consumed before timing out.
    pub fn tick_count(&self) -> u32 {
        self.tick_count
    }

    /// Tick budget after which `tick()` forces a `Failed` transition.
    /// Read-only — paired with `tick_count()` for failure-path diagnostics.
    pub fn timeout_ticks(&self) -> u32 {
        TIMEOUT_TICKS
    }

    /// Role this trainer is playing (Caller or Answerer).  Read-only —
    /// paired with the failure-path WARN logging.
    pub fn role(&self) -> TrainingRole {
        self.role
    }

    /// Whether the trainer has currently observed enough received-symbol
    /// energy to consider the peer's carrier "present".  Used by the
    /// failure-path WARN logging to distinguish "never heard the peer"
    /// from "heard the peer, but couldn't sync to its TRN sequence".
    pub fn carrier_detected(&self) -> bool {
        self.carrier_detected
    }

    /// Get the configured modulator and demodulator for data mode
    /// (transfers ownership -- call only after Complete).
    pub fn into_data_mode(self) -> (QamModulator, QamDemodulator) {
        (self.modulator, self.demodulator)
    }

    // -----------------------------------------------------------------------
    // Caller state machine
    // -----------------------------------------------------------------------

    fn tick_caller(&mut self, demod_symbols: &[(f64, f64)]) {
        match self.state {
            TrainingState::SendingAA => {
                // Caller: listen for answerer's AA (carrier energy above threshold).
                // Don't transmit meaningful symbols.

                if self.carrier_detected {
                    self.state = TrainingState::SendingAC;
                    self.symbol_count = 0;
                }
            }
            TrainingState::SendingAC => {
                // Caller: generate alternating 4-QAM symbols.
                self.queue_alternating_symbols();

                if self.symbol_count >= AA_AC_SYMBOLS {
                    self.enter_trn();
                }
            }
            TrainingState::SendingTRN => {
                // Caller: send S, S-bar, then scrambled TRN.
                if self.queue_trn_symbols() {
                    self.enter_rate_signal();
                }
            }
            TrainingState::SendingRateSignal => {
                self.rate_ticks += 1;
                // Half-duplex protocol:
                //   Ticks 1..RATE_HALF_TICKS: answerer sends, caller listens (silence).
                //   Ticks RATE_HALF_TICKS+1..2*RATE_HALF_TICKS: caller sends.
                //   After 2*RATE_HALF_TICKS: decode and complete.

                if self.rate_ticks <= RATE_HALF_TICKS {
                    // First half: caller is silent, collecting answerer's rate signal.
                    if self.rate_ticks > RATE_GUARD_TICKS {
                        self.collect_demod_symbols(demod_symbols);
                    }
                } else if self.rate_ticks <= 2 * RATE_HALF_TICKS {
                    // Second half: caller transmits its rate signal.
                    self.queue_rate_symbols();
                } else {
                    // Done: decode and complete.
                    self.decode_and_complete();
                }
            }
            TrainingState::Complete | TrainingState::Failed => {}
        }
    }

    // -----------------------------------------------------------------------
    // Answerer state machine
    // -----------------------------------------------------------------------

    fn tick_answerer(&mut self, demod_symbols: &[(f64, f64)]) {
        match self.state {
            TrainingState::SendingAA => {
                // Answerer: generate alternating 4-QAM symbols (AA pattern).
                self.queue_alternating_symbols();

                if self.symbol_count >= AA_AC_SYMBOLS {
                    self.state = TrainingState::SendingAC;
                    self.symbol_count = 0;
                    // Reset carrier detection for detecting caller's AC.
                    self.carrier_detected = false;
                    self.carrier_energy_count = 0;
                }
            }
            TrainingState::SendingAC => {
                // Answerer: continue sending alternating symbols while looking
                // for caller's carrier (AC signal).
                self.queue_alternating_symbols();

                if self.carrier_detected {
                    self.enter_trn();
                }
            }
            TrainingState::SendingTRN => {
                // Answerer: send S, S-bar, then scrambled TRN.
                if self.queue_trn_symbols() {
                    self.enter_rate_signal();
                }
            }
            TrainingState::SendingRateSignal => {
                self.rate_ticks += 1;
                // Half-duplex protocol:
                //   Ticks 1..RATE_HALF_TICKS: answerer sends its rate signal.
                //   Ticks RATE_HALF_TICKS+1..2*RATE_HALF_TICKS: answerer listens.
                //   After 2*RATE_HALF_TICKS: decode and complete.

                if self.rate_ticks <= RATE_HALF_TICKS {
                    // First half: answerer transmits its rate signal.
                    self.queue_rate_symbols();
                } else if self.rate_ticks <= 2 * RATE_HALF_TICKS {
                    // Second half: answerer is silent, collecting caller's rate signal.
                    if self.rate_ticks > RATE_HALF_TICKS + RATE_GUARD_TICKS {
                        self.collect_demod_symbols(demod_symbols);
                    }
                } else {
                    // Done: decode and complete.
                    self.decode_and_complete();
                }
            }
            TrainingState::Complete | TrainingState::Failed => {}
        }
    }

    // -----------------------------------------------------------------------
    // State transitions
    // -----------------------------------------------------------------------

    /// Transition to the rate signal phase.
    fn enter_rate_signal(&mut self) {
        self.state = TrainingState::SendingRateSignal;
        self.symbol_count = 0;
        self.s_sym_count = 0;
        self.rate_symbols.clear();
        self.rate_ticks = 0;
        self.rate_tx_last_sym = 0;
        self.rate_tx_step_toggle = false;
        // Freeze PLL during rate signal exchange. The carrier phase is already
        // locked from TRN; continued PLL tracking at symbol-rate bandwidth
        // chases data-induced phase transitions (especially the 14400 alternating
        // 90° pattern), corrupting hard decisions.
        self.demodulator.set_pll_tracking(false);
    }

    /// Transition into the S -> S-bar -> TRN sequence.
    fn enter_trn(&mut self) {
        self.state = TrainingState::SendingTRN;
        self.symbol_count = 0;
        self.trn_sub = TrnSubPhase::S;
        self.s_sym_count = 0;
        self.scrambler.reset();
    }

    // -----------------------------------------------------------------------
    // Symbol generation helpers
    // -----------------------------------------------------------------------

    /// Queue alternating 4-QAM symbols: point 0, point 2, point 0, point 2...
    fn queue_alternating_symbols(&mut self) {
        for i in 0..SYMBOLS_PER_TICK {
            let point = if (self.symbol_count + i).is_multiple_of(2) {
                0
            } else {
                2
            };
            self.modulator.modulate_symbol_raw(point);
        }
        self.symbol_count += SYMBOLS_PER_TICK;
    }

    /// Queue one conditioning tick for S or S-bar on the 1800 Hz carrier.
    fn queue_conditioning_symbols(&mut self, start_symbol: u8) {
        for i in 0..SYMBOLS_PER_TICK {
            let phase = (self.s_sym_count + i).is_multiple_of(2);
            let point = if phase {
                start_symbol
            } else {
                (start_symbol + 2) % 4
            };
            self.modulator.modulate_symbol_raw(point);
        }
        self.s_sym_count += SYMBOLS_PER_TICK;
    }

    /// Queue the current TRN sub-phase.
    ///
    /// Returns `true` once the scrambled TRN has completed and the caller or
    /// answerer can advance to the rate-signal phase.
    fn queue_trn_symbols(&mut self) -> bool {
        match self.trn_sub {
            TrnSubPhase::S => {
                self.queue_conditioning_symbols(0);
                if self.s_sym_count >= S_SYMBOLS {
                    self.trn_sub = TrnSubPhase::SBar;
                    self.s_sym_count = 0;
                }
                false
            }
            TrnSubPhase::SBar => {
                self.queue_conditioning_symbols(2);
                if self.s_sym_count >= S_BAR_SYMBOLS {
                    self.trn_sub = TrnSubPhase::Main;
                    self.s_sym_count = 0;
                }
                false
            }
            TrnSubPhase::Main => {
                self.queue_scrambled_symbols();
                self.symbol_count >= TRN_SYMBOLS
            }
        }
    }

    /// Queue scrambled 4-QAM symbols for TRN.
    fn queue_scrambled_symbols(&mut self) {
        for _ in 0..SYMBOLS_PER_TICK {
            // Scramble a constant input (all 1s) to produce pseudo-random bits.
            let scrambled_bits = self.scrambler.scramble_bit(true) as u8
                | ((self.scrambler.scramble_bit(true) as u8) << 1);
            // Use only the lower 2 bits for 4-QAM (0..3).
            self.modulator.modulate_symbol_raw(scrambled_bits & 0x03);
        }
        self.symbol_count += SYMBOLS_PER_TICK;
    }

    /// Queue one tick's worth of rate signal symbols.
    ///
    /// Each consecutive symbol advances by a constant step (mod 4) that
    /// encodes the speed. For 14400, the step alternates between 1 and 3.
    fn queue_rate_symbols(&mut self) {
        for _ in 0..SYMBOLS_PER_TICK {
            let step = if self.our_max_speed == 14400 {
                let s = if self.rate_tx_step_toggle { 3 } else { 1 };
                self.rate_tx_step_toggle = !self.rate_tx_step_toggle;
                s
            } else {
                speed_to_step(self.our_max_speed)
            };
            self.rate_tx_last_sym = (self.rate_tx_last_sym + step) % 4;
            self.modulator.modulate_symbol_raw(self.rate_tx_last_sym);
        }
        self.symbol_count += SYMBOLS_PER_TICK;
    }

    // -----------------------------------------------------------------------
    // Carrier detection
    // -----------------------------------------------------------------------

    /// Update carrier detection based on demodulated I/Q symbols.
    fn update_carrier_detection(&mut self, symbols: &[(f64, f64)]) {
        if self.carrier_detected {
            return;
        }

        // Compute RMS of demodulated symbol magnitudes over this tick.
        if symbols.is_empty() {
            self.carrier_energy_accum = 0.0;
            self.carrier_energy_count = 0;
            return;
        }

        let sum_mag_sq: f64 = symbols.iter().map(|&(i, q)| i * i + q * q).sum();
        let rms = (sum_mag_sq / symbols.len() as f64).sqrt();

        if rms > CARRIER_RMS_THRESHOLD {
            self.carrier_energy_count += 1;
        } else {
            self.carrier_energy_count = 0;
        }

        if self.carrier_energy_count >= CARRIER_DETECT_TICKS {
            self.carrier_detected = true;
        }
    }

    // -----------------------------------------------------------------------
    // Rate signal decoding
    // -----------------------------------------------------------------------

    /// Collect demodulated symbols as hard decisions.
    fn collect_demod_symbols(&mut self, demod_symbols: &[(f64, f64)]) {
        for &(i, q) in demod_symbols {
            self.rate_symbols
                .push(self.demodulator.hard_decision_raw(i, q));
        }
    }

    /// Decode the collected rate signal and transition to Complete.
    ///
    /// Uses majority voting on consecutive-pair differential steps.
    fn decode_and_complete(&mut self) {
        if self.rate_symbols.len() < 4 {
            // Not enough data; fall back to minimum speed.
            self.negotiated_speed = Some(4800);
            self.state = TrainingState::Complete;
            return;
        }

        // Count differential steps between ALL consecutive symbol pairs.
        let mut step_votes = [0u32; 4]; // votes for step 0, 1, 2, 3

        for pair in self.rate_symbols.windows(2) {
            let s0 = pair[0] % 4;
            let s1 = pair[1] % 4;
            let step = (s1 + 4 - s0) % 4;
            step_votes[step as usize] += 1;
        }

        let speed = decode_step_votes(&step_votes);
        let agreed = self.our_max_speed.min(speed);
        self.negotiated_speed = Some(agreed);
        self.state = TrainingState::Complete;
    }
}

// ---------------------------------------------------------------------------
// Rate signal encoding/decoding
// ---------------------------------------------------------------------------

/// Map a speed to a constant differential step value.
///
/// For 14400, we use alternating steps 1/3 in the transmitter, so this
/// function is only used for speeds 4800-12000.
fn speed_to_step(speed: u32) -> u8 {
    match speed {
        4800 => 0,
        7200 => 1,
        9600 => 2,
        12000 => 3,
        _ => 0,
    }
}

/// Decode the step vote distribution to a speed.
///
/// For constant-step speeds (4800-12000), the majority step directly maps
/// to the speed. For 14400, the alternating (1,3) pattern produces roughly
/// equal votes for steps 1 and 3, which is distinguishable from any single
/// dominant step.
///
/// Timing recovery jitter in the demodulator causes spurious step-0 votes
/// (the same symbol decoded twice), so step 0 is treated as noise unless
/// it clearly dominates.
fn decode_step_votes(votes: &[u32; 4]) -> u32 {
    let total: u32 = votes.iter().sum();
    if total == 0 {
        return 4800;
    }

    // Check for the 14400 alternating pattern first: steps 1 and 3 both
    // have significant vote counts. Timing jitter inflates step 0, so we
    // check steps 1 and 3 each being >15% of total, and together >40%.
    let steps_1_and_3 = votes[1] + votes[3];
    let threshold_each = total / 6; // ~16.7%
    let threshold_combined = total * 4 / 10; // 40%
    if votes[1] > threshold_each && votes[3] > threshold_each && steps_1_and_3 > threshold_combined
    {
        // Both step 1 and step 3 are significant -- 14400 alternating pattern.
        return 14400;
    }

    // For constant-step speeds, find the step with the most votes among
    // steps 1, 2, 3 (excluding step 0 which is inflated by timing jitter).
    // Only fall back to step 0 (4800) if no other step has significant votes.
    let mut best_step = 0u8;
    let mut best_count = 0u32;
    for step in 1..4u8 {
        if votes[step as usize] > best_count {
            best_count = votes[step as usize];
            best_step = step;
        }
    }

    // If the best non-zero step has meaningful votes (>25% of total),
    // use it. Otherwise, step 0 wins (true 4800 signal or noise).
    if best_count > total / 4 {
        return step_to_speed(best_step);
    }

    // Step 0 dominant -- 4800.
    4800
}

/// Map a step value back to a speed.
fn step_to_speed(step: u8) -> u32 {
    match step {
        0 => 4800,
        1 => 7200,
        2 => 9600,
        3 => 12000,
        _ => 4800,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// Run a training session between caller and answerer, feeding each
    /// other's output with a one-tick delay.
    fn run_training_pair(
        caller: &mut Training,
        answerer: &mut Training,
        max_ticks: u32,
    ) -> (TrainingState, TrainingState) {
        let mut prev_caller_out = [0i16; TICK_SAMPLES];
        let mut prev_answerer_out = [0i16; TICK_SAMPLES];

        for _ in 0..max_ticks {
            let mut caller_out = [0i16; TICK_SAMPLES];
            let mut answerer_out = [0i16; TICK_SAMPLES];

            let cs = caller.tick(&prev_answerer_out, &mut caller_out);
            let as_ = answerer.tick(&prev_caller_out, &mut answerer_out);

            prev_caller_out = caller_out;
            prev_answerer_out = answerer_out;

            if cs == TrainingState::Complete && as_ == TrainingState::Complete {
                return (cs, as_);
            }
            if cs == TrainingState::Failed || as_ == TrainingState::Failed {
                return (cs, as_);
            }
        }

        (caller.state(), answerer.state())
    }

    #[test]
    fn test_training_caller_answerer() {
        let mut caller = Training::new(TrainingRole::Caller, 14400);
        let mut answerer = Training::new(TrainingRole::Answerer, 14400);

        let (cs, as_) = run_training_pair(&mut caller, &mut answerer, TIMEOUT_TICKS);

        assert_eq!(
            cs,
            TrainingState::Complete,
            "Caller should reach Complete state"
        );
        assert_eq!(
            as_,
            TrainingState::Complete,
            "Answerer should reach Complete state"
        );
        assert!(
            caller.negotiated_speed().is_some(),
            "Caller should have a negotiated speed"
        );
        assert!(
            answerer.negotiated_speed().is_some(),
            "Answerer should have a negotiated speed"
        );
        assert_eq!(caller.negotiated_speed(), answerer.negotiated_speed());
    }

    #[test]
    fn test_training_negotiates_speed() {
        let mut caller = Training::new(TrainingRole::Caller, 14400);
        let mut answerer = Training::new(TrainingRole::Answerer, 9600);

        let (cs, as_) = run_training_pair(&mut caller, &mut answerer, TIMEOUT_TICKS);

        assert_eq!(cs, TrainingState::Complete);
        assert_eq!(as_, TrainingState::Complete);
        assert_eq!(
            caller.negotiated_speed(),
            Some(9600),
            "Should negotiate to min(14400, 9600) = 9600"
        );
        assert_eq!(answerer.negotiated_speed(), Some(9600));
    }

    #[test]
    fn test_training_same_speed() {
        let mut caller = Training::new(TrainingRole::Caller, 9600);
        let mut answerer = Training::new(TrainingRole::Answerer, 9600);

        let (cs, as_) = run_training_pair(&mut caller, &mut answerer, TIMEOUT_TICKS);

        assert_eq!(cs, TrainingState::Complete);
        assert_eq!(as_, TrainingState::Complete);
        assert_eq!(caller.negotiated_speed(), Some(9600));
        assert_eq!(answerer.negotiated_speed(), Some(9600));
    }

    #[test]
    fn test_training_caller_timeout() {
        let mut caller = Training::new(TrainingRole::Caller, 14400);

        let silence = [0i16; TICK_SAMPLES];
        let mut out = [0i16; TICK_SAMPLES];

        let mut final_state = TrainingState::SendingAA;
        for _ in 0..TIMEOUT_TICKS + 10 {
            final_state = caller.tick(&silence, &mut out);
            if final_state == TrainingState::Failed {
                break;
            }
        }

        assert_eq!(
            final_state,
            TrainingState::Failed,
            "Caller should timeout and fail"
        );
        assert_eq!(caller.negotiated_speed(), None);
    }

    #[test]
    fn test_training_produces_audio() {
        let mut answerer = Training::new(TrainingRole::Answerer, 9600);

        let silence = [0i16; TICK_SAMPLES];
        let mut out = [0i16; TICK_SAMPLES];

        // Run a few ticks -- the answerer should be generating AA pattern.
        let mut any_nonzero = false;
        for _ in 0..10 {
            answerer.tick(&silence, &mut out);
            if out.iter().any(|&s| s != 0) {
                any_nonzero = true;
                break;
            }
        }

        assert!(
            any_nonzero,
            "Answerer should produce non-zero PCM output during AA phase"
        );
    }

    #[test]
    fn test_training_trn_inserts_s_and_sbar_before_scrambled_sequence() {
        let mut answerer = Training::new(TrainingRole::Answerer, 9600);
        answerer.state = TrainingState::SendingTRN;
        answerer.symbol_count = 0;
        answerer.trn_sub = TrnSubPhase::S;
        answerer.s_sym_count = 0;

        // S_TICKS = 3 (ceil(64/24)); timer bound is explicit via the constant.
        for tick in 0..S_TICKS {
            answerer.tick_answerer(&[]);
            let expected_subphase = if tick + 1 == S_TICKS {
                TrnSubPhase::SBar
            } else {
                TrnSubPhase::S
            };
            assert_eq!(
                answerer.trn_sub, expected_subphase,
                "Unexpected subphase after S tick {tick}"
            );
            assert_eq!(
                answerer.symbol_count, 0,
                "Scrambled TRN should not begin during S conditioning"
            );
        }

        // S_BAR_TICKS = 3 (ceil(64/24)); same bound as S.
        for tick in 0..S_BAR_TICKS {
            answerer.tick_answerer(&[]);
            let expected_subphase = if tick + 1 == S_BAR_TICKS {
                TrnSubPhase::Main
            } else {
                TrnSubPhase::SBar
            };
            assert_eq!(
                answerer.trn_sub, expected_subphase,
                "Unexpected subphase after S-bar tick {tick}"
            );
            assert_eq!(
                answerer.symbol_count, 0,
                "Scrambled TRN should not begin during S-bar conditioning"
            );
        }

        answerer.tick_answerer(&[]);
        assert_eq!(answerer.trn_sub, TrnSubPhase::Main);
        assert_eq!(
            answerer.symbol_count, SYMBOLS_PER_TICK,
            "Scrambled TRN should start only after S and S-bar complete"
        );
    }

    /// Mirror of `test_training_trn_inserts_s_and_sbar_before_scrambled_sequence`
    /// but for the caller role, ensuring both sides condition correctly before TRN.
    #[test]
    fn test_trn_caller_s_and_sbar_conditioning() {
        let mut caller = Training::new(TrainingRole::Caller, 9600);
        caller.state = TrainingState::SendingTRN;
        caller.symbol_count = 0;
        caller.trn_sub = TrnSubPhase::S;
        caller.s_sym_count = 0;

        for tick in 0..S_TICKS {
            caller.tick_caller(&[]);
            let expected_subphase = if tick + 1 == S_TICKS {
                TrnSubPhase::SBar
            } else {
                TrnSubPhase::S
            };
            assert_eq!(
                caller.trn_sub, expected_subphase,
                "Caller: unexpected subphase after S tick {tick}"
            );
            assert_eq!(
                caller.symbol_count, 0,
                "Caller: scrambled TRN should not begin during S conditioning"
            );
        }

        for tick in 0..S_BAR_TICKS {
            caller.tick_caller(&[]);
            let expected_subphase = if tick + 1 == S_BAR_TICKS {
                TrnSubPhase::Main
            } else {
                TrnSubPhase::SBar
            };
            assert_eq!(
                caller.trn_sub, expected_subphase,
                "Caller: unexpected subphase after S-bar tick {tick}"
            );
            assert_eq!(
                caller.symbol_count, 0,
                "Caller: scrambled TRN should not begin during S-bar conditioning"
            );
        }

        caller.tick_caller(&[]);
        assert_eq!(caller.trn_sub, TrnSubPhase::Main);
        assert_eq!(
            caller.symbol_count, SYMBOLS_PER_TICK,
            "Caller: scrambled TRN should start only after S and S-bar complete"
        );
    }

    /// Verify that TRN_TOTAL_TICKS correctly predicts exactly how many ticks
    /// elapse between entering SendingTRN and transitioning to SendingRateSignal.
    ///
    /// This pins the timer-bound: S (3 ticks) + S-bar (3 ticks) + main (50 ticks) = 56 ticks.
    #[test]
    fn test_trn_total_phase_ticks() {
        // Answerer path.
        let mut answerer = Training::new(TrainingRole::Answerer, 9600);
        answerer.state = TrainingState::SendingTRN;
        answerer.symbol_count = 0;
        answerer.trn_sub = TrnSubPhase::S;
        answerer.s_sym_count = 0;
        answerer.scrambler.reset();

        let mut ticks_in_trn: u32 = 0;
        loop {
            answerer.tick_answerer(&[]);
            ticks_in_trn += 1;
            if answerer.state == TrainingState::SendingRateSignal {
                break;
            }
            assert!(
                ticks_in_trn <= TRN_TOTAL_TICKS + 5,
                "answerer TRN phase exceeded {TRN_TOTAL_TICKS} ticks (at {ticks_in_trn})"
            );
        }
        assert_eq!(
            ticks_in_trn, TRN_TOTAL_TICKS,
            "answerer TRN should take exactly {TRN_TOTAL_TICKS} ticks \
             (S={S_TICKS} + S-bar={S_BAR_TICKS} + main={TRN_MAIN_TICKS})"
        );

        // Caller path.
        let mut caller = Training::new(TrainingRole::Caller, 9600);
        caller.state = TrainingState::SendingTRN;
        caller.symbol_count = 0;
        caller.trn_sub = TrnSubPhase::S;
        caller.s_sym_count = 0;
        caller.scrambler.reset();

        let mut ticks_in_trn: u32 = 0;
        loop {
            caller.tick_caller(&[]);
            ticks_in_trn += 1;
            if caller.state == TrainingState::SendingRateSignal {
                break;
            }
            assert!(
                ticks_in_trn <= TRN_TOTAL_TICKS + 5,
                "caller TRN phase exceeded {TRN_TOTAL_TICKS} ticks (at {ticks_in_trn})"
            );
        }
        assert_eq!(
            ticks_in_trn, TRN_TOTAL_TICKS,
            "caller TRN should take exactly {TRN_TOTAL_TICKS} ticks \
             (S={S_TICKS} + S-bar={S_BAR_TICKS} + main={TRN_MAIN_TICKS})"
        );
    }

    #[test]
    fn test_training_into_data_mode() {
        let mut caller = Training::new(TrainingRole::Caller, 14400);
        let mut answerer = Training::new(TrainingRole::Answerer, 14400);

        run_training_pair(&mut caller, &mut answerer, TIMEOUT_TICKS);

        assert_eq!(caller.state(), TrainingState::Complete);
        assert_eq!(answerer.state(), TrainingState::Complete);

        // Transfer ownership of modulator/demodulator for data mode.
        let (mut mod_c, mut demod_c) = caller.into_data_mode();
        let (mut mod_a, mut demod_a) = answerer.into_data_mode();

        // Verify they are functional: modulate a symbol on one side,
        // generate samples, demodulate on the other.
        mod_c.modulate_symbol_raw(1);
        let mut pcm = Vec::new();
        mod_c.generate(80, &mut pcm);
        let symbols = demod_a.demodulate(&pcm);
        let _ = symbols;

        // Same for the reverse direction.
        mod_a.modulate_symbol_raw(2);
        let mut pcm2 = Vec::new();
        mod_a.generate(80, &mut pcm2);
        let symbols2 = demod_c.demodulate(&pcm2);
        let _ = symbols2;
    }

    // -----------------------------------------------------------------------
    // Rate signal encoding/decoding unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_rate_signal_roundtrip() {
        for &speed in &[4800u32, 7200, 9600, 12000] {
            let step = speed_to_step(speed);
            let decoded = step_to_speed(step);
            assert_eq!(decoded, speed, "Rate signal roundtrip failed for {speed}");
        }
    }

    #[test]
    fn test_rate_signal_unknown_speed() {
        let step = speed_to_step(2400);
        let decoded = step_to_speed(step);
        // 2400 maps to default step 0 -> 4800.
        assert_eq!(decoded, 4800);
    }

    #[test]
    fn test_decode_step_votes_clear_majority() {
        // 9600 = step 2 dominant.
        let votes = [5, 3, 80, 2];
        assert_eq!(decode_step_votes(&votes), 9600);
    }

    #[test]
    fn test_decode_step_votes_14400_pattern() {
        // 14400: alternating steps 1 and 3 -> roughly equal votes.
        let votes = [5, 45, 3, 42];
        assert_eq!(decode_step_votes(&votes), 14400);
    }

    #[test]
    fn test_decode_step_votes_empty() {
        let votes = [0, 0, 0, 0];
        assert_eq!(decode_step_votes(&votes), 4800);
    }
}
