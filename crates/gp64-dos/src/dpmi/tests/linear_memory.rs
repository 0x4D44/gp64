use super::*;

#[test]
fn test_get_page_attributes_before_block_fails() {
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
    cpu.regs.ebx = 0x2000; // Offset beyond block size
    cpu.regs.ecx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_get_page_attributes_buffer_overflow_reports_error() {
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
    cpu.regs.cr0 = 1;

    let buf_base = memory.ram_size().saturating_sub(1) as u32;
    dpmi.selectors.push(Selector {
        selector: 0x0007,
        base: buf_base,
        limit: 0xFFFF_FFFF,
        access: 0xF2,
        flags: 0,
        lock_count: 0,
    });

    cpu.regs.es = 0x0007;
    cpu.regs.edx = 0;
    cpu.regs.set_ax(0x0506);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_LINEAR);
}

#[test]
fn test_get_page_attributes_zero_count_fails() {
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
    cpu.regs.ecx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
    assert_eq!(cpu.regs.ecx, 0);
}

#[test]
fn test_set_page_attributes() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client

    // First allocate a linear memory block with uncommitted pages
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x4000; // 16KB (4 pages)
    cpu.regs.ebx = 0;
    cpu.regs.edx = 0; // Don't commit pages
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    // Verify pages are uncommitted via get
    cpu.regs.es = 0x1000;
    cpu.regs.edx = 0;
    cpu.regs.set_ax(0x0506);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 2;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(memory.read_u16(0x10000) & 1, 0); // Not present

    // Set page attributes (0x0507) to commit pages
    memory.write_u16(0x10000, 0x0001); // Type=1 committed
    memory.write_u16(0x10002, 0x0001);
    cpu.regs.es = 0x1000;
    cpu.regs.edx = 0;
    cpu.regs.set_ax(0x0507);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 2;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify pages are now committed
    cpu.regs.es = 0x1000;
    cpu.regs.edx = 0x100; // Different buffer offset
    cpu.regs.set_ax(0x0506);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 2;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(memory.read_u16(0x10100), 0x0001);
    assert_eq!(memory.read_u16(0x10102), 0x0001);
}

#[test]
fn test_set_page_attributes_unaligned_base_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handle = cpu.regs.esi;

    cpu.regs.es = 0x1000;
    cpu.regs.set_dx(0);
    cpu.regs.set_ax(0x0507);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 1;
    cpu.regs.ecx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_page_attributes_invalid_buffer_selector_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handle = cpu.regs.esi;

    dpmi.protected_mode = true;
    cpu.regs.es = 0x0007; // LDT selector without entry
    cpu.regs.set_dx(0);
    cpu.regs.set_ax(0x0507);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_page_attributes_range_exceeds_block_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handle = cpu.regs.esi;

    cpu.regs.es = 0x1000;
    cpu.regs.set_dx(0);
    cpu.regs.set_ax(0x0507);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 3;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_page_attributes_before_block_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handle = cpu.regs.esi;

    cpu.regs.es = 0x1000;
    cpu.regs.set_dx(0);
    cpu.regs.set_ax(0x0507);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0x2000; // Offset beyond block size
    cpu.regs.ecx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_page_attributes_zero_count_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handle = cpu.regs.esi;

    cpu.regs.es = 0x1000;
    cpu.regs.set_dx(0);
    cpu.regs.set_ax(0x0507);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
    assert_eq!(cpu.regs.ecx, 0);
}

#[test]
fn test_set_page_attributes_buffer_overflow_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handle = cpu.regs.esi;

    dpmi.protected_mode = true;
    cpu.regs.cr0 = 1;

    let buf_base = memory.ram_size().saturating_sub(1) as u32;
    dpmi.selectors.push(Selector {
        selector: 0x0007,
        base: buf_base,
        limit: 0xFFFF_FFFF,
        access: 0xF2,
        flags: 0,
        lock_count: 0,
    });

    cpu.regs.es = 0x0007;
    cpu.regs.set_dx(0);
    cpu.regs.set_ax(0x0507);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_LINEAR);
}

#[test]
fn test_set_page_attributes_mapped_type_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handle = cpu.regs.esi;

    memory.write_u16(0x10000, 0x0002); // Type=2 (mapped) - unsupported
    cpu.regs.es = 0x1000;
    cpu.regs.set_dx(0);
    cpu.regs.set_ax(0x0507);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_page_attributes_uncommitted_pages() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handle = cpu.regs.esi;

    memory.write_u16(0x10000, 0x0000);
    cpu.regs.es = 0x1000;
    cpu.regs.set_dx(0);
    cpu.regs.set_ax(0x0507);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_map_device_in_memory_block_maps_physical() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;
    let base = cpu.regs.ebx;

    cpu.regs.set_ax(0x0508);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 1;
    cpu.regs.edx = 0x2000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dpmi.physical_mappings.len(), 1);
    let mapping = &dpmi.physical_mappings[0];
    assert_eq!(mapping.physical, 0x2000);
    assert_eq!(mapping.linear, base);
    assert_eq!(mapping.size, 0x1000);

    let block = dpmi
        .linear_memory_blocks
        .iter()
        .find(|b| b.handle == handle)
        .expect("linear memory block exists");
    assert!(block.is_page_committed(0));
}

/// MDD-BUG-CRU-01488: the VBE aperture sits above the DOS RAM ceiling
/// (1 GiB). DPMI 0800h must map it before 4F02h has enabled the decoder,
/// and 0508h must map it at all.
#[test]
fn dpmi_maps_the_lfb_aperture_before_the_mode_is_set() {
    let lfb = gp64_memory::LFB_BASE as u32;
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    assert!(
        !memory.is_lfb_address(lfb as usize),
        "the decoder starts disabled"
    );

    cpu.regs.set_ax(0x0800);
    cpu.regs.set_bx((lfb >> 16) as u16);
    cpu.regs.set_cx(lfb as u16);
    cpu.regs.set_si(0x0040); // 4 MiB
    cpu.regs.set_di(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "0800h maps PhysBasePtr before 4F02h, AX={:#06X}",
        cpu.regs.ax()
    );
    assert_eq!(dpmi.physical_mappings[0].physical, lfb);

    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handle = cpu.regs.esi;
    cpu.regs.set_ax(0x0508);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 2;
    cpu.regs.edx = lfb;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "0508h maps the LFB aperture into a block, AX={:#06X}",
        cpu.regs.ax()
    );
    assert_eq!(dpmi.physical_mappings[0].physical, lfb);
}

#[test]
fn test_map_device_in_memory_block_invalid_handle_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client

    cpu.regs.set_ax(0x0508);
    cpu.regs.esi = 0x1234;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 1;
    cpu.regs.edx = 0x2000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_HANDLE);
}

/// MDD-BUG-CRU-01402: freeing a 1.0 block (0502h) releases the mappings
/// made inside it. Seventy allocate/map/free rounds used to fill the 64-slot
/// table and refuse every later map.
#[test]
fn freeing_a_linear_block_releases_its_physical_mappings() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;

    for round in 0..70 {
        cpu.regs.set_ax(0x0504);
        cpu.regs.ebx = 0;
        cpu.regs.ecx = 0x1000;
        cpu.regs.edx = 1;
        dpmi.int31(&mut cpu, &mut memory).unwrap();
        assert!(
            !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "0504h round {round}"
        );
        let (handle, base) = (cpu.regs.esi, cpu.regs.ebx);

        cpu.regs.set_ax(0x0509);
        cpu.regs.esi = handle;
        cpu.regs.ebx = 0;
        cpu.regs.ecx = 1;
        cpu.regs.edx = 0x0008_0000;
        dpmi.int31(&mut cpu, &mut memory).unwrap();
        assert!(
            !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "0509h round {round}: AX={:#06x}",
            cpu.regs.ax()
        );

        cpu.regs.set_ax(0x0502);
        cpu.regs.esi = handle;
        dpmi.int31(&mut cpu, &mut memory).unwrap();
        assert!(
            !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "0502h round {round}"
        );
        assert!(!memory.linear_range_has_physical_mapping(base, 0x1000));
    }
    assert_eq!(dpmi.physical_mappings.len(), 0);
}

/// MDD-BUG-CRU-01402: a 0505h resize that MOVES the block carries the
/// mappings inside it to the new base; the old linear no longer redirects.
#[test]
fn moving_a_linear_block_relocates_its_physical_mappings() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;
    let allocate = |dpmi: &mut Dpmi, cpu: &mut Cpu, memory: &mut Memory| {
        cpu.regs.set_ax(0x0504);
        cpu.regs.ebx = 0;
        cpu.regs.ecx = 0x2000;
        cpu.regs.edx = 1;
        dpmi.int31(cpu, memory).unwrap();
        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        (cpu.regs.esi, cpu.regs.ebx)
    };
    let (handle, old_base) = allocate(&mut dpmi, &mut cpu, &mut memory);
    // A neighbour directly behind the block, so growing it must move it.
    let (_, neighbour) = allocate(&mut dpmi, &mut cpu, &mut memory);
    assert_eq!(
        neighbour,
        old_base + 0x2000,
        "the neighbour must pin the block"
    );

    cpu.regs.set_ax(0x0509);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0x1000;
    cpu.regs.ecx = 1;
    cpu.regs.edx = 0x0008_0000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    cpu.regs.set_ax(0x0505);
    cpu.regs.esi = handle;
    cpu.regs.ecx = 0x8000;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let new_base = cpu.regs.ebx;
    assert_ne!(new_base, old_base, "the resize must have moved the block");

    assert_eq!(dpmi.physical_mappings.len(), 1);
    assert_eq!(dpmi.physical_mappings[0].linear, new_base + 0x1000);
    assert!(memory.linear_range_has_physical_mapping(new_base + 0x1000, 0x1000));
    assert!(!memory.linear_range_has_physical_mapping(old_base + 0x1000, 0x1000));
}

#[test]
fn test_map_conventional_memory_in_memory_block_maps_physical() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;
    let base = cpu.regs.ebx;

    cpu.regs.set_ax(0x0509);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0x1000;
    cpu.regs.ecx = 1;
    cpu.regs.edx = 0x3000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dpmi.physical_mappings.len(), 1);
    let mapping = &dpmi.physical_mappings[0];
    assert_eq!(mapping.physical, 0x3000);
    assert_eq!(mapping.linear, base + 0x1000);
    assert_eq!(mapping.size, 0x1000);
}

#[test]
fn test_int31_0800_physical_mapping_full_table_returns_error_not_panic() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    fill_physical_mappings_to_capacity(&mut dpmi);

    cpu.regs.set_ax(0x0800);
    cpu.regs.set_bx(0x000E); // physical 0xE0000 (does not overlap the fill)
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_si(0x0000);
    cpu.regs.set_di(0x1000); // size 0x1000
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_LINEAR_UNAVAILABLE);
}

#[test]
fn test_int31_0508_map_device_full_table_returns_error_not_panic() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1; // 32-bit client

    // Allocate a linear block to get a valid handle.
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    fill_physical_mappings_to_capacity(&mut dpmi);

    cpu.regs.set_ax(0x0508);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 1;
    cpu.regs.edx = 0x2000; // does not overlap the fill
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_LINEAR_UNAVAILABLE);
}

#[test]
fn test_int31_0509_map_conventional_full_table_returns_error_not_panic() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1; // 32-bit client

    // Allocate a linear block to get a valid handle.
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    fill_physical_mappings_to_capacity(&mut dpmi);

    cpu.regs.set_ax(0x0509);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0x1000;
    cpu.regs.ecx = 1;
    cpu.regs.edx = 0x3000; // does not overlap the fill
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_LINEAR_UNAVAILABLE);
}

#[test]
fn test_int31_0800_respects_shared_memory_mapping_capacity() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    install_memory_only_physical_mapping(&mut memory);

    for index in 0..63u32 {
        let physical = 0x0010_0000 + index * 0x1000;
        cpu.regs.set_ax(0x0800);
        cpu.regs.set_bx((physical >> 16) as u16);
        cpu.regs.set_cx(physical as u16);
        cpu.regs.set_si(0);
        cpu.regs.set_di(0x1000);
        dpmi.int31(&mut cpu, &mut memory).unwrap();
        assert!(
            !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "mapping {} should fit both ledgers",
            index
        );
    }

    let rejected_linear = dpmi.find_free_linear_range(0x1000, 0x1000).unwrap();
    let before_dpmi = dpmi.physical_mappings.len();
    assert_eq!(before_dpmi, 63);
    assert_eq!(memory.map_linear_physical(rejected_linear), None);

    let physical = 0x0010_0000 + 63 * 0x1000;
    cpu.regs.set_ax(0x0800);
    cpu.regs.set_bx((physical >> 16) as u16);
    cpu.regs.set_cx(physical as u16);
    cpu.regs.set_si(0);
    cpu.regs.set_di(0x1000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_LINEAR_UNAVAILABLE);
    assert_eq!(dpmi.physical_mappings.len(), before_dpmi);
    assert_eq!(memory.map_linear_physical(rejected_linear), None);
    assert_eq!(memory.map_linear_physical(0xFF00_0000), Some(0x03F0_0000));
}

#[test]
fn test_int31_0508_respects_shared_memory_mapping_capacity() {
    assert_block_mapping_respects_shared_memory_capacity(0x0508);
}

#[test]
fn test_int31_0509_respects_shared_memory_mapping_capacity() {
    assert_block_mapping_respects_shared_memory_capacity(0x0509);
}

#[test]
fn test_resize_linear_memory_zero_size_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handle = ((cpu.regs.si() as u32) << 16) | cpu.regs.di() as u32;

    cpu.regs.set_ax(0x0505);
    cpu.regs.ecx = 0;
    cpu.regs.edx = 0;
    cpu.regs.set_si((handle >> 16) as u16);
    cpu.regs.set_di(handle as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_resize_linear_memory_commit_new_grows() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1;
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let base = cpu.regs.ebx;
    let handle = ((cpu.regs.si() as u32) << 16) | cpu.regs.di() as u32;

    cpu.regs.set_ax(0x0505);
    cpu.regs.ecx = 0x3000;
    cpu.regs.edx = 1; // commit_new
    cpu.regs.set_si((handle >> 16) as u16);
    cpu.regs.set_di(handle as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ebx, base);
}

#[test]
fn test_get_memory_block_size_and_base_returns_block() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // DPMI 1.0 linear memory requires 32-bit mode

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x3000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;
    let base = cpu.regs.ebx;

    cpu.regs.set_ax(0x050A);
    cpu.regs.esi = handle;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let returned_base = cpu.regs.ebx;
    let returned_size = cpu.regs.ecx;
    assert_eq!(returned_base, base);
    assert_eq!(returned_size, 0x3000);
}

#[test]
fn test_get_memory_block_size_and_base_invalid_handle_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // DPMI 1.0 linear memory requires 32-bit mode

    cpu.regs.set_ax(0x050A);
    cpu.regs.esi = 0x1234;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_HANDLE);
}

#[test]
fn test_get_memory_information_050b_populates_buffer() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let buf_addr = 0x50000usize;
    cpu.regs.set_ax(0x050B);
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let highest_linear = memory.read_u32(buf_addr + 0x24);
    assert_eq!(
        highest_linear,
        memory.ram_size() as u32 - 1,
        "Highest linear address should match RAM size"
    );
    assert_eq!(memory.read_u32(buf_addr + 0x2C), 0x1000);
    assert_eq!(memory.read_u32(buf_addr + 0x30), 0x1000);
    assert_eq!(memory.read_u8(buf_addr + 0x34), 0);
}

#[test]
fn test_resize_linear_memory_shrink_in_place() {
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

    let base = cpu.regs.ebx;
    let handle = ((cpu.regs.si() as u32) << 16) | cpu.regs.di() as u32;

    cpu.regs.set_ax(0x0505);
    cpu.regs.ecx = 0x2000;
    cpu.regs.edx = 0;
    cpu.regs.set_si((handle >> 16) as u16);
    cpu.regs.set_di(handle as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ebx, base);
}

#[test]
fn test_resize_linear_memory_shrink_with_commit_new() {
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

    let base = cpu.regs.ebx;
    let handle = ((cpu.regs.si() as u32) << 16) | cpu.regs.di() as u32;

    cpu.regs.set_ax(0x0505);
    cpu.regs.ecx = 0x2000;
    cpu.regs.edx = 1;
    cpu.regs.set_si((handle >> 16) as u16);
    cpu.regs.set_di(handle as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ebx, base);
}

#[test]
fn test_resize_linear_memory_moves_and_copies_pages() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let handle = 0x2001;
    dpmi.linear_memory_blocks
        .push(LinearMemoryBlock::new(handle, 0x200000, 0x2000, true));
    dpmi.linear_memory_blocks
        .push(LinearMemoryBlock::new(0x2002, 0x202000, 0x2000, true));
    memory.ensure_ram_size(0x202000);
    memory.write_u8(0x200000, 0xAA);
    memory.write_u8(0x201000, 0xBB);

    cpu.regs.set_ax(0x0505);
    cpu.regs.ecx = 0x4000;
    cpu.regs.edx = 1;
    cpu.regs.set_si((handle >> 16) as u16);
    cpu.regs.set_di(handle as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let new_base = cpu.regs.ebx;
    assert_ne!(new_base, 0x200000);
    assert_eq!(memory.read_u8(new_base as usize), 0xAA);
    assert_eq!(memory.read_u8((new_base + 0x1000) as usize), 0xBB);
}

#[test]
fn test_resize_linear_memory_moves_and_copies_partial_page() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let handle = 0x4001;
    dpmi.linear_memory_blocks
        .push(LinearMemoryBlock::new(handle, 0x200000, 0x1001, true));
    dpmi.linear_memory_blocks
        .push(LinearMemoryBlock::new(0x4002, 0x201000, 0x2000, true));
    memory.ensure_ram_size(0x202000);
    memory.write_u8(0x200000, 0xAA);
    memory.write_u8(0x201000, 0xBB);

    cpu.regs.set_ax(0x0505);
    cpu.regs.ecx = 0x3000;
    cpu.regs.edx = 1;
    cpu.regs.set_si((handle >> 16) as u16);
    cpu.regs.set_di(handle as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let new_base = cpu.regs.ebx;
    assert_ne!(new_base, 0x200000);
    assert_eq!(memory.read_u8(new_base as usize), 0xAA);
    assert_eq!(memory.read_u8((new_base + 0x1000) as usize), 0xBB);
}

#[test]
fn test_resize_linear_memory_moves_without_commit() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let handle = 0x3001;
    dpmi.linear_memory_blocks
        .push(LinearMemoryBlock::new(handle, 0x200000, 0x2000, false));
    dpmi.linear_memory_blocks
        .push(LinearMemoryBlock::new(0x3002, 0x202000, 0x2000, true));

    cpu.regs.set_ax(0x0505);
    cpu.regs.ecx = 0x4000;
    cpu.regs.edx = 0;
    cpu.regs.set_si((handle >> 16) as u16);
    cpu.regs.set_di(handle as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_ne!(cpu.regs.ebx, 0x200000);
}

#[test]
fn test_get_page_attributes_out_of_range() {
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
    cpu.regs.set_dx(0);
    cpu.regs.set_ax(0x0506);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0x3000;
    cpu.regs.ecx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_page_attributes_out_of_range() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handle = cpu.regs.esi;

    cpu.regs.es = 0x1000;
    cpu.regs.set_dx(0);
    cpu.regs.set_ax(0x0507);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0x3000;
    cpu.regs.ecx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_segment_to_descriptor() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0002);
    cpu.regs.set_bx(0xB800); // Video segment
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    let sel = cpu.regs.ax();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify base is B8000
    cpu.regs.set_ax(0x0006);
    cpu.regs.set_bx(sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    let base = ((cpu.regs.cx() as u32) << 16) | (cpu.regs.dx() as u32);
    assert_eq!(base, 0xB8000);
}

#[test]
fn test_segment_to_descriptor_caches_selector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0002);
    cpu.regs.set_bx(0xB800);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let first_sel = cpu.regs.ax();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dpmi.selectors.len(), 1);

    cpu.regs.set_ax(0x0002);
    cpu.regs.set_bx(0xB800);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let second_sel = cpu.regs.ax();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(first_sel, second_sel);
    assert_eq!(dpmi.selectors.len(), 1);
}

#[test]
fn test_segment_to_descriptor_overflow_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.next_selector = 0xFFFC;
    cpu.regs.set_ax(0x0002);
    cpu.regs.set_bx(0xB800);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_DESC_UNAVAILABLE);
}

#[test]
fn test_create_alias_descriptor() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate a selector first
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let code_sel = cpu.regs.ax();

    // Set its base
    cpu.regs.set_ax(0x0007);
    cpu.regs.set_bx(code_sel);
    cpu.regs.set_cx(0x0010);
    cpu.regs.set_dx(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Create alias
    cpu.regs.set_ax(0x000A);
    cpu.regs.set_bx(code_sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    let alias_sel = cpu.regs.ax();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_ne!(alias_sel, code_sel);

    // Verify alias has same base
    cpu.regs.set_ax(0x0006);
    cpu.regs.set_bx(alias_sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    let base = ((cpu.regs.cx() as u32) << 16) | (cpu.regs.dx() as u32);
    assert_eq!(base, 0x00100000);
}

#[test]
fn test_create_alias_descriptor_allocation_failure() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate a selector to alias.
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let code_sel = cpu.regs.ax();

    // Force alias allocation to fail.
    dpmi.next_selector = 0xFFFC;
    cpu.regs.set_ax(0x000A);
    cpu.regs.set_bx(code_sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_DESC_UNAVAILABLE);
}

#[test]
fn test_create_alias_descriptor_missing_selector_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x000A);
    cpu.regs.set_bx(0x0017); // LDT selector, not allocated
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

/// Regression test for alias descriptor not copying base when selector is reused.
/// Bug: When creating an alias, if the new selector comes from the free_selectors
/// pool (a previously freed selector), the base was not being copied from the source.
#[test]
fn test_create_alias_copies_base_with_reused_selector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Step 1: Allocate two selectors
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(2);
    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel1 = cpu.regs.ax();
    let sel2 = sel1.wrapping_add(8);

    // Step 2: Set base on sel1 to a distinctive value
    let test_base = 0x05050505_u32;
    cpu.regs.set_ax(0x0007);
    cpu.regs.set_bx(sel1);
    cpu.regs.set_cx((test_base >> 16) as u16);
    cpu.regs.set_dx(test_base as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Step 3: Free sel2 (this puts it in free_selectors pool)
    cpu.regs.set_ax(0x0001);
    cpu.regs.set_bx(sel2);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Step 4: Create alias of sel1 - should reuse sel2 from free pool
    cpu.regs.set_ax(0x000A);
    cpu.regs.set_bx(sel1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let alias_sel = cpu.regs.ax();

    // Verify the alias was created from the free pool (should be sel2)
    assert_eq!(alias_sel, sel2, "Alias should reuse freed selector");

    // Step 5: Verify alias has the correct base (same as source)
    cpu.regs.set_ax(0x0006);
    cpu.regs.set_bx(alias_sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let alias_base = ((cpu.regs.cx() as u32) << 16) | (cpu.regs.dx() as u32);
    assert_eq!(
        alias_base, test_base,
        "Alias base {:08X} should match source base {:08X}",
        alias_base, test_base
    );

    // Also verify via internal API
    let internal_base = dpmi.get_selector_base(alias_sel);
    assert_eq!(
        internal_base,
        Some(test_base),
        "Internal alias base should match source base"
    );
}

/// Verify that alias base is independent of source after creation.
/// DPMI aliases are copies, not references - changing source should NOT affect alias.
#[test]
fn test_alias_base_is_independent_of_source() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate source selector
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let source_sel = cpu.regs.ax();

    // Set initial base on source
    let initial_base = 0x12340000_u32;
    cpu.regs.set_ax(0x0007);
    cpu.regs.set_bx(source_sel);
    cpu.regs.set_cx((initial_base >> 16) as u16);
    cpu.regs.set_dx(initial_base as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Create alias
    cpu.regs.set_ax(0x000A);
    cpu.regs.set_bx(source_sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let alias_sel = cpu.regs.ax();

    // Verify alias has the initial base
    assert_eq!(dpmi.get_selector_base(alias_sel), Some(initial_base));

    // Change source base to a different value
    let new_base = 0x56780000_u32;
    cpu.regs.set_ax(0x0007);
    cpu.regs.set_bx(source_sel);
    cpu.regs.set_cx((new_base >> 16) as u16);
    cpu.regs.set_dx(new_base as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Verify source has new base
    assert_eq!(dpmi.get_selector_base(source_sel), Some(new_base));

    // Verify alias STILL has the INITIAL base (NOT changed)
    assert_eq!(
        dpmi.get_selector_base(alias_sel),
        Some(initial_base),
        "Alias base should NOT change when source base changes"
    );
}

#[test]
fn test_get_set_descriptor_raw() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let sel = cpu.regs.ax();

    // Write raw descriptor to buffer
    let buf_addr = 0x50000_usize;
    memory.write_u16(buf_addr, 0xFFFF); // limit 15:0
    memory.write_u16(buf_addr + 2, 0x0000); // base 15:0
    memory.write_u8(buf_addr + 4, 0x10); // base 23:16
    memory.write_u8(buf_addr + 5, 0x92); // access (data, R/W)
    memory.write_u8(buf_addr + 6, 0x4F); // limit 19:16 + flags (D=1)
    memory.write_u8(buf_addr + 7, 0x00); // base 31:24

    // Set descriptor
    cpu.regs.set_ax(0x000C);
    cpu.regs.set_bx(sel);
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Get descriptor into different buffer
    let buf2_addr = 0x50010_usize;
    cpu.regs.set_ax(0x000B);
    cpu.regs.set_bx(sel);
    cpu.regs.es = (buf2_addr >> 4) as u16;
    cpu.regs.set_di((buf2_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Compare key fields
    assert_eq!(memory.read_u16(buf_addr), memory.read_u16(buf2_addr)); // limit
    assert_eq!(memory.read_u8(buf_addr + 5), memory.read_u8(buf2_addr + 5));
    // access
}

#[test]
fn test_set_descriptor_rejects_system_descriptor() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.cpl = 3;

    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let sel = cpu.regs.ax();

    let buf_addr = 0x50200_usize;
    memory.write_u16(buf_addr, 0xFFFF); // limit 15:0
    memory.write_u16(buf_addr + 2, 0x0000); // base 15:0
    memory.write_u8(buf_addr + 4, 0x00); // base 23:16
    memory.write_u8(buf_addr + 5, 0x89); // system descriptor (S=0)
    memory.write_u8(buf_addr + 6, 0x00); // limit 19:16 + flags
    memory.write_u8(buf_addr + 7, 0x00); // base 31:24

    cpu.regs.set_ax(0x000C);
    cpu.regs.set_bx(sel);
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_descriptor_forces_dpl_ring3() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.cpl = 3;

    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let sel = cpu.regs.ax();

    let buf_addr = 0x50280_usize;
    memory.write_u16(buf_addr, 0xFFFF); // limit 15:0
    memory.write_u16(buf_addr + 2, 0x0000); // base 15:0
    memory.write_u8(buf_addr + 4, 0x00); // base 23:16
    memory.write_u8(buf_addr + 5, 0x92); // data, P=1, DPL=0 (will be forced)
    memory.write_u8(buf_addr + 6, 0x00); // limit 19:16 + flags
    memory.write_u8(buf_addr + 7, 0x00); // base 31:24

    cpu.regs.set_ax(0x000C);
    cpu.regs.set_bx(sel);
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let entry = dpmi.selectors.iter().find(|s| s.selector == sel).unwrap();
    assert_eq!(entry.access, 0xF2);
}

/// Regression test: Verify D-bit is correctly stored when setting descriptor via INT 31h/000C.
/// The D-bit (bit 6 of byte 6) must be preserved so that loading CS with this selector
/// results in cs_cache.big=true for 32-bit code segments.
#[test]
fn test_set_descriptor_d_bit_preserved_for_32bit_code() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up DPMI LDT so we can write descriptors
    cpu.regs.ldt_base = 0x100000;
    cpu.regs.ldt_limit = 0x7FF; // 256 descriptors

    // Allocate a selector
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let sel = cpu.regs.ax();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Write descriptor with D=1 (32-bit code segment)
    // Byte 6 format: high nibble = flags (G=0, D=1, L=0, AVL=0) = 0x4
    //                low nibble = limit bits 19:16
    // So byte 6 = 0x4F means D=1 and limit bits = 0xF
    let buf_addr = 0x50000_usize;
    memory.write_u16(buf_addr, 0xFFFF); // limit 15:0
    memory.write_u16(buf_addr + 2, 0x0000); // base 15:0
    memory.write_u8(buf_addr + 4, 0x10); // base 23:16
    memory.write_u8(buf_addr + 5, 0xFA); // access: present, ring 3, code, readable
    memory.write_u8(buf_addr + 6, 0x4F); // D=1 (bit 6), limit 19:16 = 0xF
    memory.write_u8(buf_addr + 7, 0x00); // base 31:24

    // Set descriptor via INT 31h/000C
    cpu.regs.set_ax(0x000C);
    cpu.regs.set_bx(sel);
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify internal tracking has D-bit set (0x40 in byte format)
    let selector_entry = dpmi.selectors.iter().find(|s| s.selector == sel).unwrap();
    assert_eq!(
        selector_entry.flags & 0x40,
        0x40,
        "Internal selector flags should have D-bit (0x40) set"
    );

    // Verify is_selector_32bit returns true
    assert!(
        dpmi.is_selector_32bit(sel),
        "is_selector_32bit should return true for D=1 segment"
    );

    // Verify the LDT memory has D-bit correctly written
    let ldt_index = (sel >> 3) as usize;
    let desc_addr = cpu.regs.ldt_base as usize + ldt_index * 8;
    let byte6 = memory.read_u8(desc_addr + 6);
    assert_eq!(
        byte6 & 0x40,
        0x40,
        "LDT descriptor byte 6 should have D-bit (0x40) set, got {:02X}",
        byte6
    );

    // Most critical: verify CPU reads descriptor correctly and sets big=true
    cpu.regs.cr0 |= 1; // PE bit - enter protected mode
    let cache = cpu.read_descriptor(&memory, sel).unwrap();
    assert!(
        cache.big,
        "SegmentCache.big should be true after reading descriptor with D=1"
    );
}

/// Test that INT 31h/0009 (Set Descriptor Access Rights) respects the caller's flags
/// and does NOT auto-set the D-bit even for 32-bit clients.
///
/// We previously auto-set D-bit for code segments when the DPMI client was 32-bit.
/// exception handlers) that explicitly pass CH=0x00 meaning D=0 (16-bit code).
/// Auto-setting D=1 caused these segments to be decoded as 32-bit, leading to GPF.
#[test]
fn test_set_access_rights_respects_caller_flags_no_auto_d_bit() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up as 32-bit DPMI client
    cpu.regs.ldt_base = 0x100000;
    cpu.regs.ldt_limit = 0x7FF;
    cpu.regs.cpl = 3; // Ring 3 to match DPL=3 in access byte
    dpmi.cpu_mode = 1; // 32-bit mode
    dpmi.protected_mode = true;

    // Allocate a selector
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let sel = cpu.regs.ax();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Call INT 31h/0009 with:
    //   CL = 0xFA (code segment: present, DPL=3, code, readable)
    //   CH = 0x00 (NO D-bit - caller explicitly requests 16-bit code)
    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(sel);
    cpu.regs.set_cl(0xFA); // Code segment
    cpu.regs.set_ch(0x00); // No flags (D-bit not set - caller wants 16-bit)
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify that the D-bit was NOT set (respects caller's request)
    let selector_entry = dpmi.selectors.iter().find(|s| s.selector == sel).unwrap();
    assert_eq!(
        selector_entry.flags & 0x40,
        0x00,
        "D-bit should NOT be auto-set (flags={:02X})",
        selector_entry.flags
    );

    // Verify the LDT memory has D-bit NOT set
    let ldt_index = (sel >> 3) as usize;
    let desc_addr = cpu.regs.ldt_base as usize + ldt_index * 8;
    let byte6 = memory.read_u8(desc_addr + 6);
    assert_eq!(
        byte6 & 0x40,
        0x00,
        "LDT descriptor byte 6 should NOT have D-bit set, got {:02X}",
        byte6
    );

    // Verify CPU reads descriptor correctly and sets big=false
    cpu.regs.cr0 |= 1;
    let cache = cpu.read_descriptor(&memory, sel).unwrap();
    assert!(
        !cache.big,
        "SegmentCache.big should be false for 16-bit code segment"
    );
}

/// Test that INT 31h/0009 does NOT auto-set D-bit for 16-bit clients.
#[test]
fn test_set_access_rights_no_auto_d_bit_for_16bit_client() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up as 16-bit DPMI client
    cpu.regs.ldt_base = 0x100000;
    cpu.regs.ldt_limit = 0x7FF;
    dpmi.cpu_mode = 0; // 16-bit mode
    dpmi.protected_mode = true;

    // Allocate a selector
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let sel = cpu.regs.ax();

    // Call INT 31h/0009 with no D-bit
    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(sel);
    cpu.regs.set_cl(0xFA); // Code segment
    cpu.regs.set_ch(0x00); // No flags
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Verify D-bit was NOT set (16-bit client)
    let selector_entry = dpmi.selectors.iter().find(|s| s.selector == sel).unwrap();
    assert_eq!(
        selector_entry.flags & 0x40,
        0x00,
        "D-bit should NOT be set for 16-bit client"
    );
}

/// Test that INT 31h/0009 does NOT auto-set D-bit for data segments.
#[test]
fn test_set_access_rights_no_auto_d_bit_for_data_segment() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up as 32-bit DPMI client
    cpu.regs.ldt_base = 0x100000;
    cpu.regs.ldt_limit = 0x7FF;
    dpmi.cpu_mode = 1; // 32-bit mode
    dpmi.protected_mode = true;

    // Allocate a selector
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let sel = cpu.regs.ax();

    // Call INT 31h/0009 with data segment (bit 3 = 0)
    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(sel);
    cpu.regs.set_cl(0xF2); // Data segment (not code)
    cpu.regs.set_ch(0x00); // No flags
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Verify D-bit was NOT set (data segment)
    let selector_entry = dpmi.selectors.iter().find(|s| s.selector == sel).unwrap();
    assert_eq!(
        selector_entry.flags & 0x40,
        0x00,
        "D-bit should NOT be auto-set for data segments"
    );
}

#[test]
fn test_set_descriptor_granularity_from_buffer() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    let buf_addr = 0x52000_usize;
    memory.write_u16(buf_addr, 0x1234); // limit 15:0
    memory.write_u16(buf_addr + 2, 0x5678); // base 15:0
    memory.write_u8(buf_addr + 4, 0x9A); // base 23:16
    memory.write_u8(buf_addr + 5, 0xF2); // access
    memory.write_u8(buf_addr + 6, 0x80); // flags=0x8 (granularity), limit 19:16 = 0
    memory.write_u8(buf_addr + 7, 0xDC); // base 31:24

    cpu.regs.set_ax(0x000C);
    cpu.regs.set_bx(selector);
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let selector_entry = dpmi
        .selectors
        .iter()
        .find(|s| s.selector == selector)
        .unwrap();
    assert_eq!(selector_entry.flags & 0x80, 0x80);
    assert_eq!(selector_entry.limit, (0x1234u32 << 12) | 0x0FFF);
}

#[test]
fn test_get_descriptor_unallocated_selector_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let buf_addr = 0x54000_usize;
    cpu.regs.set_ax(0x000B);
    cpu.regs.set_bx(0x0017); // LDT selector, not allocated
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_descriptor_unallocated_selector_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let buf_addr = 0x55000_usize;
    memory.write_u16(buf_addr, 0xFFFF);
    memory.write_u16(buf_addr + 2, 0x0000);
    memory.write_u8(buf_addr + 4, 0x00);
    memory.write_u8(buf_addr + 5, 0x92);
    memory.write_u8(buf_addr + 6, 0x4F);
    memory.write_u8(buf_addr + 7, 0x00);

    cpu.regs.set_ax(0x000C);
    cpu.regs.set_bx(0x0017); // LDT selector, not allocated
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_get_descriptor_buffer_invalid_selector_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.protected_mode = true;
    cpu.regs.set_ax(0x000B);
    cpu.regs.set_bx(0x0017); // LDT selector, not allocated
    cpu.regs.es = 0x0017; // Invalid ES selector for buffer
    cpu.regs.set_di(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_descriptor_buffer_invalid_selector_reports_error() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.protected_mode = true;
    cpu.regs.set_ax(0x000C);
    cpu.regs.set_bx(0x0017); // LDT selector, not allocated
    cpu.regs.es = 0x0017; // Invalid ES selector for buffer
    cpu.regs.set_di(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_dpmi_pm_idt_initialization() {
    let dpmi = Dpmi::new();
    // Check that pm_idt is initialized with default entries
    for i in 0..256 {
        assert_eq!(dpmi.pm_idt[i].selector, 0x0008); // Default code selector
        assert_eq!(dpmi.pm_idt[i].offset, 0xFF00); // BIOS IRET stub
        assert!(dpmi.pm_idt[i].present);
    }
}

#[test]
fn test_dpmi_exception_handlers_initially_empty() {
    let dpmi = Dpmi::new();
    // Exception handlers should be default (not present) initially
    for i in 0..32 {
        assert!(!dpmi.exception_handlers[i].present);
    }
}

#[test]
fn test_dpmi_rm_callbacks_initially_not_in_use() {
    let dpmi = Dpmi::new();
    for cb in dpmi.rm_callbacks.iter() {
        assert!(!cb.in_use);
    }
}

/// Integration test simulating a real DPMI client startup sequence
#[test]
fn test_dpmi_client_simulation() {
    let _tracing = init_tracing();
    // Simulates a simple DPMI client startup sequence:
    // 1. Detect DPMI (INT 2Fh AX=1687h)
    // 2. Enter protected mode
    // 3. Allocate a descriptor
    // 4. Set up descriptor for video memory
    // 5. Allocate extended memory
    // 6. Set up real-mode call struct (for INT 10h simulation)
    // 7. Free resources and exit

    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // 1. Detect DPMI
    cpu.regs.set_ax(0x1687);
    dpmi.int2f_dpmi_detect(&mut cpu);
    assert_eq!(cpu.regs.ax(), 0); // DPMI available
    let entry_es = cpu.regs.es;
    let entry_di = cpu.regs.di();
    assert_eq!(entry_es, 0xF000);
    assert_eq!(entry_di, 0xFF10);

    // 2. Set up for mode switch (simulate CALL FAR to entry point)
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0xFFFE);
    let stack = 0x10000_usize;
    memory.write_u16(stack + 0xFFFC, 0x0100); // Return IP
    memory.write_u16(stack + 0xFFFE, 0x2000); // Return CS
    cpu.regs.set_sp(0xFFFC);
    cpu.regs.set_ax(1); // Request 32-bit mode
    cpu.regs.ds = 0x3000;
    cpu.regs.es = 0x4000;

    dpmi.enter_protected_mode(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.cr0 & 1 != 0);
    assert!(dpmi.in_protected_mode());

    // 3. Allocate descriptor
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let video_sel = cpu.regs.ax();

    // 4. Set up for video memory (B8000h for text mode)
    cpu.regs.set_ax(0x0007);
    cpu.regs.set_bx(video_sel);
    cpu.regs.set_cx(0x000B);
    cpu.regs.set_dx(0x8000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    cpu.regs.set_ax(0x0008);
    cpu.regs.set_bx(video_sel);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dx(0x0FFF); // 4KB limit
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Verify video base was set correctly
    cpu.regs.set_ax(0x0006);
    cpu.regs.set_bx(video_sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let base = ((cpu.regs.cx() as u32) << 16) | (cpu.regs.dx() as u32);
    assert_eq!(base, 0xB8000);

    // 5. Allocate extended memory (1MB)
    cpu.regs.set_ax(0x0501);
    cpu.regs.set_bx(0x0010);
    cpu.regs.set_cx(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let mem_handle_hi = cpu.regs.si();
    let mem_handle_lo = cpu.regs.di();

    // 6. Set up real-mode call struct for INT 10h (set mode)
    let rm_struct_addr = 0x50000_usize;
    // EAX at offset 0x1C (28 decimal)
    memory.write_u32(rm_struct_addr + 28, 0x0003); // EAX = 0003 (set 80x25 text mode)
    memory.write_u16(rm_struct_addr + 32, 0); // Flags
    memory.write_u16(rm_struct_addr + 46, 0); // SP = 0 (use internal stack)
    memory.write_u16(rm_struct_addr + 48, 0); // SS = 0

    cpu.regs.set_ax(0x0300);
    cpu.regs.set_bl(0x10); // INT 10h
    cpu.regs.set_cx(0); // No stack words to copy
    cpu.regs.es = (rm_struct_addr >> 4) as u16;
    cpu.regs.edi = (rm_struct_addr & 0xF) as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    // Note: actual execution would require running the real-mode code
    // For now we just verify no panic/error

    // 7. Free extended memory
    cpu.regs.set_ax(0x0502);
    cpu.regs.set_si(mem_handle_hi);
    cpu.regs.set_di(mem_handle_lo);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // 8. Free descriptor
    cpu.regs.set_ax(0x0001);
    cpu.regs.set_bx(video_sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_dpmi_v86_state_initial() {
    let dpmi = Dpmi::new();
    assert!(!dpmi.is_v86_active());
    assert!(!dpmi.should_exit_v86_on_iret());
}

#[test]
fn test_dpmi_v86_exit_on_retf_initial_false() {
    let dpmi = Dpmi::new();
    assert!(!dpmi.should_exit_v86_on_retf());
}

#[test]
fn test_dpmi_v86_exit_on_iret_requires_sentinel_return() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();

    dpmi.v86_active = true;
    dpmi.v86_exit_on_iret = true;

    cpu.regs.cs = 0x1234;
    cpu.regs.set_ip(0x5678);
    assert!(!dpmi.should_exit_v86_on_iret_state(&cpu));

    cpu.regs.cs = 0x0000;
    cpu.regs.set_ip(0x0000);
    assert!(dpmi.should_exit_v86_on_iret_state(&cpu));
}

#[test]
fn test_dpmi_v86_exit_on_retf_requires_sentinel_return() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();

    dpmi.v86_active = true;
    dpmi.v86_exit_on_retf = true;

    cpu.regs.cs = 0x00F0;
    cpu.regs.set_ip(0x1000);
    assert!(!dpmi.should_exit_v86_on_retf_state(&cpu));

    cpu.regs.cs = 0x0000;
    cpu.regs.set_ip(0x0000);
    assert!(dpmi.should_exit_v86_on_retf_state(&cpu));
}

#[test]
fn test_dpmi_v86_enter_exit() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up protected mode
    cpu.regs.cr0 = 1;
    dpmi.protected_mode = true;

    // Set up a minimal IVT for INT 10h
    // IVT[0x10] at address 0x40 should point to F000:1234
    memory.write_u16(0x40, 0x1234); // offset
    memory.write_u16(0x42, 0xF000); // segment

    // Set up PM state
    cpu.regs.cs = 0x0008;
    cpu.regs.set_ip(0x5000);
    cpu.regs.ss = 0x0010;
    cpu.regs.esp = 0x10000;
    cpu.regs.ds = 0x0018;
    cpu.regs.es = 0x0018;
    cpu.regs.fs = 0x0000;
    cpu.regs.gs = 0x0000;
    cpu.cs_bits = 32;
    cpu.regs.cs_cache = SegmentCache {
        big: true,
        present: true,
        is_code: true,
        ..SegmentCache::default()
    };

    // Create a RealModeCallStruct
    let rm_struct = RealModeCallStruct {
        edi: 0,
        esi: 0,
        ebp: 0,
        reserved: 0,
        ebx: 0,
        edx: 0,
        ecx: 0,
        eax: 0x0013, // Set video mode 13h
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

    // Write rm_struct to memory
    let struct_addr = 0x80000usize;
    rm_struct.write_to_memory(&mut memory, struct_addr);

    // Enter V86 mode for INT 10h
    dpmi.enter_v86_for_interrupt(&mut cpu, &memory, &rm_struct, struct_addr, 0x10)
        .unwrap();

    // Verify V86 state is active
    assert!(dpmi.is_v86_active());
    assert!(dpmi.should_exit_v86_on_iret());
    assert!(cpu.regs.is_v86_mode());

    // Verify CPU is set up for V86 execution
    assert_eq!(cpu.regs.cs, 0xF000);
    assert_eq!(cpu.regs.ip(), 0x1234);
    assert_eq!(cpu.regs.eax, 0x0013);

    // Exit V86 mode
    dpmi.exit_v86(&mut cpu, &mut memory);

    // Verify state is restored
    assert!(!dpmi.is_v86_active());
    assert!(!dpmi.should_exit_v86_on_iret());
    assert!(!cpu.regs.is_v86_mode());

    // Verify PM state was restored
    assert_eq!(cpu.regs.cs, 0x0008);
    assert_eq!(cpu.regs.ip(), 0x5000);
    assert_eq!(cpu.cs_bits, 32);
}

#[test]
fn test_v86_saved_state_struct() {
    // V86SavedState should capture all segment registers and caches
    let dpmi = Dpmi::new();
    assert!(dpmi.v86_saved_state.is_empty());
}

#[test]
fn test_exit_v86_without_saved_state_is_noop() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.exit_v86(&mut cpu, &mut memory);
    assert!(!dpmi.is_v86_active());
}

#[test]
fn test_nested_v86_entries_restore_previous_state() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ss = 0x1000;
    cpu.regs.ss_cache = SegmentCache::real_mode(0x1000);
    cpu.regs.esp = 0x0100;

    let rm_struct = RealModeCallStruct::default();
    dpmi.enter_v86(
        &mut cpu, &rm_struct, 0x10000, 0x2000, 0x0100, 0x3000, 0x0100, true,
    )
    .unwrap();
    assert!(dpmi.is_v86_active());
    assert_eq!(dpmi.v86_saved_state.len(), 1);

    dpmi.enter_v86(
        &mut cpu, &rm_struct, 0x20000, 0x4000, 0x0200, 0x5000, 0x0200, false,
    )
    .unwrap();
    assert!(dpmi.is_v86_active());
    assert_eq!(dpmi.v86_saved_state.len(), 2);

    dpmi.exit_v86(&mut cpu, &mut memory);
    assert!(dpmi.is_v86_active());
    assert_eq!(dpmi.v86_saved_state.len(), 1);

    dpmi.exit_v86(&mut cpu, &mut memory);
    assert!(!dpmi.is_v86_active());
    assert!(dpmi.v86_saved_state.is_empty());
}

#[test]
fn test_nested_translation_capacity_returns_resource_error_and_preserves_outer_states() {
    // MDD-BUG-CRUCIBLE-00139: the seventeenth nested translation used to
    // panic in FixedVec::push. Exercise the public INT 31h dispatcher so
    // the capacity failure follows the same path a nested guest request does.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    const INT_NUM: u8 = 0x10;
    const RMCS_BASE: usize = 0x50000;

    memory.write_u16(INT_NUM as usize * 4, 0x1234);
    memory.write_u16(INT_NUM as usize * 4 + 2, 0x1000);

    cpu.regs.cs = 0x0008;
    cpu.regs.eip = 0x7000;
    cpu.regs.ss = 0x0010;
    cpu.regs.esp = 0x8000;
    let mut expected_outer_states = Vec::new();

    for depth in 0..16_u32 {
        let rmcs_addr = RMCS_BASE + depth as usize * 0x100;
        let rm_ss = 0x6000 + depth as u16 * 0x10;
        let rm_sp = 0x0100;
        RealModeCallStruct {
            eax: 0x1100_0000 | depth,
            edx: 0x2200_0000 | depth,
            flags: gp64_cpu::Flags::RESERVED1.bits(),
            ss: rm_ss,
            sp: rm_sp,
            ..Default::default()
        }
        .write_to_memory(&mut memory, rmcs_addr);

        cpu.regs.eip = 0x7000 + depth;
        cpu.regs.edx = 0xD000_0000 | depth;
        cpu.regs.set_ax(0x0300);
        cpu.regs.set_bx(INT_NUM as u16);
        cpu.regs.set_cx(0);
        cpu.regs.es = (rmcs_addr >> 4) as u16;
        cpu.regs.edi = (rmcs_addr & 0xF) as u32;
        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
        expected_outer_states.push((
            cpu.regs.cs,
            cpu.regs.eip,
            cpu.regs.ss,
            cpu.regs.esp,
            cpu.regs.edx,
        ));

        dpmi.int31(&mut cpu, &mut memory).unwrap();
        assert_eq!(dpmi.v86_saved_state.len(), depth as usize + 1);
        assert!(dpmi.is_v86_active());
    }

    let reflected_rmcs = RealModeCallStruct::default();
    let reflected_unchanged = (
        cpu.regs.cs,
        cpu.regs.eip,
        cpu.regs.ss,
        cpu.regs.esp,
        cpu.regs.eax,
        cpu.regs.edx,
    );
    assert_eq!(
        dpmi.enter_v86_for_interrupt(
            &mut cpu,
            &memory,
            &reflected_rmcs,
            RMCS_BASE + 0x0F00,
            INT_NUM,
        ),
        Err(DPMI_ERR_RESOURCE_UNAVAILABLE)
    );
    assert_eq!(
        (
            cpu.regs.cs,
            cpu.regs.eip,
            cpu.regs.ss,
            cpu.regs.esp,
            cpu.regs.eax,
            cpu.regs.edx,
        ),
        reflected_unchanged,
        "a rejected reflected interrupt must not mutate CPU state"
    );
    assert_eq!(dpmi.v86_saved_state.len(), 16);

    // Every translation service that enters V86 must reject at the same
    // boundary without touching its proposed real-mode return stack.
    for subfunc in [0x0300_u16, 0x0301, 0x0302] {
        let rmcs_addr = RMCS_BASE + 0x1000 + subfunc as usize;
        let rm_ss = 0x7000 + (subfunc & 0x000F) * 0x10;
        let rm_sp = 0x0200;
        RealModeCallStruct {
            cs: 0x2000,
            ip: 0x0100,
            flags: gp64_cpu::Flags::RESERVED1.bits(),
            ss: rm_ss,
            sp: rm_sp,
            ..Default::default()
        }
        .write_to_memory(&mut memory, rmcs_addr);

        let stack_linear = Memory::seg_offset_to_linear(rm_ss, rm_sp.wrapping_sub(6));
        memory.write_u16(stack_linear, 0xCAFE);
        memory.write_u16(stack_linear + 2, 0xBABE);
        memory.write_u16(stack_linear + 4, 0xF00D);

        cpu.regs.eip = 0x9000 + subfunc as u32;
        cpu.regs.edx = 0xE000_0000 | subfunc as u32;
        cpu.regs.set_ax(subfunc);
        cpu.regs
            .set_bx(if subfunc == 0x0300 { INT_NUM as u16 } else { 0 });
        cpu.regs.set_cx(0);
        cpu.regs.es = (rmcs_addr >> 4) as u16;
        cpu.regs.edi = (rmcs_addr & 0xF) as u32;
        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
        let unchanged = (
            cpu.regs.cs,
            cpu.regs.eip,
            cpu.regs.ss,
            cpu.regs.esp,
            cpu.regs.edx,
        );

        dpmi.int31(&mut cpu, &mut memory).unwrap();

        assert_eq!(cpu.regs.ax(), DPMI_ERR_RESOURCE_UNAVAILABLE);
        assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(
            (
                cpu.regs.cs,
                cpu.regs.eip,
                cpu.regs.ss,
                cpu.regs.esp,
                cpu.regs.edx,
            ),
            unchanged
        );
        assert_eq!(dpmi.v86_saved_state.len(), 16);
        assert_eq!(memory.read_u16(stack_linear), 0xCAFE);
        assert_eq!(memory.read_u16(stack_linear + 2), 0xBABE);
        assert_eq!(memory.read_u16(stack_linear + 4), 0xF00D);
    }

    for (remaining, expected) in expected_outer_states.into_iter().enumerate().rev() {
        dpmi.exit_v86(&mut cpu, &mut memory);
        assert_eq!(
            (
                cpu.regs.cs,
                cpu.regs.eip,
                cpu.regs.ss,
                cpu.regs.esp,
                cpu.regs.edx,
            ),
            expected
        );
        assert_eq!(dpmi.v86_saved_state.len(), remaining);
        assert_eq!(dpmi.is_v86_active(), remaining != 0);
        assert_eq!(cpu.regs.is_v86_mode(), remaining != 0);
        assert!(
            !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "successful outer translation must retain its PM-side CF=0 result"
        );
    }
}

#[test]
fn test_enter_v86_with_zero_rmcs_flags_clears_stale_flags() {
    // MDD-BUG-FLUXHOMEARPA-00012: RMCS flags=0 is a valid value (clear all flags),
    // not a sentinel. enter_v86 must load the low flags from the RMCS so a cleared
    // RMCS clears IF/DF/TF/CF instead of inheriting the protected-mode caller's
    // flags, and must keep IOPL=3 (which the flag load clobbers) for V86.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();

    cpu.regs.ss = 0x1000;
    cpu.regs.ss_cache = SegmentCache::real_mode(0x1000);
    cpu.regs.esp = 0x0100;
    // Protected-mode caller has IF and DF set.
    cpu.regs.flags =
        gp64_cpu::Flags::INTERRUPT | gp64_cpu::Flags::DIRECTION | gp64_cpu::Flags::RESERVED1;

    let rm_struct = RealModeCallStruct::default(); // flags = 0
    dpmi.enter_v86(
        &mut cpu, &rm_struct, 0x10000, 0x2000, 0x0100, 0x3000, 0x0100, true,
    )
    .unwrap();

    // flags=0 must clear IF/DF, not inherit the PM caller's state.
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::INTERRUPT),
        "IF must be cleared by RMCS flags=0"
    );
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::DIRECTION),
        "DF must be cleared by RMCS flags=0"
    );
    assert!(
        !cpu.interrupts_enabled,
        "interrupts_enabled must match the loaded (cleared) IF"
    );
    // V86 requires IOPL=3, which the flag load would otherwise clobber to 0.
    assert_eq!(cpu.regs.iopl(), 3, "V86 IOPL must remain 3");
    assert!(cpu.regs.is_v86_mode(), "VM must be set");
}

/// Regression test for the V86 EIP truncation bug (backlog #34).
///
/// When a USE32 PM client (EIP > 0xFFFF) calls DPMI 0x0300 (Simulate
/// Real Mode Interrupt), enter_v86() must preserve the full 32-bit EIP.
/// Previously, `cpu.regs.ip() as u32` was used which truncated to 16 bits,
/// causing the PM code to resume at the wrong address after exit_v86().
#[test]
fn test_v86_preserves_full_32bit_eip() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up a USE32 PM caller with EIP > 0xFFFF for a DOS extender
    cpu.regs.eip = 0x1001_0000; // Synthetic instruction pointer beyond 16 bits
    cpu.regs.cs = 0x0268;
    cpu.regs.cs_cache = SegmentCache {
        base: 0x0010_0000,
        limit: 0x000F_FFFF,
        big: true,
        is_code: true,
        access: 0x9B, // present, DPL=0, code, read/exec
        ..SegmentCache::default()
    };
    cpu.regs.ss = 0x0270;
    cpu.regs.ss_cache = SegmentCache {
        base: 0x0010_0000,
        limit: 0x000F_FFFF,
        big: true,
        is_code: false,
        access: 0x93, // present, DPL=0, data, read/write
        ..SegmentCache::default()
    };
    cpu.regs.esp = 0x000F_8000;
    cpu.regs.ds = 0x0270;
    cpu.regs.es = 0x0270;
    cpu.regs.fs = 0;
    cpu.regs.gs = 0;

    let rm_struct = RealModeCallStruct::default();
    let struct_addr = 0x5_0000;

    // Enter V86 mode (simulating DPMI 0x0300)
    dpmi.enter_v86(
        &mut cpu,
        &rm_struct,
        struct_addr,
        0x0000,
        0x0000,
        0x9FC0,
        0x0000,
        true,
    )
    .unwrap();
    assert!(dpmi.is_v86_active());

    // Exit V86 mode (simulating real-mode handler completion)
    dpmi.exit_v86(&mut cpu, &mut memory);
    assert!(!dpmi.is_v86_active());

    // The CRITICAL assertion: full 32-bit EIP must be preserved
    assert_eq!(
        cpu.regs.eip, 0x1001_0000,
        "V86 round-trip must preserve full 32-bit EIP; got {:#010X} (truncation would give {:#010X})",
        cpu.regs.eip, 0x1001_0000 & 0xFFFF
    );

    // Also verify other PM state is restored
    assert_eq!(cpu.regs.cs, 0x0268);
    assert_eq!(cpu.regs.ss, 0x0270);
    assert_eq!(cpu.regs.esp, 0x000F_8000);
}
// MDD-BUG-CRUCIBLE-00057 regressions: filling the fixed-capacity physical-mapping
// table (FixedVec<_, 64>) must make the next mapping request via any of the three
// mapping services (0800h, 0508h, 0509h) fail guest-visibly, not panic in
// FixedVec::push. Fill with high physical addresses so a low-address request does
// not overlap and reaches the is_full() guard.
fn fill_physical_mappings_to_capacity(dpmi: &mut Dpmi) {
    for i in 0..64u32 {
        dpmi.physical_mappings.push(PhysicalMapping {
            physical: 0x100000 + i * 0x1000,
            linear: 0x100000 + i * 0x1000,
            size: 0x1000,
        });
    }
    assert!(dpmi.physical_mappings.is_full(), "table should be full");
}

fn assert_block_mapping_respects_shared_memory_capacity(service: u16) {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    install_memory_only_physical_mapping(&mut memory);
    let (handle, base) = allocate_physical_mapping_test_block(&mut dpmi, &mut cpu, &mut memory);

    for index in 0..63u32 {
        cpu.regs.set_ax(service);
        cpu.regs.esi = handle;
        cpu.regs.ebx = index * 0x1000;
        cpu.regs.ecx = 1;
        cpu.regs.edx = 0x0020_0000 + index * 0x1000;
        dpmi.int31(&mut cpu, &mut memory).unwrap();
        assert!(
            !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "service {:04X} mapping {} should fit both ledgers",
            service,
            index
        );
    }

    let rejected_linear = base + 63 * 0x1000;
    let before_dpmi = dpmi.physical_mappings.len();
    assert_eq!(before_dpmi, 63);
    assert_eq!(memory.map_linear_physical(rejected_linear), None);
    let block_index = dpmi
        .linear_memory_blocks
        .iter()
        .position(|block| block.handle == handle)
        .unwrap();
    assert!(!dpmi.linear_memory_blocks[block_index].is_page_committed(63));

    cpu.regs.set_ax(service);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 63 * 0x1000;
    cpu.regs.ecx = 1;
    cpu.regs.edx = 0x0020_0000 + 63 * 0x1000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_LINEAR_UNAVAILABLE);
    assert_eq!(dpmi.physical_mappings.len(), before_dpmi);
    assert_eq!(memory.map_linear_physical(rejected_linear), None);
    assert!(!dpmi.linear_memory_blocks[block_index].is_page_committed(63));
    assert_eq!(memory.map_linear_physical(0xFF00_0000), Some(0x03F0_0000));
}
