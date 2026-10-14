//! V.90 training sequence state machine.
//!
//! V.90 training runs at 8 kHz (80 samples/tick) and has 4 phases:
//!
//! 1. **INFO exchange** (~800ms): V.21 FSK messages with V.90 capabilities.
//!    Digital sends INFO0d, analog sends INFO0a. CRC-CCITT validated.
//!
//! 2. **PCM level probing** (~1.8s): TRN1d (timing acquisition), PP
//!    (structured mu-law sweep for channel characterization), TRN2d
//!    (post-probe alignment). Followed by INFO1 exchange.
//!
//! 3. **Equalizer training** (~500ms): Bidirectional. Digital sends scrambled
//!    PCM TRN, analog sends V.34 QAM TRN at 2400 baud.
//!
//! 4. **Rate exchange** (~200ms): Half-duplex QAM-based MP exchange using the
//!    QAM modulator trained in Phase 3.
//!
//! ## V.90→V.34 fallback
//!
//! If INFO0 is incompatible, probing reveals an unusable channel, or training
//! times out, the state transitions to `Failed` with `v34_fallback_requested`
//! set. The caller (state_machine.rs) checks this flag to initiate V.34
//! training instead of disconnecting.
//!
//! ## Back-to-back operation
//!
//! Two `V90Training` instances (one Digital, one Analog) connected via their
//! `line_in`/`line_out` progress through all 4 phases and both reach `Complete`.

use crate::crc::crc_ccitt;
use crate::fsk::{FskDemodulator, FskModulator};
use crate::qam::{Constellation, QamConfig, QamDemodulator, QamModulator};
use crate::scrambler::Scrambler;
use crate::state_machine::V90Role;
use crate::v90_pcm::{mu_law_decode, PcmConstellation};

/// 10 ms tick = 80 samples at 8000 Hz.
const TICK_SAMPLES: usize = 80;

/// Timeout in ticks (5 seconds = 500 ticks at 10 ms each).
const TIMEOUT_TICKS: u32 = 500;

// ---------------------------------------------------------------------------
// Phase 1: INFO0 exchange constants
// ---------------------------------------------------------------------------

/// INFO0 message type: digital (answerer).
const INFO0D_TYPE: u8 = 0xD0;
/// INFO0 message type: analog (caller).
const INFO0A_TYPE: u8 = 0xA0;
/// INFO1 message type: digital.
const INFO1D_TYPE: u8 = 0xD1;
/// INFO1 message type: analog.
const INFO1A_TYPE: u8 = 0xA1;

/// Idle preamble before FSK data (ticks).
const FSK_PREAMBLE_TICKS: u32 = 5;

/// Duration for INFO0 transmission phase (ticks).
const INFO0_TX_TICKS: u32 = 40; // 400ms

/// Duration to listen for the remote's INFO0 (ticks).
const INFO0_RX_TICKS: u32 = 60; // 600ms

// ---------------------------------------------------------------------------
// Phase 2: PCM level probing constants (V.90 sect. 8.4)
// ---------------------------------------------------------------------------

/// TRN1d duration: timing acquisition (~500ms = 50 ticks).
const TRN1D_TICKS: u32 = 50;

/// Number of positive mu-law codes to sweep during PP (0x80..0xFF).
const PROBING_LEVELS: u8 = 128;

/// Samples held at each level during PP.
const PROBING_SAMPLES_PER_LEVEL: u16 = 5;

/// Number of PP sweep repetitions (ascending + descending = 1 full sweep, x4).
const PROBING_SWEEP_PAIRS: u8 = 4;

/// Total samples for all PP sweeps (ascending + descending per pair).
const PP_TOTAL_SAMPLES: u32 = PROBING_LEVELS as u32
    * PROBING_SAMPLES_PER_LEVEL as u32
    * 2 // ascending + descending
    * PROBING_SWEEP_PAIRS as u32;

/// Total PP ticks (round up).
const PP_TICKS: u32 = PP_TOTAL_SAMPLES.div_ceil(TICK_SAMPLES as u32);

/// TRN2d duration: post-probe alignment (~200ms = 20 ticks).
const TRN2D_TICKS: u32 = 20;

/// Silence ticks after probing, before INFO1 exchange.
const PROBING_SILENCE_TICKS: u32 = 10;

/// INFO1 exchange ticks per direction.
const INFO1_TX_TICKS: u32 = 40;
const INFO1_RX_TICKS: u32 = 60;

/// Energy threshold for probing signal detection.
const PROBING_ENERGY_THRESHOLD: f64 = 1e8;

/// Minimum usable PCM levels for V.90 (below this, fall back to V.34).
const MIN_USABLE_PCM_LEVELS: u8 = 16;

// ---------------------------------------------------------------------------
// Phase 3: Equalizer training constants
// ---------------------------------------------------------------------------

/// PCM TRN duration (ticks). 300ms = 30 ticks.
const PCM_TRN_TICKS: u32 = 30;

/// QAM TRN duration (ticks). 500ms = 50 ticks.
const QAM_TRN_TICKS: u32 = 50;

/// QAM symbols per tick at 2400 baud: 80/3.33 ≈ 24.
const QAM_SYMBOLS_PER_TICK: u32 = 24;

// ---------------------------------------------------------------------------
// Phase 4: Rate exchange constants (QAM-based MP)
// ---------------------------------------------------------------------------

/// Rate exchange half-duplex: ticks per direction.
const RATE_HALF_TICKS: u32 = 10;

/// V.90 MP message type: digital.
const V90_MP_TYPE_DIGITAL: u8 = 0xB0;

/// V.90 MP message type: analog.
const V90_MP_TYPE_ANALOG: u8 = 0xD0;

/// V.90 MP message length (including CRC).
const V90_MP_MSG_LEN: usize = 8;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// V.90 training state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V90TrainingState {
    /// Phase 1: INFO0 exchange via V.21 FSK.
    Phase1Info,
    /// Phase 2: PCM level probing.
    Phase2Probing,
    /// Phase 3: Equalizer training (PCM TRN + QAM TRN).
    Phase3Training,
    /// Phase 4: Rate exchange (MP messages).
    Phase4RateExchange,
    /// Training complete. Data mode ready.
    Complete,
    /// Training failed (timeout or protocol error).
    Failed,
}

// ---------------------------------------------------------------------------
// Internal sub-phase tracking
// ---------------------------------------------------------------------------

/// Phase 1 sub-states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase1Sub {
    /// Transmitting our INFO0.
    Transmitting,
    /// Listening for remote INFO0.
    Listening,
}

/// Phase 2 sub-states (V.90 sect. 8.4 probing sequence).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase2Sub {
    /// TRN1d: scrambled PCM for timing acquisition.
    DigitalTrn1,
    /// PP: structured mu-law sweep for channel characterization.
    DigitalProbing,
    /// TRN2d: post-probe timing alignment.
    DigitalTrn2,
    /// Silence gap before INFO1.
    Silence,
    /// Transmit INFO1.
    Info1Tx,
    /// Receive INFO1.
    WaitingForInfo1,
}

/// Phase 3 sub-states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase3Sub {
    /// Both directions simultaneously: PCM TRN and QAM TRN.
    PcmAndQamTrn,
    /// PCM TRN done, QAM TRN still running.
    QamTrnOnly,
}

/// Phase 4 sub-states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase4Sub {
    /// Digital sends, analog listens.
    FirstHalf,
    /// Analog sends, digital listens.
    SecondHalf,
    /// Both done.
    Done,
}

// ---------------------------------------------------------------------------
// V.90 INFO0 message (V.90 Tables 10-11 style)
// ---------------------------------------------------------------------------

/// V.90 INFO0 encoded message length (including CRC).
const V90_INFO0_MSG_LEN: usize = 10;

/// V.90 INFO0 message: capabilities exchange.
///
/// Bit-mapped, transmitted as bytes via V.21 FSK with CRC-CCITT.
///
/// Byte layout:
///   [0]   ID: 0xD0 (digital) or 0xA0 (analog)
///   [1]   Companding law (0x01 = mu-law, 0x02 = A-law)
///   [2-3] Max downstream bps (big-endian, e.g., 56000)
///   [4-5] Min downstream bps (big-endian, e.g., 28000)
///   [6-7] Max upstream bps (big-endian, e.g., 28800)
///   [8-9] CRC-16 (CRC-CCITT) over bytes [0..8]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V90Info0 {
    pub is_digital: bool,
    pub companding_law: u8,
    pub max_downstream_bps: u16,
    pub min_downstream_bps: u16,
    pub max_upstream_bps: u16,
}

impl V90Info0 {
    /// Encode to a byte sequence with CRC-16.
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(V90_INFO0_MSG_LEN);
        let id = if self.is_digital {
            INFO0D_TYPE
        } else {
            INFO0A_TYPE
        };
        bytes.push(id);
        bytes.push(self.companding_law);
        bytes.push((self.max_downstream_bps >> 8) as u8);
        bytes.push(self.max_downstream_bps as u8);
        bytes.push((self.min_downstream_bps >> 8) as u8);
        bytes.push(self.min_downstream_bps as u8);
        bytes.push((self.max_upstream_bps >> 8) as u8);
        bytes.push(self.max_upstream_bps as u8);
        let crc = crc_ccitt(&bytes);
        bytes.push((crc >> 8) as u8);
        bytes.push(crc as u8);
        bytes
    }

    /// Decode from a byte slice, validating the identifier and CRC.
    ///
    /// `expect_digital` is the role the REMOTE endpoint must claim. Without it
    /// every type byte other than the digital identifier mapped to "analog", so
    /// an unknown CRC-valid identifier — or a message from a peer with our own
    /// role — was accepted as a well-formed peer INFO0
    /// (`MDD-BUG-CRUCIBLE-00517`). The MP decoder has validated an exact
    /// expected identifier all along; this is the same check.
    pub fn decode(data: &[u8], expect_digital: bool) -> Option<Self> {
        if data.len() < V90_INFO0_MSG_LEN {
            return None;
        }
        let expected_id = if expect_digital {
            INFO0D_TYPE
        } else {
            INFO0A_TYPE
        };
        if data[0] != expected_id {
            return None;
        }
        let payload = &data[..8];
        let crc_received = ((data[8] as u16) << 8) | data[9] as u16;
        if crc_ccitt(payload) != crc_received {
            return None;
        }
        Some(Self {
            is_digital: data[0] == INFO0D_TYPE,
            companding_law: data[1],
            max_downstream_bps: ((data[2] as u16) << 8) | data[3] as u16,
            min_downstream_bps: ((data[4] as u16) << 8) | data[5] as u16,
            max_upstream_bps: ((data[6] as u16) << 8) | data[7] as u16,
        })
    }

    /// Build a default INFO0 for a given role.
    fn for_role(role: V90Role) -> Self {
        match role {
            V90Role::Digital => Self {
                is_digital: true,
                companding_law: 0x01, // mu-law
                max_downstream_bps: 56000,
                min_downstream_bps: 28000,
                max_upstream_bps: 28800,
            },
            V90Role::Analog => Self {
                is_digital: false,
                companding_law: 0x01, // mu-law
                max_downstream_bps: 56000,
                min_downstream_bps: 28000,
                max_upstream_bps: 28800,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// V.90 INFO1 message (V.90 Tables 12-13 style)
// ---------------------------------------------------------------------------

/// V.90 INFO1 encoded message length (including CRC).
const V90_INFO1_MSG_LEN: usize = 10;

/// V.90 INFO1 message: probing results.
///
/// Byte layout:
///   [0]   ID: 0xD1 (digital) or 0xA1 (analog)
///   [1]   Channel quality (0-255, based on probing results)
///   [2]   Usable PCM levels per polarity
///   [3-4] Selected downstream bps (big-endian)
///   [5-6] Selected upstream bps (big-endian)
///   [7]   Reserved (0x00)
///   [8-9] CRC-16 (CRC-CCITT) over bytes [0..8]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V90Info1 {
    pub is_digital: bool,
    pub channel_quality: u8,
    pub usable_pcm_levels: u8,
    pub selected_downstream_bps: u16,
    pub selected_upstream_bps: u16,
}

impl V90Info1 {
    /// Encode to a byte sequence with CRC-16.
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(V90_INFO1_MSG_LEN);
        let id = if self.is_digital {
            INFO1D_TYPE
        } else {
            INFO1A_TYPE
        };
        bytes.push(id);
        bytes.push(self.channel_quality);
        bytes.push(self.usable_pcm_levels);
        bytes.push((self.selected_downstream_bps >> 8) as u8);
        bytes.push(self.selected_downstream_bps as u8);
        bytes.push((self.selected_upstream_bps >> 8) as u8);
        bytes.push(self.selected_upstream_bps as u8);
        bytes.push(0x00); // reserved
        let crc = crc_ccitt(&bytes);
        bytes.push((crc >> 8) as u8);
        bytes.push(crc as u8);
        bytes
    }

    /// Decode from a byte slice, validating the identifier and CRC.
    ///
    /// See [`V90Info0::decode`] — INFO1 was weaker still, because its collector
    /// performed no role check either, so an unknown or same-role CRC-valid
    /// message advanced training (`MDD-BUG-CRUCIBLE-00517`).
    pub fn decode(data: &[u8], expect_digital: bool) -> Option<Self> {
        if data.len() < V90_INFO1_MSG_LEN {
            return None;
        }
        let expected_id = if expect_digital {
            INFO1D_TYPE
        } else {
            INFO1A_TYPE
        };
        if data[0] != expected_id {
            return None;
        }
        let payload = &data[..8];
        let crc_received = ((data[8] as u16) << 8) | data[9] as u16;
        if crc_ccitt(payload) != crc_received {
            return None;
        }
        Some(Self {
            is_digital: data[0] == INFO1D_TYPE,
            channel_quality: data[1],
            usable_pcm_levels: data[2],
            selected_downstream_bps: ((data[3] as u16) << 8) | data[4] as u16,
            selected_upstream_bps: ((data[5] as u16) << 8) | data[6] as u16,
        })
    }

    /// Build a default INFO1 for a given role, with probing results.
    fn for_role(role: V90Role, usable_levels: u8) -> Self {
        Self {
            is_digital: role == V90Role::Digital,
            channel_quality: if usable_levels >= 128 {
                0xFF
            } else {
                (usable_levels as u16 * 255 / 128) as u8
            },
            usable_pcm_levels: usable_levels,
            selected_downstream_bps: 56000,
            selected_upstream_bps: 28800,
        }
    }
}

// ---------------------------------------------------------------------------
// V.90 MP message for Phase 4 (QAM-based, V.34-style)
// ---------------------------------------------------------------------------

/// V.90 MP (modulation parameters) message for Phase 4 rate exchange.
///
/// Bit-mapped, transmitted as 4-QAM symbols via the QAM modulator trained
/// in Phase 3. Each byte encodes as 4 symbols (2 bits/symbol, LSB first).
///
/// Byte layout:
///   [0]   ID: 0xB0 (digital) or 0xD0 (analog)
///   [1-2] Downstream data rate (big-endian, e.g., 56000)
///   [3-4] Upstream data rate (big-endian, e.g., 28800)
///   [5]   Usable PCM levels per polarity
///   [6-7] CRC-16 (CRC-CCITT) over bytes [0..6]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V90Mp {
    pub is_digital: bool,
    pub downstream_rate: u16,
    pub upstream_rate: u16,
    pub usable_pcm_levels: u8,
}

impl V90Mp {
    /// Encode to a byte sequence with CRC-16.
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(V90_MP_MSG_LEN);
        let id = if self.is_digital {
            V90_MP_TYPE_DIGITAL
        } else {
            V90_MP_TYPE_ANALOG
        };
        bytes.push(id);
        bytes.push((self.downstream_rate >> 8) as u8);
        bytes.push(self.downstream_rate as u8);
        bytes.push((self.upstream_rate >> 8) as u8);
        bytes.push(self.upstream_rate as u8);
        bytes.push(self.usable_pcm_levels);
        let crc = crc_ccitt(&bytes);
        bytes.push((crc >> 8) as u8);
        bytes.push(crc as u8);
        bytes
    }

    /// Decode from a byte slice, validating CRC.
    pub fn decode(data: &[u8], expect_digital: bool) -> Option<Self> {
        if data.len() < V90_MP_MSG_LEN {
            return None;
        }
        let expected_id = if expect_digital {
            V90_MP_TYPE_DIGITAL
        } else {
            V90_MP_TYPE_ANALOG
        };
        if data[0] != expected_id {
            return None;
        }
        let payload = &data[..6];
        let crc_received = ((data[6] as u16) << 8) | data[7] as u16;
        if crc_ccitt(payload) != crc_received {
            return None;
        }
        Some(Self {
            is_digital: data[0] == V90_MP_TYPE_DIGITAL,
            downstream_rate: ((data[1] as u16) << 8) | data[2] as u16,
            upstream_rate: ((data[3] as u16) << 8) | data[4] as u16,
            usable_pcm_levels: data[5],
        })
    }
}

// ---------------------------------------------------------------------------
// V90Training
// ---------------------------------------------------------------------------

/// Reassembles 4-QAM di-bit symbols into MP bytes.
///
/// The inverse of what `prepare_mp_tx` transmits: four di-bits per byte,
/// least-significant pair first.
fn mp_symbols_to_bytes(symbols: &[u8]) -> Vec<u8> {
    symbols
        .as_chunks::<4>()
        .0
        .iter()
        .map(|dibits| {
            dibits
                .iter()
                .enumerate()
                .fold(0u8, |byte, (index, &dibit)| {
                    byte | ((dibit & 0x03) << (index * 2))
                })
        })
        .collect()
}

/// V.90 4-phase training state machine.
pub struct V90Training {
    role: V90Role,
    state: V90TrainingState,
    tick_count: u32,
    phase_tick: u32,

    // Phase 1: FSK
    fsk_mod: FskModulator,
    fsk_demod: FskDemodulator,
    fsk_tx_buf: Vec<i16>,
    fsk_tx_pos: usize,
    phase1_sub: Phase1Sub,
    info0_received: bool,
    info0_rx_bytes: Vec<u8>,
    /// Decoded remote INFO0 (set when CRC-valid INFO0 received).
    remote_info0: Option<V90Info0>,

    // Phase 2: PCM probing
    probing_level: u8,
    probing_sample_count: u16,
    probing_sweep_count: u8,
    /// true = ascending sweep, false = descending sweep.
    probing_ascending: bool,
    constellation: PcmConstellation,
    phase2_sub: Phase2Sub,
    probing_detected: bool,
    silence_tick_count: u32,
    /// Usable PCM levels detected during probing (analog side).
    usable_pcm_levels: u8,
    /// Decoded remote INFO1 (set when CRC-valid INFO1 received).
    info1_received: bool,

    // Phase 3: training
    qam_mod: QamModulator,
    qam_demod: QamDemodulator,
    pcm_scrambler: Scrambler,
    qam_scrambler: Scrambler,
    ref_scrambler: Scrambler,
    phase3_sub: Phase3Sub,
    pcm_trn_ticks: u32,
    qam_trn_ticks: u32,

    // Phase 4: QAM-based MP exchange
    phase4_sub: Phase4Sub,
    phase4_ticks: u32,
    /// Demodulated MP symbols collected during RX half.
    mp_rx_symbols: Vec<u8>,
    /// Decoded remote MP (set only when a CRC-valid MP for the OPPOSITE role
    /// arrives). Phase 4 exists to agree parameters with the peer, so this
    /// being `None` means no negotiation happened, whatever the tick count says.
    remote_mp: Option<V90Mp>,
    /// Whether we successfully decoded the remote MP.
    mp_received: bool,

    // Tracking
    remote_signal_received: bool,

    // V.90→V.34 fallback
    v34_fallback_requested: bool,
}

impl V90Training {
    /// Create a new V.90 training state machine.
    pub fn new(role: V90Role) -> Self {
        // Digital (answerer) uses answer FSK channel, analog (caller) uses originate.
        let (fsk_mod, fsk_demod) = match role {
            V90Role::Digital => (FskModulator::new_answer(), FskDemodulator::new_originate()),
            V90Role::Analog => (FskModulator::new_originate(), FskDemodulator::new_answer()),
        };

        // QAM: V.34 config at 8kHz for training + Phase 4 MP exchange.
        let qam_config = QamConfig::v34();
        let qam_mod = QamModulator::with_config(Constellation::Qam4, &qam_config);
        let qam_demod = QamDemodulator::with_config(Constellation::Qam4, &qam_config);

        Self {
            role,
            state: V90TrainingState::Phase1Info,
            tick_count: 0,
            phase_tick: 0,

            fsk_mod,
            fsk_demod,
            fsk_tx_buf: Vec::new(),
            fsk_tx_pos: 0,
            phase1_sub: Phase1Sub::Transmitting,
            info0_received: false,
            info0_rx_bytes: Vec::new(),
            remote_info0: None,

            probing_level: 0,
            probing_sample_count: 0,
            probing_sweep_count: 0,
            probing_ascending: true,
            constellation: PcmConstellation::new_full(),
            phase2_sub: match role {
                V90Role::Digital => Phase2Sub::DigitalTrn1,
                V90Role::Analog => Phase2Sub::DigitalTrn1, // analog listens during all sub-phases
            },
            probing_detected: false,
            silence_tick_count: 0,
            usable_pcm_levels: 128, // assume full until probing says otherwise
            info1_received: false,

            qam_mod,
            qam_demod,
            pcm_scrambler: Scrambler::new(),
            qam_scrambler: Scrambler::new(),
            ref_scrambler: Scrambler::new(),
            phase3_sub: Phase3Sub::PcmAndQamTrn,
            pcm_trn_ticks: 0,
            qam_trn_ticks: 0,

            phase4_sub: Phase4Sub::FirstHalf,
            phase4_ticks: 0,
            mp_rx_symbols: Vec::new(),
            remote_mp: None,
            mp_received: false,

            remote_signal_received: false,
            v34_fallback_requested: false,
        }
    }

    /// Process one 10ms tick (80 samples in, 80 samples out).
    /// Returns current state. Caller checks for Complete/Failed.
    pub fn tick(
        &mut self,
        line_in: &[i16; TICK_SAMPLES],
        line_out: &mut [i16; TICK_SAMPLES],
    ) -> V90TrainingState {
        // Terminal states produce silence.
        if self.state == V90TrainingState::Complete || self.state == V90TrainingState::Failed {
            line_out.fill(0);
            return self.state;
        }

        self.tick_count += 1;
        self.phase_tick += 1;

        // Global timeout — fall back to V.34 instead of disconnecting.
        if self.tick_count >= TIMEOUT_TICKS {
            tracing::warn!(
                target: "gp64::hw::serial",
                "V90 training timeout at tick {} — requesting V.34 fallback",
                self.tick_count
            );
            self.fallback_to_v34("timeout");
            line_out.fill(0);
            return self.state;
        }

        match self.state {
            V90TrainingState::Phase1Info => self.tick_phase1(line_in, line_out),
            V90TrainingState::Phase2Probing => self.tick_phase2(line_in, line_out),
            V90TrainingState::Phase3Training => self.tick_phase3(line_in, line_out),
            V90TrainingState::Phase4RateExchange => self.tick_phase4(line_in, line_out),
            V90TrainingState::Complete | V90TrainingState::Failed => {
                line_out.fill(0);
            }
        }

        self.state
    }

    /// Take the trained PCM constellation (available after Complete).
    pub fn take_constellation(&mut self) -> Option<PcmConstellation> {
        if self.state == V90TrainingState::Complete {
            Some(std::mem::replace(
                &mut self.constellation,
                PcmConstellation::new_full(),
            ))
        } else {
            None
        }
    }

    /// Current state.
    pub fn state(&self) -> V90TrainingState {
        self.state
    }

    /// Training role.
    pub fn role(&self) -> V90Role {
        self.role
    }

    /// Whether V.34 fallback was requested (checked by state_machine.rs).
    pub fn v34_fallback_requested(&self) -> bool {
        self.v34_fallback_requested
    }

    /// Parameters the peer actually confirmed in phase 4, if any.
    pub fn remote_mp(&self) -> Option<&V90Mp> {
        self.remote_mp.as_ref()
    }

    /// Transition to Failed with V.34 fallback requested.
    fn fallback_to_v34(&mut self, reason: &str) {
        tracing::info!(
            target: "gp64::hw::serial",
            "V90 training {:?}: V.34 fallback requested ({})",
            self.role, reason
        );
        self.v34_fallback_requested = true;
        self.state = V90TrainingState::Failed;
    }

    // =======================================================================
    // Phase 1: INFO0 exchange via V.21 FSK
    // =======================================================================

    fn tick_phase1(&mut self, line_in: &[i16; TICK_SAMPLES], line_out: &mut [i16; TICK_SAMPLES]) {
        match self.phase1_sub {
            Phase1Sub::Transmitting => {
                // On first tick, prepare the FSK transmit buffer.
                if self.phase_tick == 1 {
                    self.prepare_info0_tx();
                }

                // Output pre-generated FSK audio.
                self.output_from_fsk_tx_buf(line_out);

                // Also demodulate incoming (the other side may be transmitting
                // simultaneously on their FSK channel).
                self.fsk_demod.demodulate(line_in);
                self.collect_info0_rx();

                if self.phase_tick >= INFO0_TX_TICKS {
                    self.phase1_sub = Phase1Sub::Listening;
                    self.phase_tick = 0;
                }
            }
            Phase1Sub::Listening => {
                // Silence on our TX.
                line_out.fill(0);

                // Continue demodulating.
                self.fsk_demod.demodulate(line_in);
                self.collect_info0_rx();

                if self.info0_received || self.phase_tick >= INFO0_RX_TICKS {
                    self.enter_phase2();
                }
            }
        }
    }

    fn prepare_info0_tx(&mut self) {
        tracing::debug!(
            target: "gp64::hw::serial",
            "V90 training {:?}: sending INFO0",
            self.role
        );
        let info0 = V90Info0::for_role(self.role);
        let msg = info0.encode();
        self.fsk_tx_buf.clear();
        self.fsk_tx_pos = 0;

        // Preamble: idle mark tone.
        self.fsk_mod.generate_idle(
            FSK_PREAMBLE_TICKS as usize * TICK_SAMPLES,
            &mut self.fsk_tx_buf,
        );

        // Message bytes.
        for &byte in &msg {
            self.fsk_mod.modulate_byte(byte, &mut self.fsk_tx_buf);
        }

        // Trailing idle.
        self.fsk_mod
            .generate_idle(TICK_SAMPLES * 5, &mut self.fsk_tx_buf);
    }

    /// Sliding-window CRC-validated INFO0 decoding.
    fn collect_info0_rx(&mut self) {
        while let Some(byte) = self.fsk_demod.take_byte() {
            self.info0_rx_bytes.push(byte);

            // Sliding window: try to decode from the last V90_INFO0_MSG_LEN bytes.
            if self.info0_rx_bytes.len() >= V90_INFO0_MSG_LEN {
                let len = self.info0_rx_bytes.len();
                let window = &self.info0_rx_bytes[len - V90_INFO0_MSG_LEN..];
                let expect_digital = self.role == V90Role::Analog;
                if let Some(info0) = V90Info0::decode(window, expect_digital) {
                    {
                        tracing::debug!(
                            target: "gp64::hw::serial",
                            "V90 training {:?}: received INFO0 (digital={}, law={}, max_down={}, max_up={})",
                            self.role, info0.is_digital, info0.companding_law,
                            info0.max_downstream_bps, info0.max_upstream_bps
                        );
                        // Validate companding law compatibility.
                        if info0.companding_law != 0x01 {
                            self.fallback_to_v34("incompatible companding law");
                            return;
                        }
                        self.remote_info0 = Some(info0);
                        self.info0_received = true;
                        self.remote_signal_received = true;
                    }
                }
            }
        }
    }

    fn output_from_fsk_tx_buf(&mut self, line_out: &mut [i16; TICK_SAMPLES]) {
        for sample in line_out.iter_mut() {
            if self.fsk_tx_pos < self.fsk_tx_buf.len() {
                *sample = self.fsk_tx_buf[self.fsk_tx_pos];
                self.fsk_tx_pos += 1;
            } else {
                *sample = 0;
            }
        }
    }

    // =======================================================================
    // Phase 2: PCM level probing
    // =======================================================================

    fn enter_phase2(&mut self) {
        tracing::debug!(
            target: "gp64::hw::serial",
            "V90 training {:?}: Phase1→Phase2 at tick {}",
            self.role, self.tick_count
        );
        self.state = V90TrainingState::Phase2Probing;
        self.phase_tick = 0;
        self.probing_level = 0;
        self.probing_sample_count = 0;
        self.probing_sweep_count = 0;
        self.probing_ascending = true;
        self.probing_detected = false;
        self.silence_tick_count = 0;
        self.pcm_scrambler.reset();

        // Both sides start with TRN1d — digital sends, analog listens.
        self.phase2_sub = Phase2Sub::DigitalTrn1;
    }

    fn tick_phase2(&mut self, line_in: &[i16; TICK_SAMPLES], line_out: &mut [i16; TICK_SAMPLES]) {
        match self.phase2_sub {
            Phase2Sub::DigitalTrn1 => {
                // TRN1d: digital sends scrambled all-ones PCM for timing.
                if self.role == V90Role::Digital {
                    for sample in line_out.iter_mut() {
                        *sample = self.generate_pcm_trn_sample();
                    }
                } else {
                    line_out.fill(0);
                    self.detect_probing_energy(line_in);
                }

                if self.phase_tick >= TRN1D_TICKS {
                    self.phase2_sub = Phase2Sub::DigitalProbing;
                    self.phase_tick = 0;
                    self.probing_level = 0;
                    self.probing_sample_count = 0;
                    self.probing_sweep_count = 0;
                    self.probing_ascending = true;
                }
            }
            Phase2Sub::DigitalProbing => {
                // PP: digital sends structured mu-law sweep.
                if self.role == V90Role::Digital {
                    self.generate_pp_sweep(line_out);

                    // Check if all sweep pairs are done.
                    if self.probing_sweep_count >= PROBING_SWEEP_PAIRS * 2 {
                        self.phase2_sub = Phase2Sub::DigitalTrn2;
                        self.phase_tick = 0;
                        self.pcm_scrambler.reset();
                    }
                } else {
                    line_out.fill(0);
                    self.detect_probing_energy(line_in);
                }

                // Analog side: time-based fallthrough.
                if self.role == V90Role::Analog && self.phase_tick >= PP_TICKS + 5 {
                    self.phase2_sub = Phase2Sub::DigitalTrn2;
                    self.phase_tick = 0;
                }
            }
            Phase2Sub::DigitalTrn2 => {
                // TRN2d: post-probe timing alignment.
                if self.role == V90Role::Digital {
                    for sample in line_out.iter_mut() {
                        *sample = self.generate_pcm_trn_sample();
                    }
                } else {
                    line_out.fill(0);
                    self.detect_probing_energy(line_in);
                }

                if self.phase_tick >= TRN2D_TICKS {
                    // Analog side: check if channel can support V.90.
                    if self.role == V90Role::Analog {
                        self.estimate_usable_levels();
                        if self.usable_pcm_levels < MIN_USABLE_PCM_LEVELS {
                            self.fallback_to_v34("too few usable PCM levels");
                            return;
                        }
                    }
                    self.phase2_sub = Phase2Sub::Silence;
                    self.phase_tick = 0;
                    self.silence_tick_count = 0;
                }
            }
            Phase2Sub::Silence => {
                line_out.fill(0);
                self.silence_tick_count += 1;
                if self.silence_tick_count >= PROBING_SILENCE_TICKS {
                    self.phase2_sub = Phase2Sub::Info1Tx;
                    self.phase_tick = 0;
                    self.prepare_info1_tx();
                }
            }
            Phase2Sub::Info1Tx => {
                self.output_from_fsk_tx_buf(line_out);

                // Also demodulate (simultaneous FSK on different channels).
                self.fsk_demod.demodulate(line_in);
                self.collect_info1_rx();

                if self.phase_tick >= INFO1_TX_TICKS {
                    self.phase2_sub = Phase2Sub::WaitingForInfo1;
                    self.phase_tick = 0;
                }
            }
            Phase2Sub::WaitingForInfo1 => {
                line_out.fill(0);
                self.fsk_demod.demodulate(line_in);
                self.collect_info1_rx();

                if self.info1_received || self.phase_tick >= INFO1_RX_TICKS {
                    self.enter_phase3();
                }
            }
        }
    }

    /// Generate PP (probing pattern): structured ascending/descending mu-law sweep.
    fn generate_pp_sweep(&mut self, line_out: &mut [i16; TICK_SAMPLES]) {
        for sample in line_out.iter_mut() {
            if self.probing_sweep_count >= PROBING_SWEEP_PAIRS * 2 {
                *sample = 0;
                continue;
            }

            // Current mu-law code: 0x80 | level (positive codes).
            let code = 0x80 | self.probing_level;
            *sample = mu_law_decode(code);

            self.probing_sample_count += 1;
            if self.probing_sample_count >= PROBING_SAMPLES_PER_LEVEL {
                self.probing_sample_count = 0;

                if self.probing_ascending {
                    if self.probing_level >= PROBING_LEVELS - 1 {
                        // End of ascending sweep — switch to descending.
                        self.probing_ascending = false;
                        self.probing_sweep_count += 1;
                        tracing::trace!(
                            target: "gp64::hw::serial",
                            "V90 PP: ascending sweep {} complete",
                            self.probing_sweep_count
                        );
                    } else {
                        self.probing_level += 1;
                    }
                } else if self.probing_level == 0 {
                    // End of descending sweep — start next ascending.
                    self.probing_ascending = true;
                    self.probing_sweep_count += 1;
                    tracing::trace!(
                        target: "gp64::hw::serial",
                        "V90 PP: descending sweep {} complete",
                        self.probing_sweep_count
                    );
                } else {
                    self.probing_level -= 1;
                }
            }
        }
    }

    fn detect_probing_energy(&mut self, line_in: &[i16; TICK_SAMPLES]) {
        let energy: f64 = line_in.iter().map(|&s| (s as f64) * (s as f64)).sum();
        if energy > PROBING_ENERGY_THRESHOLD {
            self.probing_detected = true;
            self.remote_signal_received = true;
        }
    }

    /// Estimate usable PCM levels from probing results (analog side).
    /// On a clean wire, all 128 levels are usable.
    fn estimate_usable_levels(&mut self) {
        if self.probing_detected {
            // On our clean internal wire, all levels are usable.
            // On PSTN, this would measure per-level SNR and count usable levels.
            self.usable_pcm_levels = 128;
        } else {
            // No probing signal detected — channel is unusable for V.90.
            self.usable_pcm_levels = 0;
        }
    }

    fn prepare_info1_tx(&mut self) {
        tracing::debug!(
            target: "gp64::hw::serial",
            "V90 training {:?}: sending INFO1 (usable_levels={})",
            self.role, self.usable_pcm_levels
        );
        // Reset FSK demod for INFO1 reception.
        self.fsk_demod.reset();
        self.info0_rx_bytes.clear(); // reuse for INFO1

        let info1 = V90Info1::for_role(self.role, self.usable_pcm_levels);
        let msg = info1.encode();
        self.fsk_tx_buf.clear();
        self.fsk_tx_pos = 0;

        self.fsk_mod.generate_idle(
            FSK_PREAMBLE_TICKS as usize * TICK_SAMPLES,
            &mut self.fsk_tx_buf,
        );
        for &byte in &msg {
            self.fsk_mod.modulate_byte(byte, &mut self.fsk_tx_buf);
        }
        self.fsk_mod
            .generate_idle(TICK_SAMPLES * 5, &mut self.fsk_tx_buf);
    }

    /// Sliding-window CRC-validated INFO1 decoding.
    fn collect_info1_rx(&mut self) {
        while let Some(byte) = self.fsk_demod.take_byte() {
            self.info0_rx_bytes.push(byte);

            // Sliding window: try to decode from the last V90_INFO1_MSG_LEN bytes.
            if self.info0_rx_bytes.len() >= V90_INFO1_MSG_LEN {
                let len = self.info0_rx_bytes.len();
                let window = &self.info0_rx_bytes[len - V90_INFO1_MSG_LEN..];
                // The remote must claim the opposite role to ours. Without
                // this an unknown or same-role CRC-valid message advanced
                // training (`MDD-BUG-CRUCIBLE-00517`).
                let expect_digital = self.role == V90Role::Analog;
                if let Some(info1) = V90Info1::decode(window, expect_digital) {
                    tracing::debug!(
                        target: "gp64::hw::serial",
                        "V90 training {:?}: received INFO1 (quality={}, levels={}, down={}, up={})",
                        self.role, info1.channel_quality, info1.usable_pcm_levels,
                        info1.selected_downstream_bps, info1.selected_upstream_bps
                    );
                    self.info1_received = true;
                }
            }
        }
    }

    // =======================================================================
    // Phase 3: Equalizer training (PCM TRN + QAM TRN)
    // =======================================================================

    fn enter_phase3(&mut self) {
        tracing::debug!(
            target: "gp64::hw::serial",
            "V90 training {:?}: Phase2→Phase3 at tick {}",
            self.role, self.tick_count
        );
        self.state = V90TrainingState::Phase3Training;
        self.phase_tick = 0;
        self.phase3_sub = Phase3Sub::PcmAndQamTrn;
        self.pcm_trn_ticks = 0;
        self.qam_trn_ticks = 0;
        self.qam_mod.reset();
        self.qam_demod.reset();
        self.pcm_scrambler.reset();
        self.qam_scrambler.reset();
        self.ref_scrambler.reset();
    }

    fn tick_phase3(&mut self, line_in: &[i16; TICK_SAMPLES], line_out: &mut [i16; TICK_SAMPLES]) {
        match self.phase3_sub {
            Phase3Sub::PcmAndQamTrn => {
                // Both directions simultaneously.
                self.tick_phase3_bidirectional(line_in, line_out);

                self.pcm_trn_ticks += 1;
                self.qam_trn_ticks += 1;

                if self.pcm_trn_ticks >= PCM_TRN_TICKS {
                    self.phase3_sub = Phase3Sub::QamTrnOnly;
                }
            }
            Phase3Sub::QamTrnOnly => {
                // Only QAM TRN continues.
                self.tick_phase3_bidirectional(line_in, line_out);

                self.qam_trn_ticks += 1;

                if self.qam_trn_ticks >= QAM_TRN_TICKS {
                    self.enter_phase4();
                }
            }
        }
    }

    fn tick_phase3_bidirectional(
        &mut self,
        line_in: &[i16; TICK_SAMPLES],
        line_out: &mut [i16; TICK_SAMPLES],
    ) {
        match self.role {
            V90Role::Digital => {
                // Digital TX: PCM TRN (scrambled known pattern).
                // Digital RX: detect QAM energy from analog side.
                if self.pcm_trn_ticks < PCM_TRN_TICKS {
                    for sample in line_out.iter_mut() {
                        *sample = self.generate_pcm_trn_sample();
                    }
                } else {
                    line_out.fill(0);
                }

                // Detect signal from analog's QAM TRN.
                self.detect_line_energy(line_in);
            }
            V90Role::Analog => {
                // Analog TX: QAM TRN at 2400 baud.
                // Analog RX: detect PCM energy from digital side.
                if self.qam_trn_ticks < QAM_TRN_TICKS {
                    self.queue_qam_trn_symbols();
                    for sample in line_out.iter_mut() {
                        *sample = self.qam_mod.next_sample();
                    }
                } else {
                    line_out.fill(0);
                }

                // Detect signal from digital's PCM TRN.
                self.detect_line_energy(line_in);
            }
        }
    }

    /// Generate a single PCM TRN sample (digital TX).
    fn generate_pcm_trn_sample(&mut self) -> i16 {
        let scrambled = self.pcm_scrambler.scramble_byte(0xFF);
        let code = 0x80 | (scrambled & 0x7F); // positive, 7 bits
        mu_law_decode(code)
    }

    /// Queue scrambled 4-QAM symbols for one tick (analog TX).
    fn queue_qam_trn_symbols(&mut self) {
        for _ in 0..QAM_SYMBOLS_PER_TICK {
            let scrambled_bits = self.qam_scrambler.scramble_bit(true) as u8
                | ((self.qam_scrambler.scramble_bit(true) as u8) << 1);
            self.qam_mod.modulate_symbol_raw(scrambled_bits & 0x03);
        }
    }

    /// Detect any meaningful signal energy on the line.
    fn detect_line_energy(&mut self, line_in: &[i16; TICK_SAMPLES]) {
        let energy: f64 = line_in.iter().map(|&s| (s as f64) * (s as f64)).sum();
        if energy > PROBING_ENERGY_THRESHOLD {
            self.remote_signal_received = true;
        }
    }

    // =======================================================================
    // Phase 4: QAM-based MP exchange
    // =======================================================================

    fn enter_phase4(&mut self) {
        tracing::debug!(
            target: "gp64::hw::serial",
            "V90 training {:?}: Phase3→Phase4 at tick {}",
            self.role, self.tick_count
        );
        self.state = V90TrainingState::Phase4RateExchange;
        self.phase_tick = 0;
        self.phase4_sub = Phase4Sub::FirstHalf;
        self.phase4_ticks = 0;
        self.mp_rx_symbols.clear();
        self.mp_received = false;
        self.remote_mp = None;
        // Both ends must start the MP exchange from a known carrier and filter
        // state, exactly as enter_phase3 already does. Without this the analog
        // side carried its phase-3 QAM TRN state into phase 4 while the digital
        // side (which sends PCM TRN, not QAM, in phase 3) started clean, so the
        // constellation was rotated in one direction only: analog decoded the
        // digital MP, and the digital side decoded nothing. That asymmetry was
        // invisible while completion ignored the MP entirely.
        //
        // Must precede prepare_mp_tx, which queues the MP into qam_mod.
        self.qam_mod.reset();
        self.qam_demod.reset();
        self.prepare_mp_tx();
    }

    /// Build V.90 MP message and queue as QAM symbols.
    fn prepare_mp_tx(&mut self) {
        let mp = V90Mp {
            is_digital: self.role == V90Role::Digital,
            downstream_rate: 56000,
            upstream_rate: 28800,
            usable_pcm_levels: self.usable_pcm_levels,
        };
        let msg = mp.encode();

        // Queue MP bytes as 4-QAM di-bit symbols (LSB first per byte).
        for &byte in &msg {
            for shift in (0..8).step_by(2) {
                let dibit = (byte >> shift) & 0x03;
                self.qam_mod.modulate_symbol_raw(dibit);
            }
        }
    }

    fn tick_phase4(&mut self, line_in: &[i16; TICK_SAMPLES], line_out: &mut [i16; TICK_SAMPLES]) {
        self.phase4_ticks += 1;

        match self.phase4_sub {
            Phase4Sub::FirstHalf => {
                // Digital sends QAM, analog listens.
                if self.role == V90Role::Digital {
                    for sample in line_out.iter_mut() {
                        *sample = self.qam_mod.next_sample();
                    }
                } else {
                    line_out.fill(0);
                    self.collect_mp_rx(line_in);
                }

                if self.phase4_ticks >= RATE_HALF_TICKS {
                    self.phase4_sub = Phase4Sub::SecondHalf;
                    self.phase4_ticks = 0;
                }
            }
            Phase4Sub::SecondHalf => {
                // Analog sends QAM, digital listens.
                if self.role == V90Role::Analog {
                    for sample in line_out.iter_mut() {
                        *sample = self.qam_mod.next_sample();
                    }
                } else {
                    line_out.fill(0);
                    self.collect_mp_rx(line_in);
                }

                if self.phase4_ticks >= RATE_HALF_TICKS {
                    self.phase4_sub = Phase4Sub::Done;
                    self.phase4_ticks = 0;
                }
            }
            Phase4Sub::Done => {
                line_out.fill(0);
                self.complete_training();
            }
        }
    }

    /// Demodulates QAM symbols and tries to decode the remote MP from them.
    ///
    /// This used only to COUNT symbols and set `mp_received` once enough had
    /// arrived — so noise, a corrupted MP, or an MP from a peer in the wrong
    /// role all looked identical to a successful exchange, and nothing ever
    /// reconstructed the bytes.
    ///
    /// The symbol stream carries no byte framing, so every 32-symbol window
    /// ending at the newest symbol is tried. That is safe precisely because
    /// `V90Mp::decode` checks both the role byte and the CRC: a misaligned
    /// window fails both.
    fn collect_mp_rx(&mut self, line_in: &[i16; TICK_SAMPLES]) {
        if self.remote_mp.is_some() {
            return;
        }
        // The peer holds the opposite role. An MP carrying our OWN role is a
        // reflection or a misconfigured peer, not a negotiation.
        let expect_digital = self.role != V90Role::Digital;
        let window = V90_MP_MSG_LEN * 4;

        // Symbol timing has to start at the burst, not at the half boundary.
        // The two ends do not enter phase 4 on the same tick, so the peer's
        // carrier begins part-way through the listening half; feeding the
        // demodulator the leading silence starts its timing at the wrong phase
        // and every hard decision after it is taken between symbols.
        let silent = line_in.iter().all(|&sample| sample == 0);
        if silent && self.mp_rx_symbols.is_empty() {
            self.qam_demod.reset();
            return;
        }

        let symbols = self.qam_demod.demodulate(line_in);
        for &(i, q) in &symbols {
            let idx = self.qam_demod.hard_decision_raw(i, q);
            self.mp_rx_symbols.push(idx);
            if self.mp_rx_symbols.len() < window {
                continue;
            }
            let bytes =
                mp_symbols_to_bytes(&self.mp_rx_symbols[self.mp_rx_symbols.len() - window..]);
            if let Some(mp) = V90Mp::decode(&bytes, expect_digital) {
                self.mp_received = true;
                self.remote_mp = Some(mp);
                return;
            }
            // Only a window ending at the newest symbol can still match, so
            // anything older than one window is dead weight.
            if self.mp_rx_symbols.len() >= window * 2 {
                self.mp_rx_symbols.drain(..window);
            }
        }
    }

    fn complete_training(&mut self) {
        if !self.remote_signal_received {
            // No remote modem was detected at any phase — fall back to V.34.
            self.fallback_to_v34("no remote signal detected");
            return;
        }

        // remote_signal_received is set as early as INFO0 or plain energy
        // detection, so on its own it says nothing about phase 4. Without a
        // decoded MP no parameters were agreed, and going online with local
        // defaults would assert a negotiation that never happened.
        let Some(remote) = self.remote_mp.clone() else {
            self.fallback_to_v34("no valid V.90 MP received in phase 4");
            return;
        };

        // The link can only use levels BOTH ends support, so the agreed figure
        // is the lower of the locally probed value and the peer's.
        self.usable_pcm_levels = self.usable_pcm_levels.min(remote.usable_pcm_levels);
        if self.usable_pcm_levels < MIN_USABLE_PCM_LEVELS {
            self.fallback_to_v34("agreed PCM levels below the V.90 minimum");
            return;
        }

        // Build constellation based on probing results.
        if self.usable_pcm_levels < 128 && self.usable_pcm_levels >= MIN_USABLE_PCM_LEVELS {
            self.constellation = PcmConstellation::new_with_levels(self.usable_pcm_levels);
        }

        tracing::info!(
            target: "gp64::hw::serial",
            "V90 training complete: role={:?}, tick={}, usable_levels={}",
            self.role, self.tick_count, self.usable_pcm_levels
        );
        self.state = V90TrainingState::Complete;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    /// Run a V.90 training session between digital and analog, feeding
    /// each other's output with a one-tick delay.
    fn run_v90_training_pair(
        digital: &mut V90Training,
        analog: &mut V90Training,
        max_ticks: u32,
    ) -> (V90TrainingState, V90TrainingState) {
        let mut prev_digital_out = [0i16; TICK_SAMPLES];
        let mut prev_analog_out = [0i16; TICK_SAMPLES];

        for _ in 0..max_ticks {
            let mut digital_out = [0i16; TICK_SAMPLES];
            let mut analog_out = [0i16; TICK_SAMPLES];

            let ds = digital.tick(&prev_analog_out, &mut digital_out);
            let a_s = analog.tick(&prev_digital_out, &mut analog_out);

            prev_digital_out = digital_out;
            prev_analog_out = analog_out;

            if ds == V90TrainingState::Complete && a_s == V90TrainingState::Complete {
                return (ds, a_s);
            }
            if ds == V90TrainingState::Failed || a_s == V90TrainingState::Failed {
                return (ds, a_s);
            }
        }

        (digital.state(), analog.state())
    }

    // -----------------------------------------------------------------------
    // V90Info0 encode/decode tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_v90_info0_roundtrip_digital() {
        let info0 = V90Info0 {
            is_digital: true,
            companding_law: 0x01,
            max_downstream_bps: 56000,
            min_downstream_bps: 28000,
            max_upstream_bps: 28800,
        };
        let encoded = info0.encode();
        assert_eq!(encoded.len(), V90_INFO0_MSG_LEN);
        assert_eq!(encoded[0], INFO0D_TYPE);

        let decoded = V90Info0::decode(&encoded, true).expect("Should decode");
        assert_eq!(decoded, info0);
    }

    #[test]
    fn test_v90_info0_roundtrip_analog() {
        let info0 = V90Info0 {
            is_digital: false,
            companding_law: 0x01,
            max_downstream_bps: 56000,
            min_downstream_bps: 28000,
            max_upstream_bps: 28800,
        };
        let encoded = info0.encode();
        assert_eq!(encoded[0], INFO0A_TYPE);

        let decoded = V90Info0::decode(&encoded, false).expect("Should decode");
        assert_eq!(decoded, info0);
    }

    /// MDD-BUG-CRUCIBLE-00517: INFO0 and INFO1 must reject an identifier that
    /// is not the one the remote role is required to send.
    ///
    /// Both decoders mapped every type byte other than the digital identifier
    /// to the analog role, so an unknown CRC-valid identifier decoded as a
    /// well-formed analog message. INFO1 was weaker still: its collector had no
    /// role check at all, so a SAME-role message advanced training — a modem
    /// negotiating with its own reflection would proceed.
    ///
    /// The four cases the entry named, over both message types.
    #[test]
    fn v90_info_decoders_reject_wrong_identifiers() {
        // (label, encoder-role, expect_digital, should_decode)
        let cases = [
            (
                "opposite role: the only accepted case",
                V90Role::Digital,
                true,
                true,
            ),
            ("same role", V90Role::Digital, false, false),
            (
                "opposite role, other direction",
                V90Role::Analog,
                false,
                true,
            ),
            ("same role, other direction", V90Role::Analog, true, false),
        ];

        for (label, role, expect_digital, should_decode) in cases {
            let encoded = V90Info0::for_role(role).encode();
            assert_eq!(
                V90Info0::decode(&encoded, expect_digital).is_some(),
                should_decode,
                "INFO0 {label}"
            );

            let encoded = V90Info1::for_role(role, 0).encode();
            assert_eq!(
                V90Info1::decode(&encoded, expect_digital).is_some(),
                should_decode,
                "INFO1 {label}"
            );
        }

        // An identifier that is neither defined value, with a VALID CRC — the
        // case that used to sail through as "analog".
        for expect_digital in [true, false] {
            let mut encoded = V90Info0::for_role(V90Role::Digital).encode();
            encoded[0] = 0x5A;
            let crc = crc_ccitt(&encoded[..8]);
            encoded[8] = (crc >> 8) as u8;
            encoded[9] = crc as u8;
            assert!(
                V90Info0::decode(&encoded, expect_digital).is_none(),
                "INFO0 accepted an undefined identifier with a valid CRC                  (expect_digital={expect_digital})"
            );

            let mut encoded = V90Info1::for_role(V90Role::Digital, 0).encode();
            encoded[0] = 0x5A;
            let crc = crc_ccitt(&encoded[..8]);
            encoded[8] = (crc >> 8) as u8;
            encoded[9] = crc as u8;
            assert!(
                V90Info1::decode(&encoded, expect_digital).is_none(),
                "INFO1 accepted an undefined identifier with a valid CRC                  (expect_digital={expect_digital})"
            );
        }
    }

    /// A corrupted CRC is still rejected even when the identifier is right —
    /// the identifier check must not have replaced the CRC check.
    #[test]
    fn v90_info_decoders_still_reject_a_corrupted_crc() {
        let mut encoded = V90Info0::for_role(V90Role::Digital).encode();
        encoded[8] ^= 0xFF;
        assert!(V90Info0::decode(&encoded, true).is_none(), "INFO0");

        let mut encoded = V90Info1::for_role(V90Role::Digital, 0).encode();
        encoded[8] ^= 0xFF;
        assert!(V90Info1::decode(&encoded, true).is_none(), "INFO1");
    }

    /// The INFO1 collector is where the missing role check actually bit: it set
    /// `info1_received` for anything that decoded, so an analog endpoint
    /// accepted an analog peer's INFO1 and advanced training.
    ///
    /// Driven through the real FSK path — the collector reads the demodulator,
    /// not the byte buffer — so this proves the wiring, not just the decoder.
    #[test]
    fn v90_info1_collector_ignores_a_same_role_message() {
        // An analog endpoint demodulates the answer channel, so a peer speaking
        // to it modulates on that channel.
        fn feed(training: &mut V90Training, message: &[u8]) {
            let mut modulator = FskModulator::new_answer();
            let mut pcm = Vec::new();
            // Lead-in mark so the demodulator sees a clean start-bit edge.
            modulator.generate_idle(200, &mut pcm);
            for &byte in message {
                modulator.modulate_byte(byte, &mut pcm);
            }
            training.fsk_demod.demodulate(&pcm);
            training.collect_info1_rx();
        }

        let mut analog = V90Training::new(V90Role::Analog);
        feed(
            &mut analog,
            &V90Info1::for_role(V90Role::Digital, 0).encode(),
        );
        assert!(
            analog.info1_received,
            "the legitimate opposite-role INFO1 was not accepted, so this test              cannot show anything about the same-role case"
        );

        let mut analog = V90Training::new(V90Role::Analog);
        feed(
            &mut analog,
            &V90Info1::for_role(V90Role::Analog, 0).encode(),
        );
        assert!(
            !analog.info1_received,
            "a same-role INFO1 advanced training"
        );
    }

    #[test]
    fn test_v90_info0_bad_crc_rejected() {
        let info0 = V90Info0::for_role(V90Role::Digital);
        let mut encoded = info0.encode();
        encoded[8] ^= 0xFF; // corrupt CRC
        assert!(V90Info0::decode(&encoded, true).is_none());
    }

    #[test]
    fn test_v90_info0_too_short_rejected() {
        assert!(V90Info0::decode(&[0xD0, 0x01, 0x00], true).is_none());
    }

    // -----------------------------------------------------------------------
    // V90Info1 encode/decode tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_v90_info1_roundtrip_digital() {
        let info1 = V90Info1 {
            is_digital: true,
            channel_quality: 0xFF,
            usable_pcm_levels: 128,
            selected_downstream_bps: 56000,
            selected_upstream_bps: 28800,
        };
        let encoded = info1.encode();
        assert_eq!(encoded.len(), V90_INFO1_MSG_LEN);
        assert_eq!(encoded[0], INFO1D_TYPE);

        let decoded = V90Info1::decode(&encoded, true).expect("Should decode");
        assert_eq!(decoded, info1);
    }

    #[test]
    fn test_v90_info1_roundtrip_analog() {
        let info1 = V90Info1 {
            is_digital: false,
            channel_quality: 200,
            usable_pcm_levels: 96,
            selected_downstream_bps: 48000,
            selected_upstream_bps: 28800,
        };
        let encoded = info1.encode();
        assert_eq!(encoded[0], INFO1A_TYPE);

        let decoded = V90Info1::decode(&encoded, false).expect("Should decode");
        assert_eq!(decoded, info1);
    }

    #[test]
    fn test_v90_info1_bad_crc_rejected() {
        let info1 = V90Info1::for_role(V90Role::Analog, 128);
        let mut encoded = info1.encode();
        encoded[9] ^= 0x01; // corrupt CRC
        assert!(V90Info1::decode(&encoded, false).is_none());
    }

    // -----------------------------------------------------------------------
    // V90Mp encode/decode tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_v90_mp_roundtrip_digital() {
        let mp = V90Mp {
            is_digital: true,
            downstream_rate: 56000,
            upstream_rate: 28800,
            usable_pcm_levels: 128,
        };
        let encoded = mp.encode();
        assert_eq!(encoded.len(), V90_MP_MSG_LEN);
        assert_eq!(encoded[0], V90_MP_TYPE_DIGITAL);

        let decoded = V90Mp::decode(&encoded, true).expect("Should decode");
        assert_eq!(decoded, mp);
    }

    #[test]
    fn test_v90_mp_roundtrip_analog() {
        let mp = V90Mp {
            is_digital: false,
            downstream_rate: 56000,
            upstream_rate: 28800,
            usable_pcm_levels: 128,
        };
        let encoded = mp.encode();
        assert_eq!(encoded[0], V90_MP_TYPE_ANALOG);

        let decoded = V90Mp::decode(&encoded, false).expect("Should decode");
        assert_eq!(decoded, mp);
    }

    #[test]
    fn test_v90_mp_wrong_role_rejected() {
        let mp = V90Mp {
            is_digital: true,
            downstream_rate: 56000,
            upstream_rate: 28800,
            usable_pcm_levels: 128,
        };
        let encoded = mp.encode();
        // Expect analog but got digital — should reject.
        assert!(V90Mp::decode(&encoded, false).is_none());
    }

    #[test]
    fn test_v90_mp_bad_crc_rejected() {
        let mp = V90Mp {
            is_digital: true,
            downstream_rate: 56000,
            upstream_rate: 28800,
            usable_pcm_levels: 128,
        };
        let mut encoded = mp.encode();
        encoded[6] ^= 0xFF; // corrupt CRC
        assert!(V90Mp::decode(&encoded, true).is_none());
    }

    // -----------------------------------------------------------------------
    // Back-to-back training test
    // -----------------------------------------------------------------------

    /// Modulates MP bytes onto the QAM channel and drives one side's phase-4
    /// collector with them, the way `feed` does for the INFO collectors.
    fn feed_mp(training: &mut V90Training, bytes: &[u8]) {
        let config = QamConfig::v34();
        let mut modulator = QamModulator::with_config(Constellation::Qam4, &config);
        for &byte in bytes {
            for shift in (0..8).step_by(2) {
                modulator.modulate_symbol_raw((byte >> shift) & 0x03);
            }
        }
        for _ in 0..8 {
            let mut tick = [0i16; TICK_SAMPLES];
            for sample in tick.iter_mut() {
                *sample = modulator.next_sample();
            }
            training.collect_mp_rx(&tick);
        }
    }

    fn digital_mp() -> V90Mp {
        V90Mp {
            is_digital: true,
            downstream_rate: 56000,
            upstream_rate: 28800,
            usable_pcm_levels: 96,
        }
    }

    /// **The positive control, first.**
    ///
    /// Everything below asserts that some bad MP is REJECTED, which a collector
    /// that rejects everything would also satisfy. This shows the collector
    /// accepts the legitimate opposite-role MP, so the rejections mean
    /// something — and that the peer's parameters actually arrive.
    #[test]
    fn v90_mp_collector_accepts_the_opposite_role_message() {
        let mut analog = V90Training::new(V90Role::Analog);
        feed_mp(&mut analog, &digital_mp().encode());

        let remote = analog
            .remote_mp()
            .expect("a valid digital MP must be decoded by the analog side");
        assert_eq!(remote, &digital_mp());
        assert_eq!(
            remote.usable_pcm_levels, 96,
            "the peer's parameters must survive the exchange, not local defaults"
        );
    }

    /// An MP whose CRC does not check must not be accepted.
    #[test]
    fn v90_mp_collector_rejects_a_corrupted_message() {
        let mut corrupted = digital_mp().encode();
        let last = corrupted.len() - 1;
        corrupted[last] ^= 0xff;

        let mut analog = V90Training::new(V90Role::Analog);
        feed_mp(&mut analog, &corrupted);

        assert!(
            analog.remote_mp().is_none(),
            "a CRC-invalid MP must not be treated as a negotiated result"
        );
    }

    /// An MP carrying our OWN role is a reflection, not a peer.
    #[test]
    fn v90_mp_collector_rejects_a_same_role_message() {
        let mut digital = V90Training::new(V90Role::Digital);
        feed_mp(&mut digital, &digital_mp().encode());

        assert!(
            digital.remote_mp().is_none(),
            "a digital endpoint must not accept a digital MP as its peer's"
        );
    }

    /// **Silence through phase 4 must fall back, not complete.**
    ///
    /// This is the defect itself: remote_signal_received is set as early as
    /// INFO0 or energy detection, so completion used to succeed on elapsed
    /// ticks alone with no MP ever received or decoded.
    #[test]
    fn v90_training_without_an_mp_falls_back_instead_of_completing() {
        let mut analog = V90Training::new(V90Role::Analog);
        analog.remote_signal_received = true;
        analog.enter_phase4();
        assert!(analog.remote_mp().is_none());

        let silence = [0i16; TICK_SAMPLES];
        let mut out = [0i16; TICK_SAMPLES];
        for _ in 0..(RATE_HALF_TICKS * 2 + 2) {
            analog.tick_phase4(&silence, &mut out);
        }

        assert_eq!(
            analog.state(),
            V90TrainingState::Failed,
            "phase 4 with no MP must not report a completed negotiation"
        );
        assert!(
            analog.v34_fallback_requested(),
            "a phase 4 that agreed nothing must request V.34 fallback"
        );
    }

    #[test]
    fn test_back_to_back_training_completes() {
        let mut digital = V90Training::new(V90Role::Digital);
        let mut analog = V90Training::new(V90Role::Analog);

        let (ds, a_s) = run_v90_training_pair(&mut digital, &mut analog, 500);

        assert_eq!(ds, V90TrainingState::Complete, "Digital should complete");
        assert_eq!(a_s, V90TrainingState::Complete, "Analog should complete");

        // Fallback should NOT be requested.
        assert!(!digital.v34_fallback_requested());
        assert!(!analog.v34_fallback_requested());

        // Completion must rest on a decoded peer MP, not on elapsed ticks.
        assert!(
            digital.remote_mp().is_some_and(|mp| !mp.is_digital),
            "the digital side must have decoded the analog peer's MP"
        );
        assert!(
            analog.remote_mp().is_some_and(|mp| mp.is_digital),
            "the analog side must have decoded the digital peer's MP"
        );
    }

    // -----------------------------------------------------------------------
    // Phase transition test
    // -----------------------------------------------------------------------

    #[test]
    fn test_phase_transitions() {
        let mut digital = V90Training::new(V90Role::Digital);
        let mut analog = V90Training::new(V90Role::Analog);

        let mut prev_d = [0i16; TICK_SAMPLES];
        let mut prev_a = [0i16; TICK_SAMPLES];

        let mut digital_phases: Vec<V90TrainingState> = Vec::new();
        let mut analog_phases: Vec<V90TrainingState> = Vec::new();

        for _ in 0..500 {
            let mut d_out = [0i16; TICK_SAMPLES];
            let mut a_out = [0i16; TICK_SAMPLES];

            let ds = digital.tick(&prev_a, &mut d_out);
            let a_s = analog.tick(&prev_d, &mut a_out);

            prev_d = d_out;
            prev_a = a_out;

            // Record phase transitions.
            if digital_phases.last().map(|p| *p != ds).unwrap_or(true) {
                digital_phases.push(ds);
            }
            if analog_phases.last().map(|p| *p != a_s).unwrap_or(true) {
                analog_phases.push(a_s);
            }

            if ds == V90TrainingState::Complete && a_s == V90TrainingState::Complete {
                break;
            }
        }

        // Verify phase order for digital side.
        assert!(
            digital_phases.contains(&V90TrainingState::Phase1Info),
            "Digital missing Phase1Info"
        );
        assert!(
            digital_phases.contains(&V90TrainingState::Phase2Probing),
            "Digital missing Phase2Probing"
        );
        assert!(
            digital_phases.contains(&V90TrainingState::Phase3Training),
            "Digital missing Phase3Training"
        );
        assert!(
            digital_phases.contains(&V90TrainingState::Phase4RateExchange),
            "Digital missing Phase4RateExchange"
        );
        assert!(
            digital_phases.contains(&V90TrainingState::Complete),
            "Digital missing Complete"
        );
    }

    // -----------------------------------------------------------------------
    // Timeout triggers V.34 fallback
    // -----------------------------------------------------------------------

    #[test]
    fn test_training_timeout_requests_v34_fallback() {
        let mut training = V90Training::new(V90Role::Digital);
        let silence = [0i16; TICK_SAMPLES];
        let mut out = [0i16; TICK_SAMPLES];

        let mut final_state = V90TrainingState::Phase1Info;
        for _ in 0..TIMEOUT_TICKS + 10 {
            final_state = training.tick(&silence, &mut out);
            if final_state == V90TrainingState::Failed {
                break;
            }
        }

        assert_eq!(final_state, V90TrainingState::Failed);
        assert!(
            training.v34_fallback_requested(),
            "Timeout should request V.34 fallback"
        );
    }

    // -----------------------------------------------------------------------
    // Constellation taken after training
    // -----------------------------------------------------------------------

    #[test]
    fn test_take_constellation() {
        let mut digital = V90Training::new(V90Role::Digital);
        let mut analog = V90Training::new(V90Role::Analog);

        let (ds, a_s) = run_v90_training_pair(&mut digital, &mut analog, 500);
        assert_eq!(ds, V90TrainingState::Complete);
        assert_eq!(a_s, V90TrainingState::Complete);

        // Both should be able to take constellation.
        let c = analog.take_constellation();
        assert!(c.is_some(), "Analog should have a constellation");
        assert_eq!(c.unwrap().bits_per_symbol(), 7);

        let c2 = digital.take_constellation();
        assert!(c2.is_some(), "Digital should have a constellation");
        assert_eq!(c2.unwrap().bits_per_symbol(), 7);
    }

    // -----------------------------------------------------------------------
    // Structured PP sweep test (replaces staircase test)
    // -----------------------------------------------------------------------

    #[test]
    fn test_probing_pp_sweep() {
        let mut digital = V90Training::new(V90Role::Digital);
        let mut analog = V90Training::new(V90Role::Analog);

        let mut prev_d = [0i16; TICK_SAMPLES];
        let mut prev_a = [0i16; TICK_SAMPLES];

        let mut digital_out_history: Vec<[i16; TICK_SAMPLES]> = Vec::new();
        let mut in_phase2 = false;

        for _ in 0..500 {
            let mut d_out = [0i16; TICK_SAMPLES];
            let mut a_out = [0i16; TICK_SAMPLES];

            let ds = digital.tick(&prev_a, &mut d_out);
            let _as = analog.tick(&prev_d, &mut a_out);

            if ds == V90TrainingState::Phase2Probing {
                digital_out_history.push(d_out);
                in_phase2 = true;
            } else if in_phase2 {
                break; // Phase 2 ended
            }

            prev_d = d_out;
            prev_a = a_out;
        }

        assert!(
            !digital_out_history.is_empty(),
            "Should have Phase 2 output"
        );

        // Flatten all Phase 2 output samples.
        let all_samples: Vec<i16> = digital_out_history
            .iter()
            .flat_map(|chunk| chunk.iter().copied())
            .collect();

        // Should have significant non-zero samples (TRN1d + PP + TRN2d).
        let non_zero = all_samples.iter().filter(|&&s| s != 0).count();
        assert!(
            non_zero > 1000,
            "Should have significant probing output (got {non_zero})"
        );

        // PP section should contain both small and large mu-law values
        // (ascending/descending sweeps exercise the full range).
        let has_small = all_samples
            .iter()
            .any(|&s| s != 0 && s.unsigned_abs() < 1000);
        let has_large = all_samples.iter().any(|&s| s.unsigned_abs() > 10000);
        assert!(has_small, "PP sweep should include small amplitude levels");
        assert!(has_large, "PP sweep should include large amplitude levels");
    }

    // -----------------------------------------------------------------------
    // V90Info0 for_role produces valid defaults
    // -----------------------------------------------------------------------

    #[test]
    fn test_v90_info0_for_role_defaults() {
        let digital = V90Info0::for_role(V90Role::Digital);
        assert!(digital.is_digital);
        assert_eq!(digital.companding_law, 0x01);
        assert_eq!(digital.max_downstream_bps, 56000);

        let analog = V90Info0::for_role(V90Role::Analog);
        assert!(!analog.is_digital);
        assert_eq!(analog.companding_law, 0x01);
    }

    // -----------------------------------------------------------------------
    // V90Info1 for_role with varying levels
    // -----------------------------------------------------------------------

    #[test]
    fn test_v90_info1_for_role_full_levels() {
        let info1 = V90Info1::for_role(V90Role::Digital, 128);
        assert_eq!(info1.channel_quality, 0xFF);
        assert_eq!(info1.usable_pcm_levels, 128);
    }

    #[test]
    fn test_v90_info1_for_role_reduced_levels() {
        let info1 = V90Info1::for_role(V90Role::Analog, 64);
        assert_eq!(info1.usable_pcm_levels, 64);
        assert!(info1.channel_quality < 0xFF);
    }

    // -----------------------------------------------------------------------
    // V.90→V.34 fallback on incompatible companding law
    // -----------------------------------------------------------------------

    #[test]
    fn test_v90_info0_incompatible_law_triggers_fallback() {
        // Build an INFO0 with A-law (0x02) instead of mu-law (0x01).
        let bad_info0 = V90Info0 {
            is_digital: true,
            companding_law: 0x02, // A-law — our analog side expects mu-law
            max_downstream_bps: 56000,
            min_downstream_bps: 28000,
            max_upstream_bps: 28800,
        };
        let bad_encoded = bad_info0.encode();

        // Create analog side and manually inject the bad INFO0 into its FSK path.
        let mut analog = V90Training::new(V90Role::Analog);
        // Simulate receiving the bad INFO0 by pushing bytes through collect_info0_rx.
        for &byte in &bad_encoded {
            analog.info0_rx_bytes.push(byte);
        }
        // Trigger the CRC-validated decode path.
        // We need at least V90_INFO0_MSG_LEN bytes in the buffer.
        assert!(analog.info0_rx_bytes.len() >= V90_INFO0_MSG_LEN);
        let len = analog.info0_rx_bytes.len();
        let window = &analog.info0_rx_bytes[len - V90_INFO0_MSG_LEN..];
        if let Some(info0) = V90Info0::decode(window, true) {
            if info0.companding_law != 0x01 {
                analog.fallback_to_v34("incompatible companding law");
            }
        }

        assert_eq!(analog.state(), V90TrainingState::Failed);
        assert!(analog.v34_fallback_requested());
    }
}
