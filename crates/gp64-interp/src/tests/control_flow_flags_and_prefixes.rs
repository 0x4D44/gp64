use super::*;

// ========================================================================
// Group 1: ADD/SUB/CMP r16, imm8
// ========================================================================

#[test]
fn test_add_r16_imm8() {
    // ADD CX, 1 (0x83 0xC1 0x01: modrm=0xC1, sub_op=0, rm=1(CX))
    let (mut cpu, mut mem) = setup(&[0x83, 0xC1, 0x01]);
    cpu.regs.ecx = 0x0005;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ecx as u16, 0x0006);
}

#[test]
fn test_sub_r16_imm8() {
    // SUB CX, 3 (0x83 0xE9 0x03: modrm=0xE9, sub_op=5, rm=1(CX))
    let (mut cpu, mut mem) = setup(&[0x83, 0xE9, 0x03]);
    cpu.regs.ecx = 0x0005;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ecx as u16, 0x0002);
}

#[test]
fn test_cmp_r16_imm8() {
    // CMP CX, 5 (0x83 0xF9 0x05: modrm=0xF9, sub_op=7, rm=1(CX))
    let (mut cpu, mut mem) = setup(&[0x83, 0xF9, 0x05]);
    cpu.regs.ecx = 0x0005;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.ecx as u16, 0x0005); // CMP doesn't change operand
}

#[test]
fn test_add_r16_imm8_sign_extend() {
    // ADD AX, -1 (0x83 0xC0 0xFF: sign-extends to 0xFFFF)
    let (mut cpu, mut mem) = setup(&[0x83, 0xC0, 0xFF]);
    cpu.regs.eax = 0x0010;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u16, 0x000F);
}

// ========================================================================
// Group 1: ADD/SUB/CMP r16, imm16
// ========================================================================

#[test]
fn test_cmp_r16_imm16() {
    // CMP CX, 0x2710 (0x81 0xF9 0x10 0x27)
    let (mut cpu, mut mem) = setup(&[0x81, 0xF9, 0x10, 0x27]);
    cpu.regs.ecx = 0x2710;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_add_r16_imm16() {
    // ADD AX, 0x100 (0x81 0xC0 0x00 0x01)
    let (mut cpu, mut mem) = setup(&[0x81, 0xC0, 0x00, 0x01]);
    cpu.regs.eax = 0x0050;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u16, 0x0150);
}

// ========================================================================
// ADD/SUB/CMP AX, imm16
// ========================================================================

#[test]
fn test_add_ax_imm16() {
    // ADD AX, 0x0100 (0x05 0x00 0x01)
    let (mut cpu, mut mem) = setup(&[0x05, 0x00, 0x01]);
    cpu.regs.eax = 0x0050;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u16, 0x0150);
}

#[test]
fn test_sub_ax_imm16() {
    // SUB AX, 0x0010 (0x2D 0x10 0x00)
    let (mut cpu, mut mem) = setup(&[0x2D, 0x10, 0x00]);
    cpu.regs.eax = 0x0050;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u16, 0x0040);
}

#[test]
fn test_cmp_ax_imm16() {
    // CMP AX, 0x0050 (0x3D 0x50 0x00)
    let (mut cpu, mut mem) = setup(&[0x3D, 0x50, 0x00]);
    cpu.regs.eax = 0x0050;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// JMP rel8
// ========================================================================

#[test]
fn test_jmp_rel8_forward() {
    // JMP +5 (0xEB 0x05) at 0x0100: after instr (0x0102) + 5 = 0x0107
    let (mut cpu, mut mem) = setup(&[0xEB, 0x05]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eip, 0x0107);
}

#[test]
fn test_jmp_rel8_backward() {
    // JMP -2 (0xEB 0xFE) at 0x0100: after instr (0x0102) + (-2) = 0x0100
    let (mut cpu, mut mem) = setup(&[0xEB, 0xFE]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eip, 0x0100); // jumps back to itself
}

// ========================================================================
// JMP rel16
// ========================================================================

#[test]
fn test_jmp_rel16_forward() {
    // JMP +0x100 (0xE9 0x00 0x01) at 0x0100: after instr (0x0103) + 0x100 = 0x0203
    let (mut cpu, mut mem) = setup(&[0xE9, 0x00, 0x01]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eip, 0x0203);
}

// ========================================================================
// Jcc rel8
// ========================================================================

#[test]
fn test_jne_taken() {
    // JNE -5 (0x75 0xFB) with ZF=0 → taken
    let (mut cpu, mut mem) = setup(&[0x75, 0xFB]);
    cpu.regs.flags.remove(Flags::ZERO);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    // After instruction at 0x0102, rel=-5 → 0x00FD
    assert_eq!(cpu.regs.eip, 0x00FD);
}

#[test]
fn test_jne_not_taken() {
    // JNE -5 (0x75 0xFB) with ZF=1 → not taken
    let (mut cpu, mut mem) = setup(&[0x75, 0xFB]);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eip, 0x0102);
}

#[test]
fn test_je_taken() {
    // JE +3 (0x74 0x03) with ZF=1 → taken
    let (mut cpu, mut mem) = setup(&[0x74, 0x03]);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eip, 0x0105);
}

#[test]
fn test_jb_carry() {
    // JB +3 (0x72 0x03) with CF=1 → taken
    let (mut cpu, mut mem) = setup(&[0x72, 0x03]);
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eip, 0x0105);
}

// ========================================================================
// INT n
// ========================================================================

#[test]
fn test_int_n() {
    // INT 0x21 (0xCD 0x21)
    let (mut cpu, mut mem) = setup(&[0xCD, 0x21]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(
        result,
        RunResult::SoftwareInterrupt {
            vector: 0x21,
            executed: 1
        }
    ));
    assert_eq!(cpu.regs.eip, 0x0102);
}

// ========================================================================
// HLT
// ========================================================================

#[test]
fn test_hlt() {
    let (mut cpu, mut mem) = setup(&[0xF4]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::Halt { executed: 1 }));
    assert!(cpu.halted);
}

#[test]
fn test_sti_hlt_consumes_interrupt_shadow() {
    let (mut cpu, mut mem) = setup(&[0xFB, 0xF4]);
    cpu.regs.flags.remove(Flags::INTERRUPT);
    cpu.interrupts_enabled = false;
    cpu.interrupt_inhibit = 0;
    let mut io = ();

    let sti_result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(matches!(
        sti_result,
        RunResult::InterruptPollBoundary { executed: 1 }
    ));
    assert_eq!(cpu.interrupt_inhibit, 1, "STI must arm the shadow");

    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert!(matches!(result, RunResult::Halt { executed: 1 }));
    assert!(cpu.halted);
    assert!(cpu.regs.flags.contains(Flags::INTERRUPT));
    assert!(cpu.interrupts_enabled);
    assert_eq!(
        cpu.interrupt_inhibit, 0,
        "HLT must consume the STI interrupt shadow so IRQ polling can wake it"
    );
}

/// MDD-BUG-CRUCIBLE-00328 — this test previously asserted the opposite
/// (that a shadowed HLT halts and the TF trap is deferred). That pinned the
/// defect: the IRQ shadow was suppressing the debug trap.
///
/// HLT here is the *protected successor*. Whichever instruction armed the
/// shadow — `STI`, which never suppresses a debug trap at all, or an SS
/// load, whose deferred trap is delivered at the boundary following the
/// next instruction — the boundary after this HLT is where the `#DB` is
/// taken. So the trap fires and HLT does not halt.
#[test]
fn test_hlt_with_tf_under_interrupt_shadow_traps() {
    let (mut cpu, mut mem) = setup(&[0xF4]);
    cpu.regs.flags.insert(Flags::TRAP);
    cpu.interrupt_inhibit = 1;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 1,
                executed: 1,
                ..
            }
        ),
        "the protected successor's own single-step trap must be delivered, got {result:?}"
    );
    assert!(!cpu.halted, "the debug trap pre-empts the halt");
}

// ========================================================================
// MDD-BUG-CRUCIBLE-00328 — the IRQ shadow and the debug shadow are
// different objects.
//
// | after…         | own TF #DB | successor's TF #DB | successor IRQ |
// |----------------|------------|--------------------|---------------|
// | STI            | delivered  | delivered          | blocked       |
// | MOV/POP SS/LSS | suppressed | delivered          | blocked       |
//
// Each test below asserts one cell, and the IRQ-blocking column is
// asserted independently of the trap column so a regression in one cannot
// be masked by the other.
// ========================================================================

/// Run one instruction with TF set and report whether the single-step trap
/// was taken at that boundary.
fn stepped(cpu: &mut Cpu, mem: &mut Memory) -> bool {
    let mut io = ();
    matches!(
        run(cpu, mem, &mut io, 1),
        RunResult::Exception { vector: 1, .. }
    )
}

#[test]
fn tf_trap_is_delivered_after_sti_itself() {
    // STI; NOP  — IF does not mask exceptions, so STI's own trap fires.
    let (mut cpu, mut mem) = setup(&[0xFB, 0x90]);
    cpu.regs.flags.insert(Flags::TRAP);
    cpu.regs.flags.remove(Flags::INTERRUPT);
    cpu.interrupts_enabled = false;
    cpu.interrupt_inhibit = 0;

    assert!(
        stepped(&mut cpu, &mut mem),
        "STI must not suppress its own single-step trap"
    );
    assert_eq!(
        cpu.interrupt_inhibit, 1,
        "…while still arming the IRQ shadow for one successor"
    );
}

#[test]
fn tf_trap_is_delivered_after_the_instruction_sti_protects() {
    // The successor of STI is protected from IRQs, never from #DB.
    let (mut cpu, mut mem) = setup(&[0x90]);
    cpu.regs.flags.insert(Flags::TRAP);
    cpu.interrupt_inhibit = 1;

    assert!(
        stepped(&mut cpu, &mut mem),
        "the IRQ-protected successor must still take its own trap"
    );
}

#[test]
fn tf_trap_is_suppressed_by_mov_ss_but_not_by_its_successor() {
    // MOV SS, AX ; NOP  — real mode, so the load needs no descriptor.
    let (mut cpu, mut mem) = setup(&[0x8E, 0xD0, 0x90]);
    cpu.regs.flags.insert(Flags::TRAP);
    cpu.regs.set_ax(0x0000);
    cpu.interrupt_inhibit = 0;

    assert!(
        !stepped(&mut cpu, &mut mem),
        "MOV SS must suppress its OWN single-step trap (SDM Vol 3A 6.8.3)"
    );
    assert_eq!(
        cpu.interrupt_inhibit, 1,
        "MOV SS must arm the IRQ shadow for one successor"
    );

    assert!(
        stepped(&mut cpu, &mut mem),
        "the deferred trap is delivered at the boundary following the successor"
    );
}

#[test]
fn tf_trap_is_suppressed_by_pop_ss_but_not_by_its_successor() {
    // POP SS ; NOP
    let (mut cpu, mut mem) = setup(&[0x17, 0x90]);
    cpu.regs.flags.insert(Flags::TRAP);
    cpu.regs.esp = 0x0FFC;
    cpu.regs.ss = 0x0000;
    mem.ram[0x0FFC] = 0x00;
    mem.ram[0x0FFD] = 0x00;
    cpu.interrupt_inhibit = 0;

    assert!(
        !stepped(&mut cpu, &mut mem),
        "POP SS must suppress its OWN single-step trap"
    );
    assert_eq!(cpu.interrupt_inhibit, 1, "…and arm the IRQ shadow");

    assert!(
        stepped(&mut cpu, &mut mem),
        "the successor of POP SS still takes its own trap"
    );
}

#[test]
fn tf_trap_is_suppressed_by_lss_but_not_by_its_successor() {
    // LSS BX, [0x200] ; NOP  — 0F B2 /r.
    let (mut cpu, mut mem) = setup(&[0x0F, 0xB2, 0x1E, 0x00, 0x02, 0x90]);
    cpu.regs.flags.insert(Flags::TRAP);
    // offset 0x1000, segment 0x0000
    mem.ram[0x200] = 0x00;
    mem.ram[0x201] = 0x10;
    mem.ram[0x202] = 0x00;
    mem.ram[0x203] = 0x00;
    cpu.interrupt_inhibit = 0;

    assert!(
        !stepped(&mut cpu, &mut mem),
        "LSS loads SS, so it must suppress its OWN single-step trap"
    );
    assert_eq!(cpu.interrupt_inhibit, 1, "…and arm the IRQ shadow");

    assert!(
        stepped(&mut cpu, &mut mem),
        "the successor of LSS still takes its own trap"
    );
}

#[test]
fn ss_load_debug_suppression_does_not_depend_on_tf_being_set() {
    // The suppression is a property of the SS load, not of TF: with TF
    // clear the sequence simply runs, and the IRQ shadow is still armed.
    let (mut cpu, mut mem) = setup(&[0x8E, 0xD0, 0x90]);
    cpu.regs.flags.remove(Flags::TRAP);
    cpu.regs.set_ax(0x0000);
    cpu.interrupt_inhibit = 0;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        !matches!(result, RunResult::Exception { vector: 1, .. }),
        "no TF, no trap"
    );
    assert_eq!(cpu.interrupt_inhibit, 1, "the IRQ shadow is independent");
}

#[test]
fn test_hlt_pm_cpl0_succeeds() {
    // HLT at CPL=0 in PM should succeed
    let (mut cpu, mut mem) = setup(&[0xF4]);
    cpu.regs.cr0 |= 1; // Protected mode
    cpu.regs.cpl = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Halt { executed: 1 }),
        "HLT at CPL=0 should halt, got {:?}",
        result,
    );
    assert!(cpu.halted);
}

#[test]
fn test_hlt_pm_cpl3_raises_gp() {
    // HLT at CPL=3 in PM should raise #GP(0)
    let (mut cpu, mut mem) = setup(&[0xF4]);
    cpu.regs.cr0 |= 1; // Protected mode
    cpu.regs.cpl = 3;
    cpu.regs.cs = 0x001B; // RPL=3
    cpu.regs.cs_cache.base = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 13,
                error_code: 0,
                ..
            }
        ),
        "HLT at CPL=3 should raise #GP(0), got {:?}",
        result,
    );
    assert!(!cpu.halted);
}

#[test]
fn test_hlt_real_mode_succeeds() {
    // Real mode HLT should always succeed (no CPL check)
    let (mut cpu, mut mem) = setup(&[0xF4]);
    // Real mode: CR0.PE=0 (default from Cpu::new())
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Halt { executed: 1 }),
        "Real mode HLT should halt, got {:?}",
        result,
    );
    assert!(cpu.halted);
}

// ========================================================================
// INC / DEC
// ========================================================================

#[test]
fn test_inc_ax() {
    // INC AX (0x40)
    let (mut cpu, mut mem) = setup(&[0x40]);
    cpu.regs.eax = 0x0005;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u16, 0x0006);
}

#[test]
fn test_inc_preserves_carry() {
    // INC AX with CF set — should preserve CF
    let (mut cpu, mut mem) = setup(&[0x40]);
    cpu.regs.eax = 0x0005;
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_dec_cx() {
    // DEC CX (0x49)
    let (mut cpu, mut mem) = setup(&[0x49]);
    cpu.regs.ecx = 0x0001;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ecx as u16, 0);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// Flag manipulation
// ========================================================================

#[test]
fn test_clc_stc() {
    // CLC (0xF8), STC (0xF9)
    let (mut cpu, mut mem) = setup(&[0xF8, 0xF9]);
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(!cpu.regs.flags.contains(Flags::CARRY));

    // Now run STC
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_cld_std() {
    // CLD (0xFC), STD (0xFD)
    let (mut cpu, mut mem) = setup(&[0xFC, 0xFD]);
    cpu.regs.flags.insert(Flags::DIRECTION);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(!cpu.regs.flags.contains(Flags::DIRECTION));

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(cpu.regs.flags.contains(Flags::DIRECTION));
}

#[test]
fn test_cmc() {
    // CMC (0xF5) — toggle carry
    let (mut cpu, mut mem) = setup(&[0xF5, 0xF5]);
    cpu.regs.flags.remove(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_sti_caps_budget() {
    // STI (0xFB) must end the batch before its shadowed NOP executes.
    let (mut cpu, mut mem) = setup(&[0xFB, 0x90, 0x90]);
    cpu.regs.flags.remove(Flags::INTERRUPT);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 100);
    assert!(matches!(
        result,
        RunResult::InterruptPollBoundary { executed: 1 }
    ));
    assert!(cpu.regs.flags.contains(Flags::INTERRUPT));
    assert_eq!(cpu.regs.ip(), 0x0101);
    assert_eq!(cpu.interrupt_inhibit, 1);
}

// M1: CLI/STI IOPL privilege checks

#[test]
fn test_cli_pm_cpl3_iopl0_raises_gp() {
    // CLI at CPL=3 with IOPL=0 should raise #GP(0)
    let (mut cpu, mut mem) = setup(&[0xFA]); // CLI
    cpu.regs.cr0 |= 1; // Protected mode
    cpu.regs.cpl = 3;
    cpu.regs.cs = 0x001B;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.set_iopl(0);
    cpu.regs.flags.insert(Flags::INTERRUPT); // IF=1 before CLI
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 13,
                error_code: 0,
                ..
            }
        ),
        "CLI at CPL=3/IOPL=0 should #GP(0), got {:?}",
        result,
    );
    // IF should be unchanged (faulted before modification)
    assert!(cpu.regs.flags.contains(Flags::INTERRUPT));
}

#[test]
fn test_cli_pm_cpl0_iopl3_succeeds() {
    // CLI at CPL=0 with IOPL=3 should succeed
    let (mut cpu, mut mem) = setup(&[0xFA]); // CLI
    cpu.regs.cr0 |= 1;
    cpu.regs.cpl = 0;
    cpu.regs.set_iopl(3);
    cpu.regs.flags.insert(Flags::INTERRUPT);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "CLI at CPL=0/IOPL=3 should succeed, got {:?}",
        result,
    );
    assert!(!cpu.regs.flags.contains(Flags::INTERRUPT));
}

#[test]
fn test_sti_pm_cpl3_iopl0_raises_gp() {
    // STI at CPL=3 with IOPL=0 should raise #GP(0)
    let (mut cpu, mut mem) = setup(&[0xFB]); // STI
    cpu.regs.cr0 |= 1;
    cpu.regs.cpl = 3;
    cpu.regs.cs = 0x001B;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.set_iopl(0);
    cpu.regs.flags.remove(Flags::INTERRUPT); // IF=0 before STI
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 13,
                error_code: 0,
                ..
            }
        ),
        "STI at CPL=3/IOPL=0 should #GP(0), got {:?}",
        result,
    );
    assert!(!cpu.regs.flags.contains(Flags::INTERRUPT));
}

#[test]
fn test_sti_pm_cpl0_iopl3_succeeds() {
    // STI at CPL=0 with IOPL=3 should succeed
    let (mut cpu, mut mem) = setup(&[0xFB, 0x90]); // STI + NOP
    cpu.regs.cr0 |= 1;
    cpu.regs.cpl = 0;
    cpu.regs.set_iopl(3);
    cpu.regs.flags.remove(Flags::INTERRUPT);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 10);
    assert!(
        matches!(result, RunResult::InterruptPollBoundary { executed: 1 }),
        "STI at CPL=0/IOPL=3 should succeed, got {:?}",
        result,
    );
    assert!(cpu.regs.flags.contains(Flags::INTERRUPT));
}

#[test]
fn test_cli_real_mode_always_succeeds() {
    // CLI in real mode should always succeed regardless of IOPL
    let (mut cpu, mut mem) = setup(&[0xFA]); // CLI
    cpu.regs.set_iopl(0);
    cpu.regs.flags.insert(Flags::INTERRUPT);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(!cpu.regs.flags.contains(Flags::INTERRUPT));
}

// MDD-BUG-ANVIL-00002: terminal fault paths must materialize pending lazy
// ALU flags before publishing FLAGS to the trap frame.

#[test]
fn test_int_n_v86_gp_publishes_materialized_flags() {
    // INT n from V86 mode with IOPL<3 raises #GP(0) (monitor trap). The
    // FLAGS image published to cpu.regs.flags — which a V86 monitor later
    // reads off the trap frame — must reflect the PRECEDING ALU
    // instruction's result, not the stale pre-ALU flags. We run:
    //   ADD AL, 1   (AL=0xFF -> 0x00: sets CF, ZF, AF, PF; leaves flags lazy)
    //   INT 21h     (#GP(0) in V86 IOPL<3, CR4.VME=0)
    // and assert the materialized carry/zero survive onto cpu.regs.flags.
    // Before the fix, op_int_n's V86 early return published flags_reg
    // WITHOUT materializing lazy, so CF/ZF read back clear (stale).
    let (mut cpu, mut mem) = setup(&[0x04, 0x01, 0xCD, 0x21]);
    cpu.regs.cr0 |= 1; // Protected mode (PE)
    cpu.regs.eflags_high |= 0x0002; // VM flag (V86 mode)
    cpu.regs.set_iopl(0);
    cpu.regs.cpl = 3;
    cpu.regs.eax = 0x0000_00FF; // AL = 0xFF
                                // Force a known "stale" baseline: every ALU flag clear.
    cpu.regs.flags.remove(
        Flags::CARRY
            | Flags::ZERO
            | Flags::SIGN
            | Flags::OVERFLOW
            | Flags::AUX_CARRY
            | Flags::PARITY,
    );
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 4);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 13,
                error_code: 0,
                ..
            }
        ),
        "INT n in V86 with IOPL<3 must #GP(0), got {result:?}",
    );
    assert_eq!(
        cpu.regs.eax & 0xFF,
        0x00,
        "ADD AL,1 with AL=0xFF must wrap to 0"
    );
    // With the fix, the lazy ADD flags are materialized before the #GP
    // frame is built. Without it, these were the stale pre-ADD values.
    assert!(
        cpu.regs.flags.contains(Flags::CARRY),
        "CF from ADD (0xFF+1) must reach the #GP trap frame"
    );
    assert!(
        cpu.regs.flags.contains(Flags::ZERO),
        "ZF from ADD (result 0) must reach the #GP trap frame"
    );
    assert!(
        cpu.regs.flags.contains(Flags::AUX_CARRY),
        "AF from ADD must reach the #GP trap frame"
    );
    assert!(
        cpu.regs.flags.contains(Flags::PARITY),
        "PF from ADD (result 0) must reach the #GP trap frame"
    );
    assert!(!cpu.regs.flags.contains(Flags::SIGN));
    assert!(!cpu.regs.flags.contains(Flags::OVERFLOW));
}

#[test]
fn test_clts_pm_cpl3_gp_publishes_materialized_flags() {
    // Representative op_0f privileged-arm fault: CLTS (0F 06) at CPL!=0 in
    // protected mode raises #GP(0). The published FLAGS must reflect the
    // preceding ALU op, not the stale pre-ALU flags. Before the fix the
    // 0F 06 arm published flags_reg without materializing lazy.
    //   ADD AL, 1   (AL=0xFF -> 0x00: sets CF, ZF; leaves flags lazy)
    //   CLTS        (#GP(0) at CPL 3)
    let (mut cpu, mut mem) = setup(&[0x04, 0x01, 0x0F, 0x06]);
    cpu.regs.cr0 |= 1; // Protected mode (PE)
    cpu.regs.cpl = 3;
    cpu.regs.cs = 0x001B;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.cs_cache.limit = 0xF_FFFF;
    cpu.regs.cs_cache.granularity = true;
    cpu.regs.cs_cache.recompute_effective_limit();
    cpu.regs.eax = 0x0000_00FF; // AL = 0xFF
    cpu.regs.flags.remove(
        Flags::CARRY
            | Flags::ZERO
            | Flags::SIGN
            | Flags::OVERFLOW
            | Flags::AUX_CARRY
            | Flags::PARITY,
    );
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 4);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 13,
                error_code: 0,
                ..
            }
        ),
        "CLTS at CPL 3 must #GP(0), got {result:?}",
    );
    assert!(
        cpu.regs.flags.contains(Flags::CARRY),
        "CF from ADD must survive onto the CLTS #GP trap frame"
    );
    assert!(
        cpu.regs.flags.contains(Flags::ZERO),
        "ZF from ADD must survive onto the CLTS #GP trap frame"
    );
}

// L2: POPFD V86 mode #GP check

#[test]
fn test_popfd_v86_iopl0_raises_gp() {
    // POPFD (0x66 0x9D) in V86 mode with IOPL=0 should raise #GP(0)
    let (mut cpu, mut mem) = setup(&[0x66, 0x9D]); // o32 prefix + POPF = POPFD
    cpu.regs.cr0 |= 1; // Protected mode
    cpu.regs.eflags_high |= 0x0002; // VM flag (V86 mode)
    cpu.regs.set_iopl(0);
    cpu.regs.cpl = 3;
    // Push a dword onto the stack so POPFD has something to pop
    cpu.regs.esp = 0x7FFC;
    cpu.regs.set_sp(0x7FFC);
    cpu.regs.ss_cache.base = 0;
    let val: u32 = 0x0000_0202; // reserved flag set
    mem.ram[0x7FFC] = val as u8;
    mem.ram[0x7FFD] = (val >> 8) as u8;
    mem.ram[0x7FFE] = (val >> 16) as u8;
    mem.ram[0x7FFF] = (val >> 24) as u8;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 13,
                error_code: 0,
                ..
            }
        ),
        "POPFD in V86 with IOPL=0 should #GP(0), got {:?}",
        result,
    );
}

#[test]
fn test_popfd_v86_iopl3_succeeds() {
    // POPFD (0x66 0x9D) in V86 mode with IOPL=3 should succeed
    let (mut cpu, mut mem) = setup(&[0x66, 0x9D]); // o32 prefix + POPF = POPFD
    cpu.regs.cr0 |= 1;
    cpu.regs.eflags_high |= 0x0002; // VM flag
    cpu.regs.set_iopl(3);
    cpu.regs.cpl = 3;
    cpu.regs.esp = 0x7FFC;
    cpu.regs.set_sp(0x7FFC);
    cpu.regs.ss_cache.base = 0;
    let val: u32 = 0x0000_0202;
    mem.ram[0x7FFC] = val as u8;
    mem.ram[0x7FFD] = (val >> 8) as u8;
    mem.ram[0x7FFE] = (val >> 16) as u8;
    mem.ram[0x7FFF] = (val >> 24) as u8;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::InterruptPollBoundary { executed: 1 }),
        "POPFD in V86 with IOPL=3 should succeed, got {:?}",
        result,
    );
    assert!(
        cpu.regs.is_v86_mode(),
        "POPFD must preserve VM even when the stack image has VM clear"
    );
}

#[test]
fn test_popfd_non_v86_pm_succeeds() {
    // POPFD in non-V86 PM should succeed (IOPL check via apply_popf_privilege, no #GP)
    let (mut cpu, mut mem) = setup(&[0x66, 0x9D]); // o32 prefix + POPF = POPFD
    cpu.regs.cr0 |= 1;
    cpu.regs.cpl = 3;
    cpu.regs.cs = 0x001B;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.set_iopl(0);
    cpu.regs.esp = 0x7FFC;
    cpu.regs.set_sp(0x7FFC);
    cpu.regs.ss_cache.base = 0;
    let val: u32 = 0x0000_0202;
    mem.ram[0x7FFC] = val as u8;
    mem.ram[0x7FFD] = (val >> 8) as u8;
    mem.ram[0x7FFE] = (val >> 16) as u8;
    mem.ram[0x7FFF] = (val >> 24) as u8;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "POPFD in non-V86 PM should succeed, got {:?}",
        result,
    );
}

// L2: PUSHF / PUSHFD V86 mode #GP check (Phase 4b Step 0b)

#[test]
fn test_pushfd_v86_iopl0_raises_gp() {
    // PUSHFD (0x66 0x9C) in V86 with IOPL<3 must #GP(0) so a V86
    // V86 monitor can virtualise VIF into the pushed image.
    let (mut cpu, mut mem) = setup(&[0x66, 0x9C]); // o32 prefix + PUSHF = PUSHFD
    cpu.regs.cr0 |= 1;
    cpu.regs.eflags_high |= 0x0002; // VM flag
    cpu.regs.set_iopl(0);
    cpu.regs.cpl = 3;
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.ss_cache.base = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 13,
                error_code: 0,
                ..
            }
        ),
        "PUSHFD in V86 with IOPL=0 should #GP(0), got {:?}",
        result,
    );
}

#[test]
fn test_pushf_v86_iopl0_raises_gp() {
    // 16-bit PUSHF (0x9C) in V86 with IOPL<3 must also #GP(0).
    // The V86 monitor relies on the trap to virtualize 16-bit PUSHF.
    let (mut cpu, mut mem) = setup(&[0x9C]); // PUSHF (no operand-size prefix)
    cpu.regs.cr0 |= 1;
    cpu.regs.eflags_high |= 0x0002;
    cpu.regs.set_iopl(0);
    cpu.regs.cpl = 3;
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.ss_cache.base = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 13,
                error_code: 0,
                ..
            }
        ),
        "PUSHF (16-bit) in V86 with IOPL=0 should #GP(0), got {:?}",
        result,
    );
}

#[test]
fn test_pushfd_v86_iopl3_succeeds() {
    // PUSHFD in V86 with IOPL=3: no #GP (IOPL grants unrestricted access).
    let (mut cpu, mut mem) = setup(&[0x66, 0x9C]);
    cpu.regs.cr0 |= 1;
    cpu.regs.eflags_high |= 0x0002;
    cpu.regs.set_iopl(3);
    cpu.regs.cpl = 3;
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.ss_cache.base = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "PUSHFD in V86 with IOPL=3 should succeed, got {:?}",
        result,
    );
}

#[test]
fn test_pushfd_non_v86_pm_succeeds() {
    // Regression: PUSHFD in non-V86 PM must not be affected by the
    // new V86 guard — IOPL<3 in plain PM does not trap PUSHFD.
    let (mut cpu, mut mem) = setup(&[0x66, 0x9C]);
    cpu.regs.cr0 |= 1;
    cpu.regs.cpl = 3;
    cpu.regs.cs = 0x001B;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.set_iopl(0);
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.ss_cache.base = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "PUSHFD in non-V86 PM should succeed, got {:?}",
        result,
    );
}

// L2: IRET / IRETD V86 mode #GP check (Phase 4b Step 2)

#[test]
fn test_iret_v86_iopl0_raises_gp() {
    // 16-bit IRET (0xCF) in V86 with IOPL<3 must #GP(0) so a V86 monitor
    // can virtualise the return. Intel SDM vol 3 §20.3.1.1.
    let (mut cpu, mut mem) = setup(&[0xCF]); // IRET (no operand-size prefix)
    cpu.regs.cr0 |= 1;
    cpu.regs.eflags_high |= 0x0002; // VM flag
    cpu.regs.set_iopl(0);
    cpu.regs.cpl = 3;
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.ss_cache.base = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 13,
                error_code: 0,
                ..
            }
        ),
        "IRET (16-bit) in V86 with IOPL=0 should #GP(0), got {:?}",
        result,
    );
}

#[test]
fn test_iretd_v86_iopl0_raises_gp() {
    // IRETD (0x66 0xCF) in V86 with IOPL<3 must #GP(0). Same rule as
    // 16-bit IRET — applies to both operand sizes.
    let (mut cpu, mut mem) = setup(&[0x66, 0xCF]); // o32 prefix + IRET = IRETD
    cpu.regs.cr0 |= 1;
    cpu.regs.eflags_high |= 0x0002;
    cpu.regs.set_iopl(0);
    cpu.regs.cpl = 3;
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.ss_cache.base = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 13,
                error_code: 0,
                ..
            }
        ),
        "IRETD in V86 with IOPL=0 should #GP(0), got {:?}",
        result,
    );
}

#[test]
fn test_iretd_v86_iopl3_succeeds() {
    // IRETD in V86 with IOPL=3: no #GP from the new guard (IOPL grants
    // unrestricted access). The actual IRET may fail/exit V86 depending
    // on the stack contents — we just assert it is NOT our #GP(0).
    let (mut cpu, _mem) = setup(&[0x66, 0xCF]);
    cpu.regs.cr0 |= 1;
    cpu.regs.eflags_high |= 0x0002;
    cpu.regs.set_iopl(3);
    cpu.regs.cpl = 3;
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.ss_cache.base = 0;
    // MDD-BUG-CRUCIBLE-00435: this used to assert `!is_our_guard_gp ||
    // true` — true for every possible result, so it stayed green no matter
    // what the guard did. The guard's #GP(0) is indistinguishable from a
    // downstream #GP(0) in the RunResult, so the only honest way to assert
    // "the guard did NOT fire" is to ask the guard.
    assert!(
        !iret_v86_guard_traps(&cpu),
        "IOPL=3 in V86 must leave the IRET guard inactive"
    );
}

#[test]
fn test_iretd_non_v86_pm_succeeds() {
    // Regression: IRETD in non-V86 PM must not be affected by the
    // V86 guard — IOPL<3 in plain PM does not trap IRETD via this path.
    let (mut cpu, mut mem) = setup(&[0x66, 0xCF]);
    cpu.regs.cr0 |= 1;
    cpu.regs.cpl = 3;
    cpu.regs.cs = 0x001B;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.set_iopl(0);
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.ss_cache.base = 0;

    // MDD-BUG-CRUCIBLE-00435: this used to run the interpreter and then
    // `let _ = result;` — no assertion at all, so it could not fail. Plain
    // PM at IOPL=0 is precisely the case a careless widening of the guard
    // would break, and nothing was watching it.
    assert!(
        !iret_v86_guard_traps(&cpu),
        "plain PM at IOPL=0 must not trap through the V86 IRET guard"
    );

    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
}

/// MDD-BUG-CRUCIBLE-00435: the guard's decision table, asserted directly.
///
/// Both "must not fire" cases above are one-sided, so this pins the whole
/// rule — including that the guard DOES fire where it should, so it cannot
/// be satisfied by a predicate that always returns false.
#[test]
fn iret_v86_guard_decision_table_crucible435() {
    let cases: [(bool, bool, u32, bool); 6] = [
        // (real_mode, v86, iopl, expect_trap)
        (true, true, 0, false),   // real mode is exempt
        (false, true, 0, true),   // V86, IOPL 0 -> trap
        (false, true, 2, true),   // V86, IOPL 2 -> trap
        (false, true, 3, false),  // V86, IOPL 3 -> no trap
        (false, false, 0, false), // plain PM at IOPL 0 -> no trap
        (false, false, 3, false), // plain PM at IOPL 3 -> no trap
    ];

    for (real_mode, v86, iopl, expect_trap) in cases {
        let (mut cpu, _mem) = setup(&[0x66, 0xCF]);
        if real_mode {
            cpu.regs.cr0 &= !1;
        } else {
            cpu.regs.cr0 |= 1;
        }
        if v86 {
            cpu.regs.eflags_high |= 0x0002;
        } else {
            cpu.regs.eflags_high &= !0x0002;
        }
        cpu.regs.set_iopl(iopl as u8);
        cpu.regs.cpl = 3;

        assert_eq!(
            iret_v86_guard_traps(&cpu),
            expect_trap,
            "real_mode={real_mode} v86={v86} iopl={iopl}"
        );
    }
}

// ========================================================================
// Prefixes
// ========================================================================

#[test]
fn test_segment_prefix_doesnt_crash() {
    // ES: NOP (0x26 0x90) — prefix is harmless for NOP
    let (mut cpu, mut mem) = setup(&[0x26, 0x90]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eip, 0x0102);
}

#[test]
fn test_operand_size_prefix() {
    // 0x66 MOV EAX, imm32 in 16-bit mode (0x66 0xB8 + 4 bytes)
    let (mut cpu, mut mem) = setup(&[0x66, 0xB8, 0x78, 0x56, 0x34, 0x12]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax, 0x12345678);
}

#[test]
fn test_lock_prefix_invalid_raises_ud() {
    // LOCK NOP (0xF0 0x90) — #UD: NOP is not a lockable instruction
    let (mut cpu, mut mem) = setup(&[0xF0, 0x90]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 6, .. }),
        "LOCK NOP should raise #UD, got {result:?}"
    );
}

#[test]
fn invalid_lock_wins_over_total_instruction_length_fault() {
    let mut code = vec![0x67; 13];
    code.extend_from_slice(&[0xF0, 0xC7, 0xC0, 0, 0, 0, 0]);
    let (mut cpu, mut mem) = setup(&code);

    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 6, .. }),
        "invalid LOCK must raise #UD before the overlength #GP, got {result:?}"
    );
}

/// Group 11 (`C6`/`C7`) defines only `/0` = MOV r/m, imm. `/1` through `/7`
/// are reserved and #UD on a 386 through Pentium. The fast interpreter used
/// only the mode and r/m fields, so it executed them as MOV — `C6 C8 42`
/// silently set AL (MDD-BUG-CRUCIBLE-00431). The reject must land before any
/// effective-address, immediate or destination access, so nothing is
/// retired and no destination moves.
#[test]
fn c6_c7_reserved_extensions_raise_ud_without_side_effects_crucible431() {
    const SCRATCH: usize = 0x0800;
    const SENTINEL: u8 = 0x5A;

    for op in [0xC6u8, 0xC7u8] {
        for ext in 1u8..=7 {
            // Register form: mod=11, reg=ext, rm=000 (AL / AX).
            let (mut cpu, mut mem) = setup(&[op, 0xC0 | (ext << 3), 0x42, 0x00]);
            cpu.regs.eax = 0x1234_5678;
            let mut io = ();
            let result = run(&mut cpu, &mut mem, &mut io, 1);
            assert!(
                matches!(
                    result,
                    RunResult::Exception {
                        vector: 6,
                        executed: 0,
                        ..
                    }
                ),
                "{op:#04X} /{ext} register form must #UD with nothing retired, got {result:?}"
            );
            assert_eq!(
                cpu.regs.eip, 0x0100,
                "{op:#04X} /{ext} must not advance EIP"
            );
            assert_eq!(
                cpu.regs.eax, 0x1234_5678,
                "{op:#04X} /{ext} must not write the register destination"
            );

            // Memory form: mod=00, reg=ext, rm=110 (disp16).
            let (mut cpu, mut mem) = setup(&[
                op,
                (ext << 3) | 0x06,
                (SCRATCH & 0xFF) as u8,
                (SCRATCH >> 8) as u8,
                0x42,
                0x00,
            ]);
            mem.ram[SCRATCH] = SENTINEL;
            let mut io = ();
            let result = run(&mut cpu, &mut mem, &mut io, 1);
            assert!(
                matches!(
                    result,
                    RunResult::Exception {
                        vector: 6,
                        executed: 0,
                        ..
                    }
                ),
                "{op:#04X} /{ext} memory form must #UD with nothing retired, got {result:?}"
            );
            assert_eq!(
                cpu.regs.eip, 0x0100,
                "{op:#04X} /{ext} must not advance EIP"
            );
            assert_eq!(
                mem.ram[SCRATCH], SENTINEL,
                "{op:#04X} /{ext} must not write the memory destination"
            );
        }
    }
}

/// Control for the guard above: `/0` is the real encoding and must still
/// execute as MOV, in both the register and the memory form.
#[test]
fn c6_c7_extension_zero_still_moves_crucible431() {
    // C6 C0 42 — MOV AL, 42h
    let (mut cpu, mut mem) = setup(&[0xC6, 0xC0, 0x42]);
    cpu.regs.eax = 0x1234_5678;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax & 0xFF, 0x42, "C6 /0 must still write AL");

    // C6 06 00 08 42 — MOV byte [0800h], 42h
    let (mut cpu, mut mem) = setup(&[0xC6, 0x06, 0x00, 0x08, 0x42]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x0800], 0x42, "C6 /0 must still write memory");
}

#[test]
fn lock_hlt_raises_ud_before_halting() {
    // MDD-BUG-ANVIL-00004: an invalid LOCK prefix must #UD BEFORE the opcode
    // runs. LOCK HLT (F0 F4) previously reached HLT (RunResult::Halt) before the
    // epilogue LOCK check — it must fault with #UD instead of halting the CPU.
    let (mut cpu, mut mem) = setup(&[0xF0, 0xF4]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 6, .. }),
        "LOCK HLT must raise #UD, not halt; got {result:?}"
    );
    assert_eq!(
        cpu.regs.eip, 0x0100,
        "#UD must be reported at the original instruction IP"
    );
}

/// `lock_prefix_valid` is the byte-slice reader DBX admits LOCK forms with; it
/// must agree with the interpreter's own table and fail closed on short input.
#[test]
fn lock_prefix_valid_reads_the_interpreter_table_and_fails_closed() {
    // Memory-destination RMW forms: legal.
    for bytes in [
        &[0xFF, 0x00][..],         // INC dword [eax]
        &[0xFF, 0x08],             // DEC dword [eax]
        &[0x01, 0x03],             // ADD [ebx], eax
        &[0x87, 0x03],             // XCHG [ebx], eax
        &[0x0F, 0xC1, 0x03],       // XADD [ebx], eax
        &[0x0F, 0xB1, 0x0B],       // CMPXCHG [ebx], ecx
        &[0x0F, 0xBA, 0x28, 1],    // BTS dword [eax], 1
        &[0x81, 0x30, 1, 0, 0, 0], // XOR dword [eax], 1
    ] {
        assert!(lock_prefix_valid(bytes), "{bytes:02X?} must be lockable");
    }
    // #UD forms: register destinations, read-only ops, non-RMW opcodes.
    for bytes in [
        &[0x90][..],            // NOP
        &[0x01, 0xC3],          // ADD ebx, eax
        &[0xFF, 0xC0],          // INC eax
        &[0xFF, 0x10],          // CALL [eax]
        &[0x83, 0x38, 1],       // CMP dword [eax], 1
        &[0x89, 0x03],          // MOV [ebx], eax
        &[0x0F, 0xBA, 0x20, 1], // BT dword [eax], 1
    ] {
        assert!(!lock_prefix_valid(bytes), "{bytes:02X?} must be #UD");
    }
    // Too short to decide: fail closed.
    assert!(!lock_prefix_valid(&[]));
    assert!(!lock_prefix_valid(&[0xFF]));
    assert!(!lock_prefix_valid(&[0x0F, 0xC1]));
}

#[test]
fn test_lock_prefix_valid_add_mem() {
    // LOCK ADD [BX], AL (0xF0 0x00 0x07) — valid: ADD with memory destination
    let (mut cpu, mut mem) = setup(&[0xF0, 0x00, 0x07]);
    cpu.regs.ebx = 0x200;
    cpu.regs.eax = 1;
    mem.write_u8(0x200, 5);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "LOCK ADD [mem] should succeed, got {result:?}"
    );
    assert_eq!(mem.read_u8(0x200), 6);
}

#[test]
fn test_lock_add_mem_reg_valid() {
    // LOCK ADD [BX+SI], AX — valid LOCK usage (memory destination, word-sized)
    // 0xF0 0x01 0x00 = LOCK ADD [BX+SI], AX
    let (mut cpu, mut mem) = setup(&[0xF0, 0x01, 0x00]);
    cpu.regs.eax = 0x0005;
    cpu.regs.ebx = 0x0200;
    cpu.regs.esi = 0x0000;
    let addr = 0x200usize;
    mem.ram[addr] = 0x0A;
    mem.ram[addr + 1] = 0x00;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "LOCK ADD [mem], reg should execute normally, got {:?}",
        result
    );
    // 0x000A + 0x0005 = 0x000F
    assert_eq!(mem.ram[addr], 0x0F);
    assert_eq!(mem.ram[addr + 1], 0x00);
}

#[test]
fn test_lock_mov_raises_ud() {
    // LOCK MOV AX, BX (0xF0 0x89 0xD8) — MOV is not lockable → #UD
    let (mut cpu, mut mem) = setup(&[0xF0, 0x89, 0xD8]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 6, .. }),
        "LOCK MOV should raise #UD, got {:?}",
        result
    );
}

#[test]
fn test_lock_add_reg_reg_raises_ud() {
    // LOCK ADD AX, CX (0xF0 0x01 0xC8) — register destination → #UD
    // 0x01 0xC8: ADD AX, CX (modrm=0xC8 → mod=11, reg dest)
    let (mut cpu, mut mem) = setup(&[0xF0, 0x01, 0xC8]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 6, .. }),
        "LOCK ADD reg, reg should raise #UD, got {:?}",
        result
    );
}

#[test]
fn test_lock_inc_mem_valid() {
    // LOCK INC WORD [0x200] — valid LOCK usage
    // 0xF0 0xFF 0x06 0x00 0x02 = LOCK INC WORD PTR [0x0200]
    let (mut cpu, mut mem) = setup(&[0xF0, 0xFF, 0x06, 0x00, 0x02]);
    let addr = 0x200usize;
    mem.ram[addr] = 0x07;
    mem.ram[addr + 1] = 0x00;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "LOCK INC [mem] should execute normally, got {:?}",
        result
    );
    assert_eq!(mem.ram[addr], 0x08);
}

#[test]
fn test_lock_xchg_reg_reg_raises_ud() {
    // LOCK XCHG AX, CX (0xF0 0x87 0xC8) — register destination → #UD
    let (mut cpu, mut mem) = setup(&[0xF0, 0x87, 0xC8]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 6, .. }),
        "LOCK XCHG reg, reg should raise #UD, got {:?}",
        result
    );
}

#[test]
fn test_lock_group1_cmp_mem_raises_ud() {
    // LOCK CMP [BX+SI], 0x05 (0xF0 0x83 0x38 0x05) — CMP is read-only → #UD
    // modrm=0x38: mod=00, reg=111 (CMP /7), rm=000 ([BX+SI])
    let (mut cpu, mut mem) = setup(&[0xF0, 0x83, 0x38, 0x05]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 6, .. }),
        "LOCK CMP should raise #UD, got {:?}",
        result
    );
}

#[test]
fn test_lock_neg_mem_valid() {
    // LOCK NEG BYTE [BX+SI] (0xF0 0xF6 0x18) — valid LOCK usage
    // 0xF6 modrm=0x18: mod=00, reg=011 (NEG /3), rm=000 ([BX+SI])
    let (mut cpu, mut mem) = setup(&[0xF0, 0xF6, 0x18]);
    cpu.regs.ebx = 0x0200;
    cpu.regs.esi = 0x0000;
    let addr = 0x200usize;
    mem.ram[addr] = 0x05;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "LOCK NEG [mem] should execute normally, got {:?}",
        result
    );
    // NEG 5 = -5 = 0xFB (two's complement)
    assert_eq!(mem.ram[addr], 0xFB);
}
