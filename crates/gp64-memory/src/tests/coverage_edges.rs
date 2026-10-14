use super::*;

// ========== reset_ram ==========

#[test]
fn test_reset_ram_clears_and_reinitializes() {
    let mut mem = Memory::new();
    mem.write_u8(0x5000, 0xFF);
    mem.reset_ram();

    // Normal RAM should be zeroed
    assert_eq!(mem.ram[0x5000], 0x00);

    // BDA should be re-initialized
    assert_eq!(mem.ram[0x410], 0x21);
    assert_eq!(mem.ram[0x449], 0x03);

    // IRET stub should be restored
    let iret_addr = Memory::seg_offset_to_linear(0xF000, 0xFF00);
    assert_eq!(mem.ram[iret_addr], 0xCF);

    // Instruction cache dirty flag should be set
    assert!(mem.is_instr_cache_dirty());
}

// ========== hash_state ==========

#[test]
fn test_hash_state_deterministic() {
    let mem1 = Memory::new();
    let mem2 = Memory::new();
    assert_eq!(mem1.hash_state(), mem2.hash_state());
}

#[test]
fn test_hash_state_changes_on_write() {
    let mut mem = Memory::new();
    let h1 = mem.hash_state();
    mem.write_u8(0x5000, 0xFF);
    let h2 = mem.hash_state();
    assert_ne!(h1, h2);
}

// ========== load out of bounds ==========

#[test]
fn test_load_beyond_ram_returns_error() {
    let mut mem = Memory::new();
    let data = [0xAA; 10];
    let result = mem.load(MAX_RAM_SIZE + 100, &data);
    assert!(result.is_err());
}

// ========== MemoryError display ==========

#[test]
fn test_memory_error_variants_display() {
    let e1 = MemoryError::OutOfBounds(0x12345);
    assert!(format!("{}", e1).contains("0x12345"));

    let e2 = MemoryError::Unaligned(0xABC);
    assert!(format!("{}", e2).contains("0xABC"));
}

#[test]
fn error_display_contracts_match_all_variants() {
    fn assert_error<E: std::fmt::Display + std::error::Error>(error: E, expected: &str) {
        assert_eq!(error.to_string(), expected);
        assert_eq!(format!("{:>80.3}", error), expected);
        assert!(std::error::Error::source(&error).is_none());
    }

    assert_error(
        MemoryError::OutOfBounds(0x12AB),
        "Memory access out of bounds: 0x12AB",
    );
    assert_error(
        MemoryError::Unaligned(0xABC),
        "Unaligned memory access at 0xABC",
    );
    assert_error(
        MemoryError::RawLoadOverlapsHleEmsFrame {
            addr: 0xABCD,
            len: 7,
        },
        "raw load at 0xABCD for 7 bytes overlaps the HLE EMS page frame",
    );

    assert_error(
        EmsPoolError::SizeOverflow(123),
        "EMS pool size overflow for 123 pages",
    );
    assert_error(
        EmsPoolError::InvalidPoolPage(4),
        "EMS pool page 4 is not allocated",
    );
    assert_error(
        EmsPoolError::InvalidOffset(0x4000),
        "EMS pool offset 0x4000 is outside a 16 KiB EMS page",
    );
    assert_error(
        EmsPoolError::RangeOutOfBounds,
        "EMS backing range exceeds allocated pool",
    );
    assert_error(
        EmsPoolError::SnapshotSizeMismatch {
            expected: 4096,
            actual: 4095,
        },
        "EMS snapshot size mismatch: expected 4096 bytes, got 4095 bytes",
    );
    assert_error(
        EmsPoolError::InvalidFrameSlot(4),
        "EMS frame slot 4 is outside the four-slot page frame",
    );
    assert_error(
        EmsPoolError::FrameNotOwned,
        "HLE EMS frame is not owned by Memory",
    );

    assert_error(
        MemoryOwnerError::InvalidHleEmsFrame {
            base: 0xE1000,
            size: 0x1000,
        },
        "HLE EMS frame must be exactly 0xE0000..0xEFFFF, got base=0xE1000 size=0x1000",
    );
    assert_error(
        MemoryOwnerError::OutOfRam,
        "HLE EMS frame extends beyond RAM",
    );
    assert_error(
        MemoryOwnerError::OverlapsVideo,
        "HLE EMS frame overlaps VGA memory",
    );
    assert_error(
        MemoryOwnerError::OverlapsSystemBios,
        "HLE EMS frame overlaps system BIOS",
    );
    assert_error(
        MemoryOwnerError::OverlapsRom,
        "HLE EMS frame overlaps a ROM-marked page",
    );

    assert_error(
        VgaPlanesSizeError { actual: 123 },
        &format!("VGA plane backing must contain exactly {VGA_PLANES_SIZE} bytes, got 123"),
    );
}

// ========== VGA pipeline setters ==========

#[test]
fn test_vga_setters() {
    let mut mem = Memory::new();
    mem.set_plane_mask(0xFF);
    assert_eq!(mem.vga.plane_mask, 0x0F); // masked to 4 bits

    mem.set_write_mode(0xFF);
    assert_eq!(mem.vga.write_mode, 0x03); // masked to 2 bits

    mem.set_bit_mask(0xA5);
    assert_eq!(mem.vga.bit_mask, 0xA5);

    mem.set_set_reset(0xFF);
    assert_eq!(mem.vga.set_reset, 0x0F); // masked to 4 bits

    mem.set_enable_set_reset(0xFF);
    assert_eq!(mem.vga.enable_set_reset, 0x0F); // masked to 4 bits

    mem.set_data_rotate(0x18);
    assert_eq!(mem.vga.data_rotate, 0x18);
}

#[test]
fn test_set_video_mode_updates_handler() {
    let mut mem = Memory::new();
    mem.set_video_mode(0x12);
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Planar);
    assert_eq!(mem.vga.video_mode, 0x12);
}

#[test]
fn test_set_latches() {
    let mut mem = Memory::new();
    mem.set_latches([0x11, 0x22, 0x33, 0x44]);
    assert_eq!(mem.get_latches(), [0x11, 0x22, 0x33, 0x44]);
}

// ========== mark_vga_pipeline_dirty ==========

#[test]
fn test_mark_vga_pipeline_dirty_no_panic() {
    let mut mem = Memory::new();
    // Just ensure it doesn't panic (it's a no-op in release builds)
    mem.mark_vga_pipeline_dirty();
}

// ========== write_u8 write tracking with slow path ==========

#[test]
fn test_write_u16_with_tracking() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x0100, 0x0200);
    mem.write_u16(0x50000, 0xBEEF);
    let writes = mem.drain_pending_writes();
    // Should record as a single 16-bit write
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].size, 2);
    assert_eq!(writes[0].new_value, 0xBEEF);
}

#[test]
fn test_write_u32_with_tracking() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x0100, 0x0200);
    mem.write_u32(0x50000, 0xDEADBEEF);
    let writes = mem.drain_pending_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].size, 4);
    assert_eq!(writes[0].new_value, 0xDEADBEEF);
}

// ========== write_planar_ega_tracked ==========

#[test]
fn test_write_planar_ega_tracked_records_plane_writes() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.write_mode = 0;
    mem.vga.plane_mask = 0x0F;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.vga.data_rotate = 0;

    mem.enable_write_tracking();
    mem.set_write_context(0x0100, 0x0200);

    mem.write_planar_ega_tracked(0, 0xAB);

    let writes = mem.drain_pending_writes();
    // Should record 4 plane writes
    assert_eq!(writes.len(), 4);
    for w in &writes {
        assert!(w.is_vga_write);
        assert!(w.vga_plane.is_some());
    }
}

// ========== Planar write mode 1 with tracking ==========

#[test]
fn test_write_planar_ega_tracked_mode1() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.write_mode = 1;
    mem.vga.plane_mask = 0x0F;

    mem.set_latches([0x11, 0x22, 0x33, 0x44]);
    mem.enable_write_tracking();
    mem.set_write_context(0x0100, 0x0200);

    mem.write_planar_ega_tracked(0, 0xFF);

    let writes = mem.drain_pending_writes();
    assert_eq!(writes.len(), 4);
}

// ========== note_jit_write ==========

#[test]
fn test_note_jit_write_marks_dirty_for_exec_page() {
    let mut mem = Memory::new();
    let addr = 0x10000;
    mem.mark_instr_exec_page(addr);
    // Clear the dirty flag from marking
    mem.take_instr_cache_dirty();

    mem.note_jit_write(addr, 1);
    assert!(mem.is_instr_cache_dirty());
}

#[test]
fn test_note_jit_write_invalid_size_ignored() {
    let mut mem = Memory::new();
    mem.mark_instr_exec_page(0x10000);
    mem.take_instr_cache_dirty();

    // Size 3 is not 1, 2, or 4 — should be ignored
    mem.note_jit_write(0x10000, 3);
    assert!(!mem.is_instr_cache_dirty());
}

#[test]
fn test_note_jit_write_size_2_contiguous() {
    let mut mem = Memory::new();
    let addr = 0x20000;
    mem.mark_instr_exec_page(addr);
    mem.take_instr_cache_dirty();

    mem.note_jit_write(addr, 2);
    assert!(mem.is_instr_cache_dirty());
}

#[test]
fn test_note_jit_write_size_4_contiguous() {
    let mut mem = Memory::new();
    let addr = 0x30000;
    mem.mark_instr_exec_page(addr);
    mem.take_instr_cache_dirty();

    mem.note_jit_write(addr, 4);
    assert!(mem.is_instr_cache_dirty());
}

// ========== prepare_write_addr ROM protection ==========

#[test]
fn test_write_u8_to_rom_with_allow_flag() {
    let mut mem = Memory::new();
    mem.allow_rom_writes = true;
    let rom_addr = BIOS_BASE + 0x200;
    mem.write_u8(rom_addr, 0x42);
    assert_eq!(mem.read_u8(rom_addr), 0x42);
}

#[test]
fn test_note_jit_write_invalid_size() {
    let mut mem = Memory::new();
    // Size 3 is not valid (only 1, 2, 4)
    mem.note_jit_write(0x1000, 3);
    // Should be a no-op
}

#[test]
fn test_note_jit_write_valid_sizes() {
    let mut mem = Memory::new();
    mem.mark_instr_exec_page(0x10000);
    mem.note_jit_write(0x10000, 1);
    mem.note_jit_write(0x10000, 2);
    mem.note_jit_write(0x10000, 4);
    // Just verify no panics and dirty flag is set
    assert!(mem.is_instr_cache_dirty());
}

// ========== remove_physical_mapping flushes TLB ==========

#[test]
fn test_remove_physical_mapping_flushes_tlb() {
    let mut mem = Memory::new();
    mem.tlb_stats_mut().enable();
    // Add mapping that spans at least one page
    mem.add_physical_mapping(0x300000, 0x100000, 0x2000);
    let flush_before = mem.tlb_stats().flush_single_count();

    // Remove it — should flush TLB pages
    assert!(mem.remove_physical_mapping(0x300000));
    let flush_after = mem.tlb_stats().flush_single_count();
    assert!(flush_after > flush_before);
}

// ========== read_u8 out-of-bounds above video ==========

#[test]
fn test_read_u8_beyond_usable_len() {
    let mut mem = Memory::new();
    // Ensure address is above video but still maskable.
    // With A20 disabled, addresses wrap at bit 20. Use A20 enabled.
    mem.set_a20(true);
    // Read just past usable len — should return 0xFF (open bus)
    let val = mem.read_u8(mem.usable_len() + 100);
    assert_eq!(val, 0xFF);
}

// ========== read_u16/read_u32 with A20 disabled slow paths ==========

#[test]
fn test_read_u16_a20_disabled_video_wrap() {
    let mut mem = Memory::new();
    mem.set_a20(false);
    // With A20 off, addresses get bit 20 cleared.
    // Address 0xFFFFF wrapping_add(1) = 0x100000, which after A20 mask = 0.
    // This forces the slow path.
    mem.ram[0] = 0x11;
    mem.ram[0xFFFFF & !0x100000] = 0x22; // A20 masked
                                         // Reading at a large-ish address to force slow path
    let val = mem.read_u16(VIDEO_BASE);
    // Should read from video area (slow path)
    let _ = val;
}

// ========== note_jit_write non-contiguous path ==========

#[test]
fn test_note_jit_write_non_contiguous() {
    let mut mem = Memory::new();
    mem.mark_instr_exec_page(0xF0000); // ROM area — writes blocked
                                       // Size 2 write at the last valid address before ROM boundary
                                       // This creates non-contiguous resolved addresses since ROM writes
                                       // are blocked, forcing the fallback loop
    mem.note_jit_write(0xFFFFF, 2);
}

// ========== bulk_copy_ram copy_len == 0 edge ==========

#[test]
fn test_bulk_copy_ram_src_at_ram_end() {
    let mut mem = Memory::new();
    let ram_len = mem.ram.len();
    // src at last byte, len > remaining -> copy_len clamped, should succeed
    assert!(mem.bulk_copy_ram(ram_len - 1, 0x1000, 100));
}

// ========== ensure_ram_size actual resize ==========
// (The 32MB pre-allocation means this doesn't normally trigger.
//  We can trigger it by shrinking ram first, then calling ensure.)

#[test]
fn test_ensure_ram_size_triggers_resize_when_small() {
    let mut mem = Memory::new();
    // Shrink RAM to something small
    mem.ram.resize(1024 + FETCH_PADDING, 0);
    assert_eq!(mem.usable_len(), 1024);

    // Now ensure larger size — should trigger resize
    mem.ensure_ram_size(8192);
    assert!(mem.usable_len() >= 8192);
}

// ========== MMIO read/write through a synthetic aperture ==========

#[test]
fn test_mmio_read_write_at_synthetic_aperture() {
    let mut mem = Memory::new();
    let vram_base = MAX_RAM_SIZE + 0x1000;
    mem.add_mmio_region(vram_base, 256);

    // Write through mmio_write_u8 path
    with_trace_subscriber(|| {
        mem.write_u8(vram_base, 0xAB);
        mem.write_u8(vram_base + 1, 0xCD);
    });

    // Read through mmio_read_u8 path
    with_trace_subscriber(|| {
        let v0 = mem.read_u8(vram_base);
        assert_eq!(v0, 0xAB);
        let v1 = mem.read_u8(vram_base + 1);
        assert_eq!(v1, 0xCD);
    });
}

// ========== prepare_write_addr resize path ==========

#[test]
fn test_prepare_write_addr_resize_on_write() {
    let mut mem = Memory::new();
    // Shrink RAM to trigger resize in write path
    mem.ram.resize(4096 + FETCH_PADDING, 0);
    assert_eq!(mem.usable_len(), 4096);

    // Enable write tracking to bypass the fast path in write_u8
    mem.enable_write_tracking();
    // Write past current usable_len but within MAX_RAM_SIZE
    mem.write_u8(0x10000, 0xAB);
    // Should have resized
    assert!(mem.usable_len() >= 0x10001);
    assert_eq!(mem.read_u8(0x10000), 0xAB);
    mem.disable_write_tracking();
}

#[test]
fn memory_policy_epoch_increments_when_prepare_write_addr_grows_ram() {
    let mut mem = Memory::new();
    mem.ram.resize(4096 + FETCH_PADDING, 0);
    assert_eq!(mem.usable_len(), 4096);
    assert_epoch(&mem, 0);

    // Force the slow write path so the growth comes from prepare_write_addr.
    mem.enable_write_tracking();
    let epoch_before_growth = mem.memory_policy_epoch();
    mem.write_u8(0x10000, 0xAB);
    assert!(mem.usable_len() >= 0x10001);
    assert_eq!(mem.read_u8(0x10000), 0xAB);
    assert_epoch(&mem, epoch_before_growth.wrapping_add(1));

    // An in-range data write must not advance the policy epoch.
    mem.write_u8(0x10000, 0xCD);
    assert_eq!(mem.read_u8(0x10000), 0xCD);
    assert_epoch(&mem, epoch_before_growth.wrapping_add(1));
    mem.disable_write_tracking();
}

// ========== GDT descriptor invalidation coverage ==========

#[test]
fn test_snoop_gdt_write_entry14_byte6() {
    let mut mem = Memory::new();
    // Set up a GDT starting at 0x10000, big enough for entry 14
    // Entry 14 starts at offset 14*8 = 112
    // We need GDT limit >= 112+7 = 119 = 0x77
    mem.set_gdt_snoop_range(0x10000, 0xFF);

    // Set up a minimal CS descriptor at GDT entry 3 (selector 0x0018)
    // so that the code dump has a valid CS base
    let cs_entry_addr = 0x10000 + 3 * 8;
    // Base = 0, limit = 0xFFFF, present, code segment
    mem.ram[cs_entry_addr] = 0xFF; // limit low
    mem.ram[cs_entry_addr + 1] = 0xFF; // limit high
    mem.ram[cs_entry_addr + 2] = 0x00; // base low
    mem.ram[cs_entry_addr + 3] = 0x00; // base mid
    mem.ram[cs_entry_addr + 4] = 0x00; // base high
    mem.ram[cs_entry_addr + 5] = 0x9A; // access byte
    mem.ram[cs_entry_addr + 6] = 0x00; // flags
    mem.ram[cs_entry_addr + 7] = 0x00; // base highest

    // Set up entry 14 descriptor with base >= 0x100000
    let entry14_addr = 0x10000 + 14 * 8;
    mem.ram[entry14_addr] = 0xFF; // limit low
    mem.ram[entry14_addr + 1] = 0xFF; // limit high
    mem.ram[entry14_addr + 2] = 0x00; // base low
    mem.ram[entry14_addr + 3] = 0x00; // base mid
    mem.ram[entry14_addr + 4] = 0x10; // base high = 0x10xxxx
    mem.ram[entry14_addr + 5] = 0x93; // access byte
    mem.ram[entry14_addr + 6] = 0x00; // flags byte 6
    mem.ram[entry14_addr + 7] = 0x00; // base highest

    // Set INTERP_WATCHPOINT_CSIP to look like CS=0x0018 IP=0x0000
    INTERP_WATCHPOINT_CSIP.store(0x0018_0000, Ordering::Relaxed);
    INTERP_WATCHPOINT_SIDI.store(0x0000_0000, Ordering::Relaxed);
    INTERP_WATCHPOINT_DSES.store(0x0020_0028, Ordering::Relaxed);

    // Write to entry 14, byte 6 = offset 14*8 + 6 = 118
    mem.snoop_gdt_write_ext(0x10000 + 118, 0x00, 1);

    assert!(mem.take_gdt_dirty());

    // Clean up atomics
    INTERP_WATCHPOINT_CSIP.store(0, Ordering::Relaxed);
    INTERP_WATCHPOINT_SIDI.store(0, Ordering::Relaxed);
    INTERP_WATCHPOINT_DSES.store(0, Ordering::Relaxed);
}

// ========== IVT corruption watchpoint in write_u8_inner ==========

#[test]
fn test_ivt_write_watchpoint_normal() {
    let mut mem = Memory::new();
    // Write to IVT with a normal segment (not VGA range) — should not trigger
    mem.write_u8(0x00, 0x00); // offset low
    mem.write_u8(0x01, 0x00); // offset high
    mem.write_u8(0x02, 0x00); // segment low (not in VGA range)
    mem.write_u8(0x03, 0x00); // segment high
}

#[test]
fn test_ivt_write_watchpoint_vga_segment() {
    let mut mem = Memory::new();
    // Write an IVT entry with a VGA-range segment (A000-BFFF)
    // Vector 0 at address 0x0000
    with_trace_subscriber(|| {
        mem.write_u8(0x02, 0x00); // segment low = 0x00
        mem.write_u8(0x03, 0xA0); // segment high = 0xA0 → segment 0xA000
    });
}

// ========== planar write to offset >= PLANE_SIZE (boundary) ==========

#[test]
fn test_write_u8_planar_beyond_plane_size() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.plane_mask = 0x0F;
    mem.update_vga_write_handler();
    mem.set_vga_memory_map_select(0); // A0000-BFFFF (128K)

    // Write at offset >= PLANE_SIZE within the vga window
    // PLANE_SIZE = 65536, so VIDEO_BASE + 65536 = 0xB0000
    mem.write_u8(VIDEO_BASE + PLANE_SIZE, 0xAB);
    // Should fall through to linear write since offset >= PLANE_SIZE
    assert_eq!(mem.ram[VIDEO_BASE + PLANE_SIZE], 0xAB);
}

// ========== chain4 write offset >= PLANE_SIZE (boundary) ==========

#[test]
fn test_write_u8_chain4_beyond_plane_size() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.plane_mask = 0x0F;
    mem.vga.write_mode = 0;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.update_vga_write_handler();
    mem.set_vga_memory_map_select(0); // A0000-BFFFF (128K)

    // Chain-4 mapping uses cell = x & ~3, so this second-half
    // aperture address maps past PLANE_SIZE and must fall through to RAM.
    let addr = VIDEO_BASE + 0x1FFFC;
    mem.write_u8(addr, 0xCC);
    assert_eq!(mem.ram[addr], 0xCC);
    // The old x >> 2 expectation put this byte at plane 0 cell 0x7FFF.
    assert_eq!(mem.plane(0)[0x7FFF], 0x00);
}

// ========== read_u8 planar offset out of PLANE_SIZE ==========

#[test]
fn test_read_u8_planar_beyond_plane_size() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.set_vga_memory_map_select(0); // A0000-BFFFF (128K)

    // Read at offset >= PLANE_SIZE within the 128K window
    let val = mem.read_u8(VIDEO_BASE + PLANE_SIZE);
    // Falls through to ram read
    assert_eq!(val, mem.ram[VIDEO_BASE + PLANE_SIZE]);
}

// ========== read_u8 chain4 offset leads to plane_offset >= PLANE_SIZE ==========

#[test]
fn test_read_u8_chain4_near_boundary() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.read_mode = 0;
    mem.set_vga_memory_map_select(0); // A0000-BFFFF

    // Chain-4 mapping maps x=0x1FFFF to cell 0x1FFFC, beyond
    // PLANE_SIZE, so this in-window BFFFF byte comes from raw RAM.
    let addr = VIDEO_BASE + 0x1FFFF;
    mem.ram[addr] = 0x6D;
    assert_eq!(mem.read_u8(addr), 0x6D);
}

// ========== MemoryAccess trait ==========

#[test]
fn test_memory_access_trait_u8() {
    let mut mem = Memory::new();
    <Memory as MemoryAccess>::write_u8(&mut mem, 0x6000, 0xAA);
    assert_eq!(<Memory as MemoryAccess>::read_u8(&mem, 0x6000), 0xAA);
}

#[test]
fn test_memory_access_trait_u16() {
    let mut mem = Memory::new();
    <Memory as MemoryAccess>::write_u16(&mut mem, 0x6000, 0xBEEF);
    assert_eq!(<Memory as MemoryAccess>::read_u16(&mem, 0x6000), 0xBEEF);
}

#[test]
fn test_memory_access_trait_u32() {
    let mut mem = Memory::new();
    <Memory as MemoryAccess>::write_u32(&mut mem, 0x6000, 0xDEADBEEF);
    assert_eq!(<Memory as MemoryAccess>::read_u32(&mem, 0x6000), 0xDEADBEEF);
}

#[test]
fn test_memory_access_trait_u64() {
    let mut mem = Memory::new();
    <Memory as MemoryAccess>::write_u64(&mut mem, 0x6000, 0xCAFE_BABE_DEAD_BEEF);
    assert_eq!(
        <Memory as MemoryAccess>::read_u64(&mem, 0x6000),
        0xCAFE_BABE_DEAD_BEEF
    );
}

#[test]
fn test_memory_access_trait() {
    let mut mem = Memory::new();

    // Use MemoryAccess trait methods
    <Memory as MemoryAccess>::write_u8(&mut mem, 0x5000, 0x42);
    assert_eq!(<Memory as MemoryAccess>::read_u8(&mem, 0x5000), 0x42);

    <Memory as MemoryAccess>::write_u16(&mut mem, 0x5000, 0x1234);
    assert_eq!(<Memory as MemoryAccess>::read_u16(&mem, 0x5000), 0x1234);

    <Memory as MemoryAccess>::write_u32(&mut mem, 0x5000, 0xDEADBEEF);
    assert_eq!(<Memory as MemoryAccess>::read_u32(&mem, 0x5000), 0xDEADBEEF);
}

// ========== PagedMemory trait ==========

#[test]
fn test_paged_memory_trait_tlb_roundtrip() {
    let mut mem = Memory::new();
    <Memory as PagedMemory>::tlb_insert(&mut mem, 0x50, 0x50, true, true, true);
    let result = <Memory as PagedMemory>::tlb_lookup(&mem, 0x50);
    assert!(result.is_some());
    let (phys, w, u, d) = result.unwrap();
    assert_eq!(phys, 0x50);
    assert!(w);
    assert!(u);
    assert!(d);
}

#[test]
fn test_paged_memory_trait_invalidate() {
    let mut mem = Memory::new();
    <Memory as PagedMemory>::tlb_insert(&mut mem, 0x60, 0x60, true, true, true);
    <Memory as PagedMemory>::invalidate_tlb(&mut mem);
    assert!(<Memory as PagedMemory>::tlb_lookup(&mem, 0x60).is_none());
}

#[test]
fn test_paged_memory_trait_invalidate_page() {
    let mut mem = Memory::new();
    <Memory as PagedMemory>::tlb_insert(&mut mem, 0x70, 0x70, true, true, true);
    <Memory as PagedMemory>::invalidate_tlb_page(&mut mem, 0x70 << 12);
    assert!(<Memory as PagedMemory>::tlb_lookup(&mem, 0x70).is_none());
}

/// MDD-BUG-FLUX-00086, stated where the guest actually reaches it: an
/// INVLPG against a page the flat projection never published must still
/// move `flat_mutation_epoch`.
///
/// That epoch is the drc native chain link's whole validity signal — the
/// link records it at patch time and re-checks it on traversal. A cold code
/// page legitimately sits at all-zero flat slots, so if its INVLPG were
/// silent, a PTE rewrite could remap the same linear EIP to different
/// physical code while the link still matched, and the guest would jump to
/// the stale physical successor.
#[test]
fn invalidating_a_never_published_page_moves_the_flat_mutation_epoch() {
    let mut mem = Memory::new();
    let linear = 0x0F00_0000u32;

    // Never touched, so the flat projection holds nothing for it.
    let before = mem.flat_mutation_epoch();
    <Memory as PagedMemory>::invalidate_tlb_page(&mut mem, linear);
    assert!(
        mem.flat_mutation_epoch() > before,
        "INVLPG on a cold page must announce the translation change: \
         {before} -> {}",
        mem.flat_mutation_epoch()
    );
}

#[test]
fn test_paged_memory_trait_physical_rw() {
    let mut mem = Memory::new();
    <Memory as PagedMemory>::write_u32_physical(&mut mem, 0x4000, 0xBAADF00D);
    assert_eq!(
        <Memory as PagedMemory>::read_u32_physical(&mem, 0x4000),
        0xBAADF00D
    );
}

#[test]
fn test_paged_memory_trait_map_linear_physical() {
    let mut mem = Memory::new();
    mem.add_physical_mapping(0x200_000, 0x300_000, 0x1000);
    assert_eq!(
        <Memory as PagedMemory>::map_linear_physical(&mem, 0x200_000),
        Some(0x300_000)
    );
}

// ========== MMIO additional tests ==========

#[test]
fn test_find_mmio_region_containing() {
    let mut mem = Memory::new();
    mem.add_mmio_region(0x6000_0000, 4096);
    assert_eq!(
        mem.find_mmio_region_containing(0x6000_0100),
        Some((0x6000_0000, 4096))
    );
    assert!(mem.find_mmio_region_containing(0x7000_0000).is_none());
}

#[test]
fn test_first_mmio_region() {
    let mut mem = Memory::new();
    assert!(mem.first_mmio_region().is_none());
    mem.add_mmio_region(0x5000_0000, 1024);
    assert_eq!(mem.first_mmio_region(), Some((0x5000_0000, 1024)));
}

#[test]
fn test_has_mmio_region() {
    let mut mem = Memory::new();
    assert!(!mem.has_mmio_region(0x6000_0000));
    mem.add_mmio_region(0x6000_0000, 512);
    assert!(mem.has_mmio_region(0x6000_0000));
    assert!(!mem.has_mmio_region(0x7000_0000));
}

#[test]
fn test_mmio_slice_immutable() {
    let mut mem = Memory::new();
    mem.add_mmio_region(0x6000_0000, 256);
    mem.mmio_slice_mut(0x6000_0000).unwrap()[10] = 0x77;
    assert_eq!(mem.mmio_slice(0x6000_0000).unwrap()[10], 0x77);
    assert!(mem.mmio_slice(0x7000_0000).is_none());
}

#[test]
fn test_mmio_slice_mut_none_for_missing() {
    let mut mem = Memory::new();
    assert!(mem.mmio_slice_mut(0x6000_0000).is_none());
}

// ========== Video dirty pages ==========

#[test]
fn test_video_dirty_pages_tracking() {
    let mem = Memory::new();
    assert!(!mem.has_dirty_video_pages());

    // Mark page 0 (A0000)
    mem.mark_video_page_dirty(VIDEO_BASE);
    assert!(mem.has_dirty_video_pages());

    // Take clears the mask
    let mask = mem.take_video_dirty_pages();
    assert_eq!(mask & 1, 1); // page 0 was dirty
    assert!(!mem.has_dirty_video_pages());
}

#[test]
fn test_video_dirty_pages_multiple() {
    let mem = Memory::new();
    mem.mark_video_page_dirty(VIDEO_BASE); // page 0
    mem.mark_video_page_dirty(VIDEO_BASE + 0x1000); // page 1
    mem.mark_video_page_dirty(VIDEO_BASE + 0xF000); // page 15

    let mask = mem.take_video_dirty_pages();
    assert!(mask & (1 << 0) != 0);
    assert!(mask & (1 << 1) != 0);
    assert!(mask & (1 << 15) != 0);
}

#[test]
fn test_video_dirty_pages_out_of_range_ignored() {
    let mem = Memory::new();
    // Address below video range
    mem.mark_video_page_dirty(0x5000);
    assert!(!mem.has_dirty_video_pages());

    // Address above video range
    mem.mark_video_page_dirty(VIDEO_BASE + VIDEO_SIZE);
    assert!(!mem.has_dirty_video_pages());
}

// ========== Video write count ==========

#[test]
fn test_video_write_count_incremented_on_video_write() {
    let mut mem = Memory::new();
    assert_eq!(mem.video_write_count(), 0);
    // Write to video memory
    mem.write_u8(VIDEO_BASE + 0x100, 0x42);
    assert!(mem.video_write_count() > 0);
}

// ========== Instruction cache dirty ==========

#[test]
fn test_instr_cache_dirty_after_write_to_exec_page() {
    let mut mem = Memory::new();
    let addr = 0x20000;
    mem.mark_instr_exec_page(addr);
    mem.take_instr_cache_dirty(); // clear

    // Write to the exec page triggers dirty flag
    mem.write_u8(addr, 0x90);
    assert!(mem.is_instr_cache_dirty());
    assert!(mem.take_instr_cache_dirty());
    // After take, it's clear
    assert!(!mem.is_instr_cache_dirty());
}

#[test]
fn test_instr_cache_dirty_not_set_for_non_exec_page() {
    let mut mem = Memory::new();
    // Don't mark any exec page
    mem.take_instr_cache_dirty(); // clear
    mem.write_u8(0x20000, 0x90);
    assert!(!mem.is_instr_cache_dirty());
}

// ========== map_linear_physical edge cases ==========

#[test]
fn test_map_linear_physical_no_mappings() {
    let mem = Memory::new();
    assert!(mem.map_linear_physical(0x100_000).is_none());
}

// ========== JIT write methods ==========

#[test]
fn test_write_u8_jit_basic() {
    let mut mem = Memory::new();
    mem.write_u8_jit(0x8000, 0xAB);
    assert_eq!(mem.read_u8(0x8000), 0xAB);
}

// ========== JIT write tracking ==========

#[test]
fn test_write_u8_jit_with_tracking() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x10, 0x20);
    mem.ram[0x8000] = 0x11;
    mem.write_u8_jit(0x8000, 0x22);
    let writes = mem.drain_pending_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].old_value, 0x11);
    assert_eq!(writes[0].new_value, 0x22);
}

#[test]
fn test_write_u16_vga_no_smc_jit_linear_video_semantics() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x10, 0x20);
    let addr = VIDEO_BASE + 0x20;

    assert!(mem.write_u16_vga_no_smc_jit(addr, 0x3412));

    assert_eq!(mem.ram[addr], 0x12);
    assert_eq!(mem.ram[addr + 1], 0x34);
    assert_eq!(mem.video_write_count(), 2);
    assert!(mem.take_video_dirty_pages() & 1 != 0);
    assert!(mem.drain_pending_writes().is_empty());
}

fn decode_vga_trace_record(record: &[u8; 20]) -> (u64, u32, u32, u16, u8, u8) {
    (
        u64::from_le_bytes(record[0..8].try_into().unwrap()),
        u32::from_le_bytes(record[8..12].try_into().unwrap()),
        u32::from_le_bytes(record[12..16].try_into().unwrap()),
        u16::from_le_bytes(record[16..18].try_into().unwrap()),
        record[18],
        record[19],
    )
}

#[test]
fn test_write_u16_vga_no_smc_jit_records_vga_trace_bytes() {
    let mut mem = Memory::new();
    let addr = VIDEO_BASE + 0x70;

    let _capture = vga_trace_test_begin_capture();
    vga_trace_test_set_ctx(0x1122_3344_5566_7788, 0x1234, 0x89AB_CDEF);

    assert!(mem.write_u16_vga_no_smc_jit(addr, 0xBBAA));

    let records = vga_trace_test_take_capture();
    assert_eq!(records.len(), 2);
    assert_eq!(
        decode_vga_trace_record(&records[0]),
        (
            0x1122_3344_5566_7788,
            0x89AB_CDEF,
            addr as u32,
            0x1234,
            0xAA,
            0
        )
    );
    assert_eq!(
        decode_vga_trace_record(&records[1]),
        (
            0x1122_3344_5566_7788,
            0x89AB_CDEF,
            (addr + 1) as u32,
            0x1234,
            0xBB,
            0,
        )
    );
}

#[test]
fn test_write_u16_vga_no_smc_jit_skips_smc_notifier() {
    let mut mem = Memory::new();
    let notifier = Arc::new(TestSmcNotify::default());
    mem.bind_smc(Some(notifier.clone()));
    mem.set_smc_has_code_pages(true);

    assert!(mem.write_u16_vga_no_smc_jit(VIDEO_BASE + 0x40, 0x7856));

    assert!(notifier.snapshot().is_empty());
}

#[test]
fn test_write_u16_vga_no_smc_jit_chain4_matches_byte_writes() {
    let mut fast = Memory::new();
    let mut reference = Memory::new();
    for mem in [&mut fast, &mut reference] {
        mem.vga.video_mode = 0x13;
        mem.vga.chain4 = true;
        mem.vga.plane_mask = 0x0F;
        mem.vga.write_mode = 0;
        mem.vga.bit_mask = 0xFF;
        mem.vga.enable_set_reset = 0;
        mem.vga.set_reset = 0;
        mem.vga.data_rotate = 0;
        mem.vga.memory_map_select = 0;
        mem.update_vga_write_handler();
    }

    let addr = VIDEO_BASE + 4;
    assert!(fast.write_u16_vga_no_smc_jit(addr, 0xBBAA));
    reference.write_u8(addr, 0xAA);
    reference.write_u8(addr + 1, 0xBB);

    assert_eq!(fast.ram[addr..addr + 2], reference.ram[addr..addr + 2]);
    assert_eq!(fast.planes_flat, reference.planes_flat);
    assert_eq!(fast.video_write_count(), reference.video_write_count());
}

fn configure_planar_write_test(mem: &mut Memory, memory_map_select: u8) {
    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.plane_mask = 0x05;
    mem.vga.write_mode = 0;
    mem.vga.bit_mask = 0xF0;
    mem.vga.enable_set_reset = 0;
    mem.vga.set_reset = 0;
    mem.vga.data_rotate = 0;
    mem.vga.memory_map_select = memory_map_select;
    mem.update_vga_write_handler();
    mem.latches.set([0x0F, 0xF0, 0x33, 0xCC]);
}

#[test]
fn test_mode12_a000_map1_writes_planes_not_raw_ram() {
    let mut mem = Memory::new();
    configure_planar_write_test(&mut mem, 1);

    mem.write_u8(VIDEO_BASE, 0xAA);

    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Planar);
    assert_eq!(mem.vga_memory_map_select(), 1);
    assert_eq!(mem.ram[VIDEO_BASE], 0);
    assert_eq!(mem.plane(0)[0], 0xAF);
    assert_eq!(mem.plane(1)[0], 0);
    assert_eq!(mem.plane(2)[0], 0xA3);
    assert_eq!(mem.plane(3)[0], 0);
}

#[test]
fn test_mode12_a000_map3_does_not_write_planes() {
    let mut mem = Memory::new();
    configure_planar_write_test(&mut mem, 3);

    mem.write_u8(VIDEO_BASE, 0xAA);

    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Planar);
    assert_eq!(mem.vga_memory_map_select(), 3);
    assert_eq!(mem.ram[VIDEO_BASE], 0xAA);
    assert_eq!(mem.plane(0)[0], 0);
    assert_eq!(mem.plane(1)[0], 0);
    assert_eq!(mem.plane(2)[0], 0);
    assert_eq!(mem.plane(3)[0], 0);
}

#[test]
fn test_mode12_b800_map3_writes_planes_not_raw_ram() {
    let mut mem = Memory::new();
    configure_planar_write_test(&mut mem, 3);

    mem.write_u8(TEXT_VIDEO_BASE_COLOR, 0xAA);

    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Planar);
    assert_eq!(mem.vga_memory_map_select(), 3);
    assert_eq!(mem.ram[TEXT_VIDEO_BASE_COLOR], 0);
    assert_eq!(mem.plane(0)[0], 0xAF);
    assert_eq!(mem.plane(1)[0], 0);
    assert_eq!(mem.plane(2)[0], 0xA3);
    assert_eq!(mem.plane(3)[0], 0);
}

#[test]
fn planar_write_without_tracking_records_no_events() {
    let mut mem = Memory::new();
    configure_planar_write_test(&mut mem, 3);
    mem.write_u8(TEXT_VIDEO_BASE_COLOR, 0xAA);
    assert_eq!(mem.pending_write_count(), 0);
}

#[test]
fn planar_map3_a000_raw_write_preserves_context() {
    let mut mem = Memory::new();
    configure_planar_write_test(&mut mem, 3);
    mem.enable_write_tracking();
    mem.set_write_context(0x1234, 0x5678);
    mem.write_u8(VIDEO_BASE, 0xAA);
    let context = mem.write_context().unwrap();
    assert_eq!(context.cs, 0x1234);
    assert_eq!(context.ip, 0x5678);
    assert!(mem.video_write_change_samples().samples.is_empty());
    assert_eq!(mem.ram[VIDEO_BASE], 0xAA);
    assert_eq!(mem.vga_memory_map_select(), 3);
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Planar);
    assert_eq!(mem.plane(0)[0], 0);
}

#[test]
fn planar_write_without_context_records_no_source() {
    let mut mem = Memory::new();
    configure_planar_write_test(&mut mem, 3);
    mem.enable_write_tracking();
    mem.write_u8(VIDEO_BASE, 0xAA);
    assert!(mem.write_context().is_none());
    assert_eq!(mem.pending_write_count(), 0);
    assert_eq!(mem.ram[VIDEO_BASE], 0xAA);
}

#[test]
fn planar_map3_b800_write_samples_plane_route() {
    let mut mem = Memory::new();
    configure_planar_write_test(&mut mem, 3);
    mem.enable_write_tracking();
    mem.set_write_context(0x1234, 0x5678);
    mem.write_u8(TEXT_VIDEO_BASE_COLOR, 0xAA);
    let events = mem.video_write_change_samples().samples;
    assert_eq!(events.len(), 2);
    assert_eq!(mem.vga_memory_map_select(), 3);
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Planar);
    assert_eq!(mem.ram[TEXT_VIDEO_BASE_COLOR], 0);
    assert_eq!(mem.plane(0)[0], 0xAF);
    assert_eq!(mem.plane(2)[0], 0xA3);
    assert_eq!(events[0].offset, 0);
    assert_eq!(events[0].plane, 0);
    assert_eq!(events[0].source, mem.write_context());
}

#[test]
fn planar_write_samples_keep_each_byte_write() {
    let mut mem = Memory::new();
    configure_planar_write_test(&mut mem, 3);
    mem.enable_write_tracking();
    mem.set_write_context(0x1234, 0x5678);
    mem.write_u8(TEXT_VIDEO_BASE_COLOR, 0xAA);
    mem.write_u8(TEXT_VIDEO_BASE_COLOR + 1, 0xBB);
    let events = mem.video_write_change_samples().samples;
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].offset, 0);
    assert_eq!(events[2].offset, 1);
}

#[test]
fn test_write_u16_vga_no_smc_jit_planar_matches_byte_writes() {
    let mut fast = Memory::new();
    let mut reference = Memory::new();
    for mem in [&mut fast, &mut reference] {
        configure_planar_write_test(mem, 1);
    }

    let addr = VIDEO_BASE + 0x30;
    assert!(fast.write_u16_vga_no_smc_jit(addr, 0xE26D));
    reference.write_u8(addr, 0x6D);
    reference.write_u8(addr + 1, 0xE2);

    assert_eq!(fast.planes_flat, reference.planes_flat);
    assert_eq!(fast.ram[addr..addr + 2], reference.ram[addr..addr + 2]);
    assert_eq!(fast.video_write_count(), reference.video_write_count());
    assert_eq!(
        fast.take_video_dirty_pages(),
        reference.take_video_dirty_pages()
    );
}

#[test]
fn test_write_u16_vga_no_smc_jit_current_window_matches_byte_writes() {
    let mut fast = Memory::new();
    let mut reference = Memory::new();
    for mem in [&mut fast, &mut reference] {
        configure_planar_write_test(mem, 2);
    }

    let addr = TEXT_VIDEO_BASE_MONO + 0x24;
    assert!(fast.write_u16_vga_no_smc_jit(addr, 0x5AA5));
    reference.write_u8(addr, 0xA5);
    reference.write_u8(addr + 1, 0x5A);

    assert_eq!(fast.planes_flat, reference.planes_flat);
    assert_eq!(fast.ram[addr..addr + 2], reference.ram[addr..addr + 2]);
    assert_eq!(fast.video_write_count(), reference.video_write_count());
    assert_eq!(
        fast.take_video_dirty_pages(),
        reference.take_video_dirty_pages()
    );
}

#[test]
fn test_write_u16_vga_no_smc_jit_marks_instruction_cache_dirty() {
    let mut mem = Memory::new();
    let addr = VIDEO_BASE + 0x60;
    mem.mark_instr_exec_page(addr);
    assert!(!mem.take_instr_cache_dirty());

    assert!(mem.write_u16_vga_no_smc_jit(addr, 0xA55A));

    assert!(mem.take_instr_cache_dirty());
}

#[test]
fn test_write_u16_vga_no_smc_jit_guard_rejects_invalid_addresses() {
    let mut mem = Memory::new();

    assert!(!mem.can_write_u16_vga_no_smc_jit(usize::MAX));
    assert!(!mem.can_write_u16_vga_no_smc_jit(VIDEO_BASE - 1));
    assert!(!mem.can_write_u16_vga_no_smc_jit(VIDEO_BASE + VIDEO_SIZE - 1));
    assert!(!mem.write_u16_vga_no_smc_jit(VIDEO_BASE - 1, 0x1111));
}

#[test]
fn test_write_u16_vga_no_smc_jit_false_path_is_all_or_nothing() {
    let mut mem = Memory::new();
    let addr = VIDEO_BASE + 0x0FFF;
    mem.ram[addr] = 0x11;
    mem.ram[addr + 1] = 0x22;
    mem.mark_rom_page((addr + 1) >> ROM_PAGE_SHIFT);

    assert!(!mem.can_write_u16_vga_no_smc_jit(addr));
    assert!(!mem.write_u16_vga_no_smc_jit(addr, 0xBBAA));
    assert_eq!(mem.ram[addr], 0x11);
    assert_eq!(mem.ram[addr + 1], 0x22);
}

#[test]
fn test_write_u16_vga_no_smc_jit_rejects_insufficient_backing_without_write() {
    let mut mem = Memory::new();
    let addr = VIDEO_BASE;
    mem.ram[addr] = 0x5A;
    mem.ram.resize(addr + 1, 0);

    assert!(!mem.can_write_u16_vga_no_smc_jit(addr));
    assert!(!mem.write_u16_vga_no_smc_jit(addr, 0x1234));
    assert_eq!(mem.ram[addr], 0x5A);
}

// ========== Planar write tracking ==========

#[test]
fn test_planar_ega_tracked_records_plane_writes() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.plane_mask = 0x05; // planes 0 and 2
    mem.vga.write_mode = 0;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.vga.data_rotate = 0;
    mem.update_vga_write_handler();
    mem.set_latches([0; 4]);

    mem.enable_write_tracking();
    mem.set_write_context(0x10, 0x20);
    mem.write_planar_ega_tracked(0, 0x42);

    let writes = mem.drain_pending_writes();
    assert_eq!(writes.len(), 2); // planes 0 and 2
    assert!(writes[0].is_vga_write);
    assert_eq!(writes[0].vga_plane, Some(0));
    assert!(writes[1].is_vga_write);
    assert_eq!(writes[1].vga_plane, Some(2));
}

// ========== Plane access ==========

#[test]
fn test_plane_accessors() {
    let mut mem = Memory::new();
    mem.plane_mut(2)[100] = 0x77;
    assert_eq!(mem.plane(2)[100], 0x77);
}

#[test]
fn test_set_latches_and_get() {
    let mut mem = Memory::new();
    mem.set_latches([0xAA, 0xBB, 0xCC, 0xDD]);
    assert_eq!(mem.get_latches(), [0xAA, 0xBB, 0xCC, 0xDD]);
}

// ========== VGA setter methods ==========

#[test]
fn test_vga_setter_methods() {
    let mut mem = Memory::new();
    mem.set_plane_mask(0x07);
    assert_eq!(mem.vga.plane_mask, 0x07);

    mem.set_write_mode(0x03);
    assert_eq!(mem.vga.write_mode, 0x03);

    mem.set_bit_mask(0xAA);
    assert_eq!(mem.vga.bit_mask, 0xAA);

    mem.set_set_reset(0x0B);
    assert_eq!(mem.vga.set_reset, 0x0B);

    mem.set_enable_set_reset(0x06);
    assert_eq!(mem.vga.enable_set_reset, 0x06);

    mem.set_data_rotate(0x18);
    assert_eq!(mem.vga.data_rotate, 0x18);

    mem.set_video_mode(0x12);
    assert_eq!(mem.vga.video_mode, 0x12);

    mem.set_vga_memory_map_select(2);
    assert_eq!(mem.vga.memory_map_select, 2);
}

// ========== MemorySliceMut Drop ==========

#[test]
fn test_slice_mut_owned_writeback_on_drop() {
    let mut mem = Memory::new();
    // Create an owned slice (by going past usable_len boundary via a20-masked addr)
    // Actually use the zero-length path → owned path for empty
    {
        let slice = mem.slice_mut(0x1000, 0);
        assert!(slice.is_empty());
    }
    // Since the slice was empty, nothing to write back
}

// ========== SMC (Self-Modifying Code) ==========

#[test]
fn test_smc_before_write_zero_size() {
    let mut mem = Memory::new();
    let notifier = Arc::new(TestSmcNotify::default());
    mem.bind_smc(Some(notifier.clone()));
    assert!(!mem.smc_before_write(0x1000, 0));
    assert!(notifier.snapshot().is_empty());
}

#[test]
fn test_take_smc_notifier() {
    let mut mem = Memory::new();
    let notifier = Arc::new(TestSmcNotify::default());
    mem.bind_smc(Some(notifier));
    assert!(mem.take_smc_notifier().is_some());
    assert!(mem.take_smc_notifier().is_none());
}

// ========== Write to video via write_u8_inner linear path ==========

#[test]
fn test_write_u8_inner_linear_text_mode_video() {
    let mut mem = Memory::new();
    // Default text mode (0x03) uses Linear handler
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Linear);

    // Write to text video area B8000
    mem.write_u8(0xB8000, 0x41); // 'A'
    assert_eq!(mem.ram[0xB8000], 0x41);
}

// ========== prepare_write_addr RAM expansion ==========

#[test]
fn test_prepare_write_addr_expands_ram() {
    let mut mem = Memory::new();
    // Shrink RAM artificially to test expansion
    mem.ram.resize(0x10000 + FETCH_PADDING, 0);
    let old_len = mem.ram.len();

    // Enable write tracking to force the slow path through prepare_write_addr
    mem.enable_write_tracking();
    mem.set_write_context(0x10, 0x20);

    // Write to address beyond current usable size triggers expansion
    mem.write_u8(0x20000, 0xAA);
    assert!(mem.ram.len() > old_len);
    assert_eq!(mem.read_u8(0x20000), 0xAA);
}

#[test]
fn prepare_write_addr_batches_ascending_first_touch_growth() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x10, 0x20);
    mem.tlb_stats_mut().enable();

    let start = DEFAULT_RAM_SIZE;
    let span = 4096;
    let flushes_before = mem.tlb_stats().flush_full_count();
    for offset in 0..span {
        mem.write_u8(start + offset, 0xA5);
    }

    assert_eq!(
        mem.tlb_stats().flush_full_count() - flushes_before,
        1,
        "ascending first-touch writes should grow RAM once per 64 KiB batch"
    );
    assert_eq!(mem.usable_len(), start + 64 * 1024);
    assert_eq!(mem.read_u8(start), 0xA5);
    assert_eq!(mem.read_u8(start + span - 1), 0xA5);
}

// ========== write_u16/u32 to video ==========

#[test]
fn test_write_u16_to_video_area() {
    let mut mem = Memory::new();
    // Text mode, linear handler — write to B8000
    mem.write_u16(0xB8000, 0x0741); // 'A' with grey attribute
    assert_eq!(mem.ram[0xB8000], 0x41);
    assert_eq!(mem.ram[0xB8001], 0x07);
}

#[test]
fn test_write_u32_to_video_area() {
    let mut mem = Memory::new();
    mem.write_u32(0xB8000, 0x07420741);
    assert_eq!(mem.ram[0xB8000], 0x41);
    assert_eq!(mem.ram[0xB8001], 0x07);
    assert_eq!(mem.ram[0xB8002], 0x42);
    assert_eq!(mem.ram[0xB8003], 0x07);
}

// ========== bulk_copy_ram beyond RAM ==========

#[test]
fn test_bulk_copy_ram_src_beyond_ram() {
    let mut mem = Memory::new();
    // Source address beyond RAM length
    let beyond = mem.ram.len() + 0x1000;
    assert!(mem.bulk_copy_ram(beyond, 0xD0000, 16));
}

#[test]
fn test_bulk_read_ram_src_beyond_ram() {
    let mem = Memory::new();
    let mut buf = [0u8; 16];
    let beyond = mem.ram.len() + 0x1000;
    assert!(mem.bulk_read_ram(beyond, 16, &mut buf));
}

#[test]
fn test_bulk_read_ram_zero_length() {
    let mem = Memory::new();
    let mut buf = [0u8; 16];
    assert!(mem.bulk_read_ram(0x1000, 0, &mut buf));
}

#[test]
fn test_bulk_read_ram_empty_dst() {
    let mem = Memory::new();
    let mut buf = [];
    assert!(mem.bulk_read_ram(0x1000, 16, &mut buf));
}

// ========== Planar read with trace subscriber ==========

#[test]
fn test_planar_write_mode1_with_trace_subscriber() {
    with_trace_subscriber(|| {
        let mut mem = Memory::new();
        mem.vga.video_mode = 0x12;
        mem.vga.chain4 = false;
        mem.vga.plane_mask = 0x0F;
        mem.vga.write_mode = 1;
        mem.vga.memory_map_select = 1;
        mem.update_vga_write_handler();
        mem.set_latches([0x11, 0x22, 0x33, 0x44]);

        // This exercises the trace! logging path in write_planar_ega_masked
        mem.write_u8(VIDEO_BASE + 10, 0xFF);

        assert_eq!(mem.planes_flat[10], 0x11);
    });
}

// ========== Planar read: loads latches even for zero data ==========

#[test]
fn test_planar_read_loads_zero_latches_with_trace() {
    with_trace_subscriber(|| {
        let mut mem = Memory::new();
        mem.vga.video_mode = 0x12;
        mem.vga.chain4 = false;
        mem.vga.read_mode = 0;
        mem.vga.read_plane = 0;
        mem.vga.write_mode = 1; // Needed to trigger "zeros in WM1 context" trace
        mem.vga.memory_map_select = 1;

        // All planes are zero — reading exercises the all-zero trace path
        let val = mem.read_u8(VIDEO_BASE + 50);
        assert_eq!(val, 0x00);
        assert_eq!(mem.get_latches(), [0, 0, 0, 0]);
    });
}

// ========== note_jit_write non-contiguous path ==========

#[test]
fn test_note_jit_write_non_contiguous_rom_boundary() {
    let mut mem = Memory::new();
    // Mark exec pages near ROM boundary so non-contiguous resolution happens
    mem.mark_instr_exec_page(BIOS_BASE - 1);
    mem.take_instr_cache_dirty(); // clear

    // Write that straddles ROM boundary: addr (BIOS_BASE - 1) writes low byte,
    // addr BIOS_BASE is ROM-protected → addr_hi = None.
    // This triggers the non-contiguous path in note_jit_write.
    mem.note_jit_write(BIOS_BASE - 1, 2);
    // The non-contiguous path individually marks each resolved address
    assert!(mem.is_instr_cache_dirty());
}

// ========== ensure_ram_size resize path ==========

#[test]
fn test_ensure_ram_size_actually_resizes() {
    let mut mem = Memory::new();
    // Shrink RAM to force ensure_ram_size to actually resize
    mem.ram.resize(0x20000 + FETCH_PADDING, 0);
    assert_eq!(mem.usable_len(), 0x20000);

    mem.ensure_ram_size(0x40000);
    assert!(mem.usable_len() >= 0x40000);
    assert_eq!(mem.ram.len(), mem.usable_len() + FETCH_PADDING);
}

// ========== read_u8 returning 0xFF for out-of-bounds below video ==========

#[test]
fn test_read_u8_below_video_beyond_usable_returns_0xff() {
    let mut mem = Memory::new();
    // Shrink RAM so addresses below VIDEO_BASE can be beyond usable_len
    mem.ram.resize(0x10000 + FETCH_PADDING, 0);
    let val = mem.read_u8(0x50000); // below VIDEO_BASE, beyond usable
    assert_eq!(val, 0xFF);
}

// ========== chain4/planar read out of plane_offset bounds ==========

#[test]
fn test_chain4_read_plane_offset_beyond_plane_size() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.read_mode = 0;
    mem.vga.memory_map_select = 0; // A0000-BFFFF (128K)

    // With cell = x & ~3, offset 0x10000 maps to a plane cell at
    // PLANE_SIZE, so the second half of this 128 KiB aperture falls back to RAM.
    let in_aperture_beyond_planes = VIDEO_BASE + PLANE_SIZE;
    mem.ram[in_aperture_beyond_planes] = 0x5A;
    assert_eq!(mem.read_u8(in_aperture_beyond_planes), 0x5A);

    // An address outside the selected VGA window also falls through to RAM.
    let outside_window = 0xC0000;
    mem.ram[outside_window] = 0xA6;
    assert_eq!(mem.read_u8(outside_window), 0xA6);
}

#[test]
fn test_planar_read_beyond_plane_size() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.read_mode = 0;
    mem.vga.memory_map_select = 0; // A0000-BFFFF (128K window)

    // Offset within the window but >= PLANE_SIZE (64K)
    // At offset 0x10000 within A0000 window, offset >= PLANE_SIZE → falls through
    let addr = VIDEO_BASE + PLANE_SIZE;
    mem.ram[addr] = 0x77;
    let val = mem.read_u8(addr);
    // Should fall through to normal RAM read since offset >= PLANE_SIZE
    assert_eq!(val, 0x77);
}

// ========== chain4/planar write out of plane_offset bounds ==========

#[test]
fn test_chain4_write_plane_offset_beyond_plane_size() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.plane_mask = 0x0F;
    mem.vga.write_mode = 0;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.vga.data_rotate = 0;
    mem.vga.memory_map_select = 0; // 128K window
    mem.update_vga_write_handler();

    // Chain-4 mapping maps the first byte of the second 64 KiB to
    // cell PLANE_SIZE, which is outside the plane arrays and falls through.
    let beyond_plane_addr = VIDEO_BASE + PLANE_SIZE;
    mem.plane_mut(0)[0x4000] = 0xA5;
    mem.write_u8(beyond_plane_addr, 0x33);
    assert_eq!(mem.ram[beyond_plane_addr], 0x33);
    // The former x >> 2 layout incorrectly stored this byte at cell 0x4000.
    assert_eq!(mem.plane(0)[0x4000], 0xA5);

    // Use memory_map_select=1 (A0000-AFFFF 64K) and write to B8000
    mem.set_vga_memory_map_select(1);
    mem.update_vga_write_handler();

    // B8000 is outside A0000-AFFFF window → chain4 handler doesn't intercept
    mem.write_u8(0xB8000, 0x42);
    assert_eq!(mem.ram[0xB8000], 0x42);
}

#[test]
fn test_planar_write_beyond_plane_size_falls_through() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.plane_mask = 0x0F;
    mem.vga.write_mode = 0;
    mem.vga.bit_mask = 0xFF;
    mem.vga.memory_map_select = 0; // 128K window
    mem.update_vga_write_handler();

    // Write at offset >= PLANE_SIZE within the window → Planar handler skips
    let addr = VIDEO_BASE + PLANE_SIZE;
    mem.write_u8(addr, 0xBB);
    assert_eq!(mem.ram[addr], 0xBB);
}

// ========== read_u16/u32 fast path miss and slow path ==========

#[test]
fn test_read_u16_fast_path_miss_wrapping() {
    let mut mem = Memory::new();
    // Shrink RAM so fast path check fails for an address below VIDEO_BASE
    mem.ram.resize(0x10000 + FETCH_PADDING, 0);
    mem.ram[0x8000] = 0xAA;
    mem.ram[0x8001] = 0xBB;

    // Address below VIDEO_BASE but wrapping_add(1) >= usable_len → falls to slow path
    let val = mem.read_u16(0x8000);
    assert_eq!(val, 0xBBAA);
}

#[test]
fn test_read_u32_fast_path_miss_wrapping() {
    let mut mem = Memory::new();
    mem.ram.resize(0x10000 + FETCH_PADDING, 0);
    mem.ram[0x8000] = 0x11;
    mem.ram[0x8001] = 0x22;
    mem.ram[0x8002] = 0x33;
    mem.ram[0x8003] = 0x44;

    let val = mem.read_u32(0x8000);
    assert_eq!(val, 0x44332211);
}

// ========== bulk_copy_ram copy_len = 0 after clamping ==========

#[test]
fn test_bulk_copy_ram_dst_near_end_zero_copy() {
    let mut mem = Memory::new();
    let ram_len = mem.ram.len();
    // Source is near end, length exceeds both src and dst available
    // This hits the `copy_len = 0` early return path
    assert!(mem.bulk_copy_ram(ram_len - 1, ram_len - 1, 0x10000));
}

// ========== mark_vga_pipeline_dirty (debug assertions) ==========

#[cfg(debug_assertions)]
#[test]
fn test_mark_vga_pipeline_dirty() {
    let mut mem = Memory::new();
    assert!(!mem.vga_pipeline_debug_dirty);
    mem.mark_vga_pipeline_dirty();
    assert!(mem.vga_pipeline_debug_dirty);

    // apply_vga_pipeline should clear it
    let state = mem.vga;
    mem.apply_vga_pipeline(&state);
    assert!(!mem.vga_pipeline_debug_dirty);
}

// ========== IVT watchpoint: write VGA-range segment to IVT ==========

#[test]
fn test_ivt_write_vga_range_segment_detected() {
    let mut mem = Memory::new();
    // Write a VGA-range segment (0xA000) into IVT vector 0x10 (entry at 0x40)
    // Entry format: [offset_lo, offset_hi, segment_lo, segment_hi]
    // This triggers the IVT corruption watchpoint in write_u8_inner.
    // Since this uses a static AtomicBool that only fires once, we can only
    // verify it doesn't panic.
    mem.write_u8(0x42, 0x00); // segment low byte: 0xA000
    mem.write_u8(0x43, 0xA0); // segment high byte
}

// ========== GDT snoop: write to upper bytes of a GDT entry ==========

#[test]
fn test_gdt_snoop_upper_byte_write() {
    let mut mem = Memory::new();
    // Set up GDT snoop for a region at 0x10000 with 256 entries
    mem.set_gdt_snoop_range(0x10000, 0x7FF); // 256 * 8 - 1

    // Write to byte 5 of entry 0 (byte 5 is in the upper half)
    // entry_index=0, byte_in_entry=5, write_end=6 → triggers `write_end > 4` logging
    mem.snoop_gdt_write_ext(0x10005, 0x42, 1);
    assert!(mem.take_gdt_dirty());
}

#[test]
fn test_gdt_snoop_byte6_write_to_high_entry() {
    let mut mem = Memory::new();
    // Set up GDT at 0x10000 with room for many entries
    mem.set_gdt_snoop_range(0x10000, 0x7FF);

    // Write to byte 6 of entry 8 (entry_index >= 8 triggers detailed logging)
    // Entry 8 starts at offset 64 (8*8), byte 6 is at offset 70
    let addr = 0x10000 + 8 * 8 + 6;
    mem.snoop_gdt_write_ext(addr, 0x40, 1); // 0x40 = D/B bit set
    assert!(mem.take_gdt_dirty());
}

// ========== Drop on Memory with stats enabled ==========

#[test]
fn test_memory_drop_prints_stats_when_enabled() {
    // Create a memory with stats enabled, then drop it
    // Just verify it doesn't panic
    let mut mem = Memory::new();
    mem.tlb_stats_mut().enable();
    mem.tlb_stats().record_hit();
    drop(mem);
}

// ========== read_u8 for address in video range but no video mode match ==========

#[test]
fn test_read_u8_video_range_non_planar_non_chain4() {
    let mut mem = Memory::new();
    // Text mode (0x03) — not chain4, not planar
    mem.vga.video_mode = 0x03;
    mem.vga.chain4 = true;

    // Write directly to video RAM
    mem.ram[VIDEO_BASE + 100] = 0x55;

    // Read should just return the RAM value (no planar/chain4 logic)
    assert_eq!(mem.read_u8(VIDEO_BASE + 100), 0x55);
}

// ========== write_u8 slow path with LFB ==========

#[test]
fn test_write_u8_lfb_increments_video_write_count() {
    let mut mem = Memory::new();
    mem.init_lfb(4096);
    mem.set_lfb_enabled(true);
    let count_before = mem.video_write_count();
    mem.write_u8(LFB_BASE + 50, 0x77);
    assert!(mem.video_write_count() > count_before);
}

// ========== write_u8 MMIO path ==========

#[test]
fn test_write_u8_mmio_path() {
    let mut mem = Memory::new();
    mem.add_mmio_region(0x6000_0000, 256);
    // Write through the slow write path (MMIO check)
    mem.enable_write_tracking(); // Force slow path
    mem.set_write_context(0x10, 0x20);
    mem.write_u8(0x6000_0000, 0xAA);
    assert_eq!(mem.mmio_slice(0x6000_0000).unwrap()[0], 0xAA);
}

// ========== write_u16/u32 LFB paths ==========

#[test]
fn test_write_u16_lfb_increments_video_count() {
    let mut mem = Memory::new();
    mem.init_lfb(4096);
    mem.set_lfb_enabled(true);
    let count_before = mem.video_write_count();
    mem.write_u16(LFB_BASE + 100, 0x1234);
    assert!(mem.video_write_count() > count_before);
}

#[test]
fn test_write_u32_lfb_increments_video_count() {
    let mut mem = Memory::new();
    mem.init_lfb(4096);
    mem.set_lfb_enabled(true);
    let count_before = mem.video_write_count();
    mem.write_u32(LFB_BASE + 200, 0xDEADBEEF);
    assert!(mem.video_write_count() > count_before);
}

#[test]
fn test_mixed_writers_lfb_writes_update_lfb_and_video_count() {
    let mut mem = Memory::new();
    mem.init_lfb(4096);
    mem.set_lfb_enabled(true);

    let count_before = mem.video_write_count();
    mem.write_u8_jit(LFB_BASE + 10, 0x11);
    // write_u16_jit/write_u32_jit were retired; write_u16/write_u32 go
    // through the same `lfb_span_offset` LFB path, so they still pin the
    // little-endian byte split and video/dirty-page counting below.
    mem.write_u16(LFB_BASE + 20, 0x3322);
    mem.write_u32(LFB_BASE + 30, 0x77665544);

    assert_eq!(mem.lfb_read_u8(10), 0x11);
    assert_eq!(mem.lfb_read_u8(20), 0x22);
    assert_eq!(mem.lfb_read_u8(21), 0x33);
    assert_eq!(mem.lfb_read_u8(30), 0x44);
    assert_eq!(mem.lfb_read_u8(31), 0x55);
    assert_eq!(mem.lfb_read_u8(32), 0x66);
    assert_eq!(mem.lfb_read_u8(33), 0x77);
    assert!(mem.video_write_count() >= count_before + 3);
    assert!(mem.has_dirty_lfb_pages());
}

// ========== write_u16/u32 tracking: non-contiguous (video) falls to None ==========

#[test]
fn test_write_u16_tracking_video_not_tracked_as_contiguous() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x10, 0x20);

    // Write to video memory — should NOT be tracked as a contiguous write
    mem.write_u16(VIDEO_BASE + 100, 0x1234);
    let writes = mem.drain_pending_writes();
    // Video writes are not tracked
    assert!(writes.is_empty());
}

#[test]
fn test_write_u32_tracking_video_not_tracked_as_contiguous() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x10, 0x20);

    mem.write_u32(VIDEO_BASE + 200, 0x12345678);
    let writes = mem.drain_pending_writes();
    assert!(writes.is_empty());
}

// ========== write_u8_jit to video ==========

#[test]
fn test_write_u8_jit_video_not_tracked() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x10, 0x20);
    mem.write_u8_jit(VIDEO_BASE + 50, 0xAA);
    let writes = mem.drain_pending_writes();
    assert!(writes.is_empty());
}

// ========== write_u16/u32 with (None, None) path ==========

#[test]
fn test_write_u16_both_bytes_in_rom() {
    let mut mem = Memory::new();
    // Both bytes are in ROM — prepare_write_addr returns None for both
    mem.write_u16(BIOS_BASE + 0x100, 0xABCD);
    // Nothing should be written
    assert_ne!(mem.read_u16(BIOS_BASE + 0x100), 0xABCD);
}

#[test]
fn test_write_u32_all_bytes_in_rom() {
    let mut mem = Memory::new();
    mem.write_u32(BIOS_BASE + 0x200, 0x12345678);
    assert_ne!(mem.read_u32(BIOS_BASE + 0x200), 0x12345678);
}

// ========== slice_mut for A20-masked address ==========

#[test]
fn test_slice_mut_with_a20_masking() {
    let mut mem = Memory::new();
    mem.set_a20(false);
    {
        let mut slice = mem.slice_mut(0x10_0200, 4);
        slice[0] = 0xAA;
        slice[1] = 0xBB;
        slice[2] = 0xCC;
        slice[3] = 0xDD;
    }
    // With A20 disabled, 0x100200 maps to 0x200
    assert_eq!(mem.ram[0x200], 0xAA);
    assert_eq!(mem.ram[0x201], 0xBB);
}

// ========== write_rom with A20 disabled ==========

#[test]
fn test_write_rom_with_a20_disabled() {
    let mut mem = Memory::new();
    mem.set_a20(false);
    // write_rom bypasses ROM protection but applies A20 masking
    mem.write_rom(0x10_F100, 0x42);
    // A20 disabled: 0x10F100 → clear bit 20 → 0x0F100
    assert_eq!(mem.ram[0x0F100], 0x42);
}

// ========== read_u32_physical with A20 disabled ==========

#[test]
fn test_read_u32_physical_a20_masking() {
    let mut mem = Memory::new();
    mem.ram[0x500] = 0x11;
    mem.ram[0x501] = 0x22;
    mem.ram[0x502] = 0x33;
    mem.ram[0x503] = 0x44;
    mem.set_a20(false);

    // read_u32_physical applies mask_address
    let val = mem.read_u32_physical(0x10_0500);
    assert_eq!(val, 0x44332211);
}

// ========== write_u32_physical with A20 disabled ==========

#[test]
fn test_write_u32_physical_a20_masking() {
    let mut mem = Memory::new();
    mem.set_a20(false);
    mem.write_u32_physical(0x10_0600, 0xDEADBEEF);
    assert_eq!(
        u32::from_le_bytes([
            mem.ram[0x600],
            mem.ram[0x601],
            mem.ram[0x602],
            mem.ram[0x603],
        ]),
        0xDEADBEEF
    );
}

// ========== read_bytes with A20 disabled ==========

#[test]
fn test_read_bytes_a20_disabled() {
    let mut mem = Memory::new();
    mem.ram[0x700] = 0xAA;
    mem.ram[0x701] = 0xBB;
    mem.set_a20(false);

    let mut buf = [0u8; 2];
    mem.read_bytes(0x10_0700, &mut buf);
    assert_eq!(buf, [0xAA, 0xBB]);
}

/// MDD-BUG-KIL-00816: with A20 off, a span that crosses 1 MiB wraps to
/// 0x00000 after 0xFFFFF, exactly as the per-byte `read_u8` does. The
/// start-only mask used to copy one contiguous RAM slice instead, handing
/// back the byte at 0x100000.
#[test]
fn read_bytes_a20_disabled_wraps_a_span_that_crosses_one_mib() {
    let mut mem = Memory::new();
    mem.ram[0xF_FFFE] = 0x10;
    mem.ram[0xF_FFFF] = 0x11;
    mem.ram[0x0_0000] = 0x22;
    mem.ram[0x0_0001] = 0x23;
    mem.ram[0x10_0000] = 0x33;
    mem.ram[0x10_0001] = 0x34;
    mem.set_a20(false);

    let mut buf = [0u8; 4];
    mem.read_bytes(0xF_FFFE, &mut buf);
    assert_eq!(
        buf,
        [0x10, 0x11, 0x22, 0x23],
        "A20 off wraps at 1 MiB: never the bytes at 0x100000"
    );
    let per_byte: Vec<u8> = (0..4).map(|i| mem.read_u8(0xF_FFFE + i)).collect();
    assert_eq!(buf.to_vec(), per_byte, "read_bytes must agree with read_u8");

    // The same discontinuity sits at every 1 MiB boundary while A20 is
    // off: 0x1FFFFF folds onto 0xFFFFF and 0x200000 stays put.
    mem.ram[0x20_0000] = 0x44;
    let mut buf = [0u8; 2];
    mem.read_bytes(0x1F_FFFF, &mut buf);
    assert_eq!(buf, [0x11, 0x44]);

    // And with A20 on, the same span is the contiguous RAM it looks like.
    mem.set_a20(true);
    let mut buf = [0u8; 4];
    mem.read_bytes(0xF_FFFE, &mut buf);
    assert_eq!(buf, [0x10, 0x11, 0x33, 0x34]);
}

// ========== LFB lfb_write_u8 out of bounds ==========

#[test]
fn test_lfb_write_u8_out_of_bounds() {
    let mut mem = Memory::new();
    mem.init_lfb(256);
    // Writing past the LFB size should be a no-op
    mem.lfb_write_u8(300, 0xFF);
    // Just verify no panic
}

// ========== read_u16/u32 fast path branch: masked addr near end of RAM ==========

#[test]
fn test_read_u16_masked_addr_near_end_falls_to_slow() {
    let mut mem = Memory::new();
    // Shrink RAM to a small size, then read near the boundary
    mem.ram.resize(0x8000 + FETCH_PADDING, 0);
    mem.ram[0x7FFF] = 0xEE;
    // addr 0x7FFF: addr.wrapping_add(1) = 0x8000 which is NOT < usable_len (0x8000)
    // This causes the fast path condition to fail, falling to slow path
    let val = mem.read_u16(0x7FFF);
    // Low byte = 0xEE, high byte at 0x8000 is beyond usable → 0xFF
    assert_eq!(val, 0xFF_EE);
}

#[test]
fn test_read_u32_masked_addr_near_end_falls_to_slow() {
    let mut mem = Memory::new();
    mem.ram.resize(0x8000 + FETCH_PADDING, 0);
    mem.ram[0x7FFD] = 0x11;
    mem.ram[0x7FFE] = 0x22;
    mem.ram[0x7FFF] = 0x33;
    // addr.wrapping_add(3) = 0x8000 NOT < 0x8000 → slow path
    let val = mem.read_u32(0x7FFD);
    // First 3 bytes from RAM, byte at 0x8000 is beyond → 0xFF
    assert_eq!(val, 0xFF332211);
}

// ========== chain4 read: offset not in window → fallthrough ==========

#[test]
fn test_chain4_read_outside_window_returns_ram() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.read_mode = 0;
    mem.vga.memory_map_select = 1; // A0000-AFFFF (64K)

    // Write directly to RAM at 0xB0000 (outside the A0000-AFFFF window)
    mem.ram[0xB0000] = 0x77;
    // Read should bypass chain4 and return from RAM
    let val = mem.read_u8(0xB0000);
    assert_eq!(val, 0x77);
}

// ========== planar read: offset >= PLANE_SIZE → fallthrough ==========

#[test]
fn test_planar_read_offset_above_plane_size_returns_ram() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.read_mode = 0;
    mem.vga.memory_map_select = 0; // A0000-BFFFF (128K)

    // At VIDEO_BASE + PLANE_SIZE, offset = PLANE_SIZE (0x10000) which is >= PLANE_SIZE
    // This causes the `offset < PLANE_SIZE` check to fail → falls through
    let addr = VIDEO_BASE + PLANE_SIZE;
    mem.ram[addr] = 0xCC;
    let val = mem.read_u8(addr);
    assert_eq!(val, 0xCC);
}

// ========== chain4 write: offset not in window or plane_offset out of bounds ==========

#[test]
fn test_chain4_write_outside_window_linear() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.plane_mask = 0x0F;
    mem.vga.write_mode = 0;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.vga.data_rotate = 0;
    mem.vga.memory_map_select = 1; // A0000-AFFFF (64K)
    mem.update_vga_write_handler();

    // Write to B0000 — outside the A0000-AFFFF window
    // Chain4 handler gets vga_offset = None → falls through to linear
    mem.write_u8(0xB0000, 0x55);
    assert_eq!(mem.ram[0xB0000], 0x55);
}

// ========== planar write: offset >= PLANE_SIZE → linear fallthrough ==========

#[test]
fn test_planar_write_offset_above_plane_size_linear() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.plane_mask = 0x0F;
    mem.vga.write_mode = 0;
    mem.vga.bit_mask = 0xFF;
    mem.vga.memory_map_select = 0; // A0000-BFFFF (128K)
    mem.update_vga_write_handler();

    // Write at offset = PLANE_SIZE within the window → skips planar
    let addr = VIDEO_BASE + PLANE_SIZE;
    mem.write_u8(addr, 0xDD);
    assert_eq!(mem.ram[addr], 0xDD);
}

// ========== bulk_copy_ram: copy_len = 0 after clamping ==========

#[test]
fn test_bulk_copy_ram_src_at_ram_end_zero_copy() {
    let mut mem = Memory::new();
    let ram_end = mem.ram.len();
    // Source is the last byte of RAM, destination is in safe area, len=2
    // copy_len = min(2, ram_len - ram_end + 1 = 1) = 1
    // But if src = ram_end, then copy_len = 0
    assert!(mem.bulk_copy_ram(ram_end, 0xD0000, 2));
}

// ========== GDT snoop: byte6 via 2-byte write at byte 5 ==========

#[test]
fn test_gdt_snoop_byte6_via_word_write_at_byte5() {
    let mut mem = Memory::new();
    mem.set_gdt_snoop_range(0x10000, 0x7FF);

    // Write 2 bytes at byte_in_entry=5 of entry 9 (entry_index=9 >= 8)
    // byte_in_entry=5, write_size=2, write_end=7 → covers byte 6
    // Since byte_in_entry == 5 and write_size == 2, byte6_val = (val >> 8) as u8
    let addr = 0x10000 + 9 * 8 + 5;
    mem.snoop_gdt_write_ext(addr, 0x4000, 2); // byte5=0x00, byte6=0x40
    assert!(mem.take_gdt_dirty());
}

#[test]
fn test_gdt_snoop_byte6_via_wide_write_at_byte4() {
    let mut mem = Memory::new();
    mem.set_gdt_snoop_range(0x10000, 0x7FF);

    // Write 4 bytes at byte_in_entry=4 of entry 10 (entry_index >= 8)
    // byte_in_entry=4, write_size=4, write_end=8 → covers byte 6
    // byte6_val = ((val >> ((6 - 4) * 8)) & 0xFF) as u8 = (val >> 16) & 0xFF
    let addr = 0x10000 + 10 * 8 + 4;
    mem.snoop_gdt_write_ext(addr, 0x0040_0000, 4); // byte6 = 0x40 (D/B set)
    assert!(mem.take_gdt_dirty());
}

// ========== GDT snoop: entry < 8 (no detailed logging) ==========

#[test]
fn test_gdt_snoop_byte6_low_entry_no_detail() {
    let mut mem = Memory::new();
    mem.set_gdt_snoop_range(0x10000, 0x7FF);

    // Write to byte 6 of entry 2 (entry_index < 8 → no detailed dump)
    let addr = 0x10000 + 2 * 8 + 6;
    mem.snoop_gdt_write_ext(addr, 0x40, 1);
    assert!(mem.take_gdt_dirty());
}

// ========== GDT snoop: write in lower bytes (byte_in_entry < 4, write_end <= 4) ==========

#[test]
fn test_gdt_snoop_lower_bytes_no_upper_log() {
    let mut mem = Memory::new();
    mem.set_gdt_snoop_range(0x10000, 0x7FF);

    // Write to byte 0 of entry 0 — lower bytes only, no upper byte logging
    let addr = 0x10000;
    mem.snoop_gdt_write_ext(addr, 0x42, 1);
    assert!(mem.take_gdt_dirty());
}

// ========== write_u16/u32 with tracking: non-contiguous (None, None) path ==========

#[test]
fn test_write_u16_tracking_both_bytes_rom() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x10, 0x20);
    // Both bytes in ROM → no tracking
    mem.write_u16(BIOS_BASE + 0x500, 0xABCD);
    let writes = mem.drain_pending_writes();
    assert!(writes.is_empty());
}

#[test]
fn test_write_u32_tracking_all_bytes_rom() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x10, 0x20);
    mem.write_u32(BIOS_BASE + 0x600, 0x12345678);
    let writes = mem.drain_pending_writes();
    assert!(writes.is_empty());
}

// ========== write_u32 non-contiguous: some bytes in ROM ==========

#[test]
fn test_write_u32_partial_rom_non_contiguous() {
    let mut mem = Memory::new();
    // Write spanning ROM boundary: 2 bytes before ROM, 2 in ROM
    let addr = BIOS_BASE - 2;
    mem.write_u32(addr, 0xAABBCCDD);
    // First 2 bytes written
    assert_eq!(mem.read_u8(addr), 0xDD);
    assert_eq!(mem.read_u8(addr + 1), 0xCC);
    // ROM bytes not written
}

// ========== note_jit_write: all resolved are None ==========

#[test]
fn test_note_jit_write_all_resolved_none() {
    let mut mem = Memory::new();
    // All addresses in ROM → all resolved = None → early return
    mem.note_jit_write(BIOS_BASE + 0x100, 2);
    // Should not panic and should not mark dirty
    assert!(!mem.is_instr_cache_dirty());
}

// ========== remove_physical_mapping: edge case ==========

#[test]
fn test_add_physical_mapping_replaces_existing() {
    let mut mem = Memory::new();
    assert!(mem.add_physical_mapping(0x200_000, 0x300_000, 0x1000));
    // Add again at same linear address → replaces
    assert!(mem.add_physical_mapping(0x200_000, 0x400_000, 0x2000));
    assert_eq!(mem.map_linear_physical(0x200_000), Some(0x400_000));
}

#[test]
fn test_add_physical_mapping_size_zero_ignored() {
    let mut mem = Memory::new();
    let before_count = mem.physical_mappings.len();
    assert!(mem.add_physical_mapping(0x200_000, 0x300_000, 0));
    assert_eq!(mem.physical_mappings.len(), before_count);
}

#[test]
fn test_add_physical_mapping_full_table_rejects_distinct_mapping_atomically() {
    let mut mem = Memory::new();
    for index in 0..64u32 {
        let linear = 0x100_000 + index * 0x1000;
        assert!(mem.add_physical_mapping(linear, 0x200_000 + index * 0x1000, 0x1000));
    }
    assert!(mem.physical_mappings.is_full());

    assert!(!mem.add_physical_mapping(0x300_000, 0x400_000, 0x1000));
    assert_eq!(mem.physical_mappings.len(), 64);
    assert_eq!(mem.map_linear_physical(0x300_000), None);
    for index in 0..64u32 {
        assert_eq!(
            mem.map_linear_physical(0x100_000 + index * 0x1000),
            Some((0x200_000 + index * 0x1000) as usize)
        );
    }

    assert!(mem.add_physical_mapping(0x100_000, 0x500_000, 0x1000));
    assert_eq!(mem.physical_mappings.len(), 64);
    assert_eq!(mem.map_linear_physical(0x100_000), Some(0x500_000));
}

// ========== MDD-BUG-CRUCIBLE-00666: mappings that reach 4 GiB ==========

/// A one-byte mapping at the very last linear address must be findable.
///
/// Its exclusive end is 0x1_0000_0000. Computed as a `u32` saturating add
/// that pinned back to 0xFFFF_FFFF, so `linear < end` rejected the only
/// byte the mapping had.
#[test]
fn test_add_physical_mapping_one_byte_at_the_last_linear_address() {
    let mut mem = Memory::new();
    assert!(mem.add_physical_mapping(0xFFFF_FFFF, 0x40_0000, 1));
    assert_eq!(mem.map_linear_physical(0xFFFF_FFFF), Some(0x40_0000));
    // Nothing below it is covered by a one-byte mapping.
    assert_eq!(mem.map_linear_physical(0xFFFF_FFFE), None);
}

/// The final page, ending exactly at 4 GiB, must be covered end to end.
#[test]
fn test_add_physical_mapping_final_page_covers_its_last_byte() {
    let mut mem = Memory::new();
    assert!(mem.add_physical_mapping(0xFFFF_F000, 0x50_0000, 0x1000));

    assert_eq!(mem.map_linear_physical(0xFFFF_F000), Some(0x50_0000));
    assert_eq!(mem.map_linear_physical(0xFFFF_F800), Some(0x50_0800));
    // The byte the u32 saturation used to drop.
    assert_eq!(mem.map_linear_physical(0xFFFF_FFFF), Some(0x50_0FFF));
    assert_eq!(mem.map_linear_physical(0xFFFF_EFFF), None);

    // The range helper delegates to the same lookup, so it must agree.
    assert!(mem.linear_range_has_physical_mapping(0xFFFF_FFFF, 1));
}

/// A range whose mathematical end passes 4 GiB is rejected, and rejecting
/// it must not disturb the table — including a mapping that shares its
/// linear base, which the old code would have evicted before failing.
#[test]
fn test_add_physical_mapping_rejects_a_range_past_four_gib_without_side_effects() {
    let mut mem = Memory::new();
    assert!(mem.add_physical_mapping(0xFFFF_FFFF, 0x60_0000, 1));
    let before = mem.physical_mappings.len();

    // 0xFFFF_FFFF + 2 = 0x1_0000_0001, one past the end.
    assert!(!mem.add_physical_mapping(0xFFFF_FFFF, 0x70_0000, 2));
    // A whole page starting one byte late also overruns.
    assert!(!mem.add_physical_mapping(0xFFFF_F001, 0x70_0000, 0x1000));
    // And the largest possible overrun.
    assert!(!mem.add_physical_mapping(0x1000, 0x70_0000, u32::MAX));

    assert_eq!(
        mem.physical_mappings.len(),
        before,
        "a rejected range changed the table"
    );
    assert_eq!(
        mem.map_linear_physical(0xFFFF_FFFF),
        Some(0x60_0000),
        "the rejected request evicted the mapping sharing its linear base"
    );
}

/// The exact boundary between accepted and rejected, from both sides.
#[test]
fn test_add_physical_mapping_boundary_is_an_end_of_exactly_four_gib() {
    let mut mem = Memory::new();
    // Ends exactly at 4 GiB: allowed.
    assert!(mem.add_physical_mapping(0x8000_0000, 0x10_0000, 0x8000_0000));
    assert_eq!(
        mem.map_linear_physical(0xFFFF_FFFF),
        Some(0x10_0000 + 0x7FFF_FFFF)
    );

    // One byte more: rejected.
    let mut mem = Memory::new();
    assert!(!mem.add_physical_mapping(0x8000_0001, 0x10_0000, 0x8000_0000));
}

// ========== chain4 read_mode_1 with partial dont_care ==========

#[test]
fn test_chain4_read_mode1_partial_dont_care() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.read_mode = 1;
    mem.vga.color_compare = 0x01; // bit 0 only
    mem.vga.color_dont_care = 0x01; // Only plane 0 matters
    mem.vga.memory_map_select = 1;

    let cell = 20;
    mem.planes_flat[cell] = 0xFF; // plane 0: all 1s → matches compare bit 0
    mem.planes_flat[PLANE_SIZE + cell] = 0x00; // plane 1: irrelevant
    mem.planes_flat[PLANE_SIZE * 2 + cell] = 0xFF; // plane 2: irrelevant
    mem.planes_flat[PLANE_SIZE * 3 + cell] = 0xFF; // plane 3: irrelevant

    let addr = VIDEO_BASE + cell; // x&3 selects plane 0; x&~3 is this cell
    let val = mem.read_u8(addr);
    // Only plane 0 checked, and it matches → 0xFF
    assert_eq!(val, 0xFF);
}

#[test]
fn test_paged_memory_trait() {
    let mut mem = Memory::new();

    // tlb_lookup should work through trait
    assert!(<Memory as PagedMemory>::tlb_lookup(&mem, 0x42).is_none());

    // tlb_insert through trait
    <Memory as PagedMemory>::tlb_insert(&mut mem, 0x42, 0x10, true, true, false);
    let result = <Memory as PagedMemory>::tlb_lookup(&mem, 0x42);
    assert!(result.is_some());
    let (phys, w, u, d) = result.unwrap();
    assert_eq!(phys, 0x10);
    assert!(w);
    assert!(u);
    assert!(!d);

    // invalidate_tlb_page
    <Memory as PagedMemory>::invalidate_tlb_page(&mut mem, 0x42 << 12);
    assert!(<Memory as PagedMemory>::tlb_lookup(&mem, 0x42).is_none());

    // invalidate_tlb
    <Memory as PagedMemory>::tlb_insert(&mut mem, 0x42, 0x10, true, true, false);
    <Memory as PagedMemory>::invalidate_tlb(&mut mem);
    assert!(<Memory as PagedMemory>::tlb_lookup(&mem, 0x42).is_none());

    // read/write_u32_physical
    <Memory as PagedMemory>::write_u32_physical(&mut mem, 0x10000, 0x12345678);
    assert_eq!(
        <Memory as PagedMemory>::read_u32_physical(&mem, 0x10000),
        0x12345678
    );
}
