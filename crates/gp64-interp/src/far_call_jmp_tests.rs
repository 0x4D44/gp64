use super::test_helpers::{make_descriptor, write_gdt_entry};
use super::*;
use gp64_memory::Memory;

fn setup(code_addr: u16, code: &[u8]) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cs = 0x0000;
    cpu.regs.set_ip(code_addr);
    cpu.regs.set_sp(0x1000);
    cpu.regs.esp = 0x1000;
    cpu.regs.ss = 0x0000;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[code_addr as usize + i] = b;
    }
    (cpu, mem)
}

// ---- C1 + C2: CALL far (0x9A) round-trip with RETF ----

#[test]
fn test_c1_c2_call_far_9a_16bit_round_trip() {
    // CALL far 0x0000:0x0300 at address 0x0100
    // 0x9A 0x00 0x03 0x00 0x00 = CALL far ptr16:16 (5 bytes)
    // At 0x0300: RETF (0xCB)
    let (mut cpu, mut mem) = setup(0x0100, &[0x9A, 0x00, 0x03, 0x00, 0x00]);
    mem.ram[0x0300] = 0xCB; // RETF at target

    let mut io = ();

    // Execute CALL far (budget=1)
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "CALL far should succeed, got {:?}",
        result
    );
    // C1: IP should point to target (0x0300), not next sequential (0x0105)
    assert_eq!(
        cpu.regs.ip(),
        0x0300,
        "IP should be at target after CALL far"
    );
    assert_eq!(
        cpu.regs.eip, 0x0300,
        "EIP should be at target after CALL far"
    );

    // C2: Return address on stack should be 0x0105 (next instruction), not 0x0100 (CALL itself)
    // Stack was at 0x1000, push16(CS=0x0000) then push16(return_ip)
    // After two push16: SP = 0x1000 - 4 = 0x0FFC
    assert_eq!(cpu.regs.sp(), 0x0FFC, "SP should have two 16-bit pushes");
    // Return IP is at SP (0x0FFC), return CS is at SP+2 (0x0FFE)
    let return_ip = u16::from_le_bytes([mem.ram[0x0FFC], mem.ram[0x0FFD]]);
    let return_cs = u16::from_le_bytes([mem.ram[0x0FFE], mem.ram[0x0FFF]]);
    assert_eq!(
        return_ip, 0x0105,
        "Return IP should be address after CALL far (0x0100 + 5)"
    );
    assert_eq!(return_cs, 0x0000, "Return CS should be original CS");

    // Execute RETF (budget=1) — should return to 0x0105
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Retf { .. }),
        "RETF should succeed, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.ip(),
        0x0105,
        "IP should return to instruction after CALL far"
    );
}

// ---- C1: JMP far (0xEA) ----

#[test]
fn test_c1_jmp_far_ea_16bit() {
    // JMP far 0x0000:0x0400 at address 0x0100
    // 0xEA 0x00 0x04 0x00 0x00 = JMP far ptr16:16 (5 bytes)
    let (mut cpu, mut mem) = setup(0x0100, &[0xEA, 0x00, 0x04, 0x00, 0x00]);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "JMP far should succeed, got {:?}",
        result
    );
    // C1: IP should point to target (0x0400), not next sequential (0x0105)
    assert_eq!(
        cpu.regs.ip(),
        0x0400,
        "IP should be at target after JMP far"
    );
    assert_eq!(
        cpu.regs.eip, 0x0400,
        "EIP should be at target after JMP far"
    );
    // JMP far should not modify SP (no push)
    assert_eq!(
        cpu.regs.sp(),
        0x1000,
        "SP should be unchanged after JMP far"
    );
}

// ---- C1 + C2: FF/3 CALL far indirect round-trip ----

#[test]
fn test_c1_c2_call_far_indirect_ff3_16bit_round_trip() {
    // FF /3 with ModR/M for memory operand: CALL FAR [addr]
    // We'll use ModR/M = 0x1E (sub_op=3, mod=00, rm=110 = disp16)
    // So the encoding is: FF 1E disp_lo disp_hi
    // The far pointer at the memory address will be: offset=0x0500, segment=0x0000
    let code_addr: u16 = 0x0200;
    let ptr_addr: u16 = 0x0600;
    // FF 1E 00 06 = CALL FAR [0x0600] (4 bytes)
    let (mut cpu, mut mem) = setup(
        code_addr,
        &[0xFF, 0x1E, ptr_addr as u8, (ptr_addr >> 8) as u8],
    );
    // Store far pointer at 0x0600: offset=0x0500, segment=0x0000
    mem.ram[ptr_addr as usize] = 0x00;
    mem.ram[ptr_addr as usize + 1] = 0x05; // offset = 0x0500
    mem.ram[ptr_addr as usize + 2] = 0x00;
    mem.ram[ptr_addr as usize + 3] = 0x00; // segment = 0x0000
                                           // RETF at target
    mem.ram[0x0500] = 0xCB;

    let mut io = ();

    // Execute CALL far indirect
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "CALL far indirect should succeed, got {:?}",
        result
    );
    // C1: IP should be at target
    assert_eq!(
        cpu.regs.ip(),
        0x0500,
        "IP should be at target after CALL far indirect"
    );

    // C2: Return address should be code_addr + 4 (instruction length)
    let expected_return = code_addr + 4;
    // Stack had two push16s
    let return_ip = u16::from_le_bytes([
        mem.ram[cpu.regs.sp() as usize],
        mem.ram[cpu.regs.sp() as usize + 1],
    ]);
    assert_eq!(
        return_ip, expected_return,
        "Return IP should be instruction after CALL far indirect"
    );

    // Execute RETF — should return to code_addr + 4
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Retf { .. }),
        "RETF should succeed, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.ip(),
        expected_return,
        "IP should return to instruction after CALL far indirect"
    );
}

// ---- C1: FF/5 JMP far indirect ----

#[test]
fn test_c1_jmp_far_indirect_ff5_16bit() {
    // FF /5 with ModR/M for memory operand: JMP FAR [addr]
    // ModR/M = 0x2E (sub_op=5, mod=00, rm=110 = disp16)
    // FF 2E disp_lo disp_hi
    let code_addr: u16 = 0x0200;
    let ptr_addr: u16 = 0x0700;
    let (mut cpu, mut mem) = setup(
        code_addr,
        &[0xFF, 0x2E, ptr_addr as u8, (ptr_addr >> 8) as u8],
    );
    // Store far pointer at 0x0700: offset=0x0800, segment=0x0000
    mem.ram[ptr_addr as usize] = 0x00;
    mem.ram[ptr_addr as usize + 1] = 0x08; // offset = 0x0800
    mem.ram[ptr_addr as usize + 2] = 0x00;
    mem.ram[ptr_addr as usize + 3] = 0x00; // segment = 0x0000

    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "JMP far indirect should succeed, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.ip(),
        0x0800,
        "IP should be at target after JMP far indirect"
    );
    assert_eq!(
        cpu.regs.sp(),
        0x1000,
        "SP should be unchanged after JMP far indirect"
    );
}

#[test]
fn test_jmp_far_indirect_ff5_wraps_selector_offset_at_ffff() {
    // FF 29 = JMP FAR [BX+DI]. BX+DI wraps to FFFE, so the offset
    // word is at DS:FFFE and the selector word starts at DS:0000.
    let (mut cpu, mut mem) = setup(0x0200, &[0xFF, 0x29]);
    cpu.regs.ds = 0x1000;
    cpu.regs.ds_cache = SegmentCache::real_mode(0x1000);
    cpu.regs.set_bx(0xFFFF);
    cpu.regs.set_di(0xFFFF);
    mem.ram[0x1_FFFE] = 0x56;
    mem.ram[0x1_FFFF] = 0x34;
    mem.ram[0x1_0000] = 0x00;
    mem.ram[0x1_0001] = 0x20;

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eip, 0x3456);
    assert_eq!(cpu.regs.cs, 0x2000);
}

// ---- C3: CALL far 0x9A error path restores SP ----

fn setup_pm(cpu: &mut Cpu, mem: &mut Memory) {
    let gdt_base: u32 = 0x10000;
    let gdt_limit: u16 = 7 * 8 - 1;
    write_gdt_entry(mem, gdt_base as usize, [0u8; 8]); // null
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x08,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x08),
    ); // ring-0 code
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x10,
        make_descriptor(0, 0xFFFFF, 0x92, 0x08),
    ); // ring-0 data
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x18,
        make_descriptor(0, 0xFFFFF, 0xFA, 0x08),
    ); // ring-3 code
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x20,
        make_descriptor(0, 0xFFFFF, 0xF2, 0x08),
    ); // ring-3 data
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x28,
        make_descriptor(0, 0xFFFFF, 0x82, 0x00),
    ); // system (invalid for CS)
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x30,
        make_descriptor(0, 0xFFFFF, 0x12, 0x08),
    ); // not-present data
    cpu.regs.gdtr.base = gdt_base;
    cpu.regs.gdtr.limit = gdt_limit;
    cpu.regs.cr0 |= 1;
}

#[test]
fn test_c3_call_far_9a_error_restores_sp() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    setup_pm(&mut cpu, &mut mem);

    // Ring-3 setup
    cpu.regs.cs = 0x1B;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x1B).unwrap();
    cpu.cs_bits = 16;
    cpu.regs.cpl = 3;
    cpu.regs.ss = 0x23;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x23).unwrap();
    cpu.regs.ds = 0x23;
    cpu.regs.ds_cache = cpu.read_descriptor(&mem, 0x23).unwrap();
    cpu.regs.set_sp(0x5000);
    cpu.regs.esp = 0x5000;

    // CALL far 0x0028:0x1000 — selector 0x28 is a system descriptor, invalid for CS
    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0x9A;
    mem.ram[0x2001] = 0x00;
    mem.ram[0x2002] = 0x10; // offset = 0x1000
    mem.ram[0x2003] = 0x28;
    mem.ram[0x2004] = 0x00; // selector = 0x0028

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 13, .. }),
        "should get #GP, got {:?}",
        result
    );
    // C3: SP/ESP should be restored to original values
    assert_eq!(
        cpu.regs.sp(),
        0x5000,
        "SP must be restored after CALL far error"
    );
    assert_eq!(
        cpu.regs.esp, 0x5000,
        "ESP must be restored after CALL far error"
    );
}

// ---- C3: FF/3 CALL far indirect error path restores SP ----

#[test]
fn test_c3_call_far_indirect_ff3_error_restores_sp() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    setup_pm(&mut cpu, &mut mem);

    // Ring-3 setup
    cpu.regs.cs = 0x1B;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x1B).unwrap();
    cpu.cs_bits = 16;
    cpu.regs.cpl = 3;
    cpu.regs.ss = 0x23;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x23).unwrap();
    cpu.regs.ds = 0x23;
    cpu.regs.ds_cache = cpu.read_descriptor(&mem, 0x23).unwrap();
    cpu.regs.set_sp(0x5000);
    cpu.regs.esp = 0x5000;

    // FF /3 CALL far indirect [0x3000] — bad selector at memory
    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0xFF;
    mem.ram[0x2001] = 0x1E; // ModR/M: sub_op=3, mod=00, rm=110 (disp16)
    mem.ram[0x2002] = 0x00;
    mem.ram[0x2003] = 0x30; // disp16 = 0x3000
                            // Far pointer at 0x3000: offset=0x1000, selector=0x0028 (system, invalid for CS)
    mem.ram[0x3000] = 0x00;
    mem.ram[0x3001] = 0x10; // offset = 0x1000
    mem.ram[0x3002] = 0x28;
    mem.ram[0x3003] = 0x00; // selector = 0x0028

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 13, .. }),
        "should get #GP, got {:?}",
        result
    );
    // C3: SP/ESP should be restored
    assert_eq!(
        cpu.regs.sp(),
        0x5000,
        "SP must be restored after CALL far indirect error"
    );
    assert_eq!(
        cpu.regs.esp, 0x5000,
        "ESP must be restored after CALL far indirect error"
    );
}

// ---- C3: CALL far error path must pop ret_guard ----

#[test]
fn test_c3_call_far_error_retguard_clean() {
    // the ret_guard enable env var was retired (gp64-cpu now toggles it
    // programmatically). The interpreter's CALL-far push/pop balance is unconditional,
    // so this test needs no enable step.
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    setup_pm(&mut cpu, &mut mem);

    // Ring-3 setup
    cpu.regs.cs = 0x1B;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x1B).unwrap();
    cpu.cs_bits = 16;
    cpu.regs.cpl = 3;
    cpu.regs.ss = 0x23;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x23).unwrap();
    cpu.regs.ds = 0x23;
    cpu.regs.ds_cache = cpu.read_descriptor(&mem, 0x23).unwrap();
    cpu.regs.set_sp(0x5000);
    cpu.regs.esp = 0x5000;

    let depth_before = cpu.ret_guard_depth();

    // CALL far 0x0028:0x1000 — selector 0x28 is a system descriptor, invalid for CS
    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0x9A;
    mem.ram[0x2001] = 0x00;
    mem.ram[0x2002] = 0x10; // offset = 0x1000
    mem.ram[0x2003] = 0x28;
    mem.ram[0x2004] = 0x00; // selector = 0x0028

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 13, .. }),
        "should get #GP, got {:?}",
        result
    );
    assert_eq!(
        cpu.ret_guard_depth(),
        depth_before,
        "ret_guard depth must be unchanged after CALL far error"
    );

    // Also test FF/3 indirect path
    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    cpu.regs.set_sp(0x5000);
    cpu.regs.esp = 0x5000;

    let depth_before = cpu.ret_guard_depth();

    mem.ram[0x2000] = 0xFF;
    mem.ram[0x2001] = 0x1E; // ModR/M: sub_op=3, mod=00, rm=110 (disp16)
    mem.ram[0x2002] = 0x00;
    mem.ram[0x2003] = 0x30; // disp16 = 0x3000
                            // Far pointer at 0x3000: offset=0x1000, selector=0x0028 (system, invalid for CS)
    mem.ram[0x3000] = 0x00;
    mem.ram[0x3001] = 0x10;
    mem.ram[0x3002] = 0x28;
    mem.ram[0x3003] = 0x00;

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 13, .. }),
        "FF/3 should get #GP, got {:?}",
        result
    );
    assert_eq!(
        cpu.ret_guard_depth(),
        depth_before,
        "ret_guard depth must be unchanged after CALL far indirect error"
    );
}

// ---- 32-bit CALL far (0x66 0x9A) in 16-bit mode ----

#[test]
fn test_call_far_9a_32bit_operand_size() {
    // 0x66 0x9A imm32 imm16 — CALL far ptr16:32 with operand size override
    // offset = 0x00000300 (32-bit), selector = 0x0000
    let (mut cpu, mut mem) = setup(
        0x0100,
        &[
            0x66, 0x9A, 0x00, 0x03, 0x00, 0x00, // offset = 0x00000300 (little-endian u32)
            0x00, 0x00, // selector = 0x0000
        ],
    );
    mem.ram[0x0300] = 0x90; // NOP at target

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "32-bit CALL far should succeed, got {:?}",
        result
    );
    assert_eq!(cpu.regs.eip, 0x0300, "EIP should be at target");
    assert_eq!(
        cpu.regs.ip(),
        0x0300,
        "IP should be at target (low 16 bits)"
    );
    // Return address = 0x0100 + 8 (prefix + opcode + 4-byte offset + 2-byte selector) = 0x0108
    // Stack should have CS:EIP pushed as 32-bit values (push32 twice)
    let sp = cpu.regs.sp();
    // Check pushed return EIP (top of stack)
    let pushed_eip = u32::from_le_bytes([
        mem.ram[sp as usize],
        mem.ram[sp as usize + 1],
        mem.ram[sp as usize + 2],
        mem.ram[sp as usize + 3],
    ]);
    assert_eq!(pushed_eip, 0x0108, "pushed return EIP should be 0x0108");
}

// ---- CALL far with budget=2 — verify target instruction executes ----

#[test]
fn test_call_far_9a_budget_2_executes_target() {
    // CALL far 0x0000:0x0300 at 0x0100, then NOP at 0x0300
    let (mut cpu, mut mem) = setup(0x0100, &[0x9A, 0x00, 0x03, 0x00, 0x00]);
    // At target: INC AX (0x40)
    mem.ram[0x0300] = 0x40;

    cpu.regs.eax = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "should execute 2 instructions (CALL far + INC AX), got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.eax & 0xFFFF,
        1,
        "AX should be 1 after INC AX at target"
    );
    assert_eq!(cpu.regs.ip(), 0x0301, "IP should advance past INC AX");
}

// ---- CALL far with segment override prefix — return IP includes prefix ----

#[test]
fn test_call_far_9a_with_segment_prefix_return_ip() {
    // CS: 0x2E prefix before CALL far should still be counted in instruction length
    // 0x2E 0x9A 0x00 0x03 0x00 0x00 = 6 bytes total
    // return_ip = 0x0100 + 6 = 0x0106
    let (mut cpu, mut mem) = setup(0x0100, &[0x2E, 0x9A, 0x00, 0x03, 0x00, 0x00]);
    mem.ram[0x0300] = 0xCB; // RETF at target

    let mut io = ();
    // Execute CALL far (budget=1)
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "CALL far should succeed, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.ip(),
        0x0300,
        "IP should be at target after CALL far"
    );

    // Check return address on stack includes the prefix byte
    let sp = cpu.regs.sp();
    let return_ip = u16::from_le_bytes([mem.ram[sp as usize], mem.ram[sp as usize + 1]]);
    assert_eq!(
        return_ip, 0x0106,
        "Return IP should be 0x0106 (prefix + CALL far = 6 bytes)"
    );

    // Execute RETF (budget=1) — should return past prefix + CALL far
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Retf { .. }),
        "RETF should succeed, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.ip(),
        0x0106,
        "IP should return to 0x0106 (after prefix + CALL far)"
    );
}

// ---- FF/3 and FF/5 with mod=11 (register operand) must raise #UD ----

#[test]
fn test_call_far_indirect_ff3_register_operand_raises_ud() {
    // FF DE = FF /3 with mod=11, rm=110 (SI register)
    // CALL FAR is memory-only; mod=11 is #UD on real hardware.
    let (mut cpu, mut mem) = setup(0x0100, &[0xFF, 0xDE]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 6,
                error_code: 0,
                ..
            }
        ),
        "FF /3 with register operand (mod=11) should raise #UD, got {:?}",
        result
    );
    // IP should be rewound to the faulting instruction
    assert_eq!(
        cpu.regs.ip(),
        0x0100,
        "IP should point to faulting instruction"
    );
}

#[test]
fn test_call_far_indirect_ff3_register_sp_raises_ud() {
    // FF DC = FF /3 with mod=11, rm=100 (SP register)
    // Same bug as above but with different rm field.
    let (mut cpu, mut mem) = setup(0x0100, &[0xFF, 0xDC]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 6,
                error_code: 0,
                ..
            }
        ),
        "FF /3 with register operand (mod=11, rm=SP) should raise #UD, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.ip(),
        0x0100,
        "IP should point to faulting instruction"
    );
}

#[test]
fn test_jmp_far_indirect_ff5_register_operand_raises_ud() {
    // FF EE = FF /5 with mod=11, rm=110 (SI register)
    // JMP FAR is memory-only; mod=11 is #UD on real hardware.
    let (mut cpu, mut mem) = setup(0x0100, &[0xFF, 0xEE]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 6,
                error_code: 0,
                ..
            }
        ),
        "FF /5 with register operand (mod=11) should raise #UD, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.ip(),
        0x0100,
        "IP should point to faulting instruction"
    );
}

#[test]
fn test_jmp_far_indirect_ff5_register_sp_raises_ud() {
    // FF EC = FF /5 with mod=11, rm=100 (SP register)
    let (mut cpu, mut mem) = setup(0x0100, &[0xFF, 0xEC]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 6,
                error_code: 0,
                ..
            }
        ),
        "FF /5 with register operand (mod=11, rm=SP) should raise #UD, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.ip(),
        0x0100,
        "IP should point to faulting instruction"
    );
}
