use super::test_helpers::setup;
use super::*;

/// REP MOVSB bulk fast path: 512-byte copy produces identical results to
/// per-element copy.
#[test]
fn test_rep_movsb_bulk_fast_path() {
    // F3 A4 = REP MOVSB
    let (mut cpu, mut mem) = setup(&[0xF3, 0xA4]);
    // Fill source area with a recognizable pattern
    for i in 0..512usize {
        mem.ram[0x2000 + i] = ((i * 7 + 13) & 0xFF) as u8;
    }
    cpu.regs.set_cx(512);
    cpu.regs.set_si(0x2000);
    cpu.regs.set_di(0x4000);
    cpu.regs.es = 0x0000;
    // DF=0 (forward), A20 enabled by default — should hit bulk path

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 512);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 512 }),
        "got {:?}",
        result
    );
    // CX should be 0 (completed within budget)
    assert_eq!(cpu.regs.cx(), 0, "CX should be 0");
    // SI should have advanced by 512
    assert_eq!(cpu.regs.si(), 0x2000 + 512);
    // DI should have advanced by 512
    assert_eq!(cpu.regs.di(), 0x4000 + 512);
    // Verify all bytes were copied correctly
    for i in 0..512usize {
        let expected = ((i * 7 + 13) & 0xFF) as u8;
        assert_eq!(
            mem.ram[0x4000 + i],
            expected,
            "byte {} mismatch: got {:#x}, expected {:#x}",
            i,
            mem.ram[0x4000 + i],
            expected
        );
    }
    // Verify source is unchanged
    for i in 0..512usize {
        let expected = ((i * 7 + 13) & 0xFF) as u8;
        assert_eq!(mem.ram[0x2000 + i], expected, "source byte {} changed", i);
    }
}

/// REP MOVSD bulk fast path: 128 dword moves (512 bytes total).
#[test]
fn test_rep_movsd_bulk_fast_path() {
    // 66 F3 A5 = REP MOVSD (operand-size override + REP + MOVSW)
    let (mut cpu, mut mem) = setup(&[0x66, 0xF3, 0xA5]);
    // Fill source with dword pattern
    for i in 0..128u32 {
        let val = 0xDEAD_0000u32.wrapping_add(i);
        let base = 0x2000 + (i as usize * 4);
        let bytes = val.to_le_bytes();
        mem.ram[base] = bytes[0];
        mem.ram[base + 1] = bytes[1];
        mem.ram[base + 2] = bytes[2];
        mem.ram[base + 3] = bytes[3];
    }
    cpu.regs.set_cx(128);
    cpu.regs.set_si(0x2000);
    cpu.regs.set_di(0x4000);
    cpu.regs.es = 0x0000;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 128);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 128 }),
        "got {:?}",
        result
    );
    assert_eq!(cpu.regs.cx(), 0, "CX should be 0");
    // SI advanced by 128*4 = 512
    assert_eq!(cpu.regs.si(), 0x2000 + 512);
    // DI advanced by 128*4 = 512
    assert_eq!(cpu.regs.di(), 0x4000 + 512);
    // Verify all dwords were copied correctly
    for i in 0..128u32 {
        let expected = 0xDEAD_0000u32.wrapping_add(i);
        let base = 0x4000 + (i as usize * 4);
        let got = u32::from_le_bytes([
            mem.ram[base],
            mem.ram[base + 1],
            mem.ram[base + 2],
            mem.ram[base + 3],
        ]);
        assert_eq!(got, expected, "dword {} mismatch", i);
    }
}

/// REP STOSB bulk fast path: fill 512 bytes with a value.
#[test]
fn test_rep_stosb_bulk_fast_path() {
    // F3 AA = REP STOSB
    let (mut cpu, mut mem) = setup(&[0xF3, 0xAA]);
    cpu.regs.set_al(0xBB);
    cpu.regs.set_cx(512);
    cpu.regs.set_di(0x4000);
    cpu.regs.es = 0x0000;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 512);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 512 }),
        "got {:?}",
        result
    );
    assert_eq!(cpu.regs.cx(), 0, "CX should be 0");
    assert_eq!(cpu.regs.di(), 0x4000 + 512);
    for i in 0..512usize {
        assert_eq!(
            mem.ram[0x4000 + i],
            0xBB,
            "byte {} should be 0xBB, got {:#x}",
            i,
            mem.ram[0x4000 + i]
        );
    }
    // Byte after the fill region should be untouched
    assert_eq!(mem.ram[0x4000 + 512], 0x00);
}

/// REP STOSD bulk fast path: fill 128 dwords (512 bytes).
#[test]
fn test_rep_stosd_bulk_fast_path() {
    // 66 F3 AB = REP STOSD (operand-size override + REP + STOSW)
    let (mut cpu, mut mem) = setup(&[0x66, 0xF3, 0xAB]);
    cpu.regs.eax = 0xCAFE_BABE;
    cpu.regs.set_cx(128);
    cpu.regs.set_di(0x4000);
    cpu.regs.es = 0x0000;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 128);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 128 }),
        "got {:?}",
        result
    );
    assert_eq!(cpu.regs.cx(), 0, "CX should be 0");
    assert_eq!(cpu.regs.di(), 0x4000 + 512);
    for i in 0..128u32 {
        let base = 0x4000 + (i as usize * 4);
        let got = u32::from_le_bytes([
            mem.ram[base],
            mem.ram[base + 1],
            mem.ram[base + 2],
            mem.ram[base + 3],
        ]);
        assert_eq!(got, 0xCAFE_BABE, "dword {} mismatch: got {:#x}", i, got);
    }
}

/// REP MOVSB with destination in VGA range must use the slow path (VGA
/// writes must go through the Memory pipeline for planar logic).
#[test]
fn test_rep_movsb_vga_uses_slow_path() {
    // F3 A4 = REP MOVSB
    let (mut cpu, mut mem) = setup(&[0xF3, 0xA4]);
    // Source is in normal RAM
    for i in 0..64usize {
        mem.ram[0x2000 + i] = 0xAA;
    }
    cpu.regs.set_cx(64);
    cpu.regs.set_si(0x2000);
    // Destination is in VGA range: segment A000:0000
    cpu.regs.es = 0xA000;
    cpu.regs.set_di(0x0000);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 64);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 64 }),
        "got {:?}",
        result
    );
    assert_eq!(cpu.regs.cx(), 0, "CX should be 0");
    // SI and DI should have advanced by 64
    assert_eq!(cpu.regs.si(), 0x2000 + 64);
    assert_eq!(cpu.regs.di(), 64);
    // The copy should still have happened (via slow path through VGA pipeline).
    // In chain-4 mode (default), bytes are written directly to ram.
    for i in 0..64usize {
        assert_eq!(mem.ram[0xA0000 + i], 0xAA, "VGA byte {} should be 0xAA", i);
    }
}

/// REP MOVSB with DF=1 (backward) must use the slow path.
#[test]
fn test_rep_movsb_backward_uses_slow_path() {
    // F3 A4 = REP MOVSB
    let (mut cpu, mut mem) = setup(&[0xF3, 0xA4]);
    // Fill source
    for i in 0..64usize {
        mem.ram[0x2000 + i] = ((i + 1) & 0xFF) as u8;
    }
    cpu.regs.set_cx(64);
    // Start at end of region, moving backward
    cpu.regs.set_si(0x2000 + 63);
    cpu.regs.set_di(0x4000 + 63);
    cpu.regs.es = 0x0000;
    // Set DF=1 (backward)
    cpu.regs.flags.insert(Flags::DIRECTION);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 64);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 64 }),
        "got {:?}",
        result
    );
    assert_eq!(cpu.regs.cx(), 0, "CX should be 0");
    // SI should have decremented by 64
    assert_eq!(cpu.regs.si(), 0x2000 + 63 - 64);
    // DI should have decremented by 64
    assert_eq!(cpu.regs.di(), 0x4000 + 63 - 64);
    // Verify the copy was correct (backward copy of bytes 63..0)
    for i in 0..64usize {
        let expected = ((i + 1) & 0xFF) as u8;
        assert_eq!(
            mem.ram[0x4000 + i],
            expected,
            "byte {} mismatch: got {:#x}, expected {:#x}",
            i,
            mem.ram[0x4000 + i],
            expected
        );
    }
}

/// REP MOVSB with overlapping source/destination falls to slow path
/// and produces the correct element-by-element "smear" pattern.
#[test]
fn test_rep_movsb_overlapping_correct() {
    // F3 A4 = REP MOVSB
    let (mut cpu, mut mem) = setup(&[0xF3, 0xA4]);
    // Set up: [1, 2, 3, 4, 5, 0, 0]
    // Copy 5 bytes from offset 0x3000 to 0x3002 (overlapping forward)
    // Real x86 REP MOVSB with DF=0 copies one byte at a time:
    //   [1]->pos2, [2]->pos3, [1]->pos4, [2]->pos5, [1]->pos6
    // Result: [1, 2, 1, 2, 1, 2, 1] — the classic forward-overlap smear.
    mem.ram[0x3000] = 1;
    mem.ram[0x3001] = 2;
    mem.ram[0x3002] = 3;
    mem.ram[0x3003] = 4;
    mem.ram[0x3004] = 5;
    cpu.regs.set_cx(5);
    cpu.regs.set_si(0x3000);
    cpu.regs.set_di(0x3002);
    cpu.regs.es = 0x0000;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 5);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 5 }),
        "got {:?}",
        result
    );
    assert_eq!(cpu.regs.cx(), 0);
    // Overlap guard rejects bulk path → slow per-element path → correct smear
    let expected: [u8; 7] = [1, 2, 1, 2, 1, 2, 1];
    for (i, &exp) in expected.iter().enumerate() {
        assert_eq!(
            mem.ram[0x3000 + i],
            exp,
            "byte {} mismatch: got {}, expected {}",
            i,
            mem.ram[0x3000 + i],
            exp
        );
    }
}
