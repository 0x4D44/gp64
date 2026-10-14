//! Virtual Modem Emulation (Hayes-compatible AT command set)
//!
//! Provides Hayes-compatible modem emulation that can be attached to a serial port.
//! Supports the standard AT command set for BBS software and terminal programs.

use std::collections::VecDeque;

/// S-register indices for common modem settings
pub mod sregs {
    /// Auto-answer ring count (0 = disabled)
    pub const S0: usize = 0;
    /// Ring counter (internal)
    pub const S1: usize = 1;
    /// Escape character (default: '+' = 43)
    pub const S2: usize = 2;
    /// Carriage return character (default: 13)
    pub const S3: usize = 3;
    /// Line feed character (default: 10)
    pub const S4: usize = 4;
    /// Backspace character (default: 8)
    pub const S5: usize = 5;
    /// Wait for dial tone (seconds)
    pub const S6: usize = 6;
    /// Wait for carrier (seconds)
    pub const S7: usize = 7;
    /// Pause time for comma (seconds)
    pub const S8: usize = 8;
    /// Carrier detect response time (1/10 sec)
    pub const S9: usize = 9;
    /// Carrier loss hangup delay (1/10 sec)
    pub const S10: usize = 10;
    /// DTMF tone duration (milliseconds)
    pub const S11: usize = 11;
    /// Escape code guard time (1/50 sec)
    pub const S12: usize = 12;
}

/// Modem result codes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultCode {
    Ok,
    Connect,
    Ring,
    NoCarrier,
    Error,
    Connect1200,
    NoDialtone,
    Busy,
    NoAnswer,
    Connect2400,
    Connect4800,
    Connect9600,
    Connect14400,
    Connect19200,
    Connect28800,
    Connect38400,
    Connect56000,
    Connect57600,
    Connect115200,
}

impl ResultCode {
    /// Get the numeric code
    pub fn numeric(&self) -> u8 {
        match self {
            ResultCode::Ok => 0,
            ResultCode::Connect => 1,
            ResultCode::Ring => 2,
            ResultCode::NoCarrier => 3,
            ResultCode::Error => 4,
            ResultCode::Connect1200 => 5,
            ResultCode::NoDialtone => 6,
            ResultCode::Busy => 7,
            ResultCode::NoAnswer => 8,
            ResultCode::Connect2400 => 10,
            ResultCode::Connect4800 => 11,
            ResultCode::Connect9600 => 12,
            ResultCode::Connect14400 => 13,
            ResultCode::Connect19200 => 14,
            ResultCode::Connect28800 => 18,
            ResultCode::Connect38400 => 28,
            ResultCode::Connect56000 => 20,
            ResultCode::Connect57600 => 86,
            ResultCode::Connect115200 => 87,
        }
    }

    /// Get the verbose text
    pub fn verbose(&self) -> &'static str {
        match self {
            ResultCode::Ok => "OK",
            ResultCode::Connect => "CONNECT",
            ResultCode::Ring => "RING",
            ResultCode::NoCarrier => "NO CARRIER",
            ResultCode::Error => "ERROR",
            ResultCode::Connect1200 => "CONNECT 1200",
            ResultCode::NoDialtone => "NO DIALTONE",
            ResultCode::Busy => "BUSY",
            ResultCode::NoAnswer => "NO ANSWER",
            ResultCode::Connect2400 => "CONNECT 2400",
            ResultCode::Connect4800 => "CONNECT 4800",
            ResultCode::Connect9600 => "CONNECT 9600",
            ResultCode::Connect14400 => "CONNECT 14400",
            ResultCode::Connect19200 => "CONNECT 19200",
            ResultCode::Connect28800 => "CONNECT 28800",
            ResultCode::Connect38400 => "CONNECT 38400",
            ResultCode::Connect56000 => "CONNECT 56000",
            ResultCode::Connect57600 => "CONNECT 57600",
            ResultCode::Connect115200 => "CONNECT 115200",
        }
    }

    /// Whether this is a speed-specific CONNECT variant (Connect1200, etc.).
    /// Used by the X register logic: X0 downgrades these to plain `Connect`.
    fn is_connect_speed(&self) -> bool {
        matches!(
            self,
            ResultCode::Connect1200
                | ResultCode::Connect2400
                | ResultCode::Connect4800
                | ResultCode::Connect9600
                | ResultCode::Connect14400
                | ResultCode::Connect19200
                | ResultCode::Connect28800
                | ResultCode::Connect38400
                | ResultCode::Connect56000
                | ResultCode::Connect57600
                | ResultCode::Connect115200
        )
    }
}

/// Modem model (determines speed, handshake timing, and training profile)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModemModel {
    /// V.22bis
    Modem2400,
    /// V.32
    Modem9600,
    /// V.32bis
    Modem14400,
    /// V.34
    Modem28800,
    /// V.90
    Modem56k,
}

impl ModemModel {
    /// Maximum line speed in bits per second
    pub fn max_speed(&self) -> u32 {
        match self {
            ModemModel::Modem2400 => 2400,
            ModemModel::Modem9600 => 9600,
            ModemModel::Modem14400 => 14400,
            ModemModel::Modem28800 => 28800,
            ModemModel::Modem56k => 56000,
        }
    }

    /// Duration of the handshake/training sequence in milliseconds
    pub fn handshake_duration_ms(&self) -> u32 {
        match self {
            ModemModel::Modem2400 => 800,
            ModemModel::Modem9600 => 2000,
            ModemModel::Modem14400 => 3000,
            ModemModel::Modem28800 => 5000,
            ModemModel::Modem56k => 8000,
        }
    }

    /// Throughput in bytes per second (max_speed / 10 for 8N1 framing)
    pub fn bytes_per_sec(&self) -> u32 {
        self.max_speed() / 10
    }

    /// Human-readable display name
    pub fn display_name(&self) -> &'static str {
        match self {
            ModemModel::Modem2400 => "2400",
            ModemModel::Modem9600 => "9600",
            ModemModel::Modem14400 => "14400",
            ModemModel::Modem28800 => "28800",
            ModemModel::Modem56k => "56K",
        }
    }

    /// Training segment profile for audio generation during handshake
    pub fn training_segments(&self) -> &'static [TrainingSegment] {
        static SEGMENTS_2400: &[TrainingSegment] = &[
            TrainingSegment::Tone {
                freq_hz: 2100.0,
                duration_ms: 500,
            },
            TrainingSegment::Tone {
                freq_hz: 1200.0,
                duration_ms: 300,
            },
        ];
        static SEGMENTS_9600: &[TrainingSegment] = &[
            TrainingSegment::Tone {
                freq_hz: 2100.0,
                duration_ms: 400,
            },
            TrainingSegment::Sweep {
                start_hz: 600.0,
                end_hz: 3000.0,
                duration_ms: 800,
            },
            TrainingSegment::Noise {
                bandwidth_hz: 3000.0,
                duration_ms: 800,
            },
        ];
        static SEGMENTS_14400: &[TrainingSegment] = &[
            TrainingSegment::Tone {
                freq_hz: 2100.0,
                duration_ms: 400,
            },
            TrainingSegment::Sweep {
                start_hz: 600.0,
                end_hz: 3000.0,
                duration_ms: 600,
            },
            TrainingSegment::Noise {
                bandwidth_hz: 3000.0,
                duration_ms: 800,
            },
            TrainingSegment::Sweep {
                start_hz: 3000.0,
                end_hz: 600.0,
                duration_ms: 600,
            },
            TrainingSegment::Noise {
                bandwidth_hz: 3000.0,
                duration_ms: 600,
            },
        ];
        static SEGMENTS_28800: &[TrainingSegment] = &[
            TrainingSegment::Tone {
                freq_hz: 2100.0,
                duration_ms: 300,
            },
            TrainingSegment::Tone {
                freq_hz: 600.0,
                duration_ms: 300,
            },
            TrainingSegment::Tone {
                freq_hz: 1200.0,
                duration_ms: 300,
            },
            TrainingSegment::Tone {
                freq_hz: 1800.0,
                duration_ms: 300,
            },
            TrainingSegment::Tone {
                freq_hz: 2400.0,
                duration_ms: 300,
            },
            TrainingSegment::Tone {
                freq_hz: 3000.0,
                duration_ms: 300,
            },
            TrainingSegment::Sweep {
                start_hz: 600.0,
                end_hz: 3400.0,
                duration_ms: 1000,
            },
            TrainingSegment::Noise {
                bandwidth_hz: 3400.0,
                duration_ms: 2200,
            },
        ];
        // V.90-structured handshake approximation (total = 8000ms):
        //   Phase 1 (V.8 ANSam):      2100 Hz answer tone, ~2.5s
        //   Phase 1 (V.8 FSK CM/JM):  alternating 1650/1850 Hz, ~500ms
        //   Phase 2 (INFO exchange):   V.21 low-channel FSK, 400ms
        //   Phase 2 (PCM probing):     rising sweep 300-3400 Hz, 1.5s
        //   Phase 3 (equalizer TRN):   wideband noise, 1.5s
        //   Phase 4 (rate exchange):   V.21 FSK, 200ms
        //   Final convergence:         wideband noise tail, 1.4s
        static SEGMENTS_56K: &[TrainingSegment] = &[
            // Phase 1: V.8 ANSam answer tone (real is ~3s with phase reversals)
            TrainingSegment::Tone {
                freq_hz: 2100.0,
                duration_ms: 2500,
            },
            // Phase 1: V.8 FSK capability exchange (CM/JM)
            TrainingSegment::Tone {
                freq_hz: 1650.0,
                duration_ms: 250,
            },
            TrainingSegment::Tone {
                freq_hz: 1850.0,
                duration_ms: 250,
            },
            // Phase 2: INFO exchange via V.21 low-channel FSK
            TrainingSegment::Tone {
                freq_hz: 980.0,
                duration_ms: 400,
            },
            // Phase 2: PCM probing staircase (rising sweep approximation)
            TrainingSegment::Sweep {
                start_hz: 300.0,
                end_hz: 3400.0,
                duration_ms: 1500,
            },
            // Phase 3: Bidirectional equalizer training (TRN)
            TrainingSegment::Noise {
                bandwidth_hz: 3400.0,
                duration_ms: 1500,
            },
            // Phase 4: Rate exchange FSK
            TrainingSegment::Tone {
                freq_hz: 980.0,
                duration_ms: 200,
            },
            // Final training convergence
            TrainingSegment::Noise {
                bandwidth_hz: 3400.0,
                duration_ms: 1400,
            },
        ];
        match self {
            ModemModel::Modem2400 => SEGMENTS_2400,
            ModemModel::Modem9600 => SEGMENTS_9600,
            ModemModel::Modem14400 => SEGMENTS_14400,
            ModemModel::Modem28800 => SEGMENTS_28800,
            ModemModel::Modem56k => SEGMENTS_56K,
        }
    }
}

/// Handshake state machine phase
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakePhase {
    /// No handshake in progress
    Idle,
    /// Playing dial tone before dialing
    DialTone { remaining_ms: u32 },
    /// Playing DTMF digit
    Dtmf {
        digit_index: usize,
        remaining_ms: u32,
    },
    /// Gap between DTMF digits
    DtmfGap {
        digit_index: usize,
        remaining_ms: u32,
    },
    /// Ringing the remote end (outgoing call)
    Ringing { remaining_ms: u32, tone_on: bool },
    /// Incoming ring (auto-answer)
    IncomingRing {
        ring_count: u8,
        remaining_ms: u32,
        ri_high: bool,
    },
    /// Waiting for carrier detect
    WaitingForCarrier { remaining_ms: u32 },
    /// Modem training/negotiation phase
    Training {
        remaining_ms: u32,
        carrier_detected: bool,
    },
    /// Handshake completed successfully
    Complete,
}

/// A segment of the modem training/handshake audio profile
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrainingSegment {
    /// Pure tone at a fixed frequency
    Tone { freq_hz: f32, duration_ms: u32 },
    /// Frequency sweep from start to end
    Sweep {
        start_hz: f32,
        end_hz: f32,
        duration_ms: u32,
    },
    /// Band-limited noise
    Noise { bandwidth_hz: f32, duration_ms: u32 },
    /// Silence gap
    Silence { duration_ms: u32 },
}

/// Modem operating mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModemMode {
    /// Command mode - interpreting AT commands
    Command,
    /// Online data mode - passing data through
    Online,
    /// Online command mode - temporarily in command mode while connected
    OnlineCommand,
}

/// Distinguishes caller (ATD) from answerer (ATA / incoming ring)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeRole {
    Caller,
    Answerer,
}

/// Escape sequence detection state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EscapeState {
    /// No escape sequence detected
    None,
    /// First '+' received
    Plus1,
    /// Second '+' received
    Plus2,
    /// Third '+' received, waiting for guard time
    Plus3,
}

/// Maximum accumulated AT command-line length. A real Hayes modem caps the
/// command line and discards the overflow. Matches `gp64-modem`'s
/// `at_parser::MAX_AT_COMMAND_LEN`, which fixed the same failure class in that
/// crate (MDD-BUG-ANVIL-00019); this is the `gp64-hw` sibling
/// (MDD-BUG-CRUCIBLE-00622). Generous — well above any real init or dial
/// string — so it never truncates a legitimate command.
const MAX_AT_COMMAND_LEN: usize = 256;

/// Virtual modem emulation
#[derive(Clone)]
pub struct VirtualModem {
    /// Current operating mode
    mode: ModemMode,
    /// AT command buffer
    command_buffer: String,
    /// Data to send to the serial port (from modem to DTE)
    tx_queue: VecDeque<u8>,
    /// Data received from serial port (from DTE to modem)
    rx_queue: VecDeque<u8>,
    /// Bytes dropped because [`Self::REMOTE_QUEUE_CAP`] was reached.
    rx_queue_overflow: u64,
    /// S-registers (256 registers, most unused)
    s_registers: [u8; 256],
    /// Echo mode enabled (E1)
    echo: bool,
    /// Verbose result codes (V1)
    verbose: bool,
    /// Result codes enabled (Q0)
    quiet: bool,
    /// Speaker mode (M0-M3)
    speaker_mode: u8,
    /// Speaker volume (L0-L3)
    speaker_volume: u8,
    /// Extended result codes (&W prefix enables)
    extended_results: bool,
    /// Current connection state
    connected: bool,
    /// Connection speed (for result codes)
    connect_speed: u32,
    /// Escape sequence state machine
    escape_state: EscapeState,
    /// Timestamp of last character received (for escape guard time)
    last_char_time: u64,
    /// Current emulator tick count (updated via tick())
    current_tick: u64,
    /// Dial string being processed
    dial_string: Option<String>,
    /// DTR state from MCR
    dtr: bool,
    /// Flow control mode (0=none, 1=XON/XOFF, 2=RTS/CTS)
    flow_control: u8,
    /// Last executed AT command (for A/ repeat)
    last_command: String,
    /// DSR behavior mode (AT&S)
    dsr_mode: u8,
    /// Modem model (None = legacy instant connect)
    modem_model: Option<ModemModel>,
    /// Current handshake state machine phase
    handshake_phase: HandshakePhase,
    /// Set by cmd_dial(), cleared by notify/abort — signals that we want a TCP connection
    wants_conn: bool,
    /// Extracted dial digits for DTMF playback during handshake
    dial_digits: Vec<char>,
    /// Grace period remaining (ms) after ATDT — ignore stale characters
    grace_period_remaining: u32,
    /// Caller vs answerer role (None when Idle)
    handshake_role: Option<HandshakeRole>,
    /// TCP connected during early handshake phase (buffered for WaitingForCarrier)
    tcp_connected_pending: bool,
    /// Buffered escape chars ('+') pending escape sequence outcome
    escape_buffer: Vec<u8>,
}

impl Default for VirtualModem {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualModem {
    /// Create a new virtual modem with default settings
    pub fn new() -> Self {
        let mut modem = Self {
            mode: ModemMode::Command,
            command_buffer: String::new(),
            tx_queue: VecDeque::new(),
            rx_queue: VecDeque::with_capacity(Self::REMOTE_QUEUE_CAP),
            rx_queue_overflow: 0,
            s_registers: [0; 256],
            echo: true,
            verbose: true,
            quiet: false,
            speaker_mode: 1,
            speaker_volume: 1,
            extended_results: true,
            connected: false,
            connect_speed: 0,
            escape_state: EscapeState::None,
            last_char_time: 0,
            current_tick: 0,
            dial_string: None,
            dtr: true,
            flow_control: 0,
            last_command: String::new(),
            dsr_mode: 0,
            modem_model: None,
            handshake_phase: HandshakePhase::Idle,
            wants_conn: false,
            dial_digits: Vec::new(),
            grace_period_remaining: 0,
            handshake_role: None,
            tcp_connected_pending: false,
            escape_buffer: Vec::new(),
        };
        modem.reset_to_defaults();
        modem
    }

    /// Reset modem to factory defaults
    pub fn reset_to_defaults(&mut self) {
        // Initialize S-registers to default values
        self.s_registers[sregs::S0] = 0; // Auto-answer disabled
        self.s_registers[sregs::S1] = 0; // Ring counter
        self.s_registers[sregs::S2] = 43; // Escape character '+'
        self.s_registers[sregs::S3] = 13; // Carriage return
        self.s_registers[sregs::S4] = 10; // Line feed
        self.s_registers[sregs::S5] = 8; // Backspace
        self.s_registers[sregs::S6] = 2; // Wait for dial tone
        self.s_registers[sregs::S7] = 50; // Wait for carrier (seconds)
        self.s_registers[sregs::S8] = 2; // Pause for comma
        self.s_registers[sregs::S9] = 6; // Carrier detect response time
        self.s_registers[sregs::S10] = 14; // Carrier loss hangup delay
        self.s_registers[sregs::S11] = 95; // DTMF tone duration
        self.s_registers[sregs::S12] = 50; // Escape guard time (1 second)

        self.echo = true;
        self.verbose = true;
        self.quiet = false;
        self.speaker_mode = 1;
        self.speaker_volume = 1;
        self.extended_results = true;
        self.flow_control = 0;
        self.dsr_mode = 0;
    }

    /// Full modem reset (ATZ)
    pub fn reset(&mut self) {
        self.mode = ModemMode::Command;
        self.command_buffer.clear();
        self.connected = false;
        self.connect_speed = 0;
        self.escape_state = EscapeState::None;
        self.dial_string = None;
        self.handshake_phase = HandshakePhase::Idle;
        self.wants_conn = false;
        self.dial_digits.clear();
        self.grace_period_remaining = 0;
        self.handshake_role = None;
        self.tcp_connected_pending = false;
        self.escape_buffer.clear();
        self.reset_to_defaults();
    }

    /// Get current modem mode
    pub fn mode(&self) -> ModemMode {
        self.mode
    }

    /// Set modem mode (for testing/escape sequence handling)
    pub fn set_mode(&mut self, mode: ModemMode) {
        self.mode = mode;
    }

    /// Check if modem is connected
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Check if echo is enabled
    pub fn is_echo_enabled(&self) -> bool {
        self.echo
    }

    /// Check if verbose mode is enabled
    pub fn is_verbose_enabled(&self) -> bool {
        self.verbose
    }

    /// Check if quiet mode is enabled
    pub fn is_quiet_enabled(&self) -> bool {
        self.quiet
    }

    /// Get current DSR behavior mode (AT&S)
    pub fn dsr_mode(&self) -> u8 {
        self.dsr_mode
    }

    /// Get speaker mode (M0-M3)
    pub fn speaker_mode(&self) -> u8 {
        self.speaker_mode
    }

    /// Set the modem model (None = legacy instant connect)
    pub fn set_modem_model(&mut self, model: Option<ModemModel>) {
        self.modem_model = model;
    }

    /// Get the current modem model
    pub fn modem_model(&self) -> Option<ModemModel> {
        self.modem_model
    }

    /// Get the current handshake phase
    pub fn handshake_phase(&self) -> HandshakePhase {
        self.handshake_phase
    }

    /// Get the dial digit at the given index, if any.
    pub fn dial_digit_at(&self, index: usize) -> Option<char> {
        self.dial_digits.get(index).copied()
    }

    /// Returns true when the modem wants a TCP connection (set by dial, cleared on connect/abort)
    pub fn wants_connection(&self) -> bool {
        self.wants_conn
    }

    /// Notify that TCP connection has been established (WaitingForCarrier -> Training)
    pub fn notify_tcp_connected(&mut self) {
        match self.handshake_phase {
            HandshakePhase::WaitingForCarrier { .. } => {
                if let Some(model) = self.modem_model {
                    self.handshake_phase = HandshakePhase::Training {
                        remaining_ms: model.handshake_duration_ms(),
                        carrier_detected: false,
                    };
                }
            }
            HandshakePhase::DialTone { .. }
            | HandshakePhase::Dtmf { .. }
            | HandshakePhase::DtmfGap { .. }
            | HandshakePhase::Ringing { .. } => {
                self.tcp_connected_pending = true;
            }
            _ => {} // Idle, Training, IncomingRing, Complete — ignore
        }
    }

    /// Notify that TCP connection has been lost while online
    pub fn notify_tcp_disconnected(&mut self) {
        if self.connected {
            self.connected = false;
            self.connect_speed = 0;
            self.mode = ModemMode::Command;
            self.escape_buffer.clear();
            self.escape_state = EscapeState::None;
            self.send_result(ResultCode::NoCarrier);
        }
    }

    /// Start an incoming ring sequence (answerer side)
    pub fn start_incoming_ring(&mut self) {
        self.s_registers[sregs::S1] = 0; // Reset ring counter
        self.handshake_phase = HandshakePhase::IncomingRing {
            ring_count: 1,
            remaining_ms: 2000, // 2s RI-high
            ri_high: true,
        };
        self.handshake_role = Some(HandshakeRole::Answerer);
        self.send_result(ResultCode::Ring);
    }

    /// Returns true when in answerer role (incoming ring or ATA)
    pub fn is_answering(&self) -> bool {
        self.handshake_role == Some(HandshakeRole::Answerer)
    }

    /// Receive a byte from the serial port (DTE -> modem)
    pub fn receive_from_serial(&mut self, byte: u8) {
        // If handshake is active, handle character abort
        if self.handshake_phase != HandshakePhase::Idle {
            match self.handshake_phase {
                HandshakePhase::Training { .. } => {
                    // During training, characters are ignored (modem is "deaf")
                    return;
                }
                HandshakePhase::IncomingRing { .. } => {
                    // During incoming ring, the modem is in command mode and
                    // accepts AT commands (e.g., ATA to answer). Fall through.
                }
                HandshakePhase::Idle | HandshakePhase::Complete => {}
                _ => {
                    // During caller-side handshake phases (DialTone, Dtmf, DtmfGap,
                    // WaitingForCarrier, Ringing), characters abort after grace period
                    if self.grace_period_remaining == 0 {
                        self.abort_handshake();
                        return;
                    }
                    // Within grace period, ignore the character
                    return;
                }
            }
        }

        match self.mode {
            ModemMode::Command | ModemMode::OnlineCommand => {
                self.process_command_byte(byte);
            }
            ModemMode::Online => {
                self.process_data_byte(byte);
            }
        }
    }

    /// Get a byte to send to the serial port (modem -> DTE)
    pub fn send_to_serial(&mut self) -> Option<u8> {
        self.tx_queue.pop_front()
    }

    /// Check if there's data waiting to send to serial port
    pub fn has_data_for_serial(&self) -> bool {
        !self.tx_queue.is_empty()
    }

    /// Queue data received from the "remote" end (e.g., null modem partner)
    pub fn receive_from_remote(&mut self, data: &[u8]) {
        if self.mode == ModemMode::Online && self.connected {
            for &byte in data {
                self.tx_queue.push_back(byte);
            }
        }
    }

    /// Maximum DTE-to-remote bytes held before the modem stops accepting.
    ///
    /// The outer serial buffer was already bounded, but every tick drained it
    /// WHOLESALE into this queue, which had no limit — so the bound merely moved
    /// bytes from a capped buffer into an uncapped one. A guest transmitting
    /// while the remote TCP write is stalled grew host memory without bound
    /// (`MDD-BUG-CRUCIBLE-00623`).
    pub const REMOTE_QUEUE_CAP: usize = 8192;

    /// Bytes currently queued for the remote end.
    ///
    /// Callers include this in their flow-control decision: the guest has to see
    /// backpressure from the WHOLE path, not just the buffer nearest to it.
    pub fn remote_queue_len(&self) -> usize {
        self.rx_queue.len()
    }

    /// Whether the modem can take another DTE byte for the remote end.
    pub fn can_accept_from_serial(&self) -> bool {
        self.rx_queue.len() < Self::REMOTE_QUEUE_CAP
    }

    /// Bytes dropped at the cap. Non-zero means a caller fed the modem without
    /// consulting [`Self::can_accept_from_serial`].
    pub fn remote_queue_overflow(&self) -> u64 {
        self.rx_queue_overflow
    }

    /// Queue one byte for the remote end, dropping at the cap.
    ///
    /// The cap is a backstop, not the flow-control mechanism: a caller that
    /// honours `can_accept_from_serial` never reaches it. Dropping is counted
    /// rather than silent so that a caller which does not is visible.
    fn queue_for_remote(&mut self, byte: u8) {
        if self.rx_queue.len() >= Self::REMOTE_QUEUE_CAP {
            self.rx_queue_overflow += 1;
            if self.rx_queue_overflow.is_power_of_two() {
                tracing::warn!(
                    target: "gp64::hw::serial",
                    cap = Self::REMOTE_QUEUE_CAP,
                    dropped = self.rx_queue_overflow,
                    "VirtualModem: DTE-to-remote queue full; dropping"
                );
            }
            return;
        }
        self.rx_queue.push_back(byte);
    }

    /// Get data to send to the "remote" end
    pub fn send_to_remote(&mut self) -> Option<u8> {
        self.rx_queue.pop_front()
    }

    /// Check if there's data to send to remote
    pub fn has_data_for_remote(&self) -> bool {
        !self.rx_queue.is_empty()
    }

    /// Update DTR signal state
    pub fn set_dtr(&mut self, dtr: bool) {
        let was_dtr = self.dtr;
        self.dtr = dtr;

        // If DTR drops during handshake, abort
        if was_dtr && !dtr && self.handshake_phase != HandshakePhase::Idle {
            self.abort_handshake();
            return;
        }

        // If DTR drops while connected, hang up
        if was_dtr && !dtr && self.connected {
            self.hangup();
        }
    }

    /// Get carrier detect state (reflects connection state or training carrier detect)
    pub fn get_dcd(&self) -> bool {
        if let HandshakePhase::Training {
            carrier_detected, ..
        } = self.handshake_phase
        {
            if carrier_detected {
                return true;
            }
        }
        self.connected
    }

    /// Get data set ready (always ready in emulation)
    pub fn get_dsr(&self) -> bool {
        true
    }

    /// Get clear to send (always ready in emulation)
    pub fn get_cts(&self) -> bool {
        true
    }

    /// Get ring indicator (high during IncomingRing when ri_high is set)
    pub fn get_ri(&self) -> bool {
        if let HandshakePhase::IncomingRing { ri_high, .. } = self.handshake_phase {
            return ri_high;
        }
        false
    }

    /// Tick the modem (call periodically for timing-based operations)
    /// Each tick represents 1ms.
    pub fn tick(&mut self, tick_count: u64) {
        self.current_tick = tick_count;

        // Check for escape sequence timeout
        if self.mode == ModemMode::Online {
            self.check_escape_timeout();
        }

        // Advance handshake state machine (1ms per tick)
        self.tick_handshake();
    }

    /// Advance the handshake state machine by 1ms
    fn tick_handshake(&mut self) {
        // Decrement grace period
        if self.grace_period_remaining > 0 {
            self.grace_period_remaining -= 1;
        }

        match self.handshake_phase {
            HandshakePhase::DialTone { remaining_ms } => {
                if remaining_ms <= 1 {
                    // Transition to first DTMF digit (or WaitingForCarrier if no digits)
                    if self.dial_digits.is_empty() {
                        if self.tcp_connected_pending {
                            // TCP already connected during early phase — skip to Training
                            self.tcp_connected_pending = false;
                            if let Some(model) = self.modem_model {
                                self.handshake_phase = HandshakePhase::Training {
                                    remaining_ms: model.handshake_duration_ms(),
                                    carrier_detected: false,
                                };
                            }
                        } else {
                            self.handshake_phase = HandshakePhase::WaitingForCarrier {
                                remaining_ms: (self.s_registers[sregs::S7] as u32) * 1000,
                            };
                        }
                    } else {
                        self.handshake_phase = HandshakePhase::Dtmf {
                            digit_index: 0,
                            remaining_ms: self.s_registers[sregs::S11] as u32,
                        };
                    }
                } else {
                    self.handshake_phase = HandshakePhase::DialTone {
                        remaining_ms: remaining_ms - 1,
                    };
                }
            }
            HandshakePhase::Dtmf {
                digit_index,
                remaining_ms,
            } => {
                if remaining_ms <= 1 {
                    // After this digit, is there another?
                    let next_index = digit_index + 1;
                    if next_index < self.dial_digits.len() {
                        // Gap before next digit
                        self.handshake_phase = HandshakePhase::DtmfGap {
                            digit_index,
                            remaining_ms: 70, // 70ms inter-digit gap
                        };
                    } else {
                        // Last digit done — go to WaitingForCarrier (or Training if TCP buffered)
                        if self.tcp_connected_pending {
                            self.tcp_connected_pending = false;
                            if let Some(model) = self.modem_model {
                                self.handshake_phase = HandshakePhase::Training {
                                    remaining_ms: model.handshake_duration_ms(),
                                    carrier_detected: false,
                                };
                            }
                        } else {
                            self.handshake_phase = HandshakePhase::WaitingForCarrier {
                                remaining_ms: (self.s_registers[sregs::S7] as u32) * 1000,
                            };
                        }
                    }
                } else {
                    self.handshake_phase = HandshakePhase::Dtmf {
                        digit_index,
                        remaining_ms: remaining_ms - 1,
                    };
                }
            }
            HandshakePhase::DtmfGap {
                digit_index,
                remaining_ms,
            } => {
                if remaining_ms <= 1 {
                    // Start next digit
                    self.handshake_phase = HandshakePhase::Dtmf {
                        digit_index: digit_index + 1,
                        remaining_ms: self.s_registers[sregs::S11] as u32,
                    };
                } else {
                    self.handshake_phase = HandshakePhase::DtmfGap {
                        digit_index,
                        remaining_ms: remaining_ms - 1,
                    };
                }
            }
            HandshakePhase::WaitingForCarrier { remaining_ms } => {
                if remaining_ms <= 1 {
                    // S7 timeout — NO CARRIER
                    self.handshake_phase = HandshakePhase::Idle;
                    self.wants_conn = false;
                    self.handshake_role = None;
                    self.tcp_connected_pending = false;
                    self.send_result(ResultCode::NoCarrier);
                } else {
                    self.handshake_phase = HandshakePhase::WaitingForCarrier {
                        remaining_ms: remaining_ms - 1,
                    };
                }
            }
            HandshakePhase::Training {
                remaining_ms,
                carrier_detected,
            } => {
                if remaining_ms <= 1 {
                    // Training complete — connection established
                    self.handshake_phase = HandshakePhase::Idle;
                    self.wants_conn = false;
                    self.handshake_role = None;
                    if let Some(model) = self.modem_model {
                        self.connected = true;
                        self.connect_speed = model.max_speed();
                        self.mode = ModemMode::Online;
                        let result = match model.max_speed() {
                            1200 => ResultCode::Connect1200,
                            2400 => ResultCode::Connect2400,
                            4800 => ResultCode::Connect4800,
                            9600 => ResultCode::Connect9600,
                            14400 => ResultCode::Connect14400,
                            19200 => ResultCode::Connect19200,
                            28800 => ResultCode::Connect28800,
                            38400 => ResultCode::Connect38400,
                            56000 => ResultCode::Connect56000,
                            57600 => ResultCode::Connect57600,
                            115200 => ResultCode::Connect115200,
                            _ => ResultCode::Connect,
                        };
                        self.send_result(result);
                    }
                } else {
                    // Check if DCD should go HIGH (~60% through training)
                    let new_carrier = if !carrier_detected {
                        if let Some(model) = self.modem_model {
                            let total = model.handshake_duration_ms();
                            let elapsed = total - remaining_ms;
                            elapsed >= (total * 60) / 100
                        } else {
                            false
                        }
                    } else {
                        true
                    };
                    self.handshake_phase = HandshakePhase::Training {
                        remaining_ms: remaining_ms - 1,
                        carrier_detected: new_carrier,
                    };
                }
            }
            HandshakePhase::IncomingRing {
                ring_count,
                remaining_ms,
                ri_high,
            } => {
                if remaining_ms <= 1 {
                    if ri_high {
                        // RI was high (2s ring), now go low (4s silence)
                        self.handshake_phase = HandshakePhase::IncomingRing {
                            ring_count,
                            remaining_ms: 4000,
                            ri_high: false,
                        };
                    } else {
                        // RI was low (4s silence), ring cycle complete
                        let new_count = ring_count + 1;
                        self.s_registers[sregs::S1] = ring_count; // Update ring counter

                        // Check auto-answer (S0)
                        let auto_answer = self.s_registers[sregs::S0];
                        if auto_answer > 0 && ring_count >= auto_answer {
                            // Auto-answer: transition to Training
                            if let Some(model) = self.modem_model {
                                self.handshake_phase = HandshakePhase::Training {
                                    remaining_ms: model.handshake_duration_ms(),
                                    carrier_detected: false,
                                };
                            }
                        } else {
                            // Another ring
                            self.handshake_phase = HandshakePhase::IncomingRing {
                                ring_count: new_count,
                                remaining_ms: 2000,
                                ri_high: true,
                            };
                            self.send_result(ResultCode::Ring);
                        }
                    }
                } else {
                    self.handshake_phase = HandshakePhase::IncomingRing {
                        ring_count,
                        remaining_ms: remaining_ms - 1,
                        ri_high,
                    };
                }
            }
            HandshakePhase::Ringing { .. } => {
                // Not used in this implementation yet
            }
            HandshakePhase::Complete => {
                // Transient state, should not persist
                self.handshake_phase = HandshakePhase::Idle;
            }
            HandshakePhase::Idle => {}
        }
    }

    /// Abort the current handshake, returning to Idle
    fn abort_handshake(&mut self) {
        self.handshake_phase = HandshakePhase::Idle;
        self.wants_conn = false;
        self.dial_digits.clear();
        self.grace_period_remaining = 0;
        self.handshake_role = None;
        self.tcp_connected_pending = false;
        self.escape_buffer.clear();
        self.escape_state = EscapeState::None;
        self.send_result(ResultCode::NoCarrier);
    }

    /// Process a byte in command mode
    fn process_command_byte(&mut self, byte: u8) {
        let ch = byte as char;
        let cr = self.s_registers[sregs::S3] as char;
        let lf = self.s_registers[sregs::S4] as char;
        let bs = self.s_registers[sregs::S5];

        // Echo if enabled
        if self.echo {
            self.tx_queue.push_back(byte);
        }

        // Handle special characters
        if ch == cr {
            // Execute command on CR
            self.execute_command();
            return;
        }

        if byte == bs {
            // Backspace - remove last character
            if !self.command_buffer.is_empty() {
                self.command_buffer.pop();
            }
            return;
        }

        if ch == lf {
            // Ignore line feeds
            return;
        }

        // Accumulate command characters, up to the command-line limit. A real
        // Hayes modem caps the line and discards the overflow; without this a
        // guest streaming printable bytes in command mode with no terminating
        // CR grows this String until the host runs out of memory
        // (MDD-BUG-CRUCIBLE-00622). CR and backspace are handled above, so an
        // overflowed line can still be edited or executed.
        if ch.is_ascii() && !ch.is_ascii_control() && self.command_buffer.len() < MAX_AT_COMMAND_LEN
        {
            self.command_buffer.push(ch.to_ascii_uppercase());
        }

        // Detect A/ (repeat last command) — executes immediately without CR
        if self.command_buffer == "A/" {
            self.command_buffer.clear();
            if self.last_command.is_empty() {
                self.send_result(ResultCode::Error);
            } else {
                // Re-execute the last command
                let cmd = self.last_command.clone();
                self.command_buffer = cmd;
                self.execute_command();
            }
        }
    }

    /// Process a byte in online data mode
    fn process_data_byte(&mut self, byte: u8) {
        let escape_char = self.s_registers[sregs::S2];

        if byte == escape_char {
            let prev_state = self.escape_state;
            self.handle_escape_char();
            if self.escape_state != EscapeState::None {
                // Escape state machine is advancing -- buffer the '+' instead of forwarding
                self.escape_buffer.push(byte);
            } else if prev_state != EscapeState::None {
                // Escape was broken (e.g. 4th '+' after Plus3) -- flush buffer + current byte
                self.flush_escape_buffer();
                self.queue_for_remote(byte);
            } else {
                // Guard time check failed in handle_escape_char -- forward normally
                self.queue_for_remote(byte);
            }
        } else {
            if self.escape_state != EscapeState::None {
                // Non-escape char breaks the sequence -- flush buffered chars first
                self.flush_escape_buffer();
            }
            self.escape_state = EscapeState::None;
            self.queue_for_remote(byte);
        }

        self.last_char_time = self.current_tick;
    }

    /// Flush buffered escape characters to the remote queue
    fn flush_escape_buffer(&mut self) {
        let buffered = std::mem::take(&mut self.escape_buffer);
        for b in buffered {
            self.queue_for_remote(b);
        }
    }

    /// Handle a potential escape character
    fn handle_escape_char(&mut self) {
        // S12=0 disables escape sequence detection (Hayes spec)
        if self.s_registers[sregs::S12] == 0 {
            return;
        }
        let guard_ticks = (self.s_registers[sregs::S12] as u64) * 20; // Approx ticks

        match self.escape_state {
            EscapeState::None => {
                // Check if enough time has passed since last char (guard time)
                if self.current_tick - self.last_char_time >= guard_ticks {
                    self.escape_state = EscapeState::Plus1;
                }
            }
            EscapeState::Plus1 => {
                self.escape_state = EscapeState::Plus2;
            }
            EscapeState::Plus2 => {
                self.escape_state = EscapeState::Plus3;
            }
            EscapeState::Plus3 => {
                // 4th '+' breaks the escape pattern
                self.escape_state = EscapeState::None;
            }
        }
    }

    /// Check for escape sequence completion
    fn check_escape_timeout(&mut self) {
        if self.escape_state != EscapeState::Plus3 {
            return;
        }
        // S12=0 disables escape sequence detection
        if self.s_registers[sregs::S12] == 0 {
            return;
        }

        let guard_ticks = (self.s_registers[sregs::S12] as u64) * 20;
        if self.current_tick - self.last_char_time >= guard_ticks {
            // Escape sequence complete - switch to command mode
            // Discard buffered '+' chars (they were part of the escape, not data)
            self.escape_buffer.clear();
            self.mode = ModemMode::OnlineCommand;
            self.escape_state = EscapeState::None;
            self.send_result(ResultCode::Ok);
        }
    }

    /// Execute the buffered AT command
    fn execute_command(&mut self) {
        let command = std::mem::take(&mut self.command_buffer);
        let command = command.trim().to_string();

        // Empty command returns OK
        if command.is_empty() {
            return;
        }

        if !command.starts_with("AT") {
            self.send_result(ResultCode::Error);
            return;
        }

        // Store as last command for A/ repeat
        self.last_command = command.clone();

        // Parse and execute AT commands
        let commands = &command[2..]; // Skip "AT"
        if commands.is_empty() {
            // Just "AT" returns OK
            self.send_result(ResultCode::Ok);
            return;
        }

        if !self.parse_commands(commands) {
            self.send_result(ResultCode::Error);
        }
    }

    /// Parse and execute AT command string
    fn parse_commands(&mut self, commands: &str) -> bool {
        let mut chars = commands.chars().peekable();

        while let Some(cmd) = chars.next() {
            match cmd {
                'A' => {
                    // Answer call
                    if let HandshakePhase::IncomingRing { .. } = self.handshake_phase {
                        // Manual answer during IncomingRing: transition to Training
                        if let Some(model) = self.modem_model {
                            self.handshake_phase = HandshakePhase::Training {
                                remaining_ms: model.handshake_duration_ms(),
                                carrier_detected: false,
                            };
                        }
                    } else {
                        self.send_result(ResultCode::NoCarrier);
                    }
                    return true;
                }
                'D' => {
                    // Dial
                    let dial_string: String = chars.collect();
                    return self.cmd_dial(&dial_string);
                }
                'E' => {
                    // Echo mode
                    let n = self.parse_digit(&mut chars, 1);
                    self.echo = n != 0;
                }
                'H' => {
                    // Hangup
                    let n = self.parse_digit(&mut chars, 0);
                    if n == 0 {
                        self.hangup();
                    }
                }
                'I' => {
                    // Information/identification
                    let n = self.parse_digit(&mut chars, 0);
                    self.cmd_info(n);
                    return true;
                }
                'L' => {
                    // Speaker volume
                    self.speaker_volume = self.parse_digit(&mut chars, 1);
                }
                'M' => {
                    // Speaker control
                    self.speaker_mode = self.parse_digit(&mut chars, 1);
                }
                'O' => {
                    // Return to online mode
                    let _n = self.parse_digit(&mut chars, 0);
                    if self.connected && self.mode == ModemMode::OnlineCommand {
                        self.mode = ModemMode::Online;
                        self.send_result(ResultCode::Connect);
                        return true;
                    }
                }
                'Q' => {
                    // Quiet mode
                    let n = self.parse_digit(&mut chars, 0);
                    self.quiet = n != 0;
                }
                'S' => {
                    // S-register access
                    if !self.cmd_sreg(&mut chars) {
                        return false;
                    }
                }
                'V' => {
                    // Verbose mode
                    let n = self.parse_digit(&mut chars, 1);
                    self.verbose = n != 0;
                }
                'W' => {
                    // Connect message control (no-op in emulation)
                    let _n = self.parse_digit(&mut chars, 0);
                }
                'X' => {
                    // Extended result codes
                    let n = self.parse_digit(&mut chars, 4);
                    self.extended_results = n >= 1;
                }
                'Z' => {
                    // Reset
                    let _n = self.parse_digit(&mut chars, 0);
                    self.reset();
                    self.send_result(ResultCode::Ok);
                    return true;
                }
                '&' => {
                    // Extended & commands
                    if let Some(sub) = chars.next() {
                        if !self.cmd_extended(sub, &mut chars) {
                            return false;
                        }
                    }
                }
                '\\' => {
                    // Backslash commands
                    if let Some(sub) = chars.next() {
                        if !self.cmd_backslash(sub, &mut chars) {
                            return false;
                        }
                    }
                }
                '%' => {
                    // Percent commands
                    if let Some(sub) = chars.next() {
                        if !self.cmd_percent(sub, &mut chars) {
                            return false;
                        }
                    }
                }
                ' ' => {
                    // Ignore spaces
                }
                _ => {
                    // Unknown command
                    return false;
                }
            }
        }

        self.send_result(ResultCode::Ok);
        true
    }

    /// Parse a digit from command string
    fn parse_digit(&self, chars: &mut std::iter::Peekable<std::str::Chars>, default: u8) -> u8 {
        if let Some(&c) = chars.peek() {
            if c.is_ascii_digit() {
                chars.next();
                return (c as u8) - b'0';
            }
        }
        default
    }

    /// Parse a number from command string
    fn parse_number(&self, chars: &mut std::iter::Peekable<std::str::Chars>, default: u16) -> u16 {
        let mut value: u16 = 0;
        let mut found = false;
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() {
                // Saturate rather than wrap: an oversized number must stay
                // oversized, so every caller's range check rejects it. Wrapping
                // let `ATS0=65578` alias the legal 42 (and panicked with
                // overflow checks on) — MDD-BUG-CRU-01030. All the digits are
                // still consumed, so the rest of the command parses as typed.
                value = value
                    .saturating_mul(10)
                    .saturating_add((c as u16) - ('0' as u16));
                found = true;
                chars.next();
            } else {
                break;
            }
        }
        if found {
            value
        } else {
            default
        }
    }

    /// Handle ATD (dial) command
    fn cmd_dial(&mut self, dial_string: &str) -> bool {
        // Parse dial string - skip tone/pulse prefix and modifiers
        let mut number = String::new();
        let mut return_to_command = false;
        let mut chars = dial_string.chars().peekable();

        // Check for dial mode prefix
        if let Some(&c) = chars.peek() {
            if c == 'T' || c == 'P' {
                chars.next(); // Skip T (tone) or P (pulse)
            }
        }

        // Collect the number/address
        for c in chars {
            match c {
                '0'..='9' | '*' | '#' | 'A'..='D' => {
                    number.push(c);
                }
                ',' => {
                    // Pause (ignore in emulation)
                }
                ';' => {
                    // Return to command mode after dialing (DTMF-only)
                    return_to_command = true;
                }
                ' ' | '-' | '(' | ')' => {
                    // Formatting characters, ignore
                }
                _ => {
                    // Unknown character
                }
            }
        }

        if !number.is_empty() {
            self.dial_string = Some(number.clone());
            if return_to_command {
                // Semicolon: play DTMF only, return to command mode with OK
                self.send_result(ResultCode::Ok);
            } else if self.modem_model.is_some() {
                // Handshake mode: start the FSM
                self.dial_digits = number.chars().collect();
                self.handshake_phase = HandshakePhase::DialTone { remaining_ms: 2000 };
                self.wants_conn = true;
                self.handshake_role = Some(HandshakeRole::Caller);
                self.grace_period_remaining = 200; // 200ms grace period
            } else {
                // Legacy instant connect
                self.simulate_connection(9600);
            }
            return true;
        }

        self.send_result(ResultCode::Error);
        true
    }

    /// Simulate establishing a connection
    fn simulate_connection(&mut self, speed: u32) {
        self.connected = true;
        self.connect_speed = speed;
        self.mode = ModemMode::Online;

        // Send appropriate CONNECT result
        let result = match speed {
            1200 => ResultCode::Connect1200,
            2400 => ResultCode::Connect2400,
            4800 => ResultCode::Connect4800,
            9600 => ResultCode::Connect9600,
            14400 => ResultCode::Connect14400,
            19200 => ResultCode::Connect19200,
            28800 => ResultCode::Connect28800,
            38400 => ResultCode::Connect38400,
            56000 => ResultCode::Connect56000,
            57600 => ResultCode::Connect57600,
            115200 => ResultCode::Connect115200,
            _ => ResultCode::Connect,
        };
        self.send_result(result);
    }

    /// Handle ATI (info) command
    fn cmd_info(&mut self, level: u8) {
        let info = match level {
            0 => "GP64 Virtual Modem",
            1 => "V1.0",
            2 => "OK",
            3 => "GP64 Virtual Modem V1.0",
            4 => "Hayes Compatible",
            _ => "",
        };

        if !info.is_empty() {
            self.send_string(info);
        }
        self.send_result(ResultCode::Ok);
    }

    /// Handle S-register commands
    fn cmd_sreg(&mut self, chars: &mut std::iter::Peekable<std::str::Chars>) -> bool {
        // Parse register number
        let reg = self.parse_number(chars, 0) as usize;
        if reg > 255 {
            return false;
        }

        // Check for query or assignment
        match chars.peek() {
            Some(&'?') => {
                // Query register
                chars.next();
                let value = self.s_registers[reg];
                let response = format!("{value:03}");
                self.send_string(&response);
            }
            Some(&'=') => {
                // Set register
                chars.next();
                let value = self.parse_number(chars, 0);
                if value > 255 {
                    return false;
                }
                self.s_registers[reg] = value as u8;
            }
            _ => {
                // Just selecting register (for subsequent ? or =)
            }
        }

        true
    }

    /// Handle extended AT& commands
    fn cmd_extended(
        &mut self,
        cmd: char,
        chars: &mut std::iter::Peekable<std::str::Chars>,
    ) -> bool {
        match cmd {
            'C' => {
                // DCD behavior
                let _n = self.parse_digit(chars, 1);
                // In emulation, DCD always follows connection state
            }
            'D' => {
                // DTR behavior
                let _n = self.parse_digit(chars, 2);
                // In emulation, DTR drop causes hangup
            }
            'F' => {
                // Factory defaults
                self.reset_to_defaults();
            }
            'K' => {
                // Flow control
                self.flow_control = self.parse_digit(chars, 3);
            }
            'V' => {
                // View configuration
                self.cmd_view_config();
                return true;
            }
            'Q' => {
                // Async/sync mode (no-op in emulation)
                let _n = self.parse_digit(chars, 0);
            }
            'S' => {
                // DSR behavior
                self.dsr_mode = self.parse_digit(chars, 0);
            }
            'W' => {
                // Store profile (ignored in emulation)
                let _n = self.parse_digit(chars, 0);
            }
            _ => {
                // Unknown extended command
                return false;
            }
        }
        true
    }

    /// Handle AT\ (backslash) commands
    fn cmd_backslash(
        &mut self,
        cmd: char,
        chars: &mut std::iter::Peekable<std::str::Chars>,
    ) -> bool {
        match cmd {
            'N' => {
                // Error correction mode (no-op in emulation)
                let _n = self.parse_digit(chars, 0);
            }
            _ => {
                return false;
            }
        }
        true
    }

    /// Handle AT% commands
    fn cmd_percent(&mut self, cmd: char, chars: &mut std::iter::Peekable<std::str::Chars>) -> bool {
        match cmd {
            'C' => {
                // Compression control (no-op in emulation)
                let _n = self.parse_digit(chars, 0);
            }
            _ => {
                return false;
            }
        }
        true
    }

    /// View modem configuration
    fn cmd_view_config(&mut self) {
        let config = format!(
            "E{} L{} M{} Q{} V{} X{} &K{}",
            if self.echo { 1 } else { 0 },
            self.speaker_volume,
            self.speaker_mode,
            if self.quiet { 1 } else { 0 },
            if self.verbose { 1 } else { 0 },
            if self.extended_results { 4 } else { 0 },
            self.flow_control
        );
        self.send_string(&config);
        self.send_result(ResultCode::Ok);
    }

    /// Hangup the connection
    fn hangup(&mut self) {
        // Clear handshake state
        self.handshake_phase = HandshakePhase::Idle;
        self.wants_conn = false;
        self.dial_digits.clear();
        self.grace_period_remaining = 0;
        self.handshake_role = None;
        self.tcp_connected_pending = false;
        self.escape_buffer.clear();
        self.escape_state = EscapeState::None;

        if self.connected {
            self.connected = false;
            self.connect_speed = 0;
            self.mode = ModemMode::Command;
            self.send_result(ResultCode::NoCarrier);
        } else {
            self.mode = ModemMode::Command;
        }
    }

    /// Send a result code to the serial port.
    ///
    /// The X register (extended result codes) affects speed-specific CONNECT
    /// codes: when X0, `Connect9600` etc. are downgraded to plain
    /// `Connect` ("CONNECT" / numeric 1).
    fn send_result(&mut self, result: ResultCode) {
        if self.quiet {
            return;
        }

        // X0: speed-specific CONNECT variants downgrade to plain CONNECT
        let result = if !self.extended_results && result.is_connect_speed() {
            ResultCode::Connect
        } else {
            result
        };

        let cr = self.s_registers[sregs::S3];
        let lf = self.s_registers[sregs::S4];

        if self.verbose {
            // Verbose format: <CR><LF>text<CR><LF>
            self.tx_queue.push_back(cr);
            self.tx_queue.push_back(lf);
            for byte in result.verbose().bytes() {
                self.tx_queue.push_back(byte);
            }
            self.tx_queue.push_back(cr);
            self.tx_queue.push_back(lf);
        } else {
            // Numeric format: digit<CR>
            let code = result.numeric();
            for ch in code.to_string().bytes() {
                self.tx_queue.push_back(ch);
            }
            self.tx_queue.push_back(cr);
        }
    }

    /// Send a string to the serial port
    fn send_string(&mut self, s: &str) {
        let cr = self.s_registers[sregs::S3];
        let lf = self.s_registers[sregs::S4];

        self.tx_queue.push_back(cr);
        self.tx_queue.push_back(lf);
        for byte in s.bytes() {
            self.tx_queue.push_back(byte);
        }
        self.tx_queue.push_back(cr);
        self.tx_queue.push_back(lf);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn send_command(modem: &mut VirtualModem, cmd: &str) {
        for byte in cmd.bytes() {
            modem.receive_from_serial(byte);
        }
        modem.receive_from_serial(13); // CR to execute
    }

    fn collect_response(modem: &mut VirtualModem) -> String {
        let mut response = Vec::new();
        while let Some(byte) = modem.send_to_serial() {
            response.push(byte);
        }
        String::from_utf8_lossy(&response).to_string()
    }

    #[test]
    fn test_modem_creation() {
        let modem = VirtualModem::new();
        assert_eq!(modem.mode(), ModemMode::Command);
        assert!(!modem.is_connected());
        assert!(modem.echo);
        assert!(modem.verbose);
    }

    #[test]
    fn test_receive_from_remote_ignored_when_offline() {
        let mut modem = VirtualModem::new();

        modem.receive_from_remote(b"Hi");
        assert!(!modem.has_data_for_serial());

        modem.mode = ModemMode::Online;
        modem.receive_from_remote(b"Hi");
        assert!(!modem.has_data_for_serial());
    }

    #[test]
    fn test_at_command() {
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "AT");
        let response = collect_response(&mut modem);
        assert!(
            response.contains("OK"),
            "Expected OK in response: {response}"
        );
    }

    #[test]
    fn test_echo_mode() {
        let mut modem = VirtualModem::new();

        // Echo is on by default - send complete command
        send_command(&mut modem, "AT");
        let response = collect_response(&mut modem);
        // Should see "AT\r" echoed plus the OK response
        assert!(response.contains("AT"), "Echo should include command");

        // Turn echo off
        send_command(&mut modem, "ATE0");
        collect_response(&mut modem); // Clear response

        // Now echo should be off - command should not be echoed
        send_command(&mut modem, "AT");
        let response = collect_response(&mut modem);
        // Should only have OK, no echo of "AT"
        // The response should just be the result code
        assert!(response.contains("OK"), "Should have OK: {response}");
        // But should NOT have "AT" echoed at the start
        let lines: Vec<&str> = response.lines().collect();
        // First non-empty line should be OK, not AT
        for line in lines {
            if !line.is_empty() {
                assert!(!line.starts_with("AT"), "Echo should be off: {response}");
                break;
            }
        }
    }

    #[test]
    fn test_backspace_and_linefeed_handling() {
        let mut modem = VirtualModem::new();

        modem.receive_from_serial(b'A');
        modem.receive_from_serial(b'T');
        modem.receive_from_serial(b'Q');
        modem.receive_from_serial(b'1');
        modem.receive_from_serial(modem.s_registers[sregs::S5]);
        modem.receive_from_serial(b'0');
        modem.receive_from_serial(modem.s_registers[sregs::S4]);
        modem.receive_from_serial(modem.s_registers[sregs::S3]);

        let _ = collect_response(&mut modem);
        assert!(!modem.quiet);
    }

    #[test]
    fn test_empty_command_no_result() {
        let mut modem = VirtualModem::new();

        modem.receive_from_serial(modem.s_registers[sregs::S3]);
        let response = collect_response(&mut modem);
        assert!(!response.contains("OK"));
    }

    #[test]
    fn test_backspace_on_empty_buffer() {
        let mut modem = VirtualModem::new();

        modem.receive_from_serial(modem.s_registers[sregs::S5]);
        modem.receive_from_serial(modem.s_registers[sregs::S3]);

        let response = collect_response(&mut modem);
        assert!(!response.contains("OK"));
    }

    #[test]
    fn test_ignore_non_printable_bytes() {
        let mut modem = VirtualModem::new();

        modem.receive_from_serial(1);
        assert!(modem.command_buffer.is_empty());

        modem.receive_from_serial(0xFF);
        assert!(modem.command_buffer.is_empty());
    }

    #[test]
    fn command_line_is_capped_below_at_and_beyond_the_limit() {
        // A guest can stream printable bytes in command mode and never send a
        // CR. Without a cap the buffer grows until the host is out of memory
        // (MDD-BUG-CRUCIBLE-00622).
        let mut modem = VirtualModem::new();

        for _ in 0..MAX_AT_COMMAND_LEN - 1 {
            modem.receive_from_serial(b'X');
        }
        assert_eq!(modem.command_buffer.len(), MAX_AT_COMMAND_LEN - 1);

        modem.receive_from_serial(b'X');
        assert_eq!(modem.command_buffer.len(), MAX_AT_COMMAND_LEN);

        for _ in 0..10_000 {
            modem.receive_from_serial(b'X');
        }
        assert_eq!(
            modem.command_buffer.len(),
            MAX_AT_COMMAND_LEN,
            "overflow bytes must be discarded, not buffered"
        );
    }

    #[test]
    fn a_carriage_return_recovers_command_parsing_after_overflow() {
        // The cap must not wedge the parser: CR still executes and clears the
        // line, and the next command is parsed normally.
        let mut modem = VirtualModem::new();
        for _ in 0..MAX_AT_COMMAND_LEN + 500 {
            modem.receive_from_serial(b'X');
        }
        assert_eq!(modem.command_buffer.len(), MAX_AT_COMMAND_LEN);

        modem.receive_from_serial(13); // CR executes the overflowed garbage
        assert!(
            modem.command_buffer.is_empty(),
            "CR must clear the command line"
        );
        let response = collect_response(&mut modem);
        assert!(
            response.contains("ERROR"),
            "an overflowed line is not a valid command: {response}"
        );

        send_command(&mut modem, "AT");
        let response = collect_response(&mut modem);
        assert!(
            response.contains("OK"),
            "the parser must recover after an overflow: {response}"
        );
    }

    #[test]
    fn backspace_still_edits_an_overflowed_command_line() {
        let mut modem = VirtualModem::new();
        for _ in 0..MAX_AT_COMMAND_LEN + 50 {
            modem.receive_from_serial(b'X');
        }
        let bs = modem.s_registers[sregs::S5];
        modem.receive_from_serial(bs);
        assert_eq!(modem.command_buffer.len(), MAX_AT_COMMAND_LEN - 1);
        modem.receive_from_serial(b'Y');
        assert!(modem.command_buffer.ends_with('Y'));
    }

    #[test]
    fn test_a_slash_command_errors() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "A/");
        let response = collect_response(&mut modem);
        assert!(response.contains("ERROR"));
    }

    #[test]
    fn test_non_at_command_errors() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "HELLO");
        let response = collect_response(&mut modem);
        assert!(response.contains("ERROR"));
    }

    #[test]
    fn test_extended_without_subcommand_ok() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "AT&");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));
    }

    #[test]
    fn test_unknown_extended_command_errors() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "AT&Z");
        let response = collect_response(&mut modem);
        assert!(response.contains("ERROR"));
    }

    #[test]
    fn test_verbose_mode() {
        let mut modem = VirtualModem::new();

        // Verbose is on by default
        send_command(&mut modem, "AT");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));

        // Turn verbose off
        send_command(&mut modem, "ATV0");
        collect_response(&mut modem);

        // Now should get numeric code
        send_command(&mut modem, "AT");
        let response = collect_response(&mut modem);
        assert!(
            response.contains("\r0\r"),
            "Expected numeric 0 in response: {response}"
        );
    }

    #[test]
    fn test_numeric_connect_code_two_digits() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATE0V0X1");
        collect_response(&mut modem);

        send_command(&mut modem, "ATDT5551234");
        let response = collect_response(&mut modem);
        assert!(
            response.contains("12"),
            "Expected 12 in response: {response}"
        );
    }

    #[test]
    fn test_quiet_mode() {
        let mut modem = VirtualModem::new();

        // Turn quiet mode on
        send_command(&mut modem, "ATQ1");
        let _ = collect_response(&mut modem); // Might still have OK from before Q1 takes effect

        // Now should get no response
        send_command(&mut modem, "AT");
        let response = collect_response(&mut modem);
        // Response should be just the echo of "AT\r"
        assert!(!response.contains("OK"));
    }

    #[test]
    fn test_s_register_query() {
        let mut modem = VirtualModem::new();

        // Query S0 (auto-answer)
        send_command(&mut modem, "ATS0?");
        let response = collect_response(&mut modem);
        assert!(response.contains("000"), "S0 should be 000: {response}");
    }

    #[test]
    fn test_s_register_set() {
        let mut modem = VirtualModem::new();

        // Set S0 to 2
        send_command(&mut modem, "ATS0=2");
        collect_response(&mut modem);

        // Query it back
        send_command(&mut modem, "ATS0?");
        let response = collect_response(&mut modem);
        assert!(response.contains("002"), "S0 should be 002: {response}");
    }

    #[test]
    fn test_s_register_default_number() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATS?");
        let response = collect_response(&mut modem);
        assert!(response.contains("000"));
    }

    #[test]
    fn test_s_register_invalid_index() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATS256?");
        let response = collect_response(&mut modem);
        assert!(response.contains("ERROR"));
    }

    #[test]
    fn test_s_register_invalid_value() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATS1=256");
        let response = collect_response(&mut modem);
        assert!(response.contains("ERROR"));
    }

    #[test]
    fn test_reset() {
        let mut modem = VirtualModem::new();

        // Change some settings
        send_command(&mut modem, "ATE0V0");
        collect_response(&mut modem);

        // Reset
        send_command(&mut modem, "ATZ");
        let _ = collect_response(&mut modem);

        // Echo should be back on
        assert!(modem.echo);
        assert!(modem.verbose);
    }

    #[test]
    fn test_factory_defaults() {
        let mut modem = VirtualModem::new();

        // Change settings
        modem.echo = false;
        modem.verbose = false;

        // Restore factory defaults
        send_command(&mut modem, "AT&F");
        let _ = collect_response(&mut modem);

        assert!(modem.echo);
        assert!(modem.verbose);
    }

    #[test]
    fn test_dial() {
        let mut modem = VirtualModem::new();

        // Dial a number
        send_command(&mut modem, "ATDT5551234");
        let response = collect_response(&mut modem);
        assert!(response.contains("CONNECT"), "Expected CONNECT: {response}");
        assert!(modem.is_connected());
        assert_eq!(modem.mode(), ModemMode::Online);
    }

    #[test]
    fn test_dial_without_prefix() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATD5551234");
        let response = collect_response(&mut modem);
        assert!(response.contains("CONNECT"));
        assert!(modem.is_connected());
    }

    #[test]
    fn test_dial_empty_string_errors() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATD");
        let response = collect_response(&mut modem);
        assert!(response.contains("ERROR"));
    }

    #[test]
    fn test_dial_pulse_prefix() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATDP5551234");
        let response = collect_response(&mut modem);
        assert!(response.contains("CONNECT"));
        assert!(modem.is_connected());
    }

    #[test]
    fn test_hangup() {
        let mut modem = VirtualModem::new();

        // Connect first
        send_command(&mut modem, "ATDT5551234");
        collect_response(&mut modem);
        assert!(modem.is_connected());

        // Force back to command mode for testing
        modem.mode = ModemMode::OnlineCommand;

        // Hangup
        send_command(&mut modem, "ATH0");
        let response = collect_response(&mut modem);
        assert!(
            response.contains("NO CARRIER"),
            "Expected NO CARRIER: {response}"
        );
        assert!(!modem.is_connected());
    }

    #[test]
    fn test_ato_returns_online() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATDT5551234");
        collect_response(&mut modem);
        assert!(modem.is_connected());

        modem.mode = ModemMode::OnlineCommand;
        send_command(&mut modem, "ATO");
        let response = collect_response(&mut modem);

        assert_eq!(modem.mode(), ModemMode::Online);
        assert!(response.contains("CONNECT"));
    }

    #[test]
    fn test_ato_ignored_when_not_connected() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATO");
        let response = collect_response(&mut modem);

        assert_eq!(modem.mode(), ModemMode::Command);
        assert!(response.contains("OK"));
    }

    #[test]
    fn test_ato_ignored_when_not_online_command() {
        let mut modem = VirtualModem::new();

        modem.connected = true;
        modem.mode = ModemMode::Command;
        send_command(&mut modem, "ATO");
        let response = collect_response(&mut modem);

        assert_eq!(modem.mode(), ModemMode::Command);
        assert!(response.contains("OK"));
    }

    #[test]
    fn test_info_command() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATI0");
        let response = collect_response(&mut modem);
        assert!(
            response.contains("Virtual Modem"),
            "Expected modem ID: {response}"
        );
    }

    #[test]
    fn test_info_command_empty() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATI9");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));
    }

    #[test]
    fn test_data_passthrough() {
        let mut modem = VirtualModem::new();

        // Connect
        send_command(&mut modem, "ATDT5551234");
        collect_response(&mut modem);

        // Clear any pending echo
        assert!(modem.send_to_serial().is_none());

        // Send data in online mode
        modem.receive_from_serial(b'H');
        modem.receive_from_serial(b'i');

        // Data should go to rx_queue (for remote)
        assert_eq!(modem.send_to_remote(), Some(b'H'));
        assert_eq!(modem.send_to_remote(), Some(b'i'));
    }

    /// MDD-BUG-CRUCIBLE-00623: the DTE-to-remote queue is bounded.
    ///
    /// The outer serial buffer was already capped, but every tick drained it
    /// wholesale into this queue, which had no limit — so the cap merely moved
    /// bytes from a bounded buffer into an unbounded one, and a guest
    /// transmitting while the remote write is stalled grew host memory without
    /// bound.
    #[test]
    fn remote_queue_is_bounded_and_reports_its_pressure() {
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "ATDT5551234");
        collect_response(&mut modem);

        assert!(modem.can_accept_from_serial(), "a fresh modem must accept");
        assert_eq!(modem.remote_queue_len(), 0);

        // Fill to exactly the cap: still bounded, nothing dropped.
        for _ in 0..VirtualModem::REMOTE_QUEUE_CAP {
            modem.receive_from_serial(b'x');
        }
        assert_eq!(modem.remote_queue_len(), VirtualModem::REMOTE_QUEUE_CAP);
        assert_eq!(modem.remote_queue_overflow(), 0);
        assert!(
            !modem.can_accept_from_serial(),
            "a full modem must stop accepting — that is the backpressure signal"
        );

        // Push well past it. The queue must not grow, and the excess must be
        // counted rather than silently retained or silently discarded.
        for _ in 0..1_000 {
            modem.receive_from_serial(b'y');
        }
        assert_eq!(
            modem.remote_queue_len(),
            VirtualModem::REMOTE_QUEUE_CAP,
            "the queue grew past its cap"
        );
        assert_eq!(modem.remote_queue_overflow(), 1_000);

        // Draining restores acceptance, so a stalled remote that recovers does
        // not leave the link wedged.
        for _ in 0..VirtualModem::REMOTE_QUEUE_CAP {
            assert!(modem.send_to_remote().is_some());
        }
        assert_eq!(modem.remote_queue_len(), 0);
        assert!(
            modem.can_accept_from_serial(),
            "draining must restore acceptance"
        );
    }

    #[test]
    fn test_escape_guard_time_not_elapsed() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATDT5551234");
        collect_response(&mut modem);

        modem.s_registers[sregs::S12] = 10;
        modem.current_tick = 5;
        modem.last_char_time = 0;
        modem.receive_from_serial(b'+');

        assert_eq!(modem.escape_state, EscapeState::None);
    }

    #[test]
    fn test_escape_timeout_guard_not_elapsed() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATDT5551234");
        collect_response(&mut modem);

        modem.s_registers[sregs::S12] = 10;
        modem.last_char_time = 0;
        modem.current_tick = 5;
        modem.escape_state = EscapeState::Plus3;
        modem.tick(5);

        assert_eq!(modem.mode(), ModemMode::Online);
        assert_eq!(modem.escape_state, EscapeState::Plus3);
    }

    #[test]
    fn test_escape_sequence_to_online_command() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATDT5551234");
        collect_response(&mut modem);

        modem.s_registers[sregs::S12] = 1;
        modem.current_tick = 100;
        modem.last_char_time = 0;
        modem.receive_from_serial(b'+');
        modem.receive_from_serial(b'+');
        modem.receive_from_serial(b'+');

        modem.tick(200);
        let response = collect_response(&mut modem);
        assert_eq!(modem.mode(), ModemMode::OnlineCommand);
        assert!(response.contains("OK"));
    }

    #[test]
    fn test_escape_timeout_requires_plus3() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATDT5551234");
        collect_response(&mut modem);

        modem.escape_state = EscapeState::Plus2;
        modem.tick(50);

        assert_eq!(modem.mode(), ModemMode::Online);
        assert!(collect_response(&mut modem).is_empty());
    }

    #[test]
    fn test_tick_does_not_process_escape_in_command_mode() {
        let mut modem = VirtualModem::new();

        modem.escape_state = EscapeState::Plus3;
        modem.s_registers[sregs::S12] = 1;
        modem.last_char_time = 0;
        modem.current_tick = 100;
        modem.tick(100);

        assert_eq!(modem.mode(), ModemMode::Command);
        assert_eq!(modem.escape_state, EscapeState::Plus3);
    }

    #[test]
    fn test_remote_data() {
        let mut modem = VirtualModem::new();

        // Connect
        send_command(&mut modem, "ATDT5551234");
        collect_response(&mut modem);

        // Receive data from remote
        modem.receive_from_remote(b"Hello");

        // Should appear in tx_queue (for serial)
        let mut received = Vec::new();
        while let Some(b) = modem.send_to_serial() {
            received.push(b);
        }
        assert_eq!(&received, b"Hello");
    }

    #[test]
    fn test_dtr_hangup() {
        let mut modem = VirtualModem::new();

        // Connect
        send_command(&mut modem, "ATDT5551234");
        collect_response(&mut modem);
        assert!(modem.is_connected());

        // Drop DTR
        modem.set_dtr(false);
        assert!(!modem.is_connected());
    }

    #[test]
    fn test_dtr_drop_without_connection() {
        let mut modem = VirtualModem::new();

        modem.set_dtr(false);
        assert!(!modem.is_connected());
        assert_eq!(modem.mode(), ModemMode::Command);
    }

    #[test]
    fn test_dtr_reassert_without_connection() {
        let mut modem = VirtualModem::new();

        modem.set_dtr(false);
        modem.set_dtr(true);
        assert!(!modem.is_connected());
        assert_eq!(modem.mode(), ModemMode::Command);
    }

    #[test]
    fn test_dtr_set_true_when_already_true() {
        let mut modem = VirtualModem::new();

        modem.set_dtr(true);
        assert!(!modem.is_connected());
    }

    #[test]
    fn test_error_on_invalid_command() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATXYZ123");
        let response = collect_response(&mut modem);
        // Should get ERROR for unknown command
        assert!(response.contains("ERROR"), "Expected ERROR: {response}");
    }

    #[test]
    fn test_hangup_nonzero_argument_does_not_disconnect() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATDT5551234");
        collect_response(&mut modem);
        assert!(modem.is_connected());

        modem.mode = ModemMode::OnlineCommand;
        send_command(&mut modem, "ATH1");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));
        assert!(modem.is_connected());
    }

    #[test]
    fn test_hangup_without_connection() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATH0");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));
        assert!(!modem.is_connected());
    }

    #[test]
    fn test_result_codes() {
        assert_eq!(ResultCode::Ok.numeric(), 0);
        assert_eq!(ResultCode::Connect.numeric(), 1);
        assert_eq!(ResultCode::NoCarrier.numeric(), 3);
        assert_eq!(ResultCode::Error.numeric(), 4);

        assert_eq!(ResultCode::Ok.verbose(), "OK");
        assert_eq!(ResultCode::Connect.verbose(), "CONNECT");
        assert_eq!(ResultCode::NoCarrier.verbose(), "NO CARRIER");
    }

    #[test]
    fn test_multiple_commands() {
        let mut modem = VirtualModem::new();

        // Multiple commands on one line
        send_command(&mut modem, "ATE0V0Q0");
        let _ = collect_response(&mut modem);

        assert!(!modem.echo);
        assert!(!modem.verbose);
        assert!(!modem.quiet);
    }

    #[test]
    fn test_view_config() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "AT&V");
        let response = collect_response(&mut modem);
        assert!(
            response.contains("E1"),
            "Should show echo state: {response}"
        );
    }

    #[test]
    fn test_view_config_flags() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATX1Q1");
        let _ = collect_response(&mut modem);

        send_command(&mut modem, "AT&V");
        let response = collect_response(&mut modem);
        assert!(response.contains("Q1"));
        assert!(response.contains("X4"));
    }

    #[test]
    fn test_view_config_flags_off() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATE0V0X0");
        let _ = collect_response(&mut modem);

        send_command(&mut modem, "AT&V");
        let response = collect_response(&mut modem);
        assert!(response.contains("E0"));
        assert!(response.contains("V0"));
        assert!(response.contains("X0"));
    }

    // ---- Dial command variants (lines 624-667) ----

    #[test]
    fn test_dial_dtmf_extensions() {
        let mut modem = VirtualModem::new();

        // DTMF extensions A, B, C, D are valid dial characters
        send_command(&mut modem, "ATDT5551234A");
        let response = collect_response(&mut modem);
        assert!(response.contains("CONNECT"));
        assert!(modem.is_connected());
        assert_eq!(modem.dial_string.as_deref(), Some("5551234A"));
    }

    #[test]
    fn test_dial_dtmf_all_extensions() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATDT1ABCD");
        let response = collect_response(&mut modem);
        assert!(response.contains("CONNECT"));
        assert_eq!(modem.dial_string.as_deref(), Some("1ABCD"));
    }

    #[test]
    fn test_dial_star_hash() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATDT*67#5551234");
        let response = collect_response(&mut modem);
        assert!(response.contains("CONNECT"));
        assert_eq!(modem.dial_string.as_deref(), Some("*67#5551234"));
    }

    #[test]
    fn test_dial_comma_pause() {
        let mut modem = VirtualModem::new();

        // Commas are pause characters, should be stripped from number
        send_command(&mut modem, "ATDT555,1,234");
        let response = collect_response(&mut modem);
        assert!(response.contains("CONNECT"));
        // Commas are not added to the number string
        assert_eq!(modem.dial_string.as_deref(), Some("5551234"));
    }

    #[test]
    fn test_dial_semicolon() {
        let mut modem = VirtualModem::new();

        // Semicolon = DTMF only, return to command mode with OK
        send_command(&mut modem, "ATDT5551234;");
        let response = collect_response(&mut modem);
        assert!(
            response.contains("OK"),
            "Semicolon dial should return OK: {response}"
        );
        assert!(!modem.is_connected());
        assert_eq!(modem.dial_string.as_deref(), Some("5551234"));
    }

    #[test]
    fn test_dial_formatting_chars() {
        let mut modem = VirtualModem::new();

        // Dashes, parens, and spaces are formatting (ignored)
        send_command(&mut modem, "ATDT555-1234");
        let response = collect_response(&mut modem);
        assert!(response.contains("CONNECT"));
        assert_eq!(modem.dial_string.as_deref(), Some("5551234"));
    }

    #[test]
    fn test_dial_parens() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATDT(555)1234");
        let response = collect_response(&mut modem);
        assert!(response.contains("CONNECT"));
        assert_eq!(modem.dial_string.as_deref(), Some("5551234"));
    }

    #[test]
    fn test_dial_spaces() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATDT555 1234");
        let response = collect_response(&mut modem);
        assert!(response.contains("CONNECT"));
        assert_eq!(modem.dial_string.as_deref(), Some("5551234"));
    }

    #[test]
    fn test_dial_unknown_chars() {
        let mut modem = VirtualModem::new();

        // Unknown characters like @ are silently ignored
        send_command(&mut modem, "ATDT555@1234");
        let response = collect_response(&mut modem);
        assert!(response.contains("CONNECT"));
        assert_eq!(modem.dial_string.as_deref(), Some("5551234"));
    }

    #[test]
    fn test_dial_only_formatting_chars_errors() {
        let mut modem = VirtualModem::new();

        // Only formatting chars -> empty number -> ERROR
        send_command(&mut modem, "ATDT---");
        let response = collect_response(&mut modem);
        assert!(response.contains("ERROR"));
        assert!(!modem.is_connected());
    }

    // ---- Connection speed result codes (lines 676-687) ----

    #[test]
    fn test_simulate_connection_1200() {
        let mut modem = VirtualModem::new();
        modem.extended_results = true; // X1: include speed in CONNECT
        modem.simulate_connection(1200);
        let response = collect_response(&mut modem);
        assert!(response.contains("CONNECT 1200"), "Got: {response}");
        assert!(modem.is_connected());
    }

    #[test]
    fn test_simulate_connection_2400() {
        let mut modem = VirtualModem::new();
        modem.extended_results = true;
        modem.simulate_connection(2400);
        let response = collect_response(&mut modem);
        assert!(response.contains("CONNECT 2400"), "Got: {response}");
    }

    #[test]
    fn test_simulate_connection_4800() {
        let mut modem = VirtualModem::new();
        modem.extended_results = true;
        modem.simulate_connection(4800);
        let response = collect_response(&mut modem);
        assert!(response.contains("CONNECT 4800"), "Got: {response}");
    }

    #[test]
    fn test_simulate_connection_14400() {
        let mut modem = VirtualModem::new();
        modem.extended_results = true;
        modem.simulate_connection(14400);
        let response = collect_response(&mut modem);
        assert!(response.contains("CONNECT 14400"), "Got: {response}");
    }

    #[test]
    fn test_simulate_connection_19200() {
        let mut modem = VirtualModem::new();
        modem.extended_results = true;
        modem.simulate_connection(19200);
        let response = collect_response(&mut modem);
        assert!(response.contains("CONNECT 19200"), "Got: {response}");
    }

    #[test]
    fn test_simulate_connection_38400() {
        let mut modem = VirtualModem::new();
        modem.extended_results = true;
        modem.simulate_connection(38400);
        let response = collect_response(&mut modem);
        assert!(response.contains("CONNECT 38400"), "Got: {response}");
    }

    #[test]
    fn test_simulate_connection_57600() {
        let mut modem = VirtualModem::new();
        modem.extended_results = true;
        modem.simulate_connection(57600);
        let response = collect_response(&mut modem);
        assert!(response.contains("CONNECT 57600"), "Got: {response}");
    }

    #[test]
    fn test_simulate_connection_115200() {
        let mut modem = VirtualModem::new();
        modem.extended_results = true;
        modem.simulate_connection(115200);
        let response = collect_response(&mut modem);
        assert!(response.contains("CONNECT 115200"), "Got: {response}");
    }

    #[test]
    fn test_simulate_connection_unknown_speed() {
        let mut modem = VirtualModem::new();
        modem.extended_results = true;
        modem.simulate_connection(99999);
        let response = collect_response(&mut modem);
        // Unknown speed falls through to generic CONNECT (no speed suffix)
        assert!(response.contains("CONNECT"), "Got: {response}");
        // Should be bare CONNECT with no speed number
        assert!(
            response.contains("CONNECT\r\n"),
            "Should be bare CONNECT: {response}"
        );
    }

    // ---- Extended AT& commands (lines 742-782) ----

    #[test]
    fn test_extended_cmd_c0() {
        // Parsed but no observable effect in emulation; verify it returns OK.
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "AT&C0");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));
    }

    #[test]
    fn test_extended_cmd_c1() {
        // Parsed but no observable effect in emulation; verify it returns OK.
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "AT&C1");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));
    }

    #[test]
    fn test_extended_cmd_d0() {
        // Parsed but no observable effect in emulation; verify it returns OK.
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "AT&D0");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));
    }

    #[test]
    fn test_extended_cmd_d1() {
        // Parsed but no observable effect in emulation; verify it returns OK.
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "AT&D1");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));
    }

    #[test]
    fn test_extended_cmd_d2() {
        // Parsed but no observable effect in emulation; verify it returns OK.
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "AT&D2");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));
    }

    #[test]
    fn test_extended_cmd_k0() {
        // Parsed but no observable effect in emulation; verify it returns OK.
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "AT&K0");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));
        assert_eq!(modem.flow_control, 0);
    }

    #[test]
    fn test_extended_cmd_k1() {
        // Parsed but no observable effect in emulation; verify it returns OK.
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "AT&K1");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));
        assert_eq!(modem.flow_control, 1);
    }

    #[test]
    fn test_extended_cmd_k2() {
        // Parsed but no observable effect in emulation; verify it returns OK.
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "AT&K2");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));
        assert_eq!(modem.flow_control, 2);
    }

    #[test]
    fn test_extended_cmd_w0() {
        // Parsed but no observable effect in emulation; verify it returns OK.
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "AT&W0");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));
    }

    // ---- ATI information levels (lines 692-705) ----

    #[test]
    fn test_info_level_1() {
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "ATI1");
        let response = collect_response(&mut modem);
        assert!(response.contains("V1.0"), "Expected version: {response}");
        assert!(response.contains("OK"));
    }

    #[test]
    fn test_info_level_2() {
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "ATI2");
        let response = collect_response(&mut modem);
        // Level 2 returns "OK" as the info string, plus the result OK
        assert!(response.contains("OK"));
    }

    #[test]
    fn test_info_level_3() {
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "ATI3");
        let response = collect_response(&mut modem);
        assert!(
            response.contains("GP64 Virtual Modem V1.0"),
            "Expected full ID: {response}"
        );
    }

    #[test]
    fn test_info_level_4() {
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "ATI4");
        let response = collect_response(&mut modem);
        assert!(
            response.contains("Hayes Compatible"),
            "Expected Hayes compat: {response}"
        );
    }

    // ---- Remote data injection modes (lines 301-308) ----

    #[test]
    fn test_receive_from_remote_ignored_in_online_command_mode() {
        let mut modem = VirtualModem::new();

        // Connect first
        send_command(&mut modem, "ATDT5551234");
        collect_response(&mut modem);
        assert!(modem.is_connected());

        // Switch to OnlineCommand mode
        modem.mode = ModemMode::OnlineCommand;

        // Data should be ignored (not Online mode)
        modem.receive_from_remote(b"Hello");
        assert!(!modem.has_data_for_serial());
    }

    #[test]
    fn test_receive_from_remote_ignored_in_command_mode() {
        let mut modem = VirtualModem::new();

        // Not connected, command mode - data should be ignored
        modem.receive_from_remote(b"Hello");
        assert!(!modem.has_data_for_serial());
    }

    #[test]
    fn test_receive_from_remote_connected_but_command_mode() {
        let mut modem = VirtualModem::new();

        // Connected but in command mode (odd state but testable)
        modem.connected = true;
        modem.mode = ModemMode::Command;
        modem.receive_from_remote(b"Data");
        assert!(!modem.has_data_for_serial());
    }

    // ---- Answer command (lines 498-501) ----

    #[test]
    fn test_answer_command() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATA");
        let response = collect_response(&mut modem);
        assert!(
            response.contains("NO CARRIER"),
            "Expected NO CARRIER: {response}"
        );
    }

    // ---- X parameter variants (lines 559-562) ----

    #[test]
    fn test_x_parameter_0() {
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "ATX0");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));
        assert!(!modem.extended_results);
    }

    #[test]
    fn test_x_parameter_1() {
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "ATX1");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));
        assert!(modem.extended_results);
    }

    #[test]
    fn test_x_parameter_4() {
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "ATX4");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));
        assert!(modem.extended_results);
    }

    // ---- O parameter variants (lines 534-541) ----

    #[test]
    fn test_o_parameter_0() {
        let mut modem = VirtualModem::new();

        // Connect and switch to OnlineCommand
        send_command(&mut modem, "ATDT5551234");
        collect_response(&mut modem);
        modem.mode = ModemMode::OnlineCommand;

        send_command(&mut modem, "ATO0");
        let response = collect_response(&mut modem);
        assert!(response.contains("CONNECT"));
        assert_eq!(modem.mode(), ModemMode::Online);
    }

    #[test]
    fn test_o_parameter_1() {
        let mut modem = VirtualModem::new();

        // Connect and switch to OnlineCommand
        send_command(&mut modem, "ATDT5551234");
        collect_response(&mut modem);
        modem.mode = ModemMode::OnlineCommand;

        send_command(&mut modem, "ATO1");
        let response = collect_response(&mut modem);
        assert!(response.contains("CONNECT"));
        assert_eq!(modem.mode(), ModemMode::Online);
    }

    // ---- S-register overflow (line 610+) ----

    #[test]
    fn test_s_register_value_overflow() {
        let mut modem = VirtualModem::new();

        // Value 256 > 255 should return ERROR
        send_command(&mut modem, "ATS0=256");
        let response = collect_response(&mut modem);
        assert!(
            response.contains("ERROR"),
            "Expected ERROR for overflow: {response}"
        );
    }

    /// MDD-BUG-CRU-01030: numbers past `u16` neither panic nor wrap into range.
    /// 65578 is 65536 + 42, which a wrapping accumulator turns into a legal 42.
    #[test]
    fn oversized_s_register_numbers_are_errors_in_both_positions() {
        let digits = "9".repeat(MAX_AT_COMMAND_LEN - "ATS0=".len());
        let values = ["256", "65535", "65536", "65578", digits.as_str()];
        for value in values {
            let mut modem = VirtualModem::new();
            let before = modem.s_registers;
            send_command(&mut modem, &format!("ATS0={value}"));
            let response = collect_response(&mut modem);
            assert!(response.contains("ERROR"), "ATS0={value}: {response}");
            assert_eq!(modem.s_registers, before, "ATS0={value} changed a register");

            let mut modem = VirtualModem::new();
            let before = modem.s_registers;
            send_command(&mut modem, &format!("ATS{value}=7"));
            let response = collect_response(&mut modem);
            assert!(response.contains("ERROR"), "ATS{value}=7: {response}");
            assert_eq!(modem.s_registers, before, "ATS{value}=7 changed a register");
        }

        let mut modem = VirtualModem::new();
        send_command(&mut modem, "ATS255=255");
        let response = collect_response(&mut modem);
        assert!(
            response.contains("OK"),
            "the 255 boundary stays legal: {response}"
        );
        assert_eq!(modem.s_registers[255], 255);
    }

    #[test]
    fn test_s_register_value_max_valid() {
        let mut modem = VirtualModem::new();

        // Value 255 should be accepted
        send_command(&mut modem, "ATS0=255");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));
        assert_eq!(modem.s_registers[0], 255);
    }

    #[test]
    fn test_s_register_select_only() {
        let mut modem = VirtualModem::new();

        // Just selecting a register without ? or = should be OK
        send_command(&mut modem, "ATS5");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));
    }

    // ---- Escape sequence edge cases (lines 416-440) ----

    #[test]
    fn test_escape_four_pluses() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATDT5551234");
        collect_response(&mut modem);

        // Set short guard time and advance past it
        modem.s_registers[sregs::S12] = 1;
        modem.current_tick = 100;
        modem.last_char_time = 0;

        // Send 4 pluses - 4th breaks the escape pattern
        modem.receive_from_serial(b'+');
        assert_eq!(modem.escape_state, EscapeState::Plus1);
        modem.receive_from_serial(b'+');
        assert_eq!(modem.escape_state, EscapeState::Plus2);
        modem.receive_from_serial(b'+');
        assert_eq!(modem.escape_state, EscapeState::Plus3);
        modem.receive_from_serial(b'+');
        // 4th plus breaks the escape sequence - all chars flushed to remote
        assert_eq!(modem.escape_state, EscapeState::None);
    }

    #[test]
    fn test_escape_reset_on_non_plus_char() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATDT5551234");
        collect_response(&mut modem);

        modem.s_registers[sregs::S12] = 1;
        modem.current_tick = 100;
        modem.last_char_time = 0;

        // Start escape sequence
        modem.receive_from_serial(b'+');
        assert_eq!(modem.escape_state, EscapeState::Plus1);

        // Non-plus character resets escape state
        modem.receive_from_serial(b'A');
        assert_eq!(modem.escape_state, EscapeState::None);
    }

    // ---- Full connection/disconnection cycle ----

    #[test]
    fn test_full_connection_cycle() {
        let mut modem = VirtualModem::new();

        // 1. Dial
        send_command(&mut modem, "ATDT5551234");
        let response = collect_response(&mut modem);
        assert!(response.contains("CONNECT"));
        assert!(modem.is_connected());
        assert_eq!(modem.mode(), ModemMode::Online);

        // 2. Transfer data
        modem.receive_from_serial(b'H');
        modem.receive_from_serial(b'i');
        assert_eq!(modem.send_to_remote(), Some(b'H'));
        assert_eq!(modem.send_to_remote(), Some(b'i'));

        // 3. Receive from remote
        modem.receive_from_remote(b"Reply");
        let mut received = Vec::new();
        while let Some(b) = modem.send_to_serial() {
            received.push(b);
        }
        assert_eq!(&received, b"Reply");

        // 4. Escape to command mode
        modem.s_registers[sregs::S12] = 1;
        modem.current_tick = 100;
        modem.last_char_time = 0;
        modem.receive_from_serial(b'+');
        modem.receive_from_serial(b'+');
        modem.receive_from_serial(b'+');
        modem.tick(200);
        let response = collect_response(&mut modem);
        assert_eq!(modem.mode(), ModemMode::OnlineCommand);
        assert!(response.contains("OK"));

        // 5. Return online
        send_command(&mut modem, "ATO");
        let response = collect_response(&mut modem);
        assert_eq!(modem.mode(), ModemMode::Online);
        assert!(response.contains("CONNECT"));

        // 6. Escape again and hangup
        modem.s_registers[sregs::S12] = 1;
        modem.current_tick = 400;
        modem.last_char_time = 0;
        modem.receive_from_serial(b'+');
        modem.receive_from_serial(b'+');
        modem.receive_from_serial(b'+');
        modem.tick(600);
        collect_response(&mut modem);
        assert_eq!(modem.mode(), ModemMode::OnlineCommand);

        send_command(&mut modem, "ATH0");
        let response = collect_response(&mut modem);
        assert!(response.contains("NO CARRIER"));
        assert!(!modem.is_connected());
        assert_eq!(modem.mode(), ModemMode::Command);
    }

    // ---- Additional result code coverage ----

    #[test]
    fn test_result_codes_all_numeric() {
        assert_eq!(ResultCode::Ring.numeric(), 2);
        assert_eq!(ResultCode::Connect1200.numeric(), 5);
        assert_eq!(ResultCode::NoDialtone.numeric(), 6);
        assert_eq!(ResultCode::Busy.numeric(), 7);
        assert_eq!(ResultCode::NoAnswer.numeric(), 8);
        assert_eq!(ResultCode::Connect2400.numeric(), 10);
        assert_eq!(ResultCode::Connect4800.numeric(), 11);
        assert_eq!(ResultCode::Connect9600.numeric(), 12);
        assert_eq!(ResultCode::Connect14400.numeric(), 13);
        assert_eq!(ResultCode::Connect19200.numeric(), 14);
        assert_eq!(ResultCode::Connect38400.numeric(), 28);
        assert_eq!(ResultCode::Connect57600.numeric(), 86);
        assert_eq!(ResultCode::Connect115200.numeric(), 87);
    }

    #[test]
    fn test_result_codes_all_verbose() {
        assert_eq!(ResultCode::Error.verbose(), "ERROR");
        assert_eq!(ResultCode::Ring.verbose(), "RING");
        assert_eq!(ResultCode::NoDialtone.verbose(), "NO DIALTONE");
        assert_eq!(ResultCode::Busy.verbose(), "BUSY");
        assert_eq!(ResultCode::NoAnswer.verbose(), "NO ANSWER");
        assert_eq!(ResultCode::Connect1200.verbose(), "CONNECT 1200");
        assert_eq!(ResultCode::Connect2400.verbose(), "CONNECT 2400");
        assert_eq!(ResultCode::Connect4800.verbose(), "CONNECT 4800");
        assert_eq!(ResultCode::Connect9600.verbose(), "CONNECT 9600");
        assert_eq!(ResultCode::Connect14400.verbose(), "CONNECT 14400");
        assert_eq!(ResultCode::Connect19200.verbose(), "CONNECT 19200");
        assert_eq!(ResultCode::Connect38400.verbose(), "CONNECT 38400");
        assert_eq!(ResultCode::Connect57600.verbose(), "CONNECT 57600");
        assert_eq!(ResultCode::Connect115200.verbose(), "CONNECT 115200");
    }

    // ---- Modem signal accessors ----

    #[test]
    fn test_get_dcd_reflects_connection() {
        let mut modem = VirtualModem::new();
        assert!(!modem.get_dcd());

        modem.simulate_connection(9600);
        collect_response(&mut modem);
        assert!(modem.get_dcd());
    }

    #[test]
    fn test_get_dsr_always_true() {
        let modem = VirtualModem::new();
        assert!(modem.get_dsr());
    }

    #[test]
    fn test_get_cts_always_true() {
        let modem = VirtualModem::new();
        assert!(modem.get_cts());
    }

    #[test]
    fn test_get_ri_always_false() {
        let modem = VirtualModem::new();
        assert!(!modem.get_ri());
    }

    // ---- Speaker controls ----

    #[test]
    fn test_speaker_volume_control() {
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "ATL3");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));
        assert_eq!(modem.speaker_volume, 3);
    }

    #[test]
    fn test_speaker_mode_control() {
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "ATM0");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));
        assert_eq!(modem.speaker_mode, 0);
    }

    // ---- Default instance ----

    #[test]
    fn test_default_matches_new() {
        let default_modem = VirtualModem::default();
        let new_modem = VirtualModem::new();
        assert_eq!(default_modem.mode(), new_modem.mode());
        assert_eq!(default_modem.is_connected(), new_modem.is_connected());
        assert_eq!(default_modem.echo, new_modem.echo);
        assert_eq!(default_modem.verbose, new_modem.verbose);
    }

    // ---- has_data_for_remote ----

    #[test]
    fn test_has_data_for_remote() {
        let mut modem = VirtualModem::new();
        assert!(!modem.has_data_for_remote());

        // Connect and send data
        send_command(&mut modem, "ATDT5551234");
        collect_response(&mut modem);

        modem.receive_from_serial(b'X');
        assert!(modem.has_data_for_remote());
        assert_eq!(modem.send_to_remote(), Some(b'X'));
        assert!(!modem.has_data_for_remote());
    }

    // ---- set_mode ----

    #[test]
    fn test_set_mode() {
        let mut modem = VirtualModem::new();
        assert_eq!(modem.mode(), ModemMode::Command);

        modem.set_mode(ModemMode::Online);
        assert_eq!(modem.mode(), ModemMode::Online);

        modem.set_mode(ModemMode::OnlineCommand);
        assert_eq!(modem.mode(), ModemMode::OnlineCommand);
    }

    // ---- is_echo / is_verbose / is_quiet accessors ----

    #[test]
    fn test_accessors() {
        let mut modem = VirtualModem::new();
        assert!(modem.is_echo_enabled());
        assert!(modem.is_verbose_enabled());
        assert!(!modem.is_quiet_enabled());

        modem.echo = false;
        modem.verbose = false;
        modem.quiet = true;
        assert!(!modem.is_echo_enabled());
        assert!(!modem.is_verbose_enabled());
        assert!(modem.is_quiet_enabled());
    }

    // ---- parse_number with multi-digit values ----

    #[test]
    fn test_s_register_multi_digit_value() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATS0=123");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));
        assert_eq!(modem.s_registers[0], 123);

        send_command(&mut modem, "ATS0?");
        let response = collect_response(&mut modem);
        assert!(response.contains("123"), "Got: {response}");
    }

    // ---- Spaces in AT commands ----

    #[test]
    fn test_spaces_in_commands() {
        let mut modem = VirtualModem::new();

        // Spaces should be ignored in command parsing
        send_command(&mut modem, "AT E0 V0");
        let _ = collect_response(&mut modem);
        assert!(!modem.echo);
        assert!(!modem.verbose);
    }

    // ---- Numeric mode result codes for multi-digit codes ----

    #[test]
    fn test_numeric_error_code() {
        let mut modem = VirtualModem::new();

        // Set numeric mode
        send_command(&mut modem, "ATE0V0");
        collect_response(&mut modem);

        // Unknown command should produce ERROR (code 4)
        send_command(&mut modem, "ATXYZ");
        let response = collect_response(&mut modem);
        assert!(response.contains('4'), "Expected code 4: {response}");
    }

    // ---- S-register high index (e.g., S255) ----

    #[test]
    fn test_s_register_high_index() {
        let mut modem = VirtualModem::new();

        send_command(&mut modem, "ATS255=42");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));
        assert_eq!(modem.s_registers[255], 42);

        send_command(&mut modem, "ATS255?");
        let response = collect_response(&mut modem);
        assert!(response.contains("042"), "Got: {response}");
    }

    // ---- Q parameter (quiet) toggle ----

    #[test]
    fn test_quiet_mode_toggle() {
        let mut modem = VirtualModem::new();

        // Turn quiet on
        send_command(&mut modem, "ATQ1");
        let _ = collect_response(&mut modem);
        assert!(modem.quiet);

        // Turn quiet off
        send_command(&mut modem, "ATQ0");
        let _ = collect_response(&mut modem);
        assert!(!modem.quiet);
    }

    // ---- Stage 1: ModemModel, HandshakePhase, new AT commands, A/, dial semicolon ----

    #[test]
    fn test_modem_model_max_speed() {
        assert_eq!(ModemModel::Modem2400.max_speed(), 2400);
        assert_eq!(ModemModel::Modem9600.max_speed(), 9600);
        assert_eq!(ModemModel::Modem14400.max_speed(), 14400);
        assert_eq!(ModemModel::Modem28800.max_speed(), 28800);
        assert_eq!(ModemModel::Modem56k.max_speed(), 56000);
    }

    #[test]
    fn test_modem_model_handshake_duration() {
        assert_eq!(ModemModel::Modem2400.handshake_duration_ms(), 800);
        assert_eq!(ModemModel::Modem9600.handshake_duration_ms(), 2000);
        assert_eq!(ModemModel::Modem14400.handshake_duration_ms(), 3000);
        assert_eq!(ModemModel::Modem28800.handshake_duration_ms(), 5000);
        assert_eq!(ModemModel::Modem56k.handshake_duration_ms(), 8000);
    }

    #[test]
    fn test_modem_model_bytes_per_sec() {
        assert_eq!(ModemModel::Modem2400.bytes_per_sec(), 240);
        assert_eq!(ModemModel::Modem9600.bytes_per_sec(), 960);
        assert_eq!(ModemModel::Modem14400.bytes_per_sec(), 1440);
        assert_eq!(ModemModel::Modem28800.bytes_per_sec(), 2880);
        assert_eq!(ModemModel::Modem56k.bytes_per_sec(), 5600);
    }

    #[test]
    fn test_modem_model_training_segments() {
        // Each model should return non-empty segments whose total duration
        // matches the handshake_duration_ms.
        for model in &[
            ModemModel::Modem2400,
            ModemModel::Modem9600,
            ModemModel::Modem14400,
            ModemModel::Modem28800,
            ModemModel::Modem56k,
        ] {
            let segments = model.training_segments();
            assert!(
                !segments.is_empty(),
                "{:?} should have training segments",
                model
            );
            let total_ms: u32 = segments
                .iter()
                .map(|seg| match seg {
                    TrainingSegment::Tone { duration_ms, .. } => *duration_ms,
                    TrainingSegment::Sweep { duration_ms, .. } => *duration_ms,
                    TrainingSegment::Noise { duration_ms, .. } => *duration_ms,
                    TrainingSegment::Silence { duration_ms, .. } => *duration_ms,
                })
                .sum();
            assert_eq!(
                total_ms,
                model.handshake_duration_ms(),
                "{:?} training segments total {total_ms}ms, expected {}ms",
                model,
                model.handshake_duration_ms()
            );
        }
    }

    #[test]
    fn test_connect_28800_result_code() {
        assert_eq!(ResultCode::Connect28800.numeric(), 18);
        assert_eq!(ResultCode::Connect28800.verbose(), "CONNECT 28800");
    }

    #[test]
    fn test_connect_56000_result_code() {
        assert_eq!(ResultCode::Connect56000.numeric(), 20);
        assert_eq!(ResultCode::Connect56000.verbose(), "CONNECT 56000");
    }

    #[test]
    fn test_at_backslash_n_noop() {
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "AT\\N3");
        let response = collect_response(&mut modem);
        assert!(
            response.contains("OK"),
            "Expected OK for AT\\N3: {response}"
        );
    }

    #[test]
    fn test_at_percent_c_noop() {
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "AT%C1");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"), "Expected OK for AT%C1: {response}");
    }

    #[test]
    fn test_at_w_noop() {
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "ATW2");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"), "Expected OK for ATW2: {response}");
    }

    #[test]
    fn test_at_ampersand_s_noop() {
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "AT&S1");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"), "Expected OK for AT&S1: {response}");
    }

    #[test]
    fn test_at_ampersand_q_noop() {
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "AT&Q5");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"), "Expected OK for AT&Q5: {response}");
    }

    #[test]
    fn test_a_slash_repeats_last() {
        let mut modem = VirtualModem::new();

        // Send ATE0 to disable echo
        send_command(&mut modem, "ATE0");
        let _ = collect_response(&mut modem);
        assert!(!modem.echo);

        // Re-enable echo so we can verify A/ re-runs ATE0
        modem.echo = true;
        assert!(modem.echo);

        // Send A/ (repeats ATE0), which should disable echo again.
        // A/ executes immediately on the '/' — no CR needed.
        for byte in b"A/" {
            modem.receive_from_serial(*byte);
        }
        let _ = collect_response(&mut modem);
        assert!(!modem.echo, "A/ should have replayed ATE0, disabling echo");
    }

    #[test]
    fn test_a_slash_no_previous_errors() {
        let mut modem = VirtualModem::new();

        // Send A/ with no previous command — should get ERROR
        for byte in b"A/" {
            modem.receive_from_serial(*byte);
        }
        let response = collect_response(&mut modem);
        assert!(
            response.contains("ERROR"),
            "A/ with no previous command should ERROR: {response}"
        );
    }

    #[test]
    fn test_dial_semicolon_returns_command() {
        let mut modem = VirtualModem::new();

        // Dial with trailing semicolon should return OK in command mode,
        // not CONNECT
        send_command(&mut modem, "ATDT5551234;");
        let response = collect_response(&mut modem);
        assert!(
            response.contains("OK"),
            "Dial with ; should return OK: {response}"
        );
        assert!(
            !response.contains("CONNECT"),
            "Dial with ; should NOT connect: {response}"
        );
        assert!(!modem.is_connected());
        assert_eq!(modem.mode(), ModemMode::Command);
    }

    #[test]
    fn test_modem_model_display_name() {
        // Verify all 5 models return expected display strings
        assert_eq!(ModemModel::Modem2400.display_name(), "2400");
        assert_eq!(ModemModel::Modem9600.display_name(), "9600");
        assert_eq!(ModemModel::Modem14400.display_name(), "14400");
        assert_eq!(ModemModel::Modem28800.display_name(), "28800");
        assert_eq!(ModemModel::Modem56k.display_name(), "56K");
    }

    #[test]
    fn test_a_slash_after_error_replays() {
        // A/ after a command that returned ERROR should replay and get ERROR again
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "AT&Z"); // Invalid extended command -> ERROR
        let response = collect_response(&mut modem);
        assert!(
            response.contains("ERROR"),
            "Expected ERROR from AT&Z, got: {}",
            response
        );

        // Now send A/ - should replay AT&Z and get ERROR again
        for b in b"A/" {
            modem.receive_from_serial(*b);
        }
        let response = collect_response(&mut modem);
        assert!(
            response.contains("ERROR"),
            "Expected ERROR from A/ replay, got: {}",
            response
        );
    }

    #[test]
    fn test_a_slash_after_reset() {
        // ATZ stores as last_command, then resets. A/ should replay ATZ.
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "ATZ");
        let response = collect_response(&mut modem);
        assert!(response.contains("OK"));

        // A/ replays ATZ
        for b in b"A/" {
            modem.receive_from_serial(*b);
        }
        let response = collect_response(&mut modem);
        assert!(
            response.contains("OK"),
            "Expected OK from A/ replaying ATZ, got: {}",
            response
        );
    }

    #[test]
    fn test_dsr_mode_reset_by_factory_defaults() {
        // AT&S1 sets dsr_mode, AT&F should reset it
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "AT&S1");
        collect_response(&mut modem);
        assert_eq!(modem.dsr_mode(), 1);

        send_command(&mut modem, "AT&F");
        collect_response(&mut modem);
        assert_eq!(modem.dsr_mode(), 0);
    }

    #[test]
    fn test_compound_init_string() {
        // Real modem init strings should parse without ERROR
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "ATE0Q0V1W2\\N3%C1&S0&Q5");
        let response = collect_response(&mut modem);
        assert!(
            response.contains("OK"),
            "Init string should return OK, got: {}",
            response
        );
        assert!(!modem.is_echo_enabled()); // E0 took effect
    }

    #[test]
    fn test_dial_multiple_semicolons() {
        // Multiple semicolons in dial string - should return to command mode
        let mut modem = VirtualModem::new();
        send_command(&mut modem, "ATDT555;1234;");
        let response = collect_response(&mut modem);
        assert!(
            response.contains("OK"),
            "Multiple semicolons should return OK, got: {}",
            response
        );
        assert!(!modem.is_connected());
    }

    // === Stage 2: Handshake State Machine Tests ===

    #[test]
    fn test_handshake_phase_initial_idle() {
        let modem = VirtualModem::new();
        assert_eq!(modem.handshake_phase(), HandshakePhase::Idle);
    }

    #[test]
    fn test_caller_dial_starts_dialtone() {
        // ATDTxxx should start DialTone phase (not instant connect)
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem14400));
        send_command(&mut modem, "ATDT5551234");
        // Should be in DialTone phase, NOT connected
        assert!(!modem.is_connected());
        assert!(
            matches!(modem.handshake_phase(), HandshakePhase::DialTone { .. }),
            "Expected DialTone, got: {:?}",
            modem.handshake_phase()
        );
    }

    #[test]
    fn test_caller_dialtone_to_dtmf() {
        // After DialTone duration expires, transitions to Dtmf
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem14400));
        send_command(&mut modem, "ATDT5551234");
        collect_response(&mut modem); // drain echo

        // Tick through 2000ms of dial tone
        for _ in 0..2000 {
            modem.tick(1);
        }
        assert!(
            matches!(
                modem.handshake_phase(),
                HandshakePhase::Dtmf { digit_index: 0, .. }
            ),
            "Expected Dtmf digit 0, got: {:?}",
            modem.handshake_phase()
        );
    }

    #[test]
    fn test_caller_dtmf_sequence() {
        // DTMF plays each digit for S11 ms with 70ms gaps
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem14400));
        send_command(&mut modem, "ATDT12");
        collect_response(&mut modem);

        // Skip dial tone (2000ms)
        for _ in 0..2000 {
            modem.tick(1);
        }
        assert!(
            matches!(
                modem.handshake_phase(),
                HandshakePhase::Dtmf { digit_index: 0, .. }
            ),
            "Expected Dtmf digit 0, got: {:?}",
            modem.handshake_phase()
        );

        // S11 default is 95ms - tick through first digit
        for _ in 0..95 {
            modem.tick(1);
        }
        assert!(
            matches!(
                modem.handshake_phase(),
                HandshakePhase::DtmfGap { digit_index: 0, .. }
            ),
            "Expected DtmfGap after digit 0, got: {:?}",
            modem.handshake_phase()
        );

        // 70ms gap
        for _ in 0..70 {
            modem.tick(1);
        }
        assert!(
            matches!(
                modem.handshake_phase(),
                HandshakePhase::Dtmf { digit_index: 1, .. }
            ),
            "Expected Dtmf digit 1, got: {:?}",
            modem.handshake_phase()
        );
    }

    #[test]
    fn test_caller_dtmf_to_waiting_for_carrier() {
        // After all DTMF digits, enters WaitingForCarrier
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem14400));
        send_command(&mut modem, "ATDT1");
        collect_response(&mut modem);

        // DialTone(2000) + DTMF(95) = 2095, last digit goes to WaitingForCarrier
        for _ in 0..2000 {
            modem.tick(1);
        } // dial tone
        for _ in 0..95 {
            modem.tick(1);
        } // DTMF digit "1"
        assert!(
            matches!(
                modem.handshake_phase(),
                HandshakePhase::WaitingForCarrier { .. }
            ),
            "Expected WaitingForCarrier, got: {:?}",
            modem.handshake_phase()
        );
    }

    #[test]
    fn test_notify_tcp_connected_starts_training() {
        // When TCP connects during WaitingForCarrier, transition to Training
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem14400));
        send_command(&mut modem, "ATDT1");
        collect_response(&mut modem);

        // Advance to WaitingForCarrier
        for _ in 0..2096 {
            modem.tick(1);
        } // 2000 + 95 + 1 safety
        assert!(
            matches!(
                modem.handshake_phase(),
                HandshakePhase::WaitingForCarrier { .. }
            ),
            "Expected WaitingForCarrier, got: {:?}",
            modem.handshake_phase()
        );

        modem.notify_tcp_connected();
        // Should now be in Training
        assert!(
            matches!(modem.handshake_phase(), HandshakePhase::Training { .. }),
            "Expected Training, got: {:?}",
            modem.handshake_phase()
        );
    }

    #[test]
    fn test_training_completes_with_connect() {
        // Training duration expires -> Complete -> CONNECT result
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem2400)); // shortest: 800ms
        modem.extended_results = true; // X1: include speed in CONNECT
        send_command(&mut modem, "ATDT1");
        collect_response(&mut modem);

        // Advance to WaitingForCarrier (2000 + 95 = 2095)
        for _ in 0..2096 {
            modem.tick(1);
        }
        modem.notify_tcp_connected();

        // Tick through training (800ms for V.22bis)
        for _ in 0..800 {
            modem.tick(1);
        }

        // Should now be Complete and connected
        assert!(modem.is_connected());
        let response = collect_response(&mut modem);
        assert!(
            response.contains("CONNECT 2400"),
            "Expected CONNECT 2400, got: {}",
            response
        );
        assert_eq!(modem.handshake_phase(), HandshakePhase::Idle);
    }

    #[test]
    fn test_s7_timeout_no_carrier() {
        // S7 timeout in WaitingForCarrier sends NO CARRIER
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem14400));
        // S7 default = 50 seconds = 50000ms
        send_command(&mut modem, "ATS7=2"); // 2 second timeout for faster test
        collect_response(&mut modem);
        send_command(&mut modem, "ATDT1");
        collect_response(&mut modem);

        // Advance to WaitingForCarrier (2000ms dial tone + 95ms DTMF + 1 safety)
        for _ in 0..2096 {
            modem.tick(1);
        }
        assert!(
            matches!(
                modem.handshake_phase(),
                HandshakePhase::WaitingForCarrier { .. }
            ),
            "Expected WaitingForCarrier, got: {:?}",
            modem.handshake_phase()
        );

        // Tick through S7 timeout (2000ms)
        for _ in 0..2001 {
            modem.tick(1);
        }

        let response = collect_response(&mut modem);
        assert!(
            response.contains("NO CARRIER"),
            "Expected NO CARRIER, got: {}",
            response
        );
        assert_eq!(modem.handshake_phase(), HandshakePhase::Idle);
    }

    #[test]
    fn test_wants_connection_set_on_dial() {
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem14400));
        assert!(!modem.wants_connection());
        send_command(&mut modem, "ATDT5551234");
        assert!(modem.wants_connection());
    }

    #[test]
    fn test_abort_during_dialtone() {
        // Any character during DialTone aborts handshake (after grace period)
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem14400));
        send_command(&mut modem, "ATDT5551234");
        collect_response(&mut modem);

        // In DialTone now. Wait past the 200ms grace period
        for _ in 0..201 {
            modem.tick(1);
        }

        // Send a character to abort
        modem.receive_from_serial(b'x');
        modem.tick(1);

        let response = collect_response(&mut modem);
        assert!(
            response.contains("NO CARRIER") || response.contains("OK"),
            "Expected abort response, got: {}",
            response
        );
        assert_eq!(modem.handshake_phase(), HandshakePhase::Idle);
    }

    #[test]
    fn test_abort_during_training_ignored() {
        // Characters during Training are ignored (modem is "deaf")
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem2400));
        send_command(&mut modem, "ATDT1");
        collect_response(&mut modem);

        // Advance to WaitingForCarrier (2000ms dial tone + 95ms DTMF + 1 safety)
        for _ in 0..2096 {
            modem.tick(1);
        }
        modem.notify_tcp_connected();
        assert!(
            matches!(modem.handshake_phase(), HandshakePhase::Training { .. }),
            "Expected Training, got: {:?}",
            modem.handshake_phase()
        );

        // Send character during training - should be ignored
        modem.receive_from_serial(b'x');
        modem.tick(1);
        assert!(
            matches!(modem.handshake_phase(), HandshakePhase::Training { .. }),
            "Training should not be aborted by character"
        );
    }

    #[test]
    fn test_dtr_drop_aborts_training() {
        // DTR drop aborts at any phase including Training
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem2400));
        send_command(&mut modem, "ATDT1");
        collect_response(&mut modem);

        // Advance to WaitingForCarrier (2000ms dial tone + 95ms DTMF + 1 safety)
        for _ in 0..2096 {
            modem.tick(1);
        }
        modem.notify_tcp_connected();
        assert!(matches!(
            modem.handshake_phase(),
            HandshakePhase::Training { .. }
        ));

        modem.set_dtr(false);
        assert_eq!(modem.handshake_phase(), HandshakePhase::Idle);
    }

    #[test]
    fn test_dcd_during_training() {
        // DCD goes HIGH partway through Training (after ~60%)
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem2400)); // 800ms training
        send_command(&mut modem, "ATDT1");
        collect_response(&mut modem);

        // Advance to WaitingForCarrier (2000ms dial tone + 95ms DTMF + 1 safety)
        for _ in 0..2096 {
            modem.tick(1);
        }
        assert!(!modem.get_dcd()); // No DCD before training

        modem.notify_tcp_connected();
        // DCD should be false at start of training
        assert!(!modem.get_dcd());

        // Tick ~60% through training (480ms of 800ms)
        for _ in 0..481 {
            modem.tick(1);
        }
        assert!(modem.get_dcd(), "DCD should be HIGH after 60% of training");
    }

    #[test]
    fn test_answerer_incoming_ring() {
        // When TCP connects to host, start IncomingRing
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem14400));

        modem.start_incoming_ring();
        assert!(matches!(
            modem.handshake_phase(),
            HandshakePhase::IncomingRing { .. }
        ));

        // Should send RING result
        let response = collect_response(&mut modem);
        assert!(
            response.contains("RING"),
            "Expected RING, got: {}",
            response
        );
    }

    #[test]
    fn test_answerer_ri_signal() {
        // RI pulses during IncomingRing
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem14400));
        modem.start_incoming_ring();

        // RI should be HIGH at start
        assert!(modem.get_ri());

        // After 2000ms, RI goes LOW
        for _ in 0..2001 {
            modem.tick(1);
        }
        assert!(!modem.get_ri());
    }

    #[test]
    fn test_answerer_auto_answer() {
        // S0=1 means auto-answer after 1 ring
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem2400));
        send_command(&mut modem, "ATS0=1");
        collect_response(&mut modem);

        modem.start_incoming_ring();
        collect_response(&mut modem); // RING

        // One full ring cycle: 2s RI-high + 4s RI-low = 6000ms
        for _ in 0..6001 {
            modem.tick(1);
        }

        // After S0 rings, should auto-answer -> Training
        assert!(
            matches!(modem.handshake_phase(), HandshakePhase::Training { .. }),
            "Expected Training after auto-answer, got: {:?}",
            modem.handshake_phase()
        );
    }

    #[test]
    fn test_answerer_ata_manual_answer() {
        // ATA during IncomingRing triggers Training immediately
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem2400));
        modem.start_incoming_ring();
        collect_response(&mut modem);

        send_command(&mut modem, "ATA");
        // The ATA should transition to Training
        assert!(
            matches!(modem.handshake_phase(), HandshakePhase::Training { .. }),
            "Expected Training after ATA, got: {:?}",
            modem.handshake_phase()
        );
    }

    #[test]
    fn test_dial_without_model_uses_instant_connect() {
        // When no modem model is set, dial should use instant connect (backward compat)
        let mut modem = VirtualModem::new();
        // No set_modem_model call - model is None
        send_command(&mut modem, "ATDT5551234");
        // Should be instantly connected (old behavior)
        assert!(modem.is_connected());
    }

    #[test]
    fn test_notify_tcp_disconnected() {
        // TCP drop during online sends NO CARRIER
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem2400));
        send_command(&mut modem, "ATDT1");
        collect_response(&mut modem);

        // Complete handshake (2000ms dial tone + 95ms DTMF + 1 safety)
        for _ in 0..2096 {
            modem.tick(1);
        }
        modem.notify_tcp_connected();
        for _ in 0..801 {
            modem.tick(1);
        }
        assert!(modem.is_connected());
        collect_response(&mut modem); // drain CONNECT

        modem.notify_tcp_disconnected();
        let response = collect_response(&mut modem);
        assert!(response.contains("NO CARRIER"));
        assert!(!modem.is_connected());
    }

    #[test]
    fn test_modem_model_accessors() {
        let mut modem = VirtualModem::new();
        assert_eq!(modem.modem_model(), None);

        modem.set_modem_model(Some(ModemModel::Modem9600));
        assert_eq!(modem.modem_model(), Some(ModemModel::Modem9600));
    }

    #[test]
    fn test_is_answering() {
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem2400));
        assert!(!modem.is_answering());

        modem.start_incoming_ring();
        assert!(modem.is_answering());
    }

    #[test]
    fn test_grace_period_prevents_abort() {
        // Within 200ms grace period, characters should be ignored (not abort)
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem14400));
        send_command(&mut modem, "ATDT5551234");
        collect_response(&mut modem);

        // Only 100ms in — still within grace period
        for _ in 0..100 {
            modem.tick(1);
        }

        modem.receive_from_serial(b'x');
        // Should still be in DialTone (not aborted)
        assert!(
            matches!(modem.handshake_phase(), HandshakePhase::DialTone { .. }),
            "Expected DialTone during grace period, got: {:?}",
            modem.handshake_phase()
        );
    }

    #[test]
    fn test_reset_clears_handshake() {
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem14400));
        send_command(&mut modem, "ATDT1");
        collect_response(&mut modem);
        assert!(modem.wants_connection());

        modem.reset();
        assert_eq!(modem.handshake_phase(), HandshakePhase::Idle);
        assert!(!modem.wants_connection());
    }

    #[test]
    fn test_notify_tcp_connected_ignored_if_not_waiting() {
        // notify_tcp_connected should be a no-op if not in WaitingForCarrier (or early phase)
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem14400));
        modem.notify_tcp_connected();
        assert_eq!(modem.handshake_phase(), HandshakePhase::Idle);
    }

    #[test]
    fn test_notify_tcp_disconnected_ignored_if_not_connected() {
        // notify_tcp_disconnected should be a no-op if not connected
        let mut modem = VirtualModem::new();
        modem.notify_tcp_disconnected();
        assert!(!modem.is_connected());
        // No NO CARRIER should be queued
        let response = collect_response(&mut modem);
        assert!(
            !response.contains("NO CARRIER"),
            "Should not send NO CARRIER when not connected"
        );
    }

    #[test]
    fn test_handshake_abort_clears_wants_conn() {
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem14400));
        send_command(&mut modem, "ATDT5551234");
        collect_response(&mut modem);
        assert!(modem.wants_connection());

        // Wait past grace period and abort
        for _ in 0..201 {
            modem.tick(1);
        }
        modem.receive_from_serial(b'x');

        assert!(!modem.wants_connection());
        assert_eq!(modem.handshake_phase(), HandshakePhase::Idle);
    }

    #[test]
    fn test_answerer_training_completes_with_connect() {
        // Full answerer sequence: IncomingRing -> ATA -> Training -> CONNECT
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem2400)); // 800ms training
        modem.extended_results = true; // X1: include speed in CONNECT
        modem.start_incoming_ring();
        collect_response(&mut modem); // drain RING

        send_command(&mut modem, "ATA");
        collect_response(&mut modem); // drain any response from ATA
        assert!(matches!(
            modem.handshake_phase(),
            HandshakePhase::Training { .. }
        ));

        // Tick through full training duration (800ms)
        for _ in 0..801 {
            modem.tick(1);
        }

        assert!(modem.is_connected());
        let response = collect_response(&mut modem);
        assert!(
            response.contains("CONNECT 2400"),
            "Expected CONNECT 2400, got: {}",
            response
        );
        assert_eq!(modem.handshake_phase(), HandshakePhase::Idle);
    }

    #[test]
    fn test_tcp_connect_during_dtmf_buffered() {
        // TCP connects during DTMF (LAN scenario) - should be buffered
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem2400));
        send_command(&mut modem, "ATDT123");
        collect_response(&mut modem);

        // In DialTone now. TCP connects immediately (LAN).
        modem.notify_tcp_connected();

        // Advance through DialTone + DTMF to WaitingForCarrier
        // DialTone: 2000ms, DTMF: 3 digits x (95ms + 70ms) - 70ms (no trailing gap) = 425ms
        // Total: 2425ms
        for _ in 0..2426 {
            modem.tick(1);
        }

        // Should have immediately transitioned to Training (not stuck in WaitingForCarrier)
        assert!(
            matches!(modem.handshake_phase(), HandshakePhase::Training { .. }),
            "Expected Training after buffered TCP connect, got: {:?}",
            modem.handshake_phase()
        );
    }

    #[test]
    fn test_is_answering_caller_vs_answerer() {
        let mut modem = VirtualModem::new();
        modem.set_modem_model(Some(ModemModel::Modem14400));

        // Caller should not be "answering"
        send_command(&mut modem, "ATDT1");
        collect_response(&mut modem);
        assert!(!modem.is_answering());

        // Even during Training (2000ms dial tone + 95ms DTMF + 1 safety)
        for _ in 0..2096 {
            modem.tick(1);
        }
        modem.notify_tcp_connected();
        assert!(matches!(
            modem.handshake_phase(),
            HandshakePhase::Training { .. }
        ),);
        assert!(
            !modem.is_answering(),
            "Caller in Training should NOT be answering"
        );

        // Reset and test answerer
        modem.reset();
        modem.set_modem_model(Some(ModemModel::Modem14400));
        modem.start_incoming_ring();
        assert!(modem.is_answering(), "Answerer should be answering");
    }
}
