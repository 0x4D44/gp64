use super::*;

#[test]
fn test_indos_flag_address() {
    let dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();

    dos.get_indos_flag_address(&mut cpu);

    assert_eq!(cpu.regs.es, super::INDOS_SEGMENT);
    assert_eq!(cpu.regs.bx(), super::INDOS_OFFSET);
}

#[test]
fn test_indos_flag_init() {
    let mut memory = Memory::new();
    Dos::init_indos_flag(&mut memory);

    let indos_addr = Memory::seg_offset_to_linear(super::INDOS_SEGMENT, super::INDOS_OFFSET);
    let crit_addr =
        Memory::seg_offset_to_linear(super::INDOS_SEGMENT, super::CRITICAL_ERROR_OFFSET);

    assert_eq!(memory.read_u8(indos_addr), 0);
    assert_eq!(memory.read_u8(crit_addr), 0);
}

#[test]
fn test_int21_34h_indos_flag() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x34);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    // Should return InDOS flag address in ES:BX
    assert_eq!(cpu.regs.es, super::INDOS_SEGMENT);
    assert_eq!(cpu.regs.bx(), super::INDOS_OFFSET);
}

#[test]
fn test_last_exit_code_default() {
    let dos = Dos::new();
    assert_eq!(dos.last_exit_code, 0);
}

#[test]
fn test_int21_4dh_returns_exit_code() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Set a known exit code
    dos.last_exit_code = 42;

    cpu.regs.set_ah(0x4D);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    // AX should contain the exit code (AL = code, AH = termination type 0)
    assert_eq!(cpu.regs.al(), 42);
}

#[test]
fn test_int21_31h_tsr() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    // Initialize memory management
    dos.dos_mem.init(&mut memory);

    // Allocate a memory block for the "program"
    let seg = dos.dos_mem.alloc(&mut memory, 0x100, 0x20).unwrap();
    dos.set_current_psp(seg);

    // Push a parent process state to make TSR work properly
    dos.process_stack.push(ProcessState {
        psp_segment: 0x0100,
        registers: cpu.regs.clone(),
        parent_psp: 0x0100,
        exec_ret_ip: 0x0000,
        exec_ret_cs: 0x0100,
        exec_ret_flags: 0x0000,
        pm_parent: false,
    });

    // TSR with 16 paragraphs and return code 5
    cpu.regs.set_ah(0x31);
    cpu.regs.set_al(5); // Return code
    cpu.regs.set_dx(0x10); // Paragraphs to keep

    let disp = dos.terminate_stay_resident(&mut cpu, &mut memory);

    // Should return NoIret since we had a parent process
    assert_eq!(disp, Int21Disposition::NoIret);

    // Exit code should be stored
    assert_eq!(dos.last_exit_code, 5);
}

#[test]
fn test_int27_tsr() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    // Initialize memory management
    dos.dos_mem.init(&mut memory);

    // Allocate a memory block
    let seg = dos.dos_mem.alloc(&mut memory, 0x100, 0x20).unwrap();
    dos.set_current_psp(seg);

    // Push a parent process state
    dos.process_stack.push(ProcessState {
        psp_segment: 0x0100,
        registers: cpu.regs.clone(),
        parent_psp: 0x0100,
        exec_ret_ip: 0x0000,
        exec_ret_cs: 0x0100,
        exec_ret_flags: 0x0000,
        pm_parent: false,
    });

    // INT 27h with DX = bytes to keep (256 bytes = 16 paragraphs)
    cpu.regs.set_dx(0x100);

    let disp = dos.int27_tsr(&mut cpu, &mut memory);

    // Should return NoIret since we had a parent process
    assert_eq!(disp, Int21Disposition::NoIret);

    // Exit code should be 0 for INT 27h
    assert_eq!(dos.last_exit_code, 0);
}

#[test]
fn test_mcb_owner_system_constant() {
    // Verify the constant value matches DOS convention
    assert_eq!(super::MCB_OWNER_SYSTEM, 0x0008);
}

#[test]
fn test_enter_leave_dos() {
    let mut memory = Memory::new();
    Dos::init_indos_flag(&mut memory);

    let indos_addr = Memory::seg_offset_to_linear(super::INDOS_SEGMENT, super::INDOS_OFFSET);

    // Initially 0
    assert_eq!(memory.read_u8(indos_addr), 0);

    // Enter DOS increments
    Dos::enter_dos(&mut memory);
    assert_eq!(memory.read_u8(indos_addr), 1);

    // Enter again (nested call)
    Dos::enter_dos(&mut memory);
    assert_eq!(memory.read_u8(indos_addr), 2);

    // Leave decrements
    Dos::leave_dos(&mut memory);
    assert_eq!(memory.read_u8(indos_addr), 1);

    Dos::leave_dos(&mut memory);
    assert_eq!(memory.read_u8(indos_addr), 0);
}

#[test]
fn test_enter_leave_dos_saturating() {
    let mut memory = Memory::new();
    Dos::init_indos_flag(&mut memory);

    let indos_addr = Memory::seg_offset_to_linear(super::INDOS_SEGMENT, super::INDOS_OFFSET);

    // Set to max
    memory.write_u8(indos_addr, 255);

    // Enter should saturate at 255
    Dos::enter_dos(&mut memory);
    assert_eq!(memory.read_u8(indos_addr), 255);

    // Reset to 0
    memory.write_u8(indos_addr, 0);

    // Leave should saturate at 0
    Dos::leave_dos(&mut memory);
    assert_eq!(memory.read_u8(indos_addr), 0);
}

#[test]
fn test_int21_4bh_03h_overlay_file_not_found() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    // Set up filename for non-existent overlay
    let filename_addr = Memory::seg_offset_to_linear(0x1000, 0x0000);
    let filename = b"NONEXIST.OVL\x00";
    for (i, &b) in filename.iter().enumerate() {
        memory.write_u8(filename_addr + i, b);
    }

    // Set up parameter block at ES:BX
    let param_addr = Memory::seg_offset_to_linear(0x2000, 0x0000);
    memory.write_u16(param_addr, 0x3000); // Load segment
    memory.write_u16(param_addr + 2, 0x3000); // Reloc factor

    cpu.regs.set_ah(0x4B);
    cpu.regs.set_al(0x03); // Load overlay
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0000);
    cpu.regs.es = 0x2000;
    cpu.regs.set_bx(0x0000);

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Should fail with file not found
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 2);
}

#[test]
fn test_int21_4bh_invalid_subfunction() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    cpu.regs.set_ah(0x4B);
    cpu.regs.set_al(0x05); // Invalid subfunction

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Should fail with invalid function
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1); // Invalid function
}

#[test]
fn test_ioctl_00h_get_device_info_stdin() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x00);
    cpu.regs.set_bx(0); // STDIN

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert_eq!(cpu.regs.dx(), 0x80D3); // Character device
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_ioctl_00h_get_device_info_ems() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x00);
    cpu.regs.set_bx(0x00FF); // EMS device handle

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert_eq!(cpu.regs.dx(), 0x80C0); // EMS device
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_ioctl_00h_get_device_info_uses_handle_drive() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    let temp_dir = TempDir::new().unwrap();
    let host_path = temp_dir.path().join("TEST.TXT");
    std::fs::write(&host_path, b"data").unwrap();

    let drive = 3u8; // D:
    dos.drives.insert(
        drive,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"D:\\TEST.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0); // Read-only
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.ax();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x00);
    cpu.regs.set_bx(handle);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert_eq!(cpu.regs.dx(), (drive as u16) | 0x0040);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    dos.file_table.free(handle);
}

#[test]
fn test_ioctl_00h_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x00);
    cpu.regs.set_bx(999); // Invalid handle

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert_eq!(cpu.regs.ax(), 6); // Invalid handle error
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_ioctl_06h_input_status_stdin() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x06);
    cpu.regs.set_bx(0); // STDIN

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert_eq!(cpu.regs.al(), 0xFF); // Ready
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_ioctl_07h_output_status_stdout() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x07);
    cpu.regs.set_bx(1); // STDOUT

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert_eq!(cpu.regs.al(), 0xFF); // Ready
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_ioctl_08h_removable_floppy() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x08);
    cpu.regs.set_bl(1); // A: drive

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert_eq!(cpu.regs.ax(), 0); // Removable
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_ioctl_08h_fixed_hard_disk() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x08);
    cpu.regs.set_bl(3); // C: drive

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert_eq!(cpu.regs.ax(), 1); // Fixed
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_ioctl_09h_not_remote() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x09);
    cpu.regs.set_bl(3); // C: drive

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert_eq!(cpu.regs.dx(), 0x0000); // Local (not remote)
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_ioctl_0bh_sharing_retry() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x0B);

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Should succeed (ignored but accepted)
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_ioctl_0eh_get_logical_drive_map() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x0E);

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert_eq!(cpu.regs.al(), 0); // Only one letter assigned
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_ioctl_invalid_subfunction() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0xFF); // Invalid subfunction

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert_eq!(cpu.regs.ax(), 1); // Invalid function
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_60h_truename_simple() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    // Set up source path at DS:SI
    let src_addr = Memory::seg_offset_to_linear(0x1000, 0x0000);
    let test_path = b"C:\\TEST\\FILE.TXT\x00";
    for (i, &b) in test_path.iter().enumerate() {
        memory.write_u8(src_addr + i, b);
    }

    // Set up destination buffer at ES:DI
    cpu.regs.set_ah(0x60);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_si(0x0000);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0x0000);

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Read result from destination buffer
    let dest_addr = Memory::seg_offset_to_linear(0x2000, 0x0000);
    let mut result = Vec::new();
    for i in 0..128 {
        let b = memory.read_u8(dest_addr + i);
        if b == 0 {
            break;
        }
        result.push(b);
    }
    let result_str = String::from_utf8_lossy(&result);

    // Should be uppercase canonical path
    assert!(result_str.starts_with("C:\\"));
    assert!(result_str.contains("TEST"));
    assert!(result_str.contains("FILE.TXT"));
}

#[test]
fn test_int21_60h_truename_relative() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    // Set current directory first
    dos.path_resolver.set_current_dir("C:\\MYDIR");

    // Set up relative path at DS:SI
    let src_addr = Memory::seg_offset_to_linear(0x1000, 0x0000);
    let test_path = b"SUBDIR\\FILE.DAT\x00";
    for (i, &b) in test_path.iter().enumerate() {
        memory.write_u8(src_addr + i, b);
    }

    cpu.regs.set_ah(0x60);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_si(0x0000);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0x0000);

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Read and verify result
    let dest_addr = Memory::seg_offset_to_linear(0x2000, 0x0000);
    let mut result = Vec::new();
    for i in 0..128 {
        let b = memory.read_u8(dest_addr + i);
        if b == 0 {
            break;
        }
        result.push(b);
    }
    let result_str = String::from_utf8_lossy(&result);

    // Should include current directory
    assert!(result_str.starts_with("C:\\"));
    assert!(result_str.to_uppercase().contains("MYDIR"));
}

#[test]
fn test_int21_60h_truename_long_path_error() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    let src_addr = Memory::seg_offset_to_linear(0x1000, 0x0000);
    let long_path = "A".repeat(126);
    let path_bytes = format!("{long_path}\0").into_bytes();
    for (i, &b) in path_bytes.iter().enumerate() {
        memory.write_u8(src_addr + i, b);
    }

    cpu.regs.set_ah(0x60);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_si(0x0000);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0x0000);

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3);
}

#[test]
fn test_int21_5bh_create_new_invalid_drive() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    // Set up filename for non-existent drive
    let filename_addr = Memory::seg_offset_to_linear(0x1000, 0x0000);
    let filename = b"Z:\\NEWFILE.TXT\x00";
    for (i, &b) in filename.iter().enumerate() {
        memory.write_u8(filename_addr + i, b);
    }

    cpu.regs.set_ah(0x5B);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0000);
    cpu.regs.set_cx(0x00); // Normal attributes

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Should fail with path not found
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3); // Path not found
}

#[test]
fn test_int21_58h_get_allocation_strategy_default() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x00); // Get allocation strategy

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0); // First fit (default)
}

#[test]
fn test_int21_58h_set_allocation_strategy_first_fit() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    // Set to first fit
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x01); // Set allocation strategy
    cpu.regs.set_bl(0x00); // First fit

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify by getting
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x00);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.ax(), 0);
}

#[test]
fn test_int21_58h_set_allocation_strategy_best_fit() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    // Set to best fit
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x01); // Set allocation strategy
    cpu.regs.set_bl(0x01); // Best fit

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify by getting
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x00);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.ax(), 1);
}

#[test]
fn test_int21_58h_set_allocation_strategy_last_fit() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    // Set to last fit
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x01); // Set allocation strategy
    cpu.regs.set_bl(0x02); // Last fit

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify by getting
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x00);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.ax(), 2);
}

#[test]
fn test_int21_58h_set_allocation_strategy_high_memory() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    // Set to first fit in high memory first
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x01); // Set allocation strategy
    cpu.regs.set_bl(0x80); // High memory first + first fit

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify by getting
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x00);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.ax(), 0x80);
}

#[test]
fn test_int21_58h_set_allocation_strategy_invalid() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    // Set to invalid strategy (base strategy > 2)
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x01); // Set allocation strategy
    cpu.regs.set_bl(0x03); // Invalid base strategy

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1); // Invalid function
}

#[test]
fn test_int21_58h_get_umb_link_default() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x02); // Get UMB link state

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.al(), 0); // Not linked (default)
}

#[test]
fn test_int21_58h_set_umb_link_enabled() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    // Enable UMB link
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x03); // Set UMB link state
    cpu.regs.set_bx(0x01); // Link UMBs

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify by getting
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x02);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 1);
}

#[test]
fn test_int21_58h_set_umb_link_disabled() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    // First enable UMB link
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x03);
    cpu.regs.set_bx(0x01);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Then disable UMB link
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x03); // Set UMB link state
    cpu.regs.set_bx(0x00); // Unlink UMBs

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify by getting
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x02);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0);
}

#[test]
fn test_int21_58h_set_umb_link_invalid() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    // Set invalid UMB link value
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x03); // Set UMB link state
    cpu.regs.set_bx(0x02); // Invalid (only 0 or 1 valid)

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1); // Invalid function
}

#[test]
fn test_int21_58h_invalid_subfunction() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x04); // Invalid subfunction

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1); // Invalid function
}

#[test]
fn test_ioctl_04h_read_block_device_control() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x04); // Read from block device control channel
    cpu.regs.set_bl(3); // C: drive
    cpu.regs.set_cx(10); // Request 10 bytes

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Should succeed with 0 bytes read (stub implementation)
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0);
}

#[test]
fn test_ioctl_05h_write_block_device_control() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x05); // Write to block device control channel
    cpu.regs.set_bl(3); // C: drive
    cpu.regs.set_cx(10); // Write 10 bytes

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Should succeed pretending we wrote everything
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 10);
}

#[test]
fn test_ioctl_0ah_handle_remote_stdin() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x0A); // Check if handle is remote
    cpu.regs.set_bx(0); // STDIN

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Should succeed with DX = 0 (local)
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.dx(), 0x0000);
}

#[test]
fn test_ioctl_0ah_handle_remote_file_handle() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    let temp_dir = TempDir::new().unwrap();
    let host_path = temp_dir.path().join("IOCTL.TXT");
    std::fs::write(&host_path, b"data").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"C:\\IOCTL.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0); // Read-only
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    let handle = cpu.regs.ax();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x0A); // Check if handle is remote
    cpu.regs.set_bx(handle);

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.dx(), 0x0000);
}

#[test]
fn test_ioctl_0ah_handle_remote_invalid() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x0A); // Check if handle is remote
    cpu.regs.set_bx(999); // Invalid handle

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Should fail with invalid handle
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 6);
}

#[test]
fn test_ioctl_0ch_generic_char_ioctl() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x0C); // Generic IOCTL for character devices

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Should fail - not supported
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1);
}

#[test]
fn test_ioctl_0ch_display_code_page_query() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    // Set up a buffer for the response
    let buf_seg = 0x2000u16;
    let buf_off = 0x0100u16;
    memory.write_u16(Memory::seg_offset_to_linear(buf_seg, buf_off), 0xFFFF);
    memory.write_u16(Memory::seg_offset_to_linear(buf_seg, buf_off) + 2, 0xFFFF);

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x0C); // Generic IOCTL for character devices
    cpu.regs.set_ch(0x03); // Display category
    cpu.regs.set_cl(0x6A); // Get Code Page
    cpu.regs.ds = buf_seg;
    cpu.regs.set_dx(buf_off);

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Generic IOCTL 0Ch is not implemented — all subcalls return error
    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "IOCTL 0Ch display code page query should fail (not implemented)"
    );
    assert_eq!(cpu.regs.ax(), 1);
}

#[test]
fn test_ioctl_0ch_other_category_still_fails() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x0C);
    cpu.regs.set_ch(0x05); // Printer category (not display)
    cpu.regs.set_cl(0x6A);

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Non-display IOCTL 0Ch should still fail"
    );
    assert_eq!(cpu.regs.ax(), 1);
}

#[test]
fn test_ioctl_0fh_set_logical_drive_map() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x0F); // Set logical drive map
    cpu.regs.set_bl(3); // C: drive

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Should succeed (no-op)
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_5bh_create_new_success() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    // Create temp directory and mount as C:
    let temp_dir = TempDir::new().unwrap();
    dos.drives.insert(
        2, // C:
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    // Set up filename
    let filename_addr = Memory::seg_offset_to_linear(0x1000, 0x0000);
    let filename = b"C:\\NEWFILE.TXT\x00";
    for (i, &b) in filename.iter().enumerate() {
        memory.write_u8(filename_addr + i, b);
    }

    cpu.regs.set_ah(0x5B);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0000);
    cpu.regs.set_cx(0x00); // Normal attributes

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Should succeed with file handle
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Create new file failed with error {}",
        cpu.regs.ax()
    );
    assert!(cpu.regs.ax() >= 5); // Should be a valid file handle
}

#[test]
fn test_int21_5bh_file_already_exists() {
    use std::fs::File;
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    // Create temp directory with existing file
    let temp_dir = TempDir::new().unwrap();
    let existing_file = temp_dir.path().join("EXISTING.TXT");
    File::create(&existing_file).unwrap();

    dos.drives.insert(
        2, // C:
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    // Set up filename for existing file
    let filename_addr = Memory::seg_offset_to_linear(0x1000, 0x0000);
    let filename = b"C:\\EXISTING.TXT\x00";
    for (i, &b) in filename.iter().enumerate() {
        memory.write_u8(filename_addr + i, b);
    }

    cpu.regs.set_ah(0x5B);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0000);
    cpu.regs.set_cx(0x00);

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Should fail with "file exists" error
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 80); // File exists
}

#[test]
fn test_int21_6ch_open_existing_file() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("EXIST.TXT");
    std::fs::write(&file_path, b"data").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\EXIST.TXT");

    cpu.regs.set_ah(0x6C);
    cpu.regs.set_bl(0x00); // Read
    cpu.regs.set_bh(0x00);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dl(0x01); // Open if exists
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.cx(), 1);
    let handle = cpu.regs.ax();
    dos.file_table.free(handle);
}

#[test]
fn test_int21_6ch_create_new_file() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    let temp_dir = TempDir::new().unwrap();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\NEWFILE.TXT");

    cpu.regs.set_ah(0x6C);
    cpu.regs.set_bl(0x02); // Read/write
    cpu.regs.set_bh(0x00);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dl(0x10); // Create if not exists
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.cx(), 2);
    let handle = cpu.regs.ax();
    dos.file_table.free(handle);
}

#[test]
fn test_int21_6ch_existing_file_without_open_flag() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("EXISTING.TXT");
    std::fs::write(&file_path, b"data").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\EXISTING.TXT");

    cpu.regs.set_ah(0x6C);
    cpu.regs.set_bl(0x00);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dl(0x10); // Create only (no open)
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 80);
}

#[test]
fn test_int21_6ch_truncate_existing_file() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("TRUNC.TXT");
    std::fs::write(&file_path, b"data").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\TRUNC.TXT");

    cpu.regs.set_ah(0x6C);
    cpu.regs.set_bl(0x02); // Read/write
    cpu.regs.set_bh(0x00);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dl(0x03); // Open + truncate
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.cx(), 3);
    let handle = cpu.regs.ax();
    dos.file_table.free(handle);

    let size = std::fs::metadata(&file_path).unwrap().len();
    assert_eq!(size, 0);
}

#[test]
fn test_int21_4bh_03h_overlay_drive_not_found() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    let filename_addr = Memory::seg_offset_to_linear(0x1000, 0x0000);
    let filename = b"D:\\NOPE.OVL\x00";
    for (i, &b) in filename.iter().enumerate() {
        memory.write_u8(filename_addr + i, b);
    }

    let param_addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    memory.write_u16(param_addr, 0x3000);
    memory.write_u16(param_addr + 2, 0x3000);

    cpu.regs.set_ah(0x4B);
    cpu.regs.set_al(0x03);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0000);
    cpu.regs.es = 0x1000;
    cpu.regs.set_bx(0x0100);

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3);
}

#[test]
fn test_int21_4bh_03h_overlay_open_error_sets_path_not_found() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    dos.drives.insert(2, Box::new(DenyWriteDrive));

    let filename_addr = Memory::seg_offset_to_linear(0x1000, 0x0000);
    let filename = b"C:\\OPENERR.OVL\x00";
    for (i, &b) in filename.iter().enumerate() {
        memory.write_u8(filename_addr + i, b);
    }

    let param_addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    memory.write_u16(param_addr, 0x3000);
    memory.write_u16(param_addr + 2, 0x3000);

    cpu.regs.set_ah(0x4B);
    cpu.regs.set_al(0x03);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0000);
    cpu.regs.es = 0x1000;
    cpu.regs.set_bx(0x0100);

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3);
}

#[test]
fn test_int21_4bh_03h_overlay_read_failure_sets_access_denied() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    dos.drives.insert(2, Box::new(FailingReadDrive));

    let filename_addr = Memory::seg_offset_to_linear(0x1000, 0x0000);
    let filename = b"C:\\FAIL.OVL\x00";
    for (i, &b) in filename.iter().enumerate() {
        memory.write_u8(filename_addr + i, b);
    }

    let param_addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    memory.write_u16(param_addr, 0x3000);
    memory.write_u16(param_addr + 2, 0x3000);

    cpu.regs.set_ah(0x4B);
    cpu.regs.set_al(0x03);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0000);
    cpu.regs.es = 0x1000;
    cpu.regs.set_bx(0x0100);

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 5);
}

#[test]
fn test_int21_4bh_03h_overlay_access_denied_preserved() {
    // MDD-BUG-CRUCIBLE-00062: the load-overlay (4B03h) path must map an
    // access-denied open to DOS 5, not collapse it to path-not-found (3) or
    // file-not-found (2), matching the regular EXEC path. Cover both a name WITH
    // a path separator and one WITHOUT (the old code split those to 3 vs 2).
    for name in [&b"C:\\DENIED.OVL\x00"[..], &b"DENIED.OVL\x00"[..]] {
        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        let mut vga = gp64_hw::vga::Vga::new();
        let mut keyboard = gp64_hw::keyboard::Keyboard::new();

        dos.path_resolver.set_current_drive(2); // C: for the no-separator case
        dos.drives.insert(2, Box::new(AccessDeniedDrive));

        let filename_addr = Memory::seg_offset_to_linear(0x1000, 0x0000);
        for (i, &b) in name.iter().enumerate() {
            memory.write_u8(filename_addr + i, b);
        }
        let param_addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
        memory.write_u16(param_addr, 0x3000);
        memory.write_u16(param_addr + 2, 0x3000);

        cpu.regs.set_ah(0x4B);
        cpu.regs.set_al(0x03);
        cpu.regs.ds = 0x1000;
        cpu.regs.set_dx(0x0000);
        cpu.regs.es = 0x1000;
        cpu.regs.set_bx(0x0100);

        let _ = dos.int21(
            &mut cpu,
            &mut memory,
            &mut vga,
            &mut keyboard,
            &mut Pic::new(),
        );
        assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(
            cpu.regs.ax(),
            5,
            "4B03h overlay of an access-denied file (name={:?}) must set AX=5",
            std::str::from_utf8(name).unwrap()
        );
    }
}

#[test]
fn test_int21_4bh_03h_overlay_memory_bounds_error() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    let temp_dir = TempDir::new().unwrap();
    let overlay_path = temp_dir.path().join("BIG.OVL");
    std::fs::write(&overlay_path, vec![0x90u8; 32]).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    let filename_addr = Memory::seg_offset_to_linear(0x1000, 0x0000);
    let filename = b"C:\\BIG.OVL\x00";
    for (i, &b) in filename.iter().enumerate() {
        memory.write_u8(filename_addr + i, b);
    }

    let param_addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    memory.write_u16(param_addr, 0x9FFF);
    memory.write_u16(param_addr + 2, 0x3000);

    cpu.regs.set_ah(0x4B);
    cpu.regs.set_al(0x03);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0000);
    cpu.regs.es = 0x1000;
    cpu.regs.set_bx(0x0100);

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 8);
}

#[test]
fn test_int21_4bh_03h_overlay_mz_second_byte_mismatch() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    let temp_dir = TempDir::new().unwrap();
    let overlay_path = temp_dir.path().join("MISMATCH.BIN");
    let mut data = vec![0x90u8; 32];
    data[0] = b'M';
    data[1] = b'X';
    std::fs::write(&overlay_path, &data).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    let filename_addr = Memory::seg_offset_to_linear(0x1000, 0x0000);
    let filename = b"C:\\MISMATCH.BIN\x00";
    for (i, &b) in filename.iter().enumerate() {
        memory.write_u8(filename_addr + i, b);
    }

    let param_addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    memory.write_u16(param_addr, 0x3000);
    memory.write_u16(param_addr + 2, 0x3000);

    cpu.regs.set_ah(0x4B);
    cpu.regs.set_al(0x03);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0000);
    cpu.regs.es = 0x1000;
    cpu.regs.set_bx(0x0100);

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let load_addr = Memory::seg_offset_to_linear(0x3000, 0x0000);
    assert_eq!(memory.read_u8(load_addr), b'M');
    assert_eq!(memory.read_u8(load_addr + 1), b'X');
}

#[test]
fn test_int21_4bh_03h_overlay_binary_success() {
    use std::io::Write;
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    // Create temp directory with overlay file
    let temp_dir = TempDir::new().unwrap();
    let overlay_path = temp_dir.path().join("OVERLAY.BIN");
    let mut overlay_file = std::fs::File::create(&overlay_path).unwrap();

    // Write a simple binary overlay (just some code bytes)
    let overlay_data: Vec<u8> = vec![
        0xB8, 0x00, 0x4C, // MOV AX, 4C00h
        0xCD, 0x21, // INT 21h
        0x90, 0x90, 0x90, // NOP padding
    ];
    overlay_file.write_all(&overlay_data).unwrap();

    dos.drives.insert(
        2, // C:
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    // Set up filename
    let filename_addr = Memory::seg_offset_to_linear(0x1000, 0x0000);
    let filename = b"C:\\OVERLAY.BIN\x00";
    for (i, &b) in filename.iter().enumerate() {
        memory.write_u8(filename_addr + i, b);
    }

    // Set up overlay parameter block at ES:BX
    let param_addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    memory.write_u16(param_addr, 0x3000); // Load segment
    memory.write_u16(param_addr + 2, 0x3000); // Relocation factor

    cpu.regs.set_ah(0x4B);
    cpu.regs.set_al(0x03); // Load overlay
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0000);
    cpu.regs.es = 0x1000;
    cpu.regs.set_bx(0x0100);

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Should succeed
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Overlay load failed with error {}",
        cpu.regs.ax()
    );

    // Verify overlay was loaded at segment 0x3000
    let load_addr = Memory::seg_offset_to_linear(0x3000, 0x0000);
    assert_eq!(memory.read_u8(load_addr), 0xB8);
    assert_eq!(memory.read_u8(load_addr + 1), 0x00);
    assert_eq!(memory.read_u8(load_addr + 2), 0x4C);
    assert_eq!(memory.read_u8(load_addr + 3), 0xCD);
    assert_eq!(memory.read_u8(load_addr + 4), 0x21);
}

#[test]
fn test_int21_4bh_03h_overlay_non_mz_long_data() {
    use std::io::Write;
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    let temp_dir = TempDir::new().unwrap();
    let overlay_path = temp_dir.path().join("LONGRAW.BIN");
    let mut overlay_file = std::fs::File::create(&overlay_path).unwrap();

    let mut overlay_data = vec![0x90u8; 32];
    overlay_data[0] = b'N';
    overlay_data[1] = b'O';
    overlay_file.write_all(&overlay_data).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    let filename_addr = Memory::seg_offset_to_linear(0x1000, 0x0000);
    let filename = b"C:\\LONGRAW.BIN\x00";
    for (i, &b) in filename.iter().enumerate() {
        memory.write_u8(filename_addr + i, b);
    }

    let param_addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    memory.write_u16(param_addr, 0x3000);
    memory.write_u16(param_addr + 2, 0x3000);

    cpu.regs.set_ah(0x4B);
    cpu.regs.set_al(0x03);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0000);
    cpu.regs.es = 0x1000;
    cpu.regs.set_bx(0x0100);

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let load_addr = Memory::seg_offset_to_linear(0x3000, 0x0000);
    assert_eq!(memory.read_u8(load_addr), b'N');
    assert_eq!(memory.read_u8(load_addr + 1), b'O');
}

#[test]
fn test_int21_4bh_03h_overlay_exe_with_relocations() {
    use std::io::Write;
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    // Create temp directory with EXE overlay
    let temp_dir = TempDir::new().unwrap();
    let overlay_path = temp_dir.path().join("OVERLAY.OVL");
    let mut overlay_file = std::fs::File::create(&overlay_path).unwrap();

    // Create a minimal MZ EXE with one relocation
    let mut exe_data = vec![0u8; 544]; // 512 header + 32 code

    // MZ header
    exe_data[0] = b'M';
    exe_data[1] = b'Z';
    exe_data[2] = 32; // Bytes on last page (32 bytes of code)
    exe_data[3] = 0;
    exe_data[4] = 2; // Pages (512 + 32 = 544 bytes = 2 pages)
    exe_data[5] = 0;
    exe_data[6] = 1; // Relocation count = 1
    exe_data[7] = 0;
    exe_data[8] = 32; // Header paragraphs = 32 (512 bytes)
    exe_data[9] = 0;
    exe_data[24] = 28; // Relocation table offset
    exe_data[25] = 0;

    // Relocation entry at offset 28: word at offset 1 in code (the imm16 of MOV AX)
    exe_data[28] = 1; // Offset low
    exe_data[29] = 0; // Offset high
    exe_data[30] = 0; // Segment low
    exe_data[31] = 0; // Segment high

    // Code at offset 512:
    // MOV AX, <segment> (the segment will be relocated)
    exe_data[512] = 0xB8; // MOV AX, imm16
    exe_data[513] = 0x00; // Low byte of segment (will be relocated)
    exe_data[514] = 0x00; // High byte of segment
    exe_data[515] = 0xCD; // INT
    exe_data[516] = 0x21; // 21h

    overlay_file.write_all(&exe_data).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    // Set up filename
    let filename_addr = Memory::seg_offset_to_linear(0x1000, 0x0000);
    let filename = b"C:\\OVERLAY.OVL\x00";
    for (i, &b) in filename.iter().enumerate() {
        memory.write_u8(filename_addr + i, b);
    }

    // Set up overlay parameter block
    let param_addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    memory.write_u16(param_addr, 0x4000); // Load segment
    memory.write_u16(param_addr + 2, 0x1000); // Relocation factor

    cpu.regs.set_ah(0x4B);
    cpu.regs.set_al(0x03);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0000);
    cpu.regs.es = 0x1000;
    cpu.regs.set_bx(0x0100);

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Should succeed
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "EXE overlay load failed with error {}",
        cpu.regs.ax()
    );

    // Verify code was loaded at segment 0x4000
    let load_addr = Memory::seg_offset_to_linear(0x4000, 0x0000);
    assert_eq!(memory.read_u8(load_addr), 0xB8); // MOV AX opcode

    // Verify relocation was applied (segment at offset 1-2 should be 0x1000)
    let reloc_value = memory.read_u16(load_addr + 1);
    assert_eq!(
        reloc_value, 0x1000,
        "Relocation not applied: got {reloc_value:04X}, expected 0x1000"
    );
}

#[test]
fn test_load_exe_overlay_invalid_header() {
    let dos = Dos::new();
    let mut memory = Memory::new();

    let mut data = vec![0u8; 32];
    data[8] = 3;

    let result = dos.load_exe_overlay(&mut memory, &data, 0x2000, 0x0000);
    assert!(result.is_err());
}

#[test]
fn test_load_exe_overlay_too_large() {
    let dos = Dos::new();
    let mut memory = Memory::new();

    let mut data = vec![0u8; 64];
    data[8] = 1;
    data[24] = 0;
    data[25] = 0;

    let result = dos.load_exe_overlay(&mut memory, &data, 0x9FFF, 0x0000);
    assert!(result.is_err());
}

#[test]
fn test_load_exe_overlay_reloc_table_short() {
    let dos = Dos::new();
    let mut memory = Memory::new();

    let mut data = vec![0u8; 32];
    data[8] = 1;
    data[6] = 1;
    data[24] = 30;
    data[25] = 0;

    let result = dos.load_exe_overlay(&mut memory, &data, 0x2000, 0x0000);
    assert!(result.is_ok());
}

#[test]
fn test_load_exe_overlay_reloc_address_out_of_bounds() {
    let dos = Dos::new();
    let mut memory = Memory::new();

    let mut data = vec![0u8; 32];
    data[8] = 1;
    data[6] = 1;
    data[24] = 20;
    data[25] = 0;

    data[20] = 0x00;
    data[21] = 0x00;
    data[22] = 0x01;
    data[23] = 0x00;

    let result = dos.load_exe_overlay(&mut memory, &data, 0x9FFF, 0x0000);
    assert!(result.is_ok());
}

#[test]
fn test_tsr_mcb_ownership_set_to_system() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.dos_mem.init(&mut memory);

    // Allocate a memory block
    let seg = dos.dos_mem.alloc(&mut memory, 0x100, 0x20).unwrap();
    dos.set_current_psp(seg);

    // Push a parent process so TSR can return
    dos.process_stack.push(crate::ProcessState {
        psp_segment: 0x0050,
        parent_psp: 0x0050,
        registers: gp64_cpu::Registers::new(),
        exec_ret_cs: 0x0100,
        exec_ret_ip: 0x0000,
        exec_ret_flags: 0x0000,
        pm_parent: false,
    });

    cpu.regs.set_ah(0x31);
    cpu.regs.set_al(0); // Return code
    cpu.regs.set_dx(0x10); // Keep 16 paragraphs

    dos.terminate_stay_resident(&mut cpu, &mut memory);

    // Read MCB to verify owner is set to SYSTEM (0x0008)
    let mcb_addr = Memory::seg_offset_to_linear(seg - 1, 0);
    let owner = memory.read_u16(mcb_addr + 1);
    assert_eq!(
        owner, MCB_OWNER_SYSTEM,
        "MCB owner should be SYSTEM (0x0008), got {owner:04X}"
    );
}

#[test]
fn test_tsr_memory_resize() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.dos_mem.init(&mut memory);

    // Allocate a larger memory block (64 paragraphs = 1KB)
    let seg = dos.dos_mem.alloc(&mut memory, 0x100, 0x40).unwrap();
    dos.set_current_psp(seg);

    // Push a parent process
    dos.process_stack.push(crate::ProcessState {
        psp_segment: 0x0050,
        parent_psp: 0x0050,
        registers: gp64_cpu::Registers::new(),
        exec_ret_cs: 0x0100,
        exec_ret_ip: 0x0000,
        exec_ret_flags: 0x0000,
        pm_parent: false,
    });

    // TSR requesting only 16 paragraphs (should resize down)
    cpu.regs.set_ah(0x31);
    cpu.regs.set_al(0);
    cpu.regs.set_dx(0x10); // Keep only 16 paragraphs

    dos.terminate_stay_resident(&mut cpu, &mut memory);

    // Read MCB to verify size was reduced
    let mcb_addr = Memory::seg_offset_to_linear(seg - 1, 0);
    let size = memory.read_u16(mcb_addr + 3);

    // Size should be approximately 16 paragraphs (may vary due to implementation)
    assert!(
        size <= 0x20,
        "MCB size should be reduced, got {size:04X} paragraphs"
    );
}
