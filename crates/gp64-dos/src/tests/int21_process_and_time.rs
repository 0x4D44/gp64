use super::*;

#[test]
fn test_int21_buffered_input_regular_char_appends() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    bda::kbd_enqueue(&mut memory, 0x1E, b'A');

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, 4);
    memory.write_u8(addr + 1, 0);

    cpu.regs.set_ah(0x0A);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.ss = 0x3000;
    cpu.regs.set_sp(0x0200);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(addr + 1), 1);
    assert_eq!(memory.read_u8(addr + 2), b'A');
}

#[test]
fn test_int21_buffered_input_regular_char_ignored_when_full() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    bda::kbd_enqueue(&mut memory, 0x30, b'B');

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, 2);
    memory.write_u8(addr + 1, 1);
    memory.write_u8(addr + 2, b'A');

    cpu.regs.set_ah(0x0A);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.ss = 0x3000;
    cpu.regs.set_sp(0x0200);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(addr + 1), 1);
    assert_eq!(memory.read_u8(addr + 2), b'A');
}

#[test]
fn test_int21_buffered_input_control_char_ignored() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    bda::kbd_enqueue(&mut memory, 0x10, 0x01);

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, 4);
    memory.write_u8(addr + 1, 0);
    memory.write_u8(addr + 2, 0xCC);

    cpu.regs.set_ah(0x0A);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.ss = 0x3000;
    cpu.regs.set_sp(0x0200);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(addr + 1), 0);
    assert_eq!(memory.read_u8(addr + 2), 0xCC);
}

#[test]
fn test_int21_console_io_ah06_output() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x06);
    cpu.regs.set_dl(b'X'); // Output character X

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
        cell[0], b'X',
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
fn test_int21_console_io_ah06_input() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x06);
    cpu.regs.set_dl(0xFF); // Input check

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
fn test_int21_read_char_ah01() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x01);

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
fn test_int21_no_key_returns_kbd_wait_stub() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x01);
    cpu.regs.ss = 0x2000;
    cpu.regs.set_sp(0x0100);

    let ret_ip: u16 = 0x1234;
    memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp(), ret_ip);
    memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp().wrapping_add(2), 0xABCD);
    memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp().wrapping_add(4), 0x0002);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // Real-mode path with no key returns KbdWaitStub — the assembler
    // stub at F000:F8D0 handles looping and re-entering INT 21h.
    assert_eq!(result.unwrap(), Int21Disposition::KbdWaitStub);

    // Stack should NOT be modified — the stub manages its own looping.
    let ip_on_stack = memory.read_u16_seg(cpu.regs.ss, cpu.regs.sp());
    assert_eq!(ip_on_stack, ret_ip);
}

#[test]
fn test_int21_read_char_no_echo_ah08() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x08);

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
fn test_int21_read_char_no_echo_ah07() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x07);

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
fn test_int21_write_stdaux_handle3() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, b'X');

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(3); // stdaux
    cpu.regs.set_cx(1);
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
    // STDAUX is a real device now: the byte queues for COM1's transmit
    // path instead of being refused.
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1);
    assert_eq!(dos.device_bridge.lock().unwrap().take_aux_tx(), b"X");
}

#[test]
fn test_int21_write_stdin_handle0() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, b'X');

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(0); // stdin
    cpu.regs.set_cx(1);
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
    // stdin write should fail
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_write_stdprn_handle4() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, b'X');

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(4); // stdprn
    cpu.regs.set_cx(1);
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
    // STDPRN is a real device now: the byte reaches the printer
    // accumulator instead of being refused.
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1);
    assert_eq!(dos.device_bridge.lock().unwrap().take_prn_out(), b"X");
}

#[test]
fn test_int21_write_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, b'X');

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(99); // Invalid handle
    cpu.regs.set_cx(1);
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
    // Invalid handle write should fail
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 6); // Invalid handle error
}

#[test]
fn test_int21_read_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(99); // Invalid handle
    cpu.regs.set_cx(10);
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 6); // Invalid handle
}

#[test]
fn test_int21_seek_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x42);
    cpu.regs.set_al(0); // Start
    cpu.regs.set_bx(99); // Invalid handle
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(0);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 6); // Invalid handle
}

#[test]
fn test_int21_seek_invalid_origin() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Create a temp file
    let mut path = std::env::temp_dir();
    path.push(format!("test_seek_{}.txt", std::process::id()));
    let filename = path.to_str().unwrap().to_string();
    std::fs::write(&filename, b"test content").unwrap();

    // Open the file
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in format!("{filename}\0").as_bytes().iter().enumerate() {
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

    // Try seek with invalid origin
    cpu.regs.set_ah(0x42);
    cpu.regs.set_al(5); // Invalid origin
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(0);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Cleanup
    dos.file_table.free(handle);
    let _ = std::fs::remove_file(&filename);
}

#[test]
fn test_int21_close_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x3E);
    cpu.regs.set_bx(99); // Invalid handle

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
fn test_int21_close_standard_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Closing standard handles should succeed
    cpu.regs.set_ah(0x3E);
    cpu.regs.set_bx(0); // stdin

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
fn test_int21_open_invalid_access_mode() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"TEST.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0xFF); // Invalid access mode
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
    // Invalid access mode returns CF=1, AX=1
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_ioctl_check_output_ah44_07() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x07); // Check output status
    cpu.regs.set_bx(1); // stdout

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
fn test_int21_ioctl_check_input_ah44_06() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x06); // Check input status
    cpu.regs.set_bx(0); // stdin

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
fn test_int21_free_memory_ah49_invalid() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x49);
    cpu.regs.es = 0xFFFF; // Invalid segment

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
fn test_int21_resize_memory_ah4a_invalid() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x4A);
    cpu.regs.es = 0xFFFF; // Invalid segment
    cpu.regs.set_bx(0x100);

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
fn test_int21_get_country_info_ah38() {
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
}

#[test]
fn test_int21_get_country_info_al_ff_ah38() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x38);
    cpu.regs.set_al(0xFF); // Get using BX
    cpu.regs.set_bx(1);
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
fn test_int21_set_date_time_ah2b_ah2d() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Set date
    cpu.regs.set_ah(0x2B);
    cpu.regs.set_cx(2024); // Year
    cpu.regs.set_dh(12); // Month
    cpu.regs.set_dl(25); // Day

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    // Set time
    cpu.regs.set_ah(0x2D);
    cpu.regs.set_ch(12); // Hour
    cpu.regs.set_cl(30); // Minute
    cpu.regs.set_dh(45); // Second
    cpu.regs.set_dl(0); // Hundredths

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
fn test_int21_set_time_invalid_ah2d() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2D);
    cpu.regs.set_ch(25); // Invalid hour
    cpu.regs.set_cl(0);
    cpu.regs.set_dh(0);
    cpu.regs.set_dl(0);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0xFF);
}

#[test]
fn test_int21_buffered_input_ctrl_c() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let vector_addr = 0x23 * 4;
    memory.write_u16(vector_addr, 0x1111);
    memory.write_u16(vector_addr + 2, 0x2222);

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, 4);
    memory.write_u8(addr + 1, 0);

    cpu.regs.set_ah(0x0A);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.ss = 0x3000;
    cpu.regs.set_sp(0x0200);
    memory.write_u16_seg(0x3000, 0x0200, 0);
    memory.write_u16_seg(0x3000, 0x0202, 0);
    memory.write_u16_seg(0x3000, 0x0204, 0);

    bda::kbd_enqueue(&mut memory, 0x2E, 0x03);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ip(), 0x1111);
    assert_eq!(cpu.regs.cs, 0x2222);
}

#[test]
fn test_int21_set_date_leap_year_valid() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2B);
    cpu.regs.set_cx(2024);
    cpu.regs.set_dh(2);
    cpu.regs.set_dl(29);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x00);
}

#[test]
fn test_int21_set_date_century_leap_year_valid() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2B);
    cpu.regs.set_cx(2000);
    cpu.regs.set_dh(2);
    cpu.regs.set_dl(29);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x00);
}

#[test]
fn test_int21_set_date_non_leap_year_invalid() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2B);
    cpu.regs.set_cx(2023);
    cpu.regs.set_dh(2);
    cpu.regs.set_dl(29);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0xFF);
}

#[test]
fn test_int21_set_date_invalid_month() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2B);
    cpu.regs.set_cx(2024);
    cpu.regs.set_dh(0);
    cpu.regs.set_dl(10);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0xFF);
}

#[test]
fn test_int21_set_date_invalid_year() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2B);
    cpu.regs.set_cx(1979);
    cpu.regs.set_dh(1);
    cpu.regs.set_dl(1);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0xFF);
}

#[test]
fn test_int21_set_date_invalid_day_zero() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2B);
    cpu.regs.set_cx(2024);
    cpu.regs.set_dh(1);
    cpu.regs.set_dl(0);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0xFF);
}

/// MDD-BUG-CRU-01394: the BIOS midnight flag advances the cached date,
/// whether DOS reads the clock with AH=2Ch or the date with AH=2Ah.
#[test]
fn midnight_rollovers_advance_the_dos_date() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let flag = gp64_types::bda::BDA_MIDNIGHT_FLAG_ADDR;
    dos.emulated_date = (2024, 2, 28, 3);

    memory.write_u8(flag, 1);
    cpu.regs.set_ah(0x2C);
    dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    )
    .unwrap();
    assert_eq!(memory.read_u8(flag), 0, "DOS consumes the flag");
    assert_eq!(dos.emulated_date, (2024, 2, 29, 4), "leap day, Thursday");

    memory.write_u8(flag, 2);
    cpu.regs.set_ah(0x2A);
    dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    )
    .unwrap();
    assert_eq!(
        (cpu.regs.cx(), cpu.regs.dh(), cpu.regs.dl(), cpu.regs.al()),
        (2024, 3, 2, 6),
        "two rollovers: Saturday 2 March"
    );

    cpu.regs.set_ah(0x2A);
    dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    )
    .unwrap();
    assert_eq!(
        (cpu.regs.dh(), cpu.regs.dl()),
        (3, 2),
        "no flag, no advance"
    );
}

#[test]
fn test_int21_get_date_time_ah2a_ah2c() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Get date
    cpu.regs.set_ah(0x2A);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    // Get time
    cpu.regs.set_ah(0x2C);
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
fn test_int21_unhandled_function() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // AH=0xFF is unhandled
    cpu.regs.set_ah(0xFF);

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
fn test_ah_range_ordering() {
    let r1 = AhRange { start: 5, end: 3 };
    assert_eq!(r1.start, 5);
    assert_eq!(r1.end, 3);
}

#[test]
fn test_should_trace_ah_filtering() {
    let _lock = env_lock();
    // This tests the trace filtering without setting env vars
    // The default behavior (no filter) should trace everything
    assert!(should_trace_ah(0x00));
    assert!(should_trace_ah(0x30));
    assert!(should_trace_ah(0xFF));
}

#[test]
fn test_int21_dup_handle_invalid_ah45() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x45);
    cpu.regs.set_bx(99); // Invalid handle

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
fn test_int21_get_verify_flag_ah54() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x54);

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
fn test_int21_set_verify_flag_ah2e() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // AL=1 should enable verify
    cpu.regs.set_ah(0x2E);
    cpu.regs.set_al(0x01);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    cpu.regs.set_ah(0x54);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 1, "AL=1 should enable verify");

    // AL=0 should disable verify
    cpu.regs.set_ah(0x2E);
    cpu.regs.set_al(0x00);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    cpu.regs.set_ah(0x54);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0, "AL=0 should disable verify");

    // AL=0x1F (non-zero, non-one) should NOT enable verify
    cpu.regs.set_ah(0x2E);
    cpu.regs.set_al(0x1F);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    cpu.regs.set_ah(0x54);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0, "AL=0x1F should not enable verify");
}

#[test]
fn test_find_entry_dir_attribute() {
    let entry = FindEntry {
        name: "SUBDIR".to_string(),
        size: 0,
        attr: 0x10, // Directory
        time: 0,
        date: 0,
    };
    assert_eq!(entry.attr, 0x10);
    assert_eq!(entry.size, 0);
}

#[test]
fn test_int21_disposition_enum() {
    assert_ne!(Int21Disposition::Iret, Int21Disposition::NoIret);
}

#[test]
fn test_int21_create_file_ah3c() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Use temp dir for test file
    let temp_path = std::env::temp_dir().join("gp64_test_create.tmp");
    let path_str = temp_path.to_string_lossy();
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, b) in path_str.bytes().enumerate() {
        memory.write_u8(addr + i, b);
    }
    memory.write_u8(addr + path_str.len(), 0);

    cpu.regs.set_ah(0x3C);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.set_cx(0); // Normal attributes

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    // Clean up
    let _ = std::fs::remove_file(&temp_path);
}

/// MDD-BUG-CRU-01398: AH=3Ch and AH=5Bh ignored CX, so a file created
/// read-only came out plain and a later delete succeeded, although MS-DOS
/// refuses it with access denied. CX=0 stays an ordinary writable file.
#[test]
#[allow(clippy::permissions_set_readonly_false)]
fn test_int21_create_applies_the_cx_attributes() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let cases: [(u8, u16, bool); 3] = [(0x3C, 0x01, true), (0x5B, 0x01, true), (0x3C, 0x00, false)];
    for (ah, cx, read_only) in cases {
        let mut dos = Dos::new();
        let mut cpu = gp64_cpu::Cpu::new();
        let mut memory = Memory::new();
        let (mut vga, mut keyboard) = create_test_io();
        dos.drives.insert(
            2,
            Box::new(crate::filesystem::HostDrive::new(
                temp_dir.path().to_path_buf(),
            )),
        );
        let name = format!("C{ah:02X}{cx}.TXT");
        let file_path = temp_dir.path().join(&name);
        let path_str = format!("C:\\{name}");
        write_asciiz(&mut memory, 0x2000, 0x0100, &path_str);
        let mut call = |cpu: &mut gp64_cpu::Cpu, memory: &mut Memory, ax: u16, cx: u16| {
            cpu.regs.set_ax(ax);
            cpu.regs.set_cx(cx);
            cpu.regs.ds = 0x2000;
            cpu.regs.set_dx(0x0100);
            dos.int21(cpu, memory, &mut vga, &mut keyboard, &mut Pic::new())
                .unwrap();
        };

        call(&mut cpu, &mut memory, u16::from(ah) << 8, cx);
        assert!(
            !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "AH={ah:02X}h create must succeed"
        );
        let handle = cpu.regs.ax();
        cpu.regs.set_bx(handle);
        call(&mut cpu, &mut memory, 0x3E00, 0); // close

        let host_read_only = std::fs::metadata(&file_path)
            .unwrap()
            .permissions()
            .readonly();
        assert_eq!(host_read_only, read_only, "AH={ah:02X}h CX={cx:#x}");

        call(&mut cpu, &mut memory, 0x4100, 0); // delete
        assert_eq!(
            cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            read_only,
            "AH={ah:02X}h CX={cx:#x}: a read-only file must refuse deletion"
        );
        if read_only {
            assert_eq!(cpu.regs.ax(), 5, "access denied");
            let mut permissions = std::fs::metadata(&file_path).unwrap().permissions();
            permissions.set_readonly(false);
            std::fs::set_permissions(&file_path, permissions).unwrap();
        }
    }
}

/// Attribute and delete success must remain independent of a previous failed
/// FindFirst (02715). Exercise the DOS file-service sequence on an existing file.
#[test]
fn test_int21_existing_file_attributes_and_delete_after_empty_find() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );
    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\TEST.TMP");
    write_asciiz(&mut memory, 0x2000, 0x0200, "C:\\MISSING.TMP");
    let mut call = |cpu: &mut gp64_cpu::Cpu, memory: &mut Memory, ax, cx, dx| {
        cpu.regs.set_ax(ax);
        cpu.regs.set_cx(cx);
        cpu.regs.ds = 0x2000;
        cpu.regs.set_dx(dx);
        dos.int21(cpu, memory, &mut vga, &mut keyboard, &mut Pic::new())
            .unwrap();
    };
    call(&mut cpu, &mut memory, 0x3C00, 0, 0x0100);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    cpu.regs.set_bx(cpu.regs.ax());
    call(&mut cpu, &mut memory, 0x3E00, 0, 0x0100);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    call(&mut cpu, &mut memory, 0x4E00, 0, 0x0200);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 18);
    call(&mut cpu, &mut memory, 0x4300, 0, 0x0100);
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "get attributes: AX={:04x}",
        cpu.regs.ax()
    );
    assert_eq!(cpu.regs.cx() & 0x1F, 0);
    call(&mut cpu, &mut memory, 0x4301, 1, 0x0100);
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "set read-only"
    );
    call(&mut cpu, &mut memory, 0x4300, 0, 0x0100);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.cx() & 1, 1);
    // Clear the read-only attribute before deletion.
    call(&mut cpu, &mut memory, 0x4301, 0, 0x0100);
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "clear read-only"
    );
    call(&mut cpu, &mut memory, 0x4100, 0, 0x0100);
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "delete existing file"
    );
    assert_eq!(cpu.regs.ax(), 0);
    assert!(!temp_dir.path().join("TEST.TMP").exists());
}

#[test]
fn test_int21_create_file_invalid_drive_ah3c() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    write_asciiz(&mut memory, 0x2000, 0x0100, "Z:\\BAD.TXT");

    cpu.regs.set_ah(0x3C);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.set_cx(0);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 15);
}

#[test]
#[allow(clippy::permissions_set_readonly_false)]
fn test_int21_create_file_permission_denied() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("READONLY.TXT");
    std::fs::write(&file_path, "data").unwrap();

    let mut permissions = std::fs::metadata(&file_path).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&file_path, permissions).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"C:\\READONLY.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x3C);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.set_cx(0);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 5);

    let mut permissions = std::fs::metadata(&file_path).unwrap().permissions();
    permissions.set_readonly(false);
    std::fs::set_permissions(&file_path, permissions).unwrap();
}

#[test]
fn test_int21_create_file_permission_error_mapping() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.drives.insert(2, Box::new(DenyWriteDrive));

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"C:\\DENY.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x3C);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.set_cx(0);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 5);
}

#[test]
fn test_int21_create_file_success() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"C:\\NEWFILE.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x3C);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.set_cx(0);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let created = temp_dir.path().join("NEWFILE.TXT");
    assert!(created.exists());
}

#[test]
fn test_int21_create_file_invalid_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    write_asciiz(&mut memory, 0x2000, 0x0100, "Z:\\BAD.TXT");

    cpu.regs.set_ah(0x3C);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.set_cx(0);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 15);
}

#[test]
fn test_int21_create_file_not_found_error_mapping() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.drives.insert(2, Box::new(NotFoundDrive));

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\MISSING\\FILE.TXT");

    cpu.regs.set_ah(0x3C);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.set_cx(0);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3);
}

#[test]
fn test_int21_create_file_no_such_file_error_mapping() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.drives.insert(2, Box::new(NoSuchFileDrive));

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\MISSING.TXT");

    cpu.regs.set_ah(0x3C);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.set_cx(0);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3);
}

#[test]
fn test_int21_open_file_ah3d() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Try to open non-existent file
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"NONEXISTENT_FILE.DAT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x00); // Read only
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
    // Should fail with carry flag
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_open_emmxxxx0_device_disabled_no_ems() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.set_ems_present(false);
    cpu.regs.ss = 0x2000;
    cpu.regs.set_sp(0xFF00);
    memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp().wrapping_add(4), 0x0200);
    Dos::init_indos_flag(&mut memory);

    write_asciiz(&mut memory, 0x2000, 0x0100, "EMMXXXX0");

    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x00);
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_ne!(cpu.regs.ax(), 0x00FF);
}

#[test]
fn test_int21_open_nul_device() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.ss = 0x2000;
    cpu.regs.set_sp(0xFF00);
    memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp().wrapping_add(4), 0x0200);

    write_asciiz(&mut memory, 0x2000, 0x0100, "NUL");

    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x00);
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
fn test_int21_open_invalid_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    write_asciiz(&mut memory, 0x2000, 0x0100, "Z:\\MISSING.TXT");

    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x00);
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 15);
}

#[test]
fn test_int21_open_trace_success() {
    let _lock = env_lock();
    let _trace = trace_subscriber("gp64::dos=debug");
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("OPENOK.TXT");
    std::fs::write(&file_path, b"ok").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\OPENOK.TXT");

    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x00);
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
fn test_int21_open_trace_read_only() {
    let _lock = env_lock();
    let _trace = trace_subscriber("gp64::dos=debug");

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.drives.insert(2, Box::new(ReadOnlyDrive));

    cpu.regs.ss = 0x2000;
    cpu.regs.set_sp(0xFF00);
    memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp().wrapping_add(4), 0x0200);

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\READONLY.TXT");

    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x01);
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 5);
}

#[test]
fn test_int21_open_trace_too_many_files() {
    let _lock = env_lock();
    let _trace = trace_subscriber("gp64::dos=debug");
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("FULL.TXT");
    std::fs::write(&file_path, b"ok").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    // Grow the JFT to its maximum and fill every user handle (5..255).
    dos.file_table.set_handle_count(255);
    for i in 0..(255 - 5) {
        let handle = dos.file_table.allocate(
            Box::new(FailingReadFile),
            format!("C:\\FILL{i}.TXT"),
            dos.current_psp(),
        );
        assert!(handle.is_some());
    }

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\FULL.TXT");

    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x00);
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 4);
}

#[test]
fn test_int21_open_trace_file_not_found() {
    let _lock = env_lock();
    let _trace = trace_subscriber("gp64::dos=debug");
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\MISSING.TXT");

    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x00);
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 2);
}

/// DOS separates a missing file (AX=2) from a missing directory component
/// (AX=3). Period installers branch on 3 to decide whether to MKDIR, so
/// collapsing both onto 2 tells them the wrong thing.
#[test]
fn test_int21_open_missing_directory_returns_path_not_found() {
    let _lock = env_lock();
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\SAVE\\MISSING.CFG");

    // The directory component is absent.
    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x00);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    )
    .unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3, "missing directory component is error 3");

    // Directory now present, file still absent.
    std::fs::create_dir(temp_dir.path().join("SAVE")).unwrap();
    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x00);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    )
    .unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(
        cpu.regs.ax(),
        2,
        "missing file in an existing directory is error 2"
    );
}

/// MDD-BUG-FLU-02715: an empty FindFirst search reports NO_MORE_FILES,
/// including literal names. the DOS model src/dos/drive_local.cpp delegates
/// FindFirst to FindNext, which sets DOSERR_NO_MORE_FILES. Microsoft's
/// MS-DOS v2.0/source/SYSCALL.txt FindFirst "Error Returns" explicitly
/// assigns error_no_more_files when no files match. Missing directories
/// remain PATH_NOT_FOUND (the DOS model FindFirst's opendir failure).
/// An exhausted directory search reports extended error 18.
#[test]
fn test_int21_find_first_missing_directory_returns_path_not_found() {
    let _lock = env_lock();
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let temp_dir = TempDir::new().unwrap();
    std::fs::create_dir(temp_dir.path().join("EMPTY")).unwrap();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    for (pattern, expected) in [
        ("C:\\EMPTY\\MISSING.TXT", 18),
        ("C:\\EMPTY\\*.XYZ", 18),
        ("C:\\EMPTY\\*.*", 18),
        ("C:\\NOTHING.XYZ", 18),
        ("C:\\NODIR\\MISSING.TXT", 3),
        ("C:\\NODIR\\*.*", 3),
    ] {
        write_asciiz(&mut memory, 0x2000, 0x0100, pattern);
        cpu.regs.set_ah(0x4E);
        cpu.regs.set_cx(0);
        cpu.regs.ds = 0x2000;
        cpu.regs.set_dx(0x0100);
        dos.int21(
            &mut cpu,
            &mut memory,
            &mut vga,
            &mut keyboard,
            &mut Pic::new(),
        )
        .unwrap();
        assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY), "{pattern}");
        assert_eq!(cpu.regs.ax(), expected, "FindFirst {pattern}");

        cpu.regs.set_ah(0x59);
        cpu.regs.set_bx(0);
        dos.int21(
            &mut cpu,
            &mut memory,
            &mut vga,
            &mut keyboard,
            &mut Pic::new(),
        )
        .unwrap();
        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(cpu.regs.ax(), expected, "AH=59h after FindFirst {pattern}");
        // MS-DOS v4.0/src/DOS/MS_TABLE.ASM ErrTab maps both errors
        // 3 and 18 to NotFnd/User/Disk (class 8, action 3, locus 2).
        assert_eq!((cpu.regs.bh(), cpu.regs.bl(), cpu.regs.ch()), (8, 3, 2));
    }
}

/// AH=6Ch extended open and its LFN twin AX=716Ch split the same way.
/// the DOS model reaches it by delegating to `DOS_OpenFile`, which tests
/// `PathExists` before choosing between the two codes.
#[test]
fn test_int21_extended_open_missing_directory_returns_path_not_found() {
    let _lock = env_lock();
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );
    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\SAVE\\GAME.DAT");

    // DX action bit 0 = open if it exists; bit 4 (create) deliberately clear.
    let mut open_ex = |dos: &mut Dos, cpu: &mut gp64_cpu::Cpu, memory: &mut Memory, ax: u16| {
        cpu.regs.set_ax(ax);
        cpu.regs.set_bx(0x0000); // access mode: read
        cpu.regs.set_cx(0x0000); // create attributes
        cpu.regs.set_dx(0x0001); // action: open existing only
        cpu.regs.ds = 0x2000;
        cpu.regs.set_si(0x0100);
        dos.int21(cpu, memory, &mut vga, &mut keyboard, &mut Pic::new())
            .unwrap();
        assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        cpu.regs.ax()
    };

    assert_eq!(
        open_ex(&mut dos, &mut cpu, &mut memory, 0x6C00),
        3,
        "AH=6Ch: absent directory component is error 3"
    );
    assert_eq!(
        open_ex(&mut dos, &mut cpu, &mut memory, 0x716C),
        3,
        "AX=716Ch: absent directory component is error 3"
    );

    std::fs::create_dir(temp_dir.path().join("SAVE")).unwrap();
    assert_eq!(
        open_ex(&mut dos, &mut cpu, &mut memory, 0x6C00),
        2,
        "AH=6Ch: absent file in a present directory is error 2"
    );
    assert_eq!(
        open_ex(&mut dos, &mut cpu, &mut memory, 0x716C),
        2,
        "AX=716Ch: absent file in a present directory is error 2"
    );
}

/// AH=6Ch must leave the extended error agreeing with the AX it just
/// returned, so a following AH=59h does not read a stale value from an
/// earlier call. Its LFN twin already did this; the plain handler set AX
/// alone. MDD-BUG-CRUCIBLE-00063.
#[test]
fn test_int21_6ch_failures_refresh_the_extended_error() {
    let _lock = env_lock();
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );
    std::fs::write(temp_dir.path().join("THERE.DAT"), b"x").unwrap();

    let mut open_6c =
        |dos: &mut Dos, cpu: &mut gp64_cpu::Cpu, memory: &mut Memory, name: &str, action: u16| {
            write_asciiz(memory, 0x2000, 0x0100, name);
            cpu.regs.set_ax(0x6C00);
            cpu.regs.set_bx(0x0000);
            cpu.regs.set_cx(0x0000);
            cpu.regs.set_dx(action);
            cpu.regs.ds = 0x2000;
            cpu.regs.set_si(0x0100);
            dos.int21(cpu, memory, &mut vga, &mut keyboard, &mut Pic::new())
                .unwrap();
            assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
            cpu.regs.ax()
        };

    // Seed a different extended error, so a stale value is visible.
    dos.set_extended_error(5);

    // No such drive -> 3.
    let ax = open_6c(&mut dos, &mut cpu, &mut memory, "Z:\\ANY.DAT", 0x0001);
    assert_eq!(ax, 3);
    assert_eq!(dos.extended_error.0, 3, "no-such-drive must refresh AH=59h");

    // Exists, but open-if-exists not requested -> 80.
    dos.set_extended_error(5);
    let ax = open_6c(&mut dos, &mut cpu, &mut memory, "C:\\THERE.DAT", 0x0010);
    assert_eq!(ax, 80);
    assert_eq!(dos.extended_error.0, 80, "file-exists must refresh AH=59h");

    // Absent, and create not requested -> 2.
    dos.set_extended_error(5);
    let ax = open_6c(&mut dos, &mut cpu, &mut memory, "C:\\GONE.DAT", 0x0001);
    assert_eq!(ax, 2);
    assert_eq!(dos.extended_error.0, 2, "not-found must refresh AH=59h");
}

/// AH=3Dh takes the early-IRET route, so its result has to be written into
/// the FLAGS word already on the stack — the caller's IRET restores that,
/// not the live register. Both directions matter: a device open must CLEAR
/// the stacked carry, and a failed open must SET it. (This used to open
/// `AUX` purely as a way of producing a failure; AUX now succeeds, which
/// is what makes it the success half of the pair.)
#[test]
fn test_int21_open_syncs_carry_onto_iret_stack_both_ways() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let _temp = dev_mount_empty_c(&mut dos);

    cpu.regs.ss = 0x2000;
    cpu.regs.set_sp(0xFF00);
    let flags_addr = cpu.regs.sp().wrapping_add(4);

    // Success: AUX opens, and the stale carry on the stack is cleared.
    memory.write_u16_seg(cpu.regs.ss, flags_addr, 0x0201);
    write_asciiz(&mut memory, 0x2000, 0x0100, "AUX");
    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x00); // Read only
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
    assert_eq!(
        memory.read_u16_seg(cpu.regs.ss, flags_addr) & 1,
        0,
        "a successful open must clear the stacked carry"
    );

    // Failure: an absent file sets it again.
    memory.write_u16_seg(cpu.regs.ss, flags_addr, 0x0200);
    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\NOSUCH.DAT");
    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x00);
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(
        memory.read_u16_seg(cpu.regs.ss, flags_addr) & 1,
        1,
        "a failed open must set the stacked carry"
    );
}

#[test]
fn test_int21_set_file_date_time_ah57_al01() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x57);
    cpu.regs.set_al(0x01); // Set file date/time
    cpu.regs.set_bx(0); // STDIN handle (won't work but tests path)
    cpu.regs.set_cx(0x1234); // Time
    cpu.regs.set_dx(0x5678); // Date

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
fn test_int21_get_set_file_date_time_valid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = std::env::temp_dir().join("gp64_ah57_test");
    let _ = std::fs::create_dir(&temp_dir);
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(temp_dir.clone())),
    );

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"C:\\AH57.TMP\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x3C);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.set_cx(0);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.ax();

    cpu.regs.set_ah(0x57);
    cpu.regs.set_al(0x00);
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

    cpu.regs.set_ah(0x57);
    cpu.regs.set_al(0x01);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(0x1234);
    cpu.regs.set_dx(0x5678);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    cpu.regs.set_ah(0x57);
    cpu.regs.set_al(0x02);
    cpu.regs.set_bx(handle);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1);

    dos.file_table.free(handle);
    let _ = std::fs::remove_file(temp_dir.join("AH57.TMP"));
    let _ = std::fs::remove_dir(&temp_dir);
}

#[test]
fn test_int21_ioctl_unknown_subfunction() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0xFF); // Unknown subfunction
    cpu.regs.set_bx(0);

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
fn test_int21_terminate_ah00() {
    // INT 21h AH=00h (legacy CP/M terminate) must route through the same
    // process-termination path as AH=4Ch, with the exit code forced to 0
    // regardless of the incoming AL. For a child process it returns to the
    // parent (NoIret), restoring the parent PSP and CS:IP.
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Push the parent's saved state. As EXEC builds it (process_ops.rs),
    // the pushed state IS the parent: psp_segment == parent_psp, and
    // terminate_process restores current_psp from psp_segment.
    let regs = gp64_cpu::Registers::new();
    dos.process_stack.push(ProcessState {
        psp_segment: 0x0100,
        registers: regs.clone(),
        parent_psp: 0x0100,
        exec_ret_ip: 0x0200,
        exec_ret_cs: 0x3000,
        exec_ret_flags: 0x0202,
        pm_parent: false,
    });
    // Current PSP is the child; termination must restore the parent (0x0100).
    dos.set_current_psp(0x2000);
    // EXEC points the child's PSP:0Ah at the return address.
    memory.write_u32(0x2000 * 16 + PSP_INT22_TERMINATE, 0x3000_0200);

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x05); // Deliberately non-zero: AH=00h must ignore it.

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Child path returns Ok (NoIret) — control redirected to the parent.
    assert!(result.is_ok());
    // Exit code is forced to 0; termination type is "normal" (0x00).
    assert_eq!(dos.last_exit_code, 0);
    assert_eq!(dos.last_termination_type, 0x00);
    // Parent PSP restored.
    assert_eq!(dos.current_psp(), 0x0100);
    // CS:IP from the child's PSP:0Ah, the EXEC return address.
    assert_eq!(cpu.regs.cs, 0x3000);
    assert_eq!(cpu.regs.ip(), 0x0200);
}

#[test]
fn test_int21_terminate_ah00_exit_code_via_ah4d() {
    // After AH=00h terminates a child, AH=4Dh (Get Return Code) must report
    // AL=0 (exit code) and AH=0 (normal termination), i.e. AX == 0x0000.
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let regs = gp64_cpu::Registers::new();
    dos.process_stack.push(ProcessState {
        psp_segment: 0x0100,
        registers: regs.clone(),
        parent_psp: 0x0100,
        exec_ret_ip: 0x0200,
        exec_ret_cs: 0x3000,
        exec_ret_flags: 0x0202,
        pm_parent: false,
    });
    dos.set_current_psp(0x2000);

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x05); // Non-zero: ignored by AH=00h.
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    // Now query the return code.
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
fn test_int21_terminate_ah00_root_process_errors() {
    // For the root process (empty process_stack), AH=00h ends the run by
    // returning Err with a "Program terminated"-prefixed message (so
    // handle_step_error treats it as a clean exit), and the recorded exit
    // code is forced to 0 regardless of the incoming AL.
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Seed a non-zero sentinel so the post-call assertion proves the AH=00h
    // arm actually routed through terminate_process (which overwrites it),
    // rather than the field merely retaining its default of 0.
    dos.last_exit_code = 0xFF;

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x07); // Non-zero: must still produce exit code 0.

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .starts_with("Program terminated"));
    assert_eq!(dos.last_exit_code, 0);
}

#[test]
fn test_int21_ioctl_al0d() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x0D); // Generic I/O control
    cpu.regs.set_bx(2); // Drive C:
    cpu.regs.set_cx(0x0860); // Category/function

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
fn test_int21_lseek_ah42_from_current() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x42);
    cpu.regs.set_al(0x01); // Seek from current
    cpu.regs.set_bx(99); // Invalid handle
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(0);

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
fn test_int21_lseek_ah42_from_end() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x42);
    cpu.regs.set_al(0x02); // Seek from end
    cpu.regs.set_bx(99); // Invalid handle
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(0);

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
fn test_int21_dup2_ah46() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x46);
    cpu.regs.set_bx(0); // STDIN
    cpu.regs.set_cx(5); // New handle

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
fn test_try_read_file_by_dos_name_with_extension() {
    let mut dos = Dos::new();
    let cpu = Cpu::new();
    let memory = Memory::new();

    // Create a temp file to read
    let temp_path = std::env::temp_dir().join("test_dos_read.txt");
    let _ = std::fs::write(&temp_path, "test");

    // This just exercises the code path
    let _ = dos.try_read_file_by_dos_name(&cpu, &memory, 0, 0);

    let _ = std::fs::remove_file(&temp_path);
}

#[test]
fn test_process_state_debug() {
    let regs = gp64_cpu::Registers::new();
    let state = ProcessState {
        psp_segment: 0x1000,
        registers: regs.clone(),
        parent_psp: 0x0100,
        exec_ret_ip: 0x0200,
        exec_ret_cs: 0x3000,
        exec_ret_flags: 0x0202,
        pm_parent: false,
    };
    let debug = format!("{state:?}");
    assert!(debug.contains("ProcessState"));
}

#[test]
fn test_int21_exec_ah4b_file_not_found() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Set up filename at DS:DX
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"NONEXISTENT.EXE\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    // Set up parameter block at ES:BX
    let pb_addr = Memory::seg_offset_to_linear(0x3000, 0x0200);
    memory.write_u16(pb_addr, 0); // env segment
    memory.write_u32(pb_addr + 2, 0); // cmdline ptr
    memory.write_u32(pb_addr + 6, 0); // fcb1 ptr
    memory.write_u32(pb_addr + 10, 0); // fcb2 ptr

    cpu.regs.set_ah(0x4B);
    cpu.regs.set_al(0x00); // Load and execute
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_bx(0x0200);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // Should set carry (file not found)
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_exec_load_only_com_success() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.dos_mem.init(&mut memory);

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("TEST.COM");
    std::fs::write(&file_path, [0x90u8, 0xC3]).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"C:\\TEST.COM\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_bx(0x0200);

    let result = dos.exec_load_only(&mut cpu, &mut memory);
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let param_addr = Memory::seg_offset_to_linear(0x3000, 0x0200);
    let sp = memory.read_u16(param_addr + 0x0E);
    let ss = memory.read_u16(param_addr + 0x10);
    let ip = memory.read_u16(param_addr + 0x12);
    let cs = memory.read_u16(param_addr + 0x14);
    assert_eq!(sp, 0xFFFE);
    assert_eq!(ip, 0x0100);
    assert_eq!(ss, cs);

    let load_addr = Memory::seg_offset_to_linear(cs, 0x0100);
    assert_eq!(memory.read_u8(load_addr), 0x90);
    assert_eq!(memory.read_u8(load_addr + 1), 0xC3);
}

#[test]
fn test_exec_load_only_com_mz_prefix_not_exe() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.dos_mem.init(&mut memory);

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("MZLIKE.COM");
    std::fs::write(&file_path, [b'M', b'X', 0x90]).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\MZLIKE.COM");

    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_bx(0x0200);

    let result = dos.exec_load_only(&mut cpu, &mut memory);
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let param_addr = Memory::seg_offset_to_linear(0x3000, 0x0200);
    let ip = memory.read_u16(param_addr + 0x12);
    assert_eq!(ip, 0x0100);
}

#[test]
fn test_exec_load_only_com_uses_env_segment() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.dos_mem.init(&mut memory);

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("ENV.COM");
    std::fs::write(&file_path, [0x90u8]).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"C:\\ENV.COM\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    let param_addr = Memory::seg_offset_to_linear(0x3000, 0x0200);
    memory.write_u16(param_addr, 0x4000);
    memory.write_u32(param_addr + 2, 0);

    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_bx(0x0200);

    let result = dos.exec_load_only(&mut cpu, &mut memory);
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let sp = memory.read_u16(param_addr + 0x0E);
    let ss = memory.read_u16(param_addr + 0x10);
    let psp_base = Memory::seg_offset_to_linear(ss, 0);
    assert_eq!(sp, 0xFFFE);
    assert_eq!(memory.read_u16(psp_base + 0x2C), 0x4000);
}

#[test]
fn test_exec_load_only_exe_success() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.dos_mem.init(&mut memory);

    let mut exe_data = vec![0u8; 528];
    exe_data[0] = b'M';
    exe_data[1] = b'Z';
    exe_data[2] = 16;
    exe_data[4] = 2;
    exe_data[6] = 0;
    exe_data[8] = 32;
    exe_data[14] = 0x00;
    exe_data[15] = 0x00;
    exe_data[16] = 0xFE;
    exe_data[17] = 0xFF;
    exe_data[20] = 0x00;
    exe_data[21] = 0x00;
    exe_data[22] = 0x00;
    exe_data[23] = 0x00;
    exe_data[24] = 28;
    exe_data[25] = 0;
    exe_data[512] = 0xCD;
    exe_data[513] = 0x20;

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("TEST.EXE");
    std::fs::write(&file_path, &exe_data).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"C:\\TEST.EXE\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_bx(0x0200);

    let result = dos.exec_load_only(&mut cpu, &mut memory);
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let param_addr = Memory::seg_offset_to_linear(0x3000, 0x0200);
    let sp = memory.read_u16(param_addr + 0x0E);
    let ss = memory.read_u16(param_addr + 0x10);
    let ip = memory.read_u16(param_addr + 0x12);
    let cs = memory.read_u16(param_addr + 0x14);
    assert_eq!(sp, 0xFFFE);
    assert_eq!(ip, 0x0000);
    assert_eq!(ss, cs);
}

#[test]
fn test_exec_load_only_exe_bad_header_size() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.dos_mem.init(&mut memory);

    let mut exe_data = vec![0u8; 32];
    exe_data[0] = b'M';
    exe_data[1] = b'Z';
    exe_data[8] = 3;

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("BADHDR.EXE");
    std::fs::write(&file_path, &exe_data).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"C:\\BADHDR.EXE\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_bx(0x0200);

    let result = dos.exec_load_only(&mut cpu, &mut memory);
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 11);
}

#[test]
fn test_exec_program_com_trace_enabled() {
    let _trace = trace_subscriber("gp64::dos::exec=debug");
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.dos_mem.init(&mut memory);

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("PROGRAM.COM");
    std::fs::write(&file_path, [0xC3u8]).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"PROGRAM\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.es = 0x0000;
    cpu.regs.set_bx(0x0000);
    cpu.regs.ss = 0x3000;
    cpu.regs.set_sp(0x0200);
    memory.write_u16_seg(0x3000, 0x0200, 0x1111);
    memory.write_u16_seg(0x3000, 0x0202, 0x2222);
    memory.write_u16_seg(0x3000, 0x0204, 0x0202);

    let result = dos.exec_program(&mut cpu, &mut memory);
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ip(), 0x0100);
    assert_eq!(dos.process_stack.len(), 1);
}

#[test]
fn test_exec_program_com_uses_env_segment() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    use tempfile::TempDir;

    dos.dos_mem.init(&mut memory);

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("ENVPROG.COM");
    std::fs::write(&file_path, [0xC3u8]).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"ENVPROG.COM\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    let param_addr = Memory::seg_offset_to_linear(0x3000, 0x0100);
    memory.write_u16(param_addr, 0x4000);
    memory.write_u32(param_addr + 2, 0);

    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_bx(0x0100);
    cpu.regs.ss = 0x3000;
    cpu.regs.set_sp(0x0200);
    memory.write_u16_seg(0x3000, 0x0200, 0x1111);
    memory.write_u16_seg(0x3000, 0x0202, 0x2222);
    memory.write_u16_seg(0x3000, 0x0204, 0x0202);

    let result = dos.exec_program(&mut cpu, &mut memory);
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let psp_base = Memory::seg_offset_to_linear(dos.current_psp(), 0);
    // The child's env_seg is a clone of the parent's 0x4000, so it should be non-zero
    // but not necessarily 0x4000 (clone_env_for_exec allocates a new block).
    let child_env_seg = memory.read_u16(psp_base + 0x2C);
    assert_ne!(child_env_seg, 0, "child env_seg should be non-zero");
}

#[test]
fn test_exec_program_full_process_stack_returns_error_not_panic() {
    // MDD-BUG-CRUCIBLE-00058: a nested EXEC that reaches the 16-entry process
    // stack depth must fail with DOS "insufficient memory" (AX=8), not panic in
    // FixedVec::push — and must not commit a partial process entry.
    use tempfile::TempDir;
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.dos_mem.init(&mut memory);

    let temp_dir = TempDir::new().unwrap();
    std::fs::write(temp_dir.path().join("PROG.COM"), [0xC3u8]).unwrap();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    // Fill the 16-entry process stack to the boundary.
    let frame = ProcessState {
        psp_segment: 0,
        registers: cpu.regs.clone(),
        parent_psp: 0,
        exec_ret_ip: 0,
        exec_ret_cs: 0,
        exec_ret_flags: 0,
        pm_parent: false,
    };
    for _ in 0..16 {
        dos.process_stack.push(frame.clone());
    }
    assert!(dos.process_stack.is_full(), "process stack should be full");

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"PROG.COM\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }
    let param_addr = Memory::seg_offset_to_linear(0x3000, 0x0100);
    memory.write_u16(param_addr, 0x4000);
    memory.write_u32(param_addr + 2, 0);

    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_bx(0x0100);
    cpu.regs.ss = 0x3000;
    cpu.regs.set_sp(0x0200);

    dos.exec_program(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(
        cpu.regs.ax(),
        8,
        "a full process stack must fail EXEC with DOS insufficient memory"
    );
    assert_eq!(
        dos.process_stack.len(),
        16,
        "a refused EXEC must not push a partial process entry"
    );
}

#[test]
fn test_exec_program_com_non_mz_two_bytes() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.dos_mem.init(&mut memory);

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("NONMZ.COM");
    std::fs::write(&file_path, [0x90u8, 0x90]).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, "NONMZ.COM");

    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.es = 0x0000;
    cpu.regs.set_bx(0x0000);
    cpu.regs.ss = 0x3000;
    cpu.regs.set_sp(0x0200);
    memory.write_u16_seg(0x3000, 0x0200, 0x1111);
    memory.write_u16_seg(0x3000, 0x0202, 0x2222);
    memory.write_u16_seg(0x3000, 0x0204, 0x0202);

    let result = dos.exec_program(&mut cpu, &mut memory);
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ip(), 0x0100);
}

#[test]
fn test_exec_program_com_mz_prefix_not_exe() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.dos_mem.init(&mut memory);

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("MZLIKE.COM");
    std::fs::write(&file_path, [b'M', b'X', 0x90]).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, "MZLIKE.COM");

    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.es = 0x0000;
    cpu.regs.set_bx(0x0000);
    cpu.regs.ss = 0x3000;
    cpu.regs.set_sp(0x0200);
    memory.write_u16_seg(0x3000, 0x0200, 0x1111);
    memory.write_u16_seg(0x3000, 0x0202, 0x2222);
    memory.write_u16_seg(0x3000, 0x0204, 0x0202);

    let result = dos.exec_program(&mut cpu, &mut memory);
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ip(), 0x0100);
}

#[test]
fn test_exec_program_exe_rolls_back_on_error() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.dos_mem.init(&mut memory);

    let mut exe_data = vec![0u8; 32];
    exe_data[0] = b'M';
    exe_data[1] = b'Z';
    exe_data[8] = 32;

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("BAD.EXE");
    std::fs::write(&file_path, &exe_data).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"BAD.EXE\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ax(0x1234);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.es = 0x0000;
    cpu.regs.set_bx(0x0000);
    cpu.regs.ss = 0x3000;
    cpu.regs.set_sp(0x0200);
    memory.write_u16_seg(0x3000, 0x0200, 0x1111);
    memory.write_u16_seg(0x3000, 0x0202, 0x2222);
    memory.write_u16_seg(0x3000, 0x0204, 0x0202);

    let result = dos.exec_program(&mut cpu, &mut memory);
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 11);
    assert_eq!(dos.current_psp(), 0x0100);
    assert!(dos.process_stack.is_empty());
}

#[test]
fn test_exec_load_exe_rejects_short_header() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let exe_data = vec![0x00u8; 10];
    let result = dos.exec_load_exe(
        &mut cpu,
        &mut memory,
        &exe_data,
        0,
        None,
        None,
        None,
        0x0100,
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 11);
}

#[test]
fn test_exec_load_exe_rejects_bad_signature_first_byte() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let exe_data = vec![b'N'; 28];
    let result = dos.exec_load_exe(
        &mut cpu,
        &mut memory,
        &exe_data,
        0,
        None,
        None,
        None,
        0x0100,
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 11);
}

#[test]
fn test_exec_load_exe_rejects_bad_signature_second_byte() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let mut exe_data = vec![0u8; 28];
    exe_data[0] = b'M';
    exe_data[1] = b'X';

    let result = dos.exec_load_exe(
        &mut cpu,
        &mut memory,
        &exe_data,
        0,
        None,
        None,
        None,
        0x0100,
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 11);
}

#[test]
fn test_exec_load_exe_accepts_valid_header() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.dos_mem.init(&mut memory);

    let mut exe_data = vec![0u8; 544];
    exe_data[0] = b'M';
    exe_data[1] = b'Z';
    exe_data[2] = 32;
    exe_data[4] = 2;
    exe_data[6] = 0;
    exe_data[8] = 32;
    exe_data[24] = 28;

    exe_data[512] = 0xCD;
    exe_data[513] = 0x20;

    let result = dos.exec_load_exe(
        &mut cpu,
        &mut memory,
        &exe_data,
        0,
        None,
        None,
        None,
        0x0100,
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_exec_load_exe_sizes_image_from_header_not_file() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.dos_mem.init(&mut memory);

    let header_paragraphs = 2u16;
    let header_size = header_paragraphs as usize * 16;
    let declared_size = header_size + 2;
    let mut exe_data = vec![0u8; declared_size + 16];
    exe_data[0] = b'M';
    exe_data[1] = b'Z';
    exe_data[2] = declared_size as u8;
    exe_data[3] = (declared_size >> 8) as u8;
    exe_data[4] = 1;
    exe_data[5] = 0;
    exe_data[8] = header_paragraphs as u8;
    exe_data[9] = 0;
    exe_data[24] = 28;
    exe_data[25] = 0;
    exe_data[header_size] = 0xCD;
    exe_data[header_size + 1] = 0x20;
    exe_data[declared_size..].fill(0xCC);

    let result = dos.exec_load_exe(
        &mut cpu,
        &mut memory,
        &exe_data,
        0,
        None,
        None,
        None,
        0x0100,
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let load_segment = dos.current_psp().wrapping_add(0x10);
    let load_addr = Memory::seg_offset_to_linear(load_segment, 0);
    assert_eq!(memory.read_u8(load_addr), 0xCD);
    assert_eq!(memory.read_u8(load_addr + 1), 0x20);
    assert_eq!(memory.read_u8(load_addr + 2), 0x00);
}

#[test]
fn test_exec_load_exe_insufficient_memory() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.dos_mem.init(&mut memory);
    let largest = dos.dos_mem.largest_free();
    let image_paras = largest.saturating_add(0x20);
    let header_paragraphs = 32u16;
    let header_size = header_paragraphs as usize * 16;
    let image_size = image_paras as usize * 16;
    let total_size = header_size + image_size;

    let mut exe_data = vec![0u8; total_size];
    exe_data[0] = b'M';
    exe_data[1] = b'Z';
    exe_data[8] = (header_paragraphs & 0xFF) as u8;
    exe_data[9] = (header_paragraphs >> 8) as u8;
    exe_data[10] = 0;
    exe_data[11] = 0;
    exe_data[24] = 28;
    exe_data[25] = 0;

    let result = dos.exec_load_exe(
        &mut cpu,
        &mut memory,
        &exe_data,
        0,
        None,
        None,
        None,
        0x0100,
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 8);
    assert_eq!(cpu.regs.bx(), largest);
}

#[test]
fn test_exec_apply_relocations_breaks_on_short_table() {
    let mut memory = Memory::new();
    let data = vec![0u8; 6];
    let load_segment = 0x2000;
    let addr = Memory::seg_offset_to_linear(load_segment, 0x0000);
    memory.write_u16(addr, 0x1234);

    Dos::exec_apply_relocations(&mut memory, &data, 4, 1, load_segment);

    assert_eq!(memory.read_u16(addr), 0x1234);
}

#[test]
fn test_int21_ioctl_al01_set_device_info() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x01); // Set device info
    cpu.regs.set_bx(0); // Handle
    cpu.regs.set_dx(0x0080); // Device info

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
fn test_int21_ioctl_al01_set_device_info_con_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x01); // Set device info
    cpu.regs.set_bx(0x00FE); // CON handle
    cpu.regs.set_dx(0x0080);

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
fn test_int21_ioctl_al01_set_device_info_file_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(Vec::new())),
            "C:\\IOCTL.TXT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x01); // Set device info
    cpu.regs.set_bx(handle);
    cpu.regs.set_dx(0x0080);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1);
}

#[test]
fn test_int21_ioctl_al08_removable_default_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x08); // Removable check
    cpu.regs.set_bl(0); // Default drive (C:)

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1);
}

#[test]
fn test_int21_ioctl_al08_removable_drive_a() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x08); // Removable check
    cpu.regs.set_bl(1); // A: drive

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0);
}

#[test]
fn test_int21_ioctl_al02_read() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x02); // Read from device
    cpu.regs.set_bx(0); // Handle
    cpu.regs.set_cx(10); // Bytes to read
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
}

#[test]
fn test_int21_ioctl_al03_write() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x03); // Write to device
    cpu.regs.set_bx(0); // Handle
    cpu.regs.set_cx(10); // Bytes to write
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
}

#[test]
fn test_int21_ioctl_al06_input_status() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x06); // Get input status
    cpu.regs.set_bx(0); // Handle

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
fn test_int21_ioctl_al07_output_status() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x07); // Get output status
    cpu.regs.set_bx(0); // Handle

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
fn test_int21_set_time_invalid_minutes_ah2d() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2D);
    cpu.regs.set_ch(12); // Valid hour
    cpu.regs.set_cl(60); // Invalid minute
    cpu.regs.set_dh(0);
    cpu.regs.set_dl(0);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0xFF);
}

#[test]
fn test_int21_set_time_invalid_seconds_ah2d() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2D);
    cpu.regs.set_ch(12); // Valid hour
    cpu.regs.set_cl(30); // Valid minute
    cpu.regs.set_dh(60); // Invalid second
    cpu.regs.set_dl(0);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0xFF);
}

#[test]
fn test_int21_ioctl_al06_input_status_file_handle() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("IOCTL.TXT");
    std::fs::write(&file_path, "data").unwrap();

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
    cpu.regs.set_al(0x00);
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
    let handle = cpu.regs.ax();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x06);
    cpu.regs.set_bx(handle);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0xFF);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    cpu.regs.set_ah(0x3E);
    cpu.regs.set_bx(handle);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
}

#[test]
fn test_int21_ioctl_al06_input_status_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x06);
    cpu.regs.set_bx(999);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 6);
}

#[test]
fn test_int21_ioctl_al07_output_status_file_handle() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("IOCTLOUT.TXT");
    std::fs::write(&file_path, "data").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"C:\\IOCTLOUT.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x00);
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
    let handle = cpu.regs.ax();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x07);
    cpu.regs.set_bx(handle);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0xFF);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    cpu.regs.set_ah(0x3E);
    cpu.regs.set_bx(handle);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
}

#[test]
fn test_int21_ioctl_al06_input_status_con_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x06);
    cpu.regs.set_bx(0x00FE);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0xFF);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_ioctl_al07_output_status_con_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // CON device (0xFE) is a valid character device for output status
    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x07);
    cpu.regs.set_bx(0x00FE);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0xFF); // Ready
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_ioctl_al06_input_status_ems_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // EMS device handle (0xFF) from opening EMMXXXX0 — must report ready
    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x06);
    cpu.regs.set_bx(0x00FF);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0xFF); // Ready
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_ioctl_al07_output_status_ems_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // EMS device handle (0xFF) from opening EMMXXXX0 — must report ready.
    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x07);
    cpu.regs.set_bx(0x00FF);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0xFF); // Ready
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_lock_unlock_standard_handle_ah5c() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x5C);
    cpu.regs.set_al(0); // Lock
    cpu.regs.set_bx(0); // STDIN
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(0);
    cpu.regs.set_si(0);
    cpu.regs.set_di(1);

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
fn test_find_first_valid_pattern() {
    let mut dos = Dos::new();

    // Create a temp directory with a test file
    let temp_dir = std::env::temp_dir().join("gp64_test_find3");
    let _ = std::fs::create_dir(&temp_dir);
    let test_file = temp_dir.join("TESTFILE.TXT");
    let _ = std::fs::write(&test_file, "test");

    // Try to find with valid pattern in temp dir
    // This exercises the find_first path
    let _ = dos.find_first(&format!("{}\\*.TXT", temp_dir.display()), 0xFF);

    // Cleanup
    let _ = std::fs::remove_file(&test_file);
    let _ = std::fs::remove_dir(&temp_dir);
}

#[test]
fn test_find_first_caps_oversized_match_set_no_panic() {
    // MDD-BUG-FLUXHOMEARPA-00008: a wildcard search matching more than the
    // FindSearchEntries capacity (4096) must cap gracefully, not panic on
    // FixedVec::push.
    let mut dos = Dos::new();
    dos.drives
        .insert(2, Box::new(ManyEntriesDrive { count: 4097 }));

    let result = dos.find_first("C:\\*.*", 0xFF);
    assert!(
        result.is_ok(),
        "find_first must cap an oversized match set, not panic/err: {result:?}"
    );
}

#[test]
fn test_find_get_entry_after_find_first() {
    use crate::filesystem::HostDrive;
    use tempfile::TempDir;

    let mut dos = Dos::new();

    // Create a temp directory with a test file
    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("GETENTRY.TXT");
    let _ = std::fs::write(&test_file, "test content");

    dos.drives
        .insert(2, Box::new(HostDrive::new(temp_dir.path().to_path_buf())));

    let search_id = dos
        .find_first("C:\\*.TXT", 0xFF)
        .expect("find_first should succeed");
    let entry = dos.find_get_entry(search_id);
    assert!(entry.is_some());
    assert!(entry.unwrap().name.to_uppercase().ends_with(".TXT"));
}

#[test]
fn test_interleaved_find_operations() {
    use crate::filesystem::HostDrive;

    let mut dos = Dos::new();

    // Create two temp directories with different files
    let temp_dir1 = std::env::temp_dir().join("gp64_test_interleave1");
    let temp_dir2 = std::env::temp_dir().join("gp64_test_interleave2");
    let _ = std::fs::create_dir(&temp_dir1);
    let _ = std::fs::create_dir(&temp_dir2);

    // Create files in each directory
    let file1a = temp_dir1.join("FILE1A.TXT");
    let file1b = temp_dir1.join("FILE1B.TXT");
    let file2a = temp_dir2.join("FILE2A.COM");
    let file2b = temp_dir2.join("FILE2B.COM");
    let _ = std::fs::write(&file1a, "dir1 file a");
    let _ = std::fs::write(&file1b, "dir1 file b");
    let _ = std::fs::write(&file2a, "dir2 file a");
    let _ = std::fs::write(&file2b, "dir2 file b");

    // Mount temp directories as drives D: and E:
    dos.drives
        .insert(3, Box::new(HostDrive::new(temp_dir1.clone())));
    dos.drives
        .insert(4, Box::new(HostDrive::new(temp_dir2.clone())));

    // Start search in dir1 (D:)
    let search1 = dos
        .find_first("D:\\*.TXT", 0xFF)
        .expect("find_first D: should succeed");

    // Start search in dir2 (E:) - interleaved
    let search2 = dos
        .find_first("E:\\*.COM", 0xFF)
        .expect("find_first E: should succeed");

    // Get first entry from search1
    let entry1 = dos.find_get_entry(search1);
    assert!(entry1.is_some());
    assert!(entry1.unwrap().name.to_uppercase().ends_with(".TXT"));

    // Get first entry from search2
    let entry2 = dos.find_get_entry(search2);
    assert!(entry2.is_some());
    assert!(entry2.unwrap().name.to_uppercase().ends_with(".COM"));

    // Advance search1 and get next
    dos.find_advance(search1);
    let entry1b = dos.find_get_entry(search1);
    assert!(entry1b.is_some());
    assert!(entry1b.unwrap().name.to_uppercase().ends_with(".TXT"));

    // Advance search2 and get next
    dos.find_advance(search2);
    let entry2b = dos.find_get_entry(search2);
    assert!(entry2b.is_some());
    assert!(entry2b.unwrap().name.to_uppercase().ends_with(".COM"));

    // Both searches should have reached end after 2 entries each
    dos.find_advance(search1);
    assert!(dos.find_get_entry(search1).is_none());
    dos.find_advance(search2);
    assert!(dos.find_get_entry(search2).is_none());

    // Cleanup
    let _ = std::fs::remove_file(&file1a);
    let _ = std::fs::remove_file(&file1b);
    let _ = std::fs::remove_file(&file2a);
    let _ = std::fs::remove_file(&file2b);
    let _ = std::fs::remove_dir(&temp_dir1);
    let _ = std::fs::remove_dir(&temp_dir2);
}
