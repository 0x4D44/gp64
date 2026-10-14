use super::*;

#[test]
fn test_dpmi_trace_functions_exercise() {
    // dpmi_trace_enabled() now uses tracing::enabled!() with target gp64::dos::dpmi.
    // Just exercise the trace functions to verify they work.
    let _ = dpmi_trace_enabled();
    let _ = dpmi_trace_env_enabled();
    // dpmi_trace_emit() should not panic when trace is disabled
    dpmi_trace_emit("[DPMI TEST] This should not appear while the DPMI trace gate is off");
}

#[test]
fn test_dpmi_new() {
    let dpmi = Dpmi::new();
    assert!(!dpmi.protected_mode);
    assert!(dpmi.selectors.is_empty());
    assert_eq!(dpmi.next_selector, 0x0107);
    assert!(dpmi.memory_blocks.is_empty());
}

#[test]
fn test_dpmi_default() {
    let dpmi = Dpmi::default();
    assert!(!dpmi.protected_mode);
    assert_eq!(dpmi.next_selector, 0x0107);
}

#[test]
fn test_dpmi_reset_clears_state() {
    let mut dpmi = Dpmi::new();

    // Set up some state to verify it gets cleared
    dpmi.protected_mode = true;
    dpmi.cpu_mode = 1;
    dpmi.next_selector = 0x0200;
    dpmi.next_mem_handle = 42;
    dpmi.virtual_interrupts_enabled = false;
    dpmi.env_selector = 0x1234;
    dpmi.v86_active = true;
    dpmi.v86_exit_on_iret = true;
    dpmi.pm_callback_depth = 1;

    // Add some entries to vectors
    dpmi.selectors.push(Selector {
        selector: 0x0107,
        base: 0x1000,
        limit: 0xFFFF,
        access: 0x92,
        flags: 0,
        lock_count: 0,
    });
    dpmi.memory_blocks
        .push(MemoryBlock::new(1, 0x100000, 0x10000));
    dpmi.free_selectors.push(0x0107);
    dpmi.pending_interrupts.push(0x21);
    dpmi.physical_mappings.push(PhysicalMapping {
        physical: 0xA0000,
        linear: 0xA0000,
        size: 0x10000,
    });
    dpmi.dos_memory_blocks.push(DosMemoryBlock {
        segment: 0x1000,
        selector: 0x0107,
        paragraphs: 0x100,
        selector_count: 1,
    });

    // Modify a callback
    dpmi.rm_callbacks[0].in_use = true;
    dpmi.rm_callbacks[0].pm_selector = 0x08;

    // Modify an exception handler
    dpmi.exception_handlers[0x0D].selector = 0x08;
    dpmi.exception_handlers[0x0D].offset = 0x1234;

    // Modify a watchpoint
    dpmi.debug_watchpoints[0].active = true;
    dpmi.debug_watchpoints[0].linear_address = 0x1000;

    // Set page lock stats
    dpmi.page_lock_stats.lock_calls = 10;

    // Call reset
    dpmi.reset();

    // Verify everything is cleared
    assert!(!dpmi.protected_mode);
    assert_eq!(dpmi.cpu_mode, 0);
    assert_eq!(dpmi.next_selector, 0x0107);
    assert_eq!(dpmi.next_mem_handle, 1);
    assert!(dpmi.virtual_interrupts_enabled);
    assert_eq!(dpmi.env_selector, 0);
    assert!(!dpmi.v86_active);
    assert!(!dpmi.v86_exit_on_iret);
    assert_eq!(dpmi.pm_callback_depth, 0);

    // Verify vectors are empty
    assert!(dpmi.selectors.is_empty());
    assert!(dpmi.memory_blocks.is_empty());
    assert!(dpmi.free_selectors.is_empty());
    assert!(dpmi.pending_interrupts.is_empty());
    assert!(dpmi.physical_mappings.is_empty());
    assert!(dpmi.dos_memory_blocks.is_empty());
    assert!(dpmi.linear_memory_blocks.is_empty());

    // Verify callbacks are reset
    assert!(!dpmi.rm_callbacks[0].in_use);
    assert_eq!(dpmi.rm_callbacks[0].pm_selector, 0);

    // Verify exception handlers are reset
    assert_eq!(dpmi.exception_handlers[0x0D].selector, 0);
    assert_eq!(dpmi.exception_handlers[0x0D].offset, 0);

    // Verify watchpoints are reset
    assert!(!dpmi.debug_watchpoints[0].active);
    assert_eq!(dpmi.debug_watchpoints[0].linear_address, 0);

    // Verify page lock stats are reset
    assert_eq!(dpmi.page_lock_stats.lock_calls, 0);

    // Verify PM IDT is reset to default values (not zeroed)
    assert_eq!(dpmi.pm_idt[0].selector, 0x0008);
    assert_eq!(dpmi.pm_idt[0].offset, 0xFF00);
    assert!(dpmi.pm_idt[0].present);
}

#[test]
fn test_tile_count_for_size() {
    // ceil(size / 64 KB), with a zero-size block still owning one tile.
    assert_eq!(Dpmi::tile_count_for_size(0), 1);
    assert_eq!(Dpmi::tile_count_for_size(1), 1);
    assert_eq!(Dpmi::tile_count_for_size(0x10000), 1); // exactly 64 KB
    assert_eq!(Dpmi::tile_count_for_size(0x10001), 2); // 64 KB + 1
    assert_eq!(Dpmi::tile_count_for_size(4 * 65_536 + 1), 5); // one byte into tile five
    assert_eq!(Dpmi::tile_count_for_size(0x00FF_0000), 255); // ~16 MB → 255 tiles
                                                             // Pathological u32 size saturates the count rather than wrapping u16.
    assert_eq!(Dpmi::tile_count_for_size(0xFFFF_FFFF), u16::MAX);
}

#[test]
fn test_alloc_and_free_tiled_block_selector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    const BASE: u32 = 0x0040_0000; // independently chosen aligned allocation
    const SIZE: u32 = 4 * 65_536 + 1; // one byte into tile five

    let first = dpmi.alloc_tiled_block_selector(&mut cpu, &mut memory, BASE, SIZE);
    assert_ne!(first, 0, "tiled alloc must succeed");
    assert_eq!((first & 0x04), 0x04, "must be an LDT selector (TI=1)");

    // Allocation policy: contiguous +8 stride, base = BASE + tile*64K.
    // The limit for tile t spans to the end of the block (= size-1 - t*64K),
    // so the base selector covers the whole
    // allocation (32-bit flat access) while later tiles cover 16-bit offsets.
    for tile in 0u32..5 {
        let sel = first.wrapping_add((tile as u16) * 8);
        assert_eq!(
            dpmi.get_selector_base(sel),
            Some(BASE + tile * 0x10000),
            "tile {tile} base"
        );
        let expected_limit = (SIZE - 1).saturating_sub(tile * 0x10000);
        assert_eq!(
            dpmi.get_descriptor_limit(sel),
            Some(expected_limit),
            "tile {tile} limit"
        );
        // Global-block descriptors include the accessed bit before any load.
        assert_eq!(
            dpmi.get_descriptor_access(sel),
            Some((0xF3, 0)),
            "tile {tile} access"
        );
    }

    // Free the whole run; every tile selector is released.
    dpmi.free_tiled_descriptors(&mut cpu, &mut memory, first, 5);
    for tile in 0u16..5 {
        assert_eq!(
            dpmi.get_selector_base(first.wrapping_add(tile * 8)),
            None,
            "tile {tile} should be freed"
        );
    }

    // A ≤64 KB block is a single selector with limit = size-1 — the prior
    // single-selector behaviour, unchanged.
    let small = dpmi.alloc_tiled_block_selector(&mut cpu, &mut memory, 0x50000, 0x4000);
    assert_ne!(small, 0);
    assert_eq!(dpmi.get_selector_base(small), Some(0x50000));
    assert_eq!(dpmi.get_descriptor_limit(small), Some(0x3FFF));
    // first+8 is not part of a 1-tile block.
    assert_eq!(dpmi.get_selector_base(small.wrapping_add(8)), None);
}

#[test]
fn test_client_offsets_follow_cpu_mode() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();

    cpu.regs.edi = 0x1234_5678;
    cpu.regs.edx = 0xDEAD_BEEF;

    // 16-bit client: uses DI/DX (16-bit)
    dpmi.cpu_mode = 0;
    assert_eq!(dpmi.client_offset(&cpu), 0x5678);
    assert_eq!(dpmi.client_offset_reg(&cpu), 0xBEEF);
    dpmi.set_client_offset_reg(&mut cpu, 0xCAFE_BABE);
    assert_eq!(cpu.regs.dx(), 0xBABE);

    // 32-bit client: uses EDI/EDX (32-bit)
    dpmi.cpu_mode = 1;
    assert_eq!(dpmi.client_offset(&cpu), 0x1234_5678);
    assert_eq!(dpmi.client_offset_reg(&cpu), 0xDEAD_BABE);
    dpmi.set_client_offset_reg(&mut cpu, 0xCAFE_BABE);
    assert_eq!(cpu.regs.edx, 0xCAFE_BABE);
}

#[test]
fn test_exception_handler_gate_ignores_handler_descriptor_width() {
    // MDD-BUG-KILN-00635: the exception frame width follows the CLIENT's
    // bitness, not the handler code segment's D bit. An earlier revision
    // USE16 #GP handler got a word frame and cascaded to the reentry
    // threshold). See test_exception_handler_frame_width_follows_client_bitness
    // for the full 0203h/0213h matrix; this one exercises the
    // interrupt_services entry point directly.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1;
    cpu.regs.gdtr.base = 0x20000;
    cpu.regs.gdtr.limit = 0x17;

    dpmi.write_descriptor_raw(&mut memory, 0x20000, 0, 0, 0, 0, 0);
    dpmi.write_descriptor_raw(&mut memory, 0x20000, 1, 0, 0xFFFF, 0xFA, 0x0);
    dpmi.write_descriptor_raw(&mut memory, 0x20000, 2, 0, 0xFFFFF, 0xFA, 0xC);

    // 32-bit client + USE16 handler segment: still a 32-bit frame.
    cpu.regs.set_al(0x03);
    cpu.regs.set_bl(0x0D);
    cpu.regs.set_cx(0x08);
    cpu.cs_bits = 16;
    cpu.regs.set_dx(0x1234);
    dpmi.interrupt_services(&mut cpu, &mut memory).unwrap();
    assert_eq!(dpmi.exception_handlers[0x0D].gate_type, 0x0E);

    // 16-bit client + USE32 handler segment: still a 16-bit frame.
    dpmi.cpu_mode = 0;
    cpu.regs.set_cx(0x10);
    cpu.cs_bits = 16;
    cpu.regs.set_dx(0x5678);
    dpmi.interrupt_services(&mut cpu, &mut memory).unwrap();
    assert_eq!(dpmi.exception_handlers[0x0D].gate_type, 0x06);
}

#[test]
fn test_int2f_dpmi_detect() {
    let _lock = env_lock();
    let dpmi = Dpmi::new();
    let mut cpu = Cpu::new();

    cpu.regs.set_ax(0x1687);
    dpmi.int2f_dpmi_detect(&mut cpu);

    assert_eq!(cpu.regs.ax(), 0); // DPMI available
    assert_eq!(cpu.regs.bx(), 0x0003); // 16-bit + 32-bit supported
    assert_eq!(cpu.regs.cl(), 0x03); // 386+
    assert_eq!(cpu.regs.dx(), 0x005A); // Version 0.90
    assert_eq!(cpu.regs.es, 0xF000);
    assert_eq!(cpu.regs.di(), 0xFF10);
}

#[test]
fn test_int2f_non_dpmi_call() {
    let dpmi = Dpmi::new();
    let mut cpu = Cpu::new();

    cpu.regs.set_ax(0x1234); // Not a DPMI detection call
    let original_ax = cpu.regs.ax();
    dpmi.int2f_dpmi_detect(&mut cpu);

    // Should not modify anything
    assert_eq!(cpu.regs.ax(), original_ax);
}

#[test]
fn test_memory_info_uses_configured_ram_size() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    memory.ensure_ram_size(4 * 1024 * 1024);

    cpu.regs.set_al(0x00); // Get free memory info
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0x0100);

    let result = dpmi.memory_services(&mut cpu, &mut memory);
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let buffer = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.di());
    let total_pages = memory.read_u32(buffer + 0x0C);
    let expected_pages = ((memory.ram_size() as u32).saturating_sub(0x100000)) / 4096;
    assert_eq!(total_pages, expected_pages);
}

#[test]
fn test_memory_info_invalid_selector_sets_carry() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.protected_mode = true;
    cpu.regs.set_al(0x00);
    cpu.regs.es = 0x0004; // LDT selector without entry
    cpu.regs.set_di(0x0100);

    dpmi.memory_services(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_memory_info_with_gap_updates_free_bytes() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    memory.ensure_ram_size(4 * 1024 * 1024);

    dpmi.memory_blocks
        .push(MemoryBlock::new(1, 0x200000, 0x1000));

    cpu.regs.set_al(0x00);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0x0100);

    dpmi.memory_services(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let buffer = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.di());
    let largest_free = memory.read_u32(buffer);
    let total_bytes = memory.read_u32(buffer + 0x0C) * 4096;
    assert!(largest_free > 0);
    assert!(largest_free < total_bytes);
}

#[test]
fn test_memory_info_full_range_no_tail_gap() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    memory.ensure_ram_size(2 * 1024 * 1024);
    let ram_end = memory.ram_size() as u32;

    dpmi.memory_blocks.push(MemoryBlock::new(
        1,
        0x100000,
        ram_end.saturating_sub(0x100000),
    ));

    cpu.regs.set_al(0x00);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0x0200);
    dpmi.memory_services(&mut cpu, &mut memory).unwrap();

    let buffer = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.di());
    let largest_free = memory.read_u32(buffer);
    assert_eq!(largest_free, 0);
}

#[test]
fn test_memory_services_alloc_size_zero_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_al(0x01);
    cpu.regs.set_bx(0);
    cpu.regs.set_cx(0);
    dpmi.memory_services(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_memory_services_resize_zero_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_al(0x03);
    cpu.regs.set_si(0);
    cpu.regs.set_di(0);
    cpu.regs.set_bx(0);
    cpu.regs.set_cx(0);
    dpmi.memory_services(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_memory_services_resize_too_large_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let handle = 0x1234;
    dpmi.memory_blocks.push(MemoryBlock::new(
        handle,
        (DOS_RAM_CEILING as u32) - 0x1000,
        0x1000,
    ));

    cpu.regs.set_al(0x03);
    cpu.regs.set_si((handle >> 16) as u16);
    cpu.regs.set_di(handle as u16);
    cpu.regs.set_bx(0);
    cpu.regs.set_cx(0x2000);
    dpmi.memory_services(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_LINEAR_UNAVAILABLE);
}

#[test]
fn test_memory_services_resize_overlap_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let handle_a = 0x1001;
    let handle_b = 0x1002;
    dpmi.memory_blocks
        .push(MemoryBlock::new(handle_a, 0x200000, 0x2000));
    dpmi.memory_blocks
        .push(MemoryBlock::new(handle_b, 0x202000, 0x2000));

    cpu.regs.set_al(0x03);
    cpu.regs.set_si((handle_a >> 16) as u16);
    cpu.regs.set_di(handle_a as u16);
    cpu.regs.set_bx(0x0000);
    cpu.regs.set_cx(0x4000);
    dpmi.memory_services(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_LINEAR_UNAVAILABLE);
}

#[test]
fn test_memory_services_resize_overlap_physical_mapping_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let handle = 0x1003;
    dpmi.memory_blocks
        .push(MemoryBlock::new(handle, 0x200000, 0x1000));
    dpmi.physical_mappings.push(PhysicalMapping {
        physical: 0x0,
        linear: 0x202000,
        size: 0x2000,
    });

    cpu.regs.set_al(0x03);
    cpu.regs.set_si((handle >> 16) as u16);
    cpu.regs.set_di(handle as u16);
    cpu.regs.set_bx(0x0000);
    cpu.regs.set_cx(0x4000);
    dpmi.memory_services(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_LINEAR_UNAVAILABLE);
}

#[test]
fn test_memory_services_resize_overlap_linear_block_fails_atomically() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let handle = 0x1004;
    dpmi.memory_blocks
        .push(MemoryBlock::new(handle, 0x200000, 0x1000));
    dpmi.linear_memory_blocks
        .push(LinearMemoryBlock::new(0x1005, 0x201000, 0x1000, true));
    let ram_size_before = memory.ram_size();

    cpu.regs.set_al(0x03);
    cpu.regs.set_si((handle >> 16) as u16);
    cpu.regs.set_di(handle as u16);
    cpu.regs.set_bx(0x0000);
    cpu.regs.set_cx(0x2000);
    dpmi.memory_services(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_LINEAR_UNAVAILABLE);
    assert_eq!(dpmi.memory_blocks[0].size, 0x1000);
    assert_eq!(dpmi.memory_blocks[0].capacity, 0x1000);
    assert_eq!(memory.ram_size(), ram_size_before);
}

#[test]
fn test_memory_services_resize_overlap_descriptor_table_fails_atomically() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let handle = 0x1006;
    dpmi.memory_blocks
        .push(MemoryBlock::new(handle, 0x200000, 0x1000));
    dpmi.gdt_base = 0x201000;
    dpmi.gdt_size = 0x1000;
    let ram_size_before = memory.ram_size();

    cpu.regs.set_al(0x03);
    cpu.regs.set_si((handle >> 16) as u16);
    cpu.regs.set_di(handle as u16);
    cpu.regs.set_bx(0x0000);
    cpu.regs.set_cx(0x2000);
    dpmi.memory_services(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_LINEAR_UNAVAILABLE);
    assert_eq!(dpmi.memory_blocks[0].size, 0x1000);
    assert_eq!(dpmi.memory_blocks[0].capacity, 0x1000);
    assert_eq!(memory.ram_size(), ram_size_before);
}

#[test]
fn test_memory_services_resize_updates_block() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let handle = 0x1004;
    dpmi.memory_blocks
        .push(MemoryBlock::new(handle, 0x200000, 0x1000));

    cpu.regs.set_al(0x03);
    cpu.regs.set_si((handle >> 16) as u16);
    cpu.regs.set_di(handle as u16);
    cpu.regs.set_bx(0x0000);
    cpu.regs.set_cx(0x2000);
    dpmi.memory_services(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dpmi.memory_blocks[0].size, 0x2000);
}

#[test]
fn test_memory_services_resize_ignores_non_overlapping_ranges() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let target_handle = 0x2005;
    dpmi.memory_blocks
        .push(MemoryBlock::new(0x2004, 0x100000, 0x1000));
    dpmi.memory_blocks
        .push(MemoryBlock::new(target_handle, 0x300000, 0x1000));
    dpmi.physical_mappings.push(PhysicalMapping {
        physical: 0x0,
        linear: 0x100000,
        size: 0x1000,
    });

    cpu.regs.set_al(0x03);
    cpu.regs.set_si((target_handle >> 16) as u16);
    cpu.regs.set_di(target_handle as u16);
    cpu.regs.set_bx(0x0000);
    cpu.regs.set_cx(0x2000);
    dpmi.memory_services(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let updated = dpmi
        .memory_blocks
        .iter()
        .find(|b| b.handle == target_handle)
        .unwrap();
    assert_eq!(updated.size, 0x2000);
}

#[test]
fn test_int31_physical_mapping_rejects_unaligned() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0800);
    cpu.regs.set_bx(0x0000);
    cpu.regs.set_cx(0x0001); // Unaligned physical address
    cpu.regs.set_si(0x0000);
    cpu.regs.set_di(0x1000); // Aligned size

    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_int31_physical_mapping_rejects_unaligned_size() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0800);
    cpu.regs.set_bx(0x0000);
    cpu.regs.set_cx(0x1000); // Aligned physical address
    cpu.regs.set_si(0x0000);
    cpu.regs.set_di(0x1001); // Unaligned size

    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_int31_physical_mapping_rejects_zero_size() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0800);
    cpu.regs.set_bx(0x0000);
    cpu.regs.set_cx(0x1000);
    cpu.regs.set_si(0x0000);
    cpu.regs.set_di(0x0000); // Zero size

    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_int31_physical_mapping_rejects_overflowing_size() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let physical = (DOS_RAM_CEILING as u32).saturating_sub(0x1000);
    cpu.regs.set_ax(0x0800);
    cpu.regs.set_bx((physical >> 16) as u16);
    cpu.regs.set_cx(physical as u16);
    cpu.regs.set_si(0x0000);
    cpu.regs.set_di(0x2000); // size pushes past DOS_RAM_CEILING

    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_LINEAR);
}

#[test]
fn test_int31_physical_mapping_rejects_unknown_subfunc() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0802); // Undefined subfunction
    cpu.regs.set_bx(0x0000);
    cpu.regs.set_cx(0x1000);

    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_UNSUPPORTED);
}

#[test]
fn test_int31_physical_mapping_rejects_no_linear_space() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.memory_blocks.push(MemoryBlock::new(
        1,
        0x100000,
        (DOS_RAM_CEILING as u32).saturating_sub(0x100000),
    ));

    cpu.regs.set_ax(0x0800);
    cpu.regs.set_bx(0x0000);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_si(0x0000);
    cpu.regs.set_di(0x1000);

    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_LINEAR_UNAVAILABLE);
}

#[test]
fn test_physical_mapping_rejects_overlapping_physical_address() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // First mapping: physical 0xE0000, size 0x10000 (64K)
    cpu.regs.set_ax(0x0800);
    cpu.regs.set_bx(0x000E);
    cpu.regs.set_cx(0x0000); // physical = 0xE0000
    cpu.regs.set_si(0x0001);
    cpu.regs.set_di(0x0000); // size = 0x10000
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "First mapping should succeed"
    );

    // Second mapping: physical 0xE8000, size 0x10000 - overlaps with first
    cpu.regs.set_ax(0x0800);
    cpu.regs.set_bx(0x000E);
    cpu.regs.set_cx(0x8000); // physical = 0xE8000
    cpu.regs.set_si(0x0001);
    cpu.regs.set_di(0x0000); // size = 0x10000
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Should fail due to overlapping physical address
    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Overlapping physical mapping should fail"
    );
    assert_eq!(cpu.regs.ax(), DPMI_ERR_LINEAR_UNAVAILABLE);
}

#[test]
fn test_physical_mapping_accepts_non_overlapping_physical_address() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // First mapping: physical 0xE0000, size 0x10000 (64K)
    cpu.regs.set_ax(0x0800);
    cpu.regs.set_bx(0x000E);
    cpu.regs.set_cx(0x0000); // physical = 0xE0000
    cpu.regs.set_si(0x0001);
    cpu.regs.set_di(0x0000); // size = 0x10000
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "First mapping should succeed"
    );

    // Second mapping: physical 0xF0000, size 0x10000 - does NOT overlap
    cpu.regs.set_ax(0x0800);
    cpu.regs.set_bx(0x000F);
    cpu.regs.set_cx(0x0000); // physical = 0xF0000
    cpu.regs.set_si(0x0001);
    cpu.regs.set_di(0x0000); // size = 0x10000
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Should succeed - no overlap
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Non-overlapping physical mapping should succeed"
    );
}

#[test]
fn test_int31_virtual_interrupt_disable_reports_disabled_state() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.virtual_interrupts_enabled = false;
    cpu.regs.set_ax(0x0900);

    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert_eq!(cpu.regs.al(), 0);
    assert!(!dpmi.virtual_interrupts_enabled);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int31_virtual_interrupt_enable_reports_enabled_state() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.virtual_interrupts_enabled = true;
    cpu.regs.set_ax(0x0901);

    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert_eq!(cpu.regs.al(), 1);
    assert!(dpmi.virtual_interrupts_enabled);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_interrupt_queuing_when_virtual_interrupts_disabled() {
    let mut dpmi = Dpmi::new();

    // Queue starts empty
    assert!(!dpmi.has_pending_interrupts());
    assert_eq!(dpmi.pop_pending_interrupt(), None);

    // Queue an interrupt
    dpmi.queue_interrupt(0x08); // Timer interrupt
    assert!(dpmi.has_pending_interrupts());

    // Queueing the same interrupt again doesn't duplicate
    dpmi.queue_interrupt(0x08);
    assert!(dpmi.has_pending_interrupts());

    // Pop returns the queued interrupt
    assert_eq!(dpmi.pop_pending_interrupt(), Some(0x08));
    assert!(!dpmi.has_pending_interrupts());
    assert_eq!(dpmi.pop_pending_interrupt(), None);
}

#[test]
fn test_interrupt_queue_fifo_order() {
    let mut dpmi = Dpmi::new();

    // Queue multiple different interrupts
    dpmi.queue_interrupt(0x08); // Timer
    dpmi.queue_interrupt(0x09); // Keyboard
    dpmi.queue_interrupt(0x0A); // Cascade/slave PIC

    // Should come out in FIFO order
    assert_eq!(dpmi.pop_pending_interrupt(), Some(0x08));
    assert_eq!(dpmi.pop_pending_interrupt(), Some(0x09));
    assert_eq!(dpmi.pop_pending_interrupt(), Some(0x0A));
    assert_eq!(dpmi.pop_pending_interrupt(), None);
}

#[test]
fn test_interrupt_queue_no_duplicates() {
    let mut dpmi = Dpmi::new();

    // Queue same interrupt multiple times
    dpmi.queue_interrupt(0x08);
    dpmi.queue_interrupt(0x09);
    dpmi.queue_interrupt(0x08); // Duplicate - should be ignored

    // Should only have two distinct interrupts
    assert_eq!(dpmi.pop_pending_interrupt(), Some(0x08));
    assert_eq!(dpmi.pop_pending_interrupt(), Some(0x09));
    assert_eq!(dpmi.pop_pending_interrupt(), None);
}

#[test]
fn test_int31_get_version() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0400); // Get DPMI version
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert_eq!(cpu.regs.ax(), 0x005A); // Version 0.90
    assert_eq!(cpu.regs.bx(), 0x0003); // Flags (16-bit + 32-bit supported)
    assert_eq!(cpu.regs.cl(), 0x03); // 386
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int31_get_capabilities_unsupported_16bit() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0401);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0x0100);

    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_UNSUPPORTED);
}

#[test]
fn test_int31_get_capabilities_unsupported_32bit() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1;
    cpu.regs.set_ax(0x0401);
    cpu.regs.es = 0x2000;
    cpu.regs.edi = 0x0200;

    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_UNSUPPORTED);
}

#[test]
fn test_linear_memory_block_commit_tracking() {
    let mut block = LinearMemoryBlock::new(1, 0x1000, 0x2000, false);
    assert!(!block.is_page_committed(0));
    assert!(!block.is_page_committed(99));

    block.committed.truncate(0);
    block.set_page_committed(0, true);
    assert!(block.is_page_committed(0));

    block.set_page_committed(1, false);
    assert!(!block.is_page_committed(1));

    block.resize_pages(4, true);
    assert!(block.is_page_committed(3));

    block.resize_pages(3, false);
    assert_eq!(block.page_count, 3);
}

#[test]
fn test_linear_memory_block_set_page_committed_out_of_range_noop() {
    let mut block = LinearMemoryBlock::new(1, 0x1000, 0x1000, false);
    let committed_before = block.committed.clone();
    block.set_page_committed(5, true);
    assert_eq!(block.committed, committed_before);
    assert_eq!(block.page_count, 1);
}

#[test]
fn test_linear_memory_block_resize_grow_without_commit() {
    let mut block = LinearMemoryBlock::new(1, 0x1000, 0x1000, false);
    block.resize_pages(3, false);
    assert!(!block.is_page_committed(2));
    assert_eq!(block.page_count, 3);
}

#[test]
fn test_linear_memory_block_resize_multiple_of_64_pages() {
    let mut block = LinearMemoryBlock::new(1, 0x1000, 0x1000 * 65, true);
    assert_eq!(block.page_count, 65);
    assert_eq!(block.committed.len(), 2);

    block.resize_pages(64, false);
    assert_eq!(block.page_count, 64);
    assert_eq!(block.committed.len(), 1);
}

#[test]
fn test_linear_memory_block_resize_shrink_multiple_of_64_pages_keeps_full_word() {
    let mut block = LinearMemoryBlock::new(1, 0x1000, 0x1000 * 128, true);
    block.resize_pages(64, false);
    assert_eq!(block.page_count, 64);
    assert_eq!(block.committed.len(), 1);
    assert_eq!(block.committed[0], u64::MAX);
}

#[test]
fn test_linear_memory_block_resize_to_zero_clears_pages() {
    let mut block = LinearMemoryBlock::new(1, 0x1000, 0x1000 * 2, true);
    block.resize_pages(0, false);
    assert_eq!(block.page_count, 0);
    assert!(block.committed.is_empty());
}

#[test]
fn test_int31_output_buffer_write_protected_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.cr3 = 0x1000;
    cpu.regs.cr0 |= 0x8000_0000 | 0x0001_0000; // PG + WP

    let buffer_offset = 0x0100u16;
    let buffer_linear = cpu.regs.es_cache.base + buffer_offset as u32;
    let page_base = buffer_linear & 0xFFFFF000;

    memory.write_u32(0x1000, 0x2000 | 0x1); // PDE: present, read-only
    let pte_index = ((page_base >> 12) & 0x3FF) as usize;
    let pte_addr = 0x2000usize + pte_index * 4;
    memory.write_u32(pte_addr, page_base | 0x1); // PTE: present, read-only

    cpu.regs.set_di(buffer_offset);
    cpu.regs.set_ax(0x0401); // Get DPMI capabilities
    memory.write_u8(buffer_linear as usize, 0xCC);

    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
    assert_eq!(memory.read_u8(buffer_linear as usize), 0xCC);
}

#[test]
fn test_linear_alloc_skips_descriptor_tables() {
    let mut dpmi = Dpmi::new();
    dpmi.gdt_base = 0x100000;
    dpmi.gdt_size = 0x1000;
    dpmi.ldt_base = 0x101000;
    dpmi.ldt_size = 0x8000;
    dpmi.idt_base = 0x109000;
    dpmi.idt_size = 0x1000;
    // The real layout continues past the IDT with the TSS, its IOPM and
    // the ring-0 stack. This fixture used to stop at the IDT and assert
    // 0x10A000 — which is exactly the TSS base, so it recorded the bug as
    // the expected answer.
    dpmi.host_control_base = 0x10A000;
    dpmi.host_control_size = 0x3069; // 0x68 header + 0x2000 IOPM + 1 + 0x1000 stack

    let base = dpmi.find_free_linear_range(0x1000, 0x1000).unwrap();
    assert!(
        base >= 0x10A000 + 0x3069,
        "allocation at {base:#x} lands inside the host TSS/IOPM/ring-0 stack span"
    );
}

/// MDD-BUG-CRU-01400: a range nested inside an earlier one must not
/// pull the first-fit cursor back into the earlier range, in any of the
/// three linear-space walkers.
#[test]
fn linear_first_fit_skips_past_a_range_nested_in_an_earlier_one() {
    let mut dpmi = Dpmi::new();
    dpmi.physical_mappings.push(PhysicalMapping {
        physical: 0x0200_0000,
        linear: 0x0020_0000,
        size: 0x0010_0000,
    });
    dpmi.physical_mappings.push(PhysicalMapping {
        physical: 0x0300_0000,
        linear: 0x0021_0000,
        size: 0x1000,
    });
    let size = 0x0020_0000;
    let outer_end = 0x0030_0000;
    for base in [
        dpmi.find_free_linear_range(size, 0x1000),
        dpmi.find_free_linear_range_for_1_0(size, 0x1000),
        dpmi.find_free_linear_range_for_resize(size, 0x1000, u32::MAX),
    ] {
        let base = base.expect("RAM above the mappings is free");
        assert!(base >= outer_end, "{base:#x} lies inside the outer mapping");
    }
}

/// **The host's TSS, IOPM and ring-0 stack are not client memory.**
///
/// `enter_protected_mode` lays them immediately after the IDT, and the
/// linear allocator's first-fit search starts just past the IDT — so the
/// very first client allocation was handed the live TSS. The CPU reads the
/// TSS out of guest memory on every privilege transition, so a client that
/// wrote there corrupted the interrupt stack switch; the comment claiming
/// the CPU caches the TSS did not prevent it.
///
/// Driven through the real entry path rather than by setting the fields by
/// hand, because the defect was that nothing recorded this span at all.
#[test]
fn test_linear_alloc_skips_the_host_tss_and_ring0_stack() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // The span is recorded, and it really does sit right after the IDT.
    assert_ne!(
        dpmi.host_control_base, 0,
        "entering protected mode must record the host control span"
    );
    assert_ne!(dpmi.host_control_size, 0);
    assert_eq!(
        dpmi.host_control_base,
        dpmi.idt_base + dpmi.idt_size,
        "the TSS is laid immediately after the IDT"
    );
    let control_end = dpmi.host_control_base + dpmi.host_control_size;

    // A first-fit allocation must clear it entirely.
    let base = dpmi
        .find_free_linear_range(0x1000, 0x1000)
        .expect("a page is allocatable");
    assert!(
        base >= control_end || base + 0x1000 <= dpmi.host_control_base,
        "allocation at {base:#x} overlaps the host TSS/ring-0 stack span \
         {:#x}..{control_end:#x}",
        dpmi.host_control_base
    );

    // And the explicit overlap guard agrees, including at both edges.
    assert!(dpmi.overlaps_descriptor_tables(dpmi.host_control_base, control_end));
    assert!(dpmi.overlaps_descriptor_tables(control_end - 1, control_end + 0x1000));
    assert!(!dpmi.overlaps_descriptor_tables(control_end, control_end + 0x1000));
}

#[test]
fn test_overlaps_descriptor_tables() {
    let mut dpmi = Dpmi::new();
    dpmi.gdt_base = 0x100000;
    dpmi.gdt_size = 0x1000;

    assert!(dpmi.overlaps_descriptor_tables(0x100000, 0x100800));
    assert!(!dpmi.overlaps_descriptor_tables(0x200000, 0x201000));
    assert!(!dpmi.overlaps_descriptor_tables(0x0F0000, 0x0FFFFF));
}

#[test]
fn test_is_linear_range_free_for_1_0_overlap() {
    let mut dpmi = Dpmi::new();
    dpmi.memory_blocks
        .push(MemoryBlock::new(1, 0x200000, 0x1000));

    assert!(!dpmi.is_linear_range_free_for_1_0(0x200000, 0x1000));
    assert!(dpmi.is_linear_range_free_for_1_0(0x300000, 0x1000));
}

#[test]
fn test_is_linear_range_free_for_resize_overlaps_09() {
    let mut dpmi = Dpmi::new();
    dpmi.memory_blocks
        .push(MemoryBlock::new(1, 0x200000, 0x2000));

    assert!(!dpmi.is_linear_range_free_for_resize(0x200800, 0x1000, 99));
}

#[test]
fn test_is_linear_range_free_for_resize_overlaps_linear_block() {
    let mut dpmi = Dpmi::new();
    dpmi.linear_memory_blocks
        .push(LinearMemoryBlock::new(2, 0x220000, 0x2000, true));

    assert!(!dpmi.is_linear_range_free_for_resize(0x220800, 0x1000, 1));
}

#[test]
fn test_is_linear_range_free_for_resize_overlaps_physical_mapping() {
    let mut dpmi = Dpmi::new();
    dpmi.physical_mappings.push(PhysicalMapping {
        physical: 0x0,
        linear: 0x230000,
        size: 0x2000,
    });

    assert!(!dpmi.is_linear_range_free_for_resize(0x230800, 0x1000, 1));
}

#[test]
fn test_is_linear_range_free_for_resize_no_overlap_with_block() {
    let mut dpmi = Dpmi::new();
    dpmi.memory_blocks
        .push(MemoryBlock::new(1, 0x300000, 0x2000));

    assert!(dpmi.is_linear_range_free_for_resize(0x200000, 0x1000, 1));
}

#[test]
fn test_is_linear_range_free_for_resize_base_after_block() {
    let mut dpmi = Dpmi::new();
    dpmi.memory_blocks
        .push(MemoryBlock::new(1, 0x200000, 0x1000));

    assert!(dpmi.is_linear_range_free_for_resize(0x300000, 0x1000, 2));
}

#[test]
fn test_is_linear_range_free_for_resize_handle_nonoverlap() {
    let mut dpmi = Dpmi::new();
    dpmi.linear_memory_blocks
        .push(LinearMemoryBlock::new(0x10, 0x400000, 0x2000, false));

    assert!(dpmi.is_linear_range_free_for_resize(0x500000, 0x1000, 0x10));
}

#[test]
fn test_is_linear_range_free_for_resize_range_below_linear_block() {
    let mut dpmi = Dpmi::new();
    dpmi.linear_memory_blocks
        .push(LinearMemoryBlock::new(0x11, 0x400000, 0x2000, true));

    assert!(dpmi.is_linear_range_free_for_resize(0x300000, 0x1000, 0x10));
}

#[test]
fn test_is_linear_range_free_for_resize_range_after_linear_block() {
    let mut dpmi = Dpmi::new();
    dpmi.linear_memory_blocks
        .push(LinearMemoryBlock::new(0x12, 0x200000, 0x2000, true));

    assert!(dpmi.is_linear_range_free_for_resize(0x300000, 0x1000, 0x10));
}

#[test]
fn test_is_linear_range_free_for_resize_no_overlap_physical_mapping() {
    let mut dpmi = Dpmi::new();
    dpmi.physical_mappings.push(PhysicalMapping {
        physical: 0x0,
        linear: 0x260000,
        size: 0x2000,
    });

    assert!(dpmi.is_linear_range_free_for_resize(0x270000, 0x1000, 1));
}

#[test]
fn test_is_linear_range_free_for_1_0_overlap_linear_block() {
    let mut dpmi = Dpmi::new();
    dpmi.linear_memory_blocks
        .push(LinearMemoryBlock::new(1, 0x220000, 0x2000, true));

    assert!(!dpmi.is_linear_range_free_for_1_0(0x220800, 0x1000));
    assert!(dpmi.is_linear_range_free_for_1_0(0x230000, 0x1000));
}

#[test]
fn test_is_linear_range_free_for_1_0_overlap_physical_mapping() {
    let mut dpmi = Dpmi::new();
    dpmi.physical_mappings.push(PhysicalMapping {
        physical: 0x0,
        linear: 0x240000,
        size: 0x2000,
    });

    assert!(!dpmi.is_linear_range_free_for_1_0(0x240800, 0x1000));
    assert!(dpmi.is_linear_range_free_for_1_0(0x250000, 0x1000));
}

/// Drain the DPMI linear allocation pool and assert each block stays below
/// the DOS RAM ceiling.
#[test]
fn dpmi_linear_allocations_stay_below_dos_ram_ceiling() {
    let mut dpmi = Dpmi::new();
    let mut handle = 1;
    for size in [0x40_0000u32, 0x10_0000] {
        while let Some(base) = dpmi.find_free_linear_range(size, 0x1000) {
            let end = base + size;
            assert!(
                end <= DOS_RAM_CEILING as u32,
                "block {base:#X}..{end:#X} exceeds DOS_RAM_CEILING"
            );
            dpmi.memory_blocks
                .push(MemoryBlock::new(handle, base, size));
            handle += 1;
        }
    }
    let (largest, free) = dpmi.compute_free_memory_above_1mb(DOS_RAM_CEILING as u32);
    assert_eq!((largest, free), (0, 0), "pool not drained");
}

#[test]
fn test_find_free_linear_range_gap_before_block() {
    let mut dpmi = Dpmi::new();
    dpmi.memory_blocks
        .push(MemoryBlock::new(1, 0x120000, 0x1000));

    let base = dpmi.find_free_linear_range(0x1000, 0x1000).unwrap();
    assert_eq!(base, 0x100000);
}

#[test]
fn test_find_free_linear_range_for_1_0_gap_before_block() {
    let mut dpmi = Dpmi::new();
    dpmi.memory_blocks
        .push(MemoryBlock::new(1, 0x120000, 0x1000));

    let base = dpmi.find_free_linear_range_for_1_0(0x1000, 0x1000).unwrap();
    assert_eq!(base, 0x100000);
}

#[test]
fn test_find_free_linear_range_for_1_0_no_gap_at_start() {
    let mut dpmi = Dpmi::new();
    dpmi.memory_blocks
        .push(MemoryBlock::new(1, 0x100000, 0x2000));

    let base = dpmi.find_free_linear_range_for_1_0(0x3000, 0x1000).unwrap();
    assert_eq!(base, 0x102000);
}

#[test]
fn test_find_free_linear_range_for_1_0_too_large() {
    let dpmi = Dpmi::new();
    let base = dpmi.find_free_linear_range_for_1_0(0xFFFF_0000, 0x1000);
    assert!(base.is_none());
}

#[test]
fn test_find_free_linear_range_for_resize_excludes_handle() {
    let mut dpmi = Dpmi::new();
    dpmi.linear_memory_blocks
        .push(LinearMemoryBlock::new(1, 0x200000, 0x2000, true));
    dpmi.linear_memory_blocks
        .push(LinearMemoryBlock::new(2, 0x240000, 0x2000, true));

    let base = dpmi.find_free_linear_range_for_resize(0x1000, 0x1000, 1);
    assert_eq!(base, Some(0x100000));
}

#[test]
fn test_find_free_linear_range_for_resize_requires_gap() {
    let mut dpmi = Dpmi::new();
    dpmi.memory_blocks
        .push(MemoryBlock::new(1, 0x100000, 0x2000));

    let base = dpmi.find_free_linear_range_for_resize(0x3000, 0x1000, 99);
    assert_eq!(base, Some(0x102000));
}

#[test]
fn test_find_free_linear_range_for_resize_too_large() {
    let dpmi = Dpmi::new();
    let base = dpmi.find_free_linear_range_for_resize(0xFFFF_0000, 0x1000, 0);
    assert!(base.is_none());
}

#[test]
fn test_is_linear_range_free_for_resize_excludes_handle() {
    let mut dpmi = Dpmi::new();
    dpmi.linear_memory_blocks
        .push(LinearMemoryBlock::new(0x10, 0x400000, 0x2000, false));
    dpmi.linear_memory_blocks
        .push(LinearMemoryBlock::new(0x11, 0x500000, 0x2000, false));

    assert!(dpmi.is_linear_range_free_for_resize(0x400000, 0x1000, 0x10));
    assert!(!dpmi.is_linear_range_free_for_resize(0x500000, 0x1000, 0x10));
}

#[test]
fn test_read_write_handle_32bit_mode() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    dpmi.cpu_mode = 1;

    // DPMI 0.9 §4.4: 32-bit programs use ESI instead of SI:DI pair
    cpu.regs.esi = 0xABCD_1234;
    assert_eq!(dpmi.read_handle(&cpu), 0xABCD_1234);

    dpmi.write_handle(&mut cpu, 0x1357_2468);
    assert_eq!(cpu.regs.esi, 0x1357_2468);
}

#[test]
fn test_linear_alloc_specific_rejects_descriptor_tables() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1;
    dpmi.gdt_base = 0x100000;
    dpmi.gdt_size = 0x1000;
    dpmi.ldt_base = 0x101000;
    dpmi.ldt_size = 0x8000;
    dpmi.idt_base = 0x109000;
    dpmi.idt_size = 0x1000;

    cpu.regs.set_ax(0x0504); // DPMI 1.0 allocate linear memory
    cpu.regs.ebx = 0x100000;
    cpu.regs.ecx = 0x1000;
    cpu.regs.edx = 0;

    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_LINEAR_UNAVAILABLE);
}

#[test]
fn test_int31_get_version_unsupported_subfunc() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x04FF); // Unsupported subfunction
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int31_page_locking() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Test lock linear region (0x0600) - no-op, always succeeds
    cpu.regs.set_ax(0x0600);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Test unlock linear region (0x0601) - no-op, always succeeds
    cpu.regs.set_ax(0x0601);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Test mark real mode region as pageable (0x0602) - no-op, always succeeds
    cpu.regs.set_ax(0x0602);
    cpu.regs.set_bx(0x0010); // BX:CX = linear address
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_si(0x0000); // SI:DI = size
    cpu.regs.set_di(0x1000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Test relock real mode region (0x0603) - no-op, always succeeds
    cpu.regs.set_ax(0x0603);
    cpu.regs.set_bx(0x0010);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_si(0x0000);
    cpu.regs.set_di(0x1000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Test get page size (0x0604)
    cpu.regs.set_ax(0x0604);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.bx(), 0);
    assert_eq!(cpu.regs.cx(), 0x1000); // 4096 bytes
}

#[test]
fn test_int31_page_locking_invalid_linear_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let linear = memory.ram_size() as u32;
    let size = 0x1000u32;

    for subfunc in [0x0600u16, 0x0601u16, 0x0602u16, 0x0603u16] {
        cpu.regs.set_ax(subfunc);
        cpu.regs.set_bx((linear >> 16) as u16);
        cpu.regs.set_cx(linear as u16);
        cpu.regs.set_si((size >> 16) as u16);
        cpu.regs.set_di(size as u16);

        dpmi.int31(&mut cpu, &mut memory).unwrap();

        assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_LINEAR);
    }
}

#[test]
fn test_int31_demand_paging() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Region-based functions (0x0700/0x0701) are no-ops in emulator but should succeed.
    cpu.regs.set_ax(0x0700);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    cpu.regs.set_ax(0x0701);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Test mark page as demand paging candidate (0x0702) - no-op, always succeeds
    cpu.regs.set_ax(0x0702);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Test discard page contents (0x0703) - no-op, always succeeds
    cpu.regs.set_ax(0x0703);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Test unsupported subfunction (0x0704) - should fail
    cpu.regs.set_ax(0x0704);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_UNSUPPORTED);
}
