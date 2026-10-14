use super::*;
use gp64_memory::Memory;

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

// ---- Test 1: Budget exhaustion at exactly 32 ----

#[test]
fn test_gap2_budget_exhaustion_exactly_32() {
    // Place 50 NOPs (0x90) at 0x0100
    let code = [0x90u8; 50];
    let (mut cpu, mut mem) = setup(0x0100, &code);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 32);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 32 }),
        "Expected BudgetExhausted with 32, got {:?}",
        result
    );
    // IP should have advanced by 32 NOPs
    assert_eq!(cpu.regs.ip(), 0x0100 + 32);
}

// ---- Test 2: Multi-instruction loop ----

#[test]
fn test_gap2_multi_instruction_loop() {
    // Loop: MOV CX, 5; top: DEC CX; JNZ top; HLT
    // MOV CX, imm16 = B9 05 00
    // DEC CX = 49         (at offset +3)
    // JNZ rel8 = 75 FC    (at offset +4; rel8 = -2 from end of JNZ, back to DEC at +3)
    //                      (JNZ is 2 bytes at +4/+5; next instr at +6; target = +6 + FC(-4) = +2? No.)
    //                      Actually: JNZ encodes relative to next instruction.
    //                      DEC CX is at 0x0103. JNZ at 0x0104 (2 bytes) → next = 0x0106.
    //                      To jump to 0x0103, rel8 = 0x0103 - 0x0106 = -3 = 0xFD.
    // HLT = F4
    let code: &[u8] = &[
        0xB9, 0x05, 0x00, // MOV CX, 5        (0x0100)
        0x49, // DEC CX            (0x0103)
        0x75, 0xFD, // JNZ -3 (to 0x0103)(0x0104)
        0xF4, // HLT               (0x0106)
    ];
    let (mut cpu, mut mem) = setup(0x0100, code);
    let mut io = ();

    // The loop runs: MOV(1) + 5*(DEC+JNZ) + HLT = 1 + 10 + 1 = 12 instructions
    // (Last iteration: DEC CX makes CX=0, ZF=1, JNZ falls through, then HLT)
    let result = run(&mut cpu, &mut mem, &mut io, 32);
    assert!(
        matches!(result, RunResult::Halt { executed: 12 }),
        "Expected Halt after 12 instructions, got {:?}",
        result
    );
    assert_eq!(cpu.regs.cx(), 0, "CX should be 0 after loop");
}

// ---- Test 3 (mid-batch HLE detection) and the real-boot gate moved to the machine execution layer
// (M2.3): the sentinels live in PcExec. ----

// ---- Test 4: STI returns with the interrupt shadow armed ----

#[test]
fn test_gap2_interrupt_inhibit_after_sti() {
    // STI (0xFB) followed by NOPs. The interpreter must return after STI;
    // timing.rs owns the one instruction protected by the armed shadow.
    let code: &[u8] = &[
        0xFB, // STI
        0x90, // NOP (runs within inhibit window)
        0x90, // NOP (should NOT run — budget capped)
    ];
    let (mut cpu, mut mem) = setup(0x0100, code);
    cpu.interrupts_enabled = false; // start with IF=0 so STI changes it
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 32);
    // STI alone retires before the interrupt-poll boundary.
    assert!(
        matches!(result, RunResult::InterruptPollBoundary { executed: 1 }),
        "Expected STI alone before the poll boundary, got {:?}",
        result
    );
    assert_eq!(cpu.regs.ip(), 0x0101, "IP should point at the shadowed NOP");
    assert_eq!(cpu.interrupt_inhibit, 1, "STI must leave its shadow armed");

    let successor = run(&mut cpu, &mut mem, &mut io, 32);
    assert!(
        matches!(successor, RunResult::BudgetExhausted { executed: 1 }),
        "an armed-shadow re-entry may retire only one successor, got {:?}",
        successor
    );
    assert_eq!(cpu.regs.ip(), 0x0102, "only the first NOP may retire");
    assert_eq!(
        cpu.interrupt_inhibit, 0,
        "the one-instruction shadow must be consumed"
    );

    cpu.interrupt_inhibit = 1;
    let zero_retire = run(&mut cpu, &mut mem, &mut io, 0);
    assert!(matches!(
        zero_retire,
        RunResult::BudgetExhausted { executed: 0 }
    ));
    assert_eq!(
        cpu.interrupt_inhibit, 1,
        "a zero-retire boundary must not consume the shadow"
    );

    mem.ram[0x0102] = 0x0F;
    mem.ram[0x0103] = 0x0B; // UD2
    let exception = run(&mut cpu, &mut mem, &mut io, 32);
    assert!(matches!(
        exception,
        RunResult::Exception {
            vector: 6,
            executed: 0,
            ..
        }
    ));
    assert_eq!(
        cpu.interrupt_inhibit, 0,
        "an exception in the protected instruction must consume the shadow"
    );
}

// ---- Test 5: Mid-batch exception (DIV by zero) ----

#[test]
fn test_gap2_mid_batch_exception() {
    // 3 NOPs, then DIV CL with CL=0
    // NOP x3 = 90 90 90
    // XOR CX,CX = 31 C9 (2 bytes)
    // DIV CL = F6 F1 (2 bytes)
    let code: &[u8] = &[
        0x90, 0x90, 0x90, // 3 NOPs
        0x31, 0xC9, // XOR CX, CX
        0xF6, 0xF1, // DIV CL (divide by zero)
    ];
    let (mut cpu, mut mem) = setup(0x0100, code);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 32);
    // 3 NOPs + XOR + DIV = 5 instructions attempted, DIV faults
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 0,
                executed: 4,
                ..
            }
        ),
        "Expected #DE (vector 0) after 4 successful instructions, got {:?}",
        result
    );
}

// ---- Test 6: Lazy flag state preserved across batch ----

#[test]
fn test_gap2_lazy_flags_across_batch() {
    // ADD AX, 1; NOP; NOP; NOP; JZ skip_to_hlt; HLT (success path); HLT (fail path)
    // AX starts at 0 → ADD sets AX=1, clears ZF → JZ should NOT be taken
    // ADD AX, imm16 = 05 01 00     (0x0100, 3 bytes)
    // NOP x3 = 90 90 90             (0x0103, 3 bytes)
    // JZ rel8 = 74 01               (0x0106, 2 bytes) skip 1 byte forward if ZF
    // HLT = F4                      (0x0108, success — JZ not taken)
    // HLT = F4                      (0x0109, would reach if JZ taken)
    let code: &[u8] = &[
        0x05, 0x01, 0x00, // ADD AX, 1
        0x90, 0x90, 0x90, // 3 NOPs
        0x74, 0x01, // JZ +1 (to 0x0109 — should NOT be taken since AX=1, ZF=0)
        0xF4, // HLT (success path at 0x0108)
        0xF4, // HLT (fail path at 0x0109)
    ];
    let (mut cpu, mut mem) = setup(0x0100, code);
    cpu.regs.set_ax(0);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 32);
    // ADD(1) + 3 NOP(2,3,4) + JZ(5) + HLT(6) = 6 instructions
    assert!(
        matches!(result, RunResult::Halt { executed: 6 }),
        "Expected Halt after 6 instructions, got {:?}",
        result
    );
    assert_eq!(cpu.regs.ip(), 0x0109, "IP should be past success-path HLT");
    assert_eq!(cpu.regs.ax(), 1, "AX should be 1");
}
