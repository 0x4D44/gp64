use super::*;
#[test]
fn interpreter_memory_hook_runs_only_with_active_stats() {
    let stats = StatsCollector::new();
    let run_read = || {
        let (mut cpu, mut mem) = setup(&[0xA0, 0x00, 0x02]);
        mem.ram[0x0200] = 0x5A;
        let result = run(&mut cpu, &mut mem, &mut (), 1);
        assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
        assert_eq!(cpu.regs.eax as u8, 0x5A);
    };
    MEMORY_ACCESS_HOOK_CALLS.with(|calls| calls.set(0));
    run_read();
    with_stats(&stats, run_read);
    assert_eq!(MEMORY_ACCESS_HOOK_CALLS.with(Cell::get), 0);
    assert_eq!(stats.snapshot_memory_counts().0.iter().sum::<u64>(), 0);
    stats.set_active(true);
    with_stats(&stats, run_read);
    assert_eq!(MEMORY_ACCESS_HOOK_CALLS.with(Cell::get), 1);
    assert_eq!(stats.snapshot_memory_counts().0[0], 1);
}
#[test]
fn with_stats_records_interpreter_memory_accesses() {
    let (mut cpu, mut mem) = setup(&[0xA0, 0x00, 0x02]);
    mem.ram[0x0200] = 0x5A;
    let stats = StatsCollector::new();
    stats.set_active(true);
    let mut io = ();
    let result = with_stats(&stats, || run(&mut cpu, &mut mem, &mut io, 1));
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u8, 0x5A);
    let (reads, writes) = stats.snapshot_memory_counts();
    assert_eq!(reads[0], 1, "the operand read should be sampled");
    assert_eq!(writes.iter().sum::<u64>(), 0);
}
#[test]
fn with_stats_records_interpreter_memory_writes() {
    let (mut cpu, mut mem) = setup(&[0xA2, 0x00, 0x02]);
    cpu.regs.eax = (cpu.regs.eax & !0xFF) | 0xA5;
    let stats = StatsCollector::new();
    stats.set_active(true);
    let mut io = ();
    let result = with_stats(&stats, || run(&mut cpu, &mut mem, &mut io, 1));
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(mem.ram[0x0200], 0xA5);
    let (reads, writes) = stats.snapshot_memory_counts();
    assert_eq!(reads.iter().sum::<u64>(), 0);
    assert_eq!(writes[0], 1, "the operand write should be sampled");
}
#[test]
fn real_mode_modrm_word_read_crossing_segment_limit_faults() {
    let (mut cpu, mut mem) = setup(&[0x8B, 0x07]);
    let mut io = ();
    cpu.regs.ebx = 0xFFFF;
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector, error_code, ..
        } => {
            assert_eq!(vector, 13);
            assert_eq!(error_code, 0);
            assert_eq!(cpu.regs.eip, 0x0100);
        }
        other => panic!("expected real-mode #GP for word read at FFFFh, got {other:?}"),
    }
}
#[test]
fn real_mode_addr32_limit_fault_uses_overridden_segment() {
    for (prefix, expected_vector) in [(0x36, 12), (0x2E, 13)] {
        let (mut cpu, mut mem) = setup(&[0x67, prefix, 0x8B, 0x03]);
        cpu.regs.ebx = 0x1_0000;
        let result = run(&mut cpu, &mut mem, &mut (), 1);
        assert!(
            matches!(result, RunResult::Exception { vector, .. } if vector ==
            expected_vector),
            "override {prefix:02X} must select exception vector {expected_vector}, got {result:?}"
        );
    }
}
#[test]
fn real_mode_bound_table_crossing_segment_limit_faults() {
    let (mut cpu, mut mem) = setup(&[0x62, 0x07]);
    let mut io = ();
    cpu.regs.ebx = 0xFFFD;
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector, error_code, ..
        } => {
            assert_eq!(vector, 13, "real-mode limit violation is #GP");
            assert_eq!(error_code, 0);
            assert_eq!(
                cpu.regs.eip, 0x0100,
                "the faulting EIP must be the instruction start"
            );
        }
        other => panic!("expected real-mode #GP for BOUND table at FFFDh, got {other:?}"),
    }
}
#[test]
fn real_mode_bound_fields_wrap_independently_at_ffff() {
    let (mut cpu, mut mem) = setup(&[0x62, 0x07]);
    cpu.regs.ebx = 0xFFFE;
    cpu.regs.eax = 5;
    mem.write_u16(0xFFFE, 1);
    mem.write_u16(0x0000, 9);
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "BOUND fields may wrap between word reads, got {result:?}"
    );
}
#[test]
fn unreal_mode_bound_table_crossing_ffff_uses_cached_limit() {
    let (mut cpu, mut mem) = setup(&[0x62, 0x07]);
    let mut io = ();
    cpu.regs.ebx = 0xFFFD;
    cpu.regs.eax = 5;
    cpu.regs.ds_cache.limit = 0xFFFFF;
    cpu.regs.ds_cache.granularity = true;
    cpu.regs.ds_cache.recompute_effective_limit();
    mem.write_u8(0xFFFD, 0x01);
    mem.write_u8(0xFFFE, 0x00);
    mem.write_u8(0xFFFF, 0x09);
    mem.write_u8(0x10000, 0x00);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "unreal-mode cached DS limit should allow the BOUND table read, got {result:?}"
    );
}
#[test]
fn real_mode_bound_within_limit_still_works() {
    let (mut cpu, mut mem) = setup(&[0x62, 0x07]);
    let mut io = ();
    cpu.regs.ebx = 0x0200;
    cpu.regs.eax = 5;
    mem.write_u16(0x0200, 1);
    mem.write_u16(0x0202, 9);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "in-bounds real-mode BOUND must still retire, got {result:?}"
    );
}
#[test]
fn real_mode_bound_out_of_range_still_raises_br() {
    let (mut cpu, mut mem) = setup(&[0x62, 0x07]);
    let mut io = ();
    cpu.regs.ebx = 0x0200;
    cpu.regs.eax = 50;
    mem.write_u16(0x0200, 1);
    mem.write_u16(0x0202, 9);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception { vector, .. } => {
            assert_eq!(vector, 5, "out-of-range BOUND is #BR(5)");
        }
        other => panic!("expected #BR for out-of-range BOUND, got {other:?}"),
    }
}
#[test]
fn unreal_mode_modrm_word_read_crossing_ffff_uses_cached_limit() {
    let (mut cpu, mut mem) = setup(&[0x8B, 0x07]);
    let mut io = ();
    cpu.regs.ebx = 0xFFFF;
    cpu.regs.ds_cache.limit = 0xFFFFF;
    cpu.regs.ds_cache.granularity = true;
    cpu.regs.ds_cache.recompute_effective_limit();
    mem.write_u8(0xFFFF, 0x34);
    mem.write_u8(0x10000, 0x12);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "unreal-mode cached DS limit should allow the read, got {result:?}"
    );
    assert_eq!(cpu.regs.ax(), 0x1234);
}
#[test]
fn movzx_r16_rm16_writes_16bit_destination() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0xB7, 0xC3]);
    let mut io = ();
    cpu.regs.eax = 0xDEAD_BEEF;
    cpu.regs.ebx = 0x1111_5678;
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "MOVZX AX,BX should retire exactly one instruction, got {result:?}"
    );
    assert_eq!(
        cpu.regs.eax, 0xDEAD_5678,
        "AX must take the source, upper EAX preserved"
    );
    assert_eq!(cpu.regs.eip, 0x0103);
    let (mut cpu, mut mem) = setup(&[0x0F, 0xB7, 0x0F]);
    let mut io = ();
    cpu.regs.ecx = 0x1234_0000;
    cpu.regs.ebx = 0x0200;
    mem.write_u8(0x0200, 0xCD);
    mem.write_u8(0x0201, 0xAB);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "MOVZX CX,[BX] should retire exactly one instruction, got {result:?}"
    );
    assert_eq!(
        cpu.regs.ecx, 0x1234_ABCD,
        "CX must take the loaded word, upper ECX preserved"
    );
    assert_eq!(cpu.regs.eip, 0x0103);
}
#[test]
fn test_run_result_executed() {
    let r = RunResult::BudgetExhausted { executed: 42 };
    assert_eq!(r.executed(), 42);
    let r = RunResult::Halt { executed: 7 };
    assert_eq!(r.executed(), 7);
    let r = RunResult::SoftwareInterrupt {
        vector: 0x21,
        executed: 3,
    };
    assert_eq!(r.executed(), 3);
    let r = RunResult::Exception {
        vector: 0x0D,
        error_code: 0,
        fault_addr: None,
        executed: 1,
    };
    assert_eq!(r.executed(), 1);
    let r = RunResult::ScriptSentinel { executed: 0 };
    assert_eq!(r.executed(), 0);
}
#[test]
fn test_protected_mode_addressing() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    cpu.regs.cr0 |= 1;
    cpu.regs.cs = 0x0008;
    cpu.regs.cs_cache.base = 0x1000;
    cpu.regs.set_ip(0x0050);
    cpu.regs.eip = 0x0050;
    mem.ram[0x1050] = 0x90;
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eip, 0x0051);
}
#[test]
fn test_real_mode_fetch_uses_retained_cached_cs_base() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    cpu.regs.cr0 = 0;
    cpu.regs.cs = 0x0018;
    cpu.regs.cs_cache = gp64_cpu::SegmentCache::real_mode(cpu.regs.cs);
    cpu.regs.cs_cache.base = 0xA700;
    cpu.regs.set_ip(0x0000);
    cpu.regs.eip = 0x0000;
    mem.ram[0xA700] = 0x90;
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ip(), 0x0001);
}
#[test]
fn test_real_mode_ret_uses_retained_cached_ss_base() {
    let (mut cpu, mut mem) = setup(&[0xC3]);
    let mut io = ();
    cpu.regs.cr0 = 0;
    // Synthetic cached base differs from the selector base; keep SP odd.
    cpu.regs.ss = 0x0080;
    cpu.regs.ss_cache = gp64_cpu::SegmentCache::real_mode(cpu.regs.ss);
    cpu.regs.ss_cache.base = 0x18000;
    cpu.regs.set_sp(0x0101);
    cpu.regs.esp = 0x0101;
    mem.write_u16(0x00800 + 0x0101, 0x0000);
    mem.write_u16(0x18000 + 0x0101, 0x1234);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ip(), 0x1234);
    assert_eq!(cpu.regs.sp(), 0x0103);
}
#[test]
fn test_nop() {
    let (mut cpu, mut mem) = setup(&[0x90]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eip, 0x0101);
}
#[test]
fn test_multiple_nops() {
    let (mut cpu, mut mem) = setup(&[0x90, 0x90, 0x90]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 3);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 3 }));
    assert_eq!(cpu.regs.eip, 0x0103);
}
#[test]
fn test_mov_reg8_imm() {
    let (mut cpu, mut mem) = setup(&[0xB0, 0x42]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u8, 0x42);
}
#[test]
fn test_mov_reg16_imm() {
    let (mut cpu, mut mem) = setup(&[0xB9, 0x34, 0x12]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ecx as u16, 0x1234);
}
#[test]
fn test_mov_reg16_preserves_high() {
    let (mut cpu, mut mem) = setup(&[0xB8, 0x78, 0x56]);
    cpu.regs.eax = 0xAAAA_0000;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax, 0xAAAA_5678);
}
#[test]
fn test_xor_cx_cx() {
    let (mut cpu, mut mem) = setup(&[0x31, 0xC9]);
    cpu.regs.ecx = 0x1234;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ecx as u16, 0);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert!(!cpu.regs.flags.contains(Flags::CARRY));
    assert!(!cpu.regs.flags.contains(Flags::OVERFLOW));
}
#[test]
fn test_xor_r16_r16_direction() {
    let (mut cpu, mut mem) = setup(&[0x33, 0xD8]);
    cpu.regs.eax = 0xFF00;
    cpu.regs.ebx = 0x00FF;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ebx as u16, 0xFFFF);
    assert!(cpu.regs.flags.contains(Flags::SIGN));
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}
#[test]
fn test_xor_memory_operand() {
    let (mut cpu, mut mem) = setup(&[0x31, 0x00]);
    cpu.regs.ebx = 0x0200;
    cpu.regs.esi = 0x0010;
    cpu.regs.eax = 0x00FF;
    mem.ram[0x0210] = 0x00;
    mem.ram[0x0211] = 0xFF;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    let val = u16::from_le_bytes([mem.ram[0x0210], mem.ram[0x0211]]);
    assert_eq!(val, 0xFFFF);
}
#[test]
fn test_add_r16_r16() {
    let (mut cpu, mut mem) = setup(&[0x01, 0xC8]);
    cpu.regs.eax = 0x0003;
    cpu.regs.ecx = 0x0005;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u16, 0x0008);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
    assert!(!cpu.regs.flags.contains(Flags::CARRY));
}
#[test]
fn test_add_r16_r16_carry() {
    let (mut cpu, mut mem) = setup(&[0x01, 0xC8]);
    cpu.regs.eax = 0xFFF0;
    cpu.regs.ecx = 0x0020;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u16, 0x0010);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}
#[test]
fn test_sub_r16_r16() {
    let (mut cpu, mut mem) = setup(&[0x29, 0xC8]);
    cpu.regs.eax = 0x0008;
    cpu.regs.ecx = 0x0003;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u16, 0x0005);
    assert!(!cpu.regs.flags.contains(Flags::CARRY));
}
#[test]
fn test_sub_r16_r16_borrow() {
    let (mut cpu, mut mem) = setup(&[0x29, 0xC8]);
    cpu.regs.eax = 0x0003;
    cpu.regs.ecx = 0x0008;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u16, 0xFFFB);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
    assert!(cpu.regs.flags.contains(Flags::SIGN));
}
#[test]
fn test_cmp_r16_r16_equal() {
    let (mut cpu, mut mem) = setup(&[0x39, 0xC8]);
    cpu.regs.eax = 0x0005;
    cpu.regs.ecx = 0x0005;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert!(!cpu.regs.flags.contains(Flags::CARRY));
    assert_eq!(cpu.regs.eax as u16, 0x0005);
}
#[test]
fn test_cmp_r16_r16_less() {
    let (mut cpu, mut mem) = setup(&[0x39, 0xC8]);
    cpu.regs.eax = 0x0003;
    cpu.regs.ecx = 0x0008;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}
#[test]
fn test_mov_r16_r16() {
    let (mut cpu, mut mem) = setup(&[0x8B, 0xC1]);
    cpu.regs.ecx = 0x5678;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u16, 0x5678);
}
#[test]
fn test_mov_rm16_r16() {
    let (mut cpu, mut mem) = setup(&[0x89, 0xC1]);
    cpu.regs.eax = 0xABCD;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ecx as u16, 0xABCD);
}
#[test]
fn lldt_invalid_selector_does_not_commit_ldtr() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let gdt_base: u32 = 0x10000;
    write_gdt_entry(&mut mem, gdt_base as usize, [0u8; 8]);
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x08,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x10,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C),
    );
    cpu.regs.gdtr.base = gdt_base;
    cpu.regs.gdtr.limit = 0x17;
    cpu.regs.cr0 |= 1;
    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x08).unwrap();
    cpu.cs_bits = 32;
    cpu.regs.cpl = 0;
    cpu.regs.ds = 0x10;
    cpu.regs.ds_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.ss = 0x10;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.ldtr = 0x0088;
    cpu.regs.ldt_base = 0xCAFE_0000;
    cpu.regs.ldt_limit = 0x0000_1234;
    let code_addr = 0x1000usize;
    mem.ram[code_addr..code_addr + 3].copy_from_slice(&[0x0F, 0x00, 0xD0]);
    cpu.regs.set_ip(code_addr as u16);
    cpu.regs.eip = code_addr as u32;
    cpu.regs.eax = 0x0020;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 13, .. }),
        "LLDT with an out-of-limit selector should #GP, got {result:?}"
    );
    assert_eq!(cpu.regs.ldtr, 0x0088, "LDTR must be unchanged on fault");
    assert_eq!(
        cpu.regs.ldt_base, 0xCAFE_0000,
        "ldt_base must be unchanged on fault"
    );
    assert_eq!(
        cpu.regs.ldt_limit, 0x0000_1234,
        "ldt_limit must be unchanged on fault"
    );
}
