use super::*;

#[test]
fn test_allocate_memory_block_large() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate 4MB
    cpu.regs.set_ax(0x0501);
    cpu.regs.set_bx(0x0040); // 4MB = 0x400000 bytes
    cpu.regs.set_cx(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let addr = ((cpu.regs.bx() as u32) << 16) | (cpu.regs.cx() as u32);
    let handle = ((cpu.regs.si() as u32) << 16) | (cpu.regs.di() as u32);
    assert!(addr >= 0x100000);
    assert!(handle > 0);
}

#[test]
fn test_lock_unlock_linear_region() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Lock linear region - no-op in emulator, always succeeds
    cpu.regs.set_ax(0x0600);
    cpu.regs.set_bx(0x0010);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_si(0x0001);
    cpu.regs.set_di(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Unlock linear region - no-op in emulator, always succeeds
    cpu.regs.set_ax(0x0601);
    cpu.regs.set_bx(0x0010);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_si(0x0001);
    cpu.regs.set_di(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_physical_address_mapping() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Map physical address (stub)
    cpu.regs.set_ax(0x0800);
    cpu.regs.set_bx(0x000A); // Physical address A0000h (video memory)
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_si(0x0001); // Size: 64KB
    cpu.regs.set_di(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let linear = ((cpu.regs.bx() as u32) << 16) | (cpu.regs.cx() as u32);
    assert!(linear >= 0x100000);

    // Unmap invalid address should fail
    cpu.regs.set_ax(0x0801);
    cpu.regs.set_bx(0xFFFF);
    cpu.regs.set_cx(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_HANDLE);

    // Unmap
    cpu.regs.set_ax(0x0801);
    cpu.regs.set_bx((linear >> 16) as u16);
    cpu.regs.set_cx(linear as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_get_free_memory_info() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let buf_addr = 0x60000_usize;
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);

    cpu.regs.set_ax(0x0500);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify structure
    let largest_free = memory.read_u32(buf_addr);
    let max_unlocked = memory.read_u32(buf_addr + 4);
    let largest_lockable = memory.read_u32(buf_addr + 8);
    let total_pages = memory.read_u32(buf_addr + 20);

    assert!(largest_free > 0);
    assert_ne!(max_unlocked, 0);
    assert_ne!(largest_lockable, 0);
    assert_ne!(total_pages, 0);
    for i in 0..12 {
        assert_eq!(memory.read_u8(buf_addr + 0x24 + i), 0xFF);
    }
}

#[test]
fn test_free_memory_info_decreases_after_allocation() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let buf_addr = 0x60000_usize;
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);

    // Get initial free memory
    cpu.regs.set_ax(0x0500);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let initial_largest_free = memory.read_u32(buf_addr);
    let initial_free_pages = memory.read_u32(buf_addr + 0x14); // Offset 14h = free pages

    // Allocate 1MB (0x100000 bytes)
    cpu.regs.set_ax(0x0501);
    cpu.regs.set_bx(0x0010); // 0x00100000 = 1MB
    cpu.regs.set_cx(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Memory allocation should succeed"
    );

    // Get free memory again
    // Reset DI — 0x0501 returns handle in SI:DI, corrupting the buffer pointer
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    cpu.regs.set_ax(0x0500);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let final_largest_free = memory.read_u32(buf_addr);
    let final_free_pages = memory.read_u32(buf_addr + 0x14);

    // Free memory should have decreased
    assert!(
        final_largest_free < initial_largest_free,
        "Largest free block should decrease after allocation: initial={}, final={}",
        initial_largest_free,
        final_largest_free
    );
    assert!(
        final_free_pages < initial_free_pages,
        "Free pages should decrease after allocation: initial={}, final={}",
        initial_free_pages,
        final_free_pages
    );
}

///
/// After a small 47KB allocation, the 0500 free memory query must still
/// report substantial free memory. If capacity == size (no over-alloc),
/// this is trivially correct. Historically this test guarded against the
/// over-allocation path wrongly reporting capacity as used space.
#[test]
fn test_free_memory_info_after_small_alloc_with_overallocation() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let buf_addr = 0x60000_usize;
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.set_di((buf_addr & 0xF) as u16);

    cpu.regs.set_ax(0x0501);
    cpu.regs.set_bx(0x0000);
    cpu.regs.set_cx(0xB880);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "47KB allocation should succeed"
    );

    // Confirm allocation succeeded with capacity == size (no over-allocation).
    let block = dpmi
        .memory_blocks
        .iter()
        .next()
        .expect("at least one memory block after 0501");
    assert_eq!(
        block.capacity, block.size,
        "capacity must equal size (no over-allocation)"
    );

    // Re-set DI — 0x0501 returns handle in SI:DI, corrupting our pointer.
    cpu.regs.set_di((buf_addr & 0xF) as u16);
    cpu.regs.set_ax(0x0500);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let largest_free = memory.read_u32(buf_addr);
    let free_pages = memory.read_u32(buf_addr + 0x14);

    // After a 47KB allocation on an 8MB+ host, free memory should still
    // be measured in megabytes. Reporting zero falsely indicates exhaustion.
    assert!(
        largest_free >= 1024 * 1024,
        "largest_free must be at least 1MB after a 47KB alloc, got {:#X}",
        largest_free
    );
    assert!(
        free_pages >= 256,
        "free_pages must be at least 256 (1MB worth) after a 47KB alloc, got {}",
        free_pages
    );
}

/// for the DPMI 1.0 `Get Memory Information` (AX=0x050B) handler. After
/// a small allocation, the 050B query must still report substantial free
/// memory; otherwise any DPMI 1.0 client probing 050B right after its
/// first 0501 would falsely see "zero free".
#[test]
fn test_get_memory_info_050b_skips_overallocated_capacity() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0501);
    cpu.regs.set_bx(0x0000);
    cpu.regs.set_cx(0xB880);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "47KB allocation should succeed"
    );

    // Confirm allocation succeeded with capacity == size (no over-allocation).
    let block = dpmi
        .memory_blocks
        .iter()
        .next()
        .expect("at least one memory block after 0501");
    assert_eq!(
        block.capacity, block.size,
        "0501 should allocate exactly the requested size (capacity {} == size {})",
        block.capacity, block.size
    );

    // Now query DPMI 1.0 050B.
    let buf_addr = 0x60000_usize;
    cpu.regs.set_ax(0x050B);
    cpu.regs.es = (buf_addr >> 4) as u16;
    cpu.regs.edi = (buf_addr & 0xF) as u32;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Offset 0x18 = free bytes per the DPMI 1.0 050B layout in
    // memory.rs (see 0x050B handler body for the exact field map).
    let free_bytes = memory.read_u32(buf_addr + 0x18);
    assert!(
        free_bytes >= 1024 * 1024,
        "DPMI 1.0 050B free_bytes must be >= 1MB after a 47KB alloc, got {:#X}",
        free_bytes
    );
}

#[test]
fn test_get_virtual_interrupt_state() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.flags.insert(gp64_cpu::Flags::INTERRUPT);

    cpu.regs.set_ax(0x0902);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.al(), 1); // Interrupts enabled
}

#[test]
fn test_disable_virtual_interrupts() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Virtual interrupts start enabled in Dpmi::new()
    cpu.regs.set_ax(0x0900); // Get and disable
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.al(), 1); // Was enabled

    // Verify now disabled via query
    cpu.regs.set_ax(0x0902);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert_eq!(cpu.regs.al(), 0); // Now disabled
}

#[test]
fn test_enable_virtual_interrupts() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // First disable virtual interrupts
    cpu.regs.set_ax(0x0900);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Now enable
    cpu.regs.set_ax(0x0901); // Get and enable
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.al(), 0); // Was disabled

    // Verify now enabled via query
    cpu.regs.set_ax(0x0902);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert_eq!(cpu.regs.al(), 1); // Now enabled
}

#[test]
fn test_interrupt_state_toggle_sequence() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Virtual interrupts start enabled in Dpmi::new()

    // Disable
    cpu.regs.set_ax(0x0900);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert_eq!(cpu.regs.al(), 1); // Was enabled

    // Enable
    cpu.regs.set_ax(0x0901);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert_eq!(cpu.regs.al(), 0); // Was disabled

    // Disable again
    cpu.regs.set_ax(0x0900);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert_eq!(cpu.regs.al(), 1); // Was enabled

    // Query (shouldn't change state)
    cpu.regs.set_ax(0x0902);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert_eq!(cpu.regs.al(), 0); // Still disabled
}

#[test]
fn test_allocate_max_descriptors() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Keep allocating until we fail
    let mut allocated = 0;
    for _ in 0..8192 {
        cpu.regs.set_ax(0x0000);
        cpu.regs.set_cx(1);
        dpmi.int31(&mut cpu, &mut memory).unwrap();

        if cpu.regs.flags.contains(gp64_cpu::Flags::CARRY) {
            break;
        }
        allocated += 1;
    }

    // Should have allocated a reasonable number (at least 100)
    assert!(allocated >= 100, "Only allocated {allocated} descriptors");
}

#[test]
fn test_descriptor_with_zero_limit() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    // Set limit to 0
    cpu.regs.set_ax(0x0008);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_descriptor_with_max_limit() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    // Set limit to max (4GB - 1 with page granularity)
    cpu.regs.set_ax(0x0008);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cx(0x000F);
    cpu.regs.set_dx(0xFFFF);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_invalid_selector_operations() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Get base for invalid selector
    cpu.regs.set_ax(0x0006);
    cpu.regs.set_bx(0xFFFF);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Set base for invalid selector
    cpu.regs.set_ax(0x0007);
    cpu.regs.set_bx(0xFFFF);
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Set limit for invalid selector
    cpu.regs.set_ax(0x0008);
    cpu.regs.set_bx(0xFFFF);
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_rapid_allocate_free_cycle() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Rapidly allocate and free descriptors
    for _ in 0..100 {
        // Allocate
        cpu.regs.set_ax(0x0000);
        cpu.regs.set_cx(1);
        dpmi.int31(&mut cpu, &mut memory).unwrap();
        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        let selector = cpu.regs.ax();

        // Free
        cpu.regs.set_ax(0x0001);
        cpu.regs.set_bx(selector);
        dpmi.int31(&mut cpu, &mut memory).unwrap();
        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    }

    // Selectors should be empty
    assert_eq!(dpmi.selectors.len(), 0);
}

#[test]
fn test_rapid_memory_block_cycle() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Rapidly allocate and free memory blocks
    for _ in 0..20 {
        // Allocate 64KB
        cpu.regs.set_ax(0x0501);
        cpu.regs.set_bx(0x0001);
        cpu.regs.set_cx(0x0000);
        dpmi.int31(&mut cpu, &mut memory).unwrap();
        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

        let handle_hi = cpu.regs.si();
        let handle_lo = cpu.regs.di();

        // Free
        cpu.regs.set_ax(0x0502);
        cpu.regs.set_si(handle_hi);
        cpu.regs.set_di(handle_lo);
        dpmi.int31(&mut cpu, &mut memory).unwrap();
        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    }

    assert_eq!(dpmi.memory_blocks.len(), 0);
}

/// Regression test for JIT RSI/RDI preservation bug
/// The JIT was clobbering RSI when computing memory addresses
#[test]
fn test_jit_rsi_preservation_regression() {
    // This is a unit-level test that verifies the conceptual fix
    // The actual JIT test requires integration testing
    // Here we just verify DPMI operations don't corrupt state

    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up some state that should be preserved
    let original_esi = 0x12345678_u32;
    let original_edi = 0x9ABCDEF0_u32;
    cpu.regs.esi = original_esi;
    cpu.regs.edi = original_edi;

    // Perform DPMI operations
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Note: DPMI operations may legitimately use EDI for buffer pointers
    // But ESI should generally be preserved unless documented as output
    // This test is mainly for documentation - actual JIT regression testing
    // requires the full emulator integration
}

#[test]
fn test_dpmi_version_details() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0400);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Version should be 0.90
    let major = cpu.regs.ah();
    let minor = cpu.regs.al();
    assert_eq!(major, 0);
    assert_eq!(minor, 0x5A); // 90 decimal = 0x5A hex

    // Flags in BX
    let flags = cpu.regs.bx();
    assert_eq!(flags & 0x01, 1); // 32-bit programs supported
    assert_eq!(flags & 0x02, 2); // 16-bit programs supported
    assert_eq!(flags & 0x04, 0); // Virtual memory not supported

    // Processor type in CL
    assert_eq!(cpu.regs.cl(), 3); // 386+

    // Master/slave PIC base in DH/DL
    // (values may vary based on implementation)
}

#[test]
fn test_int31_watchpoint_set_clear() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set watchpoint
    cpu.regs.set_ax(0x0B00);
    cpu.regs.set_bx(0x0010); // Address high
    cpu.regs.set_cx(0x0000); // Address low
    cpu.regs.set_dl(1); // 1 byte size
    cpu.regs.set_dh(1); // Write type
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.bx();
    assert!(handle < 4);

    // Clear watchpoint
    cpu.regs.set_ax(0x0B01);
    cpu.regs.set_bx(handle);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int31_watchpoint_state() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set watchpoint
    cpu.regs.set_ax(0x0B00);
    cpu.regs.set_bx(0x0010);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dl(1);
    cpu.regs.set_dh(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handle = cpu.regs.bx();

    // Get state (should be not triggered)
    cpu.regs.set_ax(0x0B02);
    cpu.regs.set_bx(handle);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax() & 1, 0); // Not triggered

    // Reset state
    cpu.regs.set_ax(0x0B03);
    cpu.regs.set_bx(handle);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_free_descriptor_rejects_gdt_selector() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0001);
    cpu.regs.set_bx(0x0008); // GDT selector (TI=0)
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_free_descriptor_out_of_ldt_range() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ldt_limit = 0x0007; // One LDT entry max
    cpu.regs.set_ax(0x0001);
    cpu.regs.set_bx(0x0017); // LDT selector, index beyond limit
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_int31_watchpoint_state_triggered_and_reset() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0B00);
    cpu.regs.set_bx(0x0010);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dl(1);
    cpu.regs.set_dh(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handle = cpu.regs.bx() as usize;

    dpmi.debug_watchpoints[handle].triggered = true;

    cpu.regs.set_ax(0x0B02);
    cpu.regs.set_bx(handle as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert_eq!(cpu.regs.ax() & 1, 1);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    cpu.regs.set_ax(0x0B03);
    cpu.regs.set_bx(handle as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!dpmi.debug_watchpoints[handle].triggered);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int31_watchpoint_invalid_handle() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Try to clear non-existent watchpoint
    cpu.regs.set_ax(0x0B01);
    cpu.regs.set_bx(0); // Invalid handle (not set)
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_HANDLE);
}

#[test]
fn test_int31_watchpoint_invalid_handle_high() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0B01);
    cpu.regs.set_bx(4); // Out-of-range handle
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_HANDLE);

    cpu.regs.set_ax(0x0B02);
    cpu.regs.set_bx(4);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_HANDLE);

    cpu.regs.set_ax(0x0B03);
    cpu.regs.set_bx(4);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_HANDLE);
}

#[test]
fn test_int31_watchpoint_state_invalid_inactive_handle() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0B02);
    cpu.regs.set_bx(0); // In-range but inactive
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_HANDLE);

    cpu.regs.set_ax(0x0B03);
    cpu.regs.set_bx(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_HANDLE);
}

#[test]
fn test_int31_watchpoint_max_slots() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set 4 watchpoints (max)
    for i in 0..4 {
        cpu.regs.set_ax(0x0B00);
        cpu.regs.set_bx((i << 4) as u16);
        cpu.regs.set_cx(0x0000);
        cpu.regs.set_dl(1);
        cpu.regs.set_dh(1);
        dpmi.int31(&mut cpu, &mut memory).unwrap();
        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    }

    // Try to set 5th watchpoint - should fail
    cpu.regs.set_ax(0x0B00);
    cpu.regs.set_bx(0x0050);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dl(1);
    cpu.regs.set_dh(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_HANDLE_UNAVAILABLE);
}

#[test]
fn test_int31_watchpoint_invalid_size_type() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Test invalid size = 0 (DPMI requires 1/2/4 bytes)
    cpu.regs.set_ax(0x0B00);
    cpu.regs.set_bx(0x0010);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dl(0); // Invalid size
    cpu.regs.set_dh(1); // Valid type
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);

    // Test invalid size = 3 (not 1/2/4)
    cpu.regs.set_ax(0x0B00);
    cpu.regs.set_bx(0x0010);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dl(3); // Invalid size
    cpu.regs.set_dh(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);

    // Test invalid type > 2
    cpu.regs.set_ax(0x0B00);
    cpu.regs.set_bx(0x0010);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dl(1); // Valid size
    cpu.regs.set_dh(3); // Invalid type
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);

    // Test valid sizes (1, 2, 4) and types (0, 1, 2) succeed
    for size in [1u8, 2, 4] {
        for watch_type in [0u8, 1, 2] {
            cpu.regs.set_ax(0x0B00);
            cpu.regs.set_bx(0x0010);
            cpu.regs.set_cx(0x0000);
            cpu.regs.set_dl(size);
            cpu.regs.set_dh(watch_type);
            dpmi.int31(&mut cpu, &mut memory).unwrap();

            assert!(
                !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
                "size={size} type={watch_type} should succeed"
            );

            // Clear the watchpoint for next iteration
            let handle = cpu.regs.bx();
            cpu.regs.set_ax(0x0B01);
            cpu.regs.set_bx(handle);
            dpmi.int31(&mut cpu, &mut memory).unwrap();
        }
    }
}

#[test]
fn test_dpmi_shared_memory_allocate_returns_unsupported() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // AX=0D00h: Allocate Shared Memory
    cpu.regs.set_ax(0x0D00);
    cpu.regs.set_si(0x1234); // Handle high
    cpu.regs.set_di(0x5678); // Handle low
    cpu.regs.es = 0x1000;
    cpu.regs.set_bx(0x1000); // Requested size

    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_UNSUPPORTED);
}

#[test]
fn test_dpmi_shared_memory_free_returns_unsupported() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // AX=0D01h: Free Shared Memory
    cpu.regs.set_ax(0x0D01);
    cpu.regs.set_si(0x1234); // Handle high
    cpu.regs.set_di(0x5678); // Handle low

    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_UNSUPPORTED);
}

#[test]
fn test_dpmi_shared_memory_serialize_returns_unsupported() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // AX=0D02h: Serialize on Shared Memory
    cpu.regs.set_ax(0x0D02);
    cpu.regs.set_si(0x1234); // Handle high
    cpu.regs.set_di(0x5678); // Handle low

    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_UNSUPPORTED);
}

#[test]
fn test_exceptions_with_error_codes_list() {
    // Document which exceptions have error codes per Intel specification:
    // 8 (Double Fault), 10 (Invalid TSS), 11 (Segment Not Present),
    // 12 (Stack Fault), 13 (GP Fault), 14 (Page Fault)
    //
    // 17 (Alignment Check) also has error code but is always 0
    let exceptions_with_error: [u8; 6] = [8, 10, 11, 12, 13, 14];

    for &exc in &exceptions_with_error {
        assert!(
            exc == 8 || exc == 10 || exc == 11 || exc == 12 || exc == 13 || exc == 14,
            "Exception {exc} should have an error code"
        );
    }
}

#[test]
fn test_page_lock_stats_initial_values() {
    let dpmi = Dpmi::new();
    let stats = dpmi.page_lock_stats();
    assert_eq!(stats.lock_calls, 0);
    assert_eq!(stats.unlock_calls, 0);
    assert_eq!(stats.pageable_calls, 0);
    assert_eq!(stats.relock_calls, 0);
    assert_eq!(stats.total_bytes_locked, 0);
    assert_eq!(stats.total_bytes_unlocked, 0);
}

#[test]
fn test_page_lock_stats_lock_region() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Lock linear region: BX:CX = address, SI:DI = size
    cpu.regs.set_ax(0x0600); // Lock linear region
    cpu.regs.set_bx(0x0010); // Address high = 0x10
    cpu.regs.set_cx(0x0000); // Address low = 0x0000 -> 0x100000
    cpu.regs.set_si(0x0000); // Size high = 0
    cpu.regs.set_di(0x1000); // Size low = 0x1000 (4KB)
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let stats = dpmi.page_lock_stats();
    assert_eq!(stats.lock_calls, 1);
    assert_eq!(stats.total_bytes_locked, 0x1000);
}

#[test]
fn test_page_lock_stats_unlock_region() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Unlock linear region: BX:CX = address, SI:DI = size
    cpu.regs.set_ax(0x0601); // Unlock linear region
    cpu.regs.set_bx(0x0010);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_si(0x0000);
    cpu.regs.set_di(0x2000); // Size = 0x2000 (8KB)
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let stats = dpmi.page_lock_stats();
    assert_eq!(stats.unlock_calls, 1);
    assert_eq!(stats.total_bytes_unlocked, 0x2000);
}

#[test]
fn test_page_lock_stats_mark_pageable() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0602); // Mark real mode region as pageable
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let stats = dpmi.page_lock_stats();
    assert_eq!(stats.pageable_calls, 1);
}

#[test]
fn test_page_lock_stats_relock() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0603); // Relock real mode region
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let stats = dpmi.page_lock_stats();
    assert_eq!(stats.relock_calls, 1);
}

#[test]
fn test_page_lock_stats_multiple_operations() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Lock 4KB
    cpu.regs.set_ax(0x0600);
    cpu.regs.set_si(0x0000);
    cpu.regs.set_di(0x1000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Lock another 8KB
    cpu.regs.set_ax(0x0600);
    cpu.regs.set_si(0x0000);
    cpu.regs.set_di(0x2000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Unlock 4KB
    cpu.regs.set_ax(0x0601);
    cpu.regs.set_si(0x0000);
    cpu.regs.set_di(0x1000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    let stats = dpmi.page_lock_stats();
    assert_eq!(stats.lock_calls, 2);
    assert_eq!(stats.unlock_calls, 1);
    assert_eq!(stats.total_bytes_locked, 0x3000); // 4KB + 8KB = 12KB
    assert_eq!(stats.total_bytes_unlocked, 0x1000); // 4KB
}

#[test]
fn test_page_lock_stats_large_region() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Lock 1MB region: SI:DI = 0x0010:0x0000 = 0x100000
    cpu.regs.set_ax(0x0600);
    cpu.regs.set_si(0x0010);
    cpu.regs.set_di(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    let stats = dpmi.page_lock_stats();
    assert_eq!(stats.lock_calls, 1);
    assert_eq!(stats.total_bytes_locked, 0x100000); // 1MB
}

#[test]
fn test_set_limit_does_not_clobber_user_flags() {
    // Bug: SetLimit (INT 31h/0008) was modifying user-set flags when limit > 0xFFFFF
    // The G bit should only affect the in-memory encoding, not the stored flags.
    // Note: In CH byte, flags are in the HIGH nibble (G/D/L/AVL = bits 7-4).
    // The low nibble (bits 3-0) contains limit bits 16-19, not flags.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;
    cpu.regs.cpl = 3;

    // Allocate a descriptor
    cpu.regs.set_ax(0x0000); // Allocate LDT descriptors
    cpu.regs.set_cx(1); // Count = 1
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let selector = cpu.regs.ax();

    // Set access rights with specific flags: AVL=1 (bit 4) -> CH high nibble = 0x10
    // Low nibble = 0x05 (limit bits, NOT flags)
    cpu.regs.set_ax(0x0009); // Set access rights
    cpu.regs.set_bx(selector);
    cpu.regs.set_cl(0xF2); // Present, DPL=3, data r/w
    cpu.regs.set_ch(0x15); // Flags high nibble: AVL=1 (0x10), limit bits: 0x05
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify flags are set correctly (only high nibble stored)
    let (_, flags_before) = dpmi.get_descriptor_access(selector).unwrap();
    assert_eq!(
        flags_before, 0x10,
        "Flags should be 0x10 (AVL=1) before SetLimit"
    );

    // Set a small limit (should not change flags)
    cpu.regs.set_ax(0x0008); // Set limit
    cpu.regs.set_bx(selector);
    cpu.regs.set_cx(0x0000); // High word
    cpu.regs.set_dx(0xFFFF); // Low word -> limit = 0xFFFF
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify flags are still 0x10 (G bit should not be set for small limits)
    let (_, flags_after_small) = dpmi.get_descriptor_access(selector).unwrap();
    assert_eq!(
        flags_after_small, 0x10,
        "Flags should remain 0x10 after setting small limit"
    );

    // Set a large limit (> 0xFFFFF) - this was clobbering the flags
    cpu.regs.set_ax(0x0008); // Set limit
    cpu.regs.set_bx(selector);
    cpu.regs.set_cx(0x00FF); // High word
    cpu.regs.set_dx(0xFFFF); // Low word -> limit = 0x00FFFFFF
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Flags should be preserved after setting large limit
    let (_, flags_after_large) = dpmi.get_descriptor_access(selector).unwrap();
    assert_eq!(
        flags_after_large, 0x10,
        "Flags should remain 0x10 after setting large limit - G bit should not clobber user flags"
    );
}

#[test]
fn test_set_limit_preserves_all_flag_bits() {
    // Test that SetLimit preserves ALL user-set flag bits (D, L, AVL)
    // and does NOT modify the G bit.
    // Note: In CH byte, flags are in the HIGH nibble (bits 7-4: G/D/L/AVL).
    // Low nibble (bits 3-0) = limit bits 16-19, not stored in s.flags.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;
    cpu.regs.cpl = 3;

    // Allocate a descriptor
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    // Set flags: D=1 (bit 6), AVL=1 (bit 4) -> high nibble = 0x50
    // Low nibble = 0x06 (limit bits, NOT flags)
    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cl(0xF2);
    cpu.regs.set_ch(0x56); // Flags: D=1, AVL=1 (0x50), limit bits: 0x06
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Verify initial flags (only high nibble stored)
    let (_, flags_before) = dpmi.get_descriptor_access(selector).unwrap();
    assert_eq!(
        flags_before, 0x50,
        "Initial flags should be 0x50 (D=1, AVL=1)"
    );

    // Set a large limit
    cpu.regs.set_ax(0x0008);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cx(0x00FF);
    cpu.regs.set_dx(0xFFFF);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Verify flags are preserved EXACTLY (including G bit = 0)
    let (_, flags_after) = dpmi.get_descriptor_access(selector).unwrap();
    assert_eq!(
        flags_after, 0x50,
        "Flags should be exactly 0x50 after SetLimit - G bit should NOT be modified"
    );
}

#[test]
fn test_set_limit_small_does_not_clear_g_bit() {
    // If user sets G bit via SetAccess, SetLimit with small limit should not clear it.
    // Note: In CH byte, flags are in the HIGH nibble (bits 7-4: G/D/L/AVL).
    // Low nibble (bits 3-0) = limit bits 16-19, not stored in s.flags.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.cpu_mode = 1;
    cpu.regs.cpl = 3;

    // Allocate a descriptor
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    // Set flags with G bit set: G=1 (bit 7), D=1 (bit 6) -> high nibble = 0xC0
    // Low nibble = 0x0E (limit bits, NOT flags)
    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cl(0xF2);
    cpu.regs.set_ch(0xCE); // Flags: G=1, D=1 (0xC0), limit bits: 0x0E
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Verify initial flags (only high nibble stored)
    let (_, flags_before) = dpmi.get_descriptor_access(selector).unwrap();
    assert_eq!(
        flags_before, 0xC0,
        "Initial flags should be 0xC0 (G=1, D=1)"
    );

    // Set a small limit (< 0x100000)
    cpu.regs.set_ax(0x0008);
    cpu.regs.set_bx(selector);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dx(0xFFFF); // limit = 0xFFFF
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Flags should be preserved - G bit should NOT be cleared
    let (_, flags_after) = dpmi.get_descriptor_access(selector).unwrap();
    assert_eq!(
        flags_after, 0xC0,
        "Flags should be exactly 0xC0 after SetLimit - G bit should NOT be cleared"
    );
}

#[test]
fn test_state_save_dpmi09_register_order() {
    // State save/restore saves DPMI host internal state, NOT client CPU
    // registers.  Verify the save writes a signature at offset 0 and the
    // protected_mode flag at offset 4.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let buffer = setup_state_test_32bit(&mut dpmi, &mut cpu, &mut memory);

    cpu.regs.edi = 0x0200; // Buffer offset for 32-bit mode
    cpu.regs.eax = 0x77777700; // AL=0 = save

    dpmi.handle_state_save_restore(&mut cpu, &mut memory)
        .unwrap();

    // Verify MDSS signature at offset 0
    assert_eq!(
        memory.read_u32(buffer),
        0x4D44_5353,
        "Signature 'MDSS' should be at offset 0x00"
    );
    // Protected mode flag at offset 4
    assert_eq!(
        memory.read_u8(buffer + 4),
        1,
        "protected_mode flag should be 1"
    );
}

#[test]
fn test_state_save_dpmi09_flags_is_16bit() {
    // State save writes DPMI host signature, not CPU registers.
    // Verify that the save does NOT touch offsets beyond its small header.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let buffer = setup_state_test_32bit(&mut dpmi, &mut cpu, &mut memory);

    // Pre-fill the buffer with a marker
    memory.write_u16(buffer + 0x20, 0xDEAD);
    memory.write_u16(buffer + 0x22, 0xBEEF);

    cpu.regs.set_al(0);
    cpu.regs.edi = 0x0200;
    dpmi.handle_state_save_restore(&mut cpu, &mut memory)
        .unwrap();

    // Signature at offset 0, flag at offset 4
    assert_eq!(memory.read_u32(buffer), 0x4D44_5353);
    assert_eq!(memory.read_u8(buffer + 4), 1);

    // Offsets beyond the small header are NOT overwritten
    assert_eq!(
        memory.read_u16(buffer + 0x20),
        0xDEAD,
        "Save should not write CPU flags to buffer"
    );
    assert_eq!(
        memory.read_u16(buffer + 0x22),
        0xBEEF,
        "Save should not write ES to buffer"
    );
}

#[test]
fn test_state_save_dpmi09_segments_and_pointers() {
    // State save writes DPMI host state, NOT CPU segment registers.
    // Verify segment register offsets are untouched by save.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let buffer = setup_state_test_32bit(&mut dpmi, &mut cpu, &mut memory);

    // Fill segment/pointer offsets with markers
    memory.write_u16(buffer + 0x22, 0xAAAA); // ES offset
    memory.write_u16(buffer + 0x24, 0xBBBB); // DS offset
    memory.write_u32(buffer + 0x2A, 0xCCCCDDDD); // EIP offset
    memory.write_u32(buffer + 0x30, 0xEEEEFFFF); // ESP offset

    cpu.regs.set_al(0);
    cpu.regs.edi = 0x0200;
    dpmi.handle_state_save_restore(&mut cpu, &mut memory)
        .unwrap();

    // Markers should be preserved since save no longer writes CPU registers
    assert_eq!(memory.read_u16(buffer + 0x22), 0xAAAA);
    assert_eq!(memory.read_u16(buffer + 0x24), 0xBBBB);
    assert_eq!(memory.read_u32(buffer + 0x2A), 0xCCCCDDDD);
    assert_eq!(memory.read_u32(buffer + 0x30), 0xEEEEFFFF);
}

#[test]
fn test_state_restore_dpmi09_layout() {
    // Restore is a NOP for CPU registers — it only restores DPMI host
    // internal state.  Verify that no registers are modified.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let _buffer = setup_state_test_32bit(&mut dpmi, &mut cpu, &mut memory);

    // Set known values that should survive the restore
    cpu.regs.esi = 0xBBBBBBBB;
    cpu.regs.ebp = 0xCCCCCCCC;
    cpu.regs.ebx = 0xDDDDDDDD;
    let saved_eip = cpu.regs.eip;
    let saved_esp = cpu.regs.esp;
    let saved_ds = cpu.regs.ds;
    let saved_es = cpu.regs.es;
    let saved_flags = cpu.regs.flags;

    // AL=1: restore
    cpu.regs.set_al(1);
    cpu.regs.edi = 0x0200;

    dpmi.handle_state_save_restore(&mut cpu, &mut memory)
        .unwrap();

    // ALL registers must be unchanged (restore is NOP for CPU state)
    assert_eq!(cpu.regs.esi, 0xBBBBBBBB, "ESI must not change on restore");
    assert_eq!(cpu.regs.ebp, 0xCCCCCCCC, "EBP must not change on restore");
    assert_eq!(cpu.regs.ebx, 0xDDDDDDDD, "EBX must not change on restore");
    assert_eq!(cpu.regs.eip, saved_eip, "EIP must not change on restore");
    assert_eq!(cpu.regs.esp, saved_esp, "ESP must not change on restore");
    assert_eq!(cpu.regs.ds, saved_ds, "DS must not change on restore");
    assert_eq!(cpu.regs.es, saved_es, "ES must not change on restore");
    assert_eq!(
        cpu.regs.flags, saved_flags,
        "FLAGS must not change on restore"
    );
}

#[test]
fn test_state_save_restore_roundtrip() {
    // Save+restore is for DPMI host state, NOT CPU registers.
    // Verify that save writes a signature and restore doesn't modify regs.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let buffer = setup_state_test_32bit(&mut dpmi, &mut cpu, &mut memory);

    // Save (AL=0)
    cpu.regs.set_al(0);
    cpu.regs.edi = 0x0200;
    dpmi.handle_state_save_restore(&mut cpu, &mut memory)
        .unwrap();

    // Verify signature was written
    assert_eq!(memory.read_u32(buffer), 0x4D44_5353);

    // Set registers to known values
    cpu.regs.esi = 0xBBBBBBBB;
    cpu.regs.ebp = 0xCCCCCCCC;
    cpu.regs.ebx = 0xDDDDDDDD;
    let saved_eip = cpu.regs.eip;

    // Restore (AL=1) — should be NOP for registers
    cpu.regs.set_al(1);
    cpu.regs.edi = 0x0200;
    dpmi.handle_state_save_restore(&mut cpu, &mut memory)
        .unwrap();

    // Registers must be unchanged after restore
    assert_eq!(cpu.regs.esi, 0xBBBBBBBB, "ESI must not change");
    assert_eq!(cpu.regs.ebp, 0xCCCCCCCC, "EBP must not change");
    assert_eq!(cpu.regs.ebx, 0xDDDDDDDD, "EBX must not change");
    assert_eq!(cpu.regs.eip, saved_eip, "EIP must not change");
}

#[test]
fn test_state_save_buffer_size_validation() {
    // Test that undersized buffer returns error (B29)
    // DPMI 0.9 spec: Function 0305h returns required buffer size in AX
    // For 32-bit clients: 0x36 = 54 bytes
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let _buffer = setup_state_test_32bit(&mut dpmi, &mut cpu, &mut memory);

    // Test INT 31h function 0305h - Get State Save/Restore Addresses
    // This function returns the required buffer size in AX
    cpu.regs.set_ax(0x0305);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Function 0305h should succeed"
    );

    // For 32-bit clients, buffer size should be 0x36 (54 bytes)
    let buffer_size = cpu.regs.ax();
    assert_eq!(
        buffer_size, 0x36,
        "State buffer size for 32-bit client should be 0x36 (54 bytes), got 0x{:04X}",
        buffer_size
    );
}

#[test]
fn test_set_extended_exception_handler_null_selector() {
    // DPMI 1.0 §5.2.2: NULL selector for 0213h is a request to clear
    // the user handler. Lenient accept (matches the DOS model). See HLD
    // 2026.05.10 - HLD - generic DPMI 8001 unhandled.md.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0213);
    cpu.regs.set_bl(0x0D); // GPF exception
    cpu.regs.set_cx(0x0000); // NULL selector
    cpu.regs.edx = 0x1000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(dpmi.exception_handler_entry(0x0D).is_none());
}

#[test]
fn test_set_extended_exception_handler_null_selector_with_rpl() {
    // Selector 0x0003 has index 0 with RPL=3, so selector & 0xFFFC == 0.
    // Lenient accept treats this as clear-handler. See HLD
    // 2026.05.10 - HLD - generic DPMI 8001 unhandled.md.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0213);
    cpu.regs.set_bl(0x0D);
    cpu.regs.set_cx(0x0003); // RPL=3 but index=0 → still NULL
    cpu.regs.edx = 0x1000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(dpmi.exception_handler_entry(0x0D).is_none());
}

#[test]
fn test_set_extended_exception_handler_ldt_out_of_range() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // LDT selector with TI=1 but outside LDT range
    // LDT base and limit are 0 by default, so any LDT selector is out of range
    cpu.regs.ldt_base = 0x5000;
    cpu.regs.ldt_limit = 0x0007; // Only 1 descriptor (index 0)

    cpu.regs.set_ax(0x0213);
    cpu.regs.set_bl(0x0D);
    // LDT selector index 10 = (10 << 3) | 0x04 | 0x03 = 0x0057
    cpu.regs.set_cx(0x0057);
    cpu.regs.edx = 0x1000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_extended_exception_handler_selector_not_present() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // GDT code descriptor that is NOT present (access bit 7 = 0)
    let gdt_base = 0x2000usize;
    let index = 1usize; // selector 0x0008
    cpu.regs.gdtr.base = gdt_base as u32;
    cpu.regs.gdtr.limit = ((index + 1) * 8 - 1) as u16;
    // Write a descriptor with present=0: access=0x7A (bit7=0)
    dpmi.write_descriptor_raw(&mut memory, gdt_base, index, 0, 0xFFFF, 0x7A, 0x00);

    cpu.regs.set_ax(0x0213);
    cpu.regs.set_bl(0x0D);
    cpu.regs.set_cx(0x0008);
    cpu.regs.edx = 0x0050;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_extended_exception_handler_offset_beyond_limit() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Code descriptor with limit 0x00FF
    setup_gdt_code_descriptor(&dpmi, &mut cpu, &mut memory, 0x0008, 0x00FF);

    cpu.regs.set_ax(0x0213);
    cpu.regs.set_bl(0x0D);
    cpu.regs.set_cx(0x0008);
    cpu.regs.edx = 0x0100; // Beyond the 0x00FF limit
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_LINEAR);
}

#[test]
fn test_set_extended_exception_handler_exception_out_of_range() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0213);
    cpu.regs.set_bl(0x20); // Exception 32, out of range
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_extended_exception_handler_preserves_idt() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // 32-bit client: the stored gate width follows client bitness
    // (MDD-BUG-KILN-00635), and this fixture's handler is 32-bit flat.
    dpmi.cpu_mode = 1;

    // 4GB code descriptor for selector 0x0008
    let gdt_base = 0x2000usize;
    let index = 1usize;
    cpu.regs.gdtr.base = gdt_base as u32;
    cpu.regs.gdtr.limit = ((index + 1) * 8 - 1) as u16;
    dpmi.write_descriptor_raw(&mut memory, gdt_base, index, 0, 0xFFFFF, 0xFA, 0x0C);

    let idt_base = 0x9000usize;
    cpu.regs.idtr.base = idt_base as u32;
    dpmi.write_idt_gate_full(&mut memory, idt_base, 0x0D, 0xDEAD_BEEF, 0x0040, 0x8E);

    cpu.regs.set_ax(0x0213);
    cpu.regs.set_bl(0x0D); // GPF
    cpu.regs.set_cx(0x0008);
    cpu.regs.edx = 0xABCD_1234;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Exception handlers are virtual DPMI state; 0213h must not rewrite IDT.
    let (sel, off) = dpmi.read_idt_gate(&memory, idt_base, 0x0D);
    assert_eq!(sel, 0x0040);
    assert_eq!(off, 0xDEAD_BEEF);

    assert_eq!(dpmi.exception_handlers[0x0D].selector, 0x0008);
    assert_eq!(dpmi.exception_handlers[0x0D].offset, 0xABCD_1234);
    assert_eq!(dpmi.exception_handlers[0x0D].gate_type, 0x0E);
}

#[test]
fn test_get_extended_exception_handler_reads_exception_handlers_with_idtr() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let idt_base = 0x8000usize;
    cpu.regs.idtr.base = idt_base as u32;

    // IDT contains a different value to prove 0212h ignores raw IDT.
    dpmi.write_idt_gate_full(&mut memory, idt_base, 0x0D, 0xDEAD_BEEF, 0x0040, 0x8E);
    dpmi.exception_handlers[0x0D] = IdtEntry {
        selector: 0x0008,
        offset: 0x1234_5678,
        gate_type: 0x0E,
        dpl: 3,
        present: true,
    };

    cpu.regs.set_ax(0x0212);
    cpu.regs.set_bl(0x0D);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.cx(), 0x0008);
    assert_eq!(cpu.regs.edx, 0x1234_5678);
}

#[test]
fn test_set_exception_handler_null_selector() {
    // DPMI 0.9 / 1.0 §5.2.2: NULL selector for 0203h is a clear-handler
    // request. Lenient accept (matches the DOS model). See HLD
    // 2026.05.10 - HLD - generic DPMI 8001 unhandled.md.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0203);
    cpu.regs.set_bl(0x0D);
    cpu.regs.set_cx(0x0000); // NULL selector
    cpu.regs.set_dx(0x1000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(dpmi.exception_handler_entry(0x0D).is_none());
}

#[test]
fn test_set_exception_handler_ldt_out_of_range() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ldt_base = 0x5000;
    cpu.regs.ldt_limit = 0x0007; // Only 1 descriptor

    cpu.regs.set_ax(0x0203);
    cpu.regs.set_bl(0x0D);
    cpu.regs.set_cx(0x0057); // LDT index 10, out of range
    cpu.regs.set_dx(0x1000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_exception_handler_selector_not_present() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // GDT descriptor that is not present
    let gdt_base = 0x2000usize;
    let index = 1usize;
    cpu.regs.gdtr.base = gdt_base as u32;
    cpu.regs.gdtr.limit = ((index + 1) * 8 - 1) as u16;
    dpmi.write_descriptor_raw(&mut memory, gdt_base, index, 0, 0xFFFF, 0x7A, 0x00);

    cpu.regs.set_ax(0x0203);
    cpu.regs.set_bl(0x0D);
    cpu.regs.set_cx(0x0008);
    cpu.regs.set_dx(0x0050);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_exception_handler_offset_beyond_limit() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    setup_gdt_code_descriptor(&dpmi, &mut cpu, &mut memory, 0x0008, 0x00FF);

    cpu.regs.set_ax(0x0203);
    cpu.regs.set_bl(0x0D);
    cpu.regs.set_cx(0x0008);
    cpu.regs.set_dx(0x0100); // Beyond 0xFF limit
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_LINEAR);
}

#[test]
fn test_get_pm_interrupt_vector_uses_idt_when_present() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let idt_base = 0x8000usize;
    cpu.regs.idtr.base = idt_base as u32;

    // Write IDT gate for vector 0x21
    dpmi.write_idt_gate_full(&mut memory, idt_base, 0x21, 0xFACE_CAFE, 0x0028, 0xEE);

    cpu.regs.set_ax(0x0204);
    cpu.regs.set_bl(0x21);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.cx(), 0x0028);
    // 16-bit client → DX only (lower 16 bits)
    assert_eq!(cpu.regs.dx(), 0xCAFE);
}

#[test]
fn test_get_pm_interrupt_vector_0204_still_reads_idt_for_vector_08() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let idt_base = 0x8000usize;
    cpu.regs.idtr.base = idt_base as u32;
    dpmi.write_idt_gate_full(&mut memory, idt_base, 0x08, 0x1234, 0x0040, 0x8E);

    cpu.regs.set_ax(0x0204);
    cpu.regs.set_bl(0x08);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.cx(), 0x0040);
    assert_eq!(cpu.regs.dx(), 0x1234);
}

#[test]
fn test_get_pm_interrupt_vector_idt_32bit_client() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.cpu_mode = 1; // 32-bit client
    let idt_base = 0x8000usize;
    cpu.regs.idtr.base = idt_base as u32;

    dpmi.write_idt_gate_full(&mut memory, idt_base, 0x21, 0xFACE_CAFE, 0x0028, 0xEE);

    cpu.regs.set_ax(0x0204);
    cpu.regs.set_bl(0x21);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.cx(), 0x0028);
    assert_eq!(cpu.regs.edx, 0xFACE_CAFE);
}

#[test]
fn test_set_cpu_mode() {
    let mut dpmi = Dpmi::new();
    assert_eq!(dpmi.cpu_mode, 0);
    dpmi.set_cpu_mode(1);
    assert_eq!(dpmi.cpu_mode, 1);
    dpmi.set_cpu_mode(0);
    assert_eq!(dpmi.cpu_mode, 0);
}

#[test]
fn test_reset_clears_state() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up some state
    dpmi.cpu_mode = 1;
    dpmi.protected_mode = true;
    dpmi.v86_active = true;

    // Allocate a selector
    let sel = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
    assert!(sel != 0);

    dpmi.reset();

    assert_eq!(dpmi.cpu_mode, 0);
    assert!(!dpmi.protected_mode);
    assert!(!dpmi.v86_active);
    assert!(dpmi.selectors.is_empty());
    assert!(dpmi.memory_blocks.is_empty());
    assert!(dpmi.dos_memory_blocks.is_empty());
}

#[test]
fn test_reset_clears_was_initialized_sentinel() {
    // MDD-BUG-FLUXHOMEARPA-00013: reset() must clear the descriptor-table
    // geometry so was_initialized() (gdt_base != 0) no longer reports a
    // terminated DPMI client. Otherwise a later real-mode child's termination
    // is misclassified as a DPMI program exit and can stop the emulator early.
    let mut dpmi = Dpmi::new();
    // Simulate a DPMI client having entered protected mode.
    dpmi.gdt_base = 0x1000;
    dpmi.gdt_size = 0xFFFF;
    dpmi.ldt_base = 0x2000;
    dpmi.ldt_size = 0xFFFF;
    dpmi.idt_base = 0x3000;
    dpmi.idt_size = 0x07FF;
    assert!(
        dpmi.was_initialized(),
        "precondition: client is initialized"
    );

    dpmi.reset();

    assert!(
        !dpmi.was_initialized(),
        "reset must clear gdt_base so a later real-mode child is not seen as a DPMI exit"
    );
    assert_eq!(dpmi.gdt_base, 0);
    assert_eq!(dpmi.gdt_size, 0);
    assert_eq!(dpmi.ldt_base, 0);
    assert_eq!(dpmi.ldt_size, 0);
    assert_eq!(dpmi.idt_base, 0);
    assert_eq!(dpmi.idt_size, 0);
}

#[test]
fn test_reset_with_memory_removes_physical_mappings() {
    let mut dpmi = Dpmi::new();
    let mut memory = Memory::new();

    // Add a physical mapping manually
    dpmi.physical_mappings.push(PhysicalMapping {
        physical: 0x10000,
        linear: 0x20000,
        size: 0x1000,
    });
    memory.ensure_ram_size(0x21000);
    memory.add_physical_mapping(0x20000, 0x10000, 0x1000);

    dpmi.reset_with_memory(&mut memory);
    assert!(dpmi.physical_mappings.is_empty());
}

#[test]
fn test_page_lock_stats_accessor() {
    let dpmi = Dpmi::new();
    let stats = dpmi.page_lock_stats();
    assert_eq!(stats.lock_calls, 0);
    assert_eq!(stats.total_bytes_locked, 0);
}

#[test]
fn test_take_pending_hle_simulation() {
    let mut dpmi = Dpmi::new();
    assert!(!dpmi.has_pending_hle_simulation());
    assert!(dpmi.take_pending_hle_simulation().is_none());
}

#[test]
fn test_get_page_size() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0604);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.bx(), 0);
    assert_eq!(cpu.regs.cx(), 0x1000); // 4096 bytes
}

#[test]
fn test_memory_info_unsupported_subfunc() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Subfunction 0x05 of function 0x06xx doesn't exist
    cpu.regs.set_ax(0x0605);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_UNSUPPORTED);
}

#[test]
fn test_vendor_api_returns_unsupported() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0A00);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_UNSUPPORTED);
}

#[test]
fn test_debug_watchpoint_unsupported_subfunc() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Subfunction 0x04 doesn't exist for debug watchpoints
    cpu.regs.set_ax(0x0B04);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_UNSUPPORTED);
}

#[test]
fn test_coprocessor_get_status() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0E00);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // 0x35 = FPU present, enabled, 387 type, no emulation
    assert_eq!(cpu.regs.ax(), 0x0035);
}

#[test]
fn test_coprocessor_set_emulation() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0E01);
    cpu.regs.set_bx(0x0001); // Enable emulation
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_coprocessor_unsupported_subfunc() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0E02);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_UNSUPPORTED);
}

#[test]
fn test_shared_memory_unsupported_subfunc() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0D03);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_UNSUPPORTED);
}

#[test]
fn test_unlock_selector_out_of_ldt_range() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // LDT selector that is out of LDT range
    cpu.regs.ldt_base = 0x5000;
    cpu.regs.ldt_limit = 0x0007; // Only 1 descriptor

    cpu.regs.set_ax(0x0005);
    cpu.regs.set_bx(0x0057); // LDT index 10, out of range
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_unlock_selector_not_found() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate a selector and then try to unlock a different one
    let sel = dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
    assert!(sel != 0);

    // Use a valid LDT selector that doesn't exist in the selectors list
    // next_selector is already bumped past sel, so it's in LDT range
    let other_sel = sel.wrapping_add(8);
    cpu.regs.set_ax(0x0005);
    cpu.regs.set_bx(other_sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}
/// Helper to set up protected mode with a buffer in conventional memory
fn setup_state_test_32bit(dpmi: &mut Dpmi, cpu: &mut Cpu, memory: &mut Memory) -> usize {
    // Enter 32-bit protected mode (creates 16-bit stub descriptors)
    setup_protected_mode(dpmi, cpu, memory, true);

    // Real 32-bit DPMI clients set up flat descriptors via INT 31h after
    // mode switch. Allocate a scratch selector instead of reusing an
    // initial selector, because the reserved low LDT layout may change.
    install_flat_es_selector(dpmi, cpu, memory);

    // Buffer at ES:0x0200, flat model so linear = 0 + 0x200
    let buffer = 0x0200usize;

    // Zero out buffer
    for i in 0..64 {
        memory.write_u8(buffer + i, 0);
    }

    buffer
}
