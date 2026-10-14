use super::*;

#[test]
fn test_matches_attr_filter_normal_file() {
    use crate::filesystem::FileAttributes;
    let attrs = FileAttributes::default(); // Normal file
                                           // Normal files should always match
    assert!(Dos::matches_attr_filter(&attrs, 0x00)); // No filter
    assert!(Dos::matches_attr_filter(&attrs, 0xFF)); // All filter bits
}

#[test]
fn test_matches_attr_filter_hidden_file() {
    use crate::filesystem::FileAttributes;
    let attrs = FileAttributes {
        hidden: true,
        ..Default::default()
    };
    // Hidden files only returned if hidden bit set
    assert!(!Dos::matches_attr_filter(&attrs, 0x00)); // No hidden bit
    assert!(Dos::matches_attr_filter(&attrs, 0x02)); // Hidden bit set
    assert!(Dos::matches_attr_filter(&attrs, 0xFF)); // All bits set
}

#[test]
fn test_matches_attr_filter_system_file() {
    use crate::filesystem::FileAttributes;
    let attrs = FileAttributes {
        system: true,
        ..Default::default()
    };
    // System files only returned if system bit set
    assert!(!Dos::matches_attr_filter(&attrs, 0x00)); // No system bit
    assert!(Dos::matches_attr_filter(&attrs, 0x04)); // System bit set
    assert!(Dos::matches_attr_filter(&attrs, 0xFF)); // All bits set
}

#[test]
fn test_matches_attr_filter_directory() {
    use crate::filesystem::FileAttributes;
    let attrs = FileAttributes {
        directory: true,
        ..Default::default()
    };
    // Directories only returned if directory bit set
    assert!(!Dos::matches_attr_filter(&attrs, 0x00)); // No dir bit
    assert!(Dos::matches_attr_filter(&attrs, 0x10)); // Dir bit set
    assert!(Dos::matches_attr_filter(&attrs, 0xFF)); // All bits set
}

#[test]
fn test_matches_attr_filter_volume_label_exclusive() {
    use crate::filesystem::FileAttributes;
    let volume = FileAttributes {
        volume_label: true,
        ..Default::default()
    };
    let normal = FileAttributes::default();

    assert!(!Dos::matches_attr_filter(&volume, 0x00));
    assert!(Dos::matches_attr_filter(&volume, 0x08));
    assert!(Dos::matches_attr_filter(&volume, 0xFF));
    assert!(!Dos::matches_attr_filter(&normal, 0x08));
}

#[test]
fn test_attrs_to_dos_byte() {
    use crate::filesystem::FileAttributes;

    // Normal file with archive bit
    let mut attrs = FileAttributes {
        archive: true,
        ..Default::default()
    };
    assert_eq!(Dos::attrs_to_dos_byte(&attrs), 0x20);

    // Directory
    attrs = FileAttributes {
        directory: true,
        ..Default::default()
    };
    assert_eq!(Dos::attrs_to_dos_byte(&attrs), 0x10);

    // Hidden + System + Archive
    attrs = FileAttributes {
        hidden: true,
        system: true,
        archive: true,
        ..Default::default()
    };
    assert_eq!(Dos::attrs_to_dos_byte(&attrs), 0x02 | 0x04 | 0x20);

    // Read-only
    attrs = FileAttributes {
        read_only: true,
        ..Default::default()
    };
    assert_eq!(Dos::attrs_to_dos_byte(&attrs), 0x01);

    // Volume label
    attrs = FileAttributes {
        volume_label: true,
        ..Default::default()
    };
    assert_eq!(Dos::attrs_to_dos_byte(&attrs), 0x08);
}

#[test]
fn test_systemtime_to_dos_datetime_epoch() {
    use std::time::UNIX_EPOCH;
    // Unix epoch (1970-01-01 00:00:00)
    let time = UNIX_EPOCH;
    let (date, _time_val) = Dos::systemtime_to_dos_datetime(time);
    // DOS dates start at 1980, so 1970 would be before DOS epoch
    // Year 1970 - 1980 = -10, but we clamp to 0
    // Date: year=0, month=1, day=1 -> (0 << 9) | (1 << 5) | 1 = 0x0021
    assert_eq!(date & 0xFE00, 0); // Year field is 0
}

#[test]
fn test_systemtime_to_dos_datetime_year_2000() {
    use std::time::{Duration, UNIX_EPOCH};
    // 2000-06-15 12:30:45 (rough calculation)
    // Days from 1970-01-01 to 2000-06-15: approximately 11124 days
    let secs = 11124 * 86400 + 12 * 3600 + 30 * 60 + 44;
    let time = UNIX_EPOCH + Duration::from_secs(secs);
    let (date, time_val) = Dos::systemtime_to_dos_datetime_with(time, |_| 0);

    // Year 2000 - 1980 = 20
    let year = (date >> 9) & 0x7F;
    assert_eq!(year, 20);

    // Time should have reasonable values
    let hours = (time_val >> 11) & 0x1F;
    let minutes = (time_val >> 5) & 0x3F;
    assert_eq!(hours, 12);
    assert_eq!(minutes, 30);
}

#[test]
fn test_systemtime_to_dos_datetime_rounds_seconds() {
    use std::time::{Duration, UNIX_EPOCH};
    // 1980-01-01 is 3652 days after 1970-01-01.
    let base = 3652 * 86400;

    let (date, time_val) =
        Dos::systemtime_to_dos_datetime_with(UNIX_EPOCH + Duration::from_secs(base + 1), |_| 0);
    assert_eq!(date, 0x0021);
    assert_eq!(time_val, 0);

    let (_date, time_val) =
        Dos::systemtime_to_dos_datetime_with(UNIX_EPOCH + Duration::from_secs(base + 59), |_| 0);
    assert_eq!(time_val, 29);
}

/// MDD-BUG-NMI-00208: DOS file times are local. A file modified at
/// 2026-09-24 21:47:20 UTC on a UTC+01:00 host reads as 22:47:20, which is
#[test]
fn systemtime_to_dos_datetime_reports_host_local_time_nmi208() {
    use std::time::{Duration, UNIX_EPOCH};
    let mtime = UNIX_EPOCH + Duration::from_secs(1_790_286_440); // 21:47:20Z
    let (date, time) = Dos::systemtime_to_dos_datetime_with(mtime, |_| 3_600);
    assert_eq!(date, (46 << 9) | (9 << 5) | 24);
    assert_eq!(time, (22 << 11) | (47 << 5) | 10);

    // West of UTC the local date can still be the previous day.
    let (date, time) = Dos::systemtime_to_dos_datetime_with(mtime, |_| -23 * 3_600);
    assert_eq!(date, (46 << 9) | (9 << 5) | 23);
    assert_eq!(time, (22 << 11) | (47 << 5) | 10);
}

/// MDD-BUG-NMI-00208: 5701h stores the UTC instant for a local DOS time,
/// so a set-then-get round trip returns the value the guest wrote.
#[test]
fn dos_datetime_to_systemtime_is_the_local_inverse_nmi208() {
    use std::time::{Duration, UNIX_EPOCH};
    let date = (46 << 9) | (9 << 5) | 24;
    let time = (22 << 11) | (47 << 5) | 10;
    let stored = Dos::dos_datetime_to_systemtime_with(date, time, |_| 3_600);
    assert_eq!(stored, UNIX_EPOCH + Duration::from_secs(1_790_286_440));
    assert_eq!(
        Dos::systemtime_to_dos_datetime_with(stored, |_| 3_600),
        (date, time)
    );
}

#[test]
fn test_days_to_ymd_negative() {
    let (year, month, day) = Dos::days_to_ymd(-719469);
    assert_eq!((year, month, day), (0, 2, 29));
}

#[test]
fn test_dos_date_from_duration_result_ok_leap_day() {
    let days = 365u64 * 2 + 31 + 28; // 1972-02-29
    let duration = Duration::from_secs(days * 86400);
    let (year, month, day, day_of_week) = Dos::dos_date_from_duration_result(Ok(duration));
    assert_eq!((year, month, day), (1972, 2, 29));
    assert_eq!(day_of_week, 2);
}

#[test]
fn test_dos_date_from_duration_result_err_fallback() {
    let err =
        SystemTime::UNIX_EPOCH.duration_since(SystemTime::UNIX_EPOCH + Duration::from_secs(1));
    let (year, month, day, day_of_week) = Dos::dos_date_from_duration_result(err);
    assert_eq!((year, month, day, day_of_week), (2025, 1, 1, 3));
}

#[test]
fn test_ctrl_break_check_flag() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Initially off
    cpu.regs.set_ah(0x33);
    cpu.regs.set_al(0x00); // Get state
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.dl(), 0);

    // Turn on
    cpu.regs.set_ah(0x33);
    cpu.regs.set_al(0x01); // Set state
    cpu.regs.set_dl(1); // Enable
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Verify on
    cpu.regs.set_ah(0x33);
    cpu.regs.set_al(0x00); // Get state
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.dl(), 1);
}

#[test]
fn test_int21_33h_get_boot_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x33);
    cpu.regs.set_al(0x05); // Get boot drive
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.dl(), 3); // C: drive
}

#[test]
fn test_int21_33h_get_true_version() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x33);
    cpu.regs.set_al(0x06); // Get true DOS version
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // INT 21h/33h/06h returns true version in BL (major) and BH (minor)
    // Emulator is configured as DOS 6.22
    let major = cpu.regs.bx() & 0xFF;
    let minor = (cpu.regs.bx() >> 8) & 0xFF;
    assert_eq!(major, 6);
    assert_eq!(minor, 22);
}

#[test]
fn test_stdin_read_with_keyboard_buffer() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Pre-populate the cooked-mode line buffer (simulates a completed line).
    dos.stdin_line_buf = vec![b'A', b'B', b'C'];

    // Read from stdin
    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(0); // STDIN
    cpu.regs.set_cx(10); // Request 10 bytes
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

    // Should have read 3 characters
    assert_eq!(cpu.regs.ax(), 3);

    // Verify buffer contents
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    assert_eq!(memory.read_u8(addr), b'A');
    assert_eq!(memory.read_u8(addr + 1), b'B');
    assert_eq!(memory.read_u8(addr + 2), b'C');
}

#[test]
fn test_extended_key_handling_ah07() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Put an extended key (F1) in the keyboard buffer
    // F1 has scan code 0x3B and ASCII = 0
    bda::kbd_enqueue(&mut memory, 0x3B, 0);

    // First call should return AL=0 (the ASCII code)
    cpu.regs.set_ah(0x07);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0, "First call should return ASCII 0");

    // Second call should return AL=0x3B (the scan code)
    cpu.regs.set_ah(0x07);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x3B, "Second call should return scan code");
}

#[test]
fn test_extended_key_handling_ah01() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Put an extended key (Up Arrow) in the keyboard buffer
    // Up Arrow has scan code 0x48 and ASCII = 0
    bda::kbd_enqueue(&mut memory, 0x48, 0);

    // First call should return AL=0
    cpu.regs.set_ah(0x01);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0, "First call should return ASCII 0");

    // Second call should return AL=0x48 (the scan code)
    cpu.regs.set_ah(0x01);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x48, "Second call should return scan code");
}

#[test]
fn test_int21_input_ah01_ctrl_c() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let vector_addr = 0x23 * 4;
    memory.write_u16(vector_addr, 0x1111);
    memory.write_u16(vector_addr + 2, 0x2222);

    bda::kbd_enqueue(&mut memory, 0x2E, 0x03);

    cpu.regs.set_ah(0x01);
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
fn test_int21_input_ah01_normal_key() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    bda::kbd_enqueue(&mut memory, 0x1E, b'A');

    cpu.regs.set_ah(0x01);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), b'A');
}

#[test]
fn test_int21_input_ah01_extended_prefix_e0() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    bda::kbd_enqueue(&mut memory, 0x48, 0xE0);

    cpu.regs.set_ah(0x01);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0xE0);

    cpu.regs.set_ah(0x01);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x48);
}

#[test]
fn test_int21_input_ah08_ctrl_c() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let vector_addr = 0x23 * 4;
    memory.write_u16(vector_addr, 0x3333);
    memory.write_u16(vector_addr + 2, 0x4444);

    cpu.regs.ss = 0x3000;
    cpu.regs.set_sp(0x0200);
    memory.write_u16_seg(0x3000, 0x0200, 0);
    memory.write_u16_seg(0x3000, 0x0202, 0);
    memory.write_u16_seg(0x3000, 0x0204, 0);

    bda::kbd_enqueue(&mut memory, 0x2E, 0x03);

    cpu.regs.set_ah(0x08);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ip(), 0x3333);
    assert_eq!(cpu.regs.cs, 0x4444);
}

#[test]
fn test_int21_input_ah08_extended_prefix_e0() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    bda::kbd_enqueue(&mut memory, 0x48, 0xE0);

    cpu.regs.set_ah(0x08);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0xE0);

    cpu.regs.set_ah(0x08);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x48);
}

#[test]
fn test_int21_trace_enabled_logs() {
    let _lock = env_lock();
    let _trace = trace_subscriber("gp64::dos=debug");

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
}

#[test]
fn test_extended_key_mixed_with_normal() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Queue: F1 (extended), then 'A' (normal)
    bda::kbd_enqueue(&mut memory, 0x3B, 0); // F1
    bda::kbd_enqueue(&mut memory, 0x1E, b'A'); // 'A'

    // First call: F1 returns 0
    cpu.regs.set_ah(0x08);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0);

    // Second call: F1 scan code
    cpu.regs.set_ah(0x08);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0x3B);

    // Third call: 'A' (normal key)
    cpu.regs.set_ah(0x08);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), b'A');
}

#[test]
fn test_format_dos_bytes_escapes_and_truncates() {
    let bytes = [b'A', b'\r', b'\n', b'\t', 0x01, 0x7F];
    let (formatted, truncated) = format_dos_bytes(&bytes, 4);
    assert_eq!(formatted, "A\\r\\n\\t");
    assert!(truncated);

    let (formatted, truncated) = format_dos_bytes(&bytes, 10);
    assert_eq!(formatted, "A\\r\\n\\t\\x01\\x7F");
    assert!(!truncated);
}

#[test]
fn test_extended_error_mapping_and_clear() {
    let mut dos = Dos::new();
    dos.set_extended_error(2);
    assert_eq!(dos.extended_error, (2, 0x08, 0x03, 0x02));

    dos.set_extended_error(80);
    assert_eq!(dos.extended_error, (80, 0x0C, 0x04, 0x02));

    dos.set_extended_error(999);
    assert_eq!(dos.extended_error, (999, 0x0D, 0x04, 0x01));

    dos.clear_extended_error();
    assert_eq!(dos.extended_error, (0, 0, 0, 0));
}

#[test]
fn test_set_error_return_sets_ax_carry_and_extended_error() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();

    dos.set_error_return(&mut cpu, 5);

    assert_eq!(cpu.regs.ax(), 5);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dos.extended_error, (5, 0x03, 0x04, 0x02));
}

#[test]
fn test_sync_iret_carry_updates_stack_flags() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0x0100);

    let flags_addr = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.sp().wrapping_add(4));
    memory.write_u16(flags_addr, 0x0200);

    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
    Dos::sync_iret_carry(&cpu, &mut memory, false);
    assert_eq!(memory.read_u16(flags_addr) & 1, 1);

    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
    Dos::sync_iret_carry(&cpu, &mut memory, false);
    assert_eq!(memory.read_u16(flags_addr) & 1, 0);
}

#[test]
fn test_prep_input_wait_rewinds_ip_and_sets_if() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0x0100);

    let ret_ip_addr = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.sp());
    memory.write_u16(ret_ip_addr, 0x1234);
    let flags_addr = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.sp().wrapping_add(4));
    memory.write_u16(flags_addr, 0x0002);

    Dos::prep_input_wait(&mut cpu, &mut memory, false);

    assert_eq!(memory.read_u16(ret_ip_addr), 0x1232);
    assert!(cpu.interrupts_enabled);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::INTERRUPT));
    assert_eq!(memory.read_u16(flags_addr) & 0x0200, 0x0200);
}

#[test]
fn test_wait_for_input_sets_carry_in_iret_frame() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0x0100);

    let ret_ip_addr = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.sp());
    memory.write_u16(ret_ip_addr, 0x2000);
    let flags_addr = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.sp().wrapping_add(4));
    memory.write_u16(flags_addr, 0x0200);

    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
    let disposition = Dos::wait_for_input(&mut cpu, &mut memory, false);

    assert_eq!(disposition, Int21Disposition::Iret);
    assert!(
        cpu.halted,
        "wait_for_input should block via HLT until IRQ wake"
    );
    assert_eq!(memory.read_u16(ret_ip_addr), 0x1FFE);
    assert_eq!(memory.read_u16(flags_addr) & 1, 1);
}

#[test]
fn test_sync_iret_carry_skipped_when_flag_set() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0x0100);

    let flags_addr = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.sp().wrapping_add(4));
    memory.write_u16(flags_addr, 0x0200); // No carry on stack

    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
    // With skip=true, the stack should NOT be modified
    Dos::sync_iret_carry(&cpu, &mut memory, true);
    assert_eq!(
        memory.read_u16(flags_addr) & 1,
        0,
        "sync_iret_carry with skip=true should not modify stack"
    );
}

#[test]
fn test_prep_input_wait_skipped_when_flag_set() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0x0100);

    let ret_ip_addr = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.sp());
    memory.write_u16(ret_ip_addr, 0x1234);
    let flags_addr = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.sp().wrapping_add(4));
    memory.write_u16(flags_addr, 0x0002);

    // With skip=true, stack writes should be skipped but IF should still be set
    Dos::prep_input_wait(&mut cpu, &mut memory, true);

    assert_eq!(
        memory.read_u16(ret_ip_addr),
        0x1234,
        "prep_input_wait with skip=true should not rewind IP on stack"
    );
    assert!(
        cpu.interrupts_enabled,
        "IF should still be set even with skip"
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::INTERRUPT));
    assert_eq!(
        memory.read_u16(flags_addr),
        0x0002,
        "prep_input_wait with skip=true should not modify flags on stack"
    );
}

#[test]
fn test_resolve_alias_handle_follows_chain() {
    let mut dos = Dos::new();
    dos.file_table.std_handle_aliases[5] = Some(6);
    dos.file_table.std_handle_aliases[6] = Some(7);
    dos.file_table.std_handle_aliases[7] = Some(7);

    assert_eq!(dos.resolve_alias_handle(5), 7);
    assert_eq!(dos.resolve_alias_handle(4), 4);
}

#[test]
fn test_allocate_alias_handle_skips_valid_and_aliases() {
    let mut dos = Dos::new();
    let handle = dos.file_table.allocate(
        Box::new(crate::filesystem::NulDevice),
        "NUL".to_string(),
        dos.current_psp(),
    );
    assert_eq!(handle, Some(5));
    dos.file_table.std_handle_aliases[6] = Some(7);

    let alias = dos.allocate_alias_handle();
    assert_eq!(alias, Some(7));
}

#[test]
fn test_create_psp_initializes_expected_fields() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let segment = 0x2000;

    dos.create_psp(
        &mut memory,
        segment,
        0x3000,
        0x4000,
        None,
        &["ONE".to_string(), "TWO".to_string()],
    );

    let base = Memory::seg_offset_to_linear(segment, 0);
    assert_eq!(memory.read_u8(base), 0xCD);
    assert_eq!(memory.read_u8(base + 1), 0x20);
    assert_eq!(memory.read_u16(base + 2), 0x3000);
    assert_eq!(memory.read_u16(base + 0x2C), 0x4000);
    assert_eq!(memory.read_u8(base + 0x18), 0x00); // stdin  -> SFT 0
    assert_eq!(memory.read_u8(base + 0x19), 0x01); // stdout -> SFT 1
    assert_eq!(memory.read_u8(base + 0x1A), 0x02); // stderr -> SFT 2
    assert_eq!(memory.read_u8(base + 0x1B), 0x03); // stdaux -> SFT 3
    assert_eq!(memory.read_u8(base + 0x1C), 0x04); // stdprn -> SFT 4

    let cmd_len = memory.read_u8(base + 0x80);
    assert_eq!(cmd_len, 8);
    assert_eq!(memory.read_u8(base + 0x81), b' ');
    assert_eq!(memory.read_u8(base + 0x81 + cmd_len as usize), 0x0D);
}

#[test]
fn test_save_and_restore_psp_vectors_roundtrip() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    let psp_segment = 0x2000;

    memory.write_u32(0x22 * 4, 0xAAAA1111);
    memory.write_u32(0x23 * 4, 0xBBBB2222);
    memory.write_u32(0x24 * 4, 0xCCCC3333);

    dos.save_psp_vectors(&mut memory, psp_segment);

    memory.write_u32(0x22 * 4, 0);
    memory.write_u32(0x23 * 4, 0);
    memory.write_u32(0x24 * 4, 0);

    dos.restore_psp_vectors(&mut memory, psp_segment);

    assert_eq!(memory.read_u32(0x22 * 4), 0xAAAA1111);
    assert_eq!(memory.read_u32(0x23 * 4), 0xBBBB2222);
    assert_eq!(memory.read_u32(0x24 * 4), 0xCCCC3333);
}

#[test]
fn test_init_kernel_structures_sets_first_mcb_segment() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    dos.dos_mem.init(&mut memory);

    dos.init_kernel_structures(&mut memory);

    let base = Memory::seg_offset_to_linear(LOL_SEGMENT, LOL_OFFSET);
    let mcb_segment = memory.read_u16(base.wrapping_sub(2));
    assert_eq!(mcb_segment, dos.dos_mem.first_mcb_segment());
    assert_eq!(mcb_segment, 0x0700);
}

#[test]
fn test_set_current_psp_updates_dta_defaults() {
    let mut dos = Dos::new();

    dos.set_current_psp(0x1234);

    assert_eq!(dos.current_psp(), 0x1234);
    assert_eq!(dos.dta_segment, 0x1234);
    assert_eq!(dos.dta_offset, 0x0080);
}

#[test]
fn test_int21_fcb_find_next_no_extension() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let entries = vec![
        ("ALPHA.TXT".to_string(), attrs_with_size(1)),
        ("BETA".to_string(), attrs_with_size(2)),
    ];
    dos.drives.insert(2, Box::new(ListingDrive::new(entries)));
    dos.set_dta(0x3000, 0x0000);

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "*", "*");

    cpu.regs.set_ah(0x11);
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
    assert_eq!(cpu.regs.al(), 0x00);

    cpu.regs.set_ah(0x12);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x00);

    let dta_addr = Memory::seg_offset_to_linear(0x3000, 0x0000);
    assert_eq!(memory.read_u8(dta_addr + 9), b' ');
    assert_eq!(memory.read_u8(dta_addr + 10), b' ');
    assert_eq!(memory.read_u8(dta_addr + 11), b' ');
}

#[test]
fn test_int21_fcb_delete_wildcards_missing_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 4, "*", "*");

    cpu.regs.set_ah(0x13);
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
    assert_eq!(cpu.regs.al(), 0xFF);
}

#[test]
fn test_int21_fcb_delete_wildcards_read_dir_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.drives.insert(2, Box::new(FailingReadDrive));
    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "*", "*");

    cpu.regs.set_ah(0x13);
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
    assert_eq!(cpu.regs.al(), 0xFF);
}

#[test]
fn test_int21_fcb_delete_wildcards_remove_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let entries = vec![("DEL1.TXT".to_string(), attrs_with_size(1))];
    dos.drives.insert(
        2,
        Box::new(ListingDrive::new(entries).with_remove_ok(false)),
    );
    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "DEL*", "TXT");

    cpu.regs.set_ah(0x13);
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
    assert_eq!(cpu.regs.al(), 0xFF);
}

#[test]
fn test_int21_fcb_sequential_read_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "SEQREAD", "DAT");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        99,
    );
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);

    cpu.regs.set_ah(0x14);
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
    assert_eq!(cpu.regs.al(), 0x01);
}

#[test]
fn test_int21_fcb_sequential_read_full_record() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "SEQFULL", "DAT");
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.set_dta(0x3000, 0x0000);

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(b"DATA".to_vec())),
            "C:\\SEQFULL.DAT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );

    cpu.regs.set_ah(0x14);
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
    assert_eq!(cpu.regs.al(), 0x00);
}

#[test]
fn test_int21_fcb_sequential_write_no_size_extend() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "SEQWRT", "DAT");
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.fcb_set_file_size(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        32,
    );
    dos.set_dta(0x3000, 0x0000);
    let dta_addr = Memory::seg_offset_to_linear(0x3000, 0x0000);
    for (i, b) in b"ABCD".iter().enumerate() {
        memory.write_u8(dta_addr + i, *b);
    }

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(vec![0u8; 8])),
            "C:\\SEQWRT.DAT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );

    cpu.regs.set_ah(0x15);
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
    assert_eq!(cpu.regs.al(), 0x00);
}

#[test]
fn test_int21_fcb_create_missing_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 4, "NEWFILE", "TXT");

    cpu.regs.set_ah(0x16);
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
    assert_eq!(cpu.regs.al(), 0xFF);
}

#[test]
fn test_int21_fcb_create_allocation_failure() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.drives.insert(2, Box::new(FailingReadDrive));
    // Grow the JFT to its maximum and fill every user handle (5..255).
    dos.file_table.set_handle_count(255);
    for i in 5..0xFF {
        let handle = dos.file_table.allocate(
            Box::new(CursorFile::new(vec![])),
            format!("C:\\FILL{i:02X}.TXT"),
            dos.current_psp,
        );
        assert!(handle.is_some());
    }

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "NEWFILE", "TXT");

    cpu.regs.set_ah(0x16);
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
    assert_eq!(cpu.regs.al(), 0xFF);
}

#[test]
fn test_int21_fcb_rename_wildcards_missing_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_rename_fcb(
        &mut memory,
        0x2000,
        0x0100,
        4,
        "OLD??",
        "TXT",
        "NEW??",
        "TXT",
    );

    cpu.regs.set_ah(0x17);
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
    assert_eq!(cpu.regs.al(), 0xFF);
}

#[test]
fn test_int21_fcb_rename_wildcards_read_dir_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.drives.insert(2, Box::new(FailingReadDrive));
    create_rename_fcb(
        &mut memory,
        0x2000,
        0x0100,
        3,
        "OLD??",
        "TXT",
        "NEW??",
        "TXT",
    );

    cpu.regs.set_ah(0x17);
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
    assert_eq!(cpu.regs.al(), 0xFF);
}

#[test]
fn test_int21_fcb_rename_wildcards_rename_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let entries = vec![("FOO12.TXT".to_string(), attrs_with_size(1))];
    dos.drives.insert(
        2,
        Box::new(ListingDrive::new(entries).with_rename_ok(false)),
    );
    create_rename_fcb(
        &mut memory,
        0x2000,
        0x0100,
        3,
        "FOO??",
        "TXT",
        "BAR??",
        "TXT",
    );

    cpu.regs.set_ah(0x17);
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
    assert_eq!(cpu.regs.al(), 0xFF);
}

#[test]
fn test_int21_fcb_rename_missing_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_rename_fcb(&mut memory, 0x2000, 0x0100, 4, "FILE", "TXT", "NEW", "TXT");

    cpu.regs.set_ah(0x17);
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
    assert_eq!(cpu.regs.al(), 0xFF);
}

#[test]
fn test_int21_fcb_random_read_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "RAND", "DAT");
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 0);
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        99,
    );

    cpu.regs.set_ah(0x21);
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
    assert_eq!(cpu.regs.al(), 0x01);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_fcb_random_write_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "RANDW", "DAT");
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 0);
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        99,
    );

    cpu.regs.set_ah(0x22);
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
    assert_eq!(cpu.regs.al(), 0x01);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_fcb_random_write_no_size_extend() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "RANDW", "DAT");
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 0);
    dos.fcb_set_file_size(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        32,
    );
    dos.set_dta(0x3000, 0x0000);
    let dta_addr = Memory::seg_offset_to_linear(0x3000, 0x0000);
    for (i, b) in b"WXYZ".iter().enumerate() {
        memory.write_u8(dta_addr + i, *b);
    }

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(Vec::new())),
            "C:\\RANDW.DAT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );

    cpu.regs.set_ah(0x22);
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
    assert_eq!(cpu.regs.al(), 0x00);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_fcb_get_file_size_missing_current_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.path_resolver.set_current_drive(0);
    create_test_fcb(&mut memory, 0x2000, 0x0100, 0, "SIZE", "TXT");

    cpu.regs.set_ah(0x23);
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
    assert_eq!(cpu.regs.al(), 0xFF);
}

#[test]
fn test_int21_fcb_random_block_read_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "BLOCK", "DAT");
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 0);
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        99,
    );

    cpu.regs.set_ah(0x27);
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
    assert_eq!(cpu.regs.al(), 0x01);
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn test_int21_fcb_random_block_read_overflow() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "BLOCK", "DAT");
    dos.fcb_set_record_size(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        512,
    );
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 0);

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(Vec::new())),
            "C:\\BLOCK.DAT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );

    cpu.regs.set_ah(0x27);
    cpu.regs.set_cx(200);
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
    assert_eq!(cpu.regs.al(), 0x01);
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn test_int21_fcb_random_block_read_full_records() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "BLOCK", "DAT");
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 0);
    dos.set_dta(0x3000, 0x0000);

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(b"ABCDEFGH".to_vec())),
            "C:\\BLOCK.DAT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );

    cpu.regs.set_ah(0x27);
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
    assert_eq!(cpu.regs.al(), 0x00);
    assert_eq!(cpu.regs.cx(), 2);
}

#[test]
fn test_int21_fcb_random_block_read_partial_aligned() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "BLOCK", "DAT");
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 0);
    dos.set_dta(0x3000, 0x0000);

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(b"ABCDEFGH".to_vec())),
            "C:\\BLOCK.DAT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );

    cpu.regs.set_ah(0x27);
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
    assert_eq!(cpu.regs.al(), 0x01);
    assert_eq!(cpu.regs.cx(), 2);
}

#[test]
fn test_int21_fcb_random_block_write_zero_count_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "BLOCK", "DAT");
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 1);
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        99,
    );

    cpu.regs.set_ah(0x28);
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
    assert_eq!(cpu.regs.al(), 0x01);
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn test_int21_fcb_random_block_write_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "BLOCK", "DAT");
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 0);
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        99,
    );

    cpu.regs.set_ah(0x28);
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
    assert_eq!(cpu.regs.al(), 0x01);
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn test_int21_fcb_random_block_write_overflow() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "BLOCK", "DAT");
    dos.fcb_set_record_size(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        512,
    );
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 0);

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(Vec::new())),
            "C:\\BLOCK.DAT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );

    cpu.regs.set_ah(0x28);
    cpu.regs.set_cx(200);
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
    assert_eq!(cpu.regs.al(), 0x01);
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn test_int21_fcb_random_block_write_no_size_extend() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "BLOCK", "DAT");
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 0);
    dos.fcb_set_file_size(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        32,
    );
    dos.set_dta(0x3000, 0x0000);
    let dta_addr = Memory::seg_offset_to_linear(0x3000, 0x0000);
    for (i, b) in b"DATA".iter().enumerate() {
        memory.write_u8(dta_addr + i, *b);
    }

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(Vec::new())),
            "C:\\BLOCK.DAT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );

    cpu.regs.set_ah(0x28);
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
    assert_eq!(cpu.regs.al(), 0x00);
    assert_eq!(cpu.regs.cx(), 1);
    assert_eq!(
        dos.fcb_get_file_size(&memory, Memory::seg_offset_to_linear(0x2000, 0x0100)),
        32
    );
}

#[test]
fn test_int21_parse_filename_into_fcb_separator_overflow() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let src_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for i in 0..128 {
        memory.write_u8(src_addr + i, b' ');
    }

    cpu.regs.set_ah(0x29);
    cpu.regs.set_al(0x01);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0200);

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
fn test_int21_read_stdin_ctrl_c_no_break() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // In cooked mode, Ctrl-C on STDIN always triggers INT 23h regardless
    // of ctrl_break_check.  Test that a 0x03 byte placed directly in the
    // line buffer is served as normal data (bypassing the Ctrl-C path).
    dos.ctrl_break_check = false;
    dos.stdin_line_buf = vec![0x03];

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(0); // STDIN
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
    assert_eq!(cpu.regs.ax(), 1);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    assert_eq!(memory.read_u8(addr), 0x03);
}

#[test]
fn test_int21_read_stdin_fills_count() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Pre-populate the cooked-mode line buffer (simulates a completed line).
    dos.stdin_line_buf = vec![b'A', b'B'];

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(0); // STDIN
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
    assert_eq!(cpu.regs.ax(), 2);

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    assert_eq!(memory.read_u8(addr), b'A');
    assert_eq!(memory.read_u8(addr + 1), b'B');
}

#[test]
fn test_int21_create_directory_missing_drive_ah39() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    write_asciiz(&mut memory, 0x2000, 0x0100, "D:\\MISSING");
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
fn test_int21_remove_directory_missing_drive_ah3a() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    write_asciiz(&mut memory, 0x2000, 0x0100, "D:\\MISSING");
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3);
}

#[test]
fn test_int21_change_directory_missing_drive_ah3b() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    write_asciiz(&mut memory, 0x2000, 0x0100, "D:\\MISSING");
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
fn test_int21_create_file_allocation_failure_ah3c() {
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
    fill_file_table(&mut dos);

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\FAIL.TXT");
    cpu.regs.set_ah(0x3C);
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
    assert_eq!(cpu.regs.ax(), 4);
}

#[test]
fn test_int21_open_nul_device_allocation_failure() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    fill_file_table(&mut dos);
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 4);
}

#[test]
#[allow(clippy::permissions_set_readonly_false)]
fn test_int21_open_file_read_only_write_access() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("READONLY.TXT");
    std::fs::write(&file_path, b"data").unwrap();
    let mut perms = std::fs::metadata(&file_path).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&file_path, perms).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

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

    let mut perms = std::fs::metadata(&file_path).unwrap().permissions();
    perms.set_readonly(false);
    let _ = std::fs::set_permissions(&file_path, perms);
    let _ = std::fs::remove_file(&file_path);
}

#[test]
fn test_int21_open_file_write_access_non_read_only() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("WRITEOK.TXT");
    std::fs::write(&file_path, b"data").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\WRITEOK.TXT");
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
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let handle = cpu.regs.ax();
    dos.file_table.free(handle);
    let _ = std::fs::remove_file(&file_path);
}

#[test]
fn test_int21_open_file_attr_error_allows_open() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.drives.insert(2, Box::new(AttrErrorDrive));
    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\ATTRERR.TXT");

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
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    dos.file_table.free(cpu.regs.ax());
}

#[test]
fn test_int21_open_file_allocation_failure_trace_disabled() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("FULL.TXT");
    std::fs::write(&file_path, b"data").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );
    fill_file_table(&mut dos);

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
fn test_int21_delete_file_missing_drive_ah41() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    write_asciiz(&mut memory, 0x2000, 0x0100, "D:\\DELETE.TXT");
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3);
}

#[test]
fn test_int21_force_duplicate_handle_success() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(vec![1, 2, 3])),
            "C:\\SRC.TXT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");
    let dst_handle = handle + 1;

    cpu.regs.set_ah(0x46);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(dst_handle);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(dos.file_table.is_valid(dst_handle));
}

#[test]
fn test_int21_force_duplicate_handle_invalid_dst() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(vec![1, 2, 3])),
            "C:\\SRC.TXT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");

    cpu.regs.set_ah(0x46);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(0xFFFF);

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
fn test_int21_force_duplicate_handle_std_alias() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x46);
    cpu.regs.set_bx(1);
    cpu.regs.set_cx(6);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dos.file_table.std_handle_aliases[6], Some(1));
}

#[test]
fn test_int21_force_duplicate_handle_invalid_src() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x46);
    cpu.regs.set_bx(10);
    cpu.regs.set_cx(5);

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
