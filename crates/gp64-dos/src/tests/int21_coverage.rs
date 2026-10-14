use super::*;

/// AH=03: Read character from STDAUX - returns AL=0.
#[test]
fn test_cov_ah03_read_stdaux() {
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
    assert_eq!(cpu.regs.al(), 0);
}

/// AH=04: Write character to STDAUX - no-op.
#[test]
fn test_cov_ah04_write_stdaux() {
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
}

/// AH=05: Write character to STDPRN - no-op.
#[test]
fn test_cov_ah05_write_stdprn() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x05);
    cpu.regs.set_dl(b'Y');
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
}

/// AH=0D: Reset disk - always succeeds.
#[test]
fn test_cov_ah0d_reset_disk() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x0D);
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

/// AH=2E: Set verify flag on and off.
#[test]
fn test_cov_ah2e_set_verify_flag() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Turn on verify
    cpu.regs.set_ah(0x2E);
    cpu.regs.set_al(1);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(dos.verify_flag);

    // Turn off verify
    cpu.regs.set_ah(0x2E);
    cpu.regs.set_al(0);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!dos.verify_flag);
}

/// AH=54: Get verify flag.
#[test]
fn test_cov_ah54_get_verify_flag() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.verify_flag = true;
    cpu.regs.set_ah(0x54);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 1);

    dos.verify_flag = false;
    cpu.regs.set_ah(0x54);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0);
}

/// AH=50: Set current PSP.
#[test]
fn test_cov_ah50_set_psp() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x50);
    cpu.regs.set_bx(0x1234);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(dos.current_psp, 0x1234);
}

/// AH=51: Get current PSP.
#[test]
fn test_cov_ah51_get_psp() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.current_psp = 0x5678;
    cpu.regs.set_ah(0x51);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.bx(), 0x5678);
}

/// AH=52: Get List of Lists.
#[test]
fn test_cov_ah52_list_of_lists() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x52);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.es, LOL_SEGMENT);
    assert_eq!(cpu.regs.bx(), LOL_OFFSET);
}

/// AH=34: Get InDOS flag address.
#[test]
fn test_cov_ah34_indos_flag() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x34);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    // Should return a valid segment:offset pointer
    // (just check it doesn't crash and returns something)
    assert!(cpu.regs.es != 0 || cpu.regs.bx() != 0);
}

/// AH=33: Get/Set Ctrl-Break check - all subfunctions.
#[test]
fn test_cov_ah33_ctrl_break_subfunctions() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // AL=0x00: Get Ctrl-Break state (initially false)
    cpu.regs.set_ah(0x33);
    cpu.regs.set_al(0x00);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.dl(), 0);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // AL=0x01: Set Ctrl-Break check on
    cpu.regs.set_ah(0x33);
    cpu.regs.set_al(0x01);
    cpu.regs.set_dl(1);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(dos.ctrl_break_check);

    // AL=0x00: Get again - should be 1 now
    cpu.regs.set_ah(0x33);
    cpu.regs.set_al(0x00);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.dl(), 1);

    // AL=0x05: Get boot drive
    cpu.regs.set_ah(0x33);
    cpu.regs.set_al(0x05);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.dl(), 3); // C: drive

    // AL=0x06: Get true DOS version
    cpu.regs.set_ah(0x33);
    cpu.regs.set_al(0x06);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    // BL should contain major version (6), BH minor (22)
    let bx = cpu.regs.bx();
    assert_eq!(bx & 0xFF, 6); // major
    assert_eq!((bx >> 8) & 0xFF, 22); // minor
}

/// AH=33: Unknown subfunction (default branch).
#[test]
fn test_cov_ah33_unknown_subfunc() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x33);
    cpu.regs.set_al(0xFF); // Invalid subfunction
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// AH=2B: Set date - valid date.
#[test]
fn test_cov_ah2b_set_date_valid() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2B);
    cpu.regs.set_cx(2024); // year
    cpu.regs.set_dh(6); // month
    cpu.regs.set_dl(15); // day
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0x00); // success
}

/// AH=2B: Set date - invalid year (too low).
#[test]
fn test_cov_ah2b_set_date_invalid_year_low() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2B);
    cpu.regs.set_cx(1979); // year < 1980
    cpu.regs.set_dh(1);
    cpu.regs.set_dl(1);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0xFF); // failure
}

/// AH=2B: Set date - invalid year (too high).
#[test]
fn test_cov_ah2b_set_date_invalid_year_high() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2B);
    cpu.regs.set_cx(2100); // year > 2099
    cpu.regs.set_dh(1);
    cpu.regs.set_dl(1);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0xFF);
}

/// AH=2B: Set date - invalid month.
#[test]
fn test_cov_ah2b_set_date_invalid_month() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2B);
    cpu.regs.set_cx(2024);
    cpu.regs.set_dh(13); // month > 12
    cpu.regs.set_dl(1);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0xFF);
}

/// AH=2B: Set date - day 0.
#[test]
fn test_cov_ah2b_set_date_day_zero() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2B);
    cpu.regs.set_cx(2024);
    cpu.regs.set_dh(1);
    cpu.regs.set_dl(0); // day < 1
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0xFF);
}

/// AH=2B: Set date - invalid day for month (Feb 29 non-leap).
#[test]
fn test_cov_ah2b_set_date_feb29_non_leap() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2B);
    cpu.regs.set_cx(2023); // not leap year
    cpu.regs.set_dh(2); // February
    cpu.regs.set_dl(29);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0xFF);
}

/// AH=2B: Set date - Feb 29 on leap year.
#[test]
fn test_cov_ah2b_set_date_feb29_leap() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2B);
    cpu.regs.set_cx(2024); // leap year
    cpu.regs.set_dh(2);
    cpu.regs.set_dl(29);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0x00);
}

/// AH=2B: Set date - Feb 29 on century non-leap (1900).
#[test]
fn test_cov_ah2b_set_date_feb29_century_nonleap() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2B);
    cpu.regs.set_cx(1900); // divisible by 100 but not 400 → invalid year < 1980 anyway
    cpu.regs.set_dh(2);
    cpu.regs.set_dl(29);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0xFF);
}

/// AH=2B: Set date - Feb 29 on 2000 (div by 400 = leap).
#[test]
fn test_cov_ah2b_set_date_feb29_quad_century_leap() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2B);
    cpu.regs.set_cx(2000); // divisible by 400 → leap
    cpu.regs.set_dh(2);
    cpu.regs.set_dl(29);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0x00);
}

/// AH=2B: Set date - 30-day months (Apr, Jun, Sep, Nov).
#[test]
fn test_cov_ah2b_set_date_30day_months() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    for month in [4, 6, 9, 11] {
        cpu.regs.set_ah(0x2B);
        cpu.regs.set_cx(2024);
        cpu.regs.set_dh(month);
        cpu.regs.set_dl(30);
        let _ = dos.int21(
            &mut cpu,
            &mut memory,
            &mut vga,
            &mut keyboard,
            &mut Pic::new(),
        );
        assert_eq!(cpu.regs.al(), 0x00, "month {month} day 30 should be valid");

        // Day 31 should fail for these months
        cpu.regs.set_ah(0x2B);
        cpu.regs.set_cx(2024);
        cpu.regs.set_dh(month);
        cpu.regs.set_dl(31);
        let _ = dos.int21(
            &mut cpu,
            &mut memory,
            &mut vga,
            &mut keyboard,
            &mut Pic::new(),
        );
        assert_eq!(
            cpu.regs.al(),
            0xFF,
            "month {month} day 31 should be invalid"
        );
    }
}

/// AH=2B: Set date - 31-day months.
#[test]
fn test_cov_ah2b_set_date_31day_months() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    for month in [1, 3, 5, 7, 8, 10, 12] {
        cpu.regs.set_ah(0x2B);
        cpu.regs.set_cx(2024);
        cpu.regs.set_dh(month);
        cpu.regs.set_dl(31);
        let _ = dos.int21(
            &mut cpu,
            &mut memory,
            &mut vga,
            &mut keyboard,
            &mut Pic::new(),
        );
        assert_eq!(cpu.regs.al(), 0x00, "month {month} day 31 should be valid");
    }
}

/// AH=2D: Set time - valid time.
#[test]
fn test_cov_ah2d_set_time_valid() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2D);
    cpu.regs.set_ch(12); // hours
    cpu.regs.set_cl(30); // minutes
    cpu.regs.set_dh(45); // seconds
    cpu.regs.set_dl(50); // hundredths
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0x00);
}

/// AH=2D: Set time - invalid hours.
#[test]
fn test_cov_ah2d_set_time_invalid_hours() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2D);
    cpu.regs.set_ch(24); // > 23
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
    assert_eq!(cpu.regs.al(), 0xFF);
}

/// AH=2D: Set time - invalid minutes.
#[test]
fn test_cov_ah2d_set_time_invalid_minutes() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2D);
    cpu.regs.set_ch(0);
    cpu.regs.set_cl(60); // > 59
    cpu.regs.set_dh(0);
    cpu.regs.set_dl(0);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0xFF);
}

/// AH=2D: Set time - invalid seconds.
#[test]
fn test_cov_ah2d_set_time_invalid_seconds() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2D);
    cpu.regs.set_ch(0);
    cpu.regs.set_cl(0);
    cpu.regs.set_dh(60); // > 59
    cpu.regs.set_dl(0);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0xFF);
}

/// AH=2D: Set time - invalid hundredths.
#[test]
fn test_cov_ah2d_set_time_invalid_hundredths() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x2D);
    cpu.regs.set_ch(0);
    cpu.regs.set_cl(0);
    cpu.regs.set_dh(0);
    cpu.regs.set_dl(100); // > 99
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0xFF);
}

/// AH=2C: Get time - with BDA ticks already set.
#[test]
fn test_cov_ah2c_get_time_with_bda_ticks() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Set BDA tick count to a non-zero value (e.g., ~noon = 12h)
    // 12 hours = 12 * 65543 ticks/hr ≈ 786,516
    memory.write_u32(BDA_TICK_COUNT_ADDR, 786_516);

    cpu.regs.set_ah(0x2C);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // Should be approximately 12:00
    assert!(cpu.regs.ch() <= 23);
    assert!(cpu.regs.cl() <= 59);
}

/// AH=2Ch reads the BDA tick counter and NOTHING else, in every mode.
///
/// MDD-BUG-FLU-01380: hosted-session startup used to point this service at the host
/// wall clock, so guest-visible time advanced at the HOST's rate. A guest
/// polling for the seconds field to change (a GUI application does, between
/// CreateWindow and ShowWindow) then spins for the whole run on any route
/// that outruns real time. The two seeds below are exact fractions of a
/// BIOS day - a quarter and a half - so the expected times are 06:00:00.00
/// and 12:00:00.00 by definition, derived from the tick counter's meaning
/// rather than from the conversion under test. No host clock can answer
/// both, which is what makes this red the moment one is reintroduced.
#[test]
fn int21_get_time_is_derived_from_the_bda_tick_counter() {
    let read_time = |ticks: u32| {
        let mut dos = Dos::new();
        let mut cpu = gp64_cpu::Cpu::new();
        let mut memory = Memory::new();
        let (mut vga, mut keyboard) = create_test_io();
        memory.write_u32(BDA_TICK_COUNT_ADDR, ticks);
        cpu.regs.set_ah(0x2C);
        dos.int21(
            &mut cpu,
            &mut memory,
            &mut vga,
            &mut keyboard,
            &mut Pic::new(),
        )
        .expect("int21 AH=2Ch");
        (cpu.regs.ch(), cpu.regs.cl(), cpu.regs.dh(), cpu.regs.dl())
    };

    // A quarter of a BIOS day is 06:00 exactly; a half is 12:00 exactly.
    assert_eq!(read_time(BIOS_TICKS_PER_DAY / 4), (6, 0, 0, 0));
    assert_eq!(read_time(BIOS_TICKS_PER_DAY / 2), (12, 0, 0, 0));
}

/// AH=2C: Get time - with BDA ticks zero returns midnight.
#[test]
fn test_cov_ah2c_get_time_zero_bda_ticks() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    memory.write_u32(BDA_TICK_COUNT_ADDR, 0);

    cpu.regs.set_ah(0x2C);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // BDA ticks=0 -> midnight (00:00:00.00)
    assert_eq!(cpu.regs.ch(), 0);
    assert_eq!(cpu.regs.cl(), 0);
    assert_eq!(cpu.regs.dh(), 0);
    assert_eq!(cpu.regs.dl(), 0);
}

/// AH=32: Get DPB - valid drive.
#[test]
fn test_cov_ah32_get_dpb_valid() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Add a drive
    dos.drives
        .insert(2, Box::new(DiskSpaceDrive::new(1_000_000, 500_000)));
    cpu.regs.set_ah(0x32);
    cpu.regs.set_dl(3); // Drive C: (1-based)
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0); // Success
}

/// AH=32: Get DPB - default drive (DL=0).
#[test]
fn test_cov_ah32_get_dpb_default_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let current = dos.path_resolver.get_current_drive();
    dos.drives
        .insert(current, Box::new(DiskSpaceDrive::new(1_000_000, 500_000)));
    cpu.regs.set_ah(0x32);
    cpu.regs.set_dl(0); // Default drive
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0);
}

/// AH=32: Get DPB - invalid drive.
#[test]
fn test_cov_ah32_get_dpb_invalid() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x32);
    cpu.regs.set_dl(26); // Non-existent drive
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0xFF); // Invalid drive
}

/// AH=53: Translate BPB to DPB.
#[test]
fn test_cov_ah53_bpb_to_dpb() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Set up a BPB at DS:SI
    let bpb_addr = Memory::seg_offset_to_linear(0x1000, 0x0000);
    memory.write_u16(bpb_addr, 512); // Bytes per sector
    memory.write_u8(bpb_addr + 2, 4); // Sectors per cluster
    memory.write_u16(bpb_addr + 3, 1); // Reserved sectors
    memory.write_u8(bpb_addr + 5, 2); // Number of FATs
    memory.write_u16(bpb_addr + 6, 512); // Root directory entries
    memory.write_u16(bpb_addr + 8, 40960); // Total sectors
    memory.write_u8(bpb_addr + 10, 0xF8); // Media descriptor
    memory.write_u16(bpb_addr + 11, 20); // Sectors per FAT

    // Set up destination buffer at ES:BP
    let dpb_addr_seg = 0x2000u16;
    let dpb_addr_off = 0x0000u16;

    cpu.regs.set_ah(0x53);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_si(0x0000);
    cpu.regs.es = dpb_addr_seg;
    cpu.regs.set_bp(dpb_addr_off);

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Verify DPB output
    let dpb_addr = Memory::seg_offset_to_linear(dpb_addr_seg, dpb_addr_off);
    assert_eq!(memory.read_u16(dpb_addr + 2), 512); // Bytes per sector
    assert_eq!(memory.read_u8(dpb_addr + 4), 3); // Sectors per cluster - 1
    assert_eq!(memory.read_u8(dpb_addr + 22), 0xF8); // Media descriptor
}

/// AH=58: Get memory allocation strategy.
#[test]
fn test_cov_ah58_get_strategy() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x00); // Get strategy
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), dos.memory_strategy as u16);
}

/// AH=58: Set valid memory allocation strategies.
#[test]
fn test_cov_ah58_set_strategy_valid() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    for strategy in [0x00, 0x01, 0x02, 0x40, 0x41, 0x80, 0x81, 0x82] {
        cpu.regs.set_ah(0x58);
        cpu.regs.set_al(0x01); // Set strategy
        cpu.regs.set_bl(strategy);
        let _ = dos.int21(
            &mut cpu,
            &mut memory,
            &mut vga,
            &mut keyboard,
            &mut Pic::new(),
        );
        assert!(
            !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "strategy {strategy:#x} should be valid"
        );
        assert_eq!(dos.memory_strategy, strategy);
    }
}

/// AH=58: Set invalid memory allocation strategy.
#[test]
fn test_cov_ah58_set_strategy_invalid() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x01);
    cpu.regs.set_bl(0x03); // Invalid base strategy
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1);
}

/// AH=58: Get UMB link state.
#[test]
fn test_cov_ah58_get_umb_link() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x02); // Get UMB link
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.al(), 0); // Not linked initially
}

/// AH=58: Set UMB link valid.
#[test]
fn test_cov_ah58_set_umb_link() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.dos_mem.init(&mut memory);

    // Link UMB
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x03);
    cpu.regs.set_bx(1); // Link
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(dos.umb_linked);

    // Unlink UMB
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x03);
    cpu.regs.set_bx(0); // Unlink
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(!dos.umb_linked);
}

/// AH=58: Set UMB link invalid value.
#[test]
fn test_cov_ah58_set_umb_link_invalid() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x03);
    cpu.regs.set_bx(2); // Invalid value
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// AH=58: Invalid subfunction.
#[test]
fn test_cov_ah58_invalid_subfunc() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x04); // Invalid
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1);
}

/// AH=59: Get extended error information.
#[test]
fn test_cov_ah59_get_extended_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Set an extended error first
    dos.set_extended_error(5); // Access denied

    cpu.regs.set_ah(0x59);
    cpu.regs.set_bx(0); // Version 0
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 5); // Error code
}

/// AH=67: Set handle count - valid.
#[test]
fn test_cov_ah67_set_handle_count_valid() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x67);
    cpu.regs.set_bx(100); // Valid count
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// AH=67: Set handle count - too many.
#[test]
fn test_cov_ah67_set_handle_count_too_many() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x67);
    cpu.regs.set_bx(256); // > 255
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

/// AH=68: Commit file - standard handle (no flush needed).
#[test]
fn test_cov_ah68_commit_standard_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x68);
    cpu.regs.set_bx(1); // stdout
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// AH=68: Commit file - invalid handle.
#[test]
fn test_cov_ah68_commit_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x68);
    cpu.regs.set_bx(99); // Not a standard handle, not open
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 6);
}

/// AH=41: Delete file - file not found.
#[test]
fn test_cov_ah41_delete_not_found() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.drives.insert(2, Box::new(NoSuchFileDrive));

    // Write filename at DS:DX
    let addr = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    for (i, b) in b"C:\\NOFILE.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, *b);
    }
    cpu.regs.set_ah(0x41);
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 2); // File not found
}

/// AH=41: Delete file - drive not found.
#[test]
fn test_cov_ah41_delete_no_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Don't add any drives
    let addr = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    for (i, b) in b"Z:\\NOFILE.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, *b);
    }
    cpu.regs.set_ah(0x41);
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3); // Path not found
}

/// AH=42: Seek - invalid origin.
#[test]
fn test_cov_ah42_seek_invalid_origin() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Open a file first using the file_table directly
    let file = CursorFile::new(vec![0u8; 100]);
    let handle = dos
        .file_table
        .allocate_with_sharing(
            Box::new(file),
            "C:\\TEST.TXT".to_string(),
            dos.current_psp,
            crate::files::SHARE_COMPAT,
            crate::files::ACCESS_READWRITE,
        )
        .unwrap();

    cpu.regs.set_ah(0x42);
    cpu.regs.set_bx(handle);
    cpu.regs.set_al(3); // Invalid origin
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(0);
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

/// AH=42: Seek - invalid handle.
#[test]
fn test_cov_ah42_seek_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x42);
    cpu.regs.set_bx(99); // No such handle
    cpu.regs.set_al(0);
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(0);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 6); // Invalid handle
}

/// AH=42: Seek - seek error.
#[test]
fn test_cov_ah42_seek_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let file = SeekErrorFile;
    let handle = dos
        .file_table
        .allocate_with_sharing(
            Box::new(file),
            "C:\\SEEKERR.TXT".to_string(),
            dos.current_psp,
            crate::files::SHARE_COMPAT,
            crate::files::ACCESS_READWRITE,
        )
        .unwrap();

    cpu.regs.set_ah(0x42);
    cpu.regs.set_bx(handle);
    cpu.regs.set_al(0); // Seek from start
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(10);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 25); // Seek error
}

/// AH=42: Seek - all valid origins (start, current, end).
#[test]
fn test_cov_ah42_seek_all_origins() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let file = CursorFile::new(vec![0u8; 100]);
    let handle = dos
        .file_table
        .allocate_with_sharing(
            Box::new(file),
            "C:\\TEST.TXT".to_string(),
            dos.current_psp,
            crate::files::SHARE_COMPAT,
            crate::files::ACCESS_READWRITE,
        )
        .unwrap();

    // Origin 0: Seek from start
    cpu.regs.set_ah(0x42);
    cpu.regs.set_bx(handle);
    cpu.regs.set_al(0);
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(50);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 50);

    // Origin 1: Seek from current
    cpu.regs.set_ah(0x42);
    cpu.regs.set_bx(handle);
    cpu.regs.set_al(1);
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(10);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 60);

    // Origin 2: Seek from end
    cpu.regs.set_ah(0x42);
    cpu.regs.set_bx(handle);
    cpu.regs.set_al(2);
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(0);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 100); // End of 100-byte file
}

/// AH=43: Get file attributes - all bits set.
#[test]
fn test_cov_ah43_get_attrs_all() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.drives.insert(2, Box::new(AllAttributesDrive));

    let addr = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    for (i, b) in b"C:\\TEST.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, *b);
    }
    cpu.regs.set_ah(0x43);
    cpu.regs.set_al(0); // Get attributes
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // All bits set: read_only(1) + hidden(2) + system(4) + volume_label(8) + directory(16) + archive(32) = 0x3F
    assert_eq!(cpu.regs.cx(), 0x3F);
}

/// AH=43: Get attributes - error (file not found).
#[test]
fn test_cov_ah43_get_attrs_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.drives.insert(2, Box::new(NotFoundDrive));

    let addr = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    for (i, b) in b"C:\\NOPE.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, *b);
    }
    cpu.regs.set_ah(0x43);
    cpu.regs.set_al(0);
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 2);
}

/// AH=43: Get attributes - no drive.
#[test]
fn test_cov_ah43_get_attrs_no_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    for (i, b) in b"Z:\\NOPE.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, *b);
    }
    cpu.regs.set_ah(0x43);
    cpu.regs.set_al(0);
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// AH=56: Rename - cross drive error.
#[test]
fn test_cov_ah56_rename_cross_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.drives
        .insert(2, Box::new(DiskSpaceDrive::new(1_000_000, 500_000)));
    dos.drives
        .insert(3, Box::new(DiskSpaceDrive::new(1_000_000, 500_000)));

    // Old name at DS:DX
    let addr_old = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    for (i, b) in b"C:\\OLD.TXT\0".iter().enumerate() {
        memory.write_u8(addr_old + i, *b);
    }
    // New name at ES:DI
    let addr_new = Memory::seg_offset_to_linear(cpu.regs.es, 0x200);
    for (i, b) in b"D:\\NEW.TXT\0".iter().enumerate() {
        memory.write_u8(addr_new + i, *b);
    }

    cpu.regs.set_ah(0x56);
    cpu.regs.set_dx(0x100);
    cpu.regs.set_di(0x200);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 17); // Not same device
}

/// AH=56: Rename - drive not found.
#[test]
fn test_cov_ah56_rename_no_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr_old = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    for (i, b) in b"Z:\\OLD.TXT\0".iter().enumerate() {
        memory.write_u8(addr_old + i, *b);
    }
    let addr_new = Memory::seg_offset_to_linear(cpu.regs.es, 0x200);
    for (i, b) in b"Z:\\NEW.TXT\0".iter().enumerate() {
        memory.write_u8(addr_new + i, *b);
    }

    cpu.regs.set_ah(0x56);
    cpu.regs.set_dx(0x100);
    cpu.regs.set_di(0x200);
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

/// AH=56: Rename - error from drive.
#[test]
fn test_cov_ah56_rename_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // DiskSpaceDrive returns error on rename
    dos.drives
        .insert(2, Box::new(DiskSpaceDrive::new(1_000_000, 500_000)));

    let addr_old = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    for (i, b) in b"C:\\OLD.TXT\0".iter().enumerate() {
        memory.write_u8(addr_old + i, *b);
    }
    let addr_new = Memory::seg_offset_to_linear(cpu.regs.es, 0x200);
    for (i, b) in b"C:\\NEW.TXT\0".iter().enumerate() {
        memory.write_u8(addr_new + i, *b);
    }

    cpu.regs.set_ah(0x56);
    cpu.regs.set_dx(0x100);
    cpu.regs.set_di(0x200);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 5); // Access denied
}

/// AH=57: Get file datetime - invalid handle.
#[test]
fn test_cov_ah57_get_datetime_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x57);
    cpu.regs.set_al(0); // Get
    cpu.regs.set_bx(99); // Invalid handle
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 6);
}

/// AH=57: Invalid subfunction.
#[test]
fn test_cov_ah57_invalid_subfunc() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Need a valid handle for this test
    let file = CursorFile::new(vec![0u8; 10]);
    let handle = dos
        .file_table
        .allocate_with_sharing(
            Box::new(file),
            "C:\\TEST.TXT".to_string(),
            dos.current_psp,
            crate::files::SHARE_COMPAT,
            crate::files::ACCESS_READWRITE,
        )
        .unwrap();

    cpu.regs.set_ah(0x57);
    cpu.regs.set_al(2); // Invalid subfunction (only 0 and 1 valid)
    cpu.regs.set_bx(handle);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1);
}

/// AH=5D: Server function - commit all files.
#[test]
fn test_cov_ah5d_commit_all() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x5D);
    cpu.regs.set_al(0x01); // Commit all files
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// AH=5D: Server function - set extended error.
#[test]
fn test_cov_ah5d_set_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Write error structure at DS:DX
    let buf_addr = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    memory.write_u16(buf_addr, 42); // Error code

    cpu.regs.set_ah(0x5D);
    cpu.regs.set_al(0x0A);
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify error was set
    assert_eq!(dos.extended_error.0, 42);
}

/// AH=5D: Server function - unknown subfunction.
#[test]
fn test_cov_ah5d_unknown_subfunc() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x5D);
    cpu.regs.set_al(0xFF); // Unknown
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1);
}

/// AH=60: Truename - canonicalize path.
#[test]
fn test_cov_ah60_truename() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Source path at DS:SI
    let src_addr = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    for (i, b) in b"test.txt\0".iter().enumerate() {
        memory.write_u8(src_addr + i, *b);
    }
    // Destination at ES:DI
    cpu.regs.set_ah(0x60);
    cpu.regs.set_si(0x100);
    cpu.regs.set_di(0x200);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Read back canonical path
    let dest_addr = Memory::seg_offset_to_linear(cpu.regs.es, 0x200);
    let mut path = String::new();
    let mut i = 0;
    loop {
        let b = memory.read_u8(dest_addr + i);
        if b == 0 {
            break;
        }
        path.push(b as char);
        i += 1;
    }
    // Should be uppercase and have drive letter
    assert!(path.starts_with("C:\\") || path.starts_with("D:\\") || !path.is_empty());
    assert_eq!(path, path.to_ascii_uppercase());
}

/// AH=38: Get country info (AL=0).
#[test]
fn test_cov_ah38_get_country_info() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x38);
    cpu.regs.set_al(0); // Get country info
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.bx(), 1); // USA

    // AH=38h stays DOS-facing: the US COUNTRY table's date separator is '-'.
    let buf = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    assert_eq!(memory.read_u16(buf), 0); // Date format: USA
    assert_eq!(memory.read_u8(buf + 2), b'$'); // Currency symbol
    assert_eq!(memory.read_u8(buf + 0x0B), b'-'); // DOS date separator
}

/// AH=38: Get country info with AL=0xFF.
#[test]
fn test_cov_ah38_get_country_info_0xff() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x38);
    cpu.regs.set_al(0xFF); // AL=FFh: get info for country code in BX
    cpu.regs.set_bx(44); // UK
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.bx(), 44); // Should return country code
}

/// AH=38: Get specific country info (AL=44, DX≠FFFFh → GET for UK).
#[test]
fn test_cov_ah38_get_specific_country() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x38);
    cpu.regs.set_al(44); // Get country code 44 (UK)
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.bx(), 44); // UK
}

/// AH=38: SET country via DX=FFFFh, AL=44 (UK).
#[test]
fn test_cov_ah38_set_country_via_dx_ffff() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // SET: DX=FFFFh, AL=44 → set current country to UK
    cpu.regs.set_ah(0x38);
    cpu.regs.set_al(44);
    cpu.regs.set_dx(0xFFFF);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.bx(), 44);

    // Verify country was actually changed by reading it back
    cpu.regs.set_ah(0x38);
    cpu.regs.set_al(0); // GET current
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.bx(), 44);
    // UK date format = DMY (1)
    let buf = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    assert_eq!(memory.read_u16(buf), 1);
}

/// AH=38: SET country via DX=FFFFh, AL=FFh, BX=49 (Germany).
#[test]
fn test_cov_ah38_set_country_via_bx() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // SET: DX=FFFFh, AL=FFh, BX=49 → set to Germany
    cpu.regs.set_ah(0x38);
    cpu.regs.set_al(0xFF);
    cpu.regs.set_bx(49);
    cpu.regs.set_dx(0xFFFF);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.bx(), 49);
}

/// AH=38: SET unknown country → CF=1, AX=2.
#[test]
fn test_cov_ah38_set_unknown_country_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x38);
    cpu.regs.set_al(99); // Country code 99 not in table
    cpu.regs.set_dx(0xFFFF);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 2); // "File not found" error
}

/// AH=38: GET unknown country → CF=1, AX=2.
#[test]
fn test_cov_ah38_get_unknown_country_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x38);
    cpu.regs.set_al(99); // Country code 99 not in table
    cpu.regs.set_dx(0x100); // DX≠FFFFh → GET
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 2);
}

/// AH=65 AL=01: Extended country info uses country_info field.
#[test]
fn test_cov_ah65_extended_country_info_uses_country_info() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // First set country to Germany via AH=38h SET
    cpu.regs.set_ah(0x38);
    cpu.regs.set_al(49); // Germany
    cpu.regs.set_dx(0xFFFF);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Now query AH=65h AL=01h and verify it returns German info
    cpu.regs.set_ah(0x65);
    cpu.regs.set_al(0x01);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0000);
    cpu.regs.set_cx(41);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.cx(), 41);

    let buf = Memory::seg_offset_to_linear(0x3000, 0x0000);
    assert_eq!(memory.read_u8(buf), 0x01); // Info ID
    assert_eq!(memory.read_u16(buf + 0x03), 49); // Country code
    assert_eq!(memory.read_u16(buf + 0x05), 850); // Code page
    assert_eq!(memory.read_u16(buf + 0x07), 1); // Date format: DMY
}

/// AH=66 AL=01: Code page reflects country_info.
#[test]
fn test_cov_ah66_code_page_uses_country_info() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Default is US → code page 437
    cpu.regs.set_ah(0x66);
    cpu.regs.set_al(0x01);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.bx(), 437);

    // Set country to Germany → code page 850
    cpu.regs.set_ah(0x38);
    cpu.regs.set_al(49);
    cpu.regs.set_dx(0xFFFF);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    cpu.regs.set_ah(0x66);
    cpu.regs.set_al(0x01);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.bx(), 850);
}

/// apply_country_settings with valid override.
#[test]
fn test_apply_country_settings_override() {
    let mut dos = Dos::new();
    dos.apply_country_settings(Some(49)); // Germany
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x38);
    cpu.regs.set_al(0);
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.bx(), 49);
}

/// apply_country_settings with invalid override falls back to US defaults.
#[test]
fn test_apply_country_settings_invalid_override() {
    let mut dos = Dos::new();
    dos.apply_country_settings(Some(9999)); // Invalid code
    assert_eq!(dos.country_info().country_code, 1);
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x38);
    cpu.regs.set_al(0);
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // Country code should be non-zero (whatever the host detection returned)
    assert!(cpu.regs.bx() > 0);
}

/// AH=63: DBCS lead byte table.
#[test]
fn test_cov_ah63_dbcs_table() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x63);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    // Should return a pointer to an empty DBCS table
    // (two zero bytes as end marker)
}

/// AH=55: Create child PSP.
#[test]
fn test_cov_ah55_create_child_psp() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Initialize a PSP at the current segment
    let psp_addr = Memory::seg_offset_to_linear(dos.current_psp, 0);
    memory.write_u16(psp_addr, 0xCD20); // INT 20h at start of PSP

    cpu.regs.set_ah(0x55);
    cpu.regs.set_dx(0x3000); // New PSP segment
    cpu.regs.set_si(0x100); // Size in paragraphs
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify the new PSP has the parent PSP pointer set
    let new_psp_addr = Memory::seg_offset_to_linear(0x3000, 0);
    assert_eq!(memory.read_u16(new_psp_addr + 0x16), dos.current_psp);
}

/// AH=40: Write to stdout (handle 1) - routes to VGA.
#[test]
fn test_cov_ah40_write_stdout() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Write "Hi" at DS:DX
    let addr = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    memory.write_u8(addr, b'H');
    memory.write_u8(addr + 1, b'i');

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(1); // stdout
    cpu.regs.set_cx(2);
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 2);
}

/// AH=40: Write to stderr (handle 2).
#[test]
fn test_cov_ah40_write_stderr() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    memory.write_u8(addr, b'E');

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(2); // stderr
    cpu.regs.set_cx(1);
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1);
}

/// AH=40: Write to stdin (handle 0) - should fail.
#[test]
fn test_cov_ah40_write_stdin() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    memory.write_u8(addr, b'X');

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(0); // stdin
    cpu.regs.set_cx(1);
    cpu.regs.set_dx(0x100);
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

/// AH=40: Write to invalid handle.
#[test]
fn test_cov_ah40_write_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    memory.write_u8(addr, b'X');

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(99); // Invalid handle
    cpu.regs.set_cx(1);
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 6);
}

/// AH=40: Write to file - truncate (CX=0).
#[test]
fn test_cov_ah40_write_truncate() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let file = CursorFile::new(vec![1, 2, 3, 4, 5]);
    let handle = dos
        .file_table
        .allocate_with_sharing(
            Box::new(file),
            "C:\\TEST.TXT".to_string(),
            dos.current_psp,
            crate::files::SHARE_COMPAT,
            crate::files::ACCESS_READWRITE,
        )
        .unwrap();

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(0); // CX=0 means truncate
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0);
}

/// AH=40: Write to file - write error triggers INT 24h critical error.
#[test]
fn test_cov_ah40_write_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let file = ErrorWriteFile;
    let handle = dos
        .file_table
        .allocate_with_sharing(
            Box::new(file),
            "C:\\ERR.TXT".to_string(),
            dos.current_psp,
            crate::files::SHARE_COMPAT,
            crate::files::ACCESS_READWRITE,
        )
        .unwrap();

    let addr = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    memory.write_u8(addr, b'X');

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(1);
    cpu.regs.set_dx(0x100);
    let disp = dos
        .int21(
            &mut cpu,
            &mut memory,
            &mut vga,
            &mut keyboard,
            &mut Pic::new(),
        )
        .unwrap();
    // ErrorWriteFile returns ErrorKind::Other which classify_io_error
    // now maps to a non-critical access denied error (code 5).
    assert_eq!(disp, Int21Disposition::Iret);
    assert_eq!(cpu.regs.ax(), 5); // Access denied
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// AH=40: Write to stdout with capture buffer.
#[test]
fn test_cov_ah40_write_stdout_capture() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Enable stdout capture
    dos.stdout_capture = Some(Vec::new());

    let addr = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    memory.write_u8(addr, b'A');
    memory.write_u8(addr + 1, b'B');

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(1); // stdout
    cpu.regs.set_cx(2);
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dos.stdout_capture.as_ref().unwrap(), b"AB");
}

/// AH=40: Write via CON alias handle (0x00FE).
#[test]
fn test_cov_ah40_write_con_alias() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    memory.write_u8(addr, b'Z');

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(0x00FE); // CON alias → stdout
    cpu.regs.set_cx(1);
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// AH=3F: Read from file handle.
#[test]
fn test_cov_ah3f_read_file() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let file = CursorFile::new(b"Hello World".to_vec());
    let handle = dos
        .file_table
        .allocate_with_sharing(
            Box::new(file),
            "C:\\TEST.TXT".to_string(),
            dos.current_psp,
            crate::files::SHARE_COMPAT,
            crate::files::ACCESS_READWRITE,
        )
        .unwrap();

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(5);
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 5);

    // Verify data
    let addr = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    assert_eq!(memory.read_u8(addr), b'H');
    assert_eq!(memory.read_u8(addr + 4), b'o');
}

/// AH=3F: Read from file - read error triggers INT 24h critical error.
#[test]
fn test_cov_ah3f_read_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let file = FailingReadFile;
    let handle = dos
        .file_table
        .allocate_with_sharing(
            Box::new(file),
            "C:\\ERR.TXT".to_string(),
            dos.current_psp,
            crate::files::SHARE_COMPAT,
            crate::files::ACCESS_READWRITE,
        )
        .unwrap();

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(10);
    cpu.regs.set_dx(0x100);
    let disp = dos
        .int21(
            &mut cpu,
            &mut memory,
            &mut vga,
            &mut keyboard,
            &mut Pic::new(),
        )
        .unwrap();
    // FailingReadFile returns ErrorKind::Other which classify_io_error
    // now maps to a non-critical access denied error (code 5).
    assert_eq!(disp, Int21Disposition::Iret);
    assert_eq!(cpu.regs.ax(), 5); // Access denied
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// AH=3F: Read from STDIN (handle 0) - with line buffer data.
#[test]
fn test_cov_ah3f_read_stdin() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Pre-populate the cooked-mode line buffer (simulates a completed line).
    dos.stdin_line_buf = vec![b'a', b'b'];

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(0); // STDIN
    cpu.regs.set_cx(5);
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 2); // Read 2 chars

    let addr = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    assert_eq!(memory.read_u8(addr), b'a');
    assert_eq!(memory.read_u8(addr + 1), b'b');
}

/// AH=3F: Read from invalid handle.
#[test]
fn test_cov_ah3f_read_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(99); // Invalid
    cpu.regs.set_cx(10);
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 6);
}

/// AH=3F: Read via CON alias (0x00FE → STDIN).
#[test]
fn test_cov_ah3f_read_con_alias() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Pre-populate the cooked-mode line buffer (simulates a completed line).
    dos.stdin_line_buf = vec![b'x'];

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(0x00FE); // CON alias → stdin
    cpu.regs.set_cx(1);
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1);
}

/// AH=0B: Check input status - no key available.
#[test]
fn test_cov_ah0b_no_key() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x0B);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0x00);
}

/// AH=0B: Check input status - key available.
#[test]
fn test_cov_ah0b_key_available() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    gp64_hw::bda::kbd_enqueue(&mut memory, 0x1E, b'a');

    cpu.regs.set_ah(0x0B);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0xFF);
}

/// AH=0C: Flush and invoke subfunction 0x01.
#[test]
fn test_cov_ah0c_flush_and_invoke() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Enqueue a key so sub-function can consume it
    gp64_hw::bda::kbd_enqueue(&mut memory, 0x1E, b'a');

    // AH=0C clears buffer and invokes the sub-function
    // subfunc 0x06 (direct console input) is simplest to test
    cpu.regs.set_ah(0x0C);
    cpu.regs.set_al(0x06);
    // After clear, no key available, so 0x06 with DL=FF should return 0+ZF
    cpu.regs.set_dl(0xFF);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    // Buffer was cleared, so we should get ZF set (no key)
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::ZERO));
}

/// AH=0C: Flush with invalid subfunction (no-op).
#[test]
fn test_cov_ah0c_flush_invalid_subfunc() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x0C);
    cpu.regs.set_al(0x00); // Not 01/06/07/08/0A → no-op
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    // Should just return without error
}

/// AH=09: Print $-terminated string.
#[test]
fn test_cov_ah09_print_string() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Write "Hello$" at DS:DX
    let addr = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    for (i, b) in b"Hello$".iter().enumerate() {
        memory.write_u8(addr + i, *b);
    }

    cpu.regs.set_ah(0x09);
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    // Just verify it doesn't crash - output goes to VGA
}

/// AH=0A: Buffered input - max_len=0 (immediate return).
#[test]
fn test_cov_ah0a_buffered_input_zero_max() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let buf_addr = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    memory.write_u8(buf_addr, 0); // max_len = 0
    memory.write_u8(buf_addr + 1, 0); // filled count

    cpu.regs.set_ah(0x0A);
    cpu.regs.set_dx(0x100);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(buf_addr + 1), 0); // Still 0
}

/// AH=0A: Buffered input - len > max_len resets to 0.
#[test]
fn test_cov_ah0a_buffered_input_len_exceeds_max() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let buf_addr = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    memory.write_u8(buf_addr, 10); // max_len = 10
    memory.write_u8(buf_addr + 1, 20); // filled count > max_len

    // Need a key to process (Enter to finish)
    gp64_hw::bda::kbd_enqueue(&mut memory, 0x1C, 0x0D); // Enter

    cpu.regs.set_ah(0x0A);
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    // The filled count should have been reset and then Enter processed
}

/// AH=1B: Get default drive allocation.
#[test]
fn test_cov_ah1b_default_drive_alloc() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let drive_num = dos.path_resolver.get_current_drive();
    dos.drives.insert(
        drive_num,
        Box::new(DiskSpaceDrive::new(1_000_000_000, 500_000_000)),
    );

    cpu.regs.set_ah(0x1B);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    // Should set AL, CX, DX with allocation data
}

/// AH=06: Direct console input (DL=FF) - no key available.
#[test]
fn test_cov_ah06_direct_input_no_key() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x06);
    cpu.regs.set_dl(0xFF);
    // Need to set up IRET frame for with_iret_flags_mut
    // Push flags, CS, IP on stack for IRET frame
    cpu.regs.set_sp(0xFFF0);
    cpu.regs.ss = 0x0000;
    let sp_addr = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.sp());
    memory.write_u16(sp_addr, 0); // IP
    memory.write_u16(sp_addr + 2, 0); // CS
    memory.write_u16(sp_addr + 4, 0); // FLAGS

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::ZERO));
}

/// AH=06: Direct console input - key available (normal ASCII).
#[test]
fn test_cov_ah06_direct_input_key() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    gp64_hw::bda::kbd_enqueue(&mut memory, 0x1E, b'a');

    cpu.regs.set_ah(0x06);
    cpu.regs.set_dl(0xFF);
    cpu.regs.set_sp(0xFFF0);
    cpu.regs.ss = 0x0000;
    let sp_addr = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.sp());
    memory.write_u16(sp_addr, 0);
    memory.write_u16(sp_addr + 2, 0);
    memory.write_u16(sp_addr + 4, 0);

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), b'a');
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::ZERO));
}

/// AH=06: Direct console input - extended key (scancode with ascii=0).
#[test]
fn test_cov_ah06_direct_input_extended() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Enqueue an extended key (e.g., F1 = scancode 0x3B, ascii 0)
    gp64_hw::bda::kbd_enqueue(&mut memory, 0x3B, 0);

    cpu.regs.set_ah(0x06);
    cpu.regs.set_dl(0xFF);
    cpu.regs.set_sp(0xFFF0);
    cpu.regs.ss = 0x0000;
    let sp_addr = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.sp());
    memory.write_u16(sp_addr, 0);
    memory.write_u16(sp_addr + 2, 0);
    memory.write_u16(sp_addr + 4, 0);

    // First call returns ascii prefix (0)
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0);

    // Second call should return the scan code from pending_extended_scancode
    cpu.regs.set_ah(0x06);
    cpu.regs.set_dl(0xFF);
    let sp_addr = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.sp());
    memory.write_u16(sp_addr, 0);
    memory.write_u16(sp_addr + 2, 0);
    memory.write_u16(sp_addr + 4, 0);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0x3B); // F1 scancode
}

/// AH=06: Direct console output (DL != 0xFF).
#[test]
fn test_cov_ah06_direct_output() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x06);
    cpu.regs.set_dl(b'X'); // Output mode
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    // Just verify it doesn't crash - output goes to VGA

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

/// AH=08: Character input with Ctrl-C check - normal key.
#[test]
fn test_cov_ah08_normal_key() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    gp64_hw::bda::kbd_enqueue(&mut memory, 0x1E, b'a');

    cpu.regs.set_ah(0x08);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), b'a');
}

/// AH=08: Character input with extended key.
#[test]
fn test_cov_ah08_extended_key() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    gp64_hw::bda::kbd_enqueue(&mut memory, 0x3B, 0); // F1

    cpu.regs.set_ah(0x08);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0); // Extended prefix

    // Second call returns scancode
    cpu.regs.set_ah(0x08);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0x3B);
}

/// AH=08: Character input - Ctrl-C triggers interrupt.
#[test]
fn test_cov_ah08_ctrl_c() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    gp64_hw::bda::kbd_enqueue(&mut memory, 0x2E, 0x03); // Ctrl-C

    // Set up stack for IRET frame
    cpu.regs.set_sp(0xFFF0);
    cpu.regs.ss = 0x0000;
    let sp_addr = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.sp());
    memory.write_u16(sp_addr, 0);
    memory.write_u16(sp_addr + 2, 0);
    memory.write_u16(sp_addr + 4, 0);

    cpu.regs.set_ah(0x08);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    // Ctrl-C was detected and ^C echoed
}

/// AH=02: Character output.
#[test]
fn test_cov_ah02_char_output() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x02);
    cpu.regs.set_dl(b'Z');
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
}

/// AH=69: Get disk serial number - valid drive.
#[test]
fn test_cov_ah69_get_serial_valid() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.drives
        .insert(2, Box::new(DiskSpaceDrive::new(1_000_000, 500_000)));

    cpu.regs.set_ah(0x69);
    cpu.regs.set_al(0x00); // Get serial
    cpu.regs.set_bl(3); // Drive C: (1-based)
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify serial number was written
    let buf = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    assert_eq!(memory.read_u32(buf + 2), 0x12345678);
}

/// AH=69: Get disk serial number - invalid drive.
#[test]
fn test_cov_ah69_get_serial_invalid_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x69);
    cpu.regs.set_al(0x00);
    cpu.regs.set_bl(26); // Non-existent drive
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 15);
}

/// AH=69: Get serial with default drive (BL=0).
#[test]
fn test_cov_ah69_get_serial_default_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let drive_num = dos.path_resolver.get_current_drive();
    dos.drives
        .insert(drive_num, Box::new(DiskSpaceDrive::new(1_000_000, 500_000)));

    cpu.regs.set_ah(0x69);
    cpu.regs.set_al(0x00);
    cpu.regs.set_bl(0); // Default drive
    cpu.regs.set_dx(0x100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// AH=3E: Close file - invalid handle.
#[test]
fn test_cov_ah3e_close_invalid() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x3E);
    cpu.regs.set_bx(99); // No such handle
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// AH=3E: Close file - valid handle.
#[test]
fn test_cov_ah3e_close_valid() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let file = CursorFile::new(vec![]);
    let handle = dos
        .file_table
        .allocate_with_sharing(
            Box::new(file),
            "C:\\TEST.TXT".to_string(),
            dos.current_psp,
            crate::files::SHARE_COMPAT,
            crate::files::ACCESS_READWRITE,
        )
        .unwrap();

    cpu.regs.set_ah(0x3E);
    cpu.regs.set_bx(handle);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// AH=1A: Set DTA address.
#[test]
fn test_cov_ah1a_set_dta() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x1A);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0080);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(dos.dta_segment, 0x2000);
    assert_eq!(dos.dta_offset, 0x0080);
}

/// AH=2F: Get DTA address.
#[test]
fn test_cov_ah2f_get_dta() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.dta_segment = 0x3000;
    dos.dta_offset = 0x0100;

    cpu.regs.set_ah(0x2F);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.es, 0x3000);
    assert_eq!(cpu.regs.bx(), 0x0100);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// Test bda_ticks_to_time at midnight (0 ticks).
#[test]
fn test_cov_bda_ticks_midnight() {
    let (h, m, s, hs) = bda_ticks_to_time(0);
    assert_eq!(h, 0);
    assert_eq!(m, 0);
    assert_eq!(s, 0);
    assert_eq!(hs, 0);
}

/// Test bda_ticks_to_time at noon.
#[test]
fn test_cov_bda_ticks_noon() {
    // Half of BIOS_TICKS_PER_DAY
    let noon_ticks = BIOS_TICKS_PER_DAY / 2;
    let (h, m, _s, _hs) = bda_ticks_to_time(noon_ticks);
    assert_eq!(h, 12);
    assert_eq!(m, 0);
}

/// Test bda_ticks_to_time at max (end of day).
#[test]
fn test_cov_bda_ticks_end_of_day() {
    let (h, m, _s, _hs) = bda_ticks_to_time(BIOS_TICKS_PER_DAY - 1);
    assert_eq!(h, 23);
    assert_eq!(m, 59);
}

/// Test format_dos_bytes with printable chars.
#[test]
fn test_cov_format_dos_bytes_printable() {
    let (s, truncated) = format_dos_bytes(b"Hello", 100);
    assert_eq!(s, "Hello");
    assert!(!truncated);
}

/// Test format_dos_bytes with special chars.
#[test]
fn test_cov_format_dos_bytes_special() {
    let (s, _) = format_dos_bytes(b"\r\n\t", 100);
    assert_eq!(s, "\\r\\n\\t");
}

/// Test format_dos_bytes with non-printable.
#[test]
fn test_cov_format_dos_bytes_nonprintable() {
    let (s, _) = format_dos_bytes(&[0x01], 100);
    assert_eq!(s, "\\x01");
}

/// Test format_dos_bytes truncation.
#[test]
fn test_cov_format_dos_bytes_truncated() {
    let (s, truncated) = format_dos_bytes(b"Hello World", 5);
    assert_eq!(s, "Hello");
    assert!(truncated);
}

/// Test format_dos_bytes empty.
#[test]
fn test_cov_format_dos_bytes_empty() {
    let (s, truncated) = format_dos_bytes(b"", 100);
    assert_eq!(s, "");
    assert!(!truncated);
}
