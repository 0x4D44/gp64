use super::test_helpers::setup;
use super::*;
use gp64_cpu::Flags;

// A2: SHL 8-bit, count=9 — CF should be false (all bits shifted past carry)
#[test]
fn test_shl8_count_9_cf_false() {
    // SHL AL, 9: C0 E0 09
    let (mut cpu, mut mem) = setup(&[0xC0, 0xE0, 0x09]);
    cpu.regs.set_al(0xFF);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(
        !cpu.regs.flags.contains(Flags::CARRY),
        "SHL AL,9 with AL=0xFF: CF should be false"
    );
}

// A3: SHR 8-bit, count=9 — CF should be false
#[test]
fn test_shr8_count_9_cf_false() {
    // SHR AL, 9: C0 E8 09
    let (mut cpu, mut mem) = setup(&[0xC0, 0xE8, 0x09]);
    cpu.regs.set_al(0xFF);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(
        !cpu.regs.flags.contains(Flags::CARRY),
        "SHR AL,9 with AL=0xFF: CF should be false"
    );
}

// A4: SHL 16-bit, count=17 — CF should be false
#[test]
fn test_shl16_count_17_cf_false() {
    // SHL AX, 17: C1 E0 11
    let (mut cpu, mut mem) = setup(&[0xC1, 0xE0, 0x11]);
    cpu.regs.set_ax(0xFFFF);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(
        !cpu.regs.flags.contains(Flags::CARRY),
        "SHL AX,17 with AX=0xFFFF: CF should be false"
    );
}

// A5: SHR 16-bit, count=17 — CF should be false
#[test]
fn test_shr16_count_17_cf_false() {
    // SHR AX, 17: C1 E8 11
    let (mut cpu, mut mem) = setup(&[0xC1, 0xE8, 0x11]);
    cpu.regs.set_ax(0xFFFF);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(
        !cpu.regs.flags.contains(Flags::CARRY),
        "SHR AX,17 with AX=0xFFFF: CF should be false"
    );
}

// Boundary: SHL AL, 8 with AL=0x01 — CF should be true (bit 0 shifted into carry)
#[test]
fn test_shl8_count_8_cf_from_bit0() {
    // SHL AL, 8: C0 E0 08
    let (mut cpu, mut mem) = setup(&[0xC0, 0xE0, 0x08]);
    cpu.regs.set_al(0x01);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(
        cpu.regs.flags.contains(Flags::CARRY),
        "SHL AL,8 with AL=0x01: CF should be true"
    );
}

// Boundary: SHR AL, 8 with AL=0x80 — CF should be true (bit 7 shifted into carry)
#[test]
fn test_shr8_count_8_cf_from_bit7() {
    // SHR AL, 8: C0 E8 08
    let (mut cpu, mut mem) = setup(&[0xC0, 0xE8, 0x08]);
    cpu.regs.set_al(0x80);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(
        cpu.regs.flags.contains(Flags::CARRY),
        "SHR AL,8 with AL=0x80: CF should be true"
    );
}

// A6: SHLD 16-bit, count=17 — source bits repeat on a 386
#[test]
fn test_shld16_count_17_repeats_source_bits() {
    // SHLD AX, BX, 17: 0F A4 D8 11
    let (mut cpu, mut mem) = setup(&[0x0F, 0xA4, 0xD8, 0x11]);
    cpu.regs.set_ax(0x1234);
    cpu.regs.set_bx(0x5678);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "SHLD AX,BX,17 should not panic, got {:?}",
        result
    );
    assert_eq!(cpu.regs.ax(), 0xACF0);
    assert!(!cpu.regs.flags.contains(Flags::CARRY));
}

// A6: SHRD 16-bit, count=17 — source bits repeat on a 386
#[test]
fn test_shrd16_count_17_repeats_source_bits() {
    // SHRD AX, BX, 17: 0F AC D8 11
    let (mut cpu, mut mem) = setup(&[0x0F, 0xAC, 0xD8, 0x11]);
    cpu.regs.set_ax(0x1234);
    cpu.regs.set_bx(0x5678);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "SHRD AX,BX,17 should not panic, got {:?}",
        result
    );
    assert_eq!(cpu.regs.ax(), 0x2B3C);
    assert!(!cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_shld16_zero_count_still_checks_memory_segment_limit() {
    // SHLD word [FFFF], DI, C0h: C0h masks to zero, but the word
    // operand still crosses the real-mode segment limit and raises #GP.
    let code: &[u8] = &[0x0F, 0xA4, 0x3E, 0xFF, 0xFF, 0xC0];
    let (mut cpu, mut mem) = setup(code);

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(matches!(result, RunResult::Exception { vector: 13, .. }));
}
