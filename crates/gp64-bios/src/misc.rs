//! Miscellaneous BIOS interrupts (INT 11h, 12h, 14h, 17h)

use anyhow::Result;
use gp64_cpu::Cpu;
use gp64_memory::Memory;

use super::Bios;

/// Disposition returned by the INT 14h HLE handler.
pub enum Int14Disp {
    /// The subfunction completed; the caller should return to the guest.
    Iret,
    /// A timeout wait is still running and has yielded (MDD-BUG-KILN-00278).
    /// The caller must return to the guest with CS:IP rewound onto the `INT
    /// 14h` instruction so the next tick re-enters the handler, which resumes
    /// from the saved poll progress.
    Retry,
}

/// Which of a wait subfunction's two phases is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SerialWaitPhase {
    /// AH=01h: DSR+CTS. AH=02h: DSR.
    ModemReady,
    /// AH=01h: THRE. AH=02h: DR.
    Transfer,
}

/// Poll progress of an INT 14h timeout wait, carried across the dispatches the
/// wait is chunked over (MDD-BUG-KILN-00278).
///
/// Keyed by `(ah, dx)`: a re-entry for a different subfunction or port is a
/// DIFFERENT request — an interrupt handler using another COM port mid-wait,
/// say — so it starts fresh rather than inheriting these polls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SerialWait {
    ah: u8,
    dx: u16,
    phase: SerialWaitPhase,
    /// Polls already spent in `phase`, against the BDA-derived limit.
    polled: u32,
}

/// Outcome of one bounded run of polls.
enum PollOutcome {
    /// The awaited condition became true.
    Ready,
    /// The BDA-derived limit was reached: the guest's timeout has expired.
    Expired,
    /// This dispatch's host-work budget ran out; the wait must resume later.
    Yielded,
}

impl Bios {
    /// INT 11h - Get equipment list
    pub fn int11(&self, cpu: &mut Cpu) -> Result<()> {
        // Equipment word:
        // Bit 0: Floppy disk installed
        // Bit 1: Math coprocessor installed
        // Bit 2: PS/2 pointing device installed
        // Bits 4-5: Initial video mode (10 = 80x25 color)
        // Bits 6-7: Number of floppy drives - 1
        cpu.regs.set_ax(Self::EQUIPMENT_WORD);
        Ok(())
    }

    /// INT 12h - Get memory size
    pub fn int12(&self, cpu: &mut Cpu) -> Result<()> {
        cpu.regs.set_ax(Self::CONVENTIONAL_MEMORY_KB);
        Ok(())
    }

    /// Look up UART baud rate divisor (115200 / baud).
    /// Codes 0-7 match AH=00h bits 7-5; code 8 adds 19200 for AH=04h.
    fn baud_divisor(code: u8) -> u16 {
        match code {
            0 => 0x0417, // 110
            1 => 0x0300, // 150
            2 => 0x0180, // 300
            3 => 0x00C0, // 600
            4 => 0x0060, // 1200
            5 => 0x0030, // 2400
            6 => 0x0018, // 4800
            7 => 0x000C, // 9600
            8 => 0x0006, // 19200
            _ => 0x000C, // default: 9600
        }
    }

    /// Program UART baud rate and line control, enable FIFO, set DTR+RTS+OUT2.
    fn uart_init(serial: &mut gp64_hw::serial::SerialPorts, port_base: u16, divisor: u16, lcr: u8) {
        serial.write(port_base + 3, lcr | 0x80); // LCR with DLAB=1
        serial.write(port_base, (divisor & 0xFF) as u8); // DLL
        serial.write(port_base + 1, (divisor >> 8) as u8); // DLM
        serial.write(port_base + 3, lcr); // LCR with DLAB=0
        serial.write(port_base + 2, 0x01); // FCR: enable 16550 FIFO
        serial.write(port_base + 1, 0x00); // IER = 0
        serial.write(port_base + 4, 0x0B); // MCR = DTR + RTS + OUT2
        let _ = serial.read(port_base + 2); // Clear THRE pending
        let _ = serial.read(port_base); // Clear DR
    }

    /// Read BDA COM timeout and convert to polling iteration count.
    /// The BDA byte at 0040:007C+port is the outer loop count; each outer
    /// iteration does 0xFFFF inner polls (matching IBM BIOS structure).
    fn bda_timeout_iterations(memory: &Memory, port_index: u16) -> u32 {
        debug_assert!(port_index < 4, "BDA only has COM1-COM4 timeout slots");
        let timeout = memory.read_u8(0x47C + port_index as usize);
        if timeout == 0 {
            0xFFFF
        } else {
            (timeout as u32) * 0xFFFF
        }
    }

    /// Number of I/O ports a 16550 UART occupies: RBR/THR/DLL, IER/DLM, IIR/FCR,
    /// LCR, MCR, LSR, MSR, SCR.
    const UART_REGISTER_WINDOW: u16 = 8;

    /// Highest COM base that can still address a whole UART register window.
    ///
    /// The BDA is guest-writable, so `0040:0000` can hold any `u16` — including
    /// values so high that `port_base + offset` leaves the port space. Every
    /// INT 14h subfunction derives register addresses by adding to this base,
    /// so a base above this bound overflowed: an abort in overflow-checked
    /// development and test builds, and silent wrapping onto unrelated ports in
    /// release (MDD-BUG-KILN-00275).
    const MAX_COM_BASE: u16 = u16::MAX - (Self::UART_REGISTER_WINDOW - 1);

    /// Polls one INT 14h dispatch may spend before yielding to the scheduler
    /// (MDD-BUG-KILN-00278).
    ///
    /// A multiple of the 256-poll serial-pump group, so chunking does not move
    /// a single pump tick: the wait still ticks at poll 0, 256, 512, … exactly
    /// as the monolithic loop did. Large enough that a device which is already
    /// ready — the overwhelmingly common case — still completes in the first
    /// dispatch with no re-entry at all, and small enough that the worst case
    /// (BDA timeout FFh, 16,711,425 polls per phase) can no longer hold the
    /// host for the whole wait.
    pub const INT14_POLL_CHUNK: u32 = 1024;

    /// Guest cycles charged per poll.
    ///
    /// The wait used to run at zero guest cost, which is what let it starve the
    /// scheduler AND made the wait take no guest time at all. One cycle per
    /// poll is this emulator's own unit of "one instruction's worth of work"
    /// (see the Timing Model in CLAUDE.md) and is the floor for a real BIOS
    /// poll, which is at least an `IN` plus a test and a branch.
    pub const INT14_CYCLES_PER_POLL: u64 = 1;

    /// Run up to `budget` polls of one wait phase.
    ///
    /// Returns the outcome and the polls actually spent. `polled` is advanced
    /// in place so the caller can persist it across a yield; the pump tick
    /// cadence is keyed on it, which is what keeps a chunked wait tick-for-tick
    /// identical to the monolithic one.
    fn poll_wait_phase(
        serial: &mut gp64_hw::serial::SerialPorts,
        serial_tick: &mut dyn FnMut(&mut gp64_hw::serial::SerialPorts, usize),
        port_index: usize,
        limit: u32,
        polled: &mut u32,
        budget: &mut u32,
        mut ready: impl FnMut(&mut gp64_hw::serial::SerialPorts) -> bool,
    ) -> (PollOutcome, u32) {
        let started_at = *polled;
        while *polled < limit {
            if *budget == 0 {
                return (PollOutcome::Yielded, *polled - started_at);
            }
            if (*polled).is_multiple_of(256) {
                serial_tick(serial, port_index);
            }
            *polled += 1;
            *budget -= 1;
            if ready(serial) {
                return (PollOutcome::Ready, *polled - started_at);
            }
        }
        (PollOutcome::Expired, *polled - started_at)
    }

    /// Resume the saved wait if this call is the same request, else start one.
    fn resume_serial_wait(&mut self, ah: u8, dx: u16) -> SerialWait {
        match self.serial_wait {
            Some(saved) if saved.ah == ah && saved.dx == dx => saved,
            _ => SerialWait {
                ah,
                dx,
                phase: SerialWaitPhase::ModemReady,
                polled: 0,
            },
        }
    }

    /// Whether a BDA COM base addresses a usable UART.
    ///
    /// Zero is the BDA's own "no port installed" encoding; anything that cannot
    /// cover the register window is malformed. Both are reported to the guest
    /// as an absent port rather than being probed.
    fn com_base_is_usable(port_base: u16) -> bool {
        port_base != 0 && port_base <= Self::MAX_COM_BASE
    }

    /// INT 14h - Serial port services
    ///
    /// Implements the standard BIOS serial port interface by reading/writing
    /// UART registers at the base address stored in the BDA. This is how DOS
    ///
    /// The two timeout waits (AH=01h, AH=02h) are CHUNKED: each dispatch spends
    /// at most [`Self::INT14_POLL_CHUNK`] polls and then returns
    /// [`Int14Disp::Retry`], leaving the caller to rewind CS:IP onto the `INT
    /// 14h` instruction so the next tick resumes the wait. Before
    /// MDD-BUG-KILN-00278 the whole wait ran as one synchronous host loop at
    /// zero guest cost, so a guest that set BDA `0040:007C` to FFh and issued
    /// an idle request could stall the CPU, PIT, IRQs, input and IPC for up to
    /// 16,711,425 polls per phase.
    pub fn int14(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        serial: &mut gp64_hw::serial::SerialPorts,
        serial_tick: &mut dyn FnMut(&mut gp64_hw::serial::SerialPorts, usize),
    ) -> Result<Int14Disp> {
        let ah = cpu.regs.ah();
        let dx = cpu.regs.dx(); // Port number (0=COM1, 1=COM2, etc.)

        // Look up COM port base address from BDA (0040:0000 + port * 2).
        let port_base = if (dx as usize) < 4 {
            memory.read_u16(0x400 + (dx as usize) * 2)
        } else {
            0
        };

        if !Self::com_base_is_usable(port_base) {
            // No port at this index, or a BDA base that cannot cover the UART
            // register window (MDD-BUG-KILN-00275) — return timeout.
            tracing::debug!(
                target: "gp64::hw::serial",
                ah = format_args!("{:02X}", ah),
                dx = format_args!("{:04X}", dx),
                "INT 14h: no port at index"
            );
            cpu.regs.set_ah(0x80);
            cpu.regs.set_al(0x00);
            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            self.serial_wait = None;
            return Ok(Int14Disp::Iret);
        }

        tracing::debug!(
            target: "gp64::hw::serial",
            ah = format_args!("{:02X}", ah),
            dx = dx,
            base = format_args!("0x{:03X}", port_base),
            "INT 14h"
        );

        match ah {
            // AH=00h: Initialize serial port
            // AL = parameters: bits 7-5 = baud rate, 4-3 = parity, 2 = stop bits, 1-0 = word length
            // Returns: AH = line status (LSR), AL = modem status (MSR)
            0x00 => {
                let al = cpu.regs.al();

                let divisor = Self::baud_divisor((al >> 5) & 0x07);

                // Build LCR from bits 4-0 of AL
                // Bits 1-0: word length (00=5, 01=6, 10=7, 11=8)
                // Bit 2: stop bits (0=1 stop, 1=2 stop)
                // Bits 4-3: parity (00=none, 01=odd, 11=even)
                let lcr = al & 0x1F;

                Self::uart_init(serial, port_base, divisor, lcr);

                // Return status
                let lsr = serial.read(port_base + 5).unwrap_or(0x00);
                let msr = serial.read(port_base + 6).unwrap_or(0x00);
                cpu.regs.set_ah(lsr);
                cpu.regs.set_al(msr);
            }

            // AH=01h: Write character to serial port
            // AL = character to write
            // Returns: AH = line status (bit 7 set = timeout)
            0x01 => {
                let ch = cpu.regs.al();

                // Assert DTR + RTS (modem control: we're ready to send).
                // Idempotent, so a resumed chunk re-asserting it is harmless.
                serial.write(port_base + 4, 0x03); // MCR = DTR + RTS

                let limit = Self::bda_timeout_iterations(memory, dx);
                let mut wait = self.resume_serial_wait(ah, dx);
                let mut budget = Self::INT14_POLL_CHUNK;
                let mut spent = 0;

                // Phase 1: Wait for DSR + CTS (modem ready to receive).
                // MSR bit 5 = DSR, bit 4 = CTS.
                if wait.phase == SerialWaitPhase::ModemReady {
                    let (outcome, polls) = Self::poll_wait_phase(
                        serial,
                        serial_tick,
                        dx as usize,
                        limit,
                        &mut wait.polled,
                        &mut budget,
                        |serial| serial.read(port_base + 6).unwrap_or(0) & 0x30 == 0x30,
                    );
                    spent += polls;
                    match outcome {
                        PollOutcome::Ready => {
                            wait.phase = SerialWaitPhase::Transfer;
                            wait.polled = 0;
                        }
                        PollOutcome::Expired => {
                            let msr = serial.read(port_base + 6).unwrap_or(0);
                            cpu.regs.set_ah(0x80 | msr);
                            return self.finish_int14(cpu, spent);
                        }
                        PollOutcome::Yielded => return self.yield_int14(cpu, wait, spent),
                    }
                }

                // Phase 2: Wait for THRE (transmitter ready). Tick the
                // serial pump periodically because the UART's external_tx
                // mode holds bytes until the data pump drains them —
                // without ticking, THRE would never clear.
                let mut sent_lsr = None;
                let (outcome, polls) = Self::poll_wait_phase(
                    serial,
                    serial_tick,
                    dx as usize,
                    limit,
                    &mut wait.polled,
                    &mut budget,
                    |serial| {
                        let lsr = serial.read(port_base + 5).unwrap_or(0);
                        if lsr & 0x20 != 0 {
                            sent_lsr = Some(lsr);
                            true
                        } else {
                            false
                        }
                    },
                );
                spent += polls;
                match outcome {
                    PollOutcome::Ready => {
                        let lsr = sent_lsr.unwrap_or(0);
                        serial.write(port_base, ch);
                        cpu.regs.set_ah(lsr & 0x7F);
                    }
                    PollOutcome::Expired => {
                        let lsr = serial.read(port_base + 5).unwrap_or(0);
                        cpu.regs.set_ah(0x80 | lsr);
                    }
                    PollOutcome::Yielded => return self.yield_int14(cpu, wait, spent),
                }
                return self.finish_int14(cpu, spent);
            }

            // AH=02h: Read character from serial port
            // Returns: AH = line status (bit 7 set = timeout), AL = character read
            0x02 => {
                // Assert DTR (we're ready to receive; no RTS needed for RX).
                // Idempotent, so a resumed chunk re-asserting it is harmless.
                serial.write(port_base + 4, 0x01); // MCR = DTR only

                let limit = Self::bda_timeout_iterations(memory, dx);
                let mut wait = self.resume_serial_wait(ah, dx);
                let mut budget = Self::INT14_POLL_CHUNK;
                let mut spent = 0;

                // Phase 1: Wait for DSR (remote is ready).
                // MSR bit 5 = DSR. CTS not required for receiving.
                if wait.phase == SerialWaitPhase::ModemReady {
                    let (outcome, polls) = Self::poll_wait_phase(
                        serial,
                        serial_tick,
                        dx as usize,
                        limit,
                        &mut wait.polled,
                        &mut budget,
                        |serial| serial.read(port_base + 6).unwrap_or(0) & 0x20 != 0,
                    );
                    spent += polls;
                    match outcome {
                        PollOutcome::Ready => {
                            wait.phase = SerialWaitPhase::Transfer;
                            wait.polled = 0;
                        }
                        PollOutcome::Expired => {
                            let msr = serial.read(port_base + 6).unwrap_or(0);
                            cpu.regs.set_ah(0x80 | msr);
                            return self.finish_int14(cpu, spent);
                        }
                        PollOutcome::Yielded => return self.yield_int14(cpu, wait, spent),
                    }
                }

                // Phase 2: Wait for DR (data ready). Tick the serial pump
                // so the modem can deliver responses to the UART RX.
                let mut got_lsr = None;
                let (outcome, polls) = Self::poll_wait_phase(
                    serial,
                    serial_tick,
                    dx as usize,
                    limit,
                    &mut wait.polled,
                    &mut budget,
                    |serial| {
                        let lsr = serial.read(port_base + 5).unwrap_or(0);
                        if lsr & 0x01 != 0 {
                            got_lsr = Some(lsr);
                            true
                        } else {
                            false
                        }
                    },
                );
                spent += polls;
                match outcome {
                    PollOutcome::Ready => {
                        let lsr = got_lsr.unwrap_or(0);
                        let ch = serial.read(port_base).unwrap_or(0);
                        cpu.regs.set_al(ch);
                        // Return error bits only (OE, PE, FE, BI = bits 1-4).
                        // DR (bit 0) was just consumed, THRE/TEMT (bits 5-6)
                        // are irrelevant for receive, bit 7 = 0 means success.
                        cpu.regs.set_ah(lsr & 0x1E);
                    }
                    PollOutcome::Expired => {
                        let lsr = serial.read(port_base + 5).unwrap_or(0);
                        cpu.regs.set_ah(0x80 | lsr);
                    }
                    PollOutcome::Yielded => return self.yield_int14(cpu, wait, spent),
                }
                return self.finish_int14(cpu, spent);
            }

            // AH=03h: Get port status
            // Returns: AH = line status (LSR), AL = modem status (MSR)
            0x03 => {
                let lsr = serial.read(port_base + 5).unwrap_or(0);
                let msr = serial.read(port_base + 6).unwrap_or(0);
                cpu.regs.set_ah(lsr);
                cpu.regs.set_al(msr);
            }

            // AH=04h: Extended initialize (PS/2)
            // CL = baud rate, CH = word length, BL = stop bits, BH = parity, AL = break
            0x04 => {
                let divisor = Self::baud_divisor(cpu.regs.cl());

                // Build LCR from CH (word length), BL (stop bits), BH (parity), AL (break)
                let word_len = cpu.regs.ch() & 0x03; // bits 1-0
                let stop_bit = (cpu.regs.bl() & 0x01) << 2; // bit 2
                let parity = match cpu.regs.bh() {
                    0x01 => 0x08, // odd:  PEN=1
                    0x02 => 0x18, // even: PEN=1, EPS=1
                    0x03 => 0x28, // stick odd:  PEN=1, STICK=1
                    0x04 => 0x38, // stick even: PEN=1, EPS=1, STICK=1
                    _ => 0x00,    // none (0x00 or any other value)
                };
                let brk = if cpu.regs.al() == 0x00 { 0x40 } else { 0x00 }; // bit 6
                let lcr = word_len | stop_bit | parity | brk;

                Self::uart_init(serial, port_base, divisor, lcr);

                // Return status
                let lsr = serial.read(port_base + 5).unwrap_or(0);
                let msr = serial.read(port_base + 6).unwrap_or(0);
                cpu.regs.set_ah(lsr);
                cpu.regs.set_al(msr);
            }

            _ => {
                tracing::warn!(
                    target: "gp64::hw::serial",
                    ah = format_args!("{:02X}", ah),
                    "Unhandled INT 14h function"
                );
                cpu.regs.set_ah(0x80);
            }
        }
        self.finish_int14(cpu, 0)
    }

    /// Complete an INT 14h subfunction: charge the polls it spent, drop any
    /// saved wait progress, and clear CF.
    fn finish_int14(&mut self, cpu: &mut Cpu, polls: u32) -> Result<Int14Disp> {
        self.serial_wait = None;
        Self::charge_int14_polls(cpu, polls);
        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
        Ok(Int14Disp::Iret)
    }

    /// Suspend an INT 14h wait: save its progress and charge the polls spent.
    ///
    /// Deliberately leaves AH/AL and CF alone — the guest must not observe a
    /// half-finished result. It never sees this return at all, because the
    /// caller rewinds CS:IP onto the `INT 14h` instruction.
    fn yield_int14(&mut self, cpu: &mut Cpu, wait: SerialWait, polls: u32) -> Result<Int14Disp> {
        self.serial_wait = Some(wait);
        Self::charge_int14_polls(cpu, polls);
        Ok(Int14Disp::Retry)
    }

    /// Advance guest time by the polls this dispatch performed, so a timeout
    /// wait costs the guest time instead of being free (MDD-BUG-KILN-00278).
    fn charge_int14_polls(cpu: &mut Cpu, polls: u32) {
        cpu.cycles = cpu
            .cycles
            .wrapping_add(u64::from(polls) * Self::INT14_CYCLES_PER_POLL);
    }

    /// INT 17h - Printer services (stub)
    ///
    /// All printer operations return "printer not ready" status.
    /// This prevents hangs in programs that probe for printers.
    pub fn int17(&self, cpu: &mut Cpu) -> Result<()> {
        let ah = cpu.regs.ah();
        let dx = cpu.regs.dx(); // Printer number (0=LPT1, 1=LPT2, etc.)

        tracing::debug!(
            ah = format_args!("{:02X}", ah),
            dx = format_args!("{:04X}", dx),
            "INT 17h (printer stub)"
        );

        // Printer status byte:
        // Bit 7: Not busy (0 = busy)
        // Bit 6: Acknowledge
        // Bit 5: Out of paper
        // Bit 4: Selected
        // Bit 3: I/O error
        // Bit 0: Timeout
        //
        // Return 0x01 (timeout) to indicate printer not available

        // All functions return timeout - no printer available
        if !matches!(ah, 0x00..=0x02) {
            tracing::warn!(ah = format_args!("{:02X}", ah), "Unhandled INT 17h");
        }
        cpu.regs.set_ah(0x01); // Timeout - no printer
        Ok(())
    }
}
