use super::*;

#[test]
fn test_idt_entry_default() {
    let entry = IdtEntry::default();
    assert_eq!(entry.selector, 0);
    assert_eq!(entry.offset, 0);
    assert_eq!(entry.gate_type, 0);
    assert_eq!(entry.dpl, 0);
    assert!(!entry.present);
}

#[test]
fn test_real_mode_call_struct_size() {
    // DPMI spec says the memory layout is 50 bytes (0x32)
    // The Rust struct may be larger due to alignment, but I/O uses exact offsets.
    // Verify the last field ends at offset 0x30 + 2 = 0x32 = 50
    assert_eq!(RealModeCallStruct::MEMORY_SIZE, 50);
}

#[test]
fn test_real_mode_call_struct_read_write() {
    let mut memory = Memory::new();
    let addr = 0x70000_usize;

    let original = RealModeCallStruct {
        edi: 0x12345678,
        esi: 0x9ABCDEF0,
        ebp: 0x11111111,
        reserved: 0,
        ebx: 0x22222222,
        edx: 0x33333333,
        ecx: 0x44444444,
        eax: 0x55555555,
        flags: 0x0202,
        es: 0xB800,
        ds: 0x0040,
        fs: 0x0000,
        gs: 0x0000,
        ip: 0x0100,
        cs: 0x0200,
        sp: 0xFFFE,
        ss: 0x0300,
    };

    original.write_to_memory(&mut memory, addr);
    let loaded = RealModeCallStruct::read_from_memory(&memory, addr);

    assert_eq!(loaded.edi, original.edi);
    assert_eq!(loaded.esi, original.esi);
    assert_eq!(loaded.ebp, original.ebp);
    assert_eq!(loaded.ebx, original.ebx);
    assert_eq!(loaded.edx, original.edx);
    assert_eq!(loaded.ecx, original.ecx);
    assert_eq!(loaded.eax, original.eax);
    assert_eq!(loaded.flags, original.flags);
    assert_eq!(loaded.es, original.es);
    assert_eq!(loaded.ds, original.ds);
    assert_eq!(loaded.fs, original.fs);
    assert_eq!(loaded.gs, original.gs);
    assert_eq!(loaded.ip, original.ip);
    assert_eq!(loaded.cs, original.cs);
    assert_eq!(loaded.sp, original.sp);
    assert_eq!(loaded.ss, original.ss);
}

/// Property-based test: roundtrip (write -> read) should preserve all fields
/// Uses pseudo-random data to test edge cases like all zeros, all ones, etc.
#[test]
fn test_real_mode_call_struct_roundtrip_property() {
    let mut memory = Memory::new();
    let addr = 0x70000_usize;

    // Test cases with various patterns
    let test_patterns: [(u32, u16); 6] = [
        (0x00000000, 0x0000), // All zeros
        (0xFFFFFFFF, 0xFFFF), // All ones
        (0xAAAAAAAA, 0xAAAA), // Alternating bits
        (0x55555555, 0x5555), // Alternating bits (inverted)
        (0x12345678, 0x1234), // Sequential pattern
        (0xDEADBEEF, 0xCAFE), // Typical magic values
    ];

    for (dword_val, word_val) in test_patterns {
        let original = RealModeCallStruct {
            edi: dword_val,
            esi: dword_val.wrapping_add(1),
            ebp: dword_val.wrapping_add(2),
            reserved: dword_val.wrapping_add(3),
            ebx: dword_val.wrapping_add(4),
            edx: dword_val.wrapping_add(5),
            ecx: dword_val.wrapping_add(6),
            eax: dword_val.wrapping_add(7),
            flags: word_val,
            es: word_val.wrapping_add(1),
            ds: word_val.wrapping_add(2),
            fs: word_val.wrapping_add(3),
            gs: word_val.wrapping_add(4),
            ip: word_val.wrapping_add(5),
            cs: word_val.wrapping_add(6),
            sp: word_val.wrapping_add(7),
            ss: word_val.wrapping_add(8),
        };

        // Write to memory
        original.write_to_memory(&mut memory, addr);

        // Read back
        let loaded = RealModeCallStruct::read_from_memory(&memory, addr);

        // Roundtrip should produce identical struct
        assert_eq!(
            original, loaded,
            "Roundtrip failed for pattern dword=0x{:08X} word=0x{:04X}",
            dword_val, word_val
        );
    }
}

/// Test that writing to different addresses doesn't corrupt each other
#[test]
fn test_real_mode_call_struct_non_overlapping_writes() {
    let mut memory = Memory::new();
    let addr1 = 0x70000_usize;
    let addr2 = addr1 + RealModeCallStruct::MEMORY_SIZE;

    let struct1 = RealModeCallStruct {
        eax: 0x11111111,
        ebx: 0x22222222,
        ecx: 0x33333333,
        edx: 0x44444444,
        esi: 0x55555555,
        edi: 0x66666666,
        ebp: 0x77777777,
        reserved: 0x88888888,
        flags: 0x9999,
        es: 0xAAAA,
        ds: 0xBBBB,
        fs: 0xCCCC,
        gs: 0xDDDD,
        ip: 0xEEEE,
        cs: 0xFFFF,
        sp: 0x0000,
        ss: 0x1111,
    };

    let struct2 = RealModeCallStruct {
        eax: 0xFFFFFFFF,
        ebx: 0xEEEEEEEE,
        ecx: 0xDDDDDDDD,
        edx: 0xCCCCCCCC,
        esi: 0xBBBBBBBB,
        edi: 0xAAAAAAAA,
        ebp: 0x99999999,
        reserved: 0x88888888,
        flags: 0x7777,
        es: 0x6666,
        ds: 0x5555,
        fs: 0x4444,
        gs: 0x3333,
        ip: 0x2222,
        cs: 0x1111,
        sp: 0x0000,
        ss: 0xFFFF,
    };

    // Write both
    struct1.write_to_memory(&mut memory, addr1);
    struct2.write_to_memory(&mut memory, addr2);

    // Read both back
    let loaded1 = RealModeCallStruct::read_from_memory(&memory, addr1);
    let loaded2 = RealModeCallStruct::read_from_memory(&memory, addr2);

    // Both should be intact
    assert_eq!(struct1, loaded1, "struct1 corrupted after writing struct2");
    assert_eq!(struct2, loaded2, "struct2 not correctly written");
}

#[test]
fn test_load_rm_registers_sets_stack_when_nonzero() {
    let dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let rm_struct = RealModeCallStruct {
        ss: 0x2000,
        sp: 0x1234,
        ..RealModeCallStruct::default()
    };

    dpmi.load_rm_registers(&mut cpu, &mut memory, &rm_struct)
        .unwrap();

    assert_eq!(cpu.regs.ss, 0x2000);
    assert_eq!(cpu.regs.sp(), 0x1234);
}

#[test]
fn test_load_rm_registers_sets_stack_when_sp_only() {
    let dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let rm_struct = RealModeCallStruct {
        ss: 0x0000,
        sp: 0x4321,
        ..RealModeCallStruct::default()
    };

    dpmi.load_rm_registers(&mut cpu, &mut memory, &rm_struct)
        .unwrap();

    assert_eq!(cpu.regs.ss, 0x0000);
    assert_eq!(cpu.regs.sp(), 0x4321);
}

#[test]
fn test_load_rm_registers_skips_stack_when_zero() {
    let dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ss = 0x1111;
    cpu.regs.set_sp(0x2222);

    let rm_struct = RealModeCallStruct::default();
    dpmi.load_rm_registers(&mut cpu, &mut memory, &rm_struct)
        .unwrap();

    assert_eq!(cpu.regs.ss, 0x1111);
    assert_eq!(cpu.regs.sp(), 0x2222);
}

#[test]
fn test_load_rm_registers_applies_zero_flags() {
    let dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.flags =
        gp64_cpu::Flags::CARRY | gp64_cpu::Flags::INTERRUPT | gp64_cpu::Flags::RESERVED1;

    let rm_struct = RealModeCallStruct {
        flags: 0,
        ..RealModeCallStruct::default()
    };
    dpmi.load_rm_registers(&mut cpu, &mut memory, &rm_struct)
        .unwrap();

    assert_eq!(cpu.regs.flags, gp64_cpu::Flags::RESERVED1);
}

#[test]
fn test_lock_unlock_selector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate a descriptor to lock
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let selector = cpu.regs.ax();

    // Lock selector
    cpu.regs.set_ax(0x0004);
    cpu.regs.set_bx(selector);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Unlock selector
    cpu.regs.set_ax(0x0005);
    cpu.regs.set_bx(selector);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_lock_selector_rejects_gdt_selector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0004);
    cpu.regs.set_bx(0x0008); // GDT selector (TI=0)
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_lock_selector_missing_descriptor_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0004);
    cpu.regs.set_bx(0x0017); // LDT selector, not allocated
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_lock_selector_out_of_ldt_range_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ldt_limit = 0x0007; // One LDT entry (selector 0x0007)
    cpu.regs.set_ax(0x0004);
    cpu.regs.set_bx(0x0017); // LDT selector beyond limit
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_unlock_selector_rejects_gdt_selector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0005);
    cpu.regs.set_bx(0x0008); // GDT selector (TI=0)
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_unlock_selector_missing_descriptor_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0005);
    cpu.regs.set_bx(0x0017); // LDT selector, not allocated
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_unlock_selector_without_lock_succeeds() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    cpu.regs.set_ax(0x0005);
    cpu.regs.set_bx(selector);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_allocate_specific_ldt_descriptor() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Try to allocate a specific LDT selector (with TI bit set)
    let selector = 0x007F; // LDT selector (TI=1, RPL=3, index=0x0F)
    cpu.regs.set_ax(0x000D);
    cpu.regs.set_bx(selector);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.bx(), selector);

    // "hardwires" initial mode-switch selectors via 000Dh, expecting success
    // even if the DPMI host already set them up).
    cpu.regs.set_ax(0x000D);
    cpu.regs.set_bx(selector);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.bx(), selector);
}

/// MDD-BUG-CRU-01401: with the selector table full, AX=000Dh refuses a
/// free low LDT index with 8011h instead of panicking on the push.
#[test]
fn allocate_specific_descriptor_at_table_capacity_is_refused() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    while dpmi.selectors.len() < dpmi.selectors.capacity() {
        let selector = 0x1007 + (dpmi.selectors.len() as u16) * 8;
        dpmi.selectors.push(Selector {
            selector,
            base: 0,
            limit: 0,
            access: 0xF2,
            flags: 0,
            lock_count: 0,
        });
    }

    cpu.regs.set_ax(0x000D);
    cpu.regs.set_bx(0x007F);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0x8011);
    assert_eq!(dpmi.selectors.len(), dpmi.selectors.capacity());
}

#[test]
fn test_allocate_specific_gdt_descriptor_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Try to allocate a GDT selector (TI bit = 0) - should fail
    let selector = 0x0108; // GDT selector
    cpu.regs.set_ax(0x000D);
    cpu.regs.set_bx(selector);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0x8022); // Invalid selector
}

#[test]
fn test_allocate_specific_ldt_descriptor_out_of_range_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ldt_limit = 0x0007; // One LDT entry (selector 0x0007)
    cpu.regs.set_ax(0x000D);
    cpu.regs.set_bx(0x0017); // Beyond LDT limit
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0x8022);
}

#[test]
fn test_allocate_specific_ldt_descriptor_small_range_rejects() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ldt_limit = 0x007F; // 16 entries (0x0-0x7F)
    cpu.regs.set_ax(0x000D);
    cpu.regs.set_bx(0x010F); // index 0x21 -> out of range
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0x8022);
}

#[test]
fn test_allocate_specific_ldt_descriptor_rejects_above_spec_range() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ldt_limit = 0x0FFF; // Large enough LDT
    cpu.regs.set_ax(0x000D);
    cpu.regs.set_bx(0x010F); // index 0x21 -> beyond first 16 entries
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_allocate_specific_ldt_descriptor_updates_next_selector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.next_selector = 0x0007;
    let selector: u16 = 0x0057;
    let expected_next = selector.wrapping_add(8);
    cpu.regs.set_ax(0x000D);
    cpu.regs.set_bx(selector);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dpmi.next_selector, expected_next);
}

#[test]
fn test_allocate_specific_ldt_descriptor_overflow_does_not_advance() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.next_selector = 0xFFFE;
    let selector: u16 = 0xFFFF;
    cpu.regs.set_ax(0x000D);
    cpu.regs.set_bx(selector);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dpmi.next_selector, 0xFFFE);
}

#[test]
fn test_allocate_specific_ldt_descriptor_does_not_rewind_next_selector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.next_selector = 0x0207;
    let selector: u16 = 0x0077;
    cpu.regs.set_ax(0x000D);
    cpu.regs.set_bx(selector);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dpmi.next_selector, 0x0207);
}

#[test]
fn test_allocate_specific_invalidates_segment_cache() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.cr0 = 1;
    cpu.regs.ldt_base = 0x2000;
    cpu.regs.ldt_limit = 0x0FFF;

    // Allocate a descriptor and load DS with it.
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    cpu.load_data_segment(&memory, gp64_cpu::DataSegReg::DS, selector);
    assert!(cpu.regs.ds_cache.present);

    // Free the descriptor - DS cache should become not present.
    cpu.regs.set_ax(0x0001);
    cpu.regs.set_bx(selector);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.ds_cache.present);

    // Allocate the same selector via 0x000D; cache should be refreshed to present.
    cpu.regs.set_ax(0x000D);
    cpu.regs.set_bx(selector);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(cpu.regs.ds_cache.present);
}

#[test]
fn test_allocate_descriptors_zero_count_returns_zero() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let selector = dpmi.allocate_descriptors(&mut cpu, &mut memory, 0);
    assert_eq!(selector, 0);
}

#[test]
fn test_int31_allocate_descriptors_zero_count_returns_unavailable() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_allocate_descriptors_uses_free_selector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.free_selectors.push(0x0207);
    let selector = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);

    assert_eq!(selector, 0x0207);
    assert!(dpmi.selectors.iter().any(|s| s.selector == 0x0207));
}

#[test]
fn test_allocate_descriptors_skips_free_selector_out_of_range() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ldt_limit = 0x0007; // One LDT entry (selector 0x0007)
    dpmi.free_selectors.push(0x0007);
    dpmi.free_selectors.push(0x0017); // Out of range, should be skipped

    let selector = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);

    assert_eq!(selector, 0x0007);
    assert!(dpmi.selectors.iter().any(|s| s.selector == 0x0007));
}

#[test]
fn test_allocate_descriptors_count_two_succeeds() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let selector = dpmi.allocate_descriptors(&mut cpu, &mut memory, 2);
    assert_eq!(selector, 0x0107);
    assert_eq!(dpmi.selectors.len(), 2);
}

#[test]
fn test_allocate_descriptors_reuses_free_run() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.free_selectors.push(0x0207);
    dpmi.free_selectors.push(0x020F);

    let selector = dpmi.allocate_descriptors(&mut cpu, &mut memory, 2);

    assert_eq!(selector, 0x0207);
    assert!(dpmi.selectors.iter().any(|s| s.selector == 0x0207));
    assert!(dpmi.selectors.iter().any(|s| s.selector == 0x020F));
    assert!(!dpmi.free_selectors.contains(&0x0207));
    assert!(!dpmi.free_selectors.contains(&0x020F));
}

#[test]
fn test_allocate_descriptors_overflow_returns_zero() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.next_selector = 0xFFFE;
    let selector = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
    assert_eq!(selector, 0);
}

#[test]
fn test_free_descriptor_invalid_range_returns_false() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ldt_limit = 0x0007;
    let result = dpmi.free_descriptor(&mut cpu, &mut memory, 0x0017);
    assert!(!result);
}

#[test]
fn test_free_descriptor_missing_selector_returns_false() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ldt_limit = 0x0FFF;
    let result = dpmi.free_descriptor(&mut cpu, &mut memory, 0x0207);
    assert!(!result);
}

#[test]
fn test_free_descriptor_removes_selector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let selector = 0x0207;
    dpmi.selectors.push(Selector {
        selector,
        base: 0,
        limit: 0xFFFF,
        access: 0xF2,
        flags: 0,
        lock_count: 0,
    });

    let result = dpmi.free_descriptor(&mut cpu, &mut memory, selector);
    assert!(result);
    assert!(!dpmi.selectors.iter().any(|s| s.selector == selector));
}

#[test]
fn test_write_selector_descriptor_ignores_gdt_selector() {
    let dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let wrote = dpmi.write_selector_descriptor(&mut cpu, &mut memory, 0x0008, 0, 0, 0, 0);
    assert!(!wrote);
}

#[test]
fn test_write_selector_descriptor_out_of_range_returns() {
    let dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ldt_base = 0x20000;
    cpu.regs.ldt_limit = 0x0007;
    let wrote = dpmi.write_selector_descriptor(&mut cpu, &mut memory, 0x0017, 0, 0, 0, 0);
    assert!(!wrote);
}

#[test]
fn test_write_selector_descriptor_returns_true_on_success() {
    let dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ldt_base = 0x20000;
    cpu.regs.ldt_limit = 0x0FFF;
    let wrote = dpmi.write_selector_descriptor(&mut cpu, &mut memory, 0x0007, 0, 0xFFFF, 0xF2, 0);
    assert!(wrote);
}

#[test]
fn test_set_descriptor_base_updates_entry() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ldt_base = 0x20000;
    cpu.regs.ldt_limit = 0x0FFF;
    let selector = 0x0207;
    dpmi.selectors.push(Selector {
        selector,
        base: 0,
        limit: 0xFFFF,
        access: 0xF2,
        flags: 0,
        lock_count: 0,
    });

    dpmi.set_descriptor_base(&mut cpu, &mut memory, selector, 0x1234_0000);
    assert_eq!(dpmi.selectors[0].base, 0x1234_0000);
}

#[test]
fn test_set_descriptor_base_ignores_gdt_selector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let selector = 0x0207;
    dpmi.selectors.push(Selector {
        selector,
        base: 0,
        limit: 0xFFFF,
        access: 0xF2,
        flags: 0,
        lock_count: 0,
    });

    dpmi.set_descriptor_base(&mut cpu, &mut memory, 0x0008, 0x1234_0000);
    assert_eq!(dpmi.selectors[0].base, 0);
}

#[test]
fn test_set_descriptor_base_missing_selector_noop() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.set_descriptor_base(&mut cpu, &mut memory, 0x0207, 0x1234_0000);
    assert!(dpmi.selectors.is_empty());
}

#[test]
fn test_set_descriptor_base_out_of_range_noop() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let selector = 0x0207;
    dpmi.selectors.push(Selector {
        selector,
        base: 0x1111_0000,
        limit: 0xFFFF,
        access: 0xF2,
        flags: 0,
        lock_count: 0,
    });

    cpu.regs.ldt_base = 0x20000;
    cpu.regs.ldt_limit = 0x0007; // Only one LDT entry available.
    dpmi.set_descriptor_base(&mut cpu, &mut memory, selector, 0x2222_0000);
    assert_eq!(dpmi.selectors[0].base, 0x1111_0000);
}

#[test]
fn test_set_descriptor_limit_preserves_user_flags() {
    // Test that set_descriptor_limit preserves user-set flags and does NOT
    // modify the G bit. The G bit is computed automatically during memory
    // encoding based on the limit value.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ldt_base = 0x20000;
    cpu.regs.ldt_limit = 0x0FFF;
    let selector = 0x0207;
    dpmi.selectors.push(Selector {
        selector,
        base: 0,
        limit: 0xFFFF,
        access: 0xF2,
        flags: 0,
        lock_count: 0,
    });

    dpmi.set_descriptor_limit(&mut cpu, &mut memory, selector, 0x0010_0000);
    assert_eq!(dpmi.selectors[0].limit, 0x0010_0000);
    // User-visible flags should NOT have G bit set
    // (G bit is only computed during memory encoding)
    assert_eq!(
        dpmi.selectors[0].flags & 0x8,
        0,
        "User-visible flags should not have G bit modified"
    );
}

#[test]
fn test_set_descriptor_limit_ignores_gdt_selector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let selector = 0x0207;
    dpmi.selectors.push(Selector {
        selector,
        base: 0,
        limit: 0xFFFF,
        access: 0xF2,
        flags: 0,
        lock_count: 0,
    });

    dpmi.set_descriptor_limit(&mut cpu, &mut memory, 0x0008, 0x0010_0000);
    assert_eq!(dpmi.selectors[0].limit, 0xFFFF);
}

#[test]
fn test_set_descriptor_limit_missing_selector_noop() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.set_descriptor_limit(&mut cpu, &mut memory, 0x0207, 0x0010_0000);
    assert!(dpmi.selectors.is_empty());
}

#[test]
fn test_set_descriptor_limit_out_of_range_noop() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let selector = 0x0207;
    dpmi.selectors.push(Selector {
        selector,
        base: 0,
        limit: 0x1234,
        access: 0xF2,
        flags: 0,
        lock_count: 0,
    });

    cpu.regs.ldt_base = 0x20000;
    cpu.regs.ldt_limit = 0x0007;
    dpmi.set_descriptor_limit(&mut cpu, &mut memory, selector, 0x5678);
    assert_eq!(dpmi.selectors[0].limit, 0x1234);
}

#[test]
fn test_set_descriptor_access_updates_entry() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ldt_base = 0x20000;
    cpu.regs.ldt_limit = 0x0FFF;
    let selector = 0x0207;
    dpmi.selectors.push(Selector {
        selector,
        base: 0,
        limit: 0xFFFF,
        access: 0xF2,
        flags: 0,
        lock_count: 0,
    });

    dpmi.set_descriptor_access(&mut cpu, &mut memory, selector, 0xFA, 0x40);
    assert_eq!(dpmi.selectors[0].access, 0xFA);
    assert_eq!(dpmi.selectors[0].flags, 0x40);
}

#[test]
fn test_set_descriptor_access_ignores_gdt_selector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let selector = 0x0207;
    dpmi.selectors.push(Selector {
        selector,
        base: 0,
        limit: 0xFFFF,
        access: 0xF2,
        flags: 0,
        lock_count: 0,
    });

    dpmi.set_descriptor_access(&mut cpu, &mut memory, 0x0008, 0xFA, 0x40);
    assert_eq!(dpmi.selectors[0].access, 0xF2);
    assert_eq!(dpmi.selectors[0].flags, 0);
}

#[test]
fn test_set_descriptor_access_missing_selector_noop() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.set_descriptor_access(&mut cpu, &mut memory, 0x0207, 0xFA, 0x40);
    assert!(dpmi.selectors.is_empty());
}

#[test]
fn test_set_descriptor_access_out_of_range_noop() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let selector = 0x0207;
    dpmi.selectors.push(Selector {
        selector,
        base: 0,
        limit: 0xFFFF,
        access: 0xF2,
        flags: 0,
        lock_count: 0,
    });

    cpu.regs.ldt_base = 0x20000;
    cpu.regs.ldt_limit = 0x0007;
    dpmi.set_descriptor_access(&mut cpu, &mut memory, selector, 0xFA, 0x40);
    assert_eq!(dpmi.selectors[0].access, 0xF2);
    assert_eq!(dpmi.selectors[0].flags, 0);
}

#[test]
fn test_get_descriptor_access_returns_access_and_flags() {
    let mut dpmi = Dpmi::new();
    let selector = 0x0207;
    dpmi.selectors.push(Selector {
        selector,
        base: 0x12345000,
        limit: 0xFFFF,
        access: 0xF2,
        flags: 0x40,
        lock_count: 0,
    });

    let result = dpmi.get_descriptor_access(selector);
    assert_eq!(result, Some((0xF2, 0x40)));
}

#[test]
fn test_get_descriptor_access_nonexistent_returns_none() {
    let dpmi = Dpmi::new();
    assert!(dpmi.get_descriptor_access(0x0207).is_none());
}

#[test]
fn test_get_descriptor_access_ignores_gdt_selector() {
    let mut dpmi = Dpmi::new();
    dpmi.selectors.push(Selector {
        selector: 0x0207,
        base: 0,
        limit: 0xFFFF,
        access: 0xF2,
        flags: 0x40,
        lock_count: 0,
    });

    // GDT selector (TI bit = 0)
    assert!(dpmi.get_descriptor_access(0x0008).is_none());
}

#[test]
fn test_is_selector_32bit_ldt_cases() {
    let mut dpmi = Dpmi::new();
    let selector = 0x0207;
    dpmi.selectors.push(Selector {
        selector,
        base: 0,
        limit: 0xFFFF,
        access: 0xF2,
        flags: 0x40,
        lock_count: 0,
    });

    assert!(dpmi.is_selector_32bit(selector));
    assert!(!dpmi.is_selector_32bit(0x0217));
}

#[test]
fn test_is_selector_32bit_gdt_selector() {
    let dpmi = Dpmi::new();
    assert!(!dpmi.is_selector_32bit(0x0008));
}

#[test]
fn test_is_selector_32bit_mem_internal_list() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let cpu = Cpu::new();
    let memory = Memory::new();

    let selector = 0x0207;
    dpmi.selectors.push(Selector {
        selector,
        base: 0,
        limit: 0xFFFF,
        access: 0xF2,
        flags: 0x40,
        lock_count: 0,
    });

    assert!(dpmi.is_selector_32bit_mem(&cpu, &memory, selector));
}

#[test]
fn test_is_selector_32bit_mem_reads_ldt() {
    let _tracing = init_tracing();
    let dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ldt_base = 0x20000;
    let selector = 0x0207;
    let index = (selector >> 3) as usize;
    let desc_addr = cpu.regs.ldt_base as usize + index * 8;
    memory.write_u8(desc_addr + 6, 0x40);

    assert!(dpmi.is_selector_32bit_mem(&cpu, &memory, selector));
}

#[test]
fn test_is_selector_32bit_mem_gdt_selector() {
    let dpmi = Dpmi::new();
    let cpu = Cpu::new();
    let memory = Memory::new();

    assert!(!dpmi.is_selector_32bit_mem(&cpu, &memory, 0x0008));
}

#[test]
fn test_is_selector_32bit_mem_missing_ldt_base_returns_false() {
    let _tracing = init_tracing();
    let dpmi = Dpmi::new();
    let cpu = Cpu::new();
    let memory = Memory::new();

    assert!(!dpmi.is_selector_32bit_mem(&cpu, &memory, 0x0207));
}

#[test]
fn test_linear_address_for_selector_out_of_limit() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.protected_mode = true;
    let selector = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
    dpmi.set_descriptor_limit(&mut cpu, &mut memory, selector, 0x10);

    let result = dpmi.linear_address_for_selector(&cpu, &mut memory, selector, 0x20);
    assert_eq!(result, Err(DPMI_ERR_INVALID_SELECTOR));
}

#[test]
fn test_linear_address_for_selector_within_limit() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.protected_mode = true;
    let selector = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
    dpmi.set_descriptor_limit(&mut cpu, &mut memory, selector, 0x20);

    let result = dpmi.linear_address_for_selector(&cpu, &mut memory, selector, 0x10);
    assert!(result.is_ok());
}

#[test]
fn test_get_set_exception_handler() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    setup_gdt_code_descriptor(&dpmi, &mut cpu, &mut memory, 0x0008, 0xFFFF);

    // Set exception handler for GPF (0x0D)
    cpu.regs.set_ax(0x0203);
    cpu.regs.set_bl(0x0D);
    cpu.regs.set_cx(0x0008); // selector
    cpu.regs.edx = 0x12345678; // offset
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Get it back
    cpu.regs.set_ax(0x0202);
    cpu.regs.set_bl(0x0D);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.cx(), 0x0008);
    assert_eq!(cpu.regs.edx, 0x12345678);
}

#[test]
fn test_exception_handler_frame_width_follows_client_bitness() {
    // MDD-BUG-KILN-00635: DPMI exception-frame width follows the client's
    // bitness, even when its handler uses a code segment of the opposite width.
    // A 32-bit client reads dword slots (EIP at [bp+12h], CS at [bp+16h])
    // and returns with a 32-bit far return (66 CB). A word frame would load
    // EIP's high word as CS/DS and cause repeated faults.
    // An earlier revision of this test asserted the opposite rule
    // ("frames follow the handler code segment width") — that pinned the
    // bug. Same rule as AX=0205h ("Gate size matches DPMI client type").
    //
    // Covers 0203h and the DPMI 1.0 extended form 0213h, in both client
    // modes, with a handler segment of the OPPOSITE width each time.
    for (set_fn, cpu_mode, handler_flags, want_gate) in [
        (0x0203u16, 1u8, 0x00u8, 0x0Eu8), // 32-bit client, USE16 handler
        (0x0203u16, 0u8, 0x40u8, 0x06u8), // 16-bit client, USE32 handler
        (0x0213u16, 1u8, 0x00u8, 0x0Eu8),
        (0x0213u16, 0u8, 0x40u8, 0x06u8),
    ] {
        let mut dpmi = Dpmi::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();

        dpmi.cpu_mode = cpu_mode;

        let gdt_base = 0x2000;
        cpu.regs.gdtr.base = gdt_base;
        cpu.regs.gdtr.limit = 0x000F; // 2 descriptors (16 bytes)

        // GDT selector index 1 (selector 0x0008): code segment whose D
        // bit deliberately CONTRADICTS the client bitness.
        dpmi.write_descriptor_raw(
            &mut memory,
            gdt_base as usize,
            1,
            0,
            0xFFFF,
            0xFA,
            handler_flags >> 4,
        );

        cpu.regs.set_ax(set_fn);
        cpu.regs.set_bl(0x0D);
        cpu.regs.set_cx(0x0008);
        cpu.regs.edx = 0x1234;
        dpmi.int31(&mut cpu, &mut memory).unwrap();

        assert!(
            !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "AX={set_fn:#06X} cpu_mode={cpu_mode}: install must succeed"
        );
        assert_eq!(
            dpmi.exception_handlers[0x0D].gate_type, want_gate,
            "AX={set_fn:#06X} cpu_mode={cpu_mode}: exception frame width \
             must follow client bitness"
        );
    }
}

#[test]
fn test_exception_handler_out_of_range() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Try to set exception 0x20 (out of range - only 0-31 are exceptions)
    cpu.regs.set_ax(0x0203);
    cpu.regs.set_bl(0x20);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0x8021);
}

#[test]
fn test_set_exception_handler_invalid_linear_returns_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    setup_gdt_code_descriptor(&dpmi, &mut cpu, &mut memory, 0x0008, 0x00FF);

    cpu.regs.set_ax(0x0203);
    cpu.regs.set_bl(0x0D);
    cpu.regs.set_cx(0x0008);
    cpu.regs.set_dx(0x0100);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_LINEAR);
}

#[test]
fn test_set_exception_handler_rejects_data_selector() {
    // MDD-BUG-FLUXHOMEARPA-00011: a DPMI exception handler is entered as CS, so
    // the target selector must be a CODE segment. AX=0203h and AX=0213h must
    // reject a present writable DATA descriptor with CF=1, AX=8022h, and leave
    // the handler table unchanged (matching AX=0205h).
    let gdt_base = 0x2000u32;
    for ax in [0x0203u16, 0x0213u16] {
        let mut dpmi = Dpmi::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        cpu.regs.gdtr.base = gdt_base;
        cpu.regs.gdtr.limit = 0x000F;
        // GDT[1] (selector 0x0008): present writable DATA descriptor (access 0x92).
        dpmi.write_descriptor_raw(&mut memory, gdt_base as usize, 1, 0, 0xFFFF, 0x92, 0x00);

        let before_selector = dpmi.exception_handlers[0x0D].selector;
        let before_present = dpmi.exception_handlers[0x0D].present;
        cpu.regs.set_ax(ax);
        cpu.regs.set_bl(0x0D);
        cpu.regs.set_cx(0x0008);
        cpu.regs.edx = 0x1234;
        dpmi.int31(&mut cpu, &mut memory).unwrap();

        assert!(
            cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "AX={ax:#06X}: a data selector must set CF"
        );
        assert_eq!(
            cpu.regs.ax(),
            DPMI_ERR_INVALID_SELECTOR,
            "AX={ax:#06X}: expect AX=0x8022"
        );
        assert_eq!(
            dpmi.exception_handlers[0x0D].selector, before_selector,
            "AX={ax:#06X}: handler table must be unchanged"
        );
        assert_eq!(
            dpmi.exception_handlers[0x0D].present, before_present,
            "AX={ax:#06X}: handler table must be unchanged"
        );
    }
}

#[test]
fn test_get_exception_handler_out_of_range() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0202);
    cpu.regs.set_bl(0x20);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_get_exception_handler_reads_from_exception_handlers() {
    // DPMI 0202 must always read from exception_handlers[] (set by 0203),
    // NOT from the IDT.  0203 intentionally does NOT write exception
    // handlers to the IDT because vectors 0-31 overlap with hardware IRQ
    // vectors.  Reading from the IDT would return stale HLE stubs or
    // hardware IRQ handlers instead of the handler set via 0203.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up an IDT with a different value to prove we DON'T read from it
    let idt_base = 0x8000usize;
    cpu.regs.idtr.base = idt_base as u32;
    dpmi.write_idt_gate_full(&mut memory, idt_base, 0x0D, 0xBEEF, 0x00AA, 0x8E);

    // Set exception handler 0x0D via the exception_handlers[] table
    dpmi.exception_handlers[0x0D].selector = 0x0040;
    dpmi.exception_handlers[0x0D].offset = 0x1234;

    cpu.regs.set_ax(0x0202);
    cpu.regs.set_bl(0x0D);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // Should return the exception_handlers[] value, NOT the IDT value
    assert_eq!(cpu.regs.cx(), 0x0040);
    assert_eq!(cpu.regs.dx(), 0x1234);
}

#[test]
fn test_set_exception_handler_32bit_preserves_idt_gate() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // 4GB code descriptor: limit=0xFFFFF with G=1 (flags=0x0C) gives 4GB range
    let gdt_base = 0x2000usize;
    let index = (0x0050u16 >> 3) as usize;
    cpu.regs.gdtr.base = gdt_base as u32;
    cpu.regs.gdtr.limit = ((index + 1) * 8 - 1) as u16;
    dpmi.write_descriptor_raw(&mut memory, gdt_base, index, 0, 0xFFFFF, 0xFA, 0x0C);

    let idt_base = 0x9000usize;
    cpu.regs.idtr.base = idt_base as u32;
    dpmi.write_idt_gate_full(&mut memory, idt_base, 0x0E, 0xBEEF_CAFE, 0x00AA, 0x8E);
    dpmi.cpu_mode = 1;
    cpu.cs_bits = 32; // Required for 32-bit offset (EDX vs DX)

    cpu.regs.set_ax(0x0203);
    cpu.regs.set_bl(0x0E);
    cpu.regs.set_cx(0x0050);
    cpu.regs.edx = 0x1234_5678;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let (selector, offset) = dpmi.read_idt_gate(&memory, idt_base, 0x0E);
    assert_eq!(selector, 0x00AA);
    assert_eq!(offset, 0xBEEF_CAFE);
    assert_eq!(dpmi.exception_handlers[0x0E].selector, 0x0050);
    assert_eq!(dpmi.exception_handlers[0x0E].offset, 0x1234_5678);
    assert_eq!(dpmi.exception_handlers[0x0E].gate_type, 0x0E);
}

#[test]
fn test_extended_exception_handler_uses_edx_and_handler_descriptor_width() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    setup_gdt_code_descriptor(&dpmi, &mut cpu, &mut memory, 0x0008, 0xFFFFF);

    cpu.regs.set_ax(0x0213);
    cpu.regs.set_bl(0x0D);
    cpu.regs.set_cx(0x0008);
    cpu.regs.edx = 0x0001_2345;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dpmi.exception_handlers[0x0D].offset, 0x0001_2345);
    assert_eq!(dpmi.exception_handlers[0x0D].gate_type, 0x06);

    cpu.regs.set_ax(0x0212);
    cpu.regs.set_bl(0x0D);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.cx(), 0x0008);
    assert_eq!(cpu.regs.edx, 0x0001_2345);
}

#[test]
fn test_extended_exception_handler_out_of_range() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0212);
    cpu.regs.set_bl(0x20);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_get_set_pm_interrupt_vector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    setup_gdt_code_descriptor(&dpmi, &mut cpu, &mut memory, 0x0028, 0xFFFF);

    // Set PM interrupt vector for INT 21h
    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x21);
    cpu.regs.set_cx(0x0028); // 32-bit code selector
    cpu.regs.edx = 0xABCD1234; // offset
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Get it back
    cpu.regs.set_ax(0x0204);
    cpu.regs.set_bl(0x21);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.cx(), 0x0028);
    assert_eq!(cpu.regs.edx, 0xABCD1234);
}

#[test]
fn test_set_pm_interrupt_vector_invalid_linear_returns_error() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    setup_gdt_code_descriptor(&dpmi, &mut cpu, &mut memory, 0x0008, 0x00FF);

    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x21);
    cpu.regs.set_cx(0x0008);
    cpu.regs.set_dx(0x0100);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_LINEAR);
}

#[test]
fn test_get_protected_mode_interrupt_vector_from_idt() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let idt_base = 0xA000usize;
    cpu.regs.idtr.base = idt_base as u32;
    dpmi.cpu_mode = 1;
    cpu.cs_bits = 32; // Required for 32-bit offset (EDX vs DX)
    dpmi.write_idt_gate_full(&mut memory, idt_base, 0x10, 0x2222_3333, 0x0048, 0x8E);

    cpu.regs.set_ax(0x0204);
    cpu.regs.set_bl(0x10);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.cx(), 0x0048);
    assert_eq!(cpu.regs.edx, 0x2222_3333);

    cpu.regs.set_ax(0x0204);
    cpu.regs.set_bl(0x80);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_allocate_rm_callback() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0303);
    cpu.regs.ds = 0x0008;
    cpu.regs.esi = 0x1000;
    cpu.regs.es = 0x0010;
    cpu.regs.edi = 0x2000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // CX:DX = callback address
    assert_eq!(cpu.regs.cx(), 0xF200); // Callback segment
    let _ = cpu.regs.dx(); // Callback offset (can be 0)
}

#[test]
fn test_allocate_rm_callback_16bit_uses_si_di() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 0; // 16-bit client

    cpu.regs.set_ax(0x0303);
    cpu.regs.ds = 0x0008;
    cpu.regs.set_si(0x1111);
    cpu.regs.esi = 0xAAAA_1111;
    cpu.regs.es = 0x0010;
    cpu.regs.set_di(0x2222);
    cpu.regs.edi = 0xBBBB_2222;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let cb = &dpmi.rm_callbacks[0];
    assert!(cb.in_use);
    assert_eq!(cb.pm_offset, 0x1111);
    assert_eq!(cb.reg_struct_offset, 0x2222);
}

#[test]
fn test_allocate_rm_callback_32bit_uses_esi_edi() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client
    cpu.cs_bits = 32;

    cpu.regs.set_ax(0x0303);
    cpu.regs.ds = 0x0008;
    cpu.regs.set_si(0x1111);
    cpu.regs.esi = 0xAAAA_1111;
    cpu.regs.es = 0x0010;
    cpu.regs.set_di(0x2222);
    cpu.regs.edi = 0xBBBB_2222;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let cb = &dpmi.rm_callbacks[0];
    assert!(cb.in_use);
    assert_eq!(cb.pm_offset, 0xAAAA_1111);
    assert_eq!(cb.reg_struct_offset, 0xBBBB_2222);
}

#[test]
fn test_free_rm_callback() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // First allocate a callback
    cpu.regs.set_ax(0x0303);
    cpu.regs.ds = 0x0008;
    cpu.regs.esi = 0x1000;
    cpu.regs.es = 0x0010;
    cpu.regs.edi = 0x2000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    let seg = cpu.regs.cx();
    let off = cpu.regs.dx();

    // Free it
    cpu.regs.set_ax(0x0304);
    cpu.regs.set_cx(seg);
    cpu.regs.set_dx(off);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_free_invalid_rm_callback() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Try to free a callback that wasn't allocated
    cpu.regs.set_ax(0x0304);
    cpu.regs.set_cx(0xF000);
    cpu.regs.set_dx(0xFFFF);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0x8024); // Invalid callback address
}

#[test]
fn test_get_memory_info() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up buffer
    let buf_addr = 0x60000_usize;
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);

    cpu.regs.set_ax(0x0500);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify structure was filled based on the current RAM size.
    let largest = memory.read_u32(buf_addr);
    let expected = (memory.ram_size() as u32).saturating_sub(0x100000);
    assert_eq!(largest, expected);
}

#[test]
fn test_allocate_memory_block() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate 64KB
    cpu.regs.set_ax(0x0501);
    cpu.regs.set_bx(0x0001);
    cpu.regs.set_cx(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Should return address in BX:CX and handle in SI:DI
    let addr = ((cpu.regs.bx() as u32) << 16) | (cpu.regs.cx() as u32);
    let handle = ((cpu.regs.si() as u32) << 16) | (cpu.regs.di() as u32);

    assert!(addr >= 0x100000); // Above 1MB
    assert!(handle > 0);
}

#[test]
fn test_allocate_memory_block_skips_existing_handles() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.memory_blocks
        .push(MemoryBlock::new(1, 0x100000, 0x1000));
    dpmi.linear_memory_blocks
        .push(LinearMemoryBlock::new(2, 0x200000, 0x1000, true));
    dpmi.next_mem_handle = 1;

    cpu.regs.set_ax(0x0501);
    cpu.regs.set_bx(0x0001);
    cpu.regs.set_cx(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = ((cpu.regs.si() as u32) << 16) | (cpu.regs.di() as u32);
    assert_eq!(handle, 3);
}

#[test]
fn test_allocate_mem_handle_wrap_skips_zero() {
    let mut dpmi = Dpmi::new();

    dpmi.next_mem_handle = u32::MAX;
    let handle = dpmi
        .allocate_mem_handle()
        .expect("handle should be allocated");
    assert_eq!(handle, u32::MAX);
    assert_eq!(dpmi.next_mem_handle, 1);

    let handle2 = dpmi
        .allocate_mem_handle()
        .expect("second handle should be allocated");
    assert_eq!(handle2, 1);
    assert_ne!(handle2, 0);
}

#[test]
fn test_allocate_mem_handle_start_zero_adjusts() {
    let mut dpmi = Dpmi::new();

    dpmi.next_mem_handle = 0;
    let handle = dpmi
        .allocate_mem_handle()
        .expect("handle should be allocated");
    assert_eq!(handle, 1);
    assert_ne!(dpmi.next_mem_handle, 0);
}

#[test]
fn test_resize_memory_block() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate 64KB
    cpu.regs.set_ax(0x0501);
    cpu.regs.set_bx(0x0001);
    cpu.regs.set_cx(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    let handle_hi = cpu.regs.si();
    let handle_lo = cpu.regs.di();

    // Resize to 128KB
    cpu.regs.set_ax(0x0503);
    cpu.regs.set_bx(0x0002);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_si(handle_hi);
    cpu.regs.set_di(handle_lo);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let out_handle = ((cpu.regs.si() as u32) << 16) | (cpu.regs.di() as u32);
    assert!(
        dpmi.memory_blocks.iter().any(|b| b.handle == out_handle),
        "Resize should return a valid handle"
    );
}

#[test]
fn test_free_memory_block() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate
    cpu.regs.set_ax(0x0501);
    cpu.regs.set_bx(0x0001);
    cpu.regs.set_cx(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    let handle_hi = cpu.regs.si();
    let handle_lo = cpu.regs.di();

    // Free
    cpu.regs.set_ax(0x0502);
    cpu.regs.set_si(handle_hi);
    cpu.regs.set_di(handle_lo);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_free_memory_block_32bit_handle() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client

    // Allocate 64KB
    cpu.regs.set_ax(0x0501);
    cpu.regs.set_bx(0x0001);
    cpu.regs.set_cx(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Get handle from ESI (32-bit client)
    let handle = cpu.regs.esi;
    assert!(handle > 0);

    // Clear SI:DI to prove that Free reads from ESI, not SI:DI
    cpu.regs.set_si(0);
    cpu.regs.set_di(0);

    // Free using handle in ESI (B8 bug: was using SI:DI instead of ESI)
    cpu.regs.set_ax(0x0502);
    cpu.regs.esi = handle;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_resize_memory_block_32bit_handle() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client

    // Allocate 64KB
    cpu.regs.set_ax(0x0501);
    cpu.regs.set_bx(0x0001);
    cpu.regs.set_cx(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Get handle from ESI (32-bit client)
    let handle = cpu.regs.esi;
    assert!(handle > 0);

    // Clear SI:DI to prove that Resize reads from ESI, not SI:DI
    cpu.regs.set_si(0);
    cpu.regs.set_di(0);

    // Resize to 128KB using handle in ESI (B9 bug: was using SI:DI instead of ESI)
    cpu.regs.set_ax(0x0503);
    cpu.regs.set_bx(0x0002);
    cpu.regs.set_cx(0x0000);
    cpu.regs.esi = handle;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let out_handle = cpu.regs.esi;
    assert!(
        dpmi.memory_blocks.iter().any(|b| b.handle == out_handle),
        "Resize should return a valid handle"
    );
}

#[test]
fn test_free_invalid_memory_block() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Try to free non-existent block
    cpu.regs.set_ax(0x0502);
    cpu.regs.set_si(0xFFFF);
    cpu.regs.set_di(0xFFFF);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0x8023);
}

#[test]
fn test_resize_invalid_memory_block() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Try to resize non-existent block
    cpu.regs.set_ax(0x0503);
    cpu.regs.set_bx(0x0001);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_si(0xFFFF);
    cpu.regs.set_di(0xFFFF);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0x8023);
}

#[test]
fn test_allocate_linear_memory_block() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client

    // Function 0x0504 (allocate linear memory block)
    // EBX = address (0 = any), ECX = size, EDX = flags
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x8000; // 32KB
    cpu.regs.ebx = 0; // Any address
    cpu.regs.edx = 1; // Commit pages
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Should succeed
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let base = cpu.regs.ebx;
    assert!(base >= 0x100000); // Above 1MB
    let handle = cpu.regs.esi;
    assert!(handle > 0);

    // Free it
    cpu.regs.set_ax(0x0502);
    cpu.regs.esi = handle;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_allocate_linear_memory_block_rejects_unaligned_address() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000; // 8KB
    cpu.regs.ebx = 0x0010_0001; // Unaligned address
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_allocate_linear_memory_block_rejects_base_beyond_ram() {
    // MDD-BUG-CRUCIBLE-00055: a 0504h request whose base+size overflows or exceeds
    // DOS_RAM_CEILING must be rejected, not handed a handle based beyond the RAM
    // ceiling (the memory layer clamps growth via ensure_ram_size rather than
    // rejecting).
    let max = gp64_memory::DOS_RAM_CEILING as u32;
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1; // 32-bit client

    // Base one page below the ceiling with two pages of size -> end overshoots.
    cpu.regs.set_ax(0x0504);
    cpu.regs.ebx = max - 0x1000;
    cpu.regs.ecx = 0x2000;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_LINEAR);

    // A base well beyond the ceiling.
    cpu.regs.set_ax(0x0504);
    cpu.regs.ebx = max + 0x10000;
    cpu.regs.ecx = 0x1000;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_LINEAR);
}

#[test]
fn test_allocate_linear_memory_block_accepts_up_to_ram_ceiling() {
    // MDD-BUG-CRUCIBLE-00055 boundary: a block ending exactly at DOS_RAM_CEILING is
    // valid and must still be accepted (guards the <= boundary against off-by-one).
    let max = gp64_memory::DOS_RAM_CEILING as u32;
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1; // 32-bit client

    cpu.regs.set_ax(0x0504);
    cpu.regs.ebx = max - 0x1000; // last page
    cpu.regs.ecx = 0x1000; // end == DOS_RAM_CEILING
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "a block ending exactly at DOS_RAM_CEILING must be allowed, ax={:#06X}",
        cpu.regs.ax()
    );
}

#[test]
fn test_allocate_linear_0504_supports_block_above_32_mib() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    cpu.regs.set_ax(0x0504);
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 33 * 1024 * 1024;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "33 MiB is below MAX_RAM_SIZE and must not overflow commit tracking"
    );
    let block = dpmi
        .linear_memory_blocks
        .iter()
        .find(|block| block.handle == cpu.regs.esi)
        .unwrap();
    assert_eq!(block.size, 33 * 1024 * 1024);
    assert_eq!(block.page_count, 33 * 1024 * 1024 / 0x1000);
}

#[test]
fn test_resize_linear_0505_grows_above_32_mib_in_place() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    cpu.regs.set_ax(0x0504);
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 32 * 1024 * 1024;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;
    let old_base = cpu.regs.ebx;

    cpu.regs.set_ax(0x0505);
    cpu.regs.esi = handle;
    cpu.regs.ecx = 33 * 1024 * 1024;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ebx, old_base);
    let block = dpmi
        .linear_memory_blocks
        .iter()
        .find(|block| block.handle == handle)
        .unwrap();
    assert_eq!(block.size, 33 * 1024 * 1024);
}

#[test]
fn test_resize_linear_0505_moves_block_when_growing_above_32_mib() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    cpu.regs.set_ax(0x0504);
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 32 * 1024 * 1024;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;
    let old_base = cpu.regs.ebx;

    cpu.regs.set_ax(0x0504);
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 0x1000;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ebx, old_base + 32 * 1024 * 1024);

    cpu.regs.set_ax(0x0505);
    cpu.regs.esi = handle;
    cpu.regs.ecx = 33 * 1024 * 1024;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_ne!(cpu.regs.ebx, old_base);
    let block = dpmi
        .linear_memory_blocks
        .iter()
        .find(|block| block.handle == handle)
        .unwrap();
    assert_eq!(block.base, cpu.regs.ebx);
    assert_eq!(block.size, 33 * 1024 * 1024);
}

/// MDD-BUG-CRUCIBLE-00649: a relocating 0505h copied every committed page a
/// byte at a time. Committed pages are now coalesced into spans and moved
/// with `Memory::bulk_copy_ram`. This pins the observable contract that
/// change must not alter: committed bytes arrive at the new base, and
/// uncommitted pages stay holes rather than being copied.
#[test]
fn test_resize_linear_0505_moves_mixed_committed_runs_and_preserves_holes() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    // An 8-page block, allocated committed.
    cpu.regs.set_ax(0x0504);
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 8 * 0x1000;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;
    let old_base = cpu.regs.ebx;

    // A second block lands immediately after, so growing the first cannot
    // be satisfied in place and must relocate.
    cpu.regs.set_ax(0x0504);
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 0x1000;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ebx, old_base + 8 * 0x1000);

    // Decommit pages 2 and 5, leaving three runs: [0,1] [3,4] [6,7].
    // The span walk must emit exactly those and skip the holes.
    {
        let idx = dpmi
            .linear_memory_blocks
            .iter()
            .position(|b| b.handle == handle)
            .unwrap();
        for page in 0..8 {
            dpmi.linear_memory_blocks[idx].set_page_committed(page, true);
        }
        dpmi.linear_memory_blocks[idx].set_page_committed(2, false);
        dpmi.linear_memory_blocks[idx].set_page_committed(5, false);
    }

    // Stamp every page with its index, holes included, so a hole that WAS
    // copied would be detectable at the destination.
    memory.ensure_ram_size((old_base as usize) + 8 * 0x1000);
    for page in 0..8usize {
        let at = old_base as usize + page * 0x1000;
        memory.write_u8(at, 0xA0 + page as u8);
        memory.write_u8(at + 0x0FFF, 0xB0 + page as u8);
    }

    // Grow, which forces a move to a fresh base.
    cpu.regs.set_ax(0x0505);
    cpu.regs.esi = handle;
    cpu.regs.ecx = 16 * 0x1000;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let new_base = cpu.regs.ebx;
    assert_ne!(new_base, old_base, "block should have moved");

    for page in [0usize, 1, 3, 4, 6, 7] {
        let at = new_base as usize + page * 0x1000;
        assert_eq!(
            memory.read_u8(at),
            0xA0 + page as u8,
            "committed page {page} first byte did not move"
        );
        assert_eq!(
            memory.read_u8(at + 0x0FFF),
            0xB0 + page as u8,
            "committed page {page} last byte did not move — the span stopped short"
        );
    }

    for page in [2usize, 5] {
        let at = new_base as usize + page * 0x1000;
        assert_eq!(
            memory.read_u8(at),
            0,
            "uncommitted page {page} must stay a hole, not be copied"
        );
    }
}

#[test]
fn test_resize_linear_0505_rejects_above_max_ram_atomically() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    cpu.regs.set_ax(0x0504);
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 0x2000;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;
    let block = dpmi
        .linear_memory_blocks
        .iter()
        .find(|block| block.handle == handle)
        .unwrap();
    let before = (
        block.base,
        block.size,
        block.page_count,
        block.committed.len(),
    );

    cpu.regs.set_ax(0x0505);
    cpu.regs.esi = handle;
    cpu.regs.ecx = DOS_RAM_CEILING as u32 + 0x1000;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_LINEAR_UNAVAILABLE);
    let block = dpmi
        .linear_memory_blocks
        .iter()
        .find(|block| block.handle == handle)
        .unwrap();
    assert_eq!(
        (
            block.base,
            block.size,
            block.page_count,
            block.committed.len()
        ),
        before
    );
}

#[test]
fn test_get_page_attributes() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client

    // First allocate a linear memory block
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x4000; // 16KB (4 pages)
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1; // Commit pages
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    // Get page attributes (0x0506)
    // Set up buffer at ES:EDX
    cpu.regs.es = 0x1000;
    cpu.regs.edx = 0;
    cpu.regs.set_ax(0x0506);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0; // Offset in block
    cpu.regs.ecx = 2; // 2 pages
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // Check attributes in buffer - pages should be committed (type=1)
    let attr0 = memory.read_u16(0x10000);
    let attr1 = memory.read_u16(0x10002);
    assert_eq!(attr0, 0x0001);
    assert_eq!(attr1, 0x0001);
}

#[test]
fn test_get_page_attributes_unaligned_base_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handle = cpu.regs.esi;

    cpu.regs.es = 0x1000;
    cpu.regs.edx = 0;
    cpu.regs.set_ax(0x0506);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 1;
    cpu.regs.ecx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_get_page_attributes_invalid_buffer_selector_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handle = cpu.regs.esi;

    dpmi.protected_mode = true;
    cpu.regs.es = 0x0007; // LDT selector without entry
    cpu.regs.edx = 0;
    cpu.regs.set_ax(0x0506);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_get_page_attributes_range_exceeds_block_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handle = cpu.regs.esi;

    cpu.regs.es = 0x1000;
    cpu.regs.edx = 0;
    cpu.regs.set_ax(0x0506);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 3;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}
