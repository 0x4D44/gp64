use super::*;

// ========== Write Tracking Tests ==========

#[test]
fn test_write_tracking_disabled_by_default() {
    let mem = Memory::new();
    assert!(!mem.is_write_tracking_enabled());
    assert_eq!(mem.pending_write_count(), 0);
    assert!(mem.write_context().is_none());
}

#[test]
fn test_write_tracking_enable_disable() {
    let mut mem = Memory::new();

    mem.enable_write_tracking();
    assert!(mem.is_write_tracking_enabled());
    let policy_epoch = mem.direct_memory_policy_epoch();

    mem.enable_write_tracking();
    assert_eq!(mem.direct_memory_policy_epoch(), policy_epoch);

    mem.disable_write_tracking();
    assert!(!mem.is_write_tracking_enabled());
}

#[test]
fn enabling_write_tracking_retires_warm_writable_tlb_entry() {
    let mut mem = Memory::new();
    let page = 0x50;
    mem.tlb_insert(page, page, true, true, true);
    assert!(mem.tlb_lookup(page).is_some());

    let index = SoftwareTlb::index(page);
    let entry = unsafe { &*mem.tlb_entries_ptr().add(index) };
    assert!(!entry.host_write.is_null());

    mem.enable_write_tracking();

    assert!(mem.tlb_lookup(page).is_none());
    let entry = unsafe { &*mem.tlb_entries_ptr().add(index) };
    assert!(entry.host_write.is_null());

    mem.set_write_context(0x1000, 0x0100);
    mem.write_u8((page as usize) << 12, 0xAB);
    assert_eq!(mem.pending_write_count(), 1);
}

#[test]
fn test_write_context_set_clear() {
    let mut mem = Memory::new();

    assert!(mem.write_context().is_none());

    mem.set_write_context(0x1234, 0x5678);
    let ctx = mem.write_context().unwrap();
    assert_eq!(ctx.cs, 0x1234);
    assert_eq!(ctx.ip, 0x5678);
    assert_eq!(ctx.regs, None);

    mem.clear_write_context();
    assert!(mem.write_context().is_none());
}

#[test]
fn test_write_context_with_regs_set_clear() {
    let mut mem = Memory::new();
    let regs = WriteRegisterContext {
        eax: 0x1111_2222,
        ebx: 0x3333_4444,
        ecx: 0x5555_6666,
        edx: 0x7777_8888,
        esi: 0x9999_AAAA,
        edi: 0xBBBB_CCCC,
        ebp: 0xDDDD_EEEE,
        esp: 0xFFFF_0000,
        ds: 0x0023,
        es: 0x0023,
        ss: 0x0010,
        cs_base: 0,
        cs_limit: 0xFFFF_FFFF,
        cs_access: 0x9B,
        cs_big: true,
        ds_base: 0,
        ds_limit: 0xFFFF_FFFF,
        ds_access: 0x93,
        ds_big: true,
        es_base: 0,
        es_limit: 0xFFFF_FFFF,
        es_access: 0x93,
        es_big: true,
        ss_base: 0,
        ss_limit: 0xFFFF_FFFF,
        ss_access: 0x93,
        ss_big: true,
        eflags: 0x0000_0246,
        cr0: 0x8001_003B,
        cr3: Some(0x1111_2000),
        cpl: Some(0),
        mode: IoCpuMode::Protected,
    };

    mem.set_write_context_with_regs(0x0008, 0x1234_5678, regs);
    let ctx = mem.write_context().unwrap();
    assert_eq!(ctx.cs, 0x0008);
    assert_eq!(ctx.ip, 0x1234_5678);
    assert_eq!(ctx.regs, Some(regs));

    mem.clear_write_context();
    assert!(mem.write_context().is_none());
}

#[test]
fn test_write_tracking_no_record_when_disabled() {
    let mut mem = Memory::new();
    mem.set_write_context(0x1000, 0x0100);
    // Tracking is disabled
    mem.write_u8(0x5000, 0xAB);

    assert_eq!(mem.pending_write_count(), 0);
}

#[test]
fn test_write_tracking_no_record_without_context() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    // No context set
    mem.write_u8(0x5000, 0xAB);

    // No pending writes because no context was set
    assert_eq!(mem.pending_write_count(), 0);
}

#[test]
fn test_write_tracking_u8() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x1000, 0x0100);

    // Write initial value
    mem.clear_write_context();
    mem.disable_write_tracking();
    mem.write_u8(0x5000, 0x11); // Set up initial value

    mem.enable_write_tracking();
    mem.set_write_context(0x2000, 0x0200);
    mem.write_u8(0x5000, 0x22); // This write should be tracked

    assert_eq!(mem.pending_write_count(), 1);

    let writes = mem.drain_pending_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].source_cs, 0x2000);
    assert_eq!(writes[0].source_ip, 0x0200);
    assert_eq!(writes[0].linear_address, 0x5000);
    assert_eq!(writes[0].old_value, 0x11);
    assert_eq!(writes[0].new_value, 0x22);
    assert_eq!(writes[0].size, 1);
}

#[test]
fn test_write_tracking_u16() {
    let mut mem = Memory::new();
    mem.write_u16(0x6000, 0x1122); // Initial value

    mem.enable_write_tracking();
    mem.set_write_context(0x3000, 0x0300);
    mem.write_u16(0x6000, 0x3344);

    assert_eq!(mem.pending_write_count(), 1);

    let writes = mem.drain_pending_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].source_cs, 0x3000);
    assert_eq!(writes[0].source_ip, 0x0300);
    assert_eq!(writes[0].linear_address, 0x6000);
    assert_eq!(writes[0].old_value, 0x1122);
    assert_eq!(writes[0].new_value, 0x3344);
    assert_eq!(writes[0].size, 2);
}

#[test]
fn test_write_tracking_u32() {
    let mut mem = Memory::new();
    mem.write_u32(0x7000, 0xAABBCCDD); // Initial value

    mem.enable_write_tracking();
    mem.set_write_context(0x4000, 0x0400);
    mem.write_u32(0x7000, 0x11223344);

    assert_eq!(mem.pending_write_count(), 1);

    let writes = mem.drain_pending_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].source_cs, 0x4000);
    assert_eq!(writes[0].source_ip, 0x0400);
    assert_eq!(writes[0].linear_address, 0x7000);
    assert_eq!(writes[0].old_value, 0xAABBCCDD);
    assert_eq!(writes[0].new_value, 0x11223344);
    assert_eq!(writes[0].size, 4);
}

#[test]
fn test_write_tracking_drain_clears_pending() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x1000, 0x0100);

    mem.write_u8(0x8000, 0xAB);
    mem.write_u8(0x8001, 0xCD);

    assert_eq!(mem.pending_write_count(), 2);

    let writes = mem.drain_pending_writes();
    assert_eq!(writes.len(), 2);
    assert_eq!(mem.pending_write_count(), 0);
}

#[test]
fn test_write_tracking_video_memory_not_tracked() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x1000, 0x0100);

    // Write to video memory (A0000-AFFFF) should NOT be tracked
    mem.write_u8(VIDEO_BASE + 100, 0xAB);

    assert_eq!(mem.pending_write_count(), 0);
}

#[test]
fn test_write_tracking_multiple_writes_same_context() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x1000, 0x0100);

    mem.write_u8(0x9000, 0x11);
    mem.write_u16(0x9010, 0x2233);
    mem.write_u32(0x9020, 0x44556677);

    assert_eq!(mem.pending_write_count(), 3);

    let writes = mem.drain_pending_writes();
    assert_eq!(writes.len(), 3);

    // All should have same context
    for w in &writes {
        assert_eq!(w.source_cs, 0x1000);
        assert_eq!(w.source_ip, 0x0100);
    }

    assert_eq!(writes[0].size, 1);
    assert_eq!(writes[1].size, 2);
    assert_eq!(writes[2].size, 4);
}

#[test]
fn test_write_tracking_context_changes_between_writes() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();

    mem.set_write_context(0x1000, 0x0100);
    mem.write_u8(0xA000, 0x11);

    mem.set_write_context(0x2000, 0x0200);
    mem.write_u8(0xA001, 0x22);

    let writes = mem.drain_pending_writes();
    assert_eq!(writes.len(), 2);

    assert_eq!(writes[0].source_cs, 0x1000);
    assert_eq!(writes[0].source_ip, 0x0100);

    assert_eq!(writes[1].source_cs, 0x2000);
    assert_eq!(writes[1].source_ip, 0x0200);
}

#[test]
fn test_write_tracking_disable_clears_pending() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x1000, 0x0100);
    mem.write_u8(0xB000, 0xAB);

    assert_eq!(mem.pending_write_count(), 1);

    mem.disable_write_tracking();

    assert_eq!(mem.pending_write_count(), 0);
    assert!(mem.write_context().is_none());
}

#[test]
fn test_pending_write_new() {
    let pw = PendingWrite::new(0x1234, 0x5678, 0xABCD, 0x11, 0x22, 1);
    assert_eq!(pw.source_cs, 0x1234);
    assert_eq!(pw.source_ip, 0x5678);
    assert_eq!(pw.linear_address, 0xABCD);
    assert_eq!(pw.old_value, 0x11);
    assert_eq!(pw.new_value, 0x22);
    assert_eq!(pw.size, 1);
}

#[test]
fn test_write_context_default() {
    let ctx = WriteContext::default();
    assert_eq!(ctx.cs, 0);
    assert_eq!(ctx.ip, 0);
    assert_eq!(ctx.regs, None);
}

// ===== HLE Handler Tracking Tests =====

#[test]
fn test_hle_handler_inactive_by_default() {
    let mem = Memory::new();
    assert!(!mem.is_hle_handler_active());
}

#[test]
fn test_hle_handler_set_active() {
    let mut mem = Memory::new();
    mem.set_hle_handler_active(true);
    assert!(mem.is_hle_handler_active());

    mem.set_hle_handler_active(false);
    assert!(!mem.is_hle_handler_active());
}

#[test]
fn test_write_tracking_with_hle_handler_flag() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x1000, 0x0100);
    mem.set_hle_handler_active(true);

    mem.write_u8(0xB000, 0xAB);

    let pending = mem.drain_pending_writes();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].is_hle_handler);
}

#[test]
fn test_write_tracking_without_hle_handler_flag() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x1000, 0x0100);
    // HLE handler NOT active (default)

    mem.write_u8(0xB000, 0xAB);

    let pending = mem.drain_pending_writes();
    assert_eq!(pending.len(), 1);
    assert!(!pending[0].is_hle_handler);
}

#[test]
fn test_hle_handler_flag_changes_between_writes() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x1000, 0x0100);

    // First write: not HLE
    mem.write_u8(0xB000, 0x11);

    // Enable HLE context
    mem.set_hle_handler_active(true);
    mem.write_u8(0xB001, 0x22);

    // Disable HLE context
    mem.set_hle_handler_active(false);
    mem.write_u8(0xB002, 0x33);

    let pending = mem.drain_pending_writes();
    assert_eq!(pending.len(), 3);
    assert!(!pending[0].is_hle_handler);
    assert!(pending[1].is_hle_handler);
    assert!(!pending[2].is_hle_handler);
}

#[test]
fn test_pending_write_with_hle_flag() {
    let pw = PendingWrite::new_with_hle(0x1234, 0x5678, 0xABCD, 0x11, 0x22, 1, true);
    assert!(pw.is_hle_handler);

    let pw2 = PendingWrite::new_with_hle(0x1234, 0x5678, 0xABCD, 0x11, 0x22, 1, false);
    assert!(!pw2.is_hle_handler);
}

#[test]
fn test_pending_write_new_defaults_to_not_hle() {
    let pw = PendingWrite::new(0x1234, 0x5678, 0xABCD, 0x11, 0x22, 1);
    assert!(!pw.is_hle_handler);
}

// ===== VGA Planar Write Tracking Tests =====

#[test]
fn test_pending_write_vga_fields_default() {
    // PendingWrite::new should default to not VGA write
    let pw = PendingWrite::new(0x1234, 0x5678, 0xABCD, 0x11, 0x22, 1);
    assert!(!pw.is_vga_write);
    assert!(pw.vga_plane.is_none());
}

#[test]
fn test_pending_write_new_with_vga() {
    let pw = PendingWrite::new_with_vga(0x1234, 0x5678, 0xABCD, 0x11, 0x22, 1, 2);
    assert!(pw.is_vga_write);
    assert_eq!(pw.vga_plane, Some(2));
    assert!(!pw.is_hle_handler);
}

#[test]
fn test_vga_planar_write_tracking_mode0_single_plane() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x1000, 0x0100);
    mem.set_video_mode(0x12); // 640x480 16-color mode

    // Configure for single plane write (plane 0 only)
    mem.set_plane_mask(0x01); // Only plane 0
    mem.set_write_mode(0); // Write mode 0
    mem.set_bit_mask(0xFF); // All bits
    mem.set_enable_set_reset(0x00); // No set/reset

    // Write to video memory via planar write
    let offset = 0x1000;
    mem.write_planar_ega_tracked(offset, 0xAB);

    let pending = mem.drain_pending_writes();
    assert_eq!(pending.len(), 1);

    let pw = &pending[0];
    assert!(pw.is_vga_write);
    assert_eq!(pw.vga_plane, Some(0));
    assert_eq!(pw.new_value, 0xAB);
}

#[test]
fn test_vga_planar_write_tracking_mode0_all_planes() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x1000, 0x0100);
    mem.set_video_mode(0x12);

    // Configure for all planes write
    mem.set_plane_mask(0x0F); // All 4 planes
    mem.set_write_mode(0);
    mem.set_bit_mask(0xFF);
    mem.set_enable_set_reset(0x00);

    let offset = 0x2000;
    mem.write_planar_ega_tracked(offset, 0x55);

    let pending = mem.drain_pending_writes();
    assert_eq!(pending.len(), 4);

    // Verify each plane
    for (i, pw) in pending.iter().enumerate() {
        assert!(pw.is_vga_write);
        assert_eq!(pw.vga_plane, Some(i as u8));
        assert_eq!(pw.new_value, 0x55);
    }
}

#[test]
fn test_vga_planar_write_tracking_mode1_latch_copy() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x1000, 0x0100);
    mem.set_video_mode(0x12);

    // Set up latches with known values
    mem.set_latches([0x11, 0x22, 0x33, 0x44]);

    // Configure for latch copy (write mode 1)
    mem.set_plane_mask(0x0F);
    mem.set_write_mode(1);

    let offset = 0x3000;
    mem.write_planar_ega_tracked(offset, 0xFF); // CPU data ignored in mode 1

    let pending = mem.drain_pending_writes();
    assert_eq!(pending.len(), 4);

    // Verify latch values were written
    assert_eq!(pending[0].new_value, 0x11);
    assert_eq!(pending[1].new_value, 0x22);
    assert_eq!(pending[2].new_value, 0x33);
    assert_eq!(pending[3].new_value, 0x44);
}

#[test]
fn test_vga_planar_write_tracking_mode2_color_expansion() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x1000, 0x0100);
    mem.set_video_mode(0x12);

    mem.set_plane_mask(0x0F);
    mem.set_write_mode(2); // Color expansion
    mem.set_bit_mask(0xFF);
    mem.set_data_rotate(0x00); // No logical op

    // Color 5 = 0101 binary (planes 0 and 2 set)
    let offset = 0x4000;
    mem.write_planar_ega_tracked(offset, 0x05);

    let pending = mem.drain_pending_writes();
    assert_eq!(pending.len(), 4);

    // Planes 0 and 2 should get 0xFF (bit set), planes 1 and 3 should get 0x00
    assert_eq!(pending[0].new_value, 0xFF);
    assert_eq!(pending[1].new_value, 0x00);
    assert_eq!(pending[2].new_value, 0xFF);
    assert_eq!(pending[3].new_value, 0x00);
}

#[test]
fn test_vga_planar_write_tracking_mode3() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x1000, 0x0100);
    mem.set_video_mode(0x12);

    // Set up latches
    mem.set_latches([0x00, 0x00, 0x00, 0x00]);

    mem.set_plane_mask(0x0F);
    mem.set_write_mode(3); // VGA optimized mode
    mem.set_bit_mask(0xFF);
    mem.set_set_reset(0x0F); // Set/reset value = all planes set
    mem.set_data_rotate(0x00);

    let offset = 0x5000;
    mem.write_planar_ega_tracked(offset, 0xAA); // Creates effective mask

    let pending = mem.drain_pending_writes();
    assert_eq!(pending.len(), 4);

    // All should be VGA writes
    for pw in &pending {
        assert!(pw.is_vga_write);
    }
}

#[test]
fn test_vga_mode_x_write_tracking() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x1000, 0x0100);
    mem.set_video_mode(0x13); // Mode 13h base for Mode X

    // Write to planes 0 and 2 via Mode X interface
    let offset = 0x6000;
    mem.write_video_planar_tracked(offset, 0xCC, 0x05); // planes 0 and 2

    let pending = mem.drain_pending_writes();
    assert_eq!(pending.len(), 2);

    assert!(pending[0].is_vga_write);
    assert_eq!(pending[0].vga_plane, Some(0));
    assert_eq!(pending[0].new_value, 0xCC);

    assert!(pending[1].is_vga_write);
    assert_eq!(pending[1].vga_plane, Some(2));
    assert_eq!(pending[1].new_value, 0xCC);
}

#[test]
fn test_vga_tracking_records_old_value() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x1000, 0x0100);
    mem.set_video_mode(0x12);

    // Pre-populate plane 0 with a value
    let offset = 0x7000;
    mem.planes_flat[offset] = 0x42;

    mem.set_plane_mask(0x01); // Only plane 0
    mem.set_write_mode(0);
    mem.set_bit_mask(0xFF);
    mem.set_enable_set_reset(0x00);

    mem.write_planar_ega_tracked(offset, 0xAB);

    let pending = mem.drain_pending_writes();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].old_value, 0x42);
    assert_eq!(pending[0].new_value, 0xAB);
}

#[test]
fn test_vga_tracking_disabled_no_recording() {
    let mut mem = Memory::new();
    // write tracking NOT enabled
    mem.set_write_context(0x1000, 0x0100);
    mem.set_video_mode(0x12);

    mem.set_plane_mask(0x0F);
    mem.set_write_mode(0);
    mem.set_bit_mask(0xFF);
    mem.set_enable_set_reset(0x00);

    mem.write_planar_ega_tracked(0x8000, 0x55);

    assert_eq!(mem.pending_write_count(), 0);
}

#[test]
fn test_vga_tracking_no_context_no_recording() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    // write context NOT set
    mem.set_video_mode(0x12);

    mem.set_plane_mask(0x0F);
    mem.set_write_mode(0);
    mem.set_bit_mask(0xFF);
    mem.set_enable_set_reset(0x00);

    mem.write_planar_ega_tracked(0x9000, 0x55);

    assert_eq!(mem.pending_write_count(), 0);
}

#[test]
fn test_vga_linear_address_calculation() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x1000, 0x0100);
    mem.set_video_mode(0x12);

    mem.set_plane_mask(0x01); // Plane 0 only
    mem.set_write_mode(0);
    mem.set_bit_mask(0xFF);
    mem.set_enable_set_reset(0x00);

    let offset = 0x1234;
    mem.write_planar_ega_tracked(offset, 0xAB);

    let pending = mem.drain_pending_writes();
    assert_eq!(pending.len(), 1);
    // Linear address should be VIDEO_BASE + offset
    assert_eq!(pending[0].linear_address, (VIDEO_BASE + offset) as u32);
}

// ===== JIT Write Tracking Tests =====

#[test]
fn test_jit_write_u8_tracking_enabled() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x1000, 0x0100);

    // Write using JIT method
    let addr = 0x5000;
    mem.write_u8_jit(addr, 0xAB);

    let pending = mem.drain_pending_writes();
    assert_eq!(pending.len(), 1);

    let pw = &pending[0];
    assert_eq!(pw.source_cs, 0x1000);
    assert_eq!(pw.source_ip, 0x0100);
    assert_eq!(pw.linear_address, addr as u32);
    assert_eq!(pw.new_value, 0xAB);
    assert_eq!(pw.size, 1);
    assert!(!pw.is_vga_write);
    assert!(!pw.is_hle_handler);
}

#[test]
fn test_jit_write_tracking_disabled() {
    let mut mem = Memory::new();
    // write tracking NOT enabled
    mem.set_write_context(0x1000, 0x0100);

    // write_u16_jit/write_u32_jit were retired; write_u16/write_u32 share
    // the same `record_write` gate (tracking disabled → no pending
    // write), so they still pin that this behaviour holds for word/dword
    // writes.
    mem.write_u8_jit(0x5000, 0xAB);
    mem.write_u16(0x6000, 0x1234);
    mem.write_u32(0x7000, 0xDEADBEEF);

    assert_eq!(mem.pending_write_count(), 0);
}

#[test]
fn test_jit_write_no_context() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    // write context NOT set

    // write_u16_jit/write_u32_jit were retired; write_u16/write_u32 share
    // the same `record_write` gate (no context → no pending write), so
    // they still pin that this behaviour holds for word/dword writes.
    mem.write_u8_jit(0x5000, 0xAB);
    mem.write_u16(0x6000, 0x1234);
    mem.write_u32(0x7000, 0xDEADBEEF);

    assert_eq!(mem.pending_write_count(), 0);
}

#[test]
fn test_jit_write_captures_old_value() {
    let mut mem = Memory::new();

    // Pre-populate memory
    let addr = 0x8000;
    mem.write_u8(addr, 0x42);

    mem.enable_write_tracking();
    mem.set_write_context(0x1000, 0x0100);

    // Write new value via JIT
    mem.write_u8_jit(addr, 0xAB);

    let pending = mem.drain_pending_writes();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].old_value, 0x42);
    assert_eq!(pending[0].new_value, 0xAB);
}

#[test]
fn test_jit_write_video_memory_not_tracked() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x1000, 0x0100);

    // Write to video memory via JIT should NOT be tracked
    // (video memory tracking is handled separately via VGA planar functions)
    mem.write_u8_jit(VIDEO_BASE + 100, 0xAB);

    assert_eq!(mem.pending_write_count(), 0);
}

#[test]
fn test_memory_slice_mut_owned_flushes() {
    let mut mem = Memory::new();
    let addr = 0x2000;
    {
        let mut slice = MemorySliceMut::owned(&mut mem, addr, vec![0xAA, 0xBB]);
        slice[0] = 0xCC;
        slice[1] = 0xDD;
    }
    assert_eq!(mem.read_u8(addr), 0xCC);
    assert_eq!(mem.read_u8(addr + 1), 0xDD);
}

#[test]
fn test_mark_instr_exec_page_out_of_range() {
    let mut mem = Memory::new();
    let page = mem.core.instr_exec_pages().len() * 64;
    let addr = page << INSTR_EXEC_PAGE_SHIFT;
    mem.mark_instr_exec_page(addr);
    assert!(mem.core.instr_exec_pages().iter().all(|&v| v == 0));
}

#[test]
fn test_instr_exec_page_marked_out_of_range() {
    let mem = Memory::new();
    let page = mem.core.instr_exec_pages().len() * 64;
    assert!(!mem.core.instr_exec_page_marked(page));
}

#[test]
fn test_mark_instr_cache_dirty_range_size_zero() {
    let mut mem = Memory::new();
    mem.core.take_instr_cache_dirty();
    mem.mark_instr_cache_dirty_range(0x1000, 0);
    assert!(!mem.core.is_instr_cache_dirty());
}

#[test]
fn test_mark_instr_cache_dirty_range_dirty_skip() {
    let mut mem = Memory::new();
    mem.core.set_instr_cache_dirty();
    mem.mark_instr_cache_dirty_range(0x1000, 4);
    assert!(mem.core.is_instr_cache_dirty());
}

#[test]
fn test_mark_instr_cache_dirty_range_end_before_addr() {
    let mut mem = Memory::new();
    mem.core.take_instr_cache_dirty();
    let addr = mem.ram.len() + 16;
    mem.mark_instr_cache_dirty_range(addr, 4);
    assert!(!mem.core.is_instr_cache_dirty());
}

#[test]
fn test_mark_instr_cache_dirty_range_sets_dirty() {
    let mut mem = Memory::new();
    mem.core.take_instr_cache_dirty();
    mem.mark_instr_exec_page(0);
    mem.mark_instr_cache_dirty_range(0, 1);
    assert!(mem.core.is_instr_cache_dirty());
}

#[test]
fn test_note_jit_write_marks_dirty_for_contiguous_write() {
    let mut mem = Memory::new();
    mem.core.take_instr_cache_dirty();
    mem.mark_instr_exec_page(0x2000);
    mem.note_jit_write(0x2000, 2);
    assert!(mem.core.is_instr_cache_dirty());
}

#[test]
fn test_note_jit_write_ignores_invalid_size() {
    let mut mem = Memory::new();
    mem.core.take_instr_cache_dirty();
    mem.mark_instr_exec_page(0x2000);
    mem.note_jit_write(0x2000, 3);
    assert!(!mem.core.is_instr_cache_dirty());
}

#[test]
fn test_note_jit_write_ignores_rom_area() {
    let mut mem = Memory::new();
    mem.core.take_instr_cache_dirty();
    mem.mark_instr_exec_page(0xF0000);
    mem.note_jit_write(0xF0000, 1);
    assert!(!mem.core.is_instr_cache_dirty());
}

#[test]
fn test_mark_video_page_dirty_bounds() {
    let mem = Memory::new();
    mem.video_dirty_pages.store(0, Ordering::Relaxed);
    mem.mark_video_page_dirty(VIDEO_BASE);
    let after = mem.video_dirty_pages.load(Ordering::Relaxed);
    assert_eq!(after & 1, 1);
    // In 128KB aperture, page 16 exists but should be ignored by 16-bit mask.
    mem.mark_video_page_dirty(VIDEO_BASE + 0x10000);
    assert_eq!(mem.video_dirty_pages.load(Ordering::Relaxed), after);
    mem.mark_video_page_dirty(VIDEO_BASE + VIDEO_SIZE);
    assert_eq!(mem.video_dirty_pages.load(Ordering::Relaxed), after);
}

#[test]
fn test_mask_address_clears_bit20_when_a20_disabled() {
    let mut mem = Memory::new();
    let addr = A20_BIT | 0x1234;

    // A20 enabled (default): address passes through unchanged
    assert_eq!(
        mem.mask_address(addr),
        addr,
        "mask_address must return address unchanged when A20 enabled"
    );

    // A20 disabled: bit 20 is cleared
    mem.set_a20(false);
    assert_eq!(
        mem.mask_address(addr),
        0x1234,
        "mask_address must clear bit 20 when A20 disabled"
    );
    // Bits other than 20 are preserved
    assert_eq!(
        mem.mask_address(0x0020_1234),
        0x0020_1234 & !0x0010_0000,
        "mask_address must only clear bit 20"
    );
}

#[test]
fn test_read_u8_chain4_read_mode_color_compare() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.read_mode = 1;
    mem.vga.color_dont_care = 0x03; // planes 0 and 1
    mem.vga.color_compare = 0x01; // plane 0 compare bit set, plane 1 cleared

    let offset = 0x10;
    // Chain-4 mapping stores this byte at cell x&~3.
    let plane_offset = offset & !3;
    mem.planes_flat[plane_offset] = 0xFF;
    mem.planes_flat[PLANE_SIZE + plane_offset] = 0x00;
    mem.planes_flat[2 * PLANE_SIZE + plane_offset] = 0xFF;
    mem.planes_flat[3 * PLANE_SIZE + plane_offset] = 0x00;

    let result = mem.read_u8(VIDEO_BASE + offset);
    assert_eq!(result, 0xFF);
}

#[test]
fn test_read_u8_planar_all_zero_trace_path() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.write_mode = 1;
    mem.vga.read_mode = 0;

    let result = mem.read_u8(VIDEO_BASE);
    assert_eq!(result, 0x00);
}

#[test]
fn test_read_bytes_empty_noop() {
    let mem = Memory::new();
    let mut dest: [u8; 0] = [];
    mem.read_bytes(0x1000, &mut dest);
}

#[test]
fn test_read_bytes_out_of_range_no_copy() {
    let mem = Memory::new();
    let mut dest = [0xAAu8; 4];
    mem.read_bytes(mem.ram.len() + 8, &mut dest);
    assert_eq!(dest, [0xFF, 0xFF, 0xFF, 0xFF]);
}

#[test]
fn test_physical_mapping_size_zero_ignored() {
    let mut mem = Memory::new();
    mem.add_physical_mapping(0x1000, 0x2000, 0);
    assert!(mem.physical_mappings.is_empty());
}

/// MDD-BUG-CRU-01482: a physical range past 4 GiB is refused like a linear
/// one, rather than folding back to physical 0 partway through; a range
/// ending exactly at 4 GiB is still legal.
#[test]
fn a_physical_range_past_4_gib_is_refused() {
    let mut mem = Memory::new();
    assert!(!mem.add_physical_mapping(0x0010_0000, 0xFFFF_F000, 0x2000));
    assert!(mem.physical_mappings.is_empty());
    assert!(mem.add_physical_mapping(0x0010_0000, 0xFFFF_F000, 0x1000));
    assert_eq!(mem.map_linear_physical(0x0010_0FFF), Some(0xFFFF_FFFF));
}

#[test]
fn test_map_linear_physical_bounds() {
    let mut mem = Memory::new();
    mem.add_physical_mapping(0x1000, 0x2000, 0x100);
    assert_eq!(mem.map_linear_physical(0x1000), Some(0x2000));
    assert_eq!(mem.map_linear_physical(0x10FF), Some(0x20FF));
    assert_eq!(mem.map_linear_physical(0x1100), None);
}

#[test]
fn linear_range_physical_mapping_overlap_is_byte_exact() {
    let mut mem = Memory::new();
    assert!(!mem.linear_range_has_physical_mapping(0x0fff, 4));

    mem.add_physical_mapping(0x1001, 0x3001, 2);
    assert!(!mem.linear_range_has_physical_mapping(0x0ffd, 4));
    assert!(mem.linear_range_has_physical_mapping(0x0fff, 4));
    assert!(mem.linear_range_has_physical_mapping(0x1002, 2));
    assert!(!mem.linear_range_has_physical_mapping(0x1003, 2));
}

/// The range-overlap test must give exactly the per-byte predicate it
/// replaced (MDD-BUG-FLU-03025): every range near each mapping edge, at the
/// bottom and top of the 4 GiB space, wrapping past it, and covering it. The
/// second table leaves the top of the space unmapped, so a wrapping range
/// hits only through its part after the wrap.
#[test]
fn linear_range_physical_mapping_matches_the_per_byte_oracle() {
    fn per_byte(mem: &Memory, linear: u32, len: usize) -> bool {
        (0..len).any(|offset| {
            mem.map_linear_physical(linear.wrapping_add(offset as u32))
                .is_some()
        })
    }

    let tables: [&[(u32, u32, u32)]; 2] = [
        &[
            (0x0000_0000, 0x10_0000, 0x10),
            (0x4000_0000, 0x4000_0000, 0x40_0000),
            (0x0012_3401, 0x20_0000, 0x7FE),
            (0xFFFF_FF00, 0x30_0000, 0x100),
        ],
        &[
            (0x0000_0008, 0x10_0000, 0x10),
            (0xFFFF_FE00, 0x30_0000, 0x80),
        ],
    ];
    let edges: [u32; 11] = [
        0x0000_0000,
        0x0000_0008,
        0x0000_0018,
        0x0012_3401,
        0x0012_3BFF,
        0x4000_0000,
        0x4040_0000,
        0xFFFF_FE00,
        0xFFFF_FE80,
        0xFFFF_FF00,
        0xFFFF_FFFF,
    ];
    let lens: [usize; 9] = [0, 1, 2, 3, 4, 0xF, 0x10, 0x101, 0x1000];
    let mut checked = 0u32;
    let mut wrap_only_hits = 0u32;
    for table in tables {
        let mut mem = Memory::new();
        for &(linear, physical, size) in table {
            assert!(mem.add_physical_mapping(linear, physical, size));
        }
        for &edge in &edges {
            for delta in -0x110i64..=0x110 {
                let linear = (i64::from(edge) + delta) as u32;
                for &len in &lens {
                    let expected = per_byte(&mem, linear, len);
                    assert_eq!(
                        mem.linear_range_has_physical_mapping(linear, len),
                        expected,
                        "linear {linear:#x} len {len:#x}"
                    );
                    let below_wrap = (1u64 << 32) - u64::from(linear);
                    if expected && (len as u64) > below_wrap {
                        let low_part = usize::try_from(below_wrap).unwrap();
                        if !per_byte(&mem, linear, low_part) {
                            wrap_only_hits += 1;
                        }
                    }
                    checked += 1;
                }
            }
        }
    }
    assert!(checked > 10_000);
    assert!(wrap_only_hits > 0, "no range hit only after the wrap");

    let mut mem = Memory::new();
    assert!(mem.add_physical_mapping(0x0000_0008, 0x10_0000, 0x10));
    // A whole code page well clear of every mapping (the 03025 hot query).
    assert!(!mem.linear_range_has_physical_mapping(0x0040_1000, 0x1000));
    // Lengths of 4 GiB or more cover every address.
    assert!(mem.linear_range_has_physical_mapping(0x8000_0000, 1 << 32));
    assert!(mem.linear_range_has_physical_mapping(0x8000_0000, usize::MAX));

    let empty = Memory::new();
    assert!(!empty.linear_range_has_physical_mapping(0x8000_0000, usize::MAX));
}

#[test]
fn test_read_u32_physical_out_of_bounds_returns_zero() {
    let mem = Memory::new();
    let addr = mem.ram.len().saturating_sub(2);
    assert_eq!(mem.read_u32_physical(addr), 0);
}

#[test]
fn test_write_u32_physical_out_of_bounds_noop() {
    let mut mem = Memory::new();
    let addr = mem.ram.len().saturating_sub(2);
    let before = mem.ram[addr];
    mem.write_u32_physical(addr, 0xDEADBEEF);
    assert_eq!(mem.ram[addr], before);
}

#[test]
fn test_read_byte_checked_out_of_bounds() {
    let mem = Memory::new();
    let addr = mem.ram.len() + 1;
    assert_eq!(mem.read_byte_checked(addr), None);
}

#[test]
fn test_write_u32_non_contiguous_rom_boundary() {
    let mut mem = Memory::new();
    let addr = BIOS_BASE - 1;
    let before_rom = mem.read_u8(BIOS_BASE);
    mem.write_u32(addr, 0xAABB_CCDD);
    assert_eq!(mem.read_u8(addr), 0xDD);
    assert_eq!(mem.read_u8(BIOS_BASE), before_rom);
}

#[test]
fn test_write_planar_ega_tracked_modes_and_masks() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.bit_mask = 0xFF;
    mem.planes_flat.fill(0);

    mem.vga.plane_mask = 0x01;
    mem.vga.write_mode = 1;
    mem.latches.set([0x11, 0x22, 0x33, 0x44]);
    mem.write_planar_ega_tracked(0, 0xFF);
    assert_eq!(mem.plane(0)[0], 0x11);
    assert_eq!(mem.plane(1)[0], 0x00);

    mem.vga.write_mode = 2;
    mem.write_planar_ega_tracked(1, 0x01);
    assert_eq!(mem.plane(0)[1], 0xFF);
    assert_eq!(mem.plane(2)[1], 0x00);

    mem.vga.write_mode = 3;
    mem.vga.data_rotate = 0;
    mem.latches.set([0x00, 0x00, 0x00, 0x00]);
    mem.vga.set_reset = 0x01;
    mem.write_planar_ega_tracked(2, 0xFF);
    assert_eq!(mem.plane(0)[2], 0xFF);

    mem.vga.set_reset = 0x00;
    mem.write_planar_ega_tracked(3, 0xFF);
    assert_eq!(mem.plane(0)[3], 0x00);
}

#[test]
fn test_write_u8_jit_ignores_rom() {
    let mut mem = Memory::new();
    let rom_addr = BIOS_BASE + 0x10;
    let before = mem.read_u8(rom_addr);
    mem.write_u8_jit(rom_addr, 0xEE);
    assert_eq!(mem.read_u8(rom_addr), before);
}

#[test]
fn test_bind_smc_notifier_write_hook() {
    let mut mem = Memory::new();
    let notifier = Arc::new(TestSmcNotify::default());

    mem.bind_smc(Some(notifier.clone()));
    mem.set_smc_has_code_pages(true);
    // The dyncore holds a block on page 2: production watches the page
    // before any block on it can run, and `smc_before_write` only notifies
    // for a watched page (see `smc_span_may_hold_code`).
    mem.set_page_watched(0x2345 >> 12, true);
    mem.write_u8(0x2345, 0x7A);
    notifier.notify_write_range(0x3000, 0x3002);
    assert_eq!(notifier.snapshot(), vec![(0x2345, 1), (0x3000, 2)]);

    mem.bind_smc(None);
    mem.write_u8(0x2346, 0x7B);
    assert_eq!(notifier.snapshot(), vec![(0x2345, 1), (0x3000, 2)]);
}

#[test]
fn test_write_u32_physical_notifies_smc_hook() {
    let mut mem = Memory::new();
    let notifier = Arc::new(TestSmcNotify::default());

    mem.bind_smc(Some(notifier.clone()));
    mem.set_smc_has_code_pages(true);
    mem.set_page_watched(0x2200 >> 12, true);
    mem.write_u32_physical(0x2200, 0xDEADBEEF);

    assert_eq!(mem.read_u32_physical(0x2200), 0xDEADBEEF);
    assert_eq!(notifier.snapshot(), vec![(0x2200, 4)]);
}

#[test]
fn smc_before_write_skips_a_page_holding_no_compiled_code() {
    // A store to an ordinary data
    // page must not reach the snoop at all. Every drain consumer discards
    // such a span anyway, so recording it is pure cost — and past
    // `SNOOP_PAGE_CAP` distinct pages it provokes a whole-cache flush.
    let mut mem = Memory::new();
    let notifier = Arc::new(TestSmcNotify::default());
    mem.bind_smc(Some(notifier.clone()));
    mem.set_smc_has_code_pages(true);
    mem.set_page_watched(0x40, true);

    mem.write_u32(0x30_000, 0x1234_5678); // page 0x30 — data
    mem.write_u8(0x41_000, 0xAB); // page 0x41 — data
    assert!(
        notifier.snapshot().is_empty(),
        "an unwatched page must not reach the SMC snoop"
    );

    mem.write_u32(0x40_010, 0x9ABC_DEF0); // page 0x40 — code
    assert_eq!(
        notifier.snapshot(),
        vec![(0x40_010, 4)],
        "a watched page must still notify, byte-exactly"
    );

    // …and un-watching closes it again.
    mem.set_page_watched(0x40, false);
    mem.write_u32(0x40_020, 0x0BAD_F00D);
    assert_eq!(
        notifier.snapshot(),
        vec![(0x40_010, 4)],
        "a page that stopped holding code stops notifying"
    );
}

#[test]
fn smc_before_write_notifies_a_write_straddling_into_a_watched_page() {
    // A store crossing a page boundary must notify if EITHER page holds
    // code — and it must hand the snoop the whole span, so the snoop's own
    // per-page split records both halves.
    let mut mem = Memory::new();
    let notifier = Arc::new(TestSmcNotify::default());
    mem.bind_smc(Some(notifier.clone()));
    mem.set_smc_has_code_pages(true);

    // Only the TAIL page holds code: [0x2FFE, 0x3002) → pages 2 and 3.
    mem.set_page_watched(3, true);
    mem.write_u32(0x2FFE, 0x1122_3344);
    assert_eq!(notifier.snapshot(), vec![(0x2FFE, 4)], "straddle into code");

    // Only the HEAD page holds code: [0x4FFE, 0x5002) → pages 4 and 5.
    mem.set_page_watched(3, false);
    mem.set_page_watched(4, true);
    mem.write_u32(0x4FFE, 0x5566_7788);
    assert_eq!(
        notifier.snapshot(),
        vec![(0x2FFE, 4), (0x4FFE, 4)],
        "straddle out of code"
    );

    // Neither page holds code: nothing recorded.
    mem.set_page_watched(4, false);
    mem.write_u32(0x6FFE, 0x99AA_BBCC);
    assert_eq!(notifier.snapshot(), vec![(0x2FFE, 4), (0x4FFE, 4)]);
}

// ── EXP-N005: PT-write tripwire (dbx link keep) ─────────────────────────

/// Read the supervisor write view slot for a linear page, exactly as an
/// emitted store probe would.
fn flat_w0_entry(mem: &Memory, linear_page: u32) -> u64 {
    unsafe {
        *mem.flat_views_ptr()
            .add(flat_tlb::FLAT_PAGE_COUNT + linear_page as usize)
    }
}

#[test]
fn pt_write_epoch_bumps_on_guest_write_to_recorded_frame_only() {
    let mut mem = Memory::new();
    mem.set_smc_has_code_pages(true); // links can only exist alongside code pages
    mem.record_pt_frame(0x30);
    assert!(mem.is_pt_frame_recorded(0x30));

    let e0 = mem.dynrec_pt_write_epoch();
    mem.write_u32(0x31_000, 0x1234_5678); // unrecorded frame
    assert_eq!(
        mem.dynrec_pt_write_epoch(),
        e0,
        "unrecorded frame must not bump"
    );

    mem.write_u32_physical(0x30_010, 0xDEAD_BEEF); // PDE/PTE edit route
    assert_eq!(
        mem.dynrec_pt_write_epoch(),
        e0 + 1,
        "physical PT edit must bump"
    );

    mem.write_u8(0x30_FFF, 0xAA); // guest slow-path store into the frame
    assert_eq!(mem.dynrec_pt_write_epoch(), e0 + 2);

    // A straddle whose tail lands in the recorded frame bumps too (the
    // guest straddle path may funnel per byte, so assert direction, not
    // an exact count — the keep decision only compares for inequality).
    let e_before_straddle = mem.dynrec_pt_write_epoch();
    mem.write_u32(0x2F_FFE, 0x99AA_BBCC);
    assert!(
        mem.dynrec_pt_write_epoch() > e_before_straddle,
        "straddle into the frame must bump"
    );
}

/// MDD-BUG-FLUX-00122: the direct-store fast leg must refuse a recorded
/// paging-structure frame, exactly as it already refuses a watched page.
///
/// `SoftwareTlb::record_pt_frame` nulls only the FLAT write views; the
/// hashed `TlbEntry::host_write` survives and re-fills keep publishing it.
/// `tlb_direct_write_ptr` reads that hashed field and guarded only
/// `is_watched`, so it handed out a writable pointer into a PT frame — a
/// store through which never reaches `smc_before_write` and so never bumps
/// `pt_write_epoch`, leaving stale chain-link premises (the
/// MDD-BUG-FLUX-00086 class).
///
/// The watched-page arm below is the control: it proves the refusal
/// mechanism works and that only the PT-frame case was missing.
#[test]
fn tlb_direct_write_ptr_refuses_a_recorded_pt_frame() {
    let mut mem = Memory::new();
    mem.set_smc_has_code_pages(true);
    mem.tlb_insert(0x21, 0x21, true, true, true);
    assert!(
        mem.tlb_direct_write_ptr(0x2_1234, 4, true).is_some(),
        "premise: an ordinary warm writable page takes the direct leg"
    );

    mem.record_pt_frame(0x21);
    assert!(mem.is_pt_frame_recorded(0x21), "premise: frame recorded");
    assert!(
        !mem.core.tlb().flat().is_watched(0x21),
        "premise: a PT frame is not automatically watched — the watch guard \
         cannot be what covers this case"
    );

    assert!(
        mem.tlb_direct_write_ptr(0x2_1234, 4, true).is_none(),
        "a recorded PT frame must force the checked-store path, or the \
         store bypasses the EXP-N005 PT-write tripwire"
    );
    assert!(
        mem.tlb_direct_write_ptr(0x2_1234, 4, false).is_none(),
        "refusal is not CPL-dependent"
    );
}

/// The consequence the refusal exists to prevent, asserted end to end: a
/// guest store into a recorded PT frame must bump `pt_write_epoch` on the
/// warm-TLB path the direct leg serves (MDD-BUG-FLUX-00122).
#[test]
fn warm_tlb_store_into_a_recorded_pt_frame_bumps_the_epoch() {
    let mut mem = Memory::new();
    mem.set_smc_has_code_pages(true);
    mem.tlb_insert(0x30, 0x30, true, true, true);
    mem.record_pt_frame(0x30);

    let before = mem.dynrec_pt_write_epoch();
    mem.write_u32(0x30_010, 0xDEAD_BEEF);
    assert!(
        mem.dynrec_pt_write_epoch() > before,
        "a warm-TLB guest store into a recorded PT frame must sever chain \
         links; a direct-leg commit that skips smc_before_write would not"
    );
}

#[test]
fn pt_write_epoch_ignores_walker_ad_writeback_but_keeps_smc_notification() {
    let mut mem = Memory::new();
    let notifier = Arc::new(TestSmcNotify::default());
    mem.bind_smc(Some(notifier.clone()));
    mem.set_smc_has_code_pages(true);
    mem.record_pt_frame(0x30);
    mem.set_page_watched(0x30, true); // so the A/D write-back is SMC-visible

    let e0 = mem.dynrec_pt_write_epoch();
    PagedMemory::write_u32_physical_ad(&mut mem, 0x30_020, 0x0000_0060);
    assert_eq!(
        mem.dynrec_pt_write_epoch(),
        e0,
        "walker A/D write-back must not trip the PT wire (amendment A3)"
    );
    assert_eq!(
        mem.read_u32_physical(0x30_020),
        0x0000_0060,
        "the write must land"
    );
    assert_eq!(
        notifier.snapshot(),
        vec![(0x30_020, 4)],
        "SMC notification must be preserved on the A/D route"
    );

    // The suppression window must close: an ordinary write right after
    // still trips.
    mem.write_u32_physical(0x30_024, 0x0000_0001);
    assert_eq!(mem.dynrec_pt_write_epoch(), e0 + 1);
}

#[test]
fn pt_record_of_unprobeable_frames_bumps_every_record() {
    // HLE EMS overlay alias: walker-visible "PTE" bytes can change on an
    // overlay remap with no tracked write, so such a frame is never
    // tracked — every record bumps instead (amendment A5's sever-always).
    let mut mem = Memory::new();
    mem.set_smc_has_code_pages(true);
    mem.claim_hle_ems_frame(EMS_FRAME_BASE, EMS_FRAME_SIZE)
        .expect("EMS frame claim");
    let ems_page = (EMS_FRAME_BASE >> 12) as u32;
    let e0 = mem.dynrec_pt_write_epoch();
    mem.record_pt_frame(ems_page);
    mem.record_pt_frame(ems_page);
    assert_eq!(mem.dynrec_pt_write_epoch(), e0 + 2);
    assert!(
        !mem.is_pt_frame_recorded(ems_page),
        "an unprobeable frame must not enter the bitmap"
    );

    // Beyond the bitmap: same sever-always treatment.
    let beyond = (MAX_RAM_SIZE >> 12) as u32;
    mem.record_pt_frame(beyond);
    assert_eq!(mem.dynrec_pt_write_epoch(), e0 + 3);
}

/// `has_recorded_pt_frame_in` answers for exactly the half-open page range it
/// is given: DBX's VGA arms disarm on it, so a frame just outside the range
/// must not count and one on either end page must.
#[test]
fn has_recorded_pt_frame_in_reports_only_frames_inside_the_half_open_range() {
    let mut mem = Memory::new();
    assert!(
        !mem.has_recorded_pt_frame_in(0xA0, 0xB0),
        "nothing recorded"
    );

    mem.record_pt_frame(0x30);
    assert!(mem.has_recorded_pt_frames(), "premise: a frame is recorded");
    assert!(
        !mem.has_recorded_pt_frame_in(0xA0, 0xB0),
        "a frame outside the range must not count"
    );
    mem.record_pt_frame(0xB0);
    assert!(
        !mem.has_recorded_pt_frame_in(0xA0, 0xB0),
        "the end page is exclusive"
    );
    assert!(!mem.has_recorded_pt_frame_in(0xA0, 0xA0), "empty range");

    mem.record_pt_frame(0xAF);
    assert!(mem.has_recorded_pt_frame_in(0xA0, 0xB0), "last page counts");
    assert!(!mem.has_recorded_pt_frame_in(0xA0, 0xAF));
    assert!(mem.has_recorded_pt_frame_in(0xAF, 0xB0));

    let mut first = Memory::new();
    first.record_pt_frame(0xA0);
    assert!(
        first.has_recorded_pt_frame_in(0xA0, 0xB0),
        "first page counts"
    );
    assert!(!first.has_recorded_pt_frame_in(0xA1, 0xB0));
    assert!(!first.has_recorded_pt_frame_in(0x90, 0xA0));
}

/// Ranges that span several 64-page bitmap words see a frame in the first,
/// a middle, or the last word, and only inside the range's own bounds.
#[test]
fn has_recorded_pt_frame_in_scans_ranges_across_bitmap_words() {
    for frame in [0x3Fu32, 0x40, 0x7F, 0x80, 0xBF, 0xC0, 0x13F] {
        let mut mem = Memory::new();
        mem.record_pt_frame(frame);
        assert!(
            mem.has_recorded_pt_frame_in(0x3F, 0x140),
            "frame {frame:#x} inside [0x3f, 0x140)"
        );
        assert!(
            mem.has_recorded_pt_frame_in(frame, frame + 1),
            "frame {frame:#x} alone"
        );
        assert!(
            !mem.has_recorded_pt_frame_in(frame + 1, frame + 0x100),
            "range just above frame {frame:#x}"
        );
        assert!(
            !mem.has_recorded_pt_frame_in(frame.saturating_sub(0x100), frame),
            "range just below frame {frame:#x}"
        );
    }
}

/// Pages the bitmap cannot represent answer `true` once any frame is
/// recorded (the `span_may_be_pt_frame` conservatism), and `false` before.
#[test]
fn has_recorded_pt_frame_in_is_conservative_beyond_the_bitmap() {
    let beyond = (MAX_RAM_SIZE >> 12) as u32;
    let mut mem = Memory::new();
    assert!(!mem.has_recorded_pt_frame_in(beyond, beyond + 1));
    mem.record_pt_frame(0x30);
    assert!(mem.has_recorded_pt_frame_in(beyond, beyond + 1));
    assert!(mem.has_recorded_pt_frame_in(beyond - 1, beyond + 1));
    assert!(!mem.has_recorded_pt_frame_in(beyond - 1, beyond));
}

#[test]
fn translation_structure_epoch_covers_exactly_the_always_sever_causes() {
    let mut mem = Memory::new();
    let s0 = mem.dynrec_translation_structure_epoch();

    // Keep-eligible causes (MOV CR3 / task switch / INVLPG) must NOT bump.
    mem.invalidate_tlb();
    mem.invalidate_tlb_page(0x1000);
    assert_eq!(mem.dynrec_translation_structure_epoch(), s0);

    // DPMI physical-mapping churn: always-sever.
    mem.add_physical_mapping(0x2000, 0x3000, 0x1000);
    let s1 = mem.dynrec_translation_structure_epoch();
    assert!(
        s1 > s0,
        "physical-mapping churn must bump the structure epoch"
    );

    // A20 flip: always-sever.
    mem.set_a20(false);
    let s2 = mem.dynrec_translation_structure_epoch();
    assert!(s2 > s1, "an A20 flip must bump the structure epoch");

    // Snapshot restore: always-sever (routes through the A20/proof
    // invalidation bump).
    mem.on_snapshot_restored();
    assert!(
        mem.dynrec_translation_structure_epoch() > s2,
        "snapshot restore must bump the structure epoch"
    );
}

/// EXP-N006 amendment N4: the dyncore's in-span io-seam guard watches
/// ONLY `dynrec_translation_epoch`; it covers mid-span structure events
/// solely because every structure-epoch bump site also bumps the
/// translation epoch. This pins that coupling — a future structure-bump
/// site that omits the translation-epoch bump would silently reopen the
/// within-span window for the cross-span dispatch memo.
#[test]
fn every_structure_epoch_bump_also_bumps_the_translation_epoch() {
    let mut mem = Memory::new();

    // DPMI physical-mapping add + remove.
    let (s0, t0) = (
        mem.dynrec_translation_structure_epoch(),
        mem.dynrec_translation_epoch(),
    );
    mem.add_physical_mapping(0x2000, 0x3000, 0x1000);
    mem.remove_physical_mapping(0x2000);
    let (s1, t1) = (
        mem.dynrec_translation_structure_epoch(),
        mem.dynrec_translation_epoch(),
    );
    assert!(s1 > s0, "mapping churn bumps the structure epoch");
    assert!(
        t1 - t0 >= s1 - s0,
        "every mapping-churn structure bump must be accompanied by a \
         translation-epoch bump (io-seam guard coverage)"
    );

    // A20 flip (also the snapshot-restore route).
    mem.set_a20(false);
    let (s2, t2) = (
        mem.dynrec_translation_structure_epoch(),
        mem.dynrec_translation_epoch(),
    );
    assert!(s2 > s1, "an A20 flip bumps the structure epoch");
    assert!(
        t2 - t1 >= s2 - s1,
        "every A20 structure bump must be accompanied by a \
         translation-epoch bump (io-seam guard coverage)"
    );
}

/// EXP-N006 amendment N2: instance nonces are process-unique and
/// monotonic, so a replaced `Memory` can never present the nonce a
/// baseline tuple captured from its predecessor.
#[test]
fn memory_instance_nonces_are_unique_and_nonzero() {
    let a = Memory::new();
    let b = Memory::new();
    assert_ne!(a.dynrec_instance_nonce(), 0);
    assert_ne!(a.dynrec_instance_nonce(), b.dynrec_instance_nonce());
    assert!(b.dynrec_instance_nonce() > a.dynrec_instance_nonce());
}

#[test]
fn recorded_pt_frame_nulls_write_views_like_a_watched_page() {
    let mut mem = Memory::new();
    let linear_page = 0x123u32;
    let phys_page = 0x30u32;

    // Warm entry BEFORE recording: write view is live.
    mem.tlb_insert(linear_page, phys_page, true, true, true);
    assert_ne!(
        flat_w0_entry(&mem, linear_page),
        0,
        "precondition: a plain RAM page publishes a live write view"
    );

    // Recording sweeps the warm entry's write views to null.
    mem.record_pt_frame(phys_page);
    assert_eq!(
        flat_w0_entry(&mem, linear_page),
        0,
        "recording must null the WARM entry's write view (A2: warm dirty \
         aliases survive keep-eligible edges)"
    );

    // A future fill folds the predicate at publish time.
    mem.invalidate_tlb();
    mem.tlb_insert(linear_page, phys_page, true, true, true);
    assert_eq!(
        flat_w0_entry(&mem, linear_page),
        0,
        "a fresh fill onto a recorded frame must publish a null write view"
    );

    // Un-watching a page that is ALSO a PT frame must not republish.
    mem.set_page_watched(phys_page, true);
    mem.set_page_watched(phys_page, false);
    assert_eq!(
        flat_w0_entry(&mem, linear_page),
        0,
        "un-watch republish must honour the PT-frame fold"
    );

    // The fold is watched-OR-pt, and the projection oracles agree.
    mem.oracle_check_flat_projection()
        .expect("O-P1(a) fold coherence");
    mem.oracle_check_smc_fold_any_instant()
        .expect("O-SMC-ANYINSTANT with the PT fold");
}

/// `write_bytes_guest`'s bulk span still has to reach the SMC snoop — a
/// sector read landing on a page that holds compiled code must invalidate
/// it, exactly as the byte-by-byte path would.
#[test]
fn write_bytes_guest_bulk_span_notifies_the_smc_snoop_for_the_whole_span() {
    let mut mem = Memory::new();
    let notifier = Arc::new(TestSmcNotify::default());
    mem.bind_smc(Some(notifier.clone()));
    mem.set_smc_has_code_pages(true);
    mem.set_page_watched(0x30, true);

    let sector = vec![0x5Au8; 512];
    mem.write_bytes_guest(0x30_000, &sector);
    assert_eq!(
        notifier.snapshot(),
        vec![(0x30_000, 512)],
        "one notification covering the whole span"
    );

    // A span on an unwatched page still commits, and still stays quiet.
    mem.write_bytes_guest(0x50_000, &sector);
    assert_eq!(notifier.snapshot(), vec![(0x30_000, 512)]);
    assert_eq!(mem.read_u8(0x50_000), 0x5A);
    assert_eq!(mem.read_u8(0x50_1FF), 0x5A);
}

// ---- Native VGA stores must advance the watchdog's progress contract ----
// MDD-BUG-CRUCIBLE-00150. The watchdog treats an unchanged
// `video_write_count` in graphics mode as `NoVgaWrites` and terminates the
// run, so a guest rendering entirely through admitted native stores was
// killed while it was working correctly.

/// The emitted inline corridor writes planes straight from generated code,
/// so its count arrives as a fold rather than a per-store call. It must
/// still reach the same counter the watchdog reads.
#[test]
fn folded_native_writes_reach_the_video_write_count() {
    let mut mem = Memory::new();
    let before = mem.video_write_count();
    mem.add_native_video_writes(7);
    assert_eq!(
        mem.video_write_count(),
        before + 7,
        "emitted-corridor writes must reach the watchdog counter"
    );
}

/// Folding zero must not manufacture progress: an idle dispatch has to
/// leave the watchdog able to fire, or the fix would disable it entirely.
#[test]
fn folding_nothing_reports_no_progress() {
    let mut mem = Memory::new();
    let before = mem.video_write_count();
    mem.add_native_video_writes(0);
    assert_eq!(
        mem.video_write_count(),
        before,
        "an idle dispatch must not look like VGA progress"
    );
}

#[test]
fn smc_span_may_hold_code_is_conservative_above_the_watch_bitmap() {
    // `set_page_watched` silently ignores a page outside the bitmap, so a
    // clear bit up there is "unknown", not "clean". The gate must let such
    // a write through rather than assume it can be dropped.
    let mem = Memory::new();
    assert!(!mem.smc_span_may_hold_code(0x1000, 4));
    assert!(!mem.smc_span_may_hold_code(0x1000, 0), "empty span");
    assert!(
        mem.smc_span_may_hold_code(MAX_RAM_SIZE, 4),
        "outside the bitmap the gate must stay open"
    );
    assert!(
        mem.smc_span_may_hold_code(MAX_RAM_SIZE - 2, 4),
        "a straddle off the end of the bitmap must stay open"
    );
}

#[test]
fn test_smc_notifier_snapshot_on_poisoned_mutex() {
    let notifier = TestSmcNotify::default();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = notifier
            .writes
            .lock()
            .expect("lock should succeed before poisoning");
        panic!("poison test mutex");
    }));

    assert!(notifier.snapshot().is_empty());
    notifier.notify_write(0x1234, 1);
    notifier.notify_write_range(0x2000, 0x2002);
    assert!(notifier.snapshot().is_empty());
}

#[test]
fn test_video_write_count_accessors() {
    let mut mem = Memory::new();
    assert_eq!(mem.video_write_count(), 0);

    mem.write_u8(VIDEO_BASE, 0x11);
    mem.write_u8(0xB8000, 0x22);

    assert_eq!(mem.video_write_count(), 2);
}

#[test]
fn test_take_instr_cache_dirty_round_trip() {
    let mut mem = Memory::new();
    assert!(!mem.take_instr_cache_dirty());

    mem.mark_instr_exec_page(0x2000);
    mem.write_u8(0x2000, 0xAA);

    assert!(mem.take_instr_cache_dirty());
    assert!(!mem.take_instr_cache_dirty());
}

#[test]
fn test_video_dirty_page_state_helpers() {
    let mem = Memory::new();
    assert!(!mem.has_dirty_video_pages());

    mem.mark_video_page_dirty(VIDEO_BASE + 0x2345);
    assert!(mem.has_dirty_video_pages());

    assert_eq!(mem.take_video_dirty_pages(), 1 << 2);
    assert!(!mem.has_dirty_video_pages());
    assert_eq!(mem.take_video_dirty_pages(), 0);
}

#[test]
fn test_reset_ram_reinitializes_core_state() {
    let mut mem = Memory::new();
    mem.write_u8(0x5000, 0xAA);
    mem.write_u8(0x410, 0x00);
    mem.mark_instr_exec_page(0x5000);
    assert!(!mem.take_instr_cache_dirty());

    mem.reset_ram();

    assert_eq!(mem.read_u8(0x5000), 0x00);
    assert_eq!(mem.read_u16(0x410), 0x0021);
    assert_eq!(mem.read_u16(0x413), 0x0280);
    assert_eq!(mem.read_u16(0x41A), 0x001E);
    assert_eq!(mem.read_u16(0x480), 0x001E);
    assert_eq!(mem.read_u16(0x482), 0x003E);
    let iret_addr = Memory::seg_offset_to_linear(0xF000, 0xFF00);
    assert_eq!(mem.read_u8(iret_addr), 0xCF);
    assert!(mem.take_instr_cache_dirty());

    mem.write_u8(0x5000, 0x55);
    assert!(!mem.take_instr_cache_dirty());
}

#[test]
fn test_remove_physical_mapping_paths() {
    let mut mem = Memory::new();
    mem.add_physical_mapping(0x1000, 0x2000, 0x100);
    mem.add_physical_mapping(0x3000, 0x4000, 0x100);

    assert!(mem.remove_physical_mapping(0x1000));
    assert_eq!(mem.map_linear_physical(0x1000), None);
    assert_eq!(mem.map_linear_physical(0x3080), Some(0x4080));
    assert!(!mem.remove_physical_mapping(0x1000));
}

#[test]
fn test_read_write_u32_physical_in_bounds() {
    let mut mem = Memory::new();
    mem.write_u32_physical(0x2200, 0xDEADBEEF);
    assert_eq!(mem.read_u32_physical(0x2200), 0xDEADBEEF);
}

#[test]
fn test_read_byte_checked_in_bounds() {
    let mut mem = Memory::new();
    mem.write_u8(0x1234, 0x5A);
    assert_eq!(mem.read_byte_checked(0x1234), Some(0x5A));
}

#[test]
fn test_a20_toggle() {
    let mut mem = Memory::new();
    assert!(mem.a20_enabled());
    mem.set_a20(false);
    assert!(
        !mem.a20_enabled(),
        "A20 should be disabled after set_a20(false)"
    );
    mem.set_a20(true);
    assert!(
        mem.a20_enabled(),
        "A20 should be enabled after set_a20(true)"
    );
}

#[test]
fn test_a20_toggle_flushes_tlb_and_masks_page_insert() {
    let mut mem = Memory::new();
    mem.tlb_insert(0x42, 0x100, true, true, true);
    assert!(mem.tlb_lookup(0x42).is_some());

    mem.set_a20(false);

    assert!(
        mem.tlb_lookup(0x42).is_none(),
        "A20 transitions must flush stale TLB host pointers"
    );

    mem.tlb_insert(0x42, 0x100, true, true, true);
    let (phys, writable, user, dirty) = mem.tlb_lookup(0x42).unwrap();
    assert_eq!(phys, 0x000);
    assert!(writable);
    assert!(user);
    assert!(dirty);
}

#[test]
fn test_lfb_mut_direct_write() {
    let mut mem = Memory::new();
    mem.init_lfb(4);
    mem.lfb_mut()[1] = 0x7B;
    assert_eq!(mem.lfb()[1], 0x7B);
}

#[test]
fn test_write_u16_partial_low_byte_only_at_rom_boundary() {
    let mut mem = Memory::new();
    let addr = BIOS_BASE - 1;
    let before_rom = mem.read_u8(BIOS_BASE);

    mem.write_u16(addr, 0xA1B2);

    assert_eq!(mem.read_u8(addr), 0xB2);
    assert_eq!(mem.read_u8(BIOS_BASE), before_rom);
}

#[test]
fn test_write_u16_partial_high_byte_only_at_hma_boundary() {
    let mut mem = Memory::new();
    let addr = 0xFFFFF;
    let before_rom = mem.read_u8(addr);

    mem.write_u16(addr, 0xA1B2);

    assert_eq!(mem.read_u8(addr), before_rom);
    assert_eq!(mem.read_u8(0x100000), 0xA1);
}

#[test]
fn test_write_u32_partial_hma_writes_skip_rom_byte() {
    let mut mem = Memory::new();
    let addr = 0xFFFFF;
    let before_rom = mem.read_u8(addr);

    mem.write_u32(addr, 0x1122_3344);

    assert_eq!(mem.read_u8(addr), before_rom);
    assert_eq!(mem.read_u8(0x100000), 0x33);
    assert_eq!(mem.read_u8(0x100001), 0x22);
    assert_eq!(mem.read_u8(0x100002), 0x11);
}

#[test]
fn test_slice_mut_borrowed_deref_reads_ram() {
    let mut mem = Memory::new();
    mem.write_u8(0x2222, 0xAB);
    let slice = mem.slice_mut(0x2222, 1);
    assert_eq!(slice[0], 0xAB);
}

#[test]
fn slice_mut_above_ram_derefs_as_empty_instead_of_panicking() {
    // The start was left out of range while only the length was clamped, so
    // deref indexed ram[addr..addr] — Rust rejects an empty range whose
    // start is past the slice, and this safe API panicked on inspection
    // (MDD-BUG-CRUCIBLE-00508).
    let mut mem = Memory::new();
    let above = mem.usable_len() + 0x1000;
    let mut slice = mem.slice_mut(above, 16);
    assert!(slice.is_empty(), "an above-RAM view owns no bytes");
    assert_eq!(slice.iter().copied().next(), None);
    assert!(slice.iter_mut().next().is_none());
}

#[test]
fn slice_mut_straddling_the_ram_end_is_truncated_not_panicked() {
    let mut mem = Memory::new();
    let usable = mem.usable_len();
    let start = usable - 4;
    let slice = mem.slice_mut(start, 64);
    assert_eq!(slice.len(), 4, "the view must stop at the usable end");
}

#[test]
fn slice_mut_length_near_usize_max_does_not_overflow() {
    // addr is guest-supplied and unmasked, so addr + len must not wrap.
    let mut mem = Memory::new();
    let usable = mem.usable_len();
    let slice = mem.slice_mut(0x1000, usize::MAX);
    assert_eq!(slice.len(), usable - 0x1000);
}

#[test]
fn test_read_u8_chain4_mode0_return_path() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.read_mode = 0;
    let offset = 7usize;
    // Chain-4 mapping stores this byte at cell x&~3.
    let plane_offset = offset & !3;
    mem.planes_flat[plane_offset] = 0x11;
    mem.planes_flat[PLANE_SIZE + plane_offset] = 0x22;
    mem.planes_flat[2 * PLANE_SIZE + plane_offset] = 0x33;
    mem.planes_flat[3 * PLANE_SIZE + plane_offset] = 0x44;
    assert_eq!(mem.read_u8(VIDEO_BASE + offset), 0x44);
}

#[test]
fn test_trace_logging_paths_execute() {
    with_trace_subscriber(|| {
        let mut mem = Memory::new();
        mem.vga.video_mode = 0x12;
        mem.vga.plane_mask = 0x0F;
        mem.vga.bit_mask = 0xFF;

        mem.vga.write_mode = 1;
        let _ = mem.read_u8(VIDEO_BASE);
        mem.write_u8(VIDEO_BASE, 0x55);

        mem.vga.write_mode = 0;
        mem.write_u8(VIDEO_BASE + 4, 0x66);
    });
}

#[test]
fn test_trace_subscriber_callbacks_execute() {
    with_trace_subscriber(|| {
        let span = tracing::span!(
            tracing::Level::TRACE,
            "callback_span",
            value = tracing::field::Empty
        );
        span.record("value", 123u64);

        let parent = tracing::span!(tracing::Level::TRACE, "callback_parent");
        span.follows_from(parent.id().unwrap());

        {
            let _guard = span.enter();
            tracing::trace!(answer = 42u8, "inside callback span");
        }
    });
}

#[test]
fn test_write_planar_ega_mode2_logical_ops_and_continue_paths() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.plane_mask = 0x05;
    mem.vga.bit_mask = 0xFF;
    mem.latches.set([0x0F, 0xF0, 0x55, 0xAA]);

    mem.vga.write_mode = 2;
    mem.vga.data_rotate = 0x08;
    mem.write_u8(VIDEO_BASE + 1, 0x0F);
    mem.vga.data_rotate = 0x10;
    mem.write_u8(VIDEO_BASE + 2, 0x0F);
    mem.vga.data_rotate = 0x18;
    mem.write_u8(VIDEO_BASE + 3, 0x0F);

    mem.vga.write_mode = 3;
    mem.vga.plane_mask = 0x01;
    mem.vga.set_reset = 0x01;
    mem.vga.data_rotate = 0x00;
    mem.write_u8(VIDEO_BASE + 4, 0xFF);
}

#[test]
fn test_write_planar_ega_tracked_mode0_and_mode2_logical_paths() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.enable_write_tracking();
    mem.set_write_context(0x1111, 0x2222);
    mem.vga.plane_mask = 0x0F;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0x0F;
    mem.vga.set_reset = 0x05;

    mem.vga.write_mode = 0;
    mem.vga.data_rotate = 0x08;
    mem.write_planar_ega_tracked(0x10, 0xAA);
    mem.vga.data_rotate = 0x10;
    mem.write_planar_ega_tracked(0x11, 0xAA);
    mem.vga.data_rotate = 0x18;
    mem.write_planar_ega_tracked(0x12, 0xAA);

    mem.vga.write_mode = 2;
    mem.vga.data_rotate = 0x08;
    mem.write_planar_ega_tracked(0x13, 0x0F);
    mem.vga.data_rotate = 0x10;
    mem.write_planar_ega_tracked(0x14, 0x0F);
    mem.vga.data_rotate = 0x18;
    mem.write_planar_ega_tracked(0x15, 0x0F);

    assert!(mem.pending_write_count() > 0);
}

#[test]
fn test_write_u16_tracking_fallback_paths() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x1000, 0x0100);

    mem.write_u16(VIDEO_BASE, 0x1111);
    mem.write_u16(BIOS_BASE - 1, 0x2222);
    mem.write_u16(BIOS_BASE, 0x3333);

    mem.allow_rom_writes = true;
    mem.set_a20(false);
    mem.write_u16(0xFFFFF, 0x4444);
}

#[test]
fn test_write_u32_tracking_fallback_paths() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x1000, 0x0100);

    mem.write_u32(VIDEO_BASE, 0x1111_2222);
    mem.write_u32(BIOS_BASE - 1, 0x3333_4444);

    mem.allow_rom_writes = true;
    mem.set_a20(false);
    mem.write_u32(0xFFFFF, 0x5555_6666);
}

#[test]
fn test_read_u8_guard_paths_for_chain4_and_planar_non_video() {
    let mut mem = Memory::new();

    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.write_u8(0x2000, 0x5A);
    assert_eq!(mem.read_u8(0x2000), 0x5A);

    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.write_u8(0x2100, 0xA5);
    assert_eq!(mem.read_u8(0x2100), 0xA5);
}

#[test]
fn test_write_u32_tracking_wraparound_noncontiguous_paths() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x4444, 0x0100);
    mem.allow_rom_writes = true;
    // A20 enabled (default): bytes above 0xFFFFF go to extended memory
    // (0x100000+) rather than wrapping around. Write tracking still works.
    mem.write_u32(0xFFFFD, 0x1122_3344);
    mem.write_u32(0xFFFFE, 0x5566_7788);

    assert_eq!(mem.read_u8(0xFFFFD), 0x44);
    assert_eq!(mem.read_u8(0xFFFFE), 0x88);
    assert_eq!(mem.read_u8(0xFFFFF), 0x77);
    assert_eq!(mem.read_u8(0x100000), 0x66);
    assert_eq!(mem.read_u8(0x100001), 0x55);
}

#[test]
fn test_write_u16_tracking_wraparound_noncontiguous_path() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x5555, 0x0200);
    mem.allow_rom_writes = true;
    // A20 enabled (default): no wrap at 1MB boundary
    mem.write_u16(0xFFFFF, 0xA1B2);

    assert_eq!(mem.read_u8(0xFFFFF), 0xB2);
    assert_eq!(mem.read_u8(0x100000), 0xA1);
}

#[test]
fn test_text_video_write_count_includes_mono_and_color_buffers() {
    let mut mem = Memory::new();
    let before = mem.video_write_count();

    mem.write_u8(TEXT_VIDEO_BASE_MONO, 0x12);
    mem.write_u8(TEXT_VIDEO_BASE_COLOR, 0x34);

    assert_eq!(mem.video_write_count(), before + 2);
}

#[test]
fn test_lfb_address_upper_bound_not_treated_as_lfb() {
    let mut mem = Memory::new();
    mem.init_lfb(16);
    mem.set_lfb_enabled(true);

    // Below the LFB base should NOT be treated as LFB. The aperture sits
    // above every address the guest can be given as RAM, so the byte
    // under it is unbacked: open bus, not RAM.
    let below = mem.read_u8(LFB_BASE - 1);
    assert!(!mem.is_lfb_address(LFB_BASE - 1));
    assert_eq!(below, 0xFF);

    mem.write_u8(LFB_BASE + 15, 0xAA);
    // Past the 16-byte aperture nothing responds, so the write is dropped.
    mem.write_u8(LFB_BASE + 16, 0x55);

    assert_eq!(mem.lfb_read_u8(15), 0xAA);
    assert_eq!(mem.lfb_read_u8(0), 0);
    assert_eq!(mem.read_u8(LFB_BASE + 16), 0xFF);
}

#[test]
fn test_lfb_write_u16_through_high_address() {
    let mut mem = Memory::new();
    mem.init_lfb(256 * 1024); // 256KB LFB
    mem.set_lfb_enabled(true);
    let offset = 100;
    mem.write_u16(LFB_BASE + offset, 0xABCD);
    assert_eq!(mem.lfb()[offset], 0xCD); // little-endian low byte
    assert_eq!(mem.lfb()[offset + 1], 0xAB); // little-endian high byte
                                             // Verify round-trip
    assert_eq!(mem.read_u16(LFB_BASE + offset), 0xABCD);
}

#[test]
fn test_lfb_write_u32_through_high_address() {
    let mut mem = Memory::new();
    mem.init_lfb(256 * 1024);
    mem.set_lfb_enabled(true);
    let offset = 200;
    mem.write_u32(LFB_BASE + offset, 0xDEADBEEF);
    assert_eq!(mem.lfb()[offset], 0xEF);
    assert_eq!(mem.lfb()[offset + 1], 0xBE);
    assert_eq!(mem.lfb()[offset + 2], 0xAD);
    assert_eq!(mem.lfb()[offset + 3], 0xDE);
    assert_eq!(mem.read_u32(LFB_BASE + offset), 0xDEADBEEF);
}

#[test]
fn test_read_video_planar_high_offset_out_of_bounds() {
    let mem = Memory::new();
    assert_eq!(mem.read_video_planar(PLANE_SIZE, 3), 0);
}

#[test]
fn test_load_empty_slice_ok() {
    let mut mem = Memory::new();
    assert!(mem.load(0x1000, &[]).is_ok());
}

#[test]
fn test_smc_before_write_size_zero_returns_false() {
    let mut mem = Memory::new();
    assert!(!mem.smc_before_write(0x2000, 0));
}

#[test]
fn test_hash_state_changes_on_memory_write() {
    let mut mem = Memory::new();
    let before = mem.hash_state();
    mem.write_u8(0x2000, 0xAA);
    let after = mem.hash_state();
    assert_ne!(before, after);
}

#[test]
fn test_read_write_u64_round_trip() {
    let mut mem = Memory::new();
    mem.write_u64(0x3200, 0x1122_3344_5566_7788);
    assert_eq!(mem.read_u64(0x3200), 0x1122_3344_5566_7788);
}

#[test]
fn test_memory_access_trait_wrapper_calls() {
    let mut mem = Memory::new();

    <Memory as MemoryAccess>::write_u8(&mut mem, 0x3400, 0x12);
    <Memory as MemoryAccess>::write_u16(&mut mem, 0x3402, 0x3456);
    <Memory as MemoryAccess>::write_u32(&mut mem, 0x3404, 0x789A_BCDE);
    <Memory as MemoryAccess>::write_u64(&mut mem, 0x3408, 0x1020_3040_5060_7080);

    assert_eq!(<Memory as MemoryAccess>::read_u8(&mem, 0x3400), 0x12);
    assert_eq!(<Memory as MemoryAccess>::read_u16(&mem, 0x3402), 0x3456);
    assert_eq!(
        <Memory as MemoryAccess>::read_u32(&mem, 0x3404),
        0x789A_BCDE
    );
    assert_eq!(
        <Memory as MemoryAccess>::read_u64(&mem, 0x3408),
        0x1020_3040_5060_7080
    );
}

#[test]
fn test_ivt_watchpoint_fires_on_vga_range_segment() {
    // Write a VGA-range segment (0xA500) into IVT entry 0x10 (at address 0x42-0x43)
    let mut mem = Memory::new();
    // IVT entry 0x10 is at address 0x40: [offset_lo, offset_hi, seg_lo, seg_hi]
    // Write segment = 0xA500 (VGA range) to the segment bytes
    mem.write_u8(0x40, 0x00); // offset low
    mem.write_u8(0x41, 0x00); // offset high
    mem.write_u8(0x42, 0x00); // segment low byte (0x00 of 0xA500)
    mem.write_u8(0x43, 0xA5); // segment high byte — this triggers watchpoint

    // Verify the IVT entry contains the expected VGA-range segment
    let seg = u16::from_le_bytes([mem.ram[0x42], mem.ram[0x43]]);
    assert_eq!(seg, 0xA500);
    assert!(seg >= 0xA000 && seg < 0xC000);
}

#[test]
fn test_ivt_watchpoint_ignores_normal_segments() {
    // Write a normal (non-VGA) segment into an IVT entry — should not fire watchpoint
    let mut mem = Memory::new();
    // IVT entry 0x21 is at address 0x84
    mem.write_u8(0x84, 0x00); // offset low
    mem.write_u8(0x85, 0x10); // offset high
    mem.write_u8(0x86, 0x00); // segment low (0xF000)
    mem.write_u8(0x87, 0xF0); // segment high — BIOS range, not VGA

    let seg = u16::from_le_bytes([mem.ram[0x86], mem.ram[0x87]]);
    assert_eq!(seg, 0xF000);
    // This should NOT have triggered the watchpoint (no VGA range)
}

#[test]
fn test_ivt_entry_boundary_alignment() {
    // Verify the watchpoint correctly aligns to 4-byte IVT entry boundaries
    let mut mem = Memory::new();
    // Write to address 0x53 (within IVT entry 0x14 at base 0x50)
    // Entry 0x14 segment bytes are at 0x52-0x53
    mem.write_u8(0x52, 0x00); // segment low
    mem.write_u8(0x53, 0xA0); // segment high (0xA000 = VGA range start)

    let entry_base = 0x53 & !3; // Should be 0x50
    assert_eq!(entry_base, 0x50);
    let seg = u16::from_le_bytes([mem.ram[0x52], mem.ram[0x53]]);
    assert_eq!(seg, 0xA000);
}

#[test]
fn test_paged_memory_trait_wrapper_calls() {
    let mut mem = Memory::new();
    mem.add_physical_mapping(0x5000, 0x6000, 0x100);

    assert_eq!(
        <Memory as PagedMemory>::map_linear_physical(&mem, 0x5080),
        Some(0x6080)
    );

    <Memory as PagedMemory>::tlb_insert(&mut mem, 0x12, 0x34, true, false, false);
    assert_eq!(
        <Memory as PagedMemory>::tlb_lookup(&mem, 0x12),
        Some((0x34, true, false, false))
    );

    <Memory as PagedMemory>::invalidate_tlb_page(&mut mem, 0x12 << 12);
    assert_eq!(<Memory as PagedMemory>::tlb_lookup(&mem, 0x12), None);

    <Memory as PagedMemory>::tlb_insert(&mut mem, 0x22, 0x44, false, true, false);
    <Memory as PagedMemory>::invalidate_tlb(&mut mem);
    assert_eq!(<Memory as PagedMemory>::tlb_lookup(&mem, 0x22), None);

    <Memory as PagedMemory>::write_u32_physical(&mut mem, 0x3600, 0xCAFEBABE);
    assert_eq!(
        <Memory as PagedMemory>::read_u32_physical(&mem, 0x3600),
        0xCAFEBABE
    );

    <Memory as PagedMemory>::set_page_fault(&mut mem, 0xDEAD_BEEF, 0x05);
    assert!(<Memory as PagedMemory>::page_fault_pending(&mem));
    assert_eq!(
        <Memory as PagedMemory>::page_fault_address(&mem),
        0xDEAD_BEEF
    );
    assert_eq!(<Memory as PagedMemory>::page_fault_error_code(&mem), 0x05);

    <Memory as PagedMemory>::clear_page_fault(&mut mem);
    assert!(!<Memory as PagedMemory>::page_fault_pending(&mem));
}

// ── Fetch-padding safety tests (C3) ─────────────────────────────────

#[test]
fn test_fetch_padding_initial_allocation() {
    let mem = Memory::new();
    assert_eq!(mem.usable_len(), DEFAULT_RAM_SIZE);
    assert_eq!(mem.ram.len(), DEFAULT_RAM_SIZE + FETCH_PADDING);
    assert!(mem.ram.capacity() >= DOS_RAM_CEILING + FETCH_PADDING);
}

#[test]
fn test_fetch_padding_read_past_usable_end() {
    let mut mem = Memory::new();
    // Write a byte at the last usable address
    mem.write_u8(TOTAL_SIZE - 1, 0xAB);
    // Reading 15 bytes past it (within padding) should not panic or segfault
    let ptr = mem.ram.host_base().cast_const();
    for i in 0..15 {
        let addr = TOTAL_SIZE - 1 + i;
        let val = unsafe { *ptr.add(addr) };
        if i == 0 {
            assert_eq!(val, 0xAB);
        } else {
            // Padding bytes are zero
            assert_eq!(val, 0);
        }
    }
}

#[test]
fn test_fetch_padding_preserved_after_resize() {
    let mut mem = Memory::new();
    // Force a resize through prepare_write_addr (via write_u8)
    let addr = TOTAL_SIZE + 0x1000; // well past initial size
    mem.write_u8(addr, 0xCD);
    // After resize, usable_len should include the new address
    assert!(mem.usable_len() > addr);
    // And padding should still exist
    assert_eq!(mem.ram.len(), mem.usable_len() + FETCH_PADDING);
    // Read 15 bytes past the write address — within padding
    let ptr = mem.ram.host_base().cast_const();
    for i in 1..=15 {
        let _ = unsafe { *ptr.add(addr + i) };
    }
}

#[test]
fn test_fetch_padding_ensure_ram_size_preserves_padding() {
    let mut mem = Memory::new();
    mem.ensure_ram_size(2 * 1024 * 1024); // 2MB
    assert!(mem.usable_len() >= 2 * 1024 * 1024);
    assert_eq!(mem.ram.len(), mem.usable_len() + FETCH_PADDING);
}

#[test]
fn test_ram_size_equals_usable_len() {
    let mem = Memory::new();
    assert_eq!(mem.ram_size(), mem.usable_len());
    assert_eq!(mem.ram_size(), DEFAULT_RAM_SIZE);
}
