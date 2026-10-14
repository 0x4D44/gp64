use super::*;

/// MDD-BUG-KILN-00277: with a tape attached, an AH=03h set-time that the
/// tape does not cover must FAIL without touching CMOS.
///
/// The arm used to call the setter first and read the tape outcome after,
/// so a replay gap mutated RTC state it never authorised and still cleared
/// CF. Worse, `set_rtc_time_bcd` preserves the current DATE, which derives
/// from the host clock unless the separate fixed-RTC option is set — so an
/// unauthorised set-time pulled host-derived state into the run, exactly
/// what the AH=02h/04h read arms refuse to do.
#[test]
fn test_int1a_ah03_tape_gap_does_not_mutate_cmos() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();
    let mut cursor = empty_tape_cursor();

    let before = cmos.get_rtc_time_bcd();

    cpu.regs.set_ah(0x03);
    cpu.regs.set_ch(0x11);
    cpu.regs.set_cl(0x22);
    cpu.regs.set_dh(0x33);
    cpu.regs.set_dl(0x00);

    bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        Some(&mut cursor),
    )
    .expect("int1a should not error");

    assert_eq!(
        cmos.get_rtc_time_bcd(),
        before,
        "an uncovered set-time must leave CMOS untouched"
    );
    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "an uncovered set-time must report failure (CF=1)"
    );
}

/// Same contract for AH=05h set-date (MDD-BUG-KILN-00277).
#[test]
fn test_int1a_ah05_tape_gap_does_not_mutate_cmos() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();
    let mut cursor = empty_tape_cursor();

    let before = cmos.get_rtc_date_bcd();

    cpu.regs.set_ah(0x05);
    cpu.regs.set_ch(0x20);
    cpu.regs.set_cl(0x99);
    cpu.regs.set_dh(0x12);
    cpu.regs.set_dl(0x31);

    bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        Some(&mut cursor),
    )
    .expect("int1a should not error");

    assert_eq!(
        cmos.get_rtc_date_bcd(),
        before,
        "an uncovered set-date must leave CMOS untouched"
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// A COVERED set-time must still write through — the fix must not turn
/// every tape-mode write into a no-op (MDD-BUG-KILN-00277).
#[test]
fn test_int1a_ah03_tape_hit_still_writes_cmos() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();
    let mut cursor = ah0205_tape_cursor(0x03, false);

    cpu.regs.set_ah(0x03);
    cpu.regs.set_ch(0x11);
    cpu.regs.set_cl(0x22);
    cpu.regs.set_dh(0x33);
    cpu.regs.set_dl(0x00);

    bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        Some(&mut cursor),
    )
    .expect("int1a should not error");

    assert_eq!(
        cmos.get_rtc_time_bcd(),
        (0x11, 0x22, 0x33),
        "a covered set-time must reach CMOS"
    );
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "the tape recorded CF=0, so the replay must report success"
    );
}

/// With NO tape attached the pre-tape behaviour is preserved verbatim:
/// the write goes through and CF is cleared (MDD-BUG-KILN-00277).
#[test]
fn test_int1a_ah03_without_tape_still_writes_cmos() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    cpu.regs.set_ah(0x03);
    cpu.regs.set_ch(0x11);
    cpu.regs.set_cl(0x22);
    cpu.regs.set_dh(0x33);
    cpu.regs.set_dl(0x00);

    bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    )
    .expect("int1a should not error");

    assert_eq!(cmos.get_rtc_time_bcd(), (0x11, 0x22, 0x33));
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int1a_ah00_tape_coverage_gap_returns_poisoned() {
    // Tape active but has no entry covering the current tick →
    // AL=0xFF, CX=DX=0xFFFF, CF=1, host BDA NOT consulted.
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();
    let mut cursor = empty_tape_cursor();

    // Pre-write distinctive BDA values that the handler MUST
    // NOT return (those would be host-fallback leakage).
    memory.write_u32(BDA_TICK_COUNT_ADDR, 0xDEAD_BEEF);
    memory.write_u8(BDA_MIDNIGHT_FLAG_ADDR, 1);

    cpu.regs.set_ah(0x00);
    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        Some(&mut cursor),
    );
    assert!(result.is_ok());

    // Poisoned values, NOT the BDA fallback.
    assert_eq!(cpu.regs.al(), 0xFF, "AL should be poisoned 0xFF");
    assert_eq!(cpu.regs.cx(), 0xFFFF, "CX should be poisoned 0xFFFF");
    assert_eq!(cpu.regs.dx(), 0xFFFF, "DX should be poisoned 0xFFFF");
    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "CF should be set on poisoned response"
    );
    // Midnight flag must NOT be cleared by a poisoned AL=0xFF.
    assert_eq!(
        memory.read_u8(BDA_MIDNIGHT_FLAG_ADDR),
        1,
        "Midnight flag must not be cleared on poisoned response"
    );
    // Counter check: one coverage_gap recorded.
    let counters = cursor.counters();
    assert_eq!(counters.coverage_gaps, 1);
    assert_eq!(counters.hits, 0);
    assert_eq!(counters.poisoned_reads, 1);
}

#[test]
fn test_int1a_ah02_tape_coverage_gap_returns_poisoned() {
    // Tape active but has no Int1aAh02_05 entry → CH=CL=DH=DL=
    // 0xFF, CF=1, cmos.get_rtc_time_bcd NOT called.
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();
    let mut cursor = empty_tape_cursor();

    cpu.regs.set_ah(0x02);
    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        Some(&mut cursor),
    );
    assert!(result.is_ok());

    assert_eq!(cpu.regs.ch(), 0xFF);
    assert_eq!(cpu.regs.cl(), 0xFF);
    assert_eq!(cpu.regs.dh(), 0xFF);
    assert_eq!(cpu.regs.dl(), 0xFF);
    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "CF should be set on poisoned response"
    );
    let counters = cursor.counters();
    assert_eq!(counters.coverage_gaps, 1);
    assert_eq!(counters.hits, 0);
    assert_eq!(counters.poisoned_reads, 1);
}

#[test]
fn test_int1a_ah04_tape_coverage_gap_returns_poisoned() {
    // Tape active but has no Int1aAh02_05 entry → CH=CL=DH=DL=
    // 0xFF, CF=1, cmos.get_rtc_date_bcd NOT called.
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();
    let mut cursor = empty_tape_cursor();

    cpu.regs.set_ah(0x04);
    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        Some(&mut cursor),
    );
    assert!(result.is_ok());

    assert_eq!(cpu.regs.ch(), 0xFF);
    assert_eq!(cpu.regs.cl(), 0xFF);
    assert_eq!(cpu.regs.dh(), 0xFF);
    assert_eq!(cpu.regs.dl(), 0xFF);
    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "CF should be set on poisoned response"
    );
    let counters = cursor.counters();
    assert_eq!(counters.coverage_gaps, 1);
    assert_eq!(counters.hits, 0);
    assert_eq!(counters.poisoned_reads, 1);
}

#[test]
fn test_int1a_interval_timer_set_stub() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    // AH=08h: Set RTC interval timer (stub)
    cpu.regs.set_ah(0x08);

    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    );
    assert!(result.is_ok());
    // Stub should succeed (no carry flag)
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int1a_interval_timer_read_stub() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    // AH=09h: Read RTC interval timer counter (stub)
    cpu.regs.set_ah(0x09);

    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    );
    assert!(result.is_ok());
    // Stub should succeed (no carry flag)
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // Stub returns 0 in CX
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn test_int1a_unsupported_function_sets_carry() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    // Use an unsupported function number
    cpu.regs.set_ah(0x0A); // Not implemented

    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    );
    assert!(result.is_ok());
    // Unsupported function should set carry flag
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_bios_tick_sets_midnight_flag_on_rollover() {
    // Note: The tick() method returns true on rollover but doesn't write to memory.
    // The caller (timer interrupt handler) is responsible for writing the flag.
    // This test verifies the tick() behavior - it should return true on rollover.
    let mut bios = Bios::new();

    // Set tick count to one less than a full day
    bios.tick_count = BIOS_TICKS_PER_DAY - 1;

    // Call tick - should rollover and return true
    let midnight = bios.tick();
    assert!(midnight, "tick() should return true on midnight rollover");
    assert_eq!(bios.tick_count, 0, "tick_count should reset to 0");

    // Normal tick should not trigger midnight
    let midnight = bios.tick();
    assert!(!midnight, "tick() should return false for normal tick");
    assert_eq!(bios.tick_count, 1);
}

#[test]
fn test_int1a_get_system_time_returns_correct_tick_parts() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    // Test with specific tick counts to verify CX:DX split
    let test_cases = [
        (0x00000000u32, 0x0000u16, 0x0000u16), // Zero
        (0x0000FFFFu32, 0x0000u16, 0xFFFFu16), // Max low word
        (0xFFFF0000u32, 0xFFFFu16, 0x0000u16), // Max high word
        (0x12345678u32, 0x1234u16, 0x5678u16), // Mixed
        (0x001800AFu32, 0x0018u16, 0x00AFu16), // Near midnight
    ];

    for (tick_count, expected_cx, expected_dx) in test_cases {
        memory.write_u32(BDA_TICK_COUNT_ADDR, tick_count);
        memory.write_u8(BDA_MIDNIGHT_FLAG_ADDR, 0);

        cpu.regs.set_ah(0x00);
        let result = bios.int1a(
            &mut cpu,
            &mut memory,
            &mut cmos,
            &gp64_hw::timer::Pit::new(),
            None,
        );
        assert!(result.is_ok());
        assert_eq!(
            cpu.regs.cx(),
            expected_cx,
            "CX mismatch for tick_count {:08X}",
            tick_count
        );
        assert_eq!(
            cpu.regs.dx(),
            expected_dx,
            "DX mismatch for tick_count {:08X}",
            tick_count
        );
    }
}

#[test]
fn test_int1a_set_system_time_updates_bios_and_bda() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    // Set different values to ensure they're both updated
    cpu.regs.set_ah(0x01);
    cpu.regs.set_cx(0x0012);
    cpu.regs.set_dx(0x3456);

    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    );
    assert!(result.is_ok());

    // Verify both BDA memory and bios.tick_count are updated
    assert_eq!(memory.read_u32(BDA_TICK_COUNT_ADDR), 0x00123456);
    assert_eq!(bios.tick_count, 0x00123456);
}

#[test]
fn test_int10_get_video_mode() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set mode via INT 10h AH=00 so BDA 0x449 is populated
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x03);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    cpu.regs.set_ah(0x0F);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x03);
    assert_eq!(cpu.regs.ah(), 80); // columns
}

#[test]
fn test_int10_set_video_mode() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x13); // Mode 13h

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    // Check BDA is updated
    assert_eq!(memory.read_u8(0x449), 0x13);
}

#[test]
fn test_int10_set_video_mode_no_clear() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set mode 0x13 first so Memory's pipeline state matches chain4.
    // This prevents apply_vga_pipeline from triggering a chain4 transition
    // that would rearrange the pre-written VRAM data.
    vga.set_mode_bios(0x13);
    memory.apply_vga_pipeline(&vga.build_pipeline_state());

    memory.write_u8(0xA0000, 0xAB);

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x93); // Mode 13h with no-clear bit set

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    assert_eq!(memory.read_u8(0xA0000), 0xAB);
    // MDD-BUG-CRUCIBLE-00424: this used to assert 0x93 — the RAW byte. BDA
    // 0x449 holds the MASKED mode; the no-clear flag lives in bit 7 of
    // 0x487, and AH=0Fh recomposes the two.
    assert_eq!(memory.read_u8(0x449), 0x13);
    assert_eq!(memory.read_u8(0x487) & 0x80, 0x80);
}

/// MDD-BUG-CRUCIBLE-00424: the no-clear flag belongs in BDA 0x487 bit 7,
/// not in the mode number, and a following ordinary mode set must CLEAR it.
///
/// 0x449 and `0x60 | (raw AL & 0x80)` in 0x487; `SVC_0F` returns
/// `BDA[0x449] | (BDA[0x487] & 0x80)`.
///
/// The second mode set is the half a single-shot test misses: a stale bit 7
/// would keep reporting "don't clear" forever.
#[test]
fn no_clear_flag_lives_in_bda_0x487_bit7_crucible424() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Mode 3 with the no-clear bit set.
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x83);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    assert_eq!(
        memory.read_u8(0x449),
        0x03,
        "0x449 must hold the masked mode"
    );
    assert_eq!(
        memory.read_u8(0x487) & 0x80,
        0x80,
        "0x487 bit 7 must carry the no-clear flag"
    );

    // AH=0Fh recomposes the caller's original byte from the two fields.
    cpu.regs.set_ah(0x0F);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x83, "AH=0Fh must recompose mode | no-clear");

    // An ordinary mode set clears the flag again.
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x03);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    assert_eq!(memory.read_u8(0x449), 0x03);
    assert_eq!(
        memory.read_u8(0x487) & 0x80,
        0x00,
        "a mode set without bit 7 must CLEAR 0x487 bit 7"
    );

    cpu.regs.set_ah(0x0F);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x03, "AH=0Fh must not report a stale flag");
}

#[test]
fn test_int10_set_cursor_position() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x03);

    cpu.regs.set_ah(0x02);
    cpu.regs.set_bh(0); // Page 0
    cpu.regs.set_dh(10); // Row
    cpu.regs.set_dl(20); // Column

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    let (row, col) = vga.get_cursor(0);
    assert_eq!(row, 10);
    assert_eq!(col, 20);
    assert_eq!(memory.read_u16(0x450), 0x0A14);
}

#[test]
fn test_int10_get_cursor_position() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 15, 25);
    memory.write_u16(0x460, 0x0607);

    cpu.regs.set_ah(0x03);
    cpu.regs.set_bh(0); // Page 0

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    assert_eq!(cpu.regs.dh(), 15); // Row
    assert_eq!(cpu.regs.dl(), 25); // Column
    assert_eq!(cpu.regs.ch(), 0x06);
    assert_eq!(cpu.regs.cl(), 0x07);
}

#[test]
fn test_int10_read_light_pen() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ax(0x04A5);
    cpu.regs.set_bx(0xBEEF);
    cpu.regs.set_cx(0xCAFE);
    cpu.regs.set_dx(0xD00D);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ax(), 0); // Light pen not triggered
    assert_eq!(cpu.regs.bx(), 0); // No stale coordinates
    assert_eq!(cpu.regs.cx(), 0);
    assert_eq!(cpu.regs.dx(), 0);
}

#[test]
fn test_int10_select_display_page() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x03);

    cpu.regs.set_ah(0x05);
    cpu.regs.set_al(1); // Page 1

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    // Check BDA is updated
    assert_eq!(memory.read_u8(0x462), 1);
}

#[test]
fn test_int10_teletype_output() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    bios.init_ivt(&mut memory, false);
    vga.set_mode_bios(0x03);

    cpu.regs.set_ah(0x0E);
    cpu.regs.set_al(b'A');
    cpu.regs.set_bh(0); // Page 0

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    // Cursor shape at BDA 0x460: depends on VGA mode initialization
    let cursor_shape = memory.read_u16(0x460);
    assert!(
        cursor_shape != 0,
        "Cursor shape should be set after mode 3 init"
    );
}

#[test]
fn test_int10_get_vga_info() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x1A);
    cpu.regs.set_al(0);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x1A); // Function supported
    assert_eq!(cpu.regs.bl(), 0x08); // Active: VGA color
    assert_eq!(cpu.regs.bh(), 0x00); // Alternate: none
}

#[test]
fn test_int10_get_font_info() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    bios.init_ivt(&mut memory, false);
    memory.write_u8(0x485, 8);
    memory.write_u8(0x484, 24);

    // Test getting 8x8 font info (BH=1). CX describes the requested
    // font, not the current BDA character height.
    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x30); // Get font info
    cpu.regs.set_bh(1); // Request INT 43h font (8x8)
    memory.write_u8(0x485, 16);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.cx(), 8); // Bytes per character (8x8 font)
    assert_eq!(cpu.regs.dl(), 24); // Rows - 1
    assert_eq!(cpu.regs.es, 0xF000); // Font in BIOS ROM
    assert_eq!(cpu.regs.bp(), Bios::ROM_FONT_8X8_OFFSET); // Points to 8x8 font

    // Test getting 8x16 font info (BH=6)
    memory.write_u8(0x485, 16);
    cpu.regs.set_bh(6); // Request 8x16 font
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.cx(), 16); // Bytes per character (8x16 font)
    assert_eq!(cpu.regs.bp(), Bios::ROM_FONT_8X16_OFFSET); // Points to 8x16 font
}

#[test]
fn test_int10_get_ega_info() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x10); // Get EGA info

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bh(), 0); // Color mode
    assert_eq!(cpu.regs.bl(), 3); // 256K memory
}

#[test]
fn test_int10_functionality_state() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    bios.init_ivt(&mut memory, false);
    memory.write_u8(0x484, 24);
    memory.write_u8(0x485, 16);

    cpu.regs.set_ah(0x1B);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0x0100);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x1B);

    let buffer_addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.di());
    assert_eq!(
        memory.read_u16(buffer_addr),
        Bios::ROM_FUNCTIONALITY_TABLE_OFFSET
    );
    assert_eq!(memory.read_u16(buffer_addr + 2), 0xF000);
    assert_eq!(memory.read_u8(buffer_addr + 0x04), memory.read_u8(0x449));
    assert_eq!(memory.read_u8(buffer_addr + 0x22), 24);
    assert_eq!(memory.read_u8(buffer_addr + 0x23), 16);
}

#[test]
fn test_int10_display_combination_code_set_get() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x1A);
    cpu.regs.set_al(0x01); // set DCC
    cpu.regs.set_bx(0x0005); // BL=0x05 (EGA mono active), BH=0x00 (no alternate)
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x1A);

    cpu.regs.set_al(0x00); // get DCC
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bx(), 0x0005);
}

#[test]
fn test_int10_toggle_blinking_bit() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x03);
    cpu.regs.set_bl(1);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let attr = vga.get_attribute_regs();
    assert!((attr[0x10] & 0x08) != 0);
    assert!((memory.read_u8(0x465) & 0x20) != 0);

    cpu.regs.set_bl(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let attr = vga.get_attribute_regs();
    assert_eq!(attr[0x10] & 0x08, 0);
    assert_eq!(memory.read_u8(0x465) & 0x20, 0);
}

#[test]
fn test_int10_palette_get_set_and_border() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x00);
    cpu.regs.set_bl(5);
    cpu.regs.set_bh(0x2A);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    cpu.regs.set_al(0x07);
    cpu.regs.set_bl(5);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bh(), 0x2A);

    cpu.regs.set_al(0x01);
    cpu.regs.set_bh(0x0C);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    cpu.regs.set_al(0x08);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bh(), 0x0C);
}

#[test]
fn test_int10_palette_all_roundtrip() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    let addr = Memory::seg_offset_to_linear(0x3000, 0x0100);
    for i in 0..16 {
        memory.write_u8(addr + i, i as u8);
    }
    memory.write_u8(addr + 16, 0x0F);

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x02);
    cpu.regs.es = 0x3000;
    cpu.regs.set_dx(0x0100);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    let out_addr = Memory::seg_offset_to_linear(0x3000, 0x0200);
    cpu.regs.set_al(0x09);
    cpu.regs.set_dx(0x0200);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    for i in 0..16 {
        assert_eq!(memory.read_u8(out_addr + i), i as u8);
    }
    assert_eq!(memory.read_u8(out_addr + 16), 0x0F);
}

#[test]
fn test_int10_dac_get_set() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x10);
    cpu.regs.set_bx(4);
    cpu.regs.set_dh(10);
    cpu.regs.set_ch(20);
    cpu.regs.set_cl(30);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    cpu.regs.set_al(0x15);
    cpu.regs.set_bl(4);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.dh(), 10);
    assert_eq!(cpu.regs.ch(), 20);
    assert_eq!(cpu.regs.cl(), 30);
}

// Regression test: INT 10h/10h and 10h/15h use BL (not BX) for register number
// BX values > 255 should use only the low byte (BL)
// Bug: Previously used full BX, causing out-of-bounds access for BX > 255
#[test]
fn test_int10_dac_uses_bl_not_bx() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set DAC entry 124 (0x7C) with known values using BX=0x0E7C (3708)
    // Real hardware uses only BL (0x7C = 124), ignoring BH (0x0E)
    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x10); // Set individual DAC register
    cpu.regs.set_bx(0x0E7C); // BL=124, BH=14 (ignored)
    cpu.regs.set_dh(63); // Red
    cpu.regs.set_ch(32); // Green
    cpu.regs.set_cl(16); // Blue
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    // Read back using the same high BX value
    cpu.regs.set_al(0x15); // Read individual DAC register
    cpu.regs.set_bx(0x0E7C); // Should read entry 124, not entry 3708
    cpu.regs.set_dh(0);
    cpu.regs.set_ch(0);
    cpu.regs.set_cl(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    // Verify we got the values from entry 124
    assert_eq!(cpu.regs.dh(), 63, "Red should be 63");
    assert_eq!(cpu.regs.ch(), 32, "Green should be 32");
    assert_eq!(cpu.regs.cl(), 16, "Blue should be 16");
}

#[test]
fn test_int10_dac_block_roundtrip() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    let addr = Memory::seg_offset_to_linear(0x3000, 0x0300);
    memory.write_u8(addr, 1);
    memory.write_u8(addr + 1, 2);
    memory.write_u8(addr + 2, 3);
    memory.write_u8(addr + 3, 4);
    memory.write_u8(addr + 4, 5);
    memory.write_u8(addr + 5, 6);

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x12);
    cpu.regs.set_bx(0);
    cpu.regs.set_cx(2);
    cpu.regs.es = 0x3000;
    cpu.regs.set_dx(0x0300);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    let out_addr = Memory::seg_offset_to_linear(0x3000, 0x0400);
    cpu.regs.set_al(0x17);
    cpu.regs.set_dx(0x0400);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    assert_eq!(memory.read_u8(out_addr), 1);
    assert_eq!(memory.read_u8(out_addr + 1), 2);
    assert_eq!(memory.read_u8(out_addr + 2), 3);
    assert_eq!(memory.read_u8(out_addr + 3), 4);
    assert_eq!(memory.read_u8(out_addr + 4), 5);
    assert_eq!(memory.read_u8(out_addr + 5), 6);
}

#[test]
fn test_int10_pel_mask_get_set() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x18);
    cpu.regs.set_bl(0xAA);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    cpu.regs.set_al(0x19);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bl(), 0xAA);
}

#[test]
fn test_int10_dac_page_get_set() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x13);
    cpu.regs.set_bl(0); // set paging mode
    cpu.regs.set_bh(1);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    cpu.regs.set_bl(1); // select page
    cpu.regs.set_bh(2);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    cpu.regs.set_al(0x1A);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bh(), 1);
    assert_eq!(cpu.regs.bl(), 2);

    cpu.regs.set_al(0x13);
    cpu.regs.set_bl(0); // set paging mode 0
    cpu.regs.set_bh(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    cpu.regs.set_bl(1);
    cpu.regs.set_bh(3);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    cpu.regs.set_al(0x1A);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bh(), 0);
    assert_eq!(cpu.regs.bl(), 3);
}

#[test]
fn test_int10_64_color_dac_pages_select_query_and_render_scanline() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    vga.set_mode_bios(0x10);

    let mut attr = vga.get_attribute_regs();
    attr[0x0A] = 0x2A;
    attr[0x10] |= 0x80;
    vga.set_attribute_regs(attr);

    let page_colors = [(1, 2, 3), (4, 5, 6), (7, 8, 9), (10, 11, 12)];
    for (page, &(r, g, b)) in page_colors.iter().enumerate() {
        vga.set_dac_reg((0x2A + page * 0x40) as u16, r, g, b);
    }
    vga.write_pixel(0, 0, 0, 0x0A, &mut memory);

    // Select four 64-color blocks through AX=1013h, BL=0, BH=0.
    cpu.regs.set_ax(0x1013);
    cpu.regs.set_bl(0);
    cpu.regs.set_bh(0);
    bios.int10(&mut cpu, &mut memory, &mut vga)
        .expect("AX=1013h should select 64-color DAC paging");

    let to_8bit = |val: u8| (val & 0x3F) << 2 | (val & 0x3F) >> 4;
    for (page, &(r, g, b)) in page_colors.iter().enumerate() {
        cpu.regs.set_ax(0x1013);
        cpu.regs.set_bl(1);
        cpu.regs.set_bh(page as u8);
        bios.int10(&mut cpu, &mut memory, &mut vga)
            .expect("AX=1013h should select a 64-color DAC page");

        cpu.regs.set_ax(0x101A);
        bios.int10(&mut cpu, &mut memory, &mut vga)
            .expect("AX=101Ah should query the selected DAC page");
        assert_eq!(
            cpu.regs.bh(),
            0,
            "page {page} should remain in 64-color mode"
        );
        assert_eq!(
            cpu.regs.bl(),
            page as u8,
            "BIOS query should report page {page}"
        );

        vga.render_scanline(0, &mut memory);
        let expected =
            (u32::from(to_8bit(r)) << 16) | (u32::from(to_8bit(g)) << 8) | u32::from(to_8bit(b));
        assert_eq!(
            vga.framebuffer()[0],
            expected,
            "active scanline pixel should use the DAC color for page {page}"
        );
    }
}

#[test]
fn test_int10_grayscale_summing() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_dac_reg(0, 10, 20, 30);

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x1B);
    cpu.regs.set_bx(0);
    cpu.regs.set_cx(1);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    let (r, g, b) = vga.get_dac_entries()[0];
    assert_eq!(r, g);
    assert_eq!(g, b);
}

#[test]
fn test_int10_dac_set_with_grayscale_enabled() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    memory.write_u8(0x489, 0x02);

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x10);
    cpu.regs.set_bx(7);
    cpu.regs.set_dh(5);
    cpu.regs.set_ch(10);
    cpu.regs.set_cl(20);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    let (r, g, b) = vga.get_dac_entries()[7];
    assert_eq!(r, g);
    assert_eq!(g, b);
}

#[test]
fn test_int10_alt_select_functions() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x30);
    cpu.regs.set_al(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x12);
    assert!((memory.read_u8(0x489) & 0x80) != 0);

    cpu.regs.set_bl(0x31);
    cpu.regs.set_al(1);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert!((memory.read_u8(0x489) & 0x08) != 0);

    cpu.regs.set_bl(0x33);
    cpu.regs.set_al(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert!((memory.read_u8(0x489) & 0x02) != 0);

    cpu.regs.set_bl(0x34);
    cpu.regs.set_al(1);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(0x487) & 0x01, 0x01);

    cpu.regs.set_bl(0x32);
    cpu.regs.set_al(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x12);

    cpu.regs.set_bl(0x36);
    cpu.regs.set_al(1);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert!((vga.get_sequencer_regs()[1] & 0x20) != 0);
}

#[test]
fn test_int10_ega_ril_registers() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0xF1);
    cpu.regs.set_bl(2);
    cpu.regs.set_bh(0x0F);
    cpu.regs.set_dx(0x08); // Sequencer
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    cpu.regs.set_ah(0xF0);
    cpu.regs.set_bl(2);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bl(), 0x0F);

    cpu.regs.set_ah(0xF1);
    cpu.regs.set_bl(0);
    cpu.regs.set_bh(0x55);
    cpu.regs.set_dx(0x20); // Misc output
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    cpu.regs.set_ah(0xF0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bl(), 0x55);

    cpu.regs.set_ah(0xF1);
    cpu.regs.set_bl(0x10);
    cpu.regs.set_bh(0x66);
    cpu.regs.set_dx(0x18); // Attribute controller
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    cpu.regs.set_ah(0xF0);
    cpu.regs.set_bl(0x10);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bl(), 0x66);

    cpu.regs.set_ah(0xF1);
    cpu.regs.set_bl(0);
    cpu.regs.set_bh(0x77);
    cpu.regs.set_dx(0x28); // Feature control
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    cpu.regs.set_ah(0xF0);
    cpu.regs.set_dx(0x38); // Graphics 2 position (3CA)
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bl(), 0x77);

    cpu.regs.set_dx(0x30); // Graphics 1 position (3CC)
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bl(), 0x55);
}

#[test]
fn test_int10_ega_ril_range_and_set() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, 0x11);
    memory.write_u8(addr + 1, 0x22);

    cpu.regs.set_ah(0xF3);
    cpu.regs.set_ch(0);
    cpu.regs.set_cl(2);
    cpu.regs.set_dx(0x10); // Graphics controller
    cpu.regs.es = 0x2000;
    cpu.regs.set_bx(0x0100);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    let out_addr = Memory::seg_offset_to_linear(0x2000, 0x0200);
    cpu.regs.set_ah(0xF2);
    cpu.regs.set_bx(0x0200);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(out_addr), 0x11);
    assert_eq!(memory.read_u8(out_addr + 1), 0x22);

    let set_addr = Memory::seg_offset_to_linear(0x2000, 0x0300);
    memory.write_u16(set_addr, 0x0008); // Sequencer group
    memory.write_u8(set_addr + 2, 1);
    memory.write_u8(set_addr + 3, 0x33);
    cpu.regs.set_ah(0xF5);
    cpu.regs.es = 0x2000;
    cpu.regs.set_bx(0x0300);
    cpu.regs.set_cx(1);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    cpu.regs.set_ah(0xF4);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(set_addr + 3), 0x33);
}

#[test]
fn test_int10_ega_ril_version_ptr() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0xFA);
    cpu.regs.set_bx(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.es, 0xC000);
    assert_eq!(cpu.regs.bx(), 0x0030);

    let ril_addr = Memory::seg_offset_to_linear(0xC000, 0x0030);
    assert_eq!(memory.read_u8(ril_addr), 0x01);
    assert_eq!(memory.read_u8(ril_addr + 1), 0x00);

    cpu.regs.set_bx(0x1234);
    cpu.regs.es = 0x2222;
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bx(), 0);
    assert_eq!(cpu.regs.es, 0x2222);
}

#[test]
fn test_int10_ega_ril_defaults_define_and_revert() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    let base = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (idx, value) in (0u8..5).enumerate() {
        memory.write_u8(base + idx, 0x10 + value);
    }

    cpu.regs.set_ah(0xF7);
    cpu.regs.set_dx(0x08); // Sequencer
    cpu.regs.es = 0x2000;
    cpu.regs.set_bx(0x0100);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    let mut regs = vga.get_sequencer_regs();
    for reg in regs.iter_mut().take(5) {
        *reg = 0;
    }
    vga.set_sequencer_regs(regs);

    cpu.regs.set_ah(0xF6);
    cpu.regs.set_dx(0x08);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    let regs = vga.get_sequencer_regs();
    for (idx, reg) in regs.iter().take(5).enumerate() {
        assert_eq!(*reg, 0x10 + idx as u8);
    }

    memory.write_u8(base, 0x5A);
    cpu.regs.set_ah(0xF7);
    cpu.regs.set_dx(0x30); // Graphics 1 position (3CC)
    cpu.regs.set_bx(0x0100);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    vga.set_misc_output(0x00);
    cpu.regs.set_ah(0xF6);
    cpu.regs.set_dx(0x30);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(vga.get_misc_output(), 0x5A);

    memory.write_u8(base, 0x3C);
    cpu.regs.set_ah(0xF7);
    cpu.regs.set_dx(0x38); // Graphics 2 position (3CA)
    cpu.regs.set_bx(0x0100);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    vga.set_feature_control(0x00);
    cpu.regs.set_ah(0xF6);
    cpu.regs.set_dx(0x38);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(vga.get_feature_control(), 0x3C);
}

#[test]
fn test_int10_font_block_spec_and_user_font() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x03);
    memory.write_u8(0x449, 0x03);

    let font_addr = Memory::seg_offset_to_linear(0x3000, 0x0200);
    for idx in 0..8usize {
        memory.write_u8(font_addr + idx, 0xF0u8.wrapping_add(idx as u8));
    }

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x10);
    cpu.regs.set_bh(8);
    cpu.regs.set_bl(0);
    cpu.regs.set_cx(1);
    cpu.regs.set_dx(0);
    cpu.regs.es = 0x3000;
    cpu.regs.set_bp(0x0200);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(0x485), 8);
    assert_eq!(memory.read_u8(0x484), 49);

    let plane2 = memory.plane(2);
    assert_eq!(plane2[0], 0xF0);

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x03);
    cpu.regs.set_bl(0x31);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(vga.get_font_block_spec(), 0x31);

    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(0);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0x0400);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    let buffer_addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.di());
    assert_eq!(memory.read_u8(buffer_addr + 0x2B), 0x05);
    assert_eq!(memory.read_u8(buffer_addr + 0x2C), 0x04);
}

#[test]
fn test_int10_video_state_get_size() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x00); // Get size
    cpu.regs.set_cx(0x0007); // Hardware + BDA + DAC

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x1C);
    assert!(cpu.regs.bx() > 0);
}

fn recalculating_font_fixture() -> (Bios, Cpu, Memory, Vga) {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    cpu.regs.set_ax(0x0003);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    (bios, cpu, memory, vga)
}

fn recalculate_font(
    bios: &mut Bios,
    cpu: &mut Cpu,
    memory: &mut Memory,
    vga: &mut Vga,
    al: u8,
    height: u8,
) {
    cpu.regs.set_ax(0x1100 | u16::from(al));
    cpu.regs.set_bx(u16::from(height) << 8);
    cpu.regs.set_cx(0);
    bios.int10(cpu, memory, vga).unwrap();
}

#[test]
fn test_recalculating_font_crtc_height() {
    let mut failures = Vec::new();
    for (al, height) in [
        (0x12, 8),
        (0x11, 14),
        (0x14, 16),
        (0x10, 12),
        (0x10, 1),
        (0x10, 255),
    ] {
        let (mut bios, mut cpu, mut memory, mut vga) = recalculating_font_fixture();
        recalculate_font(&mut bios, &mut cpu, &mut memory, &mut vga, al, height);
        let expected_height = height.min(32);
        let expected_rows = (400 / u16::from(expected_height)).min(255);
        let observed = (
            vga.get_crtc_regs()[9] & 31,
            memory.read_u16(0x485),
            u16::from(memory.read_u8(0x484)) + 1,
        );
        let expected = (
            expected_height - 1,
            u16::from(expected_height),
            expected_rows,
        );
        if observed != expected {
            failures.push((height, observed, expected));
        }
    }
    assert!(
        failures.is_empty(),
        "font geometry mismatches: {failures:?}"
    );
}

#[test]
fn test_recalculating_font_page_allocation() {
    let (mut bios, mut cpu, mut memory, mut vga) = recalculating_font_fixture();
    recalculate_font(&mut bios, &mut cpu, &mut memory, &mut vga, 0x12, 8);
    assert_eq!(memory.read_u16(0x44C), 0x2000);
}

#[test]
fn test_recalculating_font_cursor_shape() {
    let (mut bios, mut cpu, mut memory, mut vga) = recalculating_font_fixture();
    recalculate_font(&mut bios, &mut cpu, &mut memory, &mut vga, 0x12, 8);
    let crtc = vga.get_crtc_regs();
    assert_eq!(
        (memory.read_u16(0x460), crtc[0x0A], crtc[0x0B]),
        (0x0607, 6, 7)
    );
}

#[test]
fn test_recalculating_font_40_column_and_mono_geometry() {
    let mut mismatches = Vec::new();
    for mode in [0x00, 0x01, 0x02, 0x07] {
        let (mut bios, mut cpu, mut memory, mut vga) = recalculating_font_fixture();
        cpu.regs.set_ax(mode);
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
        let dimensions = vga.dimensions();
        recalculate_font(&mut bios, &mut cpu, &mut memory, &mut vga, 0x12, 8);
        let expected_page = if mode < 2 { 0x1000 } else { 0x2000 };
        let observed = (
            vga.dimensions(),
            vga.get_crtc_regs()[9] & 31,
            memory.read_u16(0x44C),
        );
        let expected = (dimensions, 7, expected_page);
        if observed != expected {
            mismatches.push((mode, observed, expected));
        }
    }
    assert!(
        mismatches.is_empty(),
        "text mode geometry mismatches: {mismatches:?}"
    );
}

#[test]
fn test_recalculating_font_normalizes_unavailable_page() {
    let (mut bios, mut cpu, mut memory, mut vga) = recalculating_font_fixture();
    cpu.regs.set_ax(0x0507);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    recalculate_font(&mut bios, &mut cpu, &mut memory, &mut vga, 0x12, 8);
    let crtc = vga.get_crtc_regs();
    assert_eq!(
        (
            memory.read_u8(0x462),
            memory.read_u16(0x44E),
            vga.get_mode().2,
            crtc[0x0C],
            crtc[0x0D]
        ),
        (0, 0, 0, 0, 0)
    );
}

#[test]
fn test_recalculating_font_page_stride_and_cursor() {
    let (mut bios, mut cpu, mut memory, mut vga) = recalculating_font_fixture();
    recalculate_font(&mut bios, &mut cpu, &mut memory, &mut vga, 0x12, 8);
    cpu.regs.set_ax(0x0501);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    cpu.regs.set_ax(0x0200);
    cpu.regs.set_bx(0x0100);
    cpu.regs.set_dx(0x3100);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    cpu.regs.set_ax(0x0941);
    cpu.regs.set_bx(0x010F);
    cpu.regs.set_cx(1);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    let crtc = vga.get_crtc_regs();
    let start = u16::from_be_bytes([crtc[0x0C], crtc[0x0D]]);
    let cursor = u16::from_be_bytes([crtc[0x0E], crtc[0x0F]]);
    assert_eq!(
        (
            memory.read_u16(0x44E),
            start,
            cursor,
            memory.read_u16(0xB8000 + 0x2000 + 49 * 160)
        ),
        (0x2000, 0x1000, 0x1000 + 49 * 80, 0x0F41)
    );
}

#[test]
fn test_recalculating_font_row49_renders() {
    let (mut bios, mut cpu, mut memory, mut vga) = recalculating_font_fixture();
    recalculate_font(&mut bios, &mut cpu, &mut memory, &mut vga, 0x12, 8);
    // An independently generated solid glyph contrasts with the empty cells.
    memory.plane_mut(2).fill(0);
    memory.plane_mut(2)[32..40].fill(0xFF);
    memory.write_u16(0xB8000 + 49 * 160, 0x0F01);
    vga.set_cursor_shape(0x20, 0);
    vga.render(&mut memory);
    let width = vga.dimensions().0;
    assert_ne!(
        vga.framebuffer()[392 * width],
        vga.framebuffer()[392 * width + 16]
    );
}

#[test]
fn test_recalculating_font_row49_scanline_renders() {
    let (mut bios, mut cpu, mut memory, mut vga) = recalculating_font_fixture();
    recalculate_font(&mut bios, &mut cpu, &mut memory, &mut vga, 0x12, 8);
    memory.plane_mut(2).fill(0);
    memory.plane_mut(2)[32..40].fill(0xFF);
    memory.write_u16(0xB8000 + 49 * 160, 0x0F01);
    vga.set_cursor_shape(0x20, 0);
    vga.render_scanline(392, &mut memory);
    let width = vga.dimensions().0;
    assert_ne!(
        vga.framebuffer()[392 * width],
        vga.framebuffer()[392 * width + 16]
    );
}

#[test]
fn test_recalculating_font_shrink_refreshes_active_page() {
    let (mut bios, mut cpu, mut memory, mut vga) = recalculating_font_fixture();
    recalculate_font(&mut bios, &mut cpu, &mut memory, &mut vga, 0x12, 8);
    cpu.regs.set_ax(0x0501);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    recalculate_font(&mut bios, &mut cpu, &mut memory, &mut vga, 0x11, 14);
    let crtc = vga.get_crtc_regs();
    assert_eq!(
        (
            memory.read_u16(0x44C),
            memory.read_u16(0x44E),
            u16::from_be_bytes([crtc[0x0C], crtc[0x0D]]),
            u16::from_be_bytes([crtc[0x0E], crtc[0x0F]])
        ),
        (0x1800, 0x1800, 0x0C00, 0x0C00)
    );
}

#[test]
fn test_font_load_only_preserves_layout() {
    for mode in [0x03u8, 0x12] {
        let (mut bios, mut cpu, mut memory, mut vga) = recalculating_font_fixture();
        cpu.regs.set_ax(u16::from(mode));
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
        let before = (
            vga.get_crtc_regs(),
            memory.read_u16(0x44C),
            memory.read_u8(0x484),
            memory.read_u16(0x460),
        );
        recalculate_font(
            &mut bios,
            &mut cpu,
            &mut memory,
            &mut vga,
            if mode == 3 { 2 } else { 0x12 },
            8,
        );
        assert_eq!(
            (
                vga.get_crtc_regs(),
                memory.read_u16(0x44C),
                memory.read_u8(0x484),
                memory.read_u16(0x460)
            ),
            before
        );
    }
}

#[test]
fn test_int10_video_state_save_header() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x01); // Save
    cpu.regs.set_cx(0x0001); // Hardware state only
    cpu.regs.es = 0x3000;
    cpu.regs.set_bx(0x0100);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x1C);

    let buffer_addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.bx());
    assert_eq!(memory.read_u16(buffer_addr), 0x20);
}

#[test]
fn test_int10_video_state_save_all_states() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Save with all state types (hardware + BDA + DAC)
    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x01); // Save
    cpu.regs.set_cx(0x0007); // All states
    cpu.regs.es = 0x3000;
    cpu.regs.set_bx(0x0100);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x1C);

    // Verify header offsets are written
    let buffer_addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.bx());
    let hw_offset = memory.read_u16(buffer_addr);
    let bda_offset = memory.read_u16(buffer_addr + 2);
    let dac_offset = memory.read_u16(buffer_addr + 4);
    assert!(hw_offset > 0);
    assert!(bda_offset > hw_offset);
    assert!(dac_offset > bda_offset);
}

#[test]
fn test_int10_video_state_save_dac_only() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set some DAC state to verify it's saved
    vga.set_dac_reg(0, 63, 0, 0); // Red
    vga.set_dac_reg(1, 0, 63, 0); // Green
    vga.set_pel_mask(0xAA);

    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x01); // Save
    cpu.regs.set_cx(0x0004); // DAC state only
    cpu.regs.es = 0x3000;
    cpu.regs.set_bx(0x0100);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x1C);
}

#[test]
fn test_int10_video_state_restore_hardware() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // First save the state
    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x01); // Save
    cpu.regs.set_cx(0x0001); // Hardware state
    cpu.regs.es = 0x3000;
    cpu.regs.set_bx(0x0100);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // Modify VGA state
    vga.set_misc_output(0xFF);

    // Now restore
    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x02); // Restore
    cpu.regs.set_cx(0x0001); // Hardware state
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x1C);
}

#[test]
fn test_int10_video_state_restore_bda() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set up BDA state
    memory.write_u8(0x449, 0x03); // Video mode
    memory.write_u8(0x410, 0x30); // Equipment byte

    // Save BDA state
    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x01); // Save
    cpu.regs.set_cx(0x0002); // BDA state
    cpu.regs.es = 0x3000;
    cpu.regs.set_bx(0x0100);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // Modify BDA
    memory.write_u8(0x449, 0x13); // Change video mode

    // Restore BDA state
    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x02); // Restore
    cpu.regs.set_cx(0x0002); // BDA state
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x1C);
    // Video mode should be restored
    assert_eq!(memory.read_u8(0x449), 0x03);
}

#[test]
fn test_int10_video_state_restore_dac() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set up DAC state
    vga.set_dac_reg(0, 63, 0, 0); // Red
    vga.set_dac_reg(1, 0, 63, 0); // Green
    vga.set_pel_mask(0xAA);

    // Save DAC state
    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x01); // Save
    cpu.regs.set_cx(0x0004); // DAC state
    cpu.regs.es = 0x3000;
    cpu.regs.set_bx(0x0100);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // Modify DAC
    vga.set_dac_reg(0, 0, 0, 63); // Blue
    vga.set_pel_mask(0x55);

    // Restore DAC state
    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x02); // Restore
    cpu.regs.set_cx(0x0004); // DAC state
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x1C);
    // PEL mask should be restored
    assert_eq!(vga.get_pel_mask(), 0xAA);
}

#[test]
fn test_int10_video_state_restore_all() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set up initial state
    memory.write_u8(0x449, 0x03);
    vga.set_pel_mask(0xBB);

    // Save all states
    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x01); // Save
    cpu.regs.set_cx(0x0007); // All states
    cpu.regs.es = 0x3000;
    cpu.regs.set_bx(0x0100);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // Modify state
    memory.write_u8(0x449, 0x13);
    vga.set_pel_mask(0x11);

    // Restore all states
    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x02); // Restore
    cpu.regs.set_cx(0x0007); // All states
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x1C);
    assert_eq!(memory.read_u8(0x449), 0x03);
    assert_eq!(vga.get_pel_mask(), 0xBB);
}

#[test]
fn test_int10_set_mode_ega_0d() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x0D); // EGA 320x200 16-color

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(0x449), 0x0D);
    assert_eq!(memory.read_u16(0x44C), 0x2000); // CRUCIBLE-00426: regen allocation, not the 0x1F40 visible frame
}

#[test]
fn test_int10_set_mode_ega_0e() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x0E); // EGA 640x200 16-color

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(0x449), 0x0E);
    assert_eq!(memory.read_u16(0x44C), 0x4000); // CRUCIBLE-00426: regen allocation, not the 0x3E80 visible frame
}

#[test]
fn test_int10_set_mode_ega_10() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x10); // EGA 640x350 16-color

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(0x449), 0x10);
    // Mode 10h uses 8x14 font: 350/14 = 25 rows
    assert_eq!(memory.read_u8(0x484), 24); // rows - 1
    assert_eq!(memory.read_u16(0x44C), 0x8000); // CRUCIBLE-00426: regen allocation, not the 0x6D60 visible frame
}

#[test]
fn test_int10_set_mode_vga_12() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x12); // VGA 640x480 16-color

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(0x449), 0x12);
    // Mode 12h uses 8x16 font: 480/16 = 30 rows
    assert_eq!(memory.read_u8(0x484), 29); // rows - 1
    assert_eq!(memory.read_u16(0x44C), 0xA000); // CRUCIBLE-00426: regen allocation, not the 0x9600 visible frame
}

/// MDD-BUG-CRUCIBLE-00426: BDA 0x44C is the BIOS regen-buffer ALLOCATION
/// for planar modes, and a nonzero page must land on that boundary.
///
/// The four mode-set tests above check 0x44C in isolation; this checks the
/// consequence, which is what actually broke. Publishing the visible-frame
/// byte count (0x1F40 for mode 0Dh rather than 0x2000) put page 1 at
/// 0x1F40 — 192 bytes INSIDE page 0's allocation — so the two pages
/// overlapped and page 1 rendered the tail of page 0.
///
#[test]
fn planar_page_boundaries_follow_the_regen_allocation_crucible426() {
    // (mode, BIOS regeneration-buffer allocation).
    let cases: [(u8, usize); 4] = [
        (0x0D, 0x2000),
        (0x0E, 0x4000),
        (0x10, 0x8000),
        (0x12, 0xA000),
    ];

    for (mode, allocation) in cases {
        let mut bios = Bios::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        let mut vga = Vga::new();

        cpu.regs.set_ah(0x00);
        cpu.regs.set_al(mode);
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

        assert_eq!(
            memory.read_u16(0x44C) as usize,
            allocation,
            "mode {mode:#04X}: BDA 0x44C must be the regen-buffer allocation"
        );

        // Page 1 must start exactly one allocation in — never inside page 0.
        cpu.regs.set_ah(0x05);
        cpu.regs.set_al(1);
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

        let start_bytes = memory.read_u16(0x44E) as usize;
        assert_eq!(
            start_bytes,
            allocation & 0xFFFF,
            "mode {mode:#04X}: page 1 must begin at its allocation boundary"
        );

        // And the CRTC must be aimed there too. Planar modes have vram_ratio
        // 4, so the display-start word equals the byte offset
        // (byte * 4 / 4) — see MDD-BUG-CRUCIBLE-00427.
        let crtc = vga.get_crtc_regs();
        let crtc_start = ((crtc[0x0C] as usize) << 8) | crtc[0x0D] as usize;
        assert_eq!(
            crtc_start,
            (allocation) & 0xFFFF,
            "mode {mode:#04X}: CRTC start must aim at the page-1 boundary"
        );
    }
}

/// Regression: after a Mode 13h round trip, INT 10h AH=00 mode 3
/// must restore the standard 8×16 ROM font in plane 2.  Real VGA
/// BIOS POST does this on every cold text-mode set.
///
/// Returning to text mode with plane 2 still holding chain-4 pixel
/// residue can leave garbled glyphs. The text renderer reads plane 2
/// whenever the `custom_font_loaded` latch is set.
#[test]
fn test_int10_text_mode_reloads_font_after_graphics_round_trip() {
    use gp64_hw::vga::CP437_FONT_8X16;
    const PLANE2_CHAR_SLOT: usize = 32;

    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Step 1: mode 3.  After this, plane 2 should contain
    // CP437_FONT_8X16 at block 0.
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x03);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    for ch in 0u8..=255 {
        for row in 0..16usize {
            let p2 = memory.plane(2)[ch as usize * PLANE2_CHAR_SLOT + row];
            let expected = CP437_FONT_8X16[ch as usize * 16 + row];
            assert_eq!(
                p2, expected,
                "plane 2 char {ch:#04X} row {row} mismatch after mode 3 set"
            );
        }
    }

    // Step 2: mode 13h.  The clear in INT 10h AH=00 zeroes the first
    // gameplay would scatter non-zero bytes across plane 2.  Simulate
    // that by writing a sentinel pattern over the font region after
    // the mode set.
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x13);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    for offset in 0..(256 * PLANE2_CHAR_SLOT) {
        memory.plane_mut(2)[offset] = 0xCC ^ (offset as u8);
    }

    // Step 3: back to mode 3.  The fix reloads the standard font.
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x03);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    for ch in 0u8..=255 {
        for row in 0..16usize {
            let p2 = memory.plane(2)[ch as usize * PLANE2_CHAR_SLOT + row];
            let expected = CP437_FONT_8X16[ch as usize * 16 + row];
            assert_eq!(
                p2, expected,
                "plane 2 char {ch:#04X} row {row} mismatch after \
                 mode 13h → mode 3 round trip"
            );
        }
    }
}

/// Companion: AL bit 7 set means "warm reset" — VRAM and plane-2
/// font are preserved.  A custom font uploaded to plane 2 must
/// survive a mode 3 (with bit 7) re-issue.
#[test]
fn test_int10_text_mode_warm_reset_preserves_plane2() {
    const PLANE2_CHAR_SLOT: usize = 32;

    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set mode 3 cold, then overwrite plane 2 with a sentinel that
    // a guest font upload would leave behind.
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x03);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    for ch in 0u8..=255 {
        for row in 0..16usize {
            memory.plane_mut(2)[ch as usize * PLANE2_CHAR_SLOT + row] = 0xA5 ^ ch ^ row as u8;
        }
    }

    // Warm re-issue: AL = 0x83 (mode 3 with bit 7 set).  Must NOT
    // clear VRAM and must NOT reload the ROM font.
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x83);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    for ch in 0u8..=255 {
        for row in 0..16usize {
            let p2 = memory.plane(2)[ch as usize * PLANE2_CHAR_SLOT + row];
            let expected = 0xA5u8 ^ ch ^ row as u8;
            assert_eq!(
                p2, expected,
                "warm mode 3 set clobbered plane 2 at char {ch:#04X} row {row}"
            );
        }
    }
}

#[test]
fn test_int16_get_shift_flags() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    bda::kbd_set_shift_flags(&mut memory, 0x05);

    cpu.regs.set_ah(0x02);

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x05);
}

#[test]
fn test_int16_keyboard_intercept() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    bda::kbd_enqueue(&mut memory, 0x1E, b'a');

    cpu.regs.set_ah(0x05);
    cpu.regs.set_ch(0x30);
    cpu.regs.set_cl(b'b');

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0); // Success
}

#[test]
fn test_int16_set_typematic_rate() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    // AL=05h: Set rate and delay
    cpu.regs.set_ah(0x03);
    cpu.regs.set_al(0x05);
    cpu.regs.set_bh(2); // delay=2 (750ms)
    cpu.regs.set_bl(15); // rate=15 (~8 cps)

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    assert_eq!(keyboard.get_typematic_delay(), 2);
    assert_eq!(keyboard.get_typematic_rate(), 15);
}

#[test]
fn test_kbd_trace_allow_is_instance_local_and_gated() {
    use tracing_subscriber::layer::SubscriberExt;

    let bios = Bios::new();
    // A disabled target must not spend this BIOS instance's budget.
    let disabled =
        tracing_subscriber::registry().with(tracing_subscriber::filter::EnvFilter::new("off"));
    tracing::subscriber::with_default(disabled, || {
        assert!(!bios.kbd_trace_allow());
    });

    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::filter::EnvFilter::new("gp64::kbd::int=debug"),
    );
    tracing::subscriber::with_default(subscriber, || {
        for _ in 0..5000 {
            assert!(bios.kbd_trace_allow());
        }
        assert!(!bios.kbd_trace_allow());

        // A separate BIOS starts with a fresh diagnostic budget.
        assert!(Bios::new().kbd_trace_allow());

        // Snapshot clones carry an independent budget, not a shared atomic.
        let source = Bios::new();
        assert!(source.kbd_trace_allow());
        let clone = source.clone();
        for _ in 1..5000 {
            assert!(source.kbd_trace_allow());
        }
        assert!(!source.kbd_trace_allow());
        assert!(clone.kbd_trace_allow());
    });
}

#[test]
fn test_int10_set_palette_individual() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x00); // Set individual palette register
    cpu.regs.set_bl(0); // Register 0
    cpu.regs.set_bh(0x3F); // Value

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_set_dac_individual() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x10); // Set individual DAC register
    cpu.regs.set_bx(0); // Register 0
    cpu.regs.set_dh(63); // R
    cpu.regs.set_ch(32); // G
    cpu.regs.set_cl(16); // B

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_set_dac_block() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set up palette data in memory
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    // RGB triplets for colors 0-2
    memory.write_u8(addr, 63);
    memory.write_u8(addr + 1, 0);
    memory.write_u8(addr + 2, 0);
    memory.write_u8(addr + 3, 0);
    memory.write_u8(addr + 4, 63);
    memory.write_u8(addr + 5, 0);
    memory.write_u8(addr + 6, 0);
    memory.write_u8(addr + 7, 0);
    memory.write_u8(addr + 8, 63);

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x12); // Set block of DAC registers
    cpu.regs.set_bx(0); // Start register
    cpu.regs.set_cx(3); // Count
    cpu.regs.es = 0x2000;
    cpu.regs.set_dx(0x0100);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_write_pixel() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x13);

    cpu.regs.set_ah(0x0C);
    cpu.regs.set_al(15); // Color
    cpu.regs.set_bh(0); // Page
    cpu.regs.set_cx(100); // X
    cpu.regs.set_dx(50); // Y

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_read_pixel() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x13);

    // Write a pixel first
    vga.write_pixel(0, 100, 50, 25, &mut memory);

    cpu.regs.set_ah(0x0D);
    cpu.regs.set_bh(0); // Page
    cpu.regs.set_cx(100); // X
    cpu.regs.set_dx(50); // Y

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 25);
}

#[test]
fn test_int10_cga_palette() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x04);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let initial_color_select = vga.get_cga_color_select();
    let initial_bda_color_select = memory.read_u8(0x466);

    // Set background color
    cpu.regs.set_ah(0x0B);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_bl(0x05); // Background color

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let attr = vga.get_attribute_regs();
    assert_eq!(attr[0], 0x05);
    assert_eq!(attr[1] & 0x10, 0);
    assert_eq!(attr[2] & 0x10, 0);
    assert_eq!(attr[3] & 0x10, 0);
    assert_eq!(vga.get_cga_color_select(), initial_color_select);
    assert_eq!(memory.read_u8(0x466), initial_bda_color_select);

    // Set palette
    cpu.regs.set_ah(0x0B);
    cpu.regs.set_bh(0x01);
    cpu.regs.set_bl(0x01); // Palette 1, low intensity

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let attr = vga.get_attribute_regs();
    assert_eq!(attr[0], 0x05);
    assert_eq!(attr[1] & 0x01, 0x01);
    assert_eq!(attr[2] & 0x01, 0x01);
    assert_eq!(attr[3] & 0x01, 0x01);
    assert_eq!(vga.get_cga_color_select(), initial_color_select);
    assert_eq!(memory.read_u8(0x466), initial_bda_color_select);

    // BH=01 only changes the AC palette-select bit, preserving intensity.
    // To set intensity, use BH=00 with BL bit 4 set.
    cpu.regs.set_ah(0x0B);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_bl(0x10); // Background black + intensity on
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let attr = vga.get_attribute_regs();
    assert_eq!(attr[0], 0x00);
    assert_eq!(attr[1] & 0x10, 0x10);
    assert_eq!(attr[2] & 0x10, 0x10);
    assert_eq!(attr[3] & 0x10, 0x10);
    assert_eq!(vga.get_cga_color_select(), initial_color_select);
    assert_eq!(memory.read_u8(0x466), initial_bda_color_select);
}

#[test]
fn int10_ah0b_programs_attribute_palette_kiln78() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    let mut attr = vga.get_attribute_regs();
    attr[0] = 0x02;
    attr[1] = 0x03;
    attr[2] = 0x05;
    attr[3] = 0x07;
    attr[0x11] = 0x2D;
    vga.set_attribute_regs(attr);
    vga.set_cga_color_select(0x35);
    memory.write_u8(0x466, 0x5A);

    // BH=00h maps the CGA IRGB background into AC0 and sets intensity on
    // AC1-AC3, without using the overscan register or CGA shadow state.
    cpu.regs.set_ah(0x0B);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_bl(0x1F); // bright white + intensity
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    let attr = vga.get_attribute_regs();
    assert_eq!(
        attr[0], 0x17,
        "bright background must use the AC bright bank"
    );
    assert_eq!(attr[1], 0x13);
    assert_eq!(attr[2], 0x15);
    assert_eq!(attr[3], 0x17);
    assert_eq!(attr[0x11], 0x2D, "AH=0Bh must not alter overscan");
    assert_eq!(vga.get_cga_color_select(), 0x35);
    assert_eq!(memory.read_u8(0x466), 0x5A);

    // BH=01h changes only the palette-select bit on AC1-AC3.
    cpu.regs.set_ah(0x0B);
    cpu.regs.set_bh(0x01);
    cpu.regs.set_bl(0x00);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    let attr = vga.get_attribute_regs();
    assert_eq!(attr[1], 0x12);
    assert_eq!(attr[2], 0x14);
    assert_eq!(attr[3], 0x16);

    // Reserved BH values leave all attribute registers unchanged.
    let before = vga.get_attribute_regs();
    cpu.regs.set_bh(0x02);
    cpu.regs.set_bl(0x1F);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(vga.get_attribute_regs(), before);
}

#[test]
fn test_int10_set_cursor_shape() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x01); // Set cursor shape
    cpu.regs.set_ch(0);
    cpu.regs.set_cl(15);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_set_cursor_shape_cga_emulation() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set up for 16-scanline character height (mode 3 default)
    memory.write_u8(0x485, 16);
    // Cursor emulation enabled (bit 0 of BDA 0x487 = 0)
    memory.write_u8(0x487, 0x60);

    // CGA underscore cursor (start=6, end=7) should be scaled to (14, 15)
    cpu.regs.set_ah(0x01);
    cpu.regs.set_ch(6);
    cpu.regs.set_cl(7);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    let crtc = vga.get_crtc_regs();
    assert_eq!(crtc[0x0A], 14, "cursor start should be scaled from 6 to 14");
    assert_eq!(crtc[0x0B], 15, "cursor end should be scaled from 7 to 15");
    // BDA stores the original (unemulated) values
    assert_eq!(memory.read_u16(0x460), (6 << 8) | 7);

    // The BIOS scales the low 5 CRTC scanline bits, so a CGA block cursor
    // starts at 1 rather than preserving raw start 0.
    cpu.regs.set_ah(0x01);
    cpu.regs.set_ch(0);
    cpu.regs.set_cl(7);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    let crtc = vga.get_crtc_regs();
    assert_eq!(
        crtc[0x0A], 1,
        "block cursor start follows BIOS scanline scaling"
    );
    assert_eq!(crtc[0x0B], 15, "block cursor end scaled to 15");

    // CGA half-block cursor (start=4, end=7) → (9, 15)
    cpu.regs.set_ah(0x01);
    cpu.regs.set_ch(4);
    cpu.regs.set_cl(7);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    let crtc = vga.get_crtc_regs();
    assert_eq!(crtc[0x0A], 9, "half-block start scaled from 4 to 9");
    assert_eq!(crtc[0x0B], 15, "half-block end scaled from 4+7 to 15");
}

#[test]
fn test_int10_set_cursor_shape_high_end_bits_scale() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    memory.write_u8(0x485, 16);
    memory.write_u8(0x487, 0x60); // emulation enabled

    cpu.regs.set_ah(0x01);
    cpu.regs.set_ch(0x1F);
    cpu.regs.set_cl(0x22);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    let crtc = vga.get_crtc_regs();
    assert_eq!(crtc[0x0A], 0x3F, "start scales from CH low 5 bits");
    assert_eq!(crtc[0x0B], 0x05, "end scales from CL low 5 bits");
    assert_eq!(memory.read_u16(0x460), 0x1F22, "BDA stores raw shape");
}

#[test]
fn test_int10_set_cursor_shape_cga_emulation_disabled() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    memory.write_u8(0x485, 16);
    // Disable cursor emulation (bit 0 of BDA 0x487 = 1)
    memory.write_u8(0x487, 0x61);

    // CGA underscore cursor should NOT be scaled when emulation disabled
    cpu.regs.set_ah(0x01);
    cpu.regs.set_ch(6);
    cpu.regs.set_cl(7);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    let crtc = vga.get_crtc_regs();
    assert_eq!(crtc[0x0A], 6, "no scaling when emulation disabled");
    assert_eq!(crtc[0x0B], 7, "no scaling when emulation disabled");
}

#[test]
fn test_int10_set_cursor_shape_no_emulation_for_native_values() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    memory.write_u8(0x485, 16);
    memory.write_u8(0x487, 0x60); // emulation enabled

    // Values >= 8 are native VGA values — should not be scaled
    cpu.regs.set_ah(0x01);
    cpu.regs.set_ch(14);
    cpu.regs.set_cl(15);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    let crtc = vga.get_crtc_regs();
    assert_eq!(crtc[0x0A], 14, "native values pass through unchanged");
    assert_eq!(crtc[0x0B], 15, "native values pass through unchanged");
}

#[test]
fn test_int10_set_cursor_shape_ega_14line() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // EGA-style 14-scanline characters
    memory.write_u8(0x485, 14);
    memory.write_u8(0x487, 0x60); // emulation enabled

    // CGA underscore (6,7) → (12, 13) for 14-line cells
    cpu.regs.set_ah(0x01);
    cpu.regs.set_ch(6);
    cpu.regs.set_cl(7);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    let crtc = vga.get_crtc_regs();
    assert_eq!(crtc[0x0A], 12);
    assert_eq!(crtc[0x0B], 13);
}

#[test]
fn test_int10_set_cursor_shape_8line_no_scaling() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // 8-scanline characters — no scaling needed
    memory.write_u8(0x485, 8);
    memory.write_u8(0x487, 0x60);

    cpu.regs.set_ah(0x01);
    cpu.regs.set_ch(6);
    cpu.regs.set_cl(7);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    let crtc = vga.get_crtc_regs();
    assert_eq!(crtc[0x0A], 6, "no scaling for 8-line cells");
    assert_eq!(crtc[0x0B], 7, "no scaling for 8-line cells");
}

#[test]
fn test_int10_scroll_up() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x03);

    cpu.regs.set_ah(0x06); // Scroll up
    cpu.regs.set_al(1); // Lines
    cpu.regs.set_bh(0x07); // Attribute
    cpu.regs.set_ch(0); // Top
    cpu.regs.set_cl(0); // Left
    cpu.regs.set_dh(24); // Bottom
    cpu.regs.set_dl(79); // Right

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_scroll_down() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x03);

    cpu.regs.set_ah(0x07); // Scroll down
    cpu.regs.set_al(1); // Lines
    cpu.regs.set_bh(0x07); // Attribute
    cpu.regs.set_ch(0); // Top
    cpu.regs.set_cl(0); // Left
    cpu.regs.set_dh(24); // Bottom
    cpu.regs.set_dl(79); // Right

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_scroll_up_clamps_window_to_bda_geometry() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    set_text_mode_03(&mut bios, &mut cpu, &mut memory, &mut vga);
    write_text_cell(&mut memory, 23, 79, b'M', 0x1E);
    write_text_cell(&mut memory, 25, 0, b'X', 0x4F);

    cpu.regs.set_ax(0x0601);
    cpu.regs.set_bh(0x07);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dx(0xFFFF);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    assert_eq!(
        read_text_cell(&memory, 22, 79),
        (b'M', 0x1E),
        "row 23 marker should scroll up inside the live BDA window"
    );
    assert_eq!(
        read_text_cell(&memory, 24, 0),
        (b' ', 0x07),
        "clamped bottom row should be blank-filled, not copied from row 25"
    );
    assert_eq!(
        read_text_cell(&memory, 25, 0),
        (b'X', 0x4F),
        "out-of-screen row 25 must not be touched"
    );
}

#[test]
fn test_int10_scroll_down_clamps_window_to_bda_geometry() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    set_text_mode_03(&mut bios, &mut cpu, &mut memory, &mut vga);
    write_text_cell(&mut memory, 1, 79, b'N', 0x2E);
    write_text_cell(&mut memory, 25, 0, b'Y', 0x5F);

    cpu.regs.set_ax(0x0701);
    cpu.regs.set_bh(0x07);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dx(0xFFFF);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    assert_eq!(
        read_text_cell(&memory, 2, 79),
        (b'N', 0x2E),
        "row 1 marker should scroll down inside the live BDA window"
    );
    assert_eq!(
        read_text_cell(&memory, 0, 0),
        (b' ', 0x07),
        "clamped top row should be blank-filled"
    );
    assert_eq!(
        read_text_cell(&memory, 25, 0),
        (b'Y', 0x5F),
        "out-of-screen row 25 must not be touched"
    );
}

#[test]
fn test_int10_scroll_up_empty_when_top_exceeds_bda_bottom() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    set_text_mode_03(&mut bios, &mut cpu, &mut memory, &mut vga);
    write_text_cell(&mut memory, 24, 0, b'T', 0x6E);

    cpu.regs.set_ax(0x0601);
    cpu.regs.set_bh(0x07);
    cpu.regs.set_cx(0x1900);
    cpu.regs.set_dx(0xFFFF);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    assert_eq!(
        read_text_cell(&memory, 24, 0),
        (b'T', 0x6E),
        "top row beyond the live BDA window should make the scroll empty"
    );
}

#[test]
fn test_int10_scroll_down_empty_when_left_exceeds_bda_right() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    set_text_mode_03(&mut bios, &mut cpu, &mut memory, &mut vga);
    write_text_cell(&mut memory, 0, 79, b'L', 0x3E);
    write_text_cell(&mut memory, 1, 79, b'R', 0x4E);

    cpu.regs.set_ax(0x0701);
    cpu.regs.set_bh(0x07);
    cpu.regs.set_cx(0x0050);
    cpu.regs.set_dx(0xFFFF);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    assert_eq!(
        read_text_cell(&memory, 0, 79),
        (b'L', 0x3E),
        "left column beyond the live BDA window should make the scroll empty"
    );
    assert_eq!(
        read_text_cell(&memory, 1, 79),
        (b'R', 0x4E),
        "empty scroll must not move the rightmost visible column"
    );
}

#[test]
fn test_int10_read_char_attr() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 0, 0);
    vga.write_char_attr(0, b'X', 0x70, 1, &mut memory);

    cpu.regs.set_ah(0x08); // Read char and attr
    cpu.regs.set_bh(0); // Page 0

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), b'X');
    assert_eq!(cpu.regs.ah(), 0x70);
}

/// MDD-BUG-CRUCIBLE-00598 — an invalid page must not send the BDA cursor
/// write outside the eight-word table at 0x450.
///
/// `Vga::set_cursor`/`teletype` fold an out-of-range page onto page 0, so
/// computing the BDA slot from the RAW `BH` split the two: VGA page 0
/// moved while the write landed on unrelated BDA state. `BH=8` hits 0x460
/// (the cursor SHAPE word), `BH=9` hits 0x462 (active page + the low byte
/// of the CRTC base), `BH=0xFF` lands far past the table.
///
/// AH=02h was fixed incidentally by `de1a550ea` (2026-08-01), which routed
/// it through the normalizing `set_cursor_position`; it is covered here so
/// the pair cannot drift apart again. AH=0Eh was still raw.
#[test]
fn int10_invalid_page_cursor_writes_stay_inside_the_bda_cursor_table() {
    for ah in [0x02u8, 0x0E] {
        for bh in [0x08u8, 0x09, 0xFF] {
            let mut bios = Bios::new();
            let mut cpu = Cpu::new();
            let mut memory = Memory::new();
            let mut vga = Vga::new();
            set_text_mode_03(&mut bios, &mut cpu, &mut memory, &mut vga);

            // BDA words that sit immediately past the cursor table.
            let shape_before = memory.read_u16(0x460);
            let active_page_before = memory.read_u8(0x462);
            let crtc_base_lo_before = memory.read_u8(0x463);
            // The slot the RAW page would have targeted.
            let escaped = 0x450 + (bh as usize) * 2;
            let escaped_before = memory.read_u16(escaped);

            vga.set_cursor(0, 3, 4);
            cpu.regs.set_ah(ah);
            cpu.regs.set_bh(bh);
            if ah == 0x02 {
                cpu.regs.set_dh(7);
                cpu.regs.set_dl(9);
            } else {
                cpu.regs.set_al(b'Z');
            }
            bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

            let label = format!("AH={ah:#04X} BH={bh:#04X}");

            // The page-zero VGA cursor and the page-zero BDA slot agree.
            let (row, col) = vga.get_cursor(0);
            let bda = memory.read_u16(0x450);
            assert_eq!(
                bda,
                ((row as u16) << 8) | col as u16,
                "{label}: BDA page-0 cursor disagrees with the VGA cursor"
            );

            // Nothing past the cursor table moved.
            assert_eq!(
                memory.read_u16(0x460),
                shape_before,
                "{label}: corrupted the BDA cursor SHAPE word at 0x460"
            );
            assert_eq!(
                memory.read_u8(0x462),
                active_page_before,
                "{label}: corrupted the BDA active-page byte at 0x462"
            );
            assert_eq!(
                memory.read_u8(0x463),
                crtc_base_lo_before,
                "{label}: corrupted the BDA CRTC base low byte at 0x463"
            );
            if escaped > 0x450 + 7 * 2 {
                assert_eq!(
                    memory.read_u16(escaped),
                    escaped_before,
                    "{label}: wrote the cursor outside the 0x450 table"
                );
            }
        }
    }
}

#[test]
fn test_int10_write_char_and_attr() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 0, 0);

    cpu.regs.set_ah(0x09); // Write char and attr
    cpu.regs.set_al(b'Y');
    cpu.regs.set_bl(0x0F); // Attribute
    cpu.regs.set_bh(0); // Page 0
    cpu.regs.set_cx(3); // Count

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}
fn empty_tape_cursor() -> gp64_types::gremlin_tape::RtcTapeCursor {
    let tape = gp64_types::gremlin_tape::RtcTape {
        schema_version: 1,
        capture_date: String::new(),
        target_workload: String::new(),
        entries: Vec::new(),
    };
    gp64_types::gremlin_tape::RtcTapeCursor::new(tape)
}

/// Build a cursor whose single entry covers AH=03h/05h at tick 0.
fn ah0205_tape_cursor(ah: u8, cf: bool) -> gp64_types::gremlin_tape::RtcTapeCursor {
    let tape = gp64_types::gremlin_tape::RtcTape {
        schema_version: 1,
        capture_date: String::new(),
        target_workload: String::new(),
        entries: vec![gp64_types::gremlin_tape::TapeEntry {
            at_pit_tick: 0,
            seq: 0,
            source: gp64_types::gremlin_tape::TapeSource::Int1aAh0205 {
                ah,
                ch: 0,
                cl: 0,
                dh: 0,
                dl: 0,
                cf,
            },
        }],
    };
    gp64_types::gremlin_tape::RtcTapeCursor::new(tape)
}

fn write_text_cell(memory: &mut Memory, row: usize, col: usize, ch: u8, attr: u8) {
    let addr = text_cell_addr(row, col);
    memory.write_u8(addr, ch);
    memory.write_u8(addr + 1, attr);
}

#[test]
fn int10_invalid_text_pages_fall_back_to_page_zero_without_escaping_vga() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    set_text_mode_03(&mut bios, &mut cpu, &mut memory, &mut vga);
    vga.set_cursor(0, 3, 4);
    write_text_cell(&mut memory, 3, 4, b'R', 0x2A);

    let escaped_cursor_addr = 0xB8000 + 0xFF * 0x1000 + (3 * 80 + 4) * 2;
    memory.write_u8(escaped_cursor_addr, 0xA5);
    memory.write_u8(escaped_cursor_addr + 1, 0x5A);
    let escaped_bda_cursor_addr = 0x450 + 0xFF * 2;
    memory.write_u16(escaped_bda_cursor_addr, 0xA55A);

    cpu.regs.set_ax(0x0800);
    cpu.regs.set_bh(0xFF);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.ax(), 0x2A52, "AH=08 must read page zero");

    cpu.regs.set_ax(0x0957);
    cpu.regs.set_bh(0xFF);
    cpu.regs.set_bl(0x1E);
    cpu.regs.set_cx(1);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(read_text_cell(&memory, 3, 4), (b'W', 0x1E));

    cpu.regs.set_ax(0x0A43);
    cpu.regs.set_bh(0xFF);
    cpu.regs.set_cx(1);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(
        read_text_cell(&memory, 3, 4),
        (b'C', 0x1E),
        "AH=0A must preserve the page-zero attribute"
    );
    assert_eq!(
        (
            memory.read_u8(escaped_cursor_addr),
            memory.read_u8(escaped_cursor_addr + 1)
        ),
        (0xA5, 0x5A),
        "AH=09/AH=0A must not write beyond the VGA aperture"
    );

    let source = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(source, b'S');
    let escaped_string_addr = 0xB8000 + 0xFF * 0x1000 + (5 * 80 + 6) * 2;
    memory.write_u8(escaped_string_addr, 0xC3);
    memory.write_u8(escaped_string_addr + 1, 0x3C);

    cpu.regs.set_ax(0x1300);
    cpu.regs.set_bh(0xFF);
    cpu.regs.set_bl(0x4E);
    cpu.regs.set_cx(1);
    cpu.regs.set_dh(5);
    cpu.regs.set_dl(6);
    cpu.regs.es = 0x2000;
    cpu.regs.set_bp(0x0100);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    assert_eq!(
        read_text_cell(&memory, 5, 6),
        (b'S', 0x4E),
        "AH=13 must apply its requested position to page zero"
    );
    assert_eq!(
        (
            memory.read_u8(escaped_string_addr),
            memory.read_u8(escaped_string_addr + 1)
        ),
        (0xC3, 0x3C),
        "AH=13 must not write beyond the VGA aperture"
    );
    assert_eq!(vga.get_cursor(0), (3, 4));
    assert_eq!(memory.read_u16(0x450), 0x0304);
    assert_eq!(
        memory.read_u16(escaped_bda_cursor_addr),
        0xA55A,
        "AH=13 must not write beyond the eight BDA cursor slots"
    );
}

// BIOS font loads populate native VGA banks with synthetic glyphs; selection
// tests check both device rendering and BIOS primary/secondary metadata.
fn font_selection_fixture() -> (Bios, Cpu, Memory, Vga) {
    let (mut bios, mut cpu, mut memory, mut vga) =
        (Bios::new(), Cpu::new(), Memory::new(), Vga::new());
    cpu.regs.set_ax(0x0003);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    for (bank, pattern) in [(0, 0x80), (1, 0x40), (4, 0x80), (5, 0x40)] {
        // Independent single-glyph bitmaps: one foreground pixel in column 0/1.
        for row in 0..16 {
            memory.write_u8(0x30000 + row, pattern);
        }
        cpu.regs.set_ax(0x1100);
        cpu.regs.set_bh(16);
        cpu.regs.set_bl(bank);
        cpu.regs.set_cx(1);
        cpu.regs.set_dx(1);
        cpu.regs.es = 0x3000;
        cpu.regs.set_bp(0);
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    }
    memory.write_u16(0xB8000, 0x0701);
    memory.write_u16(0xB8002, 0x0F01);
    vga.set_cursor_shape(0x20, 0);
    (bios, cpu, memory, vga)
}

fn select_font_blocks(
    bios: &mut Bios,
    cpu: &mut Cpu,
    memory: &mut Memory,
    vga: &mut Vga,
    spec: u8,
) {
    cpu.regs.set_ax(0x1103);
    cpu.regs.set_bl(spec);
    bios.int10(cpu, memory, vga).unwrap();
}

fn reported_font_blocks(
    bios: &mut Bios,
    cpu: &mut Cpu,
    memory: &mut Memory,
    vga: &mut Vga,
) -> (u8, u8) {
    cpu.regs.set_ax(0x1B00);
    cpu.regs.set_bx(0);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0);
    bios.int10(cpu, memory, vga).unwrap();
    (memory.read_u8(0x2002B), memory.read_u8(0x2002C))
}

fn assert_selected_font_pixels(vga: &mut Vga, memory: &mut Memory, columns: [usize; 2]) {
    // Exercise both full-frame and scanline render paths after each selection.
    for scanline in [false, true] {
        if scanline {
            vga.render_scanline(0, memory);
        } else {
            vga.render(memory);
        }
        for (cell, column) in columns.into_iter().enumerate() {
            // Column 2 is blank in either fixture; use the current mode-3 grid.
            let x = cell * (vga.dimensions().0 / 80);
            let pixels = vga.framebuffer();
            let background = pixels[x + 2];
            assert_ne!(
                pixels[x + column],
                background,
                "selected glyph foreground, cell={cell}, column={column}, scanline={scanline}"
            );
            assert_eq!(
                pixels[x + 1 - column],
                background,
                "other glyph column must be blank, cell={cell}, scanline={scanline}"
            );
        }
    }
}

#[test]
fn test_font_selection_bios_switches_rendered_bank() {
    let (mut bios, mut cpu, mut memory, mut vga) = font_selection_fixture();
    for (spec, bank, column) in [(0x05, 1, 1), (0x00, 0, 0), (0x05, 1, 1)] {
        select_font_blocks(&mut bios, &mut cpu, &mut memory, &mut vga, spec);
        // Pixels first: the original detached setter leaves bank 0 displayed.
        assert_selected_font_pixels(&mut vga, &mut memory, [column; 2]);
        assert_eq!(vga.get_sequencer_regs()[3], spec);
        assert_eq!(vga.get_font_block_spec(), spec);
        assert!(!vga.is_512_char_mode());
        assert_eq!(
            reported_font_blocks(&mut bios, &mut cpu, &mut memory, &mut vga),
            (bank, bank)
        );
    }
}

#[test]
fn test_font_selection_bios_dual_fields_and_reserved_bits() {
    let (mut bios, mut cpu, mut memory, mut vga) = font_selection_fixture();
    // Native VGA attribute bit 3 selects A when set and B when clear;
    // BIOS metadata reports primary B followed by secondary A.
    for (spec, metadata, columns) in [
        (0x01, (1, 0), [1, 0]),
        (0x04, (0, 1), [0, 1]),
        (0xF1, (5, 4), [1, 0]),
        (0xF4, (4, 5), [0, 1]),
    ] {
        select_font_blocks(&mut bios, &mut cpu, &mut memory, &mut vga, spec);
        assert_eq!(vga.get_sequencer_regs()[3], spec);
        assert_eq!(vga.get_font_block_spec(), spec);
        assert!(vga.is_512_char_mode());
        assert_eq!(
            reported_font_blocks(&mut bios, &mut cpu, &mut memory, &mut vga),
            metadata
        );
        assert_selected_font_pixels(&mut vga, &mut memory, columns);
    }
}

#[test]
fn test_font_selection_port_state_restore_and_reset_metadata() {
    let (mut bios, mut cpu, mut memory, mut vga) = font_selection_fixture();
    vga.write_port(0x3C4, 3);
    vga.write_port(0x3C5, 0xF5);
    assert_eq!(
        reported_font_blocks(&mut bios, &mut cpu, &mut memory, &mut vga),
        (5, 5)
    );
    assert_eq!(vga.get_font_block_spec(), 0xF5);
    assert_selected_font_pixels(&mut vga, &mut memory, [1, 1]);

    cpu.regs.set_ax(0x1C01);
    cpu.regs.set_cx(1);
    cpu.regs.es = 0x4000;
    cpu.regs.set_bx(0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    select_font_blocks(&mut bios, &mut cpu, &mut memory, &mut vga, 0);
    assert_selected_font_pixels(&mut vga, &mut memory, [0, 0]);
    cpu.regs.set_ax(0x1C02);
    cpu.regs.set_cx(1);
    cpu.regs.es = 0x4000;
    cpu.regs.set_bx(0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(vga.get_sequencer_regs()[3], 0xF5);
    assert_eq!(vga.get_font_block_spec(), 0xF5);
    assert_eq!(
        reported_font_blocks(&mut bios, &mut cpu, &mut memory, &mut vga),
        (5, 5)
    );
    assert_selected_font_pixels(&mut vga, &mut memory, [1, 1]);

    cpu.regs.set_ax(0x0003);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(vga.get_sequencer_regs()[3], 0);
    assert_eq!(vga.get_font_block_spec(), 0);
    assert_eq!(
        reported_font_blocks(&mut bios, &mut cpu, &mut memory, &mut vga),
        (0, 0)
    );
}
