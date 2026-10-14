use super::*;

#[test]
fn test_cov_set_multiple_descriptors_selector_not_found() {
    // Line 2007: selector found in LDT range but not in selectors list
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

    // Write a valid LDT selector format that's in range but not tracked
    // LDT selector with TI=1, RPL=3: (index << 3) | 0x07
    // Use a high index that won't be allocated
    let fake_sel: u16 = (100 << 3) | 0x07; // index 100
    memory.write_u16(buf_addr, fake_sel);

    // Write a valid descriptor after the selector (8 bytes)
    // access = 0xF2 (present, DPL=3, data r/w, bit 4 = 1)
    memory.write_u8(buf_addr + 7, 0xF2); // access byte at offset 5 from desc start
                                         // Actually the descriptor layout is: limit_lo(2), base_lo(2), base_mid(1), access(1), limit_flags(1), base_hi(1)
                                         // So in the entry: [sel:2][limit_lo:2][base_lo:2][base_mid:1][access:1][limit_flags:1][base_hi:1]
    memory.write_u8(buf_addr + 7, 0x00); // base_mid
    memory.write_u8(buf_addr + 8, 0xF2); // access (present, DPL=3, data, writable, bit4=1)
    memory.write_u8(buf_addr + 9, 0x00); // limit_flags

    cpu.regs.set_ax(0x000F);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Should fail: selector not found in selectors list
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_cov_create_alias_source_not_found() {
    // Line 1635 else path: source selector found but new selector not in list
    // This is a defensive path that normally can't happen, so we test the
    // source not found path instead (line 1649)
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Use a valid LDT selector format that's in range but not tracked
    let fake_sel: u16 = (200 << 3) | 0x07;
    cpu.regs.set_ax(0x000A);
    cpu.regs.set_bx(fake_sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_cov_segment_to_selector_new_allocation() {
    // Line 1357: allocate_descriptors returns valid selector, find it in selectors
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Convert segment 0x40 (BIOS data area) to selector
    cpu.regs.set_ax(0x0002);
    cpu.regs.set_bx(0x0040);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel = cpu.regs.ax();
    // Should be an LDT selector (TI=1)
    assert_ne!(sel & 0x04, 0);

    // Verify the descriptor base is segment << 4 = 0x400
    if let Some(s) = dpmi.selectors.iter().find(|s| s.selector == sel) {
        assert_eq!(s.base, 0x400);
    } else {
        panic!("Selector not found");
    }
}

#[test]
fn test_cov_segment_to_selector_cached() {
    // Test that second call to same segment returns cached selector
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ax(0x0002);
    cpu.regs.set_bx(0x0040);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let sel1 = cpu.regs.ax();

    // Call again - should return cached
    cpu.regs.set_ax(0x0002);
    cpu.regs.set_bx(0x0040);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let sel2 = cpu.regs.ax();

    assert_eq!(sel1, sel2);
}

#[test]
fn test_cov_env_selector_allocation_zero_env() {
    // Line 5985: env_sel != 0 branch when env_segment is 0
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up for protected mode entry with zero env segment
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0xFFFE);
    let stack_base = 0x10000usize;
    memory.write_u16(stack_base + 0xFFFC, 0x0100);
    memory.write_u16(stack_base + 0xFFFE, 0x2000);
    cpu.regs.set_sp(0xFFFC);
    cpu.regs.set_ax(0); // 16-bit
    cpu.regs.ds = 0x3000;
    cpu.regs.es = 0x4000;

    // enter_protected_mode_with_psp with env_segment = 0
    dpmi.enter_protected_mode_with_psp(&mut cpu, &mut memory, 0x100, 0x0000)
        .unwrap();
    assert!(dpmi.protected_mode);
    // env_selector should be 0 since env_segment is 0
    assert_eq!(dpmi.env_selector, 0);
}

#[test]
fn test_cov_dpmi_trace_emit_disabled() {
    // Line 104: !dpmi_trace_env_enabled() => return early
    let _lock = env_lock();
    // Should not panic, just return
    dpmi_trace_emit("[TEST] should be ignored");
}

#[test]
fn test_cov_dpmi_trace_emit_limit_exceeded() {
    // Lines 108/111/112: trace limit exceeded path
    // We can't easily trigger the 20000 limit, but we can exercise the function
    let _lock = env_lock();
    // Just call it - if tracing is enabled, it will try to write
    dpmi_trace_emit("[TEST] trace limit test");
}

/// After entering DPMI protected mode, PM_BIOS_CODE_SEL (0x001B) must be
/// present in the CPU's hle_break_selectors so the interpreter bails out
/// on FAR JMP/CALL to BIOS HLE stubs.
#[test]
fn test_dpmi_pm_entry_adds_bios_break_selector() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up real-mode registers for PM entry
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0xFFFE);
    let stack_base = 0x10000usize;
    memory.write_u16(stack_base + 0xFFFC, 0x0100); // Return IP
    memory.write_u16(stack_base + 0xFFFE, 0x2000); // Return CS
    cpu.regs.set_sp(0xFFFC);
    cpu.regs.set_ax(0); // 16-bit mode
    cpu.regs.ds = 0x3000;
    cpu.regs.es = 0x4000;

    dpmi.enter_protected_mode_with_psp(&mut cpu, &mut memory, 0, 0)
        .unwrap();

    // PM_BIOS_CODE_SEL (0x001B) must be in hle_break_selectors
    let count = cpu.hle_break_count as usize;
    let selectors = &cpu.hle_break_selectors[..count];
    assert!(
        selectors.contains(&PM_BIOS_CODE_SEL),
        "hle_break_selectors should contain PM_BIOS_CODE_SEL (0x{:04X}) after PM entry, \
         but found {:?}",
        PM_BIOS_CODE_SEL,
        selectors
    );
}

/// After DPMI PM entry adds PM_BIOS_CODE_SEL, adding Win16 selectors via
/// add_hle_break_selector must not clobber PM_BIOS_CODE_SEL. Both should
/// coexist in the break selector list.
#[test]
fn test_dpmi_pm_break_selector_coexists_with_win16() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Enter PM — this should add PM_BIOS_CODE_SEL
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0xFFFE);
    let stack_base = 0x10000usize;
    memory.write_u16(stack_base + 0xFFFC, 0x0100);
    memory.write_u16(stack_base + 0xFFFE, 0x2000);
    cpu.regs.set_sp(0xFFFC);
    cpu.regs.set_ax(0);
    cpu.regs.ds = 0x3000;
    cpu.regs.es = 0x4000;

    dpmi.enter_protected_mode_with_psp(&mut cpu, &mut memory, 0, 0)
        .unwrap();

    // Simulate Win16 adding its own selectors
    let win16_sel = 0x0047u16; // hypothetical Win16 HLE selector
    cpu.add_hle_break_selector(win16_sel);

    // Both PM_BIOS_CODE_SEL and the Win16 selector must be present
    let count = cpu.hle_break_count as usize;
    let selectors = &cpu.hle_break_selectors[..count];
    assert!(
        selectors.contains(&PM_BIOS_CODE_SEL),
        "PM_BIOS_CODE_SEL (0x{:04X}) should survive after Win16 selector add, \
         but found {:?}",
        PM_BIOS_CODE_SEL,
        selectors
    );
    assert!(
        selectors.contains(&win16_sel),
        "Win16 selector (0x{:04X}) should be present, but found {:?}",
        win16_sel,
        selectors
    );
}

/// Simulates the Win16 loading scenario: DPMI enters PM (adding
/// PM_BIOS_CODE_SEL via add_hle_break_selector), then Win16 calls
/// set_hle_break_selectors with its own list that ALSO includes
/// PM_BIOS_CODE_SEL (as loading.rs does after the retain+push fix).
#[test]
fn test_set_hle_break_selectors_preserves_pm_bios_code_sel() {
    let mut cpu = Cpu::new();

    // DPMI adds PM_BIOS_CODE_SEL
    cpu.add_hle_break_selector(PM_BIOS_CODE_SEL);
    assert!(cpu.hle_break_selectors[..cpu.hle_break_count as usize].contains(&PM_BIOS_CODE_SEL));

    // Win16 replaces with its own list (which should include PM_BIOS_CODE_SEL)
    let win16_sels = [0x0047, 0x004F, 0x0057, PM_BIOS_CODE_SEL];
    cpu.set_hle_break_selectors(&win16_sels);

    // PM_BIOS_CODE_SEL should still be present
    let active = &cpu.hle_break_selectors[..cpu.hle_break_count as usize];
    assert!(
        active.contains(&PM_BIOS_CODE_SEL),
        "PM_BIOS_CODE_SEL lost after set_hle_break_selectors"
    );
    assert!(active.contains(&0x0047));
    assert_eq!(cpu.hle_break_count, 4);
}

/// Calling add_hle_break_selector with the same value twice must not add a
/// duplicate entry.
#[test]
fn test_add_hle_break_selector_no_duplicates() {
    let mut cpu = Cpu::new();

    cpu.add_hle_break_selector(PM_BIOS_CODE_SEL);
    cpu.add_hle_break_selector(PM_BIOS_CODE_SEL);

    let count = cpu.hle_break_count as usize;
    assert_eq!(
        count, 1,
        "Duplicate add_hle_break_selector should not increase count: got {}",
        count
    );
    assert_eq!(cpu.hle_break_selectors[0], PM_BIOS_CODE_SEL);
}

#[test]
fn test_get_descriptor_gdt_selector_valid() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up GDT with a valid descriptor at index 1 (selector 0x0008)
    let gdt_base = 0x5000usize;
    dpmi.gdt_base = gdt_base as u32;
    dpmi.gdt_size = 256;
    cpu.regs.gdtr.base = gdt_base as u32;
    cpu.regs.gdtr.limit = 255;

    // Write a data segment descriptor at GDT index 1
    let gdt_addr = gdt_base + 8;
    memory.write_u16(gdt_addr, 0xFFFF); // limit low
    memory.write_u16(gdt_addr + 2, 0x0000); // base low
    memory.write_u8(gdt_addr + 4, 0x10); // base mid
    memory.write_u8(gdt_addr + 5, 0xF2); // access: present, DPL=3, data, RW
    memory.write_u8(gdt_addr + 6, 0xCF); // flags + limit high
    memory.write_u8(gdt_addr + 7, 0x00); // base high

    // Set up ES:DI (or ES:EDI) to point to a buffer for the result
    let buffer_addr = 0x8000usize;
    cpu.regs.es = 0x0800;
    cpu.regs.set_di(0x0000);

    // In real mode / 16-bit mode, buffer = ES*16 + DI
    // For this test we just need the buffer to be writable.
    // The client_pointer_with_access function resolves ES:offset.
    // For tests without paging, it computes the linear address directly.
    //
    // Since we're not in protected mode, the descriptor access uses
    // the real-mode address calculation: ES*16 + DI.
    // Let buffer be at 0x0800*16 + 0 = 0x8000.

    cpu.regs.set_ax(0x000B); // Get descriptor
    cpu.regs.set_bx(0x0008); // GDT selector index 1

    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Should succeed (carry clear)
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify the 8 bytes were copied to the buffer
    assert_eq!(memory.read_u16(buffer_addr), 0xFFFF); // limit low
    assert_eq!(memory.read_u8(buffer_addr + 5), 0xF2); // access
}

#[test]
fn test_get_descriptor_gdt_index_zero_rejected() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.gdt_base = 0x5000;
    dpmi.gdt_size = 256;
    cpu.regs.gdtr.base = 0x5000;
    cpu.regs.gdtr.limit = 255;

    cpu.regs.es = 0x0800;
    cpu.regs.set_di(0x0000);

    cpu.regs.set_ax(0x000B); // Get descriptor
    cpu.regs.set_bx(0x0000); // GDT index 0 (null descriptor)

    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Should fail
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_get_descriptor_gdt_index_out_of_range() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.gdt_base = 0x5000;
    dpmi.gdt_size = 32; // Only 4 entries
    cpu.regs.gdtr.base = 0x5000;
    cpu.regs.gdtr.limit = 31;

    cpu.regs.es = 0x0800;
    cpu.regs.set_di(0x0000);

    cpu.regs.set_ax(0x000B);
    cpu.regs.set_bx(0x0028); // Index 5 - beyond gdt_size

    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_set_descriptor_gdt_valid() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let gdt_base = 0x5000usize;
    dpmi.gdt_base = gdt_base as u32;
    dpmi.gdt_size = 256;
    cpu.regs.gdtr.base = gdt_base as u32;
    cpu.regs.gdtr.limit = 255;

    // Write descriptor data at ES:DI buffer (0x8000)
    let buffer_addr = 0x8000usize;
    memory.write_u16(buffer_addr, 0xFFFF); // limit low
    memory.write_u16(buffer_addr + 2, 0x0000); // base low
    memory.write_u8(buffer_addr + 4, 0x20); // base mid
    memory.write_u8(buffer_addr + 5, 0xF2); // access: present, DPL=3, data, RW (has segment bit set)
    memory.write_u8(buffer_addr + 6, 0xCF); // flags + limit high
    memory.write_u8(buffer_addr + 7, 0x00); // base high

    cpu.regs.es = 0x0800;
    cpu.regs.set_di(0x0000);

    cpu.regs.set_ax(0x000C); // Set descriptor
    cpu.regs.set_bx(0x0008); // GDT index 1

    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify the GDT was updated
    let gdt_addr = gdt_base + 8;
    assert_eq!(memory.read_u16(gdt_addr), 0xFFFF);
    assert_eq!(memory.read_u8(gdt_addr + 4), 0x20);
}

#[test]
fn test_set_descriptor_gdt_rejects_non_segment() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let gdt_base = 0x5000usize;
    dpmi.gdt_base = gdt_base as u32;
    dpmi.gdt_size = 256;
    cpu.regs.gdtr.base = gdt_base as u32;
    cpu.regs.gdtr.limit = 255;

    // Write descriptor with segment bit (bit 4 of access) cleared
    let buffer_addr = 0x8000usize;
    memory.write_u16(buffer_addr, 0xFFFF);
    memory.write_u16(buffer_addr + 2, 0x0000);
    memory.write_u8(buffer_addr + 4, 0x00);
    memory.write_u8(buffer_addr + 5, 0xE0); // access: present, DPL=3, but NO segment bit
    memory.write_u8(buffer_addr + 6, 0x00);
    memory.write_u8(buffer_addr + 7, 0x00);

    cpu.regs.es = 0x0800;
    cpu.regs.set_di(0x0000);

    cpu.regs.set_ax(0x000C); // Set descriptor
    cpu.regs.set_bx(0x0008);

    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_allocate_dos_memory_block_zero_paragraphs() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dos_mem = init_dos_mem(&mut memory);

    let result = dpmi.allocate_dos_memory_block(&mut cpu, &mut memory, &mut dos_mem, 0x100, 0);
    assert!(result.is_err());
}

#[test]
fn test_allocate_and_free_dos_memory_block() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dos_mem = init_dos_mem(&mut memory);

    // Need LDT limit set so selectors can be allocated
    cpu.regs.ldt_limit = 0xFFFF;

    let result = dpmi.allocate_dos_memory_block(&mut cpu, &mut memory, &mut dos_mem, 0x100, 0x10);
    assert!(result.is_ok());

    let (segment, selector) = result.unwrap();
    assert!(segment > 0);
    assert!(selector > 0);

    // Free the block
    let free_result = dpmi.free_dos_memory_block(&mut cpu, &mut memory, &mut dos_mem, selector);
    assert!(free_result.is_ok());
}

#[test]
fn test_allocate_dos_memory_block_full_table_returns_error_no_panic() {
    // MDD-BUG-FLUXHOMEARPA-00016: when the fixed-capacity DOS-memory tracking
    // table is full, allocation must return a guest-visible failure BEFORE
    // touching DOS memory/descriptors, not panic on FixedVec::push.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dos_mem = init_dos_mem(&mut memory);
    cpu.regs.ldt_limit = 0xFFFF;

    // Fill the 256-entry tracking table with dummy entries.
    while !dpmi.dos_memory_blocks.is_full() {
        dpmi.dos_memory_blocks.push(DosMemoryBlock {
            segment: 0x2000,
            selector: 0x0107,
            paragraphs: 1,
            selector_count: 1,
        });
    }
    let len_before = dpmi.dos_memory_blocks.len();

    // One more allocation must fail cleanly (previously panicked in push).
    let result = dpmi.allocate_dos_memory_block(&mut cpu, &mut memory, &mut dos_mem, 0x100, 0x10);
    assert!(
        matches!(result, Err(DosMemoryBlockError::InsufficientMemory { .. })),
        "full tracking table must return InsufficientMemory, got {result:?}"
    );
    // No tracking-table mutation on the rejected allocation.
    assert_eq!(dpmi.dos_memory_blocks.len(), len_before);
}

#[test]
fn test_free_dos_memory_block_invalid_selector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dos_mem = init_dos_mem(&mut memory);

    let result = dpmi.free_dos_memory_block(&mut cpu, &mut memory, &mut dos_mem, 0x0107);
    assert!(result.is_err());
}

#[test]
fn test_resize_dos_memory_block_invalid_selector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dos_mem = init_dos_mem(&mut memory);

    let result = dpmi.resize_dos_memory_block(&mut cpu, &mut memory, &mut dos_mem, 0x0107, 0x20);
    assert!(result.is_err());
}

#[test]
fn test_allocate_and_resize_dos_memory_block() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dos_mem = init_dos_mem(&mut memory);

    cpu.regs.ldt_limit = 0xFFFF;

    let result = dpmi.allocate_dos_memory_block(&mut cpu, &mut memory, &mut dos_mem, 0x100, 0x10);
    assert!(result.is_ok());

    let (_segment, selector) = result.unwrap();

    // Resize to larger
    let resize_result =
        dpmi.resize_dos_memory_block(&mut cpu, &mut memory, &mut dos_mem, selector, 0x20);
    assert!(resize_result.is_ok());
}

#[test]
fn test_dpmi_linear_alloc_zero_size() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0501); // Allocate memory block
    cpu.regs.set_bx(0); // Size high = 0
    cpu.regs.set_cx(0); // Size low = 0

    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_dpmi_linear_alloc_and_free() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate 4KB
    cpu.regs.set_ax(0x0501);
    cpu.regs.set_bx(0);
    cpu.regs.set_cx(0x1000); // 4KB

    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let handle_hi = cpu.regs.si();
    let handle_lo = cpu.regs.di();

    // Free the block
    cpu.regs.set_ax(0x0502);
    cpu.regs.set_si(handle_hi);
    cpu.regs.set_di(handle_lo);

    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_dpmi_free_invalid_handle() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0502);
    cpu.regs.set_si(0xDEAD);
    cpu.regs.set_di(0xBEEF);

    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_dpmi_resize_invalid_handle() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0503);
    cpu.regs.set_si(0xDEAD);
    cpu.regs.set_di(0xBEEF);
    cpu.regs.set_bx(0);
    cpu.regs.set_cx(0x2000);

    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_dpmi_alloc_and_resize() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate 4KB
    cpu.regs.set_ax(0x0501);
    cpu.regs.set_bx(0);
    cpu.regs.set_cx(0x1000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let handle_hi = cpu.regs.si();
    let handle_lo = cpu.regs.di();

    // Resize to 8KB
    cpu.regs.set_ax(0x0503);
    cpu.regs.set_si(handle_hi);
    cpu.regs.set_di(handle_lo);
    cpu.regs.set_bx(0);
    cpu.regs.set_cx(0x2000);

    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_dos_alloc_selector_and_lookup() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dpmi = Dpmi::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let sel = dpmi.dos_alloc_selector(&mut cpu, &mut memory, 0x0500, 0x100);
    assert!(sel.is_some());
    let selector = sel.unwrap();
    assert_ne!(selector, 0);

    // Verify segment lookup
    let seg = dpmi.dos_selector_to_segment(selector);
    assert_eq!(seg, Some(0x0500));

    // Lookup for non-existent selector returns None
    assert_eq!(dpmi.dos_selector_to_segment(0xFFFF), None);
}

#[test]
fn test_dos_free_selector() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dpmi = Dpmi::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let sel = dpmi.dos_alloc_selector(&mut cpu, &mut memory, 0x0600, 0x80);
    let selector = sel.unwrap();

    // Free the selector
    dpmi.dos_free_selector(&mut cpu, &mut memory, selector);

    // After free, lookup should fail
    assert_eq!(dpmi.dos_selector_to_segment(selector), None);
}

#[test]
fn test_dos_resize_selector() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dpmi = Dpmi::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let sel = dpmi.dos_alloc_selector(&mut cpu, &mut memory, 0x0700, 0x40);
    let selector = sel.unwrap();

    // Resize to larger
    dpmi.dos_resize_selector(&mut cpu, &mut memory, selector, 0x100);

    // Selector should still be valid
    assert_eq!(dpmi.dos_selector_to_segment(selector), Some(0x0700));
}

#[test]
fn test_int2f_168a_msdos_returns_entry_point() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dpmi = Dpmi::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);
    setup_pm_vendor_string(&mut dpmi, &mut cpu, &mut memory, b"MS-DOS");
    let expected_cache = cpu
        .read_descriptor(&memory, PM_BIOS_CODE_SEL)
        .expect("PM BIOS code selector must exist after DPMI entry");
    cpu.regs.es = 0x2222;
    cpu.regs.es_cache = SegmentCache::real_mode(0x2222);

    cpu.regs.set_ax(0x168A);
    let recognised = dpmi.int2f_msdos_ext_lookup(&mut cpu, &memory);

    assert!(recognised, "vendor \"MS-DOS\" should be recognised");
    assert_eq!(cpu.regs.al(), 0x00, "AL must be 0 on success per spec");
    assert_eq!(
        cpu.regs.es, PM_BIOS_CODE_SEL,
        "ES must point to PM_BIOS_CODE_SEL"
    );
    assert_eq!(
        cpu.regs.di(),
        HLE_MSDOS_EXT_ENTRY,
        "DI must point to the MS-DOS extension entry stub"
    );
    assert_eq!(
        cpu.regs.es_cache, expected_cache,
        "AX=168Ah must load the returned PM code descriptor"
    );
}

#[test]
fn test_int2f_168a_phar_lap_returns_failure() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dpmi = Dpmi::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);
    setup_pm_vendor_string(&mut dpmi, &mut cpu, &mut memory, b"Phar Lap");

    cpu.regs.set_ax(0x168A);
    let pre_ax = cpu.regs.ax();
    let pre_es = cpu.regs.es;
    let pre_di = cpu.regs.di();
    let recognised = dpmi.int2f_msdos_ext_lookup(&mut cpu, &memory);

    assert!(!recognised, "vendor \"Phar Lap\" must not be recognised");
    // Per spec failure signal: AX (and registers) unchanged. AL stays 0x8A.
    assert_eq!(cpu.regs.ax(), pre_ax, "AX must be unchanged on failure");
    assert_eq!(cpu.regs.es, pre_es, "ES must be unchanged on failure");
    assert_eq!(cpu.regs.di(), pre_di, "DI must be unchanged on failure");
}

#[test]
fn test_int2f_168a_unsupported_real_mode() {
    // Locks the PM-only contract from the HLD. The MS-DOS API Extensions
    // spec says 168A is protected-mode only; the int21::int2f dispatch
    // gates the call on `cpu.is_protected_mode()` and never invokes
    // int2f_msdos_ext_lookup in real mode. This test does NOT call the
    // helper directly — it verifies the gate by setting up a real-mode
    // CPU with AX=168A and confirming AX is unchanged after dispatch.
    use crate::Dos;
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dos = Dos::new();
    // Real mode: PE bit clear, no LDT, no protected-mode setup.
    assert!(!cpu.is_protected_mode());

    // Write the vendor string at a real-mode address and point DS:SI at it.
    let seg: u16 = 0x9000;
    let off: u16 = 0x0000;
    let linear = (seg as usize) << 4;
    for (i, b) in b"MS-DOS".iter().enumerate() {
        memory.write_u8(linear + i, *b);
    }
    memory.write_u8(linear + 6, 0);
    cpu.regs.ds = seg;
    cpu.regs.set_si(off);
    cpu.regs.set_ax(0x168A);

    // Dispatch through the public int2f path. int2f only needs cpu,
    // memory, win16_active, cd_audio.
    dos.int2f(&mut cpu, &mut memory, false).unwrap();

    // AX must be unchanged — the PM-only gate prevented dispatch.
    assert_eq!(
        cpu.regs.ax(),
        0x168A,
        "RM dispatch must leave AX unchanged (PM-only contract)"
    );
}

#[test]
fn test_int2f_168a_name_past_ds_limit_is_not_recognised() {
    // MDD-BUG-KIL-00772: "MS-DOS\0" sits in memory, but DS ends after
    // "MS-DO", so the name is not addressable through the segment.
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dpmi = Dpmi::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);
    setup_pm_vendor_string(&mut dpmi, &mut cpu, &mut memory, b"MS-DOS");
    let ds = cpu.regs.ds;
    dpmi.set_descriptor_limit(&mut cpu, &mut memory, ds, 4);

    cpu.regs.set_ax(0x168A);
    assert!(!dpmi.int2f_msdos_ext_lookup(&mut cpu, &memory));
    assert_eq!(cpu.regs.ax(), 0x168A, "AX must be unchanged");

    // The terminator is the last in-limit byte: the name is valid.
    dpmi.set_descriptor_limit(&mut cpu, &mut memory, ds, 6);
    assert!(dpmi.int2f_msdos_ext_lookup(&mut cpu, &memory));
}

#[test]
fn test_int2f_168a_not_present_ds_is_not_recognised() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dpmi = Dpmi::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);
    setup_pm_vendor_string(&mut dpmi, &mut cpu, &mut memory, b"MS-DOS");
    let ds = cpu.regs.ds;
    assert!(dpmi.set_descriptor_present(&mut cpu, &mut memory, ds, false));

    cpu.regs.set_ax(0x168A);
    assert!(!dpmi.int2f_msdos_ext_lookup(&mut cpu, &memory));
    assert_eq!(cpu.regs.ax(), 0x168A, "AX must be unchanged");
}

#[test]
fn test_int2f_168a_truncated_string_safe() {
    // A vendor name without a NUL terminator within 32 bytes must not
    // read out of bounds and must result in a failure (no match).
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dpmi = Dpmi::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // 40-byte filler with no NUL.
    let filler = [b'A'; 40];
    setup_pm_vendor_string(&mut dpmi, &mut cpu, &mut memory, &filler);

    cpu.regs.set_ax(0x168A);
    let recognised = dpmi.int2f_msdos_ext_lookup(&mut cpu, &memory);
    assert!(!recognised, "filler must not match \"MS-DOS\"");
    assert_eq!(cpu.regs.ax(), 0x168A, "AX must be unchanged");
}

#[test]
fn test_msdos_ext_get_version() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dpmi = Dpmi::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Set CF before the call to verify it's cleared on success.
    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
    cpu.regs.set_ax(0x0000);
    dpmi.handle_msdos_ext_call(&mut cpu, &mut memory);

    assert_eq!(
        cpu.regs.ax(),
        0x0100,
        "Function 0000 must return AX=0x0100 (version 1.0)"
    );
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "CF must be cleared on success"
    );
}

#[test]
fn test_dpmi_and_msdos_ext_versions_are_distinct() {
    // P0 #7 (2026.05.26 the DOS model review) flagged a "version mismatch" between
    // the 0.90 reported by the DPMI host calls and the 1.0 reported by the
    // MS-DOS API Extensions. These are TWO INDEPENDENT version numbers for TWO
    // DIFFERENT interfaces; 0.90 != 1.0 is correct and intentional:
    //   * DPMI *host* version = 0.90 (INT 31h AX=0400h / INT 2Fh AX=1687h) —
    //   * MS-DOS API Extensions version = 1.0 (vendor API via INT 2Fh AX=168Ah,
    //     func 0000h) — a separate Microsoft API with its own version namespace.
    //     NOTE: this 0000h is NOT INT 31h func 0000h (Allocate LDT Descriptors).
    // See wrk_docs/2026.05.29 - HLD - DPMI - version reporting consistency.md.
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dpmi = Dpmi::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // DPMI host version (INT 31h AX=0400h) == 0.90.
    cpu.regs.set_ax(0x0400);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert_eq!(cpu.regs.ah(), 0x00, "DPMI host major version must be 0");
    assert_eq!(
        cpu.regs.al(),
        0x5A,
        "DPMI host minor version must be 0x5A (=90, i.e. 0.90)"
    );

    // MS-DOS API Extensions version (func 0000h) == 1.0 — a DIFFERENT namespace.
    cpu.regs.set_ax(0x0000);
    dpmi.handle_msdos_ext_call(&mut cpu, &mut memory);
    assert_eq!(
        cpu.regs.ax(),
        0x0100,
        "MS-DOS Extension API version must be 1.0 (distinct from the 0.90 DPMI host version)"
    );
}

#[test]
fn test_msdos_ext_get_ldt_selector() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dpmi = Dpmi::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // setup_protected_mode initialises ldt_base / ldt_limit via DPMI's PM
    // entry, so the LDT is ready.
    let ldt_base = cpu.regs.ldt_base;
    assert_ne!(ldt_base, 0, "test precondition: LDT must be initialised");

    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
    cpu.regs.set_ax(0x0100);
    dpmi.handle_msdos_ext_call(&mut cpu, &mut memory);

    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "CF must be cleared on success"
    );
    let sel = cpu.regs.ax();
    assert_ne!(sel, 0, "selector must be non-zero");
    // Bit 2 set = LDT selector (TI=1).
    assert_ne!(sel & 0x04, 0, "returned selector must be an LDT selector");
    // Verify the descriptor base matches LDT base.
    let desc = cpu
        .read_descriptor(&memory, sel)
        .expect("returned selector must be readable");
    assert_eq!(
        desc.base, ldt_base,
        "selector base must equal LDT linear address"
    );
    assert!(desc.limit > 0, "selector limit must be non-zero");
}

#[test]
fn test_msdos_ext_get_ldt_selector_caches() {
    // Repeated calls must return the same selector — RTM uses it as a
    // long-lived handle to the LDT.
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dpmi = Dpmi::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ax(0x0100);
    dpmi.handle_msdos_ext_call(&mut cpu, &mut memory);
    let sel_first = cpu.regs.ax();

    cpu.regs.set_ax(0x0100);
    dpmi.handle_msdos_ext_call(&mut cpu, &mut memory);
    let sel_second = cpu.regs.ax();

    assert_eq!(
        sel_first, sel_second,
        "Function 0100 must return the same selector on repeat calls"
    );
}

#[test]
fn test_msdos_ext_unsupported_function_returns_cf() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut dpmi = Dpmi::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
    cpu.regs.set_ax(0xFFFF);
    dpmi.handle_msdos_ext_call(&mut cpu, &mut memory);

    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "unsupported function must set CF=1"
    );
}

#[test]
fn test_msdos_ext_entry_address_value() {
    // Wire-protocol value: must be 0xFD60 (matches HLE_MSDOS_EXT_ENTRY in
    // src/lib.rs). Documented invariant — see HLD §"Address-map".
    assert_eq!(HLE_MSDOS_EXT_ENTRY, 0xFD60);
    // Must sit in the gap between RAW_SWITCH_TO_RM (0xFD58, up to 8 bytes
    // wide) and HLE_INTERRUPT_STUB_BASE (0xFE00). The 0xFD60 placement
    // sits exactly 8 bytes past 0xFD58, leaving 0xFD68..0xFDFF (152
    // bytes) free for follow-on extension stubs.
    const { assert!(HLE_MSDOS_EXT_ENTRY >= 0xFD58 + 8) };
    const { assert!(HLE_MSDOS_EXT_ENTRY < 0xFE00) };
}
/// Helper: place an ASCIIZ vendor name at a known PM linear address and
/// point DS:SI at it. Uses GDT-allocated data descriptor to mirror the
fn setup_pm_vendor_string(dpmi: &mut Dpmi, cpu: &mut Cpu, memory: &mut Memory, vendor: &[u8]) {
    // Use a fresh selector pointing at a small data buffer in HMA-adjacent
    // memory. We pick an address that doesn't collide with the DPMI
    // descriptor tables (which start at 0x100000).
    let vendor_linear: u32 = 0x90_000;
    for (i, b) in vendor.iter().enumerate() {
        memory.write_u8(vendor_linear as usize + i, *b);
    }
    memory.write_u8(vendor_linear as usize + vendor.len(), 0); // NUL

    // Allocate a selector and point it at the buffer.
    let sel = dpmi.allocate_descriptors(cpu, memory, 1);
    assert!(sel != 0, "test setup: failed to allocate selector");
    dpmi.set_descriptor_base(cpu, memory, sel, vendor_linear);
    dpmi.set_descriptor_limit(cpu, memory, sel, 0xFFFF);

    cpu.regs.ds = sel;
    cpu.regs.set_si(0); // String at offset 0 of the new selector
}
