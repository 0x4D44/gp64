use super::test_helpers::setup;
use super::*;
use gp64_cpu::Flags;
use gp64_memory::Memory;

// ========================================================================
// SETcc — all 16 conditions (0F 90 - 0F 9F)
// ========================================================================

#[test]
fn test_seto_true() {
    // SETO AL = 0F 90 C0
    let (mut cpu, mut mem) = setup(&[0x0F, 0x90, 0xC0]);
    cpu.regs.flags.insert(Flags::OVERFLOW);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.al(), 1);
}

#[test]
fn test_seto_false() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x90, 0xC0]);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    // MDD-BUG-CRUCIBLE-00534: seed the destination with a value the
    // instruction must overwrite. AL defaults to 0, which is also the
    // expected false result, so a missing write, an early exception, an
    // unsupported opcode or a complete no-op all left 0 behind and this
    // assertion passed. The true-case siblings assert 1 and were already
    // sensitive; only the false cases could not fail.
    cpu.regs.set_al(0xFF);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "SETO must actually execute, got {result:?}"
    );
    assert_eq!(
        cpu.regs.al(),
        0,
        "SETO false must WRITE zero, not leave the seed"
    );
}

#[test]
fn test_setno_true() {
    // SETNO CL = 0F 91 C1
    let (mut cpu, mut mem) = setup(&[0x0F, 0x91, 0xC1]);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.cl(), 1);
}

#[test]
fn test_setb_true() {
    // SETB DL = 0F 92 C2
    let (mut cpu, mut mem) = setup(&[0x0F, 0x92, 0xC2]);
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.dl(), 1);
}

#[test]
fn test_setnb_true() {
    // SETNB BL = 0F 93 C3
    let (mut cpu, mut mem) = setup(&[0x0F, 0x93, 0xC3]);
    cpu.regs.flags.remove(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.bl(), 1);
}

#[test]
fn test_setz_true() {
    // SETZ AL = 0F 94 C0
    let (mut cpu, mut mem) = setup(&[0x0F, 0x94, 0xC0]);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 1);
}

#[test]
fn test_setnz_true() {
    // SETNZ AL = 0F 95 C0
    let (mut cpu, mut mem) = setup(&[0x0F, 0x95, 0xC0]);
    cpu.regs.flags.remove(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 1);
}

#[test]
fn test_setbe_true() {
    // SETBE AL = 0F 96 C0 (CF=1 or ZF=1)
    let (mut cpu, mut mem) = setup(&[0x0F, 0x96, 0xC0]);
    cpu.regs.flags.insert(Flags::CARRY);
    cpu.regs.flags.remove(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 1);
}

#[test]
fn test_setbe_false() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x96, 0xC0]);
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.flags.remove(Flags::ZERO);
    // MDD-BUG-CRUCIBLE-00534: seed the destination with a value the
    // instruction must overwrite. AL defaults to 0, which is also the
    // expected false result, so a missing write, an early exception, an
    // unsupported opcode or a complete no-op all left 0 behind and this
    // assertion passed. The true-case siblings assert 1 and were already
    // sensitive; only the false cases could not fail.
    cpu.regs.set_al(0xFF);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "SETBE must actually execute, got {result:?}"
    );
    assert_eq!(
        cpu.regs.al(),
        0,
        "SETBE false must WRITE zero, not leave the seed"
    );
}

#[test]
fn test_seta_true() {
    // SETA AL = 0F 97 C0 (CF=0 and ZF=0)
    let (mut cpu, mut mem) = setup(&[0x0F, 0x97, 0xC0]);
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.flags.remove(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 1);
}

#[test]
fn test_sets_true() {
    // SETS AL = 0F 98 C0
    let (mut cpu, mut mem) = setup(&[0x0F, 0x98, 0xC0]);
    cpu.regs.flags.insert(Flags::SIGN);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 1);
}

#[test]
fn test_setns_true() {
    // SETNS AL = 0F 99 C0
    let (mut cpu, mut mem) = setup(&[0x0F, 0x99, 0xC0]);
    cpu.regs.flags.remove(Flags::SIGN);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 1);
}

#[test]
fn test_setp_true() {
    // SETP AL = 0F 9A C0
    let (mut cpu, mut mem) = setup(&[0x0F, 0x9A, 0xC0]);
    cpu.regs.flags.insert(Flags::PARITY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 1);
}

#[test]
fn test_setnp_true() {
    // SETNP AL = 0F 9B C0
    let (mut cpu, mut mem) = setup(&[0x0F, 0x9B, 0xC0]);
    cpu.regs.flags.remove(Flags::PARITY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 1);
}

#[test]
fn test_setl_true() {
    // SETL AL = 0F 9C C0 (SF != OF)
    let (mut cpu, mut mem) = setup(&[0x0F, 0x9C, 0xC0]);
    cpu.regs.flags.insert(Flags::SIGN);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 1);
}

#[test]
fn test_setl_false() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x9C, 0xC0]);
    cpu.regs.flags.insert(Flags::SIGN);
    cpu.regs.flags.insert(Flags::OVERFLOW);
    // MDD-BUG-CRUCIBLE-00534: seed the destination with a value the
    // instruction must overwrite. AL defaults to 0, which is also the
    // expected false result, so a missing write, an early exception, an
    // unsupported opcode or a complete no-op all left 0 behind and this
    // assertion passed. The true-case siblings assert 1 and were already
    // sensitive; only the false cases could not fail.
    cpu.regs.set_al(0xFF);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "SETL must actually execute, got {result:?}"
    );
    assert_eq!(
        cpu.regs.al(),
        0,
        "SETL false must WRITE zero, not leave the seed"
    );
}

#[test]
fn test_setge_true() {
    // SETGE AL = 0F 9D C0 (SF == OF)
    let (mut cpu, mut mem) = setup(&[0x0F, 0x9D, 0xC0]);
    cpu.regs.flags.insert(Flags::SIGN);
    cpu.regs.flags.insert(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 1);
}

#[test]
fn test_setle_true() {
    // SETLE AL = 0F 9E C0 (ZF=1 or SF!=OF)
    let (mut cpu, mut mem) = setup(&[0x0F, 0x9E, 0xC0]);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 1);
}

#[test]
fn test_setg_true() {
    // SETG AL = 0F 9F C0 (ZF=0 and SF==OF)
    let (mut cpu, mut mem) = setup(&[0x0F, 0x9F, 0xC0]);
    cpu.regs.flags.remove(Flags::ZERO);
    cpu.regs.flags.remove(Flags::SIGN);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 1);
}

#[test]
fn test_setcc_memory_operand() {
    // SETZ [0x0200] = 0F 94 06 00 02 (mod=00, reg=000, rm=110 disp16)
    let (mut cpu, mut mem) = setup(&[0x0F, 0x94, 0x06, 0x00, 0x02]);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x0200], 1);
}

// ========================================================================
// String operations: STOSB/STOSW/STOSD
// ========================================================================

#[test]
fn test_stosb_basic() {
    // STOSB = 0xAA — stores AL to ES:[DI], increments DI
    let (mut cpu, mut mem) = setup(&[0xAA]);
    cpu.regs.set_al(0x42);
    cpu.regs.edi = 0x0300;
    cpu.regs.flags.remove(Flags::DIRECTION); // DF=0, forward
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x0300], 0x42);
    assert_eq!(cpu.regs.edi as u16, 0x0301);
}

#[test]
fn test_stosb_direction_flag_backward() {
    // STOSB with DF=1 — DI decrements
    let (mut cpu, mut mem) = setup(&[0xAA]);
    cpu.regs.set_al(0x99);
    cpu.regs.edi = 0x0300;
    cpu.regs.flags.insert(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x0300], 0x99);
    assert_eq!(cpu.regs.edi as u16, 0x02FF);
}

#[test]
fn test_stosw_basic() {
    // STOSW = 0xAB — stores AX to ES:[DI], increments DI by 2
    let (mut cpu, mut mem) = setup(&[0xAB]);
    cpu.regs.set_ax(0xABCD);
    cpu.regs.edi = 0x0300;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u16::from_le_bytes([mem.ram[0x0300], mem.ram[0x0301]]),
        0xABCD
    );
    assert_eq!(cpu.regs.edi as u16, 0x0302);
}

#[test]
fn test_stosd_basic() {
    // STOSD = 66 AB — stores EAX to ES:[DI], increments DI by 4
    let (mut cpu, mut mem) = setup(&[0x66, 0xAB]);
    cpu.regs.eax = 0xDEADBEEF;
    cpu.regs.edi = 0x0300;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x0300],
        mem.ram[0x0301],
        mem.ram[0x0302],
        mem.ram[0x0303],
    ]);
    assert_eq!(val, 0xDEADBEEF);
    assert_eq!(cpu.regs.edi as u16, 0x0304);
}

// ========================================================================
// String operations: LODSB/LODSW/LODSD
// ========================================================================

#[test]
fn test_lodsb_basic() {
    // LODSB = 0xAC — loads byte at DS:[SI] into AL, increments SI
    let (mut cpu, mut mem) = setup(&[0xAC]);
    mem.ram[0x0300] = 0x77;
    cpu.regs.esi = 0x0300;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x77);
    assert_eq!(cpu.regs.esi as u16, 0x0301);
}

#[test]
fn test_lodsb_backward() {
    let (mut cpu, mut mem) = setup(&[0xAC]);
    mem.ram[0x0300] = 0xEE;
    cpu.regs.esi = 0x0300;
    cpu.regs.flags.insert(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xEE);
    assert_eq!(cpu.regs.esi as u16, 0x02FF);
}

#[test]
fn test_lodsw_basic() {
    // LODSW = 0xAD — loads word at DS:[SI] into AX
    let (mut cpu, mut mem) = setup(&[0xAD]);
    mem.ram[0x0300] = 0x34;
    mem.ram[0x0301] = 0x12;
    cpu.regs.esi = 0x0300;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x1234);
    assert_eq!(cpu.regs.esi as u16, 0x0302);
}

#[test]
fn test_lodsd_basic() {
    // LODSD = 66 AD
    let (mut cpu, mut mem) = setup(&[0x66, 0xAD]);
    mem.ram[0x0300] = 0xEF;
    mem.ram[0x0301] = 0xBE;
    mem.ram[0x0302] = 0xAD;
    mem.ram[0x0303] = 0xDE;
    cpu.regs.esi = 0x0300;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xDEADBEEF);
    assert_eq!(cpu.regs.esi as u16, 0x0304);
}

// ========================================================================
// String operations: MOVSB/MOVSW/MOVSD
// ========================================================================

#[test]
fn test_movsb_basic() {
    // MOVSB = 0xA4
    let (mut cpu, mut mem) = setup(&[0xA4]);
    mem.ram[0x0300] = 0xAA;
    cpu.regs.esi = 0x0300;
    cpu.regs.edi = 0x0400;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x0400], 0xAA);
    assert_eq!(cpu.regs.esi as u16, 0x0301);
    assert_eq!(cpu.regs.edi as u16, 0x0401);
}

#[test]
fn test_movsb_backward() {
    let (mut cpu, mut mem) = setup(&[0xA4]);
    mem.ram[0x0300] = 0xBB;
    cpu.regs.esi = 0x0300;
    cpu.regs.edi = 0x0400;
    cpu.regs.flags.insert(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x0400], 0xBB);
    assert_eq!(cpu.regs.esi as u16, 0x02FF);
    assert_eq!(cpu.regs.edi as u16, 0x03FF);
}

#[test]
fn test_movsw_basic() {
    // MOVSW = 0xA5
    let (mut cpu, mut mem) = setup(&[0xA5]);
    mem.ram[0x0300] = 0x78;
    mem.ram[0x0301] = 0x56;
    cpu.regs.esi = 0x0300;
    cpu.regs.edi = 0x0400;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u16::from_le_bytes([mem.ram[0x0400], mem.ram[0x0401]]),
        0x5678
    );
    assert_eq!(cpu.regs.esi as u16, 0x0302);
    assert_eq!(cpu.regs.edi as u16, 0x0402);
}

#[test]
fn test_movsd_basic() {
    // MOVSD = 66 A5
    let (mut cpu, mut mem) = setup(&[0x66, 0xA5]);
    mem.ram[0x0300] = 0x01;
    mem.ram[0x0301] = 0x02;
    mem.ram[0x0302] = 0x03;
    mem.ram[0x0303] = 0x04;
    cpu.regs.esi = 0x0300;
    cpu.regs.edi = 0x0400;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x0400],
        mem.ram[0x0401],
        mem.ram[0x0402],
        mem.ram[0x0403],
    ]);
    assert_eq!(val, 0x04030201);
    assert_eq!(cpu.regs.esi as u16, 0x0304);
    assert_eq!(cpu.regs.edi as u16, 0x0404);
}

// ========================================================================
// String operations: SCASB/SCASW
// ========================================================================

#[test]
fn test_scasb_equal() {
    // SCASB = 0xAE — compares AL with ES:[DI]
    let (mut cpu, mut mem) = setup(&[0xAE]);
    cpu.regs.set_al(0x42);
    mem.ram[0x0300] = 0x42;
    cpu.regs.edi = 0x0300;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.edi as u16, 0x0301);
}

#[test]
fn test_scasb_not_equal() {
    let (mut cpu, mut mem) = setup(&[0xAE]);
    cpu.regs.set_al(0x42);
    mem.ram[0x0300] = 0x99;
    cpu.regs.edi = 0x0300;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_scasw_equal() {
    // SCASW = 0xAF
    let (mut cpu, mut mem) = setup(&[0xAF]);
    cpu.regs.set_ax(0x1234);
    mem.ram[0x0300] = 0x34;
    mem.ram[0x0301] = 0x12;
    cpu.regs.edi = 0x0300;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.edi as u16, 0x0302);
}

// ========================================================================
// String operations: CMPSB/CMPSW
// ========================================================================

#[test]
fn test_cmpsb_equal() {
    // CMPSB = 0xA6
    let (mut cpu, mut mem) = setup(&[0xA6]);
    mem.ram[0x0300] = 0x55;
    mem.ram[0x0400] = 0x55;
    cpu.regs.esi = 0x0300;
    cpu.regs.edi = 0x0400;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.esi as u16, 0x0301);
    assert_eq!(cpu.regs.edi as u16, 0x0401);
}

#[test]
fn test_cmpsb_not_equal() {
    let (mut cpu, mut mem) = setup(&[0xA6]);
    mem.ram[0x0300] = 0x55;
    mem.ram[0x0400] = 0xAA;
    cpu.regs.esi = 0x0300;
    cpu.regs.edi = 0x0400;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_cmpsw_equal() {
    // CMPSW = 0xA7
    let (mut cpu, mut mem) = setup(&[0xA7]);
    mem.ram[0x0300] = 0x34;
    mem.ram[0x0301] = 0x12;
    mem.ram[0x0400] = 0x34;
    mem.ram[0x0401] = 0x12;
    cpu.regs.esi = 0x0300;
    cpu.regs.edi = 0x0400;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.esi as u16, 0x0302);
    assert_eq!(cpu.regs.edi as u16, 0x0402);
}

// ========================================================================
// REP prefix with string operations
// ========================================================================

#[test]
fn test_rep_stosb_fills_buffer() {
    // REP STOSB = F3 AA
    let (mut cpu, mut mem) = setup(&[0xF3, 0xAA]);
    cpu.regs.set_al(0xFF);
    cpu.regs.edi = 0x0300;
    cpu.regs.ecx = 4;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 100);
    for i in 0..4 {
        assert_eq!(mem.ram[0x0300 + i], 0xFF, "byte {} not filled", i);
    }
    assert_eq!(cpu.regs.ecx as u16, 0);
    assert_eq!(cpu.regs.edi as u16, 0x0304);
}

#[test]
fn test_rep_movsb_copies_buffer() {
    // REP MOVSB = F3 A4
    let (mut cpu, mut mem) = setup(&[0xF3, 0xA4]);
    for i in 0..5u8 {
        mem.ram[0x0300 + i as usize] = i + 1;
    }
    cpu.regs.esi = 0x0300;
    cpu.regs.edi = 0x0400;
    cpu.regs.ecx = 5;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 100);
    for i in 0..5 {
        assert_eq!(mem.ram[0x0400 + i], (i + 1) as u8, "byte {} not copied", i);
    }
    assert_eq!(cpu.regs.ecx as u16, 0);
}

#[test]
fn test_rep_stosw_fills_words() {
    // REP STOSW = F3 AB
    let (mut cpu, mut mem) = setup(&[0xF3, 0xAB]);
    cpu.regs.set_ax(0xBEEF);
    cpu.regs.edi = 0x0300;
    cpu.regs.ecx = 3;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 100);
    for i in 0..3 {
        let addr = 0x0300 + i * 2;
        let val = u16::from_le_bytes([mem.ram[addr], mem.ram[addr + 1]]);
        assert_eq!(val, 0xBEEF, "word {} not filled", i);
    }
    assert_eq!(cpu.regs.ecx as u16, 0);
}

#[test]
fn test_rep_lodsb() {
    // REP LODSB = F3 AC — loads bytes into AL repeatedly, final AL = last byte
    let (mut cpu, mut mem) = setup(&[0xF3, 0xAC]);
    mem.ram[0x0300] = 0x11;
    mem.ram[0x0301] = 0x22;
    mem.ram[0x0302] = 0x33;
    cpu.regs.esi = 0x0300;
    cpu.regs.ecx = 3;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 100);
    assert_eq!(cpu.regs.al(), 0x33);
    assert_eq!(cpu.regs.ecx as u16, 0);
}

#[test]
fn test_repz_cmpsb_equal_strings() {
    // REPE CMPSB = F3 A6
    let (mut cpu, mut mem) = setup(&[0xF3, 0xA6]);
    let data = b"ABCD";
    for (i, &b) in data.iter().enumerate() {
        mem.ram[0x0300 + i] = b;
        mem.ram[0x0400 + i] = b;
    }
    cpu.regs.esi = 0x0300;
    cpu.regs.edi = 0x0400;
    cpu.regs.ecx = 4;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 100);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.ecx as u16, 0);
}

#[test]
fn test_repz_cmpsb_mismatch() {
    // REPE CMPSB — stops at mismatch
    let (mut cpu, mut mem) = setup(&[0xF3, 0xA6]);
    // Place matching data then a mismatch further in
    for i in 0..20usize {
        mem.ram[0x3000 + i] = 0xAA;
        mem.ram[0x6000 + i] = 0xAA;
    }
    mem.ram[0x6000 + 5] = 0xBB; // mismatch at element 5
    cpu.regs.esi = 0x3000;
    cpu.regs.edi = 0x6000;
    cpu.regs.ecx = 20;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    // Budget=6 covers the 6 comparisons (5 match, 1 mismatch)
    let _ = run(&mut cpu, &mut mem, &mut io, 6);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.ecx as u16, 14); // 20 - 6 = 14
}

#[test]
fn test_repne_scasb_finds_byte() {
    // REPNE SCASB = F2 AE — scan until AL matches
    let (mut cpu, mut mem) = setup(&[0xF2, 0xAE]);
    cpu.regs.set_al(0x00); // search for null terminator
    mem.ram[0x0300] = 0x41;
    mem.ram[0x0301] = 0x42;
    mem.ram[0x0302] = 0x00; // found here
    mem.ram[0x0303] = 0x43;
    cpu.regs.edi = 0x0300;
    cpu.regs.ecx = 4;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 100);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.ecx as u16, 1); // 4 - 3 consumed = 1
}

#[test]
fn test_rep_movsw_copies_words() {
    // REP MOVSW = F3 A5
    let (mut cpu, mut mem) = setup(&[0xF3, 0xA5]);
    mem.ram[0x0300] = 0x11;
    mem.ram[0x0301] = 0x22;
    mem.ram[0x0302] = 0x33;
    mem.ram[0x0303] = 0x44;
    cpu.regs.esi = 0x0300;
    cpu.regs.edi = 0x0400;
    cpu.regs.ecx = 2;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 100);
    assert_eq!(mem.ram[0x0400], 0x11);
    assert_eq!(mem.ram[0x0401], 0x22);
    assert_eq!(mem.ram[0x0402], 0x33);
    assert_eq!(mem.ram[0x0403], 0x44);
    assert_eq!(cpu.regs.ecx as u16, 0);
}

// ========================================================================
// Segment override handling
// ========================================================================

#[test]
fn test_es_segment_override_mov() {
    // ES: MOV AL, [0x0200] = 0x26 0xA0 0x00 0x02
    let (mut cpu, mut mem) = setup(&[0x26, 0xA0, 0x00, 0x02]);
    cpu.regs.es = 0x0010; // ES base = 0x100
    mem.ram[0x0300] = 0x77; // 0x100 + 0x200 = 0x300
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x77);
}

#[test]
fn test_cs_segment_override_mov() {
    // CS: MOV AL, [0x0200] = 0x2E 0xA0 0x00 0x02
    let (mut cpu, mut mem) = setup(&[0x2E, 0xA0, 0x00, 0x02]);
    cpu.regs.cs = 0x0000;
    mem.ram[0x0200] = 0x88;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x88);
}

#[test]
fn test_ss_segment_override_mov() {
    // SS: MOV AL, [0x0200] = 0x36 0xA0 0x00 0x02
    let (mut cpu, mut mem) = setup(&[0x36, 0xA0, 0x00, 0x02]);
    cpu.regs.ss = 0x0020; // SS base = 0x200
    mem.ram[0x0400] = 0x99; // 0x200 + 0x200 = 0x400
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x99);
}

#[test]
fn test_fs_segment_override_mov() {
    // FS: MOV AL, [0x0200] = 0x64 0xA0 0x00 0x02
    let (mut cpu, mut mem) = setup(&[0x64, 0xA0, 0x00, 0x02]);
    cpu.regs.fs = 0x0030; // FS base = 0x300
    mem.ram[0x0500] = 0xAA; // 0x300 + 0x200 = 0x500
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xAA);
}

#[test]
fn test_gs_segment_override_mov() {
    // GS: MOV AL, [0x0200] = 0x65 0xA0 0x00 0x02
    let (mut cpu, mut mem) = setup(&[0x65, 0xA0, 0x00, 0x02]);
    cpu.regs.gs = 0x0040; // GS base = 0x400
    mem.ram[0x0600] = 0xBB; // 0x400 + 0x200 = 0x600
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xBB);
}

// ========================================================================
// Operand size override
// ========================================================================

#[test]
fn test_operand_size_override_mov_eax_imm32() {
    // 66 B8 EF BE AD DE = MOV EAX, 0xDEADBEEF (operand size prefix in 16-bit mode)
    let (mut cpu, mut mem) = setup(&[0x66, 0xB8, 0xEF, 0xBE, 0xAD, 0xDE]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xDEADBEEF);
}

#[test]
fn test_operand_size_override_push_pop_32() {
    // 66 50 = PUSH EAX, 66 58 = POP EAX (32-bit in 16-bit mode)
    let (mut cpu, mut mem) = setup(&[0x66, 0x50, 0x66, 0x58]);
    cpu.regs.eax = 0xCAFE_BABE;
    cpu.regs.esp = 0x8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eax, 0xCAFE_BABE);
    assert_eq!(cpu.regs.esp as u16, 0x8000);
}

// ========================================================================
// Address size override (0x67)
// ========================================================================

#[test]
fn test_addr_size_override_nop_prefix() {
    // 67 90 = NOP with address-size prefix (should still be NOP)
    let (mut cpu, mut mem) = setup(&[0x67, 0x90]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eip, 0x0102);
}

// ========================================================================
// Shift/rotate instructions
// ========================================================================

#[test]
fn test_shl_al_1() {
    // SHL AL, 1 = D0 E0
    let (mut cpu, mut mem) = setup(&[0xD0, 0xE0]);
    cpu.regs.set_al(0x80);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x00);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_shr_al_1() {
    // SHR AL, 1 = D0 E8
    let (mut cpu, mut mem) = setup(&[0xD0, 0xE8]);
    cpu.regs.set_al(0x01);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x00);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_sar_al_1() {
    // SAR AL, 1 = D0 F8
    let (mut cpu, mut mem) = setup(&[0xD0, 0xF8]);
    cpu.regs.set_al(0x80); // -128
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xC0); // -64, sign bit preserved
}

#[test]
fn test_rol_al_1() {
    // ROL AL, 1 = D0 C0
    let (mut cpu, mut mem) = setup(&[0xD0, 0xC0]);
    cpu.regs.set_al(0x80);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x01);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_ror_al_1() {
    // ROR AL, 1 = D0 C8
    let (mut cpu, mut mem) = setup(&[0xD0, 0xC8]);
    cpu.regs.set_al(0x01);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x80);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_rcl_al_1() {
    // RCL AL, 1 = D0 D0
    let (mut cpu, mut mem) = setup(&[0xD0, 0xD0]);
    cpu.regs.set_al(0x80);
    cpu.regs.flags.insert(Flags::CARRY); // CF=1
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x01); // 0x80 << 1 | CF = 0x01, old bit7 -> CF
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_rcr_al_1() {
    // RCR AL, 1 = D0 D8
    let (mut cpu, mut mem) = setup(&[0xD0, 0xD8]);
    cpu.regs.set_al(0x01);
    cpu.regs.flags.insert(Flags::CARRY); // CF=1
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x80); // CF(1) shifted into bit7, bit0 -> CF
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_shl_ax_cl() {
    // SHL AX, CL = D3 E0
    let (mut cpu, mut mem) = setup(&[0xD3, 0xE0]);
    cpu.regs.set_ax(0x0001);
    cpu.regs.set_cl(4);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0010);
}

#[test]
fn test_shr_ax_cl() {
    // SHR AX, CL = D3 E8
    let (mut cpu, mut mem) = setup(&[0xD3, 0xE8]);
    cpu.regs.set_ax(0x0100);
    cpu.regs.set_cl(4);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0010);
}

#[test]
fn test_shl_ax_imm8() {
    // SHL AX, 3 = C1 E0 03
    let (mut cpu, mut mem) = setup(&[0xC1, 0xE0, 0x03]);
    cpu.regs.set_ax(0x0001);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0008);
}

#[test]
fn test_sar_ax_1() {
    // SAR AX, 1 = D1 F8
    let (mut cpu, mut mem) = setup(&[0xD1, 0xF8]);
    cpu.regs.set_ax(0x8000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xC000);
}

#[test]
fn test_rol_ax_1() {
    // ROL AX, 1 = D1 C0
    let (mut cpu, mut mem) = setup(&[0xD1, 0xC0]);
    cpu.regs.set_ax(0x8000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0001);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_ror_ax_1() {
    // ROR AX, 1 = D1 C8
    let (mut cpu, mut mem) = setup(&[0xD1, 0xC8]);
    cpu.regs.set_ax(0x0001);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x8000);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_shl_count_zero_no_flags_change() {
    // SHL AX, 0 — count=0 means no operation, flags unchanged
    let (mut cpu, mut mem) = setup(&[0xC1, 0xE0, 0x00]);
    cpu.regs.set_ax(0x1234);
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x1234);
    assert!(cpu.regs.flags.contains(Flags::CARRY)); // unchanged
}

// ========================================================================
// Bit manipulation: BT, BTS, BTR, BTC, BSF, BSR
// ========================================================================

#[test]
fn test_bt_reg16_imm8() {
    // BT AX, 3 = 0F BA E0 03
    let (mut cpu, mut mem) = setup(&[0x0F, 0xBA, 0xE0, 0x03]);
    cpu.regs.set_ax(0x0008); // bit 3 set
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_bt_reg16_imm8_clear() {
    // BT AX, 3 when bit 3 is clear
    let (mut cpu, mut mem) = setup(&[0x0F, 0xBA, 0xE0, 0x03]);
    cpu.regs.set_ax(0x0000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_bts_reg16_imm8() {
    // BTS AX, 5 = 0F BA E8 05
    let (mut cpu, mut mem) = setup(&[0x0F, 0xBA, 0xE8, 0x05]);
    cpu.regs.set_ax(0x0000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::CARRY)); // was clear
    assert_eq!(cpu.regs.ax(), 0x0020); // bit 5 now set
}

#[test]
fn test_btr_reg16_imm8() {
    // BTR AX, 5 = 0F BA F0 05
    let (mut cpu, mut mem) = setup(&[0x0F, 0xBA, 0xF0, 0x05]);
    cpu.regs.set_ax(0x0020); // bit 5 set
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY)); // was set
    assert_eq!(cpu.regs.ax(), 0x0000); // bit 5 now clear
}

#[test]
fn test_btc_reg16_imm8() {
    // BTC AX, 5 = 0F BA F8 05
    let (mut cpu, mut mem) = setup(&[0x0F, 0xBA, 0xF8, 0x05]);
    cpu.regs.set_ax(0x0000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::CARRY)); // was clear
    assert_eq!(cpu.regs.ax(), 0x0020); // toggled to set
}

#[test]
fn test_bt_reg16_reg16() {
    // BT AX, CX = 0F A3 C8
    let (mut cpu, mut mem) = setup(&[0x0F, 0xA3, 0xC8]);
    cpu.regs.set_ax(0x0008);
    cpu.regs.set_cx(3);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_bts_reg16_reg16() {
    // BTS AX, CX = 0F AB C8
    let (mut cpu, mut mem) = setup(&[0x0F, 0xAB, 0xC8]);
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(7);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::CARRY)); // was clear
    assert_eq!(cpu.regs.ax(), 0x0080);
}

#[test]
fn test_btr_reg16_reg16() {
    // BTR AX, CX = 0F B3 C8
    let (mut cpu, mut mem) = setup(&[0x0F, 0xB3, 0xC8]);
    cpu.regs.set_ax(0x0080);
    cpu.regs.set_cx(7);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY)); // was set
    assert_eq!(cpu.regs.ax(), 0x0000);
}

#[test]
fn test_bsf_found() {
    // BSF AX, BX = 0F BC C3
    let (mut cpu, mut mem) = setup(&[0x0F, 0xBC, 0xC3]);
    cpu.regs.set_bx(0x0010); // bit 4 is lowest set
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 4);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_bsf_zero_source() {
    // BSF AX, BX = 0F BC C3 with BX=0
    let (mut cpu, mut mem) = setup(&[0x0F, 0xBC, 0xC3]);
    cpu.regs.set_bx(0x0000);
    cpu.regs.set_ax(0x1234);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_bsr_found() {
    // BSR AX, BX = 0F BD C3
    let (mut cpu, mut mem) = setup(&[0x0F, 0xBD, 0xC3]);
    cpu.regs.set_bx(0x0100); // bit 8 is highest set
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 8);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_bsr_zero_source() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0xBD, 0xC3]);
    cpu.regs.set_bx(0x0000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// BCD instructions: DAA, DAS, AAA, AAS, AAM, AAD
// ========================================================================

#[test]
fn test_daa_no_adjust() {
    // DAA = 0x27 — no adjust needed
    let (mut cpu, mut mem) = setup(&[0x27]);
    cpu.regs.set_al(0x15); // valid BCD
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.flags.remove(Flags::AUX_CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x15);
}

#[test]
fn test_daa_low_nibble_adjust() {
    // DAA with low nibble > 9
    let (mut cpu, mut mem) = setup(&[0x27]);
    cpu.regs.set_al(0x0A);
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.flags.remove(Flags::AUX_CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x10);
    assert!(cpu.regs.flags.contains(Flags::AUX_CARRY));
}

#[test]
fn test_das_no_adjust() {
    // DAS = 0x2F
    let (mut cpu, mut mem) = setup(&[0x2F]);
    cpu.regs.set_al(0x15);
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.flags.remove(Flags::AUX_CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x15);
}

#[test]
fn test_das_low_nibble_adjust() {
    let (mut cpu, mut mem) = setup(&[0x2F]);
    cpu.regs.set_al(0x1A);
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.flags.remove(Flags::AUX_CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x14);
}

#[test]
fn test_aaa_adjust() {
    // AAA = 0x37 — adjust after addition
    let (mut cpu, mut mem) = setup(&[0x37]);
    cpu.regs.set_al(0x0A);
    cpu.regs.flags.remove(Flags::AUX_CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al() & 0x0F, 0x00);
}

#[test]
fn test_aas_adjust() {
    // AAS = 0x3F — adjust after subtraction
    let (mut cpu, mut mem) = setup(&[0x3F]);
    cpu.regs.set_ax(0x00FF); // low nibble > 9
    cpu.regs.flags.remove(Flags::AUX_CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // AAS should adjust
    let al = cpu.regs.al();
    assert_eq!(al & 0x0F, 0x09);
}

#[test]
fn test_aam_various_values() {
    // AAM = D4 0A
    let (mut cpu, mut mem) = setup(&[0xD4, 0x0A]);
    cpu.regs.set_al(0x35); // 53 decimal
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ah(), 5); // 53 / 10
    assert_eq!(cpu.regs.al(), 3); // 53 % 10
}

#[test]
fn test_aad_various_values() {
    // AAD = D5 0A
    let (mut cpu, mut mem) = setup(&[0xD5, 0x0A]);
    cpu.regs.set_ah(5);
    cpu.regs.set_al(3);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 53); // 5*10 + 3
    assert_eq!(cpu.regs.ah(), 0);
}

// ========================================================================
// XCHG instructions
// ========================================================================

#[test]
fn test_xchg_ax_bx() {
    // XCHG AX, BX = 0x93
    let (mut cpu, mut mem) = setup(&[0x93]);
    cpu.regs.set_ax(0x1111);
    cpu.regs.set_bx(0x2222);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x2222);
    assert_eq!(cpu.regs.bx(), 0x1111);
}

#[test]
fn test_xchg_ax_dx() {
    // XCHG AX, DX = 0x92
    let (mut cpu, mut mem) = setup(&[0x92]);
    cpu.regs.set_ax(0xAAAA);
    cpu.regs.set_dx(0xBBBB);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xBBBB);
    assert_eq!(cpu.regs.dx(), 0xAAAA);
}

#[test]
fn test_xchg_r8_r8() {
    // XCHG AL, CL = 86 C1 (modrm: mod=11, reg=AL, rm=CL)
    let (mut cpu, mut mem) = setup(&[0x86, 0xC1]);
    cpu.regs.set_al(0x11);
    cpu.regs.set_cl(0x22);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x22);
    assert_eq!(cpu.regs.cl(), 0x11);
}

#[test]
fn test_xchg_r16_r16() {
    // XCHG BX, CX = 87 D9 (modrm: mod=11, reg=BX, rm=CX)
    let (mut cpu, mut mem) = setup(&[0x87, 0xD9]);
    cpu.regs.set_bx(0x3333);
    cpu.regs.set_cx(0x4444);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.bx(), 0x4444);
    assert_eq!(cpu.regs.cx(), 0x3333);
}

#[test]
fn test_xchg_r16_mem() {
    // XCHG AX, [0x0200] = 87 06 00 02 (modrm: mod=00, reg=AX, rm=110 disp16)
    let (mut cpu, mut mem) = setup(&[0x87, 0x06, 0x00, 0x02]);
    cpu.regs.set_ax(0x1234);
    mem.ram[0x0200] = 0x78;
    mem.ram[0x0201] = 0x56;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x5678);
    assert_eq!(
        u16::from_le_bytes([mem.ram[0x0200], mem.ram[0x0201]]),
        0x1234
    );
}

// ========================================================================
// CMPXCHG
// ========================================================================

#[test]
fn test_cmpxchg8_equal_stores() {
    // CMPXCHG CL, DL = 0F B0 D1 — if AL==CL, store DL->CL, set ZF
    // modrm: reg=DL(2), rm=CL(1)
    let (mut cpu, mut mem) = setup(&[0x0F, 0xB0, 0xD1]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    cpu.regs.set_al(0x42);
    cpu.regs.set_cl(0x42); // matches AL
    cpu.regs.set_dl(0x99);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.cl(), 0x99);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_cmpxchg8_not_equal_loads() {
    // If AL != CL, load CL->AL, clear ZF
    let (mut cpu, mut mem) = setup(&[0x0F, 0xB0, 0xD1]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    cpu.regs.set_al(0x42);
    cpu.regs.set_cl(0x99); // doesn't match
    cpu.regs.set_dl(0xFF);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x99);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_cmpxchg16_equal() {
    // CMPXCHG CX, DX = 0F B1 D1
    let (mut cpu, mut mem) = setup(&[0x0F, 0xB1, 0xD1]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    cpu.regs.set_ax(0x1234);
    cpu.regs.set_cx(0x1234);
    cpu.regs.set_dx(0xABCD);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.cx(), 0xABCD);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_cmpxchg_mem8() {
    // CMPXCHG [BX], DL = 0F B0 17 (modrm: reg=DL(2), rm=[BX](7))
    let (mut cpu, mut mem) = setup(&[0x0F, 0xB0, 0x17]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    cpu.regs.set_al(0x55);
    cpu.regs.ebx = 0x0200;
    mem.ram[0x0200] = 0x55; // matches AL
    cpu.regs.set_dl(0xAA);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x0200], 0xAA);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// ENTER / LEAVE
// ========================================================================

#[test]
fn test_enter_level0() {
    // ENTER 8, 0 = C8 08 00 00
    let (mut cpu, mut mem) = setup(&[0xC8, 0x08, 0x00, 0x00]);
    cpu.regs.esp = 0x8000;
    cpu.regs.ebp = 0x1234;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // ENTER pushes BP, then sets BP=SP, then SP -= size
    let sp = cpu.regs.sp();
    let bp = cpu.regs.bp();
    assert_eq!(bp, 0x7FFE); // old SP - 2
    assert_eq!(sp, 0x7FF6); // BP - 8
}

#[test]
fn test_leave() {
    // LEAVE = C9 — reverses ENTER: SP = BP, POP BP
    let (mut cpu, mut mem) = setup(&[0xC9]);
    cpu.regs.esp = 0x7FF0;
    cpu.regs.ebp = 0x7FFE;
    // At [7FFE] push old BP=0x1234
    mem.ram[0x7FFE] = 0x34;
    mem.ram[0x7FFF] = 0x12;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.bp(), 0x1234);
    assert_eq!(cpu.regs.sp(), 0x8000);
}

#[test]
fn test_leave_o32_on_16bit_stack_uses_bp_and_preserves_esp_high() {
    // 66 C9 = o32 LEAVE in the default 16-bit real-mode code segment.
    // Operand size chooses POP EBP, while the 16-bit stack address size
    // still requires SP <- BP and must preserve ESP[31:16].
    let (mut cpu, mut mem) = setup(&[0x66, 0xC9]);
    cpu.regs.esp = 0xA5A5_F000;
    cpu.regs.ebp = 0xDEAD_0200;
    mem.ram[0x0200..0x0204].copy_from_slice(&0x1234_5678u32.to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ebp, 0x1234_5678);
    assert_eq!(cpu.regs.esp, 0xA5A5_0204);
}

#[test]
fn test_real_mode_retf_o32_target_limit_fault_is_atomic() {
    for (name, code) in [
        ("RETF", &[0x66, 0xCB][..]),
        ("RETF imm16", &[0x66, 0xCA, 0x04, 0x00][..]),
    ] {
        let (mut cpu, mut mem) = setup(code);
        cpu.regs.esp = 0xA5A5_0200;
        cpu.regs.cs_cache.limit = 0xFFFF;
        cpu.regs.cs_cache.granularity = false;
        cpu.regs.cs_cache.recompute_effective_limit();
        let saved_cs = cpu.regs.cs;
        let saved_cs_cache = cpu.regs.cs_cache;
        let saved_cpl = cpu.regs.cpl;
        let saved_cs_bits = cpu.cs_bits;
        let saved_esp = cpu.regs.esp;

        mem.ram[0x0200..0x0204].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        mem.ram[0x0204..0x0208].copy_from_slice(&0x0000_1234u32.to_le_bytes());

        let result = run(&mut cpu, &mut mem, &mut (), 1);

        assert!(
            matches!(
                result,
                RunResult::Exception {
                    vector: 13,
                    error_code: 0,
                    executed: 0,
                    ..
                }
            ),
            "{name} must #GP(0) for EIP beyond the preserved CS limit, got {result:?}"
        );
        assert_eq!(cpu.regs.cs, saved_cs, "{name} must restore CS");
        assert_eq!(
            cpu.regs.cs_cache, saved_cs_cache,
            "{name} must restore the hidden CS cache"
        );
        assert_eq!(cpu.regs.cpl, saved_cpl, "{name} must restore CPL");
        assert_eq!(cpu.cs_bits, saved_cs_bits, "{name} must restore CS width");
        assert_eq!(cpu.regs.esp, saved_esp, "{name} must restore full ESP");
        assert_eq!(cpu.regs.sp(), saved_esp as u16, "{name} must restore SP");
        assert_eq!(cpu.regs.eip, 0x0100, "{name} must restart at the opcode");
    }
}

// ========================================================================
// MOVSX / MOVZX
// ========================================================================

#[test]
fn test_movzx_r16_r8() {
    // MOVZX AX, CL = 0F B6 C1
    let (mut cpu, mut mem) = setup(&[0x0F, 0xB6, 0xC1]);
    cpu.regs.set_cl(0x80);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0080);
}

#[test]
fn test_movsx_r16_r8() {
    // MOVSX AX, CL = 0F BE C1
    let (mut cpu, mut mem) = setup(&[0x0F, 0xBE, 0xC1]);
    cpu.regs.set_cl(0x80);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xFF80); // sign extended
}

#[test]
fn test_movzx_r32_r8() {
    // 66 0F B6 C1 = MOVZX EAX, CL (32-bit in 16-bit mode)
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xB6, 0xC1]);
    cpu.regs.set_cl(0xFF);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x000000FF);
}

#[test]
fn test_movsx_r32_r8() {
    // 66 0F BE C1 = MOVSX EAX, CL
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xBE, 0xC1]);
    cpu.regs.set_cl(0x80);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFFFFFF80);
}

#[test]
fn test_movzx_r16_mem8() {
    // MOVZX AX, [0x0200] = 0F B6 06 00 02
    let (mut cpu, mut mem) = setup(&[0x0F, 0xB6, 0x06, 0x00, 0x02]);
    mem.ram[0x0200] = 0xAB;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x00AB);
}

#[test]
fn test_movsx_r16_mem8() {
    // MOVSX AX, [0x0200] = 0F BE 06 00 02
    let (mut cpu, mut mem) = setup(&[0x0F, 0xBE, 0x06, 0x00, 0x02]);
    mem.ram[0x0200] = 0x80;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xFF80);
}

// ========================================================================
// INC / DEC register shortcuts (0x40-0x4F)
// ========================================================================

#[test]
fn test_inc_ax() {
    // INC AX = 0x40
    let (mut cpu, mut mem) = setup(&[0x40]);
    cpu.regs.set_ax(0xFFFF);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0000);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_dec_ax() {
    // DEC AX = 0x48
    let (mut cpu, mut mem) = setup(&[0x48]);
    cpu.regs.set_ax(0x0000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xFFFF);
    assert!(cpu.regs.flags.contains(Flags::SIGN));
}

#[test]
fn test_inc_cx() {
    // INC CX = 0x41
    let (mut cpu, mut mem) = setup(&[0x41]);
    cpu.regs.set_cx(0x0000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.cx(), 0x0001);
}

#[test]
fn test_dec_all_regs() {
    // DEC CX, DEC DX, DEC BX = 49 4A 4B
    let (mut cpu, mut mem) = setup(&[0x49, 0x4A, 0x4B]);
    cpu.regs.set_cx(10);
    cpu.regs.set_dx(20);
    cpu.regs.set_bx(30);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    assert_eq!(cpu.regs.cx(), 9);
    assert_eq!(cpu.regs.dx(), 19);
    assert_eq!(cpu.regs.bx(), 29);
}

// ========================================================================
// PUSH / POP register shortcuts (0x50-0x5F)
// ========================================================================

#[test]
fn test_push_pop_all_regs() {
    // PUSH AX, PUSH CX, POP CX, POP AX = 50 51 59 58
    let (mut cpu, mut mem) = setup(&[0x50, 0x51, 0x59, 0x58]);
    cpu.regs.set_ax(0x1111);
    cpu.regs.set_cx(0x2222);
    cpu.regs.esp = 0x8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 4);
    assert_eq!(cpu.regs.ax(), 0x1111);
    assert_eq!(cpu.regs.cx(), 0x2222);
    assert_eq!(cpu.regs.sp(), 0x8000);
}

#[test]
fn test_push_imm16() {
    // PUSH 0x1234 = 68 34 12
    let (mut cpu, mut mem) = setup(&[0x68, 0x34, 0x12]);
    cpu.regs.esp = 0x8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.sp(), 0x7FFE);
    let val = u16::from_le_bytes([mem.ram[0x7FFE], mem.ram[0x7FFF]]);
    assert_eq!(val, 0x1234);
}

#[test]
fn test_push_imm8_sign_ext() {
    // PUSH sign-extended imm8 = 6A FF (-1)
    let (mut cpu, mut mem) = setup(&[0x6A, 0xFF]);
    cpu.regs.esp = 0x8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.sp(), 0x7FFE);
    let val = u16::from_le_bytes([mem.ram[0x7FFE], mem.ram[0x7FFF]]);
    assert_eq!(val, 0xFFFF);
}

// ========================================================================
// PUSHA / POPA (0x60 / 0x61)
// ========================================================================

#[test]
fn test_pusha_popa_roundtrip() {
    // PUSHA POPA = 60 61
    let (mut cpu, mut mem) = setup(&[0x60, 0x61]);
    cpu.regs.eax = 0x1111;
    cpu.regs.ecx = 0x2222;
    cpu.regs.edx = 0x3333;
    cpu.regs.ebx = 0x4444;
    cpu.regs.esp = 0x8000;
    cpu.regs.ebp = 0x5555;
    cpu.regs.esi = 0x6666;
    cpu.regs.edi = 0x7777;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.ax(), 0x1111);
    assert_eq!(cpu.regs.cx(), 0x2222);
    assert_eq!(cpu.regs.dx(), 0x3333);
    assert_eq!(cpu.regs.bx(), 0x4444);
    assert_eq!(cpu.regs.bp(), 0x5555);
    assert_eq!(cpu.regs.si(), 0x6666);
    assert_eq!(cpu.regs.di(), 0x7777);
    assert_eq!(cpu.regs.sp(), 0x8000);
}

// ========================================================================
// LEA (0x8D)
// ========================================================================

#[test]
fn test_lea_r16_mem() {
    // LEA AX, [BX+SI+5] = 8D 40 05
    let (mut cpu, mut mem) = setup(&[0x8D, 0x40, 0x05]);
    cpu.regs.ebx = 0x0100;
    cpu.regs.esi = 0x0010;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // LEA computes effective address without accessing memory
    assert_eq!(cpu.regs.ax(), 0x0115);
}

#[test]
fn test_lea_register_form_raises_ud_without_commit() {
    let (mut cpu, mut mem) = setup(&[0x8D, 0xC0]);
    cpu.regs.eax = 0x1234_5678;

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(matches!(result, RunResult::Exception { vector: 6, .. }));
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.eax, 0x1234_5678);
}

// ========================================================================
// CBW / CWD / CWDE / CDQ
// ========================================================================

#[test]
fn test_cbw_positive() {
    // CBW = 0x98 — sign-extend AL into AX
    let (mut cpu, mut mem) = setup(&[0x98]);
    cpu.regs.set_al(0x42);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0042);
}

#[test]
fn test_cbw_negative() {
    let (mut cpu, mut mem) = setup(&[0x98]);
    cpu.regs.set_al(0x80);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xFF80);
}

#[test]
fn test_cwd_positive() {
    // CWD = 0x99 — sign-extend AX into DX:AX
    let (mut cpu, mut mem) = setup(&[0x99]);
    cpu.regs.set_ax(0x1234);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.dx(), 0x0000);
}

#[test]
fn test_cwd_negative() {
    let (mut cpu, mut mem) = setup(&[0x99]);
    cpu.regs.set_ax(0x8000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.dx(), 0xFFFF);
}

// ========================================================================
// MOV segment register
// ========================================================================

#[test]
fn test_mov_es_ax() {
    // MOV ES, AX = 8E C0 (modrm: mod=11, reg=ES(000), rm=AX(000))
    let (mut cpu, mut mem) = setup(&[0x8E, 0xC0]);
    cpu.regs.set_ax(0x0050);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.es, 0x0050);
}

#[test]
fn test_mov_ax_es() {
    // MOV AX, ES = 8C C0 (modrm: mod=11, reg=ES(000), rm=AX(000))
    let (mut cpu, mut mem) = setup(&[0x8C, 0xC0]);
    cpu.regs.es = 0x0050;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0050);
}

#[test]
fn test_mov_ds_bx() {
    // MOV DS, BX = 8E DB (mod=11, reg=DS(011), rm=BX(011))
    let (mut cpu, mut mem) = setup(&[0x8E, 0xDB]);
    cpu.regs.set_bx(0x0060);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ds, 0x0060);
}

// ========================================================================
// PUSHF / POPF
// ========================================================================

#[test]
fn test_pushf_popf_roundtrip() {
    // PUSHF POPF = 9C 9D
    let (mut cpu, mut mem) = setup(&[0x9C, 0x9D]);
    cpu.regs.esp = 0x8000;
    cpu.regs.flags.insert(Flags::CARRY);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_real_mode_pushf_popf_cpu_detection_high_nibble() {
    for (cpu_type, stack_flags, expected_high) in [
        (gp64_cpu::CpuType::I8086, 0x0002, 0xF000),
        (gp64_cpu::CpuType::I8088, 0x0002, 0xF000),
        (gp64_cpu::CpuType::I80186, 0x0002, 0xF000),
        (gp64_cpu::CpuType::I80286, 0xF002, 0x0000),
    ] {
        // POPF; PUSHF; POP AX mirrors classic BIOS CPU detection. 8086-class
        // CPUs read FLAGS bits 12-15 as one; 286 real mode reads them as zero.
        let (mut cpu, mut mem) = setup(&[0x9D, 0x9C, 0x58]);
        cpu.cpu_type = cpu_type;
        cpu.regs.esp = 0x8000;
        mem.write_u16(0x8000, stack_flags);

        let mut io = ();
        let result = run(&mut cpu, &mut mem, &mut io, 3);

        assert!(
            matches!(result, RunResult::BudgetExhausted { executed: 3 }),
            "{cpu_type:?} detector sequence should execute, got {result:?}"
        );
        assert_eq!(
            cpu.regs.ax() & 0xF000,
            expected_high,
            "{cpu_type:?} real-mode FLAGS high nibble"
        );
    }
}

// ========================================================================
// SAHF / LAHF
// ========================================================================

#[test]
fn test_sahf() {
    // SAHF = 0x9E — loads AH into flags (SF, ZF, AF, PF, CF)
    let (mut cpu, mut mem) = setup(&[0x9E]);
    // Set AH with CF=1, PF=1, ZF=1, SF=1 (bits 0,2,6,7)
    cpu.regs.set_ah(0xC7); // bits: SF=1, ZF=1, AF=0, PF=1, CF=1
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
    assert!(cpu.regs.flags.contains(Flags::PARITY));
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert!(cpu.regs.flags.contains(Flags::SIGN));
}

#[test]
fn test_sahf_forces_reserved_bit_one_and_clears_reserved_inputs() {
    let (mut cpu, mut mem) = setup(&[0x9E]);
    cpu.regs.flags = Flags::TRAP
        | Flags::INTERRUPT
        | Flags::DIRECTION
        | Flags::OVERFLOW
        | Flags::IOPL0
        | Flags::IOPL1
        | Flags::NESTED;
    cpu.regs.set_ah(0x28); // Only reserved AH bits 3 and 5 are set.
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);

    let preserved = Flags::TRAP
        | Flags::INTERRUPT
        | Flags::DIRECTION
        | Flags::OVERFLOW
        | Flags::IOPL0
        | Flags::IOPL1
        | Flags::NESTED
        | Flags::RESERVED1;
    assert_eq!(cpu.regs.flags, preserved);
}

#[test]
fn test_lahf() {
    // LAHF = 0x9F — loads flags into AH
    let (mut cpu, mut mem) = setup(&[0x9F]);
    cpu.regs.flags.insert(Flags::CARRY);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let ah = cpu.regs.ah();
    assert!(ah & 0x01 != 0, "CF should be in AH bit 0");
    assert!(ah & 0x40 != 0, "ZF should be in AH bit 6");
}

// ========================================================================
// CLC / STC / CMC / CLD / STD / CLI / STI
// ========================================================================

#[test]
fn test_clc() {
    // CLC = 0xF8
    let (mut cpu, mut mem) = setup(&[0xF8]);
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_stc() {
    // STC = 0xF9
    let (mut cpu, mut mem) = setup(&[0xF9]);
    cpu.regs.flags.remove(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_cmc() {
    // CMC = 0xF5 — complement carry flag
    let (mut cpu, mut mem) = setup(&[0xF5]);
    cpu.regs.flags.remove(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_cld() {
    // CLD = 0xFC
    let (mut cpu, mut mem) = setup(&[0xFC]);
    cpu.regs.flags.insert(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::DIRECTION));
}

#[test]
fn test_std() {
    // STD = 0xFD
    let (mut cpu, mut mem) = setup(&[0xFD]);
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::DIRECTION));
}

#[test]
fn test_cli() {
    // CLI = 0xFA
    let (mut cpu, mut mem) = setup(&[0xFA]);
    cpu.regs.flags.insert(Flags::INTERRUPT);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::INTERRUPT));
}

#[test]
fn test_sti() {
    // STI = 0xFB
    let (mut cpu, mut mem) = setup(&[0xFB]);
    cpu.regs.flags.remove(Flags::INTERRUPT);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::INTERRUPT));
}

// ========================================================================
// NOT / NEG
// ========================================================================

#[test]
fn test_not_r8() {
    // NOT AL = F6 D0
    let (mut cpu, mut mem) = setup(&[0xF6, 0xD0]);
    cpu.regs.set_al(0x55);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xAA);
}

#[test]
fn test_not_r16() {
    // NOT AX = F7 D0
    let (mut cpu, mut mem) = setup(&[0xF7, 0xD0]);
    cpu.regs.set_ax(0x00FF);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xFF00);
}

#[test]
fn test_neg_r8() {
    // NEG AL = F6 D8
    let (mut cpu, mut mem) = setup(&[0xF6, 0xD8]);
    cpu.regs.set_al(0x01);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xFF); // -1
    assert!(cpu.regs.flags.contains(Flags::CARRY)); // CF=1 for non-zero
}

#[test]
fn test_neg_r8_zero() {
    // NEG AL when AL=0
    let (mut cpu, mut mem) = setup(&[0xF6, 0xD8]);
    cpu.regs.set_al(0x00);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x00);
    assert!(!cpu.regs.flags.contains(Flags::CARRY)); // CF=0 for zero
}

#[test]
fn test_neg_r16() {
    // NEG AX = F7 D8
    let (mut cpu, mut mem) = setup(&[0xF7, 0xD8]);
    cpu.regs.set_ax(0x0001);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xFFFF);
}

// ========================================================================
// MUL / IMUL / DIV / IDIV
// ========================================================================

#[test]
fn test_mul_r8() {
    // MUL CL = F6 E1 — AX = AL * CL
    let (mut cpu, mut mem) = setup(&[0xF6, 0xE1]);
    cpu.regs.set_al(10);
    cpu.regs.set_cl(20);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 200);
}

#[test]
fn test_mul_r16() {
    // MUL CX = F7 E1 — DX:AX = AX * CX
    let (mut cpu, mut mem) = setup(&[0xF7, 0xE1]);
    cpu.regs.set_ax(0x1000);
    cpu.regs.set_cx(0x0010);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.dx(), 0x0001);
    assert_eq!(cpu.regs.ax(), 0x0000);
}

#[test]
fn test_imul_r8() {
    // IMUL CL = F6 E9 — AX = AL * CL (signed)
    let (mut cpu, mut mem) = setup(&[0xF6, 0xE9]);
    cpu.regs.set_al(0xFE); // -2
    cpu.regs.set_cl(0x03); // 3
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xFFFA); // -6
}

#[test]
fn test_div_r8() {
    // DIV CL = F6 F1 — AL=AX/CL, AH=AX%CL
    let (mut cpu, mut mem) = setup(&[0xF6, 0xF1]);
    cpu.regs.set_ax(100);
    cpu.regs.set_cl(7);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 14); // 100 / 7
    assert_eq!(cpu.regs.ah(), 2); // 100 % 7
}

#[test]
fn test_div_by_zero_raises_de() {
    // DIV CL with CL=0 → exception
    let (mut cpu, mut mem) = setup(&[0xF6, 0xF1]);
    cpu.regs.set_ax(100);
    cpu.regs.set_cl(0);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::Exception { vector: 0, .. }));
}

#[test]
fn test_div_r16() {
    // DIV CX = F7 F1 — AX=DX:AX/CX, DX=DX:AX%CX
    let (mut cpu, mut mem) = setup(&[0xF7, 0xF1]);
    cpu.regs.set_dx(0);
    cpu.regs.set_ax(1000);
    cpu.regs.set_cx(7);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 142); // 1000 / 7
    assert_eq!(cpu.regs.dx(), 6); // 1000 % 7
}

// ========================================================================
// IMUL 3-operand (0x6B, 0x69)
// ========================================================================

#[test]
fn test_imul_r16_r16_imm8() {
    // IMUL AX, BX, 5 = 6B C3 05
    let (mut cpu, mut mem) = setup(&[0x6B, 0xC3, 0x05]);
    cpu.regs.set_bx(10);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 50);
}

#[test]
fn test_imul_r16_r16_imm16() {
    // IMUL AX, BX, 300 = 69 C3 2C 01
    let (mut cpu, mut mem) = setup(&[0x69, 0xC3, 0x2C, 0x01]);
    cpu.regs.set_bx(10);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 3000);
}

#[test]
fn test_imul_r16_rm16() {
    // IMUL AX, BX = 0F AF C3
    let (mut cpu, mut mem) = setup(&[0x0F, 0xAF, 0xC3]);
    cpu.regs.set_ax(7);
    cpu.regs.set_bx(8);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 56);
}

// ========================================================================
// TEST
// ========================================================================

#[test]
fn test_test_al_imm8() {
    // TEST AL, 0x80 = A8 80
    let (mut cpu, mut mem) = setup(&[0xA8, 0x80]);
    cpu.regs.set_al(0x80);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
    assert!(cpu.regs.flags.contains(Flags::SIGN));
}

#[test]
fn test_test_al_imm8_zero() {
    // TEST AL, 0x80 with AL=0x7F
    let (mut cpu, mut mem) = setup(&[0xA8, 0x80]);
    cpu.regs.set_al(0x7F);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_test_ax_imm16() {
    // TEST AX, 0x8000 = A9 00 80
    let (mut cpu, mut mem) = setup(&[0xA9, 0x00, 0x80]);
    cpu.regs.set_ax(0x8000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
    assert!(cpu.regs.flags.contains(Flags::SIGN));
}

#[test]
fn test_test_r8_r8() {
    // TEST AL, CL = 84 C1
    let (mut cpu, mut mem) = setup(&[0x84, 0xC1]);
    cpu.regs.set_al(0x0F);
    cpu.regs.set_cl(0xF0);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// Jcc (short jumps: 0x70-0x7F)
// ========================================================================

#[test]
fn test_jo_taken() {
    // JO +2 = 70 02 (skip 2 bytes)
    let (mut cpu, mut mem) = setup(&[0x70, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::OVERFLOW);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eip, 0x0104); // 0x102 + 2
}

#[test]
fn test_jno_taken() {
    let (mut cpu, mut mem) = setup(&[0x71, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jb_taken() {
    let (mut cpu, mut mem) = setup(&[0x72, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jnb_taken() {
    let (mut cpu, mut mem) = setup(&[0x73, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jz_taken() {
    let (mut cpu, mut mem) = setup(&[0x74, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jnz_taken() {
    let (mut cpu, mut mem) = setup(&[0x75, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jbe_taken() {
    let (mut cpu, mut mem) = setup(&[0x76, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_ja_taken() {
    let (mut cpu, mut mem) = setup(&[0x77, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.flags.remove(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_js_taken() {
    let (mut cpu, mut mem) = setup(&[0x78, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::SIGN);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jns_taken() {
    let (mut cpu, mut mem) = setup(&[0x79, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::SIGN);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jp_taken() {
    let (mut cpu, mut mem) = setup(&[0x7A, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::PARITY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jnp_taken() {
    let (mut cpu, mut mem) = setup(&[0x7B, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::PARITY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jl_taken() {
    // JL = SF != OF
    let (mut cpu, mut mem) = setup(&[0x7C, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::SIGN);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jge_taken() {
    // JGE = SF == OF
    let (mut cpu, mut mem) = setup(&[0x7D, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::SIGN);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jle_taken() {
    // JLE = ZF=1 or SF!=OF
    let (mut cpu, mut mem) = setup(&[0x7E, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jg_taken() {
    // JG = ZF=0 and SF==OF
    let (mut cpu, mut mem) = setup(&[0x7F, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::ZERO);
    cpu.regs.flags.remove(Flags::SIGN);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jcc_not_taken() {
    // JZ +2 with ZF=0 should NOT jump
    let (mut cpu, mut mem) = setup(&[0x74, 0x02, 0x90]);
    cpu.regs.flags.remove(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0102); // fell through
}

// ========================================================================
// LOOP / LOOPZ / LOOPNZ / JCXZ
// ========================================================================

#[test]
fn test_loop_basic() {
    // LOOP -2 = E2 FE — decrement CX, jump if CX!=0
    // Place: NOP at 0x100, LOOP back to 0x100 at 0x101
    let (mut cpu, mut mem) = setup(&[0x90, 0xE2, 0xFD]);
    cpu.regs.ecx = 3;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 10);
    // After 3 iterations, CX=0 and falls through
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn test_jcxz_taken() {
    // JCXZ +2 = E3 02
    let (mut cpu, mut mem) = setup(&[0xE3, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.ecx = 0;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jcxz_not_taken() {
    let (mut cpu, mut mem) = setup(&[0xE3, 0x02, 0x90]);
    cpu.regs.ecx = 1;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0102);
}

// ========================================================================
// JMP short / near
// ========================================================================

#[test]
fn test_jmp_short() {
    // JMP +3 = EB 03
    let (mut cpu, mut mem) = setup(&[0xEB, 0x03, 0x90, 0x90, 0x90, 0xF4]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0105);
}

#[test]
fn test_jmp_near() {
    // JMP 0x200 = E9 FD 00 (rel16 = 0x200 - 0x103 = 0x00FD)
    let (mut cpu, mut mem) = setup(&[0xE9, 0xFD, 0x00]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0200);
}

// ========================================================================
// CALL near / RET
// ========================================================================

#[test]
fn test_call_near_ret() {
    // CALL +3 = E8 03 00, then at target: RET = C3
    // Code at 0x100: E8 03 00 90 90 90 C3
    let (mut cpu, mut mem) = setup(&[0xE8, 0x03, 0x00, 0x90, 0x90, 0x90, 0xC3]);
    cpu.regs.esp = 0x8000;
    let mut io = ();
    // Execute CALL + RET
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eip, 0x0103); // returned to after CALL
    assert_eq!(cpu.regs.sp(), 0x8000);
}

// ========================================================================
// HLT
// ========================================================================

#[test]
fn test_hlt() {
    // HLT = 0xF4
    let (mut cpu, mut mem) = setup(&[0xF4]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::Halt { .. }));
}

#[test]
fn test_hlt_at_ffff_retires_to_10000() {
    let (mut cpu, mut mem) = setup(&[]);
    cpu.regs.set_eip(0xFFFF);
    mem.ram[0xFFFF] = 0xF4;

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(matches!(result, RunResult::Halt { .. }));
    assert_eq!(cpu.regs.eip, 0x1_0000);
}

// ========================================================================
// INT
// ========================================================================

#[test]
fn test_int_21h() {
    // INT 21h = CD 21
    let (mut cpu, mut mem) = setup(&[0xCD, 0x21]);
    cpu.regs.esp = 0x8000;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(
        result,
        RunResult::SoftwareInterrupt { vector: 0x21, .. }
    ));
}

// ========================================================================
// MOV AL/AX, moffs (0xA0, 0xA1) and MOV moffs, AL/AX (0xA2, 0xA3)
// ========================================================================

#[test]
fn test_mov_al_moffs() {
    // MOV AL, [0x0200] = A0 00 02
    let (mut cpu, mut mem) = setup(&[0xA0, 0x00, 0x02]);
    mem.ram[0x0200] = 0xAB;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xAB);
}

#[test]
fn test_mov_ax_moffs() {
    // MOV AX, [0x0200] = A1 00 02
    let (mut cpu, mut mem) = setup(&[0xA1, 0x00, 0x02]);
    mem.ram[0x0200] = 0x34;
    mem.ram[0x0201] = 0x12;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x1234);
}

#[test]
fn test_mov_moffs_al() {
    // MOV [0x0200], AL = A2 00 02
    let (mut cpu, mut mem) = setup(&[0xA2, 0x00, 0x02]);
    cpu.regs.set_al(0xCD);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x0200], 0xCD);
}

#[test]
fn test_mov_moffs_ax() {
    // MOV [0x0200], AX = A3 00 02
    let (mut cpu, mut mem) = setup(&[0xA3, 0x00, 0x02]);
    cpu.regs.set_ax(0xBEEF);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u16::from_le_bytes([mem.ram[0x0200], mem.ram[0x0201]]),
        0xBEEF
    );
}

// ========================================================================
// ADC / SBB
// ========================================================================

#[test]
fn test_adc_r8_with_carry() {
    // ADC AL, 0 = 14 00 — adds CF to AL
    let (mut cpu, mut mem) = setup(&[0x14, 0x00]);
    cpu.regs.set_al(0xFF);
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x00);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_sbb_r8_with_borrow() {
    // SBB AL, 0 = 1C 00 — subtracts CF from AL
    let (mut cpu, mut mem) = setup(&[0x1C, 0x00]);
    cpu.regs.set_al(0x00);
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xFF);
}

// ========================================================================
// Group 1 (80-83) with various operations
// ========================================================================

#[test]
fn test_group1_add_rm8_imm8() {
    // ADD AL, 5 = 80 C0 05
    let (mut cpu, mut mem) = setup(&[0x80, 0xC0, 0x05]);
    cpu.regs.set_al(10);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 15);
}

#[test]
fn test_group1_or_rm8_imm8() {
    // OR AL, 0xF0 = 80 C8 F0
    let (mut cpu, mut mem) = setup(&[0x80, 0xC8, 0xF0]);
    cpu.regs.set_al(0x0F);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xFF);
}

#[test]
fn test_group1_and_rm16_imm16() {
    // AND AX, 0xFF00 = 81 E0 00 FF
    let (mut cpu, mut mem) = setup(&[0x81, 0xE0, 0x00, 0xFF]);
    cpu.regs.set_ax(0x1234);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x1200);
}

#[test]
fn test_group1_sub_rm16_imm8_sign_ext() {
    // SUB AX, -1 = 83 E8 FF (sign-extend 0xFF to 0xFFFF = -1)
    let (mut cpu, mut mem) = setup(&[0x83, 0xE8, 0xFF]);
    cpu.regs.set_ax(10);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 11); // 10 - (-1) = 11
}

#[test]
fn test_group1_xor_rm8_imm8() {
    // XOR AL, 0xFF = 80 F0 FF
    let (mut cpu, mut mem) = setup(&[0x80, 0xF0, 0xFF]);
    cpu.regs.set_al(0x55);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xAA);
}

#[test]
fn test_group1_cmp_rm8_imm8() {
    // CMP AL, 5 = 80 F8 05
    let (mut cpu, mut mem) = setup(&[0x80, 0xF8, 0x05]);
    cpu.regs.set_al(5);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.al(), 5); // unchanged
}

// Encode CMP r/m32, imm32 in 16-bit addressing with an ES override.
// The ModRM /7 extension selects CMP, and r/m=7 selects [BX].
fn synthetic_cmp_es_bx(immediate: u32, displacement: Option<u8>) -> Vec<u8> {
    let addressing_mode = u8::from(displacement.is_some());
    let modrm = (addressing_mode << 6) | (7 << 3) | 7;
    let mut instruction = vec![0x66, 0x26, 0x81, modrm];
    if let Some(displacement) = displacement {
        instruction.push(displacement);
    }
    instruction.extend_from_slice(&immediate.to_le_bytes());
    instruction
}

// A dword comparison with an ES segment override in 16-bit real mode
// must read through ES and set ZF when the operands match.
#[test]
fn test_seg_override_cmp_rm32_imm32_real_mode() {
    const IMMEDIATE: u32 = 0xA6C3_591E;
    const SEGMENT: u16 = 0x3400;
    const OFFSET: u16 = 0x0120;
    let instruction = synthetic_cmp_es_bx(IMMEDIATE, None);
    let (mut cpu, mut mem) = setup(&instruction);
    cpu.set_es_real_mode(SEGMENT);
    cpu.regs.set_bx(OFFSET);
    let address = (usize::from(SEGMENT) << 4) + usize::from(OFFSET);
    mem.ram[address..address + 4].copy_from_slice(&IMMEDIATE.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        cpu.regs.flags.contains(Flags::ZERO),
        "ES-override dword CMP must set ZF for equal operands (ip now {:04X})",
        cpu.regs.ip()
    );
}

// After a host handler changes ES, its descriptor cache must be refreshed.
// The synchronized cache must select the matching operand in the new segment
// instead of the deliberately unequal operand in the old segment.
#[test]
fn test_seg_override_cmp_es_cache_base_tracks_selector() {
    const IMMEDIATE: u32 = 0xB7D2_680F;
    const OLD_SEGMENT: u16 = 0x2400;
    const NEW_SEGMENT: u16 = 0x3500;
    const OFFSET: u16 = 0x0130;
    let instruction = synthetic_cmp_es_bx(IMMEDIATE, None);
    let (mut cpu, mut mem) = setup(&instruction);
    cpu.regs.ds = OLD_SEGMENT;
    cpu.regs.ds_cache = gp64_cpu::SegmentCache::real_mode(OLD_SEGMENT);
    cpu.set_es_real_mode(OLD_SEGMENT);
    cpu.regs.set_bx(OFFSET);
    let old_address = (usize::from(OLD_SEGMENT) << 4) + usize::from(OFFSET);
    mem.ram[old_address..old_address + 4].copy_from_slice(&(!IMMEDIATE).to_le_bytes());
    cpu.set_es_real_mode(NEW_SEGMENT);
    let new_address = (usize::from(NEW_SEGMENT) << 4) + usize::from(OFFSET);
    mem.ram[new_address..new_address + 4].copy_from_slice(&IMMEDIATE.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        cpu.regs.flags.contains(Flags::ZERO),
        "ES-override CMP must read the new segment; the cache must track its selector"
    );
}

// An ES-override dword comparison with a disp8 ModRM must include the
// displacement when computing its effective address.
#[test]
fn test_seg_override_cmp_rm32_imm32_disp8_real_mode() {
    const IMMEDIATE: u32 = 0xC8E1_7B20;
    const SEGMENT: u16 = 0x3600;
    const OFFSET: u16 = 0x0140;
    const DISPLACEMENT: u8 = 12;
    let instruction = synthetic_cmp_es_bx(IMMEDIATE, Some(DISPLACEMENT));
    let (mut cpu, mut mem) = setup(&instruction);
    cpu.set_es_real_mode(SEGMENT);
    cpu.regs.set_bx(OFFSET);
    let base = (usize::from(SEGMENT) << 4) + usize::from(OFFSET);
    mem.ram[base..base + 4].copy_from_slice(&(!IMMEDIATE).to_le_bytes());
    let address = base + usize::from(DISPLACEMENT);
    mem.ram[address..address + 4].copy_from_slice(&IMMEDIATE.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        cpu.regs.flags.contains(Flags::ZERO),
        "ES-override displaced dword CMP must set ZF for equal operands (ip now {:04X})",
        cpu.regs.ip()
    );
}

// ========================================================================
// MOV r8, r/m8 and MOV r/m8, r8
// ========================================================================

#[test]
fn test_mov_r8_r8() {
    // MOV AL, CL = 8A C1
    let (mut cpu, mut mem) = setup(&[0x8A, 0xC1]);
    cpu.regs.set_cl(0xAB);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xAB);
}

#[test]
fn test_mov_rm8_r8() {
    // MOV CL, AL = 88 C1
    let (mut cpu, mut mem) = setup(&[0x88, 0xC1]);
    cpu.regs.set_al(0xCD);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.cl(), 0xCD);
}

#[test]
fn test_mov_r8_mem() {
    // MOV AL, [0x0200] = 8A 06 00 02
    let (mut cpu, mut mem) = setup(&[0x8A, 0x06, 0x00, 0x02]);
    mem.ram[0x0200] = 0xEE;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xEE);
}

#[test]
fn test_mov_mem_r8() {
    // MOV [0x0200], AL = 88 06 00 02
    let (mut cpu, mut mem) = setup(&[0x88, 0x06, 0x00, 0x02]);
    cpu.regs.set_al(0xDD);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x0200], 0xDD);
}

// ========================================================================
// XLAT (0xD7)
// ========================================================================

#[test]
fn test_xlat() {
    // XLAT = D7 — AL = [BX + AL]
    let (mut cpu, mut mem) = setup(&[0xD7]);
    cpu.regs.ebx = 0x0300;
    cpu.regs.set_al(5);
    mem.ram[0x0305] = 0xAA;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xAA);
}

// ========================================================================
// MOV r/m, imm (0xC6, 0xC7)
// ========================================================================

#[test]
fn test_mov_rm8_imm8() {
    // MOV AL, 0x42 = C6 C0 42
    let (mut cpu, mut mem) = setup(&[0xC6, 0xC0, 0x42]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x42);
}

#[test]
fn test_mov_rm16_imm16() {
    // MOV AX, 0x1234 = C7 C0 34 12
    let (mut cpu, mut mem) = setup(&[0xC7, 0xC0, 0x34, 0x12]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x1234);
}

#[test]
fn test_mov_mem8_imm8() {
    // MOV [0x0200], 0x42 = C6 06 00 02 42
    let (mut cpu, mut mem) = setup(&[0xC6, 0x06, 0x00, 0x02, 0x42]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x0200], 0x42);
}

#[test]
fn test_mov_mem16_imm16() {
    // MOV [0x0200], 0x1234 = C7 06 00 02 34 12
    let (mut cpu, mut mem) = setup(&[0xC7, 0x06, 0x00, 0x02, 0x34, 0x12]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u16::from_le_bytes([mem.ram[0x0200], mem.ram[0x0201]]),
        0x1234
    );
}

// ========================================================================
// SHLD / SHRD
// ========================================================================

#[test]
fn test_shld_basic() {
    // SHLD AX, BX, 4 = 0F A4 D8 04
    let (mut cpu, mut mem) = setup(&[0x0F, 0xA4, 0xD8, 0x04]);
    cpu.regs.set_ax(0x1234);
    cpu.regs.set_bx(0x5678);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // SHLD shifts AX left by 4, filling with top bits of BX
    assert_eq!(cpu.regs.ax(), 0x2345);
}

#[test]
fn test_shrd_basic() {
    // SHRD AX, BX, 4 = 0F AC D8 04
    let (mut cpu, mut mem) = setup(&[0x0F, 0xAC, 0xD8, 0x04]);
    cpu.regs.set_ax(0x1234);
    cpu.regs.set_bx(0x5678);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // SHRD shifts AX right by 4, filling with bottom bits of BX
    assert_eq!(cpu.regs.ax(), 0x8123);
}

#[test]
fn test_shld_cl() {
    // SHLD AX, BX, CL = 0F A5 D8
    let (mut cpu, mut mem) = setup(&[0x0F, 0xA5, 0xD8]);
    cpu.regs.set_ax(0xFF00);
    cpu.regs.set_bx(0x00FF);
    cpu.regs.set_cl(8);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0000);
}

#[test]
fn test_shrd_cl() {
    // SHRD AX, BX, CL = 0F AD D8
    let (mut cpu, mut mem) = setup(&[0x0F, 0xAD, 0xD8]);
    cpu.regs.set_ax(0x00FF);
    cpu.regs.set_bx(0xFF00);
    cpu.regs.set_cl(8);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0000);
}

// ========================================================================
// Group 3: TEST/NOT/NEG/MUL/IMUL/DIV/IDIV with memory operands
// ========================================================================

#[test]
fn test_not_mem8() {
    // NOT [0x0200] = F6 16 00 02 (wait, NOT is /2 = F6 mod=00 010 rm=110 = 0x16)
    let (mut cpu, mut mem) = setup(&[0xF6, 0x16, 0x00, 0x02]);
    mem.ram[0x0200] = 0x55;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x0200], 0xAA);
}

#[test]
fn test_neg_mem8() {
    // NEG [0x0200] = F6 1E 00 02 (NEG is /3)
    let (mut cpu, mut mem) = setup(&[0xF6, 0x1E, 0x00, 0x02]);
    mem.ram[0x0200] = 0x01;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x0200], 0xFF);
}

#[test]
fn test_test_rm8_imm8() {
    // TEST [0x0200], 0x80 = F6 06 00 02 80
    let (mut cpu, mut mem) = setup(&[0xF6, 0x06, 0x00, 0x02, 0x80]);
    mem.ram[0x0200] = 0x80;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
    assert!(cpu.regs.flags.contains(Flags::SIGN));
}

// ========================================================================
// INC/DEC r/m8 (0xFE) and INC/DEC r/m16 (0xFF)
// ========================================================================

#[test]
fn test_inc_rm8() {
    // INC AL = FE C0
    let (mut cpu, mut mem) = setup(&[0xFE, 0xC0]);
    cpu.regs.set_al(0xFF);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x00);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_dec_rm8() {
    // DEC AL = FE C8
    let (mut cpu, mut mem) = setup(&[0xFE, 0xC8]);
    cpu.regs.set_al(0x01);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x00);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_inc_mem8() {
    // INC [0x0200] = FE 06 00 02
    let (mut cpu, mut mem) = setup(&[0xFE, 0x06, 0x00, 0x02]);
    mem.ram[0x0200] = 0x41;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x0200], 0x42);
}

#[test]
fn test_inc_rm16() {
    // INC AX = FF C0
    let (mut cpu, mut mem) = setup(&[0xFF, 0xC0]);
    cpu.regs.set_ax(0xFFFF);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0000);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_dec_rm16() {
    // DEC AX = FF C8
    let (mut cpu, mut mem) = setup(&[0xFF, 0xC8]);
    cpu.regs.set_ax(0x0001);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0000);
}

// ========================================================================
// LES / LDS
// ========================================================================

#[test]
fn test_les_basic() {
    // LES AX, [0x0300] = C4 06 00 03
    let (mut cpu, mut mem) = setup(&[0xC4, 0x06, 0x00, 0x03]);
    mem.ram[0x0300] = 0x34;
    mem.ram[0x0301] = 0x12;
    mem.ram[0x0302] = 0x78;
    mem.ram[0x0303] = 0x56;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x1234);
    assert_eq!(cpu.regs.es, 0x5678);
}

#[test]
fn test_lds_basic() {
    // LDS AX, [0x0300] = C5 06 00 03
    let (mut cpu, mut mem) = setup(&[0xC5, 0x06, 0x00, 0x03]);
    mem.ram[0x0300] = 0x34;
    mem.ram[0x0301] = 0x12;
    mem.ram[0x0302] = 0x78;
    mem.ram[0x0303] = 0x56;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x1234);
    assert_eq!(cpu.regs.ds, 0x5678);
}

// ========================================================================
// CMOVcc additional conditions
// ========================================================================

#[test]
fn test_cmova_true_condition_is_ud_on_pentium() {
    // CMOVA AX, BX = 0F 47 C3 (CF=0 and ZF=0)
    let (mut cpu, mut mem) = setup(&[0x0F, 0x47, 0xC3]);
    cpu.cpu_type = gp64_cpu::CpuType::Pentium;
    cpu.regs.set_ax(0x1111);
    cpu.regs.set_bx(0x2222);
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.flags.remove(Flags::ZERO);
    let saved_flags = cpu.regs.flags;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 6,
            error_code: 0,
            fault_addr: None,
            executed: 0
        }
    ));
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.ax(), 0x1111);
    assert_eq!(cpu.regs.bx(), 0x2222);
    assert_eq!(cpu.regs.flags, saved_flags);
}

#[test]
fn test_cmova_false_condition_is_still_ud_on_pentium() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x47, 0xC3]);
    cpu.cpu_type = gp64_cpu::CpuType::Pentium;
    cpu.regs.set_ax(0x1111);
    cpu.regs.set_bx(0x2222);
    cpu.regs.flags.insert(Flags::CARRY);
    let saved_flags = cpu.regs.flags;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 6,
            error_code: 0,
            fault_addr: None,
            executed: 0
        }
    ));
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.ax(), 0x1111);
    assert_eq!(cpu.regs.bx(), 0x2222);
    assert_eq!(cpu.regs.flags, saved_flags);
}

#[test]
fn test_cmovs_true_condition_is_ud_on_pentium() {
    // CMOVS AX, BX = 0F 48 C3
    let (mut cpu, mut mem) = setup(&[0x0F, 0x48, 0xC3]);
    cpu.cpu_type = gp64_cpu::CpuType::Pentium;
    cpu.regs.set_ax(0x1111);
    cpu.regs.set_bx(0x2222);
    cpu.regs.flags.insert(Flags::SIGN);
    let saved_flags = cpu.regs.flags;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 6,
            error_code: 0,
            fault_addr: None,
            executed: 0
        }
    ));
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.ax(), 0x1111);
    assert_eq!(cpu.regs.bx(), 0x2222);
    assert_eq!(cpu.regs.flags, saved_flags);
}

#[test]
fn test_cmovl_true_condition_is_ud_on_pentium() {
    // CMOVL AX, BX = 0F 4C C3 (SF != OF)
    let (mut cpu, mut mem) = setup(&[0x0F, 0x4C, 0xC3]);
    cpu.cpu_type = gp64_cpu::CpuType::Pentium;
    cpu.regs.set_ax(0x1111);
    cpu.regs.set_bx(0x2222);
    cpu.regs.flags.insert(Flags::SIGN);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let saved_flags = cpu.regs.flags;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 6,
            error_code: 0,
            fault_addr: None,
            executed: 0
        }
    ));
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.ax(), 0x1111);
    assert_eq!(cpu.regs.bx(), 0x2222);
    assert_eq!(cpu.regs.flags, saved_flags);
}

#[test]
fn test_cmovle_true_condition_is_ud_on_pentium() {
    // CMOVLE AX, BX = 0F 4E C3 (ZF=1 or SF!=OF)
    let (mut cpu, mut mem) = setup(&[0x0F, 0x4E, 0xC3]);
    cpu.cpu_type = gp64_cpu::CpuType::Pentium;
    cpu.regs.set_ax(0x1111);
    cpu.regs.set_bx(0x2222);
    cpu.regs.flags.insert(Flags::ZERO);
    let saved_flags = cpu.regs.flags;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 6,
            error_code: 0,
            fault_addr: None,
            executed: 0
        }
    ));
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.ax(), 0x1111);
    assert_eq!(cpu.regs.bx(), 0x2222);
    assert_eq!(cpu.regs.flags, saved_flags);
}

// ========================================================================
// REP STOSD with operand size prefix
// ========================================================================

#[test]
fn test_rep_stosd_fills_dwords() {
    // REP STOSD = F3 66 AB
    let (mut cpu, mut mem) = setup(&[0xF3, 0x66, 0xAB]);
    cpu.regs.eax = 0xDEADBEEF;
    cpu.regs.edi = 0x0300;
    cpu.regs.ecx = 2;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 100);
    for i in 0..2 {
        let addr = 0x0300 + i * 4;
        let val = u32::from_le_bytes([
            mem.ram[addr],
            mem.ram[addr + 1],
            mem.ram[addr + 2],
            mem.ram[addr + 3],
        ]);
        assert_eq!(val, 0xDEADBEEF, "dword {} not filled", i);
    }
    assert_eq!(cpu.regs.ecx as u16, 0);
}

// ========================================================================
// SCASD / CMPSD with operand prefix
// ========================================================================

#[test]
fn test_scasd_equal() {
    // SCASD = 66 AF
    let (mut cpu, mut mem) = setup(&[0x66, 0xAF]);
    cpu.regs.eax = 0x12345678;
    mem.ram[0x0300] = 0x78;
    mem.ram[0x0301] = 0x56;
    mem.ram[0x0302] = 0x34;
    mem.ram[0x0303] = 0x12;
    cpu.regs.edi = 0x0300;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.edi as u16, 0x0304);
}

// ========================================================================
// LOCK prefix
// ========================================================================

#[test]
fn test_lock_add_mem8_valid() {
    // LOCK ADD [0x0200], 1 = F0 80 06 00 02 01
    let (mut cpu, mut mem) = setup(&[0xF0, 0x80, 0x06, 0x00, 0x02, 0x01]);
    mem.ram[0x0200] = 10;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x0200], 11);
}

#[test]
fn test_lock_or_mem8_valid() {
    // LOCK OR [0x0200], 0xF0 = F0 80 0E 00 02 F0
    let (mut cpu, mut mem) = setup(&[0xF0, 0x80, 0x0E, 0x00, 0x02, 0xF0]);
    mem.ram[0x0200] = 0x0F;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x0200], 0xFF);
}

// ========================================================================
// XADD (0F C0 / 0F C1)
// ========================================================================

#[test]
fn test_xadd_r8_r8() {
    // XADD CL, AL: modrm = 0xC1 (reg=AL(0), rm=CL(1))
    // CL := CL + AL, AL := old CL
    let (mut cpu, mut mem) = setup(&[0x0F, 0xC0, 0xC1]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    cpu.regs.set_al(5);
    cpu.regs.set_cl(10);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.cl(), 15); // sum
    assert_eq!(cpu.regs.al(), 10); // old dest
}

#[test]
fn test_xadd_r16_r16() {
    // XADD CX, AX: modrm = 0xC1 (reg=AX(0), rm=CX(1))
    let (mut cpu, mut mem) = setup(&[0x0F, 0xC1, 0xC1]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    cpu.regs.set_ax(100);
    cpu.regs.set_cx(200);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.cx(), 300);
    assert_eq!(cpu.regs.ax(), 200);
}

// ========================================================================
// MOVZX / MOVSX r32 from r/m16
// ========================================================================

#[test]
fn test_movzx_r32_r16() {
    // 66 0F B7 C1 = MOVZX EAX, CX
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xB7, 0xC1]);
    cpu.regs.set_cx(0xFFFF);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x0000FFFF);
}

#[test]
fn test_movsx_r32_r16() {
    // 66 0F BF C1 = MOVSX EAX, CX
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xBF, 0xC1]);
    cpu.regs.set_cx(0x8000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFFFF8000);
}

#[test]
fn test_movsx_r16_rm16_addr32_preserves_upper_word() {
    // 67 0F BF 3C E6 = MOVSX DI, word [ESI]
    let (mut cpu, mut mem) = setup(&[0x67, 0x0F, 0xBF, 0x3C, 0xE6]);
    cpu.regs.esi = 0x0200;
    cpu.regs.edi = 0xC0B7_C476;
    mem.ram[0x0200] = 0xEF;
    mem.ram[0x0201] = 0xEA;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.edi, 0xC0B7_EAEF);
}

// ========================================================================
// CWDE / CDQ (32-bit via operand size prefix)
// ========================================================================

#[test]
fn test_cwde() {
    // 66 98 = CWDE — sign-extend AX into EAX
    let (mut cpu, mut mem) = setup(&[0x66, 0x98]);
    cpu.regs.set_ax(0x8000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFFFF8000);
}

#[test]
fn test_cdq() {
    // 66 99 = CDQ — sign-extend EAX into EDX:EAX
    let (mut cpu, mut mem) = setup(&[0x66, 0x99]);
    cpu.regs.eax = 0x80000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.edx, 0xFFFFFFFF);
}

// ========================================================================
// BSF / BSR with 32-bit operand size
// ========================================================================

#[test]
fn test_bsf_32bit() {
    // 66 0F BC C3 = BSF EAX, EBX
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xBC, 0xC3]);
    cpu.regs.ebx = 0x00100000; // bit 20
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 20);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_bsr_32bit() {
    // 66 0F BD C3 = BSR EAX, EBX
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xBD, 0xC3]);
    cpu.regs.ebx = 0x00100000; // bit 20
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 20);
}

// ========================================================================
// OR / AND / XOR AL,imm and AX,imm shortcuts
// ========================================================================

#[test]
fn test_or_al_imm8() {
    // OR AL, 0xF0 = 0C F0
    let (mut cpu, mut mem) = setup(&[0x0C, 0xF0]);
    cpu.regs.set_al(0x0F);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xFF);
}

#[test]
fn test_and_al_imm8() {
    // AND AL, 0xF0 = 24 F0
    let (mut cpu, mut mem) = setup(&[0x24, 0xF0]);
    cpu.regs.set_al(0xFF);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xF0);
}

#[test]
fn test_xor_al_imm8() {
    // XOR AL, 0xFF = 34 FF
    let (mut cpu, mut mem) = setup(&[0x34, 0xFF]);
    cpu.regs.set_al(0x55);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xAA);
}

#[test]
fn test_or_ax_imm16() {
    // OR AX, 0xFF00 = 0D 00 FF
    let (mut cpu, mut mem) = setup(&[0x0D, 0x00, 0xFF]);
    cpu.regs.set_ax(0x00FF);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xFFFF);
}

#[test]
fn test_and_ax_imm16() {
    // AND AX, 0xFF00 = 25 00 FF
    let (mut cpu, mut mem) = setup(&[0x25, 0x00, 0xFF]);
    cpu.regs.set_ax(0x1234);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x1200);
}

// ========================================================================
// ADD/SUB/CMP AL,imm8 and AX,imm16 shortcuts
// ========================================================================

#[test]
fn test_add_al_imm8() {
    // ADD AL, 5 = 04 05
    let (mut cpu, mut mem) = setup(&[0x04, 0x05]);
    cpu.regs.set_al(10);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 15);
}

#[test]
fn test_sub_al_imm8() {
    // SUB AL, 3 = 2C 03
    let (mut cpu, mut mem) = setup(&[0x2C, 0x03]);
    cpu.regs.set_al(10);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 7);
}

#[test]
fn test_cmp_al_imm8() {
    // CMP AL, 5 = 3C 05
    let (mut cpu, mut mem) = setup(&[0x3C, 0x05]);
    cpu.regs.set_al(5);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_add_ax_imm16() {
    // ADD AX, 0x0100 = 05 00 01
    let (mut cpu, mut mem) = setup(&[0x05, 0x00, 0x01]);
    cpu.regs.set_ax(0x0200);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0300);
}

#[test]
fn test_sub_ax_imm16() {
    // SUB AX, 0x0100 = 2D 00 01
    let (mut cpu, mut mem) = setup(&[0x2D, 0x00, 0x01]);
    cpu.regs.set_ax(0x0300);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0200);
}

// ========================================================================
// Shift/rotate with memory operand (D0/D1 with mod!=11)
// ========================================================================

#[test]
fn test_shl_mem8_1() {
    // SHL [0x0200], 1 = D0 26 00 02
    let (mut cpu, mut mem) = setup(&[0xD0, 0x26, 0x00, 0x02]);
    mem.ram[0x0200] = 0x40;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x0200], 0x80);
}

#[test]
fn test_shr_mem16_1() {
    // SHR [0x0200], 1 = D1 2E 00 02
    let (mut cpu, mut mem) = setup(&[0xD1, 0x2E, 0x00, 0x02]);
    mem.ram[0x0200] = 0x02;
    mem.ram[0x0201] = 0x00;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u16::from_le_bytes([mem.ram[0x0200], mem.ram[0x0201]]),
        0x0001
    );
}

// ========================================================================
// Protected mode instructions: SGDT, SIDT, SLDT, STR, SMSW, VERR, VERW
// ========================================================================

/// PM helper: set up a minimal GDT with ring-0 code/data at 0x08/0x10,
/// and a ring-3 writable data segment at 0x18.
fn setup_pm_ring0(code: &[u8]) -> (Cpu, Memory) {
    use super::test_helpers::{make_descriptor, write_gdt_entry};
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let gdt_base: u32 = 0x10000;
    let gdt_limit: u16 = 5 * 8 - 1;
    write_gdt_entry(&mut mem, gdt_base as usize, [0u8; 8]); // null
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x08,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x08),
    ); // ring-0 code 16-bit
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x10,
        make_descriptor(0, 0xFFFFF, 0x92, 0x08),
    ); // ring-0 data
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x18,
        make_descriptor(0, 0xFFFFF, 0xF2, 0x08),
    ); // ring-3 writable data
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x20,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x08),
    ); // ring-0 readable code
    cpu.regs.gdtr.base = gdt_base;
    cpu.regs.gdtr.limit = gdt_limit;
    cpu.regs.idtr.base = 0x20000;
    cpu.regs.idtr.limit = 0x07FF;
    cpu.regs.cr0 |= 1;
    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x08).unwrap();
    cpu.cs_bits = 16;
    cpu.regs.cpl = 0;
    cpu.regs.ss = 0x10;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.ds = 0x10;
    cpu.regs.ds_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.es = 0x10;
    cpu.regs.es_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.esp = 0x8000;
    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x2000 + i] = b;
    }
    (cpu, mem)
}

#[test]
fn test_sgdt_stores_gdt_info() {
    // SGDT [0x3000] = 0F 01 06 00 30 (modrm: mod=00, /0, rm=110 disp16)
    let (mut cpu, mut mem) = setup_pm_ring0(&[0x0F, 0x01, 0x06, 0x00, 0x30]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let limit = u16::from_le_bytes([mem.ram[0x3000], mem.ram[0x3001]]);
    let base = u32::from_le_bytes([
        mem.ram[0x3002],
        mem.ram[0x3003],
        mem.ram[0x3004],
        mem.ram[0x3005],
    ]);
    assert_eq!(limit, cpu.regs.gdtr.limit);
    assert_eq!(base, cpu.regs.gdtr.base);
}

#[test]
fn test_sidt_stores_idt_info() {
    // SIDT [0x3000] = 0F 01 0E 00 30 (modrm: mod=00, /1, rm=110 disp16)
    let (mut cpu, mut mem) = setup_pm_ring0(&[0x0F, 0x01, 0x0E, 0x00, 0x30]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let limit = u16::from_le_bytes([mem.ram[0x3000], mem.ram[0x3001]]);
    let base = u32::from_le_bytes([
        mem.ram[0x3002],
        mem.ram[0x3003],
        mem.ram[0x3004],
        mem.ram[0x3005],
    ]);
    assert_eq!(limit, cpu.regs.idtr.limit);
    assert_eq!(base, cpu.regs.idtr.base);
}

#[test]
fn test_sldt_register() {
    // SLDT AX = 0F 00 C0 (modrm: mod=11, /0, rm=AX)
    let (mut cpu, mut mem) = setup_pm_ring0(&[0x0F, 0x00, 0xC0]);
    cpu.regs.ldtr = 0x0028;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0028);
}

#[test]
fn test_str_register() {
    // STR AX = 0F 00 C8 (modrm: mod=11, /1, rm=AX)
    let (mut cpu, mut mem) = setup_pm_ring0(&[0x0F, 0x00, 0xC8]);
    cpu.regs.tr = 0x0030;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0030);
}

#[test]
fn test_smsw_register() {
    // SMSW AX = 0F 01 E0 (modrm: mod=11, /4, rm=AX)
    let (mut cpu, mut mem) = setup_pm_ring0(&[0x0F, 0x01, 0xE0]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), (cpu.regs.cr0 & 0xFFFF) as u16);
}

#[test]
fn test_verr_valid_data_segment() {
    // VERR AX = 0F 00 E0 (modrm: mod=11, /4, rm=AX)
    let (mut cpu, mut mem) = setup_pm_ring0(&[0x0F, 0x00, 0xE0]);
    cpu.regs.set_ax(0x10); // ring-0 data segment — readable
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        cpu.regs.flags.contains(Flags::ZERO),
        "data segment should be readable"
    );
}

#[test]
fn test_verr_null_selector() {
    // VERR AX with AX=0 → ZF=0
    let (mut cpu, mut mem) = setup_pm_ring0(&[0x0F, 0x00, 0xE0]);
    cpu.regs.set_ax(0);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        !cpu.regs.flags.contains(Flags::ZERO),
        "null selector not readable"
    );
}

#[test]
fn test_verw_writable_segment() {
    // VERW AX = 0F 00 E8 (modrm: mod=11, /5, rm=AX)
    let (mut cpu, mut mem) = setup_pm_ring0(&[0x0F, 0x00, 0xE8]);
    cpu.regs.set_ax(0x10); // ring-0 data, writable
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        cpu.regs.flags.contains(Flags::ZERO),
        "data segment should be writable"
    );
}

#[test]
fn test_verw_code_segment_not_writable() {
    // VERW AX with code selector — should fail
    let (mut cpu, mut mem) = setup_pm_ring0(&[0x0F, 0x00, 0xE8]);
    cpu.regs.set_ax(0x08); // code segment — not writable
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        !cpu.regs.flags.contains(Flags::ZERO),
        "code segment not writable"
    );
}

#[test]
fn test_verr_in_real_mode_sets_zf() {
    // VERR in real mode sets ZF=1 unconditionally
    let (mut cpu, mut mem) = setup(&[0x0F, 0x00, 0xE0]);
    cpu.regs.set_ax(0x10);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_verw_in_real_mode_sets_zf() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x00, 0xE8]);
    cpu.regs.set_ax(0x10);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// LAR / LSL
// ========================================================================

#[test]
fn test_lar_valid_selector() {
    // LAR AX, CX = 0F 02 C1 (modrm: mod=11, reg=AX, rm=CX)
    let (mut cpu, mut mem) = setup_pm_ring0(&[0x0F, 0x02, 0xC1]);
    cpu.regs.set_cx(0x10); // ring-0 data selector
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    // Access rights loaded into AX (masked to 00FFxx00 style)
    let ax = cpu.regs.ax();
    assert_ne!(ax, 0, "LAR should load access rights");
}

#[test]
fn test_lar_32_preserves_descriptor_limit_high_nibble() {
    use super::test_helpers::{make_descriptor, write_gdt_entry};

    // 66 LAR EAX, CX in a 16-bit code segment. Intel defines result bits
    // 19:16 as undefined; real x86 preserves the descriptor's limit nibble
    let (mut cpu, mut mem) = setup_pm_ring0(&[0x66, 0x0F, 0x02, 0xC1]);
    write_gdt_entry(
        &mut mem,
        cpu.regs.gdtr.base as usize + 0x10,
        make_descriptor(0, 0x1149F, 0x92, 0x04),
    );
    cpu.regs.set_cx(0x10);

    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);

    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.eax, 0x0041_9200);
}

#[test]
fn test_lar_null_selector() {
    let (mut cpu, mut mem) = setup_pm_ring0(&[0x0F, 0x02, 0xC1]);
    cpu.regs.set_cx(0); // null
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_lsl_valid_selector() {
    // LSL AX, CX = 0F 03 C1
    let (mut cpu, mut mem) = setup_pm_ring0(&[0x0F, 0x03, 0xC1]);
    cpu.regs.set_cx(0x10); // ring-0 data selector
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    // Limit loaded into AX
    let ax = cpu.regs.ax();
    assert_ne!(ax, 0, "LSL should load segment limit");
}

#[test]
fn test_lsl_null_selector() {
    let (mut cpu, mut mem) = setup_pm_ring0(&[0x0F, 0x03, 0xC1]);
    cpu.regs.set_cx(0);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// LLDT from register (ring-0 only)
// ========================================================================

#[test]
fn test_lldt_register() {
    // LLDT AX = 0F 00 D0 (modrm: mod=11, /2, rm=AX)
    let (mut cpu, mut mem) = setup_pm_ring0(&[0x0F, 0x00, 0xD0]);
    cpu.regs.set_ax(0x0000); // null LDTR is valid
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ldtr, 0x0000);
}

// ========================================================================
// Shift/rotate: SAR r16 imm8, ROL r16 imm8, ROR r16 imm8
// ========================================================================

#[test]
fn test_sar_ax_imm8() {
    // SAR AX, 4 = C1 F8 04
    let (mut cpu, mut mem) = setup(&[0xC1, 0xF8, 0x04]);
    cpu.regs.set_ax(0x8000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xF800); // sign bit preserved
}

#[test]
fn test_rol_ax_imm8() {
    // ROL AX, 4 = C1 C0 04
    let (mut cpu, mut mem) = setup(&[0xC1, 0xC0, 0x04]);
    cpu.regs.set_ax(0x1234);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x2341);
}

#[test]
fn test_ror_ax_imm8() {
    // ROR AX, 4 = C1 C8 04
    let (mut cpu, mut mem) = setup(&[0xC1, 0xC8, 0x04]);
    cpu.regs.set_ax(0x1234);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x4123);
}

// ========================================================================
// Shift/rotate: 8-bit with CL count
// ========================================================================

#[test]
fn test_shl_al_cl() {
    // SHL AL, CL = D2 E0
    let (mut cpu, mut mem) = setup(&[0xD2, 0xE0]);
    cpu.regs.set_al(0x01);
    cpu.regs.set_cl(3);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x08);
}

#[test]
fn test_shr_al_cl() {
    // SHR AL, CL = D2 E8
    let (mut cpu, mut mem) = setup(&[0xD2, 0xE8]);
    cpu.regs.set_al(0x80);
    cpu.regs.set_cl(3);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x10);
}

#[test]
fn test_sar_al_cl() {
    // SAR AL, CL = D2 F8
    let (mut cpu, mut mem) = setup(&[0xD2, 0xF8]);
    cpu.regs.set_al(0x80);
    cpu.regs.set_cl(3);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xF0);
}

#[test]
fn test_rol_al_cl() {
    // ROL AL, CL = D2 C0
    let (mut cpu, mut mem) = setup(&[0xD2, 0xC0]);
    cpu.regs.set_al(0x81);
    cpu.regs.set_cl(1);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x03);
}

#[test]
fn test_ror_al_cl() {
    // ROR AL, CL = D2 C8
    let (mut cpu, mut mem) = setup(&[0xD2, 0xC8]);
    cpu.regs.set_al(0x03);
    cpu.regs.set_cl(1);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x81);
}

// ========================================================================
// Shift/rotate: 8-bit imm8 (C0)
// ========================================================================

#[test]
fn test_rol_al_imm8() {
    // ROL AL, 4 = C0 C0 04
    let (mut cpu, mut mem) = setup(&[0xC0, 0xC0, 0x04]);
    cpu.regs.set_al(0x12);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x21);
}

#[test]
fn test_ror_al_imm8() {
    // ROR AL, 4 = C0 C8 04
    let (mut cpu, mut mem) = setup(&[0xC0, 0xC8, 0x04]);
    cpu.regs.set_al(0x21);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x12);
}

// ========================================================================
// 32-bit shifts with operand size prefix
// ========================================================================

#[test]
fn test_shl_eax_1() {
    // 66 D1 E0 = SHL EAX, 1
    let (mut cpu, mut mem) = setup(&[0x66, 0xD1, 0xE0]);
    cpu.regs.eax = 0x80000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00000000);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_shr_eax_1() {
    // 66 D1 E8 = SHR EAX, 1
    let (mut cpu, mut mem) = setup(&[0x66, 0xD1, 0xE8]);
    cpu.regs.eax = 0x00000001;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00000000);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_sar_eax_1() {
    // 66 D1 F8 = SAR EAX, 1
    let (mut cpu, mut mem) = setup(&[0x66, 0xD1, 0xF8]);
    cpu.regs.eax = 0x80000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xC0000000);
}

// ========================================================================
// 32-bit BT/BTS/BTR/BTC with imm8
// ========================================================================

#[test]
fn test_bt_eax_imm8() {
    // 66 0F BA E0 1F = BT EAX, 31
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xBA, 0xE0, 0x1F]);
    cpu.regs.eax = 0x80000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_bts_eax_imm8() {
    // 66 0F BA E8 1F = BTS EAX, 31
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xBA, 0xE8, 0x1F]);
    cpu.regs.eax = 0x00000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::CARRY)); // was clear
    assert_eq!(cpu.regs.eax, 0x80000000);
}

#[test]
fn test_btr_eax_imm8() {
    // 66 0F BA F0 1F = BTR EAX, 31
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xBA, 0xF0, 0x1F]);
    cpu.regs.eax = 0x80000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY)); // was set
    assert_eq!(cpu.regs.eax, 0x00000000);
}

// ========================================================================
// PUSH/POP segment registers
// ========================================================================

#[test]
fn test_push_es_pop_es() {
    // PUSH ES = 06, POP ES = 07
    let (mut cpu, mut mem) = setup(&[0x06, 0x07]);
    cpu.regs.es = 0x1234;
    cpu.regs.esp = 0x8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.es, 0x1234);
    assert_eq!(cpu.regs.sp(), 0x8000);
}

#[test]
fn test_push_ds_pop_ds() {
    // PUSH DS = 1E, POP DS = 1F
    let (mut cpu, mut mem) = setup(&[0x1E, 0x1F]);
    cpu.regs.ds = 0x5678;
    cpu.regs.esp = 0x8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.ds, 0x5678);
    assert_eq!(cpu.regs.sp(), 0x8000);
}

#[test]
fn test_push_ss_pop_ss() {
    // PUSH SS = 16, POP SS = 17
    let (mut cpu, mut mem) = setup(&[0x16, 0x17]);
    cpu.regs.ss = 0x0010;
    cpu.regs.esp = 0x8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.ss, 0x0010);
}

#[test]
fn test_push_cs() {
    // PUSH CS = 0E
    let (mut cpu, mut mem) = setup(&[0x0E]);
    cpu.regs.cs = 0x0000;
    cpu.regs.esp = 0x8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.sp(), 0x7FFE);
    let val = u16::from_le_bytes([mem.ram[0x7FFE], mem.ram[0x7FFF]]);
    assert_eq!(val, 0x0000);
}

// ========================================================================
// XLATB
// ========================================================================

#[test]
fn test_xlat_table_lookup() {
    // XLAT = D7 — AL = DS:[BX + AL]
    let (mut cpu, mut mem) = setup(&[0xD7]);
    // Build a translation table at 0x0400
    cpu.regs.ebx = 0x0400;
    for i in 0..=255u8 {
        mem.ram[0x0400 + i as usize] = i.wrapping_add(1);
    }
    cpu.regs.set_al(0x41); // 'A'
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x42); // 'B'
}

// ========================================================================
// OR / AND / XOR with 16-bit register pairs
// ========================================================================

#[test]
fn test_or_r16_r16() {
    // OR AX, CX = 09 C8 (modrm: mod=11, reg=CX(1), rm=AX(0))
    let (mut cpu, mut mem) = setup(&[0x09, 0xC8]);
    cpu.regs.set_ax(0x00FF);
    cpu.regs.set_cx(0xFF00);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xFFFF);
}

#[test]
fn test_and_r16_r16() {
    // AND AX, CX = 21 C8
    let (mut cpu, mut mem) = setup(&[0x21, 0xC8]);
    cpu.regs.set_ax(0xFF00);
    cpu.regs.set_cx(0x0FF0);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0F00);
}

// ========================================================================
// ADC / SBB with 16-bit operands
// ========================================================================

#[test]
fn test_adc_ax_imm16() {
    // ADC AX, 0 = 15 00 00 — adds CF
    let (mut cpu, mut mem) = setup(&[0x15, 0x00, 0x00]);
    cpu.regs.set_ax(0xFFFF);
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0000);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_sbb_ax_imm16() {
    // SBB AX, 0 = 1D 00 00 — subtracts CF
    let (mut cpu, mut mem) = setup(&[0x1D, 0x00, 0x00]);
    cpu.regs.set_ax(0x0000);
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xFFFF);
}

// ========================================================================
// MOV CR0 / MOV to/from control registers
// ========================================================================

#[test]
fn test_mov_eax_cr0() {
    // MOV EAX, CR0 = 0F 20 C0 (modrm: mod=11, /0, rm=EAX(0))
    let (mut cpu, mut mem) = setup_pm_ring0(&[0x0F, 0x20, 0xC0]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, cpu.regs.cr0);
}

#[test]
fn test_mov_eax_cr2() {
    // MOV EAX, CR2 = 0F 20 D0
    let (mut cpu, mut mem) = setup_pm_ring0(&[0x0F, 0x20, 0xD0]);
    cpu.regs.cr2 = 0xDEAD0000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xDEAD0000);
}

#[test]
fn test_mov_eax_cr3() {
    // MOV EAX, CR3 = 0F 20 D8
    let (mut cpu, mut mem) = setup_pm_ring0(&[0x0F, 0x20, 0xD8]);
    cpu.regs.cr3 = 0x12345000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x12345000);
}

#[test]
fn test_mov_from_reserved_crs_raise_ud() {
    for cr_num in [1u8, 5, 6, 7] {
        let modrm = 0xC0 | (cr_num << 3); // MOV EAX, CRn
        let (mut cpu, mut mem) = setup_pm_ring0(&[0x0F, 0x20, modrm]);
        cpu.regs.eax = 0xDEAD_BEEF;
        let mut io = ();
        let result = run(&mut cpu, &mut mem, &mut io, 1);
        assert!(
            matches!(result, RunResult::Exception { vector: 6, .. }),
            "MOV from CR{cr_num} should raise #UD, got {result:?}"
        );
        assert_eq!(cpu.regs.eip, 0x2000, "#UD should report the MOV CR IP");
        assert_eq!(cpu.regs.eax, 0xDEAD_BEEF, "#UD should not write EAX");
    }
}

#[test]
fn test_mov_to_reserved_crs_raise_ud() {
    for cr_num in [1u8, 5, 6, 7] {
        let modrm = 0xC0 | (cr_num << 3); // MOV CRn, EAX
        let (mut cpu, mut mem) = setup_pm_ring0(&[0x0F, 0x22, modrm]);
        cpu.regs.eax = 0x1234_5678;
        let mut io = ();
        let result = run(&mut cpu, &mut mem, &mut io, 1);
        assert!(
            matches!(result, RunResult::Exception { vector: 6, .. }),
            "MOV to CR{cr_num} should raise #UD, got {result:?}"
        );
        assert_eq!(cpu.regs.eip, 0x2000, "#UD should report the MOV CR IP");
    }
}

#[test]
fn test_mov_cr_ignores_modrm_mod_bits() {
    // MOV EAX, CR0 = 0F 20 00. For MOV CR/DR, ModR/M.mod is ignored;
    // byte 3 is the next instruction, not a displacement.
    let (mut cpu, mut mem) = setup_pm_ring0(&[0x0F, 0x20, 0x00, 0x90]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "MOV EAX, CR0 with mod=00 should execute, got {result:?}"
    );
    assert_eq!(cpu.regs.eax, cpu.regs.cr0);
    assert_eq!(cpu.regs.eip, 0x2003, "MOV CR should consume only ModR/M");
}

// ========================================================================
// REPNE SCASW
// ========================================================================

#[test]
fn test_repne_scasw() {
    // REPNE SCASW = F2 AF
    let (mut cpu, mut mem) = setup(&[0xF2, 0xAF]);
    cpu.regs.set_ax(0x0000); // searching for 0
    mem.ram[0x0300] = 0x11;
    mem.ram[0x0301] = 0x22;
    mem.ram[0x0302] = 0x33;
    mem.ram[0x0303] = 0x44;
    mem.ram[0x0304] = 0x00;
    mem.ram[0x0305] = 0x00; // found here
    cpu.regs.edi = 0x0300;
    cpu.regs.ecx = 3;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 100);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.ecx as u16, 0); // all 3 consumed
}

// ========================================================================
// REPE SCASB
// ========================================================================

#[test]
fn test_repe_scasb() {
    // REPE SCASB = F3 AE — scan while equal
    let (mut cpu, mut mem) = setup(&[0xF3, 0xAE]);
    cpu.regs.set_al(0xAA);
    for i in 0..5 {
        mem.ram[0x0300 + i] = 0xAA;
    }
    mem.ram[0x0305] = 0xBB; // mismatch
    cpu.regs.edi = 0x0300;
    cpu.regs.ecx = 6;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 6);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.ecx as u16, 0); // 6 - 6 = 0
}

// ========================================================================
// 32-bit CMPXCHG
// ========================================================================

#[test]
fn test_cmpxchg32_equal() {
    // 66 0F B1 D1 = CMPXCHG ECX, EDX (32-bit)
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xB1, 0xD1]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    cpu.regs.eax = 0x12345678;
    cpu.regs.ecx = 0x12345678;
    cpu.regs.edx = 0xDEADBEEF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx, 0xDEADBEEF);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_cmpxchg32_not_equal() {
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xB1, 0xD1]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    cpu.regs.eax = 0x12345678;
    cpu.regs.ecx = 0xAAAAAAAA;
    cpu.regs.edx = 0xDEADBEEF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx, 0xAAAAAAAA); // unchanged
    assert_eq!(cpu.regs.eax, 0xAAAAAAAA); // EAX = old ECX
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// 32-bit INC / DEC shortcuts (0x40-0x4F with operand size prefix)
// ========================================================================

#[test]
fn test_inc_eax() {
    // 66 40 = INC EAX
    let (mut cpu, mut mem) = setup(&[0x66, 0x40]);
    cpu.regs.eax = 0xFFFFFFFF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00000000);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_dec_eax() {
    // 66 48 = DEC EAX
    let (mut cpu, mut mem) = setup(&[0x66, 0x48]);
    cpu.regs.eax = 0x00000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFFFFFFFF);
}

// ========================================================================
// Jcc near (0F 80 - 0F 8F)
// ========================================================================

#[test]
fn test_jz_near_taken() {
    // JZ rel16 = 0F 84 04 00
    let (mut cpu, mut mem) = setup(&[0x0F, 0x84, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0108); // 0x104 + 4
}

#[test]
fn test_jnz_near_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x85, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0108);
}

#[test]
fn test_jb_near_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x82, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0108);
}

#[test]
fn test_jnb_near_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x83, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0108);
}

#[test]
fn test_jbe_near_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x86, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0108);
}

#[test]
fn test_ja_near_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x87, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.flags.remove(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0108);
}

#[test]
fn test_jl_near_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x8C, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::SIGN);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0108);
}

#[test]
fn test_jge_near_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x8D, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::SIGN);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0108);
}

#[test]
fn test_jle_near_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x8E, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0108);
}

#[test]
fn test_jg_near_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x8F, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::ZERO);
    cpu.regs.flags.remove(Flags::SIGN);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0108);
}

// ========================================================================
// JMP indirect (FF /4)
// ========================================================================

#[test]
fn test_jmp_indirect_reg() {
    // JMP AX = FF E0 (modrm: mod=11, /4, rm=AX)
    let (mut cpu, mut mem) = setup(&[0xFF, 0xE0]);
    cpu.regs.set_ax(0x0200);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0200);
}

#[test]
fn test_jmp_indirect_mem() {
    // JMP [0x0300] = FF 26 00 03 (modrm: mod=00, /4, rm=110 disp16)
    let (mut cpu, mut mem) = setup(&[0xFF, 0x26, 0x00, 0x03]);
    mem.ram[0x0300] = 0x00;
    mem.ram[0x0301] = 0x04;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0400);
}

// ========================================================================
// CALL indirect (FF /2)
// ========================================================================

#[test]
fn test_call_indirect_reg() {
    // CALL AX = FF D0 (modrm: mod=11, /2, rm=AX)
    let (mut cpu, mut mem) = setup(&[0xFF, 0xD0]);
    cpu.regs.set_ax(0x0200);
    cpu.regs.esp = 0x8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0200);
    assert_eq!(cpu.regs.sp(), 0x7FFE);
    // Return address should be 0x0102
    let ret_addr = u16::from_le_bytes([mem.ram[0x7FFE], mem.ram[0x7FFF]]);
    assert_eq!(ret_addr, 0x0102);
}

// ========================================================================
// PUSH mem16 (FF /6)
// ========================================================================

#[test]
fn test_push_mem16() {
    // PUSH [0x0300] = FF 36 00 03 (modrm: mod=00, /6, rm=110 disp16)
    let (mut cpu, mut mem) = setup(&[0xFF, 0x36, 0x00, 0x03]);
    mem.ram[0x0300] = 0xEF;
    mem.ram[0x0301] = 0xBE;
    cpu.regs.esp = 0x8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.sp(), 0x7FFE);
    let val = u16::from_le_bytes([mem.ram[0x7FFE], mem.ram[0x7FFF]]);
    assert_eq!(val, 0xBEEF);
}

// ========================================================================
// Group 2: shift/rotate with memory, CL count (D2/D3)
// ========================================================================

#[test]
fn test_shl_mem8_cl() {
    // SHL [0x0200], CL = D2 26 00 02
    let (mut cpu, mut mem) = setup(&[0xD2, 0x26, 0x00, 0x02]);
    mem.ram[0x0200] = 0x01;
    cpu.regs.set_cl(4);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x0200], 0x10);
}

#[test]
fn test_shr_mem16_cl() {
    // SHR [0x0200], CL = D3 2E 00 02
    let (mut cpu, mut mem) = setup(&[0xD3, 0x2E, 0x00, 0x02]);
    mem.ram[0x0200] = 0x00;
    mem.ram[0x0201] = 0x01; // 0x0100
    cpu.regs.set_cl(4);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u16::from_le_bytes([mem.ram[0x0200], mem.ram[0x0201]]),
        0x0010
    );
}

// ========================================================================
// DS segment override with string ops
// ========================================================================

#[test]
fn test_ds_override_movsb() {
    // DS: MOVSB = 3E A4 (DS override is default for source, this is a no-op override)
    let (mut cpu, mut mem) = setup(&[0x3E, 0xA4]);
    mem.ram[0x0300] = 0xCC;
    cpu.regs.esi = 0x0300;
    cpu.regs.edi = 0x0400;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x0400], 0xCC);
}

#[test]
fn test_es_override_movsb_source() {
    // ES: MOVSB = 26 A4 — ES override changes source segment from DS to ES
    // Source = ES:SI, Dest = ES:DI (dest always uses ES for MOVS)
    let (mut cpu, mut mem) = setup(&[0x26, 0xA4]);
    cpu.regs.es = 0x0010; // ES base = 0x100
    mem.ram[0x0400] = 0xDD; // ES:0x300 = 0x100 + 0x300 = 0x400
    cpu.regs.esi = 0x0300;
    cpu.regs.edi = 0x0500;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // Dest is ES:DI = 0x100 + 0x500 = 0x600
    assert_eq!(mem.ram[0x0600], 0xDD);
}
