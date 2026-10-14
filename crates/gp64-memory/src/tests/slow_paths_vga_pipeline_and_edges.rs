use super::*;

// ========== read_u16/read_u32 slow path (video range) ==========

#[test]
fn test_read_u16_video_range() {
    let mut mem = Memory::new();
    // Write to video memory — goes through slow path for read_u16
    mem.ram[VIDEO_BASE] = 0xAB;
    mem.ram[VIDEO_BASE + 1] = 0xCD;
    // In text mode, video reads go through slow path
    let val = mem.read_u16(VIDEO_BASE);
    assert_eq!(val, 0xCDAB);
}

#[test]
fn test_read_u32_video_range() {
    let mut mem = Memory::new();
    mem.ram[VIDEO_BASE] = 0x11;
    mem.ram[VIDEO_BASE + 1] = 0x22;
    mem.ram[VIDEO_BASE + 2] = 0x33;
    mem.ram[VIDEO_BASE + 3] = 0x44;
    let val = mem.read_u32(VIDEO_BASE);
    assert_eq!(val, 0x44332211);
}

#[test]
fn test_read_u64_basic() {
    let mut mem = Memory::new();
    mem.write_u32(0x4000, 0xDEADBEEF);
    mem.write_u32(0x4004, 0xCAFEBABE);
    let val = mem.read_u64(0x4000);
    assert_eq!(val, 0xCAFEBABE_DEADBEEF);
}

#[test]
fn test_write_u64_basic() {
    let mut mem = Memory::new();
    mem.write_u64(0x5000, 0x1234_5678_9ABC_DEF0);
    assert_eq!(mem.read_u32(0x5000), 0x9ABC_DEF0);
    assert_eq!(mem.read_u32(0x5004), 0x1234_5678);
}

#[test]
fn test_read_bytes_empty_slice() {
    let mem = Memory::new();
    let mut buf = [];
    mem.read_bytes(0x1000, &mut buf);
    // Should not panic on empty slice
}

#[test]
fn test_read_bytes_beyond_usable_fills_0xff() {
    let mem = Memory::new();
    let near_end = mem.usable_len() - 2;
    let mut buf = [0u8; 6];
    mem.read_bytes(near_end, &mut buf);
    // First 2 bytes from RAM, remaining 4 should be 0xFF
    assert_eq!(buf[2], 0xFF);
    assert_eq!(buf[3], 0xFF);
    assert_eq!(buf[4], 0xFF);
    assert_eq!(buf[5], 0xFF);
}

// ========== Write tracking ==========

#[test]
fn test_write_u8_with_tracking_records_write() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x100, 0x200);
    mem.ram[0x8000] = 0x11; // pre-existing value
    mem.write_u8(0x8000, 0x22);
    let writes = mem.drain_pending_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].linear_address, 0x8000);
    assert_eq!(writes[0].old_value, 0x11);
    assert_eq!(writes[0].new_value, 0x22);
    assert_eq!(writes[0].size, 1);
}

#[test]
fn test_write_u16_with_tracking_records_contiguous() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x100, 0x300);
    mem.ram[0x9000] = 0xAA;
    mem.ram[0x9001] = 0xBB;
    mem.write_u16(0x9000, 0xCDEF);
    let writes = mem.drain_pending_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].size, 2);
    assert_eq!(writes[0].old_value, 0xBBAA);
    assert_eq!(writes[0].new_value, 0xCDEF);
}

#[test]
fn test_write_u32_with_tracking_records_contiguous() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x100, 0x400);
    mem.write_u32(0xA000, 0x12345678);
    let writes = mem.drain_pending_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].size, 4);
    assert_eq!(writes[0].new_value, 0x12345678);
}

#[test]
fn test_write_tracking_context_clear() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x10, 0x20);
    assert!(mem.write_context().is_some());
    mem.clear_write_context();
    assert!(mem.write_context().is_none());
    // No context → writes not recorded
    mem.write_u8(0x1000, 0xFF);
    assert_eq!(mem.pending_write_count(), 0);
}

#[test]
fn test_write_tracking_hle_flag_propagated() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x10, 0x20);
    mem.set_hle_handler_active(true);
    assert!(mem.is_hle_handler_active());
    mem.write_u8(0x1000, 0x42);
    let writes = mem.drain_pending_writes();
    assert_eq!(writes.len(), 1);
    assert!(writes[0].is_hle_handler);
}

// ========== GDT snooping ==========

#[test]
fn test_gdt_snoop_range_basic() {
    let mut mem = Memory::new();
    assert_eq!(mem.gdt_snoop_base(), 0);
    assert_eq!(mem.gdt_snoop_end(), 0);

    mem.set_gdt_snoop_range(0x10000, 0xFF); // 256 bytes
    assert_eq!(mem.gdt_snoop_base(), 0x10000);
    assert_eq!(mem.gdt_snoop_end(), 0x10100);
}

#[test]
fn test_gdt_snoop_dirty_flag() {
    let mut mem = Memory::new();
    mem.set_gdt_snoop_range(0x10000, 0xFF);

    // Initially clean
    assert!(!mem.take_gdt_dirty());

    // Write inside GDT range → dirty
    mem.snoop_gdt_write_ext(0x10004, 0x42, 1);
    assert!(mem.take_gdt_dirty());

    // After take, should be clean again
    assert!(!mem.take_gdt_dirty());
}

#[test]
fn test_gdt_snoop_outside_range_not_dirty() {
    let mut mem = Memory::new();
    mem.set_gdt_snoop_range(0x10000, 0xFF);

    // Write outside GDT range
    mem.snoop_gdt_write_ext(0x20000, 0x42, 1);
    assert!(!mem.take_gdt_dirty());
}

#[test]
fn test_gdt_snoop_disabled_when_end_zero() {
    let mem = Memory::new();
    // Default: end=0 → no snooping
    mem.snoop_gdt_write_ext(0x100, 0x42, 1);
    assert!(!mem.take_gdt_dirty());
}

// ========== Page fault trait methods ==========

#[test]
fn test_set_and_clear_page_fault() {
    let mut mem = Memory::new();
    assert!(!mem.page_fault_pending);

    // Use PagedMemory trait method
    <Memory as PagedMemory>::set_page_fault(&mut mem, 0xDEAD_0000, 0x07);
    assert!(mem.page_fault_pending);
    assert_eq!(mem.page_fault_address, 0xDEAD_0000);
    assert_eq!(mem.page_fault_error_code, 0x07);

    mem.clear_page_fault();
    assert!(!mem.page_fault_pending);
    assert_eq!(mem.page_fault_address, 0);
    assert_eq!(mem.page_fault_error_code, 0);
}

#[test]
fn test_page_fault_accessors_via_trait() {
    let mut mem = Memory::new();
    <Memory as PagedMemory>::set_page_fault(&mut mem, 0x1234_5000, 0x05);
    assert!(<Memory as PagedMemory>::page_fault_pending(&mem));
    assert_eq!(
        <Memory as PagedMemory>::page_fault_address(&mem),
        0x1234_5000
    );
    assert_eq!(<Memory as PagedMemory>::page_fault_error_code(&mem), 0x05);
    <Memory as PagedMemory>::clear_page_fault(&mut mem);
    assert!(!<Memory as PagedMemory>::page_fault_pending(&mem));
}

// ========== Physical memory access ==========

#[test]
fn test_read_u32_physical_within_bounds() {
    let mut mem = Memory::new();
    mem.ram[0x2000] = 0x11;
    mem.ram[0x2001] = 0x22;
    mem.ram[0x2002] = 0x33;
    mem.ram[0x2003] = 0x44;
    assert_eq!(mem.read_u32_physical(0x2000), 0x44332211);
}

#[test]
fn test_read_u32_physical_out_of_bounds() {
    let mem = Memory::new();
    // Address beyond RAM returns 0
    assert_eq!(mem.read_u32_physical(MAX_RAM_SIZE + 0x1000), 0);
}

#[test]
fn test_write_u32_physical_within_bounds() {
    let mut mem = Memory::new();
    mem.write_u32_physical(0x3000, 0xCAFEBABE);
    assert_eq!(
        u32::from_le_bytes([
            mem.ram[0x3000],
            mem.ram[0x3001],
            mem.ram[0x3002],
            mem.ram[0x3003],
        ]),
        0xCAFEBABE
    );
}

// ========== LFB via write_u64 ==========

#[test]
fn test_lfb_write_u64_through_high_address() {
    let mut mem = Memory::new();
    mem.init_lfb(4096);
    mem.set_lfb_enabled(true);

    mem.write_u64(LFB_BASE + 100, 0x1122334455667788);
    assert_eq!(
        u32::from_le_bytes([
            mem.lfb()[100],
            mem.lfb()[101],
            mem.lfb()[102],
            mem.lfb()[103],
        ]),
        0x55667788
    );
    assert_eq!(
        u32::from_le_bytes([
            mem.lfb()[104],
            mem.lfb()[105],
            mem.lfb()[106],
            mem.lfb()[107],
        ]),
        0x11223344
    );
}

// ========== VGA pipeline and mode transitions ==========

#[test]
fn test_apply_vga_pipeline_chain4_to_planar() {
    let mut mem = Memory::new();
    // Start in chain4 mode 0x13. plane_mask=0xF so all planes accept.
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.plane_mask = 0x0F;
    mem.update_vga_write_handler();

    // Write through the Chain4 dispatch (keeps ram and planes_flat
    // coherent per byte). Going through the public API matches what
    // a real guest does and is what the dispatch contract guarantees.
    mem.write_u8(VIDEO_BASE, 0x10);
    mem.write_u8(VIDEO_BASE + 1, 0x20);
    mem.write_u8(VIDEO_BASE + 2, 0x30);
    mem.write_u8(VIDEO_BASE + 3, 0x40);

    // Switch to Mode X (chain4=false, is_mode_x=true confirmed via
    // CR9 max-scan reprogramming) via pipeline. Without is_mode_x the
    // transition is the VGA firmware Mode-13h-init transient and falls
    // back to Linear dispatch rather than Planar.
    let mut state = mem.vga;
    state.chain4 = false;
    state.is_mode_x = true;
    mem.apply_vga_pipeline(&state);

    // Chain4 already kept planes coherent per byte, so planes_flat
    // already has the expected data — Chain4→Planar transition is a
    // no-op for storage (no sync needed).
    assert_eq!(mem.planes_flat[0], 0x10); // plane 0
    assert_eq!(mem.planes_flat[PLANE_SIZE], 0x20); // plane 1
    assert_eq!(mem.planes_flat[2 * PLANE_SIZE], 0x30); // plane 2
    assert_eq!(mem.planes_flat[3 * PLANE_SIZE], 0x40); // plane 3
}

#[test]
fn test_apply_vga_pipeline_planar_to_chain4() {
    let mut mem = Memory::new();
    // Start in real Mode X (chain4 disabled with is_mode_x set via
    // CR9/CR17 reprogramming).
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = false;
    mem.vga.is_mode_x = true;
    mem.update_vga_write_handler();

    // Put data in planes
    mem.planes_flat[0] = 0xAA; // plane 0, offset 0
    mem.planes_flat[PLANE_SIZE] = 0xBB; // plane 1, offset 0

    // Switch back to chain4 via pipeline (Mode X exit clears is_mode_x).
    let mut state = mem.vga;
    state.chain4 = true;
    state.is_mode_x = false;
    mem.apply_vga_pipeline(&state);

    // After sync, linear RAM should have the plane data interleaved
    assert_eq!(mem.ram[VIDEO_BASE], 0xAA);
    assert_eq!(mem.ram[VIDEO_BASE + 1], 0xBB);
}

#[test]
fn test_apply_vga_pipeline_non_mode13_no_sync() {
    let mut mem = Memory::new();
    // Not mode 0x13 — chain4 toggle should not trigger sync
    mem.vga.video_mode = 0x03;
    mem.vga.chain4 = true;
    mem.update_vga_write_handler();

    // Put sentinel in plane data
    mem.planes_flat[0] = 0x99;

    let mut state = mem.vga;
    state.chain4 = false;
    mem.apply_vga_pipeline(&state);

    // Planes should be unchanged (no sync for non-mode-0x13)
    assert_eq!(mem.planes_flat[0], 0x99);
}

#[test]
fn test_update_vga_write_handler_modes() {
    let mut mem = Memory::new();

    // Text mode → Linear
    mem.vga.video_mode = 0x03;
    mem.vga.chain4 = true;
    mem.update_vga_write_handler();
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Linear);

    // Mode 13h chain4 → Chain4
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.update_vga_write_handler();
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Chain4);

    // Mode 13h unchained AND is_mode_x set (real Mode X) → Planar
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = false;
    mem.vga.is_mode_x = true;
    mem.update_vga_write_handler();
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Planar);

    // Planar EGA modes → Planar
    mem.vga.is_mode_x = false;
    for mode in [0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x12] {
        mem.vga.video_mode = mode;
        mem.vga.chain4 = false;
        mem.update_vga_write_handler();
        assert_eq!(
            mem.vga_write_handler(),
            VgaWriteHandler::Planar,
            "mode {:#04X} should use Planar handler",
            mode
        );
    }
}

#[test]
fn text_planar_routing_mask_and_read_plane_are_independent() {
    let mut mem = Memory::new();
    let initial = [0x21, 0x43, 0x65, 0x87];
    for mask in [0x00, 0x04, 0x0C, 0x0F] {
        for read_plane in 0..4 {
            let mut state = mem.vga;
            state.video_mode = 0x03;
            state.chain4 = false;
            state.seq_odd_even_disabled = true;
            state.gc_odd_even_read = false;
            state.is_mode_x = false;
            state.plane_mask = mask;
            state.read_plane = read_plane;
            state.memory_map_select = 1;
            state.gc_graphics_mode = false;
            state.write_mode = 0;
            state.bit_mask = 0xFF;
            mem.apply_vga_pipeline(&state);
            for (plane, value) in initial.into_iter().enumerate() {
                mem.plane_mut(plane)[0] = value;
            }
            mem.ram[VIDEO_BASE] = 0x11;
            mem.write_u8(VIDEO_BASE, 0x5A);
            for (plane, value) in initial.into_iter().enumerate() {
                let expected = if mask & (1 << plane) != 0 {
                    0x5A
                } else {
                    value
                };
                assert_eq!(
                    mem.plane(plane)[0],
                    expected,
                    "mask={mask:02X}, read_plane={read_plane}, plane={plane}"
                );
            }
            let expected = if mask & (1 << read_plane) != 0 {
                0x5A
            } else {
                initial[read_plane as usize]
            };
            assert_eq!(
                mem.read_u8(VIDEO_BASE),
                expected,
                "read mask={mask:02X}, plane={read_plane}"
            );
            assert_eq!(mem.peek_video_u8(VIDEO_BASE), expected);
            let mut bytes = [0];
            mem.read_bytes(VIDEO_BASE, &mut bytes);
            assert_eq!(bytes[0], expected);
            mem.peek_video_range(VIDEO_BASE, &mut bytes);
            assert_eq!(bytes[0], expected);
            assert_eq!(
                mem.ram[VIDEO_BASE], 0x11,
                "sequential planar writes preserve raw RAM"
            );
        }
    }

    // Read odd/even and write odd/even are separate register controls.
    for (sequential_write, odd_even_read) in [(true, true), (false, false)] {
        let mut state = mem.vga;
        state.seq_odd_even_disabled = sequential_write;
        state.gc_odd_even_read = odd_even_read;
        state.plane_mask = 0x0C;
        state.read_plane = 2;
        let epoch = mem.memory_policy_epoch();
        mem.apply_vga_pipeline(&state);
        assert!(mem.memory_policy_epoch() > epoch);
        for (plane, byte) in initial.into_iter().enumerate() {
            mem.plane_mut(plane)[0] = byte;
        }
        mem.ram[VIDEO_BASE] = 0x11;
        mem.write_u8(VIDEO_BASE, 0x5A);
        let expected_plane = if sequential_write { 0x5A } else { initial[2] };
        let expected_ram = if sequential_write { 0x11 } else { 0x5A };
        assert_eq!(mem.plane(2)[0], expected_plane);
        assert_eq!(mem.ram[VIDEO_BASE], expected_ram);
        let expected_read = if odd_even_read {
            expected_ram
        } else {
            expected_plane
        };
        assert_eq!(mem.read_u8(VIDEO_BASE), expected_read);
        assert_eq!(mem.peek_video_u8(VIDEO_BASE), expected_read);
    }
    for (map, base) in [(2, TEXT_VIDEO_BASE_MONO), (3, TEXT_VIDEO_BASE_COLOR)] {
        let mut state = mem.vga;
        state.seq_odd_even_disabled = true;
        state.gc_odd_even_read = false;
        state.memory_map_select = map;
        mem.apply_vga_pipeline(&state);
        mem.ram[base] = 0x11;
        mem.write_u8(base, 0x5A);
        assert_eq!(mem.plane(2)[0], 0x5A);
        assert_eq!(mem.read_u8(base), 0x5A);
        assert_eq!(mem.ram[base], 0x11);
    }
}

#[test]
fn cga_packed_routing_b800_reads_the_packed_store() {
    for mode in [0x04, 0x05, 0x06] {
        let mut mem = Memory::new();
        let mut state = mem.vga;
        state.video_mode = mode;
        state.chain4 = false;
        state.seq_odd_even_disabled = true;
        state.gc_odd_even_read = false;
        state.memory_map_select = 3;
        state.plane_mask = 0x04;
        state.read_plane = 2;
        state.write_mode = 0;
        state.bit_mask = 0xFF;
        mem.apply_vga_pipeline(&state);

        let packed_addr = TEXT_VIDEO_BASE_COLOR + 0x123;
        let offset = packed_addr - TEXT_VIDEO_BASE_COLOR;
        mem.ram[packed_addr] = 0xA5;
        mem.ram[packed_addr + 1] = 0xB6;
        mem.plane_mut(2)[offset] = 0x3C;
        mem.plane_mut(2)[offset + 1] = 0x4D;
        let sentinel_latches = [0xD1, 0xE2, 0xF3, 0xA4];
        mem.set_latches(sentinel_latches);

        assert_eq!(
            mem.read_u8(packed_addr),
            0xA5,
            "CGA mode {mode:02X} B800 reads must use packed CGA RAM"
        );
        assert_eq!(mem.peek_video_u8(packed_addr), 0xA5);

        let mut read_bytes = [0; 2];
        mem.read_bytes(packed_addr, &mut read_bytes);
        assert_eq!(
            read_bytes,
            [0xA5, 0xB6],
            "CGA mode {mode:02X} B800 read_bytes must use packed CGA RAM"
        );

        let mut peek_bytes = [0; 2];
        mem.peek_video_range(packed_addr, &mut peek_bytes);
        assert_eq!(
            peek_bytes,
            [0xA5, 0xB6],
            "CGA mode {mode:02X} B800 peek_video_range must use packed CGA RAM"
        );
        assert_eq!(
            mem.get_latches(),
            sentinel_latches,
            "CGA mode {mode:02X} packed reads must not change VGA latches"
        );
    }
}

#[test]
fn cga_packed_routing_b800_writes_the_packed_store() {
    for mode in [0x04, 0x05, 0x06] {
        let mut mem = Memory::new();
        let mut state = mem.vga;
        state.video_mode = mode;
        state.chain4 = false;
        state.seq_odd_even_disabled = true;
        state.gc_odd_even_read = false;
        state.memory_map_select = 3;
        state.plane_mask = 0x04;
        state.read_plane = 2;
        state.write_mode = 0;
        state.bit_mask = 0xFF;
        mem.apply_vga_pipeline(&state);

        let packed_addr = TEXT_VIDEO_BASE_COLOR + 0x123;
        let offset = packed_addr - TEXT_VIDEO_BASE_COLOR;
        mem.ram[packed_addr] = 0xA5;
        mem.plane_mut(2)[offset] = 0x3C;

        mem.write_u8(packed_addr, 0x5A);

        assert_eq!(
            mem.ram[packed_addr], 0x5A,
            "CGA mode {mode:02X} B800 writes must update packed CGA RAM"
        );
        assert_eq!(
            mem.plane(2)[offset],
            0x3C,
            "CGA mode {mode:02X} B800 writes must leave the VGA plane store alone"
        );
    }
}

#[test]
fn cga_packed_routing_apply_pipeline_mode_change_invalidates_memory_policy() {
    let mut mem = Memory::new();
    let mut text_state = mem.vga;
    text_state.video_mode = 0x03;
    text_state.chain4 = false;
    text_state.seq_odd_even_disabled = true;
    text_state.gc_odd_even_read = false;
    text_state.memory_map_select = 3;
    text_state.read_plane = 2;
    mem.apply_vga_pipeline(&text_state);
    let packed_addr = TEXT_VIDEO_BASE_COLOR + 0x321;
    let offset = packed_addr - TEXT_VIDEO_BASE_COLOR;
    mem.ram[packed_addr] = 0xA5;
    mem.plane_mut(2)[offset] = 0x3C;
    assert_eq!(mem.read_u8(packed_addr), 0x3C);

    let before = mem.memory_policy_epoch();
    let direct_before = mem.direct_memory_policy_epoch();
    let mut cga_state = mem.vga;
    cga_state.video_mode = 0x04;
    mem.apply_vga_pipeline(&cga_state);

    assert!(
        mem.memory_policy_epoch() > before,
        "mode-only text-to-CGA transition must invalidate memory policy"
    );
    assert!(
        mem.direct_memory_policy_epoch() > direct_before,
        "mode-only text-to-CGA transition must invalidate direct-memory policy"
    );
    assert_eq!(
        mem.read_u8(packed_addr),
        0xA5,
        "CGA mode must read the packed B800 store after the mode-only transition"
    );

    let reverse_before = mem.memory_policy_epoch();
    let reverse_direct_before = mem.direct_memory_policy_epoch();
    let mut restored_text_state = mem.vga;
    restored_text_state.video_mode = 0x03;
    mem.apply_vga_pipeline(&restored_text_state);
    assert!(mem.memory_policy_epoch() > reverse_before);
    assert!(mem.direct_memory_policy_epoch() > reverse_direct_before);
    assert_eq!(
        mem.read_u8(packed_addr),
        0x3C,
        "restored text mode must resume register-derived planar reads"
    );
}

#[test]
fn cga_packed_routing_set_video_mode_invalidates_memory_policy() {
    let mut mem = Memory::new();
    let mut text_state = mem.vga;
    text_state.video_mode = 0x03;
    text_state.chain4 = false;
    text_state.seq_odd_even_disabled = true;
    text_state.gc_odd_even_read = false;
    text_state.memory_map_select = 3;
    text_state.read_plane = 2;
    mem.apply_vga_pipeline(&text_state);
    let packed_addr = TEXT_VIDEO_BASE_COLOR + 0x321;
    let offset = packed_addr - TEXT_VIDEO_BASE_COLOR;
    mem.ram[packed_addr] = 0xA5;
    mem.plane_mut(2)[offset] = 0x3C;
    assert_eq!(mem.read_u8(packed_addr), 0x3C);

    let before = mem.memory_policy_epoch();
    let direct_before = mem.direct_memory_policy_epoch();
    mem.set_video_mode(0x04);

    assert!(
        mem.memory_policy_epoch() > before,
        "mode-only text-to-CGA transition must invalidate memory policy"
    );
    assert!(
        mem.direct_memory_policy_epoch() > direct_before,
        "mode-only text-to-CGA transition must invalidate direct-memory policy"
    );
    assert_eq!(
        mem.read_u8(packed_addr),
        0xA5,
        "CGA mode must read the packed B800 store after the mode-only transition"
    );

    let reverse_before = mem.memory_policy_epoch();
    let reverse_direct_before = mem.direct_memory_policy_epoch();
    mem.set_video_mode(0x03);
    assert!(mem.memory_policy_epoch() > reverse_before);
    assert!(mem.direct_memory_policy_epoch() > reverse_direct_before);
    assert_eq!(
        mem.read_u8(packed_addr),
        0x3C,
        "restored text mode must resume register-derived planar reads"
    );
}

#[test]
fn cga_packed_routing_a000_remap_keeps_register_planar_access() {
    for mode in [0x04, 0x05, 0x06] {
        let mut mem = Memory::new();
        let mut state = mem.vga;
        state.video_mode = mode;
        state.chain4 = false;
        state.seq_odd_even_disabled = true;
        state.gc_odd_even_read = false;
        state.memory_map_select = 1;
        state.plane_mask = 0x04;
        state.read_plane = 2;
        state.write_mode = 0;
        state.bit_mask = 0x0F;
        mem.apply_vga_pipeline(&state);

        let offset = 0x123;
        let mapped_addr = VIDEO_BASE + offset;
        let initial_planes = [0x11, 0x22, 0x33, 0x44];
        for (plane, value) in initial_planes.into_iter().enumerate() {
            mem.plane_mut(plane)[offset] = value;
        }
        mem.ram[mapped_addr] = 0x99;

        assert_eq!(
            mem.read_u8(mapped_addr),
            initial_planes[2],
            "CGA mode {mode:02X} remapped to A000 must honor the selected read plane"
        );
        assert_eq!(mem.get_latches(), initial_planes);

        mem.write_u8(mapped_addr, 0x5A);

        assert_eq!(mem.plane(0)[offset], initial_planes[0]);
        assert_eq!(mem.plane(1)[offset], initial_planes[1]);
        assert_eq!(
            mem.plane(2)[offset],
            0x3A,
            "CGA mode {mode:02X} A000 write must honor map mask and bit mask"
        );
        assert_eq!(mem.plane(3)[offset], initial_planes[3]);
        assert_eq!(mem.ram[mapped_addr], 0x99);
        assert_eq!(
            mem.get_latches(),
            initial_planes,
            "CGA mode {mode:02X} A000 write must leave the read latches intact"
        );
        assert_eq!(mem.vga.plane_mask, state.plane_mask);
        assert_eq!(mem.vga.read_plane, state.read_plane);
        assert_eq!(mem.vga.bit_mask, state.bit_mask);
        assert_eq!(mem.vga.memory_map_select, 1);
    }
}

#[test]
fn text_planar_routing_preserves_latches_write_modes_and_text_restore() {
    let mut mem = Memory::new();
    let mut state = mem.vga;
    state.video_mode = 0x03;
    state.chain4 = false;
    state.seq_odd_even_disabled = true;
    state.gc_odd_even_read = false;
    state.plane_mask = 0x0F;
    state.read_plane = 2;
    state.memory_map_select = 1;
    state.gc_graphics_mode = false;
    state.write_mode = 1;
    mem.apply_vga_pipeline(&state);
    let source = [0x12, 0x34, 0x56, 0x78];
    for (plane, byte) in source.into_iter().enumerate() {
        mem.plane_mut(plane)[0] = byte;
    }
    assert_eq!(mem.read_u8(VIDEO_BASE), source[2]);
    assert_eq!(mem.peek_video_u8(VIDEO_BASE + 1), 0);
    mem.write_u8(VIDEO_BASE + 1, 0xFF);
    for (plane, byte) in source.into_iter().enumerate() {
        assert_eq!(
            mem.plane(plane)[1],
            byte,
            "write mode 1 copies plane {plane} latch"
        );
    }
    state.write_mode = 0;
    state.bit_mask = 0x0F;
    mem.apply_vga_pipeline(&state);
    mem.write_u8(VIDEO_BASE + 2, 0xAB);
    for (plane, byte) in source.into_iter().enumerate() {
        assert_eq!(mem.plane(plane)[2], (byte & 0xF0) | 0x0B);
    }
    for write_mode in [2, 3] {
        state.write_mode = write_mode;
        state.set_reset = 0x0A;
        mem.apply_vga_pipeline(&state);
        let data = if write_mode == 2 { 0x0A } else { 0x0F };
        mem.write_u8(VIDEO_BASE + 3, data);
        for (plane, byte) in source.into_iter().enumerate() {
            let low = if 0x0A & (1 << plane) != 0 { 0x0F } else { 0 };
            assert_eq!(mem.plane(plane)[3], (byte & 0xF0) | low);
        }
    }
    state.read_mode = 1;
    state.color_compare = 0;
    state.color_dont_care = 0x0F;
    for read_plane in 0..4 {
        state.read_plane = read_plane;
        mem.apply_vga_pipeline(&state);
        assert_eq!(
            mem.read_u8(VIDEO_BASE),
            !(source[0] | source[1] | source[2] | source[3])
        );
    }
    state.memory_map_select = 3;
    state.seq_odd_even_disabled = false;
    state.gc_odd_even_read = true;
    mem.apply_vga_pipeline(&state);
    mem.write_u16(TEXT_VIDEO_BASE_COLOR, 0x0741);
    assert_eq!(mem.read_u16(TEXT_VIDEO_BASE_COLOR), 0x0741);
    assert_eq!(mem.ram[TEXT_VIDEO_BASE_COLOR], 0x41);
    assert_eq!(mem.plane(2)[0], source[2]);
}

#[test]
fn test_text_mode_font_plane_write_routes_to_plane2() {
    let mut mem = Memory::new();
    let mut state = mem.vga;
    state.video_mode = 0x03;
    state.chain4 = false;
    state.seq_odd_even_disabled = true;
    state.gc_odd_even_read = false;
    state.is_mode_x = false;
    state.plane_mask = 0x04;
    state.read_plane = 2;
    state.memory_map_select = 1;
    state.gc_graphics_mode = false;
    mem.apply_vga_pipeline(&state);

    // This is a transient font-memory mapping, not a display-mode
    // transition, so the cached display handler must stay linear.
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Linear);

    let offset = 0x0410usize;
    mem.ram[VIDEO_BASE + offset] = 0x11;
    mem.write_u8(VIDEO_BASE + offset, 0x5A);

    assert_eq!(mem.plane(0)[offset], 0x00);
    assert_eq!(mem.plane(1)[offset], 0x00);
    assert_eq!(mem.plane(2)[offset], 0x5A);
    assert_eq!(mem.plane(3)[offset], 0x00);
    assert_eq!(
        mem.ram[VIDEO_BASE + offset],
        0x11,
        "font-plane writes must not fall back to linear A000 RAM"
    );
    assert!(mem.planar_font_latch_pending());
}

#[test]
fn test_text_mode_font_plane_read_uses_read_plane_two() {
    let mut mem = Memory::new();
    let mut state = mem.vga;
    state.video_mode = 0x03;
    state.chain4 = false;
    state.seq_odd_even_disabled = true;
    state.gc_odd_even_read = false;
    state.is_mode_x = false;
    state.plane_mask = 0x00;
    state.read_plane = 2;
    state.memory_map_select = 1;
    state.gc_graphics_mode = false;
    mem.apply_vga_pipeline(&state);

    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Linear);

    let offset = 0x0123usize;
    mem.plane_mut(2)[offset] = 0xA7;
    mem.ram[VIDEO_BASE + offset] = 0x11;

    assert_eq!(mem.read_u8(VIDEO_BASE + offset), 0xA7);
    assert_eq!(mem.ram[VIDEO_BASE + offset], 0x11);
    assert!(!mem.planar_font_latch_pending());
}

#[test]
fn test_text_mode_b800_write_stays_linear_with_plane2_mask() {
    let mut mem = Memory::new();
    let mut state = mem.vga;
    state.video_mode = 0x03;
    state.chain4 = false;
    state.is_mode_x = false;
    state.plane_mask = 0x04;
    state.read_plane = 2;
    state.memory_map_select = 3;
    state.gc_graphics_mode = false;
    mem.apply_vga_pipeline(&state);

    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Linear);

    let text_addr = TEXT_VIDEO_BASE_COLOR;
    mem.plane_mut(2)[0] = 0x22;
    mem.write_u8(text_addr, 0x41);

    assert_eq!(mem.ram[text_addr], 0x41);
    assert_eq!(
        mem.plane(2)[0],
        0x22,
        "visible B800 text writes must not be mistaken for font-plane writes"
    );
    assert!(!mem.planar_font_latch_pending());
}

#[test]
fn text_planar_routing_graphics_write_does_not_mark_font() {
    let mut mem = Memory::new();
    let mut state = mem.vga;
    state.video_mode = 0x03;
    state.chain4 = false;
    state.seq_odd_even_disabled = true;
    state.gc_odd_even_read = false;
    state.is_mode_x = false;
    state.plane_mask = 0x04;
    state.read_plane = 2;
    state.memory_map_select = 1;
    state.gc_graphics_mode = true;
    mem.apply_vga_pipeline(&state);

    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Linear);

    let offset = 0x0200usize;
    mem.plane_mut(2)[offset] = 0x22;
    mem.write_u8(VIDEO_BASE + offset, 0x5A);

    assert_eq!(mem.ram[VIDEO_BASE + offset], 0x00);
    assert_eq!(
        mem.plane(2)[offset],
        0x5A,
        "CPU sequential addressing routes graphics writes to enabled planes"
    );
    assert!(!mem.planar_font_latch_pending());
}

#[test]
fn test_text_mode_a000_plane2_read_with_graphics_bit_uses_ram() {
    let mut mem = Memory::new();
    let mut state = mem.vga;
    state.video_mode = 0x03;
    state.chain4 = false;
    state.is_mode_x = false;
    state.plane_mask = 0x00;
    state.read_plane = 2;
    state.memory_map_select = 1;
    state.gc_graphics_mode = true;
    mem.apply_vga_pipeline(&state);

    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Linear);

    let offset = 0x0201usize;
    mem.plane_mut(2)[offset] = 0xA7;
    mem.ram[VIDEO_BASE + offset] = 0x11;

    assert_eq!(mem.read_u8(VIDEO_BASE + offset), 0x11);
    assert!(!mem.planar_font_latch_pending());
}

#[test]
fn test_graphics_planar_plane2_write_does_not_latch_font() {
    let mut mem = Memory::new();
    let mut state = mem.vga;
    state.video_mode = 0x12;
    state.chain4 = false;
    state.is_mode_x = false;
    state.plane_mask = 0x04;
    state.read_plane = 2;
    state.memory_map_select = 1;
    state.gc_graphics_mode = true;
    mem.apply_vga_pipeline(&state);

    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Planar);

    let offset = 0x0202usize;
    mem.write_u8(VIDEO_BASE + offset, 0x5A);

    assert_eq!(mem.plane(2)[offset], 0x5A);
    assert!(
        !mem.planar_font_latch_pending(),
        "graphics-mode plane-2 writes are not font uploads"
    );
}

// ========== read_u8 out-of-bounds returns 0xFF ==========

#[test]
fn test_read_u8_above_ram_returns_0xff() {
    let mem = Memory::new();
    // Read way beyond usable_len with A20 enabled
    // A20 is enabled by default, MAX_RAM_SIZE+some -> masked stays large
    let val = mem.read_u8(MAX_RAM_SIZE + 0x100);
    assert_eq!(val, 0xFF);
}

// ========== LFB Tests ==========

#[test]
fn test_lfb_lifecycle() {
    let mut mem = Memory::new();
    assert!(!mem.is_lfb_enabled());
    assert!(mem.lfb().is_empty());

    mem.init_lfb(1024 * 1024); // 1MB
    assert_eq!(mem.lfb().len(), 1024 * 1024);
    assert!(!mem.is_lfb_enabled());

    mem.set_lfb_enabled(true);
    assert!(mem.is_lfb_enabled());

    // Write and read through LFB
    mem.lfb_write_u8(0, 0xAB);
    assert_eq!(mem.lfb_read_u8(0), 0xAB);

    // Write/read via write_u8/read_u8 at LFB address
    mem.write_u8(LFB_BASE + 100, 0xCD);
    assert_eq!(mem.read_u8(LFB_BASE + 100), 0xCD);

    // Out-of-bounds read returns 0
    assert_eq!(mem.lfb_read_u8(1024 * 1024 + 10), 0);

    mem.set_lfb_enabled(false);
}

#[test]
fn test_lfb_mut_access() {
    let mut mem = Memory::new();
    mem.init_lfb(256);
    {
        let buf = mem.lfb_mut();
        buf[0] = 0xEF;
    }
    assert_eq!(mem.lfb()[0], 0xEF);
}

// ========== A20 gate tests ==========

#[test]
fn test_a20_gate() {
    let mut mem = Memory::new();
    assert!(mem.a20_enabled());

    mem.set_a20(false);
    assert!(!mem.a20_enabled());

    // With A20 disabled, bit 20 is cleared
    // Write to address with bit 20 set should wrap
    mem.write_u8(A20_BIT + 0x100, 0xAA);
    // Should be readable at 0x100 (bit 20 cleared)
    assert_eq!(mem.read_u8(0x100), 0xAA);

    mem.set_a20(true);
}

// ========== read_byte_checked ==========

#[test]
fn test_read_byte_checked_coverage() {
    let mut mem = Memory::new();
    mem.write_u8(0x1234, 0x42);
    assert_eq!(mem.read_byte_checked(0x1234), Some(0x42));

    // Out of bounds should return None
    assert!(mem.read_byte_checked(MAX_RAM_SIZE + 0x1000).is_none());
}

// ========== reset_ram ==========

#[test]
fn test_reset_ram_restores_bda() {
    let mut mem = Memory::new();
    mem.write_u8(0x50000, 0xAB);

    mem.reset_ram();

    // User data should be cleared
    assert_eq!(mem.read_u8(0x50000), 0);
    // BDA should be re-initialized
    assert_eq!(mem.read_u8(0x410), 0x21);
    assert_eq!(mem.read_u8(0x449), 0x03);
    // IRET stub should be restored
    let iret_addr = Memory::seg_offset_to_linear(0xF000, 0xFF00);
    assert_eq!(mem.read_u8(iret_addr), 0xCF);
}

// ========== VGA mode / handler tests ==========

#[test]
fn test_vga_write_handler_text_mode() {
    let mut mem = Memory::new();
    mem.set_video_mode(0x03);
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Linear);
}

#[test]
fn test_vga_write_handler_mode13h() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.update_vga_write_handler();
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Chain4);
}

#[test]
fn test_vga_write_handler_mode_x() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = false;
    mem.vga.is_mode_x = true; // confirmed Mode X
    mem.update_vga_write_handler();
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Planar);
}

#[test]
fn test_vga_write_handler_planar_modes() {
    let mut mem = Memory::new();
    for mode in [0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x12] {
        mem.vga.video_mode = mode;
        mem.vga.chain4 = false;
        mem.update_vga_write_handler();
        assert_eq!(
            mem.vga_write_handler(),
            VgaWriteHandler::Planar,
            "mode {:#04X} should be planar",
            mode
        );
    }
}

// ========== VGA planar write modes ==========

#[test]
fn test_planar_write_mode1_latch_copy() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.plane_mask = 0x0F;
    mem.vga.write_mode = 1;
    mem.update_vga_write_handler();

    // Set latches with known values
    mem.set_latches([0x11, 0x22, 0x33, 0x44]);

    // Write mode 1 copies latches — CPU data is ignored
    mem.write_planar_ega_masked(0, 0xFF, 0x0F);

    assert_eq!(mem.planes_flat[0], 0x11);
    assert_eq!(mem.planes_flat[PLANE_SIZE], 0x22);
    assert_eq!(mem.planes_flat[PLANE_SIZE * 2], 0x33);
    assert_eq!(mem.planes_flat[PLANE_SIZE * 3], 0x44);
}

#[test]
fn test_planar_write_mode2_color_expansion() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.plane_mask = 0x0F;
    mem.vga.write_mode = 2;
    mem.vga.bit_mask = 0xFF;
    mem.vga.data_rotate = 0; // No logical op
    mem.set_latches([0x00, 0x00, 0x00, 0x00]);
    mem.update_vga_write_handler();

    // CPU data 0x05 = bits 0 and 2 set → planes 0 and 2 get 0xFF
    mem.write_planar_ega_masked(0, 0x05, 0x0F);

    assert_eq!(mem.planes_flat[0], 0xFF); // plane 0: bit 0 set
    assert_eq!(mem.planes_flat[PLANE_SIZE], 0x00); // plane 1: bit 1 clear
    assert_eq!(mem.planes_flat[PLANE_SIZE * 2], 0xFF); // plane 2: bit 2 set
    assert_eq!(mem.planes_flat[PLANE_SIZE * 3], 0x00); // plane 3: bit 3 clear
}

#[test]
fn test_planar_write_mode3_set_reset_mask() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.plane_mask = 0x0F;
    mem.vga.write_mode = 3;
    mem.vga.bit_mask = 0xFF;
    mem.vga.set_reset = 0x0F; // All 1s
    mem.vga.data_rotate = 0;
    mem.set_latches([0x00, 0x00, 0x00, 0x00]);
    mem.update_vga_write_handler();

    // CPU data 0xFF, bit_mask 0xFF → effective_mask = 0xFF
    // set_reset = 0x0F → all planes get 0xFF where mask is 1
    mem.write_planar_ega_masked(0, 0xFF, 0x0F);

    for p in 0..4 {
        assert_eq!(
            mem.planes_flat[p * PLANE_SIZE],
            0xFF,
            "plane {} should be 0xFF",
            p
        );
    }
}

#[test]
fn test_planar_write_mode0_logical_ops() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.plane_mask = 0x0F;
    mem.vga.write_mode = 0;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.vga.set_reset = 0;
    mem.update_vga_write_handler();

    // AND logical op: data_rotate bits 3-4 = 01
    mem.set_latches([0xF0, 0xF0, 0xF0, 0xF0]);
    mem.vga.data_rotate = 0x08; // logical op = 1 (AND)
    mem.write_planar_ega_masked(0, 0x0F, 0x0F);
    assert_eq!(mem.planes_flat[0], 0x00); // 0x0F & 0xF0 = 0x00

    // OR logical op: data_rotate bits 3-4 = 10
    mem.set_latches([0xF0, 0xF0, 0xF0, 0xF0]);
    mem.vga.data_rotate = 0x10; // logical op = 2 (OR)
    mem.write_planar_ega_masked(1, 0x0F, 0x0F);
    assert_eq!(mem.planes_flat[1], 0xFF); // 0x0F | 0xF0 = 0xFF

    // XOR logical op: data_rotate bits 3-4 = 11
    mem.set_latches([0xF0, 0xF0, 0xF0, 0xF0]);
    mem.vga.data_rotate = 0x18; // logical op = 3 (XOR)
    mem.write_planar_ega_masked(2, 0x0F, 0x0F);
    assert_eq!(mem.planes_flat[2], 0xFF); // 0x0F ^ 0xF0 = 0xFF
}

#[test]
fn test_planar_write_mode2_with_logical_ops() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.plane_mask = 0x01; // Plane 0 only
    mem.vga.write_mode = 2;
    mem.vga.bit_mask = 0xFF;
    mem.update_vga_write_handler();

    // OR logical op with latch
    mem.set_latches([0x0F, 0, 0, 0]);
    mem.vga.data_rotate = 0x10; // OR
                                // cpu_data bit 0 = 1 → expanded = 0xFF, 0xFF | 0x0F = 0xFF
    mem.write_planar_ega_masked(0, 0x01, 0x01);
    assert_eq!(mem.planes_flat[0], 0xFF);
}

#[test]
fn test_is_planar_mode_all_ranges() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    assert!(mem.is_planar_mode());

    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = false;
    mem.vga.is_mode_x = true; // CR9/CR17 confirmed real Mode X
    assert!(mem.is_planar_mode()); // Mode X

    mem.vga.chain4 = true;
    mem.vga.is_mode_x = false;
    assert!(!mem.is_planar_mode()); // Normal 13h
}

// ========== VGA memory map select ==========

#[test]
fn test_vga_memory_map_select_mono() {
    let mut mem = Memory::new();
    mem.set_vga_memory_map_select(2); // B0000-B7FFF
    let (base, size) = mem.vga_memory_window();
    assert_eq!(base, TEXT_VIDEO_BASE_MONO);
    assert_eq!(size, VIDEO_WINDOW_32K);
}

// ========== apply_vga_pipeline chain4 transitions ==========

#[test]
fn test_apply_vga_pipeline_chain4_to_planar_v2() {
    let mut mem = Memory::new();
    // Start in mode 13h chain4
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    // Write some data to linear video memory
    mem.ram[VIDEO_BASE] = 0x11;
    mem.ram[VIDEO_BASE + 1] = 0x22;
    mem.ram[VIDEO_BASE + 2] = 0x33;
    mem.ram[VIDEO_BASE + 3] = 0x44;

    // Transition to real Mode X (chain4=false, is_mode_x=true).
    let mut state = mem.vga;
    state.chain4 = false;
    state.is_mode_x = true;
    mem.apply_vga_pipeline(&state);

    // After sync_chain4_to_planes, planes should have the data
    assert_eq!(mem.plane(0)[0], 0x11);
    assert_eq!(mem.plane(1)[0], 0x22);
    assert_eq!(mem.plane(2)[0], 0x33);
    assert_eq!(mem.plane(3)[0], 0x44);
    assert!(mem.is_planar_mode());
}

#[test]
fn test_apply_vga_pipeline_planar_to_chain4_v2() {
    let mut mem = Memory::new();
    // Start in real Mode X (chain4 disabled, is_mode_x set).
    // update_vga_write_handler() syncs the cached handler so that the
    // next pipeline apply sees Planar→X, not Linear→X.
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = false;
    mem.vga.is_mode_x = true;
    mem.update_vga_write_handler();
    // Write some data to planes
    mem.plane_mut(0)[0] = 0xAA;
    mem.plane_mut(1)[0] = 0xBB;
    mem.plane_mut(2)[0] = 0xCC;
    mem.plane_mut(3)[0] = 0xDD;

    // Transition back to chain4 (Mode X exit clears is_mode_x).
    let mut state = mem.vga;
    state.chain4 = true;
    state.is_mode_x = false;
    mem.apply_vga_pipeline(&state);

    // After sync_planes_to_chain4, linear video should have the data
    assert_eq!(mem.ram[VIDEO_BASE], 0xAA);
    assert_eq!(mem.ram[VIDEO_BASE + 1], 0xBB);
    assert_eq!(mem.ram[VIDEO_BASE + 2], 0xCC);
    assert_eq!(mem.ram[VIDEO_BASE + 3], 0xDD);
}

#[test]
fn test_mode13h_bios_clear_after_text_mode() {
    let mut mem = Memory::new();

    // Step 1: Simulate text mode 03h state
    let text_state = VgaPipelineState {
        video_mode: 0x03,
        chain4: false,
        is_mode_x: false,
        plane_mask: 0x03, // text mode: planes 0+1
        read_plane: 0,
        memory_map_select: 3, // B8000-BFFFF
        seq_odd_even_disabled: false,
        gc_odd_even_read: true,
        gc_graphics_mode: false,
        write_mode: 0,
        bit_mask: 0xFF,
        set_reset: 0,
        enable_set_reset: 0,
        data_rotate: 0,
        read_mode: 0,
        color_compare: 0,
        color_dont_care: 0x0F,
    };
    mem.apply_vga_pipeline(&text_state);

    // Dirty planes_flat to simulate font data / prior writes
    // (real VGA BIOS loads font glyphs into plane 2 during text mode init)
    for i in 0..8192 {
        mem.plane_mut(0)[i] = (i & 0xFF) as u8;
        mem.plane_mut(1)[i] = ((i >> 2) & 0xFF) as u8;
        mem.plane_mut(2)[i] = 0xAA; // font data pattern
        mem.plane_mut(3)[i] = 0x55;
    }

    // Step 2: Transition to mode 13h (chain4=true)
    // This triggers sync_planes_to_chain4() which copies planes → ram
    let mode13_state = VgaPipelineState {
        video_mode: 0x13,
        chain4: true,
        is_mode_x: false,
        plane_mask: 0x0F, // all planes
        read_plane: 0,
        memory_map_select: 1, // A0000-AFFFF (64K)
        seq_odd_even_disabled: true,
        gc_odd_even_read: false,
        gc_graphics_mode: true,
        write_mode: 0,
        bit_mask: 0xFF,
        set_reset: 0,
        enable_set_reset: 0,
        data_rotate: 0,
        read_mode: 0,
        color_compare: 0,
        color_dont_care: 0x0F,
    };
    // Pre-fill ram with non-zero so the precondition is observable.
    // (After text-mode-Linear writes, ram would be whatever the BIOS
    // wrote; we're asserting the *clear* zeroes everything regardless.)
    for i in 0..64000 {
        mem.ram[VIDEO_BASE + i] = 0xCC;
    }
    mem.apply_vga_pipeline(&mode13_state);

    // Step 3: BIOS-style mode 13h clear (exactly like int10.rs lines 126-130)
    for i in 0..64000usize {
        mem.write_u8(VIDEO_BASE + i, 0);
    }

    // Step 4: Verify ALL 64000 bytes are zero
    let mut first_nonzero = None;
    let mut nonzero_count = 0usize;
    for i in 0..64000usize {
        if mem.ram[VIDEO_BASE + i] != 0 {
            nonzero_count += 1;
            if first_nonzero.is_none() {
                first_nonzero = Some((i, mem.ram[VIDEO_BASE + i]));
            }
        }
    }
    if let Some((idx, val)) = first_nonzero {
        panic!(
            "Mode 13h BIOS clear left {nonzero_count} non-zero bytes! \
             First at offset {idx:#06X} (plane {}, plane_offset {}): value {val:#04X}",
            idx & 3,
            idx & !3
        );
    }
}

/// Same test but verify that planes_flat is also zeroed (not just ram).
#[test]
fn test_mode13h_bios_clear_planes_match_ram() {
    let mut mem = Memory::new();

    // Start in text mode, dirty planes
    let text_state = VgaPipelineState {
        video_mode: 0x03,
        chain4: false,
        is_mode_x: false,
        plane_mask: 0x03,
        read_plane: 0,
        memory_map_select: 3,
        seq_odd_even_disabled: false,
        gc_odd_even_read: true,
        gc_graphics_mode: false,
        write_mode: 0,
        bit_mask: 0xFF,
        set_reset: 0,
        enable_set_reset: 0,
        data_rotate: 0,
        read_mode: 0,
        color_compare: 0,
        color_dont_care: 0x0F,
    };
    mem.apply_vga_pipeline(&text_state);
    for i in 0..16000 {
        for plane in 0..4 {
            mem.plane_mut(plane)[i] = 0xDE;
        }
    }

    // Transition to mode 13h
    let mode13_state = VgaPipelineState {
        video_mode: 0x13,
        chain4: true,
        is_mode_x: false,
        plane_mask: 0x0F,
        read_plane: 0,
        memory_map_select: 1,
        seq_odd_even_disabled: true,
        gc_odd_even_read: false,
        gc_graphics_mode: true,
        write_mode: 0,
        bit_mask: 0xFF,
        set_reset: 0,
        enable_set_reset: 0,
        data_rotate: 0,
        read_mode: 0,
        color_compare: 0,
        color_dont_care: 0x0F,
    };
    mem.apply_vga_pipeline(&mode13_state);

    // BIOS clear
    for i in 0..64000usize {
        mem.write_u8(VIDEO_BASE + i, 0);
    }

    // Verify both ram AND planes_flat are zero for the rendered region
    for i in 0..64000usize {
        let plane = i & 3;
        // Chain-4 mapping maps each byte x to cell x&~3.
        let plane_offset = i & !3;
        let ram_val = mem.ram[VIDEO_BASE + i];
        let plane_val = mem.plane(plane)[plane_offset];
        assert_eq!(
            ram_val, 0,
            "ram[A0000+{i:#X}] = {ram_val:#04X} (expected 0)"
        );
        assert_eq!(
            plane_val, 0,
            "plane[{plane}][{plane_offset:#X}] = {plane_val:#04X} (expected 0)"
        );
    }
}

// ========== read_u64 / write_u64 ==========

#[test]
fn test_read_write_u64() {
    let mut mem = Memory::new();
    mem.write_u64(0x10000, 0xDEAD_BEEF_CAFE_BABE);
    assert_eq!(mem.read_u64(0x10000), 0xDEAD_BEEF_CAFE_BABE);
}

// ========== read/write_u32_physical ==========

#[test]
fn test_read_write_u32_physical() {
    let mut mem = Memory::new();
    mem.write_u32_physical(0x10000, 0x12345678);
    assert_eq!(mem.read_u32_physical(0x10000), 0x12345678);

    // Out of bounds should return 0 and not panic
    assert_eq!(mem.read_u32_physical(MAX_RAM_SIZE + 0x100), 0);
}

// ========== PendingWrite constructors ==========

#[test]
fn test_pending_write_basic_ctor() {
    let pw = PendingWrite::new(0x1000, 0x2000, 0x3000, 0xAA, 0xBB, 1);
    assert_eq!(pw.source_cs, 0x1000);
    assert_eq!(pw.source_ip, 0x2000);
    assert_eq!(pw.linear_address, 0x3000);
    assert_eq!(pw.old_value, 0xAA);
    assert_eq!(pw.new_value, 0xBB);
    assert_eq!(pw.size, 1);
    assert!(!pw.is_hle_handler);
    assert!(!pw.is_vga_write);
    assert!(pw.vga_plane.is_none());
}

#[test]
fn test_pending_write_hle_ctor() {
    let pw = PendingWrite::new_with_hle(0x100, 0x200, 0x300, 0, 1, 2, true);
    assert!(pw.is_hle_handler);
    assert!(!pw.is_vga_write);
}

#[test]
fn test_pending_write_vga_ctor() {
    let pw = PendingWrite::new_with_vga(0x100, 0x200, 0x300, 0xAA, 0xBB, 1, 2);
    assert!(pw.is_vga_write);
    assert_eq!(pw.vga_plane, Some(2));
    assert!(!pw.is_hle_handler);
}

// ========== clear_page_fault ==========

#[test]
fn test_clear_page_fault() {
    let mut mem = Memory::new();
    mem.page_fault_pending = true;
    mem.page_fault_address = 0xDEAD;
    mem.page_fault_error_code = 0x07;

    mem.clear_page_fault();
    assert!(!mem.page_fault_pending);
    assert_eq!(mem.page_fault_address, 0);
    assert_eq!(mem.page_fault_error_code, 0);
}

// ========== video_write_count / video_dirty_pages ==========

#[test]
fn test_video_write_count() {
    let mut mem = Memory::new();
    assert_eq!(mem.video_write_count(), 0);

    // Write to video memory in text mode (goes through slow path)
    mem.write_u8(VIDEO_BASE, 0x41);
    assert!(mem.video_write_count() > 0);
}

#[test]
fn test_video_dirty_pages() {
    let mem = Memory::new();
    assert!(!mem.has_dirty_video_pages());

    // Mark a page dirty
    mem.mark_video_page_dirty(VIDEO_BASE);
    assert!(mem.has_dirty_video_pages());

    // Take clears
    let pages = mem.take_video_dirty_pages();
    assert!(pages & 1 != 0); // Page 0 dirty
    assert!(!mem.has_dirty_video_pages());
}

#[test]
fn test_mark_video_page_dirty_out_of_range() {
    let mem = Memory::new();
    // Should not panic for addresses outside video range
    mem.mark_video_page_dirty(0x50000);
    assert!(!mem.has_dirty_video_pages());
}

// ========== usable_len and ram_ptr ==========

#[test]
fn test_usable_len() {
    let mem = Memory::new();
    assert_eq!(mem.usable_len(), DEFAULT_RAM_SIZE);
    assert_eq!(mem.ram.len(), DEFAULT_RAM_SIZE + FETCH_PADDING);
    assert!(mem.ram.capacity() >= DOS_RAM_CEILING + FETCH_PADDING);
}

// ========== bulk_copy_ram edge cases ==========

#[test]
fn test_bulk_copy_ram_zero_len() {
    let mut mem = Memory::new();
    assert!(mem.bulk_copy_ram(0x1000, 0x2000, 0));
}

#[test]
fn test_bulk_copy_ram_beyond_ram() {
    let mut mem = Memory::new();
    // src beyond RAM length
    let huge = mem.ram.len() + 100;
    assert!(mem.bulk_copy_ram(huge, 0x1000, 16));
}

// ========== bulk_read_ram edge cases ==========

#[test]
fn test_bulk_read_ram_zero_len() {
    let mem = Memory::new();
    let mut buf = [0u8; 4];
    assert!(mem.bulk_read_ram(0x1000, 0, &mut buf));
}

#[test]
fn test_bulk_read_ram_empty_dest() {
    let mem = Memory::new();
    let mut buf: [u8; 0] = [];
    assert!(mem.bulk_read_ram(0x1000, 4, &mut buf));
}

#[test]
fn test_bulk_read_ram_beyond_ram() {
    let mem = Memory::new();
    let mut buf = [0u8; 4];
    let huge = mem.ram.len() + 100;
    assert!(mem.bulk_read_ram(huge, 4, &mut buf));
}

// ========== Planar write mode 1 through write_u8 ==========

#[test]
fn test_planar_write_mode1() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.write_mode = 1;
    mem.vga.plane_mask = 0x0F;
    mem.update_vga_write_handler();

    // Set latches by reading from planar memory
    mem.planes_flat[0] = 0x11; // plane 0
    mem.planes_flat[PLANE_SIZE] = 0x22; // plane 1
    mem.planes_flat[PLANE_SIZE * 2] = 0x33; // plane 2
    mem.planes_flat[PLANE_SIZE * 3] = 0x44; // plane 3
    mem.set_vga_memory_map_select(0); // A0000-BFFFF
    let _ = mem.read_u8(VIDEO_BASE); // loads latches

    // Write mode 1: latches are copied to destination, CPU data ignored
    mem.write_u8(VIDEO_BASE + 1, 0xFF); // CPU data doesn't matter

    assert_eq!(mem.planes_flat[1], 0x11); // latch[0]
    assert_eq!(mem.planes_flat[PLANE_SIZE + 1], 0x22); // latch[1]
    assert_eq!(mem.planes_flat[PLANE_SIZE * 2 + 1], 0x33); // latch[2]
    assert_eq!(mem.planes_flat[PLANE_SIZE * 3 + 1], 0x44); // latch[3]
}

// ========== EGA planar write mode 1 with trace subscriber ==========

#[test]
fn test_planar_write_mode1_with_trace() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.write_mode = 1;
    mem.vga.plane_mask = 0x0F;
    mem.update_vga_write_handler();
    mem.set_vga_memory_map_select(0);

    with_trace_subscriber(|| {
        mem.write_u8(VIDEO_BASE, 0x00);
    });
}

// ========== EGA write: write_planar_ega_masked with trace ==========

#[test]
fn test_write_planar_ega_masked_with_trace() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.write_mode = 0;
    mem.vga.plane_mask = 0x0F;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.vga.data_rotate = 0;
    mem.update_vga_write_handler();
    mem.set_vga_memory_map_select(0);

    with_trace_subscriber(|| {
        mem.write_u8(VIDEO_BASE, 0xAB);
    });

    assert_eq!(mem.planes_flat[0], 0xAB);
}

// ========== Planar read mode 1 (color compare) ==========

#[test]
fn test_planar_read_mode1_color_compare() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.read_mode = 1;
    mem.vga.color_compare = 0x05; // bits 0,2 set
    mem.vga.color_dont_care = 0x0F; // All planes matter
    mem.vga.memory_map_select = 1; // A0000-AFFFF

    // Set plane data: pixel where planes 0 and 2 are 0xFF, 1 and 3 are 0x00
    // This matches color_compare = 0x05 (bits 0 and 2)
    mem.planes_flat[42] = 0xFF; // plane 0
    mem.planes_flat[PLANE_SIZE + 42] = 0x00; // plane 1
    mem.planes_flat[PLANE_SIZE * 2 + 42] = 0xFF; // plane 2
    mem.planes_flat[PLANE_SIZE * 3 + 42] = 0x00; // plane 3

    let val = mem.read_u8(VIDEO_BASE + 42);
    assert_eq!(val, 0xFF); // All 8 pixels match
}

#[test]
fn test_planar_read_mode1() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.read_mode = 1;
    mem.vga.color_compare = 0x05; // Planes 0 and 2
    mem.vga.color_dont_care = 0x0F; // All planes matter
    mem.set_vga_memory_map_select(0);

    // Set plane data: pixel pattern where planes 0,2 are 0xFF (matching)
    mem.planes_flat[0] = 0xFF; // plane 0 = all 1s (match)
    mem.planes_flat[PLANE_SIZE] = 0x00; // plane 1 = all 0s (match compare bit 0)
    mem.planes_flat[PLANE_SIZE * 2] = 0xFF; // plane 2 = all 1s (match)
    mem.planes_flat[PLANE_SIZE * 3] = 0x00; // plane 3 = all 0s (match)

    let val = mem.read_u8(VIDEO_BASE);
    // All pixels should match: result = 0xFF
    assert_eq!(val, 0xFF);
}

// ========== Chain4 read mode 1 ==========

#[test]
fn test_chain4_read_mode1_color_compare() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.read_mode = 1;
    mem.vga.color_compare = 0x03; // bits 0,1
    mem.vga.color_dont_care = 0x0F;
    mem.vga.memory_map_select = 1; // A0000-AFFFF

    let cell = 40;
    // All planes 0xFF → color = 0x0F per pixel, compare to 0x03 → mismatch on planes 2,3
    mem.planes_flat[cell] = 0xFF;
    mem.planes_flat[PLANE_SIZE + cell] = 0xFF;
    mem.planes_flat[PLANE_SIZE * 2 + cell] = 0xFF;
    mem.planes_flat[PLANE_SIZE * 3 + cell] = 0xFF;

    let addr = VIDEO_BASE + cell; // x&~3 is cell; x&3 selects plane 0
    let val = mem.read_u8(addr);
    // Planes 2,3 are 0xFF but compare expects 0 → mismatch bits
    assert_eq!(val, 0x00);
}

#[test]
fn test_chain4_read_mode1() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.read_mode = 1;
    mem.vga.color_compare = 0x0F; // All planes set
    mem.vga.color_dont_care = 0x0F;

    let plane_offset = 0usize;
    mem.plane_mut(0)[plane_offset] = 0xFF;
    mem.plane_mut(1)[plane_offset] = 0xFF;
    mem.plane_mut(2)[plane_offset] = 0xFF;
    mem.plane_mut(3)[plane_offset] = 0xFF;

    // Read at offset 0, plane 0
    let val = mem.read_u8(VIDEO_BASE);
    assert_eq!(val, 0xFF); // All planes match all-1 compare
}

#[test]
fn test_vga_memory_window_select() {
    let mut mem = Memory::new();

    mem.vga.memory_map_select = 0;
    assert_eq!(mem.vga_memory_window(), (VIDEO_BASE, VIDEO_SIZE));

    mem.vga.memory_map_select = 1;
    assert_eq!(mem.vga_memory_window(), (VIDEO_BASE, 64 * 1024));

    mem.vga.memory_map_select = 2;
    assert_eq!(mem.vga_memory_window(), (TEXT_VIDEO_BASE_MONO, 32 * 1024));

    mem.vga.memory_map_select = 3;
    assert_eq!(mem.vga_memory_window(), (TEXT_VIDEO_BASE_COLOR, 32 * 1024));
}

#[test]
fn test_vga_window_offset_inside() {
    let mut mem = Memory::new();
    mem.vga.memory_map_select = 1; // A0000-AFFFF (64K)
    assert_eq!(mem.vga_window_offset(VIDEO_BASE), Some(0));
    assert_eq!(mem.vga_window_offset(VIDEO_BASE + 0x100), Some(0x100));
}

#[test]
fn test_vga_window_offset_outside() {
    let mut mem = Memory::new();
    mem.vga.memory_map_select = 1; // A0000-AFFFF
                                   // B8000 is outside the A0000-AFFFF window
    assert_eq!(mem.vga_window_offset(0xB8000), None);
}

// ========== TLB insert with host pointers ==========

#[test]
fn test_tlb_insert_vga_page_gets_null_pointers() {
    let mut mem = Memory::new();
    // Insert for a VGA-range physical page (0xA0..0xC0)
    mem.tlb_insert(0x50, 0xA5, true, true, true);
    let idx = SoftwareTlb::index(0x50);
    let entry = &mem.core.tlb().entries[idx];
    assert_eq!(entry.linear_page, 0x50);
    assert!(
        entry.host_read.is_null(),
        "VGA page should have null host_read"
    );
    assert!(
        entry.host_write.is_null(),
        "VGA page should have null host_write"
    );
}

#[test]
fn test_tlb_insert_non_writable_gets_null_write_pointer() {
    let mut mem = Memory::new();
    // Read-only page (writable=false)
    mem.tlb_insert(0x10, 0x10, false, true, false);
    let idx = SoftwareTlb::index(0x10);
    let entry = &mem.core.tlb().entries[idx];
    assert!(
        !entry.host_read.is_null(),
        "readable page should have host_read"
    );
    assert!(
        entry.host_write.is_null(),
        "read-only page should have null host_write"
    );
}

#[test]
fn test_tlb_insert_writable_dirty_gets_write_pointer() {
    let mut mem = Memory::new();
    // Writable + dirty page should get a host_write pointer
    mem.tlb_insert(0x20, 0x20, true, true, true);
    let idx = SoftwareTlb::index(0x20);
    let entry = &mem.core.tlb().entries[idx];
    assert!(!entry.host_read.is_null());
    assert!(
        !entry.host_write.is_null(),
        "writable+dirty should have host_write"
    );
}

#[test]
fn test_tlb_insert_writable_not_dirty_null_write() {
    let mut mem = Memory::new();
    // Writable but not dirty → no host_write
    mem.tlb_insert(0x30, 0x30, true, true, false);
    let idx = SoftwareTlb::index(0x30);
    let entry = &mem.core.tlb().entries[idx];
    assert!(!entry.host_read.is_null());
    assert!(
        entry.host_write.is_null(),
        "writable but not dirty should have null host_write"
    );
}

// ========== TLB stats: miss_rate ==========

#[test]
fn test_tlb_stats_miss_rate_with_lookups() {
    let mut stats = TlbStats::disabled();
    stats.enable();
    stats.record_hit();
    stats.record_miss();
    stats.record_miss();
    stats.record_miss();
    // 3 misses out of 4 total = 75%
    assert!((stats.miss_rate() - 75.0).abs() < 0.01);
}

#[test]
fn test_tlb_stats_miss_rate_zero_lookups() {
    let stats = TlbStats::disabled();
    assert_eq!(stats.miss_rate(), 0.0);
}
