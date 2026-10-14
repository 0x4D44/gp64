use super::*;
use gp64_cpu::PageFaultInfo;
use gp64_memory::Memory;

// ---- tlb_translate tests ----

#[test]
fn test_tlb_translate_hit_read() {
    let mut mem = Memory::new();
    // Insert a TLB entry: linear page 0x42 → physical page 0x100
    mem.tlb_insert(0x42, 0x100, true, true, true);
    let tlb = mem.tlb_entries_ptr();

    let result = unsafe { tlb_translate(tlb, 0x42_123, false, false, false) };
    assert_eq!(result, Some(0x100_123));
}

#[test]
fn test_tlb_translate_hit_write_writable() {
    let mut mem = Memory::new();
    mem.tlb_insert(0x42, 0x100, true, true, true); // writable + dirty
    let tlb = mem.tlb_entries_ptr();

    let result = unsafe { tlb_translate(tlb, 0x42_456, true, false, false) };
    assert_eq!(result, Some(0x100_456));
}

#[test]
fn test_tlb_translate_miss_invalid() {
    let mem = Memory::new(); // No entries inserted — all invalid
    let tlb = mem.tlb_entries_ptr();

    let result = unsafe { tlb_translate(tlb, 0x42_000, false, false, false) };
    assert_eq!(result, None);
}

#[test]
fn test_tlb_translate_miss_wrong_page() {
    let mut mem = Memory::new();
    mem.tlb_insert(0x42, 0x100, true, true, true);
    let tlb = mem.tlb_entries_ptr();

    // Different page that might hash to same slot but has different linear_page
    let result = unsafe { tlb_translate(tlb, 0x43_000, false, false, false) };
    assert_eq!(result, None);
}

#[test]
fn test_tlb_translate_write_readonly_cpl3() {
    let mut mem = Memory::new();
    mem.tlb_insert(0x42, 0x100, false, true, true); // NOT writable
    let tlb = mem.tlb_entries_ptr();

    // CPL3 write to read-only page → miss (protection)
    let result = unsafe { tlb_translate(tlb, 0x42_000, true, true, false) };
    assert_eq!(result, None);
}

#[test]
fn test_tlb_translate_write_readonly_wp_enabled() {
    let mut mem = Memory::new();
    mem.tlb_insert(0x42, 0x100, false, true, true); // NOT writable
    let tlb = mem.tlb_entries_ptr();

    // Supervisor write with WP → miss (protection)
    let result = unsafe { tlb_translate(tlb, 0x42_000, true, false, true) };
    assert_eq!(result, None);
}

#[test]
fn test_tlb_translate_write_readonly_supervisor_no_wp() {
    let mut mem = Memory::new();
    mem.tlb_insert(0x42, 0x100, false, true, true); // NOT writable, but dirty
    let tlb = mem.tlb_entries_ptr();

    // Supervisor write without WP → allowed (supervisor bypasses write protection)
    let result = unsafe { tlb_translate(tlb, 0x42_000, true, false, false) };
    assert_eq!(result, Some(0x100_000));
}

#[test]
fn test_tlb_translate_cpl3_not_user() {
    let mut mem = Memory::new();
    mem.tlb_insert(0x42, 0x100, true, false, true); // NOT user-accessible
    let tlb = mem.tlb_entries_ptr();

    // CPL3 read on non-user page → miss
    let result = unsafe { tlb_translate(tlb, 0x42_000, false, true, false) };
    assert_eq!(result, None);
}

#[test]
fn test_tlb_translate_write_not_dirty() {
    let mut mem = Memory::new();
    mem.tlb_insert(0x42, 0x100, true, true, false); // writable but NOT dirty
    let tlb = mem.tlb_entries_ptr();

    // Write to non-dirty page → miss (need to set dirty bit via slow path)
    let result = unsafe { tlb_translate(tlb, 0x42_000, true, false, false) };
    assert_eq!(result, None);
}

#[test]
fn test_tlb_translate_offset_preserved() {
    let mut mem = Memory::new();
    mem.tlb_insert(0x01, 0xFF, true, true, true);
    let tlb = mem.tlb_entries_ptr();

    // Offset bits (low 12 bits) should pass through unchanged
    let result = unsafe { tlb_translate(tlb, 0x01_ABC, false, false, false) };
    assert_eq!(result, Some(0xFF_ABC));
}

// ---- translate_slow tests ----

#[test]
fn test_translate_slow_dpmi_physical_mapping() {
    let cpu = gp64_cpu::Cpu::new();
    let mut mem = Memory::new();
    mem.add_physical_mapping(0x1000, 0x2000, 0x100);

    let result = translate_slow(&cpu, &mut mem, 0x1050, gp64_types::PageAccess::Read);
    assert_eq!(result.unwrap(), 0x2050);
}

#[test]
fn test_translate_slow_no_mapping_no_paging() {
    // With no physical mapping and no paging, translate_paging_checked will
    // fail because there's no page directory set up.
    let mut cpu = gp64_cpu::Cpu::new();
    cpu.regs.cr0 |= 0x8000_0001; // Enable paging + PM
    let mut mem = Memory::new();

    // CR3 points to 0 — PDE at 0 is 0 → page not present
    let result = translate_slow(&cpu, &mut mem, 0x1000, gp64_types::PageAccess::Read);
    assert!(result.is_err());
    let pf = result.unwrap_err();
    assert_eq!(pf.linear_address, 0x1000);
    // Should have set page fault state in memory
    assert!(mem.page_fault_pending);
}

#[test]
fn test_translate_slow_not_present_write_faults_below_cs_base() {
    let mut cpu = gp64_cpu::Cpu::new();
    cpu.regs.cr0 |= 0x8000_0001; // Enable paging + PM
    cpu.regs.cs_cache.base = 0x0100_0000;
    let mut mem = Memory::new();

    let result = translate_slow(&cpu, &mut mem, 0x0020_0000, gp64_types::PageAccess::Write);

    let pf = result.unwrap_err();
    assert_eq!(pf.linear_address, 0x0020_0000);
    assert_eq!(pf.error_code, 2);
    assert!(mem.page_fault_pending);
}

#[test]
fn test_translate_slow_not_present_read_faults_below_cs_base() {
    let mut cpu = gp64_cpu::Cpu::new();
    cpu.regs.cr0 |= 0x8000_0001; // Enable paging + PM
    cpu.regs.cs_cache.base = 0x0100_0000;
    let mut mem = Memory::new();

    let result = translate_slow(&cpu, &mut mem, 0x0020_0000, gp64_types::PageAccess::Read);

    let pf = result.unwrap_err();
    assert_eq!(pf.linear_address, 0x0020_0000);
    assert_eq!(pf.error_code, 0);
    assert!(mem.page_fault_pending);
}

// ---- make_pf_exception tests ----

#[test]
fn test_make_pf_exception_returns_vector_14() {
    let mut cpu = gp64_cpu::Cpu::new();
    let mut flags_reg = cpu.regs.flags;
    let mut lazy = LazyOp::None;
    let pf = PageFaultInfo::new(0xDEADBEEF, true, true, true);

    let result = make_pf_exception(&mut cpu, &mut flags_reg, &mut lazy, 0x100, pf, 42);
    match result {
        RunResult::Exception {
            vector,
            error_code,
            fault_addr,
            executed,
        } => {
            assert_eq!(vector, 14);
            assert_eq!(error_code, 0b111); // P=1, W=1, U=1
            assert_eq!(fault_addr, Some(0xDEADBEEF));
            assert_eq!(executed, 42);
        }
        _ => panic!("Expected Exception, got {:?}", result),
    }
}

#[test]
fn test_make_pf_exception_restores_ip() {
    let mut cpu = gp64_cpu::Cpu::new();
    let mut flags_reg = cpu.regs.flags;
    cpu.regs.set_ip(0x200);
    cpu.regs.eip = 0x200;
    let mut lazy = LazyOp::None;
    let pf = PageFaultInfo::new(0, false, false, false);

    let _ = make_pf_exception(&mut cpu, &mut flags_reg, &mut lazy, 0x100, pf, 0);
    // IP should be restored to snap_ip
    assert_eq!(cpu.regs.ip(), 0x100);
    assert_eq!(cpu.regs.eip, 0x100);
}

#[test]
fn test_make_pf_exception_materializes_lazy_flags() {
    let mut cpu = gp64_cpu::Cpu::new();
    let mut flags_reg = cpu.regs.flags;
    // Set up a lazy Add8 result — flags should be materialized
    let mut lazy = LazyOp::Add8 {
        a: 0xFF,
        b: 1,
        r: 0,
    };
    let pf = PageFaultInfo::new(0x1000, false, false, false);

    let _ = make_pf_exception(&mut cpu, &mut flags_reg, &mut lazy, 0, pf, 5);
    // After materialization, lazy should be None
    assert!(matches!(lazy, LazyOp::None));
    // Zero flag should be set (result is 0)
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ---- run() paging initialization tests ----

#[test]
fn test_run_initializes_paging_state() {
    // Verify that a simple instruction executes correctly with paging
    // variables initialized (even though paging isn't active).
    let mut cpu = gp64_cpu::Cpu::new();
    let mut mem = Memory::new();
    // NOP at 0000:0100
    mem.write_u8(0x100, 0x90);
    cpu.regs.cs = 0;
    cpu.regs.set_ip(0x100);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
}

// ---- MOV CR0 updates cached paging state ----

#[test]
fn test_mov_cr0_updates_paging_cache() {
    // Execute MOV CR0, EAX with paging bit set, then a NOP.
    // 0F 22 C0 = MOV CR0, EAX
    // 90       = NOP
    let mut cpu = gp64_cpu::Cpu::new();
    let mut mem = Memory::new();
    let code: &[u8] = &[0x0F, 0x22, 0xC0, 0x90];
    for (i, &b) in code.iter().enumerate() {
        mem.write_u8(0x100 + i, b);
    }
    cpu.regs.cs = 0;
    cpu.regs.set_ip(0x100);
    // Set EAX to enable PE + WP (but not PG for now — paging without page
    // tables would crash)
    cpu.regs.eax = 0x0001_0001; // PE=1, WP=1
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 2);
    // P1: MOV CR0 that changes PE returns ModeChange immediately
    assert!(
        matches!(result, RunResult::ModeChange { executed: 1 }),
        "Expected ModeChange after PE change, got {:?}",
        result
    );
    // CR0 should have WP set
    assert_ne!(cpu.regs.cr0 & 0x0001_0000, 0, "WP should be set");
    // IP should be past the MOV CR0 instruction (3 bytes)
    assert_eq!(cpu.regs.ip(), 0x103, "IP should advance past MOV CR0");
}

#[test]
fn test_mov_cr0_no_pe_change_continues() {
    // MOV CR0, EAX where PE doesn't change should NOT return ModeChange.
    // 0F 22 C0 = MOV CR0, EAX
    // 90       = NOP
    let mut cpu = gp64_cpu::Cpu::new();
    let mut mem = Memory::new();
    let code: &[u8] = &[0x0F, 0x22, 0xC0, 0x90];
    for (i, &b) in code.iter().enumerate() {
        mem.write_u8(0x100 + i, b);
    }
    cpu.regs.cs = 0;
    cpu.regs.set_ip(0x100);
    // Set EAX to WP=1 but PE stays 0 (no PE change)
    cpu.regs.eax = 0x0001_0000; // WP=1, PE=0
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "Expected BudgetExhausted (no PE change), got {:?}",
        result
    );
}

#[test]
fn test_paging_pg_without_pe_raises_gp() {
    // Setting PG=1 with PE=0 must raise #GP(0).
    // 0F 22 C0 = MOV CR0, EAX
    let mut cpu = gp64_cpu::Cpu::new();
    let mut mem = Memory::new();
    let code: &[u8] = &[0x0F, 0x22, 0xC0, 0x90];
    for (i, &b) in code.iter().enumerate() {
        mem.write_u8(0x100 + i, b);
    }
    cpu.regs.cs = 0;
    cpu.regs.set_ip(0x100);
    let old_cr0 = cpu.regs.cr0;
    // EAX = PG=1, PE=0 — invalid combination
    cpu.regs.eax = 0x8000_0000;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("Expected #GP(0), got {:?}", other),
    }
    // CR0 must NOT have been modified
    assert_eq!(cpu.regs.cr0, old_cr0, "CR0 should be unchanged after #GP");
    // IP should point back to the MOV CR0 instruction (fault address)
    assert_eq!(
        cpu.regs.ip(),
        0x100,
        "IP should point at faulting instruction"
    );
}

#[test]
fn test_paging_pg_without_pe_clearing_pe_raises_gp() {
    // Starting with PE=1, attempting MOV CR0 with PG=1 but PE=0 must raise #GP(0).
    // 0F 22 C0 = MOV CR0, EAX
    let mut cpu = gp64_cpu::Cpu::new();
    let mut mem = Memory::new();
    let code: &[u8] = &[0x0F, 0x22, 0xC0, 0x90];
    for (i, &b) in code.iter().enumerate() {
        mem.write_u8(0x100 + i, b);
    }
    cpu.regs.cs = 0;
    cpu.regs.set_ip(0x100);
    // Start with PE=1 (protected mode)
    cpu.regs.cr0 = 1;
    // Try to set PG=1 but clear PE — invalid
    cpu.regs.eax = 0x8000_0000;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("Expected #GP(0), got {:?}", other),
    }
    // CR0 must still have PE=1, not modified
    assert_eq!(cpu.regs.cr0, 1, "CR0 should be unchanged (PE=1) after #GP");
    assert_eq!(
        cpu.regs.ip(),
        0x100,
        "IP should point at faulting instruction"
    );
}

#[test]
fn test_lmsw_pe_change_returns_mode_change() {
    // LMSW AX with PE=1 should return ModeChange.
    // 0F 01 F0 = LMSW AX
    // 90       = NOP
    let mut cpu = gp64_cpu::Cpu::new();
    let mut mem = Memory::new();
    let code: &[u8] = &[0x0F, 0x01, 0xF0, 0x90];
    for (i, &b) in code.iter().enumerate() {
        mem.write_u8(0x100 + i, b);
    }
    cpu.regs.cs = 0;
    cpu.regs.set_ip(0x100);
    cpu.regs.eax = 0x0001; // AX = PE=1
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::ModeChange { executed: 1 }),
        "Expected ModeChange after LMSW sets PE, got {:?}",
        result
    );
    assert_eq!(cpu.regs.cr0 & 1, 1, "PE should be set");
    assert_eq!(cpu.regs.ip(), 0x103, "IP should advance past LMSW");
}

#[test]
fn test_lgdt_16bit_masks_base_to_24_bits() {
    // In 16-bit mode, LGDT should only use the low 24 bits of the base.
    // LGDT [0x200] = 0F 01 16 00 02
    let mut cpu = gp64_cpu::Cpu::new();
    let mut mem = Memory::new();
    let code: &[u8] = &[0x0F, 0x01, 0x16, 0x00, 0x02]; // LGDT [0x200]
    for (i, &b) in code.iter().enumerate() {
        mem.write_u8(0x100 + i, b);
    }
    // Write pseudo-descriptor at DS:0x200 (DS=0, so physical 0x200)
    // limit = 0x00FF, base = 0xAB001234 (byte 5 = 0xAB should be masked)
    mem.write_u16(0x200, 0x00FF); // limit
    mem.write_u8(0x202, 0x34); // base low
    mem.write_u8(0x203, 0x12); // base mid-low
    mem.write_u8(0x204, 0x00); // base mid-high (24-bit)
    mem.write_u8(0x205, 0xAB); // base high (should be masked in 16-bit mode)

    cpu.regs.cs = 0;
    cpu.regs.set_ip(0x100);
    cpu.regs.ds = 0;
    cpu.regs.ds_cache.base = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { .. }),
        "Expected BudgetExhausted, got {:?}",
        result
    );
    assert_eq!(cpu.regs.gdtr.limit, 0x00FF);
    // Base should be masked to 24 bits: 0x001234, NOT 0xAB001234
    assert_eq!(
        cpu.regs.gdtr.base, 0x00001234,
        "GDTR base should be 24-bit in 16-bit mode"
    );
}

#[test]
fn test_lgdt_enables_descriptor_snoop_for_same_run_write() {
    // LGDT [0x0200]; MOV [0x6010], AX. The run-entry descriptor guard starts
    // false, so the second instruction must observe the live snoop range.
    let mut cpu = gp64_cpu::Cpu::new();
    let mut mem = Memory::new();
    let code: &[u8] = &[0x0F, 0x01, 0x16, 0x00, 0x02, 0xA3, 0x10, 0x60];
    for (i, &b) in code.iter().enumerate() {
        mem.write_u8(0x100 + i, b);
    }
    mem.write_u16(0x200, 0x00FF); // limit
    mem.write_u8(0x202, 0x00); // base low
    mem.write_u8(0x203, 0x60); // base mid-low
    mem.write_u8(0x204, 0x00); // base mid-high
    mem.write_u8(0x205, 0x00); // ignored in 16-bit LGDT

    cpu.regs.cs = 0;
    cpu.regs.set_ip(0x100);
    cpu.regs.ds = 0;
    cpu.regs.ds_cache.base = 0;
    cpu.regs.set_ax(0x1234);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);

    assert!(matches!(result, RunResult::BudgetExhausted { executed: 2 }));
    assert_eq!(cpu.regs.gdtr.base, 0x6000);
    assert_eq!(mem.read_u16(0x6010), 0x1234);
    assert!(
        mem.take_gdt_dirty(),
        "same-run writes after LGDT must use live descriptor snoop state"
    );
}

#[test]
fn test_lidt_16bit_masks_base_to_24_bits() {
    // In 16-bit mode, LIDT should only use the low 24 bits of the base.
    // LIDT [0x200] = 0F 01 1E 00 02
    let mut cpu = gp64_cpu::Cpu::new();
    let mut mem = Memory::new();
    let code: &[u8] = &[0x0F, 0x01, 0x1E, 0x00, 0x02]; // LIDT [0x200]
    for (i, &b) in code.iter().enumerate() {
        mem.write_u8(0x100 + i, b);
    }
    // Write pseudo-descriptor at DS:0x200
    mem.write_u16(0x200, 0x07FF); // limit
    mem.write_u8(0x202, 0xE0); // base byte 0
    mem.write_u8(0x203, 0x65); // base byte 1
    mem.write_u8(0x204, 0x00); // base byte 2
    mem.write_u8(0x205, 0x93); // base byte 3 (should be masked)

    cpu.regs.cs = 0;
    cpu.regs.set_ip(0x100);
    cpu.regs.ds = 0;
    cpu.regs.ds_cache.base = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { .. }),
        "Expected BudgetExhausted, got {:?}",
        result
    );
    assert_eq!(cpu.regs.idtr.limit, 0x07FF);
    // Base should be masked: 0x000065E0, NOT 0x930065E0
    assert_eq!(
        cpu.regs.idtr.base, 0x000065E0,
        "IDTR base should be 24-bit in 16-bit mode"
    );
}

#[test]
fn test_lgdt_32bit_override_preserves_full_base() {
    // With operand-size override (66h prefix) in 16-bit mode,
    // LGDT should use the full 32-bit base (not mask to 24 bits).
    // 66 0F 01 16 00 02 = o32 LGDT [0x200]
    let mut cpu = gp64_cpu::Cpu::new();
    let mut mem = Memory::new();
    let code: &[u8] = &[0x66, 0x0F, 0x01, 0x16, 0x00, 0x02]; // o32 LGDT [0x200]
    for (i, &b) in code.iter().enumerate() {
        mem.write_u8(0x100 + i, b);
    }
    // Write pseudo-descriptor at DS:0x200
    mem.write_u16(0x200, 0x00FF); // limit
    mem.write_u32(0x202, 0xAB001234); // full 32-bit base

    cpu.regs.cs = 0;
    cpu.regs.set_ip(0x100);
    cpu.regs.ds = 0;
    cpu.regs.ds_cache.base = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { .. }),
        "Expected BudgetExhausted, got {:?}",
        result
    );
    assert_eq!(cpu.regs.gdtr.limit, 0x00FF);
    // With 32-bit override, full 32-bit base should be preserved
    assert_eq!(
        cpu.regs.gdtr.base, 0xAB001234,
        "GDTR base should be full 32-bit with o32 prefix"
    );
}

#[test]
fn test_mov_cr0_clear_pe_returns_mode_change() {
    // Clearing PE (PM → real mode) should also return ModeChange.
    // 0F 22 C0 = MOV CR0, EAX
    // 90       = NOP
    let mut cpu = gp64_cpu::Cpu::new();
    let mut mem = Memory::new();
    let code: &[u8] = &[0x0F, 0x22, 0xC0, 0x90];
    for (i, &b) in code.iter().enumerate() {
        mem.write_u8(0x100 + i, b);
    }
    cpu.regs.cs = 0;
    cpu.regs.set_ip(0x100);
    cpu.regs.cr0 = 1; // Start in PM (PE=1)
    cpu.regs.eax = 0; // Clear PE
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::ModeChange { executed: 1 }),
        "Expected ModeChange after clearing PE, got {:?}",
        result
    );
    assert_eq!(cpu.regs.cr0 & 1, 0, "PE should be cleared");
}
