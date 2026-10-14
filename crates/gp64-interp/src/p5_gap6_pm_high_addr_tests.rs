use super::*;
use gp64_memory::Memory;

/// Set up 32-bit PM with CS at base 0 and a custom DS base.
fn setup_pm_high_addr(code: &[u8], ds_base: u32) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cr0 |= 1; // PM
    cpu.regs.cs = 0x0008;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.cs_cache.limit = 0xFFFFF;
    cpu.regs.cs_cache.granularity = true;
    cpu.regs.cs_cache.recompute_effective_limit();
    cpu.regs.cs_cache.access = 0x9B;
    cpu.regs.cs_cache.is_code = true;
    cpu.regs.cs_cache.big = true;
    cpu.cs_bits = 32;
    cpu.regs.ds = 0x0010;
    cpu.regs.ds_cache.base = ds_base;
    cpu.regs.ds_cache.limit = 0xFFFFF;
    cpu.regs.ds_cache.granularity = true;
    cpu.regs.ds_cache.recompute_effective_limit();
    cpu.regs.ds_cache.access = 0x93;
    cpu.regs.ds_cache.is_code = false;
    cpu.regs.ds_cache.big = true;
    // SS at base 0 with 4GB limit
    cpu.regs.ss = 0x0018;
    cpu.regs.ss_cache.base = 0;
    cpu.regs.ss_cache.limit = 0xFFFFF;
    cpu.regs.ss_cache.granularity = true;
    cpu.regs.ss_cache.recompute_effective_limit();
    cpu.regs.ss_cache.access = 0x93;
    cpu.regs.ss_cache.big = true;
    // ES at same base as DS for string ops
    cpu.regs.es = 0x0020;
    cpu.regs.es_cache.base = ds_base;
    cpu.regs.es_cache.limit = 0xFFFFF;
    cpu.regs.es_cache.granularity = true;
    cpu.regs.es_cache.recompute_effective_limit();
    cpu.regs.es_cache.access = 0x93;
    cpu.regs.es_cache.is_code = false;
    cpu.regs.es_cache.big = true;
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }
    (cpu, mem)
}

// ---- Test 1: Write above ram_len (should not panic) ----

#[test]
fn test_gap6_write_above_ram_len_no_panic() {
    // DS base = 0x200000 (above 1MB+64KB allocation)
    // MOV [0], AL = 88 05 00 00 00 00 (ModR/M: [disp32])
    // Actually use: A2 00 00 00 00 = MOV [moffs32], AL
    let code: &[u8] = &[0xA2, 0x00, 0x00, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_pm_high_addr(code, 0x200000);
    cpu.regs.set_al(0x42);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Write above ram_len should succeed via slow path, got {:?}",
        result
    );
    // Verify the memory was written (Memory should have resized)
    assert_eq!(
        mem.read_u8(0x200000),
        0x42,
        "Value should be written at linear address 0x200000"
    );
}

// ---- Test 2: Read above ram_len (should not panic) ----

#[test]
fn test_gap6_read_above_ram_len_no_panic() {
    // DS base = 0x200000, MOV AL, [0] → linear = 0x200000
    // A0 00 00 00 00 = MOV AL, [moffs32]
    let code: &[u8] = &[0xA0, 0x00, 0x00, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_pm_high_addr(code, 0x200000);
    cpu.regs.set_al(0x42); // set to known value to detect change
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Read above ram_len should succeed via slow path, got {:?}",
        result
    );
    // Memory returns 0xFF for reads from beyond current allocation (uninitialized).
    // The key assertion is that this doesn't panic — the value may be 0 or 0xFF
    // depending on whether Memory resizes on read or returns a default.
    let al = cpu.regs.al();
    assert!(
        al == 0 || al == 0xFF,
        "Read from uninitialized high memory should return 0 or 0xFF, got 0x{:02X}",
        al
    );
}

// ---- Test 3: Multi-byte write above ram_len (u16 and u32) ----

#[test]
fn test_gap6_multi_byte_write_above_ram_len() {
    // MOV [0], AX (16-bit): 66 A3 00 00 00 00 — wait, in 32-bit PM, 66 prefix
    // toggles to 16-bit operand. Let's use MOV [0], EAX instead: A3 00 00 00 00
    let code: &[u8] = &[0xA3, 0x00, 0x00, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_pm_high_addr(code, 0x200000);
    cpu.regs.eax = 0xDEADBEEF;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "32-bit write above ram_len should succeed, got {:?}",
        result
    );
    assert_eq!(
        mem.read_u32(0x200000),
        0xDEADBEEF,
        "EAX value should be written at linear address 0x200000"
    );
}

// ---- Test 4: Stack operations above ram_len ----

#[test]
fn test_gap6_stack_above_ram_len() {
    // SS base = 0x200000, PUSH EAX (0x50) then POP EBX (0x5B)
    let code: &[u8] = &[0x50, 0x5B]; // PUSH EAX, POP EBX
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cr0 |= 1;
    cpu.regs.cs = 0x0008;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.cs_cache.limit = 0xFFFFF;
    cpu.regs.cs_cache.granularity = true;
    cpu.regs.cs_cache.recompute_effective_limit();
    cpu.regs.cs_cache.access = 0x9B;
    cpu.regs.cs_cache.is_code = true;
    cpu.regs.cs_cache.big = true;
    cpu.cs_bits = 32;
    cpu.regs.ds = 0x0010;
    cpu.regs.ds_cache.base = 0;
    cpu.regs.ds_cache.limit = 0xFFFFF;
    cpu.regs.ds_cache.granularity = true;
    cpu.regs.ds_cache.recompute_effective_limit();
    cpu.regs.ds_cache.access = 0x93;
    cpu.regs.ds_cache.big = true;
    // SS with high base
    cpu.regs.ss = 0x0018;
    cpu.regs.ss_cache.base = 0x200000;
    cpu.regs.ss_cache.limit = 0xFFFFF;
    cpu.regs.ss_cache.granularity = true;
    cpu.regs.ss_cache.recompute_effective_limit();
    cpu.regs.ss_cache.access = 0x93;
    cpu.regs.ss_cache.big = true;
    cpu.regs.esp = 0x1000;
    cpu.regs.set_sp(0x1000);
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }
    cpu.regs.eax = 0x12345678;
    cpu.regs.ebx = 0;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "PUSH/POP with high SS should succeed, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.ebx, 0x12345678,
        "POP should retrieve the pushed value"
    );
    // ESP should be restored after PUSH+POP
    assert_eq!(cpu.regs.esp, 0x1000, "ESP should be back to original");
}

// ---- Test 5: Read/write roundtrip above ram_len ----

#[test]
fn test_gap6_read_write_roundtrip_above_ram_len() {
    // Write then read back from high memory.
    // MOV [0], EAX; MOV EBX, [0]
    // A3 00 00 00 00 = MOV [moffs32], EAX
    // 8B 1D 00 00 00 00 = MOV EBX, [disp32=0]
    let code: &[u8] = &[
        0xA3, 0x00, 0x00, 0x00, 0x00, // MOV [0], EAX
        0x8B, 0x1D, 0x00, 0x00, 0x00, 0x00, // MOV EBX, [0]
    ];
    let (mut cpu, mut mem) = setup_pm_high_addr(code, 0x200000);
    cpu.regs.eax = 0xCAFEBABE;
    cpu.regs.ebx = 0;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "Write+read roundtrip should succeed, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.ebx, 0xCAFEBABE,
        "Read should return the written value"
    );
}
