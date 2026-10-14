use super::*;
use gp64_cpu::Flags;
use gp64_memory::Memory;

/// Helper: set up a real-mode CPU+Memory with code placed at `code_addr`,
/// CS=0, IP=code_addr, SS=0, SP=0x8000 (stack grows down from 0x8000).
fn setup(code_addr: u16, code: &[u8]) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cs = 0x0000;
    cpu.regs.set_ip(code_addr);
    cpu.regs.set_sp(0x8000);
    cpu.regs.esp = 0x8000;
    cpu.regs.ss = 0x0000;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[code_addr as usize + i] = b;
    }
    (cpu, mem)
}

// ====================================================================
// 1. Near CALL (0xE8) + RET (0xC3) round-trip with budget > 2
// ====================================================================

#[test]
fn test_fast_path_call_near_ret_round_trip() {
    // Layout at 0x0100:
    //   0x0100: CALL +0x0003       (E8 03 00)  → target = 0x0103 + 3 = 0x0106
    //   0x0103: HLT                (F4)
    //   0x0104: NOP                (90)         padding
    //   0x0105: NOP                (90)         padding
    //   0x0106: INC AX             (40)         ← call target
    //   0x0107: RET                (C3)         → returns to 0x0103
    //   0x0108: (never reached)
    //
    // Execution: CALL (pushes 0x0103), INC AX, RET (pops 0x0103), HLT
    // Total: 4 instructions.  AX = 1.  SP = 0x8000 (push then pop cancel).
    let code: &[u8] = &[
        0xE8, 0x03, 0x00, // CALL +3           @ 0x0100 (3 bytes → return addr = 0x0103)
        0xF4, // HLT               @ 0x0103
        0x90, // NOP                @ 0x0104
        0x90, // NOP                @ 0x0105
        0x40, // INC AX             @ 0x0106
        0xC3, // RET                @ 0x0107
    ];
    let (mut cpu, mut mem) = setup(0x0100, code);
    cpu.regs.eax = 0;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 10);
    // CALL + INC AX + RET + HLT = 4 instructions, then halt
    assert!(
        matches!(result, RunResult::Halt { executed: 4 }),
        "Expected Halt after 4 instructions, got {:?}",
        result
    );
    assert_eq!(cpu.regs.eax & 0xFFFF, 1, "AX should be 1 after INC AX");
    assert_eq!(cpu.regs.eip, 0x0104, "EIP should be at HLT+1 (0x0104)");
    assert_eq!(
        cpu.regs.sp(),
        0x8000,
        "SP should be restored after CALL+RET"
    );
}

#[test]
fn test_fast_path_call_near_pushes_correct_return_address() {
    // Verify the pushed return address is correct by inspecting the stack.
    //   0x0200: CALL +0x0000       (E8 00 00)  → target = 0x0203 + 0 = 0x0203
    //   0x0203: HLT                (F4)
    // The CALL pushes 0x0203 (address of next instruction) onto the stack.
    let code: &[u8] = &[
        0xE8, 0x00, 0x00, // CALL +0           @ 0x0200
        0xF4, // HLT               @ 0x0203
    ];
    let (mut cpu, mut mem) = setup(0x0200, code);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 10);
    assert!(
        matches!(result, RunResult::Halt { executed: 2 }),
        "Expected Halt after 2 instructions, got {:?}",
        result
    );
    // Stack: SP was 0x8000, CALL pushes 2 bytes → SP = 0x7FFE
    // Pushed value at [SS:0x7FFE] should be 0x0203 (return address)
    assert_eq!(
        cpu.regs.sp(),
        0x7FFE,
        "SP should be 0x7FFE after CALL (no RET)"
    );
    let pushed_lo = mem.ram[0x7FFE] as u16;
    let pushed_hi = mem.ram[0x7FFF] as u16;
    let pushed_ip = pushed_lo | (pushed_hi << 8);
    assert_eq!(pushed_ip, 0x0203, "Pushed return address should be 0x0203");
}

// ====================================================================
// 2. Near CALL indirect (FF/2) via register
// ====================================================================

#[test]
fn test_fast_path_call_near_indirect_register() {
    // FF D0 = CALL AX (ModR/M = 0xD0: mod=11, reg=010 (/2), rm=000 (AX))
    //   0x0100: CALL AX            (FF D0)     → target = AX = 0x0106
    //   0x0102: HLT                (F4)
    //   0x0103: NOP                (90)         padding
    //   0x0104: NOP                (90)         padding
    //   0x0105: NOP                (90)         padding
    //   0x0106: INC BX             (43)         ← call target
    //   0x0107: RET                (C3)         → returns to 0x0102
    let code: &[u8] = &[
        0xFF, 0xD0, // CALL AX            @ 0x0100
        0xF4, // HLT                @ 0x0102
        0x90, // NOP                 @ 0x0103
        0x90, // NOP                 @ 0x0104
        0x90, // NOP                 @ 0x0105
        0x43, // INC BX              @ 0x0106
        0xC3, // RET                 @ 0x0107
    ];
    let (mut cpu, mut mem) = setup(0x0100, code);
    cpu.regs.eax = 0x0106; // target address in AX
    cpu.regs.ebx = 0;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 10);
    assert!(
        matches!(result, RunResult::Halt { executed: 4 }),
        "Expected Halt after 4 (CALL, INC BX, RET, HLT), got {:?}",
        result
    );
    assert_eq!(cpu.regs.ebx & 0xFFFF, 1, "BX should be 1 after INC BX");
    assert_eq!(
        cpu.regs.sp(),
        0x8000,
        "SP should be restored after CALL+RET"
    );
}

#[test]
fn test_fast_path_call_near_indirect_memory() {
    // FF 16 lo hi = CALL [disp16]  (ModR/M = 0x16: mod=00, reg=010 (/2), rm=110 ([disp16]))
    // Store function pointer at 0x0300 → 0x0108
    //   0x0100: CALL [0x0300]      (FF 16 00 03)  → reads [0x0300] = 0x0108
    //   0x0104: HLT                (F4)
    //   0x0105: NOP                (90)
    //   0x0106: NOP                (90)
    //   0x0107: NOP                (90)
    //   0x0108: INC DX             (42)            ← call target
    //   0x0109: RET                (C3)
    let code: &[u8] = &[
        0xFF, 0x16, 0x00, 0x03, // CALL [0x0300]   @ 0x0100 (4 bytes → return addr = 0x0104)
        0xF4, // HLT             @ 0x0104
        0x90, // NOP              @ 0x0105
        0x90, // NOP              @ 0x0106
        0x90, // NOP              @ 0x0107
        0x42, // INC DX           @ 0x0108
        0xC3, // RET              @ 0x0109
    ];
    let (mut cpu, mut mem) = setup(0x0100, code);
    cpu.regs.edx = 0;
    // Store function pointer 0x0108 at address 0x0300 (little-endian)
    mem.ram[0x0300] = 0x08;
    mem.ram[0x0301] = 0x01;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 10);
    assert!(
        matches!(result, RunResult::Halt { executed: 4 }),
        "Expected Halt after 4 (CALL [mem], INC DX, RET, HLT), got {:?}",
        result
    );
    assert_eq!(cpu.regs.edx & 0xFFFF, 1, "DX should be 1 after INC DX");
    assert_eq!(
        cpu.regs.sp(),
        0x8000,
        "SP should be restored after CALL+RET"
    );
}

// ====================================================================
// 3. RET with imm16 (0xC2)
// ====================================================================

#[test]
fn test_fast_path_ret_imm16_adjusts_sp() {
    // Simulate a function that was called with 4 bytes of arguments on the stack.
    // The callee does RET 4 (0xC2 0x04 0x00) to pop IP + skip 4 bytes of args.
    //
    //   0x0100: CALL +0x0005       (E8 05 00)  → pushes 0x0103, target = 0x0108
    //   0x0103: HLT                (F4)
    //   0x0104-0x0107: padding
    //   0x0108: RET 4              (C2 04 00)  → pops 0x0103, SP += 4
    //
    // Before CALL: SP = 0x8000
    // After CALL:  SP = 0x7FFE (pushed 2-byte return address)
    // After RET 4: SP = 0x7FFE + 2 (pop IP) + 4 (imm16) = 0x8004
    let code: &[u8] = &[
        0xE8, 0x05, 0x00, // CALL +5            @ 0x0100
        0xF4, // HLT                @ 0x0103
        0x90, // NOP                 @ 0x0104
        0x90, // NOP                 @ 0x0105
        0x90, // NOP                 @ 0x0106
        0x90, // NOP                 @ 0x0107
        0xC2, 0x04, 0x00, // RET 4              @ 0x0108
    ];
    let (mut cpu, mut mem) = setup(0x0100, code);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 10);
    assert!(
        matches!(result, RunResult::Halt { executed: 3 }),
        "Expected Halt after 3 (CALL, RET 4, HLT), got {:?}",
        result
    );
    assert_eq!(cpu.regs.eip, 0x0104, "EIP should be at HLT+1 (0x0104)");
    // SP = 0x8000 - 2 (CALL push) + 2 (RET pop) + 4 (imm16) = 0x8004
    assert_eq!(
        cpu.regs.sp(),
        0x8004,
        "SP should be 0x8004 (original 0x8000 + 4 from RET imm16)"
    );
}

#[test]
fn test_fast_path_ret_imm16_zero() {
    // RET 0 should behave identically to RET (C3) — SP adjustment is 0.
    //   0x0100: CALL +0x0003       (E8 03 00)  → pushes 0x0103, target = 0x0106
    //   0x0103: HLT                (F4)
    //   0x0104-0x0105: padding
    //   0x0106: RET 0              (C2 00 00)
    let code: &[u8] = &[
        0xE8, 0x03, 0x00, // CALL +3            @ 0x0100
        0xF4, // HLT                @ 0x0103
        0x90, // NOP                 @ 0x0104
        0x90, // NOP                 @ 0x0105
        0xC2, 0x00, 0x00, // RET 0              @ 0x0106
    ];
    let (mut cpu, mut mem) = setup(0x0100, code);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 10);
    assert!(
        matches!(result, RunResult::Halt { executed: 3 }),
        "Expected Halt after 3 (CALL, RET 0, HLT), got {:?}",
        result
    );
    assert_eq!(cpu.regs.sp(), 0x8000, "SP should be 0x8000 (no adjustment)");
}

// ====================================================================
// 4. Jcc rel16 (0F 80-8F) — taken and not-taken
// ====================================================================

#[test]
fn test_fast_path_jcc_rel16_taken() {
    // 0F 85 = JNZ rel16.  With ZF=0 → taken.
    //   0x0100: JNZ +0x0005        (0F 85 05 00)  → target = 0x0104 + 5 = 0x0109
    //   0x0104: INC AX             (40)            ← skipped if taken
    //   0x0105: HLT                (F4)
    //   0x0106: NOP                (90)
    //   0x0107: NOP                (90)
    //   0x0108: NOP                (90)
    //   0x0109: INC BX             (43)            ← jump target
    //   0x010A: HLT                (F4)
    let code: &[u8] = &[
        0x0F, 0x85, 0x05, 0x00, // JNZ +5          @ 0x0100 (4 bytes → next = 0x0104)
        0x40, // INC AX           @ 0x0104 (not-taken path)
        0xF4, // HLT              @ 0x0105
        0x90, // NOP               @ 0x0106
        0x90, // NOP               @ 0x0107
        0x90, // NOP               @ 0x0108
        0x43, // INC BX            @ 0x0109 (taken path)
        0xF4, // HLT              @ 0x010A
    ];
    let (mut cpu, mut mem) = setup(0x0100, code);
    cpu.regs.eax = 0;
    cpu.regs.ebx = 0;
    cpu.regs.flags.remove(Flags::ZERO); // ZF=0 → JNZ taken
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 10);
    // JNZ (taken) + INC BX + HLT = 3 instructions
    assert!(
        matches!(result, RunResult::Halt { executed: 3 }),
        "Expected Halt after 3 (JNZ taken, INC BX, HLT), got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.ebx & 0xFFFF,
        1,
        "BX should be 1 (taken path executed)"
    );
    assert_eq!(
        cpu.regs.eax & 0xFFFF,
        0,
        "AX should be 0 (not-taken path skipped)"
    );
}

#[test]
fn test_fast_path_jcc_rel16_not_taken() {
    // 0F 85 = JNZ rel16.  With ZF=1 → not taken.
    //   0x0100: JNZ +0x0005        (0F 85 05 00)  → falls through to 0x0104
    //   0x0104: INC AX             (40)            ← executed (not-taken path)
    //   0x0105: HLT                (F4)
    //   0x0106-0x0108: NOP padding
    //   0x0109: INC BX             (43)            ← NOT reached
    //   0x010A: HLT                (F4)
    let code: &[u8] = &[
        0x0F, 0x85, 0x05, 0x00, // JNZ +5          @ 0x0100
        0x40, // INC AX           @ 0x0104
        0xF4, // HLT              @ 0x0105
        0x90, // NOP               @ 0x0106
        0x90, // NOP               @ 0x0107
        0x90, // NOP               @ 0x0108
        0x43, // INC BX            @ 0x0109
        0xF4, // HLT              @ 0x010A
    ];
    let (mut cpu, mut mem) = setup(0x0100, code);
    cpu.regs.eax = 0;
    cpu.regs.ebx = 0;
    cpu.regs.flags.insert(Flags::ZERO); // ZF=1 → JNZ not taken
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 10);
    // JNZ (not taken, falls through) + INC AX + HLT = 3 instructions
    assert!(
        matches!(result, RunResult::Halt { executed: 3 }),
        "Expected Halt after 3 (JNZ not-taken, INC AX, HLT), got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.eax & 0xFFFF,
        1,
        "AX should be 1 (not-taken path executed)"
    );
    assert_eq!(
        cpu.regs.ebx & 0xFFFF,
        0,
        "BX should be 0 (taken path NOT reached)"
    );
}

#[test]
fn test_fast_path_jcc_rel16_jz_taken() {
    // 0F 84 = JZ rel16.  With ZF=1 → taken.
    // Different condition code from the JNZ tests above.
    //   0x0100: JZ +0x0003         (0F 84 03 00)  → target = 0x0104 + 3 = 0x0107
    //   0x0104: INC AX             (40)
    //   0x0105: INC AX             (40)
    //   0x0106: HLT                (F4)
    //   0x0107: INC BX             (43)            ← jump target
    //   0x0108: HLT                (F4)
    let code: &[u8] = &[
        0x0F, 0x84, 0x03, 0x00, // JZ +3           @ 0x0100
        0x40, // INC AX           @ 0x0104
        0x40, // INC AX           @ 0x0105
        0xF4, // HLT              @ 0x0106
        0x43, // INC BX            @ 0x0107
        0xF4, // HLT              @ 0x0108
    ];
    let (mut cpu, mut mem) = setup(0x0100, code);
    cpu.regs.eax = 0;
    cpu.regs.ebx = 0;
    cpu.regs.flags.insert(Flags::ZERO); // ZF=1 → JZ taken
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 10);
    assert!(
        matches!(result, RunResult::Halt { executed: 3 }),
        "Expected Halt after 3 (JZ taken, INC BX, HLT), got {:?}",
        result
    );
    assert_eq!(cpu.regs.ebx & 0xFFFF, 1, "BX should be 1 (taken path)");
    assert_eq!(cpu.regs.eax & 0xFFFF, 0, "AX should be 0 (skipped)");
}

// ====================================================================
// 5. Multi-instruction loop with budget — DEC CX / JNZ
// ====================================================================

#[test]
fn test_fast_path_dec_cx_jnz_loop_budget_32() {
    // A tight loop: MOV CX, 8; .top: DEC CX; JNZ .top; HLT
    //   0x0100: MOV CX, 8          (B9 08 00)      3 bytes
    //   0x0103: DEC CX              (49)            1 byte
    //   0x0104: JNZ -3              (75 FD)         2 bytes → target = 0x0106 + (-3) = 0x0103
    //   0x0106: HLT                 (F4)
    //
    // Iterations: MOV(1) + 8*(DEC+JNZ) + HLT = 1 + 16 + 1 = 18 instructions
    // Budget=32 is sufficient to complete the loop.
    let code: &[u8] = &[
        0xB9, 0x08, 0x00, // MOV CX, 8          @ 0x0100
        0x49, // DEC CX              @ 0x0103
        0x75, 0xFD, // JNZ -3 (to 0x0103) @ 0x0104
        0xF4, // HLT                 @ 0x0106
    ];
    let (mut cpu, mut mem) = setup(0x0100, code);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 32);
    assert!(
        matches!(result, RunResult::Halt { executed: 18 }),
        "Expected Halt after 18 instructions (1 MOV + 8*(DEC+JNZ) + 1 HLT), got {:?}",
        result
    );
    assert_eq!(cpu.regs.cx(), 0, "CX should be 0 after loop completes");
    assert_eq!(cpu.regs.eip, 0x0107, "EIP should be past HLT (0x0107)");
}

#[test]
fn test_fast_path_loop_budget_exhaustion_mid_loop() {
    // Same loop structure but with a larger iteration count so the budget
    // runs out mid-loop.  Budget=32, loop count=20 → needs 1 + 20*2 + 1 = 42.
    // The loop should be interrupted at budget=32.
    let code: &[u8] = &[
        0xB9, 0x14, 0x00, // MOV CX, 20         @ 0x0100
        0x49, // DEC CX              @ 0x0103
        0x75, 0xFD, // JNZ -3 (to 0x0103) @ 0x0104
        0xF4, // HLT                 @ 0x0106
    ];
    let (mut cpu, mut mem) = setup(0x0100, code);
    let mut io = ();

    // First batch: budget=32
    let result = run(&mut cpu, &mut mem, &mut io, 32);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 32 }),
        "Expected BudgetExhausted at 32, got {:?}",
        result
    );
    // CX should have been decremented some number of times but NOT be 0 yet.
    assert!(cpu.regs.cx() > 0, "CX should still be > 0 mid-loop");

    // Second batch: finish the rest
    let result2 = run(&mut cpu, &mut mem, &mut io, 32);
    match result2 {
        RunResult::Halt { .. } => {
            assert_eq!(cpu.regs.cx(), 0, "CX should be 0 after loop completes");
        }
        RunResult::BudgetExhausted { .. } => {
            // Might need a third batch
            let result3 = run(&mut cpu, &mut mem, &mut io, 32);
            assert!(
                matches!(result3, RunResult::Halt { .. }),
                "Expected Halt after third batch, got {:?}",
                result3
            );
            assert_eq!(cpu.regs.cx(), 0, "CX should be 0 after loop completes");
        }
        other => panic!("Unexpected result from second batch: {:?}", other),
    }
}

#[test]
fn test_fast_path_nested_call_ret_with_budget() {
    // Test nested CALL/RET: func_a calls func_b, both return correctly.
    // Budget=32 to exercise multi-instruction execution across nested calls.
    //
    //   0x0100: CALL func_a        (E8 05 00)  → 0x0108
    //   0x0103: HLT                (F4)
    //   0x0104-0x0107: padding
    //   0x0108: INC AX             (40)         func_a: body
    //   0x0109: CALL func_b        (E8 03 00)  → 0x010F
    //   0x010C: RET                (C3)         func_a: return
    //   0x010D-0x010E: padding
    //   0x010F: INC BX             (43)         func_b: body
    //   0x0110: RET                (C3)         func_b: return
    let code: &[u8] = &[
        0xE8, 0x05, 0x00, // CALL func_a        @ 0x0100 → 0x0108
        0xF4, // HLT                @ 0x0103
        0x90, // NOP                 @ 0x0104
        0x90, // NOP                 @ 0x0105
        0x90, // NOP                 @ 0x0106
        0x90, // NOP                 @ 0x0107
        0x40, // INC AX (func_a)     @ 0x0108
        0xE8, 0x03, 0x00, // CALL func_b        @ 0x0109 → 0x010F
        0xC3, // RET (func_a)        @ 0x010C
        0x90, // NOP                 @ 0x010D
        0x90, // NOP                 @ 0x010E
        0x43, // INC BX (func_b)     @ 0x010F
        0xC3, // RET (func_b)        @ 0x0110
    ];
    let (mut cpu, mut mem) = setup(0x0100, code);
    cpu.regs.eax = 0;
    cpu.regs.ebx = 0;
    let mut io = ();

    // Execution order:
    //   1. CALL func_a (push 0x0103)
    //   2. INC AX
    //   3. CALL func_b (push 0x010C)
    //   4. INC BX
    //   5. RET → 0x010C
    //   6. RET → 0x0103
    //   7. HLT
    // Total: 7 instructions
    let result = run(&mut cpu, &mut mem, &mut io, 32);
    assert!(
        matches!(result, RunResult::Halt { executed: 7 }),
        "Expected Halt after 7 instructions (nested CALL/RET), got {:?}",
        result
    );
    assert_eq!(cpu.regs.eax & 0xFFFF, 1, "AX should be 1");
    assert_eq!(cpu.regs.ebx & 0xFFFF, 1, "BX should be 1");
    assert_eq!(cpu.regs.sp(), 0x8000, "SP should be fully restored");
}
