//! DOS Network Functions (INT 21h/5Exxh and 5Fxxh)
//!
//! Provides network-related DOS services including machine name,
//! printer setup, and network redirector functions.

use gp64_cpu::{Cpu, Flags};
use gp64_memory::Memory;

/// Network function handler for DOS INT 21h
#[derive(Clone)]
pub struct NetworkHandler {
    /// Machine name (15 characters max)
    machine_name: [u8; 16],
    /// Whether machine name is valid
    machine_name_valid: bool,
    /// Printer setup string (64 bytes max)
    printer_setup: [u8; 64],
    /// Printer setup length
    printer_setup_len: usize,
    /// Printer mode
    printer_mode: u8,
}

impl Default for NetworkHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkHandler {
    /// Create a new network handler
    pub fn new() -> Self {
        let mut machine_name = [0u8; 16];
        // Default machine name "GP64"
        machine_name[..4].copy_from_slice(b"GP64");

        Self {
            machine_name,
            machine_name_valid: true,
            printer_setup: [0u8; 64],
            printer_setup_len: 0,
            printer_mode: 0,
        }
    }

    /// Handle INT 21h/5Exxh network functions
    pub fn handle_5e(&mut self, cpu: &mut Cpu, memory: &mut Memory) -> bool {
        let subfunction = cpu.regs.al();

        match subfunction {
            0x00 => self.get_machine_name(cpu, memory),
            0x01 => self.set_machine_name(cpu, memory),
            0x02 => self.set_printer_setup(cpu, memory),
            0x03 => self.get_printer_setup(cpu, memory),
            0x04 => self.set_printer_mode(cpu, memory),
            0x05 => self.get_printer_mode(cpu, memory),
            _ => {
                // Unsupported subfunction
                cpu.regs.set_ax(1); // Invalid function
                cpu.regs.flags.insert(Flags::CARRY);
                false
            }
        }
    }

    /// Handle INT 21h/5Fxxh redirector functions
    pub fn handle_5f(&mut self, cpu: &mut Cpu, _memory: &mut Memory) -> bool {
        let subfunction = cpu.regs.al();

        match subfunction {
            0x00 => {
                // Get redirection mode
                // Returns: BL = redirection mode (0 = off, 1 = on)
                cpu.regs.set_bl(0); // Redirection off
                cpu.regs.flags.remove(Flags::CARRY);
                true
            }
            0x01 => {
                // Set redirection mode
                // BL = new mode (0 = off, 1 = on)
                // We accept the setting but don't do anything
                cpu.regs.flags.remove(Flags::CARRY);
                true
            }
            0x02 => {
                // Get redirection list entry
                // BX = redirection list index
                // Returns information about network drive assignments
                // Since we don't have network drives, return error
                cpu.regs.set_ax(0x12); // No more files (end of list)
                cpu.regs.flags.insert(Flags::CARRY);
                false
            }
            0x03 => {
                // Redirect device
                // Not supported
                cpu.regs.set_ax(1); // Invalid function
                cpu.regs.flags.insert(Flags::CARRY);
                false
            }
            0x04 => {
                // Cancel redirection
                // Not supported
                cpu.regs.set_ax(1); // Invalid function
                cpu.regs.flags.insert(Flags::CARRY);
                false
            }
            0x05 => {
                // Get extended redirection list entry (DOS 4.0+)
                cpu.regs.set_ax(0x12); // No more files
                cpu.regs.flags.insert(Flags::CARRY);
                false
            }
            0x07 => {
                // Enable/disable drive (DOS 4.0+)
                // BL = drive (0=A, 1=B, etc.)
                // BH = function (0=enable, 1=disable)
                // We don't support network drives, just succeed silently
                cpu.regs.flags.remove(Flags::CARRY);
                true
            }
            0x08 => {
                // Delete network connection (DOS 4.0+)
                cpu.regs.set_ax(1); // Invalid function
                cpu.regs.flags.insert(Flags::CARRY);
                false
            }
            _ => {
                // Unsupported subfunction
                cpu.regs.set_ax(1); // Invalid function
                cpu.regs.flags.insert(Flags::CARRY);
                false
            }
        }
    }

    /// INT 21h/5E00h - Get machine name
    fn get_machine_name(&self, cpu: &mut Cpu, memory: &mut Memory) -> bool {
        // DS:DX -> 16-byte buffer for ASCIIZ name
        let buf_addr = Memory::seg_offset_to_linear(cpu.regs.ds, cpu.regs.dx());

        // Write machine name (padded with spaces to 15 chars + NUL)
        for (i, &b) in self.machine_name.iter().enumerate() {
            memory.write_u8(buf_addr + i, b);
        }

        // Returns: CH = validity (0 = not defined, non-0 = valid)
        //          CL = NETBIOS name number
        cpu.regs
            .set_ch(if self.machine_name_valid { 0xFF } else { 0x00 });
        cpu.regs.set_cl(0x00); // NETBIOS name number
        cpu.regs.flags.remove(Flags::CARRY);
        true
    }

    /// INT 21h/5E01h - Set machine name
    fn set_machine_name(&mut self, cpu: &mut Cpu, memory: &mut Memory) -> bool {
        // DS:DX -> ASCIIZ machine name (max 15 chars)
        // CH = validity (0 = delete name, non-0 = set name)
        let validity = cpu.regs.ch();
        let buf_addr = Memory::seg_offset_to_linear(cpu.regs.ds, cpu.regs.dx());

        if validity == 0 {
            // Delete machine name
            self.machine_name = [0u8; 16];
            self.machine_name_valid = false;
        } else {
            // Set machine name
            self.machine_name = [0u8; 16];
            for i in 0..15 {
                let b = memory.read_u8(buf_addr + i);
                if b == 0 {
                    break;
                }
                self.machine_name[i] = b.to_ascii_uppercase();
            }
            self.machine_name_valid = true;
        }

        cpu.regs.flags.remove(Flags::CARRY);
        true
    }

    /// INT 21h/5E02h - Set network printer setup string
    fn set_printer_setup(&mut self, cpu: &mut Cpu, memory: &mut Memory) -> bool {
        // BX = redirection list index (ignored for local)
        // CX = length of setup string
        // DS:SI -> setup string
        let length = cpu.regs.cx() as usize;
        let src_addr = Memory::seg_offset_to_linear(cpu.regs.ds, cpu.regs.si());

        if length > 64 {
            cpu.regs.set_ax(0x57); // Invalid parameter
            cpu.regs.flags.insert(Flags::CARRY);
            return false;
        }

        self.printer_setup = [0u8; 64];
        for i in 0..length {
            self.printer_setup[i] = memory.read_u8(src_addr + i);
        }
        self.printer_setup_len = length;

        cpu.regs.flags.remove(Flags::CARRY);
        true
    }

    /// INT 21h/5E03h - Get network printer setup string
    fn get_printer_setup(&self, cpu: &mut Cpu, memory: &mut Memory) -> bool {
        // BX = redirection list index (ignored for local)
        // ES:DI -> 64-byte buffer for setup string
        // Returns: CX = length of setup string
        let dest_addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.di());

        for i in 0..self.printer_setup_len {
            memory.write_u8(dest_addr + i, self.printer_setup[i]);
        }

        cpu.regs.set_cx(self.printer_setup_len as u16);
        cpu.regs.flags.remove(Flags::CARRY);
        true
    }

    /// INT 21h/5E04h - Set printer mode
    fn set_printer_mode(&mut self, cpu: &mut Cpu, _memory: &mut Memory) -> bool {
        // BX = redirection list index (ignored for local)
        // DX = printer mode
        //   Bit 0: 1 = binary mode, 0 = text mode
        self.printer_mode = cpu.regs.dl();
        cpu.regs.flags.remove(Flags::CARRY);
        true
    }

    /// INT 21h/5E05h - Get printer mode
    fn get_printer_mode(&self, cpu: &mut Cpu, _memory: &mut Memory) -> bool {
        // BX = redirection list index (ignored for local)
        // Returns: DX = printer mode
        cpu.regs.set_dx(self.printer_mode as u16);
        cpu.regs.flags.remove(Flags::CARRY);
        true
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use gp64_cpu::Cpu;
    use gp64_memory::Memory;

    fn setup() -> (NetworkHandler, Cpu, Memory) {
        let handler = NetworkHandler::new();
        let cpu = Cpu::new();
        let memory = Memory::new();
        (handler, cpu, memory)
    }

    #[test]
    fn test_get_machine_name() {
        let (handler, mut cpu, mut memory) = setup();

        // Set up buffer at DS:DX
        cpu.regs.ds = 0x1000;
        cpu.regs.set_dx(0x0100);
        cpu.regs.set_al(0x00);

        let result = handler.get_machine_name(&mut cpu, &mut memory);
        assert!(result);
        assert!(!cpu.regs.flags.contains(Flags::CARRY));
        assert_eq!(cpu.regs.ch(), 0xFF); // Valid name

        // Check the name was written
        let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
        let name = &[
            memory.read_u8(addr),
            memory.read_u8(addr + 1),
            memory.read_u8(addr + 2),
            memory.read_u8(addr + 3),
        ];
        assert_eq!(name, b"GP64");
    }

    #[test]
    fn test_set_machine_name() {
        let (mut handler, mut cpu, mut memory) = setup();

        // Write new name to memory
        let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
        for (i, b) in b"NEWNAME\0".iter().enumerate() {
            memory.write_u8(addr + i, *b);
        }

        cpu.regs.ds = 0x1000;
        cpu.regs.set_dx(0x0100);
        cpu.regs.set_ch(0xFF); // Valid
        cpu.regs.set_al(0x01);

        let result = handler.set_machine_name(&mut cpu, &mut memory);
        assert!(result);
        assert!(!cpu.regs.flags.contains(Flags::CARRY));

        // Verify name was set
        assert_eq!(&handler.machine_name[..7], b"NEWNAME");
        assert!(handler.machine_name_valid);
    }

    #[test]
    fn test_delete_machine_name() {
        let (mut handler, mut cpu, mut memory) = setup();

        cpu.regs.ds = 0x1000;
        cpu.regs.set_dx(0x0100);
        cpu.regs.set_ch(0x00); // Delete
        cpu.regs.set_al(0x01);

        let result = handler.set_machine_name(&mut cpu, &mut memory);
        assert!(result);
        assert!(!handler.machine_name_valid);

        cpu.regs.set_al(0x00);
        let result = handler.get_machine_name(&mut cpu, &mut memory);
        assert!(result);
        assert_eq!(cpu.regs.ch(), 0x00);
    }

    #[test]
    fn test_printer_setup_roundtrip() {
        let (mut handler, mut cpu, mut memory) = setup();

        // Set printer setup string
        let setup_string = b"\x1B@INIT";
        let src_addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
        for (i, b) in setup_string.iter().enumerate() {
            memory.write_u8(src_addr + i, *b);
        }

        cpu.regs.ds = 0x1000;
        cpu.regs.set_si(0x0100);
        cpu.regs.set_cx(setup_string.len() as u16);
        cpu.regs.set_al(0x02);

        let result = handler.set_printer_setup(&mut cpu, &mut memory);
        assert!(result);
        assert!(!cpu.regs.flags.contains(Flags::CARRY));

        // Get printer setup string
        let dest_addr = Memory::seg_offset_to_linear(0x2000, 0x0200);
        cpu.regs.es = 0x2000;
        cpu.regs.set_di(0x0200);
        cpu.regs.set_al(0x03);

        let result = handler.get_printer_setup(&mut cpu, &mut memory);
        assert!(result);
        assert!(!cpu.regs.flags.contains(Flags::CARRY));
        assert_eq!(cpu.regs.cx() as usize, setup_string.len());

        // Verify data
        for (i, b) in setup_string.iter().enumerate() {
            assert_eq!(memory.read_u8(dest_addr + i), *b);
        }
    }

    #[test]
    fn test_printer_setup_rejects_too_long() {
        let (mut handler, mut cpu, mut memory) = setup();

        let src_addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
        for i in 0..65 {
            memory.write_u8(src_addr + i, i as u8);
        }

        cpu.regs.ds = 0x1000;
        cpu.regs.set_si(0x0100);
        cpu.regs.set_cx(65);
        cpu.regs.set_al(0x02);

        let result = handler.set_printer_setup(&mut cpu, &mut memory);
        assert!(!result);
        assert!(cpu.regs.flags.contains(Flags::CARRY));
        assert_eq!(cpu.regs.ax(), 0x57);
        assert_eq!(handler.printer_setup_len, 0);
    }

    #[test]
    fn test_printer_mode() {
        let (mut handler, mut cpu, mut memory) = setup();

        // Set printer mode to binary
        cpu.regs.set_dx(0x01); // Binary mode
        cpu.regs.set_al(0x04);

        let result = handler.set_printer_mode(&mut cpu, &mut memory);
        assert!(result);
        assert_eq!(handler.printer_mode, 0x01);

        // Get printer mode
        cpu.regs.set_al(0x05);
        let result = handler.get_printer_mode(&mut cpu, &mut memory);
        assert!(result);
        assert_eq!(cpu.regs.dx(), 0x01);
    }

    #[test]
    fn test_redirector_no_entries() {
        let (mut handler, mut cpu, mut memory) = setup();

        // Try to get redirection list entry
        cpu.regs.set_bx(0); // First entry
        cpu.regs.set_al(0x02);

        let result = handler.handle_5f(&mut cpu, &mut memory);
        assert!(!result);
        assert!(cpu.regs.flags.contains(Flags::CARRY));
        assert_eq!(cpu.regs.ax(), 0x12); // No more files
    }

    #[test]
    fn test_redirection_mode() {
        let (mut handler, mut cpu, mut memory) = setup();

        // Get redirection mode
        cpu.regs.set_al(0x00);
        let result = handler.handle_5f(&mut cpu, &mut memory);
        assert!(result);
        assert_eq!(cpu.regs.bl(), 0x00); // Off

        // Set redirection mode
        cpu.regs.set_al(0x01);
        cpu.regs.set_bl(0x01); // On
        let result = handler.handle_5f(&mut cpu, &mut memory);
        assert!(result);
    }

    #[test]
    fn test_handle_5e_unsupported_subfunction() {
        let (mut handler, mut cpu, mut memory) = setup();
        cpu.regs.set_al(0xFF);
        let result = handler.handle_5e(&mut cpu, &mut memory);
        assert!(!result);
        assert!(cpu.regs.flags.contains(Flags::CARRY));
        assert_eq!(cpu.regs.ax(), 1);
    }

    #[test]
    fn test_handle_5f_unsupported_subfunction() {
        let (mut handler, mut cpu, mut memory) = setup();
        cpu.regs.set_al(0xFF);
        let result = handler.handle_5f(&mut cpu, &mut memory);
        assert!(!result);
        assert!(cpu.regs.flags.contains(Flags::CARRY));
        assert_eq!(cpu.regs.ax(), 1);
    }

    #[test]
    fn test_handle_5f_03_redirect_device() {
        let (mut handler, mut cpu, mut memory) = setup();
        cpu.regs.set_al(0x03);
        let result = handler.handle_5f(&mut cpu, &mut memory);
        assert!(!result);
        assert!(cpu.regs.flags.contains(Flags::CARRY));
        assert_eq!(cpu.regs.ax(), 1);
    }

    #[test]
    fn test_handle_5f_04_cancel_redirection() {
        let (mut handler, mut cpu, mut memory) = setup();
        cpu.regs.set_al(0x04);
        let result = handler.handle_5f(&mut cpu, &mut memory);
        assert!(!result);
        assert!(cpu.regs.flags.contains(Flags::CARRY));
        assert_eq!(cpu.regs.ax(), 1);
    }

    #[test]
    fn test_handle_5f_05_get_extended_redirection_list() {
        let (mut handler, mut cpu, mut memory) = setup();
        cpu.regs.set_al(0x05);
        let result = handler.handle_5f(&mut cpu, &mut memory);
        assert!(!result);
        assert!(cpu.regs.flags.contains(Flags::CARRY));
        assert_eq!(cpu.regs.ax(), 0x12);
    }

    #[test]
    fn test_handle_5f_07_enable_disable_drive() {
        let (mut handler, mut cpu, mut memory) = setup();
        cpu.regs.set_al(0x07);
        // Enable drive A
        cpu.regs.set_bl(0);
        cpu.regs.set_bh(0);

        let result = handler.handle_5f(&mut cpu, &mut memory);
        assert!(result);
        assert!(!cpu.regs.flags.contains(Flags::CARRY));
    }

    #[test]
    fn test_handle_5f_08_delete_network_connection() {
        let (mut handler, mut cpu, mut memory) = setup();
        cpu.regs.set_al(0x08);
        let result = handler.handle_5f(&mut cpu, &mut memory);
        assert!(!result);
        assert!(cpu.regs.flags.contains(Flags::CARRY));
        assert_eq!(cpu.regs.ax(), 1);
    }

    // ============================================================
    // handle_5e dispatch tests - each asserts the one observable effect only
    // its own subfunction produces, so a transposed or rewired arm fails
    // (MDD-BUG-CRU-01414: they used to assert only the unconditional `true`
    // every arm in 00h..05h returns).
    // ============================================================

    #[test]
    fn test_handle_5e_dispatch_get_machine_name() {
        let (mut handler, mut cpu, mut memory) = setup();
        cpu.regs.ds = 0x1000;
        cpu.regs.set_dx(0x0100);
        cpu.regs.set_cx(0x1234);
        cpu.regs.set_al(0x00); // Get machine name

        assert!(handler.handle_5e(&mut cpu, &mut memory));
        assert_eq!(cpu.regs.ch(), 0xFF, "CH reports a valid name");
        assert_eq!(cpu.regs.cl(), 0x00, "CL is the NETBIOS name number");
        let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
        let name: Vec<u8> = (0..4).map(|i| memory.read_u8(addr + i)).collect();
        assert_eq!(name, b"GP64");
    }

    #[test]
    fn test_handle_5e_dispatch_set_machine_name() {
        let (mut handler, mut cpu, mut memory) = setup();
        let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
        for (i, b) in b"testpc\0".iter().enumerate() {
            memory.write_u8(addr + i, *b);
        }
        cpu.regs.ds = 0x1000;
        cpu.regs.set_dx(0x0100);
        cpu.regs.set_ch(0xFF);
        cpu.regs.set_al(0x01); // Set machine name

        assert!(handler.handle_5e(&mut cpu, &mut memory));
        assert_eq!(
            &handler.machine_name[..7],
            b"TESTPC\0",
            "stored upper-cased"
        );
        assert!(handler.machine_name_valid);
    }

    #[test]
    fn test_handle_5e_dispatch_set_printer_setup() {
        let (mut handler, mut cpu, mut memory) = setup();
        let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
        memory.write_u8(addr, b'X');
        memory.write_u8(addr + 1, b'Y');
        cpu.regs.ds = 0x1000;
        cpu.regs.set_si(0x0100);
        cpu.regs.set_cx(2);
        cpu.regs.set_al(0x02); // Set printer setup

        assert!(handler.handle_5e(&mut cpu, &mut memory));
        assert_eq!(handler.printer_setup_len, 2);
        assert_eq!(&handler.printer_setup[..2], b"XY");
    }

    #[test]
    fn test_handle_5e_dispatch_get_printer_setup() {
        let (mut handler, mut cpu, mut memory) = setup();
        handler.printer_setup[..3].copy_from_slice(b"\x1B@I");
        handler.printer_setup_len = 3;
        cpu.regs.es = 0x2000;
        cpu.regs.set_di(0x0200);
        cpu.regs.set_cx(0xFFFF);
        cpu.regs.set_al(0x03); // Get printer setup

        assert!(handler.handle_5e(&mut cpu, &mut memory));
        assert_eq!(cpu.regs.cx(), 3, "CX is the setup string length");
        let addr = Memory::seg_offset_to_linear(0x2000, 0x0200);
        let copied: Vec<u8> = (0..3).map(|i| memory.read_u8(addr + i)).collect();
        assert_eq!(copied, b"\x1B@I");
    }

    #[test]
    fn test_handle_5e_dispatch_set_printer_mode() {
        let (mut handler, mut cpu, mut memory) = setup();
        cpu.regs.set_dx(0x01);
        cpu.regs.set_al(0x04); // Set printer mode

        assert!(handler.handle_5e(&mut cpu, &mut memory));
        assert_eq!(handler.printer_mode, 0x01, "binary mode stored");
        assert_eq!(cpu.regs.dx(), 0x01, "set leaves DX alone");
    }

    #[test]
    fn test_handle_5e_dispatch_get_printer_mode() {
        let (mut handler, mut cpu, mut memory) = setup();
        handler.printer_mode = 0x01;
        cpu.regs.set_dx(0x7777);
        cpu.regs.set_al(0x05); // Get printer mode

        assert!(handler.handle_5e(&mut cpu, &mut memory));
        assert_eq!(cpu.regs.dx(), 0x0001, "DX reports the stored mode");
        assert_eq!(handler.printer_mode, 0x01, "get leaves the mode alone");
    }

    #[test]
    fn test_default_network_handler() {
        let handler = NetworkHandler::default();
        assert!(handler.machine_name_valid); // Default has "GP64" name set
        assert_eq!(&handler.machine_name[..4], b"GP64");
    }
}
