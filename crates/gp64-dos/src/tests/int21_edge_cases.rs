use super::*;

#[test]
fn test_int21_ah26_create_psp() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Write some data at the current PSP (0x0100:0x0000)
    let src_base = Memory::seg_offset_to_linear(0x0100, 0);
    memory.write_u8(src_base, 0xCD); // INT 20h
    memory.write_u8(src_base + 1, 0x20);
    memory.write_u16(src_base + 2, 0xA000); // Top of memory
    memory.write_u8(src_base + 10, 0x42); // Some data

    cpu.regs.set_ah(0x26);
    cpu.regs.set_dx(0x0200); // New PSP segment

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    // Verify copy happened
    let dst_base = Memory::seg_offset_to_linear(0x0200, 0);
    assert_eq!(memory.read_u8(dst_base), 0xCD);
    assert_eq!(memory.read_u8(dst_base + 1), 0x20);
    assert_eq!(memory.read_u8(dst_base + 10), 0x42);
    // Parent PSP should be set to current PSP
    assert_eq!(memory.read_u16(dst_base + PSP_PARENT_PSP), 0x0100);
    // Carry should be cleared
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_ah31_tsr_no_parent() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Allocate a block for the current PSP via DOS memory
    let _ = dos.dos_mem.alloc(&mut memory, dos.current_psp, 0x10);

    cpu.regs.set_ah(0x31);
    cpu.regs.set_al(0x42); // Return code
    cpu.regs.set_dx(0x20); // Keep 32 paragraphs

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // When no parent, returns Iret
    assert_eq!(dos.last_exit_code, 0x42);
    assert_eq!(dos.last_termination_type, 0x03);
}

#[test]
fn test_int21_ah03_read_stdaux() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x03);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0); // No data from STDAUX
}

#[test]
fn test_int21_ah04_write_stdaux() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x04);
    cpu.regs.set_dl(b'X');

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // Should succeed silently
}

#[test]
fn test_int21_ah05_write_stdprn() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x05);
    cpu.regs.set_dl(b'Z');

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // Should succeed silently
}

#[test]
fn test_int21_ah0d_reset_disk() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x0D);
    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY); // Set carry to verify it's cleared

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_ah0c_clear_and_noop() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // With AL=0 (no subfunction), just clear buffer
    cpu.regs.set_ah(0x0C);
    cpu.regs.set_al(0x00);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
}

fn initialize_int21_input_stack(cpu: &mut gp64_cpu::Cpu, memory: &mut Memory) {
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0x0100);
    let frame = Memory::seg_offset_to_linear(0x1000, 0x0100);
    memory.write_u16(frame, 0x1234);
    memory.write_u16(frame + 2, 0x5678);
    memory.write_u16(frame + 4, 0x0200);
}

fn stage_extended_key_prefix(
    dos: &mut Dos,
    cpu: &mut gp64_cpu::Cpu,
    memory: &mut Memory,
    vga: &mut gp64_hw::vga::Vga,
    keyboard: &mut gp64_hw::keyboard::Keyboard,
    pic: &mut gp64_hw::pic::Pic,
) {
    assert!(gp64_hw::bda::kbd_enqueue(memory, 0x48, 0xE0));
    cpu.regs.set_ah(0x07);
    let result = dos
        .int21(cpu, memory, vga, keyboard, pic)
        .expect("AH=07 should read the extended-key prefix");
    assert!(matches!(result, crate::Int21Disposition::Iret));
    assert_eq!(cpu.regs.al(), 0xE0);
}

fn assert_ah0c_console_subfunction_resumes_with_fresh_key(subfunction: u8) {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    initialize_int21_input_stack(&mut cpu, &mut memory);
    let (mut vga, mut keyboard) = create_test_io();
    let mut pic = Pic::new();
    stage_extended_key_prefix(
        &mut dos,
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut pic,
    );

    assert!(gp64_hw::bda::kbd_enqueue(&mut memory, 0x1E, b'X'));
    cpu.regs.set_ah(0x0C);
    cpu.regs.set_al(subfunction);
    let waiting = dos
        .int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .expect("AH=0C should dispatch the requested input function");
    assert!(
        matches!(waiting, crate::Int21Disposition::KbdWaitStub),
        "AH=0C AL={subfunction:02X} must wait after flushing the queued key and staged scan"
    );
    assert_eq!(
        gp64_hw::bda::kbd_peek(&memory),
        None,
        "AH=0C must clear the queued key before dispatch"
    );

    assert!(gp64_hw::bda::kbd_enqueue(&mut memory, 0x2D, b'R'));
    let resumed = dos
        .int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .expect("the input function should resume with the fresh key");
    assert!(matches!(resumed, crate::Int21Disposition::Iret));
    assert_eq!(cpu.regs.al(), b'R');
}

#[test]
fn ah07_keeps_the_two_read_extended_key_behavior_without_a_flush() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    initialize_int21_input_stack(&mut cpu, &mut memory);
    let (mut vga, mut keyboard) = create_test_io();
    let mut pic = Pic::new();

    stage_extended_key_prefix(
        &mut dos,
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut pic,
    );
    cpu.regs.set_ah(0x07);
    let result = dos
        .int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .expect("the second AH=07 read should return the extended scan code");

    assert!(matches!(result, crate::Int21Disposition::Iret));
    assert_eq!(cpu.regs.al(), 0x48);
}

#[test]
fn ah0c_al00_discards_the_staged_scan_and_accepts_a_fresh_key() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    initialize_int21_input_stack(&mut cpu, &mut memory);
    let (mut vga, mut keyboard) = create_test_io();
    let mut pic = Pic::new();
    stage_extended_key_prefix(
        &mut dos,
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut pic,
    );
    assert!(gp64_hw::bda::kbd_enqueue(&mut memory, 0x1E, b'X'));

    cpu.regs.set_ah(0x0C);
    cpu.regs.set_al(0x00);
    let flushed = dos
        .int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .expect("AH=0C AL=00 should flush input without dispatching a reader");
    assert!(matches!(flushed, crate::Int21Disposition::Iret));
    assert_eq!(gp64_hw::bda::kbd_peek(&memory), None);

    cpu.regs.set_ah(0x06);
    cpu.regs.set_dl(0xFF);
    let empty = dos
        .int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .expect("AH=06 input mode should report an empty buffer");
    assert!(matches!(empty, crate::Int21Disposition::Iret));
    assert_eq!(
        cpu.regs.al(),
        0,
        "the flushed scan code must not be returned"
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::ZERO));

    assert!(gp64_hw::bda::kbd_enqueue(&mut memory, 0x2D, b'R'));
    let fresh = dos
        .int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .expect("AH=06 should read a key pressed after the flush");
    assert!(matches!(fresh, crate::Int21Disposition::Iret));
    assert_eq!(cpu.regs.al(), b'R');
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::ZERO));
}

#[test]
fn ah0c_al06_reports_no_key_after_flushing_a_staged_extended_scan() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    initialize_int21_input_stack(&mut cpu, &mut memory);
    let (mut vga, mut keyboard) = create_test_io();
    let mut pic = Pic::new();
    stage_extended_key_prefix(
        &mut dos,
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut pic,
    );
    assert!(gp64_hw::bda::kbd_enqueue(&mut memory, 0x1E, b'X'));

    cpu.regs.set_ah(0x0C);
    cpu.regs.set_al(0x06);
    cpu.regs.set_dl(0xFF);
    let empty = dos
        .int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .expect("AH=0C AL=06 should return the empty-buffer result");
    assert!(matches!(empty, crate::Int21Disposition::Iret));
    assert_eq!(
        cpu.regs.al(),
        0,
        "the flushed scan code must not be returned"
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::ZERO));
    assert_eq!(gp64_hw::bda::kbd_peek(&memory), None);

    assert!(gp64_hw::bda::kbd_enqueue(&mut memory, 0x2D, b'R'));
    let fresh = dos
        .int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .expect("AH=06 should read a key pressed after the flush");
    assert!(matches!(fresh, crate::Int21Disposition::Iret));
    assert_eq!(cpu.regs.al(), b'R');
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::ZERO));
}

#[test]
fn ah0c_al01_flushes_the_staged_scan_before_waiting_for_a_fresh_key() {
    assert_ah0c_console_subfunction_resumes_with_fresh_key(0x01);
}

#[test]
fn ah0c_al07_flushes_the_staged_scan_before_waiting_for_a_fresh_key() {
    assert_ah0c_console_subfunction_resumes_with_fresh_key(0x07);
}

#[test]
fn ah0c_al08_flushes_the_staged_scan_before_waiting_for_a_fresh_key() {
    assert_ah0c_console_subfunction_resumes_with_fresh_key(0x08);
}

#[test]
fn ah0c_al0a_clears_the_staged_scan_before_buffered_input_resumes() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    initialize_int21_input_stack(&mut cpu, &mut memory);
    let (mut vga, mut keyboard) = create_test_io();
    let mut pic = Pic::new();
    stage_extended_key_prefix(
        &mut dos,
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut pic,
    );
    assert!(gp64_hw::bda::kbd_enqueue(&mut memory, 0x1E, b'X'));

    let input_buffer = Memory::seg_offset_to_linear(0x2000, 0x0100);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    memory.write_u8(input_buffer, 8);
    memory.write_u8(input_buffer + 1, 0);
    cpu.regs.set_ah(0x0C);
    cpu.regs.set_al(0x0A);
    let waiting = dos
        .int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .expect("AH=0C AL=0A should wait after flushing queued input");
    assert!(matches!(waiting, crate::Int21Disposition::KbdWaitStub));
    assert_eq!(gp64_hw::bda::kbd_peek(&memory), None);

    assert!(gp64_hw::bda::kbd_enqueue(&mut memory, 0x2D, b'R'));
    let resumed = dos
        .int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .expect("AH=0A should resume with the fresh key");
    assert!(matches!(resumed, crate::Int21Disposition::KbdWaitStub));
    assert_eq!(memory.read_u8(input_buffer + 1), 1);
    assert_eq!(memory.read_u8(input_buffer + 2), b'R');

    cpu.regs.set_ah(0x06);
    cpu.regs.set_dl(0xFF);
    let empty = dos
        .int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .expect("AH=06 should see no key left after buffered input");
    assert!(matches!(empty, crate::Int21Disposition::Iret));
    assert_eq!(cpu.regs.al(), 0, "AH=0C AL=0A must discard the staged scan");
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::ZERO));
}

#[test]
fn test_int2f_share_not_installed() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x1000); // SHARE check

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    // AL should remain 0 (not installed)
}

#[test]
fn test_int2f_network_redirector() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x1100); // Network Redirector

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    // AX should remain unchanged
    assert_eq!(cpu.regs.ax(), 0x1100);
}

#[test]
fn test_int2f_dos_internal_asciiz_length() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    // Write an ASCIIZ string at DS:SI
    let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    memory.write_u8(addr, b'H');
    memory.write_u8(addr + 1, b'E');
    memory.write_u8(addr + 2, b'L');
    memory.write_u8(addr + 3, b'L');
    memory.write_u8(addr + 4, b'O');
    memory.write_u8(addr + 5, 0); // NUL terminator

    cpu.regs.set_ax(0x1221); // Get ASCIIZ length
    cpu.regs.ds = 0x1000;
    cpu.regs.set_si(0x0100);

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.cx(), 5); // "HELLO" = 5 chars
}

#[test]
fn test_int2f_dos_internal_get_psp() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    dos.current_psp = 0x1234;
    cpu.regs.set_ax(0x1251); // Get current PSP

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bx(), 0x1234);
}

#[test]
fn test_int2f_dos_internal_unknown_subfunction() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x12FF); // Unknown internal subfunction

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
}

#[test]
fn test_int2f_windows_enhanced_mode_not_active() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x1600); // Windows Enhanced Mode check

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x00); // Not running
}

#[test]
fn test_int2f_windows_enhanced_mode_active() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x1600); // Windows Enhanced Mode check

    let result = dos.int2f(&mut cpu, &mut memory, true);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x03); // Windows 3.10 major
    assert_eq!(cpu.regs.ah(), 0x0A); // Windows 3.10 minor
}

#[test]
fn test_int2f_windows_init_broadcast() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x1605); // Init broadcast

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.cx(), 0); // No failure
}

#[test]
fn test_int2f_windows_exit_broadcast() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x1606); // Exit broadcast

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
}

#[test]
fn test_int2f_windows_nt_check() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x160A); // NT check

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    // AX should remain unchanged (NT not running)
    assert_eq!(cpu.regs.ax(), 0x160A);
}

#[test]
fn test_int2f_windows_release_timeslice() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x1680); // Release timeslice

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
}

#[test]
fn test_int2f_vxd_entry_point() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x1684); // Get VxD API entry point
    cpu.regs.es = 0x1234; // Should be zeroed
    cpu.regs.set_di(0x5678);

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.es, 0);
    assert_eq!(cpu.regs.di(), 0);
}

#[test]
fn test_int2f_windows_unknown_subfunction() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x16FF); // Unknown Windows subfunction

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
}

#[test]
fn test_int2f_xms_api() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x4300); // XMS check

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
}

#[test]
fn test_int2f_doskey_install_check() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x4800); // DOSKEY install check

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x00); // Not installed
}

#[test]
fn test_int2f_doskey_read_cmdline() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x4810); // DOSKEY read command line

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int2f_doskey_unknown() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x48FF); // Unknown DOSKEY subfunction

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
}

#[test]
fn test_int2f_hma_query_free() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x4A01); // Query free HMA space

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bx(), 0); // No HMA
}

#[test]
fn test_int2f_hma_alloc() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x4A02); // Allocate HMA space
    cpu.regs.set_bx(0x100);

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.es, 0);
    assert_eq!(cpu.regs.di(), 0);
}

#[test]
fn test_int2f_hma_unknown() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x4AFF);

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
}

#[test]
fn test_int2f_display_sys_not_installed() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0xAD00); // DISPLAY.SYS check

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x00); // Not installed
}

#[test]
fn test_int2f_append_install_check() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0xB700); // APPEND install check

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x00); // Not installed
}

#[test]
fn test_int2f_append_get_path() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0xB702); // APPEND get path

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.es, 0);
    assert_eq!(cpu.regs.di(), 0);
}

#[test]
fn test_int2f_append_get_state() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0xB706); // APPEND get state

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bx(), 0); // Disabled
}

#[test]
fn test_int2f_append_unknown() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0xB7FF); // Unknown APPEND subfunction

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
}

#[test]
fn test_int2f_unhandled_function() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0xFF00); // Completely unhandled

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
}

#[test]
fn test_int21_ah5e_network() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x5E);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
}

#[test]
fn test_int21_ah5f_network() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x5F);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
}

#[test]
fn test_int21_ah0b_no_key_available() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x0B);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x00); // No key available
}

#[test]
fn test_int21_ah2e_set_verify_on() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2E);
    cpu.regs.set_al(0x01); // Enable verify

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(dos.verify_flag);
}

#[test]
fn test_int21_ah2e_set_verify_off() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.verify_flag = true;
    cpu.regs.set_ah(0x2E);
    cpu.regs.set_al(0x00); // Disable verify

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!dos.verify_flag);
}

#[test]
fn test_int21_ah2e_nonone_value_ignored() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2E);
    cpu.regs.set_al(0x02); // Non-0/1 value

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!dos.verify_flag); // Should remain false
}

#[test]
fn test_int21_ahdc_azusa_mem() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0xDC);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ax(), 0);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_ah06_output_mode() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x06);
    cpu.regs.set_dl(b'A'); // Output mode (DL != 0xFF)

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    // MDD-BUG-CRUCIBLE-00448: observe the OUTPUT, not just that the
    // call returned Ok. These tests asserted dispatch success alone, so
    // deleting `vga.teletype`, passing it the wrong byte, or breaking the
    // cursor advance left every one of them green while DOS applications
    // silently lost AH=06h output.
    let mut cell = [0u8; 2];
    memory.read_bytes(0xB8000, &mut cell);
    assert_eq!(
        cell[0], b'A',
        "the character must reach VGA text memory, found {:02X?}",
        cell
    );
    assert_eq!(
        vga.get_cursor(0),
        (0, 1),
        "the cursor must advance one column past the written cell"
    );
}

#[test]
fn test_int21_ah06_input_no_key() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Set up IRET frame on stack so with_iret_flags_mut works
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0x0100);
    let ss_base = Memory::seg_offset_to_linear(0x1000, 0x0100);
    // Write return IP, CS, FLAGS on stack
    memory.write_u16(ss_base, 0x0000); // return IP
    memory.write_u16(ss_base + 2, 0x0000); // return CS
    memory.write_u16(ss_base + 4, 0x0000); // FLAGS

    cpu.regs.set_ah(0x06);
    cpu.regs.set_dl(0xFF); // Input mode

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // No key available: AL=0, ZF set
    assert_eq!(cpu.regs.al(), 0);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::ZERO));
}

#[test]
fn test_int21_ah02_char_output() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x02);
    cpu.regs.set_dl(b'X');

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
}

#[test]
fn test_int21_ah69_set_serial_number() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x69);
    cpu.regs.set_al(0x01); // Set disk serial number
    cpu.regs.set_bl(3); // C: drive

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_ah69_invalid_subfunction() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x69);
    cpu.regs.set_al(0xFF); // Invalid subfunction

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1); // Invalid function
}

#[test]
fn test_stdout_capture_enable_and_take() {
    let mut dos = Dos::new();
    dos.enable_stdout_capture();
    assert!(dos.stdout_capture.is_some());

    let data = dos.take_stdout_capture();
    assert!(data.is_empty());
    assert!(dos.stdout_capture.is_none()); // Consumed
}

#[test]
fn test_take_stdout_capture_when_not_enabled() {
    let mut dos = Dos::new();
    let data = dos.take_stdout_capture();
    assert!(data.is_empty());
}

#[test]
fn test_apply_country_settings_valid_override() {
    let mut dos = Dos::new();
    dos.apply_country_settings(Some(44)); // UK
    assert_eq!(dos.country_info().country_code, 44);
}

#[test]
fn test_apply_country_settings_invalid_override_falls_back() {
    let mut dos = Dos::new();
    // 9999 is not in the country table, so use the US default.
    dos.apply_country_settings(Some(9999));
    assert_eq!(dos.country_info().country_code, 1);
}

#[test]
fn test_apply_country_settings_none_uses_us_default() {
    let mut dos = Dos::new();
    dos.apply_country_settings(None);
    assert_eq!(dos.country_info().country_code, 1);
}

#[test]
fn test_set_dta_updates_linear() {
    let mut dos = Dos::new();
    dos.set_dta(0x5000, 0x0100);
    assert_eq!(dos.dta_segment, 0x5000);
    assert_eq!(dos.dta_offset, 0x0100);
    assert_eq!(dos.dta_linear, Memory::seg_offset_to_linear(0x5000, 0x0100));
}

#[test]
fn test_int21_ah3d_open_nul_device() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Write "NUL" as ASCIIZ at DS:DX
    let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    for (i, &b) in b"NUL\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x00); // Read mode
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // NUL device opens successfully
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_ah3d_open_con_device() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Write "CON" as ASCIIZ at DS:DX
    let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    for (i, &b) in b"CON\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x00);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // CON returns special handle marker
    assert_eq!(cpu.regs.ax(), 0x00FE);
}

#[test]
fn test_int21_ah3d_open_aux_device() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Write "AUX" as ASCIIZ at DS:DX
    let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    for (i, &b) in b"AUX\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x00);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // AUX is a real character device: CF clear and an SFT-backed handle.
    // It used to return file-not-found, which is why CMR's key mapping
    // could never be saved.
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.ax();
    assert!(handle >= 5, "expected a device handle, got {handle:#06X}");
    assert_eq!(
        dos.file_table.get_sft(handle).map(|sft| sft.path.as_str()),
        Some("AUX")
    );
}

#[test]
fn test_int21_ah71_lfn_find_first_missing_path() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    write_asciiz(&mut memory, 0x2000, 0x0100, r"C:\MISSING\*.TXT");
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x4E); // LFN Find First
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ax(), 3); // Path not found
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_ah71_lfn_find_next_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x4F); // LFN Find Next
    cpu.regs.set_bx(0x1234);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ax(), 6); // Invalid handle
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_ah71_lfn_mkdir_error_no_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Write path to nonexistent drive
    let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    for (i, &b) in b"Z:\\NEWDIR\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x39); // LFN Create Directory
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3); // Path not found
}

#[test]
fn test_int21_ah71_lfn_rmdir_error_no_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Write path to nonexistent drive
    let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    for (i, &b) in b"Z:\\SOMEDIR\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x3A); // LFN Remove Directory
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3); // Path not found
}

#[test]
fn test_int21_ah71_lfn_unknown_subfunction() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0xFF); // Unknown LFN subfunction

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_with_dos_trace_enabled() {
    // Enable tracing subscriber that captures "gp64::dos" at debug level
    let _guard = trace_subscriber("gp64::dos=debug");

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Set up a valid stack for IRET frame reading
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0x0100);
    let ss_base = Memory::seg_offset_to_linear(0x1000, 0x0100);
    memory.write_u16(ss_base, 0x0050); // return IP
    memory.write_u16(ss_base + 2, 0x0060); // return CS
    memory.write_u16(ss_base + 4, 0x0200); // FLAGS

    // AH=30h (Get DOS version) - simple, always succeeds
    cpu.regs.set_ah(0x30);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 6); // Version 6.22
}

#[test]
fn test_int21_with_dos_trace_multiple_functions() {
    // Enable tracing to cover the trace code paths in int21 dispatcher
    let _guard = trace_subscriber("gp64::dos=debug");

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Set up valid stack
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0x0100);
    let ss_base = Memory::seg_offset_to_linear(0x1000, 0x0100);
    memory.write_u16(ss_base, 0x0050);
    memory.write_u16(ss_base + 2, 0x0060);
    memory.write_u16(ss_base + 4, 0x0200);

    // Test several functions with trace enabled

    // AH=19h - Get current drive
    cpu.regs.set_ah(0x19);
    cpu.regs.set_sp(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // AH=2Ah - Get date
    cpu.regs.set_ah(0x2A);
    cpu.regs.set_sp(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // AH=1Ah - Set DTA
    cpu.regs.set_ah(0x1A);
    cpu.regs.ds = 0x3000;
    cpu.regs.set_dx(0x0080);
    cpu.regs.set_sp(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // AH=54h - Get verify flag
    cpu.regs.set_ah(0x54);
    cpu.regs.set_sp(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
}

#[test]
fn test_int21_ah09_print_simple() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Write "Hello$" at DS:DX
    let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    for (i, &b) in b"Hello$".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x09);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
}

#[test]
fn test_int21_ah3d_open_glide_ovl_when_configured() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Configure Glide OVL data
    dos.glide_ovl_data = Some(vec![0x4D, 0x5A, 0x90, 0x00].into()); // MZ header

    // Write "GLIDE2X.OVL" as ASCIIZ at DS:DX
    let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    for (i, &b) in b"GLIDE2X.OVL\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x00); // Read mode
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // Should succeed with a handle
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// AH=43h (get file attributes) must also intercept GLIDE2X.OVL when
/// via AH=43h before opening with AH=3Dh.
#[test]
fn test_int21_ah43_glide_ovl_exists_when_configured() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.glide_ovl_data = Some(vec![0x4D, 0x5A].into()); // dummy MZ header

    // Write "GLIDE2X.OVL" as ASCIIZ at DS:DX
    let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    for (i, &b) in b"GLIDE2X.OVL\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x43);
    cpu.regs.set_al(0x00); // Get attributes
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "AH=43h should succeed for GLIDE2X.OVL when OVL data is configured"
    );
    assert_eq!(cpu.regs.cx(), 0x20, "should return archive attribute");
}

/// AH=43h should also match paths with directory prefixes.
#[test]
fn test_int21_ah43_glide_ovl_with_path_prefix() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.glide_ovl_data = Some(vec![0x4D, 0x5A].into());

    let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    for (i, &b) in b"C:\\GAME\\GLIDE2X.OVL\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x43);
    cpu.regs.set_al(0x00);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "AH=43h should succeed for C:\\GAME\\GLIDE2X.OVL"
    );
}

/// AH=43h should NOT intercept when glide_ovl_data is None.
#[test]
fn test_int21_ah43_glide_ovl_not_intercepted_when_unconfigured() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // glide_ovl_data is None by default

    let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    for (i, &b) in b"GLIDE2X.OVL\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x43);
    cpu.regs.set_al(0x00);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "AH=43h should fail for GLIDE2X.OVL when OVL data is not configured"
    );
}

#[test]
fn test_int21_ah54_get_verify_flag() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.verify_flag = true;
    cpu.regs.set_ah(0x54);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 1);
}

#[test]
fn test_int21_ah59_get_extended_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.extended_error = (5, 1, 2, 3); // Access denied
    cpu.regs.set_ah(0x59);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ax(), 5);
}

#[test]
fn test_int21_ah50_set_psp() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x50);
    cpu.regs.set_bx(0x1234);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(dos.current_psp, 0x1234);
}

#[test]
fn test_int21_ah51_get_psp() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.current_psp = 0x5678;
    cpu.regs.set_ah(0x51);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bx(), 0x5678);
}

#[test]
fn test_int21_ah62_get_psp() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.current_psp = 0xABCD;
    cpu.regs.set_ah(0x62);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bx(), 0xABCD);
}

#[test]
fn test_int21_ah4d_get_return_code() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.last_exit_code = 0x42;
    dos.last_termination_type = 0x01;
    cpu.regs.set_ah(0x4D);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x42);
    assert_eq!(cpu.regs.ah(), 0x01);
    assert_eq!(dos.last_exit_code, 0);
    assert_eq!(dos.last_termination_type, 0);

    cpu.regs.set_ah(0x4D);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ax(), 0x0000);
}

#[test]
fn test_int21_ah4b_al05_set_execution_state() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x4B);
    cpu.regs.set_al(0x05); // Set execution state (undocumented)

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
}

#[test]
fn test_int21_ah33_get_ctrl_break() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.ctrl_break_check = true;
    cpu.regs.set_ah(0x33);
    cpu.regs.set_al(0x00); // Get

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.dl(), 1); // Ctrl-Break on
}

#[test]
fn test_int21_ah33_set_ctrl_break() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x33);
    cpu.regs.set_al(0x01); // Set
    cpu.regs.set_dl(0x01); // Enable

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(dos.ctrl_break_check);
}

#[test]
fn test_int21_ah38_get_country_info() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x38);
    cpu.regs.set_al(0x00); // Get current country
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_ah25_set_interrupt_vector() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x25);
    cpu.regs.set_al(0x80); // Vector 0x80
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0200);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    // Verify the IVT was updated
    let ivt_addr = 0x80 * 4;
    assert_eq!(memory.read_u16(ivt_addr), 0x0200); // Offset
    assert_eq!(memory.read_u16(ivt_addr + 2), 0x1000); // Segment
}

#[test]
fn test_int21_ah35_get_interrupt_vector() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Set up IVT entry for vector 0x21
    let ivt_addr = 0x21 * 4;
    memory.write_u16(ivt_addr, 0x0300);
    memory.write_u16(ivt_addr + 2, 0x4000);

    cpu.regs.set_ah(0x35);
    cpu.regs.set_al(0x21);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bx(), 0x0300); // Offset
    assert_eq!(cpu.regs.es, 0x4000); // Segment
}

#[test]
fn test_int21_ah44_ioctl_al00_get_device_info() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x00); // Get device info
    cpu.regs.set_bx(0); // STDIN handle

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
}

#[test]
fn test_int21_ah32_get_dpb() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x32);
    cpu.regs.set_dl(3); // C: drive (1-based)

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
}

#[test]
fn test_int21_ah34_get_indos() {
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
    // ES:BX should point to the InDOS flag
    assert_ne!(cpu.regs.es, 0);
}

#[test]
fn test_int21_ah52_get_list_of_lists() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x52);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
}

#[test]
fn test_int21_ah58_get_strategy() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x00); // Get

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ax(), 0); // First fit
}

#[test]
fn test_int21_ah58_set_strategy() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x01); // Set
    cpu.regs.set_bx(0x02); // Last fit

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(dos.memory_strategy, 0x02);
}

#[test]
fn test_int21_ah58_get_umb_link() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x02); // Get UMB link state

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0); // Not linked
}

#[test]
fn test_int21_ah58_set_umb_link() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x03); // Set UMB link
    cpu.regs.set_bx(0x01); // Link

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(dos.umb_linked);
}

#[test]
fn test_int21_ah2a_get_date() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2A);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // Year should be reasonable (2020-2030 range)
    let year = cpu.regs.cx();
    assert!(year >= 2020 && year <= 2030);
}

#[test]
fn test_int21_ah2c_get_time() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2C);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // Hours should be 0-23
    assert!(cpu.regs.ch() <= 23);
    // Minutes should be 0-59
    assert!(cpu.regs.cl() <= 59);
}

#[test]
fn test_int21_ah2b_set_date() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2B);
    cpu.regs.set_cx(2025); // Year
    cpu.regs.set_dh(6); // Month
    cpu.regs.set_dl(15); // Day

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x00); // Success
}

#[test]
fn test_int21_ah2d_set_time() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2D);
    cpu.regs.set_ch(12); // Hours
    cpu.regs.set_cl(30); // Minutes

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x00); // Success
}

#[test]
fn test_int21_ah47_get_current_dir() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x47);
    cpu.regs.set_dl(3); // C: (1-based)
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
}

#[test]
fn test_dta_linear_accessor() {
    let dos = Dos::new();
    assert_eq!(
        dos.dta_linear(),
        Memory::seg_offset_to_linear(0x0100, 0x0080)
    );
}

#[test]
fn test_verify_flag_accessors() {
    let mut dos = Dos::new();
    assert!(!dos.verify_flag());
    dos.set_verify_flag(true);
    assert!(dos.verify_flag());
}

#[test]
fn test_ctrl_break_check_accessors() {
    let mut dos = Dos::new();
    assert!(!dos.ctrl_break_check());
    dos.set_ctrl_break_check(true);
    assert!(dos.ctrl_break_check());
}

#[test]
fn test_set_last_exit_code() {
    let mut dos = Dos::new();
    dos.set_last_exit_code(42);
    assert_eq!(dos.last_exit_code, 42);
}

#[test]
fn test_set_last_termination_type() {
    let mut dos = Dos::new();
    dos.set_last_termination_type(3);
    assert_eq!(dos.last_termination_type, 3);
}

#[test]
fn test_restore_current_psp() {
    let mut dos = Dos::new();
    dos.set_current_psp(0x1000);
    // restore_current_psp doesn't reset DTA
    dos.restore_current_psp(0x2000);
    assert_eq!(dos.current_psp(), 0x2000);
    // DTA should NOT have changed to 0x2000:0x0080
    assert_eq!(dos.dta_segment, 0x1000);
}

#[test]
fn test_env_segment_accessor() {
    let dos = Dos::new();
    assert_eq!(dos.env_segment(), 0);
}

#[test]
fn test_int21_ah57_get_file_datetime() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Open a temp file first to get a valid handle
    // Use the test infrastructure to create a file
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let tmp_path = tmp.path().to_path_buf();
    let parent = tmp_path.parent().unwrap().to_path_buf();

    dos.drives
        .insert(2, Box::new(crate::filesystem::HostDrive::new(parent)));

    // Write filename at DS:DX
    let fname = tmp_path.file_name().unwrap().to_string_lossy();
    let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    for (i, b) in format!("C:\\{fname}\0").bytes().enumerate() {
        memory.write_u8(addr + i, b);
    }

    // Open the file
    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x00);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_dx(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    let handle = cpu.regs.ax();

    // Now get file date/time
    cpu.regs.set_ah(0x57);
    cpu.regs.set_al(0x00); // Get
    cpu.regs.set_bx(handle);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// Set date via AH=2B then get it back via AH=2A — values must match.
#[test]
fn test_int21_ah2b_set_date_roundtrip() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Set date to 2024-12-25 (Wednesday)
    cpu.regs.set_ah(0x2B);
    cpu.regs.set_cx(2024); // Year
    cpu.regs.set_dh(12); // Month
    cpu.regs.set_dl(25); // Day
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0x00, "Set date should succeed");

    // Now get date back
    cpu.regs.set_ah(0x2A);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.cx(), 2024, "Year mismatch");
    assert_eq!(cpu.regs.dh(), 12, "Month mismatch");
    assert_eq!(cpu.regs.dl(), 25, "Day mismatch");
    // 2024-12-25 is a Wednesday (day_of_week = 3)
    assert_eq!(
        cpu.regs.al(),
        3,
        "Day of week mismatch (expected Wednesday=3)"
    );
}

/// Set time via AH=2D then get it back via AH=2C — values must match within tolerance.
#[test]
fn test_int21_ah2d_set_time_roundtrip() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Set time to 14:30:45.50
    cpu.regs.set_ah(0x2D);
    cpu.regs.set_ch(14); // Hours
    cpu.regs.set_cl(30); // Minutes
    cpu.regs.set_dh(45); // Seconds
    cpu.regs.set_dl(50); // Hundredths
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0x00, "Set time should succeed");

    // Now get time back
    cpu.regs.set_ah(0x2C);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.ch(), 14, "Hours mismatch");
    assert_eq!(cpu.regs.cl(), 30, "Minutes mismatch");
    assert_eq!(cpu.regs.dh(), 45, "Seconds mismatch");
    // Hundredths may differ by up to 6 due to BDA tick resolution (~18.2 Hz ≈ 5.5 hs).
    // This is inherent to the integer tick conversion round-trip.
    let diff = (cpu.regs.dl() as i16 - 50).unsigned_abs();
    assert!(
        diff <= 6,
        "Hundredths off by more than 6: got {}",
        cpu.regs.dl()
    );
}

/// Set invalid date — AL=0xFF and emulated_date unchanged.
#[test]
fn test_int21_ah2b_invalid_date_no_change() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // First set a known valid date
    cpu.regs.set_ah(0x2B);
    cpu.regs.set_cx(2024);
    cpu.regs.set_dh(6);
    cpu.regs.set_dl(15);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0x00);

    // Now try an invalid date (month=13)
    cpu.regs.set_ah(0x2B);
    cpu.regs.set_cx(2024);
    cpu.regs.set_dh(13); // Invalid month
    cpu.regs.set_dl(1);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0xFF, "Invalid date should return 0xFF");

    // Get date — should still be the valid date we set
    cpu.regs.set_ah(0x2A);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.cx(), 2024, "Year should be unchanged");
    assert_eq!(cpu.regs.dh(), 6, "Month should be unchanged");
    assert_eq!(cpu.regs.dl(), 15, "Day should be unchanged");
}

/// Set invalid time — AL=0xFF and BDA ticks unchanged.
#[test]
fn test_int21_ah2d_invalid_time_no_change() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // First set a known valid time (12:00:00.00)
    cpu.regs.set_ah(0x2D);
    cpu.regs.set_ch(12);
    cpu.regs.set_cl(0);
    cpu.regs.set_dh(0);
    cpu.regs.set_dl(0);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0x00);
    let saved_ticks = memory.read_u32(BDA_TICK_COUNT_ADDR);

    // Now try an invalid time (hours=25)
    cpu.regs.set_ah(0x2D);
    cpu.regs.set_ch(25); // Invalid hours
    cpu.regs.set_cl(0);
    cpu.regs.set_dh(0);
    cpu.regs.set_dl(0);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0xFF, "Invalid time should return 0xFF");

    // BDA ticks should be unchanged
    assert_eq!(
        memory.read_u32(BDA_TICK_COUNT_ADDR),
        saved_ticks,
        "BDA ticks should be unchanged after invalid time"
    );
}

/// Set date: verify day_of_week calculation for known dates.
#[test]
fn test_int21_ah2b_day_of_week_calculation() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // 2000-01-01 is Saturday (day_of_week=6)
    cpu.regs.set_ah(0x2B);
    cpu.regs.set_cx(2000);
    cpu.regs.set_dh(1);
    cpu.regs.set_dl(1);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0x00);
    cpu.regs.set_ah(0x2A);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 6, "2000-01-01 should be Saturday (6)");

    // 1980-01-01 is Tuesday (day_of_week=2)
    cpu.regs.set_ah(0x2B);
    cpu.regs.set_cx(1980);
    cpu.regs.set_dh(1);
    cpu.regs.set_dl(1);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0x00);
    cpu.regs.set_ah(0x2A);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 2, "1980-01-01 should be Tuesday (2)");
}

/// Set time to midnight boundary — verify ticks are near zero.
#[test]
fn test_int21_ah2d_set_time_midnight() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2D);
    cpu.regs.set_ch(0); // Midnight
    cpu.regs.set_cl(0);
    cpu.regs.set_dh(0);
    cpu.regs.set_dl(0);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0x00);
    assert_eq!(memory.read_u32(BDA_TICK_COUNT_ADDR), 0);
}

/// Set time to 23:59:59.99 (end of day) — boundary test for max tick value.
#[test]
fn test_int21_ah2d_set_time_end_of_day() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Set to 23:59:59.99
    cpu.regs.set_ah(0x2D);
    cpu.regs.set_ch(23);
    cpu.regs.set_cl(59);
    cpu.regs.set_dh(59);
    cpu.regs.set_dl(99);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0x00);
    let ticks = memory.read_u32(BDA_TICK_COUNT_ADDR);
    // Should be just under BIOS_TICKS_PER_DAY (0x001800B0 = 1_573_040)
    assert!(
        ticks > 1_573_000,
        "Expected ticks near end of day, got {ticks}"
    );
    assert!(
        ticks < BIOS_TICKS_PER_DAY,
        "Ticks should be < TICKS_PER_DAY, got {ticks}"
    );

    // Round-trip: get time back
    cpu.regs.set_ah(0x2C);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.ch(), 23, "Hours");
    assert_eq!(cpu.regs.cl(), 59, "Minutes");
    assert_eq!(cpu.regs.dh(), 59, "Seconds");
    // Hundredths loses precision due to tick resolution (~5.5 hundredths per tick)
    assert!(
        cpu.regs.dl() >= 93,
        "Hundredths should be 93-99, got {}",
        cpu.regs.dl()
    );
}

// MS-DOS ERR_TABLE_24, with its call-specific loci resolved to block I/O.
// Exercise the DOS AH=59h API directly, without any Windows KERNEL handler.
#[test]
fn test_int21_ah59_critical_error_facts() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let mut pic = Pic::new();
    let expected = [
        (0x13, 0x0b07, 2),
        (0x14, 0x0405, 1),
        (0x15, 0x0507, 2),
        (0x16, 0x0405, 1),
        (0x17, 0x0b04, 2),
        (0x18, 0x0405, 1),
        (0x19, 0x0501, 2),
        (0x1a, 0x0b07, 2),
        (0x1b, 0x0b04, 2),
        (0x1c, 0x0207, 4),
        (0x1d, 0x0504, 2),
        (0x1e, 0x0504, 2),
        (0x1f, 0x0d04, 2),
    ];
    for (error, class_action, locus) in expected {
        dos.set_extended_error(error);
        cpu.regs.set_ax(0x5900);
        cpu.regs.set_bx(0);
        dos.int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
            .unwrap();
        assert_eq!(cpu.regs.ax(), error);
        assert_eq!(cpu.regs.bx(), class_action, "error {error:02x}");
        assert_eq!(cpu.regs.ch(), locus, "error {error:02x}");
    }
}
