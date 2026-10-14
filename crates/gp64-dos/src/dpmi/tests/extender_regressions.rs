use super::*;

#[test]
fn test_allocate_then_free_descriptor() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate a single descriptor
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let selector = cpu.regs.ax();
    assert_eq!(dpmi.selectors.len(), 1);

    // Free it
    cpu.regs.set_ax(0x0001);
    cpu.regs.set_bx(selector);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dpmi.selectors.len(), 0);
}

#[test]
fn test_free_invalid_descriptor() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Try to free a descriptor that was never allocated
    cpu.regs.set_ax(0x0001);
    cpu.regs.set_bx(0xFFFF); // Invalid selector
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0x8022); // Invalid selector error
}

#[test]
fn test_free_already_freed_descriptor() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate a descriptor
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    // Free it once
    cpu.regs.set_ax(0x0001);
    cpu.regs.set_bx(selector);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Try to free it again
    cpu.regs.set_ax(0x0001);
    cpu.regs.set_bx(selector);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0x8022); // Invalid selector error
}

#[test]
fn test_set_base_address_boundary_zero() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate a descriptor
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    // Set base to 0
    cpu.regs.set_ax(0x0007);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cx(0x0000); // Base high word
    cpu.regs.set_dx(0x0000); // Base low word
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify base is 0
    let base = dpmi.get_selector_base(selector);
    assert_eq!(base, Some(0));
}

#[test]
fn test_set_base_address_boundary_max_16mb() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate a descriptor
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    // Set base to 16MB-1 (0x00FFFFFF)
    cpu.regs.set_ax(0x0007);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cx(0x00FF); // Base high word
    cpu.regs.set_dx(0xFFFF); // Base low word
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let base = dpmi.get_selector_base(selector);
    assert_eq!(base, Some(0x00FFFFFF));
}

#[test]
fn test_set_base_address_boundary_4gb() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate a descriptor
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    // Set base to near 4GB (0xFFFF0000)
    cpu.regs.set_ax(0x0007);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cx(0xFFFF); // Base high word
    cpu.regs.set_dx(0x0000); // Base low word
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let base = dpmi.get_selector_base(selector);
    assert_eq!(base, Some(0xFFFF0000));
}

#[test]
fn test_set_limit_boundary_zero() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate a descriptor
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    // Set limit to 0 (1 byte segment)
    cpu.regs.set_ax(0x0008);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cx(0x0000); // Limit high word
    cpu.regs.set_dx(0x0000); // Limit low word
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_set_limit_boundary_64kb() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate a descriptor
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    // Set limit to 64KB-1 (0xFFFF) - byte granularity max
    cpu.regs.set_ax(0x0008);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dx(0xFFFF);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_set_limit_boundary_1mb() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate a descriptor
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    // Set limit to 1MB-1 (0x000FFFFF)
    cpu.regs.set_ax(0x0008);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cx(0x000F);
    cpu.regs.set_dx(0xFFFF);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_allocate_linear_memory_zero_size_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 0x0504 requires 32-bit mode
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_allocate_linear_memory_specific_address_succeeds() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1;
    cpu.regs.set_ax(0x0504);
    cpu.regs.ebx = 0x200000;
    cpu.regs.ecx = 0x2000;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ebx, 0x200000);
}

#[test]
fn test_allocate_linear_memory_specific_address_overlap_fails() {
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
    let base = cpu.regs.ebx;

    cpu.regs.set_ax(0x0504);
    cpu.regs.ebx = base;
    cpu.regs.ecx = 0x1000;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_LINEAR_UNAVAILABLE);
}

#[test]
fn test_set_limit_above_1mb_encodes_granularity_in_memory() {
    // Test that setting a limit > 1MB causes the G bit to be set in the
    // in-memory descriptor encoding, but NOT in the user-visible flags.
    // The G bit is automatically computed based on limit size.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate a descriptor.
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    // Set limit to 1MB (0x0010_0000), which requires granularity encoding.
    cpu.regs.set_ax(0x0008);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cx(0x0010);
    cpu.regs.set_dx(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // User-visible flags should NOT have G bit set
    // (G bit is only computed during memory encoding)
    let flags = dpmi
        .selectors
        .iter()
        .find(|s| s.selector == selector)
        .map(|s| s.flags)
        .unwrap();
    assert_eq!(
        flags & 0x8,
        0,
        "User-visible flags should not have G bit set - it's only for memory encoding"
    );

    // But the limit should be stored correctly
    let limit = dpmi.get_descriptor_limit(selector).unwrap();
    assert_eq!(limit, 0x0010_0000, "Limit should be stored as 1MB");
}

#[test]
fn test_set_access_rights_code_segment() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set CPL=3 to match DPL=3 in access byte
    cpu.regs.cpl = 3;

    // Allocate a descriptor
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    // Set access rights for code segment (executable, readable)
    // Access byte: P=1, DPL=3, S=1, E=1, R=1 = 0x9A (ring 0) or 0xFA (ring 3)
    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cx(0xC0FA); // 32-bit code, ring 3
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify via get descriptor
    cpu.regs.set_ax(0x000B);
    cpu.regs.set_bx(selector);
    let buf_addr = 0x50000_usize;
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    let access = memory.read_u8(buf_addr + 5);
    assert_eq!(access & 0x08, 0x08); // Executable bit set
}

#[test]
fn test_set_access_rights_data_segment() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set CPL=3 to match DPL=3 in access byte
    cpu.regs.cpl = 3;

    // Allocate a descriptor
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    // Set access rights for data segment (read/write)
    // Access byte: P=1, DPL=3, S=1, E=0, W=1 = 0x92 (ring 0) or 0xF2 (ring 3)
    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cx(0xC0F2); // 32-bit data, ring 3
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify via get descriptor
    cpu.regs.set_ax(0x000B);
    cpu.regs.set_bx(selector);
    let buf_addr = 0x50000_usize;
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    let access = memory.read_u8(buf_addr + 5);
    assert_eq!(access & 0x08, 0x00); // Executable bit clear (data segment)
    assert_eq!(access & 0x02, 0x02); // Writable bit set
}

#[test]
fn test_set_access_rights_unallocated_selector_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set CPL=3 to match DPL=3 in access byte
    cpu.regs.cpl = 3;

    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(0x0017); // LDT selector, not allocated
    cpu.regs.set_cx(0x00F2); // Access/flags with DPL=3
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_access_rights_rejects_non_present() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.cpl = 3;
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cl(0x72); // P=0, DPL=3, S=1, data
    cpu.regs.set_ch(0x00);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_access_rights_rejects_dpl_mismatch() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.cpl = 3;
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cl(0x92); // P=1, DPL=0, S=1, data
    cpu.regs.set_ch(0x00);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_access_rights_rejects_system_segment() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.cpl = 3;
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cl(0xE2); // P=1, DPL=3, S=0, system
    cpu.regs.set_ch(0x00);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_access_rights_rejects_code_not_readable() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.cpl = 3;
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cl(0xF8); // P=1, DPL=3, S=1, code, non-readable
    cpu.regs.set_ch(0x00);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_access_rights_rejects_code_conforming() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.cpl = 3;
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cl(0xFE); // P=1, DPL=3, S=1, code, conforming
    cpu.regs.set_ch(0x00);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_access_rights_rejects_reserved_flag_bit5() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.cpl = 3;
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cl(0xF2); // P=1, DPL=3, S=1, data
    cpu.regs.set_ch(0x20); // Reserved bit 5 set
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_access_rights_updates_limit_high_bits() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // 32-bit client mode: SetAccess (0009h) CH contains explicit flags
    dpmi.cpu_mode = 1;
    cpu.regs.cpl = 3;
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    cpu.regs.set_ax(0x0008);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dx(0xFFFF);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cl(0xF2); // P=1, DPL=3, S=1, data
    cpu.regs.set_ch(0x05); // limit high = 0x5, flags = 0
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    let limit = dpmi.get_descriptor_limit(selector).unwrap();
    assert_eq!(limit, 0x0005_FFFF);
}

#[test]
fn test_set_access_rights_16bit_client_preserves_flags() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // 16-bit client mode (default): SetAccess (0009h) preserves existing flags
    cpu.regs.cpl = 3;
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    // Set initial limit to 0xFFFF
    cpu.regs.set_ax(0x0008);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dx(0xFFFF);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // SetAccess with CH=0x05 — CH low nibble provides limit bits 16-19,
    // which extend the 20-bit limit. This applies to all clients (16-bit
    // and 32-bit). The DPMI spec does not ignore CH for 16-bit clients.
    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cl(0xF2); // P=1, DPL=3, S=1, data
    cpu.regs.set_ch(0x05); // limit_hi=5, flags=0 → limit extends to 0x5FFFF
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    let limit = dpmi.get_descriptor_limit(selector).unwrap();
    // CH low nibble (5) extends limit: (5 << 16) | 0xFFFF = 0x5FFFF
    assert_eq!(limit, 0x5FFFF);
}

#[test]
fn test_get_selector_increment_value() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0003); // Get selector increment
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Selector increment should be 8 (size of descriptor table entry)
    assert_eq!(cpu.regs.ax(), 8);
}

#[test]
fn test_int31_trace_reads_idt_entry_when_present() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let idt_base = 0x7000usize;
    cpu.regs.idtr.base = idt_base as u32;
    dpmi.write_idt_gate_full(&mut memory, idt_base, 0x31, 0x1234_5678, 0x0008, 0x8E);

    cpu.regs.set_ax(0x0003);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert_eq!(cpu.regs.ax(), 8);
}

#[test]
fn test_get_segment_base_address() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate and set up descriptor
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    // Set base to a known value
    cpu.regs.set_ax(0x0007);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cx(0x0012); // Base high = 0x0012
    cpu.regs.set_dx(0x3456); // Base low = 0x3456
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Get base address via INT 31h AX=0006h
    cpu.regs.set_ax(0x0006);
    cpu.regs.set_bx(selector);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let base = ((cpu.regs.cx() as u32) << 16) | (cpu.regs.dx() as u32);
    assert_eq!(base, 0x00123456);
}

#[test]
fn test_descriptor_services_reject_gdt_selectors() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let subfuncs = [0x0006u16, 0x0007, 0x0008, 0x0009, 0x000A, 0x000B, 0x000C];
    for ax in subfuncs {
        cpu.regs.set_ax(ax);
        cpu.regs.set_bx(0x0008); // GDT selector (TI=0)
        dpmi.int31(&mut cpu, &mut memory).unwrap();

        assert!(
            cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "AX {ax:04X} should fail"
        );
        assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR, "AX {ax:04X}");
    }
}

#[test]
fn test_canonical_ldt_selector_accepts_index_zero() {
    let dpmi = Dpmi::new();

    // LDT index 0 is valid (unlike GDT[0] which is the null descriptor).
    assert_eq!(dpmi.canonical_ldt_selector(0x0004), Some(0x0007));
    assert_eq!(dpmi.canonical_ldt_selector(0x000C), Some(0x000F));
}

#[test]
fn test_get_set_descriptor_roundtrip() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.cpl = 3; // DPL in descriptor is forced to CPL
                      // Allocate descriptor
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    // Create a custom descriptor
    // Note: flags byte must NOT have granularity bit (0x80) set for byte-exact roundtrip
    // because granularity causes limit to be stored in 4KB units (shifted >> 12)
    let buf_addr = 0x50000_usize;
    memory.write_u16(buf_addr, 0x1234); // limit 15:0
    memory.write_u16(buf_addr + 2, 0x5678); // base 15:0
    memory.write_u8(buf_addr + 4, 0x9A); // base 23:16
    memory.write_u8(buf_addr + 5, 0xF2); // access (data, R/W, ring 3)
    memory.write_u8(buf_addr + 6, 0x4B); // limit 19:16 (0xB) + flags (0x4, no granularity)
    memory.write_u8(buf_addr + 7, 0xDC); // base 31:24

    // Set descriptor
    cpu.regs.set_ax(0x000C);
    cpu.regs.set_bx(selector);
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Get descriptor into different buffer
    let buf2_addr = 0x50100_usize;
    cpu.regs.set_ax(0x000B);
    cpu.regs.set_bx(selector);
    cpu.regs.es = (buf2_addr >> 4) as u16;
    cpu.regs.set_di((buf2_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify all 8 bytes match
    for i in 0..8 {
        assert_eq!(
            memory.read_u8(buf_addr + i),
            memory.read_u8(buf2_addr + i),
            "Descriptor byte {i} mismatch"
        );
    }
}

#[test]
fn test_get_multiple_descriptors_success() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(2);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let first = cpu.regs.ax();
    let second = first.wrapping_add(8);

    // 10-byte entry format: selector(2) + descriptor(8)
    let buf_addr = 0x56000_usize;
    memory.write_u16(buf_addr, first);
    memory.write_u16(buf_addr + 10, second);

    cpu.regs.set_ax(0x000E);
    cpu.regs.set_cx(2);
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // Access byte at offset 7 within each 10-byte entry
    assert_eq!(memory.read_u8(buf_addr + 7), 0xF2);
    assert_eq!(memory.read_u8(buf_addr + 10 + 7), 0xF2);
}

#[test]
fn test_get_multiple_descriptors_zero_count_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let buf_addr = 0x56080_usize;
    cpu.regs.set_ax(0x000E);
    cpu.regs.set_cx(0);
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn test_get_multiple_descriptors_invalid_selector_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let buf_addr = 0x56100_usize;
    memory.write_u16(buf_addr, 0x0008); // GDT selector

    cpu.regs.set_ax(0x000E);
    cpu.regs.set_cx(1);
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_get_multiple_descriptors_unallocated_selector_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let buf_addr = 0x56200_usize;
    memory.write_u16(buf_addr, 0x0017); // LDT selector, not allocated

    cpu.regs.set_ax(0x000E);
    cpu.regs.set_cx(1);
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_get_multiple_descriptors_out_of_ldt_range_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ldt_limit = 0x0007; // One LDT entry (selector 0x0007)

    let buf_addr = 0x56280_usize;
    memory.write_u16(buf_addr, 0x0017); // LDT selector beyond limit

    cpu.regs.set_ax(0x000E);
    cpu.regs.set_cx(1);
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn test_get_multiple_descriptors_invalid_buffer_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.protected_mode = true;
    cpu.regs.cr0 = 1;
    cpu.regs.set_ax(0x000E);
    cpu.regs.set_cx(1);
    cpu.regs.es = 0x0017; // Invalid LDT selector for buffer
    cpu.regs.set_di(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_get_multiple_descriptors_buffer_overflow_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    // Set up flat ES descriptor so we can address memory near ram_size
    let es_sel = 0x27u16;
    dpmi.selectors.push(Selector {
        selector: es_sel,
        base: 0,
        limit: 0xFFFF_FFFF,
        access: 0xF2,
        flags: 0xC0,
        lock_count: 0,
    });
    dpmi.protected_mode = true;
    dpmi.cpu_mode = 1;

    let buf_addr = memory.ram_size() - 8;
    memory.write_u16(buf_addr, selector);

    cpu.regs.set_ax(0x000E);
    cpu.regs.set_cx(1);
    cpu.regs.es = es_sel;
    cpu.regs.edi = buf_addr as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_LINEAR);
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn test_set_multiple_descriptors_success() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.cpl = 3;
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    let buf_addr = 0x56300_usize;
    memory.write_u16(buf_addr, selector);
    memory.write_u16(buf_addr + 2, 0x1234); // limit
    memory.write_u16(buf_addr + 4, 0x5678); // base low
    memory.write_u8(buf_addr + 6, 0x9A); // base mid
    memory.write_u8(buf_addr + 7, 0xF2); // access

    cpu.regs.set_ax(0x000F);
    cpu.regs.set_cx(1);
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let entry = dpmi
        .selectors
        .iter()
        .find(|s| s.selector == selector)
        .unwrap();
    assert_eq!(entry.base, 0x009A5678);
    assert_eq!(entry.limit, 0x1234);
    assert_eq!(entry.access, 0xF2);
}

#[test]
fn test_set_multiple_descriptors_rejects_system_descriptor() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    let buf_addr = 0x56340_usize;
    memory.write_u16(buf_addr, selector);
    memory.write_u16(buf_addr + 2, 0x1234);
    memory.write_u16(buf_addr + 4, 0x5678);
    memory.write_u8(buf_addr + 6, 0x9A);
    memory.write_u8(buf_addr + 7, 0x80); // System descriptor (S=0)

    cpu.regs.set_ax(0x000F);
    cpu.regs.set_cx(1);
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn test_set_multiple_descriptors_forces_dpl_to_cpl() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.cpl = 1;
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    let buf_addr = 0x563C0_usize;
    memory.write_u16(buf_addr, selector);
    memory.write_u16(buf_addr + 2, 0x1234);
    memory.write_u16(buf_addr + 4, 0x5678);
    memory.write_u8(buf_addr + 6, 0x9A);
    memory.write_u8(buf_addr + 7, 0xF2); // DPL=3 in access byte

    cpu.regs.set_ax(0x000F);
    cpu.regs.set_cx(1);
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let entry = dpmi
        .selectors
        .iter()
        .find(|s| s.selector == selector)
        .unwrap();
    assert_eq!(entry.access, 0xB2);
}

#[test]
fn test_set_multiple_descriptors_zero_count_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let buf_addr = 0x56380_usize;
    cpu.regs.set_ax(0x000F);
    cpu.regs.set_cx(0);
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn test_set_multiple_descriptors_invalid_selector_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let buf_addr = 0x56400_usize;
    memory.write_u16(buf_addr, 0x0008); // GDT selector

    cpu.regs.set_ax(0x000F);
    cpu.regs.set_cx(1);
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_multiple_descriptors_unallocated_selector_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let buf_addr = 0x56500_usize;
    memory.write_u16(buf_addr, 0x0017); // LDT selector, not allocated

    cpu.regs.set_ax(0x000F);
    cpu.regs.set_cx(1);
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_multiple_descriptors_out_of_ldt_range_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ldt_limit = 0x0007; // One LDT entry (selector 0x0007)

    let buf_addr = 0x56580_usize;
    memory.write_u16(buf_addr, 0x0017); // LDT selector beyond limit

    cpu.regs.set_ax(0x000F);
    cpu.regs.set_cx(1);
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn test_set_multiple_descriptors_invalid_buffer_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.protected_mode = true;
    cpu.regs.cr0 = 1;
    cpu.regs.set_ax(0x000F);
    cpu.regs.set_cx(1);
    cpu.regs.es = 0x0017; // Invalid LDT selector for buffer
    cpu.regs.set_di(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_multiple_descriptors_buffer_overflow_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    // Set up flat ES descriptor so we can address memory near ram_size
    let es_sel = 0x27u16;
    dpmi.selectors.push(Selector {
        selector: es_sel,
        base: 0,
        limit: 0xFFFF_FFFF,
        access: 0xF2,
        flags: 0xC0,
        lock_count: 0,
    });
    dpmi.protected_mode = true;
    dpmi.cpu_mode = 1;

    let buf_addr = memory.ram_size() - 8;
    memory.write_u16(buf_addr, selector);

    cpu.regs.set_ax(0x000F);
    cpu.regs.set_cx(1);
    cpu.regs.es = es_sel;
    cpu.regs.edi = buf_addr as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_LINEAR);
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn test_encode_descriptor_bytes_granularity_flag() {
    let dpmi = Dpmi::new();
    let (limit_lo, _, _, _, limit_flags, _) =
        dpmi.encode_descriptor_bytes(0, 0x1234_5000, 0x92, 0x8);

    assert_eq!(limit_lo, (0x1234_5000u32 >> 12) as u16);
    assert_eq!(limit_flags & 0x80, 0x80);
}

#[test]
fn test_encode_descriptor_bytes_sets_granularity_for_large_limit() {
    let dpmi = Dpmi::new();
    let (limit_lo, _, _, _, limit_flags, _) =
        dpmi.encode_descriptor_bytes(0, 0x0010_0000, 0x92, 0x0);

    assert_eq!(limit_lo, (0x0010_0000u32 >> 12) as u16);
    assert_eq!(limit_flags & 0x80, 0x80);
}

#[test]
fn test_encode_descriptor_bytes_prefers_high_nibble_flags() {
    let dpmi = Dpmi::new();
    let (_, _, _, _, limit_flags, _) = dpmi.encode_descriptor_bytes(0, 0x0000_FFFF, 0x92, 0x25);

    assert_eq!(limit_flags & 0xF0, 0x20);
}

#[test]
fn test_encode_descriptor_bytes_uses_high_nibble_flags() {
    let dpmi = Dpmi::new();
    // Flags byte: high nibble contains G/D/L/AVL bits
    let (_, _, _, _, limit_flags, _) = dpmi.encode_descriptor_bytes(0, 0x0000_FFFF, 0x92, 0x50);

    assert_eq!(limit_flags & 0xF0, 0x50);
}

#[test]
fn test_align_up_zero_alignment() {
    assert_eq!(Dpmi::align_up(0x1234, 0), 0x1234);
}

#[test]
fn test_align_up_overflow_saturates_to_max_aligned() {
    let align = 0x1000;
    let value = u32::MAX - 0x7FF;
    let expected = !(align - 1);

    assert_eq!(Dpmi::align_up(value, align), expected);
}

#[test]
fn test_allocate_dos_memory_block() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate 16 paragraphs (256 bytes)
    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // AX should contain the real-mode segment
    // DX should contain the selector
    let segment = cpu.regs.ax();
    let selector = cpu.regs.dx();
    assert!(segment > 0);
    assert!(selector > 0);
}

#[test]
fn test_allocate_large_dos_memory_block() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate near conventional memory limit (640KB = 0xA000 paragraphs)
    // Request a large block (32KB = 0x800 paragraphs)
    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(0x0800);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Should succeed unless memory is unavailable
    // Either way, shouldn't crash
}

#[test]
fn test_allocate_dos_memory_large_16bit_allocates_multiple_descriptors() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(0x1800); // 96KB
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let selector = cpu.regs.dx();
    let block = dpmi
        .dos_memory_blocks
        .iter()
        .find(|b| b.selector == selector)
        .expect("DOS memory block should be tracked");
    assert_eq!(block.selector_count, 2);

    let base_linear = (block.segment as u32) << 4;
    let first = dpmi
        .selectors
        .iter()
        .find(|s| s.selector == selector)
        .unwrap();
    assert_eq!(first.base, base_linear);
    assert_eq!(first.limit, 0xFFFF);

    let second_sel = selector.wrapping_add(8);
    let second = dpmi
        .selectors
        .iter()
        .find(|s| s.selector == second_sel)
        .unwrap();
    assert_eq!(second.base, base_linear + 0x10000);
    assert_eq!(second.limit, 0x7FFF);
}

#[test]
fn test_resize_dos_memory_shrinks_descriptor_count() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(0x1800); // 96KB
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.dx();

    cpu.regs.set_ax(0x0102);
    cpu.regs.set_bx(0x0800); // 32KB
    cpu.regs.set_dx(selector);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let block = dpmi
        .dos_memory_blocks
        .iter()
        .find(|b| b.selector == selector)
        .unwrap();
    assert_eq!(block.selector_count, 1);
    let second_sel = selector.wrapping_add(8);
    assert!(!dpmi.selectors.iter().any(|s| s.selector == second_sel));
    assert!(dpmi.free_selectors.contains(&second_sel));
}

#[test]
fn test_free_dos_memory_block() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate first
    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.dx();

    // Free it
    cpu.regs.set_ax(0x0101);
    cpu.regs.set_dx(selector);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_free_invalid_dos_memory() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Try to free a selector that wasn't allocated
    cpu.regs.set_ax(0x0101);
    cpu.regs.set_dx(0xFFFF);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DOS_ERR_INVALID_MEMORY_BLOCK);
}

#[test]
fn test_allocate_dos_memory_with_dosmem_success() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dos_mem = init_dos_mem(&mut memory);

    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(16);
    dpmi.int31_with_dosmem(&mut cpu, &mut memory, Some(&mut dos_mem), 0x1234)
        .unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(cpu.regs.ax() != 0);
    assert!(cpu.regs.dx() != 0);
}

#[test]
fn test_allocate_dos_memory_with_dosmem_zero_paragraphs() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dos_mem = init_dos_mem(&mut memory);

    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(0);
    dpmi.int31_with_dosmem(&mut cpu, &mut memory, Some(&mut dos_mem), 0x1234)
        .unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DOS_ERR_INSUFFICIENT_MEMORY);
}

#[test]
fn test_allocate_dos_memory_with_dosmem_descriptor_unavailable() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dos_mem = init_dos_mem(&mut memory);

    dpmi.next_selector = 0xFFFC;
    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(16);
    dpmi.int31_with_dosmem(&mut cpu, &mut memory, Some(&mut dos_mem), 0x1234)
        .unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DOS_ERR_INSUFFICIENT_MEMORY);
}

#[test]
fn test_allocate_dos_memory_descriptor_unavailable_without_dosmem() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.next_selector = 0xFFFC;
    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DOS_ERR_INSUFFICIENT_MEMORY);
}

#[test]
fn test_free_dos_memory_with_dosmem_success() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dos_mem = init_dos_mem(&mut memory);

    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(16);
    dpmi.int31_with_dosmem(&mut cpu, &mut memory, Some(&mut dos_mem), 0x1234)
        .unwrap();
    let selector = cpu.regs.dx();

    cpu.regs.set_ax(0x0101);
    cpu.regs.set_dx(selector);
    dpmi.int31_with_dosmem(&mut cpu, &mut memory, Some(&mut dos_mem), 0x1234)
        .unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_free_dos_memory_invalid_selector_with_dosmem() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dos_mem = init_dos_mem(&mut memory);

    cpu.regs.set_ax(0x0101);
    cpu.regs.set_dx(0x0008); // GDT selector
    dpmi.int31_with_dosmem(&mut cpu, &mut memory, Some(&mut dos_mem), 0x1234)
        .unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DOS_ERR_INVALID_MEMORY_BLOCK);
}

#[test]
fn test_resize_dos_memory_with_dosmem_success() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dos_mem = init_dos_mem(&mut memory);

    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(16);
    dpmi.int31_with_dosmem(&mut cpu, &mut memory, Some(&mut dos_mem), 0x1234)
        .unwrap();
    let selector = cpu.regs.dx();

    cpu.regs.set_ax(0x0102);
    cpu.regs.set_bx(32);
    cpu.regs.set_dx(selector);
    dpmi.int31_with_dosmem(&mut cpu, &mut memory, Some(&mut dos_mem), 0x1234)
        .unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_resize_dos_memory_zero_paragraphs_with_dosmem() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dos_mem = init_dos_mem(&mut memory);

    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(16);
    dpmi.int31_with_dosmem(&mut cpu, &mut memory, Some(&mut dos_mem), 0x1234)
        .unwrap();
    let selector = cpu.regs.dx();

    cpu.regs.set_ax(0x0102);
    cpu.regs.set_bx(0);
    cpu.regs.set_dx(selector);
    dpmi.int31_with_dosmem(&mut cpu, &mut memory, Some(&mut dos_mem), 0x1234)
        .unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DOS_ERR_INSUFFICIENT_MEMORY);
}

#[test]
fn test_resize_dos_memory_invalid_selector_with_dosmem() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dos_mem = init_dos_mem(&mut memory);

    cpu.regs.set_ax(0x0102);
    cpu.regs.set_bx(16);
    cpu.regs.set_dx(0x0008); // GDT selector
    dpmi.int31_with_dosmem(&mut cpu, &mut memory, Some(&mut dos_mem), 0x1234)
        .unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DOS_ERR_INVALID_MEMORY_BLOCK);
}

#[test]
fn test_resize_dos_memory_missing_block_with_dosmem() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dos_mem = init_dos_mem(&mut memory);

    cpu.regs.set_ax(0x0102);
    cpu.regs.set_bx(16);
    cpu.regs.set_dx(0x0017); // LDT selector, not allocated
    dpmi.int31_with_dosmem(&mut cpu, &mut memory, Some(&mut dos_mem), 0x1234)
        .unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DOS_ERR_INVALID_MEMORY_BLOCK);
}

#[test]
fn test_resize_dos_memory_larger() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate 16 paragraphs
    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.dx();

    // Resize to 32 paragraphs
    cpu.regs.set_ax(0x0102);
    cpu.regs.set_bx(32);
    cpu.regs.set_dx(selector);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    // May fail if memory unavailable, but shouldn't crash
}

#[test]
fn test_resize_dos_memory_smaller() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate 32 paragraphs
    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(32);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.dx();

    // Resize to 16 paragraphs
    cpu.regs.set_ax(0x0102);
    cpu.regs.set_bx(16);
    cpu.regs.set_dx(selector);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    // Shrinking should generally succeed
}

#[test]
fn test_get_real_mode_interrupt_vector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up IVT entry for INT 21h
    memory.write_u16(0x21 * 4, 0x1234); // Offset
    memory.write_u16(0x21 * 4 + 2, 0x5678); // Segment

    cpu.regs.set_ax(0x0200);
    cpu.regs.set_bl(0x21);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    assert_eq!(cpu.regs.cx(), 0x5678); // Segment
    assert_eq!(cpu.regs.dx(), 0x1234); // Offset
}

#[test]
fn test_set_real_mode_interrupt_vector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0201);
    cpu.regs.set_bl(0x21);
    cpu.regs.set_cx(0xABCD); // Segment
    cpu.regs.set_dx(0x1234); // Offset
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify IVT was updated
    assert_eq!(memory.read_u16(0x21 * 4), 0x1234);
    assert_eq!(memory.read_u16(0x21 * 4 + 2), 0xABCD);
}

#[test]
fn test_get_protected_mode_interrupt_vector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    setup_gdt_code_descriptor(&dpmi, &mut cpu, &mut memory, 0x0028, 0xFFFF);

    // First set a PM interrupt vector
    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x10); // INT 10h
    cpu.regs.set_cx(0x0028);
    cpu.regs.edx = 0x12345678;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Now get it back
    cpu.regs.set_ax(0x0204);
    cpu.regs.set_bl(0x10);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    assert_eq!(cpu.regs.cx(), 0x0028);
    assert_eq!(cpu.regs.edx, 0x12345678);
}

#[test]
fn test_set_protected_mode_interrupt_vector() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    setup_gdt_code_descriptor(&dpmi, &mut cpu, &mut memory, 0x0038, 0xFFFF);

    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x33); // INT 33h (mouse)
    cpu.regs.set_cx(0x0038);
    cpu.regs.edx = 0xDEADBEEF;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify by getting
    cpu.regs.set_ax(0x0204);
    cpu.regs.set_bl(0x33);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert_eq!(cpu.regs.cx(), 0x0038);
    assert_eq!(cpu.regs.edx, 0xDEADBEEF);
}

#[test]
fn test_set_protected_mode_interrupt_vector_rejects_data_selector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1;
    cpu.cs_bits = 32;
    setup_gdt_data_descriptor(&dpmi, &mut cpu, &mut memory, 0x0008, 0xFFFF);

    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x20);
    cpu.regs.set_cx(0x0008);
    cpu.regs.edx = 0x1234_5678;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_pm_interrupt_vector_hw_irq_uses_interrupt_gate() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1;
    cpu.cs_bits = 32;
    // 4GB code descriptor for 32-bit offset
    let gdt_base = 0x2000usize;
    let index = (0x0008u16 >> 3) as usize;
    cpu.regs.gdtr.base = gdt_base as u32;
    cpu.regs.gdtr.limit = ((index + 1) * 8 - 1) as u16;
    dpmi.write_descriptor_raw(&mut memory, gdt_base, index, 0, 0xFFFFF, 0xFA, 0x0C);

    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x08); // IRQ0
    cpu.regs.set_cx(0x0008);
    cpu.regs.edx = 0x1234_5678;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dpmi.pm_idt[0x08].gate_type, 0x0E);
}

#[test]
fn test_interrupt_vector_all_256_vectors() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    setup_gdt_code_descriptor(&dpmi, &mut cpu, &mut memory, 0x0008, 0xFFFF);

    // Set and get all 256 interrupt vectors
    for int_num in 0u8..=255 {
        // Set PM interrupt vector
        cpu.regs.set_ax(0x0205);
        cpu.regs.set_bl(int_num);
        cpu.regs.set_cx(0x0008);
        cpu.regs.edx = int_num as u32 * 0x100;
        dpmi.int31(&mut cpu, &mut memory).unwrap();
        assert!(
            !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "Failed to set PM vector for INT {int_num:02X}"
        );

        // Get it back
        cpu.regs.set_ax(0x0204);
        cpu.regs.set_bl(int_num);
        dpmi.int31(&mut cpu, &mut memory).unwrap();
        assert!(
            !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "Failed to get PM vector for INT {int_num:02X}"
        );
        assert_eq!(
            cpu.regs.edx,
            int_num as u32 * 0x100,
            "PM vector offset mismatch for INT {int_num:02X}"
        );
    }
}

#[test]
fn test_simulate_real_mode_int_10h() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up IVT for INT 10h
    memory.write_u16(0x10 * 4, 0x1234);
    memory.write_u16(0x10 * 4 + 2, 0x1000); // Use non-HLE segment for testing V86 entry

    // Set up real mode call structure
    let rm_struct_addr = 0x50000_usize;
    memory.write_u32(rm_struct_addr + 28, 0x0003); // EAX = 03h (set video mode 3)
    memory.write_u16(rm_struct_addr + 32, 0x0202); // Flags
    memory.write_u16(rm_struct_addr + 46, 0); // SP
    memory.write_u16(rm_struct_addr + 48, 0); // SS

    cpu.regs.set_ax(0x0300);
    cpu.regs.set_bl(0x10); // INT 10h
    cpu.regs.set_cx(0); // No stack words to copy
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.edi = (rm_struct_addr & 0xF) as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    // Should not crash/error
}

#[test]
fn test_simulate_real_mode_int_preserves_and_reports_carry_flag() {
    // MDD-BUG-CRUCIBLE-00054: the simulated real-mode interrupt handler must
    // run with the caller-provided RMCS carry flag; success (CF=0) is reported
    // to the protected-mode caller only on the RETURN path, never by clearing
    // the live V86 flags the handler is about to run with.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Non-HLE IVT vector so the call enters V86 (not the HLE-defer path).
    memory.write_u16(0x10 * 4, 0x1234);
    memory.write_u16(0x10 * 4 + 2, 0x1000);

    let rm_struct_addr = 0x50000_usize;
    memory.write_u32(rm_struct_addr + 28, 0x0003); // EAX
    memory.write_u16(rm_struct_addr + 32, 0x0203); // Flags: CF (bit 0) set
    memory.write_u16(rm_struct_addr + 46, 0); // SP
    memory.write_u16(rm_struct_addr + 48, 0); // SS

    // The PM caller happens to have a stray CF set before the call; a
    // successful 0300h must still return with CF clear.
    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);

    cpu.regs.set_ax(0x0300);
    cpu.regs.set_bl(0x10);
    cpu.regs.set_cx(0);
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.edi = (rm_struct_addr & 0xF) as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Entered V86: the handler sees the caller-provided CF, not a forced clear.
    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "V86 handler must run with the RMCS carry flag preserved"
    );

    // Real-mode handler returns to the sentinel; exit back to protected mode.
    cpu.regs.cs = 0;
    cpu.regs.set_ip(0);
    dpmi.exit_v86(&mut cpu, &mut memory);

    // Back in PM: success reported as CF clear even though the caller had CF set.
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "successful 0300h must report CF=0 to the PM caller on return"
    );
}

#[test]
fn test_translation_call_real_mode_preserves_carry_flag() {
    // MDD-BUG-CRUCIBLE-00054, 0301h (RETF frame) and 0302h (IRET frame): the
    // called real-mode procedure must also run with the RMCS carry flag.
    for subfunc in [0x0301_u16, 0x0302_u16] {
        let mut dpmi = Dpmi::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();

        let rm_struct_addr = 0x50000_usize;
        memory.write_u16(rm_struct_addr + 32, 0x0203); // Flags: CF set
        memory.write_u16(rm_struct_addr + 42, 0x0100); // IP
        memory.write_u16(rm_struct_addr + 44, 0x2000); // CS (non-HLE)
        memory.write_u16(rm_struct_addr + 46, 0); // SP
        memory.write_u16(rm_struct_addr + 48, 0); // SS

        cpu.regs.set_ax(subfunc);
        cpu.regs.set_bx(0);
        cpu.regs.set_cx(0);
        cpu.regs.es = (rm_struct_addr >> 4) as u16;
        cpu.regs.edi = (rm_struct_addr & 0xF) as u32;
        dpmi.int31(&mut cpu, &mut memory).unwrap();

        assert!(
            cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "0{subfunc:03X}h must preserve the RMCS carry flag into the real-mode procedure"
        );
    }
}

#[test]
fn test_simulate_real_mode_int_21h() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up IVT for INT 21h
    memory.write_u16(0x21 * 4, 0x5678);
    memory.write_u16(0x21 * 4 + 2, 0xF000);

    // Set up real mode call structure for INT 21h AH=30h (get DOS version)
    let rm_struct_addr = 0x50000_usize;
    memory.write_u32(rm_struct_addr + 28, 0x3000); // EAX = 3000h
    memory.write_u16(rm_struct_addr + 32, 0x0202);
    memory.write_u16(rm_struct_addr + 46, 0);
    memory.write_u16(rm_struct_addr + 48, 0);

    cpu.regs.set_ax(0x0300);
    cpu.regs.set_bl(0x21);
    cpu.regs.set_cx(0);
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.edi = (rm_struct_addr & 0xF) as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
}

#[test]
fn test_simulate_real_mode_int_invalid_struct_selector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.protected_mode = true;
    cpu.regs.cr0 = 1;

    cpu.regs.set_ax(0x0300);
    cpu.regs.set_bl(0x10);
    cpu.regs.set_cx(0);
    cpu.regs.es = 0x0017; // LDT selector, not allocated
    cpu.regs.set_di(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_simulate_real_mode_int_copies_stack_32bit() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1;
    cpu.regs.ss = 0x1000;
    cpu.regs.esp = 0x0100;

    let pm_linear = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.esp as u16);
    memory.write_u16(pm_linear, 0xBEEF);
    memory.write_u16(pm_linear + 2, 0xCAFE);

    memory.write_u16(0x10 * 4, 0x1234);
    memory.write_u16(0x10 * 4 + 2, 0x1000); // Use non-HLE segment for testing V86 entry

    let rm_struct_addr = 0x52000_usize;
    memory.write_u16(rm_struct_addr + 46, 0x2000);
    memory.write_u16(rm_struct_addr + 48, 0x3000);

    cpu.regs.set_ax(0x0300);
    cpu.regs.set_bl(0x10);
    cpu.regs.ecx = 2;
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.edi = (rm_struct_addr & 0xF) as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    let copied_base = Memory::seg_offset_to_linear(0x3000, 0x1FFC);
    assert_eq!(memory.read_u16(copied_base), 0xBEEF);
    assert_eq!(memory.read_u16(copied_base + 2), 0xCAFE);
}

#[test]
fn test_translation_services_reject_high_ecx_32bit() {
    // ECX high bits are now ignored per DPMI spec — CX is always 16-bit.
    // With high bits set and CX=0, stack_words=0 which is valid,
    // so the call should succeed.
    for subfunc in [0x0300_u16, 0x0301_u16, 0x0302_u16] {
        let mut dpmi = Dpmi::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();

        dpmi.cpu_mode = 1;
        cpu.regs.ecx = 0x0001_0000;
        cpu.regs.set_ax(subfunc);
        cpu.regs.set_bx(0);
        cpu.regs.es = 0;
        cpu.regs.edi = 0;

        dpmi.int31(&mut cpu, &mut memory).unwrap();

        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    }
}

#[test]
fn test_translation_services_reject_large_stack_words_16bit() {
    for subfunc in [0x0300_u16, 0x0301_u16, 0x0302_u16] {
        let mut dpmi = Dpmi::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();

        dpmi.cpu_mode = 0;
        cpu.regs.set_ax(subfunc);
        cpu.regs.set_bx(0);
        cpu.regs.set_cx(0x8000);
        cpu.regs.es = 0;
        cpu.regs.set_di(0);

        dpmi.int31(&mut cpu, &mut memory).unwrap();

        assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
    }
}

#[test]
fn test_translation_services_reject_large_stack_words_32bit() {
    for subfunc in [0x0300_u16, 0x0301_u16, 0x0302_u16] {
        let mut dpmi = Dpmi::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();

        dpmi.cpu_mode = 1;
        cpu.regs.set_ax(subfunc);
        cpu.regs.set_bx(0);
        cpu.regs.ecx = 0x0000_8000;
        cpu.regs.es = 0;
        cpu.regs.edi = 0;

        dpmi.int31(&mut cpu, &mut memory).unwrap();

        assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
    }
}

#[test]
fn test_simulate_real_mode_int_zero_ss_nonzero_sp() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    memory.write_u16(0x10 * 4, 0x1234);
    memory.write_u16(0x10 * 4 + 2, 0x1000); // Use non-HLE segment for testing V86 entry

    let rm_struct_addr = 0x52080_usize;
    memory.write_u16(rm_struct_addr + 46, 0xFFFC);
    memory.write_u16(rm_struct_addr + 48, 0x0000);

    cpu.regs.set_ax(0x0300);
    cpu.regs.set_bl(0x10);
    cpu.regs.set_cx(0);
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.set_di((rm_struct_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_simulate_real_mode_int_copies_stack_16bit() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0x0200);

    let pm_linear = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.sp());
    memory.write_u16(pm_linear, 0x0BAD);

    memory.write_u16(0x10 * 4, 0x1234);
    memory.write_u16(0x10 * 4 + 2, 0x1000); // Use non-HLE segment for testing V86 entry

    let rm_struct_addr = 0x52300_usize;
    memory.write_u16(rm_struct_addr + 46, 0x2100);
    memory.write_u16(rm_struct_addr + 48, 0x3000);

    cpu.regs.set_ax(0x0300);
    cpu.regs.set_bl(0x10);
    cpu.regs.set_cx(1);
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.set_di((rm_struct_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    let copied_base = Memory::seg_offset_to_linear(0x3000, 0x20FE);
    assert_eq!(memory.read_u16(copied_base), 0x0BAD);
}

#[test]
fn test_simulate_real_mode_int_zero_ss_sp_uses_internal_stack() {
    // DPMI-065: When SS:SP=0:0, use internal stack at 9FC0:0000
    // instead of segment 0 which overlaps with IVT/BDA
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    memory.write_u16(0x10 * 4, 0x1234);
    memory.write_u16(0x10 * 4 + 2, 0x1000); // Use non-HLE segment for testing V86 entry

    // Set SS=0, SP=0 in the RealModeCallStruct
    let rm_struct_addr = 0x52400_usize;
    memory.write_u16(rm_struct_addr + 46, 0); // SP = 0
    memory.write_u16(rm_struct_addr + 48, 0); // SS = 0

    cpu.regs.set_ax(0x0300);
    cpu.regs.set_bl(0x10);
    cpu.regs.set_cx(0);
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.set_di((rm_struct_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // The stack should be host-supplied, not 0000:xxxx.
    assert_eq!(cpu.regs.ss, Dpmi::V86_CALLBACK_STACK_SEG);
    // And the frame this call pushed must be IN it. With the old
    // 9FC0:0000 default, SP=0 wrapped on the first push and the three-word
    // return frame landed at 0xAFBFA-0xAFBFE, inside the VGA aperture
    // (MDD-BUG-CRU-01386). Reading those bytes back proves nothing — the
    // aperture routes to the video device, not RAM, so the read answers 0
    // either way; the discriminating fact is where SS:SP now points.
    let stack_top = usize::from(cpu.regs.ss) * 16 + usize::from(cpu.regs.sp());
    assert!(
        (0x9_FC00..=0x9_FFFF).contains(&stack_top),
        "the pushed frame at {stack_top:#X} must stay inside the EBDA"
    );
}

#[test]
fn test_simulate_real_mode_int_uses_stub_return_frame() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    memory.write_u16(0x10 * 4, 0x1234);
    memory.write_u16(0x10 * 4 + 2, 0x1000); // Use non-HLE segment for testing V86 entry

    let rm_struct_addr = 0x52480_usize;
    memory.write_u16(rm_struct_addr + 32, 0x0202); // FLAGS
    memory.write_u16(rm_struct_addr + 42, 0xCAFE); // IP (ignored)
    memory.write_u16(rm_struct_addr + 44, 0xBEEF); // CS (ignored)
    memory.write_u16(rm_struct_addr + 46, 0x1000); // SP
    memory.write_u16(rm_struct_addr + 48, 0x2000); // SS

    cpu.regs.set_ax(0x0300);
    cpu.regs.set_bl(0x10);
    cpu.regs.set_cx(0);
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.set_di((rm_struct_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let frame_addr = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.sp());
    assert_eq!(memory.read_u16(frame_addr), 0x0000); // IP
    assert_eq!(memory.read_u16(frame_addr + 2), 0x0000); // CS
    assert_eq!(memory.read_u16(frame_addr + 4), 0x0202); // FLAGS
}

#[test]
fn test_call_real_mode_far_procedure() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up a dummy far procedure at 0x1000:0x0100
    // Just put an IRET (0xCF) there to prevent crashes
    memory.write_u8(0x10100, 0xCB); // RETF

    // Set up real mode call structure
    let rm_struct_addr = 0x50000_usize;
    memory.write_u32(rm_struct_addr + 28, 0); // EAX
    memory.write_u16(rm_struct_addr + 32, 0x0202); // Flags
    memory.write_u16(rm_struct_addr + 42, 0x0100); // IP
    memory.write_u16(rm_struct_addr + 44, 0x1000); // CS
    memory.write_u16(rm_struct_addr + 46, 0); // SP
    memory.write_u16(rm_struct_addr + 48, 0); // SS

    cpu.regs.set_ax(0x0301);
    cpu.regs.set_cx(0);
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.edi = (rm_struct_addr & 0xF) as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_call_real_mode_far_procedure_resets_if_with_bh_flag() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    memory.write_u8(0x10900, 0xCB); // RETF

    let rm_struct_addr = 0x52500_usize;
    memory.write_u16(rm_struct_addr + 32, 0x0202); // Flags with IF set
    memory.write_u16(rm_struct_addr + 42, 0x0900);
    memory.write_u16(rm_struct_addr + 44, 0x1000);
    memory.write_u16(rm_struct_addr + 46, 0);
    memory.write_u16(rm_struct_addr + 48, 0);

    cpu.regs.set_ax(0x0301);
    cpu.regs.set_bh(1); // Reset IF
    cpu.regs.set_cx(0);
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.edi = (rm_struct_addr & 0xF) as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::INTERRUPT),
        "BH reset IF should clear IF in V86 flags"
    );
}

#[test]
fn test_simulate_real_mode_int_rejects_invalid_bh_bits() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let rm_struct_addr = 0x52600_usize;
    cpu.regs.set_ax(0x0300);
    cpu.regs.set_bl(0x10);
    cpu.regs.set_bh(0x02); // Invalid (only bit 0 allowed)
    cpu.regs.set_cx(0);
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.edi = (rm_struct_addr & 0xF) as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_simulate_real_mode_int_resets_if_with_bh_flag() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    memory.write_u16(0x10 * 4, 0x1234);
    memory.write_u16(0x10 * 4 + 2, 0x1000); // Use non-HLE segment for testing V86 entry

    let rm_struct_addr = 0x52700_usize;
    memory.write_u16(rm_struct_addr + 32, 0x0202); // Flags with IF set
    memory.write_u16(rm_struct_addr + 46, 0);
    memory.write_u16(rm_struct_addr + 48, 0);

    cpu.regs.set_ax(0x0300);
    cpu.regs.set_bl(0x10);
    cpu.regs.set_bh(1); // Reset IF
    cpu.regs.set_cx(0);
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.edi = (rm_struct_addr & 0xF) as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::INTERRUPT),
        "BH reset IF should clear IF in V86 flags"
    );
}

#[test]
fn test_call_real_mode_iret_rejects_invalid_bh_bits() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let rm_struct_addr = 0x52680_usize;
    memory.write_u16(rm_struct_addr + 42, 0x0A00);
    memory.write_u16(rm_struct_addr + 44, 0x1000);
    memory.write_u16(rm_struct_addr + 46, 0);
    memory.write_u16(rm_struct_addr + 48, 0);

    cpu.regs.set_ax(0x0302);
    cpu.regs.set_bh(0x02); // Invalid (only bit 0 allowed)
    cpu.regs.set_cx(0);
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.edi = (rm_struct_addr & 0xF) as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_call_real_mode_iret_resets_if_with_bh_flag() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    memory.write_u8(0x10B00, 0xCF); // IRET

    let rm_struct_addr = 0x52780_usize;
    memory.write_u16(rm_struct_addr + 32, 0x0202); // Flags with IF set
    memory.write_u16(rm_struct_addr + 42, 0x0B00);
    memory.write_u16(rm_struct_addr + 44, 0x1000);
    memory.write_u16(rm_struct_addr + 46, 0);
    memory.write_u16(rm_struct_addr + 48, 0);

    cpu.regs.set_ax(0x0302);
    cpu.regs.set_bh(1); // Reset IF
    cpu.regs.set_cx(0);
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.edi = (rm_struct_addr & 0xF) as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::INTERRUPT),
        "BH reset IF should clear IF in V86 flags"
    );
}

#[test]
fn test_call_real_mode_far_procedure_invalid_struct_selector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.protected_mode = true;
    cpu.regs.cr0 = 1;

    cpu.regs.set_ax(0x0301);
    cpu.regs.set_cx(0);
    cpu.regs.es = 0x0017; // LDT selector, not allocated
    cpu.regs.set_di(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_call_real_mode_far_procedure_32bit_stack_words() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1;
    cpu.regs.ss = 0x1000;
    cpu.regs.esp = 0x0200;

    let pm_linear = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.esp as u16);
    memory.write_u16(pm_linear, 0x1111);

    memory.write_u8(0x10500, 0xCB); // RETF

    let rm_struct_addr = 0x52100_usize;
    memory.write_u16(rm_struct_addr + 42, 0x0500);
    memory.write_u16(rm_struct_addr + 44, 0x1000);
    memory.write_u16(rm_struct_addr + 46, 0x3000);
    memory.write_u16(rm_struct_addr + 48, 0x4000);

    cpu.regs.set_ax(0x0301);
    cpu.regs.ecx = 1;
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.edi = (rm_struct_addr & 0xF) as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_call_real_mode_far_procedure_zero_ss_nonzero_sp() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    memory.write_u8(0x10700, 0xCB); // RETF

    let rm_struct_addr = 0x52180_usize;
    memory.write_u16(rm_struct_addr + 42, 0x0700);
    memory.write_u16(rm_struct_addr + 44, 0x1000);
    memory.write_u16(rm_struct_addr + 46, 0xFFFC);
    memory.write_u16(rm_struct_addr + 48, 0x0000);

    cpu.regs.set_ax(0x0301);
    cpu.regs.set_cx(0);
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.set_di((rm_struct_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_call_real_mode_far_procedure_custom_stack() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    memory.write_u8(0x10300, 0xCB); // RETF

    let rm_struct_addr = 0x51080_usize;
    memory.write_u16(rm_struct_addr + 42, 0x0300);
    memory.write_u16(rm_struct_addr + 44, 0x1000);
    memory.write_u16(rm_struct_addr + 46, 0xFFFC);
    memory.write_u16(rm_struct_addr + 48, 0x2000);

    cpu.regs.set_ax(0x0301);
    cpu.regs.set_cx(0);
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.edi = (rm_struct_addr & 0xF) as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_should_exit_v86_on_retf_after_far_call() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    memory.write_u8(0x10A00, 0xCB); // RETF

    let rm_struct_addr = 0x52380_usize;
    memory.write_u16(rm_struct_addr + 42, 0x0A00);
    memory.write_u16(rm_struct_addr + 44, 0x1000);

    cpu.regs.set_ax(0x0301);
    cpu.regs.set_cx(0);
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.edi = (rm_struct_addr & 0xF) as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(dpmi.should_exit_v86_on_retf());
    assert!(!dpmi.should_exit_v86_on_iret());
}

#[test]
fn test_call_real_mode_iret_procedure() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up a dummy IRET procedure
    memory.write_u8(0x10200, 0xCF); // IRET

    // Set up real mode call structure
    let rm_struct_addr = 0x50000_usize;
    memory.write_u16(rm_struct_addr + 42, 0x0200); // IP
    memory.write_u16(rm_struct_addr + 44, 0x1000); // CS
    memory.write_u16(rm_struct_addr + 46, 0); // SP
    memory.write_u16(rm_struct_addr + 48, 0); // SS

    cpu.regs.set_ax(0x0302);
    cpu.regs.set_cx(0);
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.edi = (rm_struct_addr & 0xF) as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_call_real_mode_iret_procedure_custom_stack() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    memory.write_u8(0x10400, 0xCF); // IRET

    let rm_struct_addr = 0x51100_usize;
    memory.write_u16(rm_struct_addr + 42, 0x0400);
    memory.write_u16(rm_struct_addr + 44, 0x1000);
    memory.write_u16(rm_struct_addr + 46, 0xFFFC);
    memory.write_u16(rm_struct_addr + 48, 0x2000);

    cpu.regs.set_ax(0x0302);
    cpu.regs.set_cx(0);
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.edi = (rm_struct_addr & 0xF) as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_call_real_mode_iret_procedure_invalid_struct_selector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.protected_mode = true;
    cpu.regs.cr0 = 1;

    cpu.regs.set_ax(0x0302);
    cpu.regs.set_cx(0);
    cpu.regs.es = 0x0017; // LDT selector, not allocated
    cpu.regs.set_di(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_call_real_mode_iret_procedure_32bit_stack_words() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1;
    cpu.regs.ss = 0x1000;
    cpu.regs.esp = 0x0300;

    let pm_linear = Memory::seg_offset_to_linear(cpu.regs.ss, cpu.regs.esp as u16);
    memory.write_u16(pm_linear, 0x2222);

    memory.write_u8(0x10600, 0xCF); // IRET

    let rm_struct_addr = 0x52200_usize;
    memory.write_u16(rm_struct_addr + 42, 0x0600);
    memory.write_u16(rm_struct_addr + 44, 0x1000);
    memory.write_u16(rm_struct_addr + 46, 0x3000);
    memory.write_u16(rm_struct_addr + 48, 0x4000);

    cpu.regs.set_ax(0x0302);
    cpu.regs.ecx = 1;
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.edi = (rm_struct_addr & 0xF) as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_call_real_mode_iret_procedure_zero_ss_nonzero_sp() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    memory.write_u8(0x10800, 0xCF); // IRET

    let rm_struct_addr = 0x52280_usize;
    memory.write_u16(rm_struct_addr + 42, 0x0800);
    memory.write_u16(rm_struct_addr + 44, 0x1000);
    memory.write_u16(rm_struct_addr + 46, 0xFFFC);
    memory.write_u16(rm_struct_addr + 48, 0x0000);

    cpu.regs.set_ax(0x0302);
    cpu.regs.set_cx(0);
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.set_di((rm_struct_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_allocate_real_mode_callback() {
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
    assert_eq!(cpu.regs.cx(), 0xF200); // Callback segment
    let _ = cpu.regs.dx(); // Callback offset (can be 0)
}

#[test]
fn test_free_real_mode_callback() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate
    cpu.regs.set_ax(0x0303);
    cpu.regs.ds = 0x0008;
    cpu.regs.esi = 0x1000;
    cpu.regs.es = 0x0010;
    cpu.regs.edi = 0x2000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    let seg = cpu.regs.cx();
    let off = cpu.regs.dx();

    // Free
    cpu.regs.set_ax(0x0304);
    cpu.regs.set_cx(seg);
    cpu.regs.set_dx(off);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_free_real_mode_callback_invalid_address() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0304);
    cpu.regs.set_cx(0x1234);
    cpu.regs.set_dx(0x5678);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_CALLBACK);
}

#[test]
fn test_find_real_mode_callback() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0303);
    cpu.regs.ds = 0x0008;
    cpu.regs.esi = 0x1000;
    cpu.regs.es = 0x0010;
    cpu.regs.edi = 0x2000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    let seg = cpu.regs.cx();
    let off = cpu.regs.dx();

    assert!(dpmi.find_rm_callback(seg, off).is_some());
    assert!(dpmi.find_rm_callback(seg, off.wrapping_add(4)).is_none());
}

#[test]
fn test_find_real_mode_callback_segment_mismatch() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0303);
    cpu.regs.ds = 0x0008;
    cpu.regs.esi = 0x1000;
    cpu.regs.es = 0x0010;
    cpu.regs.edi = 0x2000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    let seg = cpu.regs.cx();
    let off = cpu.regs.dx();

    assert!(dpmi.find_rm_callback(seg.wrapping_add(1), off).is_none());
}

#[test]
fn test_free_real_mode_callback_segment_mismatch() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0303);
    cpu.regs.ds = 0x0008;
    cpu.regs.esi = 0x1000;
    cpu.regs.es = 0x0010;
    cpu.regs.edi = 0x2000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    let seg = cpu.regs.cx();
    let off = cpu.regs.dx();

    cpu.regs.set_ax(0x0304);
    cpu.regs.set_cx(seg.wrapping_add(1));
    cpu.regs.set_dx(off);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_CALLBACK);
}

#[test]
fn test_allocate_max_callbacks() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate callbacks until we run out
    let mut allocated = 0;
    for i in 0..32 {
        cpu.regs.set_ax(0x0303);
        cpu.regs.ds = 0x0008;
        cpu.regs.esi = 0x1000 + i * 0x100;
        cpu.regs.es = 0x0010;
        cpu.regs.edi = 0x2000 + i * 0x100;
        dpmi.int31(&mut cpu, &mut memory).unwrap();

        if cpu.regs.flags.contains(gp64_cpu::Flags::CARRY) {
            break;
        }
        allocated += 1;
    }

    // Should have allocated at least some callbacks
    assert!(allocated > 0);
}

#[test]
fn test_allocate_callback_exhausted_returns_correct_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate all 16 callbacks
    for i in 0..16 {
        cpu.regs.set_ax(0x0303);
        cpu.regs.ds = 0x0008;
        cpu.regs.esi = 0x1000 + i * 0x100;
        cpu.regs.es = 0x0010;
        cpu.regs.edi = 0x2000 + i * 0x100;
        dpmi.int31(&mut cpu, &mut memory).unwrap();
        assert!(
            !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "Callback {i} should succeed"
        );
    }

    // Try to allocate one more - should fail with DPMI_ERR_CALLBACK_UNAVAIL (0x8015)
    cpu.regs.set_ax(0x0303);
    cpu.regs.ds = 0x0008;
    cpu.regs.esi = 0x9000;
    cpu.regs.es = 0x0010;
    cpu.regs.edi = 0xA000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_CALLBACK_UNAVAIL);
}
fn setup_gdt_data_descriptor(
    dpmi: &Dpmi,
    cpu: &mut Cpu,
    memory: &mut Memory,
    selector: u16,
    limit: u32,
) {
    let gdt_base = 0x2000;
    let index = (selector >> 3) as usize;
    cpu.regs.gdtr.base = gdt_base;
    cpu.regs.gdtr.limit = ((index + 1) * 8 - 1) as u16;
    dpmi.write_descriptor_raw(memory, gdt_base as usize, index, 0, limit, 0xF2, 0x00);
}
