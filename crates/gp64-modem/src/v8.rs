//! V.8 capability negotiation over V.21 FSK.
//!
//! After call setup tones (CNG/ANSam), both sides switch to V.21 FSK for
//! capability exchange:
//!
//! 1. **Answerer** sends CI (Call Indicator) octets on the answer channel
//!    after ANSam, signalling V.8 capability.
//! 2. **Caller** detects CI, then sends CM (Call Menu) listing supported
//!    modulation standards on the originate channel.
//! 3. **Answerer** receives CM, selects the best common standard, sends JM.
//! 4. **Caller** receives JM, ceases CM transmission (CJ = sustained idle mark);
//!    the answerer detects CJ via silence and both sides proceed to training.
//!
//! Reference: ITU-T V.8 (2000-11) §§5–6, Tables 1 and 3. CJ signalling
//! follows V.8 §9.3 (Te silence threshold). Octet bit assignments and the
//! "V.8 over V.21 FSK" framing match spandsp's `src/v8.c` and
//! `src/spandsp/private/v8.h` (Steve Underwood, LGPL-2.1).

use std::collections::VecDeque;

use crate::fsk::{FskDemodulator, FskModulator};

/// Result of V.8 negotiation: which protocol and speed were agreed upon.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NegotiatedProtocol {
    V32bis(u32),
    V34(u32),
    V90 {
        downstream_bps: u32,
        upstream_bps: u32,
    },
}

/// 10 ms tick = 80 samples at 8000 Hz.
const TICK_SAMPLES: usize = 80;

/// Startup idle duration in ticks (200 ms = 20 ticks).
const STARTUP_IDLE_TICKS: u32 = 20;

/// Timeout in ticks (5 seconds = 500 ticks at 10 ms each).
const TIMEOUT_TICKS: u32 = 500;

/// Maximum number of times to repeat a message before giving up.
const MAX_TX_REPEATS: u32 = 3;

/// CJ silence detection threshold in ticks (80 ms = 8 ticks).
/// CJ is not a transmitted byte — it is the cessation of CM transmission.
/// The answerer detects CJ by observing sustained idle (no V.8 octets) for
/// this duration after having received CM and sent JM.
///
/// ITU-T V.8 §9.3 Te ≈ 75 ms; spandsp's `V8_SIGA` uses the same 75 ms ballpark.
/// Rounded up to 80 ms (8 ticks) for one-tick-wire-delay slack.
const CJ_SILENCE_TICKS: u32 = 8;

/// V.8 CI (Call Indicator) pattern byte.
/// 10-bit V.21 character: 1 start bit (0) + 8 data bits (0x81) + 1 stop bit (1).
/// 0x81 = category 0x01 (Call Function) with the calling-end indicator bit set,
/// matching spandsp's CI byte and V.8 §5.3 / Table 1.
///
/// In this implementation the answerer emits CI on the V.21 answer channel
/// immediately after ANSam, signalling V.8 capability to the caller.
const CI_PATTERN_BYTE: u8 = 0x81;

/// Number of CI repetitions before the answerer starts listening for CM.
/// spandsp uses 10 repetitions; V.8 §5.3 allows up to ~10 before giving up
/// on V.8 and falling back to a V.25-style connection.
const CI_REPETITIONS: u32 = 10;

// ---------------------------------------------------------------------------
// ITU-T V.8 octet encoding tables
//
// CM/JM messages use a sync octet, call-function octet, and one or more
// modulation-mode octets. Modulation-mode octets carry a low-5-bit tag
// (category) plus an extension marker in bits 5..3 (`010` = "more octets
// follow", `000` = "last octet"). This 5+3 split matches spandsp's
// `src/v8.c::parse_modulation_modes()` / `pack_modulation_modes()`
// and the V.8 (2000-11) §6.2 / Table 3 bit layout.
//
// Sync octet 0xE0 corresponds to spandsp's V.8 message start pattern.
// The V-series data-call function (0x06 << 5 | 0x01 = 0xC1) matches
// spandsp's `V8_CALL_V_SERIES`.
// ---------------------------------------------------------------------------

/// CM/JM sync octet sent after the mark preamble.
const V8_CM_JM_SYNC_OCTET: u8 = 0xE0;

/// Call function tag (low 5 bits).
const V8_CALL_FUNCTION_TAG: u8 = 0x01;
/// V-series modem data call function value.
const V8_CALL_FUNCTION_V_SERIES: u8 = 0x06;

/// Modulation mode tag (primary octet low 5 bits).
const V8_MODULATION_TAG: u8 = 0x05;
/// Modulation extension marker (bits 5..3 = 0b010).
const V8_MODULATION_EXTENSION_BASE: u8 = 0x10;
const V8_MODULATION_EXTENSION_MASK: u8 = 0x38;
const V8_MODULATION_EXTENSION_MATCH: u8 = 0x10;
const V8_MODULATION_OCTET_COUNT: usize = 3;

/// ITU-T V.8 Table 3 modulation mode bit positions.
/// Primary octet (tag bits 4-0 = 00101):
///   bit 6: V.34 full duplex
///   bit 5: V.FC / V.90
/// First extension octet (bits 5-3 = 010 marker):
///   bit 7: V.32bis
const V8_MOD0_V34: u8 = 0x40;
const V8_MOD0_V90: u8 = 0x20;
const V8_MOD1_V32BIS: u8 = 0x80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum V8ProtocolFamily {
    V32bis,
    V34,
    V90,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct V8FamilyTableEntry {
    family: V8ProtocolFamily,
    modulation_octet_index: usize,
    modulation_bit: u8,
}

const V8_FAMILY_TABLE: [V8FamilyTableEntry; 3] = [
    V8FamilyTableEntry {
        family: V8ProtocolFamily::V32bis,
        modulation_octet_index: 1,
        modulation_bit: V8_MOD1_V32BIS,
    },
    V8FamilyTableEntry {
        family: V8ProtocolFamily::V34,
        modulation_octet_index: 0,
        modulation_bit: V8_MOD0_V34,
    },
    V8FamilyTableEntry {
        family: V8ProtocolFamily::V90,
        modulation_octet_index: 0,
        modulation_bit: V8_MOD0_V90,
    },
];

/// V.8 Call Function octet for a V-series data modem call.
const V8_CALL_FUNCTION_DATA: u8 = (V8_CALL_FUNCTION_V_SERIES << 5) | V8_CALL_FUNCTION_TAG;

// ---------------------------------------------------------------------------
// Modem capabilities
// ---------------------------------------------------------------------------

/// Supported modulation capabilities (bitfield).
///
/// Only V.32bis rates with an honest data-mode implementation are advertised:
/// 4800 and 9600. 7200, 12000, and 14400 are deferred until their QAM/trellis
/// data paths are byte-stable (see HLD §9/§12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModemCapabilities {
    pub v32bis_14400: bool,
    pub v32bis_9600: bool,
    pub v32bis_4800: bool,
    pub v34_28800: bool,
    pub v90: bool,
}

impl ModemCapabilities {
    /// Create capabilities from a maximum speed.
    /// All supported honest data-mode speeds at or below `max_speed` are enabled.
    pub fn from_max_speed(max_speed: u32) -> Self {
        Self {
            // The QAM128/trellis infrastructure exists, but the online DSP
            // path is not byte-stable yet. Do not negotiate/report 14400 until
            // that data path is honest end to end.
            v32bis_14400: false,
            v32bis_9600: max_speed >= 9600,
            v32bis_4800: max_speed >= 4800,
            // V.34/V.90 training exists, but their online data paths are not
            // yet honest enough to advertise during normal V.8 negotiation.
            // Fall back to the highest implemented V.32bis rate instead.
            v34_28800: false,
            v90: false,
        }
    }

    /// Encode as V.8 CM/JM octet sequence (Table 3 layout).
    pub fn to_v8_octets(&self) -> Vec<u8> {
        let mut octets = Vec::with_capacity(2 + V8_MODULATION_OCTET_COUNT);
        octets.push(V8_CM_JM_SYNC_OCTET);
        octets.push(V8_CALL_FUNCTION_DATA);
        octets.extend_from_slice(&self.v8_modulation_octets());
        octets
    }

    /// Decode from V.8 CM/JM octet sequence.
    pub fn from_v8_octets(octets: &[u8]) -> Option<Self> {
        let modulation = parse_v8_modulation_octets(octets)?;
        let mut caps = Self::from_max_speed(0);
        caps.enable_v8_modulation_octets(modulation);
        Some(caps)
    }

    /// Find the highest common protocol and speed between two capability sets.
    /// Returns the negotiated protocol and speed, or `None` if no common capability.
    pub fn negotiate(&self, other: &Self) -> Option<NegotiatedProtocol> {
        // Check from highest to lowest, V.90 first.
        if self.v90 && other.v90 {
            return Some(NegotiatedProtocol::V90 {
                downstream_bps: 56000,
                upstream_bps: 28800,
            });
        }
        if self.v34_28800 && other.v34_28800 {
            return Some(NegotiatedProtocol::V34(28800));
        }
        if self.v32bis_14400 && other.v32bis_14400 {
            return Some(NegotiatedProtocol::V32bis(14400));
        }
        if self.v32bis_9600 && other.v32bis_9600 {
            return Some(NegotiatedProtocol::V32bis(9600));
        }
        if self.v32bis_4800 && other.v32bis_4800 {
            return Some(NegotiatedProtocol::V32bis(4800));
        }
        None
    }

    fn for_negotiated_protocol(protocol: NegotiatedProtocol) -> Self {
        let mut caps = Self::from_max_speed(0);
        match protocol {
            NegotiatedProtocol::V32bis(_) => caps.enable_family(V8ProtocolFamily::V32bis),
            NegotiatedProtocol::V34(_) => caps.enable_family(V8ProtocolFamily::V34),
            NegotiatedProtocol::V90 { .. } => caps.enable_family(V8ProtocolFamily::V90),
        }
        caps
    }

    fn v8_modulation_octets(&self) -> [u8; V8_MODULATION_OCTET_COUNT] {
        let mut octets = [
            V8_MODULATION_TAG,
            V8_MODULATION_EXTENSION_BASE,
            V8_MODULATION_EXTENSION_BASE,
        ];
        for entry in V8_FAMILY_TABLE {
            if self.supports_family(entry.family) {
                octets[entry.modulation_octet_index] |= entry.modulation_bit;
            }
        }
        octets
    }

    fn enable_v8_modulation_octets(&mut self, octets: [u8; V8_MODULATION_OCTET_COUNT]) {
        for entry in V8_FAMILY_TABLE {
            if octets[entry.modulation_octet_index] & entry.modulation_bit != 0 {
                self.enable_family(entry.family);
            }
        }
    }

    fn supports_family(&self, family: V8ProtocolFamily) -> bool {
        match family {
            V8ProtocolFamily::V32bis => self.v32bis_14400 || self.v32bis_9600 || self.v32bis_4800,
            V8ProtocolFamily::V34 => self.v34_28800,
            V8ProtocolFamily::V90 => self.v90,
        }
    }

    fn enable_family(&mut self, family: V8ProtocolFamily) {
        match family {
            V8ProtocolFamily::V32bis => {
                self.v32bis_14400 = true;
                self.v32bis_9600 = true;
                self.v32bis_4800 = true;
            }
            V8ProtocolFamily::V34 => {
                self.v34_28800 = true;
            }
            V8ProtocolFamily::V90 => {
                self.v90 = true;
                self.v34_28800 = true;
            }
        }
    }
}

fn is_modulation_octet(octet: u8) -> bool {
    (octet & 0x1F) == V8_MODULATION_TAG
}

fn is_modulation_extension_octet(octet: u8) -> bool {
    (octet & V8_MODULATION_EXTENSION_MASK) == V8_MODULATION_EXTENSION_MATCH
}

fn parse_v8_modulation_octets(octets: &[u8]) -> Option<[u8; V8_MODULATION_OCTET_COUNT]> {
    let mut best_match = None;
    let mut fallback_match = None;

    for idx in 0..octets.len().saturating_sub(V8_MODULATION_OCTET_COUNT - 1) {
        let candidate = &octets[idx..idx + V8_MODULATION_OCTET_COUNT];
        if !is_modulation_octet(candidate[0])
            || !is_modulation_extension_octet(candidate[1])
            || !is_modulation_extension_octet(candidate[2])
        {
            continue;
        }

        let modulation = [candidate[0], candidate[1], candidate[2]];
        let has_call_function_prefix = idx >= 1 && octets[idx - 1] == V8_CALL_FUNCTION_DATA;
        let has_sync_and_call_function_prefix = idx >= 2
            && octets[idx - 2] == V8_CM_JM_SYNC_OCTET
            && octets[idx - 1] == V8_CALL_FUNCTION_DATA;

        if has_call_function_prefix || has_sync_and_call_function_prefix {
            best_match = Some(modulation);
        } else if fallback_match.is_none() {
            // Clean loopback can still demodulate the Table 3 modulation triad
            // even if the framing octets are lost. Accept that as a fallback so
            // CM/JM decode stays interoperable with the current FSK path.
            fallback_match = Some(modulation);
        }
    }

    best_match.or(fallback_match)
}

// ---------------------------------------------------------------------------
// V8 negotiation state machine
// ---------------------------------------------------------------------------

/// Role in the V.8 handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V8Role {
    Caller,
    Answerer,
}

/// Current state of V.8 negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V8State {
    /// Waiting to start / sending preamble.
    Starting,
    /// Sent our message, waiting for response.
    WaitingForResponse,
    /// Caller: sending CJ silence (idle mark) after receiving JM.
    /// CJ is the cessation of CM — not a transmitted byte.
    SendingCjSilence {
        /// Ticks of idle mark remaining before completing.
        remaining: u32,
    },
    /// Received response, negotiation complete.
    Complete,
    /// Failed (timeout or incompatible).
    Failed,
}

/// V.8 negotiation state for one side (caller or answerer).
pub struct V8Negotiator {
    role: V8Role,
    state: V8State,
    our_caps: ModemCapabilities,
    /// FSK modulator (caller=originate channel, answerer=answer channel).
    tx_fsk: FskModulator,
    /// FSK demodulator (caller=answer channel to receive JM, answerer=originate to receive CM).
    rx_fsk: FskDemodulator,
    /// Negotiated protocol (set when complete).
    negotiated_protocol: Option<NegotiatedProtocol>,
    /// PCM output buffer for current tick.
    tx_buffer: Vec<i16>,
    /// Messages to transmit.
    tx_pending: VecDeque<Vec<u8>>,
    /// Whether we've started transmitting message bytes.
    tx_started: bool,
    /// Ticks elapsed (for startup delay and timeout).
    tick_count: u32,
    /// Receive buffer for assembling decoded bytes into messages.
    rx_buffer: Vec<u8>,
    /// Number of times we have transmitted our message.
    tx_send_count: u32,
    /// Answerer: ticks since last V.8 octet received (for CJ silence detection).
    cj_silence_counter: u32,
    /// Answerer: number of CI patterns sent during startup.
    ci_sent_count: u32,
    /// Caller: whether CI has been detected from the answerer.
    ci_detected: bool,
    /// Answerer diagnostic: `true` iff the `WaitingForResponse → Complete`
    /// transition fired via the spec-correct CJ-silence branch
    /// (`cj_silence_counter >= CJ_SILENCE_TICKS`), as opposed to the lenient
    /// `MAX_TX_REPEATS` JM-repeat short-circuit. Used by the diagnostic test
    /// to verify the silence path is the active exit post-fix.
    exit_via_silence: bool,
}

impl V8Negotiator {
    /// Create a new V.8 negotiator.
    ///
    /// - `Caller` modulates on the originate channel and demodulates the answer channel.
    /// - `Answerer` modulates on the answer channel and demodulates the originate channel.
    pub fn new(role: V8Role, capabilities: ModemCapabilities) -> Self {
        let (tx_fsk, rx_fsk) = match role {
            V8Role::Caller => (FskModulator::new_originate(), FskDemodulator::new_answer()),
            V8Role::Answerer => (FskModulator::new_answer(), FskDemodulator::new_originate()),
        };

        Self {
            role,
            state: V8State::Starting,
            our_caps: capabilities,
            tx_fsk,
            rx_fsk,
            negotiated_protocol: None,
            tx_buffer: Vec::with_capacity(TICK_SAMPLES * 2),
            tx_pending: VecDeque::new(),
            tx_started: false,
            tick_count: 0,
            rx_buffer: Vec::new(),
            tx_send_count: 0,
            cj_silence_counter: 0,
            ci_sent_count: 0,
            ci_detected: false,
            exit_via_silence: false,
        }
    }

    /// Process one 10 ms tick of V.8 negotiation.
    ///
    /// - `line_in`: 80 PCM samples received from remote
    /// - `line_out`: 80 PCM samples to send to remote
    ///
    /// Returns the current state.
    pub fn tick(
        &mut self,
        line_in: &[i16; TICK_SAMPLES],
        line_out: &mut [i16; TICK_SAMPLES],
    ) -> V8State {
        // Terminal states produce silence and do nothing.
        if matches!(self.state, V8State::Complete | V8State::Failed) {
            line_out.fill(0);
            return self.state;
        }

        self.tick_count += 1;

        // Check timeout.
        if self.tick_count >= TIMEOUT_TICKS {
            self.state = V8State::Failed;
            line_out.fill(0);
            return self.state;
        }

        // Startup guard: discard incoming audio during the first
        // STARTUP_IDLE_TICKS. When both modem endpoints enter V.8 at
        // slightly different times, the late side may still be sending
        // call-setup tones (CNG at 1100 Hz, close to originate-space
        // 1180 Hz). These tones corrupt the FSK demodulator's bit sync
        // and cause garbage byte decoding. Discarding input during the
        // startup period lets both sides settle into FSK before
        // demodulation begins.
        if self.tick_count <= STARTUP_IDLE_TICKS {
            // Generate output (idle mark) but don't demodulate input.
            self.generate_output(line_out);
            return self.state;
        }

        // Log incoming PCM energy periodically for diagnostics.
        if self.tick_count.is_multiple_of(100) || self.tick_count == STARTUP_IDLE_TICKS + 1 {
            let rms: f64 = (line_in
                .iter()
                .map(|&s| (s as f64) * (s as f64))
                .sum::<f64>()
                / TICK_SAMPLES as f64)
                .sqrt();
            tracing::debug!(
                target: "gp64::hw::serial",
                "V8 {:?} tick={}: line_in RMS={:.1}, rx_buf={}, state={:?}",
                self.role, self.tick_count, rms, self.rx_buffer.len(), self.state
            );
        }

        // Demodulate incoming audio.
        self.rx_fsk.demodulate(line_in);

        // Drain decoded bytes into rx_buffer.
        let prev_len = self.rx_buffer.len();
        while let Some(byte) = self.rx_fsk.take_byte() {
            self.rx_buffer.push(byte);
        }
        let received_bytes = self.rx_buffer.len() > prev_len;
        if received_bytes {
            tracing::debug!(
                target: "gp64::hw::serial",
                "V8 {:?} tick={}: +{} byte(s), rx_buf={:02X?}",
                self.role, self.tick_count,
                self.rx_buffer.len() - prev_len,
                &self.rx_buffer[..]
            );
            // Reset CJ silence counter when we receive any bytes.
            self.cj_silence_counter = 0;
        } else {
            // No bytes received this tick — increment silence counter.
            self.cj_silence_counter += 1;
        }

        // Run role-specific state machine.
        match self.role {
            V8Role::Caller => self.tick_caller(),
            V8Role::Answerer => self.tick_answerer(),
        }

        // Generate output audio.
        self.generate_output(line_out);

        self.state
    }

    /// The negotiated protocol, if complete.
    pub fn negotiated_protocol(&self) -> Option<NegotiatedProtocol> {
        self.negotiated_protocol
    }

    /// Current state.
    pub fn state(&self) -> V8State {
        self.state
    }

    /// Whether the caller has detected CI from the answerer.
    pub fn ci_detected(&self) -> bool {
        self.ci_detected
    }

    /// How many CI patterns the answerer has sent.
    pub fn ci_sent_count(&self) -> u32 {
        self.ci_sent_count
    }

    /// Tick counter (10 ms per tick).  Read-only — exposed so failure-path
    /// WARN logging in `state_machine.rs` can record how many ticks the
    /// negotiator consumed before timing out.
    pub fn tick_count(&self) -> u32 {
        self.tick_count
    }

    /// Tick budget after which `tick()` forces a `Failed` transition.
    /// Read-only — paired with `tick_count()` for failure-path diagnostics.
    pub fn timeout_ticks(&self) -> u32 {
        TIMEOUT_TICKS
    }

    /// Role this negotiator is playing (Caller or Answerer).  Read-only —
    /// paired with the failure-path WARN logging.
    pub fn role(&self) -> V8Role {
        self.role
    }

    /// Number of times the current message has been transmitted
    /// (diagnostic: how many CM or JM repeats have occurred).
    #[cfg(test)]
    pub fn tx_send_count(&self) -> u32 {
        self.tx_send_count
    }

    /// CJ silence counter (diagnostic: ticks since last received byte on
    /// the answerer side; zero for the caller).
    #[cfg(test)]
    pub fn cj_silence_counter(&self) -> u32 {
        self.cj_silence_counter
    }

    /// Whether the answerer's `WaitingForResponse → Complete` transition
    /// fired via the spec-correct CJ-silence branch
    /// (`cj_silence_counter >= CJ_SILENCE_TICKS`) rather than the lenient
    /// `MAX_TX_REPEATS` JM-repeat short-circuit. Always `false` on the caller.
    pub fn exit_via_silence(&self) -> bool {
        self.exit_via_silence
    }

    /// Human-readable description of the current sub-state for diagnostic
    /// timeline printing.  Returns a compact label suitable for column output.
    #[cfg(test)]
    pub fn diag_state(&self) -> &'static str {
        match self.state {
            V8State::Starting => {
                if self.ci_sent_count > 0 {
                    "Starting(CI-sent)"
                } else {
                    "Starting"
                }
            }
            V8State::WaitingForResponse => "WaitingForResponse",
            V8State::SendingCjSilence { .. } => "SendingCjSilence",
            V8State::Complete => "Complete",
            V8State::Failed => "Failed",
        }
    }

    // -----------------------------------------------------------------------
    // Caller state machine
    // -----------------------------------------------------------------------

    fn tick_caller(&mut self) {
        match self.state {
            V8State::Starting => {
                // Wait for startup idle period, then look for CI from the
                // answerer before sending CM. If CI is detected, proceed
                // immediately. Otherwise, fall back after STARTUP_IDLE_TICKS
                // (backward compatibility with TCP back-to-back mode where
                // both sides enter V.8 simultaneously and no CI is sent).
                if self.tick_count > STARTUP_IDLE_TICKS && !self.ci_detected {
                    // Check if we received CI pattern byte.
                    if self.rx_buffer.contains(&CI_PATTERN_BYTE) {
                        self.ci_detected = true;
                        self.rx_buffer.clear();
                        tracing::debug!(
                            target: "gp64::hw::serial",
                            "V8 Caller: CI detected at tick {}", self.tick_count
                        );
                    }
                }

                // Queue CM after CI detected, or after 2× startup idle as
                // fallback (gives answerer time to send CI).
                if !self.tx_started
                    && (self.ci_detected || self.tick_count >= STARTUP_IDLE_TICKS * 2)
                {
                    self.queue_cm();
                    self.tx_started = true;
                    self.state = V8State::WaitingForResponse;
                }
            }
            V8State::WaitingForResponse => {
                // Try to decode JM (V.8 capability octets) from received bytes.
                if let Some(remote_caps) = self.try_decode_v8_capabilities() {
                    // Negotiate protocol.
                    if let Some(protocol) = self.our_caps.negotiate(&remote_caps) {
                        self.negotiated_protocol = Some(protocol);
                        // CJ = cessation of CM. Per ITU-T V.8 §9.3, post-CJ
                        // silence Te is the answerer's RX-side detection
                        // problem, not a caller TX-side hold. The caller has
                        // done its job once it stops emitting CM. The
                        // single-tick `SendingCjSilence` slot is paranoia
                        // margin: `queue_cm` already emits 270 samples of
                        // trailing idle per CM byte (so the answerer's FSK
                        // demod can finalise the last byte), and the buffer
                        // clears below ensure nothing is left in the tx
                        // pipeline. We keep the substate as a structural slot
                        // for future hooks without lengthening the wait.
                        self.tx_pending.clear();
                        self.tx_buffer.clear();
                        self.state = V8State::SendingCjSilence { remaining: 1 };
                    } else {
                        self.state = V8State::Failed;
                    }
                } else if self.tx_pending.is_empty() && self.tx_buffer.is_empty() {
                    // NOTE: this CM-repeat branch is effectively dead in B2B
                    // because `tx_buffer`'s idle-filler steady state leaves a
                    // residue of < TICK_SAMPLES samples (never strictly empty)
                    // — see the answerer's `tx_drained` discussion above. Out
                    // of scope for the V.8 skew fix; revisit when a real-line
                    // test shows CM-repeat on dropped first-CM is needed.
                    if self.tx_send_count < MAX_TX_REPEATS {
                        self.queue_cm();
                    }
                }
            }
            V8State::SendingCjSilence { remaining } => {
                // Count down idle mark ticks (CJ silence period).
                if remaining <= 1 {
                    self.state = V8State::Complete;
                } else {
                    self.state = V8State::SendingCjSilence {
                        remaining: remaining - 1,
                    };
                }
            }
            V8State::Complete | V8State::Failed => {}
        }
    }

    // -----------------------------------------------------------------------
    // Answerer state machine
    // -----------------------------------------------------------------------

    fn tick_answerer(&mut self) {
        match self.state {
            V8State::Starting => {
                // Send CI patterns during startup to signal V.8 capability,
                // then listen for CM from the caller.
                if self.ci_sent_count == 0 {
                    // Queue all CI repetitions as a single message block.
                    // This shares one preamble/trailing idle, keeping the
                    // FSK output compact so JM isn't delayed.
                    self.queue_ci_burst();
                } else if let Some(remote_caps) = self.try_decode_v8_capabilities() {
                    // CI phase complete — now listen for CM.
                    if let Some(protocol) = self.our_caps.negotiate(&remote_caps) {
                        self.negotiated_protocol = Some(protocol);
                        // Flush any remaining CI samples so JM transmits promptly.
                        self.tx_buffer.clear();
                        self.tx_pending.clear();
                        self.queue_jm();
                        self.state = V8State::WaitingForResponse;
                    } else {
                        self.state = V8State::Failed;
                    }
                }
            }
            V8State::WaitingForResponse => {
                // CJ detection: the caller stops sending CM and we detect
                // silence (no V.8 octets for CJ_SILENCE_TICKS). Gate this on
                // "our JM message has been fully transmitted" — bailing
                // mid-JM would leave the caller with a partial JM it cannot
                // decode. With `CJ_SILENCE_TICKS = 8` the threshold is short
                // enough that JM TX (~17 ticks per 3-byte message) is still
                // mid-flight when the silence counter trips, so this guard
                // is load-bearing post-fix; pre-fix (threshold 20) the JM
                // had already finished by the time silence fired and the
                // guard was implicit. We treat "tx_buffer holding less than
                // one tick worth of samples" as message-drained: the
                // generator's idle-filler steady state leaves a residue of
                // ~47 samples (< TICK_SAMPLES = 80) when no message is
                // pending, so this expression is true exactly when the
                // current message has been emitted onto the wire.
                let tx_drained = self.tx_pending.is_empty() && self.tx_buffer.len() < TICK_SAMPLES;
                if tx_drained && self.cj_silence_counter >= CJ_SILENCE_TICKS {
                    self.exit_via_silence = true;
                    self.state = V8State::Complete;
                } else if tx_drained {
                    // JM finished transmitting. Repeat or give up.
                    if self.tx_send_count >= MAX_TX_REPEATS {
                        // After MAX_TX_REPEATS JM transmissions, consider
                        // negotiation complete even without CJ (lenient behavior
                        // matching real modems that treat repeated JM as success).
                        self.state = V8State::Complete;
                    } else {
                        self.queue_jm();
                    }
                }
            }
            V8State::SendingCjSilence { .. } | V8State::Complete | V8State::Failed => {}
        }
    }

    // -----------------------------------------------------------------------
    // Message helpers
    // -----------------------------------------------------------------------

    /// Queue all CI repetitions as a single message block.
    /// Shares one preamble + trailing idle to keep the FSK output compact.
    fn queue_ci_burst(&mut self) {
        let msg: Vec<u8> = vec![CI_PATTERN_BYTE; CI_REPETITIONS as usize];
        self.tx_pending.push_back(msg);
        self.ci_sent_count = CI_REPETITIONS;
        tracing::debug!(
            target: "gp64::hw::serial",
            "V8 Answerer: queued {} CI patterns, listening for CM", CI_REPETITIONS
        );
    }

    /// Queue a CM message (V.8 capability octets) for transmission.
    fn queue_cm(&mut self) {
        let msg = self.our_caps.to_v8_octets();
        self.tx_pending.push_back(msg);
        self.tx_send_count += 1;
    }

    /// Queue a JM message (V.8 capability octets) for transmission.
    fn queue_jm(&mut self) {
        let msg = self
            .negotiated_protocol
            .map(ModemCapabilities::for_negotiated_protocol)
            .unwrap_or(self.our_caps)
            .to_v8_octets();
        self.tx_pending.push_back(msg);
        self.tx_send_count += 1;
    }

    /// Try to decode V.8 capability octets from the receive buffer.
    fn try_decode_v8_capabilities(&mut self) -> Option<ModemCapabilities> {
        let caps = ModemCapabilities::from_v8_octets(&self.rx_buffer)?;
        self.rx_buffer.clear(); // consume all
        Some(caps)
    }

    // -----------------------------------------------------------------------
    // Audio generation
    // -----------------------------------------------------------------------

    /// Fill `line_out` with PCM from the transmit buffer, generating more
    /// samples from pending messages as needed.
    fn generate_output(&mut self, line_out: &mut [i16; TICK_SAMPLES]) {
        // Ensure we have enough samples in tx_buffer.
        while self.tx_buffer.len() < TICK_SAMPLES {
            if let Some(msg) = self.tx_pending.pop_front() {
                // Generate idle preamble (10 mark bits ≈ 10 bit-periods).
                // At 300 baud, 8000 Hz: ~267 samples for 10 bits.
                self.tx_fsk.generate_idle(267, &mut self.tx_buffer);
                // Modulate message bytes.
                for &byte in &msg {
                    self.tx_fsk.modulate_byte(byte, &mut self.tx_buffer);
                }
                // Trailing idle: enough mark tone for the remote demodulator
                // to complete detection of the last byte. One full byte
                // period (10 bits × 27 samples = 270) ensures the stop bit
                // and subsequent idle are cleanly received.
                self.tx_fsk.generate_idle(270, &mut self.tx_buffer);
            } else if matches!(self.state, V8State::SendingCjSilence { .. }) {
                // In CJ silence state, generate idle mark tone (the caller
                // is signaling CJ by ceasing CM transmission).
                self.tx_fsk.generate_idle(TICK_SAMPLES, &mut self.tx_buffer);
                break;
            } else {
                // Nothing to send — generate idle mark tone.
                self.tx_fsk.generate_idle(TICK_SAMPLES, &mut self.tx_buffer);
                break;
            }
        }

        // Copy samples to output.
        let n = TICK_SAMPLES.min(self.tx_buffer.len());
        line_out[..n].copy_from_slice(&self.tx_buffer[..n]);
        self.tx_buffer.drain(..n);

        // Fill remainder with silence if somehow short.
        for sample in line_out[n..].iter_mut() {
            *sample = 0;
        }
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
    fn test_capabilities_from_max_speed() {
        let caps = ModemCapabilities::from_max_speed(14400);
        assert!(!caps.v32bis_14400);
        assert!(caps.v32bis_9600);
        assert!(caps.v32bis_4800);
        assert!(!caps.v34_28800);
        assert!(!caps.v90);

        let caps = ModemCapabilities::from_max_speed(28800);
        assert!(!caps.v32bis_14400);
        assert!(caps.v32bis_9600);
        assert!(caps.v32bis_4800);
        assert!(!caps.v34_28800);
        assert!(!caps.v90);

        let caps = ModemCapabilities::from_max_speed(56000);
        assert!(!caps.v32bis_14400);
        assert!(caps.v32bis_9600);
        assert!(caps.v32bis_4800);
        assert!(!caps.v34_28800);
        assert!(!caps.v90);

        let caps = ModemCapabilities::from_max_speed(9600);
        assert!(!caps.v32bis_14400);
        assert!(caps.v32bis_9600);
        assert!(caps.v32bis_4800);
        assert!(!caps.v90);

        let caps = ModemCapabilities::from_max_speed(4800);
        assert!(!caps.v32bis_14400);
        assert!(!caps.v32bis_9600);
        assert!(caps.v32bis_4800);
        assert!(!caps.v90);

        let caps = ModemCapabilities::from_max_speed(2400);
        assert!(!caps.v32bis_14400);
        assert!(!caps.v32bis_9600);
        assert!(!caps.v32bis_4800);
        assert!(!caps.v90);
    }

    #[test]
    fn test_capabilities_negotiate() {
        let a = ModemCapabilities::from_max_speed(14400);
        let b = ModemCapabilities::from_max_speed(14400);
        assert_eq!(a.negotiate(&b), Some(NegotiatedProtocol::V32bis(9600)));

        let a = ModemCapabilities::from_max_speed(14400);
        let b = ModemCapabilities::from_max_speed(9600);
        assert_eq!(a.negotiate(&b), Some(NegotiatedProtocol::V32bis(9600)));

        let a = ModemCapabilities::from_max_speed(7200);
        let b = ModemCapabilities::from_max_speed(12000);
        assert_eq!(a.negotiate(&b), Some(NegotiatedProtocol::V32bis(4800)));
    }

    #[test]
    fn test_capabilities_no_common() {
        // Only 14400 vs only 4800 — no overlap.
        let a = ModemCapabilities {
            v32bis_14400: true,
            v32bis_9600: false,
            v32bis_4800: false,
            v34_28800: false,
            v90: false,
        };
        let b = ModemCapabilities {
            v32bis_14400: false,
            v32bis_9600: false,
            v32bis_4800: true,
            v34_28800: false,
            v90: false,
        };
        assert_eq!(a.negotiate(&b), None);
    }

    #[test]
    fn test_v8_octets_roundtrip() {
        // V.32bis at 14400 — encodes V.32bis group bit, decodes all V.32bis speeds.
        let caps_14400 = ModemCapabilities::from_max_speed(14400);
        let octets = caps_14400.to_v8_octets();
        let decoded = ModemCapabilities::from_v8_octets(&octets).unwrap();
        // V.8 encoding groups all V.32bis speeds into one bit, so decoding
        // enables all V.32bis speeds when any were present.
        assert!(decoded.v32bis_14400);
        assert!(decoded.v32bis_4800);
        assert!(!decoded.v34_28800);
        assert!(!decoded.v90);

        // 28800 max speed falls back to V.32bis advertisement.
        let caps_v34 = ModemCapabilities::from_max_speed(28800);
        let decoded = ModemCapabilities::from_v8_octets(&caps_v34.to_v8_octets()).unwrap();
        assert!(!decoded.v34_28800);
        assert!(decoded.v32bis_14400);
        assert!(!decoded.v90);

        // 56000 max speed also falls back to V.32bis advertisement.
        let caps_v90 = ModemCapabilities::from_max_speed(56000);
        let decoded = ModemCapabilities::from_v8_octets(&caps_v90.to_v8_octets()).unwrap();
        assert!(!decoded.v90);
        assert!(!decoded.v34_28800);
        assert!(decoded.v32bis_14400);

        // Empty capabilities (0 speed).
        let caps_none = ModemCapabilities::from_max_speed(0);
        let octets = caps_none.to_v8_octets();
        let decoded = ModemCapabilities::from_v8_octets(&octets).unwrap();
        assert!(!decoded.v32bis_4800);
        assert!(!decoded.v34_28800);
        assert!(!decoded.v90);
    }

    #[test]
    fn test_v8_caller_answerer_negotiate() {
        let caller_caps = ModemCapabilities::from_max_speed(14400);
        let answerer_caps = ModemCapabilities::from_max_speed(14400);

        let mut caller = V8Negotiator::new(V8Role::Caller, caller_caps);
        let mut answerer = V8Negotiator::new(V8Role::Answerer, answerer_caps);

        let max_ticks = 400; // 4 seconds — well within timeout
        let mut caller_done = false;
        let mut answerer_done = false;

        // Cross-feed with one-tick delay (realistic network propagation).
        let mut prev_caller_out = [0i16; TICK_SAMPLES];
        let mut prev_answerer_out = [0i16; TICK_SAMPLES];

        for _ in 0..max_ticks {
            let mut caller_out = [0i16; TICK_SAMPLES];
            let mut answerer_out = [0i16; TICK_SAMPLES];

            let caller_state = caller.tick(&prev_answerer_out, &mut caller_out);
            let answerer_state = answerer.tick(&prev_caller_out, &mut answerer_out);

            prev_caller_out = caller_out;
            prev_answerer_out = answerer_out;

            if caller_state == V8State::Complete {
                caller_done = true;
            }
            if answerer_state == V8State::Complete {
                answerer_done = true;
            }
            if caller_done && answerer_done {
                break;
            }
        }

        assert!(
            caller_done,
            "Caller should reach Complete state; caller_state={:?} answerer_state={:?} caller_proto={:?} answerer_proto={:?}",
            caller.state(),
            answerer.state(),
            caller.negotiated_protocol(),
            answerer.negotiated_protocol()
        );
        assert!(
            answerer_done,
            "Answerer should reach Complete state; caller_state={:?} answerer_state={:?} caller_proto={:?} answerer_proto={:?}",
            caller.state(),
            answerer.state(),
            caller.negotiated_protocol(),
            answerer.negotiated_protocol()
        );
        assert_eq!(
            caller.negotiated_protocol(),
            Some(NegotiatedProtocol::V32bis(9600))
        );
        assert_eq!(
            answerer.negotiated_protocol(),
            Some(NegotiatedProtocol::V32bis(9600))
        );
    }

    #[test]
    fn test_v8_speed_mismatch() {
        // V.8 negotiates at the modulation-group level (single bit per standard),
        // not individual speeds. Test that when one side supports only V.34 and
        // the other supports only V.32bis, they fall back to V.32bis.
        let mut caller_caps = ModemCapabilities::from_max_speed(0);
        caller_caps.v34_28800 = true; // caller supports V.34 + V.32bis
        caller_caps.v32bis_14400 = true;
        caller_caps.v32bis_9600 = true;
        caller_caps.v32bis_4800 = true;

        let mut answerer_caps = ModemCapabilities::from_max_speed(0);
        answerer_caps.v32bis_14400 = true; // answerer supports only V.32bis
        answerer_caps.v32bis_9600 = true;
        answerer_caps.v32bis_4800 = true;

        let mut caller = V8Negotiator::new(V8Role::Caller, caller_caps);
        let mut answerer = V8Negotiator::new(V8Role::Answerer, answerer_caps);

        // Use proper cross-feeding with one-tick delay.
        let mut prev_caller_out = [0i16; TICK_SAMPLES];
        let mut prev_answerer_out = [0i16; TICK_SAMPLES];

        let max_ticks = 400;
        let mut caller_done = false;
        let mut answerer_done = false;

        for _ in 0..max_ticks {
            let mut caller_out = [0i16; TICK_SAMPLES];
            let mut answerer_out = [0i16; TICK_SAMPLES];

            let cs = caller.tick(&prev_answerer_out, &mut caller_out);
            let as_ = answerer.tick(&prev_caller_out, &mut answerer_out);

            prev_caller_out = caller_out;
            prev_answerer_out = answerer_out;

            if cs == V8State::Complete {
                caller_done = true;
            }
            if as_ == V8State::Complete {
                answerer_done = true;
            }
            if caller_done && answerer_done {
                break;
            }
        }

        assert!(
            caller_done,
            "Caller should reach Complete state; caller_state={:?} answerer_state={:?} caller_proto={:?} answerer_proto={:?}",
            caller.state(),
            answerer.state(),
            caller.negotiated_protocol(),
            answerer.negotiated_protocol()
        );
        assert!(
            answerer_done,
            "Answerer should reach Complete state; caller_state={:?} answerer_state={:?} caller_proto={:?} answerer_proto={:?}",
            caller.state(),
            answerer.state(),
            caller.negotiated_protocol(),
            answerer.negotiated_protocol()
        );
        // Answerer doesn't support V.34, so both fall back to V.32bis(14400).
        assert_eq!(
            caller.negotiated_protocol(),
            Some(NegotiatedProtocol::V32bis(14400))
        );
        assert_eq!(
            answerer.negotiated_protocol(),
            Some(NegotiatedProtocol::V32bis(14400))
        );
    }

    #[test]
    fn test_v8_caller_timeout() {
        let caller_caps = ModemCapabilities::from_max_speed(14400);
        let mut caller = V8Negotiator::new(V8Role::Caller, caller_caps);

        let silence = [0i16; TICK_SAMPLES];
        let mut out = [0i16; TICK_SAMPLES];

        let mut final_state = V8State::Starting;
        for _ in 0..TIMEOUT_TICKS + 10 {
            final_state = caller.tick(&silence, &mut out);
            if final_state == V8State::Failed {
                break;
            }
        }

        assert_eq!(
            final_state,
            V8State::Failed,
            "Caller should timeout and fail"
        );
        assert_eq!(caller.negotiated_protocol(), None);
    }

    #[test]
    fn test_v8_v34_negotiation() {
        let a = ModemCapabilities {
            v32bis_14400: true,
            v32bis_9600: true,
            v32bis_4800: true,
            v34_28800: true,
            v90: false,
        };
        let b = a;
        assert_eq!(a.negotiate(&b), Some(NegotiatedProtocol::V34(28800)));
    }

    #[test]
    fn test_v8_v34_fallback_to_v32bis() {
        // from_max_speed does not advertise the incomplete V.34 data path, so
        // a 28800 max-speed modem falls back to the highest honest V.32bis rate.
        let a = ModemCapabilities::from_max_speed(28800);
        let b = ModemCapabilities::from_max_speed(14400);
        assert_eq!(a.negotiate(&b), Some(NegotiatedProtocol::V32bis(9600)));
    }

    #[test]
    fn test_v90_negotiation() {
        let a = ModemCapabilities {
            v32bis_14400: true,
            v32bis_9600: true,
            v32bis_4800: true,
            v34_28800: true,
            v90: true,
        };
        let b = a;
        assert_eq!(
            a.negotiate(&b),
            Some(NegotiatedProtocol::V90 {
                downstream_bps: 56000,
                upstream_bps: 28800,
            })
        );
    }

    #[test]
    fn test_v90_fallback_to_v34() {
        // Explicit V.90 and V.34 capability sets still negotiate V.34.
        let a = ModemCapabilities {
            v32bis_14400: true,
            v32bis_9600: true,
            v32bis_4800: true,
            v34_28800: true,
            v90: true,
        };
        let b = ModemCapabilities {
            v32bis_14400: true,
            v32bis_9600: true,
            v32bis_4800: true,
            v34_28800: true,
            v90: false,
        };
        assert_eq!(a.negotiate(&b), Some(NegotiatedProtocol::V34(28800)));
    }

    #[test]
    fn test_v8_octets_format() {
        // Verify that Table 3 CM/JM framing octets are present.
        let caps = ModemCapabilities::from_max_speed(14400);
        let octets = caps.to_v8_octets();
        assert_eq!(
            octets.len(),
            5,
            "CM/JM should be sync + callfn + 3 modulation octets"
        );
        assert_eq!(octets[0], V8_CM_JM_SYNC_OCTET);
        assert_eq!(octets[1], V8_CALL_FUNCTION_DATA);
        assert_eq!(octets[2] & 0x1F, V8_MODULATION_TAG);
        assert_eq!(
            octets[3] & V8_MODULATION_EXTENSION_MASK,
            V8_MODULATION_EXTENSION_MATCH
        );
        assert_eq!(
            octets[4] & V8_MODULATION_EXTENSION_MASK,
            V8_MODULATION_EXTENSION_MATCH
        );
    }

    #[test]
    fn test_v8_exact_payload_octets_v32bis_only() {
        // V.32bis only: primary octet has no V.34/V.FC bits; V.32bis is bit 7
        // of the first extension octet → 0x10|0x80 = 0x90.
        let caps = ModemCapabilities::from_max_speed(14400);
        assert_eq!(caps.to_v8_octets(), vec![0xE0, 0xC1, 0x05, 0x90, 0x10]);
    }

    #[test]
    fn test_v8_exact_payload_octets_v34() {
        // from_max_speed(28800) falls back to the highest honest V.32bis
        // implementation; V.34 is not advertised until its data path is honest.
        let caps = ModemCapabilities::from_max_speed(28800);
        assert_eq!(caps.to_v8_octets(), vec![0xE0, 0xC1, 0x05, 0x90, 0x10]);
    }

    #[test]
    fn test_v8_exact_payload_octets_v90() {
        // from_max_speed(56000) also falls back to V.32bis advertisement.
        let caps = ModemCapabilities::from_max_speed(56000);
        assert_eq!(caps.to_v8_octets(), vec![0xE0, 0xC1, 0x05, 0x90, 0x10]);
    }

    #[test]
    fn test_v8_exact_payload_decode_known_messages() {
        // V.32bis only — ext1 bit 7 set (0x90), no V.34/V.FC in primary.
        let caps = ModemCapabilities::from_v8_octets(&[0xE0, 0xC1, 0x05, 0x90, 0x10]).unwrap();
        assert!(caps.v32bis_14400);
        assert!(!caps.v34_28800);
        assert!(!caps.v90);

        // V.34 only — bit 6 in primary, no V.32bis bit in ext1.
        let caps = ModemCapabilities::from_v8_octets(&[0xE0, 0xC1, 0x45, 0x10, 0x10]).unwrap();
        assert!(!caps.v32bis_14400);
        assert!(caps.v34_28800);
        assert!(!caps.v90);

        // V.34 + V.32bis.
        let caps = ModemCapabilities::from_v8_octets(&[0xE0, 0xC1, 0x45, 0x90, 0x10]).unwrap();
        assert!(caps.v32bis_14400);
        assert!(caps.v34_28800);
        assert!(!caps.v90);

        // V.90 + V.34, no V.32bis.
        let caps = ModemCapabilities::from_v8_octets(&[0xE0, 0xC1, 0x65, 0x10, 0x10]).unwrap();
        assert!(!caps.v32bis_14400);
        assert!(caps.v34_28800);
        assert!(caps.v90);

        // V.90 + V.34 + V.32bis.
        let caps = ModemCapabilities::from_v8_octets(&[0xE0, 0xC1, 0x65, 0x90, 0x10]).unwrap();
        assert!(caps.v32bis_14400);
        assert!(caps.v34_28800);
        assert!(caps.v90);
    }

    #[test]
    fn test_v8_exact_table_octets() {
        let v32bis_only = ModemCapabilities {
            v32bis_14400: true,
            v32bis_9600: true,
            v32bis_4800: true,
            v34_28800: false,
            v90: false,
        };
        assert_eq!(
            v32bis_only.to_v8_octets(),
            vec![0xE0, 0xC1, 0x05, 0x90, 0x10]
        );

        let v34_family = ModemCapabilities {
            v32bis_14400: true,
            v32bis_9600: true,
            v32bis_4800: true,
            v34_28800: true,
            v90: false,
        };
        assert_eq!(
            v34_family.to_v8_octets(),
            vec![0xE0, 0xC1, 0x45, 0x90, 0x10]
        );

        let v90_family = ModemCapabilities {
            v32bis_14400: true,
            v32bis_9600: true,
            v32bis_4800: true,
            v34_28800: true,
            v90: true,
        };
        assert_eq!(
            v90_family.to_v8_octets(),
            vec![0xE0, 0xC1, 0x65, 0x90, 0x10]
        );
    }

    #[test]
    fn test_v8_jm_advertises_selected_family_only() {
        let cases = [
            (
                NegotiatedProtocol::V32bis(14400),
                vec![0xE0, 0xC1, 0x05, 0x90, 0x10],
            ),
            (
                NegotiatedProtocol::V34(28800),
                vec![0xE0, 0xC1, 0x45, 0x10, 0x10],
            ),
            (
                NegotiatedProtocol::V90 {
                    downstream_bps: 56000,
                    upstream_bps: 28800,
                },
                vec![0xE0, 0xC1, 0x65, 0x10, 0x10],
            ),
        ];

        for (protocol, expected) in cases {
            let mut answerer =
                V8Negotiator::new(V8Role::Answerer, ModemCapabilities::from_max_speed(56000));
            answerer.negotiated_protocol = Some(protocol);
            answerer.queue_jm();

            let jm = answerer
                .tx_pending
                .pop_front()
                .expect("JM should be queued");
            assert_eq!(jm, expected);
        }
    }

    #[test]
    fn test_v8_octets_non_v8_bytes_ignored() {
        // Non-V.8 bytes (bit 7 = 0) should be ignored during decoding.
        let caps = ModemCapabilities::from_max_speed(14400);
        let mut octets = caps.to_v8_octets();
        // Insert garbage bytes with bit 7 = 0.
        octets.insert(0, 0x00);
        octets.insert(1, 0x42);
        octets.push(0x7F);
        let decoded = ModemCapabilities::from_v8_octets(&octets).unwrap();
        assert!(decoded.v32bis_14400);
        assert!(!decoded.v90);
    }

    #[test]
    fn test_v8_octets_empty_returns_empty_caps() {
        // No V.8 octets at all — returns None (no modulation octet found).
        assert!(ModemCapabilities::from_v8_octets(&[]).is_none());
        // Only non-V.8 bytes.
        assert!(ModemCapabilities::from_v8_octets(&[0x00, 0x42]).is_none());
        // Only call function octet (category 00), no modulation octet.
        assert!(ModemCapabilities::from_v8_octets(&[V8_CALL_FUNCTION_DATA]).is_none());
    }

    #[test]
    fn test_negotiate_symmetric() {
        // Negotiation should be symmetric: A.negotiate(&B) == B.negotiate(&A)
        let a = ModemCapabilities::from_max_speed(14400);
        let b = ModemCapabilities::from_max_speed(9600);
        assert_eq!(a.negotiate(&b), b.negotiate(&a));
    }

    #[test]
    fn test_negotiate_self() {
        // Negotiating with yourself should return the best speed
        let caps = ModemCapabilities::from_max_speed(14400);
        assert_eq!(
            caps.negotiate(&caps),
            Some(NegotiatedProtocol::V32bis(9600))
        );
    }

    #[test]
    fn test_v8_octets_v90_implies_v34() {
        // V.90 capability should also set V.34 in the decoded result.
        let caps = ModemCapabilities {
            v32bis_14400: true,
            v32bis_9600: true,
            v32bis_4800: true,
            v34_28800: true,
            v90: true,
        };
        let octets = caps.to_v8_octets();
        let decoded = ModemCapabilities::from_v8_octets(&octets).unwrap();
        assert!(decoded.v90);
        assert!(decoded.v34_28800, "V.90 must imply V.34");
    }

    #[test]
    fn test_v8_cj_silence_detection() {
        // Verify that the answerer detects CJ via silence (no V.8 octets for
        // CJ_SILENCE_TICKS). This tests the cj_silence_counter logic directly.
        let caps = ModemCapabilities::from_max_speed(14400);
        let mut answerer = V8Negotiator::new(V8Role::Answerer, caps);

        // Manually set the answerer to WaitingForResponse with a negotiated protocol
        // (simulating that CM was received and JM was sent).
        answerer.state = V8State::WaitingForResponse;
        answerer.negotiated_protocol = Some(NegotiatedProtocol::V32bis(14400));

        let silence = [0i16; TICK_SAMPLES];
        let mut out = [0i16; TICK_SAMPLES];

        // Burn through the startup idle period first.
        for _ in 0..STARTUP_IDLE_TICKS + 1 {
            answerer.tick(&silence, &mut out);
        }

        // Tick with silence until CJ is detected. The silence-path is gated
        // on "JM is fully transmitted"; since tx_send_count == 0 here the
        // first tick will queue a JM, JM TX takes ~17 ticks, and the silence
        // counter must then accumulate to CJ_SILENCE_TICKS before Complete
        // fires. Allow a generous margin.
        let mut detected = false;
        for _ in 0..50 {
            let state = answerer.tick(&silence, &mut out);
            if state == V8State::Complete {
                detected = true;
                break;
            }
        }
        assert!(detected, "Answerer should detect CJ via silence");
        assert!(
            answerer.exit_via_silence(),
            "Answerer must exit via the spec-correct silence branch"
        );
    }

    #[test]
    fn test_v8_negotiator_initial_state() {
        let caps = ModemCapabilities::from_max_speed(14400);
        let caller = V8Negotiator::new(V8Role::Caller, caps);
        assert_eq!(caller.state(), V8State::Starting);
        assert_eq!(caller.negotiated_protocol(), None);
    }

    #[test]
    fn test_negotiate_4800_only() {
        let a = ModemCapabilities::from_max_speed(4800);
        let b = ModemCapabilities::from_max_speed(4800);
        assert_eq!(a.negotiate(&b), Some(NegotiatedProtocol::V32bis(4800)));
    }

    #[test]
    fn test_negotiate_7200_falls_back() {
        // 7200 bps lacks an honest data path (HLD S4 §9).
        // from_max_speed(7200) enables only v32bis_4800 (7200 >= 4800, < 9600).
        let a = ModemCapabilities::from_max_speed(7200);
        let b = ModemCapabilities::from_max_speed(7200);
        assert_eq!(a.negotiate(&b), Some(NegotiatedProtocol::V32bis(4800)));
    }

    #[test]
    fn test_negotiate_12000_falls_back() {
        // 12000 bps lacks an honest data path (HLD S4 §9).
        // from_max_speed(12000) enables v32bis_4800 and v32bis_9600 (12000 >= 9600).
        let a = ModemCapabilities::from_max_speed(12000);
        let b = ModemCapabilities::from_max_speed(12000);
        assert_eq!(a.negotiate(&b), Some(NegotiatedProtocol::V32bis(9600)));
    }

    // -------------------------------------------------------------------
    // CI (Call Indicator) tests
    // -------------------------------------------------------------------

    #[test]
    fn test_v8_ci_answerer_sends_ci() {
        // Verify that the answerer generates CI pattern bytes during startup.
        let caps = ModemCapabilities::from_max_speed(14400);
        let mut answerer = V8Negotiator::new(V8Role::Answerer, caps);

        let silence = [0i16; TICK_SAMPLES];
        let mut out = [0i16; TICK_SAMPLES];

        // Tick through enough cycles for CI patterns to be queued and sent.
        // CI_REPETITIONS patterns are queued one per tick during Starting state.
        for _ in 0..(STARTUP_IDLE_TICKS + CI_REPETITIONS + 5) {
            answerer.tick(&silence, &mut out);
        }

        assert_eq!(
            answerer.ci_sent_count(),
            CI_REPETITIONS,
            "Answerer should have sent {} CI patterns",
            CI_REPETITIONS
        );
    }

    #[test]
    fn test_v8_ci_caller_detects_ci() {
        // Feed CI pattern bytes from an answerer to a caller and verify
        // that the caller detects CI.
        let caps = ModemCapabilities::from_max_speed(14400);
        let mut caller = V8Negotiator::new(V8Role::Caller, caps);
        let mut answerer = V8Negotiator::new(V8Role::Answerer, caps);

        let mut prev_answerer_out = [0i16; TICK_SAMPLES];

        // Tick both sides. The answerer sends CI on the answer channel;
        // the caller demodulates the answer channel.
        let max_ticks = STARTUP_IDLE_TICKS + CI_REPETITIONS + 30;
        for _ in 0..max_ticks {
            let mut caller_out = [0i16; TICK_SAMPLES];
            let mut answerer_out = [0i16; TICK_SAMPLES];

            caller.tick(&prev_answerer_out, &mut caller_out);
            answerer.tick(&[0i16; TICK_SAMPLES], &mut answerer_out);

            prev_answerer_out = answerer_out;

            if caller.ci_detected() {
                break;
            }
        }

        assert!(
            caller.ci_detected(),
            "Caller should detect CI from answerer"
        );
    }

    #[test]
    fn test_v8_ci_full_handshake() {
        // Full back-to-back handshake with CI: answerer sends CI, caller
        // detects it, then the normal CM/JM/CJ exchange proceeds.
        let caller_caps = ModemCapabilities::from_max_speed(14400);
        let answerer_caps = ModemCapabilities::from_max_speed(14400);

        let mut caller = V8Negotiator::new(V8Role::Caller, caller_caps);
        let mut answerer = V8Negotiator::new(V8Role::Answerer, answerer_caps);

        let max_ticks = 400;
        let mut caller_done = false;
        let mut answerer_done = false;
        let mut ci_was_detected = false;

        let mut prev_caller_out = [0i16; TICK_SAMPLES];
        let mut prev_answerer_out = [0i16; TICK_SAMPLES];

        for _ in 0..max_ticks {
            let mut caller_out = [0i16; TICK_SAMPLES];
            let mut answerer_out = [0i16; TICK_SAMPLES];

            let caller_state = caller.tick(&prev_answerer_out, &mut caller_out);
            let answerer_state = answerer.tick(&prev_caller_out, &mut answerer_out);

            prev_caller_out = caller_out;
            prev_answerer_out = answerer_out;

            if caller.ci_detected() {
                ci_was_detected = true;
            }
            if caller_state == V8State::Complete {
                caller_done = true;
            }
            if answerer_state == V8State::Complete {
                answerer_done = true;
            }
            if caller_done && answerer_done {
                break;
            }
        }

        assert!(
            ci_was_detected,
            "Caller should have detected CI during handshake"
        );
        assert!(
            caller_done,
            "Caller should reach Complete state; caller_state={:?} answerer_state={:?} caller_proto={:?} answerer_proto={:?}",
            caller.state(),
            answerer.state(),
            caller.negotiated_protocol(),
            answerer.negotiated_protocol()
        );
        assert!(
            answerer_done,
            "Answerer should reach Complete state; caller_state={:?} answerer_state={:?} caller_proto={:?} answerer_proto={:?}",
            caller.state(),
            answerer.state(),
            caller.negotiated_protocol(),
            answerer.negotiated_protocol()
        );
        assert_eq!(
            caller.negotiated_protocol(),
            Some(NegotiatedProtocol::V32bis(9600))
        );
        assert_eq!(
            answerer.negotiated_protocol(),
            Some(NegotiatedProtocol::V32bis(9600))
        );
    }

    // -------------------------------------------------------------------
    // Phase 1b.1 — explicit gap-filling tests called out in the plan.
    // -------------------------------------------------------------------

    #[test]
    fn test_cm_octet_encoding_matches_v8_spec() {
        // V.8 §6.2 / Table 3 CM encoding, verified against spandsp's
        // packed-octet layout (see comments at top of this file).
        //
        // Supported modulations: V.32bis + V.34 (no V.90).
        //   - Primary modulation octet: tag 0x05 + V.34 bit 6 (0x40) = 0x45.
        //   - First extension octet:    marker 0x10 + V.32bis bit 7 (0x80) = 0x90.
        //   - Second extension octet:   marker 0x10, no bits set.
        // Framed by sync 0xE0 and call-function 0xC1 (V-series data call).
        let caps = ModemCapabilities {
            v32bis_14400: true,
            v32bis_9600: true,
            v32bis_4800: true,
            v34_28800: true,
            v90: false,
        };
        let expected = vec![0xE0, 0xC1, 0x45, 0x90, 0x10];
        assert_eq!(
            caps.to_v8_octets(),
            expected,
            "CM octets for V.32bis+V.34 must match V.8 Table 3 / spandsp layout"
        );

        // Round-trip: decoding the expected spec bytes recovers the
        // capability flags that were advertised.
        let decoded = ModemCapabilities::from_v8_octets(&expected).unwrap();
        assert!(decoded.v32bis_14400, "V.32bis must decode");
        assert!(decoded.v34_28800, "V.34 must decode");
        assert!(!decoded.v90, "V.90 must not leak into decoded caps");
    }

    #[test]
    fn test_jm_selects_highest_common_modulation() {
        // Plan requirement: caller CM lists {V.32bis, V.34, V.90}; answerer
        // supports {V.32bis, V.34}. JM must select V.34 (highest common).
        //
        // V.8 §6.3: the answerer's JM advertises the single chosen family.
        let caller_caps = ModemCapabilities {
            v32bis_14400: true,
            v32bis_9600: true,
            v32bis_4800: true,
            v34_28800: true,
            v90: true,
        };
        let answerer_caps = ModemCapabilities {
            v32bis_14400: true,
            v32bis_9600: true,
            v32bis_4800: true,
            v34_28800: true,
            v90: false,
        };

        // Answerer's negotiation picks the highest common family (V.34).
        let negotiated = answerer_caps.negotiate(&caller_caps);
        assert_eq!(
            negotiated,
            Some(NegotiatedProtocol::V34(28800)),
            "Answerer must select V.34 when both sides support V.34 but not V.90"
        );

        // The JM sent by the answerer must advertise V.34 only (not V.90,
        // not V.32bis alone). Verify against the known V.8 Table 3 bytes.
        let mut answerer = V8Negotiator::new(V8Role::Answerer, answerer_caps);
        answerer.negotiated_protocol = negotiated;
        answerer.queue_jm();
        let jm = answerer
            .tx_pending
            .pop_front()
            .expect("JM should be queued");
        assert_eq!(
            jm,
            vec![0xE0, 0xC1, 0x45, 0x10, 0x10],
            "JM must advertise V.34 only (primary bit 6 set, no V.32bis/V.90 bits)"
        );
    }

    // Note: ANSam π phase-reversal behaviour is covered by
    // `test_ansam_phase_reversal` in call_setup.rs:604 (sign-change probe
    // across the reversal boundary). No duplicate test needed here.

    #[test]
    fn test_ci_repetitions_transmitted_before_listen() {
        // Plan requirement: answerer emits all CI_REPETITIONS before
        // transitioning to "listening for CM". We verify two things:
        //   1. After STARTUP_IDLE_TICKS+1, the answerer has queued CI
        //      but has *not* yet reached WaitingForResponse (because no
        //      CM has arrived yet).
        //   2. Feeding CM *before* the answerer finishes sending CI still
        //      results in CI being transmitted first (queue-ordering).
        let caps = ModemCapabilities::from_max_speed(14400);
        let mut answerer = V8Negotiator::new(V8Role::Answerer, caps);

        let silence = [0i16; TICK_SAMPLES];
        let mut out = [0i16; TICK_SAMPLES];

        // Burn through the startup idle period.
        for _ in 0..STARTUP_IDLE_TICKS {
            assert_eq!(answerer.tick(&silence, &mut out), V8State::Starting);
        }

        // One tick past startup: the answerer should queue CI.
        answerer.tick(&silence, &mut out);
        assert_eq!(
            answerer.ci_sent_count(),
            CI_REPETITIONS,
            "Answerer should queue {} CI patterns as soon as startup ends",
            CI_REPETITIONS
        );
        assert_eq!(
            answerer.state(),
            V8State::Starting,
            "Answerer should still be in Starting while CI is being transmitted"
        );
        // Guard against a regression where queue_ci_burst() becomes a no-op
        // (the ci_sent_count check alone only proves the counter was set).
        // The CI burst is ~800 samples and one tick drains only TICK_SAMPLES
        // (80), so tx_buffer must still hold the remaining FSK-modulated
        // CI payload.
        assert!(
            !answerer.tx_buffer.is_empty(),
            "tx_buffer must hold residual CI FSK samples after first post-startup tick"
        );

        // Tick further with silence. The answerer stays in Starting until
        // a CM octet arrives. This proves CI transmission happens before
        // any listen-for-CM transition, not concurrently.
        for _ in 0..CI_REPETITIONS {
            let st = answerer.tick(&silence, &mut out);
            assert_eq!(
                st,
                V8State::Starting,
                "Answerer must remain in Starting while no CM is received"
            );
        }
    }

    #[test]
    fn test_v8_negotiation_b2b_converges_itu_t() {
        // Phase 1b.1 integration test: full caller ↔ answerer V.8 exchange
        // over an in-memory wire. Both sides must produce a NegotiatedProtocol
        // and agree. This replaces any "custom format B2B" test.
        let caller_caps = ModemCapabilities::from_max_speed(28800);
        let answerer_caps = ModemCapabilities::from_max_speed(28800);

        let mut caller = V8Negotiator::new(V8Role::Caller, caller_caps);
        let mut answerer = V8Negotiator::new(V8Role::Answerer, answerer_caps);

        let max_ticks = 400;
        let mut caller_done = false;
        let mut answerer_done = false;

        let mut prev_caller_out = [0i16; TICK_SAMPLES];
        let mut prev_answerer_out = [0i16; TICK_SAMPLES];

        for _ in 0..max_ticks {
            let mut caller_out = [0i16; TICK_SAMPLES];
            let mut answerer_out = [0i16; TICK_SAMPLES];

            let cs = caller.tick(&prev_answerer_out, &mut caller_out);
            let as_ = answerer.tick(&prev_caller_out, &mut answerer_out);

            prev_caller_out = caller_out;
            prev_answerer_out = answerer_out;

            if cs == V8State::Complete {
                caller_done = true;
            }
            if as_ == V8State::Complete {
                answerer_done = true;
            }
            if caller_done && answerer_done {
                break;
            }
        }

        assert!(caller_done && answerer_done, "Both sides must converge");
        assert_eq!(
            caller.negotiated_protocol(),
            answerer.negotiated_protocol(),
            "Both sides must agree on the negotiated protocol"
        );
        assert_eq!(
            caller.negotiated_protocol(),
            Some(NegotiatedProtocol::V32bis(9600)),
            "28800 max speed should fall back to highest honest V.32bis protocol"
        );
    }

    // -------------------------------------------------------------------
    // Step 8a: V.8 phase-transition timing diagnostic
    // -------------------------------------------------------------------
    //
    // Diagnostic test: measures the exact tick at which each side exits V.8
    // in a full B2B handshake with V.34 capability, quantifying the skew
    // and attributing it to specific sub-state budgets.
    //
    // This test is purely observational.  It prints a side-by-side timeline
    // and asserts the skew magnitude as an exact regression guard (not a
    // correctness assertion — the skew is a known asymmetry to be fixed by
    // the team-lead after this diagnostic lands).
    #[test]
    fn test_v8_diagnostic_phase_transition_timing() {
        // Use explicit V.34 capabilities on both sides to match the diagnostic path
        // that produces the reported 52-tick skew.
        let caps = ModemCapabilities {
            v32bis_14400: true,
            v32bis_9600: true,
            v32bis_4800: true,
            v34_28800: true,
            v90: false,
        };

        let mut caller = V8Negotiator::new(V8Role::Caller, caps);
        let mut answerer = V8Negotiator::new(V8Role::Answerer, caps);

        // B2B: each side sees the OTHER side's PREVIOUS tick output.
        let mut prev_caller_out = [0i16; TICK_SAMPLES];
        let mut prev_answerer_out = [0i16; TICK_SAMPLES];

        // Snapshot type: everything we want per tick per side.
        #[derive(Clone)]
        struct TickSnap {
            diag_state: &'static str,
            v8_state: V8State,
            tick_count: u32,
            tx_send_count: u32,
            cj_silence_counter: u32,
            ci_detected: bool,
        }

        let mut timeline: Vec<(TickSnap, TickSnap)> = Vec::new();

        let mut caller_complete_tick: Option<usize> = None;
        let mut answerer_complete_tick: Option<usize> = None;

        let max_ticks = 400;

        for b2b_tick in 0..max_ticks {
            let mut caller_out = [0i16; TICK_SAMPLES];
            let mut answerer_out = [0i16; TICK_SAMPLES];

            let caller_state = caller.tick(&prev_answerer_out, &mut caller_out);
            let answerer_state = answerer.tick(&prev_caller_out, &mut answerer_out);

            prev_caller_out = caller_out;
            prev_answerer_out = answerer_out;

            let c_snap = TickSnap {
                diag_state: caller.diag_state(),
                v8_state: caller_state,
                tick_count: caller.tick_count(),
                tx_send_count: caller.tx_send_count(),
                cj_silence_counter: caller.cj_silence_counter(),
                ci_detected: caller.ci_detected(),
            };
            let a_snap = TickSnap {
                diag_state: answerer.diag_state(),
                v8_state: answerer_state,
                tick_count: answerer.tick_count(),
                tx_send_count: answerer.tx_send_count(),
                cj_silence_counter: answerer.cj_silence_counter(),
                ci_detected: false, // answerer does not use ci_detected
            };

            if caller_state == V8State::Complete && caller_complete_tick.is_none() {
                caller_complete_tick = Some(b2b_tick);
            }
            if answerer_state == V8State::Complete && answerer_complete_tick.is_none() {
                answerer_complete_tick = Some(b2b_tick);
            }

            timeline.push((c_snap, a_snap));

            if caller_complete_tick.is_some() && answerer_complete_tick.is_some() {
                break;
            }
        }

        // ---------------------------------------------------------------
        // Print side-by-side timeline (state transitions only — skip rows
        // where both sides' diag_state is unchanged from previous row to
        // keep output manageable).
        // ---------------------------------------------------------------
        println!(
            "\n[V8DIAG] {:>5}  {:<22} {:>5} {:>4} {:>5}  | {:>5}  {:<22} {:>4} {:>5}",
            "b2bt", "CALLER-state", "tc", "txn", "cjcnt", "b2bt", "ANSWERER-state", "txn", "cjcnt"
        );
        println!("[V8DIAG] {}", "-".repeat(90));

        let mut prev_c_diag = "";
        let mut prev_a_diag = "";

        for (b2b_tick, (c, a)) in timeline.iter().enumerate() {
            let state_changed = c.diag_state != prev_c_diag || a.diag_state != prev_a_diag;
            // Always print Complete rows regardless of state change
            let is_terminal = matches!(c.v8_state, V8State::Complete | V8State::Failed)
                || matches!(a.v8_state, V8State::Complete | V8State::Failed);

            if state_changed || is_terminal {
                let ci_flag = if c.ci_detected { " CI!" } else { "    " };
                println!(
                    "[V8DIAG] {:>5}  {:<22}{} {:>5} {:>4} {:>5}  | {:>5}  {:<22} {:>4} {:>5}",
                    b2b_tick,
                    c.diag_state,
                    ci_flag,
                    c.tick_count,
                    c.tx_send_count,
                    c.cj_silence_counter,
                    b2b_tick,
                    a.diag_state,
                    a.tx_send_count,
                    a.cj_silence_counter,
                );
                prev_c_diag = c.diag_state;
                prev_a_diag = a.diag_state;
            }
        }

        // ---------------------------------------------------------------
        // Summary
        // ---------------------------------------------------------------
        let caller_tick = caller_complete_tick
            .expect("[V8DIAG] FATAL: caller never reached Complete within 400 ticks");
        let answerer_tick = answerer_complete_tick
            .expect("[V8DIAG] FATAL: answerer never reached Complete within 400 ticks");

        let skew = (caller_tick as i64) - (answerer_tick as i64);

        println!();
        println!("[V8DIAG] === SUMMARY ===");
        println!(
            "[V8DIAG] Caller   exits V.8 (Complete) at b2b tick: {}",
            caller_tick
        );
        println!(
            "[V8DIAG] Answerer exits V.8 (Complete) at b2b tick: {}",
            answerer_tick
        );
        println!(
            "[V8DIAG] Skew = caller_tick - answerer_tick = {} ticks",
            skew
        );
        if skew > 0 {
            println!("[V8DIAG] => Answerer completes FIRST by {} ticks", skew);
        } else if skew < 0 {
            println!("[V8DIAG] => Caller completes FIRST by {} ticks", -skew);
        } else {
            println!("[V8DIAG] => Both sides complete on the same tick (zero skew)");
        }

        // ---------------------------------------------------------------
        // Per-substate budget breakdown (derived from timeline)
        // ---------------------------------------------------------------
        // Walk the timeline and accumulate ticks per diag_state per side.
        let mut caller_budget: std::collections::HashMap<&'static str, usize> =
            std::collections::HashMap::new();
        let mut answerer_budget: std::collections::HashMap<&'static str, usize> =
            std::collections::HashMap::new();
        for (c, a) in &timeline {
            *caller_budget.entry(c.diag_state).or_insert(0) += 1;
            *answerer_budget.entry(a.diag_state).or_insert(0) += 1;
        }

        // Print in a deterministic order matching the state-machine flow.
        let ordered_states = &[
            "Starting",
            "Starting(CI-sent)",
            "WaitingForResponse",
            "SendingCjSilence",
            "Complete",
            "Failed",
        ];

        println!();
        println!(
            "[V8DIAG] {:30}  {:>14}  {:>14}  {:>12}",
            "Substate", "Caller ticks", "Answerer ticks", "Asymmetric?"
        );
        println!("[V8DIAG] {}", "-".repeat(74));
        let mut total_c = 0usize;
        let mut total_a = 0usize;
        for &s in ordered_states {
            let c_cnt = caller_budget.get(s).copied().unwrap_or(0);
            let a_cnt = answerer_budget.get(s).copied().unwrap_or(0);
            if c_cnt > 0 || a_cnt > 0 {
                let asym = if c_cnt != a_cnt { "YES" } else { "no" };
                println!(
                    "[V8DIAG] {:30}  {:>14}  {:>14}  {:>12}",
                    s, c_cnt, a_cnt, asym
                );
                total_c += c_cnt;
                total_a += a_cnt;
            }
        }
        println!("[V8DIAG] {}", "-".repeat(74));
        println!(
            "[V8DIAG] {:30}  {:>14}  {:>14}  {:>12}",
            "TOTAL",
            total_c,
            total_a,
            if total_c != total_a { "YES" } else { "no" }
        );

        // ---------------------------------------------------------------
        // Assertions:
        // 1. Both sides must reach Complete.
        // 2. Record the measured skew; do NOT assert it is 0 — the skew is
        //    the bug we are diagnosing.  Assert it is within the expected
        //    range so we detect regressions in either direction.
        // ---------------------------------------------------------------
        assert_eq!(
            caller.negotiated_protocol(),
            Some(NegotiatedProtocol::V34(28800)),
            "Caller must negotiate V.34"
        );
        assert_eq!(
            answerer.negotiated_protocol(),
            Some(NegotiatedProtocol::V34(28800)),
            "Answerer must negotiate V.34"
        );

        // Post-fix expected breakdown (HLD `2026.04.26 - HLD - V8 skew fix.md`):
        //
        // Measured at the V8Negotiator level (both sides starting simultaneously
        // in V.8). Pre-fix skew was +21 ticks (answerer earlier) with the caller
        // burning 20 ticks in `SendingCjSilence` and the answerer threshold at
        // `CJ_SILENCE_TICKS = 20`.
        //
        // Post-fix:
        //   - Caller's `SendingCjSilence` is collapsed to a single tick (the
        //     post-CJ silence Te belongs on the answerer's RX, not the caller's
        //     TX — V.8 §9.3).
        //   - `CJ_SILENCE_TICKS` lowered 20 → 8 (~80 ms, aligned with spec Te).
        //
        // Expected substate budget post-fix:
        //   Starting(no-CI) caller 27 vs answerer 20:    +7 ticks  (CI detection delay)
        //   Starting(CI-sent) caller 0 vs answerer 28:  +28 ticks  (answerer CI burst + CM wait)
        //   WaitingForResponse caller 42 vs answerer 23: caller waits longer for JM-decode
        //                                                  + idle settle; answerer exits as
        //                                                  soon as JM is drained and silence
        //                                                  threshold (8) is met.
        //   SendingCjSilence caller 1 vs answerer 0:    +1 tick   (paranoia margin)
        //   Net skew: |Δ| ≤ 5 ticks, sign may go either way depending on the
        //   exact tick at which the answerer's CJ-silence threshold trips.
        assert!(
            answerer_tick > 0 && caller_tick > 0,
            "Both sides must reach Complete with non-zero ticks; got caller={} answerer={}",
            caller_tick,
            answerer_tick
        );
        assert!(
            skew.abs() <= 5,
            "V.8 skew unexpectedly large ({}); post-fix bound is |skew| <= 5",
            skew
        );

        // §6.2 #8: verify the spec-correct silence path is the active exit on
        // the answerer, not the lenient `MAX_TX_REPEATS` JM-repeat short-circuit.
        // Pre-fix this would fail: the JM-repeat branch (`v8.rs` ~657-665) would
        // win because `CJ_SILENCE_TICKS = 20` exceeded `MAX_TX_REPEATS`-saturation
        // latency. Post-fix the silence threshold (8 ticks) trips first.
        assert!(
            answerer.exit_via_silence(),
            "spec-correct silence path must be the active exit post-fix"
        );

        println!(
            "\n[V8DIAG] Post-fix skew = {} ticks (|skew| <= 5). \
             Answerer exit_via_silence = {} (spec-correct CJ branch).",
            skew,
            answerer.exit_via_silence()
        );
    }
}
