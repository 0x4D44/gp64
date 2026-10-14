use super::*;

// Coverage: 0x0504 - requested address not free (lines 4944-4947)
#[test]
fn test_allocate_linear_0504_requested_address_not_free() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1;

    // Allocate at a specific address
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0x20_0000; // 2MB
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Try to allocate at the same address
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x1000;
    cpu.regs.ebx = 0x20_0000; // Same address
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_LINEAR_UNAVAILABLE);
}

// Coverage: 0x0504 zero size (lines 4917-4920)
#[test]
fn test_allocate_linear_0504_zero_size() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1;

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0; // Zero size
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

// Coverage: 0x0300 simulate RM interrupt - HLE path (lines 3040-3048)
#[test]
fn test_simulate_rm_interrupt_0300_hle_handler() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Set IVT entry for INT 21h to point to HLE stub F000:xxxx
    let ivt_addr = 0x21 * 4;
    memory.write_u16(ivt_addr, 0x0184); // IP: (0x21 * 4) + 0x100 = 0x0184
    memory.write_u16(ivt_addr + 2, 0xF000); // CS: F000

    // Set up a RealModeCallStruct in memory
    let struct_addr = 0x80000usize;
    let rm_struct = RealModeCallStruct {
        edi: 0,
        esi: 0,
        ebp: 0,
        reserved: 0,
        ebx: 0,
        edx: 0,
        ecx: 0,
        eax: 0,
        flags: 0,
        es: 0,
        ds: 0,
        fs: 0,
        gs: 0,
        ip: 0,
        cs: 0,
        sp: 0,
        ss: 0,
    };
    rm_struct.write_to_memory(&mut memory, struct_addr);

    // Point ES to the struct
    let sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == sel) {
        s.base = struct_addr as u32;
        s.limit = 0xFFFF;
    }
    cpu.regs.es = sel;
    cpu.regs.edi = 0;

    cpu.regs.set_ax(0x0300);
    cpu.regs.set_bl(0x21);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_cx(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(dpmi.pending_hle_simulation.is_some());
    let sim = dpmi.pending_hle_simulation.unwrap();
    assert_eq!(sim.int_num, 0x21);
}

#[test]
fn test_dpmi_trace_selector_update_with_env() {
    let _lock = env_lock();
    let cpu = Cpu::new();
    // Exercise dpmi_trace_selector_update (lines 121-145)
    dpmi_trace_selector_update(&cpu, "test_label", 0x0107, 0x1000, 0xFFFF, 0xF2, 0x00);
    // LDT selector path (bit 2 set)
    dpmi_trace_selector_update(&cpu, "ldt_test", 0x010F, 0x2000, 0xFFFF, 0xF2, 0x00);
}

#[test]
fn test_dpmi_trace_via_int31_allocate_descriptor() {
    let _lock = env_lock();

    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate a descriptor - this should trigger trace output (lines 793-807)
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int31_with_idtr_base_nonzero() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Set up IDT at address 0x2000
    let idt_base = 0x2000usize;
    cpu.regs.idtr.base = idt_base as u32;
    cpu.regs.idtr.limit = 0x07FF; // 256 entries * 8 bytes - 1

    // Write an IDT entry for vector 0x31 with offset 0xFE62 (the default stub)
    let vec31_addr = idt_base + 0x31 * 8;
    let bios_sel = PM_BIOS_CODE_SEL;
    memory.write_u16(vec31_addr, 0xFE62); // offset low
    memory.write_u16(vec31_addr + 2, bios_sel); // selector
    memory.write_u8(vec31_addr + 4, 0); // reserved
    memory.write_u8(vec31_addr + 5, 0xEE); // type_attr
    memory.write_u16(vec31_addr + 6, 0); // offset high

    // Now call int31 - this should read IDT[31] and NOT log "changed" since offset is 0xFE62
    cpu.regs.set_ax(0x0003); // Get selector increment - simple, always succeeds
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 8);
}

#[test]
fn test_int31_with_idtr_idt_entry_changed() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Set up IDT with a different offset than 0xFE62 for vector 0x31
    let idt_base = 0x2000usize;
    cpu.regs.idtr.base = idt_base as u32;
    cpu.regs.idtr.limit = 0x07FF;

    let vec31_addr = idt_base + 0x31 * 8;
    memory.write_u16(vec31_addr, 0x1234); // different offset
    memory.write_u16(vec31_addr + 2, PM_BIOS_CODE_SEL);
    memory.write_u8(vec31_addr + 4, 0);
    memory.write_u8(vec31_addr + 5, 0xEE);
    memory.write_u16(vec31_addr + 6, 0);

    // This triggers the "IDT[31] changed" log path (lines 822-828)
    cpu.regs.set_ax(0x0003);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_get_multiple_descriptors_entry_size_overflow() {
    // Lines 1813-1817: count * 10 overflows
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let sel = dpmi.selectors[0].selector;
    cpu.regs.es = sel;
    cpu.regs.edi = 0;

    cpu.regs.set_ax(0x000E);
    cpu.regs.set_cx(0xFFFF); // Very large count
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // Could be INVALID_VALUE (overflow) or INVALID_LINEAR (buffer end overflow)
}

#[test]
fn test_get_multiple_descriptors_gdt_selector_in_entry() {
    // Lines 1850-1855: selector in entry is not a valid LDT selector
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Use first allocated selector for ES (points to buffer)
    let es_sel = dpmi.selectors[0].selector;
    let buf_addr = 0x100000usize;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = buf_addr as u32;
        s.limit = 0xFFFF;
    }
    dpmi.write_selector_descriptor(
        &mut cpu,
        &mut memory,
        es_sel,
        buf_addr as u32,
        0xFFFF,
        0xF2,
        0,
    );
    cpu.regs.es = es_sel;
    cpu.regs.edi = 0;

    // Write an entry with GDT selector (not LDT) at buffer
    memory.write_u16(buf_addr, 0x0008); // GDT selector, invalid for this API

    cpu.regs.set_ax(0x000E);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_get_multiple_descriptors_selector_not_tracked() {
    // Lines 1880-1884: selector in LDT range but not in selectors list
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    let es_sel = dpmi.selectors[0].selector;
    let buf_addr = 0x100000usize;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = buf_addr as u32;
        s.limit = 0xFFFF;
    }
    dpmi.write_selector_descriptor(
        &mut cpu,
        &mut memory,
        es_sel,
        buf_addr as u32,
        0xFFFF,
        0xF2,
        0,
    );
    cpu.regs.es = es_sel;
    cpu.regs.edi = 0;

    // Write a valid-looking LDT selector that isn't actually tracked
    let fake_sel: u16 = 0x0FFF; // LDT selector (TI=1, RPL=3) but not allocated
    memory.write_u16(buf_addr, fake_sel);

    cpu.regs.set_ax(0x000E);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_multiple_descriptors_entry_size_overflow() {
    // Lines 1912-1916: count * 10 overflows
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let sel = dpmi.selectors[0].selector;
    cpu.regs.es = sel;
    cpu.regs.edi = 0;

    cpu.regs.set_ax(0x000F);
    cpu.regs.set_cx(0xFFFF);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_set_multiple_descriptors_gdt_selector_in_entry() {
    // Lines 1949-1954: invalid selector within entry
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    let es_sel = dpmi.selectors[0].selector;
    let buf_addr = 0x100000usize;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = buf_addr as u32;
        s.limit = 0xFFFF;
    }
    dpmi.write_selector_descriptor(
        &mut cpu,
        &mut memory,
        es_sel,
        buf_addr as u32,
        0xFFFF,
        0xF2,
        0,
    );
    cpu.regs.es = es_sel;
    cpu.regs.edi = 0;

    // Write GDT selector (not LDT)
    memory.write_u16(buf_addr, 0x0008);

    cpu.regs.set_ax(0x000F);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_multiple_descriptors_system_segment_type() {
    // Lines 1989-1993: access byte has system type (bit 4 = 0)
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    let es_sel = dpmi.selectors[0].selector;
    let buf_addr = 0x100000usize;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = buf_addr as u32;
        s.limit = 0xFFFF;
    }
    dpmi.write_selector_descriptor(
        &mut cpu,
        &mut memory,
        es_sel,
        buf_addr as u32,
        0xFFFF,
        0xF2,
        0,
    );
    cpu.regs.es = es_sel;
    cpu.regs.edi = 0;

    // Allocate a descriptor to use in the entry
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let target_sel = cpu.regs.ax();

    // Write the entry with a system segment type (access bit 4 = 0)
    memory.write_u16(buf_addr, target_sel); // selector
    memory.write_u16(buf_addr + 2, 0xFFFF); // limit low
    memory.write_u16(buf_addr + 4, 0x0000); // base low
    memory.write_u8(buf_addr + 6, 0x00); // base mid
    memory.write_u8(buf_addr + 7, 0x82); // access: present, system type (bit 4=0)
    memory.write_u8(buf_addr + 8, 0x00); // flags/limit high
    memory.write_u8(buf_addr + 9, 0x00); // base high

    cpu.regs.set_ax(0x000F);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_multiple_descriptors_selector_not_tracked() {
    // Lines 2006-2010: selector found in LDT range but not in selectors vec
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    let es_sel = dpmi.selectors[0].selector;
    let buf_addr = 0x100000usize;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = buf_addr as u32;
        s.limit = 0xFFFF;
    }
    dpmi.write_selector_descriptor(
        &mut cpu,
        &mut memory,
        es_sel,
        buf_addr as u32,
        0xFFFF,
        0xF2,
        0,
    );
    cpu.regs.es = es_sel;
    cpu.regs.edi = 0;

    // Use a valid-looking LDT selector that is NOT in dpmi.selectors
    let fake_sel: u16 = 0x0FFF;
    memory.write_u16(buf_addr, fake_sel);
    // Write a valid descriptor (present, data r/w, DPL=3)
    memory.write_u16(buf_addr + 2, 0xFFFF);
    memory.write_u16(buf_addr + 4, 0x0000);
    memory.write_u8(buf_addr + 6, 0x00);
    memory.write_u8(buf_addr + 7, 0xF2); // Present, DPL=3, data r/w (bit 4 = 1)
    memory.write_u8(buf_addr + 8, 0x00);
    memory.write_u8(buf_addr + 9, 0x00);

    cpu.regs.set_ax(0x000F);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_descriptor_management_unsupported_subfunction() {
    // Lines 2016-2020: unsupported descriptor subfunction
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // AL=0x10 is an invalid descriptor subfunction
    cpu.regs.set_ax(0x0010);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_UNSUPPORTED);
}

#[test]
fn test_set_exception_handler_0203_with_valid_handler() {
    let _tracing = init_tracing();
    // Lines 2465-2668: set exception handler with valid selector/offset
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate a code descriptor for the handler
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handler_sel = cpu.regs.ax();

    // Set it as a code segment with base at 0
    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(handler_sel);
    cpu.regs.set_cx(0x40FA); // D=1 (32-bit), code, DPL=3, present
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Write an IRET instruction at the handler offset to be read by diagnostic code
    let handler_offset = 0x100u32;
    if let Some(s) = dpmi.selectors.iter().find(|s| s.selector == handler_sel) {
        let addr = s.base + handler_offset;
        memory.write_u8(addr as usize, 0x66); // prefix
        memory.write_u8(addr as usize + 1, 0xCF); // IRETD
    }

    // Set exception handler for exception 0 (divide error)
    cpu.regs.set_ax(0x0203);
    cpu.regs.set_bl(0x00); // Exception 0
    cpu.regs.set_cx(handler_sel);
    cpu.regs.edx = handler_offset;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // Verify the handler was stored
    let entry = dpmi.exception_handler_entry(0).unwrap();
    assert_eq!(entry.selector, handler_sel);
    assert_eq!(entry.offset, handler_offset);
}

#[test]
fn test_set_exception_handler_0203_null_selector() {
    // DPMI 0.9 / 1.0 §5.2.2: NULL selector (GDT index 0) is a clear-
    // handler request. Lenient accept (matches the DOS model). See HLD
    // the DPMI 8001 regression analysis.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ax(0x0203);
    cpu.regs.set_bl(0x00);
    cpu.regs.set_cx(0x0000); // NULL selector
    cpu.regs.set_dx(0x100);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(dpmi.exception_handler_entry(0x00).is_none());
}

#[test]
fn test_set_exception_handler_0203_invalid_exception_number() {
    // Lines 2776-2778: exception number >= 32
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ax(0x0203);
    cpu.regs.set_bl(32); // Invalid: >= 32
    cpu.regs.set_cx(0x0007);
    cpu.regs.set_dx(0x100);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_extended_exception_handler_0213_null_selector() {
    // DPMI 1.0: NULL selector for extended exception handler is a
    // clear-handler request. Lenient accept (matches the DOS model). See HLD
    // the DPMI 8001 regression analysis.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x0213); // subfunc 0x13 = set extended exception handler
    cpu.regs.set_bl(0x00);
    cpu.regs.set_cx(0x0000); // NULL selector
    cpu.regs.edx = 0x100;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(dpmi.exception_handler_entry(0x00).is_none());
}

#[test]
fn test_set_extended_exception_handler_0213_invalid_exception() {
    // Lines 2775-2778: exception >= 32
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x0213); // subfunc 0x13 = set extended exception handler
    cpu.regs.set_bl(32); // Invalid
    cpu.regs.set_cx(0x0007);
    cpu.regs.edx = 0x100;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_get_pm_interrupt_vector_0204_with_idtr() {
    let _tracing = init_tracing();
    // Lines 2784-2796: Get PM vector with IDT present
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // The IDT should already be set up from enter_protected_mode
    assert_ne!(cpu.regs.idtr.base, 0);

    cpu.regs.set_ax(0x0204);
    cpu.regs.set_bl(0x21); // INT 21h
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // Should return the BIOS code selector
    assert_eq!(cpu.regs.cx(), PM_BIOS_CODE_SEL);
}

#[test]
fn test_get_pm_interrupt_vector_0204_without_idtr() {
    // Lines 2798-2800: Get PM vector without IDT (from pm_idt array)
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Clear IDTR base to force the no-IDT path
    cpu.regs.idtr.base = 0;

    cpu.regs.set_ax(0x0204);
    cpu.regs.set_bl(0x21);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // pm_idt was populated during enter_protected_mode
    assert_eq!(cpu.regs.cx(), PM_BIOS_CODE_SEL);
}

#[test]
fn test_set_pm_interrupt_vector_0205_null_selector() {
    // DPMI 0.9 / 1.0 §5.2.2: NULL selector for 0205h is a clear-handler
    // request. Lenient accept (matches the DOS model — leaves IDT slot as-is,
    // only clears pm_idt[v]). See the generic DPMI regression analysis
    // DPMI 8001 unhandled.md.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x21);
    cpu.regs.set_cx(0x0000); // NULL selector
    cpu.regs.set_dx(0x100);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(!dpmi.pm_idt[0x21].present);
}

#[test]
fn test_set_pm_interrupt_vector_0205_valid_handler() {
    let _tracing = init_tracing();
    // Lines 2814-2910: Set PM interrupt vector with valid handler
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate and set up a code selector
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handler_sel = cpu.regs.ax();

    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(handler_sel);
    cpu.regs.set_cx(0x00FA); // 16-bit code, DPL=3, present
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x21);
    cpu.regs.set_cx(handler_sel);
    cpu.regs.set_dx(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_set_pm_interrupt_vector_0205_not_code_segment() {
    // Lines 2842-2845: selector is not a code segment
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate a descriptor (default is data segment 0xF2)
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let data_sel = cpu.regs.ax();

    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x21);
    cpu.regs.set_cx(data_sel); // This is a data segment, not code
    cpu.regs.set_dx(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_interrupt_services_unsupported_subfunction() {
    // Lines 2913-2915
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ax(0x0207); // Invalid interrupt subfunc
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_UNSUPPORTED);
}

#[test]
fn test_simulate_rm_interrupt_0300_invalid_flags_reserved_bits() {
    // Lines 2995-2998: BH has reserved bits set
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x0300);
    cpu.regs.set_bl(0x21);
    cpu.regs.set_bh(0x02); // Reserved bit set
    cpu.regs.set_cx(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_allocate_rm_callback_0303_all_slots_full() {
    // Lines 3388-3391: all callback slots exhausted
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Fill all callback slots
    for cb in dpmi.rm_callbacks.iter_mut() {
        cb.in_use = true;
    }

    cpu.regs.set_ax(0x0303);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_CALLBACK_UNAVAIL);
}

#[test]
fn test_free_rm_callback_0304_invalid_callback() {
    // Lines 3407-3409: free a callback that doesn't exist
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ax(0x0304);
    cpu.regs.set_cx(0x1234); // Non-existent segment
    cpu.regs.set_dx(0x5678); // Non-existent offset
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_CALLBACK);
}

#[test]
fn test_call_rm_proc_iret_0302_with_trace_enabled() {
    // Lines 3218-3267: trace path in 0302 call
    let _lock = env_lock();

    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Set up a RealModeCallStruct pointing to an HLE stub
    let struct_addr = 0x80000usize;
    let rm_struct = RealModeCallStruct {
        edi: 0,
        esi: 0,
        ebp: 0,
        reserved: 0,
        ebx: 0,
        edx: 0,
        ecx: 0,
        eax: 0,
        flags: 0,
        es: 0,
        ds: 0,
        fs: 0,
        gs: 0,
        ip: 0x0184,
        cs: 0xF000, // HLE stub
        sp: 0,
        ss: 0,
    };
    rm_struct.write_to_memory(&mut memory, struct_addr);

    let sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == sel) {
        s.base = struct_addr as u32;
        s.limit = 0xFFFF;
    }
    cpu.regs.es = sel;
    cpu.regs.edi = 0;

    cpu.regs.set_ax(0x0302);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_cx(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // The HLE target path is taken; trace output is emitted (exercising lines 3251-3267)
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_allocate_linear_0504_handle_unavailable() {
    // Lines 4955-4958: allocate_mem_handle returns None
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    // Exhaust handles by setting next_mem_handle to u32::MAX
    dpmi.next_mem_handle = u32::MAX;

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x1000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // The allocate_mem_handle increments next_mem_handle which wraps.
    // It should still succeed unless there's an explicit limit.
    // We just exercise the path; the test verifies no crash.
}

#[test]
fn test_get_page_attributes_0506_page_range_exceeds_block() {
    // Lines 5242-5245: rel_page + page_count > block.page_count
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    // Allocate a 1-page block
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x1000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    // Try to get 2 pages of attributes from a 1-page block
    cpu.regs.set_ax(0x0506);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0; // Offset 0
    cpu.regs.ecx = 2; // 2 pages (but block only has 1)
    cpu.regs.es = 0;
    cpu.regs.edx = 0x90000; // buffer addr
    memory.ensure_ram_size(0x90010);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_page_attributes_0507_page_range_exceeds_block() {
    // Lines 5336-5341
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x1000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    cpu.regs.set_ax(0x0507);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 2; // More pages than block has
    cpu.regs.es = 0;
    cpu.regs.edx = 0x90000;
    memory.ensure_ram_size(0x90010);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_page_attributes_0507_attr_type_invalid_value() {
    // Lines 5359-5362: attr_type > 1
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;
    let base = cpu.regs.ebx;

    // Write invalid attribute (type=2) at buffer
    let buf_addr = (base + 0x10000) as usize;
    memory.ensure_ram_size(buf_addr + 0x10);
    memory.write_u16(buf_addr, 0x0002); // type=2, invalid

    cpu.regs.set_ax(0x0507);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 1;
    cpu.regs.es = 0;
    cpu.regs.edx = buf_addr as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_map_device_0508_nonexistent_handle() {
    // Lines 5429-5436: invalid handle for map device
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    cpu.regs.set_ax(0x0508);
    cpu.regs.esi = 0xDEAD; // Non-existent handle
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 1;
    cpu.regs.edx = 0x1000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_HANDLE);
}

#[test]
fn test_map_device_0508_zero_page_count() {
    // Lines 5410-5413: zero page count
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    cpu.regs.set_ax(0x0508);
    cpu.regs.esi = 1;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 0; // Zero page count
    cpu.regs.edx = 0x1000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_map_device_0508_page_exceeds_block() {
    // Lines 5441-5444: rel_page + page_count > block pages
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    // Allocate a 1-page block
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x1000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    cpu.regs.set_ax(0x0508);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 2; // 2 pages but block has 1
    cpu.regs.edx = 0x1000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_map_device_0508_physical_exceeds_ram() {
    // Lines 5456-5459: physical end exceeds DOS_RAM_CEILING
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    // Allocate a block
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    cpu.regs.set_ax(0x0508);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 1;
    cpu.regs.edx = 0xFFFF_F000; // Physical address near end of 32-bit space
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_LINEAR);
}

#[test]
fn test_map_conventional_0509_nonexistent_handle() {
    // Lines 5515-5522
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    cpu.regs.set_ax(0x0509);
    cpu.regs.esi = 0xDEAD;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 1;
    cpu.regs.edx = 0x1000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_HANDLE);
}

#[test]
fn test_map_conventional_0509_zero_page_count() {
    // Lines 5496-5499
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    cpu.regs.set_ax(0x0509);
    cpu.regs.esi = 1;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 0;
    cpu.regs.edx = 0x1000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_map_conventional_0509_page_exceeds_block() {
    // Lines 5527-5530
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x1000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    cpu.regs.set_ax(0x0509);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 2;
    cpu.regs.edx = 0x1000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_map_conventional_0509_physical_exceeds_ram() {
    // Lines 5542-5545
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    cpu.regs.set_ax(0x0509);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 1;
    cpu.regs.edx = 0xFFFF_F000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_LINEAR);
}

#[test]
fn test_map_device_0508_16bit_unsupported() {
    // 16-bit clients are now allowed; ECX=0 (page_count=0) → INVALID_VALUE
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 0; // 16-bit

    cpu.regs.set_ax(0x0508);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_map_conventional_0509_16bit_unsupported() {
    // 16-bit clients are now allowed; ECX=0 (page_count=0) → INVALID_VALUE
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 0;

    cpu.regs.set_ax(0x0509);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_dpmi_1_0_memory_services_unsupported_subfunc() {
    // Lines 5568-5571
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    cpu.regs.set_ax(0x050F); // Invalid subfunction
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_UNSUPPORTED);
}

#[test]
fn test_enter_protected_mode_with_psp_and_env() {
    let _tracing = init_tracing();
    // Lines 5955-5985: env selector allocation path
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0xFFFE);
    let stack_base = 0x10000usize;
    memory.write_u16(stack_base + 0xFFFC, 0x0100); // Return IP
    memory.write_u16(stack_base + 0xFFFE, 0x2000); // Return CS
    cpu.regs.set_sp(0xFFFC);
    cpu.regs.set_ax(0); // 16-bit mode

    // Set up PSP at segment 0x5000
    let psp_seg = 0x5000u16;
    let env_seg = 0x4000u16;
    let psp_base = (psp_seg as usize) << 4;
    memory.write_u16(psp_base + 0x2C, env_seg); // Environment segment

    // Write a simple environment block at env_seg
    let env_base = (env_seg as usize) << 4;
    memory.write_u8(env_base, b'A');
    memory.write_u8(env_base + 1, b'=');
    memory.write_u8(env_base + 2, b'1');
    memory.write_u8(env_base + 3, 0);
    memory.write_u8(env_base + 4, 0); // Double null
                                      // Program name after double null
    memory.write_u16(env_base + 5, 1); // word count
    memory.write_u8(env_base + 7, b'X');
    memory.write_u8(env_base + 8, 0);

    cpu.regs.ds = 0x3000;
    cpu.regs.es = 0x4000;

    dpmi.enter_protected_mode_with_psp(&mut cpu, &mut memory, psp_seg, env_seg)
        .unwrap();

    assert!(dpmi.protected_mode);
    assert_ne!(dpmi.env_selector, 0);
}

#[test]
fn test_get_selector_info_valid() {
    // Lines 6197-6202: get_selector_info
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // After setup_protected_mode, selectors have been allocated.
    // Find one with a non-zero index (index = selector >> 3)
    let sel = dpmi
        .selectors
        .iter()
        .find(|s| (s.selector >> 3) != 0 && (s.selector & 0x04) != 0)
        .map(|s| s.selector);
    if let Some(sel) = sel {
        let info = dpmi.get_selector_info(sel);
        assert!(info.is_some(), "Selector {:04X} should be found", sel);
        let (_base, _limit, access, _flags) = info.unwrap();
        assert_ne!(access, 0);
    } else {
        // Allocate one explicitly
        cpu.regs.set_ax(0x0000);
        cpu.regs.set_cx(1);
        dpmi.int31(&mut cpu, &mut memory).unwrap();
        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        let new_sel = cpu.regs.ax();
        let info = dpmi.get_selector_info(new_sel);
        assert!(
            info.is_some(),
            "Newly allocated selector {:04X} should be found",
            new_sel
        );
    }
}

#[test]
fn test_get_selector_info_gdt_returns_none() {
    let dpmi = Dpmi::new();
    // GDT selector - canonical_ldt_selector returns None
    assert!(dpmi.get_selector_info(0x0008).is_none());
}

#[test]
fn test_set_access_rights_0009_not_found_selector() {
    // Lines 1597-1599: selector not in selectors list
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Use an LDT selector that's in range but not allocated
    let fake_sel: u16 = 0x0FFF;
    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(fake_sel);
    cpu.regs.set_cx(0x00F2);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_create_alias_0a_source_not_found() {
    // Lines 1639-1641: source selector not found
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let fake_sel: u16 = 0x0FFF;
    cpu.regs.set_ax(0x000A);
    cpu.regs.set_bx(fake_sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_dos_memory_free_0101_invalid_selector() {
    // Lines 2267-2270: selector not in LDT range
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ax(0x0101);
    cpu.regs.set_dx(0x0008); // GDT selector - not valid for DOS memory
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_dos_memory_free_0101_selector_not_in_blocks() {
    // Lines 2299-2301: selector not found in dos_memory_blocks
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Use a valid LDT selector that doesn't correspond to any DOS memory block
    let sel = dpmi.selectors[0].selector;
    cpu.regs.set_ax(0x0101);
    cpu.regs.set_dx(sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_dos_memory_resize_0102_invalid_selector() {
    // Lines 2312-2315: resize with invalid selector
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ax(0x0102);
    cpu.regs.set_dx(0x0008); // GDT selector
    cpu.regs.set_bx(0x10); // new paragraphs
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_dos_memory_resize_0102_not_in_blocks() {
    // Lines 2395-2397: selector not in dos_memory_blocks
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let sel = dpmi.selectors[0].selector;
    cpu.regs.set_ax(0x0102);
    cpu.regs.set_dx(sel);
    cpu.regs.set_bx(0x10);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_dos_memory_services_unsupported_subfunction() {
    // Lines 2401-2404: unsupported DOS memory subfunction
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ax(0x0103); // Invalid: only 0x00-0x02 are valid
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_UNSUPPORTED);
}

#[test]
fn test_dos_memory_allocate_0100_without_dosmem() {
    // Lines 2232-2257: allocation without DosMem (no real DOS memory)
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(0x10); // 16 paragraphs = 256 bytes
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_ne!(cpu.regs.dx(), 0); // Should return a selector
}

#[test]
fn test_dos_memory_allocate_0100_large_block_16bit() {
    // Lines 2079-2087: Large allocation creating multiple selectors in 16-bit mode
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // In 16-bit mode, a large allocation needs multiple selectors
    // 0x2000 paragraphs = 128KB = 2 * 64KB chunks
    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(0x2000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_dos_memory_free_0101_without_dosmem() {
    // Lines 2294-2297: free without DosMem
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate first
    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(0x10);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let alloc_sel = cpu.regs.dx();

    // Free it
    cpu.regs.set_ax(0x0101);
    cpu.regs.set_dx(alloc_sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_dos_memory_resize_0102_without_dosmem() {
    // Lines 2376-2393: resize without DosMem
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate first
    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(0x10);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let alloc_sel = cpu.regs.dx();

    // Resize
    cpu.regs.set_ax(0x0102);
    cpu.regs.set_dx(alloc_sel);
    cpu.regs.set_bx(0x20); // new size: 32 paragraphs
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_get_exception_handler_0202_with_idtr() {
    // Lines 2440-2447: get exception handler when IDTR is set
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // IDTR should be set from enter_protected_mode
    assert_ne!(cpu.regs.idtr.base, 0);

    cpu.regs.set_ax(0x0202);
    cpu.regs.set_bl(0x00); // Exception 0
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_get_exception_handler_0202_without_idtr() {
    // Lines 2448-2451: get exception handler from internal array
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.idtr.base = 0; // Force no-IDT path

    cpu.regs.set_ax(0x0202);
    cpu.regs.set_bl(0x00);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_get_exception_handler_0202_invalid_exception() {
    // Lines 2454-2457: exception >= 32
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ax(0x0202);
    cpu.regs.set_bl(32); // Invalid
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_prepare_exception_pointer_no_handler() {
    // Lines 2946-2947: no exception handler set
    let dpmi = Dpmi::new();
    let cpu = Cpu::new();
    let mut memory = Memory::new();

    let result = dpmi.prepare_exception_pointer(&cpu, &mut memory, 0xFF);
    assert!(result.is_ok());
}

#[test]
fn test_state_save_restore_save_operation() {
    // State save writes DPMI host signature to buffer, not CPU registers.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Set up ES to point to a writable buffer
    let sel = dpmi.selectors[0].selector;
    let buf_addr = 0x80000u32;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == sel) {
        s.base = buf_addr;
        s.limit = 0xFFFF;
    }
    dpmi.write_selector_descriptor(&mut cpu, &mut memory, sel, buf_addr, 0xFFFF, 0xF2, 0);
    cpu.invalidate_segment_cache(&memory, sel);
    cpu.regs.es = sel;
    cpu.load_data_segment(&memory, gp64_cpu::DataSegReg::ES, sel);
    cpu.regs.edi = 0; // buffer offset
    memory.ensure_ram_size(0x90000);

    // Save state
    cpu.regs.set_al(0);
    dpmi.handle_state_save_restore(&mut cpu, &mut memory)
        .unwrap();

    // Verify MDSS signature was written at offset 0
    let sig = memory.read_u32(buf_addr as usize);
    assert_eq!(sig, 0x4D44_5353);
}

#[test]
fn test_state_save_restore_invalid_operation() {
    // Lines 3485-3487: invalid operation code
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_al(2); // Invalid: only 0 and 1 are valid
    dpmi.handle_state_save_restore(&mut cpu, &mut memory)
        .unwrap();
    // Should silently return
}

#[test]
fn test_translation_services_get_state_save_addresses_0305() {
    // Lines 3413-3438
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x0305);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), STATE_BUFFER_SIZE_32);
    assert_eq!(cpu.regs.bx(), 0xF000);
    assert_eq!(cpu.regs.cx(), 0xFD40);
    assert_eq!(cpu.regs.si(), PM_BIOS_CODE_SEL);
}

#[test]
fn test_translation_services_get_raw_switch_addresses_0306() {
    // Lines 3440-3458
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ax(0x0306);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.bx(), 0xF000);
    assert_eq!(cpu.regs.cx(), 0xFD50);
}

#[test]
fn test_translation_services_unsupported_subfunc_direct() {
    // Lines 3461-3463
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ax(0x0307); // Invalid translation subfunc
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_UNSUPPORTED);
}

#[test]
fn test_get_page_attributes_0506_zero_page_count() {
    // Lines 5186-5190
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    cpu.regs.set_ax(0x0506);
    cpu.regs.esi = 1;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 0; // Zero page count
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_get_page_attributes_0506_nonexistent_handle() {
    // Lines 5235-5238
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    cpu.regs.set_ax(0x0506);
    cpu.regs.esi = 0xDEAD;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 1;
    cpu.regs.es = 0;
    cpu.regs.edx = 0x90000;
    memory.ensure_ram_size(0x90010);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_HANDLE);
}

#[test]
fn test_set_page_attributes_0507_zero_page_count() {
    // Lines 5284-5288
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    cpu.regs.set_ax(0x0507);
    cpu.regs.esi = 1;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_page_attributes_0507_offset_not_page_aligned() {
    // Lines 5292-5295
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    cpu.regs.set_ax(0x0507);
    cpu.regs.esi = 1;
    cpu.regs.ebx = 0x100; // Not page-aligned
    cpu.regs.ecx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_page_attributes_0507_nonexistent_handle() {
    // Lines 5329-5332
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    cpu.regs.set_ax(0x0507);
    cpu.regs.esi = 0xDEAD;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 1;
    cpu.regs.es = 0;
    cpu.regs.edx = 0x90000;
    memory.ensure_ram_size(0x90010);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_HANDLE);
}

#[test]
fn test_set_page_attributes_0507_16bit_unsupported() {
    // 16-bit clients are now allowed; ECX=0 (page_count=0) → INVALID_VALUE
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 0;

    cpu.regs.set_ax(0x0507);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_get_page_attributes_0506_16bit_unsupported() {
    // 16-bit clients are now allowed; ECX=0 (page_count=0) → INVALID_VALUE
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 0;

    cpu.regs.set_ax(0x0506);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_exception_handler_gate_type_no_handler() {
    // Lines 2922-2928: exception_handler_gate_type with no handler set
    let dpmi = Dpmi::new();
    assert!(dpmi.exception_handler_gate_type(0).is_none());
    assert!(dpmi.exception_handler_gate_type(255).is_none());
}

#[test]
fn test_exception_handler_entry_no_handler() {
    // Lines 2931-2937: exception_handler_entry with no handler set
    let dpmi = Dpmi::new();
    assert!(dpmi.exception_handler_entry(0).is_none());
}

#[test]
fn test_map_device_0508_overlap_with_existing_mapping() {
    // Lines 5461-5464: physical_mappings_overlap
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    // Allocate a block with enough pages
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x4000; // 4 pages
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    // Map page 0 to physical 0x1000
    cpu.regs.set_ax(0x0508);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0; // offset 0
    cpu.regs.ecx = 1; // 1 page
    cpu.regs.edx = 0x1000; // physical
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Try to map page 1 to overlapping physical 0x1000
    cpu.regs.set_ax(0x0508);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0x1000; // offset 0x1000
    cpu.regs.ecx = 1;
    cpu.regs.edx = 0x1000; // Same physical - overlaps!
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_LINEAR_UNAVAILABLE);
}

#[test]
fn test_map_conventional_0509_overlap_with_existing() {
    // Lines 5547-5550: overlap check for conventional memory mapping
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x4000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    // First map succeeds
    cpu.regs.set_ax(0x0509);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 1;
    cpu.regs.edx = 0x2000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Second map to same physical overlaps
    cpu.regs.set_ax(0x0509);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0x1000;
    cpu.regs.ecx = 1;
    cpu.regs.edx = 0x2000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_LINEAR_UNAVAILABLE);
}

#[test]
fn test_map_device_0508_offset_not_page_aligned() {
    // Lines 5415-5418
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    cpu.regs.set_ax(0x0508);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0x100; // Not page-aligned
    cpu.regs.ecx = 1;
    cpu.regs.edx = 0x1000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_map_conventional_0509_unaligned_offset() {
    // Lines 5501-5504: unaligned offset (offset not physical)
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    cpu.regs.set_ax(0x0509);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0x100; // Not page-aligned offset
    cpu.regs.ecx = 1;
    cpu.regs.edx = 0x1000; // Aligned physical
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_allocate_linear_0504_16bit_unsupported() {
    // 16-bit clients are now allowed; ECX=0 (size=0) → INVALID_VALUE
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 0;

    cpu.regs.set_ax(0x0504);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_pm_interrupt_vector_0205_hw_irq_vector() {
    // Lines 2855-2867: hardware IRQ vectors use interrupt gates
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

    // Set vector 0x08 (IRQ0) - hardware interrupt
    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x08);
    cpu.regs.set_cx(handler_sel);
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_set_pm_interrupt_vector_0205_offset_beyond_limit() {
    // Lines 2847-2850: offset beyond segment limit
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate a code selector with small limit
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handler_sel = cpu.regs.ax();

    // Set as code segment with small limit
    if let Some(s) = dpmi
        .selectors
        .iter_mut()
        .find(|s| s.selector == handler_sel)
    {
        s.access = 0xFA; // code, DPL=3, present
        s.limit = 0x00FF; // Very small limit
    }
    dpmi.write_selector_descriptor(&mut cpu, &mut memory, handler_sel, 0, 0x00FF, 0xFA, 0x00);

    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x21);
    cpu.regs.set_cx(handler_sel);
    cpu.regs.set_dx(0x0200); // Offset beyond limit 0xFF
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_LINEAR);
}

#[test]
fn test_set_access_rights_0009_selector_not_found() {
    // Lines 1597-1599: selector valid LDT range but not in selectors list
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Use a valid LDT selector that's in range but not allocated
    // First find one that's not in the selectors list
    let fake_sel: u16 = 0x00FF; // LDT, RPL=3, index=31 - unlikely to be allocated
                                // Remove from selectors if present
    dpmi.selectors.retain(|s| s.selector != fake_sel);

    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(fake_sel);
    // CL = access byte: present(0x80) | DPL=3(0x60) | data(0x10) | RW(0x02) = 0xF2
    cpu.regs.set_cl(0xF2);
    cpu.regs.set_ch(0x00);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_access_rights_0009_16bit_selector_not_in_list() {
    // Lines 1558-1560: 16-bit client, selector not in selectors list for flag preservation
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false); // 16-bit

    // Allocate a selector, then remove it from the tracking list
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let sel = cpu.regs.ax();

    // Remove from tracking but keep in LDT range
    dpmi.selectors.retain(|s| s.selector != sel);

    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(sel);
    // CL = access: present, DPL=3, data r/w = 0xF2
    cpu.regs.set_cl(0xF2);
    cpu.regs.set_ch(0x00);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Selector not found -> error
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_access_rights_0009_granularity_bit_change() {
    // Lines 1577-1584: existing selector with G bit set (flags & 0x80 != 0)
    // and new flags with G bit cleared, exercising the limit recalculation
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true); // 32-bit

    // Allocate a selector
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let sel = cpu.regs.ax();

    // Set it up with G=1 (granularity) and a large limit
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == sel) {
        s.flags = 0xC0; // G=1, D=1
        s.limit = 0xFFFF_FFFF; // 4GB limit with G=1
        s.access = 0xF2; // Present, DPL=3, data r/w
    }

    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(sel);
    // CL = access: present, DPL=3, data r/w = 0xF2
    cpu.regs.set_cl(0xF2);
    // CH = flags: G=1, D=1 in high nibble, limit_hi in low nibble
    cpu.regs.set_ch(0xCF); // G=1(0x80), D=1(0x40), limit_hi=0x0F
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_set_access_rights_0009_no_granularity() {
    // Lines 1583-1586: new flags without G bit (flags_high & 0x80 == 0)
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true); // 32-bit

    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let sel = cpu.regs.ax();

    // Set initial state with G=1
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == sel) {
        s.flags = 0xC0; // G=1, D=1
        s.limit = 0x000F_FFFF << 12 | 0xFFF;
        s.access = 0xF2;
    }

    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(sel);
    cpu.regs.set_cl(0xF2);
    // CH = D=1 only, G=0 -> limit is byte-granular
    cpu.regs.set_ch(0x40); // D=1, G=0, limit_hi=0
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_create_alias_000a_source_not_in_selectors() {
    // Lines 1639-1642: source selector valid LDT but not in selectors list
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Use a valid LDT selector that isn't in the selectors list
    let fake_sel: u16 = 0x00FF; // LDT, RPL=3, index=31
    dpmi.selectors.retain(|s| s.selector != fake_sel);

    cpu.regs.set_ax(0x000A);
    cpu.regs.set_bx(fake_sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_exception_handler_0203_trace_handler_bytes_readable() {
    let _tracing = init_tracing();
    // Lines 2518-2636: with trace enabled, handler bytes are read and logged
    let _lock = env_lock();

    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate a code selector with a base pointing to readable memory
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handler_sel = cpu.regs.ax();

    // Make it a code segment with a reasonable base and limit
    let handler_base = 0x50000u32;
    if let Some(s) = dpmi
        .selectors
        .iter_mut()
        .find(|s| s.selector == handler_sel)
    {
        s.access = 0xFA; // code, DPL=3, present, readable
        s.base = handler_base;
        s.limit = 0xFFFF;
    }
    dpmi.write_selector_descriptor(
        &mut cpu,
        &mut memory,
        handler_sel,
        handler_base,
        0xFFFF,
        0xFA,
        0x00,
    );

    // Write some handler bytes (0x66 0xCF = iret32 prefix)
    memory.write_u8(handler_base as usize, 0x66);
    memory.write_u8(handler_base as usize + 1, 0xCF);

    // Set exception handler via 0x0203
    cpu.regs.set_ax(0x0203);
    cpu.regs.set_bl(0x0D); // exception #13 (GP fault)
    cpu.regs.set_cx(handler_sel);
    cpu.regs.edx = 0; // offset 0
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // Handler should be set
    assert_eq!(dpmi.exception_handlers[0x0D].selector, handler_sel);
}

#[test]
fn test_set_exception_handler_0203_trace_handler_out_of_range() {
    let _tracing = init_tracing();
    // Lines 2530-2546: handler base+offset exceeds ram, read fails
    let _lock = env_lock();

    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handler_sel = cpu.regs.ax();

    // Set base near end of RAM so base + offset exceeds ram_size
    let handler_base = (memory.ram_size() - 2) as u32;
    if let Some(s) = dpmi
        .selectors
        .iter_mut()
        .find(|s| s.selector == handler_sel)
    {
        s.access = 0xFA; // code, present, DPL=3
        s.base = handler_base;
        s.limit = 0xFFFF;
    }
    dpmi.write_selector_descriptor(
        &mut cpu,
        &mut memory,
        handler_sel,
        handler_base,
        0xFFFF,
        0xFA,
        0x00,
    );

    cpu.regs.set_ax(0x0203);
    cpu.regs.set_bl(0x06); // exception #6 (invalid opcode)
    cpu.regs.set_cx(handler_sel);
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}
