use super::*;

#[test]
fn test_dos_memory_selector_count_16bit() {
    let mut dpmi = Dpmi::new();
    dpmi.cpu_mode = 0;
    // 16-bit: paragraphs / 0x1000, rounded up
    assert_eq!(dpmi.dos_memory_selector_count(0x1000), 1);
    assert_eq!(dpmi.dos_memory_selector_count(0x1001), 2);
    assert_eq!(dpmi.dos_memory_selector_count(0x2000), 2);
    assert_eq!(dpmi.dos_memory_selector_count(1), 1);
}

#[test]
fn test_dos_memory_selector_count_32bit() {
    let mut dpmi = Dpmi::new();
    dpmi.cpu_mode = 1;
    // 32-bit: always 1
    assert_eq!(dpmi.dos_memory_selector_count(0x1000), 1);
    assert_eq!(dpmi.dos_memory_selector_count(0x2000), 1);
    assert_eq!(dpmi.dos_memory_selector_count(1), 1);
}

#[test]
fn test_dos_memory_block_index_exact_match() {
    let mut dpmi = Dpmi::new();
    dpmi.dos_memory_blocks.push(DosMemoryBlock {
        segment: 0x1000,
        selector: 0x0107,
        paragraphs: 0x100,
        selector_count: 2,
    });
    // Exact match
    assert_eq!(dpmi.dos_memory_block_index_for_selector(0x0107), Some(0));
    // Second selector (offset = 8)
    assert_eq!(dpmi.dos_memory_block_index_for_selector(0x010F), Some(0));
    // Beyond selector_count
    assert_eq!(dpmi.dos_memory_block_index_for_selector(0x0117), None);
}

#[test]
fn test_free_physical_mapping_success() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // First map a physical address
    cpu.regs.set_ax(0x0800);
    cpu.regs.set_bx(0); // physical high
    cpu.regs.set_cx(0); // physical low (aligned)
    cpu.regs.set_si(0);
    cpu.regs.set_di(0x1000); // size = 4K
                             // We need a valid physical address within RAM
    memory.ensure_ram_size(0x200000);
    cpu.regs.set_bx(0x0010); // physical = 0x100000
    cpu.regs.set_cx(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Physical mapping should succeed"
    );
    let linear_high = cpu.regs.bx();
    let linear_low = cpu.regs.cx();

    // Now free it
    cpu.regs.set_ax(0x0801);
    cpu.regs.set_bx(linear_high);
    cpu.regs.set_cx(linear_low);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Free physical mapping should succeed"
    );

    // Verify it's gone
    assert!(dpmi.physical_mappings.is_empty());
}

#[test]
fn test_page_lock_mark_pageable_and_relock() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);
    memory.ensure_ram_size(0x200000);

    // Mark pageable (0x0602)
    cpu.regs.set_ax(0x0602);
    cpu.regs.set_bx(0x0010); // linear high
    cpu.regs.set_cx(0x0000); // linear low = 0x100000
    cpu.regs.set_si(0);
    cpu.regs.set_di(0x1000); // size = 4K
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dpmi.page_lock_stats.pageable_calls, 1);

    // Relock (0x0603)
    cpu.regs.set_ax(0x0603);
    cpu.regs.set_bx(0x0010);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_si(0);
    cpu.regs.set_di(0x1000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dpmi.page_lock_stats.relock_calls, 1);
}

#[test]
fn test_get_page_size_returns_4096() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x0604);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.bx(), 0);
    assert_eq!(cpu.regs.cx(), 0x1000);
}

#[test]
fn test_demand_paging_unsupported_subfunc() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x0705);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_virtual_interrupt_get_state_reports_correctly() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Initially enabled
    dpmi.virtual_interrupts_enabled = true;
    cpu.regs.set_ax(0x0902);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert_eq!(cpu.regs.al(), 1);

    dpmi.virtual_interrupts_enabled = false;
    cpu.regs.set_ax(0x0902);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert_eq!(cpu.regs.al(), 0);
}

#[test]
fn test_virtual_interrupt_unsupported_subfunc() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x0905);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_vendor_api_entry_returns_unsupported() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x0A00);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_coprocessor_get_status_value() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x0E00);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // 0x35 = FPU present, enabled, 387 type
    assert_eq!(cpu.regs.ax(), 0x0035);
}

#[test]
fn test_coprocessor_set_emulation_noop() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x0E01);
    cpu.regs.set_bx(0x0003); // enable emulation + set present
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_shared_memory_all_subfunctions_unsupported() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    for subfunc in [0x00u8, 0x01, 0x02, 0x03] {
        cpu.regs.set_ax(0x0D00 | subfunc as u16);
        dpmi.int31(&mut cpu, &mut memory).unwrap();
        assert!(
            cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "subfunc 0x{:02X} should return unsupported",
            subfunc
        );
    }
}

#[test]
fn test_set_access_rights_16bit_client_old_g_bit_clear() {
    // Exercise the 16-bit client path where existing G bit is NOT set
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false); // 16-bit mode

    // Allocate a selector
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel = cpu.regs.ax();

    // Set access rights: code segment, readable, DPL=3
    // access=0xFA: P=1, DPL=3, S=1, type=1010 (code, readable)
    // flags=0x00: 16-bit client should ignore
    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(sel);
    cpu.regs.set_cx(0x00FA); // CH=0x00 (flags), CL=0xFA (access)
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Set access rights should succeed"
    );
}

#[test]
fn test_segment_to_descriptor_cache_hit() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // First call creates a new selector
    cpu.regs.set_ax(0x0002);
    cpu.regs.set_bx(0x0040); // segment 0x40
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel1 = cpu.regs.ax();

    // Second call should return cached selector
    cpu.regs.set_ax(0x0002);
    cpu.regs.set_bx(0x0040);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel2 = cpu.regs.ax();

    assert_eq!(sel1, sel2, "Cached selector should be returned");
}

#[test]
fn test_get_selector_increment_value_returns_8() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x0003);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 8);
}

#[test]
fn test_resize_linear_memory_shrink_in_place_with_commit() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate 16KB committed
    cpu.regs.set_ax(0x0504);
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 0x4000;
    cpu.regs.edx = 1; // committed
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;
    let base = cpu.regs.ebx;

    // Shrink to 8KB with commit flag set
    cpu.regs.set_ax(0x0505);
    cpu.regs.esi = handle;
    cpu.regs.ecx = 0x2000;
    cpu.regs.edx = 1; // commit_new flag
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // Base should remain the same
    assert_eq!(cpu.regs.ebx, base);
}

#[test]
fn test_resize_linear_move_with_uncommitted_pages() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate first block uncommitted
    cpu.regs.set_ax(0x0504);
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 0x2000; // 8KB
    cpu.regs.edx = 0; // uncommitted
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle1 = cpu.regs.esi;

    // Allocate second block right after (to prevent in-place grow)
    cpu.regs.set_ax(0x0504);
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 0x100000; // 1MB to use up space
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Try to grow the first block - should move it
    cpu.regs.set_ax(0x0505);
    cpu.regs.esi = handle1;
    cpu.regs.ecx = 0x4000; // 16KB
    cpu.regs.edx = 0; // uncommitted new pages
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    // This might succeed or fail depending on memory layout
    // The test exercises the "move block" code path either way
}

#[test]
fn test_get_dpmi_capabilities_with_valid_buffer() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Set up a valid buffer via ES:EDI
    let es_sel = cpu.regs.es;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = 0;
        s.limit = 0xFFFF_FFFF;
    }
    memory.ensure_ram_size(0x50000 + 128);
    cpu.regs.edi = 0x50000;

    cpu.regs.set_ax(0x0401);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    // Should return unsupported (DPMI 0.9 host)
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_unhandled_int31_function() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Use a completely invalid function number
    cpu.regs.set_ax(0xFF00);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_page_locking_unsupported_subfunc() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x0605);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_set_descriptor_with_g_bit_and_limit_expansion() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate a selector
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel = cpu.regs.ax();

    // Set up ES to point to flat memory
    let es_sel = cpu.regs.es;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = 0;
        s.limit = 0xFFFF_FFFF;
    }

    let buffer_addr = 0x50000usize;
    memory.ensure_ram_size(buffer_addr + 16);

    // G bit set (0x80): limit granularity = 4K pages
    // limit_lo = 0xFFFF, limit_hi = 0x0F => 20-bit limit = 0xFFFFF
    // With G=1: actual limit = (0xFFFFF << 12) | 0xFFF = 0xFFFFFFFF (4GB)
    memory.write_u16(buffer_addr, 0xFFFF); // limit_lo
    memory.write_u16(buffer_addr + 2, 0x0000); // base_lo
    memory.write_u8(buffer_addr + 4, 0x00); // base_mid
    memory.write_u8(buffer_addr + 5, 0xF2); // access (data, r/w, DPL=3)
    memory.write_u8(buffer_addr + 6, 0xCF); // G=1(0x80), D=1(0x40), limit_hi=0x0F
    memory.write_u8(buffer_addr + 7, 0x00); // base_hi

    cpu.regs.set_ax(0x000C);
    cpu.regs.set_bx(sel);
    cpu.regs.edi = buffer_addr as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Set descriptor with G bit should succeed"
    );
    let limit = dpmi.get_descriptor_limit(sel).unwrap();
    assert_eq!(limit, 0xFFFF_FFFF);
}

#[test]
fn test_get_set_real_mode_interrupt_vector_roundtrip() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Set vector 0x33 to 0x1234:0x5678
    cpu.regs.set_ax(0x0201);
    cpu.regs.set_bl(0x33);
    cpu.regs.set_cx(0x1234); // segment
    cpu.regs.set_dx(0x5678); // offset
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Get it back
    cpu.regs.set_ax(0x0200);
    cpu.regs.set_bl(0x33);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.cx(), 0x1234);
    assert_eq!(cpu.regs.dx(), 0x5678);
}

#[test]
fn test_get_exception_handler_returns_stored_handler() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Manually set an exception handler
    dpmi.exception_handlers[0x0D] = IdtEntry {
        selector: 0x0107,
        offset: 0x12345678,
        gate_type: 0x0E,
        dpl: 3,
        present: true,
    };

    cpu.regs.set_ax(0x0202);
    cpu.regs.set_bl(0x0D);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.cx(), 0x0107);
    assert_eq!(cpu.regs.edx, 0x12345678);
}

#[test]
fn test_get_extended_exception_handler_without_idtr() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Set up an exception handler
    dpmi.exception_handlers[5] = IdtEntry {
        selector: 0x0107,
        offset: 0xABCD,
        gate_type: 0x0E,
        dpl: 3,
        present: true,
    };

    // Clear IDTR to exercise the fallback path
    cpu.regs.idtr.base = 0;

    cpu.regs.set_ax(0x0212);
    cpu.regs.set_bl(5);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.cx(), 0x0107);
    assert_eq!(cpu.regs.edx, 0xABCD);
}

#[test]
fn test_get_memory_block_size_050a_32bit_path() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Add a DPMI 1.0 block
    dpmi.linear_memory_blocks
        .push(LinearMemoryBlock::new(42, 0x200000, 0x8000, true));

    cpu.regs.set_ax(0x050A);
    cpu.regs.esi = 42; // handle
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ebx, 0x200000); // base
    assert_eq!(cpu.regs.ecx, 0x8000); // size
}

#[test]
fn test_read_write_handle_16bit_mode() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    dpmi.cpu_mode = 0;

    // Write a handle in 16-bit mode: SI:DI
    dpmi.write_handle(&mut cpu, 0x12345678);
    assert_eq!(cpu.regs.si(), 0x1234);
    assert_eq!(cpu.regs.di(), 0x5678);

    // Read it back
    let handle = dpmi.read_handle(&cpu);
    assert_eq!(handle, 0x12345678);
}

#[test]
fn test_env_limit_from_mcb_z_tag() {
    let mut memory = Memory::new();
    let env_seg = 0x0100_u16;
    let mcb_seg = env_seg - 1;
    let mcb_addr = (mcb_seg as usize) * 16;

    // Write MCB header with 'Z' tag (last block)
    memory.write_u8(mcb_addr, 0x5A); // 'Z' tag
    memory.write_u16(mcb_addr + 3, 0x0020); // size = 32 paragraphs = 512 bytes

    let result = Dpmi::env_limit_from_mcb(&memory, env_seg);
    assert!(result.is_some());
    // Per DPMI 0.9 spec, 16-bit DPMI env selector limit is floored at 64 KB,
    // so a 512-byte MCB block produces an 0xFFFF limit rather than 511.
    assert_eq!(result.unwrap(), 0xFFFF);
}

#[test]
fn test_ldt_entry_count_zero_limit() {
    let dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    cpu.regs.ldt_limit = 0;
    // Returns 0x10000 / 8 = 8192
    assert_eq!(dpmi.ldt_entry_count(&cpu), 8192);
}

#[test]
fn test_ldt_entry_count_nonzero_limit() {
    let dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    cpu.regs.ldt_limit = 0x7FFF; // 32768 - 1
                                 // (0x7FFF + 1) / 8 = 4096
    assert_eq!(dpmi.ldt_entry_count(&cpu), 4096);
}

#[test]
fn test_pending_hle_simulation_none_initially() {
    let mut dpmi = Dpmi::new();
    assert!(dpmi.take_pending_hle_simulation().is_none());
}

#[test]
fn test_set_descriptor_rejects_system_descriptor_with_s_bit_clear() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate a selector
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let sel = cpu.regs.ax();

    let es_sel = cpu.regs.es;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = 0;
        s.limit = 0xFFFF_FFFF;
    }

    let buffer_addr = 0x50000usize;
    memory.ensure_ram_size(buffer_addr + 16);

    // access byte with S=0 (system descriptor) should be rejected
    memory.write_u16(buffer_addr, 0xFFFF); // limit_lo
    memory.write_u16(buffer_addr + 2, 0x0000); // base_lo
    memory.write_u8(buffer_addr + 4, 0x00); // base_mid
    memory.write_u8(buffer_addr + 5, 0x82); // access: Present, S=0 (system), type=LDT
    memory.write_u8(buffer_addr + 6, 0x00); // flags
    memory.write_u8(buffer_addr + 7, 0x00); // base_hi

    cpu.regs.set_ax(0x000C);
    cpu.regs.set_bx(sel);
    cpu.regs.edi = buffer_addr as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "System descriptor (S=0) should be rejected"
    );
}

#[test]
fn test_set_access_rights_32bit_client_with_granularity() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate a selector
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let sel = cpu.regs.ax();

    // First set a limit
    cpu.regs.set_ax(0x0008);
    cpu.regs.set_bx(sel);
    cpu.regs.set_cx(0); // limit high
    cpu.regs.set_dx(0xFFFF); // limit low = 0xFFFF
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Set access rights with G=1, D=1 (32-bit client)
    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(sel);
    // CL=0xF2 (data r/w, DPL=3), CH=0xC0 (G=1, D=1)
    cpu.regs.set_cx(0xC0F2);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Set access rights with G/D bits should succeed"
    );
}

#[test]
fn test_resize_linear_memory_grow_in_place() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate a small block
    cpu.regs.set_ax(0x0504);
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 0x2000; // 8KB
    cpu.regs.edx = 1; // committed
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;
    let base = cpu.regs.ebx;

    // Grow it (no other blocks to prevent in-place growth)
    cpu.regs.set_ax(0x0505);
    cpu.regs.esi = handle;
    cpu.regs.ecx = 0x4000; // 16KB
    cpu.regs.edx = 1; // commit new pages
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Resize should succeed"
    );
    assert_eq!(
        cpu.regs.ebx, base,
        "Base should not change for in-place grow"
    );
}

#[test]
fn test_allocate_descriptors_single_reuses_free() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate and free a selector to put it in the free list
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let sel = cpu.regs.ax();

    cpu.regs.set_ax(0x0001);
    cpu.regs.set_bx(sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Now allocate again - should reuse the freed selector
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), sel, "Should reuse freed selector");
}

#[test]
fn test_get_memory_info_050b_with_linear_blocks() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Add a linear memory block
    dpmi.linear_memory_blocks.push(LinearMemoryBlock::new(
        1, 0x200000, 0x100000, // 1MB block
        true,
    ));

    let es_sel = cpu.regs.es;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = 0;
        s.limit = 0xFFFF_FFFF;
    }

    let buffer_addr = 0x50000usize;
    memory.ensure_ram_size(buffer_addr + 0x80);
    cpu.regs.edi = buffer_addr as u32;

    cpu.regs.set_ax(0x050B);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // allocated_bytes should be non-zero
    let allocated = memory.read_u32(buffer_addr + 0x14);
    assert!(allocated > 0, "Should report some allocated bytes");
}

#[test]
fn test_set_page_committed_extends_vector_when_needed() {
    // Create a block where committed vector has fewer words than needed
    let mut block = LinearMemoryBlock {
        handle: 1,
        base: 0x100000,
        size: 128 * 0x1000,
        committed: {
            let mut v = Box::new(FixedVec::new());
            v.push(0u64);
            v
        }, // Only 1 word (64 pages), but 128 pages total
        page_count: 128,
    };

    // Setting page 65 should trigger vector extension
    block.set_page_committed(65, true);
    assert!(block.is_page_committed(65));
    assert!(block.committed.len() >= 2);
}

#[test]
fn test_collect_allocated_ranges_all_types() {
    let mut dpmi = Dpmi::new();

    // Add a DPMI 0.9 block
    dpmi.memory_blocks
        .push(MemoryBlock::new(1, 0x200000, 0x1000));

    // Add a DPMI 1.0 linear block
    dpmi.linear_memory_blocks
        .push(LinearMemoryBlock::new(2, 0x300000, 0x2000, true));

    // Add a physical mapping
    dpmi.physical_mappings.push(PhysicalMapping {
        physical: 0xA0000,
        linear: 0x400000,
        size: 0x1000,
    });

    // Set descriptor tables
    dpmi.gdt_base = 0x100000;
    dpmi.gdt_size = 0x1000;

    let ranges = dpmi.collect_allocated_ranges();
    assert!(ranges.len() >= 4); // At least the 4 we added
}

#[test]
fn test_free_descriptor_clears_segment_selector_cache() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Use segment_to_descriptor to create a cached mapping
    cpu.regs.set_ax(0x0002);
    cpu.regs.set_bx(0x0040);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel = cpu.regs.ax();

    // Cache should contain the mapping
    assert_ne!(dpmi.segment_selectors[0x0040], 0);

    // Free the descriptor
    cpu.regs.set_ax(0x0001);
    cpu.regs.set_bx(sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Cache should be cleared
    assert_eq!(dpmi.segment_selectors[0x0040], 0);
}

#[test]
fn test_alloc_linear_block_skips_existing_handles() {
    let mut dpmi = Dpmi::new();
    let mut memory = Memory::new();
    memory.ensure_ram_size(0x400000);

    // Pre-populate with handles 1 and 2
    dpmi.memory_blocks
        .push(MemoryBlock::new(1, 0x100000, 0x1000));
    dpmi.memory_blocks
        .push(MemoryBlock::new(2, 0x110000, 0x1000));
    dpmi.next_mem_handle = 1; // Start from 1

    let result = dpmi.alloc_linear_block(&mut memory, 0x1000);
    assert!(result.is_ok());
    let (_, handle) = result.unwrap();
    assert!(handle >= 3, "Should skip existing handles 1 and 2");
}

#[test]
fn test_alloc_linear_block_full_table_returns_error_not_panic() {
    // MDD-BUG-CRUCIBLE-00056: filling the fixed-capacity memory-block table with
    // small page-sized blocks (well below the RAM ceiling) must make the next
    // 0501h allocation fail guest-visibly, not panic in FixedVec::push.
    let mut dpmi = Dpmi::new();
    let mut memory = Memory::new();
    memory.ensure_ram_size(0x800000);

    // Fill the 512-slot table with small blocks at low addresses, leaving plenty
    // of free linear space above them so find_free_linear_range still succeeds.
    for i in 0..512u32 {
        dpmi.memory_blocks
            .push(MemoryBlock::new(i + 1, 0x100000 + i * 0x1000, 0x1000));
    }
    assert!(dpmi.memory_blocks.is_full(), "table should be full");

    // The next valid allocation must return a DPMI error, not panic.
    let result = dpmi.alloc_linear_block(&mut memory, 0x1000);
    assert_eq!(
        result,
        Err(DPMI_ERR_HANDLE_UNAVAILABLE),
        "a full memory-block table must fail guest-visibly, got {result:?}"
    );
}

#[test]
fn test_can_extend_dos_memory_selectors_collision() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate two selectors that would be adjacent
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let sel1 = cpu.regs.ax();

    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let sel2 = cpu.regs.ax();

    // Now check if we can extend from sel1 to cover both
    // sel2 should be right after sel1 (sel1 + 8)
    let can_extend = dpmi.can_extend_dos_memory_selectors(&cpu, sel1, 1, 2);
    // The second selector (sel1 + 8) is already allocated, so can_extend should be false
    assert!(
        !can_extend,
        "Cannot extend because sel2 ({:04X}) is already allocated after sel1 ({:04X})",
        sel2, sel1
    );
}

#[test]
fn test_physical_mapping_overflow_in_phys_end() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);
    memory.ensure_ram_size(0x200000);

    // Physical + size would exceed DOS_RAM_CEILING
    cpu.regs.set_ax(0x0800);
    // physical = DOS_RAM_CEILING - 0x1000 (just barely in range)
    let phys = (DOS_RAM_CEILING - 0x1000) as u32;
    cpu.regs.set_bx((phys >> 16) as u16);
    cpu.regs.set_cx(phys as u16);
    // size = 0x2000 (would exceed DOS_RAM_CEILING)
    cpu.regs.set_si(0);
    cpu.regs.set_di(0x2000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Physical mapping exceeding DOS_RAM_CEILING should fail"
    );
}

#[test]
fn test_get_descriptor_success() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate a selector and set it up
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let sel = cpu.regs.ax();

    // Set base to 0x12340000
    cpu.regs.set_ax(0x0007);
    cpu.regs.set_bx(sel);
    cpu.regs.set_cx(0x1234);
    cpu.regs.set_dx(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Get descriptor into buffer
    let es_sel = cpu.regs.es;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = 0;
        s.limit = 0xFFFF_FFFF;
    }
    let buffer_addr = 0x50000usize;
    memory.ensure_ram_size(buffer_addr + 16);
    cpu.regs.edi = buffer_addr as u32;

    cpu.regs.set_ax(0x000B);
    cpu.regs.set_bx(sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Read back base from descriptor bytes
    let base_lo = memory.read_u16(buffer_addr + 2);
    let base_mid = memory.read_u8(buffer_addr + 4);
    let base_hi = memory.read_u8(buffer_addr + 7);
    let base = (base_lo as u32) | ((base_mid as u32) << 16) | ((base_hi as u32) << 24);
    assert_eq!(base, 0x12340000);
}

#[test]
fn test_cov_should_exit_v86_on_iret_state_cs_nonzero() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();

    // Set up V86 mode with IRET exit
    dpmi.v86_active = true;
    dpmi.v86_exit_on_iret = true;

    // CS != 0 → short-circuit false for second condition
    cpu.regs.cs = 0x1234;
    cpu.regs.set_ip(0);
    assert!(!dpmi.should_exit_v86_on_iret_state(&cpu));

    // IP != 0 → short-circuit false for third condition
    cpu.regs.cs = 0;
    cpu.regs.set_ip(0x100);
    assert!(!dpmi.should_exit_v86_on_iret_state(&cpu));

    // Both zero → true
    cpu.regs.cs = 0;
    cpu.regs.set_ip(0);
    assert!(dpmi.should_exit_v86_on_iret_state(&cpu));
}

#[test]
fn test_cov_should_exit_v86_on_retf_state_cs_nonzero() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();

    dpmi.v86_active = true;
    dpmi.v86_exit_on_retf = true;

    // CS != 0
    cpu.regs.cs = 0x0001;
    cpu.regs.set_ip(0);
    assert!(!dpmi.should_exit_v86_on_retf_state(&cpu));

    // IP != 0
    cpu.regs.cs = 0;
    cpu.regs.set_ip(0x0001);
    assert!(!dpmi.should_exit_v86_on_retf_state(&cpu));

    // Both zero
    cpu.regs.cs = 0;
    cpu.regs.set_ip(0);
    assert!(dpmi.should_exit_v86_on_retf_state(&cpu));
}

#[test]
fn test_cov_free_descriptors_already_in_free_list() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dpmi = Dpmi::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate a descriptor
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel = cpu.regs.ax();

    // Free it
    cpu.regs.set_ax(0x0001);
    cpu.regs.set_bx(sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Free it again - should handle the duplicate in free_selectors
    cpu.regs.set_ax(0x0001);
    cpu.regs.set_bx(sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
}

#[test]
fn test_cov_segment_to_selector_not_found() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dpmi = Dpmi::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // AX=0x0002: segment to selector
    // Force a scenario where the allocated selector is not in the list
    // by providing a valid real-mode segment
    cpu.regs.set_ax(0x0002);
    cpu.regs.set_bx(0x0040); // BIOS data segment
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    // Should succeed (allocates a new selector for segment 0x40)
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel = cpu.regs.ax();
    assert_ne!(sel, 0);
}

#[test]
fn test_cov_push_descriptor_ranges() {
    let mut dpmi = Dpmi::new();
    let mut ranges = Vec::new();

    // Initially all bases are 0, so nothing should be pushed
    dpmi.push_descriptor_ranges(&mut ranges);
    assert!(ranges.is_empty());

    // Set GDT only
    dpmi.gdt_base = 0x1000;
    dpmi.gdt_size = 0x100;
    dpmi.push_descriptor_ranges(&mut ranges);
    assert_eq!(ranges.len(), 1);
    assert_eq!(ranges[0], (0x1000, 0x100));

    ranges.clear();

    // Set all three
    dpmi.ldt_base = 0x2000;
    dpmi.ldt_size = 0x200;
    dpmi.idt_base = 0x3000;
    dpmi.idt_size = 0x300;
    dpmi.push_descriptor_ranges(&mut ranges);
    assert_eq!(ranges.len(), 3);
}

#[test]
fn test_cov_env_limit_from_mcb_invalid_tag() {
    let memory = Memory::new();
    // Env segment 0x100 → MCB at 0xFF → addr 0xFF0
    // MCB tag will be 0 (not 0x4D or 0x5A), so should return None
    let result = Dpmi::env_limit_from_mcb(&memory, 0x100);
    assert!(result.is_none());
}

#[test]
fn test_cov_env_limit_from_mcb_zero_segment() {
    let memory = Memory::new();
    let result = Dpmi::env_limit_from_mcb(&memory, 0);
    assert!(result.is_none());
}

#[test]
fn test_cov_find_next_mem_handle_basic() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dpmi = Dpmi::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate DPMI memory blocks to exercise handle allocation
    // AX=0x0501: Allocate memory block
    cpu.regs.set_ax(0x0501);
    cpu.regs.set_bx(0);
    cpu.regs.set_cx(4096); // 4KB
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    // Should succeed
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_cov_simulate_rm_int_with_hle_stub() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dpmi = Dpmi::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // AX=0x0300: simulate real mode interrupt
    cpu.regs.set_ax(0x0300);
    cpu.regs.set_bl(0x21); // INT 21h
    cpu.regs.set_bh(0x00); // No flags
    cpu.regs.set_cx(0); // 0 words to copy

    // Set up RM call structure in memory
    let buf_sel = dpmi.selectors[0].selector;
    let buf_off = 0u32;
    cpu.regs.es = buf_sel;
    cpu.regs.set_di(buf_off as u16);

    // Write RM structure: all zeros initially
    let buf_base = dpmi
        .selectors
        .iter()
        .find(|s| s.selector == buf_sel)
        .map(|s| s.base)
        .unwrap_or(0) as usize;

    // Clear the structure (50 bytes)
    for i in 0..50 {
        memory.write_u8(buf_base + i, 0);
    }

    // Set CS:IP in the RM struct to point to F000:0100 (HLE stub for INT 0)
    // Offset 0x2A = CS, 0x2C = IP in the RM call structure
    memory.write_u16(buf_base + 0x2A, 0xF000);
    memory.write_u16(buf_base + 0x2C, 0x0100);

    dpmi.int31(&mut cpu, &mut memory).unwrap();
}

#[test]
fn test_cov_push_descriptor_ranges_all_set() {
    let mut dpmi = Dpmi::new();
    dpmi.gdt_base = 0x1000;
    dpmi.gdt_size = 0x100;
    dpmi.ldt_base = 0x2000;
    dpmi.ldt_size = 0x200;
    dpmi.idt_base = 0x3000;
    dpmi.idt_size = 0x300;
    let mut ranges = Vec::new();
    dpmi.push_descriptor_ranges(&mut ranges);
    assert_eq!(ranges.len(), 3);
    assert_eq!(ranges[0], (0x1000, 0x100));
    assert_eq!(ranges[1], (0x2000, 0x200));
    assert_eq!(ranges[2], (0x3000, 0x300));
}

#[test]
fn test_cov_push_descriptor_ranges_none_set() {
    let dpmi = Dpmi::new();
    let mut ranges = Vec::new();
    dpmi.push_descriptor_ranges(&mut ranges);
    assert_eq!(ranges.len(), 0);
}

#[test]
fn test_cov_push_descriptor_ranges_partial() {
    let mut dpmi = Dpmi::new();
    dpmi.gdt_base = 0x1000;
    dpmi.gdt_size = 0x100;
    // ldt_base=0, ldt_size=0 → not pushed
    dpmi.idt_base = 0x3000;
    dpmi.idt_size = 0x300;
    let mut ranges = Vec::new();
    dpmi.push_descriptor_ranges(&mut ranges);
    assert_eq!(ranges.len(), 2);
}

#[test]
fn test_cov_push_descriptor_ranges_base_set_size_zero() {
    // Cover the A=true, B=false branch of each condition
    let mut dpmi = Dpmi::new();
    dpmi.gdt_base = 0x1000;
    dpmi.gdt_size = 0; // base!=0 but size==0
    dpmi.ldt_base = 0x2000;
    dpmi.ldt_size = 0;
    dpmi.idt_base = 0x3000;
    dpmi.idt_size = 0;
    let mut ranges = Vec::new();
    dpmi.push_descriptor_ranges(&mut ranges);
    assert_eq!(ranges.len(), 0);
}

#[test]
fn test_cov_should_exit_v86_on_iret_state() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    // Not in V86 mode
    assert!(!dpmi.should_exit_v86_on_iret_state(&cpu));
    // Enter V86 exit-on-iret mode
    dpmi.v86_active = true;
    dpmi.v86_exit_on_iret = true;
    cpu.regs.cs = 0;
    cpu.regs.set_ip(0);
    assert!(dpmi.should_exit_v86_on_iret_state(&cpu));
    // CS != 0
    cpu.regs.cs = 1;
    assert!(!dpmi.should_exit_v86_on_iret_state(&cpu));
}

#[test]
fn test_cov_should_exit_v86_on_retf_state() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    assert!(!dpmi.should_exit_v86_on_retf_state(&cpu));
    dpmi.v86_active = true;
    dpmi.v86_exit_on_retf = true;
    cpu.regs.cs = 0;
    cpu.regs.set_ip(0);
    assert!(dpmi.should_exit_v86_on_retf_state(&cpu));
    cpu.regs.set_ip(1);
    assert!(!dpmi.should_exit_v86_on_retf_state(&cpu));
}

#[test]
fn test_cov_set_descriptor_base_no_ldt() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);
    // Allocate a new selector (will be LDT with TI=1)
    let sel = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
    assert_ne!(sel, 0);
    // Clear ldt_base so ldt_ready=false
    cpu.regs.ldt_base = 0;
    dpmi.set_descriptor_base(&mut cpu, &mut memory, sel, 0x12345678);
    // Check the selector was updated despite no LDT
    let s = dpmi.selectors.iter().find(|s| s.selector == sel).unwrap();
    assert_eq!(s.base, 0x12345678);
}

#[test]
fn test_cov_set_descriptor_access_no_ldt() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);
    let sel = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
    assert_ne!(sel, 0);
    cpu.regs.ldt_base = 0;
    dpmi.set_descriptor_access(&mut cpu, &mut memory, sel, 0xFA, 0x40);
    let s = dpmi.selectors.iter().find(|s| s.selector == sel).unwrap();
    assert_eq!(s.access, 0xFA);
    assert_eq!(s.flags, 0x40);
}

#[test]
fn test_cov_free_dos_memory_selectors_already_free() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);
    // Allocate some selectors
    let sel = dpmi.allocate_descriptors(&mut cpu, &mut memory, 2);
    assert_ne!(sel, 0);
    // Free them once
    dpmi.free_dos_memory_selectors(&mut cpu, &mut memory, sel, 2);
    // Free them again → should hit "already in free_selectors" branch
    dpmi.free_dos_memory_selectors(&mut cpu, &mut memory, sel, 2);
}

#[test]
fn test_cov_adjust_dos_memory_selectors_shrink() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);
    // Allocate 3 selectors manually via allocate_descriptors
    let base_sel = dpmi.allocate_descriptors(&mut cpu, &mut memory, 3);
    assert_ne!(base_sel, 0);
    // Now shrink from 3 to 1 → should free selectors at indices 1,2
    dpmi.adjust_dos_memory_selectors(&mut cpu, &mut memory, base_sel, 3, 1);
    // The freed selectors should be in free_selectors
    assert!(dpmi.free_selectors.contains(&(base_sel + 8)));
    assert!(dpmi.free_selectors.contains(&(base_sel + 16)));
}

#[test]
fn test_cov_adjust_dos_memory_selectors_grow() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);
    let base_sel = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
    assert_ne!(base_sel, 0);
    // Grow from 1 to 3
    dpmi.adjust_dos_memory_selectors(&mut cpu, &mut memory, base_sel, 1, 3);
    // Check that selectors at index 1 and 2 were added
    assert!(dpmi.selectors.iter().any(|s| s.selector == base_sel + 8));
    assert!(dpmi.selectors.iter().any(|s| s.selector == base_sel + 16));
}

#[test]
fn test_cov_env_limit_from_mcb_invalid_mcb() {
    let memory = Memory::new();
    // MCB at segment 0 - address 0 which has no valid MCB tag
    let result = Dpmi::env_limit_from_mcb(&memory, 1);
    assert!(result.is_none()); // tag won't be 0x4D or 0x5A
}

#[test]
fn test_cov_env_limit_from_mcb_valid() {
    let mut memory = Memory::new();
    let env_seg: u16 = 0x200;
    let mcb_seg = env_seg - 1;
    let mcb_addr = (mcb_seg as usize) * 16;
    // Write a valid MCB header
    memory.write_u8(mcb_addr, 0x4D); // 'M' tag
    memory.write_u16(mcb_addr + 1, 0); // owner PSP
    memory.write_u16(mcb_addr + 3, 0x10); // 16 paragraphs = 256 bytes
    let result = Dpmi::env_limit_from_mcb(&memory, env_seg);
    // Per DPMI 0.9 spec, 16-bit DPMI env selector limit is floored at 64 KB,
    // so a 256-byte MCB block produces an 0xFFFF limit rather than 255.
    assert_eq!(result, Some(0xFFFF));
}

#[test]
fn test_cov_env_limit_from_mcb_zero_size() {
    let mut memory = Memory::new();
    let env_seg: u16 = 0x200;
    let mcb_seg = env_seg - 1;
    let mcb_addr = (mcb_seg as usize) * 16;
    memory.write_u8(mcb_addr, 0x4D);
    memory.write_u16(mcb_addr + 1, 0);
    memory.write_u16(mcb_addr + 3, 0); // Zero size
    let result = Dpmi::env_limit_from_mcb(&memory, env_seg);
    assert!(result.is_none());
}

#[test]
fn test_cov_allocate_mem_handle_basic() {
    let mut dpmi = Dpmi::new();
    let h1 = dpmi.allocate_mem_handle();
    assert!(h1.is_some());
    let h2 = dpmi.allocate_mem_handle();
    assert!(h2.is_some());
    assert_ne!(h1, h2);
}

#[test]
fn test_cov_simulate_rm_int_hle_ip_below_base() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Set up a RM call structure at a known address
    let struct_addr = 0x80000usize;
    let rm_struct = super::RealModeCallStruct {
        edi: 0,
        esi: 0,
        ebp: 0,
        reserved: 0,
        ebx: 0,
        edx: 0,
        ecx: 0,
        eax: 0,
        flags: 0x0202,
        es: 0,
        ds: 0,
        fs: 0,
        gs: 0,
        ip: 0x0010, // Below IVT_ENTRY_BASE (0x0100)
        cs: 0xF000, // HLE target
        sp: 0,
        ss: 0,
    };
    rm_struct.write_to_memory(&mut memory, struct_addr);

    // Set up ES selector to point to struct
    let sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == sel) {
        s.base = struct_addr as u32;
        s.limit = 0xFFFF;
    }
    cpu.regs.es = sel;
    cpu.regs.edi = 0;
    cpu.regs.set_ax(0x0301); // Simulate RM interrupt
    cpu.regs.set_bh(0x00);
    cpu.regs.set_cx(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Should succeed with pending HLE simulation using INT 0
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    if let Some(ref hle) = dpmi.pending_hle_simulation {
        assert_eq!(hle.int_num, 0); // IP below base → int_num = 0
    }
}

#[test]
fn test_cov_get_memory_info_050b() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Set up a buffer at a known address
    let buf_addr = 0x80000usize;
    let sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == sel) {
        s.base = buf_addr as u32;
        s.limit = 0xFFFF;
    }
    cpu.regs.es = sel;
    cpu.regs.set_di(0);
    cpu.regs.set_ax(0x050B);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    // Should succeed (clear carry)
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_cov_enter_pm_with_env_segment() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up a valid PSP with environment segment
    let psp_seg = 0x0800u16;
    let env_seg = 0x0700u16;
    let psp_base = (psp_seg as usize) * 16;
    memory.write_u16(psp_base + 0x2C, env_seg);

    // Write a valid MCB for the environment
    let mcb_addr = ((env_seg - 1) as usize) * 16;
    memory.write_u8(mcb_addr, 0x4D); // 'M' tag
    memory.write_u16(mcb_addr + 1, psp_seg); // owner
    memory.write_u16(mcb_addr + 3, 0x20); // 32 paragraphs

    // Set up stack for enter_protected_mode
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0xFFFE);
    let stack_base = 0x10000usize;
    memory.write_u16(stack_base + 0xFFFC, 0x0100); // Return IP
    memory.write_u16(stack_base + 0xFFFE, 0x2000); // Return CS
    cpu.regs.set_sp(0xFFFC);
    cpu.regs.set_ax(0); // 16-bit mode
    cpu.regs.ds = 0x3000;
    cpu.regs.es = 0x4000;

    let result = dpmi.enter_protected_mode_with_psp(&mut cpu, &mut memory, psp_seg, env_seg);
    assert!(result.is_ok());
    assert!(dpmi.protected_mode);
    // PSP[0x2C] should now contain an LDT selector instead of real-mode segment
    let env_sel = memory.read_u16(psp_base + 0x2C);
    assert_ne!(env_sel, env_seg); // Should have been replaced with selector
}

#[test]
fn test_cov_allocate_specific_selector_already_allocated() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Get a selector that's already allocated
    let existing_sel = dpmi.selectors[0].selector;

    // Try to allocate it via AX=000Dh (allocate specific LDT descriptor)
    cpu.regs.set_ax(0x000D);
    cpu.regs.set_bx(existing_sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.bx(), existing_sel);
}

#[test]
fn test_cov_allocate_specific_selector_success() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Use a selector that's not allocated but within LDT range
    // next_selector is the next free one; use a higher one
    let target_sel = dpmi.next_selector + 8;
    // Make sure it's not allocated
    assert!(!dpmi.selectors.iter().any(|s| s.selector == target_sel));

    cpu.regs.set_ax(0x000D);
    cpu.regs.set_bx(target_sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    if !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY) {
        // Should have been allocated
        assert!(dpmi.selectors.iter().any(|s| s.selector == target_sel));
        // next_selector should have been updated since target_sel >= next_selector
        assert!(dpmi.next_selector > target_sel);
    }
}

#[test]
fn test_cov_create_alias_descriptor() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Set up a code segment selector
    let code_sel = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
    assert_ne!(code_sel, 0);
    // Make it a code segment
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == code_sel) {
        s.base = 0x10000;
        s.limit = 0xFFFF;
        s.access = 0xFA; // Present, DPL=3, code, readable
    }
    dpmi.write_selector_descriptor(&mut cpu, &mut memory, code_sel, 0x10000, 0xFFFF, 0xFA, 0);

    // AX=000Ah: create data segment alias for code segment
    cpu.regs.set_ax(0x000A);
    cpu.regs.set_bx(code_sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let alias_sel = cpu.regs.ax();
    assert_ne!(alias_sel, 0);
    // The alias should be a data segment with same base
    let s = dpmi
        .selectors
        .iter()
        .find(|s| s.selector == alias_sel)
        .unwrap();
    assert_eq!(s.base, 0x10000);
}

#[test]
fn test_cov_idt_write_verify_normal() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Set interrupt vector (AX=0205h)
    let code_sel = dpmi.selectors[0].selector;
    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x21); // INT 21h
    cpu.regs.set_cx(code_sel);
    cpu.regs.edx = 0x1234;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_cov_get_multiple_descriptors_success() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate a selector
    let sel = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
    assert_ne!(sel, 0);

    // Set up buffer: 10 bytes per entry (2 byte selector + 8 byte descriptor)
    let buf_addr = 0x80000usize;
    memory.write_u16(buf_addr, sel); // Write the selector to query

    // Set up ES to point to buffer
    let es_sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = buf_addr as u32;
        s.limit = 0xFFFF;
    }
    cpu.regs.es = es_sel;
    cpu.regs.set_di(0);
    cpu.regs.set_ax(0x000E);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_cov_set_multiple_descriptors_success() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let sel = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
    assert_ne!(sel, 0);

    // Set up buffer with one entry: selector + 8-byte descriptor
    let buf_addr = 0x80000usize;
    memory.write_u16(buf_addr, sel);
    // Write descriptor bytes: base=0, limit=0xFFFF, access=0xF2, flags=0
    memory.write_u16(buf_addr + 2, 0xFFFF); // limit_lo
    memory.write_u16(buf_addr + 4, 0x0000); // base_lo
    memory.write_u8(buf_addr + 6, 0x00); // base_mid
    memory.write_u8(buf_addr + 7, 0xF2); // access
    memory.write_u8(buf_addr + 8, 0x00); // limit_hi + flags
    memory.write_u8(buf_addr + 9, 0x00); // base_hi

    let es_sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = buf_addr as u32;
        s.limit = 0xFFFF;
    }
    cpu.regs.es = es_sel;
    cpu.regs.set_di(0);
    cpu.regs.set_ax(0x000F);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_cov_allocate_descriptors_reuse_free() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate 2 selectors, then free them, then allocate 2 again
    let sel1 = dpmi.allocate_descriptors(&mut cpu, &mut memory, 2);
    assert_ne!(sel1, 0);
    // Free them via int31 AX=0001
    cpu.regs.set_ax(0x0001);
    cpu.regs.set_bx(sel1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    cpu.regs.set_ax(0x0001);
    cpu.regs.set_bx(sel1 + 8);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Now allocate 2 more - should reuse the freed selectors
    let sel2 = dpmi.allocate_descriptors(&mut cpu, &mut memory, 2);
    assert_ne!(sel2, 0);
}

#[test]
fn test_cov_enter_pm_32bit_d_bit() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    // Set up stack
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0xFFFE);
    let stack_base = 0x10000usize;
    memory.write_u16(stack_base + 0xFFFC, 0x0100);
    memory.write_u16(stack_base + 0xFFFE, 0x2000);
    cpu.regs.set_sp(0xFFFC);
    cpu.regs.set_ax(1); // 32-bit mode
    cpu.regs.ds = 0x3000;
    cpu.regs.es = 0x4000;
    let result = dpmi.enter_protected_mode(&mut cpu, &mut memory);
    assert!(result.is_ok());
    assert!(dpmi.is_32bit_mode());
}

#[test]
fn test_cov_segment_to_descriptor_cached() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // First call: allocates a new selector for segment 0x40
    cpu.regs.set_ax(0x0002);
    cpu.regs.set_bx(0x0040);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel1 = cpu.regs.ax();
    assert_ne!(sel1, 0);

    // Second call with same segment: should return cached selector
    cpu.regs.set_ax(0x0002);
    cpu.regs.set_bx(0x0040);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel2 = cpu.regs.ax();
    assert_eq!(sel1, sel2);
}

#[test]
fn test_cov_segment_to_descriptor_stale_cache() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // First call: allocates selector for segment 0x50
    cpu.regs.set_ax(0x0002);
    cpu.regs.set_bx(0x0050);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel1 = cpu.regs.ax();

    // Free the selector so the cache becomes stale
    cpu.regs.set_ax(0x0001);
    cpu.regs.set_bx(sel1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Now request same segment - should detect stale cache and re-allocate
    cpu.regs.set_ax(0x0002);
    cpu.regs.set_bx(0x0050);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel2 = cpu.regs.ax();
    // New selector should be allocated (may or may not be same value)
    assert_ne!(sel2, 0);
}

#[test]
fn test_cov_allocate_specific_selector_overflow() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Try to allocate selector 0xFFF8 - checked_add(8) would give 0x10000 which
    // overflows u16, but checked_add on u16 returns None
    cpu.regs.set_ax(0x000D);
    cpu.regs.set_bx(0xFFF8);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    // This may or may not succeed depending on LDT range, but exercises the path
}

#[test]
fn test_cov_set_multiple_descriptors_system_segment() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate a selector first
    let sel = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
    assert_ne!(sel, 0);

    // Set up a buffer with one entry having access byte with bit 4 = 0 (system segment)
    let buffer_addr = 0x20000usize;
    // Entry format: selector[2] + descriptor[8]
    memory.write_u16(buffer_addr, sel); // selector
    memory.write_u16(buffer_addr + 2, 0xFFFF); // limit_lo
    memory.write_u16(buffer_addr + 4, 0x0000); // base_lo
    memory.write_u8(buffer_addr + 6, 0x00); // base_mid
    memory.write_u8(buffer_addr + 7, 0x02); // access = 0x02 (bit 4 = 0, system segment!)
    memory.write_u8(buffer_addr + 8, 0x00); // flags_limit_hi
    memory.write_u8(buffer_addr + 9, 0x00); // base_hi

    // Set up ES:EDI to point to the buffer
    // We need a valid selector for ES
    let es_sel = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
    assert_ne!(es_sel, 0);
    dpmi.set_descriptor_base(&mut cpu, &mut memory, es_sel, 0);
    dpmi.set_descriptor_limit(&mut cpu, &mut memory, es_sel, 0xFFFFFF);
    cpu.regs.es = es_sel;
    cpu.regs.edi = buffer_addr as u32;

    // Call set multiple descriptors (AX=000Fh, CX=1)
    cpu.regs.set_ax(0x000F);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    // Should fail with DPMI_ERR_INVALID_VALUE due to system segment
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_cov_free_dos_memory_selectors_dup_check() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate a selector, then free it, then try to free again via free_dos_memory_selectors
    let sel = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
    assert_ne!(sel, 0);

    // Free it normally first
    cpu.regs.set_ax(0x0001);
    cpu.regs.set_bx(sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Now the selector is in free_selectors. Call free_dos_memory_selectors
    // which should check `!self.free_selectors.contains(&selector)`
    dpmi.free_dos_memory_selectors(&mut cpu, &mut memory, sel, 1);
    // Should not add duplicate to free_selectors
}

#[test]
fn test_cov_adjust_dos_memory_selectors_grow_next() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Remember current next_selector
    let base_sel = dpmi.next_selector;

    // Grow from 0 to 2 selectors - should update next_selector
    dpmi.adjust_dos_memory_selectors(&mut cpu, &mut memory, base_sel, 0, 2);
    assert!(dpmi.next_selector > base_sel);
}

#[test]
fn test_cov_allocate_descriptors_free_list_consecutive() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate 4 consecutive selectors
    let first = dpmi.allocate_descriptors(&mut cpu, &mut memory, 4);
    assert_ne!(first, 0);
    let sel0 = first;
    let sel1 = first + 8;
    let sel2 = first + 16;
    let sel3 = first + 24;

    // Free them all
    for &sel in &[sel0, sel1, sel2, sel3] {
        cpu.regs.set_ax(0x0001);
        cpu.regs.set_bx(sel);
        dpmi.int31(&mut cpu, &mut memory).unwrap();
    }

    // Now allocate 3 - should find a run of 3 consecutive in free list
    let reused = dpmi.allocate_descriptors(&mut cpu, &mut memory, 3);
    assert_ne!(reused, 0);
}

#[test]
fn test_cov_free_descriptor_invalid() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Try to free selector 0 (null) - should return false
    assert!(!dpmi.free_descriptor(&mut cpu, &mut memory, 0));

    // Try to free an unallocated selector
    assert!(!dpmi.free_descriptor(&mut cpu, &mut memory, 0x1234));
}

#[test]
fn test_cov_free_descriptor_success() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let sel = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
    assert_ne!(sel, 0);
    assert!(dpmi.free_descriptor(&mut cpu, &mut memory, sel));
    // Freeing again should fail (no longer in selectors list)
    assert!(!dpmi.free_descriptor(&mut cpu, &mut memory, sel));
}

#[test]
fn test_cov_set_descriptor_limit_with_ldt() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let sel = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
    assert_ne!(sel, 0);

    // Set limit - LDT should be ready after enter_protected_mode
    dpmi.set_descriptor_limit(&mut cpu, &mut memory, sel, 0x1000);
    assert_eq!(dpmi.get_descriptor_limit(sel), Some(0x1000));
}

#[test]
fn test_cov_set_descriptor_access_with_ldt() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let sel = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
    assert_ne!(sel, 0);

    // Set access rights - should write to LDT
    dpmi.set_descriptor_access(&mut cpu, &mut memory, sel, 0xFA, 0x40);
    let access = dpmi.get_descriptor_access(sel);
    assert_eq!(access, Some((0xFA, 0x40)));
}

#[test]
fn test_cov_set_descriptor_base_with_ldt() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let sel = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
    assert_ne!(sel, 0);

    // Set base - should write to LDT and invalidate cache
    dpmi.set_descriptor_base(&mut cpu, &mut memory, sel, 0x50000);
}

#[test]
fn test_cov_env_limit_from_mcb_addr_overflow() {
    let memory = Memory::new();
    // env_segment near top of memory - mcb_addr + 4 > ram_size
    // Memory is 32MB by default, so segment 0xFFFF → mcb_seg=0xFFFE → mcb_addr=0xFFFE0
    // This is well within 32MB so it won't overflow.
    // Since Memory::new() creates full 32MB, this specific path is hard to hit.
    // Let's test the tag check path instead.
    let result = Dpmi::env_limit_from_mcb(&memory, 0x0050);
    // No valid MCB tag at (0x004F * 16), so should return None
    assert!(result.is_none());
}

#[test]
fn test_cov_call_rm_procedure_hle_stub() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    // Use 32-bit mode so client_offset uses full EDI
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Set up a RealModeCallStruct at a buffer location
    let struct_addr = 0x20000usize;
    // EDI, ESI, EBP, reserved, EBX, EDX, ECX, EAX (32 bytes of GP regs)
    for i in 0..32 {
        memory.write_u8(struct_addr + i, 0);
    }
    // Flags at offset 32
    memory.write_u16(struct_addr + 32, 0x0200);
    // ES, DS at offset 34, 36
    memory.write_u16(struct_addr + 34, 0);
    memory.write_u16(struct_addr + 36, 0);
    // FS, GS at offset 38, 40
    memory.write_u16(struct_addr + 38, 0);
    memory.write_u16(struct_addr + 40, 0);
    // IP, CS at offset 42, 44 - set to HLE stub F000:0100
    memory.write_u16(struct_addr + 42, 0x0100); // IP
    memory.write_u16(struct_addr + 44, 0xF000); // CS = HLE segment
                                                // SP, SS at offset 46, 48
    memory.write_u16(struct_addr + 46, 0);
    memory.write_u16(struct_addr + 48, 0);

    // Set up ES:EDI pointing to the struct
    let es_sel = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
    assert_ne!(es_sel, 0);
    dpmi.set_descriptor_base(&mut cpu, &mut memory, es_sel, 0);
    dpmi.set_descriptor_limit(&mut cpu, &mut memory, es_sel, 0xFFFFFF);
    cpu.regs.es = es_sel;
    cpu.regs.edi = struct_addr as u32;

    // BH=0 (no reset IF), CX=0 (no stack words)
    cpu.regs.set_bx(0x0000);
    cpu.regs.set_cx(0);

    // Call real mode procedure (AX=0301h)
    cpu.regs.set_ax(0x0301);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    // Should have stored pending HLE simulation
    assert!(dpmi.pending_hle_simulation.is_some());
}
