use super::*;

#[test]
fn test_find_free_linear_range_skips_physical_mappings() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Add a physical mapping in the linear address space
    dpmi.physical_mappings.push(PhysicalMapping {
        physical: 0xA000_0000,
        linear: 0x0020_0000,
        size: 0x0010_0000, // 1MB mapping at 2MB linear
    });

    // Allocate a memory block — find_free_linear_range should skip the physical mapping
    cpu.regs.set_ax(0x0501);
    cpu.regs.set_bx(0x0001); // BX:CX = 0x0001_0000 = 64KB
    cpu.regs.set_cx(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // The allocated block should not overlap with the physical mapping
    let base = ((cpu.regs.bx() as u32) << 16) | (cpu.regs.cx() as u32);
    assert!(
        base >= 0x0030_0000 || base + 0x0001_0000 <= 0x0020_0000,
        "Allocated block at {:08X} should not overlap physical mapping at 0x200000-0x300000",
        base
    );
}

#[test]
fn test_get_pm_interrupt_vector_0204_with_idtr_trace_coverage() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    assert_ne!(cpu.regs.idtr.base, 0);

    // Exercise the tracing path for multiple vectors
    for vector in [0x00u8, 0x10, 0x21, 0xFF] {
        cpu.regs.set_ax(0x0204);
        cpu.regs.set_bl(vector);
        dpmi.int31(&mut cpu, &mut memory).unwrap();
        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    }
}

#[test]
fn test_int2f_dpmi_detect_normal_with_tracing() {
    let _lock = env_lock();
    let _tracing = init_tracing();
    let dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    cpu.regs.set_ax(0x1687);

    dpmi.int2f_dpmi_detect(&mut cpu);

    assert_eq!(cpu.regs.ax(), 0); // DPMI available
    assert_eq!(cpu.regs.dx(), 0x005A); // Version 0.90
}

#[test]
fn test_is_linear_range_free_for_resize_with_physical_mapping() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    dpmi.physical_mappings.push(PhysicalMapping {
        physical: 0xB000_0000,
        linear: 0x0040_0000,
        size: 0x0010_0000,
    });

    // Check a range that overlaps with the physical mapping
    let overlaps = dpmi.is_linear_range_free_for_resize(0x0045_0000, 0x0010_0000, 0xDEAD);
    assert!(!overlaps);

    // Check a range that doesn't overlap
    let free = dpmi.is_linear_range_free_for_resize(0x0060_0000, 0x0001_0000, 0xDEAD);
    assert!(free);
}

#[test]
fn test_raw_mode_switch_to_pm_error_rollback() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let old_ip = cpu.regs.ip();

    // Set up an invalid SS selector to trigger failure during raw mode switch.
    // The CS must be valid but SS descriptor must fail validation.
    let cs_sel = cpu.regs.cs; // Valid code selector from setup

    // Use a GDT selector for SS that has no valid descriptor
    let bad_ss: u16 = (200 << 3) | 0x03; // Beyond GDT limit

    cpu.regs.set_ax(cpu.regs.ds); // new DS
    cpu.regs.set_cx(cpu.regs.es); // new ES
    cpu.regs.set_dx(bad_ss); // new SS — will fail
    cpu.regs.ebx = 0x1000; // new ESP
    cpu.regs.set_si(cs_sel); // new CS
    cpu.regs.edi = 0x100; // new EIP

    let result = dpmi.handle_raw_switch_to_pm(&mut cpu, &mut memory);
    assert!(result.is_err());

    // Verify rollback — IP should be restored
    assert_eq!(cpu.regs.ip(), old_ip);
}

#[test]
fn test_raw_mode_switch_to_pm_invalid_cs() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Use a GDT selector that has no descriptor
    let bad_cs: u16 = (200 << 3) | 0x03;

    cpu.regs.set_ax(cpu.regs.ds);
    cpu.regs.set_cx(cpu.regs.es);
    cpu.regs.set_dx(cpu.regs.ss);
    cpu.regs.ebx = 0x1000;
    cpu.regs.set_si(bad_cs); // invalid CS
    cpu.regs.edi = 0x100;

    let result = dpmi.handle_raw_switch_to_pm(&mut cpu, &mut memory);
    assert!(result.is_err());
}

#[test]
fn test_segment_to_descriptor_allocation_fails() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Exhaust all selectors
    dpmi.free_selectors.clear();
    dpmi.next_selector = 0xFFF8; // Beyond LDT range

    cpu.regs.set_ax(0x0002);
    cpu.regs.set_bx(0x5678);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_DESC_UNAVAILABLE);
}

#[test]
fn test_segment_to_descriptor_stale_mapping_cleanup() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let segment = 0x1234u16;

    // First, do a normal segment-to-descriptor conversion to create a mapping
    cpu.regs.set_ax(0x0002);
    cpu.regs.set_bx(segment);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let first_selector = cpu.regs.ax();
    assert_ne!(first_selector, 0);

    // Verify the mapping exists
    assert_eq!(dpmi.segment_selectors[segment as usize], first_selector);

    // Now free the selector so it's no longer in self.selectors
    cpu.regs.set_ax(0x0001);
    cpu.regs.set_bx(first_selector);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // The mapping still exists in segment_selectors, but the selector is freed
    // Manually re-insert stale mapping to simulate the condition
    dpmi.segment_selectors[segment as usize] = first_selector;

    // Now call segment-to-descriptor again — the stale mapping should be cleaned up
    // because first_selector is no longer in self.selectors
    cpu.regs.set_ax(0x0002);
    cpu.regs.set_bx(segment);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let second_selector = cpu.regs.ax();
    // Should get a new selector since the old one was cleaned up
    assert_ne!(second_selector, 0);
}

#[test]
fn test_set_extended_exception_handler_0x13_descriptor_not_found() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Use a GDT selector that is beyond GDT limit — vector_target_cache returns None
    // GDT selector index 100, RPL=3 => (100 << 3) | 3 = 0x0323
    let bad_gdt_sel: u16 = (100 << 3) | 0x03;

    cpu.regs.set_ax(0x0213);
    cpu.regs.set_bl(0x00); // Exception 0 (divide error)
    cpu.regs.set_cx(bad_gdt_sel);
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_extended_exception_handler_0x13_invalid_exception_number() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Exception 32 is out of range (only 0-31 are valid)
    cpu.regs.set_ax(0x0213);
    cpu.regs.set_bl(32);
    cpu.regs.set_cx(cpu.regs.cs);
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_extended_exception_handler_0x13_ldt_out_of_range() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Use an LDT selector with an index beyond LDT limit
    let bad_ldt_sel: u16 = (8000u16 << 3) | 0x07; // Way beyond LDT limit

    cpu.regs.set_ax(0x0213);
    cpu.regs.set_bl(0x00);
    cpu.regs.set_cx(bad_ldt_sel);
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_extended_exception_handler_0x13_not_present() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate a selector and mark it not present
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handler_sel = cpu.regs.ax();

    // Set it as code but not present
    if let Some(s) = dpmi
        .selectors
        .iter_mut()
        .find(|s| s.selector == handler_sel)
    {
        s.access = 0x7A; // Code, DPL=3, NOT present
    }
    dpmi.write_selector_descriptor(&mut cpu, &mut memory, handler_sel, 0, 0xFFFF, 0x7A, 0x00);

    cpu.regs.set_ax(0x0213);
    cpu.regs.set_bl(0x00);
    cpu.regs.set_cx(handler_sel);
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_extended_exception_handler_0x13_offset_beyond_limit() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate a code selector with a small limit
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handler_sel = cpu.regs.ax();

    if let Some(s) = dpmi
        .selectors
        .iter_mut()
        .find(|s| s.selector == handler_sel)
    {
        s.access = 0xFA; // Code, DPL=3, present
        s.limit = 0x00FF;
    }
    dpmi.write_selector_descriptor(&mut cpu, &mut memory, handler_sel, 0, 0x00FF, 0xFA, 0x00);

    cpu.regs.set_ax(0x0213);
    cpu.regs.set_bl(0x00);
    cpu.regs.set_cx(handler_sel);
    cpu.regs.edx = 0x0200; // Beyond 0xFF limit
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_LINEAR);
}

#[test]
fn test_set_extended_exception_handler_0x13_valid_with_tracing() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate a code selector
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handler_sel = cpu.regs.ax();

    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(handler_sel);
    cpu.regs.set_cx(0x40FA); // 32-bit code, DPL=3, present
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Set exception handler for exception 0 (divide error)
    cpu.regs.set_ax(0x0213);
    cpu.regs.set_bl(0x00);
    cpu.regs.set_cx(handler_sel);
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_set_pm_interrupt_vector_0205_descriptor_not_found() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Use a GDT selector beyond GDT limit
    let bad_gdt_sel: u16 = (100 << 3) | 0x03;

    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x21);
    cpu.regs.set_cx(bad_gdt_sel);
    cpu.regs.set_dx(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_pm_interrupt_vector_0205_not_present_handler() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate a selector and mark it not present
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handler_sel = cpu.regs.ax();

    // Set the descriptor as code but not present (access byte without P bit)
    if let Some(s) = dpmi
        .selectors
        .iter_mut()
        .find(|s| s.selector == handler_sel)
    {
        s.access = 0x7A; // Code, DPL=3, NOT present (P=0)
    }
    dpmi.write_selector_descriptor(&mut cpu, &mut memory, handler_sel, 0, 0xFFFF, 0x7A, 0x00);

    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x21);
    cpu.regs.set_cx(handler_sel);
    cpu.regs.set_dx(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_pm_interrupt_vector_0205_valid_handler_tracing() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate and set up a code selector
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handler_sel = cpu.regs.ax();

    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(handler_sel);
    cpu.regs.set_cx(0x40FA); // 32-bit code, DPL=3, present
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x21);
    cpu.regs.set_cx(handler_sel);
    cpu.regs.edx = 0x0000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_state_restore_invalid_cs_succeeds_nop() {
    // Restore is a NOP for CPU registers — it no longer loads CS from
    // the buffer, so even an invalid CS in the buffer won't cause a fault.
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Set up a flat data selector for ES so we can address the buffer
    let ldt_base = cpu.regs.ldt_base as usize;
    let selector_index = 4usize;
    let selector = ((selector_index as u16) << 3) | 0x7;
    dpmi.write_descriptor_raw(&mut memory, ldt_base, selector_index, 0, 0xFFFFF, 0xF2, 0xC);
    dpmi.selectors.push(Selector {
        selector,
        base: 0,
        limit: 0xFFFF_FFFF,
        access: 0xF2,
        flags: 0xC0,
        lock_count: 0,
    });
    cpu.load_data_segment(&memory, gp64_cpu::DataSegReg::ES, selector);

    let buffer_offset = 0x200000u32;
    cpu.regs.set_al(1); // Restore operation
    cpu.regs.edi = buffer_offset;

    // Write garbage to the buffer — doesn't matter since restore is NOP
    let invalid_cs: u16 = (200 << 3) | 0x03;
    memory.write_u16(buffer_offset as usize + STATE_OFF_CS as usize, invalid_cs);

    let saved_cs = cpu.regs.cs;
    let result = dpmi.handle_state_save_restore(&mut cpu, &mut memory);
    // Restore is NOP — should succeed regardless of buffer contents
    assert!(result.is_ok());
    // CS must be unchanged
    assert_eq!(cpu.regs.cs, saved_cs);
}

#[test]
fn test_state_save_restore_roundtrip_with_tracing() {
    // With the NOP restore, registers are NOT changed by save/restore.
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    install_flat_es_selector(&mut dpmi, &mut cpu, &mut memory);

    let buffer_offset = 0x200000u32;

    // Save
    cpu.regs.set_al(0);
    cpu.regs.edi = buffer_offset;
    dpmi.handle_state_save_restore(&mut cpu, &mut memory)
        .unwrap();

    // Set known values
    cpu.regs.ebx = 0x1111;
    cpu.regs.ecx = 0x2222;

    // Restore — NOP for CPU registers
    cpu.regs.set_al(1);
    cpu.regs.edi = buffer_offset;
    dpmi.handle_state_save_restore(&mut cpu, &mut memory)
        .unwrap();

    // Registers must be unchanged
    assert_eq!(cpu.regs.ebx, 0x1111);
    assert_eq!(cpu.regs.ecx, 0x2222);
}

#[test]
fn test_dos_memory_block_index_selector_count_zero() {
    // Branch 1: block.selector_count == 0 → continue
    let mut dpmi = Dpmi::new();

    // First block has selector_count == 0 (should be skipped)
    dpmi.dos_memory_blocks.push(DosMemoryBlock {
        segment: 0x1000,
        selector: 0x0107,
        paragraphs: 0x100,
        selector_count: 0,
    });
    // Second block is valid and should be found
    dpmi.dos_memory_blocks.push(DosMemoryBlock {
        segment: 0x2000,
        selector: 0x010F,
        paragraphs: 0x100,
        selector_count: 1,
    });

    // Search for 0x0107 — first block has it but selector_count==0, so skip it
    assert_eq!(dpmi.dos_memory_block_index_for_selector(0x0107), None);
    // Search for 0x010F — second block matches
    assert_eq!(dpmi.dos_memory_block_index_for_selector(0x010F), Some(1));
}

#[test]
fn test_dos_memory_block_index_selector_less_than_block() {
    // Branch 2: selector < block.selector → continue
    let mut dpmi = Dpmi::new();

    dpmi.dos_memory_blocks.push(DosMemoryBlock {
        segment: 0x2000,
        selector: 0x0200,
        paragraphs: 0x100,
        selector_count: 1,
    });

    // Search with selector=0x0100, which is less than block.selector=0x0200
    assert_eq!(dpmi.dos_memory_block_index_for_selector(0x0100), None);
}

#[test]
fn test_dos_memory_block_index_delta_not_multiple_of_8() {
    // Branch 3: !delta.is_multiple_of(8) → continue
    let mut dpmi = Dpmi::new();

    dpmi.dos_memory_blocks.push(DosMemoryBlock {
        segment: 0x2000,
        selector: 0x0200,
        paragraphs: 0x100,
        selector_count: 4,
    });

    // Search with selector=0x0203 — delta=3, not a multiple of 8
    assert_eq!(dpmi.dos_memory_block_index_for_selector(0x0203), None);
}

#[test]
fn test_update_dos_memory_descriptors_remaining_zero_early_break() {
    // Branch 4: remaining == 0 → break (selector_count exceeds needed descriptors)
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let base_selector = dpmi.next_selector;
    // Pre-allocate two selectors so write_selector_descriptor has something to write to
    for i in 0..2 {
        let sel = base_selector.wrapping_add(i * 8);
        dpmi.selectors.push(Selector {
            selector: sel,
            base: 0,
            limit: 0,
            access: 0xF2,
            flags: 0,
            lock_count: 0,
        });
    }

    // paragraphs=0x1000 needs exactly 1 descriptor in 16-bit mode (0x1000 / 0x1000 = 1)
    // but we pass selector_count=2, so the second iteration hits remaining==0 and breaks
    dpmi.update_dos_memory_descriptors(
        &mut cpu,
        &mut memory,
        0x2000, // segment
        0x1000, // paragraphs: exactly 64KB = 1 full descriptor
        base_selector,
        2, // selector_count: more than needed
    );

    // First descriptor should be updated (limit = 0x1000*16 - 1 = 0xFFFF)
    let first = dpmi
        .selectors
        .iter()
        .find(|s| s.selector == base_selector)
        .unwrap();
    assert_eq!(first.limit, 0xFFFF);

    // Second descriptor should NOT be updated (still base=0, limit=0)
    let second_sel = base_selector.wrapping_add(8);
    let second = dpmi
        .selectors
        .iter()
        .find(|s| s.selector == second_sel)
        .unwrap();
    assert_eq!(second.base, 0);
    assert_eq!(second.limit, 0);
}

#[test]
fn test_can_extend_dos_memory_selectors_out_of_ldt_range() {
    // Branch 5: selector NOT in LDT range → return false
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // After setup_protected_mode: LDT indices 0-3 are used (selectors 0x07..0x1F),
    // next_selector = 0x27, ldt_limit = 0x7FFF (4096 entries).
    // Set LDT limit to allow exactly 6 entries (indices 0-5), limit = 6*8 - 1 = 47.
    cpu.regs.ldt_limit = 47;

    let base_selector = dpmi.next_selector;

    // Extend from 0 to 1: needs index 4, selector 0x27 → index=4, 4+1=5 <= 6 → in range
    assert!(dpmi.can_extend_dos_memory_selectors(&cpu, base_selector, 0, 1));

    // Extend from 0 to 3: needs indices 4,5,6. index 6: selector=0x27+16=0x37 → index=6
    // 6+1=7 > 6 → NOT in range → returns false
    assert!(!dpmi.can_extend_dos_memory_selectors(&cpu, base_selector, 0, 3));
}

#[test]
fn test_can_extend_dos_memory_selectors_rejects_full_selector_table() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    cpu.regs.ldt_limit = 0xFFFF;
    while !dpmi.selectors.is_full() {
        dpmi.selectors.push(Selector {
            selector: 0x2007,
            base: 0,
            limit: 0xFFFF,
            access: 0xF2,
            flags: 0,
            lock_count: 0,
        });
    }

    assert!(!dpmi.can_extend_dos_memory_selectors(&cpu, 0x1007, 1, 2));
}

#[test]
fn test_adjust_dos_memory_selectors_removes_from_free_list() {
    // Branch 6: selector found in free_selectors → swap_remove
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let base_selector = dpmi.next_selector;
    let new_selector = base_selector.wrapping_add(8);

    // Pre-populate free_selectors with the selector that will be allocated
    dpmi.free_selectors.push(new_selector);
    assert!(dpmi.free_selectors.contains(&new_selector));

    // Extend from 1 to 2 selectors — this should find new_selector in free_selectors and remove it
    dpmi.adjust_dos_memory_selectors(&mut cpu, &mut memory, base_selector, 1, 2);

    // new_selector should have been removed from free_selectors
    assert!(!dpmi.free_selectors.contains(&new_selector));

    // And a Selector entry should have been added
    assert!(dpmi.selectors.iter().any(|s| s.selector == new_selector));
}

#[test]
fn test_adjust_dos_memory_selectors_not_in_free_list() {
    // Verify the false branch of the free_selectors check (selector NOT in free list)
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let base_selector = dpmi.next_selector;

    // free_selectors is empty, so the position lookup returns None
    assert!(dpmi.free_selectors.is_empty());

    dpmi.adjust_dos_memory_selectors(&mut cpu, &mut memory, base_selector, 0, 1);

    // Selector should still be allocated even though it wasn't in free list
    assert!(dpmi.selectors.iter().any(|s| s.selector == base_selector));
}

#[test]
fn test_raw_switch_pm_cs_not_present() {
    // Line 3627: !cs_cache.present branch
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let (cs_sel, ds_sel, ss_sel, es_sel) = setup_raw_switch_ldt(&dpmi, &mut cpu, &mut memory);

    // Overwrite CS descriptor: not present code segment
    // 0x7A = P=0, DPL=3, S=1, type=1010 (code readable)
    let ldt_base = cpu.regs.ldt_base as usize;
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 1, 0, 0xFFFF, 0x7A, 0x0);

    set_raw_switch_regs(&mut cpu, cs_sel, ds_sel, ss_sel, es_sel);

    let result = dpmi.handle_raw_switch_to_pm(&mut cpu, &mut memory);
    match result {
        Err(err) => match err.downcast_ref::<gp64_cpu::CpuError>() {
            Some(gp64_cpu::CpuError::GeneralProtectionFault) => {}
            other => panic!("expected GPF, got {other:?}"),
        },
        Ok(_) => panic!("expected error for not-present CS"),
    }
}

#[test]
fn test_raw_switch_pm_cs_is_data_segment_converts_to_code() {
    // raw switches. The handler converts data→code automatically.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let (cs_sel, ds_sel, ss_sel, es_sel) = setup_raw_switch_ldt(&dpmi, &mut cpu, &mut memory);

    // Overwrite CS descriptor: data segment instead of code
    // 0xF2 = P=1, DPL=3, S=1, type=0010 (data writable) — is_code=false
    let ldt_base = cpu.regs.ldt_base as usize;
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 1, 0, 0xFFFF, 0xF2, 0x0);

    set_raw_switch_regs(&mut cpu, cs_sel, ds_sel, ss_sel, es_sel);

    // Should succeed — handler auto-converts data→code
    dpmi.handle_raw_switch_to_pm(&mut cpu, &mut memory).unwrap();

    // Verify CS is now loaded and PM is active
    assert_eq!(cpu.regs.cs, cs_sel);
    assert!(cpu.is_protected_mode());
    // Verify the descriptor was patched to code type in memory
    let patched_access = memory.read_u8(ldt_base + 8 + 5);
    assert!(
        patched_access & 0x08 != 0,
        "Execute bit should be set after data→code fix"
    );
}

#[test]
fn raw_switch_rejected_for_a_bad_stack_restores_the_borrowed_cs() {
    // SS not present (0x72 = P=0, DPL=3, S=1, data writable) — rejected
    // after the CS repair, and before any rollback snapshot exists.
    assert_rejected_raw_switch_preserves_borrowed_cs(|dpmi, memory, ldt_base| {
        dpmi.write_descriptor_raw(memory, ldt_base, 2, 0, 0xFFFF, 0x72, 0x0);
    });
}

#[test]
fn raw_switch_rejected_for_a_non_writable_stack_restores_the_borrowed_cs() {
    // SS present but read-only (0xF0 = P=1, DPL=3, S=1, data read-only).
    assert_rejected_raw_switch_preserves_borrowed_cs(|dpmi, memory, ldt_base| {
        dpmi.write_descriptor_raw(memory, ldt_base, 2, 0, 0xFFFF, 0xF0, 0x0);
    });
}

#[test]
fn raw_switch_rejected_for_a_bad_data_selector_restores_the_borrowed_cs() {
    // DS not present (0x72) — the last validation gate before the switch.
    assert_rejected_raw_switch_preserves_borrowed_cs(|dpmi, memory, ldt_base| {
        dpmi.write_descriptor_raw(memory, ldt_base, 3, 0, 0xFFFF, 0x72, 0x0);
    });
}

#[test]
fn test_raw_switch_pm_cs_conforming() {
    // Line 3630: cs_cache.is_conforming true branch — new_cpl = cpu.regs.cpl
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let ldt_base = 0x20000usize;
    cpu.regs.ldt_base = ldt_base as u32;
    cpu.regs.ldt_limit = 5 * 8 - 1;

    // LDT[1]: conforming code, DPL=0, readable
    // 0x9E = P=1, DPL=0, S=1, type=1110 (code, conforming, readable)
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 1, 0, 0xFFFF, 0x9E, 0x0);
    // LDT[2]: data, DPL=0, writable
    // 0x92 = P=1, DPL=0, S=1, type=0010 (data writable)
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 2, 0, 0xFFFF, 0x92, 0x0);
    // LDT[3]: data, DPL=0, writable
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 3, 0, 0xFFFF, 0x92, 0x0);
    // LDT[4]: data, DPL=0, writable
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 4, 0, 0xFFFF, 0x92, 0x0);

    // For conforming code, new_cpl = cpu.regs.cpl (not selector RPL).
    // Set CPL=0 so SS DPL=0 matches, and selector RPL=0 too.
    cpu.regs.cpl = 0;

    let cs_sel: u16 = (1 << 3) | 0x04; // LDT index 1, TI=1, RPL=0
    let ds_sel: u16 = (3 << 3) | 0x04; // LDT index 3, TI=1, RPL=0
    let ss_sel: u16 = (2 << 3) | 0x04; // LDT index 2, TI=1, RPL=0
    let es_sel: u16 = (4 << 3) | 0x04; // LDT index 4, TI=1, RPL=0

    set_raw_switch_regs(&mut cpu, cs_sel, ds_sel, ss_sel, es_sel);

    let result = dpmi.handle_raw_switch_to_pm(&mut cpu, &mut memory);
    assert!(result.is_ok(), "conforming CS should succeed: {result:?}");

    // Verify the switch happened
    assert!(cpu.is_protected_mode());
    assert_eq!(cpu.regs.cpl, 0); // CPL stays at 0 for conforming
}

#[test]
fn test_raw_switch_pm_ss_not_present() {
    // Line 3638: !ss_cache.present branch
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let (cs_sel, ds_sel, ss_sel, es_sel) = setup_raw_switch_ldt(&dpmi, &mut cpu, &mut memory);

    // Overwrite SS descriptor: not present data
    // 0x72 = P=0, DPL=3, S=1, type=0010 (data writable but not present)
    let ldt_base = cpu.regs.ldt_base as usize;
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 2, 0, 0xFFFF, 0x72, 0x0);

    set_raw_switch_regs(&mut cpu, cs_sel, ds_sel, ss_sel, es_sel);

    let result = dpmi.handle_raw_switch_to_pm(&mut cpu, &mut memory);
    match result {
        Err(err) => match err.downcast_ref::<gp64_cpu::CpuError>() {
            Some(gp64_cpu::CpuError::StackFault) => {}
            other => panic!("expected StackFault, got {other:?}"),
        },
        Ok(_) => panic!("expected error for not-present SS"),
    }
}

#[test]
fn test_raw_switch_pm_ss_is_code() {
    // Line 3638: ss_cache.is_code branch
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let (cs_sel, ds_sel, ss_sel, es_sel) = setup_raw_switch_ldt(&dpmi, &mut cpu, &mut memory);

    // Overwrite SS descriptor: code segment
    // 0xFA = P=1, DPL=3, S=1, type=1010 (code readable)
    let ldt_base = cpu.regs.ldt_base as usize;
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 2, 0, 0xFFFF, 0xFA, 0x0);

    set_raw_switch_regs(&mut cpu, cs_sel, ds_sel, ss_sel, es_sel);

    let result = dpmi.handle_raw_switch_to_pm(&mut cpu, &mut memory);
    match result {
        Err(err) => match err.downcast_ref::<gp64_cpu::CpuError>() {
            Some(gp64_cpu::CpuError::StackFault) => {}
            other => panic!("expected StackFault, got {other:?}"),
        },
        Ok(_) => panic!("expected error for code-segment SS"),
    }
}

#[test]
fn test_raw_switch_pm_ss_not_writable() {
    // Line 3638: !ss_cache.is_writable branch
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let (cs_sel, ds_sel, ss_sel, es_sel) = setup_raw_switch_ldt(&dpmi, &mut cpu, &mut memory);

    // Overwrite SS descriptor: read-only data segment
    // 0xF0 = P=1, DPL=3, S=1, type=0000 (data, read-only)
    let ldt_base = cpu.regs.ldt_base as usize;
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 2, 0, 0xFFFF, 0xF0, 0x0);

    set_raw_switch_regs(&mut cpu, cs_sel, ds_sel, ss_sel, es_sel);

    let result = dpmi.handle_raw_switch_to_pm(&mut cpu, &mut memory);
    match result {
        Err(err) => match err.downcast_ref::<gp64_cpu::CpuError>() {
            Some(gp64_cpu::CpuError::StackFault) => {}
            other => panic!("expected StackFault, got {other:?}"),
        },
        Ok(_) => panic!("expected error for read-only SS"),
    }
}

#[test]
fn test_raw_switch_pm_ss_dpl_mismatch() {
    // Line 3641: ss_cache.dpl != new_cpl branch
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let ldt_base = 0x20000usize;
    cpu.regs.ldt_base = ldt_base as u32;
    cpu.regs.ldt_limit = 5 * 8 - 1;

    // CS: DPL=3, code readable — non-conforming, so new_cpl = RPL = 3
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 1, 0, 0xFFFF, 0xFA, 0x0);
    // SS: DPL=0 writable data — DPL(0) != new_cpl(3)
    // 0x92 = P=1, DPL=0, S=1, type=0010 (data writable)
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 2, 0, 0xFFFF, 0x92, 0x0);
    // DS/ES: DPL=3 data (valid)
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 3, 0, 0xFFFF, 0xF2, 0x0);
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 4, 0, 0xFFFF, 0xF2, 0x0);

    let cs_sel: u16 = (1 << 3) | 0x04 | 0x03; // RPL=3
    let ds_sel: u16 = (3 << 3) | 0x04 | 0x03;
    let ss_sel: u16 = (2 << 3) | 0x04 | 0x03; // RPL=3, but DPL=0
    let es_sel: u16 = (4 << 3) | 0x04 | 0x03;

    set_raw_switch_regs(&mut cpu, cs_sel, ds_sel, ss_sel, es_sel);

    let result = dpmi.handle_raw_switch_to_pm(&mut cpu, &mut memory);
    match result {
        Err(err) => match err.downcast_ref::<gp64_cpu::CpuError>() {
            Some(gp64_cpu::CpuError::StackFault) => {}
            other => panic!("expected StackFault, got {other:?}"),
        },
        Ok(_) => panic!("expected error for SS DPL mismatch"),
    }
}

#[test]
fn test_raw_switch_pm_ss_rpl_mismatch() {
    // Line 3641: ((new_ss & 3) as u8) != new_cpl branch
    // Need: dpl == new_cpl but RPL != new_cpl
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let ldt_base = 0x20000usize;
    cpu.regs.ldt_base = ldt_base as u32;
    cpu.regs.ldt_limit = 5 * 8 - 1;

    // CS: DPL=0, code readable, non-conforming -> new_cpl = RPL of cs_sel
    // Use RPL=0 for CS so new_cpl=0
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 1, 0, 0xFFFF, 0x9A, 0x0);
    // SS: DPL=0 (matches new_cpl=0), data writable
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 2, 0, 0xFFFF, 0x92, 0x0);
    // DS/ES: DPL=0 data
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 3, 0, 0xFFFF, 0x92, 0x0);
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 4, 0, 0xFFFF, 0x92, 0x0);

    let cs_sel: u16 = (1 << 3) | 0x04; // RPL=0, new_cpl=0
    let ds_sel: u16 = (3 << 3) | 0x04; // RPL=0
                                       // SS: RPL=2 but new_cpl=0 -> RPL != new_cpl
    let ss_sel: u16 = (2 << 3) | 0x04 | 0x02;
    let es_sel: u16 = (4 << 3) | 0x04; // RPL=0

    set_raw_switch_regs(&mut cpu, cs_sel, ds_sel, ss_sel, es_sel);

    let result = dpmi.handle_raw_switch_to_pm(&mut cpu, &mut memory);
    match result {
        Err(err) => match err.downcast_ref::<gp64_cpu::CpuError>() {
            Some(gp64_cpu::CpuError::StackFault) => {}
            other => panic!("expected StackFault, got {other:?}"),
        },
        Ok(_) => panic!("expected error for SS RPL mismatch"),
    }
}

#[test]
fn test_raw_switch_pm_null_ds_es_selector() {
    // Line 3645: (selector & 0xFFF8) == 0 — null selector skips validation
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let (cs_sel, _ds_sel, ss_sel, _es_sel) = setup_raw_switch_ldt(&dpmi, &mut cpu, &mut memory);

    // Use null selectors for DS and ES (index=0, RPL=3)
    // (0x0003 & 0xFFF8) == 0 -> null, skip
    let null_sel: u16 = 0x0003;

    set_raw_switch_regs(&mut cpu, cs_sel, null_sel, ss_sel, null_sel);

    let result = dpmi.handle_raw_switch_to_pm(&mut cpu, &mut memory);
    assert!(result.is_ok(), "null DS/ES should be allowed: {result:?}");
    assert!(cpu.is_protected_mode());
}

#[test]
fn test_raw_switch_pm_ldt_zero_ds_selector_is_validated() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let (cs_sel, _ds_sel, ss_sel, es_sel) = setup_raw_switch_ldt(&dpmi, &mut cpu, &mut memory);
    let ldt_zero_selector: u16 = 0x0007;
    set_raw_switch_regs(&mut cpu, cs_sel, ldt_zero_selector, ss_sel, es_sel);

    let result = dpmi.handle_raw_switch_to_pm(&mut cpu, &mut memory);
    match result {
        Err(err) => match err.downcast_ref::<gp64_cpu::CpuError>() {
            Some(gp64_cpu::CpuError::GeneralProtectionFault) => {}
            other => panic!("expected GeneralProtectionFault, got {other:?}"),
        },
        Ok(_) => panic!("LDT[0] DS selector must be validated, not skipped as null"),
    }
}

#[test]
fn test_raw_switch_pm_ds_not_present() {
    // Line 3651: !cache.present for DS/ES
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let (cs_sel, _ds_sel, ss_sel, es_sel) = setup_raw_switch_ldt(&dpmi, &mut cpu, &mut memory);

    // Overwrite LDT[3] (used for DS) with a not-present data descriptor
    // 0x72 = P=0, DPL=3, S=1, type=0010 (data writable, not present)
    let ldt_base = cpu.regs.ldt_base as usize;
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 3, 0, 0xFFFF, 0x72, 0x0);

    let bad_ds: u16 = (3 << 3) | 0x04 | 0x03; // LDT index 3, TI=1, RPL=3 = 0x1F

    set_raw_switch_regs(&mut cpu, cs_sel, bad_ds, ss_sel, es_sel);

    let result = dpmi.handle_raw_switch_to_pm(&mut cpu, &mut memory);
    match result {
        Err(err) => match err.downcast_ref::<gp64_cpu::CpuError>() {
            Some(gp64_cpu::CpuError::GeneralProtectionFault) => {}
            other => panic!("expected GPF, got {other:?}"),
        },
        Ok(_) => panic!("expected error for not-present DS"),
    }
}

#[test]
fn test_raw_switch_pm_es_execute_only_code() {
    // Line 3651: cache.is_code && !cache.is_readable for ES
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let (cs_sel, ds_sel, ss_sel, _es_sel) = setup_raw_switch_ldt(&dpmi, &mut cpu, &mut memory);

    // Overwrite LDT[4] (used for ES) with execute-only code descriptor
    // 0xF8 = P=1, DPL=3, S=1, type=1000 (code, non-conforming, not readable)
    let ldt_base = cpu.regs.ldt_base as usize;
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 4, 0, 0xFFFF, 0xF8, 0x0);

    let bad_es: u16 = (4 << 3) | 0x04 | 0x03; // LDT index 4, TI=1, RPL=3 = 0x27

    set_raw_switch_regs(&mut cpu, cs_sel, ds_sel, ss_sel, bad_es);

    let result = dpmi.handle_raw_switch_to_pm(&mut cpu, &mut memory);
    match result {
        Err(err) => match err.downcast_ref::<gp64_cpu::CpuError>() {
            Some(gp64_cpu::CpuError::GeneralProtectionFault) => {}
            other => panic!("expected GPF, got {other:?}"),
        },
        Ok(_) => panic!("expected error for execute-only ES"),
    }
}

#[test]
fn test_raw_switch_pm_inner_closure_error_rollback() {
    // Line 3683: error in inner closure triggers rollback.
    // The outer validation (lines 3627-3654) passes, but load_cs (line 3668)
    // fails because DPL(0) != CPL(3) for a non-conforming segment.
    // Outer check only verifies present + is_code, NOT DPL matching.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let ldt_base = 0x20000usize;
    cpu.regs.ldt_base = ldt_base as u32;
    cpu.regs.ldt_limit = 5 * 8 - 1;

    // CS: present code readable, DPL=0, non-conforming
    // Outer check passes (present + is_code). But load_cs needs dpl==cpl.
    // With RPL=3 selector, new_cpl = 3, but DPL=0 -> load_cs fails.
    // 0x9A = P=1, DPL=0, S=1, type=1010 (code readable)
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 1, 0, 0xFFFF, 0x9A, 0x0);
    // SS: DPL=3 (matches new_cpl=3), data writable
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 2, 0, 0xFFFF, 0xF2, 0x0);
    // DS/ES: DPL=3 data
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 3, 0, 0xFFFF, 0xF2, 0x0);
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 4, 0, 0xFFFF, 0xF2, 0x0);

    let cs_sel: u16 = (1 << 3) | 0x04 | 0x03; // RPL=3, new_cpl=3
    let ds_sel: u16 = (3 << 3) | 0x04 | 0x03;
    let ss_sel: u16 = (2 << 3) | 0x04 | 0x03;
    let es_sel: u16 = (4 << 3) | 0x04 | 0x03;

    // Save pre-switch state
    let old_cr0 = cpu.regs.cr0;
    let old_cs = cpu.regs.cs;

    set_raw_switch_regs(&mut cpu, cs_sel, ds_sel, ss_sel, es_sel);

    let result = dpmi.handle_raw_switch_to_pm(&mut cpu, &mut memory);
    assert!(result.is_err(), "should fail in inner closure");

    // Verify rollback restored CPU state
    assert_eq!(cpu.regs.cr0, old_cr0, "CR0 should be restored");
    assert_eq!(cpu.regs.cs, old_cs, "CS should be restored");
    assert!(!dpmi.protected_mode, "DPMI PM flag should be restored");
}

#[test]
fn test_set_access_rights_16bit_client_granularity_bit_set() {
    // Line 1558: 16-bit client, existing selector has G bit set (flags & 0x80 != 0)
    // This causes old_limit20 = s.limit >> 12 (true branch).
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    // 16-bit client (default cpu_mode == 0)
    cpu.regs.cpl = 3;

    // Allocate a selector
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel = cpu.regs.ax();

    // Directly set the selector to have G bit in flags and a large limit
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == sel) {
        s.flags = 0x80; // G=1 (granularity bit set)
        s.limit = 0x000F_FFFF_u32 << 12 | 0xFFF; // 4GB-1 with G=1
        s.access = 0xF2; // Present, DPL=3, data r/w
    }

    // Call Set Descriptor Access Rights (0009h) as 16-bit client
    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(sel);
    cpu.regs.set_cl(0xF2); // P=1, DPL=3, S=1, data r/w
    cpu.regs.set_ch(0x00); // 16-bit client: CH should be zero
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Set access rights should succeed"
    );
}

#[test]
fn test_set_multiple_descriptors_g_bit_expansion() {
    // Line 1993: G-bit expansion in Set Multiple Descriptors (func 000Fh)
    // When flags & 0x80 != 0, limit = (limit20 << 12) | 0x0FFF
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

    // Write a 10-byte entry into a buffer: [selector(2)] + [descriptor(8)]
    // The descriptor has G bit set in flags byte
    let buffer_addr = 0x50000usize;
    memory.ensure_ram_size(buffer_addr + 20);

    // selector (2 bytes)
    memory.write_u16(buffer_addr, sel);
    // descriptor: limit_lo=0xFFFF, base=0, access=0xF2 (Present,DPL=3,data,writable)
    // flags_limit_hi: G=1(0x80) | limit_hi=0x0F = 0x8F
    memory.write_u16(buffer_addr + 2, 0xFFFF); // limit lo
    memory.write_u16(buffer_addr + 4, 0x0000); // base lo
    memory.write_u8(buffer_addr + 6, 0x00); // base mid
    memory.write_u8(buffer_addr + 7, 0xF2); // access: Present, DPL=3, S=1, data, writable
    memory.write_u8(buffer_addr + 8, 0x8F); // flags(G=1) | limit_hi(0x0F)
    memory.write_u8(buffer_addr + 9, 0x00); // base hi

    // Make ES flat (base=0, large limit) for buffer access
    let es_sel = cpu.regs.es;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = 0;
        s.limit = 0xFFFF_FFFF;
    }

    cpu.regs.set_ax(0x000F); // Set Multiple Descriptors
    cpu.regs.set_cx(1); // 1 entry
    cpu.regs.edi = buffer_addr as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Set Multiple Descriptors should succeed"
    );
    // Verify the limit was expanded with G-bit
    let limit = dpmi.get_descriptor_limit(sel).unwrap();
    assert_eq!(limit, (0x000F_FFFF_u32 << 12) | 0x0FFF);
}

#[test]
fn test_offset_within_segment_expand_down() {
    // Lines 3954-3957: offset_within_segment with expand-down data segment
    // To trigger this, use Set Exception Handler (0x0203) with a selector
    // that has expand-down access byte.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate a selector for the expand-down handler
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handler_sel = cpu.regs.ax();

    // Set it as expand-down data segment:
    // access = 0xF6: Present(0x80), DPL=3(0x60), S=1(0x10), Data(0x00), Expand-Down(0x04), Writable(0x02)
    if let Some(s) = dpmi
        .selectors
        .iter_mut()
        .find(|s| s.selector == handler_sel)
    {
        s.access = 0xF6; // Present, DPL=3, data, expand-down, writable
        s.flags = 0x00; // No G bit, no D bit
        s.limit = 0x0100; // Expand-down: valid offsets are > 0x0100 and <= 0xFFFF
        s.base = 0;
    }

    // An expand-down DATA segment is not a valid exception handler (handlers
    // must be a code segment — MDD-BUG-FLUXHOMEARPA-00011), so this test now
    // exercises offset_within_segment directly, which is its actual intent.
    let cache = dpmi
        .vector_target_cache(&cpu, &memory, handler_sel)
        .expect("expand-down descriptor cache");
    // Expand-down: offset (0x200) > effective_limit (0x100) && <= max (0xFFFF).
    assert!(
        Dpmi::offset_within_segment(&cache, 0x0200),
        "offset 0x200 must be within the expand-down segment"
    );
    assert!(
        !Dpmi::offset_within_segment(&cache, 0x0080),
        "offset 0x080 (<= limit) must be outside the expand-down segment"
    );
}

#[test]
fn test_offset_within_segment_expand_down_out_of_range() {
    // Lines 3956-3957: expand-down segment where offset is NOT within range
    // offset <= effective_limit should fail
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate a selector
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handler_sel = cpu.regs.ax();

    // Expand-down data segment with limit=0x8000
    if let Some(s) = dpmi
        .selectors
        .iter_mut()
        .find(|s| s.selector == handler_sel)
    {
        s.access = 0xF6; // Present, DPL=3, data, expand-down, writable
        s.flags = 0x00;
        s.limit = 0x8000; // Valid offsets: > 0x8000 and <= 0xFFFF
        s.base = 0;
    }

    // Try offset=0x100, which is <= limit (0x8000) — should fail
    cpu.regs.set_ax(0x0203);
    cpu.regs.set_bl(0x01); // Exception 1
    cpu.regs.set_cx(handler_sel);
    cpu.regs.set_dx(0x0100); // offset <= limit, outside expand-down range
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Should fail: offset not within expand-down segment"
    );
}

#[test]
fn test_selector_in_ldt_range_max_entries_zero() {
    // Line 4012: ldt_entry_count returns 0 when ldt_limit is non-zero but < 8
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Set ldt_limit to a non-zero value less than 8
    // ldt_entry_count: (6 + 1) / 8 = 0
    cpu.regs.ldt_limit = 6;

    // Try to free a descriptor — it uses selector_in_ldt_range internally
    let sel: u16 = (1 << 3) | 0x07; // LDT selector, index 1
    cpu.regs.set_ax(0x0001); // Free descriptor
    cpu.regs.set_bx(sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Should fail: selector not in LDT range (max_entries==0)"
    );
}

#[test]
fn test_env_limit_from_mcb_env_segment_zero() {
    // Line 4072: env_segment == 0 → None
    let memory = Memory::new();
    assert_eq!(Dpmi::env_limit_from_mcb(&memory, 0), None);
}

#[test]
fn test_env_limit_from_mcb_env_base_exceeds_ram() {
    // Line 4076: env_base >= ram_size → None
    let mut memory = Memory::new();
    // Shrink RAM so env_base (segment*16) exceeds ram_size
    memory.resize_ram(0x1000); // 4KB RAM
                               // env_segment=0x0200 → env_base=0x2000, exceeds 0x1000
    assert_eq!(Dpmi::env_limit_from_mcb(&memory, 0x0200), None);
}

#[test]
fn test_env_limit_from_mcb_invalid_mcb_tag() {
    // Line 4085: MCB tag is not 0x4D or 0x5A → None
    // Note: line 4081 (mcb_addr + 4 > ram_size) is unreachable after line 4076 guard
    // because mcb_addr = env_base - 16, so mcb_addr < env_base < ram_size always holds.
    let memory = Memory::new();
    let env_seg = 0x0100_u16; // env_base = 0x1000
                              // Default memory is zeroed, so tag at (0x00FF * 16) = 0x0FF0 is 0x00
    assert_eq!(Dpmi::env_limit_from_mcb(&memory, env_seg), None);
}

#[test]
fn test_env_limit_from_mcb_size_paras_zero() {
    // Line 4089: size_paras == 0 → None
    let mut memory = Memory::new();
    let env_seg = 0x0100_u16; // env_base = 0x1000
    let mcb_seg = env_seg - 1; // 0x00FF
    let mcb_addr = (mcb_seg as usize) * 16; // 0x0FF0

    // Write MCB tag
    memory.write_u8(mcb_addr, 0x4D); // 'M' tag
                                     // Write size_paras = 0 at mcb_addr + 3
    memory.write_u16(mcb_addr + 3, 0);

    assert_eq!(Dpmi::env_limit_from_mcb(&memory, env_seg), None);
}

#[test]
fn test_env_limit_floors_at_64k_for_small_mcb() {
    // Before the fix, env_limit_from_mcb returned 255 (0xFF), which traps
    // 0xFFFF.
    let mut memory = Memory::new();
    let env_seg = 0x0200u16;
    let mcb_seg = env_seg - 1;
    let mcb_addr = (mcb_seg as usize) * 16;
    memory.write_u8(mcb_addr, 0x4D); // 'M' tag
    memory.write_u16(mcb_addr + 1, 0); // owner
    memory.write_u16(mcb_addr + 3, 16); // 16 paragraphs = 256 bytes

    let result = Dpmi::env_limit_from_mcb(&memory, env_seg);
    assert_eq!(result, Some(0xFFFF));
}

#[test]
fn test_env_limit_uses_mcb_size_when_larger_than_64k() {
    // If the MCB block is bigger than 64 KB, the limit covers the full
    // block — the floor must not clamp a legitimately larger block down.
    let mut memory = Memory::new();
    let env_seg = 0x1000u16;
    let mcb_seg = env_seg - 1;
    let mcb_addr = (mcb_seg as usize) * 16;
    memory.write_u8(mcb_addr, 0x4D); // 'M' tag
    memory.write_u16(mcb_addr + 1, 0); // owner
    memory.write_u16(mcb_addr + 3, 0x2000); // 0x2000 paragraphs = 128 KB

    let result = Dpmi::env_limit_from_mcb(&memory, env_seg);
    assert_eq!(result, Some(0x1FFFF)); // block_bytes - 1 = 0x20000 - 1
}

#[test]
fn test_env_limit_floors_at_64k_for_typical_dos_env() {
    // Typical DOS env block: 32 paragraphs = 512 bytes. Pre-fix limit was
    // 511 (0x1FF); post-fix limit is 0xFFFF.
    let mut memory = Memory::new();
    let env_seg = 0x0300u16;
    let mcb_seg = env_seg - 1;
    let mcb_addr = (mcb_seg as usize) * 16;
    memory.write_u8(mcb_addr, 0x4D); // 'M' tag
    memory.write_u16(mcb_addr + 1, 0); // owner
    memory.write_u16(mcb_addr + 3, 32); // 32 paragraphs = 512 bytes

    let result = Dpmi::env_limit_from_mcb(&memory, env_seg);
    assert_eq!(result, Some(0xFFFF));
}

#[test]
fn test_overlaps_descriptor_tables_table_size_zero() {
    // Line 4155: table_base != 0 but table_size == 0 → continue
    let mut dpmi = Dpmi::new();
    dpmi.gdt_base = 0x1000; // Non-zero base
    dpmi.gdt_size = 0; // Zero size — should be skipped
    dpmi.ldt_base = 0;
    dpmi.ldt_size = 0;
    dpmi.idt_base = 0;
    dpmi.idt_size = 0;

    // Should not overlap anything since all tables have size 0 or base 0
    assert!(!dpmi.overlaps_descriptor_tables(0x1000, 0x2000));
}

#[test]
fn test_allocate_mem_handle_wraparound() {
    // Line 4232: handle wraps from u32::MAX to 0, then to 1
    let mut dpmi = Dpmi::new();

    // Add a block with handle=u32::MAX so it's "in use"
    dpmi.memory_blocks
        .push(MemoryBlock::new(u32::MAX, 0x100000, 0x1000));
    dpmi.next_mem_handle = u32::MAX;

    // allocate_mem_handle should:
    // 1. start = u32::MAX
    // 2. Try handle=u32::MAX, it's in use
    // 3. handle = handle.wrapping_add(1) = 0
    // 4. if handle == 0 { handle = 1 } ← line 4232 covered!
    // 5. handle(1) != start(u32::MAX), loop continues
    // 6. handle=1 is not in use → found!
    let handle = dpmi.allocate_mem_handle();
    assert_eq!(handle, Some(1));
}

#[test]
fn test_pm_interrupt_handler_not_present() {
    // Line 2952: entry.present == false → None
    let mut dpmi = Dpmi::new();
    // Dpmi::new() initializes all pm_idt entries as present.
    // Clear the present bit on a specific entry to cover the false branch.
    dpmi.pm_idt[0x10].present = false;
    dpmi.pm_idt[0xFF].present = false;
    assert!(
        dpmi.pm_interrupt_handler(0x10).is_none(),
        "Vector 0x10 should not be present after clearing"
    );
    assert!(
        dpmi.pm_interrupt_handler(0xFF).is_none(),
        "Vector 0xFF should not be present after clearing"
    );
    // Verify a non-cleared entry is still present
    assert!(
        dpmi.pm_interrupt_handler(0x21).is_some(),
        "Vector 0x21 should still be present"
    );
}

#[test]
fn test_set_descriptor_000c_g_bit_expansion() {
    // Line 1731: in Set Descriptor (func 000Ch), when flags & 0x80 != 0,
    // limit = (limit20 << 12) | 0x0FFF
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

    // Make ES flat (base=0, large limit) for buffer access
    let es_sel = cpu.regs.es;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = 0;
        s.limit = 0xFFFF_FFFF;
    }

    let buffer_addr = 0x50000usize;
    memory.ensure_ram_size(buffer_addr + 16);

    // 8-byte descriptor: limit_lo, base_lo, base_mid, access, limit_flags, base_hi
    memory.write_u16(buffer_addr, 0xFFFF); // limit_lo
    memory.write_u16(buffer_addr + 2, 0x0000); // base_lo
    memory.write_u8(buffer_addr + 4, 0x00); // base_mid
    memory.write_u8(buffer_addr + 5, 0xF2); // access: Present, DPL=3, S=1, data, writable
    memory.write_u8(buffer_addr + 6, 0x8F); // G=1(0x80) | limit_hi=0x0F
    memory.write_u8(buffer_addr + 7, 0x00); // base_hi

    cpu.regs.set_ax(0x000C); // Set Descriptor
    cpu.regs.set_bx(sel);
    cpu.regs.edi = buffer_addr as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Set Descriptor should succeed"
    );
    // Verify limit was expanded: (0xFFFFF << 12) | 0xFFF = 0xFFFF_FFFF
    let limit = dpmi.get_descriptor_limit(sel).unwrap();
    assert_eq!(limit, 0xFFFF_FFFF);
}

#[test]
fn test_set_descriptor_000c_no_g_bit() {
    // Line 1731 false branch: when flags & 0x80 == 0, limit = limit20 directly
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

    let es_sel = cpu.regs.es;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = 0;
        s.limit = 0xFFFF_FFFF;
    }

    let buffer_addr = 0x50000usize;
    memory.ensure_ram_size(buffer_addr + 16);

    // No G bit: flags = 0x40 (D=1 only), limit = 0x1FFFF (20-bit limit, no expansion)
    memory.write_u16(buffer_addr, 0xFFFF); // limit_lo
    memory.write_u16(buffer_addr + 2, 0x0000); // base_lo
    memory.write_u8(buffer_addr + 4, 0x00); // base_mid
    memory.write_u8(buffer_addr + 5, 0xF2); // access
    memory.write_u8(buffer_addr + 6, 0x41); // D=1(0x40) | limit_hi=0x01
    memory.write_u8(buffer_addr + 7, 0x00); // base_hi

    cpu.regs.set_ax(0x000C);
    cpu.regs.set_bx(sel);
    cpu.regs.edi = buffer_addr as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let limit = dpmi.get_descriptor_limit(sel).unwrap();
    assert_eq!(limit, 0x1FFFF); // raw 20-bit limit, no expansion
}

#[test]
fn test_offset_within_segment_expand_down_big_segment() {
    // Lines 3954-3957: expand-down with big=true, max=0xFFFF_FFFF
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate a selector
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handler_sel = cpu.regs.ax();

    // Set as expand-down data segment with D/B=1 (big)
    // access=0xF6: Present(0x80), DPL=3(0x60), S=1(0x10), Expand-Down(0x04), Writable(0x02)
    // flags=0x40: D/B=1 (32-bit segment, max=0xFFFF_FFFF)
    if let Some(s) = dpmi
        .selectors
        .iter_mut()
        .find(|s| s.selector == handler_sel)
    {
        s.access = 0xF6;
        s.flags = 0x40; // D/B=1
        s.limit = 0x1000; // Expand-down: valid offsets > 0x1000 and <= 0xFFFF_FFFF
        s.base = 0;
    }

    // Handlers must be code (MDD-BUG-FLUXHOMEARPA-00011); test offset_within_segment
    // directly for the expand-down big (max 0xFFFF_FFFF) case.
    let cache = dpmi
        .vector_target_cache(&cpu, &memory, handler_sel)
        .expect("expand-down big descriptor cache");
    // offset 0x2000 > limit 0x1000 && <= max 0xFFFF_FFFF.
    assert!(
        Dpmi::offset_within_segment(&cache, 0x2000),
        "offset 0x2000 must be within the expand-down big segment"
    );
    assert!(
        !Dpmi::offset_within_segment(&cache, 0x0800),
        "offset 0x0800 (<= limit) must be outside the expand-down big segment"
    );
}

#[test]
fn test_env_limit_from_mcb_valid_mcb_with_data() {
    // Exercise the normal path: valid MCB with data
    let mut memory = Memory::new();
    let env_seg = 0x0100_u16;
    let mcb_seg = env_seg - 1;
    let mcb_addr = (mcb_seg as usize) * 16;
    let env_base = (env_seg as usize) * 16;

    // Write MCB header
    memory.write_u8(mcb_addr, 0x4D); // 'M' tag
    memory.write_u16(mcb_addr + 3, 0x0010); // size = 16 paragraphs = 256 bytes

    // Write environment data: "PATH=C:\\" + null + null (double-null terminator)
    let env_str = b"PATH=C:\\";
    for (i, &b) in env_str.iter().enumerate() {
        memory.write_u8(env_base + i, b);
    }
    memory.write_u8(env_base + env_str.len(), 0); // null terminator for string
    memory.write_u8(env_base + env_str.len() + 1, 0); // double-null terminator

    let result = Dpmi::env_limit_from_mcb(&memory, env_seg);
    assert!(result.is_some(), "Should find a valid limit");
}

#[test]
fn test_overlaps_descriptor_tables_mixed_zero_sizes() {
    // Line 4155: cover the case where one table has base!=0 but size==0,
    // while another table has both set (and does overlap)
    let mut dpmi = Dpmi::new();
    dpmi.gdt_base = 0x2000;
    dpmi.gdt_size = 0; // Zero size — should skip
    dpmi.ldt_base = 0x3000;
    dpmi.ldt_size = 0x1000; // Non-zero — should check
    dpmi.idt_base = 0;
    dpmi.idt_size = 0;

    // Range 0x3500..0x3600 overlaps LDT (0x3000..0x4000)
    assert!(dpmi.overlaps_descriptor_tables(0x3500, 0x3600));
    // Range 0x2000..0x2100 does NOT overlap (GDT skipped due to size=0)
    assert!(!dpmi.overlaps_descriptor_tables(0x2000, 0x2100));
}

#[test]
fn test_allocate_mem_handle_next_starts_at_zero() {
    // Line 4216-4217: next_mem_handle == 0 → start = 1
    let mut dpmi = Dpmi::new();
    dpmi.next_mem_handle = 0;

    let handle = dpmi.allocate_mem_handle();
    assert_eq!(
        handle,
        Some(1),
        "Should allocate handle 1 when starting from 0"
    );
}

#[test]
fn test_allocate_descriptors_retain_removes_gdt_selectors() {
    // Line 6392: (selector & 0x04) == 0 branch — GDT selectors in free_selectors
    // are removed by the retain filter when count > 1.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Push a GDT selector (TI bit = 0) — should be filtered out
    let gdt_sel: u16 = 0x0200; // bit 2 = 0 → GDT
    dpmi.free_selectors.push(gdt_sel);

    // Also push two consecutive LDT selectors that should survive the filter
    let ldt_sel_a: u16 = (4 << 3) | 0x07; // 0x0027, index 4, TI=1
    let ldt_sel_b: u16 = (5 << 3) | 0x07; // 0x002F, index 5, TI=1
    dpmi.free_selectors.push(ldt_sel_a);
    dpmi.free_selectors.push(ldt_sel_b);

    // Block the fallback path so we can observe retain behavior
    dpmi.next_selector = 0xFFF8;

    let selector = dpmi.allocate_descriptors(&mut cpu, &mut memory, 2);

    // The GDT selector should have been removed by retain
    assert!(
        !dpmi.free_selectors.contains(&gdt_sel),
        "GDT selector should be removed by retain filter"
    );
    // The two LDT selectors formed a consecutive run and were used
    assert_eq!(selector, ldt_sel_a, "Should allocate from the LDT run");
}

#[test]
fn test_allocate_descriptors_retain_removes_all_when_max_entries_zero() {
    // Line 6392: max_entries == 0 branch — all selectors removed by retain
    // when ldt_limit is non-zero but < 8, ldt_entry_count returns 0.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set ldt_limit to non-zero value less than 8 → (1+1)/8 = 0 entries
    cpu.regs.ldt_limit = 1;

    // Push valid LDT selectors into free_selectors
    let sel_a: u16 = (1 << 3) | 0x07; // 0x000F, index 1, TI=1
    let sel_b: u16 = (2 << 3) | 0x07; // 0x0017, index 2, TI=1
    dpmi.free_selectors.push(sel_a);
    dpmi.free_selectors.push(sel_b);

    // Block the fallback path
    dpmi.next_selector = 0xFFF8;

    let selector = dpmi.allocate_descriptors(&mut cpu, &mut memory, 2);

    // All selectors should have been removed by retain (max_entries == 0)
    assert!(
        dpmi.free_selectors.is_empty(),
        "All selectors should be removed when max_entries == 0"
    );
    // Allocation fails because free list is empty and fallback is blocked
    assert_eq!(selector, 0, "Should return 0 when no selectors available");
}

#[test]
fn test_watchpoint_size_from_raw_all_values() {
    assert_eq!(WatchpointSize::from_raw(1), Some(WatchpointSize::Byte));
    assert_eq!(WatchpointSize::from_raw(2), Some(WatchpointSize::Word));
    assert_eq!(WatchpointSize::from_raw(4), Some(WatchpointSize::DWord));
    assert_eq!(WatchpointSize::from_raw(0), None);
    assert_eq!(WatchpointSize::from_raw(3), None);
    assert_eq!(WatchpointSize::from_raw(5), None);
    assert_eq!(WatchpointSize::from_raw(255), None);
}

#[test]
fn test_watchpoint_type_from_raw_all_values() {
    assert_eq!(WatchpointType::from_raw(0), Some(WatchpointType::Execute));
    assert_eq!(WatchpointType::from_raw(1), Some(WatchpointType::Write));
    assert_eq!(WatchpointType::from_raw(2), Some(WatchpointType::ReadWrite));
    assert_eq!(WatchpointType::from_raw(3), None);
    assert_eq!(WatchpointType::from_raw(255), None);
}

#[test]
fn test_linear_memory_block_is_page_committed_beyond_range() {
    let block = LinearMemoryBlock::new(1, 0x100000, 0x3000, true);
    assert_eq!(block.page_count, 3);
    // Pages within range should be committed
    assert!(block.is_page_committed(0));
    assert!(block.is_page_committed(2));
    // Page beyond range returns false
    assert!(!block.is_page_committed(3));
    assert!(!block.is_page_committed(100));
}

#[test]
fn test_linear_memory_block_set_page_committed_beyond_range_noop() {
    let mut block = LinearMemoryBlock::new(1, 0x100000, 0x2000, false);
    // Setting committed beyond page_count should be a no-op
    block.set_page_committed(5, true);
    assert!(!block.is_page_committed(5));
}

#[test]
fn test_linear_memory_block_set_page_uncommit() {
    let mut block = LinearMemoryBlock::new(1, 0x100000, 0x2000, true);
    assert!(block.is_page_committed(0));
    assert!(block.is_page_committed(1));
    // Uncommit page 0
    block.set_page_committed(0, false);
    assert!(!block.is_page_committed(0));
    assert!(block.is_page_committed(1));
}

#[test]
fn test_linear_memory_block_resize_shrink_clears_upper_bits() {
    // Create a block with >64 pages to exercise word-boundary logic
    let mut block = LinearMemoryBlock::new(1, 0x100000, 65 * 0x1000, true);
    assert_eq!(block.page_count, 65);
    assert!(block.is_page_committed(64)); // page 64 should be committed

    // Shrink to 33 pages - not aligned to 64 boundary
    block.resize_pages(33, false);
    assert_eq!(block.page_count, 33);
    assert!(block.is_page_committed(0));
    assert!(block.is_page_committed(32));
    // Page 33 should now be out of range
    assert!(!block.is_page_committed(33));
}

#[test]
fn test_linear_memory_block_resize_grow_with_commit() {
    let mut block = LinearMemoryBlock::new(1, 0x100000, 2 * 0x1000, false);
    assert_eq!(block.page_count, 2);
    assert!(!block.is_page_committed(0));

    // Grow to 5 pages with commit_new=true
    block.resize_pages(5, true);
    assert_eq!(block.page_count, 5);
    // Old pages remain uncommitted
    assert!(!block.is_page_committed(0));
    assert!(!block.is_page_committed(1));
    // New pages should be committed
    assert!(block.is_page_committed(2));
    assert!(block.is_page_committed(3));
    assert!(block.is_page_committed(4));
}

#[test]
fn test_linear_memory_block_resize_shrink_to_exact_64_boundary() {
    // Create block with 128 pages (exactly 2 words)
    let mut block = LinearMemoryBlock::new(1, 0x100000, 128 * 0x1000, true);
    assert_eq!(block.page_count, 128);

    // Shrink to exactly 64 pages - word boundary
    block.resize_pages(64, false);
    assert_eq!(block.page_count, 64);
    assert!(block.is_page_committed(0));
    assert!(block.is_page_committed(63));
    assert!(!block.is_page_committed(64));
}

#[test]
fn test_free_memory_block_dpmi_1_0_linear_block() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate a DPMI 1.0 linear memory block
    cpu.regs.set_ax(0x0504);
    cpu.regs.ebx = 0; // any address
    cpu.regs.ecx = 0x4000; // 16KB
    cpu.regs.edx = 1; // committed
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "DPMI 1.0 allocate should succeed"
    );
    let handle = cpu.regs.esi;

    // Now free it using 0x0502
    cpu.regs.set_ax(0x0502);
    cpu.regs.esi = handle;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Free DPMI 1.0 block should succeed"
    );

    // Verify block is gone
    assert!(
        dpmi.linear_memory_blocks.is_empty(),
        "Linear memory blocks should be empty after free"
    );
}

#[test]
fn test_free_memory_block_invalid_handle_dpmi_1_0() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Try to free a handle that doesn't exist
    cpu.regs.set_ax(0x0502);
    cpu.regs.esi = 999;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Free nonexistent handle should fail"
    );
}

#[test]
fn test_validate_linear_region_overflow() {
    let dpmi = Dpmi::new();
    let memory = Memory::new();
    // linear + size overflows u32
    let result = dpmi.validate_linear_region(&memory, 0xFFFF_FFFF, 1);
    assert!(result.is_err());
}

#[test]
fn test_validate_linear_region_exceeds_ram() {
    let dpmi = Dpmi::new();
    let memory = Memory::new();
    // linear + size exceeds RAM
    let ram_size = memory.ram_size() as u32;
    let result = dpmi.validate_linear_region(&memory, ram_size, 1);
    assert!(result.is_err());
}

#[test]
fn test_validate_linear_region_valid() {
    let dpmi = Dpmi::new();
    let mut memory = Memory::new();
    memory.ensure_ram_size(0x200000);
    let result = dpmi.validate_linear_region(&memory, 0, 0x100000);
    assert!(result.is_ok());
}

#[test]
fn test_handle_in_use_with_linear_blocks() {
    let mut dpmi = Dpmi::new();
    // Add a linear memory block
    dpmi.linear_memory_blocks
        .push(LinearMemoryBlock::new(42, 0x200000, 0x1000, true));
    assert!(dpmi.handle_in_use(42));
    assert!(!dpmi.handle_in_use(43));
}

#[test]
fn test_handle_in_use_with_09_blocks() {
    let mut dpmi = Dpmi::new();
    dpmi.memory_blocks
        .push(MemoryBlock::new(7, 0x100000, 0x1000));
    assert!(dpmi.handle_in_use(7));
    assert!(!dpmi.handle_in_use(8));
}

#[test]
fn test_client_pm_int_handler_set_and_get() {
    let mut dpmi = Dpmi::new();
    assert!(dpmi.client_pm_int_handler(0x21).is_none());
    dpmi.set_client_pm_int_handler(0x21, 0x0107, 0x1234);
    assert_eq!(dpmi.client_pm_int_handler(0x21), Some((0x0107, 0x1234)));
    // Other vectors unaffected
    assert!(dpmi.client_pm_int_handler(0x22).is_none());
}

#[test]
fn test_pm_interrupt_handler_present_after_setup() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // After PM entry, all 256 IDT entries should be present
    let entry = dpmi.pm_interrupt_handler(0x21);
    assert!(entry.is_some());
    let idt = entry.unwrap();
    assert!(idt.present);
    assert_eq!(idt.selector, PM_BIOS_CODE_SEL);
}

#[test]
fn test_pm_interrupt_handler_default_has_bios_stub() {
    let dpmi = Dpmi::new();
    // Before PM entry, pm_idt entries have default BIOS stubs (present=true)
    let entry = dpmi.pm_interrupt_handler(0x21);
    assert!(entry.is_some());
    let idt = entry.unwrap();
    assert!(idt.present);
    assert_eq!(idt.selector, 0x0008); // Default code selector
}
/// MDD-BUG-CRUCIBLE-00443: a raw RM→PM switch that is REJECTED must leave
/// the guest exactly as it found it.
///
/// The descriptor-borrow repair converts a data-typed CS to code by
/// writing into the guest's own LDT. That write used to happen before
/// SS/DS/ES were validated, and the rollback snapshots — which live in
/// host memory — were not even created yet. So a switch refused for a bad
/// stack still left CS permanently executable, changing how the guest
/// behaved afterwards.
///
/// The eight descriptor bytes must come back byte-for-byte.
fn assert_rejected_raw_switch_preserves_borrowed_cs(spoil: fn(&Dpmi, &mut Memory, usize)) {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let (cs_sel, ds_sel, ss_sel, es_sel) = setup_raw_switch_ldt(&dpmi, &mut cpu, &mut memory);
    let ldt_base = cpu.regs.ldt_base as usize;

    // CS is borrowed as data (0xF2 = P=1, DPL=3, S=1, data writable), the
    // case the repair exists for.
    const BORROWED_CS_ACCESS: u8 = 0xF2;
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 1, 0, 0xFFFF, BORROWED_CS_ACCESS, 0x0);
    // The DPMI selector table tracks it too, and must also be restored.
    dpmi.selectors.push(Selector {
        selector: cs_sel,
        base: 0,
        limit: 0xFFFF,
        access: BORROWED_CS_ACCESS,
        flags: 0,
        lock_count: 0,
    });

    // Now make the switch fail somewhere AFTER the CS repair.
    spoil(&dpmi, &mut memory, ldt_base);

    let cs_descriptor_before: Vec<u8> = (0..8).map(|i| memory.read_u8(ldt_base + 8 + i)).collect();

    set_raw_switch_regs(&mut cpu, cs_sel, ds_sel, ss_sel, es_sel);
    let result = dpmi.handle_raw_switch_to_pm(&mut cpu, &mut memory);
    assert!(result.is_err(), "the switch was supposed to be rejected");

    let cs_descriptor_after: Vec<u8> = (0..8).map(|i| memory.read_u8(ldt_base + 8 + i)).collect();
    assert_eq!(
        cs_descriptor_after, cs_descriptor_before,
        "a rejected raw switch must not leave the guest's CS descriptor converted to code"
    );
    let tracked = dpmi
        .selectors
        .iter()
        .find(|s| s.selector == cs_sel)
        .expect("the selector-table entry should still be there");
    assert_eq!(
        tracked.access, BORROWED_CS_ACCESS,
        "a rejected raw switch must not leave the selector table converted to code"
    );
}
