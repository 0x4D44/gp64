//! Sound Blaster 2.0 style DSP: command handshake only.
//!
//! The card's FM chip lives in the shared OPL2 model and is routed by the I/O
//! bus. This device answers the DSP ports, reports version 2.01 and records
//! digitised-audio commands without producing any sound.

/// Reset port offset.
pub const PORT_RESET: u16 = 0x6;
/// Read-data port offset.
pub const PORT_READ_DATA: u16 = 0xa;
/// Write command/data (read: write-buffer status) port offset.
pub const PORT_WRITE: u16 = 0xc;
/// Read-buffer status port offset.
pub const PORT_READ_STATUS: u16 = 0xe;

// Emulator resource limit, not a claim about the physical DSP's FIFO size.
// Idle writes reserve room for the largest supported reply, so every admitted
// command can finish with a complete response. Busy idle writes are ignored;
// reads free space and reset clears the queue. Accepted parameter bytes always
// progress, and cannot append another command's response while pending.
const RESPONSE_CAPACITY: usize = 64;

const COPYRIGHT: &[u8] = b"COPYRIGHT (C) CREATIVE TECHNOLOGY LTD, 1992.\0";

/// Number of data bytes a command consumes after the command byte.
fn argument_len(command: u8) -> u8 {
    match command {
        0x10 | 0x40 | 0xe0 | 0xe4 => 1,
        0x14 | 0x16 | 0x17 | 0x24 | 0x48 | 0x74..=0x77 | 0x7d | 0x7f | 0x80 => 2,
        0xb0..=0xcf => 3,
        _ => 0,
    }
}

/// True for commands that start digitised playback or recording.
pub fn is_dma_command(command: u8) -> bool {
    matches!(
        command,
        0x14 | 0x1c | 0x90 | 0x91 | 0x16 | 0x17 | 0x74..=0x77 | 0x7d | 0x7f | 0xb0..=0xcf
    )
}

#[derive(Clone, Debug)]
pub struct SoundBlaster {
    reset_high: bool,
    output: std::collections::VecDeque<u8>,
    command: Option<u8>,
    args: [u8; 3],
    got: u8,
    speaker_on: bool,
    test_register: u8,
    irq_pending: bool,
    seen: [u64; 4],
    new_commands: Vec<u8>,
    dma_command_seen: bool,
}

impl Default for SoundBlaster {
    fn default() -> Self {
        Self::new()
    }
}

impl SoundBlaster {
    pub fn new() -> Self {
        Self {
            reset_high: false,
            output: std::collections::VecDeque::with_capacity(RESPONSE_CAPACITY),
            command: None,
            args: [0; 3],
            got: 0,
            speaker_on: false,
            test_register: 0,
            irq_pending: false,
            seen: [0; 4],
            new_commands: Vec::new(),
            dma_command_seen: false,
        }
    }

    /// Whether the DSP interrupt line is asserted.
    pub fn irq_asserted(&self) -> bool {
        self.irq_pending
    }

    /// Whether the guest has issued a digitised playback or record command.
    pub fn dma_command_seen(&self) -> bool {
        self.dma_command_seen
    }

    /// Drain command bytes seen for the first time since the last call.
    pub fn take_new_commands(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.new_commands)
    }

    /// Read a DSP port by offset from the card base.
    pub fn read(&mut self, offset: u16) -> u8 {
        match offset {
            PORT_READ_DATA => self.output.pop_front().unwrap_or(0xff),
            PORT_WRITE => {
                if self.write_ready() {
                    0x7f
                } else {
                    0xff
                }
            }
            PORT_READ_STATUS => {
                self.irq_pending = false;
                if self.output.is_empty() {
                    0x7f
                } else {
                    0xff
                }
            }
            _ => 0xff,
        }
    }

    /// Write a DSP port by offset from the card base.
    pub fn write(&mut self, offset: u16, value: u8) {
        match offset {
            PORT_RESET => {
                if self.reset_high && value & 1 == 0 {
                    self.reset();
                }
                self.reset_high = value & 1 != 0;
            }
            PORT_WRITE => self.write_byte(value),
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.output.clear();
        self.command = None;
        self.got = 0;
        self.speaker_on = false;
        self.irq_pending = false;
        self.output.push_back(0xaa);
    }

    fn write_ready(&self) -> bool {
        self.command.is_some() || self.output.len() <= RESPONSE_CAPACITY - COPYRIGHT.len()
    }

    fn write_byte(&mut self, value: u8) {
        let Some(command) = self.command else {
            if !self.write_ready() {
                return;
            }
            self.start_command(value);
            return;
        };
        self.args[usize::from(self.got)] = value;
        self.got += 1;
        if self.got >= argument_len(command) {
            self.command = None;
            self.finish_command(command);
        }
    }

    fn start_command(&mut self, command: u8) {
        let bit = 1u64 << (command & 63);
        let word = &mut self.seen[usize::from(command >> 6)];
        if *word & bit == 0 {
            *word |= bit;
            self.new_commands.push(command);
        }
        if is_dma_command(command) {
            self.dma_command_seen = true;
        }
        self.got = 0;
        if argument_len(command) > 0 {
            self.command = Some(command);
        } else {
            self.finish_command(command);
        }
    }

    fn finish_command(&mut self, command: u8) {
        match command {
            0xe1 => self.output.extend([0x02, 0x01]),
            0xd1 => self.speaker_on = true,
            0xd3 => self.speaker_on = false,
            0xd8 => self
                .output
                .push_back(if self.speaker_on { 0xff } else { 0x00 }),
            0xe0 => self.output.push_back(!self.args[0]),
            0xe3 => self.output.extend(COPYRIGHT.iter().copied()),
            0xe4 => self.test_register = self.args[0],
            0xe8 => self.output.push_back(self.test_register),
            0xf2 => self.irq_pending = true,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset(sb: &mut SoundBlaster) {
        sb.write(PORT_RESET, 1);
        sb.write(PORT_RESET, 0);
    }

    fn send(sb: &mut SoundBlaster, bytes: &[u8]) {
        for &b in bytes {
            sb.write(PORT_WRITE, b);
        }
    }

    #[test]
    fn unread_mixed_queries_keep_response_queue_bounded() {
        let mut sb = SoundBlaster::new();
        for _ in 0..10_000 {
            send(&mut sb, &[0xe1, 0xe0, 0x5a, 0xd8, 0xe8, 0xe3]);
            assert!(
                sb.output.len() <= 64,
                "unread replies exceeded the emulator limit"
            );
        }
        let mut expected = vec![2, 1, 0xa5, 0, 0];
        expected.extend(COPYRIGHT);
        assert_eq!(sb.output.iter().copied().collect::<Vec<_>>(), expected);
        assert_eq!(sb.read(PORT_WRITE) & 0x80, 0x80);
        assert_eq!(sb.read(PORT_READ_STATUS) & 0x80, 0x80);
    }

    #[test]
    fn response_capacity_boundary_preserves_whole_replies_and_recovers() {
        let mut sb = SoundBlaster::new();
        for _ in 0..10 {
            send(&mut sb, &[0xe1]);
        }
        // Twenty queued bytes leave one byte less than the longest reply needs.
        assert_eq!(sb.read(PORT_WRITE) & 0x80, 0x80);
        let queued = sb.output.clone();
        send(&mut sb, &[0xe3, 0xe1]);
        assert_eq!(
            sb.output, queued,
            "busy writes must not append partial replies"
        );
        assert_eq!(sb.read(PORT_READ_DATA), 2);
        assert_eq!(sb.read(PORT_WRITE) & 0x80, 0);
        send(&mut sb, &[0xe3]);
        assert_eq!(sb.output.len(), 64);
        assert_eq!(sb.read(PORT_WRITE) & 0x80, 0x80);
        let mut expected = vec![1];
        for _ in 0..9 {
            expected.extend([2, 1]);
        }
        expected.extend(COPYRIGHT);
        for byte in expected {
            assert_eq!(sb.read(PORT_READ_DATA), byte);
        }
        assert_eq!(sb.read(PORT_READ_STATUS) & 0x80, 0);
        assert_eq!(sb.read(PORT_WRITE) & 0x80, 0);
        send(&mut sb, &[0xe1]);
        assert_eq!(sb.read(PORT_READ_DATA), 2);
        assert_eq!(sb.read(PORT_READ_DATA), 1);
        send(&mut sb, &[0xe3]);
        reset(&mut sb);
        assert_eq!(sb.read(PORT_READ_DATA), 0xaa);
        assert_eq!(sb.read(PORT_READ_STATUS) & 0x80, 0);
        assert_eq!(sb.read(PORT_WRITE) & 0x80, 0);
        send(&mut sb, &[0xe3]);
        for &byte in COPYRIGHT {
            assert_eq!(sb.read(PORT_READ_DATA), byte);
        }
        assert_eq!(sb.read(PORT_READ_STATUS) & 0x80, 0);
    }

    #[test]
    fn admitted_parameters_progress_and_busy_commands_do_not_start() {
        let mut sb = SoundBlaster::new();
        for _ in 0..9 {
            send(&mut sb, &[0xe1]);
        }
        send(&mut sb, &[0xe0]);
        assert_eq!(sb.command, Some(0xe0));
        assert_eq!(sb.read(PORT_WRITE) & 0x80, 0);
        send(&mut sb, &[0x5a]);
        assert_eq!(sb.command, None);
        assert_eq!(sb.output.back(), Some(&0xa5));
        send(&mut sb, &[0xe4]);
        assert_eq!(sb.read(PORT_WRITE) & 0x80, 0);
        send(&mut sb, &[0xe1]);
        assert_eq!(sb.test_register, 0xe1);
        send(&mut sb, &[0xe8]);
        assert_eq!(sb.output.back(), Some(&0xe1));
        assert_eq!(sb.read(PORT_WRITE) & 0x80, 0x80);
        send(&mut sb, &[0xe0, 0xe1, 0xd1, 0xf2]);
        assert_eq!(sb.command, None);
        assert_eq!(sb.output.len(), 20);
        assert!(!sb.speaker_on);
        assert!(!sb.irq_asserted());
        while sb.read(PORT_READ_STATUS) & 0x80 != 0 {
            sb.read(PORT_READ_DATA);
        }
        send(&mut sb, &[0xd1, 0xf2]);
        assert!(sb.speaker_on);
        assert!(sb.irq_asserted());
        reset(&mut sb);
        assert_eq!(sb.read(PORT_READ_DATA), 0xaa);
        send(&mut sb, &[0xe0]);
        reset(&mut sb);
        assert_eq!(sb.command, None);
        assert_eq!(sb.read(PORT_READ_DATA), 0xaa);
        send(
            &mut sb,
            &[0xe0, 0x5a, 0xd1, 0xd8, 0xd3, 0xd8, 0xe4, 0x42, 0xe8],
        );
        for byte in [0xa5, 0xff, 0, 0x42] {
            assert_eq!(sb.read(PORT_READ_DATA), byte);
        }
        assert_eq!(sb.read(PORT_READ_STATUS) & 0x80, 0);
    }

    #[test]
    fn reset_handshake_returns_ready_byte() {
        let mut sb = SoundBlaster::new();
        assert_eq!(sb.read(PORT_READ_STATUS) & 0x80, 0);
        reset(&mut sb);
        assert_eq!(sb.read(PORT_READ_STATUS) & 0x80, 0x80);
        assert_eq!(sb.read(PORT_READ_DATA), 0xaa);
        assert_eq!(sb.read(PORT_READ_STATUS) & 0x80, 0);
        assert_eq!(sb.read(PORT_WRITE) & 0x80, 0);
    }

    #[test]
    fn version_is_two_oh_one() {
        let mut sb = SoundBlaster::new();
        send(&mut sb, &[0xe1]);
        assert_eq!(sb.read(PORT_READ_DATA), 2);
        assert_eq!(sb.read(PORT_READ_DATA), 1);
    }

    #[test]
    fn identification_inverts_and_test_register_round_trips() {
        let mut sb = SoundBlaster::new();
        send(&mut sb, &[0xe0, 0x5a]);
        assert_eq!(sb.read(PORT_READ_DATA), 0xa5);
        send(&mut sb, &[0xe4, 0x42, 0xe8]);
        assert_eq!(sb.read(PORT_READ_DATA), 0x42);
        send(&mut sb, &[0xe3]);
        assert_eq!(sb.read(PORT_READ_DATA), b'C');
    }

    #[test]
    fn speaker_status_follows_on_off() {
        let mut sb = SoundBlaster::new();
        send(&mut sb, &[0xd8]);
        assert_eq!(sb.read(PORT_READ_DATA), 0x00);
        send(&mut sb, &[0xd1, 0xd8]);
        assert_eq!(sb.read(PORT_READ_DATA), 0xff);
        send(&mut sb, &[0xd3, 0xd8]);
        assert_eq!(sb.read(PORT_READ_DATA), 0x00);
    }

    #[test]
    fn irq_command_raises_and_status_read_lowers() {
        let mut sb = SoundBlaster::new();
        assert!(!sb.irq_asserted());
        send(&mut sb, &[0xf2]);
        assert!(sb.irq_asserted());
        sb.read(PORT_READ_STATUS);
        assert!(!sb.irq_asserted());
    }

    #[test]
    fn dma_commands_consume_their_arguments() {
        for (command, args) in [
            (0x14u8, 2usize),
            (0x1c, 0),
            (0x90, 0),
            (0x91, 0),
            (0x16, 2),
            (0x17, 2),
            (0x74, 2),
            (0x77, 2),
            (0x7d, 2),
            (0x7f, 2),
            (0xb6, 3),
            (0xc6, 3),
            (0x10, 1),
            (0x80, 2),
            (0x40, 1),
        ] {
            let mut sb = SoundBlaster::new();
            let mut bytes = vec![command];
            bytes.extend(std::iter::repeat_n(0xe1, args));
            bytes.push(0xe1);
            send(&mut sb, &bytes);
            assert_eq!(sb.read(PORT_READ_DATA), 2, "command {command:#x}");
            assert_eq!(sb.read(PORT_READ_DATA), 1, "command {command:#x}");
            assert_eq!(sb.read(PORT_READ_STATUS) & 0x80, 0, "command {command:#x}");
            assert_eq!(sb.dma_command_seen(), is_dma_command(command));
        }
    }

    #[test]
    fn distinct_commands_are_reported_once() {
        let mut sb = SoundBlaster::new();
        send(&mut sb, &[0xe1, 0xe1, 0xd1, 0xe1]);
        assert_eq!(sb.take_new_commands(), vec![0xe1, 0xd1]);
        send(&mut sb, &[0xe1, 0xd3]);
        assert_eq!(sb.take_new_commands(), vec![0xd3]);
    }
}
