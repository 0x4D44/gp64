use super::*;
use gp64_cpu::{Cpu, Flags};
use gp64_memory::Memory;

// Helper: setup paging (identity map first 1MB, 32-bit flat model)
fn setup_paging(code: &[u8]) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cr0 = 0x8000_0001; // PE + PG
    cpu.regs.cpl = 0;
    cpu.regs.cr3 = 0x10000;
    mem.write_u32_physical(0x10000, 0x11007);
    for page in 0..256u32 {
        let pte_addr = 0x11000 + (page as usize) * 4;
        mem.write_u32_physical(pte_addr, (page << 12) | 0x07);
    }
    cpu.regs.cs = 0x0008;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.cs_cache.limit = 0xFFFFF;
    cpu.regs.cs_cache.granularity = true;
    cpu.regs.cs_cache.recompute_effective_limit();
    cpu.regs.cs_cache.access = 0x9B;
    cpu.regs.cs_cache.is_code = true;
    cpu.regs.cs_cache.big = true;
    cpu.regs.cs_cache.present = true;
    cpu.cs_bits = 32;
    cpu.regs.ds = 0x0010;
    cpu.regs.ds_cache.base = 0;
    cpu.regs.ds_cache.limit = 0xFFFFF;
    cpu.regs.ds_cache.granularity = true;
    cpu.regs.ds_cache.recompute_effective_limit();
    cpu.regs.ds_cache.access = 0x93;
    cpu.regs.ds_cache.is_code = false;
    cpu.regs.ds_cache.big = true;
    cpu.regs.ds_cache.present = true;
    cpu.regs.ss = 0x0018;
    cpu.regs.ss_cache.base = 0;
    cpu.regs.ss_cache.limit = 0xFFFFF;
    cpu.regs.ss_cache.granularity = true;
    cpu.regs.ss_cache.recompute_effective_limit();
    cpu.regs.ss_cache.access = 0x93;
    cpu.regs.ss_cache.is_code = false;
    cpu.regs.ss_cache.big = true;
    cpu.regs.ss_cache.present = true;
    cpu.regs.es = 0x0020;
    cpu.regs.es_cache.base = 0;
    cpu.regs.es_cache.limit = 0xFFFFF;
    cpu.regs.es_cache.granularity = true;
    cpu.regs.es_cache.recompute_effective_limit();
    cpu.regs.es_cache.access = 0x93;
    cpu.regs.es_cache.is_code = false;
    cpu.regs.es_cache.big = true;
    cpu.regs.es_cache.present = true;
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }
    (cpu, mem)
}

// ========================================================================
// Target 1: RMW ALU byte/dword with paging
// ========================================================================

// 1. ADD byte [0x5000], AL (opcode 0x00)
//    ModR/M 0x05 = mod=00, reg=AL(/0), r/m=5(disp32)
//    Encoding: 00 05 00 50 00 00
#[test]
fn test_paging_add_byte_mem_reg() {
    let (mut cpu, mut mem) = setup_paging(&[0x00, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x42;
    cpu.regs.eax = 0x10; // AL = 0x10
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "ADD byte [mem], AL with paging should succeed, got {:?}",
        result
    );
    assert_eq!(mem.ram[0x5000], 0x52); // 0x42 + 0x10
}

// 2. OR byte [0x5000], AL (opcode 0x08)
//    ModR/M 0x05 = mod=00, reg=AL(/0), r/m=5(disp32)
//    Encoding: 08 05 00 50 00 00
#[test]
fn test_paging_or_byte_mem_reg() {
    let (mut cpu, mut mem) = setup_paging(&[0x08, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0xF0;
    cpu.regs.eax = 0x0F; // AL = 0x0F
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "OR byte [mem], AL with paging should succeed, got {:?}",
        result
    );
    assert_eq!(mem.ram[0x5000], 0xFF); // 0xF0 | 0x0F
}

// 3. ADC byte [0x5000], AL (opcode 0x10) with CF=1
//    Encoding: 10 05 00 50 00 00
#[test]
fn test_paging_adc_byte_mem_reg() {
    let (mut cpu, mut mem) = setup_paging(&[0x10, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x20;
    cpu.regs.eax = 0x10; // AL = 0x10
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "ADC byte [mem], AL with paging should succeed, got {:?}",
        result
    );
    assert_eq!(mem.ram[0x5000], 0x31); // 0x20 + 0x10 + 1(CF)
}

// 4. SBB byte [0x5000], AL (opcode 0x18) with CF=1
//    Encoding: 18 05 00 50 00 00
#[test]
fn test_paging_sbb_byte_mem_reg() {
    let (mut cpu, mut mem) = setup_paging(&[0x18, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x50;
    cpu.regs.eax = 0x10; // AL = 0x10
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "SBB byte [mem], AL with paging should succeed, got {:?}",
        result
    );
    assert_eq!(mem.ram[0x5000], 0x3F); // 0x50 - 0x10 - 1(CF)
}

// 5. AND byte [0x5000], AL (opcode 0x20)
//    Encoding: 20 05 00 50 00 00
#[test]
fn test_paging_and_byte_mem_reg() {
    let (mut cpu, mut mem) = setup_paging(&[0x20, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0xAB;
    cpu.regs.eax = 0x0F; // AL = 0x0F
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "AND byte [mem], AL with paging should succeed, got {:?}",
        result
    );
    assert_eq!(mem.ram[0x5000], 0x0B); // 0xAB & 0x0F
}

// 6. SUB byte [0x5000], AL (opcode 0x28)
//    Encoding: 28 05 00 50 00 00
#[test]
fn test_paging_sub_byte_mem_reg() {
    let (mut cpu, mut mem) = setup_paging(&[0x28, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x80;
    cpu.regs.eax = 0x30; // AL = 0x30
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "SUB byte [mem], AL with paging should succeed, got {:?}",
        result
    );
    assert_eq!(mem.ram[0x5000], 0x50); // 0x80 - 0x30
}

// 7. XOR byte [0x5000], AL (opcode 0x30)
//    Encoding: 30 05 00 50 00 00
#[test]
fn test_paging_xor_byte_mem_reg() {
    let (mut cpu, mut mem) = setup_paging(&[0x30, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0xAA;
    cpu.regs.eax = 0x55; // AL = 0x55
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "XOR byte [mem], AL with paging should succeed, got {:?}",
        result
    );
    assert_eq!(mem.ram[0x5000], 0xFF); // 0xAA ^ 0x55
}

// 8. CMP dword [0x5000], EAX (opcode 0x39) - sets flags, no memory write
//    ModR/M 0x05 = mod=00, reg=EAX(/0), r/m=5(disp32)
//    Encoding: 39 05 00 50 00 00
#[test]
fn test_paging_cmp_mem_reg() {
    let (mut cpu, mut mem) = setup_paging(&[0x39, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000..0x5004].copy_from_slice(&100u32.to_le_bytes());
    cpu.regs.eax = 100;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "CMP [mem], EAX with paging should succeed, got {:?}",
        result
    );
    // CMP sets ZF when operands are equal; memory is unchanged
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert!(!cpu.regs.flags.contains(Flags::CARRY));
    let val = u32::from_le_bytes(mem.ram[0x5000..0x5004].try_into().unwrap());
    assert_eq!(val, 100, "CMP must not modify memory");
}

// 9. ADD dword [0x5000], imm32 (opcode 0x81 /0)
//    ModR/M 0x05 = mod=00, reg=/0, r/m=5(disp32)
//    Encoding: 81 05 00 50 00 00 <imm32>
#[test]
fn test_paging_add_mem_imm32() {
    let (mut cpu, mut mem) = setup_paging(&[
        0x81, 0x05, 0x00, 0x50, 0x00, 0x00, // ADD dword [0x5000], imm32
        0x0A, 0x00, 0x00, 0x00, // imm32 = 10
    ]);
    mem.ram[0x5000..0x5004].copy_from_slice(&90u32.to_le_bytes());
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "ADD [mem], imm32 with paging should succeed, got {:?}",
        result
    );
    let val = u32::from_le_bytes(mem.ram[0x5000..0x5004].try_into().unwrap());
    assert_eq!(val, 100); // 90 + 10
}

// 9b. ADD dword [0x5000], sign-extended imm8 (opcode 0x83 /0)
//     ModR/M 0x05 = mod=00, reg=/0, r/m=5(disp32)
//     Encoding: 83 05 00 50 00 00 05
#[test]
fn test_paging_add_mem_imm8_sx() {
    let (mut cpu, mut mem) = setup_paging(&[
        0x83, 0x05, 0x00, 0x50, 0x00, 0x00, // ADD dword [0x5000], imm8
        0x05, // imm8 = 5
    ]);
    mem.ram[0x5000..0x5004].copy_from_slice(&95u32.to_le_bytes());
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "ADD [mem], imm8 with paging should succeed, got {:?}",
        result
    );
    let val = u32::from_le_bytes(mem.ram[0x5000..0x5004].try_into().unwrap());
    assert_eq!(val, 100); // 95 + 5
}

// 9c. ADD byte [0x5000], imm8 (opcode 0x80 /0)
//     ModR/M 0x05 = mod=00, reg=/0, r/m=5(disp32)
//     Encoding: 80 05 00 50 00 00 03
#[test]
fn test_paging_add_byte_mem_imm8() {
    let (mut cpu, mut mem) = setup_paging(&[
        0x80, 0x05, 0x00, 0x50, 0x00, 0x00, // ADD byte [0x5000], imm8
        0x03, // imm8 = 3
    ]);
    mem.ram[0x5000] = 0x42;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "ADD byte [mem], imm8 with paging should succeed, got {:?}",
        result
    );
    assert_eq!(mem.ram[0x5000], 0x45); // 0x42 + 3
}

// 10. TEST dword [0x5000], EAX (opcode 0x85)
//     ModR/M 0x05 = mod=00, reg=EAX(/0), r/m=5(disp32)
//     Encoding: 85 05 00 50 00 00
#[test]
fn test_paging_test_mem_reg() {
    let (mut cpu, mut mem) = setup_paging(&[0x85, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000..0x5004].copy_from_slice(&0xFF00FF00u32.to_le_bytes());
    cpu.regs.eax = 0x00FF00FF;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "TEST [mem], EAX with paging should succeed, got {:?}",
        result
    );
    // AND of 0xFF00FF00 & 0x00FF00FF = 0, so ZF should be set
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    // Memory must be unchanged
    let val = u32::from_le_bytes(mem.ram[0x5000..0x5004].try_into().unwrap());
    assert_eq!(val, 0xFF00FF00, "TEST must not modify memory");
}

// 10b. TEST byte [0x5000], AL (opcode 0x84)
//      ModR/M 0x05 = mod=00, reg=AL(/0), r/m=5(disp32)
//      Encoding: 84 05 00 50 00 00
#[test]
fn test_paging_test_byte_mem_reg() {
    let (mut cpu, mut mem) = setup_paging(&[0x84, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x80;
    cpu.regs.eax = 0x80; // AL = 0x80
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "TEST byte [mem], AL with paging should succeed, got {:?}",
        result
    );
    // AND of 0x80 & 0x80 = 0x80, nonzero -> ZF clear, SF set
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
    assert!(cpu.regs.flags.contains(Flags::SIGN));
}

// 11. XCHG byte [0x5000], AL (opcode 0x86)
//     ModR/M 0x05 = mod=00, reg=AL(/0), r/m=5(disp32)
//     Encoding: 86 05 00 50 00 00
#[test]
fn test_paging_xchg_byte_mem_reg() {
    let (mut cpu, mut mem) = setup_paging(&[0x86, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0xAA;
    cpu.regs.eax = 0xBB; // AL = 0xBB
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "XCHG byte [mem], AL with paging should succeed, got {:?}",
        result
    );
    assert_eq!(cpu.regs.eax & 0xFF, 0xAA, "AL should have old memory value");
    assert_eq!(mem.ram[0x5000], 0xBB, "Memory should have old AL value");
}

// 12. NOT byte [0x5000] (opcode 0xF6 /2)
//     ModR/M 0x15 = mod=00, reg=/2, r/m=5(disp32)
//     Encoding: F6 15 00 50 00 00
#[test]
fn test_paging_not_byte_mem() {
    let (mut cpu, mut mem) = setup_paging(&[0xF6, 0x15, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0xA5;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "NOT byte [mem] with paging should succeed, got {:?}",
        result
    );
    assert_eq!(mem.ram[0x5000], 0x5A); // ~0xA5 = 0x5A
}

// 12b. NEG byte [0x5000] (opcode 0xF6 /3)
//      ModR/M 0x1D = mod=00, reg=/3, r/m=5(disp32)
//      Encoding: F6 1D 00 50 00 00
#[test]
fn test_paging_neg_byte_mem() {
    let (mut cpu, mut mem) = setup_paging(&[0xF6, 0x1D, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 42;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "NEG byte [mem] with paging should succeed, got {:?}",
        result
    );
    assert_eq!(mem.ram[0x5000] as i8, -42i8);
}

// ========================================================================
// Target 2: INC/DEC/CALL/JMP/PUSH r/m with paging
// ========================================================================

// 1. INC byte [0x5000] (opcode 0xFE /0)
//    ModR/M 0x05 = mod=00, reg=/0, r/m=5(disp32)
//    Encoding: FE 05 00 50 00 00
#[test]
fn test_paging_inc_byte_mem() {
    let (mut cpu, mut mem) = setup_paging(&[0xFE, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x7F;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "INC byte [mem] with paging should succeed, got {:?}",
        result
    );
    assert_eq!(mem.ram[0x5000], 0x80); // 0x7F + 1
                                       // INC 0x7F -> 0x80 sets overflow (signed overflow 127->-128)
    assert!(cpu.regs.flags.contains(Flags::OVERFLOW));
}

// 2. DEC byte [0x5000] (opcode 0xFE /1)
//    ModR/M 0x0D = mod=00, reg=/1, r/m=5(disp32)
//    Encoding: FE 0D 00 50 00 00
#[test]
fn test_paging_dec_byte_mem() {
    let (mut cpu, mut mem) = setup_paging(&[0xFE, 0x0D, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x01;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "DEC byte [mem] with paging should succeed, got {:?}",
        result
    );
    assert_eq!(mem.ram[0x5000], 0x00); // 0x01 - 1
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// 3. INC dword [0x5000] (opcode 0xFF /0)
//    ModR/M 0x05 = mod=00, reg=/0, r/m=5(disp32)
//    Encoding: FF 05 00 50 00 00
#[test]
fn test_paging_inc_dword_mem_at_5000() {
    let (mut cpu, mut mem) = setup_paging(&[0xFF, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000..0x5004].copy_from_slice(&0xFFFFFFFFu32.to_le_bytes());
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "INC dword [mem] with paging should succeed, got {:?}",
        result
    );
    let val = u32::from_le_bytes(mem.ram[0x5000..0x5004].try_into().unwrap());
    assert_eq!(val, 0); // 0xFFFFFFFF + 1 wraps to 0
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// 4. DEC dword [0x5000] (opcode 0xFF /1)
//    ModR/M 0x0D = mod=00, reg=/1, r/m=5(disp32)
//    Encoding: FF 0D 00 50 00 00
#[test]
fn test_paging_dec_dword_mem_at_5000() {
    let (mut cpu, mut mem) = setup_paging(&[0xFF, 0x0D, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000..0x5004].copy_from_slice(&0u32.to_le_bytes());
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "DEC dword [mem] with paging should succeed, got {:?}",
        result
    );
    let val = u32::from_le_bytes(mem.ram[0x5000..0x5004].try_into().unwrap());
    assert_eq!(val, 0xFFFFFFFF); // 0 - 1 wraps to 0xFFFFFFFF
    assert!(cpu.regs.flags.contains(Flags::SIGN));
}

// 5. CALL near indirect [0x5000] (opcode 0xFF /2)
//    Reads the call target address from [0x5000], pushes return addr, jumps.
//    ModR/M 0x15 = mod=00, reg=/2, r/m=5(disp32)
//    Encoding: FF 15 00 50 00 00
#[test]
fn test_paging_call_indirect_mem() {
    let (mut cpu, mut mem) = setup_paging(&[0xFF, 0x15, 0x00, 0x50, 0x00, 0x00]);
    // Target address stored at [0x5000]: jump to 0x0200
    mem.ram[0x5000..0x5004].copy_from_slice(&0x0200u32.to_le_bytes());
    // Place a HLT at the target so we can observe the result
    mem.ram[0x0200] = 0xF4; // HLT
    let old_esp = cpu.regs.esp;
    let return_addr = 0x0100u32 + 6; // instruction is 6 bytes
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "CALL [mem] with paging should succeed, got {:?}",
        result
    );
    // EIP should now be at 0x0200
    assert_eq!(cpu.regs.eip, 0x0200);
    // ESP should have decreased by 4 (pushed return address)
    assert_eq!(cpu.regs.esp, old_esp - 4);
    // Return address should be on the stack
    let pushed = u32::from_le_bytes(
        mem.ram[cpu.regs.esp as usize..(cpu.regs.esp + 4) as usize]
            .try_into()
            .unwrap(),
    );
    assert_eq!(pushed, return_addr);
}

// 6. JMP near indirect [0x5000] (opcode 0xFF /4)
//    Reads the jump target address from [0x5000], jumps (no stack push).
//    ModR/M 0x25 = mod=00, reg=/4, r/m=5(disp32)
//    Encoding: FF 25 00 50 00 00
#[test]
fn test_paging_jmp_indirect_mem() {
    let (mut cpu, mut mem) = setup_paging(&[0xFF, 0x25, 0x00, 0x50, 0x00, 0x00]);
    // Target address stored at [0x5000]: jump to 0x0300
    mem.ram[0x5000..0x5004].copy_from_slice(&0x0300u32.to_le_bytes());
    mem.ram[0x0300] = 0xF4; // HLT at target
    let old_esp = cpu.regs.esp;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "JMP [mem] with paging should succeed, got {:?}",
        result
    );
    // EIP should now be at 0x0300
    assert_eq!(cpu.regs.eip, 0x0300);
    // ESP should be unchanged (JMP doesn't push)
    assert_eq!(cpu.regs.esp, old_esp);
}

// 7. PUSH dword [0x5000] (opcode 0xFF /6)
//    Reads a dword from [0x5000] through paging, pushes it to paged stack.
//    ModR/M 0x35 = mod=00, reg=/6, r/m=5(disp32)
//    Encoding: FF 35 00 50 00 00
#[test]
fn test_paging_push_mem_dword() {
    let (mut cpu, mut mem) = setup_paging(&[0xFF, 0x35, 0x00, 0x50, 0x00, 0x00]);
    let push_val: u32 = 0xDEADBEEF;
    mem.ram[0x5000..0x5004].copy_from_slice(&push_val.to_le_bytes());
    let old_esp = cpu.regs.esp;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "PUSH [mem] with paging should succeed, got {:?}",
        result
    );
    // ESP should have decreased by 4
    assert_eq!(cpu.regs.esp, old_esp - 4);
    // Value on stack should match
    let stk = u32::from_le_bytes(
        mem.ram[cpu.regs.esp as usize..(cpu.regs.esp + 4) as usize]
            .try_into()
            .unwrap(),
    );
    assert_eq!(stk, push_val);
}

// 8. CALL near indirect [0x5000] 32-bit, with non-identity stack paging
//    Remap stack page so the return address push goes through page translation.
//    Page 7 (linear 0x7000-0x7FFF, where ESP-4 lands) -> physical 0x40000
#[test]
fn test_paging_call_indirect_mem_nonidentity_stack() {
    let (mut cpu, mut mem) = setup_paging(&[0xFF, 0x15, 0x00, 0x50, 0x00, 0x00]);
    // Target address stored at [0x5000]: jump to 0x0200
    mem.ram[0x5000..0x5004].copy_from_slice(&0x0200u32.to_le_bytes());
    mem.ram[0x0200] = 0xF4; // HLT at target

    // Remap stack page 7 (linear 0x7000-0x7FFF) to physical page 0x40
    mem.write_u32_physical(0x11000 + 7 * 4, (0x40 << 12) | 0x07);

    // ESP starts at 0x8000, CALL pushes to ESP-4 = 0x7FFC (page 7)
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    let return_addr = 0x0100u32 + 6; // 6-byte instruction
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "CALL [mem] with non-identity stack paging should succeed, got {:?}",
        result
    );
    assert_eq!(cpu.regs.eip, 0x0200);
    assert_eq!(cpu.regs.esp, 0x7FFC);
    // Return address should be at physical 0x40FFC (remapped page 7 -> 0x40)
    let pushed = u32::from_le_bytes(mem.ram[0x40FFC..0x41000].try_into().unwrap());
    assert_eq!(pushed, return_addr);
}
