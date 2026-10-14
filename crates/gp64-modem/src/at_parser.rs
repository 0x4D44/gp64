//! Hayes-compatible AT command parser.
//!
//! Parses AT command strings, manages S-registers, formats result codes,
//! and detects the +++ escape sequence. Pure command interpretation — no
//! modem behavior or I/O.

use std::collections::VecDeque;

/// Command emitted by the parser for the modem to act on.
#[derive(Debug, Clone, PartialEq)]
pub enum AtCommand {
    Dial {
        digits: Vec<char>,
        tone: bool,
        /// `ATD...;` — dial the digits, then return to command mode with `OK`
        /// instead of proceeding into call setup (`MDD-BUG-CRUCIBLE-00516`).
        return_to_command: bool,
    },
    Answer,
    HangUp,
    Reset,
    FactoryDefaults,
    ReturnOnline,
    SetRegister {
        reg: u8,
        val: u8,
    },
    QueryRegister {
        reg: u8,
    },
    SetEcho(bool),
    SetVerbose(bool),
    SetQuiet(bool),
    SetSpeaker(u8),
    SetVolume(u8),
    SetFlowControl(u8),
    SetDsrMode(u8),
    SetExtendedResults(u8),
    Info(u8),
    ViewConfig,
}

/// Result code sent to the serial port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultCode {
    Ok,
    Connect,
    Ring,
    NoCarrier,
    Error,
    NoDialtone,
    Busy,
    NoAnswer,
    ConnectSpeed(u32),
}

impl ResultCode {
    /// Get the numeric code for this result.
    fn numeric(&self) -> u8 {
        match self {
            ResultCode::Ok => 0,
            ResultCode::Connect => 1,
            ResultCode::Ring => 2,
            ResultCode::NoCarrier => 3,
            ResultCode::Error => 4,
            ResultCode::NoDialtone => 6,
            ResultCode::Busy => 7,
            ResultCode::NoAnswer => 8,
            ResultCode::ConnectSpeed(speed) => match speed {
                1200 => 5,
                2400 => 10,
                4800 => 11,
                9600 => 12,
                14400 => 13,
                19200 => 14,
                28800 => 18,
                38400 => 28,
                56000 => 20,
                57600 => 86,
                115200 => 87,
                _ => 1, // Generic CONNECT
            },
        }
    }

    /// Get the verbose text for this result.
    fn verbose_text(&self) -> String {
        match self {
            ResultCode::Ok => "OK".to_string(),
            ResultCode::Connect => "CONNECT".to_string(),
            ResultCode::Ring => "RING".to_string(),
            ResultCode::NoCarrier => "NO CARRIER".to_string(),
            ResultCode::Error => "ERROR".to_string(),
            ResultCode::NoDialtone => "NO DIALTONE".to_string(),
            ResultCode::Busy => "BUSY".to_string(),
            ResultCode::NoAnswer => "NO ANSWER".to_string(),
            ResultCode::ConnectSpeed(speed) => format!("CONNECT {speed}"),
        }
    }
}

/// S-register index constants.
pub mod sregs {
    pub const S0: usize = 0;
    pub const S1: usize = 1;
    pub const S2: usize = 2;
    pub const S3: usize = 3;
    pub const S4: usize = 4;
    pub const S5: usize = 5;
    pub const S6: usize = 6;
    pub const S7: usize = 7;
    pub const S8: usize = 8;
    pub const S9: usize = 9;
    pub const S10: usize = 10;
    pub const S11: usize = 11;
    pub const S12: usize = 12;
}

/// Maximum accumulated AT command-line length. A real Hayes modem caps the
/// command line and discards the overflow; without this, a guest streaming
/// printable bytes in command mode with no terminating CR would grow
/// `command_buffer` without bound (MDD-BUG-ANVIL-00019). Generous (well above
/// any real init/dial string) so it never truncates a legitimate command.
const MAX_AT_COMMAND_LEN: usize = 256;

/// Upper bound on the DTE-bound TX queue (echo + result codes). Echo pushes a
/// byte per received byte at the top of `receive_byte` (before the command-line
/// cap), so a guest streaming bytes with echo on and not reading them back off
/// the serial port would otherwise grow `tx_queue` without bound
/// (MDD-BUG-ANVIL-00029, the echo-path sibling of ANVIL-00019). When full,
/// `enqueue_tx` drops the byte (a TX overrun) — generous so a real echo/result
/// stream never trips it.
const MAX_TX_QUEUE: usize = 1 << 16;

/// Hayes-compatible AT command parser.
///
/// Handles command line accumulation, parsing, S-register management,
/// result code formatting, echo, and escape sequence character detection.
/// Does NOT handle modem state, connections, UART, or timing.
pub struct AtParser {
    /// Command buffer (accumulating input)
    command_buffer: String,
    /// S-register array
    s_registers: [u8; 256],
    /// Echo mode (E1 = true)
    echo: bool,
    /// Verbose result codes (V1 = true)
    verbose: bool,
    /// Quiet mode (Q1 = true, suppresses result codes)
    quiet: bool,
    /// Extended result codes level (X0-X4)
    extended_results: u8,
    /// Speaker mode (M0-M3)
    speaker_mode: u8,
    /// Speaker volume (L0-L3)
    speaker_volume: u8,
    /// Flow control mode (0=none, 1=XON/XOFF, 2=RTS/CTS)
    flow_control: u8,
    /// DSR behavior (AT&S)
    dsr_mode: u8,
    /// Last executed command (for A/)
    last_command: String,
    /// Output queue (result codes, echo)
    tx_queue: VecDeque<u8>,
    /// Escape sequence: count of consecutive S2 chars seen
    escape_plus_count: u8,
}

impl Default for AtParser {
    fn default() -> Self {
        Self::new()
    }
}

impl AtParser {
    /// Create a new AT parser with default settings.
    pub fn new() -> Self {
        let mut parser = Self {
            command_buffer: String::new(),
            s_registers: [0; 256],
            echo: true,
            verbose: true,
            quiet: false,
            extended_results: 0,
            speaker_mode: 1,
            speaker_volume: 1,
            flow_control: 0,
            dsr_mode: 0,
            last_command: String::new(),
            tx_queue: VecDeque::new(),
            escape_plus_count: 0,
        };
        parser.init_s_register_defaults();
        parser
    }

    /// Feed a byte from serial. Returns commands to execute (if any).
    /// Handles echo, backspace, CR detection, and A/ detection.
    pub fn receive_byte(&mut self, byte: u8) -> Vec<AtCommand> {
        let cr = self.s_registers[sregs::S3];
        let lf = self.s_registers[sregs::S4];
        let bs = self.s_registers[sregs::S5];

        // Echo if enabled
        if self.echo {
            self.enqueue_tx(byte);
        }

        // CR: execute command
        if byte == cr {
            return self.execute_command();
        }

        // Backspace: remove last char
        if byte == bs {
            self.command_buffer.pop();
            return Vec::new();
        }

        // LF: ignore
        if byte == lf {
            return Vec::new();
        }

        // Other control characters: ignore
        if byte < 0x20 {
            return Vec::new();
        }

        // Accumulate printable characters (uppercased).
        // If 'A' arrives and the buffer contains non-AT prefix garbage
        // (e.g. "+++" from a guard-time escape sequence sent in command
        // mode), flush the buffer so "ATZ" parses correctly.
        let ch = (byte as char).to_ascii_uppercase();
        if ch == 'A' && !self.command_buffer.is_empty() && !self.command_buffer.starts_with('A') {
            self.command_buffer.clear();
        }
        // Cap the command line: a real Hayes modem discards bytes past the line
        // limit rather than buffering without bound. A guest streaming printable
        // bytes with no CR would otherwise grow this String indefinitely
        // (MDD-BUG-ANVIL-00019). CR/backspace/control are handled above, so the
        // guest can still terminate or edit the (capped) command.
        if self.command_buffer.len() >= MAX_AT_COMMAND_LEN {
            return Vec::new();
        }
        self.command_buffer.push(ch);

        // Detect A/ (repeat last command) — immediate, no CR needed
        if self.command_buffer == "A/" {
            self.command_buffer.clear();
            if self.last_command.is_empty() {
                self.send_result(ResultCode::Error);
                return Vec::new();
            }
            let cmd = self.last_command.clone();
            self.command_buffer = cmd;
            return self.execute_command();
        }

        Vec::new()
    }

    /// Queue a result code for serial output.
    ///
    /// The X register (extended result codes) affects `ConnectSpeed`:
    /// - **X0**: speed is omitted — `ConnectSpeed(n)` is sent as plain `Connect`
    /// - **X1+**: speed is included — `ConnectSpeed(4800)` → "CONNECT 4800"
    pub fn send_result(&mut self, code: ResultCode) {
        tracing::info!(
            target: "gp64::hw::serial",
            "ModemDsp result: {:?}",
            code
        );
        if self.quiet {
            return;
        }

        // X0: ConnectSpeed downgrades to plain Connect (no speed number)
        let code = if self.extended_results == 0 {
            match code {
                ResultCode::ConnectSpeed(_) => ResultCode::Connect,
                other => other,
            }
        } else {
            code
        };

        let cr = self.s_registers[sregs::S3];
        let lf = self.s_registers[sregs::S4];

        if self.verbose {
            // Verbose: \r\n{text}\r\n
            self.enqueue_tx(cr);
            self.enqueue_tx(lf);
            for b in code.verbose_text().bytes() {
                self.enqueue_tx(b);
            }
            self.enqueue_tx(cr);
            self.enqueue_tx(lf);
        } else {
            // Numeric: {number}\r
            let num = code.numeric();
            for b in num.to_string().bytes() {
                self.enqueue_tx(b);
            }
            self.enqueue_tx(cr);
        }
    }

    /// Enqueue a byte for serial output, bounded at `MAX_TX_QUEUE`. All
    /// `tx_queue` producers (echo + result codes) route through this so a guest
    /// that never drains the echo cannot grow the queue without bound; when full
    /// the byte is dropped (a TX overrun). MDD-BUG-ANVIL-00029.
    fn enqueue_tx(&mut self, byte: u8) {
        if self.tx_queue.len() >= MAX_TX_QUEUE {
            return;
        }
        self.tx_queue.push_back(byte);
    }

    /// Read next byte for serial output (echo, result codes).
    pub fn send_to_serial(&mut self) -> Option<u8> {
        self.tx_queue.pop_front()
    }

    /// Whether there's data to send.
    pub fn has_data_for_serial(&self) -> bool {
        !self.tx_queue.is_empty()
    }

    /// Feed a data-mode byte for escape sequence detection.
    /// Returns true if +++ escape pattern is complete (3 consecutive S2 chars).
    /// The caller is responsible for guard time checking.
    pub fn feed_data_byte(&mut self, byte: u8) -> bool {
        let escape_char = self.s_registers[sregs::S2];
        if byte == escape_char {
            self.escape_plus_count += 1;
            if self.escape_plus_count >= 3 {
                return true;
            }
        } else {
            self.escape_plus_count = 0;
        }
        false
    }

    /// Reset escape sequence detection state.
    pub fn reset_escape(&mut self) {
        self.escape_plus_count = 0;
    }

    /// S-register read access.
    pub fn s_register(&self, reg: u8) -> u8 {
        self.s_registers[reg as usize]
    }

    /// S-register write access.
    pub fn set_s_register(&mut self, reg: u8, val: u8) {
        self.s_registers[reg as usize] = val;
    }

    /// Whether echo is enabled.
    pub fn echo_enabled(&self) -> bool {
        self.echo
    }

    /// Whether verbose result codes are enabled.
    pub fn verbose_enabled(&self) -> bool {
        self.verbose
    }

    /// Whether quiet mode is enabled.
    pub fn quiet_enabled(&self) -> bool {
        self.quiet
    }

    /// Current speaker mode.
    pub fn speaker_mode(&self) -> u8 {
        self.speaker_mode
    }

    /// Current speaker volume.
    pub fn speaker_volume(&self) -> u8 {
        self.speaker_volume
    }

    /// Current flow control mode.
    pub fn flow_control(&self) -> u8 {
        self.flow_control
    }

    /// Current DSR behavior mode.
    pub fn dsr_mode(&self) -> u8 {
        self.dsr_mode
    }

    /// Reset to factory defaults.
    pub fn reset_to_defaults(&mut self) {
        self.init_s_register_defaults();
        self.echo = true;
        self.verbose = true;
        self.quiet = false;
        self.extended_results = 0;
        self.speaker_mode = 1;
        self.speaker_volume = 1;
        self.flow_control = 0;
        self.dsr_mode = 0;
    }

    // ---- Private helpers ----

    /// Set S-register defaults.
    fn init_s_register_defaults(&mut self) {
        self.s_registers = [0; 256];
        self.s_registers[sregs::S0] = 0;
        self.s_registers[sregs::S1] = 0;
        self.s_registers[sregs::S2] = 43; // '+'
        self.s_registers[sregs::S3] = 13; // CR
        self.s_registers[sregs::S4] = 10; // LF
        self.s_registers[sregs::S5] = 8; // BS
        self.s_registers[sregs::S6] = 2;
        self.s_registers[sregs::S7] = 50;
        self.s_registers[sregs::S8] = 2;
        self.s_registers[sregs::S9] = 6;
        self.s_registers[sregs::S10] = 14;
        self.s_registers[sregs::S11] = 95;
        self.s_registers[sregs::S12] = 50;
    }

    /// Execute the accumulated command buffer.
    fn execute_command(&mut self) -> Vec<AtCommand> {
        let command = std::mem::take(&mut self.command_buffer);
        let command = command.trim().to_string();

        // Empty command — no result, no commands
        if command.is_empty() {
            return Vec::new();
        }
        tracing::info!(target: "gp64::hw::serial", "ModemDsp command: {command}");

        // Must start with "AT" (case-insensitive, but we uppercased on input)
        if !command.starts_with("AT") {
            self.send_result(ResultCode::Error);
            return Vec::new();
        }

        // Store as last command for A/
        self.last_command = command.clone();

        let remainder = &command[2..];
        if remainder.is_empty() {
            // Bare "AT" — OK, no commands
            self.send_result(ResultCode::Ok);
            return Vec::new();
        }

        match self.parse_commands(remainder) {
            Some(commands) => {
                let has_state_machine_result = commands.iter().any(|c| {
                    matches!(
                        c,
                        AtCommand::Dial { .. } | AtCommand::Answer | AtCommand::ReturnOnline
                    )
                });
                if !has_state_machine_result {
                    self.send_result(ResultCode::Ok);
                }
                commands
            }
            None => {
                self.send_result(ResultCode::Error);
                Vec::new()
            }
        }
    }

    /// Parse AT command remainder into a list of commands.
    /// Returns None on parse error.
    fn parse_commands(&mut self, commands: &str) -> Option<Vec<AtCommand>> {
        let mut result = Vec::new();
        let mut chars = commands.chars().peekable();

        while let Some(cmd) = chars.next() {
            match cmd {
                'A' => {
                    result.push(AtCommand::Answer);
                }
                'D' => {
                    // Dial — consumes rest of line
                    let dial_str: String = chars.collect();
                    let (digits, tone, return_to_command) = Self::parse_dial_string(&dial_str);
                    result.push(AtCommand::Dial {
                        digits,
                        tone,
                        return_to_command,
                    });
                    return Some(result);
                }
                'E' => {
                    let n = Self::parse_digit(&mut chars, 1);
                    self.echo = n != 0;
                    result.push(AtCommand::SetEcho(n != 0));
                }
                'H' => {
                    let n = Self::parse_digit(&mut chars, 0);
                    if n == 0 {
                        result.push(AtCommand::HangUp);
                    }
                }
                'I' => {
                    let n = Self::parse_digit(&mut chars, 0);
                    self.send_info(n);
                    result.push(AtCommand::Info(n));
                }
                'L' => {
                    let n = Self::parse_digit(&mut chars, 1);
                    self.speaker_volume = n;
                    result.push(AtCommand::SetVolume(n));
                }
                'M' => {
                    let n = Self::parse_digit(&mut chars, 1);
                    self.speaker_mode = n;
                    result.push(AtCommand::SetSpeaker(n));
                }
                'O' => {
                    let _n = Self::parse_digit(&mut chars, 0);
                    result.push(AtCommand::ReturnOnline);
                    // A successful return-online command changes the parser's
                    // consumer from command mode to data mode. Remaining bytes
                    // on this command line are therefore not AT commands.
                    return Some(result);
                }
                'Q' => {
                    let n = Self::parse_digit(&mut chars, 0);
                    self.quiet = n != 0;
                    result.push(AtCommand::SetQuiet(n != 0));
                }
                'S' => match self.parse_s_register(&mut chars) {
                    Some(Some(cmd)) => result.push(cmd),
                    Some(None) => {}
                    None => return None,
                },
                'V' => {
                    let n = Self::parse_digit(&mut chars, 1);
                    self.verbose = n != 0;
                    result.push(AtCommand::SetVerbose(n != 0));
                }
                'W' => {
                    // No-op, consume digit
                    let _n = Self::parse_digit(&mut chars, 0);
                }
                'X' => {
                    let n = Self::parse_digit(&mut chars, 4);
                    self.extended_results = n;
                    result.push(AtCommand::SetExtendedResults(n));
                }
                'Z' => {
                    let _n = Self::parse_digit(&mut chars, 0);
                    self.reset_to_defaults();
                    result.push(AtCommand::Reset);
                    // Reset terminates parsing — remaining chars ignored
                    return Some(result);
                }
                '&' => {
                    let sub = chars.next()?;
                    if let Some(command) = self.parse_ampersand(sub, &mut chars)? {
                        result.push(command);
                    }
                }
                '\\' => {
                    // No-op: consume sub-char + optional digit
                    if let Some(_sub) = chars.next() {
                        let _n = Self::parse_digit(&mut chars, 0);
                    }
                }
                '%' => {
                    // No-op: consume sub-char + optional digit
                    if let Some(_sub) = chars.next() {
                        let _n = Self::parse_digit(&mut chars, 0);
                    }
                }
                '+' | '-' => {
                    // Extended AT commands (e.g., AT+MS=V90,1 or AT-V90=1).
                    // Accept as no-ops to allow V.90-capable software to proceed.
                    // Consume all remaining characters on this command prefix.
                    for c in chars.by_ref() {
                        if c == ';' {
                            break; // semicolon separates commands in extended syntax
                        }
                    }
                }
                ' ' => {
                    // Ignore spaces
                }
                _ => {
                    // Unknown command
                    return None;
                }
            }
        }

        Some(result)
    }

    /// Parse a single digit from the char stream. Returns default if no digit.
    fn parse_digit(chars: &mut std::iter::Peekable<std::str::Chars>, default: u8) -> u8 {
        if let Some(&c) = chars.peek() {
            if c.is_ascii_digit() {
                chars.next();
                return (c as u8) - b'0';
            }
        }
        default
    }

    /// Parse a multi-digit number from the char stream. Returns default if no digits.
    fn parse_number(chars: &mut std::iter::Peekable<std::str::Chars>, default: u16) -> u16 {
        let mut value: u16 = 0;
        let mut found = false;
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() {
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

    /// Parse a dial string into (digits, tone_mode, return_to_command).
    fn parse_dial_string(dial_str: &str) -> (Vec<char>, bool, bool) {
        let mut digits = Vec::new();
        let mut tone = true; // Default: tone dialing
        let mut return_to_command = false;
        let mut chars = dial_str.chars().peekable();

        // Check for T/P prefix
        if let Some(&c) = chars.peek() {
            match c.to_ascii_uppercase() {
                'T' => {
                    tone = true;
                    chars.next();
                }
                'P' => {
                    tone = false;
                    chars.next();
                }
                _ => {}
            }
        }

        for c in chars {
            match c.to_ascii_uppercase() {
                '0'..='9' | '*' | '#' => digits.push(c),
                'A'..='D' => digits.push(c.to_ascii_uppercase()),
                '-' | '(' | ')' | ' ' | ',' => {} // Formatting/pause, ignored
                ';' => {
                    // Dial the digits, then hand control back to the caller.
                    return_to_command = true;
                    break;
                }
                _ => {} // Unknown, skip
            }
        }

        (digits, tone, return_to_command)
    }

    /// Parse S-register command (Sn? or Sn=v).
    fn parse_s_register(
        &mut self,
        chars: &mut std::iter::Peekable<std::str::Chars>,
    ) -> Option<Option<AtCommand>> {
        let reg = Self::parse_number(chars, 0);
        if reg > 255 {
            return None;
        }
        let reg = reg as u8;

        match chars.peek() {
            Some(&'?') => {
                chars.next();
                // Query: send the value as a response string
                let val = self.s_registers[reg as usize];
                self.send_string(&format!("{val:03}"));
                Some(Some(AtCommand::QueryRegister { reg }))
            }
            Some(&'=') => {
                chars.next();
                let val = Self::parse_number(chars, 0);
                if val > 255 {
                    return None;
                }
                let val = val as u8;
                self.s_registers[reg as usize] = val;
                Some(Some(AtCommand::SetRegister { reg, val }))
            }
            _ => {
                // Just selecting register — no command emitted but not an error
                // This is valid per Hayes spec (selects for subsequent ? or =)
                Some(None)
            }
        }
    }

    /// Parse AT& extended command.
    /// Returns Some(Some(cmd)) for commands that produce output,
    /// Some(None) for no-ops, None for errors.
    fn parse_ampersand(
        &mut self,
        sub: char,
        chars: &mut std::iter::Peekable<std::str::Chars>,
    ) -> Option<Option<AtCommand>> {
        match sub {
            'C' | 'D' | 'Q' | 'W' => {
                // No-op commands that consume a digit
                let _n = Self::parse_digit(chars, 0);
                Some(None)
            }
            'F' => {
                self.reset_to_defaults();
                Some(Some(AtCommand::FactoryDefaults))
            }
            'K' => {
                let n = Self::parse_digit(chars, 3);
                self.flow_control = n;
                Some(Some(AtCommand::SetFlowControl(n)))
            }
            'S' => {
                let n = Self::parse_digit(chars, 0);
                self.dsr_mode = n;
                Some(Some(AtCommand::SetDsrMode(n)))
            }
            'V' => {
                self.send_view_config();
                Some(Some(AtCommand::ViewConfig))
            }
            _ => {
                // Unknown & command — error
                None
            }
        }
    }

    /// Send a string to the output queue (with CR/LF framing).
    fn send_string(&mut self, s: &str) {
        let cr = self.s_registers[sregs::S3];
        let lf = self.s_registers[sregs::S4];
        self.enqueue_tx(cr);
        self.enqueue_tx(lf);
        for b in s.bytes() {
            self.enqueue_tx(b);
        }
        self.enqueue_tx(cr);
        self.enqueue_tx(lf);
    }

    /// Send the AT&V view-config response.
    fn send_view_config(&mut self) {
        let config = format!(
            "E{} L{} M{} Q{} V{} X{} &K{}",
            if self.echo { 1 } else { 0 },
            self.speaker_volume,
            self.speaker_mode,
            if self.quiet { 1 } else { 0 },
            if self.verbose { 1 } else { 0 },
            self.extended_results,
            self.flow_control,
        );
        self.send_string(&config);
    }

    /// Send the modem identification text for an ATI query.
    ///
    /// The parser also owns the terminal `OK` result for ordinary commands,
    /// so queuing this text here preserves the wire order: information first,
    /// then the final result code.
    fn send_info(&mut self, level: u8) {
        let info = match level {
            0 => "GP64 Virtual Modem",
            1 => "V1.0",
            2 => "OK",
            3 => "GP64 Virtual Modem V1.0",
            4 => "Hayes Compatible",
            _ => return,
        };
        self.send_string(info);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // ---- Helpers ----

    /// Send a string (without CR) then send CR to execute.
    fn send_command(parser: &mut AtParser, cmd: &str) -> Vec<AtCommand> {
        let mut all_cmds = Vec::new();
        for byte in cmd.bytes() {
            all_cmds.extend(parser.receive_byte(byte));
        }
        all_cmds.extend(parser.receive_byte(13)); // CR
        all_cmds
    }

    /// Drain all pending output bytes into a string.
    fn collect_output(parser: &mut AtParser) -> String {
        let mut bytes = Vec::new();
        while let Some(b) = parser.send_to_serial() {
            bytes.push(b);
        }
        String::from_utf8_lossy(&bytes).to_string()
    }

    /// Drain output bytes (raw).
    fn collect_output_bytes(parser: &mut AtParser) -> Vec<u8> {
        let mut bytes = Vec::new();
        while let Some(b) = parser.send_to_serial() {
            bytes.push(b);
        }
        bytes
    }

    // Regression for MDD-BUG-ANVIL-00019: a guest streaming printable bytes in
    // command mode with no CR must not grow command_buffer without bound; the
    // real Hayes modem discards bytes past the command-line limit.
    #[test]
    fn command_buffer_is_length_capped() {
        let mut p = AtParser::new();
        // Simulate the buffer already at the cap (a long no-CR printable stream).
        p.command_buffer = "X".repeat(MAX_AT_COMMAND_LEN);
        // One more printable byte must be discarded, not appended.
        let _ = p.receive_byte(b'X');
        assert_eq!(
            p.command_buffer.len(),
            MAX_AT_COMMAND_LEN,
            "command_buffer must stay capped at MAX_AT_COMMAND_LEN"
        );
    }

    // Regression for MDD-BUG-ANVIL-00029: the DTE-bound echo/result tx_queue must
    // not grow without bound (echo pushes a byte per received byte, before the
    // command-line cap). enqueue_tx (the single tx_queue producer) drops when full.
    #[test]
    fn tx_queue_is_length_capped() {
        let mut p = AtParser::new();
        // Below the cap: the byte is enqueued (also guards a broken helper).
        p.enqueue_tx(b'A');
        assert_eq!(p.tx_queue.len(), 1, "below-cap byte must be enqueued");
        // At the cap: one more byte is dropped, not appended.
        p.tx_queue = vec![0u8; MAX_TX_QUEUE].into();
        p.enqueue_tx(b'B');
        assert_eq!(
            p.tx_queue.len(),
            MAX_TX_QUEUE,
            "tx_queue must stay capped at MAX_TX_QUEUE"
        );
    }

    // ---- 1. test_new_defaults ----

    #[test]
    fn test_new_defaults() {
        let p = AtParser::new();
        assert!(p.echo_enabled());
        assert!(p.verbose_enabled());
        assert!(!p.quiet_enabled());
        assert_eq!(p.speaker_mode(), 1);
        assert_eq!(p.speaker_volume(), 1);
        assert_eq!(p.flow_control(), 0);
        assert_eq!(p.dsr_mode(), 0);

        // S-register defaults
        assert_eq!(p.s_register(0), 0);
        assert_eq!(p.s_register(1), 0);
        assert_eq!(p.s_register(2), 43); // '+'
        assert_eq!(p.s_register(3), 13); // CR
        assert_eq!(p.s_register(4), 10); // LF
        assert_eq!(p.s_register(5), 8); // BS
        assert_eq!(p.s_register(6), 2);
        assert_eq!(p.s_register(7), 50);
        assert_eq!(p.s_register(8), 2);
        assert_eq!(p.s_register(9), 6);
        assert_eq!(p.s_register(10), 14);
        assert_eq!(p.s_register(11), 95);
        assert_eq!(p.s_register(12), 50);
    }

    // ---- 2. test_bare_at_returns_ok ----

    #[test]
    fn test_bare_at_returns_ok() {
        let mut p = AtParser::new();
        let cmds = send_command(&mut p, "AT");
        assert!(cmds.is_empty(), "Bare AT should produce no commands");
        let output = collect_output(&mut p);
        assert!(output.contains("OK"), "Bare AT should produce OK result");
    }

    // ---- 3. test_echo_on ----

    #[test]
    fn test_echo_on() {
        let mut p = AtParser::new();
        // Echo is on by default. Send "AT\r" and check that 'A', 'T', '\r' are echoed.
        for byte in b"AT" {
            p.receive_byte(*byte);
        }
        p.receive_byte(13);
        let output = collect_output_bytes(&mut p);
        // First 3 bytes should be the echoed A, T, CR
        assert!(output.len() >= 3);
        assert_eq!(output[0], b'A');
        assert_eq!(output[1], b'T');
        assert_eq!(output[2], 13);
    }

    // ---- 4. test_echo_off ----

    #[test]
    fn test_echo_off() {
        let mut p = AtParser::new();
        // Turn echo off
        let _ = send_command(&mut p, "ATE0");
        let _ = collect_output(&mut p);

        assert!(!p.echo_enabled());

        // Now send "AT\r" — should NOT see echoed bytes, only the result code
        let _ = send_command(&mut p, "AT");
        let output = collect_output_bytes(&mut p);
        // Output should NOT start with 'A' 'T' (no echo)
        // It should be "\r\nOK\r\n"
        assert_eq!(output, b"\r\nOK\r\n");
    }

    // ---- 5. test_dial_command ----

    /// MDD-BUG-CRUCIBLE-00516: `;` is a dial-string modifier, not just a
    /// terminator.
    ///
    /// The parser recognised it only as "stop scanning", so `AtCommand::Dial`
    /// carried nothing to distinguish `ATDT123;` from `ATDT123` and the DSP
    /// state machine dialled a call either way. The other Hayes path in the
    /// repository (`gp64-hw/src/serial/modem.rs`) has preserved this
    /// modifier all along, which is the behaviour matched here.
    #[test]
    fn dial_semicolon_is_carried_as_return_to_command() {
        let mut parser = AtParser::new();
        parser.echo = false;

        for (input, want_digits, want_return) in [
            (
                "ATDT5551234;",
                vec!['5', '5', '5', '1', '2', '3', '4'],
                true,
            ),
            (
                "ATDT5551234",
                vec!['5', '5', '5', '1', '2', '3', '4'],
                false,
            ),
            // Digits after the semicolon are not dialled — it ends the string.
            ("ATDT12;34", vec!['1', '2'], true),
            ("ATDP99;", vec!['9', '9'], true),
        ] {
            let cmds = send_command(&mut parser, input);
            assert_eq!(cmds.len(), 1, "{input}");
            match &cmds[0] {
                AtCommand::Dial {
                    digits,
                    return_to_command,
                    ..
                } => {
                    assert_eq!(digits, &want_digits, "{input}: digits");
                    assert_eq!(
                        *return_to_command, want_return,
                        "{input}: return-to-command flag"
                    );
                }
                other => panic!("{input}: expected a Dial command, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_dial_command() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "ATD5551234");
        assert_eq!(cmds.len(), 1);
        assert_eq!(
            cmds[0],
            AtCommand::Dial {
                digits: vec!['5', '5', '5', '1', '2', '3', '4'],
                tone: true, // Default
                return_to_command: false,
            }
        );
    }

    // ---- 6. test_dial_tone_prefix ----

    #[test]
    fn test_dial_tone_prefix() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "ATDT5551234");
        assert_eq!(cmds.len(), 1);
        match &cmds[0] {
            AtCommand::Dial { digits, tone, .. } => {
                assert!(tone);
                assert_eq!(digits, &vec!['5', '5', '5', '1', '2', '3', '4']);
            }
            other => panic!("Expected Dial, got {other:?}"),
        }
    }

    // ---- 7. test_dial_pulse_prefix ----

    #[test]
    fn test_dial_pulse_prefix() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "ATDP5551234");
        assert_eq!(cmds.len(), 1);
        match &cmds[0] {
            AtCommand::Dial { digits, tone, .. } => {
                assert!(!tone);
                assert_eq!(digits, &vec!['5', '5', '5', '1', '2', '3', '4']);
            }
            other => panic!("Expected Dial, got {other:?}"),
        }
    }

    // ---- 8. test_dial_dtmf_extensions ----

    #[test]
    fn test_dial_dtmf_extensions() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "ATDT123ABCD");
        assert_eq!(cmds.len(), 1);
        match &cmds[0] {
            AtCommand::Dial { digits, tone, .. } => {
                assert!(tone);
                assert_eq!(digits, &vec!['1', '2', '3', 'A', 'B', 'C', 'D']);
            }
            other => panic!("Expected Dial, got {other:?}"),
        }
    }

    // ---- 9. test_dial_formatting_ignored ----

    #[test]
    fn test_dial_formatting_ignored() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "ATDT(555) 123-4567");
        assert_eq!(cmds.len(), 1);
        match &cmds[0] {
            AtCommand::Dial { digits, .. } => {
                assert_eq!(
                    digits,
                    &vec!['5', '5', '5', '1', '2', '3', '4', '5', '6', '7']
                );
            }
            other => panic!("Expected Dial, got {other:?}"),
        }
    }

    // ---- 10. test_answer_command ----

    #[test]
    fn test_answer_command() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "ATA");
        assert!(cmds.contains(&AtCommand::Answer));
    }

    // ---- 11. test_hangup_command ----

    #[test]
    fn test_hangup_command() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "ATH");
        assert!(cmds.contains(&AtCommand::HangUp));
        let _ = collect_output(&mut p);

        let cmds = send_command(&mut p, "ATH0");
        assert!(cmds.contains(&AtCommand::HangUp));
        let _ = collect_output(&mut p);

        let cmds = send_command(&mut p, "ATH1");
        assert!(!cmds.contains(&AtCommand::HangUp));
        assert!(collect_output(&mut p).contains("OK"));
    }

    // ---- 12. test_reset_command ----

    #[test]
    fn test_reset_command() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "ATZ");
        assert!(cmds.contains(&AtCommand::Reset));
    }

    // ---- 13. test_s_register_query ----

    #[test]
    fn test_s_register_query() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "ATS0?");
        assert!(cmds.contains(&AtCommand::QueryRegister { reg: 0 }));
        // The value should be sent as a formatted string
        let output = collect_output(&mut p);
        assert!(output.contains("000"), "S0 default is 0, formatted as 000");
    }

    // ---- 14. test_s_register_set ----

    #[test]
    fn test_s_register_set() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "ATS7=30");
        assert!(cmds.contains(&AtCommand::SetRegister { reg: 7, val: 30 }));
        assert_eq!(p.s_register(7), 30);
    }

    #[test]
    fn test_bare_s_register_selection_and_invalid_register_crucible_00692() {
        let mut p = AtParser::new();
        p.echo = false;

        let cmds = send_command(&mut p, "ATS5");
        assert!(cmds.is_empty(), "Bare S-register selection is a no-op");
        let output = collect_output(&mut p);
        assert!(
            output.contains("OK"),
            "Valid selection should return OK: {output}"
        );
        assert!(
            !output.contains("ERROR"),
            "Valid selection must not fail: {output}"
        );

        let cmds = send_command(&mut p, "ATS256");
        assert!(
            cmds.is_empty(),
            "Invalid register should produce no command"
        );
        let output = collect_output(&mut p);
        assert!(
            output.contains("ERROR"),
            "Out-of-range register should fail: {output}"
        );
    }

    // ---- 15. test_multiple_commands ----

    #[test]
    fn test_multiple_commands() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "ATE0V0");
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0], AtCommand::SetEcho(false));
        assert_eq!(cmds[1], AtCommand::SetVerbose(false));
    }

    // ---- 16. test_verbose_result ----

    #[test]
    fn test_verbose_result() {
        let mut p = AtParser::new();
        p.echo = false;
        // Verbose is on by default
        p.send_result(ResultCode::Ok);
        let output = collect_output_bytes(&mut p);
        assert_eq!(output, b"\r\nOK\r\n");
    }

    // ---- 17. test_numeric_result ----

    #[test]
    fn test_numeric_result() {
        let mut p = AtParser::new();
        p.echo = false;
        p.verbose = false;
        p.send_result(ResultCode::Ok);
        let output = collect_output_bytes(&mut p);
        // Numeric OK = "0\r"
        assert_eq!(output, b"0\r");
    }

    // ---- 18. test_quiet_suppresses ----

    #[test]
    fn test_quiet_suppresses() {
        let mut p = AtParser::new();
        p.echo = false;
        p.quiet = true;
        p.send_result(ResultCode::Ok);
        assert!(
            !p.has_data_for_serial(),
            "Quiet mode should suppress output"
        );
    }

    // ---- 19. test_a_slash_repeats ----

    #[test]
    fn test_a_slash_repeats() {
        let mut p = AtParser::new();
        p.echo = false;

        // First: run ATH which sets last_command
        let cmds1 = send_command(&mut p, "ATH");
        assert!(cmds1.contains(&AtCommand::HangUp));
        let _ = collect_output(&mut p);

        // Now A/ (no CR needed) should repeat ATH
        let mut cmds2 = Vec::new();
        for byte in b"A/" {
            cmds2.extend(p.receive_byte(*byte));
        }
        assert!(
            cmds2.contains(&AtCommand::HangUp),
            "A/ should repeat last command"
        );
    }

    // ---- 20. test_backspace_deletes ----

    #[test]
    fn test_backspace_deletes() {
        let mut p = AtParser::new();
        p.echo = false;

        // Type "ATQ1", then backspace, then "0", then CR
        // Should be "ATQ0" → SetQuiet(false)
        for byte in b"ATQ1" {
            p.receive_byte(*byte);
        }
        p.receive_byte(8); // BS
        p.receive_byte(b'0');
        let cmds = p.receive_byte(13); // CR
        assert!(
            cmds.contains(&AtCommand::SetQuiet(false)),
            "Backspace should have deleted '1', resulting in Q0: {cmds:?}"
        );
    }

    // ---- 21. test_escape_detection ----

    #[test]
    fn test_escape_detection() {
        let mut p = AtParser::new();
        // S2 default = 43 ('+')
        assert!(!p.feed_data_byte(b'+'));
        assert!(!p.feed_data_byte(b'+'));
        assert!(p.feed_data_byte(b'+'), "Third '+' should trigger escape");
    }

    // ---- 22. test_factory_defaults ----

    #[test]
    fn test_factory_defaults() {
        let mut p = AtParser::new();
        p.echo = false;

        // Change some settings
        let _ = send_command(&mut p, "ATE0V0Q1");
        let _ = collect_output(&mut p);
        assert!(!p.echo_enabled());
        assert!(!p.verbose_enabled());
        assert!(p.quiet_enabled());

        // Factory reset
        let cmds = send_command(&mut p, "AT&F");
        assert!(cmds.contains(&AtCommand::FactoryDefaults));

        assert!(p.echo_enabled());
        assert!(p.verbose_enabled());
        assert!(!p.quiet_enabled());
        assert_eq!(p.s_register(2), 43);
    }

    // ---- 23. test_info_command ----

    #[test]
    fn test_info_command() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "ATI0");
        assert!(cmds.contains(&AtCommand::Info(0)));
    }

    #[test]
    fn test_info_levels_return_identification_before_ok_crucible_00691() {
        for (level, expected) in [
            (0, "GP64 Virtual Modem"),
            (1, "V1.0"),
            (2, "OK"),
            (3, "GP64 Virtual Modem V1.0"),
            (4, "Hayes Compatible"),
        ] {
            let mut p = AtParser::new();
            p.echo = false;
            let commands = send_command(&mut p, &format!("ATI{level}"));
            assert_eq!(commands, vec![AtCommand::Info(level)]);

            let output = collect_output(&mut p);
            let info_pos = output
                .find(expected)
                .unwrap_or_else(|| panic!("ATI{level} response missing {expected:?}: {output:?}"));
            let result_pos = output
                .rfind("OK")
                .unwrap_or_else(|| panic!("ATI{level} terminal OK missing: {output:?}"));
            assert!(
                info_pos < result_pos,
                "ATI{level} must return identification before terminal OK: {output:?}"
            );
        }
    }

    // ---- 24. test_connect_speed_result ----

    #[test]
    fn test_connect_speed_result() {
        let mut p = AtParser::new();
        p.echo = false;
        p.extended_results = 1; // X1: include speed in CONNECT
                                // Verbose mode
        p.send_result(ResultCode::ConnectSpeed(9600));
        let output = collect_output(&mut p);
        assert!(
            output.contains("CONNECT 9600"),
            "Expected 'CONNECT 9600' in: {output}"
        );
    }

    // ---- 25. test_unknown_command_error ----

    #[test]
    fn test_unknown_command_error() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "AT!");
        assert!(
            cmds.is_empty(),
            "Unknown command should produce no commands"
        );
        let output = collect_output(&mut p);
        assert!(
            output.contains("ERROR"),
            "Unknown command should produce ERROR: {output}"
        );
    }

    // ---- Additional coverage tests ----

    #[test]
    fn test_escape_reset_on_non_escape() {
        let mut p = AtParser::new();
        assert!(!p.feed_data_byte(b'+'));
        assert!(!p.feed_data_byte(b'+'));
        // Non-escape char resets
        assert!(!p.feed_data_byte(b'x'));
        // Need 3 again
        assert!(!p.feed_data_byte(b'+'));
        assert!(!p.feed_data_byte(b'+'));
        assert!(p.feed_data_byte(b'+'));
    }

    #[test]
    fn test_reset_escape() {
        let mut p = AtParser::new();
        p.feed_data_byte(b'+');
        p.feed_data_byte(b'+');
        p.reset_escape();
        assert!(!p.feed_data_byte(b'+'));
    }

    #[test]
    fn test_lf_ignored_in_command() {
        let mut p = AtParser::new();
        p.echo = false;
        // LF should not be added to buffer
        for byte in b"AT" {
            p.receive_byte(*byte);
        }
        p.receive_byte(10); // LF
        let cmds = p.receive_byte(13); // CR
                                       // Should still parse as bare "AT"
        let output = collect_output(&mut p);
        assert!(cmds.is_empty());
        assert!(output.contains("OK"));
    }

    #[test]
    fn test_control_chars_ignored() {
        let mut p = AtParser::new();
        p.echo = false;
        for byte in b"AT" {
            p.receive_byte(*byte);
        }
        p.receive_byte(1); // SOH
        p.receive_byte(2); // STX
        let cmds = p.receive_byte(13);
        let output = collect_output(&mut p);
        assert!(cmds.is_empty());
        assert!(output.contains("OK"));
    }

    #[test]
    fn test_not_at_prefix_error() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "HELLO");
        assert!(cmds.is_empty());
        let output = collect_output(&mut p);
        assert!(output.contains("ERROR"));
    }

    #[test]
    fn test_return_online_command() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "ATO");
        assert!(cmds.contains(&AtCommand::ReturnOnline));
        assert!(
            collect_output(&mut p).is_empty(),
            "ATO result depends on the live carrier state"
        );
    }

    #[test]
    fn return_online_terminates_the_current_command_line() {
        for (line, expected) in [
            ("ATOH", vec![AtCommand::ReturnOnline]),
            ("ATO0H", vec![AtCommand::ReturnOnline]),
            (
                "ATM2O0H",
                vec![AtCommand::SetSpeaker(2), AtCommand::ReturnOnline],
            ),
        ] {
            let mut parser = AtParser::new();
            parser.echo = false;

            let commands = send_command(&mut parser, line);

            assert_eq!(commands, expected, "command line {line}");
            assert!(
                collect_output(&mut parser).is_empty(),
                "the state machine owns the ATO result for {line}"
            );
        }
    }

    #[test]
    fn test_set_speaker_and_volume() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "ATM2L3");
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0], AtCommand::SetSpeaker(2));
        assert_eq!(cmds[1], AtCommand::SetVolume(3));
        assert_eq!(p.speaker_mode(), 2);
        assert_eq!(p.speaker_volume(), 3);
    }

    #[test]
    fn test_extended_results() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "ATX3");
        assert!(cmds.contains(&AtCommand::SetExtendedResults(3)));
    }

    #[test]
    fn test_flow_control_command() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "AT&K2");
        assert!(cmds.contains(&AtCommand::SetFlowControl(2)));
        assert_eq!(p.flow_control(), 2);
    }

    #[test]
    fn test_dsr_mode_command() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "AT&S1");
        assert!(cmds.contains(&AtCommand::SetDsrMode(1)));
        assert_eq!(p.dsr_mode(), 1);
    }

    #[test]
    fn test_view_config_command() {
        let mut p = AtParser::new();
        // Don't disable echo, so E1 shows in config
        let cmds = send_command(&mut p, "AT&V");
        assert!(cmds.contains(&AtCommand::ViewConfig));
        let output = collect_output(&mut p);
        // Should contain config summary
        assert!(output.contains("E1"), "Config should show echo: {output}");
        assert!(
            output.contains("V1"),
            "Config should show verbose: {output}"
        );
    }

    #[test]
    fn test_s_register_access() {
        let mut p = AtParser::new();
        p.set_s_register(100, 42);
        assert_eq!(p.s_register(100), 42);
    }

    #[test]
    fn test_numeric_connect_speeds() {
        let mut p = AtParser::new();
        p.echo = false;
        p.verbose = false;
        p.extended_results = 1; // X1: speed-specific numeric codes

        // Test various connect speeds
        let cases = [
            (1200, 5),
            (2400, 10),
            (9600, 12),
            (14400, 13),
            (28800, 18),
            (115200, 87),
        ];
        for (speed, expected_code) in cases {
            p.send_result(ResultCode::ConnectSpeed(speed));
            let output = collect_output(&mut p);
            let expected = format!("{expected_code}\r");
            assert_eq!(
                output, expected,
                "ConnectSpeed({speed}) should produce numeric code {expected_code}"
            );
        }
    }

    #[test]
    fn test_verbose_result_codes() {
        let mut p = AtParser::new();
        p.echo = false;

        let cases = [
            (ResultCode::Ok, "OK"),
            (ResultCode::Connect, "CONNECT"),
            (ResultCode::Ring, "RING"),
            (ResultCode::NoCarrier, "NO CARRIER"),
            (ResultCode::Error, "ERROR"),
            (ResultCode::NoDialtone, "NO DIALTONE"),
            (ResultCode::Busy, "BUSY"),
            (ResultCode::NoAnswer, "NO ANSWER"),
        ];
        for (code, expected_text) in cases {
            p.send_result(code);
            let output = collect_output(&mut p);
            assert!(
                output.contains(expected_text),
                "Expected '{expected_text}' in verbose output: {output}"
            );
        }
    }

    #[test]
    fn test_a_slash_no_previous_command() {
        let mut p = AtParser::new();
        p.echo = false;

        // A/ with no previous command → ERROR
        let mut cmds = Vec::new();
        for byte in b"A/" {
            cmds.extend(p.receive_byte(*byte));
        }
        assert!(cmds.is_empty());
        let output = collect_output(&mut p);
        assert!(output.contains("ERROR"));
    }

    #[test]
    fn test_case_insensitive() {
        let mut p = AtParser::new();
        p.echo = false;
        // Lowercase input should work
        let cmds = send_command(&mut p, "ath");
        assert!(cmds.contains(&AtCommand::HangUp));
    }

    #[test]
    fn test_default_trait() {
        let p = AtParser::default();
        assert!(p.echo_enabled());
    }

    #[test]
    fn test_dial_with_comma_pause() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "ATDT555,1234");
        match &cmds[0] {
            AtCommand::Dial { digits, .. } => {
                // Comma should be stripped
                assert_eq!(digits, &vec!['5', '5', '5', '1', '2', '3', '4']);
            }
            other => panic!("Expected Dial, got {other:?}"),
        }
    }

    #[test]
    fn test_backslash_noop() {
        let mut p = AtParser::new();
        p.echo = false;
        // \N0 should be a no-op
        let cmds = send_command(&mut p, "AT\\N0");
        // No commands generated (backslash is no-op), but should not error
        assert!(cmds.is_empty());
        let output = collect_output(&mut p);
        assert!(output.contains("OK"));
    }

    #[test]
    fn test_percent_noop() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "AT%C0");
        assert!(cmds.is_empty());
        let output = collect_output(&mut p);
        assert!(output.contains("OK"));
    }

    #[test]
    fn test_w_noop() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "ATW0");
        assert!(cmds.is_empty());
        let output = collect_output(&mut p);
        assert!(output.contains("OK"));
    }

    #[test]
    fn test_spaces_ignored() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "AT H");
        assert!(cmds.contains(&AtCommand::HangUp));
    }

    #[test]
    fn test_connect_speed_verbose() {
        let mut p = AtParser::new();
        p.echo = false;
        p.extended_results = 1; // X1: include speed
        p.send_result(ResultCode::ConnectSpeed(14400));
        let output = collect_output(&mut p);
        assert!(output.contains("CONNECT 14400"));
    }

    #[test]
    fn test_connect_speed_unknown_fallback() {
        let mut p = AtParser::new();
        p.echo = false;
        p.verbose = false;
        p.extended_results = 1; // X1: speed-specific codes
                                // Unknown speed falls back to generic CONNECT (code 1)
        p.send_result(ResultCode::ConnectSpeed(7200));
        let output = collect_output(&mut p);
        assert_eq!(output, "1\r");
    }

    // ---- X register (extended result codes) tests ----

    #[test]
    fn test_connect_speed_x0_verbose_omits_speed() {
        let mut p = AtParser::new();
        p.echo = false;
        // Default is X0
        assert_eq!(p.extended_results, 0);
        p.send_result(ResultCode::ConnectSpeed(4800));
        let output = collect_output_bytes(&mut p);
        // X0: just "CONNECT", no speed number
        assert_eq!(output, b"\r\nCONNECT\r\n");
    }

    #[test]
    fn test_connect_speed_x1_verbose_includes_speed() {
        let mut p = AtParser::new();
        p.echo = false;
        p.extended_results = 1;
        p.send_result(ResultCode::ConnectSpeed(4800));
        let output = collect_output_bytes(&mut p);
        // X1+: "CONNECT 4800" with speed
        assert_eq!(output, b"\r\nCONNECT 4800\r\n");
    }

    #[test]
    fn test_connect_speed_x0_numeric_uses_generic_code() {
        let mut p = AtParser::new();
        p.echo = false;
        p.verbose = false;
        // X0: numeric CONNECT is always code 1 (generic), regardless of speed
        assert_eq!(p.extended_results, 0);
        p.send_result(ResultCode::ConnectSpeed(9600));
        let output = collect_output_bytes(&mut p);
        assert_eq!(output, b"1\r");
    }

    #[test]
    fn test_connect_speed_x1_numeric_uses_speed_code() {
        let mut p = AtParser::new();
        p.echo = false;
        p.verbose = false;
        p.extended_results = 1;
        // X1+: numeric code is speed-specific (9600 = 12)
        p.send_result(ResultCode::ConnectSpeed(9600));
        let output = collect_output_bytes(&mut p);
        assert_eq!(output, b"12\r");
    }

    // ---- Additional AT parser edge case tests ----

    #[test]
    fn test_reset_restores_echo() {
        let mut p = AtParser::new();
        p.echo = false;
        let _ = send_command(&mut p, "ATE0");
        let _ = collect_output(&mut p);
        assert!(!p.echo_enabled());

        let cmds = send_command(&mut p, "ATZ");
        assert!(cmds.contains(&AtCommand::Reset));
        assert!(p.echo_enabled());
        assert!(p.verbose_enabled());
    }

    #[test]
    fn test_lowercase_input_uppercased() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "ath");
        assert!(cmds.contains(&AtCommand::HangUp));
    }

    #[test]
    fn test_dial_with_semicolon_return_to_command() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "ATDT123;");
        assert_eq!(cmds.len(), 1);
        match &cmds[0] {
            AtCommand::Dial { digits, tone, .. } => {
                assert!(tone);
                assert_eq!(digits, &vec!['1', '2', '3']);
            }
            other => panic!("Expected Dial, got {other:?}"),
        }
    }

    #[test]
    fn test_dial_with_star_and_hash() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "ATDT*67#5551234");
        match &cmds[0] {
            AtCommand::Dial { digits, .. } => {
                assert!(digits.contains(&'*'));
                assert!(digits.contains(&'#'));
            }
            other => panic!("Expected Dial, got {other:?}"),
        }
    }

    #[test]
    fn test_empty_dial_string() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "ATD");
        assert_eq!(cmds.len(), 1);
        match &cmds[0] {
            AtCommand::Dial { digits, .. } => {
                assert!(digits.is_empty());
            }
            other => panic!("Expected Dial, got {other:?}"),
        }
    }

    #[test]
    fn test_result_code_numeric_values() {
        // Verify the numeric codes for all standard result codes
        assert_eq!(ResultCode::Ok.numeric(), 0);
        assert_eq!(ResultCode::Connect.numeric(), 1);
        assert_eq!(ResultCode::Ring.numeric(), 2);
        assert_eq!(ResultCode::NoCarrier.numeric(), 3);
        assert_eq!(ResultCode::Error.numeric(), 4);
        assert_eq!(ResultCode::NoDialtone.numeric(), 6);
        assert_eq!(ResultCode::Busy.numeric(), 7);
        assert_eq!(ResultCode::NoAnswer.numeric(), 8);
    }

    #[test]
    fn test_result_code_speed_numeric_values() {
        assert_eq!(ResultCode::ConnectSpeed(1200).numeric(), 5);
        assert_eq!(ResultCode::ConnectSpeed(2400).numeric(), 10);
        assert_eq!(ResultCode::ConnectSpeed(4800).numeric(), 11);
        assert_eq!(ResultCode::ConnectSpeed(9600).numeric(), 12);
        assert_eq!(ResultCode::ConnectSpeed(14400).numeric(), 13);
        assert_eq!(ResultCode::ConnectSpeed(19200).numeric(), 14);
        assert_eq!(ResultCode::ConnectSpeed(28800).numeric(), 18);
        assert_eq!(ResultCode::ConnectSpeed(38400).numeric(), 28);
        assert_eq!(ResultCode::ConnectSpeed(56000).numeric(), 20);
        assert_eq!(ResultCode::ConnectSpeed(57600).numeric(), 86);
        assert_eq!(ResultCode::ConnectSpeed(115200).numeric(), 87);
        // Unknown speed falls back to generic CONNECT code
        assert_eq!(ResultCode::ConnectSpeed(300).numeric(), 1);
    }

    #[test]
    fn test_result_code_verbose_text() {
        assert_eq!(ResultCode::Ok.verbose_text(), "OK");
        assert_eq!(ResultCode::Connect.verbose_text(), "CONNECT");
        assert_eq!(ResultCode::Ring.verbose_text(), "RING");
        assert_eq!(ResultCode::NoCarrier.verbose_text(), "NO CARRIER");
        assert_eq!(ResultCode::Error.verbose_text(), "ERROR");
        assert_eq!(ResultCode::NoDialtone.verbose_text(), "NO DIALTONE");
        assert_eq!(ResultCode::Busy.verbose_text(), "BUSY");
        assert_eq!(ResultCode::NoAnswer.verbose_text(), "NO ANSWER");
        assert_eq!(
            ResultCode::ConnectSpeed(9600).verbose_text(),
            "CONNECT 9600"
        );
    }

    #[test]
    fn test_quiet_mode_suppresses_result_codes() {
        let mut p = AtParser::new();
        p.echo = false;
        let _ = send_command(&mut p, "ATQ1");
        let _ = collect_output(&mut p); // Drain the OK from ATQ1 itself

        // Now in quiet mode, commands should not produce result codes
        let _ = send_command(&mut p, "AT");
        let output = collect_output(&mut p);
        assert!(
            output.is_empty(),
            "Quiet mode should suppress result codes, got: {output:?}"
        );
    }

    #[test]
    fn test_numeric_result_codes() {
        let mut p = AtParser::new();
        p.echo = false;
        p.verbose = false;
        let _ = send_command(&mut p, "AT");
        let output = collect_output_bytes(&mut p);
        // Numeric OK is "0\r"
        assert_eq!(output, b"0\r");
    }

    #[test]
    fn test_reset_stops_parsing_remainder() {
        let mut p = AtParser::new();
        p.echo = false;
        // ATZ should reset and ignore anything after
        let cmds = send_command(&mut p, "ATZE0");
        // Should contain Reset but NOT SetEcho
        assert!(cmds.contains(&AtCommand::Reset));
        assert!(!cmds.iter().any(|c| matches!(c, AtCommand::SetEcho(_))));
    }

    #[test]
    fn test_dial_does_not_produce_ok() {
        let mut p = AtParser::new();
        p.echo = false;
        let _ = send_command(&mut p, "ATD5551234");
        let output = collect_output(&mut p);
        // ATD should NOT produce OK — it produces its own result codes later
        assert!(
            !output.contains("OK"),
            "ATD should not produce OK result code"
        );
    }

    #[test]
    fn test_answer_does_not_produce_ok() {
        let mut p = AtParser::new();
        p.echo = false;
        let _ = send_command(&mut p, "ATA");
        let output = collect_output(&mut p);
        assert!(
            !output.contains("OK"),
            "ATA should not produce OK result code"
        );
    }

    #[test]
    fn test_extended_at_command_noop() {
        let mut p = AtParser::new();
        p.echo = false;
        // Extended +/- commands should be accepted as no-ops
        let cmds = send_command(&mut p, "AT+MS=V90,1");
        let output = collect_output(&mut p);
        // Should not error out
        assert!(
            output.contains("OK"),
            "Extended commands should be accepted: {output}"
        );
        assert!(cmds.is_empty()); // No-op produces no command objects
    }

    #[test]
    fn test_spaces_in_command_line() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "AT E0 V1");
        let _ = collect_output(&mut p);
        assert!(cmds.contains(&AtCommand::SetEcho(false)));
        assert!(cmds.contains(&AtCommand::SetVerbose(true)));
    }

    #[test]
    fn test_garbage_before_at_flushed() {
        let mut p = AtParser::new();
        p.echo = false;
        // Send garbage then a valid AT command. The 'A' should flush
        // the buffer.
        for &b in b"+++" {
            p.receive_byte(b);
        }
        let cmds = send_command(&mut p, "ATH");
        assert!(cmds.contains(&AtCommand::HangUp));
    }

    #[test]
    fn test_w_command_noop() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "ATW2");
        let output = collect_output(&mut p);
        assert!(output.contains("OK"));
        assert!(cmds.is_empty()); // W is a no-op
    }

    #[test]
    fn test_ampersand_unknown_errors() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "AT&Z");
        let output = collect_output(&mut p);
        assert!(cmds.is_empty());
        assert!(output.contains("ERROR"));
    }

    #[test]
    fn test_backslash_command_noop() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "AT\\N3");
        let output = collect_output(&mut p);
        assert!(output.contains("OK"));
        assert!(cmds.is_empty());
    }

    #[test]
    fn test_percent_command_noop() {
        let mut p = AtParser::new();
        p.echo = false;
        let cmds = send_command(&mut p, "AT%C1");
        let output = collect_output(&mut p);
        assert!(output.contains("OK"));
        assert!(cmds.is_empty());
    }
}
