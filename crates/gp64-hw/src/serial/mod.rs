//! Serial port (8250/16550 UART) emulation
//!
//! COM1: 0x3F8-0x3FF, IRQ 4
//! COM2: 0x2F8-0x2FF, IRQ 3
//! COM3: 0x3E8-0x3EF, IRQ 4
//! COM4: 0x2E8-0x2EF, IRQ 3

pub mod modem;
pub mod mouse;

pub use modem::{HandshakePhase, ModemMode, ModemModel, TrainingSegment, VirtualModem};
pub use mouse::{SerialMouse, SerialMouseController, SerialMouseProtocol};

use gp64_types::FixedRing;

/// Line Status Register bits
pub mod lsr {
    /// Data Ready - receive buffer has data
    pub const DR: u8 = 0x01;
    /// Overrun Error - data lost
    pub const OE: u8 = 0x02;
    /// Parity Error
    pub const PE: u8 = 0x04;
    /// Framing Error
    pub const FE: u8 = 0x08;
    /// Break Interrupt
    pub const BI: u8 = 0x10;
    /// Transmitter Holding Register Empty
    pub const THRE: u8 = 0x20;
    /// Transmitter Empty (both THR and shift register)
    pub const TEMT: u8 = 0x40;
    /// Error in FIFO
    pub const FIFO_ERR: u8 = 0x80;
}

/// Modem Status Register bits
pub mod msr {
    /// Delta Clear To Send
    pub const DCTS: u8 = 0x01;
    /// Delta Data Set Ready
    pub const DDSR: u8 = 0x02;
    /// Trailing Edge Ring Indicator
    pub const TERI: u8 = 0x04;
    /// Delta Data Carrier Detect
    pub const DDCD: u8 = 0x08;
    /// Clear To Send
    pub const CTS: u8 = 0x10;
    /// Data Set Ready
    pub const DSR: u8 = 0x20;
    /// Ring Indicator
    pub const RI: u8 = 0x40;
    /// Data Carrier Detect
    pub const DCD: u8 = 0x80;
}

/// Modem Control Register bits
pub mod mcr {
    /// Data Terminal Ready
    pub const DTR: u8 = 0x01;
    /// Request To Send
    pub const RTS: u8 = 0x02;
    /// Out1 (auxiliary output 1)
    pub const OUT1: u8 = 0x04;
    /// Out2 (enables IRQ)
    pub const OUT2: u8 = 0x08;
    /// Loopback mode
    pub const LOOP: u8 = 0x10;
}

/// Interrupt Enable Register bits
pub mod ier {
    /// Enable Received Data Available Interrupt
    pub const ERBFI: u8 = 0x01;
    /// Enable Transmitter Holding Register Empty Interrupt
    pub const ETBEI: u8 = 0x02;
    /// Enable Receiver Line Status Interrupt
    pub const ELSI: u8 = 0x04;
    /// Enable Modem Status Interrupt
    pub const EDSSI: u8 = 0x08;
}

/// Interrupt Identification Register values
pub mod iir {
    /// No interrupt pending (bit 0 = 1 means no interrupt)
    pub const NO_INT: u8 = 0x01;
    /// Modem Status Change (lowest priority)
    pub const MODEM_STATUS: u8 = 0x00;
    /// Transmitter Holding Register Empty
    pub const THRE: u8 = 0x02;
    /// Received Data Available
    pub const RDA: u8 = 0x04;
    /// Receiver Line Status (highest priority)
    pub const RLS: u8 = 0x06;
    /// Character Timeout (16550 FIFO mode)
    pub const TIMEOUT: u8 = 0x0C;
    /// FIFO enabled bits (16550)
    pub const FIFO_ENABLED: u8 = 0xC0;
}

/// Line Control Register bits
pub mod lcr {
    /// Word Length Select bit 0
    pub const WLS0: u8 = 0x01;
    /// Word Length Select bit 1
    pub const WLS1: u8 = 0x02;
    /// Stop Bits (0 = 1 stop bit, 1 = 2 stop bits)
    pub const STB: u8 = 0x04;
    /// Parity Enable
    pub const PEN: u8 = 0x08;
    /// Even Parity Select
    pub const EPS: u8 = 0x10;
    /// Stick Parity
    pub const SP: u8 = 0x20;
    /// Set Break
    pub const SBRK: u8 = 0x40;
    /// Divisor Latch Access Bit
    pub const DLAB: u8 = 0x80;
}

/// FIFO Control Register bits (16550)
pub mod fcr {
    /// FIFO Enable
    pub const FE: u8 = 0x01;
    /// Receiver FIFO Reset
    pub const RFR: u8 = 0x02;
    /// Transmitter FIFO Reset
    pub const TFR: u8 = 0x04;
    /// DMA Mode Select
    pub const DMS: u8 = 0x08;
    /// Receiver Trigger Level bits (14 bytes: 0xC0)
    pub const RTL_MASK: u8 = 0xC0;
}

/// 16550 FIFO size
const FIFO_SIZE: usize = 16;
const LINE_QUEUE_SIZE: usize = 32;
const COMPLETED_TX_SIZE: usize = 32;
const UART_BASE_BAUD: u64 = 115_200;
const DEFAULT_UART_CYCLES_PER_SECOND: u64 = 25_200_000;

/// Serial port emulation (8250/16550 UART)
#[derive(Clone)]
pub struct SerialPort {
    /// COM port number (1-4)
    #[allow(dead_code)]
    port_num: u8,
    /// Base I/O port address
    base_port: u16,
    /// IRQ line
    irq: u8,

    // 8250/16550 registers
    /// Receive Buffer Register (read when DLAB=0)
    rbr: u8,
    /// Transmit Holding Register (write when DLAB=0)
    thr: u8,
    /// Whether the non-FIFO transmit holding register currently contains guest data.
    thr_loaded: bool,
    /// Interrupt Enable Register
    ier: u8,
    /// Interrupt Identification Register (read-only, computed)
    /// Note: actual IIR is computed from state, not stored
    /// FIFO Control Register (write-only, 16550)
    fcr: u8,
    /// Line Control Register
    lcr: u8,
    /// Modem Control Register
    mcr: u8,
    /// Line Status Register
    lsr: u8,
    /// Modem Status Register
    msr: u8,
    /// Scratch Register
    scr: u8,
    /// Divisor Latch Low
    dll: u8,
    /// Divisor Latch High
    dlh: u8,

    // 16550 FIFO state
    /// Receive FIFO
    rx_fifo: FixedRing<u8, 16>,
    /// Transmit FIFO
    tx_fifo: FixedRing<u8, 16>,
    /// Bytes that have fully left the UART line.
    completed_tx: FixedRing<u8, COMPLETED_TX_SIZE>,
    /// Bytes accepted from the line and waiting to enter the UART receiver.
    pending_line_rx: FixedRing<u8, LINE_QUEUE_SIZE>,
    /// FIFO enabled
    fifo_enabled: bool,
    /// Receiver trigger level (1, 4, 8, or 14 bytes)
    rx_trigger: usize,

    // Interrupt pending flags
    int_pending_rls: bool, // Receiver Line Status
    int_pending_rda: bool, // Received Data Available
    int_pending_msr: bool, // Modem Status

    // THRE interrupt logic
    /// THRE interrupt has been acknowledged (cleared on next IIR read or THR write)
    thre_int_pending: bool,

    // UART timing state
    current_cycle: u64,
    cycles_per_second: u64,
    char_cycles: u64,
    tx_shift: Option<u8>,
    tx_deadline_cycle: Option<u64>,
    rx_shift: Option<u8>,
    rx_deadline_cycle: Option<u64>,
    fifo_timeout_cycle: Option<u64>,

    // Character timeout state (16550 FIFO mode)
    /// Character timeout interrupt pending
    int_pending_timeout: bool,

    /// When true, modem status signals are managed externally instead of by the
    /// built-in null-modem reflection.
    pub external_tx: bool,

    /// Set when the guest resets the RX FIFO (FCR write with RFR bit, or FIFO
    /// disable). The serial networking layer checks and clears this flag to
    /// discard stale `pending_rx` bytes that would otherwise be re-injected
    /// into the freshly-reset FIFO.
    pub rx_fifo_reset: bool,
}

impl Default for SerialPort {
    fn default() -> Self {
        Self::new(1) // Default to COM1
    }
}

impl SerialPort {
    /// Create a new serial port
    pub fn new(port_num: u8) -> Self {
        let (base_port, irq) = match port_num {
            1 => (0x3F8, 4), // COM1
            2 => (0x2F8, 3), // COM2
            3 => (0x3E8, 4), // COM3
            4 => (0x2E8, 3), // COM4
            _ => (0x3F8, 4), // Default to COM1
        };

        let mut port = Self {
            port_num,
            base_port,
            irq,
            rbr: 0,
            thr: 0,
            thr_loaded: false,
            ier: 0,
            fcr: 0,
            lcr: 0,
            mcr: 0,
            lsr: lsr::THRE | lsr::TEMT, // Transmitter empty on reset
            msr: 0,
            scr: 0,
            dll: 0x01, // Default divisor for 115200 baud
            dlh: 0x00,
            rx_fifo: FixedRing::new(),
            tx_fifo: FixedRing::new(),
            completed_tx: FixedRing::new(),
            pending_line_rx: FixedRing::new(),
            fifo_enabled: false,
            rx_trigger: 1,
            int_pending_rls: false,
            int_pending_rda: false,
            int_pending_msr: false,
            thre_int_pending: true, // THRE is initially pending since THR is empty
            current_cycle: 0,
            cycles_per_second: DEFAULT_UART_CYCLES_PER_SECOND,
            char_cycles: 0,
            tx_shift: None,
            tx_deadline_cycle: None,
            rx_shift: None,
            rx_deadline_cycle: None,
            fifo_timeout_cycle: None,
            int_pending_timeout: false,
            external_tx: false,
            rx_fifo_reset: false,
        };
        port.recompute_char_cycles();
        port
    }

    /// Get the base I/O port for this serial port
    pub fn base_port(&self) -> u16 {
        self.base_port
    }

    /// Get the IRQ for this serial port
    pub fn irq(&self) -> u8 {
        self.irq
    }

    /// Read the Modem Control Register (MCR) value.
    ///
    /// Used by external layers (e.g. serial_net) to inspect guest RTS/DTR
    /// state for hardware flow control decisions.
    pub fn mcr(&self) -> u8 {
        self.mcr
    }

    /// Whether the guest has opened this port.
    ///
    /// MCR resets to 0 and is only written by the guest (writes to offset 4).
    /// A non-zero value means the guest has asserted DTR/RTS/OUT1/OUT2/LOOP —
    /// i.e. it's currently using the port. Used by the HUD to avoid reporting
    /// auto-linked TCP listeners as "connected" when the running application
    /// never actually opened them.
    pub fn is_opened_by_guest(&self) -> bool {
        self.mcr != 0
    }

    /// Read from a register
    pub fn read(&mut self, port: u16) -> u8 {
        let offset = port - self.base_port;
        match offset {
            0 => {
                // DLAB=0: RBR (Receive Buffer Register)
                // DLAB=1: DLL (Divisor Latch Low)
                if self.lcr & lcr::DLAB != 0 {
                    self.dll
                } else {
                    self.read_rbr()
                }
            }
            1 => {
                // DLAB=0: IER (Interrupt Enable Register)
                // DLAB=1: DLH (Divisor Latch High)
                if self.lcr & lcr::DLAB != 0 {
                    self.dlh
                } else {
                    self.ier & 0x0F // Only lower 4 bits are valid
                }
            }
            2 => self.read_iir(), // IIR (Interrupt Identification Register)
            3 => self.lcr,        // LCR (Line Control Register)
            4 => self.mcr,        // MCR (Modem Control Register)
            5 => self.read_lsr(), // LSR (Line Status Register)
            6 => self.read_msr(), // MSR (Modem Status Register)
            7 => self.scr,        // SCR (Scratch Register)
            _ => 0xFF,
        }
    }

    /// Write to a register
    pub fn write(&mut self, port: u16, value: u8) {
        let offset = port - self.base_port;
        match offset {
            0 => {
                // DLAB=0: THR (Transmit Holding Register)
                // DLAB=1: DLL (Divisor Latch Low)
                if self.lcr & lcr::DLAB != 0 {
                    self.dll = value;
                    self.recompute_char_cycles();
                } else {
                    self.write_thr(value);
                }
            }
            1 => {
                // DLAB=0: IER (Interrupt Enable Register)
                // DLAB=1: DLH (Divisor Latch High)
                if self.lcr & lcr::DLAB != 0 {
                    self.dlh = value;
                    self.recompute_char_cycles();
                } else {
                    let old_ier = self.ier;
                    self.ier = value & 0x0F; // Only lower 4 bits are valid

                    // If ETBEI is newly enabled and THR is empty, raise THRE interrupt
                    if (old_ier & ier::ETBEI) == 0
                        && (self.ier & ier::ETBEI) != 0
                        && (self.lsr & lsr::THRE) != 0
                    {
                        self.thre_int_pending = true;
                    }
                }
            }
            2 => self.write_fcr(value), // FCR (FIFO Control Register)
            3 => {
                self.lcr = value;
                self.recompute_char_cycles();
            }
            4 => {
                // MCR (Modem Control Register)
                self.mcr = value & 0x1F; // Only lower 5 bits are valid
                self.update_loopback();
            }
            5 => {}                // LSR is read-only (factory test)
            6 => {}                // MSR is read-only (reserved)
            7 => self.scr = value, // SCR (Scratch Register)
            _ => {}
        }
    }

    fn recompute_char_cycles(&mut self) {
        // Half-bit units throughout, halved at the end, so a 1.5-stop-bit
        // frame is not rounded up to two.
        let numerator = u128::from(self.cycles_per_second.max(1))
            .saturating_mul(u128::from(self.half_bits_per_character()))
            .saturating_mul(u128::from(self.divisor()));
        self.char_cycles = (numerator / (2 * u128::from(UART_BASE_BAUD))).max(1) as u64;
    }

    fn divisor(&self) -> u16 {
        let divisor = u16::from(self.dll) | (u16::from(self.dlh) << 8);
        divisor.max(1)
    }

    /// Frame length in HALF-bit units.
    ///
    /// An 8250-compatible UART sends **1.5** stop bits when LCR STB is set with
    /// a five-bit word, and two stop bits only for six- to eight-bit words.
    /// Counting whole bits rounded 5N1.5 up to 5N2, so character-completion and
    /// FIFO-timeout deadlines carried an extra half bit and paced that traffic
    /// too slowly (MDD-BUG-CRUCIBLE-00557).
    fn half_bits_per_character(&self) -> u64 {
        let data_bits = 5 + u64::from(self.lcr & 0x03);
        let parity_bits = if self.lcr & lcr::PEN != 0 { 1 } else { 0 };
        let half_stop_bits = match (self.lcr & lcr::STB != 0, data_bits) {
            (false, _) => 2, // 1 stop bit
            (true, 5) => 3,  // 1.5 stop bits, five-bit words only
            (true, _) => 4,  // 2 stop bits
        };
        2 * (1 + data_bits + parity_bits) + half_stop_bits
    }

    fn tx_holding_empty(&self) -> bool {
        if self.fifo_enabled {
            self.tx_fifo.is_empty()
        } else {
            !self.thr_loaded
        }
    }

    fn set_thre(&mut self, asserted: bool) {
        let was_asserted = self.lsr & lsr::THRE != 0;
        if asserted {
            self.lsr |= lsr::THRE;
            if !was_asserted {
                self.thre_int_pending = true;
            }
        } else {
            self.lsr &= !lsr::THRE;
        }
    }

    fn refresh_thre_state(&mut self) {
        self.set_thre(self.tx_holding_empty());
    }

    fn set_temt(&mut self, asserted: bool) {
        if asserted {
            self.lsr |= lsr::TEMT;
        } else {
            self.lsr &= !lsr::TEMT;
        }
    }

    fn refresh_temt_state(&mut self) {
        self.set_temt(self.tx_holding_empty() && self.tx_shift.is_none());
    }

    fn refresh_guest_rx_state(&mut self, reset_timeout: bool) {
        if self.fifo_enabled {
            if self.rx_fifo.is_empty() {
                self.lsr &= !lsr::DR;
                self.int_pending_rda = false;
            } else {
                self.lsr |= lsr::DR;
                self.int_pending_rda = self.rx_fifo.len() >= self.rx_trigger;
            }
        } else {
            self.int_pending_rda = self.lsr & lsr::DR != 0;
        }

        if reset_timeout {
            self.int_pending_timeout = false;
        }

        if self.fifo_enabled && !self.rx_fifo.is_empty() && self.rx_fifo.len() < self.rx_trigger {
            self.fifo_timeout_cycle = Some(self.current_cycle + self.char_cycles.saturating_mul(4));
        } else {
            self.fifo_timeout_cycle = None;
            if self.rx_fifo.is_empty() || self.rx_fifo.len() >= self.rx_trigger {
                self.int_pending_timeout = false;
            }
        }
    }

    fn pop_tx_holding(&mut self) -> Option<u8> {
        if self.fifo_enabled {
            self.tx_fifo.pop_front()
        } else if self.thr_loaded {
            self.thr_loaded = false;
            Some(self.thr)
        } else {
            None
        }
    }

    fn guest_rx_capacity(&self) -> usize {
        if self.fifo_enabled {
            FIFO_SIZE - self.rx_fifo.len()
        } else if self.lsr & lsr::DR == 0 {
            1
        } else {
            0
        }
    }

    fn clear_line_rx_staging(&mut self) {
        self.pending_line_rx.clear();
        self.rx_shift = None;
        self.rx_deadline_cycle = None;
    }

    fn start_tx_shift_if_idle(&mut self) {
        if self.tx_shift.is_some() {
            return;
        }

        if let Some(byte) = self.pop_tx_holding() {
            self.tx_shift = Some(byte);
            self.tx_deadline_cycle = Some(self.current_cycle + self.char_cycles);
            self.refresh_thre_state();
            self.set_temt(false);
            if self.mcr & mcr::LOOP != 0 {
                let _ = self.enqueue_line_rx(byte);
            }
        } else {
            self.tx_deadline_cycle = None;
            self.refresh_thre_state();
            self.refresh_temt_state();
        }
    }

    fn complete_tx_shift(&mut self) {
        if let Some(byte) = self.tx_shift.take() {
            if self.mcr & mcr::LOOP == 0 && !self.completed_tx.is_full() {
                self.completed_tx.push_back(byte);
            }
        }
        self.tx_deadline_cycle = None;
        self.start_tx_shift_if_idle();
    }

    fn start_rx_shift_if_idle(&mut self) {
        if self.rx_shift.is_some() {
            return;
        }

        if let Some(byte) = self.pending_line_rx.pop_front() {
            self.rx_shift = Some(byte);
            self.rx_deadline_cycle = Some(self.current_cycle + self.char_cycles);
        } else {
            self.rx_deadline_cycle = None;
        }
    }

    fn complete_rx_shift(&mut self) {
        if let Some(byte) = self.rx_shift.take() {
            self.deliver_received_byte(byte);
        }
        self.rx_deadline_cycle = None;
        self.start_rx_shift_if_idle();
    }

    fn deliver_received_byte(&mut self, data: u8) {
        self.int_pending_timeout = false;

        if self.fifo_enabled {
            if self.rx_fifo.len() < FIFO_SIZE {
                self.rx_fifo.push_back(data);
                self.refresh_guest_rx_state(true);
            } else {
                self.lsr |= lsr::OE;
                self.int_pending_rls = true;
            }
        } else if self.lsr & lsr::DR != 0 {
            self.lsr |= lsr::OE;
            self.int_pending_rls = true;
        } else {
            self.rbr = data;
            self.lsr |= lsr::DR;
            self.int_pending_rda = true;
        }
    }

    fn process_deadlines_up_to(&mut self, target_cycle: u64) {
        while let Some(deadline) = self.next_deadline() {
            if deadline > target_cycle {
                break;
            }

            self.current_cycle = deadline;

            if self.tx_deadline_cycle == Some(deadline) {
                self.complete_tx_shift();
            }
            if self.rx_deadline_cycle == Some(deadline) {
                self.complete_rx_shift();
            }
            if self.fifo_timeout_cycle == Some(deadline) {
                self.fifo_timeout_cycle = None;
                self.int_pending_timeout = true;
            }
        }

        self.current_cycle = target_cycle;
    }

    pub fn next_deadline(&self) -> Option<u64> {
        [
            self.tx_deadline_cycle,
            self.rx_deadline_cycle,
            self.fifo_timeout_cycle,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// The current UART clock cycle (monotonically increasing).
    pub fn current_cycle(&self) -> u64 {
        self.current_cycle
    }

    /// Duration of one serial character in guest CPU cycles at the current
    /// baud rate.  Returns 0 only if the divisor latch is zero (invalid).
    pub fn char_cycles(&self) -> u64 {
        self.char_cycles
    }

    pub fn advance_to_cycle(&mut self, target_cycle: u64) {
        self.process_deadlines_up_to(target_cycle.max(self.current_cycle));
    }

    pub fn advance_cycles(&mut self, cycles: u64) {
        self.advance_to_cycle(self.current_cycle.saturating_add(cycles));
    }

    pub fn set_cycles_per_second(&mut self, cycles_per_second: u64) {
        self.cycles_per_second = cycles_per_second.max(1);
        self.recompute_char_cycles();
    }

    /// The current emulated CPU cycles-per-second used for baud rate timing.
    pub fn cycles_per_second(&self) -> u64 {
        self.cycles_per_second
    }

    /// Retime in-flight deadlines when CPU speed changes.
    ///
    /// Each active deadline (TX, RX, FIFO timeout) has a remaining duration in
    /// old-speed cycles.  We scale that remaining time by `new_cps / old_cps`
    /// so the same guest-time elapses at the new speed.  Computation uses u128
    /// intermediates to avoid overflow.
    pub fn retime_for_speed_change(&mut self, new_cycles_per_second: u64) {
        let old_cps = self.cycles_per_second.max(1);
        let new_cps = new_cycles_per_second.max(1);
        let now = self.current_cycle;

        // Retime each in-flight deadline
        for d in [
            &mut self.tx_deadline_cycle,
            &mut self.rx_deadline_cycle,
            &mut self.fifo_timeout_cycle,
        ]
        .into_iter()
        .flatten()
        {
            let remaining = (*d).saturating_sub(now);
            let scaled = (u128::from(remaining) * u128::from(new_cps) / u128::from(old_cps)) as u64;
            *d = now.saturating_add(scaled);
        }

        // Update speed and recompute char_cycles
        self.cycles_per_second = new_cps;
        self.recompute_char_cycles();
    }

    /// Read the Receive Buffer Register
    fn read_rbr(&mut self) -> u8 {
        self.int_pending_timeout = false;

        if self.fifo_enabled {
            if let Some(data) = self.rx_fifo.pop_front() {
                self.refresh_guest_rx_state(true);
                data
            } else {
                self.refresh_guest_rx_state(false);
                0
            }
        } else {
            self.lsr &= !lsr::DR;
            self.int_pending_rda = false;
            self.fifo_timeout_cycle = None;
            self.rbr
        }
    }

    /// Write to the Transmit Holding Register
    fn write_thr(&mut self, value: u8) {
        self.thre_int_pending = false;

        if self.fifo_enabled {
            if self.tx_fifo.len() < FIFO_SIZE {
                self.tx_fifo.push_back(value);
            }
        } else {
            self.thr = value;
            self.thr_loaded = true;
        }
        self.set_temt(false);
        self.refresh_thre_state();
        self.start_tx_shift_if_idle();
    }

    /// Queue a byte for transmission exactly as a guest THR write would, but
    /// refuse rather than silently destroy a byte the UART has not sent yet.
    ///
    /// Returns `false`, leaving the UART completely untouched, when there is no
    /// room. "No room" means two different things:
    ///
    /// * **FIFO mode** — the transmit FIFO already holds `FIFO_SIZE` bytes, and
    ///   `write_thr` drops anything written past that.
    /// * **Non-FIFO mode** — the transmit holding register still holds a byte
    ///   that has not moved into the shift register, and `write_thr` would
    ///   overwrite it. `FIFO_SIZE - tx_fifo.len()` is meaningless here, which is
    ///   why this is an operation and not a headroom query.
    /// * **The far end is behind** — `complete_tx_shift` silently drops a
    ///   finished byte when `completed_tx` is full, so a byte accepted here on
    ///   THR room alone could still be destroyed a character time later, after
    ///   the caller had been told it was safely handed over. The whole point of
    ///   this operation is that acceptance means the byte survives, so the
    ///   downstream queue is part of the room calculation.
    ///
    /// Baud pacing is unchanged: an accepted byte goes through `write_thr` like
    /// any guest write, so it only appears at `take_completed_tx_byte` after a
    /// character time has elapsed.
    pub fn try_write_thr(&mut self, byte: u8) -> bool {
        let has_room = if self.fifo_enabled {
            self.tx_fifo.len() < FIFO_SIZE
        } else {
            !self.thr_loaded
        };

        if !has_room {
            return false;
        }

        if !self.completed_tx_has_room_for_one_more() {
            return false;
        }

        self.write_thr(byte);
        true
    }

    /// Whether one more byte queued for transmission is guaranteed a slot in
    /// `completed_tx` when it finishes shifting out.
    ///
    /// Everything already in flight — the byte in the shift register, the ones
    /// waiting in the transmit FIFO or the holding register — lands in
    /// `completed_tx` before the new byte does, so all of them count against
    /// its capacity. Under MCR loopback nothing is pushed at all
    /// (`complete_tx_shift`), so the queue can never be the constraint.
    fn completed_tx_has_room_for_one_more(&self) -> bool {
        if self.mcr & mcr::LOOP != 0 {
            return true;
        }
        let already_bound_for_completed_tx = self.completed_tx.len()
            + self.tx_fifo.len()
            + usize::from(self.tx_shift.is_some())
            + usize::from(self.thr_loaded);
        already_bound_for_completed_tx < COMPLETED_TX_SIZE
    }

    /// Read the Interrupt Identification Register
    fn read_iir(&mut self) -> u8 {
        // Compute IIR based on pending interrupts (priority order)
        let iir = if self.int_pending_rls && (self.ier & ier::ELSI) != 0 {
            iir::RLS // Highest priority: Receiver Line Status
        } else if self.int_pending_rda && (self.ier & ier::ERBFI) != 0 {
            iir::RDA // Received Data Available
        } else if self.int_pending_timeout && (self.ier & ier::ERBFI) != 0 {
            iir::TIMEOUT // Character Timeout (same enable as RDA)
        } else if self.thre_int_pending && (self.ier & ier::ETBEI) != 0 {
            self.thre_int_pending = false; // Reading IIR clears THRE interrupt
            iir::THRE // Transmitter Holding Register Empty
        } else if self.int_pending_msr && (self.ier & ier::EDSSI) != 0 {
            iir::MODEM_STATUS // Lowest priority: Modem Status
        } else {
            iir::NO_INT // No interrupt pending
        };

        // Add FIFO status bits if FIFOs are enabled
        if self.fifo_enabled {
            iir | iir::FIFO_ENABLED
        } else {
            iir
        }
    }

    /// Write to the FIFO Control Register
    fn write_fcr(&mut self, value: u8) {
        let fifo_mode_changed = (value & fcr::FE != 0) != self.fifo_enabled;
        self.fcr = value;

        if value & fcr::FE != 0 {
            self.fifo_enabled = true;

            if value & fcr::RFR != 0 {
                self.rx_fifo.clear();
                self.clear_line_rx_staging();
                self.rx_fifo_reset = true;
                self.int_pending_timeout = false;
            }
            if value & fcr::TFR != 0 {
                self.tx_fifo.clear();
            }

            self.rx_trigger = match (value & fcr::RTL_MASK) >> 6 {
                0 => 1,
                1 => 4,
                2 => 8,
                3 => 14,
                _ => 1,
            };
        } else {
            self.fifo_enabled = false;
            self.rx_fifo.clear();
            self.clear_line_rx_staging();
            self.tx_fifo.clear();
            self.thr_loaded = false;
            self.rx_trigger = 1;
            self.rx_fifo_reset = true;
            self.int_pending_timeout = false;
        }

        if fifo_mode_changed {
            // FIFO mode changes discard receive data; clear DR so the
            // non-FIFO refresh cannot revalidate the stale RBR latch.
            self.lsr &= !lsr::DR;
            self.int_pending_rda = false;
            self.int_pending_timeout = false;
        }

        self.refresh_guest_rx_state(false);
        self.refresh_thre_state();
        self.refresh_temt_state();
    }

    /// Read the Line Status Register
    fn read_lsr(&mut self) -> u8 {
        let lsr = self.lsr;
        // Reading LSR clears error bits (OE, PE, FE, BI) and FIFO error
        self.lsr &= !(lsr::OE | lsr::PE | lsr::FE | lsr::BI | lsr::FIFO_ERR);
        self.int_pending_rls = false;
        lsr
    }

    /// Read the Modem Status Register
    fn read_msr(&mut self) -> u8 {
        let msr = self.msr;
        // Reading MSR clears delta bits
        self.msr &= !(msr::DCTS | msr::DDSR | msr::TERI | msr::DDCD);
        self.int_pending_msr = false;
        msr
    }

    /// Update modem status signals in response to MCR writes.
    ///
    /// Three modes:
    /// 1. **Loopback** (MCR.LOOP set): MCR outputs feed directly into MSR inputs
    ///    (DTR->DSR, RTS->CTS, OUT1->RI, OUT2->DCD). Used by software for UART
    ///    self-test.
    /// 2. **External device** (`external_tx` set): An external modem or serial
    ///    networking layer manages MSR via `update_msr_signals()`. We don't
    ///    touch MSR here.
    /// 3. **Null modem** (default): Simulates a null modem cable where RTS->CTS,
    ///    DTR->DSR, and DTR->DCD. This provides programs with
    ///    proper hardware flow control signals without requiring an actual
    ///    modem or TCP connection.
    fn update_loopback(&mut self) {
        if self.mcr & mcr::LOOP != 0 {
            // In loopback mode, MCR outputs are connected to MSR inputs
            let mut new_msr = 0u8;

            // DTR -> DSR
            if self.mcr & mcr::DTR != 0 {
                new_msr |= msr::DSR;
            }
            // RTS -> CTS
            if self.mcr & mcr::RTS != 0 {
                new_msr |= msr::CTS;
            }
            // OUT1 -> RI
            if self.mcr & mcr::OUT1 != 0 {
                new_msr |= msr::RI;
            }
            // OUT2 -> DCD
            if self.mcr & mcr::OUT2 != 0 {
                new_msr |= msr::DCD;
            }

            // Calculate delta bits
            if (new_msr & msr::CTS) != (self.msr & msr::CTS) {
                new_msr |= msr::DCTS;
                self.int_pending_msr = true;
            }
            if (new_msr & msr::DSR) != (self.msr & msr::DSR) {
                new_msr |= msr::DDSR;
                self.int_pending_msr = true;
            }
            if (new_msr & msr::RI) != (self.msr & msr::RI) && (self.msr & msr::RI) != 0 {
                // TERI is set on trailing edge (RI going low)
                new_msr |= msr::TERI;
                self.int_pending_msr = true;
            }
            if (new_msr & msr::DCD) != (self.msr & msr::DCD) {
                new_msr |= msr::DDCD;
                self.int_pending_msr = true;
            }

            new_msr |= self.msr & (msr::DCTS | msr::DDSR | msr::TERI | msr::DDCD);
            self.msr = new_msr;
        } else if !self.external_tx {
            // Null modem simulation: when no external device is managing
            // signals, reflect MCR outputs as if a null modem cable is
            // connected. In a null modem cable:
            //   RTS (local) -> CTS (local, via crossover)
            //   DTR (local) -> DSR (local, via crossover)
            //   DTR (local) -> DCD (local, via crossover)
            let mut new_msr = 0u8;

            if self.mcr & mcr::DTR != 0 {
                new_msr |= msr::DSR;
                new_msr |= msr::DCD;
            }
            if self.mcr & mcr::RTS != 0 {
                new_msr |= msr::CTS;
            }

            // Calculate delta bits
            if (new_msr & msr::CTS) != (self.msr & msr::CTS) {
                new_msr |= msr::DCTS;
                self.int_pending_msr = true;
            }
            if (new_msr & msr::DSR) != (self.msr & msr::DSR) {
                new_msr |= msr::DDSR;
                self.int_pending_msr = true;
            }
            if (new_msr & msr::DCD) != (self.msr & msr::DCD) {
                new_msr |= msr::DDCD;
                self.int_pending_msr = true;
            }

            new_msr |= self.msr & (msr::DCTS | msr::DDSR | msr::TERI | msr::DDCD);
            self.msr = new_msr;
        }
        // When external_tx is true, MSR is managed by the external device
        // (modem or serial networking) via update_msr_signals(). Don't
        // override it here.
    }

    /// Check if any interrupt is pending and enabled
    pub fn interrupt_pending(&self) -> bool {
        if self.mcr & mcr::OUT2 == 0 {
            return false; // OUT2 must be set to enable interrupts
        }

        (self.int_pending_rls && (self.ier & ier::ELSI) != 0)
            || (self.int_pending_rda && (self.ier & ier::ERBFI) != 0)
            || (self.int_pending_timeout && (self.ier & ier::ERBFI) != 0)
            || (self.thre_int_pending && (self.ier & ier::ETBEI) != 0)
            || (self.int_pending_msr && (self.ier & ier::EDSSI) != 0)
    }

    /// Compatibility shim for the old FIFO-timeout contract.
    pub fn tick_char_timeout(&mut self) {
        self.current_cycle = self.current_cycle.saturating_add(self.char_cycles);
        if self
            .fifo_timeout_cycle
            .is_some_and(|deadline| deadline <= self.current_cycle)
        {
            self.fifo_timeout_cycle = None;
            self.int_pending_timeout = true;
        }
    }

    /// Queue a byte that has arrived on the serial line.
    pub fn enqueue_line_rx(&mut self, data: u8) -> bool {
        if self.pending_line_rx.is_full() {
            return false;
        }

        self.pending_line_rx.push_back(data);
        self.start_rx_shift_if_idle();
        true
    }

    /// Compatibility wrapper for legacy transport callers.
    /// Preserves the old guest-visible receive semantics; Stage 1A line timing
    /// uses `enqueue_line_rx()` instead.
    pub fn receive_byte(&mut self, data: u8) {
        self.deliver_received_byte(data);
    }

    pub fn line_rx_capacity(&self) -> usize {
        LINE_QUEUE_SIZE - self.pending_line_rx.len()
    }

    /// Compatibility wrapper for legacy transport callers.
    pub fn can_receive(&self) -> bool {
        self.guest_rx_capacity() != 0
    }

    /// Compatibility wrapper for legacy transport callers.
    pub fn rx_available(&self) -> usize {
        self.guest_rx_capacity()
    }

    /// How many received bytes the guest could read right now.
    ///
    /// **Non-consuming.** Unlike `read_rbr`, which pops the FIFO and clears the
    /// data-ready bit, this only observes: a status poll must not steal a byte
    /// from a later direct RBR read. It is the mirror image of
    /// `guest_rx_capacity`, which reports the free space on the same receiver.
    pub fn rx_data_len(&self) -> usize {
        if self.fifo_enabled {
            self.rx_fifo.len()
        } else if self.lsr & lsr::DR != 0 {
            1
        } else {
            0
        }
    }

    /// Whether the guest-visible receiver holds at least one byte.
    ///
    /// **Non-consuming**, for the same reason as `rx_data_len`.
    pub fn rx_data_ready(&self) -> bool {
        self.rx_data_len() != 0
    }

    pub fn take_completed_tx_byte(&mut self) -> Option<u8> {
        self.completed_tx.pop_front()
    }

    /// Compatibility wrapper for legacy transport callers.
    pub fn get_transmitted_byte(&mut self) -> Option<u8> {
        if self.mcr & mcr::LOOP != 0 {
            return None;
        }

        if let Some(byte) = self.take_completed_tx_byte() {
            return Some(byte);
        }

        if let Some(byte) = self.tx_shift.take() {
            self.tx_deadline_cycle = None;
            self.start_tx_shift_if_idle();
            return Some(byte);
        }

        let byte = self.pop_tx_holding();
        if byte.is_some() {
            self.refresh_thre_state();
            self.refresh_temt_state();
        }
        byte
    }

    /// Update modem status signals from an external source (e.g., serial networking).
    /// Delta-preserving: unread delta bits are preserved across calls.
    /// TERI fires only on trailing edge (RI high->low).
    pub fn update_msr_signals(&mut self, dsr: bool, cts: bool, dcd: bool, ri: bool) {
        let mut new_msr = 0u8;
        if cts {
            new_msr |= msr::CTS;
        }
        if dsr {
            new_msr |= msr::DSR;
        }
        if ri {
            new_msr |= msr::RI;
        }
        if dcd {
            new_msr |= msr::DCD;
        }
        let old = self.msr;
        // Preserve unread delta bits
        new_msr |= old & (msr::DCTS | msr::DDSR | msr::TERI | msr::DDCD);
        // New deltas from signal transitions
        if (old ^ new_msr) & msr::CTS != 0 {
            new_msr |= msr::DCTS;
        }
        if (old ^ new_msr) & msr::DSR != 0 {
            new_msr |= msr::DDSR;
        }
        if (old ^ new_msr) & msr::DCD != 0 {
            new_msr |= msr::DDCD;
        }
        // TERI fires on trailing edge only (RI was high, now low)
        if old & msr::RI != 0 && new_msr & msr::RI == 0 {
            new_msr |= msr::TERI;
        }
        if new_msr != self.msr {
            self.msr = new_msr;
            self.int_pending_msr = true;
        }
    }

    // ------------------------------------------------------------------
    // Checkpoint save / restore
    // ------------------------------------------------------------------

    /// Capture all mutable state for checkpointing.
    ///
    /// Static config (`port_num`, `base_port`, `irq`) is omitted — it is
    /// fixed at construction and reconstructed from the port index.
    /// FixedRing queues are returned as `Vec<u8>`.
    #[allow(clippy::type_complexity)]
    pub fn save_state(
        &self,
    ) -> (
        /* registers */ (u8, u8, bool, u8, u8, u8, u8, u8, u8, u8, u8, u8),
        /* fifos */ (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>),
        /* fifo control */ (bool, usize),
        /* int flags */ (bool, bool, bool, bool, bool),
        /* misc flags */ (bool, bool),
        /* timing */
        (
            u64,
            u64,
            u64,
            Option<u8>,
            Option<u64>,
            Option<u8>,
            Option<u64>,
            Option<u64>,
        ),
    ) {
        let regs = (
            self.rbr,
            self.thr,
            self.thr_loaded,
            self.ier,
            self.fcr,
            self.lcr,
            self.mcr,
            self.lsr,
            self.msr,
            self.scr,
            self.dll,
            self.dlh,
        );
        let fifos = (
            self.rx_fifo.iter().copied().collect(),
            self.tx_fifo.iter().copied().collect(),
            self.completed_tx.iter().copied().collect(),
            self.pending_line_rx.iter().copied().collect(),
        );
        let fifo_ctl = (self.fifo_enabled, self.rx_trigger);
        let int_flags = (
            self.int_pending_rls,
            self.int_pending_rda,
            self.int_pending_msr,
            self.thre_int_pending,
            self.int_pending_timeout,
        );
        let misc = (self.external_tx, self.rx_fifo_reset);
        let timing = (
            self.current_cycle,
            self.cycles_per_second,
            self.char_cycles,
            self.tx_shift,
            self.tx_deadline_cycle,
            self.rx_shift,
            self.rx_deadline_cycle,
            self.fifo_timeout_cycle,
        );
        (regs, fifos, fifo_ctl, int_flags, misc, timing)
    }

    /// Restore all mutable state from a checkpoint.
    ///
    /// The caller passes the same tuple layout returned by `save_state`.
    /// `char_cycles` is restored directly (not recomputed) so the
    /// checkpoint is bit-exact.
    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    pub fn restore_state(
        &mut self,
        regs: (u8, u8, bool, u8, u8, u8, u8, u8, u8, u8, u8, u8),
        fifos: (&[u8], &[u8], &[u8], &[u8]),
        fifo_ctl: (bool, usize),
        int_flags: (bool, bool, bool, bool, bool),
        misc: (bool, bool),
        timing: (
            u64,
            u64,
            u64,
            Option<u8>,
            Option<u64>,
            Option<u8>,
            Option<u64>,
            Option<u64>,
        ),
    ) {
        // Registers
        self.rbr = regs.0;
        self.thr = regs.1;
        self.thr_loaded = regs.2;
        self.ier = regs.3;
        self.fcr = regs.4;
        self.lcr = regs.5;
        self.mcr = regs.6;
        self.lsr = regs.7;
        self.msr = regs.8;
        self.scr = regs.9;
        self.dll = regs.10;
        self.dlh = regs.11;

        // FIFOs — clear and repopulate
        debug_assert!(fifos.0.len() <= FIFO_SIZE, "rx_fifo snapshot too large");
        debug_assert!(fifos.1.len() <= FIFO_SIZE, "tx_fifo snapshot too large");
        debug_assert!(
            fifos.2.len() <= COMPLETED_TX_SIZE,
            "completed_tx snapshot too large"
        );
        debug_assert!(
            fifos.3.len() <= LINE_QUEUE_SIZE,
            "pending_line_rx snapshot too large"
        );
        self.rx_fifo.clear();
        for &b in fifos.0 {
            self.rx_fifo.push_back(b);
        }
        self.tx_fifo.clear();
        for &b in fifos.1 {
            self.tx_fifo.push_back(b);
        }
        self.completed_tx.clear();
        for &b in fifos.2 {
            self.completed_tx.push_back(b);
        }
        self.pending_line_rx.clear();
        for &b in fifos.3 {
            self.pending_line_rx.push_back(b);
        }

        // FIFO control
        self.fifo_enabled = fifo_ctl.0;
        self.rx_trigger = fifo_ctl.1;

        // Interrupt flags
        self.int_pending_rls = int_flags.0;
        self.int_pending_rda = int_flags.1;
        self.int_pending_msr = int_flags.2;
        self.thre_int_pending = int_flags.3;
        self.int_pending_timeout = int_flags.4;

        // Misc flags
        self.external_tx = misc.0;
        self.rx_fifo_reset = misc.1;

        // Timing — restore directly (char_cycles is NOT recomputed)
        self.current_cycle = timing.0;
        self.cycles_per_second = timing.1;
        self.char_cycles = timing.2;
        self.tx_shift = timing.3;
        self.tx_deadline_cycle = timing.4;
        self.rx_shift = timing.5;
        self.rx_deadline_cycle = timing.6;
        self.fifo_timeout_cycle = timing.7;
    }

    /// Reset the serial port to initial state
    pub fn reset(&mut self) {
        self.rbr = 0;
        self.thr = 0;
        self.thr_loaded = false;
        self.ier = 0;
        self.fcr = 0;
        self.lcr = 0;
        self.mcr = 0;
        self.lsr = lsr::THRE | lsr::TEMT;
        self.msr = 0;
        self.scr = 0;
        self.dll = 0x01;
        self.dlh = 0x00;
        self.rx_fifo.clear();
        self.tx_fifo.clear();
        self.completed_tx.clear();
        self.pending_line_rx.clear();
        self.fifo_enabled = false;
        self.rx_trigger = 1;
        self.int_pending_rls = false;
        self.int_pending_rda = false;
        self.int_pending_msr = false;
        self.thre_int_pending = true;
        self.current_cycle = 0;
        self.cycles_per_second = DEFAULT_UART_CYCLES_PER_SECOND;
        self.char_cycles = 0;
        self.tx_shift = None;
        self.tx_deadline_cycle = None;
        self.rx_shift = None;
        self.rx_deadline_cycle = None;
        self.fifo_timeout_cycle = None;
        self.int_pending_timeout = false;
        self.external_tx = false;
        self.rx_fifo_reset = false;
        self.recompute_char_cycles();
    }
}

/// Serial port controller managing all COM ports
#[derive(Clone)]
pub struct SerialPorts {
    /// COM1 through COM4
    pub ports: [SerialPort; 4],
    /// Whether each COM port is present on the ISA I/O bus.
    present: [bool; 4],
    /// Flag set by any guest serial I/O read or write, signalling that the
    /// emulator should call `reschedule_uart_deadline_event()` and
    /// `refresh_serial_pic_irqs()` after the current quantum.
    pub event_reschedule_pending: bool,
    /// The value each register last read back, so the read trace shows
    /// changes rather than every poll of a status register.
    last_read: [[Option<u8>; 8]; 4],
}

/// Log target for guest COM register access: writes at debug, changed
/// reads at trace.
const IO_TARGET: &str = "gp64::hw::serial::io";

impl Default for SerialPorts {
    fn default() -> Self {
        Self::new()
    }
}

impl SerialPorts {
    /// Create a new serial port controller with all COM ports
    pub fn new() -> Self {
        Self::new_with_present_mask([true; 4])
    }

    /// Create a serial port controller with an explicit COM1-COM4 presence mask.
    pub fn new_with_present_mask(present: [bool; 4]) -> Self {
        Self {
            ports: [
                SerialPort::new(1),
                SerialPort::new(2),
                SerialPort::new(3),
                SerialPort::new(4),
            ],
            present,
            event_reschedule_pending: false,
            last_read: [[None; 8]; 4],
        }
    }

    /// The COM index (0 = COM1) of a UART register address.
    fn index_of(port: u16) -> Option<usize> {
        match port {
            0x3F8..=0x3FF => Some(0),
            0x2F8..=0x2FF => Some(1),
            0x3E8..=0x3EF => Some(2),
            0x2E8..=0x2EF => Some(3),
            _ => None,
        }
    }

    /// Mark a COM port as present or absent on the I/O bus.
    pub fn set_present(&mut self, port_index: usize, present: bool) {
        if let Some(slot) = self.present.get_mut(port_index) {
            *slot = present;
        }
    }

    /// Return whether a COM port is present on the I/O bus.
    pub fn is_present(&self, port_index: usize) -> bool {
        self.present.get(port_index).copied().unwrap_or(false)
    }

    /// Get COM port by number (1-4)
    pub fn get_port(&mut self, port_num: u8) -> Option<&mut SerialPort> {
        match port_num {
            1..=4 => Some(&mut self.ports[(port_num - 1) as usize]),
            _ => None,
        }
    }

    /// Read from a serial port I/O address
    pub fn read(&mut self, port: u16) -> Option<u8> {
        // Determine which COM port this address belongs to
        let result = match port {
            0x3F8..=0x3FF if self.present[0] => Some(self.ports[0].read(port)), // COM1
            0x2F8..=0x2FF if self.present[1] => Some(self.ports[1].read(port)), // COM2
            0x3E8..=0x3EF if self.present[2] => Some(self.ports[2].read(port)), // COM3
            0x2E8..=0x2EF if self.present[3] => Some(self.ports[3].read(port)), // COM4
            _ => None,
        };
        if let Some(value) = result {
            self.event_reschedule_pending = true;
            if tracing::enabled!(target: IO_TARGET, tracing::Level::TRACE) {
                if let Some(index) = Self::index_of(port) {
                    let last = &mut self.last_read[index][usize::from(port & 7)];
                    if *last != Some(value) {
                        *last = Some(value);
                        tracing::trace!(target: IO_TARGET, "COM{} read {port:03X} = {value:02X}", index + 1);
                    }
                }
            }
        }
        result
    }

    /// Write to a serial port I/O address
    pub fn write(&mut self, port: u16, value: u8) -> bool {
        // Determine which COM port this address belongs to
        let handled = match port {
            0x3F8..=0x3FF if self.present[0] => {
                self.ports[0].write(port, value);
                true
            }
            0x2F8..=0x2FF if self.present[1] => {
                self.ports[1].write(port, value);
                true
            }
            0x3E8..=0x3EF if self.present[2] => {
                self.ports[2].write(port, value);
                true
            }
            0x2E8..=0x2EF if self.present[3] => {
                self.ports[3].write(port, value);
                true
            }
            _ => false,
        };
        if handled {
            self.event_reschedule_pending = true;
            if let Some(index) = Self::index_of(port) {
                tracing::debug!(target: IO_TARGET, "COM{} write {port:03X} = {value:02X}", index + 1);
            }
        }
        handled
    }

    /// Check if any serial port has a pending interrupt
    /// Returns (has_irq3, has_irq4)
    pub fn check_interrupts(&self) -> (bool, bool) {
        let irq3 = (self.present[1] && self.ports[1].interrupt_pending())
            || (self.present[3] && self.ports[3].interrupt_pending());
        let irq4 = (self.present[0] && self.ports[0].interrupt_pending())
            || (self.present[2] && self.ports[2].interrupt_pending());
        (irq3, irq4)
    }

    pub fn advance_to_cycle(&mut self, target_cycle: u64) {
        for port in &mut self.ports {
            port.advance_to_cycle(target_cycle);
        }
    }

    /// Retime all serial ports for a CPU speed change.
    ///
    /// Call this after updating `cycles_per_ms` so in-flight UART deadlines
    /// preserve their remaining guest-time at the new speed.
    pub fn retime_for_speed_change(&mut self, new_cycles_per_second: u64) {
        for port in &mut self.ports {
            port.retime_for_speed_change(new_cycles_per_second);
        }
    }

    /// Reset all serial ports
    pub fn reset(&mut self) {
        for port in &mut self.ports {
            port.reset();
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn advance_char(port: &mut SerialPort) {
        port.advance_cycles(port.char_cycles);
    }

    fn advance_until_deadline(port: &mut SerialPort, deadline: u64) {
        let delta = deadline.saturating_sub(port.current_cycle);
        port.advance_cycles(delta);
    }

    #[test]
    fn test_advance_to_cycle_processes_due_deadline_without_readvancing_clock() {
        let mut port = SerialPort::new(1);
        port.write(0x3FA, fcr::FE);
        assert!(port.enqueue_line_rx(0x11));
        assert!(port.enqueue_line_rx(0x22));

        let first_deadline = port.next_deadline().unwrap();
        port.tick_char_timeout();
        port.advance_to_cycle(first_deadline);

        assert_eq!(port.read(0x3F8), 0x11);
        assert_eq!(port.read(0x3F8), 0x00);
        assert_eq!(port.next_deadline(), Some(first_deadline * 2));
    }

    #[test]
    fn test_advance_to_cycle_catches_up_to_current_uart_clock_without_overshoot() {
        let mut port = SerialPort::new(1);
        port.write(0x3FA, fcr::FE);
        assert!(port.enqueue_line_rx(0x11));
        assert!(port.enqueue_line_rx(0x22));
        assert!(port.enqueue_line_rx(0x33));

        let first_deadline = port.next_deadline().unwrap();
        port.tick_char_timeout();
        port.tick_char_timeout();
        port.advance_to_cycle(first_deadline);

        assert_eq!(port.read(0x3F8), 0x11);
        assert_eq!(port.read(0x3F8), 0x22);
        assert_eq!(port.read(0x3F8), 0x00);
        assert_eq!(port.next_deadline(), Some(first_deadline * 3));
    }

    #[test]
    fn test_serial_port_creation() {
        let com1 = SerialPort::new(1);
        assert_eq!(com1.base_port(), 0x3F8);
        assert_eq!(com1.irq(), 4);

        let com2 = SerialPort::new(2);
        assert_eq!(com2.base_port(), 0x2F8);
        assert_eq!(com2.irq(), 3);

        let com3 = SerialPort::new(3);
        assert_eq!(com3.base_port(), 0x3E8);
        assert_eq!(com3.irq(), 4);

        let com4 = SerialPort::new(4);
        assert_eq!(com4.base_port(), 0x2E8);
        assert_eq!(com4.irq(), 3);
    }

    #[test]
    fn test_lsr_initial_state() {
        let port = SerialPort::new(1);
        // Initially, transmitter should be empty
        assert_ne!(port.lsr & lsr::THRE, 0);
        assert_ne!(port.lsr & lsr::TEMT, 0);
        // And no data available
        assert_eq!(port.lsr & lsr::DR, 0);
    }

    #[test]
    fn test_divisor_latch_access() {
        let mut port = SerialPort::new(1);

        // Set DLAB to access divisor registers
        port.write(0x3FB, lcr::DLAB); // LCR with DLAB=1

        // Write divisor for 9600 baud (divisor = 12)
        port.write(0x3F8, 0x0C); // DLL
        port.write(0x3F9, 0x00); // DLH

        // Read back
        assert_eq!(port.read(0x3F8), 0x0C);
        assert_eq!(port.read(0x3F9), 0x00);

        // Clear DLAB
        port.write(0x3FB, 0);
    }

    #[test]
    fn test_scratch_register() {
        let mut port = SerialPort::new(1);

        // Write to scratch register
        port.write(0x3FF, 0xAA);
        assert_eq!(port.read(0x3FF), 0xAA);

        port.write(0x3FF, 0x55);
        assert_eq!(port.read(0x3FF), 0x55);
    }

    #[test]
    fn test_loopback_mode() {
        let mut port = SerialPort::new(1);

        port.write(0x3FC, mcr::LOOP | mcr::OUT2);
        port.write(0x3F8, 0x42);

        assert_eq!(
            port.lsr & lsr::DR,
            0,
            "loopback should not deliver instantly"
        );
        advance_char(&mut port);

        let lsr = port.read(0x3FD);
        assert_ne!(lsr & lsr::DR, 0, "Data Ready should be set in loopback");

        let data = port.read(0x3F8);
        assert_eq!(data, 0x42);
    }

    #[test]
    fn test_loopback_modem_signals() {
        let mut port = SerialPort::new(1);

        // Enable loopback mode
        port.write(0x3FC, mcr::LOOP);

        // Set DTR and RTS
        port.write(0x3FC, mcr::LOOP | mcr::DTR | mcr::RTS);

        // Read MSR - should reflect DTR->DSR and RTS->CTS
        let msr = port.read(0x3FE);
        assert_ne!(msr & msr::DSR, 0, "DSR should follow DTR");
        assert_ne!(msr & msr::CTS, 0, "CTS should follow RTS");
        assert_ne!(msr & msr::DDSR, 0, "DDSR delta should be set");
        assert_ne!(msr & msr::DCTS, 0, "DCTS delta should be set");
    }

    #[test]
    fn test_fifo_enable() {
        let mut port = SerialPort::new(1);

        // Initially, FIFO should be disabled
        assert!(!port.fifo_enabled);

        // Enable FIFO
        port.write(0x3FA, fcr::FE);
        assert!(port.fifo_enabled);

        // Check IIR shows FIFO enabled bits
        let iir = port.read(0x3FA);
        assert_ne!(iir & iir::FIFO_ENABLED, 0);
    }

    #[test]
    fn test_fifo_trigger_levels() {
        let mut port = SerialPort::new(1);

        // Enable FIFO with trigger level 1
        port.write(0x3FA, fcr::FE);
        assert_eq!(port.rx_trigger, 1);

        // Enable FIFO with trigger level 4
        port.write(0x3FA, fcr::FE | 0x40);
        assert_eq!(port.rx_trigger, 4);

        // Enable FIFO with trigger level 8
        port.write(0x3FA, fcr::FE | 0x80);
        assert_eq!(port.rx_trigger, 8);

        // Enable FIFO with trigger level 14
        port.write(0x3FA, fcr::FE | 0xC0);
        assert_eq!(port.rx_trigger, 14);
    }

    #[test]
    fn test_read_ier_and_dlh() {
        let mut port = SerialPort::new(1);

        port.write(0x3F9, ier::ERBFI);
        assert_eq!(port.read(0x3F9) & ier::ERBFI, ier::ERBFI);

        port.write(0x3FB, lcr::DLAB);
        port.write(0x3F9, 0x12);
        assert_eq!(port.read(0x3F9), 0x12);
        port.write(0x3FB, 0);
    }

    #[test]
    fn test_ier_etbei_enable_sets_thre() {
        let mut port = SerialPort::new(1);

        port.thre_int_pending = false;
        port.write(0x3F9, ier::ETBEI);
        assert!(port.thre_int_pending);

        port.thre_int_pending = false;
        port.write(0x3F9, ier::ETBEI);
        assert!(!port.thre_int_pending);

        port.lsr &= !lsr::THRE;
        port.thre_int_pending = false;
        port.ier = 0;
        port.write(0x3F9, ier::ETBEI);
        assert!(!port.thre_int_pending);
    }

    #[test]
    fn test_read_rbr_empty_fifo() {
        let mut port = SerialPort::new(1);

        port.write(0x3FA, fcr::FE);
        port.lsr |= lsr::DR;
        assert_eq!(port.read(0x3F8), 0);
        assert_eq!(port.lsr & lsr::DR, 0);
    }

    #[test]
    fn test_interrupt_identification() {
        let mut port = SerialPort::new(1);

        // Enable interrupts
        port.write(0x3FC, mcr::OUT2); // OUT2 enables interrupts
        port.write(0x3F9, ier::ERBFI | ier::ETBEI); // Enable RDA and THRE interrupts

        // THRE should be pending initially (THR is empty)
        let iir = port.read(0x3FA);
        assert_eq!(iir & 0x0F, iir::THRE);

        // Reading IIR should clear THRE interrupt
        let iir2 = port.read(0x3FA);
        assert_eq!(iir2 & 0x0F, iir::NO_INT);
    }

    #[test]
    fn test_iir_skips_disabled_interrupts() {
        let mut port = SerialPort::new(1);

        port.fifo_enabled = true;
        port.int_pending_rls = true;
        port.int_pending_rda = true;
        port.thre_int_pending = true;
        port.int_pending_msr = true;
        port.ier = 0;

        let iir = port.read(0x3FA);
        assert_eq!(iir & 0x0F, iir::NO_INT);
        assert_ne!(iir & iir::FIFO_ENABLED, 0);
    }

    #[test]
    fn test_receive_byte() {
        let mut port = SerialPort::new(1);

        port.receive_byte(0x55);
        assert_ne!(port.lsr & lsr::DR, 0);
        let data = port.read(0x3F8);
        assert_eq!(data, 0x55);
        assert_eq!(port.lsr & lsr::DR, 0);
    }

    #[test]
    fn test_overrun_error() {
        let mut port = SerialPort::new(1);

        port.receive_byte(0x11);
        port.receive_byte(0x22);

        let lsr = port.read(0x3FD);
        assert_ne!(lsr & lsr::OE, 0, "Overrun error should be set");
        assert_eq!(
            port.read(0x3F8),
            0x11,
            "original unread byte must be preserved"
        );
    }

    #[test]
    fn test_non_fifo_transmit_sets_thre_before_temt() {
        let mut port = SerialPort::new(1);

        port.write(0x3F8, 0x77);
        let lsr = port.read(0x3FD);
        assert_ne!(lsr & lsr::THRE, 0);
        assert_eq!(lsr & lsr::TEMT, 0);
        assert_eq!(port.take_completed_tx_byte(), None);

        advance_char(&mut port);
        assert_eq!(port.take_completed_tx_byte(), Some(0x77));
        let lsr = port.read(0x3FD);
        assert_ne!(lsr & lsr::THRE, 0);
        assert_ne!(lsr & lsr::TEMT, 0);
    }

    #[test]
    fn test_fifo_timeout_rearms_on_receive_activity() {
        let mut port = SerialPort::new(1);

        port.write(0x3FA, fcr::FE | 0xC0);
        port.receive_byte(0x41);
        let first_deadline = port
            .fifo_timeout_cycle
            .expect("timeout deadline should arm");

        advance_until_deadline(&mut port, first_deadline.saturating_sub(1));
        assert!(!port.int_pending_timeout);

        port.receive_byte(0x42);
        let rearmed_deadline = port
            .fifo_timeout_cycle
            .expect("timeout deadline should re-arm");
        assert!(rearmed_deadline > first_deadline);

        advance_until_deadline(&mut port, rearmed_deadline.saturating_sub(1));
        assert!(!port.int_pending_timeout);
    }

    #[test]
    fn test_tx_fifo_full_write_drops_new_byte() {
        let mut port = SerialPort::new(1);

        port.write(0x3FA, fcr::FE);
        port.tx_shift = Some(0xAA);
        port.tx_deadline_cycle = Some(port.current_cycle + port.char_cycles);
        for value in 0..FIFO_SIZE as u8 {
            port.tx_fifo.push_back(value);
        }
        port.refresh_thre_state();
        port.refresh_temt_state();

        port.write(0x3F8, 0x22);
        assert_eq!(port.tx_fifo.len(), FIFO_SIZE);
        assert_eq!(port.tx_fifo.back(), Some(&(FIFO_SIZE as u8 - 1)));
    }

    #[test]
    fn test_serial_ports_controller() {
        let mut ports = SerialPorts::new();

        // Write to COM1 scratch register
        assert!(ports.write(0x3FF, 0xAB));
        assert_eq!(ports.read(0x3FF), Some(0xAB));

        // Write to COM2 scratch register
        assert!(ports.write(0x2FF, 0xCD));
        assert_eq!(ports.read(0x2FF), Some(0xCD));

        // Invalid port should return None
        assert_eq!(ports.read(0x100), None);
        assert!(!ports.write(0x100, 0x00));
    }

    #[test]
    fn test_fcr_disable_clears_fifo() {
        let mut port = SerialPort::new(1);

        port.write(0x3FA, fcr::FE);
        port.rx_fifo.push_back(0x11);
        port.tx_fifo.push_back(0x22);

        port.write(0x3FA, 0x00);
        assert!(!port.fifo_enabled);
        assert!(port.rx_fifo.is_empty());
        assert!(port.tx_fifo.is_empty());
    }

    #[test]
    fn test_fcr_mode_changes_clear_rx_readiness_and_same_mode_writes_preserve_data() {
        let mut enabling_port = SerialPort::new(1);
        enabling_port.write(0x3F9, ier::ERBFI);
        enabling_port.write(0x3FC, mcr::LOOP);
        enabling_port.write(0x3F8, 0x41);
        advance_char(&mut enabling_port);
        assert_ne!(enabling_port.read(0x3FD) & lsr::DR, 0);
        enabling_port.write(0x3FA, fcr::FE);
        assert_eq!(enabling_port.read(0x3FD) & lsr::DR, 0);
        assert!(!enabling_port.int_pending_rda);
        enabling_port.write(0x3F8, 0x42);
        advance_char(&mut enabling_port);
        assert_eq!(enabling_port.read(0x3F8), 0x42);

        let mut port = SerialPort::new(1);
        port.write(0x3F9, ier::ERBFI);
        port.write(0x3FC, mcr::LOOP);

        // A same-mode FCR write must leave a non-FIFO RBR byte available.
        port.write(0x3F8, 0x58);
        advance_char(&mut port);
        assert_ne!(port.read(0x3FD) & lsr::DR, 0);
        port.write(0x3FA, 0);
        assert_ne!(port.read(0x3FD) & lsr::DR, 0);
        assert!(port.int_pending_rda);
        assert_eq!(port.read(0x3F8), 0x58);

        // Reading X leaves its value latched in RBR. Enabling FIFO makes that
        // non-FIFO RBR value invalid until a new character arrives.
        port.write(0x3FA, fcr::FE);
        assert_eq!(port.read(0x3FD) & lsr::DR, 0);
        assert!(!port.int_pending_rda);

        // Loopback supplies Y through the guest-visible transmitter and receiver.
        port.write(0x3F8, 0x59);
        advance_char(&mut port);
        assert_eq!(port.rx_data_len(), 1);
        assert_ne!(port.read(0x3FD) & lsr::DR, 0);

        // Reprogramming the same FIFO mode keeps an unread FIFO byte intact.
        port.write(0x3FA, fcr::FE);
        assert_eq!(port.rx_data_len(), 1);
        assert_eq!(port.read(0x3F8), 0x59);

        // Leave a FIFO byte unread until the character timeout is pending.
        port.write(0x3FA, fcr::FE | 0x40);
        port.write(0x3F8, 0x5A);
        advance_char(&mut port);
        assert_eq!(port.rx_data_len(), 1);
        let timeout = port.fifo_timeout_cycle.expect("one byte below trigger");
        advance_until_deadline(&mut port, timeout);
        assert!(port.int_pending_timeout);

        // Disabling FIFO discards the remaining FIFO byte and must not make the old
        // non-FIFO RBR latch look ready again.
        port.write(0x3FA, 0);
        assert_eq!(port.read(0x3FD) & lsr::DR, 0);
        assert!(!port.rx_data_ready());
        assert!(!port.int_pending_rda);
        assert!(!port.int_pending_timeout);
        assert_eq!(port.read(0x3FA) & 0x0F, iir::NO_INT);

        // Reception resumes normally after the mode transition.
        port.write(0x3F8, 0x5B);
        advance_char(&mut port);
        assert_ne!(port.read(0x3FD) & lsr::DR, 0);
        assert_eq!(port.read(0x3F8), 0x5B);
    }

    #[test]
    fn test_fifo_loopback() {
        let mut port = SerialPort::new(1);

        port.write(0x3FA, fcr::FE);
        port.write(0x3FC, mcr::LOOP | mcr::OUT2);

        port.write(0x3F8, 0x11);
        port.write(0x3F8, 0x22);
        port.write(0x3F8, 0x33);

        advance_char(&mut port);
        assert_eq!(port.read(0x3F8), 0x11);
        advance_char(&mut port);
        assert_eq!(port.read(0x3F8), 0x22);
        advance_char(&mut port);
        assert_eq!(port.read(0x3F8), 0x33);
    }

    #[test]
    fn test_fifo_loopback_overrun() {
        let mut port = SerialPort::new(1);

        port.write(0x3FA, fcr::FE);
        port.write(0x3FC, mcr::LOOP);
        for _ in 0..FIFO_SIZE {
            port.rx_fifo.push_back(0x11);
        }

        port.write(0x3F8, 0x22);
        advance_char(&mut port);
        assert_ne!(port.lsr & lsr::OE, 0);
        assert!(port.int_pending_rls);
    }

    #[test]
    fn test_fifo_loopback_trigger_sets_rda() {
        let mut port = SerialPort::new(1);

        port.write(0x3FA, fcr::FE | 0x40);
        port.write(0x3FC, mcr::LOOP);

        port.write(0x3F8, 0x11);
        advance_char(&mut port);
        assert!(!port.int_pending_rda);

        port.write(0x3F8, 0x22);
        port.write(0x3F8, 0x33);
        port.write(0x3F8, 0x44);
        advance_char(&mut port);
        advance_char(&mut port);
        advance_char(&mut port);
        assert!(port.int_pending_rda);
    }

    #[test]
    fn test_get_transmitted_byte_none_when_empty() {
        let mut port = SerialPort::new(1);

        assert_eq!(port.get_transmitted_byte(), None);
    }

    #[test]
    fn test_fifo_rda_clears_when_below_trigger() {
        let mut port = SerialPort::new(1);

        port.write(0x3FA, fcr::FE | 0x40); // FIFO, trigger level 4
        for value in 0..4 {
            port.receive_byte(0x10 + value);
            advance_char(&mut port);
        }
        assert!(port.int_pending_rda);

        let _ = port.read(0x3F8);
        assert!(!port.int_pending_rda);
        assert_ne!(port.lsr & lsr::DR, 0);
    }

    #[test]
    fn test_loopback_overrun_non_fifo() {
        let mut port = SerialPort::new(1);

        port.write(0x3FC, mcr::LOOP);
        port.write(0x3F8, 0x11);
        advance_char(&mut port);
        port.write(0x3F8, 0x22);
        advance_char(&mut port);

        assert_ne!(port.lsr & lsr::OE, 0);
        assert!(port.int_pending_rls);
        assert_eq!(port.read(0x3F8), 0x11);
    }

    #[test]
    fn test_iir_priority_and_fifo_flag() {
        let mut port = SerialPort::new(1);

        port.fifo_enabled = true;
        port.ier = ier::ELSI | ier::ERBFI | ier::ETBEI | ier::EDSSI;

        port.int_pending_rls = true;
        assert_eq!(port.read(0x3FA) & 0x0F, iir::RLS);

        port.int_pending_rls = false;
        port.int_pending_rda = true;
        assert_eq!(port.read(0x3FA) & 0x0F, iir::RDA);

        port.int_pending_rda = false;
        port.thre_int_pending = true;
        assert_eq!(port.read(0x3FA) & 0x0F, iir::THRE);
        assert!(!port.thre_int_pending);

        port.int_pending_msr = true;
        assert_eq!(port.read(0x3FA) & 0x0F, iir::MODEM_STATUS);

        port.int_pending_msr = false;
        let iir = port.read(0x3FA);
        assert_eq!(iir & 0x0F, iir::NO_INT);
        assert_ne!(iir & iir::FIFO_ENABLED, 0);
    }

    #[test]
    fn test_write_fcr_reset_bits() {
        let mut port = SerialPort::new(1);

        port.write(0x3FA, fcr::FE);
        port.rx_fifo.push_back(0x11);
        port.lsr |= lsr::DR;
        port.tx_fifo.push_back(0x22);
        port.lsr &= !(lsr::THRE | lsr::TEMT);

        port.write(0x3FA, fcr::FE | fcr::RFR | fcr::TFR);

        assert!(port.rx_fifo.is_empty());
        assert!(port.tx_fifo.is_empty());
        assert_eq!(port.lsr & lsr::DR, 0);
        assert_ne!(port.lsr & (lsr::THRE | lsr::TEMT), 0);
    }

    #[test]
    fn test_loopback_out1_teri() {
        let mut port = SerialPort::new(1);

        port.write(0x3FC, mcr::LOOP | mcr::OUT1);
        let msr = port.read(0x3FE);
        assert_ne!(msr & msr::RI, 0);

        port.write(0x3FC, mcr::LOOP);
        let msr = port.read(0x3FE);
        assert_ne!(msr & msr::TERI, 0);
    }

    #[test]
    fn test_interrupt_pending_gate() {
        let mut port = SerialPort::new(1);

        port.int_pending_rda = true;
        port.ier = ier::ERBFI;
        assert!(!port.interrupt_pending());

        port.mcr = mcr::OUT2;
        port.int_pending_rda = false;
        port.int_pending_rls = true;
        port.ier = ier::ELSI;
        assert!(port.interrupt_pending());

        port.int_pending_rls = false;
        port.thre_int_pending = true;
        port.ier = ier::ETBEI;
        assert!(port.interrupt_pending());

        port.thre_int_pending = false;
        port.int_pending_msr = true;
        port.ier = ier::EDSSI;
        assert!(port.interrupt_pending());

        port.int_pending_msr = false;
        port.int_pending_rda = true;
        port.ier = ier::ERBFI;
        assert!(port.interrupt_pending());

        port.int_pending_rls = true;
        port.int_pending_rda = true;
        port.thre_int_pending = true;
        port.int_pending_msr = true;
        port.ier = 0;
        assert!(!port.interrupt_pending());
    }

    #[test]
    fn test_fifo_receive_overrun_and_trigger() {
        let mut port = SerialPort::new(1);

        port.write(0x3FA, fcr::FE);
        port.receive_byte(0x10);
        assert!(port.int_pending_rda);

        for _ in 1..FIFO_SIZE {
            port.receive_byte(0x11);
        }
        assert_eq!(port.rx_fifo.len(), FIFO_SIZE);

        port.receive_byte(0x22);
        assert_ne!(port.lsr & lsr::OE, 0);
        assert!(port.int_pending_rls);
    }

    #[test]
    fn test_get_transmitted_byte_paths() {
        let mut port = SerialPort::new(1);

        port.write(0x3F8, 0x5A);
        assert_eq!(port.get_transmitted_byte(), Some(0x5A));

        port.write(0x3FA, fcr::FE);
        port.write(0x3F8, 0x33);
        assert_eq!(port.get_transmitted_byte(), Some(0x33));
    }

    #[test]
    fn test_serial_ports_interrupts() {
        let mut ports = SerialPorts::new();

        ports.ports[3].mcr = mcr::OUT2;
        ports.ports[3].ier = ier::ERBFI;
        ports.ports[3].int_pending_rda = true;
        let (irq3, irq4) = ports.check_interrupts();
        assert!(irq3);
        assert!(!irq4);

        ports.ports[3].int_pending_rda = false;
        ports.ports[1].mcr = mcr::OUT2;
        ports.ports[1].ier = ier::ERBFI;
        ports.ports[1].int_pending_rda = true;
        let (irq3, _irq4) = ports.check_interrupts();
        assert!(irq3);

        ports.ports[1].int_pending_rda = false;
        ports.ports[2].mcr = mcr::OUT2;
        ports.ports[2].ier = ier::ERBFI;
        ports.ports[2].int_pending_rda = true;
        let (irq3, irq4) = ports.check_interrupts();
        assert!(!irq3);
        assert!(irq4);

        ports.ports[2].int_pending_rda = false;
        ports.ports[0].mcr = mcr::OUT2;
        ports.ports[0].ier = ier::ERBFI;
        ports.ports[0].int_pending_rda = true;
        let (_irq3, irq4) = ports.check_interrupts();
        assert!(irq4);
    }

    // --- Branch coverage tests ---

    #[test]
    fn test_tick_char_timeout_without_pending_deadline_is_a_noop() {
        let mut port = SerialPort::new(1);
        let before = port.current_cycle;

        port.tick_char_timeout();

        assert_eq!(port.current_cycle, before + port.char_cycles);
        assert!(!port.int_pending_timeout);
        assert_eq!(port.next_deadline(), None);
    }

    #[test]
    fn test_tick_char_timeout_does_not_advance_tx_or_rx_deadlines() {
        let mut port = SerialPort::new(1);

        port.write(0x3F8, 0xA5);
        assert_eq!(port.tx_shift, Some(0xA5));
        assert_eq!(port.take_completed_tx_byte(), None);

        assert!(port.enqueue_line_rx(0x5A));
        assert_eq!(port.rx_shift, Some(0x5A));
        assert_eq!(port.lsr & lsr::DR, 0);

        port.tick_char_timeout();

        assert_eq!(port.tx_shift, Some(0xA5));
        assert_eq!(port.take_completed_tx_byte(), None);
        assert_eq!(port.rx_shift, Some(0x5A));
        assert_eq!(port.lsr & lsr::DR, 0);
    }

    #[test]
    fn test_fifo_timeout_is_character_deadline_based_and_rearms_correctly() {
        let mut port = SerialPort::new(1);
        port.write(0x3FA, fcr::FE | 0xC0);

        port.receive_byte(0x41);
        let first_deadline = port.fifo_timeout_cycle.expect("timeout should arm");

        advance_until_deadline(&mut port, first_deadline.saturating_sub(1));
        assert!(!port.int_pending_timeout);

        port.receive_byte(0x42);
        let second_deadline = port.fifo_timeout_cycle.expect("timeout should re-arm");
        assert!(second_deadline > first_deadline);

        advance_until_deadline(&mut port, second_deadline.saturating_sub(1));
        assert!(!port.int_pending_timeout);

        advance_until_deadline(&mut port, second_deadline);
        assert!(port.int_pending_timeout);
    }

    #[test]
    fn test_iir_timeout_priority() {
        // Branch: IIR returns TIMEOUT when int_pending_timeout=true, ERBFI enabled,
        // and higher-priority (RLS, RDA) not pending (line 432-433)
        let mut port = SerialPort::new(1);
        port.fifo_enabled = true;
        port.ier = ier::ERBFI;
        port.int_pending_timeout = true;
        port.int_pending_rls = false;
        port.int_pending_rda = false;
        port.thre_int_pending = false;

        let iir = port.read(0x3FA);
        assert_eq!(iir & 0x0F, iir::TIMEOUT);
        assert_ne!(iir & iir::FIFO_ENABLED, 0);
    }

    #[test]
    fn test_iir_fifo_disabled_no_fifo_bits() {
        // Branch: IIR with FIFO disabled returns no FIFO_ENABLED flag (line 447)
        let mut port = SerialPort::new(1);
        assert!(!port.fifo_enabled);
        port.ier = 0;
        port.thre_int_pending = false;

        let iir = port.read(0x3FA);
        assert_eq!(iir & iir::FIFO_ENABLED, 0);
        assert_eq!(iir, iir::NO_INT);
    }

    #[test]
    fn test_loopback_out2_to_dcd() {
        // Branch: MCR.OUT2 -> MSR.DCD routing in loopback (lines 523-525)
        let mut port = SerialPort::new(1);

        // Set loopback + OUT2
        port.write(0x3FC, mcr::LOOP | mcr::OUT2);
        let msr = port.read(0x3FE);
        assert_ne!(msr & msr::DCD, 0, "DCD should follow OUT2");
        assert_ne!(msr & msr::DDCD, 0, "DDCD delta should be set");

        // Clear OUT2, keep loopback
        port.write(0x3FC, mcr::LOOP);
        let msr = port.read(0x3FE);
        assert_eq!(msr & msr::DCD, 0, "DCD should be cleared when OUT2 cleared");
        assert_ne!(msr & msr::DDCD, 0, "DDCD delta should be set on transition");
    }

    #[test]
    fn test_loopback_dcd_delta_sets_msr_interrupt() {
        // Branch: DDCD delta detection triggers int_pending_msr (lines 541-544)
        let mut port = SerialPort::new(1);

        port.write(0x3FC, mcr::LOOP | mcr::OUT2);
        assert!(
            port.int_pending_msr,
            "MSR interrupt should be pending on DCD transition"
        );
    }

    #[test]
    fn test_interrupt_pending_timeout_path() {
        // Branch: interrupt_pending returns true for timeout (line 558)
        let mut port = SerialPort::new(1);
        port.mcr = mcr::OUT2;
        port.ier = ier::ERBFI;
        port.int_pending_timeout = true;

        assert!(port.interrupt_pending());
    }

    #[test]
    fn test_thr_non_fifo_write() {
        let mut port = SerialPort::new(1);
        assert!(!port.fifo_enabled);

        port.write(0x3F8, 0xAB);

        assert_ne!(port.lsr & lsr::THRE, 0);
        assert_eq!(port.lsr & lsr::TEMT, 0);
        assert!(port.thre_int_pending);
        assert_eq!(port.get_transmitted_byte(), Some(0xAB));

        advance_char(&mut port);
        assert_ne!(port.lsr & lsr::TEMT, 0);
    }

    #[test]
    fn test_get_transmitted_byte_thre_already_set() {
        // Branch: get_transmitted_byte when THRE already set => None (lines 617-618)
        let mut port = SerialPort::new(1);
        assert!(!port.fifo_enabled);
        // THRE is set by default
        assert_ne!(port.lsr & lsr::THRE, 0);

        assert_eq!(port.get_transmitted_byte(), None);
    }

    #[test]
    fn test_serial_port_default() {
        // Covers Default::default() for SerialPort (line 201-203)
        let port = SerialPort::default();
        assert_eq!(port.base_port(), 0x3F8);
        assert_eq!(port.irq(), 4);
    }

    #[test]
    fn test_serial_ports_default() {
        // Covers Default::default() for SerialPorts (lines 656-658)
        let ports = SerialPorts::default();
        assert_eq!(ports.ports[0].base_port(), 0x3F8);
        assert_eq!(ports.ports[1].base_port(), 0x2F8);
        assert!(ports.is_present(0));
        assert!(ports.is_present(3));
    }

    #[test]
    fn test_serial_ports_presence_mask_hides_absent_ports() {
        let mut ports = SerialPorts::new_with_present_mask([true, false, false, false]);

        assert!(ports.write(0x3FF, 0x5A));
        assert_eq!(ports.read(0x3FF), Some(0x5A));

        assert!(!ports.write(0x2FF, 0xA5));
        assert_eq!(ports.read(0x2FF), None);
        assert!(!ports.is_present(1));
    }

    #[test]
    fn test_serial_ports_set_present_enables_port() {
        let mut ports = SerialPorts::new_with_present_mask([false; 4]);

        assert_eq!(ports.read(0x3FF), None);
        ports.set_present(0, true);

        assert!(ports.write(0x3FF, 0x33));
        assert_eq!(ports.read(0x3FF), Some(0x33));
        assert!(ports.is_present(0));
    }

    #[test]
    fn test_io_trace_logs_writes_and_only_changed_reads() {
        use std::sync::{Arc, Mutex};
        #[derive(Clone, Default)]
        struct Captured(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Captured {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(
                "gp64::hw::serial::io=trace",
            ))
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let mut ports = SerialPorts::new();
        ports.write(0x2FF, 0x41);
        // A polled register is logged once until its value changes.
        for _ in 0..5 {
            ports.read(0x2FF);
        }
        ports.write(0x2FF, 0x42);
        ports.read(0x2FF);

        let text = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        let lines: Vec<_> = text
            .lines()
            .map(|line| line.split("serial::io: ").nth(1).unwrap())
            .collect();
        assert_eq!(
            lines,
            [
                "COM2 write 2FF = 41",
                "COM2 read 2FF = 41",
                "COM2 write 2FF = 42",
                "COM2 read 2FF = 42",
            ]
        );
    }

    #[test]
    fn test_serial_port_invalid_port_num() {
        // Covers the _ => (0x3F8, 4) fallback in new() (line 214)
        let port = SerialPort::new(5);
        assert_eq!(port.base_port(), 0x3F8);
        assert_eq!(port.irq(), 4);
    }

    #[test]
    fn test_serial_ports_get_port() {
        // Covers get_port() (lines 675-680)
        let mut ports = SerialPorts::new();
        assert!(ports.get_port(1).is_some());
        assert!(ports.get_port(4).is_some());
        assert!(ports.get_port(0).is_none());
        assert!(ports.get_port(5).is_none());
    }

    #[test]
    fn test_serial_ports_com3_com4_rw() {
        // Covers COM3 and COM4 read/write paths in SerialPorts (lines 688-689, 706-712)
        let mut ports = SerialPorts::new();

        // COM3 scratch register
        assert!(ports.write(0x3EF, 0x12));
        assert_eq!(ports.read(0x3EF), Some(0x12));

        // COM4 scratch register
        assert!(ports.write(0x2EF, 0x34));
        assert_eq!(ports.read(0x2EF), Some(0x34));
    }

    #[test]
    fn test_serial_ports_reset() {
        // Covers SerialPorts::reset() (lines 727-731)
        let mut ports = SerialPorts::new();
        ports.ports[0].ier = ier::ERBFI;
        ports.ports[1].mcr = mcr::OUT2;

        ports.reset();

        assert_eq!(ports.ports[0].ier, 0);
        assert_eq!(ports.ports[1].mcr, 0);
    }

    #[test]
    fn test_serial_port_reset() {
        let mut port = SerialPort::new(1);
        port.ier = ier::ERBFI;
        port.mcr = mcr::OUT2;
        port.fifo_enabled = true;
        port.int_pending_rda = true;
        port.int_pending_timeout = true;
        port.current_cycle = 123;
        port.tx_shift = Some(0x12);
        port.tx_deadline_cycle = Some(124);
        port.rx_shift = Some(0x34);
        port.rx_deadline_cycle = Some(125);
        port.fifo_timeout_cycle = Some(126);

        port.reset();

        assert_eq!(port.ier, 0);
        assert_eq!(port.mcr, 0);
        assert!(!port.fifo_enabled);
        assert!(!port.int_pending_rda);
        assert!(!port.int_pending_timeout);
        assert_eq!(port.current_cycle, 0);
        assert_eq!(port.tx_shift, None);
        assert_eq!(port.rx_shift, None);
        assert_eq!(port.next_deadline(), None);
        assert_ne!(port.lsr & lsr::THRE, 0);
    }

    #[test]
    fn test_read_write_unused_port_offsets() {
        // Covers read/write to offset 5 (LSR write=noop), 6 (MSR write=noop),
        // and out-of-range offset (lines 278-283, 325-328)
        let mut port = SerialPort::new(1);

        // Reading LCR (offset 3) returns the LCR value
        port.lcr = 0x1B;
        assert_eq!(port.read(0x3FB), 0x1B);

        // Reading MCR (offset 4) returns the MCR value
        port.mcr = 0x0A;
        assert_eq!(port.read(0x3FC), 0x0A);

        // Reading out-of-range offset returns 0xFF
        assert_eq!(port.read(0x3F8 + 8), 0xFF);

        // Writing to LSR (offset 5) is a no-op
        port.write(0x3FD, 0xFF);
        // Writing to MSR (offset 6) is a no-op
        port.write(0x3FE, 0xFF);
        // Writing to out-of-range offset is a no-op
        port.write(0x3F8 + 8, 0xFF);
    }

    // --- external_tx tests ---

    #[test]
    fn test_write_thr_external_tx_preserves_byte() {
        let mut port = SerialPort::new(1);
        port.external_tx = true;

        port.write(0x3F8, 0xAB);

        assert_eq!(port.thr, 0xAB);
        assert_ne!(port.lsr & lsr::THRE, 0);
        assert_eq!(port.lsr & lsr::TEMT, 0);
        assert_eq!(port.take_completed_tx_byte(), None);
    }

    #[test]
    fn test_write_thr_external_tx_fifo_preserves_bytes() {
        let mut port = SerialPort::new(1);
        port.external_tx = true;
        port.write(0x3FA, fcr::FE); // Enable FIFO

        port.write(0x3F8, 0x11);
        port.write(0x3F8, 0x22);

        assert_eq!(port.tx_shift, Some(0x11));
        assert_eq!(port.tx_fifo.len(), 1);
        assert_eq!(port.lsr & lsr::THRE, 0);
        assert_eq!(port.lsr & lsr::TEMT, 0);
    }

    #[test]
    fn test_write_thr_external_tx_false_still_uses_timed_completion() {
        let mut port = SerialPort::new(1);
        assert!(!port.external_tx);

        port.write(0x3F8, 0x77);

        assert_ne!(port.lsr & lsr::THRE, 0);
        assert_eq!(port.lsr & lsr::TEMT, 0);
        assert_eq!(port.take_completed_tx_byte(), None);

        advance_char(&mut port);
        assert_eq!(port.take_completed_tx_byte(), Some(0x77));
        assert_ne!(port.lsr & lsr::TEMT, 0);
    }

    #[test]
    fn test_write_thr_external_tx_loopback_still_works() {
        let mut port = SerialPort::new(1);
        port.external_tx = true;
        port.write(0x3FC, mcr::LOOP | mcr::OUT2);

        port.write(0x3F8, 0x42);

        assert_eq!(port.lsr & lsr::DR, 0);
        advance_char(&mut port);
        assert_eq!(port.read(0x3F8), 0x42);
    }

    // --- get_transmitted_byte FIFO fix tests ---

    #[test]
    fn test_take_completed_tx_byte_fifo_preserves_tx_status_semantics() {
        let mut port = SerialPort::new(1);
        port.write(0x3FA, fcr::FE);
        port.write(0x3F8, 0x55);
        advance_char(&mut port);

        let byte = port.take_completed_tx_byte();
        assert_eq!(byte, Some(0x55));
        assert_ne!(port.lsr & lsr::THRE, 0);
        assert_ne!(port.lsr & lsr::TEMT, 0);
        assert!(port.thre_int_pending);
    }

    #[test]
    fn test_take_completed_tx_byte_fifo_does_not_change_non_empty_status() {
        let mut port = SerialPort::new(1);
        port.write(0x3FA, fcr::FE);
        port.write(0x3F8, 0x11);
        port.write(0x3F8, 0x22);
        advance_char(&mut port);

        let byte = port.take_completed_tx_byte();
        assert_eq!(byte, Some(0x11));
        assert_ne!(port.lsr & lsr::THRE, 0);
        assert_eq!(port.lsr & lsr::TEMT, 0);
        assert_eq!(port.tx_shift, Some(0x22));
    }

    #[test]
    fn test_take_completed_tx_byte_excludes_loopback_traffic() {
        let mut port = SerialPort::new(1);
        port.write(0x3FC, mcr::LOOP | mcr::OUT2);

        port.write(0x3F8, 0x66);
        advance_char(&mut port);

        assert_eq!(port.take_completed_tx_byte(), None);
        assert_eq!(port.read(0x3F8), 0x66);
    }

    #[test]
    fn test_get_transmitted_byte_fifo_empty_returns_none() {
        let mut port = SerialPort::new(1);
        port.fifo_enabled = true;

        assert_eq!(port.get_transmitted_byte(), None);
    }

    // --- can_receive tests ---

    #[test]
    fn test_can_receive_reflects_guest_receive_capacity() {
        let mut port = SerialPort::new(1);
        assert!(port.can_receive());

        port.receive_byte(0x00);
        assert!(!port.can_receive());
        port.read(0x3F8);
        assert!(port.can_receive());
    }

    #[test]
    fn test_rx_available_reflects_guest_receive_capacity() {
        let mut port = SerialPort::new(1);
        assert_eq!(port.rx_available(), 1);

        port.receive_byte(0x11);
        assert_eq!(port.rx_available(), 0);
    }

    #[test]
    fn test_line_rx_methods_track_staged_line_capacity() {
        let mut port = SerialPort::new(1);
        assert_eq!(port.line_rx_capacity(), LINE_QUEUE_SIZE);
        assert!(port.enqueue_line_rx(0x11));
        assert_eq!(port.line_rx_capacity(), LINE_QUEUE_SIZE);

        for _ in 0..LINE_QUEUE_SIZE {
            assert!(port.enqueue_line_rx(0x22));
        }

        assert_eq!(port.line_rx_capacity(), 0);
        assert!(!port.enqueue_line_rx(0x33));
    }

    // --- update_msr_signals tests ---

    #[test]
    fn test_update_msr_signals_connect() {
        // Setting DSR+CTS+DCD sets correct bits and delta bits
        let mut port = SerialPort::new(1);
        // Start with all signals low, no deltas
        port.msr = 0;

        port.update_msr_signals(true, true, true, false);

        // Signal bits should be set
        assert_ne!(port.msr & msr::DSR, 0);
        assert_ne!(port.msr & msr::CTS, 0);
        assert_ne!(port.msr & msr::DCD, 0);
        assert_eq!(port.msr & msr::RI, 0);

        // Delta bits should be set for the transitions
        assert_ne!(port.msr & msr::DDSR, 0);
        assert_ne!(port.msr & msr::DCTS, 0);
        assert_ne!(port.msr & msr::DDCD, 0);

        // MSR interrupt should be pending
        assert!(port.int_pending_msr);
    }

    #[test]
    fn test_update_msr_signals_disconnect() {
        // Clearing signals sets delta bits
        let mut port = SerialPort::new(1);
        // Start with signals high
        port.msr = msr::DSR | msr::CTS | msr::DCD;
        // Read MSR to clear any deltas
        port.read_msr();
        port.int_pending_msr = false;

        port.update_msr_signals(false, false, false, false);

        // Signal bits should be cleared
        assert_eq!(port.msr & msr::DSR, 0);
        assert_eq!(port.msr & msr::CTS, 0);
        assert_eq!(port.msr & msr::DCD, 0);

        // Delta bits should be set for the transitions
        assert_ne!(port.msr & msr::DDSR, 0);
        assert_ne!(port.msr & msr::DCTS, 0);
        assert_ne!(port.msr & msr::DDCD, 0);

        assert!(port.int_pending_msr);
    }

    #[test]
    fn test_update_msr_signals_preserves_unread_deltas() {
        // Calling twice without read_msr() preserves deltas
        let mut port = SerialPort::new(1);
        port.msr = 0;

        // First: raise CTS
        port.update_msr_signals(false, true, false, false);
        assert_ne!(port.msr & msr::DCTS, 0);

        // Second: raise DSR (without reading MSR first)
        port.update_msr_signals(true, true, false, false);

        // Both DCTS (preserved) and DDSR (new) should be set
        assert_ne!(port.msr & msr::DCTS, 0, "DCTS should be preserved");
        assert_ne!(port.msr & msr::DDSR, 0, "DDSR should be set");
    }

    #[test]
    fn test_update_msr_signals_teri_trailing_edge() {
        // TERI only fires when RI goes high->low
        let mut port = SerialPort::new(1);
        port.msr = 0;

        // Raise RI
        port.update_msr_signals(false, false, false, true);
        assert_ne!(port.msr & msr::RI, 0);
        // TERI should NOT be set on rising edge
        assert_eq!(port.msr & msr::TERI, 0);

        // Read MSR to clear deltas
        port.read_msr();
        port.int_pending_msr = false;

        // Lower RI (trailing edge)
        port.update_msr_signals(false, false, false, false);
        assert_eq!(port.msr & msr::RI, 0);
        // TERI should be set on trailing edge
        assert_ne!(port.msr & msr::TERI, 0);
        assert!(port.int_pending_msr);
    }

    #[test]
    fn test_update_msr_signals_no_change_no_interrupt() {
        // No signal change should not trigger interrupt
        let mut port = SerialPort::new(1);
        port.msr = msr::DSR | msr::CTS;
        port.read_msr(); // clear deltas
        port.int_pending_msr = false;

        port.update_msr_signals(true, true, false, false);

        // No transitions, no interrupt
        assert!(!port.int_pending_msr);
    }

    // --- null modem simulation tests ---

    #[test]
    fn test_null_modem_dtr_rts_sets_dsr_cts_dcd() {
        // When no external device is connected (external_tx=false) and not in
        // loopback, writing DTR+RTS to MCR should set DSR+CTS+DCD in MSR,
        // simulating a null modem cable.
        let mut port = SerialPort::new(1);
        assert!(!port.external_tx);

        // Set DTR and RTS (no loopback)
        port.write(0x3FC, mcr::DTR | mcr::RTS);

        // Read MSR
        let msr_val = port.read(0x3FE);
        assert_ne!(msr_val & msr::DSR, 0, "DSR should follow DTR (null modem)");
        assert_ne!(msr_val & msr::CTS, 0, "CTS should follow RTS (null modem)");
        assert_ne!(msr_val & msr::DCD, 0, "DCD should follow DTR (null modem)");
        // Delta bits should be set since MSR transitioned from 0
        assert_ne!(
            msr_val & msr::DDSR,
            0,
            "DDSR delta should be set on transition"
        );
        assert_ne!(
            msr_val & msr::DCTS,
            0,
            "DCTS delta should be set on transition"
        );
        assert_ne!(
            msr_val & msr::DDCD,
            0,
            "DDCD delta should be set on transition"
        );
    }

    #[test]
    fn test_null_modem_clear_dtr_rts_clears_signals() {
        let mut port = SerialPort::new(1);

        // Set DTR+RTS first
        port.write(0x3FC, mcr::DTR | mcr::RTS);
        port.read(0x3FE); // Read MSR to clear deltas

        // Now clear DTR+RTS
        port.write(0x3FC, 0);

        let msr_val = port.read(0x3FE);
        assert_eq!(msr_val & msr::DSR, 0, "DSR should be cleared");
        assert_eq!(msr_val & msr::CTS, 0, "CTS should be cleared");
        assert_eq!(msr_val & msr::DCD, 0, "DCD should be cleared");
        // Delta bits set on transition
        assert_ne!(msr_val & msr::DDSR, 0, "DDSR on transition");
        assert_ne!(msr_val & msr::DCTS, 0, "DCTS on transition");
        assert_ne!(msr_val & msr::DDCD, 0, "DDCD on transition");
    }

    #[test]
    fn test_null_modem_disabled_when_external_tx() {
        // When external_tx is set, MCR writes should NOT update MSR
        // (the external device manages signals via update_msr_signals).
        let mut port = SerialPort::new(1);
        port.external_tx = true;

        // Set DTR and RTS
        port.write(0x3FC, mcr::DTR | mcr::RTS);

        // MSR should still be 0 — external device hasn't set anything
        let msr_val = port.read(0x3FE);
        assert_eq!(
            msr_val & msr::DSR,
            0,
            "DSR should NOT be set when external_tx is true"
        );
        assert_eq!(
            msr_val & msr::CTS,
            0,
            "CTS should NOT be set when external_tx is true"
        );
    }

    #[test]
    fn test_null_modem_interrupt_on_signal_change() {
        // Null modem signal changes should raise MSR interrupt
        let mut port = SerialPort::new(1);

        // Enable modem status interrupt
        port.write(0x3F9, ier::EDSSI); // IER
        port.write(0x3FC, mcr::OUT2); // OUT2 to enable interrupts

        // Set DTR+RTS — should trigger MSR interrupt
        port.write(0x3FC, mcr::OUT2 | mcr::DTR | mcr::RTS);
        assert!(
            port.interrupt_pending(),
            "MSR interrupt should be pending after null modem signal change"
        );
    }

    #[test]
    fn test_null_modem_mcr_writes_preserve_unread_msr_deltas() {
        let mut port = SerialPort::new(1);
        port.write(0x3F9, ier::EDSSI);

        // Raise DSR and DCD, then CTS. Repeating either MCR value and later
        // lowering DSR/DCD must not erase any delta before an MSR read.
        port.write(0x3FC, mcr::OUT2 | mcr::DTR);
        port.write(0x3FC, mcr::OUT2 | mcr::DTR | mcr::RTS);
        port.write(0x3FC, mcr::OUT2 | mcr::DTR | mcr::RTS);
        port.write(0x3FC, mcr::OUT2 | mcr::RTS);
        port.write(0x3FC, mcr::OUT2 | mcr::RTS);

        let signal_mask = msr::DSR | msr::CTS | msr::DCD;
        let delta_mask = msr::DCTS | msr::DDSR | msr::DDCD;
        assert_eq!(port.msr & signal_mask, msr::CTS, "latest signal levels");
        assert_eq!(
            port.msr & delta_mask,
            delta_mask,
            "null-modem MCR writes must preserve every unread signal delta"
        );
        assert!(
            port.interrupt_pending(),
            "unread deltas should keep the IRQ pending"
        );

        let first_read = port.read(0x3FE);
        assert_eq!(
            first_read & signal_mask,
            msr::CTS,
            "MSR reports current levels"
        );
        assert_eq!(
            first_read & delta_mask,
            delta_mask,
            "MSR reports accumulated deltas"
        );
        assert!(
            !port.interrupt_pending(),
            "reading MSR clears the pending IRQ"
        );

        let second_read = port.read(0x3FE);
        assert_eq!(
            second_read & signal_mask,
            msr::CTS,
            "signal levels remain set"
        );
        assert_eq!(
            second_read & delta_mask,
            0,
            "MSR read clears deltas exactly once"
        );
    }

    #[test]
    fn test_loopback_mcr_writes_preserve_unread_msr_deltas() {
        let mut port = SerialPort::new(1);
        port.write(0x3F9, ier::EDSSI);

        // Raise DCD/RI, then DSR and CTS; lower RI to produce TERI. Repeated
        // writes and later transitions must retain all deltas until MSR read.
        port.write(0x3FC, mcr::LOOP | mcr::OUT2 | mcr::OUT1);
        port.write(0x3FC, mcr::LOOP | mcr::OUT2 | mcr::OUT1 | mcr::DTR);
        port.write(
            0x3FC,
            mcr::LOOP | mcr::OUT2 | mcr::OUT1 | mcr::DTR | mcr::RTS,
        );
        port.write(
            0x3FC,
            mcr::LOOP | mcr::OUT2 | mcr::OUT1 | mcr::DTR | mcr::RTS,
        );
        port.write(0x3FC, mcr::LOOP | mcr::OUT2 | mcr::DTR | mcr::RTS);
        port.write(0x3FC, mcr::LOOP | mcr::OUT2 | mcr::DTR | mcr::RTS);

        let signal_mask = msr::DSR | msr::CTS | msr::RI | msr::DCD;
        let delta_mask = msr::DCTS | msr::DDSR | msr::TERI | msr::DDCD;
        assert_eq!(
            port.msr & signal_mask,
            msr::DSR | msr::CTS | msr::DCD,
            "latest loopback signal levels"
        );
        assert_eq!(
            port.msr & delta_mask,
            delta_mask,
            "loopback MCR writes must preserve every unread signal delta"
        );
        assert!(
            port.interrupt_pending(),
            "unread deltas should keep the IRQ pending"
        );

        let first_read = port.read(0x3FE);
        assert_eq!(
            first_read & signal_mask,
            msr::DSR | msr::CTS | msr::DCD,
            "MSR reports current levels"
        );
        assert_eq!(
            first_read & delta_mask,
            delta_mask,
            "MSR reports accumulated deltas"
        );
        assert!(
            !port.interrupt_pending(),
            "reading MSR clears the pending IRQ"
        );

        let second_read = port.read(0x3FE);
        assert_eq!(
            second_read & signal_mask,
            msr::DSR | msr::CTS | msr::DCD,
            "signal levels remain set"
        );
        assert_eq!(
            second_read & delta_mask,
            0,
            "MSR read clears deltas exactly once"
        );
    }

    // --- reset + external_tx tests ---

    #[test]
    fn test_reset_clears_external_tx() {
        let mut port = SerialPort::new(1);
        port.external_tx = true;
        port.reset();
        assert!(!port.external_tx);
    }

    #[test]
    fn test_external_tx_round_trip() {
        let mut port = SerialPort::new(1);
        port.write(0x3FA, fcr::FE);
        port.external_tx = true;

        port.write(0x3F8, 0xAA);
        port.write(0x3F8, 0xBB);
        port.write(0x3F8, 0xCC);

        assert_eq!(port.tx_shift, Some(0xAA));
        assert_eq!(port.tx_fifo.len(), 2);
        assert_eq!(port.lsr & lsr::THRE, 0);
        assert_eq!(port.take_completed_tx_byte(), None);

        advance_char(&mut port);
        let b0 = port.get_transmitted_byte();
        advance_char(&mut port);
        let b1 = port.get_transmitted_byte();
        advance_char(&mut port);
        let b2 = port.get_transmitted_byte();

        assert_eq!(b0, Some(0xAA));
        assert_eq!(b1, Some(0xBB));
        assert_eq!(b2, Some(0xCC));
        assert_eq!(port.get_transmitted_byte(), None);
        assert_ne!(port.lsr & lsr::THRE, 0);
        assert_ne!(port.lsr & lsr::TEMT, 0);
    }

    #[test]
    fn test_write_fcr_rx_reset_flushes_line_staging() {
        let mut port = SerialPort::new(1);
        port.write(0x3FA, fcr::FE);

        assert!(port.enqueue_line_rx(0x11));
        assert!(port.enqueue_line_rx(0x22));
        assert_eq!(port.rx_shift, Some(0x11));
        assert_eq!(port.pending_line_rx.len(), 1);

        port.write(0x3FA, fcr::FE | fcr::RFR);

        assert!(port.rx_fifo.is_empty());
        assert!(port.pending_line_rx.is_empty());
        assert_eq!(port.rx_shift, None);
        assert_eq!(port.rx_deadline_cycle, None);
        advance_char(&mut port);
        assert_eq!(port.lsr & lsr::DR, 0);
    }

    #[test]
    fn test_enqueue_line_rx_uses_timed_delivery() {
        let mut port = SerialPort::new(1);

        assert!(port.enqueue_line_rx(0x42));
        assert_eq!(port.lsr & lsr::DR, 0);

        advance_char(&mut port);
        assert_ne!(port.lsr & lsr::DR, 0);
        assert_eq!(port.read(0x3F8), 0x42);
    }

    #[test]
    fn test_char_cycles_use_emulated_cpu_cycle_units() {
        let mut port = SerialPort::new(1);
        port.write(0x3FB, lcr::DLAB | 0x03);
        port.write(0x3F8, 12);
        port.write(0x3F9, 0);
        port.write(0x3FB, 0x03);
        assert_eq!(port.char_cycles, 26_250);

        port.write(0x3FB, lcr::DLAB | 0x03);
        port.write(0x3F8, 48);
        port.write(0x3F9, 0);
        port.write(0x3FB, 0x03);
        assert_eq!(port.char_cycles, 105_000);

        port.set_cycles_per_second(11_520_000);
        assert_eq!(port.char_cycles, 48_000);

        port.write(0x3FB, 0x0B);
        assert_eq!(port.char_cycles, 52_800);
    }

    /// Configure port 1 for `lcr_value` at divisor 12 and report the resulting
    /// character time.
    fn char_cycles_for_lcr(lcr_value: u8) -> u64 {
        let mut port = SerialPort::new(1);
        port.write(0x3FB, lcr::DLAB | lcr_value);
        port.write(0x3F8, 12);
        port.write(0x3F9, 0);
        port.write(0x3FB, lcr_value);
        port.char_cycles
    }

    #[test]
    fn five_bit_words_use_one_and_a_half_stop_bits() {
        // An 8250 sends 1.5 stop bits when STB is set with a five-bit word, and
        // two only for six- to eight-bit words. Whole-bit arithmetic rounded
        // 5N1.5 up to 5N2 (MDD-BUG-CRUCIBLE-00557).

        // 5N1: 1 start + 5 data + 1 stop = 7 bits.
        assert_eq!(char_cycles_for_lcr(0x00), 18_375);
        // 5N1.5: 7.5 bits, not 8 — 8 would give 21_000.
        assert_eq!(char_cycles_for_lcr(lcr::STB), 19_687);
        // Six data bits and up keep the full two stop bits: 1 + 6 + 2 = 9.
        assert_eq!(char_cycles_for_lcr(0x01 | lcr::STB), 23_625);
        // 8N2 is unchanged: 1 + 8 + 2 = 11 bits.
        assert_eq!(char_cycles_for_lcr(0x03 | lcr::STB), 28_875);
        // 8N1 is unchanged: 1 + 8 + 1 = 10 bits.
        assert_eq!(char_cycles_for_lcr(0x03), 26_250);
    }

    #[test]
    fn tx_deadline_follows_the_five_bit_frame_length() {
        // The character time is the deadline unit, so the half-bit correction
        // has to reach TX pacing and not just the stored value.
        let mut port = SerialPort::new(1);
        port.write(0x3FB, lcr::DLAB | lcr::STB);
        port.write(0x3F8, 12);
        port.write(0x3F9, 0);
        port.write(0x3FB, lcr::STB); // 5N1.5
        assert_eq!(port.char_cycles, 19_687);

        let start = port.current_cycle;
        port.write(0x3F8, b'A'); // THR
        assert_eq!(
            port.tx_deadline_cycle,
            Some(start + 19_687),
            "a 5N1.5 character must not be paced as 5N2"
        );
    }

    // ------------------------------------------------------------------
    // Checkpoint round-trip tests
    // ------------------------------------------------------------------

    #[test]
    fn test_save_restore_roundtrip_default() {
        let original = SerialPort::new(1);
        let (regs, fifos, fifo_ctl, int_flags, misc, timing) = original.save_state();

        let mut restored = SerialPort::new(1);
        // Dirty some fields to verify restore overwrites them
        restored.rbr = 0xFF;
        restored.lsr = 0;
        restored.thre_int_pending = false;

        restored.restore_state(
            regs,
            (&fifos.0, &fifos.1, &fifos.2, &fifos.3),
            fifo_ctl,
            int_flags,
            misc,
            timing,
        );

        // Verify registers match
        let (r2, f2, fc2, if2, m2, t2) = restored.save_state();
        let (r1, f1, fc1, if1, m1, t1) = original.save_state();
        assert_eq!(r1, r2, "registers mismatch");
        assert_eq!(f1, f2, "fifos mismatch");
        assert_eq!(fc1, fc2, "fifo control mismatch");
        assert_eq!(if1, if2, "interrupt flags mismatch");
        assert_eq!(m1, m2, "misc flags mismatch");
        assert_eq!(t1, t2, "timing mismatch");
    }

    #[test]
    fn test_save_restore_roundtrip_with_active_state() {
        let mut port = SerialPort::new(2);
        port.set_cycles_per_second(25_200_000);

        // Configure 9600 baud (divisor 12)
        port.write(0x2FB, lcr::DLAB | 0x03); // DLAB=1, 8N1
        port.write(0x2F8, 12); // DLL = 12
        port.write(0x2F9, 0); // DLH = 0
        port.write(0x2FB, 0x03); // DLAB=0, 8N1

        // Enable FIFO mode
        port.write(0x2FA, fcr::FE | 0xC0); // FIFO enable, trigger=14

        // Enable interrupts
        port.write(0x2F9, ier::ERBFI | ier::ETBEI);

        // Transmit some bytes to populate TX state
        port.write(0x2F8, 0x41); // 'A'
        port.write(0x2F8, 0x42); // 'B'

        // Inject some RX data
        port.receive_byte(0x58); // 'X'
        port.receive_byte(0x59); // 'Y'

        // Advance partway through a transmission
        port.advance_cycles(port.char_cycles / 2);

        // Snapshot
        let (regs, fifos, fifo_ctl, int_flags, misc, timing) = port.save_state();

        // Restore into a fresh port (same port_num to match base_port/irq)
        let mut restored = SerialPort::new(2);
        restored.restore_state(
            regs,
            (&fifos.0, &fifos.1, &fifos.2, &fifos.3),
            fifo_ctl,
            int_flags,
            misc,
            timing,
        );

        // Verify the restored port has the same state
        let (r2, f2, fc2, if2, m2, t2) = restored.save_state();
        let (r1, f1, fc1, if1, m1, t1) = port.save_state();
        assert_eq!(r1, r2, "registers mismatch after active state restore");
        assert_eq!(f1, f2, "fifos mismatch after active state restore");
        assert_eq!(fc1, fc2, "fifo control mismatch");
        assert_eq!(if1, if2, "interrupt flags mismatch");
        assert_eq!(m1, m2, "misc flags mismatch");
        assert_eq!(t1, t2, "timing mismatch");

        // Verify timing deadlines survived
        assert!(
            restored.next_deadline().is_some(),
            "deadlines should be present"
        );
        assert_eq!(port.next_deadline(), restored.next_deadline());
    }

    #[test]
    fn test_save_restore_roundtrip_fifo_order() {
        let mut port = SerialPort::new(1);
        port.write(0x3FA, fcr::FE); // Enable FIFOs

        // Push bytes into RX in order
        for b in 0x30..0x3A {
            port.receive_byte(b);
        }
        // Advance to let them arrive
        port.advance_cycles(port.char_cycles * 10);

        let (regs, fifos, fifo_ctl, int_flags, misc, timing) = port.save_state();

        let mut restored = SerialPort::new(1);
        restored.restore_state(
            regs,
            (&fifos.0, &fifos.1, &fifos.2, &fifos.3),
            fifo_ctl,
            int_flags,
            misc,
            timing,
        );

        // Read bytes back from restored port — order must be preserved
        let original_fifo = fifos.0.clone();
        let (_, restored_fifos, ..) = restored.save_state();
        assert_eq!(
            original_fifo, restored_fifos.0,
            "RX FIFO order not preserved"
        );
    }

    // ------------------------------------------------------------------
    // Speed-change retiming tests (Stage 2D)
    // ------------------------------------------------------------------

    #[test]
    fn test_retime_speed_doubles_tx_deadline_preserves_remaining() {
        let mut port = SerialPort::new(1);
        let old_cps: u64 = 25_200_000; // 25,200 cycles/ms fixed rate
        port.set_cycles_per_second(old_cps);

        // Set up a TX in flight: current_cycle=1000, deadline=2000 => remaining=1000
        port.advance_cycles(1000);
        port.tx_deadline_cycle = Some(2000);

        // Double the speed
        let new_cps: u64 = 50_400_000;
        port.retime_for_speed_change(new_cps);

        // remaining=1000, scaled = 1000 * 50_400_000 / 25_200_000 = 2000
        // new deadline = 1000 + 2000 = 3000
        assert_eq!(port.tx_deadline_cycle, Some(3000));
        assert_eq!(port.cycles_per_second(), new_cps);
    }

    #[test]
    fn test_retime_speed_halves_tx_deadline_extends() {
        let mut port = SerialPort::new(1);
        let old_cps: u64 = 50_400_000;
        port.set_cycles_per_second(old_cps);

        // current_cycle=1000, deadline=3000 => remaining=2000
        port.advance_cycles(1000);
        port.tx_deadline_cycle = Some(3000);

        // Halve the speed
        let new_cps: u64 = 25_200_000;
        port.retime_for_speed_change(new_cps);

        // remaining=2000, scaled = 2000 * 25_200_000 / 50_400_000 = 1000
        // new deadline = 1000 + 1000 = 2000
        assert_eq!(port.tx_deadline_cycle, Some(2000));
    }

    #[test]
    fn test_retime_char_cycles_recomputed() {
        let mut port = SerialPort::new(1);
        port.set_cycles_per_second(25_200_000);
        let old_char_cycles = port.char_cycles();

        port.retime_for_speed_change(50_400_000);
        let new_char_cycles = port.char_cycles();

        // char_cycles should double when cycles_per_second doubles
        assert_eq!(new_char_cycles, old_char_cycles * 2);
    }

    #[test]
    fn test_retime_no_deadlines_active_is_noop() {
        let mut port = SerialPort::new(1);
        port.set_cycles_per_second(25_200_000);
        port.advance_cycles(1000);

        // No deadlines active
        assert!(port.tx_deadline_cycle.is_none());
        assert!(port.rx_deadline_cycle.is_none());
        assert!(port.fifo_timeout_cycle.is_none());

        port.retime_for_speed_change(50_400_000);

        // Still no deadlines
        assert!(port.tx_deadline_cycle.is_none());
        assert!(port.rx_deadline_cycle.is_none());
        assert!(port.fifo_timeout_cycle.is_none());

        // But cycles_per_second and char_cycles updated
        assert_eq!(port.cycles_per_second(), 50_400_000);
    }

    #[test]
    fn test_retime_all_three_deadlines_scale() {
        let mut port = SerialPort::new(1);
        let old_cps: u64 = 25_200_000;
        port.set_cycles_per_second(old_cps);
        port.advance_cycles(1000);

        // Set all three deadlines in flight
        port.tx_deadline_cycle = Some(2000); // remaining = 1000
        port.rx_deadline_cycle = Some(3000); // remaining = 2000
        port.fifo_timeout_cycle = Some(5000); // remaining = 4000

        // Triple the speed
        let new_cps: u64 = 75_600_000;
        port.retime_for_speed_change(new_cps);

        // Scale factor = 75_600_000 / 25_200_000 = 3
        // tx: 1000 + 1000*3 = 4000
        // rx: 1000 + 2000*3 = 7000
        // fifo: 1000 + 4000*3 = 13000
        assert_eq!(port.tx_deadline_cycle, Some(4000));
        assert_eq!(port.rx_deadline_cycle, Some(7000));
        assert_eq!(port.fifo_timeout_cycle, Some(13000));
    }

    #[test]
    fn test_retime_past_deadline_clamps_to_current_cycle() {
        let mut port = SerialPort::new(1);
        let old_cps: u64 = 50_400_000;
        port.set_cycles_per_second(old_cps);
        port.advance_cycles(2000);

        // Deadline already in the past relative to current_cycle
        // (shouldn't normally happen, but defensive)
        port.tx_deadline_cycle = Some(1500);

        port.retime_for_speed_change(25_200_000);

        // remaining would be negative => clamp to current_cycle
        assert_eq!(port.tx_deadline_cycle, Some(2000));
    }

    #[test]
    fn test_retime_serial_ports_all_ports() {
        let mut ports = SerialPorts::new();
        for (i, port) in ports.ports.iter_mut().enumerate() {
            port.set_cycles_per_second(25_200_000);
            port.advance_cycles(1000);
            port.tx_deadline_cycle = Some(2000 + i as u64 * 1000);
        }

        ports.retime_for_speed_change(50_400_000);

        // All ports should have been retimed
        for (i, port) in ports.ports.iter().enumerate() {
            let old_remaining = 1000 + i as u64 * 1000;
            let new_remaining = old_remaining * 2; // speed doubled
            assert_eq!(
                port.tx_deadline_cycle,
                Some(1000 + new_remaining),
                "COM{} deadline not retimed correctly",
                i + 1
            );
            assert_eq!(port.cycles_per_second(), 50_400_000);
        }
    }

    // --- try_write_thr / non-consuming RX queries (DOS AUX device support) ---

    #[test]
    fn test_try_write_thr_refuses_rather_than_dropping_in_fifo_mode() {
        let mut port = SerialPort::new(1);
        port.write(0x3FA, fcr::FE);

        // The first byte moves straight into the shift register, so FIFO_SIZE
        // more fit behind it before the transmit FIFO is full.
        for value in 0..=FIFO_SIZE as u8 {
            assert!(port.try_write_thr(value), "byte {value} should be accepted");
        }
        assert_eq!(port.tx_fifo.len(), FIFO_SIZE);

        // A plain THR write here would silently drop the byte.
        assert!(!port.try_write_thr(0xEE), "a full TX FIFO must refuse");
        assert_eq!(port.tx_fifo.len(), FIFO_SIZE);
        assert_eq!(port.tx_fifo.back(), Some(&(FIFO_SIZE as u8)));
    }

    #[test]
    fn test_try_write_thr_refuses_rather_than_overwriting_in_non_fifo_mode() {
        let mut port = SerialPort::new(1);
        assert!(!port.fifo_enabled);

        // First byte enters the shift register, second sits in THR.
        assert!(port.try_write_thr(0x41));
        assert!(port.try_write_thr(0x42));
        assert!(port.thr_loaded);

        // A third write_thr would overwrite the un-shifted 0x42.
        assert!(!port.try_write_thr(0x43), "a loaded THR must refuse");
        assert_eq!(port.thr, 0x42);
        assert!(port.thr_loaded);

        // Both accepted bytes still leave the UART, in order.
        advance_char(&mut port);
        assert_eq!(port.take_completed_tx_byte(), Some(0x41));
        advance_char(&mut port);
        assert_eq!(port.take_completed_tx_byte(), Some(0x42));
        assert_eq!(port.take_completed_tx_byte(), None);
    }

    /// A byte the caller was TOLD was accepted must not be destroyed later.
    /// Guarding THR/FIFO room alone let exactly that happen: `complete_tx_shift`
    /// silently drops a finished byte when `completed_tx` is full, a character
    /// time after `try_write_thr` had already returned true.
    #[test]
    fn test_try_write_thr_refuses_when_completed_tx_would_overflow() {
        let mut port = SerialPort::new(1);

        // Nothing drains `completed_tx`, so it fills as bytes shift out.
        let mut accepted: Vec<u8> = Vec::new();
        for value in 0..(COMPLETED_TX_SIZE as u8 + 8) {
            if !port.try_write_thr(value) {
                break;
            }
            accepted.push(value);
            advance_char(&mut port);
        }

        assert_eq!(
            accepted.len(),
            COMPLETED_TX_SIZE,
            "acceptance must stop at the completed-tx capacity, not past it"
        );
        assert!(
            !port.try_write_thr(0xEE),
            "a byte that could not survive completion must be refused"
        );

        // Every accepted byte really did survive, in order and exactly once.
        let drained: Vec<u8> = std::iter::from_fn(|| port.take_completed_tx_byte()).collect();
        assert_eq!(drained, accepted);

        // Draining re-opens the queue.
        assert!(port.try_write_thr(0xEE));
    }

    /// The completed-tx guard must count what is still in flight, not just what
    /// has already landed: bytes sitting in the transmit FIFO are bound for the
    /// same queue and reach it first.
    #[test]
    fn test_try_write_thr_counts_in_flight_bytes_against_completed_tx() {
        const PARKED: usize = 20;
        let mut port = SerialPort::new(1);
        port.write(0x3FA, fcr::FE);

        // Park finished bytes in `completed_tx` without draining them.
        for _ in 0..PARKED {
            assert!(port.try_write_thr(0x41));
            advance_char(&mut port);
        }
        assert_eq!(port.completed_tx.len(), PARKED);

        // The transmit FIFO would take 17 more, but only 12 can be promised a
        // completed-tx slot, so that is where acceptance must stop.
        let mut accepted = 0usize;
        while port.try_write_thr(0x42) {
            accepted += 1;
            assert!(accepted <= COMPLETED_TX_SIZE, "runaway acceptance");
        }
        assert_eq!(accepted, COMPLETED_TX_SIZE - PARKED);

        // Shift them all out; not one accepted byte is lost on the way.
        for _ in 0..accepted + 2 {
            advance_char(&mut port);
        }
        let drained: Vec<u8> = std::iter::from_fn(|| port.take_completed_tx_byte()).collect();
        assert_eq!(drained.len(), PARKED + accepted);
    }

    /// Loopback never pushes to `completed_tx`, so the queue must not be
    /// allowed to throttle a loopback transmitter.
    #[test]
    fn test_try_write_thr_ignores_completed_tx_under_loopback() {
        let mut port = SerialPort::new(1);
        port.write(0x3FC, mcr::LOOP);

        for _ in 0..COMPLETED_TX_SIZE * 2 {
            assert!(port.try_write_thr(0x41), "loopback must not be throttled");
            advance_char(&mut port);
            // Drain the loopback receiver so it never becomes the constraint.
            while port.rx_data_ready() {
                let _ = port.read(0x3F8);
            }
        }
        assert_eq!(port.take_completed_tx_byte(), None);
    }

    #[test]
    fn test_try_write_thr_applies_baud_pacing() {
        let mut port = SerialPort::new(1);

        assert!(port.try_write_thr(0x5A));
        assert_eq!(
            port.take_completed_tx_byte(),
            None,
            "byte must not leave the UART before a character time"
        );

        port.advance_cycles(port.char_cycles - 1);
        assert_eq!(port.take_completed_tx_byte(), None);

        advance_char(&mut port);
        assert_eq!(port.take_completed_tx_byte(), Some(0x5A));
    }

    #[test]
    fn test_rx_data_queries_are_non_consuming_in_fifo_mode() {
        let mut port = SerialPort::new(1);
        port.write(0x3FA, fcr::FE);

        assert!(!port.rx_data_ready());
        assert_eq!(port.rx_data_len(), 0);

        port.receive_byte(0x41);
        port.receive_byte(0x42);

        // Query repeatedly: no call may pop the FIFO or clear data-ready.
        assert!(port.rx_data_ready());
        assert_eq!(port.rx_data_len(), 2);
        assert!(port.rx_data_ready());
        assert_eq!(port.rx_data_len(), 2);
        assert_ne!(port.read(0x3FD) & lsr::DR, 0);

        // Both bytes are still there for a direct RBR read.
        assert_eq!(port.read(0x3F8), 0x41);
        assert_eq!(port.rx_data_len(), 1);
        assert_eq!(port.read(0x3F8), 0x42);
        assert!(!port.rx_data_ready());
        assert_eq!(port.rx_data_len(), 0);
    }

    #[test]
    fn test_rx_data_queries_are_non_consuming_in_non_fifo_mode() {
        let mut port = SerialPort::new(1);
        assert!(!port.fifo_enabled);

        assert!(!port.rx_data_ready());
        assert_eq!(port.rx_data_len(), 0);

        port.receive_byte(0x37);
        assert!(port.rx_data_ready());
        assert_eq!(port.rx_data_len(), 1);
        assert!(port.rx_data_ready());
        assert_eq!(port.rx_data_len(), 1);
        assert_ne!(port.read(0x3FD) & lsr::DR, 0);

        // The byte survives the queries and is still readable from the RBR.
        assert_eq!(port.read(0x3F8), 0x37);
        assert!(!port.rx_data_ready());
        assert_eq!(port.rx_data_len(), 0);
    }
}
