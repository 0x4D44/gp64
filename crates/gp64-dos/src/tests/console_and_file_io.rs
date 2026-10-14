use super::*;

#[test]
fn test_ah3f_stdin_graphics_mode_waits_for_input() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0x0100);
    let frame = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.sp());
    memory.write_u16(frame, 0x0202);
    memory.write_u16(frame + 4, 0);
    dos.stdin_line_acc = vec![b'Q'];
    // Mode 13h = 320x200 graphics
    vga.set_mode_bios(0x13);
    assert!(dos.stdin_line_buf.is_empty());
    assert!(bda::kbd_peek(&memory).is_none());

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(0); // STDIN
    cpu.regs.set_cx(16); // one independently chosen paragraph of output space
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
    assert!(
        cpu.halted,
        "cooked console reads must wait in graphics mode"
    );
    assert_eq!(cpu.regs.ax(), 0x3F00, "a waiting read must not report EOF");
    assert_eq!(dos.stdin_line_acc, b"Q", "waiting preserves partial input");
    assert_eq!(memory.read_u16(frame), 0x0200, "retry rewinds INT 21h");
    assert!(
        cpu.interrupts_enabled,
        "keyboard interrupts can resume the read"
    );
    assert_ne!(memory.read_u16(frame + 4) & 0x0200, 0);
}

#[test]
fn test_ah3f_stdin_text_mode_blocks_normally() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Text mode 80x25
    vga.set_mode_bios(0x03);
    assert!(dos.stdin_line_buf.is_empty());

    // Set up an IRET frame so wait_for_input can rewind.
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0x0100);
    let ret_ip_addr = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.sp());
    memory.write_u16(ret_ip_addr, 0x2000);
    let flags_addr = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.sp().wrapping_add(4));
    memory.write_u16(flags_addr, 0x0200);

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(0);
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
    // Text-mode path takes wait_for_input → CPU halts, IP rewinds.
    assert!(cpu.halted, "text mode AH=3F BX=0 must block");
}

#[test]
fn test_ah3f_stdin_graphics_mode_drains_buffered_line_first() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    vga.set_mode_bios(0x13);
    // A completed line is returned before waiting for more input.
    dos.stdin_line_buf = vec![b'A', b'B', 0x0D, 0x0A];

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(0);
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
    assert_eq!(cpu.regs.ax(), 4);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    assert_eq!(memory.read_u8(addr), b'A');
    assert_eq!(memory.read_u8(addr + 1), b'B');
    assert_eq!(memory.read_u8(addr + 2), 0x0D);
    assert_eq!(memory.read_u8(addr + 3), 0x0A);
}

#[test]
fn test_ah3f_stdin_graphics_mode_with_pending_kbd_blocks() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    vga.set_mode_bios(0x13);
    assert!(dos.stdin_line_buf.is_empty());

    // Pending BDA input must be consumed by the line accumulator.  'x' is a
    // printable character that gets accumulated (line not yet complete),
    // so AH=3F re-enters via prep_input_wait (no halt, IP rewinds).
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0x0100);
    let ret_ip_addr = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.sp());
    memory.write_u16(ret_ip_addr, 0x2000);
    let flags_addr = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.sp().wrapping_add(4));
    memory.write_u16(flags_addr, 0x0200);
    bda::kbd_enqueue(&mut memory, 0x2D, b'x');

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(0);
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
    // The 'x' was consumed into the accumulator, not returned as EOF.
    assert_eq!(dos.stdin_line_acc, vec![b'x']);
    // Buffer is still empty (line not complete yet — needs CR).
    assert!(dos.stdin_line_buf.is_empty());
}

#[test]
fn test_int21_read_file_handle_ah3f_reads_bytes() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(b"HELLO".to_vec())),
            "C:\\READ.TXT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(5);
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
    assert_eq!(cpu.regs.ax(), 5);

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    let mut out = Vec::new();
    for i in 0..5 {
        out.push(memory.read_u8(addr + i));
    }
    assert_eq!(out.as_slice(), b"HELLO");
}

#[test]
fn test_int21_read_alias_handle_ah3f_maps_to_stdin() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(0x00FE); // Alias for STDIN
    cpu.regs.set_cx(4);
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
    // Cooked-mode stdin blocks when no input is available;
    // AX is left unchanged (still holds AH=0x3F, AL=0xFE from BX setup).
    assert_eq!(cpu.regs.ax(), 0x3F00);
}

#[test]
fn test_int21_write_stdout_ah40() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Set up buffer with "Hello"
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"Hello".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(1); // STDOUT
    cpu.regs.set_cx(5);
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
    assert_eq!(cpu.regs.ax(), 5); // Bytes written
}

#[test]
fn test_int21_write_alias_handle_ah40_trace_truncates() {
    let _lock = env_lock();
    let _max_guard = EnvVarGuard::set("GP64_DOS_TRACE", "print=1");

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, b'A');
    memory.write_u8(addr + 1, b'B');

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(0x00FE); // Alias for STDOUT
    cpu.regs.set_cx(2);
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
    assert_eq!(cpu.regs.ax(), 2);
}

#[test]
fn test_int21_write_stdout_ah40_trace_no_truncate() {
    let _lock = env_lock();
    let _max_guard = EnvVarGuard::set("GP64_DOS_TRACE", "print=8");

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, b'H');
    memory.write_u8(addr + 1, b'I');

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(1); // STDOUT
    cpu.regs.set_cx(2);
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
    assert_eq!(cpu.regs.ax(), 2);
}

#[test]
fn test_int21_write_file_handle_ah40_zero_count_truncates() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate_with_sharing(
            Box::new(CursorFile::new(b"DATA".to_vec())),
            "C:\\TRUNC.TXT".to_string(),
            dos.current_psp,
            crate::files::SHARE_COMPAT,
            crate::files::ACCESS_READWRITE,
        )
        .expect("expected handle");
    let sft = dos.file_table.get_sft_mut(handle).expect("expected handle");
    let _ = sft.file.seek(std::io::SeekFrom::Start(2));

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(0);
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
    assert_eq!(cpu.regs.ax(), 0);

    let sft = dos.file_table.get_sft_mut(handle).expect("expected handle");
    sft.file.seek(std::io::SeekFrom::Start(0)).unwrap();
    let mut out = Vec::new();
    sft.file.read_to_end(&mut out).unwrap();
    assert_eq!(out.as_slice(), b"DA");
}

#[test]
fn test_int21_write_file_handle_ah40_read_only_denied() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate_with_sharing(
            Box::new(CursorFile::new(b"DATA".to_vec())),
            "C:\\READONLY.TXT".to_string(),
            dos.current_psp,
            crate::files::SHARE_COMPAT,
            crate::files::ACCESS_READ,
        )
        .expect("expected handle");

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, b'X');

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(handle);
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 5);

    let sft = dos.file_table.get_sft_mut(handle).expect("expected handle");
    sft.file.seek(std::io::SeekFrom::Start(0)).unwrap();
    let mut out = Vec::new();
    sft.file.read_to_end(&mut out).unwrap();
    assert_eq!(out.as_slice(), b"DATA");
}

#[test]
fn test_int21_write_file_handle_ah40_zero_count_stream_position_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(SeekErrorFile),
            "C:\\TRUNCFAIL.TXT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(0);
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
fn test_int21_write_file_handle_ah40_zero_count_set_len_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let mut file = SetLenErrorFile::new();
    file.seek(SeekFrom::Start(u32::MAX as u64)).unwrap();
    let handle = dos
        .file_table
        .allocate(
            Box::new(file),
            "C:\\SETLENFAIL.TXT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(0);
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
fn test_int21_write_stderr_ah40() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, b'E');
    memory.write_u8(addr + 1, b'R');
    memory.write_u8(addr + 2, b'R');

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(2); // STDERR
    cpu.regs.set_cx(3);
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
    assert_eq!(cpu.regs.ax(), 3);
}

#[test]
fn test_stdout_capture_enabled() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.enable_stdout_capture();

    // Write "Hello" to stdout (handle 1)
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"Hello".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }
    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(1); // STDOUT
    cpu.regs.set_cx(5);
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
    assert_eq!(cpu.regs.ax(), 5);

    let captured = dos.take_stdout_capture();
    assert_eq!(captured, b"Hello");
}

#[test]
fn dos_write_marker_observes_ah40_stdout_fallback_route() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.configure_dos_write_marker(b"test-marker").unwrap();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &byte) in b"noise test-marker done".iter().enumerate() {
        memory.write_u8(addr + i, byte);
    }
    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(1);
    cpu.regs.set_cx(22);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);

    let disposition = dos
        .int21(
            &mut cpu,
            &mut memory,
            &mut vga,
            &mut keyboard,
            &mut Pic::new(),
        )
        .unwrap();

    assert!(matches!(disposition, Int21Disposition::Iret));
    assert_eq!(cpu.regs.ax(), 22);
    assert!(dos.take_dos_write_marker_hit());
    assert!(!dos.take_dos_write_marker_hit());
}

#[test]
fn test_stdout_capture_disabled_by_default() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Do NOT enable capture

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"Hello".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }
    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(1);
    cpu.regs.set_cx(5);
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

    // take_stdout_capture returns empty when capture was never enabled
    let captured = dos.take_stdout_capture();
    assert!(captured.is_empty());
}

#[test]
fn test_stdout_capture_accumulates() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.enable_stdout_capture();

    // First write: "AB"
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, b'A');
    memory.write_u8(addr + 1, b'B');
    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(1);
    cpu.regs.set_cx(2);
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

    // Second write: "CD"
    memory.write_u8(addr, b'C');
    memory.write_u8(addr + 1, b'D');
    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(1);
    cpu.regs.set_cx(2);
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

    let captured = dos.take_stdout_capture();
    assert_eq!(captured, b"ABCD");
}

#[test]
fn test_stdout_capture_take_clears_buffer() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.enable_stdout_capture();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, b'X');
    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(1);
    cpu.regs.set_cx(1);
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

    let first = dos.take_stdout_capture();
    assert_eq!(first, b"X");

    // After take, capture is disabled (Option is now None)
    let second = dos.take_stdout_capture();
    assert!(second.is_empty());
}

#[test]
fn test_int21_write_stdprn_ah40() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    memory.write_u8(Memory::seg_offset_to_linear(0x2000, 0x0100), b'P');
    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(4); // STDPRN
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
    // STDPRN is a real device now: the write succeeds and the byte lands
    // in the printer accumulator. It used to return access denied, which
    // silently swallowed every `fprintf(stdprn, …)`.
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1);
    assert_eq!(dos.device_bridge.lock().unwrap().take_prn_out(), b"P");
}

#[test]
fn test_int21_write_invalid_handle_ah40() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(99);
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 6); // Invalid handle
}

#[test]
fn test_int21_seek_file_handle_ah42_sets_position() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(b"ABCDE".to_vec())),
            "C:\\SEEK.TXT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");

    cpu.regs.set_ah(0x42);
    cpu.regs.set_al(0); // Seek from start
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(2);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 2);
    assert_eq!(cpu.regs.dx(), 0);
}

#[test]
fn test_int21_get_file_attr_ah43() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Set up a filename in memory
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"NONEXISTENT.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x43);
    cpu.regs.set_al(0x00); // Get attributes
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
    // File doesn't exist, should set carry
}

#[test]
fn test_int21_get_file_attr_missing_drive_ah43() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"Z:\\MISSING.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x43);
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
    assert_eq!(cpu.regs.ax(), 3);
}

#[test]
fn test_int21_get_file_attr_ah43_all_bits() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.drives.insert(3, Box::new(AllAttributesDrive));

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"D:\\ATTR.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x43);
    cpu.regs.set_al(0x00); // Get attributes
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
    assert_eq!(cpu.regs.cx(), 0x3F);
}

#[test]
#[allow(clippy::permissions_set_readonly_false)]
fn test_int21_get_file_attr_ah43_normal_file() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let host_path = temp_dir.path().join("NORMAL.TXT");
    std::fs::write(&host_path, b"data").unwrap();

    let mut permissions = std::fs::metadata(&host_path).unwrap().permissions();
    permissions.set_readonly(false);
    std::fs::set_permissions(&host_path, permissions).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\NORMAL.TXT");

    cpu.regs.set_ah(0x43);
    cpu.regs.set_al(0x00); // Get attributes
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
    assert_eq!(cpu.regs.cx(), 0x20);
}

#[test]
fn test_int21_get_file_attr_ah43_directory() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let dir_path = temp_dir.path().join("SUBDIR");
    std::fs::create_dir(&dir_path).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\SUBDIR");

    cpu.regs.set_ah(0x43);
    cpu.regs.set_al(0x00); // Get attributes
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
    assert_eq!(cpu.regs.cx(), 0x10);
}

#[test]
fn test_int21_set_file_attr_ah43() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = std::env::temp_dir().join("gp64_attr_test");
    let _ = std::fs::create_dir(&temp_dir);
    let test_file = temp_dir.join("ATTR.TXT");
    let _ = std::fs::write(&test_file, b"attr");

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(temp_dir.clone())),
    );

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"C:\\ATTR.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x43);
    cpu.regs.set_al(0x01); // Set attributes
    cpu.regs.set_cx(0);
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

    cpu.regs.set_ah(0x43);
    cpu.regs.set_al(0x02); // Invalid subfunction
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

    let _ = std::fs::remove_file(&test_file);
    let _ = std::fs::remove_dir(&temp_dir);
}

#[test]
fn test_int21_set_file_attr_nonexistent_ah43() {
    // Bug: When setting attributes on a non-existent file, INT 21h/43h AL=01
    // should return CF=1 and AX=2 (file not found), not AX=5 (access denied)
    // or leave AX unchanged (which was 0x4301).
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

    // Try to set attributes on a file that doesn't exist
    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\NONEXISTENT.TXT");

    cpu.regs.set_ah(0x43);
    cpu.regs.set_al(0x01); // Set attributes (subfunction 1)
    cpu.regs.set_cx(0); // Normal attribute
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
    // Should fail with carry flag set
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // Should return error code 2 (file not found), not 5 (access denied)
    assert_eq!(
        cpu.regs.ax(),
        2,
        "Expected AX=2 (file not found), got AX={}",
        cpu.regs.ax()
    );
}

#[test]
fn test_int21_find_first_ah4e_nonexistent() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"NONEXISTENT_DIR\\*.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x4E);
    cpu.regs.set_cx(0); // Normal files
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
    // Should set carry (no files found)
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_find_next_ah4f_no_find_first() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x4F);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // Should fail (no previous find first)
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_free_memory_ah49() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.dos_mem.init(&mut memory);

    cpu.regs.set_ah(0x49);
    cpu.regs.es = 0x2000; // Segment to free

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // Result depends on whether the segment is valid
}

#[test]
fn test_int21_free_memory_invalid_block_with_info_sets_ax9() {
    let _lock = env_lock();
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.dos_mem.init(&mut memory);
    let psp_seg = dos.dos_mem.alloc(&mut memory, 0x0100, 0x40).unwrap();
    let _ = dos.dos_mem.set_owner(&mut memory, psp_seg, 0x0100);
    dos.set_current_psp(0x0200); // Different PSP from the owner

    cpu.regs.set_ah(0x49);
    cpu.regs.es = psp_seg.wrapping_add(1); // Inside block -> info exists, but invalid free target

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 9);
}

#[test]
fn test_int21_free_memory_invalid_block_without_info_sets_ax9() {
    let _lock = env_lock();
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.dos_mem.init(&mut memory);
    cpu.regs.set_ah(0x49);
    cpu.regs.es = 0xDEAD; // No block info for this segment

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 9);
}

#[test]
fn test_int21_resize_memory_ah4a() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.dos_mem.init(&mut memory);

    cpu.regs.set_ah(0x4A);
    cpu.regs.es = 0x2000;
    cpu.regs.set_bx(0x0100); // New size

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // Result depends on memory state
}

#[test]
fn test_int21_print_string_ah09() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Set up string ending with $
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"Hello World$".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x09);
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
    // String should be printed
}

#[test]
fn test_int21_print_string_trace_max_terminates_loop() {
    let _lock = env_lock();
    let _guard_trace_max = EnvVarGuard::set("GP64_DOS_TRACE", "print=2");

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"ABCD$".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x09);
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
fn test_int21_print_string_trace_term_at_dollar() {
    let _lock = env_lock();
    let _guard_trace_max = EnvVarGuard::set("GP64_DOS_TRACE", "print=16");

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"HI$".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x09);
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
fn test_int21_unknown_function() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Try an unimplemented function
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
fn test_int21_lseek_ah42_start() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x42);
    cpu.regs.set_al(0x00); // Seek from start
    cpu.regs.set_bx(0); // STDIN
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
    // For stdin, should fail with invalid handle
}

#[test]
fn test_int21_lseek_ah42_invalid() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x42);
    cpu.regs.set_al(0x00);
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
}

#[test]
fn test_int21_create_directory_ah39() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"TEST_NEW_DIR\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x39);
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
    // Result depends on file system access
}

#[test]
fn test_int21_create_directory_existing_ah39() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let subdir = temp_dir.path().join("EXISTDIR");
    std::fs::create_dir(&subdir).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\EXISTDIR");

    cpu.regs.set_ah(0x39);
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
fn test_int21_create_directory_missing_parent_ah39() {
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

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\MISSING\\SUBDIR");

    cpu.regs.set_ah(0x39);
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
    assert_eq!(cpu.regs.ax(), 3);
}

#[test]
fn test_int21_create_directory_error_ah39() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.drives.insert(2, Box::new(DenyWriteDrive));
    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\DENYDIR");

    cpu.regs.set_ah(0x39);
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
    assert_eq!(cpu.regs.ax(), 3);
}

#[test]
fn test_int21_remove_directory_ah3a() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"NONEXISTENT_DIR\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x3A);
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
    // Directory doesn't exist, should set carry
}

#[test]
fn test_int21_change_directory_ah3b() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"NONEXISTENT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x3B);
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
    // Directory doesn't exist, should set carry
}

#[test]
fn test_int21_change_directory_success_ah3b() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let subdir = temp_dir.path().join("SUBDIR");
    std::fs::create_dir(&subdir).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\SUBDIR");

    cpu.regs.set_ah(0x3B);
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
    assert_eq!(dos.path_resolver.get_current_dir(2), r"\SUBDIR");
}

#[test]
fn test_int21_delete_file_ah41() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"NONEXISTENT.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x41);
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
    // File doesn't exist, should set carry
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_delete_file_success_ah41() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("DELETE.TXT");
    std::fs::write(&file_path, b"delete").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\DELETE.TXT");

    cpu.regs.set_ah(0x41);
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
    assert!(!file_path.exists());
}

#[test]
fn test_int21_ioctl_al08() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x08); // Check if removable
    cpu.regs.set_bx(2); // Drive C:

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // AX=1 means fixed, AX=0 means removable
}

#[test]
fn test_int21_ioctl_al09() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x09); // Check if remote
    cpu.regs.set_bx(2); // Drive C:

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
fn test_int21_ioctl_al07_and_al0a_invalid_handle_paths() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x07); // Check output status
    cpu.regs.set_bx(0x1234);
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

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x0A); // Check if handle is remote
    cpu.regs.set_bx(0x1234);
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
fn test_int21_ioctl_al0b_0c_0e_0f_10_11_paths() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    for al in [0x0B, 0x0E, 0x0F, 0x10, 0x11] {
        cpu.regs.set_ah(0x44);
        cpu.regs.set_al(al);
        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
        let result = dos.int21(
            &mut cpu,
            &mut memory,
            &mut vga,
            &mut keyboard,
            &mut Pic::new(),
        );
        assert!(result.is_ok(), "AH=44h AL={al:02X} should succeed");
        assert!(
            !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "AH=44h AL={al:02X} should clear carry"
        );
    }

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x0C); // Generic IOCTL char device - unsupported
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
fn test_int21_ioctl_al08_and_al0a_valid_paths() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x08); // Check removable by current drive (BL=0)
    cpu.regs.set_bx(0);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1); // Default C: is fixed

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x08);
    cpu.regs.set_bx(1); // A:
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0); // A: is removable

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x0A); // Check if handle is remote
    cpu.regs.set_bx(0); // STDIN
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.dx(), 0);
}

#[test]
fn test_int21_ioctl_al01_al02_file_handle_paths() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(b"DATA".to_vec())),
            "C:\\IOCTL.TXT".to_string(),
            dos.current_psp,
        )
        .expect("expected file handle");

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x01); // Set device info
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

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x02); // Read control channel (stubbed success path)
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
    assert_eq!(cpu.regs.ax(), 0);
}

#[test]
fn test_int21_ioctl_al0d_minor_matrix() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let run_minor = |cpu: &mut gp64_cpu::Cpu,
                     dos: &mut Dos,
                     memory: &mut Memory,
                     vga: &mut gp64_hw::vga::Vga,
                     keyboard: &mut gp64_hw::keyboard::Keyboard,
                     minor: u8| {
        cpu.regs.set_ah(0x44);
        cpu.regs.set_al(0x0D);
        cpu.regs.set_cl(minor);
        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
        dos.int21(cpu, memory, vga, keyboard, &mut Pic::new())
    };

    for minor in [0x40, 0x41, 0x42, 0x46, 0x47, 0x60, 0x61, 0x62, 0x66] {
        let result = run_minor(
            &mut cpu,
            &mut dos,
            &mut memory,
            &mut vga,
            &mut keyboard,
            minor,
        );
        assert!(result.is_ok(), "AL=0Dh minor {minor:02X} should succeed");
        assert!(
            !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "AL=0Dh minor {minor:02X} should clear carry"
        );
    }

    let result = run_minor(
        &mut cpu,
        &mut dos,
        &mut memory,
        &mut vga,
        &mut keyboard,
        0x67,
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.al(), 0);

    let result = run_minor(
        &mut cpu,
        &mut dos,
        &mut memory,
        &mut vga,
        &mut keyboard,
        0x68,
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.al(), 0x02);

    let result = run_minor(
        &mut cpu,
        &mut dos,
        &mut memory,
        &mut vga,
        &mut keyboard,
        0x12,
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1);
}

#[test]
fn test_int21_trace_entry_and_return_with_filters_cpucov() {
    let _lock = env_lock();
    let _sub = trace_subscriber("gp64::dos=debug,gp64::dos::int=debug");
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Simulate an INT frame so caller_cs/caller_ip formatting paths execute.
    cpu.regs.ss = 0x1200;
    cpu.regs.set_sp(0x0010);
    let frame = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.sp());
    memory.write_u16(frame, 0x2345); // return IP
    memory.write_u16(frame + 2, 0x4567); // return CS

    cpu.regs.set_ah(0x19); // Get current drive
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
fn test_int2f_noop_default_subfunctions_keep_registers() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    for ax in [0x1606u16, 0x1680, 0x160A, 0x4811, 0x4A03, 0xB707] {
        cpu.regs.set_ax(ax);
        cpu.regs.set_bx(0xBEEF);
        cpu.regs.set_cx(0xCAFE);
        cpu.regs.set_dx(0x1234);
        let result = dos.int2f(&mut cpu, &mut memory, false);
        assert!(result.is_ok());
        assert_eq!(cpu.regs.ax(), ax);
        assert_eq!(cpu.regs.bx(), 0xBEEF);
        assert_eq!(cpu.regs.cx(), 0xCAFE);
        assert_eq!(cpu.regs.dx(), 0x1234);
    }
}

#[test]
fn test_int21_get_set_file_date_time_ah57() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x57);
    cpu.regs.set_al(0x00); // Get file date/time
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
fn test_int21_get_extended_error_ah59() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x59);
    cpu.regs.set_bx(0);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // AX should contain extended error code
}

#[test]
fn test_int21_get_extended_country_info_ah65() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x65);
    cpu.regs.set_al(0x01);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0100);
    cpu.regs.set_cx(64);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let addr = Memory::seg_offset_to_linear(0x3000, 0x0100);
    assert_eq!(memory.read_u8(addr), 0x01);
    assert_eq!(memory.read_u16(addr + 3), 1);
}

/// INT 21h/65h must honour the CX buffer-size limit. Programs like
/// synthetic-program pass CX=7 to fetch only the 7-byte header; writing
/// the full 41 bytes overflows the caller's stack buffer.
#[test]
fn test_int21_ah65_respects_cx_buffer_size() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x3000, 0x0100);

    // Place a sentinel byte right after the 7-byte region
    memory.write_u8(addr + 7, 0xAA);

    cpu.regs.set_ah(0x65);
    cpu.regs.set_al(0x01);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0100);
    cpu.regs.set_cx(7); // Only want the header

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // CX should reflect actual bytes written
    assert_eq!(cpu.regs.cx(), 7);

    // Header should be populated
    assert_eq!(memory.read_u8(addr), 0x01); // Info ID
    assert_eq!(memory.read_u16(addr + 1), 38); // Size of following data

    // Sentinel byte must be untouched - no overflow
    assert_eq!(memory.read_u8(addr + 7), 0xAA);
}

#[test]
fn test_int21_get_extended_country_info_invalid() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x65);
    cpu.regs.set_al(0xFF);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0100);

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
fn test_int21_global_code_page_ah66() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x66);
    cpu.regs.set_al(0x01);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bx(), 437);
    assert_eq!(cpu.regs.dx(), 437);

    cpu.regs.set_ah(0x66);
    cpu.regs.set_al(0x02);
    cpu.regs.set_bx(850);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    cpu.regs.set_ah(0x66);
    cpu.regs.set_al(0xFF);
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
fn test_int21_create_temp_file_ah5a() {
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

    // Set up path at DS:DX (root of temp drive)
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"C:\\\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x5A);
    cpu.regs.set_cx(0); // Normal attributes
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
    let mut path_bytes = Vec::new();
    for i in 0..128 {
        let b = memory.read_u8(addr + i);
        if b == 0 {
            break;
        }
        path_bytes.push(b);
    }
    let path = String::from_utf8(path_bytes).unwrap_or_default();
    dos.file_table.free(handle);

    let relative = path.trim_start_matches("C:\\").trim_start_matches('\\');
    let host_path = temp_dir.path().join(relative);
    let _ = std::fs::remove_file(host_path);
}

#[test]
fn test_int21_create_temp_file_adds_missing_directory_separator() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    std::fs::create_dir(temp_dir.path().join("APP")).unwrap();
    let mut dos = Dos::new();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &byte) in b"C:\\APP\0".iter().enumerate() {
        memory.write_u8(addr + i, byte);
    }
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.set_ax(0x5A00);
    dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    )
    .unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.ax();
    let generated: Vec<u8> = (0..128)
        .map(|i| memory.read_u8(addr + i))
        .take_while(|&byte| byte != 0)
        .collect();
    let name = String::from_utf8(generated).unwrap();
    assert!(name.starts_with("C:\\APP\\"), "generated name: {name}");
    assert!(temp_dir.path().join("APP").join(&name[7..]).exists());

    cpu.regs.set_bx(handle);
    cpu.regs.set_ax(0x3E00);
    dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    )
    .unwrap();
    cpu.regs.set_ax(0x4100);
    dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    )
    .unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(!temp_dir.path().join("APP").join(&name[7..]).exists());
}

#[test]
fn test_int21_create_temp_file_ah5a_long_path_overflow() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let long_dir = format!("C:\\{}\\", "A".repeat(115));
    let path_str = format!("{long_dir}\0");
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in path_str.as_bytes().iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x5A);
    cpu.regs.set_cx(0);
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
    assert_eq!(cpu.regs.ax(), 3);
}

#[test]
fn test_int21_create_new_file_ah5b() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let file_name = format!("TESTNEW_{}.TXT", std::process::id());
    let dos_path = format!("C:\\{file_name}");
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );
    write_asciiz(&mut memory, 0x2000, 0x0100, &dos_path);

    for attempt in 0..2 {
        cpu.regs.set_ah(0x5B);
        cpu.regs.set_cx(0); // Normal attributes
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

        let created = !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY);
        if attempt == 0 {
            assert!(created);
            let handle = cpu.regs.ax();
            dos.file_table.free(handle);
        } else {
            assert!(!created);
        }
    }

    let _ = std::fs::remove_file(temp_dir.path().join(&file_name));
}

#[test]
fn test_int21_lock_unlock_file_ah5c() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x5C);
    cpu.regs.set_al(0x00); // Lock
    cpu.regs.set_bx(99); // Invalid handle

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // Should fail with invalid handle
}

#[test]
fn int21_region_locks_cover_independent_opens_and_file_io() {
    // MDD-BUG-CRUCIBLE-00137: byte-range locks belong to an open SFT
    // identity, but they protect the underlying file from every other
    // independently opened SFT, including AH=3Fh/AH=40h transfers.
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let path = String::from(r"C:\LOCKED.DAT");
    let locker = dos
        .file_table
        .allocate_with_sharing(
            Box::new(CursorFile::new(b"ABCDE".to_vec())),
            path.clone(),
            0x1000,
            crate::files::SHARE_DENY_NONE,
            crate::files::ACCESS_READWRITE,
        )
        .expect("locker handle");
    let other = dos
        .file_table
        .allocate_with_sharing(
            Box::new(CursorFile::new(b"ABCDE".to_vec())),
            path,
            0x2000,
            crate::files::SHARE_DENY_NONE,
            crate::files::ACCESS_READWRITE,
        )
        .expect("independent handle");

    assert_eq!(dos.file_table.lock_region(locker, 1, 2), Ok(()));
    assert_eq!(
        dos.file_table.lock_region(other, 2, 1),
        Err(33),
        "an independent open must see the existing file lock"
    );

    let buffer = Memory::seg_offset_to_linear(0x3000, 0x0100);
    memory.write_u8(buffer, 0xCC);
    dos.file_table
        .get_sft_mut(other)
        .unwrap()
        .file
        .seek(std::io::SeekFrom::Start(1))
        .unwrap();
    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(other);
    cpu.regs.set_cx(1);
    cpu.regs.ds = 0x3000;
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
    assert_eq!(cpu.regs.ax(), 33);
    assert_eq!(dos.extended_error.0, 33);
    assert_eq!(memory.read_u8(buffer), 0xCC, "blocked read copied data");
    assert_eq!(
        dos.file_table
            .get_sft_mut(other)
            .unwrap()
            .file
            .stream_position()
            .unwrap(),
        1,
        "blocked read advanced the file"
    );

    memory.write_u8(buffer, b'X');
    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(other);
    cpu.regs.set_cx(1);
    dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    )
    .unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 33);
    assert_eq!(
        dos.file_table
            .get_sft_mut(other)
            .unwrap()
            .file
            .stream_position()
            .unwrap(),
        1,
        "blocked write advanced the file"
    );

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(other);
    cpu.regs.set_cx(0);
    dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    )
    .unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 33, "truncate crossed a foreign lock");
    let other_file = &mut dos.file_table.get_sft_mut(other).unwrap().file;
    assert_eq!(other_file.seek(std::io::SeekFrom::End(0)).unwrap(), 5);
    other_file.seek(std::io::SeekFrom::Start(1)).unwrap();

    dos.file_table
        .get_sft_mut(other)
        .unwrap()
        .file
        .seek(std::io::SeekFrom::Start(4))
        .unwrap();
    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(other);
    cpu.regs.set_cx(1);
    dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    )
    .unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1, "disjoint write was blocked");

    dos.file_table
        .get_sft_mut(locker)
        .unwrap()
        .file
        .seek(std::io::SeekFrom::Start(1))
        .unwrap();
    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(locker);
    cpu.regs.set_cx(1);
    dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    )
    .unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1, "lock owner was blocked by its own lock");

    dos.file_table.free(locker);
    dos.file_table
        .get_sft_mut(other)
        .unwrap()
        .file
        .seek(std::io::SeekFrom::Start(1))
        .unwrap();
    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(other);
    cpu.regs.set_cx(1);
    dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    )
    .unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1, "close did not release the file lock");
}

/// A create that cannot be given a handle must leave the file alone.
///
/// MDD-BUG-KIL-01019: AH=3Ch truncated the backing file and only then asked
/// the file table for a handle. With the JFT full it returned error 4 — a
/// report of failure that had already emptied the guest's file. Neither
/// half of the cleanup can undo that: `allocate_with_sharing_identity` can
/// only remove its own SFT entry, and dropping the backend closes the host
/// file without restoring a byte of it.
///
/// The JFT is filled through the ordinary API rather than by reaching into
/// the table, so what is exercised is the path a guest actually takes.
#[test]
fn test_int21_create_ah3c_full_handle_table_leaves_the_file_intact() {
    const EXISTING: &[u8] = b"the bytes a failed create must not discard";

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = std::env::temp_dir().join("gp64_kil01019_create");
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).unwrap();
    let victim = temp_dir.join("VICTIM.TXT");
    std::fs::write(&victim, EXISTING).unwrap();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(temp_dir.clone())),
    );

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    let mut create = |dos: &mut Dos, cpu: &mut gp64_cpu::Cpu, memory: &mut Memory, path: &str| {
        for (i, &b) in path.as_bytes().iter().chain([0].iter()).enumerate() {
            memory.write_u8(addr + i, b);
        }
        cpu.regs.set_ah(0x3C);
        cpu.regs.ds = 0x2000;
        cpu.regs.set_dx(0x0100);
        cpu.regs.set_cx(0);
        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
        dos.int21(cpu, memory, &mut vga, &mut keyboard, &mut Pic::new())
            .unwrap();
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY)
    };

    // Fill the job file table with ordinary creates on throwaway names.
    let mut filled = false;
    for index in 0..64 {
        if create(
            &mut dos,
            &mut cpu,
            &mut memory,
            &format!("C:\\FILL{index:02}.TXT"),
        ) {
            filled = true;
            break;
        }
    }
    assert!(
        filled,
        "the handle table never filled, so this proves nothing about a full one"
    );

    // The create under test: an EXISTING file, with no handle to be had.
    assert!(
        create(&mut dos, &mut cpu, &mut memory, "C:\\VICTIM.TXT"),
        "a create with no free handle must fail"
    );
    assert_eq!(
        cpu.regs.ax(),
        4,
        "the failure must be 'too many open files'"
    );
    assert_eq!(
        std::fs::read(&victim).unwrap(),
        EXISTING,
        "the failed create truncated the file it reported it could not open"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
}

/// A failed AH=5Ah must not leave its temporary file behind.
///
/// MDD-BUG-KIL-01019, second consequence. This one is worse than a wasted
/// call: AH=5Ah writes the name it generated back into the caller's buffer
/// only on success, so a guest told the call failed never learns the name
/// of the file it just created and cannot clean it up. Repeated under a
/// full handle table it litters the directory indefinitely.
#[test]
fn test_int21_create_temp_ah5a_full_handle_table_creates_no_file() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = std::env::temp_dir().join("gp64_kil01019_temp");
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).unwrap();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(temp_dir.clone())),
    );

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    let write_path = |memory: &mut Memory, path: &str| {
        for (i, &b) in path.as_bytes().iter().chain([0].iter()).enumerate() {
            memory.write_u8(addr + i, b);
        }
    };

    // Fill the job file table the same ordinary way.
    let mut filled = false;
    for index in 0..64 {
        write_path(&mut memory, &format!("C:\\FILL{index:02}.TXT"));
        cpu.regs.set_ah(0x3C);
        cpu.regs.ds = 0x2000;
        cpu.regs.set_dx(0x0100);
        cpu.regs.set_cx(0);
        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
        dos.int21(
            &mut cpu,
            &mut memory,
            &mut vga,
            &mut keyboard,
            &mut Pic::new(),
        )
        .unwrap();
        if cpu.regs.flags.contains(gp64_cpu::Flags::CARRY) {
            filled = true;
            break;
        }
    }
    assert!(filled, "the handle table never filled");

    let before = std::fs::read_dir(&temp_dir).unwrap().count();

    write_path(&mut memory, "C:\\");
    cpu.regs.set_ah(0x5A);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.set_cx(0);
    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
    dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    )
    .unwrap();

    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "a temporary create with no free handle must fail"
    );
    assert_eq!(
        cpu.regs.ax(),
        4,
        "the failure must be 'too many open files'"
    );
    assert_eq!(
        std::fs::read_dir(&temp_dir).unwrap().count(),
        before,
        "the failed temporary create left a file whose name the guest was never told"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_int21_lock_unlock_file_invalid_function() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = std::env::temp_dir().join("gp64_lock_test");
    let _ = std::fs::create_dir(&temp_dir);
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(temp_dir.clone())),
    );

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"C:\\LOCK.TXT\0".iter().enumerate() {
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
    let handle = cpu.regs.ax();

    cpu.regs.set_ah(0x5C);
    cpu.regs.set_al(0x02); // Invalid function
    cpu.regs.set_bx(handle);
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1);

    dos.file_table.free(handle);
    let _ = std::fs::remove_file(temp_dir.join("LOCK.TXT"));
    let _ = std::fs::remove_dir(&temp_dir);
}

#[test]
fn test_int21_rename_file_ah56() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Source path at DS:DX
    let src_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"NONEXISTENT.OLD\0".iter().enumerate() {
        memory.write_u8(src_addr + i, b);
    }

    // Dest path at ES:DI
    let dst_addr = Memory::seg_offset_to_linear(0x3000, 0x0100);
    for (i, &b) in b"NEWNAME.NEW\0".iter().enumerate() {
        memory.write_u8(dst_addr + i, b);
    }

    cpu.regs.set_ah(0x56);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // Result depends on whether rename is implemented and succeeds
}

#[test]
fn test_int21_rename_file_ah56_cross_drive_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let src_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"C:\\OLD.TXT\0".iter().enumerate() {
        memory.write_u8(src_addr + i, b);
    }
    let dst_addr = Memory::seg_offset_to_linear(0x3000, 0x0100);
    for (i, &b) in b"D:\\NEW.TXT\0".iter().enumerate() {
        memory.write_u8(dst_addr + i, b);
    }

    cpu.regs.set_ah(0x56);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 17);
}

#[test]
fn test_int21_set_handle_count_ah67() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x67);
    cpu.regs.set_bx(100); // Request 100 handles

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // Requesting 100 (<= 255) succeeds and grows the JFT to 100 entries.
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dos.file_table.handle_limit(), 100);
}

/// AH=67h SetHandleCount grows the JFT but never shrinks it and never drops
/// below the default of 20 — exactly the contract dos_file.c's
/// `test_set_handle_count` exercises (max_open_files == handle_limit - 5).
#[test]
fn test_int21_set_handle_count_ah67_grow_no_shrink() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    let call = |dos: &mut Dos, cpu: &mut gp64_cpu::Cpu, memory: &mut Memory, bx: u16| {
        cpu.regs.set_ah(0x67);
        cpu.regs.set_bx(bx);
        let (mut vga, mut keyboard) = create_test_io();
        let _ = dos.int21(cpu, memory, &mut vga, &mut keyboard, &mut Pic::new());
    };

    // Default JFT is 20 (→ 15 user files).
    assert_eq!(dos.file_table.handle_limit(), 20);
    // Requesting 0 floors at 20 (never below the default).
    call(&mut dos, &mut cpu, &mut memory, 0);
    assert_eq!(dos.file_table.handle_limit(), 20);
    // Growing to 21, then 22.
    call(&mut dos, &mut cpu, &mut memory, 21);
    assert_eq!(dos.file_table.handle_limit(), 21);
    call(&mut dos, &mut cpu, &mut memory, 22);
    assert_eq!(dos.file_table.handle_limit(), 22);
    // Requesting 20 again must NOT shrink it.
    call(&mut dos, &mut cpu, &mut memory, 20);
    assert_eq!(dos.file_table.handle_limit(), 22);
}

#[test]
fn test_int21_set_handle_count_ah67_too_large() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x67);
    cpu.regs.set_bx(300); // Too many handles

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 8);
}

#[test]
fn test_int21_commit_invalid_handle_ah68() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x68);
    cpu.regs.set_bx(10); // Invalid handle

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
fn test_int21_commit_standard_handle_ah68() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x68);
    cpu.regs.set_bx(1); // STDOUT

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
fn test_int21_commit_file_handle_ah68() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(b"SYNC".to_vec())),
            "C:\\SYNC.TXT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");

    cpu.regs.set_ah(0x68);
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

#[test]
fn test_int21_dup_handle_file_handle_no_free_slot() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(b"DATA".to_vec())),
            "C:\\DUPFAIL.TXT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");

    // Grow the JFT to its maximum and fill every other user handle.
    dos.file_table.set_handle_count(255);
    for i in 5..0xFF {
        if i == handle {
            continue;
        }
        let filled = dos.file_table.allocate(
            Box::new(CursorFile::new(vec![])),
            format!("C:\\FILL{i:02X}.TXT"),
            dos.current_psp,
        );
        assert!(filled.is_some());
    }

    cpu.regs.set_ah(0x45);
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
    assert_eq!(cpu.regs.ax(), 4);
}

#[test]
fn test_int21_get_country_ah38() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // AL=1 with DX≠FFFFh → GET country info for code 1 (USA)
    cpu.regs.set_ah(0x38);
    cpu.regs.set_al(1);
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
    assert_eq!(cpu.regs.bx(), 1); // USA
}

#[test]
fn test_int21_get_truename_ah60() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let src_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"TEST.TXT\0".iter().enumerate() {
        memory.write_u8(src_addr + i, b);
    }

    cpu.regs.set_ah(0x60);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0100);

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
fn test_find_first_find_next_sequence() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    let patterns: [&[u8]; 2] = [b"NO_MATCH_123\0", b"src\\*.rs\0"];

    for (index, pattern) in patterns.iter().enumerate() {
        for (i, &b) in pattern.iter().enumerate() {
            memory.write_u8(addr + i, b);
        }

        cpu.regs.set_ah(0x4E);
        cpu.regs.set_cx(0);
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

        let success = !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY);
        if index == 0 {
            assert!(!success);
        } else {
            assert!(success);
        }

        if success {
            cpu.regs.set_ah(0x4F);
            let result = dos.int21(
                &mut cpu,
                &mut memory,
                &mut vga,
                &mut keyboard,
                &mut Pic::new(),
            );
            assert!(result.is_ok());
        }
    }
}

#[test]
fn test_int21_read_write_stdout() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Write to stdout (handle 1)
    let msg = b"Hello";
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in msg.iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(1); // stdout
    cpu.regs.set_cx(5);
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
    assert_eq!(cpu.regs.ax(), 5);
}

#[test]
fn test_int21_read_stdin() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(0); // stdin
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
    // Cooked-mode stdin blocks when no input is available;
    // AX is left unchanged (still holds AH=0x3F from setup).
    assert_eq!(cpu.regs.ax(), 0x3F00);
}

#[test]
fn test_int21_write_stderr() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let msg = b"Error";
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in msg.iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(2); // stderr
    cpu.regs.set_cx(5);
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
fn test_int21_dup2_handle_ah46() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x46);
    cpu.regs.set_bx(0); // Source handle (stdin)
    cpu.regs.set_cx(10); // Target handle

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
fn test_int21_dup2_same_handle_noop() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x46);
    cpu.regs.set_bx(2); // stdout
    cpu.regs.set_cx(2); // same handle

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
fn test_int21_dup2_standard_to_file_frees_destination() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(b"OUT".to_vec())),
            "C:\\DUP2.TXT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");

    cpu.regs.set_ah(0x46);
    cpu.regs.set_bx(0); // stdin
    cpu.regs.set_cx(handle);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(!dos.file_table.is_valid(handle));
    assert_eq!(dos.file_table.std_handle_aliases[handle as usize], Some(0));
}

#[test]
fn test_int21_dup2_redirects_stdout_to_file() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = std::env::temp_dir().join(format!("gp64_dup2_redirect_{}", std::process::id()));
    let _ = std::fs::create_dir(&temp_dir);
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(temp_dir.clone())),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, "OUT.TXT");

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
    let file_handle = cpu.regs.ax();

    cpu.regs.set_ah(0x46);
    cpu.regs.set_bx(file_handle);
    cpu.regs.set_cx(1); // STDOUT

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let msg = b"HELLO";
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0200);
    for (i, &b) in msg.iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(1);
    cpu.regs.set_cx(msg.len() as u16);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0200);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    cpu.regs.set_ah(0x3E);
    cpu.regs.set_bx(1);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    cpu.regs.set_ah(0x3E);
    cpu.regs.set_bx(file_handle);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    let host_path = temp_dir.join("OUT.TXT");
    let contents = std::fs::read(&host_path).unwrap_or_default();
    assert_eq!(contents, msg);

    let _ = std::fs::remove_file(&host_path);
    let _ = std::fs::remove_dir(&temp_dir);
}

#[test]
fn test_matches_pattern_via_find_first() {
    // Test pattern matching through the find_first/find_next mechanism
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Try find with a pattern that should match at least one file
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"*.rs\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x4E);
    cpu.regs.set_cx(0);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);

    // Will succeed or fail depending on current directory
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
fn test_int21_get_current_dir_ah47() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x47);
    cpu.regs.set_dl(0); // Default drive
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
}

#[test]
fn test_int21_get_current_dir_specific_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x47);
    cpu.regs.set_dl(3); // C: drive
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
}

#[test]
fn test_int21_get_current_dir_invalid_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    memory.write_u8(0x20000, 0xAA);

    cpu.regs.set_ah(0x47);
    cpu.regs.set_dl(25); // Z: (invalid)
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0000);

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
    assert_eq!(memory.read_u8(0x20000), 0xAA);
}

#[test]
fn test_int21_change_directory_success() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let subdir = temp_dir.path().join("DIR1");
    std::fs::create_dir(&subdir).unwrap();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\DIR1");
    cpu.regs.set_ah(0x3B);
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
fn test_int21_change_directory_not_found() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\NOPE");
    cpu.regs.set_ah(0x3B);
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
    assert_eq!(cpu.regs.ax(), 3);
}

#[test]
fn test_int21_string_output_ah09() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Write string terminated by $
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"Hello World$".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x09);
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
fn test_int21_buffered_input_ah0a() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Set up buffer
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, 80); // Max chars

    cpu.regs.set_ah(0x0A);
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
fn test_int21_buffered_input_len_exceeds_max_resets() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, 4);
    memory.write_u8(addr + 1, 6);

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
}

#[test]
fn test_int21_buffered_input_existing_data_invalid_resets() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, 4);
    memory.write_u8(addr + 1, 2);
    memory.write_u8(addr + 2, 0);
    memory.write_u8(addr + 3, b'A');

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
}

#[test]
fn test_int21_buffered_input_existing_data_invalid_cr_resets() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, 4);
    memory.write_u8(addr + 1, 1);
    memory.write_u8(addr + 2, 0x0D);

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
}

#[test]
fn test_int21_buffered_input_existing_data_invalid_control_resets() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, 4);
    memory.write_u8(addr + 1, 1);
    memory.write_u8(addr + 2, 0x1F);

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
}

#[test]
fn test_int21_buffered_input_existing_data_valid_kept() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, 4);
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
}

#[test]
fn test_int21_buffered_input_enter_writes_cr() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    bda::kbd_enqueue(&mut memory, 0x1C, 0x0D);

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
    assert_eq!(memory.read_u8(addr + 1), 0);
    assert_eq!(memory.read_u8(addr + 2), 0x0D);
}

#[test]
fn test_int21_buffered_input_enter_no_space() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    bda::kbd_enqueue(&mut memory, 0x1C, 0x0D);

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, 0);
    memory.write_u8(addr + 1, 0);
    memory.write_u8(addr + 2, 0xAA);

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
    assert_eq!(memory.read_u8(addr + 2), 0xAA);
}

#[test]
fn test_int21_buffered_input_backspace_deletes() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    bda::kbd_enqueue(&mut memory, 0x0E, 0x08);

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, 4);
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
    assert_eq!(memory.read_u8(addr + 1), 0);
}

#[test]
fn test_int21_buffered_input_backspace_when_empty() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    bda::kbd_enqueue(&mut memory, 0x0E, 0x08);

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
    assert_eq!(memory.read_u8(addr + 1), 0);
}

#[test]
fn test_int21_buffered_input_escape_clears() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    bda::kbd_enqueue(&mut memory, 0x01, 0x1B);

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, 4);
    memory.write_u8(addr + 1, 2);
    memory.write_u8(addr + 2, b'A');
    memory.write_u8(addr + 3, b'B');

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
}

#[test]
fn test_int21_buffered_input_tab_expands_and_clamps() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    bda::kbd_enqueue(&mut memory, 0x0F, 0x09);

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, 4);
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
    assert_eq!(memory.read_u8(addr + 1), 3);
    assert_eq!(memory.read_u8(addr + 3), b' ');
    assert_eq!(memory.read_u8(addr + 4), b' ');
}

#[test]
fn test_int21_buffered_input_extended_key_ignored() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    bda::kbd_enqueue(&mut memory, 0x48, 0x00);

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
    assert_eq!(memory.read_u8(addr + 1), 0);
}

fn flu00012_temp_call(dos: &mut Dos) -> (gp64_cpu::Cpu, Memory) {
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, b) in b"C:\\\0".iter().enumerate() {
        memory.write_u8(addr + i, *b);
    }
    cpu.regs.set_ax(0x5A00);
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
    (cpu, memory)
}

fn flu00012_path(memory: &Memory) -> String {
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    String::from_utf8(
        (0..128)
            .map(|i| memory.read_u8(addr + i))
            .take_while(|b| *b != 0)
            .collect(),
    )
    .unwrap()
}

fn flu00012_mount() -> (Dos, tempfile::TempDir) {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch");
    std::fs::create_dir_all(&base).unwrap();
    let dir = tempfile::Builder::new()
        .prefix(&format!("flu00012-{}-", std::process::id()))
        .tempdir_in(base)
        .unwrap();
    let mut dos = Dos::new();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(dir.path().to_path_buf())),
    );
    (dos, dir)
}

#[test]
fn test_flu00012_temp_preserves_collisions_and_wraps() {
    let (mut dos, dir) = flu00012_mount();
    dos.temp_file_counter = 0xFFFF;
    for name in ["~DOS0000.TMP", "~dos0001.tmp", "~DOS0002.TMP"] {
        std::fs::write(dir.path().join(name), name.as_bytes()).unwrap();
    }
    let (cpu, memory) = flu00012_temp_call(&mut dos);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    for name in ["~DOS0000.TMP", "~dos0001.tmp", "~DOS0002.TMP"] {
        assert_eq!(
            std::fs::read(dir.path().join(name)).unwrap(),
            name.as_bytes(),
            "collision truncated {name}"
        );
    }
    assert_eq!(flu00012_path(&memory), "C:\\~DOS0003.TMP");
    assert!(dir.path().join("~DOS0003.TMP").is_file());
}

#[test]
fn test_flu00012_temp_synthetic_namespace_exhaustion() {
    let (mut dos, dir) = flu00012_mount();
    for counter in 0..=u16::MAX {
        dos.register_virtual_file(&format!("C:\\~DOS{counter:04X}.TMP"), vec![1, 2, 3]);
    }
    let (cpu, memory) = flu00012_temp_call(&mut dos);
    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "exhausted namespace succeeded"
    );
    assert_eq!(cpu.regs.ax(), 80);
    assert_eq!(flu00012_path(&memory), "C:\\");
    assert_eq!(
        dos.temp_file_counter, 65536,
        "search must try the whole namespace once"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    assert!(dos
        .virtual_files
        .values()
        .all(|bytes| bytes.as_ref() == [1, 2, 3]));
}

#[test]
fn test_flu00012_temp_full_handles_preserves_buffer_and_files() {
    let (mut dos, dir) = flu00012_mount();
    std::fs::write(dir.path().join("~DOS0001.TMP"), b"sentinel").unwrap();
    while dos.file_table.has_open_capacity() {
        dos.file_table
            .allocate(
                Box::new(CursorFile::new(Vec::new())),
                "synthetic".into(),
                dos.current_psp,
            )
            .unwrap();
    }
    let (cpu, memory) = flu00012_temp_call(&mut dos);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 4);
    assert_eq!(flu00012_path(&memory), "C:\\");
    assert_eq!(
        std::fs::read(dir.path().join("~DOS0001.TMP")).unwrap(),
        b"sentinel"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn test_flu00012_temp_overlay_and_synthetic_collisions() {
    let (mut dos, root) = flu00012_mount();
    let overlay = tempfile::Builder::new()
        .prefix("flu00012-overlay-")
        .tempdir_in(root.path().parent().unwrap())
        .unwrap();
    std::fs::create_dir(overlay.path().join("ONLY")).unwrap();
    std::fs::write(
        overlay.path().join("ONLY/~DOS0001.TMP"),
        b"overlay sentinel",
    )
    .unwrap();
    let mut drive = crate::filesystem::HostDrive::new(root.path().to_path_buf());
    drive.set_write_overlay(overlay.path().to_path_buf());
    dos.drives.insert(2, Box::new(drive));
    dos.path_resolver.set_current_dir("C:\\ONLY");
    dos.register_virtual_file("C:\\ONLY\\~DOS0002.TMP", vec![7, 8]);
    // Use a drive-relative empty path so the resolver uses the overlay-only CWD.
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    cpu.regs.set_ax(0x5A00);
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
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(
        std::fs::read(overlay.path().join("ONLY/~DOS0001.TMP")).unwrap(),
        b"overlay sentinel"
    );
    assert_eq!(flu00012_path(&memory), "~DOS0003.TMP");
    assert!(overlay.path().join("ONLY/~DOS0003.TMP").is_file());
    assert!(!root.path().join("ONLY").exists());
}

#[test]
fn test_flu00012_temp_unsupported_drive_preserves_buffer() {
    let mut drive = DenyWriteDrive;
    let error = drive.create_new_file("\\NEW.TMP").err().unwrap();
    assert!(
        error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::Unsupported),
        "default exclusive-create must fail closed with Unsupported"
    );
    let mut dos = Dos::new();
    dos.drives.insert(2, Box::new(drive));
    let (cpu, memory) = flu00012_temp_call(&mut dos);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 5);
    assert_eq!(flu00012_path(&memory), "C:\\");
    assert_eq!(
        dos.temp_file_counter, 1,
        "non-collision errors must stop the search"
    );
}
