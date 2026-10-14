use super::*;

#[test]
fn test_write_pixel_mode0d() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0D);
    vga.write_pixel(0, 0, 0, 15, &mut memory);
    // All 4 planes should have bit 7 set
    for plane_idx in 0..4 {
        let plane = memory.plane(plane_idx);
        assert_eq!(plane[0] & 0x80, 0x80);
    }
}

#[test]
fn test_write_pixel_mode0e() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0E);
    vga.write_pixel(0, 0, 0, 10, &mut memory); // Color 10 = 0b1010
                                               // Planes 1 and 3 should have bit 7 set
    assert_eq!(memory.plane(1)[0] & 0x80, 0x80);
    assert_eq!(memory.plane(3)[0] & 0x80, 0x80);
    // Planes 0 and 2 should not
    assert_eq!(memory.plane(0)[0] & 0x80, 0);
    assert_eq!(memory.plane(2)[0] & 0x80, 0);
}

#[test]
fn test_write_pixel_mode10() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x10);
    vga.write_pixel(0, 8, 0, 5, &mut memory); // x=8 -> byte 1, bit 7
                                              // Color 5 = 0b0101 -> planes 0 and 2
    assert_eq!(memory.plane(0)[1] & 0x80, 0x80);
    assert_eq!(memory.plane(2)[1] & 0x80, 0x80);
}

#[test]
fn test_write_pixel_mode12() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x12);
    vga.write_pixel(0, 639, 0, 7, &mut memory);
    // x=639 -> byte 79, bit 0
    assert_eq!(memory.plane(0)[79] & 0x01, 0x01);
    assert_eq!(memory.plane(1)[79] & 0x01, 0x01);
    assert_eq!(memory.plane(2)[79] & 0x01, 0x01);
}

#[test]
fn test_write_pixel_planar_oob() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x12);
    // Out of bounds - should be no-op
    vga.write_pixel(0, 640, 0, 15, &mut memory);
    vga.write_pixel(0, 0, 480, 15, &mut memory);
}

#[test]
fn test_read_pixel_unsupported_mode() {
    let mut vga = Vga::new();
    let memory = Memory::new();
    vga.set_mode_bios(0x12);
    let color = vga.read_pixel(0, 0, 0, &memory);
    assert_eq!(color, 0); // Unsupported, returns 0
}

#[test]
fn test_attribute_controller_write_and_read() {
    let mut vga = Vga::new();
    // Reset flip-flop
    vga.read_port(0x3DA);
    // Write index (odd/even state)
    vga.write_port(0x3C0, 0x05);
    // Write data
    vga.write_port(0x3C0, 0x15);
    // Just exercise the code path
    let _ = vga.read_port(0x3C1);
}

#[test]
fn test_attribute_port_3c1_read() {
    let mut vga = Vga::new();
    // Just exercise the read path
    let _ = vga.read_port(0x3C1);
}

#[test]
fn test_sequencer_out_of_bounds() {
    let mut vga = Vga::new();
    vga.write_port(0x3C4, 0xFF); // Invalid index
    let val = vga.read_port(0x3C5);
    assert_eq!(val, 0);
}

#[test]
fn test_sequencer_chain4_disable_enable() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    vga.write_port(0x3D4, 0x09); // CRTC index: max scan line
    vga.write_port(0x3D5, 0x40); // Clear double-scan (Mode X setup)
                                 // Disable chain4
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x00);
    assert!(vga.is_mode_x());
    // Re-enable chain4
    vga.write_port(0x3C5, 0x08);
    assert!(!vga.is_mode_x());
}

#[test]
fn test_sequencer_chain4_disable_non_mode13() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x00);
    assert!(!vga.is_mode_x());
}

#[test]
fn test_crtc_mode_x_ignores_non_vde_index() {
    // A CRTC write to a non-VDE index (e.g. 0x05) must not perturb the
    // computed `mode_x_visible_height()` — that value derives only from
    // CRTC[0x12]/[0x07]/[0x09].
    let mut vga = Vga::new();
    // Programme Mode X via registers + a known VDE that yields height 123.
    vga.attribute_regs[0x10] |= 0x40;
    vga.sequencer_regs[4] &= !0x08;
    vga.crtc_regs[0x12] = 122;
    vga.crtc_regs[0x07] = 0;
    vga.crtc_regs[0x09] = 0;
    assert_eq!(vga.get_mode_x_height(), 123);
    vga.write_port(0x3D4, 0x05);
    vga.write_port(0x3D5, 0xAA);
    assert_eq!(vga.get_mode_x_height(), 123);
}

#[test]
fn test_crtc_mode_x_no_resize_when_height_matches() {
    let mut vga = Vga::new();
    // Programme Mode X via registers (so the CRTC handler's `is_mode_x()`
    // gate fires).
    vga.attribute_regs[0x10] |= 0x40;
    vga.sequencer_regs[4] &= !0x08;
    vga.fb_width = 320;
    vga.fb_height = 200;
    vga.framebuffer = vec![0u32; 1];
    vga.crtc_regs[0x07] = 0x00;
    vga.crtc_regs[0x09] = 0x40; // Clear max_scan_line (keep LC bit 9)

    vga.write_port(0x3D4, 0x12);
    vga.write_port(0x3D5, 0xC7);

    assert_eq!(vga.get_mode_x_height(), 200);
    assert_eq!(vga.framebuffer.len(), 1);
}

#[test]
fn test_crtc_mode_x_resizes_framebuffer() {
    let mut vga = Vga::new();
    // Programme Mode X via registers (so the CRTC handler's `is_mode_x()`
    // gate fires).
    vga.attribute_regs[0x10] |= 0x40;
    vga.sequencer_regs[4] &= !0x08;
    vga.fb_width = 320;
    vga.fb_height = 200;
    vga.framebuffer = vec![0u32; 1];
    vga.crtc_regs[0x07] = 0x00;
    vga.crtc_regs[0x09] = 0x40; // Clear max_scan_line (keep LC bit 9)

    vga.write_port(0x3D4, 0x12);
    vga.write_port(0x3D5, 0xEF);

    assert_eq!(vga.get_mode_x_height(), 240);
    assert_eq!(vga.fb_height, 240);
    assert_eq!(vga.framebuffer.len(), 320 * 240);
}

#[test]
fn test_sync_to_memory_chain4_toggle_copies_vram() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);

    for i in 0..8 {
        memory.ram[gp64_memory::VIDEO_BASE + i] = 0x10 + i as u8;
    }

    memory.apply_vga_pipeline(&vga.build_pipeline_state());

    // Disable chain-4 AND program CR9 max_scan_line=0 (genuine Mode X
    // entry — see Vga::is_mode_x() doc comment: under the strict
    // predicate, SR[4] bit 3 cleared alone is not sufficient).
    // Then sync.
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x00);
    vga.write_port(0x3D4, 0x09);
    vga.write_port(0x3D5, 0x40); // CR9 := max_scan_line=0
    memory.apply_vga_pipeline(&vga.build_pipeline_state());

    assert_eq!(memory.plane(0)[0], 0x10);
    assert_eq!(memory.plane(1)[0], 0x11);
    assert_eq!(memory.plane(2)[0], 0x12);
    assert_eq!(memory.plane(3)[0], 0x13);
    // Chain-4 mapping uses plane cell
    // `addr & !3`, so A000 offsets 4..7 land at cell 4, not cell 1.
    assert_eq!(memory.plane(0)[4], 0x14);
    assert_eq!(memory.plane(1)[4], 0x15);
    assert_eq!(memory.plane(2)[4], 0x16);
    assert_eq!(memory.plane(3)[4], 0x17);

    memory.plane_mut(0)[0] = 0xA0;
    memory.plane_mut(1)[0] = 0xA1;
    memory.plane_mut(2)[0] = 0xA2;
    memory.plane_mut(3)[0] = 0xA3;

    // Re-enable chain-4 and sync back to linear memory.
    vga.write_port(0x3C5, 0x08);
    memory.apply_vga_pipeline(&vga.build_pipeline_state());

    assert_eq!(memory.ram[gp64_memory::VIDEO_BASE], 0xA0);
    assert_eq!(memory.ram[gp64_memory::VIDEO_BASE + 1], 0xA1);
    assert_eq!(memory.ram[gp64_memory::VIDEO_BASE + 2], 0xA2);
    assert_eq!(memory.ram[gp64_memory::VIDEO_BASE + 3], 0xA3);
}

#[test]
fn test_sync_to_memory_no_chain4_change() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);

    memory.vga.video_mode = vga.mode();
    memory.vga.chain4 = vga.is_chain4();

    memory.apply_vga_pipeline(&vga.build_pipeline_state());
    assert_eq!(memory.vga.video_mode, vga.mode());
    assert_eq!(memory.vga.chain4, vga.is_chain4());
}

#[test]
fn test_sync_to_memory_copies_memory_map_select() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.write_port(0x3CE, 0x06);
    vga.write_port(0x3CF, 0x0C); // map-select=3 (B8000-BFFFF)

    memory.apply_vga_pipeline(&vga.build_pipeline_state());
    assert_eq!(memory.vga.memory_map_select, 3);
}

#[test]
fn test_mode13_sets_gc6_a000_64k_window() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    assert_eq!(vga.graphics_regs[0x06], 0x05);
}

#[test]
fn test_set_mode_bios_programs_gc6_for_all_mode_groups() {
    let mut vga = Vga::new();

    // Default (mode 03h at init) should have color text GC6
    assert_eq!(vga.graphics_regs[0x06], 0x0E, "default mode 03h");

    // Color text modes → 0x0E (map=3, B8000-BFFFF)
    for mode in [0x00, 0x01, 0x02, 0x03] {
        vga.set_mode_bios(0x13); // switch away first
        vga.set_mode_bios(mode);
        assert_eq!(
            vga.graphics_regs[0x06], 0x0E,
            "mode 0x{mode:02X} should set GC6=0x0E"
        );
    }

    // Mono text → 0x0A (map=2, B0000-B7FFF)
    vga.set_mode_bios(0x13);
    vga.set_mode_bios(0x07);
    assert_eq!(vga.graphics_regs[0x06], 0x0A, "mode 07h mono text");

    // CGA graphics → 0x0F (map=3, B8000-BFFFF, graphics+odd/even)
    for mode in [0x04, 0x05, 0x06] {
        vga.set_mode_bios(0x13);
        vga.set_mode_bios(mode);
        assert_eq!(
            vga.graphics_regs[0x06], 0x0F,
            "mode 0x{mode:02X} should set GC6=0x0F"
        );
    }

    // EGA/VGA planar → 0x05 (map=1, A0000-AFFFF)
    for mode in [0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x12] {
        vga.set_mode_bios(0x03);
        vga.set_mode_bios(mode);
        assert_eq!(
            vga.graphics_regs[0x06], 0x05,
            "mode 0x{mode:02X} should set GC6=0x05"
        );
    }

    // Mode 13h → 0x05
    vga.set_mode_bios(0x03);
    vga.set_mode_bios(0x13);
    assert_eq!(vga.graphics_regs[0x06], 0x05, "mode 13h");
}

#[test]
fn test_sync_to_memory_chain4_change_non_mode13() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);

    memory.vga.video_mode = vga.mode();
    memory.vga.chain4 = true;

    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x00);

    memory.apply_vga_pipeline(&vga.build_pipeline_state());
    assert_eq!(memory.vga.chain4, vga.is_chain4());
}

#[test]
fn test_sync_to_memory_chain4_toggle_on_mode13_entry() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    memory.vga.video_mode = 0x03;
    memory.vga.chain4 = true;

    vga.set_mode_bios(0x13);
    // Disable chain-4 AND program CR9 max_scan_line=0 to genuinely enter
    // Mode X — see Vga::is_mode_x() doc comment: under the strict
    // predicate, SR[4] bit 3 cleared alone is not sufficient.
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x00);
    vga.write_port(0x3D4, 0x09);
    vga.write_port(0x3D5, 0x40); // CR9 := max_scan_line=0

    for i in 0..4 {
        memory.ram[gp64_memory::VIDEO_BASE + i] = 0x20 + i as u8;
    }

    memory.apply_vga_pipeline(&vga.build_pipeline_state());

    assert_eq!(memory.plane(0)[0], 0x20);
    assert_eq!(memory.plane(1)[0], 0x21);
    assert_eq!(memory.plane(2)[0], 0x22);
    assert_eq!(memory.plane(3)[0], 0x23);
}

#[test]
fn test_graphics_controller_out_of_bounds() {
    let mut vga = Vga::new();
    vga.write_port(0x3CE, 0xFF); // Invalid index
    let val = vga.read_port(0x3CF);
    assert_eq!(val, 0);
}

#[test]
fn test_sequencer_write_out_of_bounds() {
    let mut vga = Vga::new();
    vga.write_port(0x3C4, 0xFF);
    vga.write_port(0x3C5, 0xAA);
    assert_eq!(vga.sequencer_regs[0], 0x00);
}

#[test]
fn test_graphics_write_out_of_bounds() {
    let mut vga = Vga::new();
    vga.write_port(0x3CE, 0xFF);
    vga.write_port(0x3CF, 0xAA);
    assert_eq!(vga.graphics_regs[0], 0x00);
}

#[test]
fn test_crtc_write_out_of_bounds() {
    let mut vga = Vga::new();
    let orig = vga.crtc_regs[0]; // Mode 03h default (0x5F)
    vga.write_port(0x3D4, 0xFF);
    vga.write_port(0x3D5, 0xAA);
    assert_eq!(vga.crtc_regs[0], orig);
}

#[test]
fn test_graphics_controller_color_compare() {
    let mut vga = Vga::new();
    vga.write_port(0x3CE, 0x02); // Color Compare
    vga.write_port(0x3CF, 0x0A);
    assert_eq!(vga.read_port(0x3CF), 0x0A);
}

#[test]
fn test_graphics_controller_miscellaneous() {
    let mut vga = Vga::new();
    vga.write_port(0x3CE, 0x06); // Miscellaneous
    vga.write_port(0x3CF, 0x05);
    assert_eq!(vga.read_port(0x3CF), 0x05);
}

#[test]
fn test_graphics_controller_color_dont_care() {
    let mut vga = Vga::new();
    vga.write_port(0x3CE, 0x07); // Color Don't Care
    vga.write_port(0x3CF, 0x0F);
    assert_eq!(vga.read_port(0x3CF), 0x0F);
}

#[test]
fn test_crtc_out_of_bounds() {
    let mut vga = Vga::new();
    vga.write_port(0x3D4, 0xFF); // Invalid index
    let val = vga.read_port(0x3D5);
    assert_eq!(val, 0);
}

#[test]
fn test_crtc_horizontal_total() {
    let mut vga = Vga::new();
    vga.write_port(0x3D4, 0x00); // Horizontal Total
    vga.write_port(0x3D5, 0x5F);
    vga.write_port(0x3D4, 0x00);
    assert_eq!(vga.read_port(0x3D5), 0x5F);
}

#[test]
fn test_crtc_vertical_total() {
    let mut vga = Vga::new();
    vga.write_port(0x3D4, 0x06); // Vertical Total
    vga.write_port(0x3D5, 0xBF);
    vga.write_port(0x3D4, 0x06);
    assert_eq!(vga.read_port(0x3D5), 0xBF);
}

#[test]
fn test_text_page_size_40x25() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x00);
    assert_eq!(vga.text_page_size_words(), 0x400);
}

#[test]
fn test_text_page_size_80x25() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    assert_eq!(vga.text_page_size_words(), 0x800);
}

#[test]
fn test_set_active_page_updates_crtc() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    vga.set_active_page(1);
    // Page 1 start = 0x800 words
    vga.write_port(0x3D4, 0x0C);
    assert_eq!(vga.read_port(0x3D5), 0x08); // High byte
    vga.write_port(0x3D4, 0x0D);
    assert_eq!(vga.read_port(0x3D5), 0x00); // Low byte
}

#[test]
fn test_input_status_vsync_active() {
    let mut vga = Vga::new();
    // Position within VSync pulse (between vsync_offset and vsync_offset + duration)
    vga.cycles_since_vblank = vga.vsync_offset_cycles + 1;
    assert!(vga.cycles_since_vblank < vga.vsync_offset_cycles + vga.vsync_duration_cycles);
    let status = vga.read_port(0x3DA);
    assert_eq!(
        status & 0x08,
        0x08,
        "bit 3 should be set during VSync pulse"
    );
    assert_eq!(status & 0x01, 0x01, "bit 0 should be set during VBI");
}

#[test]
fn test_input_status_vsync_inactive() {
    let mut vga = Vga::new();
    vga.cycles_since_vblank = vga.timing_retrace_cycles + 1000;
    let status = vga.read_port(0x3DA);
    assert_eq!(status & 0x08, 0x00);
}

#[test]
fn test_port_3da_override_sees_vsync_window() {
    // Input Status Register 1 bit 3 reports vertical retrace. Sampling
    // successive frame positions must eventually reach its active window.
    let mut vga = Vga::new();
    let cpf = vga.timing_cycles_per_frame;
    let vsync_start = vga.vsync_offset_cycles;
    let vsync_end = vga.vsync_offset_cycles + vga.vsync_duration_cycles;

    // Sample one frame starting from frame_start_cycle = 0.
    vga.frame_start_cycle = 0;
    let mut saw_vsync = false;
    for step in 0..cpf {
        let position = step % cpf;
        vga.port_3da_override_position = Some(position);
        let status = vga.read_port(0x3DA);
        vga.port_3da_override_position = None;
        if (status & 0x08) != 0 {
            saw_vsync = true;
            break;
        }
    }
    assert!(
        saw_vsync,
        "stepping through a full frame must cross the VSync window (start={}, end={}, cpf={})",
        vsync_start, vsync_end, cpf
    );
}

#[test]
fn test_port_3da_override_vsync_window_at_step_28() {
    // Polling loop step size is 28 cycles per iteration (3 instrs + 25 io delay).
    // Confirm at least one iteration lands in the VSync window.
    let mut vga = Vga::new();
    let cpf = vga.timing_cycles_per_frame;
    let vsync_start = vga.vsync_offset_cycles;
    let vsync_end = vga.vsync_offset_cycles + vga.vsync_duration_cycles;

    vga.frame_start_cycle = 0;
    let mut first_hit = None;
    let mut step: u64 = 0;
    while step < cpf {
        let position = step % cpf;
        vga.port_3da_override_position = Some(position);
        let status = vga.read_port(0x3DA);
        vga.port_3da_override_position = None;
        if (status & 0x08) != 0 {
            first_hit = Some(step);
            break;
        }
        step += 28;
    }
    assert!(
        first_hit.is_some(),
        "step=28 polling loop must hit VSync window within one frame (start={}, end={}, width={})",
        vsync_start,
        vsync_end,
        vsync_end - vsync_start
    );
}

#[test]
fn test_input_status_vbi_without_vsync() {
    // During VBI but before VSync pulse starts, bit 0 should be set but bit 3 clear
    let mut vga = Vga::new();
    if vga.vsync_offset_cycles > 1 {
        vga.cycles_since_vblank = 0; // Start of VBI, before VSync pulse
        let status = vga.read_port(0x3DA);
        assert_eq!(status & 0x01, 0x01, "bit 0 should be set during VBI");
        assert_eq!(
            status & 0x08,
            0x00,
            "bit 3 should be clear before VSync pulse"
        );
    }
}

#[test]
fn test_teletype_bs_at_column_zero() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 0, 0);
    vga.teletype(0x08, 0, &mut memory);
    let (row, col) = vga.get_cursor(0);
    assert_eq!(row, 0);
    assert_eq!(col, 0);
}

#[test]
fn test_scroll_up_full_screen() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    // lines=0 means clear entire region
    let region = ScrollRegion {
        top: 0,
        left: 0,
        bottom: 24,
        right: 79,
    };
    vga.scroll_up(0, 0x07, region, &mut memory);
}

#[test]
fn test_scroll_down_full_screen() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    let region = ScrollRegion {
        top: 0,
        left: 0,
        bottom: 24,
        right: 79,
    };
    vga.scroll_down(0, 0x07, region, &mut memory);
}

#[test]
fn test_scroll_up_graphics_mode_noop() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);
    let region = ScrollRegion {
        top: 0,
        left: 0,
        bottom: 24,
        right: 39,
    };
    vga.scroll_up(1, 0x07, region, &mut memory);
    // Should be no-op
}

#[test]
fn test_scroll_down_graphics_mode_no_panic() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);
    let region = ScrollRegion {
        top: 0,
        left: 0,
        bottom: 24,
        right: 39,
    };
    vga.scroll_down(1, 0x07, region, &mut memory);
}

#[test]
fn test_scroll_up_degenerate_region_no_panic() {
    // Regression: scroll_up must not panic when bottom < top.
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    let region = ScrollRegion {
        top: 10,
        left: 0,
        bottom: 5, // bottom < top = degenerate
        right: 79,
    };
    vga.scroll_up(1, 0x07, region, &mut memory);
}

#[test]
fn test_scroll_down_degenerate_region_no_panic() {
    // Regression: scroll_down must not panic when bottom < top.
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    let region = ScrollRegion {
        top: 20,
        left: 0,
        bottom: 3, // bottom < top = degenerate
        right: 79,
    };
    vga.scroll_down(1, 0x07, region, &mut memory);
}

#[test]
fn test_scroll_up_large_lines_no_overflow() {
    // Regression: row + lines must not overflow u8.
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    let region = ScrollRegion {
        top: 0,
        left: 0,
        bottom: 24,
        right: 79,
    };
    // lines=255 means clear everything (src_row will exceed bottom)
    vga.scroll_up(255, 0x07, region, &mut memory);
}

#[test]
fn test_mode07_monochrome() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x07);
    assert_eq!(vga.mode(), 0x07);
    assert_eq!(vga.columns, 80);
    assert!(vga.is_text_mode());
}

#[test]
fn test_mode07_text_memory_base() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x07);
    assert_eq!(vga.text_memory_base(), 0xB0000);
}

#[test]
fn test_mode03_text_memory_base() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    assert_eq!(vga.text_memory_base(), 0xB8000);
}

#[test]
fn test_read_text_screen_mode03() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    let mut memory = Memory::new();

    // Write "Hello" at row 0, col 0
    let base = 0xB8000;
    for (i, ch) in b"Hello".iter().enumerate() {
        memory.write_u8(base + i * 2, *ch);
        memory.write_u8(base + i * 2 + 1, 0x07); // attribute
    }

    // Write "World" at row 1, col 0
    let row1 = base + 80 * 2;
    for (i, ch) in b"World".iter().enumerate() {
        memory.write_u8(row1 + i * 2, *ch);
        memory.write_u8(row1 + i * 2 + 1, 0x07);
    }

    let text = vga.read_text_screen(&memory).unwrap();
    let lines: Vec<&str> = text.split('\n').collect();
    assert_eq!(lines.len(), 25);
    assert_eq!(lines[0], "Hello");
    assert_eq!(lines[1], "World");
    // Remaining rows should be empty (trimmed)
    for line in &lines[2..] {
        assert!(line.is_empty(), "expected empty line, got: '{line}'");
    }
}

#[test]
fn test_read_text_screen_not_text_mode() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13); // Graphics mode
    let memory = Memory::new();
    let result = vga.read_text_screen(&memory);
    assert_eq!(result, Err("not in text mode"));
}

#[test]
fn test_read_text_row_basic() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    let mut memory = Memory::new();

    // Write "Test" at row 2
    let base = 0xB8000 + 2 * 80 * 2;
    for (i, ch) in b"Test".iter().enumerate() {
        memory.write_u8(base + i * 2, *ch);
        memory.write_u8(base + i * 2 + 1, 0x07);
    }

    assert_eq!(vga.read_text_row(2, &memory).unwrap(), "Test");
    assert_eq!(vga.read_text_row(0, &memory).unwrap(), "");
}

#[test]
fn test_read_text_row_out_of_range() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    let memory = Memory::new();
    assert_eq!(vga.read_text_row(25, &memory), Err("row out of range"));
    assert_eq!(vga.read_text_row(255, &memory), Err("row out of range"));
}

#[test]
fn test_read_text_row_not_text_mode() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    let memory = Memory::new();
    assert_eq!(vga.read_text_row(0, &memory), Err("not in text mode"));
}

#[test]
fn test_read_text_screen_nonprintable_mapped_to_space() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    let mut memory = Memory::new();

    // Write characters including non-printable ones
    let base = 0xB8000;
    memory.write_u8(base, b'A');
    memory.write_u8(base + 1, 0x07);
    memory.write_u8(base + 2, 0x01); // Non-printable (SOH)
    memory.write_u8(base + 3, 0x07);
    memory.write_u8(base + 4, b'B');
    memory.write_u8(base + 5, 0x07);

    let text = vga.read_text_screen(&memory).unwrap();
    let first_line = text.split('\n').next().unwrap();
    assert_eq!(&first_line[..3], "A B");
}

#[test]
fn test_read_text_screen_mono_mode07() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x07);
    let mut memory = Memory::new();

    // Mono text mode uses base 0xB0000
    let base = 0xB0000;
    for (i, ch) in b"Mono".iter().enumerate() {
        memory.write_u8(base + i * 2, *ch);
        memory.write_u8(base + i * 2 + 1, 0x07);
    }

    let text = vga.read_text_screen(&memory).unwrap();
    let first_line = text.split('\n').next().unwrap();
    assert_eq!(first_line, "Mono");
}

#[test]
fn test_text_page_base_color_and_mono() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    let color_expected = 0xB8000 + vga.text_page_size_words() * 2;
    assert_eq!(vga.text_page_base(1), color_expected);

    vga.set_mode_bios(0x07);
    let mono_expected = 0xB0000 + vga.text_page_size_words() * 2;
    assert_eq!(vga.text_page_base(1), mono_expected);
}

#[test]
fn test_ril_defaults_bounds() {
    let mut vga = Vga::new();

    vga.set_ril_defaults(7, vec![1, 2, 3]);
    assert_eq!(vga.get_ril_defaults(7), Some([1, 2, 3].as_slice()));

    vga.set_ril_defaults(8, vec![9]);
    assert_eq!(vga.get_ril_defaults(8), None);
}

#[test]
fn test_mono_crtc_ports() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x07);

    vga.write_port(0x3B4, 0x0C);
    vga.write_port(0x3B5, 0x12);
    vga.write_port(0x3B4, 0x0D);
    vga.write_port(0x3B5, 0x34);

    assert_eq!(vga.crtc_regs[0x0C], 0x12);
    assert_eq!(vga.crtc_regs[0x0D], 0x34);

    vga.write_port(0x3B4, 0x0C);
    assert_eq!(vga.read_port(0x3B5), 0x12);
}

#[test]
fn test_palette_to_rgb_all_colors() {
    let vga = Vga::new();
    for i in 0..16u8 {
        let _rgb = vga.palette_to_rgb(i);
    }
}

#[test]
fn test_set_timing_cycles_per_frame_updates_retrace() {
    let mut vga = Vga::new();
    vga.cycles_since_vblank = 12345;
    vga.set_timing_cycles_per_frame(1000);
    assert_eq!(vga.timing_cycles_per_frame, 1000);
    // Mode 03h programs vertical blank from line 406 through line 441.
    assert_eq!(vga.timing_retrace_cycles, (1000 * 35) / 449);
    assert_eq!(vga.cycles_since_vblank, 12345 % 1000);
}

#[test]
fn test_timing_cycles_per_frame_clamps_retrace() {
    let mut vga = Vga::new();
    vga.cycles_since_vblank = 5;
    vga.set_timing_cycles_per_frame(1);
    assert_eq!(vga.timing_cycles_per_frame, 1);
    assert_eq!(vga.timing_retrace_cycles, 1);
    assert_eq!(vga.cycles_since_vblank, 0);
}

#[test]
fn test_set_timing_cycles_per_frame_zero_no_change() {
    let mut vga = Vga::new();
    vga.cycles_since_vblank = 123;
    let prev_cycles = vga.cycles_since_vblank;
    let prev_frame = vga.timing_cycles_per_frame;
    let prev_retrace = vga.timing_retrace_cycles;
    vga.set_timing_cycles_per_frame(0);
    assert_eq!(vga.cycles_since_vblank, prev_cycles);
    assert_eq!(vga.timing_cycles_per_frame, prev_frame);
    assert_eq!(vga.timing_retrace_cycles, prev_retrace);
}

#[test]
fn test_vga_tick_cycles() {
    let mut vga = Vga::new();
    let prev = vga.cycles_since_vblank;
    vga.tick(1000);
    assert!(vga.cycles_since_vblank > prev);
}

#[test]
fn test_vga_tick_frame_wrap() {
    let mut vga = Vga::new();
    vga.cycles_since_vblank = vga.timing_cycles_per_frame - 10;
    vga.tick(100);
    // Should have wrapped around
    assert!(vga.cycles_since_vblank < 200);
}

#[test]
fn test_teletype_graphics_mode13_explicit() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);
    vga.set_cursor(0, 0, 0);
    vga.teletype(b'A', 0, &mut memory);
    // Should draw character in graphics mode without crashing
}

#[test]
fn test_write_char_count_explicit() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 0, 0);
    vga.write_char(0, b'X', 5, &mut memory);
    // Should write character 5 times
}

#[test]
fn test_scroll_up_graphics_mode13_explicit() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);
    vga.scroll_up_graphics(&mut memory, 0);
    // Should scroll without crashing
}

#[test]
fn test_vga_get_mode_explicit() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    // get_mode() returns (mode, columns, active_page)
    let (mode, cols, active_page) = vga.get_mode();
    assert_eq!(mode, 0x13);
    assert_eq!(cols, 40);
    assert_eq!(active_page, 0);
}

#[test]
fn test_vga_multiple_pages_explicit() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    // set_cursor(page, row, col)
    vga.set_cursor(0, 5, 10);
    vga.set_cursor(1, 3, 7);
    let (row0, col0) = vga.get_cursor(0);
    let (row1, col1) = vga.get_cursor(1);
    assert_eq!((row0, col0), (5, 10));
    assert_eq!((row1, col1), (3, 7));
}

#[test]
fn test_render_mode_04_cga_color_explicit() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);
    vga.render(&mut memory);
    // Should render without crashing
}

#[test]
fn test_render_mode_06_cga_hires_explicit() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x06);
    vga.render(&mut memory);
    // Should render without crashing
}

#[test]
fn test_overscan_rgb() {
    let mut vga = Vga::new();
    // Set DAC entry 5 to red
    vga.set_dac_reg(5, 63, 0, 0); // R=63 (max 6-bit), G=0, B=0
                                  // Set overscan color index to 5
    vga.attribute_regs[0x11] = 5;
    // overscan_rgb should return the red color
    let rgb = vga.overscan_rgb();
    // DAC expands 6-bit to full 8-bit: 63 -> 255 (0xFF)
    assert_eq!(rgb, 0x00FF0000); // 0x00RRGGBB format
}

#[test]
fn test_overscan_rgb_with_pel_mask() {
    let mut vga = Vga::new();
    // Set DAC entry 5 to green
    vga.set_dac_reg(5, 0, 63, 0);
    // Set overscan color index to 5
    vga.attribute_regs[0x11] = 5;
    // Set PEL mask to only allow lower 4 bits (masks out bit 0 of index 5)
    vga.pel_mask = 0xFE;
    // With mask 0xFE, index 5 becomes 4
    vga.set_dac_reg(4, 0, 0, 63); // Blue
    let rgb = vga.overscan_rgb();
    // DAC expands 6-bit to full 8-bit: 63 -> 255 (0xFF)
    assert_eq!(rgb, 0x0000FF); // Should be blue (index 4)
}

#[test]
fn test_get_font_row_out_of_bounds_falls_back() {
    let vga = Vga::new();
    let memory = Memory::new();
    let row = 32;
    let value = vga.get_font_row(&memory, 0xFF, row, 64, 0);
    let expected = CP437_FONT_8X16[0xFF * 16 + 15];
    assert_eq!(value, expected);
}

#[test]
fn test_get_font_row_bank_b_selection() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.sequencer_regs[3] = 0x04 | 0x02;
    // Flag must be set to read from plane 2; the latch is a one-way flag.
    vga.custom_font_loaded = true;

    let ch = b'A';
    let row = 0;
    let (_, bank_b) = vga.get_font_bank_offsets();
    let font_offset = bank_b + ch as usize * FONT_CHAR_SIZE + row;
    memory.plane_mut(2)[font_offset] = 0xF0;

    let value = vga.get_font_row(&memory, ch, row, 16, 0x00);
    assert_eq!(value, 0xF0);
}

#[test]
fn test_get_font_row_bank_offset_out_of_bounds() {
    let mut vga = Vga::new();
    let memory = Memory::new();
    vga.sequencer_regs[3] = 0x4C;

    let value = vga.get_font_row(&memory, 0xFF, 32, 64, 0);
    let expected = CP437_FONT_8X16[0xFF * 16 + 15];
    assert_eq!(value, expected);
}

#[test]
fn test_get_font_row_builtin_8x16() {
    let vga = Vga::new();
    let memory = Memory::new();
    // Character 'A' (0x41) in 8x16 font, attr=0 (use bank A)
    let row0 = vga.get_font_row(&memory, 0x41, 0, 16, 0);
    let row4 = vga.get_font_row(&memory, 0x41, 4, 16, 0);
    // 'A' should have some pixels set in middle rows
    // Row 0 is typically blank for 'A'
    assert_eq!(row0, CP437_FONT_8X16[0x41 * 16]);
    assert_eq!(row4, CP437_FONT_8X16[0x41 * 16 + 4]);
}

#[test]
fn test_get_font_row_builtin_8x8() {
    let vga = Vga::new();
    let memory = Memory::new();
    // Character 'A' (0x41) in 8x8 font, attr=0 (use bank A)
    let row0 = vga.get_font_row(&memory, 0x41, 0, 8, 0);
    let row3 = vga.get_font_row(&memory, 0x41, 3, 8, 0);
    assert_eq!(row0, CP437_FONT_8X8[0x41 * 8]);
    assert_eq!(row3, CP437_FONT_8X8[0x41 * 8 + 3]);
}

#[test]
fn test_get_font_row_reads_plane_2_when_flag_set() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.custom_font_loaded = true;
    // Write a sentinel byte into plane 2 at the bank-A slot for 'A' row 0.
    // With seq reg 3 = 0, both banks live at offset 0.
    let offset = 0x41 * FONT_CHAR_SIZE;
    memory.plane_mut(2)[offset] = 0xA5;

    let value = vga.get_font_row(&memory, 0x41, 0, 16, 0);
    assert_eq!(value, 0xA5);
}

#[test]
fn test_get_font_row_reads_embedded_when_flag_clear() {
    let vga = Vga::new();
    let mut memory = Memory::new();
    // Explicitly keep the flag clear and drop non-zero sentinels into
    // plane 2 — the old heuristic would have returned 0xA5 here.
    assert!(!vga.custom_font_loaded);
    let offset = 0x41 * FONT_CHAR_SIZE;
    memory.plane_mut(2)[offset] = 0xA5;
    memory.plane_mut(2)[offset + 1] = 0x5A;

    let value = vga.get_font_row(&memory, 0x41, 0, 16, 0);
    assert_eq!(value, CP437_FONT_8X16[0x41 * 16]);
}

#[test]
fn test_latch_flag_persists_across_mode_change() {
    let mut vga = Vga::new();
    vga.custom_font_loaded = true;

    // Real VGA BIOSes preserve uploaded fonts across AH=00 mode
    // switches; simulate a mode change and verify the latch stays set.
    vga.set_mode_bios(0x03);
    assert!(vga.custom_font_loaded);
    vga.set_mode_bios(0x12);
    assert!(vga.custom_font_loaded);
}

/// End-to-end: a guest-code plane-2 font upload sequence (sequencer
/// map_mask=0x04, chain4 off, memory_map_select=1) must cause
/// `Memory::write_u8` into A0000–AFFFF to latch
/// `planar_font_latch_pending`, and the next `Vga::render` (or
/// `Vga::render_scanline(0,..)`) must mirror that into
/// `custom_font_loaded` so `get_font_row` reads from plane 2.
///
/// We run in video_mode 0x12 so `Memory::is_planar_mode()` routes the
/// A0000 write through `write_planar_ega_core`.  The VGA-BIOS-ROM Stage
/// 1 work only requires that *when* writes actually take the planar
/// path, the latch fires; tightening `is_planar_mode` to recognise
/// sequencer-driven planar configurations is a separate, pre-existing
/// gap (see wrk_docs/2026.01.06 - VGA Review - Memory and Latches.md).
#[test]
fn test_planar_write_latches_custom_font_and_renders_from_plane2() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    // Mode 0x12 (640x480 16-color planar) ensures Memory routes VGA
    // writes through the planar write pipeline.
    vga.set_mode_bios(0x12);

    // Configure the VGA sequencer/GC exactly as the BIOS does before a
    // plane-2 font upload.
    configure_for_font_upload(&mut vga, 1);

    // Propagate the VGA register state into Memory's pipeline snapshot
    // so `write_u8` sees non-chain4, plane_mask=0x04, map_select=1.
    memory.apply_vga_pipeline(&vga.build_pipeline_state());
    assert!(!memory.planar_font_latch_pending());
    assert!(!vga.custom_font_loaded);

    // Guest writes a sentinel glyph row for 'A' (0x41) via
    // `Memory::write_u8` into A0000 — the same path a real VGA BIOS ROM
    // running on the emulated CPU would take.  `FONT_CHAR_SIZE * 0x41`
    // matches bank-A slot 0 with seq reg 3 = 0.
    const A0000: usize = 0xA_0000;
    let font_offset = 0x41 * FONT_CHAR_SIZE;
    for row in 0..16usize {
        memory.write_u8(A0000 + font_offset + row, 0xA5 ^ row as u8);
    }

    // Memory observed the font-upload pattern.
    assert!(
        memory.planar_font_latch_pending(),
        "Memory did not latch the plane-2 font-upload pattern on planar writes"
    );
    // The VGA struct has not been polled yet, so its own flag is still
    // clear; propagation happens in render/render_scanline.
    assert!(!vga.custom_font_loaded);

    // Simulate the guest finishing its upload by switching back to the
    // default text-mode configuration (chain4 on, all planes unmasked).
    // The latch must persist across this transition.
    vga.sequencer_regs[4] |= 0x08; // chain4 on
    vga.sequencer_regs[2] = 0x0F; // plane mask = all planes
    memory.apply_vga_pipeline(&vga.build_pipeline_state());
    assert!(memory.planar_font_latch_pending());

    // First render call after the upload propagates the flag.
    vga.render(&mut memory);
    assert!(
        vga.custom_font_loaded,
        "Vga::render did not propagate the Memory planar font latch"
    );

    // With the flag now set, `get_font_row` reads from plane 2 and
    // returns the sentinel bytes the guest uploaded.
    let row0 = vga.get_font_row(&memory, 0x41, 0, 16, 0);
    assert_eq!(row0, 0xA5);
    let row5 = vga.get_font_row(&memory, 0x41, 5, 16, 0);
    assert_eq!(row5, 0xA5 ^ 5);
}

/// The per-scanline render path is also a production entry point;
/// verify it propagates the Memory latch on the first scanline.
#[test]
fn test_render_scanline_propagates_memory_font_latch() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x12);
    configure_for_font_upload(&mut vga, 0); // map_select = 0 — VGA aperture case
    memory.apply_vga_pipeline(&vga.build_pipeline_state());
    memory.write_u8(0xA_0000, 0x5A);
    assert!(memory.planar_font_latch_pending());
    assert!(!vga.custom_font_loaded);

    // `line == 0` is the sync point; later lines are cheap no-ops.
    vga.render_scanline(0, &mut memory);
    assert!(vga.custom_font_loaded);
}

/// VGA-layer behavior on mode-switch-then-back: `set_mode_bios` does
/// NOT reload the ROM font into plane 2.  Real VGA hardware has no
/// concept of an "active font" — that's the BIOS's job.  This test
/// pins that contract: after a text → mode 13h → text round trip
/// driven entirely through `Vga::set_mode_bios`, the one-way latch
/// stays set and `get_font_row` returns whatever plane 2 holds.
///
/// In production the BIOS layer (INT 10h AH=00) reloads the standard
/// 8×16 font into plane 2 after every cold text-mode set, so plane 2
/// always contains valid font data when rendering runs.  See the
/// integration test in `gp64-bios` for that path.
#[test]
fn test_vga_layer_set_mode_does_not_reload_font() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    // Step 1: upload a custom font glyph for 'A' into plane 2.
    vga.custom_font_loaded = true;
    let font_offset = 0x41 * FONT_CHAR_SIZE;
    let sentinel: u8 = 0xA5;
    for row in 0..16usize {
        memory.plane_mut(2)[font_offset + row] = sentinel;
    }
    // Verify the custom font is readable.
    assert_eq!(vga.get_font_row(&memory, 0x41, 0, 16, 0), sentinel);

    // Step 2: switch to mode 13h (320x200 256-color, chain4).
    // This does NOT clear custom_font_loaded (one-way latch).
    vga.set_mode_bios(0x13);
    assert!(
        vga.custom_font_loaded,
        "latch must persist across mode switch"
    );

    // Simulate mode 13h pixel writes overwriting plane 2 with garbage.
    // In real chain4 mode, byte writes scatter across planes; plane 2
    // receives every 4th byte.  We just zero out the font area directly.
    for row in 0..16usize {
        memory.plane_mut(2)[font_offset + row] = 0x00;
    }

    // Step 3: switch back to mode 3 (80x25 text) without reloading fonts.
    vga.set_mode_bios(0x03);
    assert!(
        vga.custom_font_loaded,
        "latch must persist across mode switch"
    );

    // The latch is still set, so get_font_row reads from plane 2.
    // But plane 2 now has zeros (garbage from the graphics mode),
    // not the original sentinel font data.
    let row0 = vga.get_font_row(&memory, 0x41, 0, 16, 0);
    assert_ne!(
        row0, sentinel,
        "plane 2 should have been overwritten by graphics mode"
    );
    assert_eq!(
        row0, 0x00,
        "get_font_row returns garbage from plane 2, not the original font"
    );
}

#[test]
fn test_palette_lut_dirty_on_dac_write() {
    let mut vga = Vga::new();
    // Clear the dirty flag
    vga.rebuild_palette_lut_if_dirty();
    assert!(!vga.palette_lut_dirty);

    // Write via set_dac_reg should mark dirty
    vga.set_dac_reg(0, 63, 32, 16);
    assert!(vga.palette_lut_dirty);
}

#[test]
fn test_palette_lut_dirty_on_port_write() {
    let mut vga = Vga::new();
    // Clear the dirty flag
    vga.rebuild_palette_lut_if_dirty();
    assert!(!vga.palette_lut_dirty);

    // Write via port 0x3C9 should mark dirty
    vga.write_port(0x3C8, 0); // Set write index to 0
    vga.write_port(0x3C9, 63); // R
    assert!(vga.palette_lut_dirty);
}

#[test]
fn test_palette_lut_rebuild() {
    let mut vga = Vga::new();
    // Set a specific DAC entry
    vga.set_dac_reg(10, 63, 32, 16); // R=63, G=32, B=16 (6-bit values)

    // Rebuild LUT
    vga.rebuild_palette_lut_if_dirty();

    // Check the LUT entry
    // DAC values are expanded to 8-bit using (val << 2) | (val >> 4):
    // 63->0xFF, 32->0x82, 16->0x41
    let expected = (0xFF_u32 << 16) | (0x82_u32 << 8) | 0x41_u32;
    assert_eq!(vga.palette_lut[10], expected);
    assert!(!vga.palette_lut_dirty);
}

#[test]
fn test_palette_lut_not_rebuilt_when_clean() {
    let mut vga = Vga::new();
    vga.set_dac_reg(5, 63, 0, 0);
    vga.rebuild_palette_lut_if_dirty();
    let lut_value = vga.palette_lut[5];

    // Manually modify LUT (simulating it not being rebuilt)
    vga.palette_lut[5] = 0xDEADBEEF;

    // Rebuild should do nothing since not dirty
    vga.rebuild_palette_lut_if_dirty();
    assert_eq!(vga.palette_lut[5], 0xDEADBEEF);

    // Mark dirty and rebuild
    vga.palette_lut_dirty = true;
    vga.rebuild_palette_lut_if_dirty();
    assert_eq!(vga.palette_lut[5], lut_value);
}

#[test]
fn test_palette_address_source_tracking() {
    let mut vga = Vga::new();
    // Default should be true (display enabled)
    assert!(vga.palette_address_source);

    // Reset flip-flop and write index without PAS
    vga.read_port(0x3DA);
    vga.write_port(0x3C0, 0x00); // Index 0, PAS clear
    assert!(!vga.palette_address_source);

    // Write index with PAS set
    vga.read_port(0x3DA);
    vga.write_port(0x3C0, 0x20); // Index 0, PAS set
    assert!(vga.palette_address_source);
}

#[test]
fn test_pas_blanking() {
    // PAS blanking is now implemented via scanline-accurate rendering.
    // When PAS is clear during active display, pixels blank to overscan color.
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    // Set mode 13h (320x200 256-color)
    vga.set_mode_bios(0x13);

    // Set a distinctive overscan color so we can detect blanking
    // Overscan uses palette index from attribute register 0x11
    vga.set_dac_reg(0, 0x3F, 0x00, 0x00); // Palette index 0 = bright red
    vga.attribute_regs[0x11] = 0; // Overscan uses palette index 0

    // Write color index 1 to video memory (should be different from overscan)
    vga.set_dac_reg(1, 0x00, 0x3F, 0x00); // Palette index 1 = bright green
    for i in 0..320 {
        memory.video_memory_mut()[i] = 1; // First row = green pixels
    }

    // Render with PAS set (display enabled) - should show green
    vga.palette_address_source = true;
    vga.render(&mut memory);
    let fb_with_pas = vga.framebuffer()[0];
    let green = 0x00FF00; // Bright green (6-bit 63 -> 8-bit 255)
    assert_eq!(
        fb_with_pas, green,
        "With PAS set, should render video memory"
    );

    // Clear PAS - this triggers a snapshot capture
    vga.read_port(0x3DA);
    vga.write_port(0x3C0, 0x00); // Index 0, PAS clear
    assert!(!vga.palette_address_source);

    // Render with PAS clear - should blank to overscan (red)
    vga.render(&mut memory);
    let fb_without_pas = vga.framebuffer()[0];
    let red = 0xFF0000; // Bright red (6-bit 63 -> 8-bit 255)
    assert_eq!(
        fb_without_pas, red,
        "With PAS clear, display should blank to overscan color"
    );

    // Re-enable PAS - should render normally again
    vga.read_port(0x3DA);
    vga.write_port(0x3C0, 0x20); // Index 0, PAS set
    assert!(vga.palette_address_source);
    vga.render(&mut memory);
    assert_eq!(
        vga.framebuffer()[0],
        green,
        "After PAS set, should render video memory"
    );
}

#[test]
fn test_port_3c1_write_is_ignored() {
    let mut vga = Vga::new();
    // Port 0x3C1 is read-only on real VGA hardware
    vga.read_port(0x3DA); // Reset flip-flop
    vga.write_port(0x3C0, 0x05); // Select palette register 5
    let original = vga.attribute_regs[5];
    vga.write_port(0x3C1, 0x12);
    assert_eq!(vga.attribute_regs[5], original);
}

#[test]
fn test_port_3c1_write_does_not_mark_palette_dirty() {
    let mut vga = Vga::new();
    vga.rebuild_palette_lut_if_dirty();
    assert!(!vga.palette_lut_dirty);
    vga.read_port(0x3DA);
    vga.write_port(0x3C0, 0x03);
    vga.write_port(0x3C1, 0x42);
    assert!(!vga.palette_lut_dirty);
}

#[test]
fn test_port_3c1_write_non_palette_register_ignored() {
    let mut vga = Vga::new();
    vga.read_port(0x3DA);
    vga.write_port(0x3C0, 0x11);
    let original = vga.attribute_regs[0x11];
    vga.write_port(0x3C1, 0x05);
    assert_eq!(vga.attribute_regs[0x11], original);
}

#[test]
fn test_dac_state_vga_spec_values() {
    let mut vga = Vga::new();
    // VGA spec: 0x00 = write mode, 0x03 = read mode

    // Setting write index puts DAC in write mode (0x00)
    vga.write_port(0x3C8, 0);
    assert_eq!(vga.get_dac_state(), 0x00);

    // Setting read index puts DAC in read mode (0x03)
    vga.write_port(0x3C7, 0);
    assert_eq!(vga.get_dac_state(), 0x03);
}

#[test]
fn test_read_3da_does_not_advance_timing() {
    // Reading port 0x3DA should NOT advance cycles_since_vblank
    // Timing should only advance via tick()
    let mut vga = Vga::new();
    vga.cycles_since_vblank = 1000;

    // Read status multiple times
    for _ in 0..100 {
        vga.read_port(0x3DA);
    }

    // cycles_since_vblank should be unchanged
    assert_eq!(
        vga.cycles_since_vblank, 1000,
        "read_port(0x3DA) should not advance timing"
    );
}

#[test]
fn test_tick_advances_timing() {
    // tick() should be the only way to advance timing
    let mut vga = Vga::new();
    vga.cycles_since_vblank = 0;

    vga.tick(500);
    assert_eq!(vga.cycles_since_vblank, 500);

    vga.tick(200);
    assert_eq!(vga.cycles_since_vblank, 700);
}

#[test]
fn test_status_stable_with_timing_advance() {
    // When timing advances, VSYNC should reflect actual retrace only (no read-counter jitter)
    let mut vga = Vga::new();
    vga.cycles_since_vblank = vga.timing_retrace_cycles + 1000; // Not in vblank

    let status1 = vga.read_port(0x3DA) & 0x08;
    vga.tick(1);
    let status2 = vga.read_port(0x3DA) & 0x08;

    assert_eq!(status1, 0, "Expected VSYNC inactive outside retrace");
    assert_eq!(status2, 0, "Expected VSYNC inactive after timing advance");
}

#[test]
fn test_palette_consolidation_initial_values() {
    // Verify attribute_regs[0..16] are initialized with default palette mapping
    let vga = Vga::new();
    // Default VGA palette registers map indices 0-15 directly
    for i in 0..16 {
        assert_eq!(
            vga.attribute_regs[i], i as u8,
            "attribute_regs[{i}] should be {i} initially"
        );
    }
}

#[test]
fn test_palette_consolidation_set_palette_reg_updates_attr() {
    // Verify set_palette_reg updates attribute_regs, not a separate array
    let mut vga = Vga::new();
    vga.set_palette_reg(5, 0x2A);
    assert_eq!(
        vga.attribute_regs[5], 0x2A,
        "set_palette_reg should update attribute_regs"
    );
}

#[test]
fn test_palette_consolidation_port_write_updates_attr() {
    // Verify port 0x3C0 palette writes update attribute_regs
    let mut vga = Vga::new();
    // Port 0x3C0 flip-flops between index and data writes
    vga.read_port(0x3DA); // Reset flip-flop to expect index
    vga.write_port(0x3C0, 0x08 | 0x20); // Select palette reg 8 with PAS set
                                        // Flip-flop now expects data
    vga.write_port(0x3C0, 0x3F); // Write data value 0x3F to palette reg 8

    assert_eq!(
        vga.attribute_regs[8], 0x3F,
        "Port write should update attribute_regs"
    );
}

#[test]
fn test_palette_consolidation_palette16_uses_attr_regs() {
    // Verify rebuild_palette16_lut_if_dirty reads from attribute_regs
    let mut vga = Vga::new();

    // Set attribute_regs[3] to point to DAC index 10
    vga.attribute_regs[3] = 10;
    vga.palette16_lut_dirty = true;

    // Set DAC index 10 to a specific color
    vga.set_dac_raw(10, 100, 150, 200);

    // Rebuild the LUT
    vga.rebuild_palette16_lut_if_dirty();

    // palette16_lut[3] should now be the RGB of DAC[10]
    let expected_rgb = (100u32 << 16) | (150u32 << 8) | 0xC8_u32;
    assert_eq!(
        vga.palette16_lut[3], expected_rgb,
        "palette16_lut should use attribute_regs for lookup"
    );
}

#[test]
fn test_palette_consolidation_get_palette_regs_from_attr() {
    // Verify get_palette_regs returns data from attribute_regs[0..16]
    let mut vga = Vga::new();

    // Modify attribute_regs directly
    for i in 0..16 {
        vga.attribute_regs[i] = (i * 3) as u8;
    }

    let regs = vga.get_palette_regs();
    for i in 0..16 {
        assert_eq!(
            regs[i],
            (i * 3) as u8,
            "get_palette_regs should return attribute_regs[{i}]"
        );
    }
}

#[test]
fn test_palette_consolidation_set_palette_regs_to_attr() {
    // Verify set_palette_regs updates attribute_regs[0..16]
    let mut vga = Vga::new();

    let new_regs: [u8; 16] = [15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0];
    vga.set_palette_regs(new_regs);

    for i in 0..16 {
        assert_eq!(
            vga.attribute_regs[i], new_regs[i],
            "set_palette_regs should update attribute_regs[{i}]"
        );
    }
}

/// Helper: configure a `Vga` for a plane-2 font-upload sequence.
/// `memory_map` picks GC register 6 bits 2-3 (0 = 128 K A0000–BFFFF, 1 = 64 K A0000–AFFFF).
fn configure_for_font_upload(vga: &mut Vga, memory_map: u8) {
    // Clear sequencer memory-mode chain4 bit (bit 3).
    vga.sequencer_regs[4] = 0x00;
    // Sequencer map mask = 0x04 (plane 2 only). Upper nibble unused.
    vga.sequencer_regs[2] = 0x04;
    // Graphics controller memory map select bits (bits 2-3). Keep the
    // graphics-mode bit clear — BIOS sets other bits during real uploads,
    // but the font-upload predicate only inspects bits 2-3.
    vga.graphics_regs[6] = (memory_map & 0x03) << 2;
}

// BIOS allocations divide each 64 KiB VGA plane into 8, 4 or 2 pages.
#[test]
fn pixel_pages_planar_write_isolation() {
    for (mode, stride, pages, width, height) in [
        (0x0D, 0x2000, 8, 320, 200),
        (0x0E, 0x4000, 4, 640, 200),
        (0x0F, 0x8000, 2, 640, 350),
        (0x10, 0x8000, 2, 640, 350),
    ] {
        let mut vga = Vga::new();
        let mut memory = Memory::new();
        vga.set_mode_bios(mode);
        for page in 0..pages {
            let color = page + 1;
            vga.write_pixel(page, 0, 0, color, &mut memory);
            vga.write_pixel(page, width - 1, height - 1, color, &mut memory);
        }
        for page in 0..pages {
            for (x, y) in [(0, 0), (width - 1, height - 1)] {
                let offset =
                    page as usize * stride + y as usize * (width as usize / 8) + x as usize / 8;
                for plane in 0..4 {
                    assert_eq!(
                        (memory.plane(plane)[offset] >> (7 - x % 8)) & 1,
                        ((page + 1) >> plane) & 1,
                        "mode {mode:02x} page {page} plane {plane}"
                    );
                }
            }
        }
    }
}

#[test]
fn pixel_pages_planar_read_independent() {
    for (mode, stride, pages) in [
        (0x0D, 0x2000, 8),
        (0x0E, 0x4000, 4),
        (0x0F, 0x8000, 2),
        (0x10, 0x8000, 2),
    ] {
        let mut vga = Vga::new();
        let mut memory = Memory::new();
        vga.set_mode_bios(mode);
        for page in 0..pages {
            for plane in 0..4 {
                memory.plane_mut(plane)[page as usize * stride] = if ((page + 1) >> plane) & 1 != 0
                {
                    0x80
                } else {
                    0
                };
            }
        }
        for page in 0..pages {
            assert_eq!(
                vga.read_pixel(page, 0, 0, &memory),
                page + 1,
                "mode {mode:02x} page {page}"
            );
        }
    }
}

#[test]
fn pixel_pages_invalid_and_single_page_keep_page_zero() {
    for (mode, invalid, color) in [
        (0x0D, 8, 0x0A),
        (0x0E, 4, 0x0A),
        (0x0F, 2, 0x0A),
        (0x10, 2, 0x0A),
        (0x11, 1, 0x0A),
        (0x12, 1, 0x0A),
        (0x13, 1, 0x8A),
        (0x04, 1, 3),
        (0x05, 1, 3),
        (0x06, 1, 1),
    ] {
        let mut vga = Vga::new();
        let mut memory = Memory::new();
        vga.set_mode_bios(mode);
        for page in [invalid, 255] {
            vga.write_pixel(0, 0, 0, 1, &mut memory);
            vga.write_pixel(page, 0, 0, color, &mut memory);
            assert_eq!(
                vga.read_pixel(0, 0, 0, &memory),
                color,
                "mode {mode:02x} page {page}"
            );
            assert_eq!(vga.read_pixel(page, 0, 0, &memory), color);
        }
    }
}

#[test]
fn bios_pixel_xor_high_color_bit_toggles_masked_page_color() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0D);
    vga.write_pixel(1, 0, 0, 3, &mut memory);
    vga.write_pixel(1, 0, 0, 0x85, &mut memory);
    assert_eq!(vga.read_pixel(1, 0, 0, &memory), 6);
    assert_eq!(memory.plane(0)[0x2000], 0);
    assert_eq!(memory.plane(1)[0x2000], 0x80);
    assert_eq!(memory.plane(2)[0x2000], 0x80);
    assert_eq!(memory.plane(3)[0x2000], 0);
    assert_eq!(vga.read_pixel(0, 0, 0, &memory), 0);
}

#[test]
fn bios_pixel_xor_cga_04_05_masks_color_and_preserves_banks_and_neighbors() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    for mode in [0x04, 0x05] {
        vga.set_mode_bios(mode);
        for y in [0, 1] {
            let x = 9;
            let neighbor_x = 8;
            let bank_offset = if y & 1 == 0 { 0 } else { 0x2000 };
            let address = 0xB8000 + bank_offset + (y as usize / 2) * 80 + x as usize / 4;
            let initial_byte = 0x90; // neighbor 2, target 1

            vga.write_pixel(1, neighbor_x, y, 2, &mut memory);
            vga.write_pixel(1, x, y, 1, &mut memory);
            assert_eq!(memory.read_u8(address), initial_byte);

            vga.write_pixel(1, x, y, 0x83, &mut memory);
            assert_eq!(vga.read_pixel(1, x, y, &memory), 2);
            assert_eq!(memory.read_u8(address), 0xA0); // neighbor unchanged, target 1 XOR 3

            vga.write_pixel(1, x, y, 0x83, &mut memory);
            assert_eq!(vga.read_pixel(1, x, y, &memory), 1);
            assert_eq!(memory.read_u8(address), initial_byte);

            vga.write_pixel(1, x, y, 0x80, &mut memory);
            assert_eq!(vga.read_pixel(1, x, y, &memory), 1);
            assert_eq!(memory.read_u8(address), initial_byte);

            vga.write_pixel(1, x, y, 0x7E, &mut memory);
            assert_eq!(vga.read_pixel(1, x, y, &memory), 2);
            assert_eq!(memory.read_u8(address), 0xA0);
            assert_eq!(vga.read_pixel(1, neighbor_x, y, &memory), 2);

            vga.write_pixel(1, x, y, 1, &mut memory);
            vga.write_pixel(1, x, y, 0xFE, &mut memory);
            assert_eq!(vga.read_pixel(1, x, y, &memory), 3);
            assert_eq!(memory.read_u8(address), 0xB0);
            vga.write_pixel(1, x, y, 0xFE, &mut memory);
            assert_eq!(vga.read_pixel(1, x, y, &memory), 1);
            assert_eq!(memory.read_u8(address), initial_byte);
        }
    }
}

#[test]
fn bios_pixel_xor_cga_06_masks_color_and_preserves_banks_and_neighbors() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x06);

    for y in [0, 1] {
        let x = 17;
        let neighbor_x = 16;
        let bank_offset = if y & 1 == 0 { 0 } else { 0x2000 };
        let address = 0xB8000 + bank_offset + (y as usize / 2) * 80 + x as usize / 8;

        vga.write_pixel(1, neighbor_x, y, 1, &mut memory);
        vga.write_pixel(1, x, y, 1, &mut memory);
        assert_eq!(memory.read_u8(address), 0xC0);

        vga.write_pixel(1, x, y, 0x83, &mut memory);
        assert_eq!(vga.read_pixel(1, x, y, &memory), 0);
        assert_eq!(memory.read_u8(address), 0x80); // only target bit changed

        vga.write_pixel(1, x, y, 0x83, &mut memory);
        assert_eq!(vga.read_pixel(1, x, y, &memory), 1);
        assert_eq!(memory.read_u8(address), 0xC0);

        vga.write_pixel(1, x, y, 0x80, &mut memory);
        assert_eq!(vga.read_pixel(1, x, y, &memory), 1);
        assert_eq!(memory.read_u8(address), 0xC0);

        vga.write_pixel(1, x, y, 0x7E, &mut memory);
        assert_eq!(vga.read_pixel(1, x, y, &memory), 0);
        assert_eq!(memory.read_u8(address), 0x80);
        assert_eq!(vga.read_pixel(1, neighbor_x, y, &memory), 1);

        vga.write_pixel(1, x, y, 1, &mut memory);
        vga.write_pixel(1, x, y, 0xFE, &mut memory);
        assert_eq!(vga.read_pixel(1, x, y, &memory), 1);
        assert_eq!(memory.read_u8(address), 0xC0);
    }
}

#[test]
fn bios_pixel_xor_planar_modes_mask_color_and_preserve_planes_pages_and_neighbors() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    for (mode, width, height, page, page_stride) in [
        (0x0D, 320, 200, 3, 0x2000),
        (0x0E, 640, 200, 2, 0x4000),
        (0x0F, 640, 350, 1, 0x8000),
        (0x10, 640, 350, 1, 0x8000),
        (0x11, 640, 480, 0, 0),
        (0x12, 640, 480, 0, 0),
    ] {
        vga.set_mode_bios(mode);
        let (x, y) = (10, 2);
        let neighbor_x = x + 1;
        let byte_offset =
            page as usize * page_stride + y as usize * (width as usize / 8) + x as usize / 8;
        let neighbor_mask = 1u8 << (7 - neighbor_x % 8);
        let pixel_mask = 1u8 << (7 - x % 8);

        if page != 0 {
            vga.write_pixel(0, x, y, 5, &mut memory);
        }
        vga.write_pixel(page, neighbor_x, y, 3, &mut memory);
        vga.write_pixel(page, x, y, 0x0A, &mut memory);
        let original_bytes: [u8; 4] = std::array::from_fn(|plane| memory.plane(plane)[byte_offset]);
        let other_page_bytes: [u8; 4] = std::array::from_fn(|plane| {
            memory.plane(plane)[y as usize * (width as usize / 8) + x as usize / 8]
        });
        assert_eq!(vga.read_pixel(page, x, y, &memory), 0x0A);

        vga.write_pixel(page, x, y, 0x85, &mut memory);
        assert_eq!(vga.read_pixel(page, x, y, &memory), 0x0F);
        for plane in 0..4 {
            let toggled = if 0x05 & (1 << plane) != 0 {
                original_bytes[plane] ^ pixel_mask
            } else {
                original_bytes[plane]
            };
            assert_eq!(
                memory.plane(plane)[byte_offset],
                toggled,
                "mode {mode:02x} plane {plane}"
            );
            let neighbor_bit = if 0x03 & (1 << plane) != 0 {
                neighbor_mask
            } else {
                0
            };
            assert_eq!(
                memory.plane(plane)[byte_offset] & neighbor_mask,
                neighbor_bit,
                "mode {mode:02x} neighbor plane {plane}"
            );
        }
        if page != 0 {
            assert_eq!(
                vga.read_pixel(0, x, y, &memory),
                5,
                "mode {mode:02x} other page"
            );
            for plane in 0..4 {
                let other_offset = y as usize * (width as usize / 8) + x as usize / 8;
                assert_eq!(memory.plane(plane)[other_offset], other_page_bytes[plane]);
            }
        }

        vga.write_pixel(page, x, y, 0x85, &mut memory);
        assert_eq!(vga.read_pixel(page, x, y, &memory), 0x0A);
        for plane in 0..4 {
            assert_eq!(memory.plane(plane)[byte_offset], original_bytes[plane]);
        }

        vga.write_pixel(page, x, y, 0x80, &mut memory);
        assert_eq!(vga.read_pixel(page, x, y, &memory), 0x0A);
        for plane in 0..4 {
            assert_eq!(memory.plane(plane)[byte_offset], original_bytes[plane]);
        }

        vga.write_pixel(page, x, y, 0x76, &mut memory);
        assert_eq!(vga.read_pixel(page, x, y, &memory), 0x06);
        for plane in 0..4 {
            let replaced = if 0x06 & (1 << plane) != 0 {
                original_bytes[plane] | pixel_mask
            } else {
                original_bytes[plane] & !pixel_mask
            };
            assert_eq!(
                memory.plane(plane)[byte_offset],
                replaced,
                "mode {mode:02x} replacement plane {plane}"
            );
        }

        vga.write_pixel(page, x, y, 0x0A, &mut memory);
        vga.write_pixel(page, x, y, 0xF6, &mut memory);
        assert_eq!(vga.read_pixel(page, x, y, &memory), 0x0C);
        for plane in 0..4 {
            let toggled = if 0x06 & (1 << plane) != 0 {
                original_bytes[plane] ^ pixel_mask
            } else {
                original_bytes[plane]
            };
            assert_eq!(memory.plane(plane)[byte_offset], toggled);
        }
        vga.write_pixel(page, x, y, 0xF6, &mut memory);
        assert_eq!(vga.read_pixel(page, x, y, &memory), 0x0A);
        for plane in 0..4 {
            assert_eq!(memory.plane(plane)[byte_offset], original_bytes[plane]);
        }

        let bytes_after_replace: [u8; 4] =
            std::array::from_fn(|plane| memory.plane(plane)[byte_offset]);
        vga.write_pixel(page, width, y, 0x0F, &mut memory);
        vga.write_pixel(page, x, height, 0x0F, &mut memory);
        for plane in 0..4 {
            assert_eq!(memory.plane(plane)[byte_offset], bytes_after_replace[plane]);
        }
    }
}

// IBM VGA/XGA Technical Reference (May 1992), p. 2-53, figures 2-34/2-35.
// Literal native addresses and register values keep these oracles independent
// of the device's font-bank helper and selector decoding.
const NATIVE_FONT_OFFSETS: [usize; 8] = [
    0x0000, 0x4000, 0x8000, 0xC000, 0x2000, 0x6000, 0xA000, 0xE000,
];
const NATIVE_FONT_A: [u8; 8] = [0x00, 0x04, 0x08, 0x0C, 0x20, 0x24, 0x28, 0x2C];
const NATIVE_FONT_B: [u8; 8] = [0x00, 0x01, 0x02, 0x03, 0x10, 0x11, 0x12, 0x13];

#[test]
fn native_font_selector_offsets() {
    let mut vga = Vga::new();
    for (bank, &offset) in NATIVE_FONT_OFFSETS.iter().enumerate() {
        assert_eq!(font_bank_offset(bank as u8), offset);
        assert_eq!(font_bank_offset(bank as u8 | 0xF8), offset);
        vga.write_port(0x3C4, 3);
        vga.write_port(0x3C5, NATIVE_FONT_A[bank]);
        assert_eq!(vga.get_font_bank_offsets(), (offset, 0));
        vga.write_port(0x3C5, NATIVE_FONT_B[bank]);
        assert_eq!(vga.get_font_bank_offsets(), (0, offset));
    }
}

#[test]
fn native_font_guest_upload_attribute_ownership() {
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    vga.set_mode_bios(0x12);
    configure_for_font_upload(&mut vga, 1);
    memory.apply_vga_pipeline(&vga.build_pipeline_state());
    // Upload every row of the last glyph through the guest planar aperture.
    // Distinct banks and rows detect aliasing, truncation and incorrect stride.
    for (bank, &offset) in NATIVE_FONT_OFFSETS.iter().enumerate() {
        for row in 0..32 {
            memory.write_u8(0xA0000 + offset + 0xFF * 32 + row, (bank * 32 + row) as u8);
        }
    }
    assert!(memory.planar_font_latch_pending());
    vga.render(&mut memory);
    assert!(vga.custom_font_loaded);
    // All 64 A/B combinations include every same-bank control as well as
    // different lower/upper banks. Attribute bit 3 selects A when set, B clear.
    for (a, &select_a) in NATIVE_FONT_A.iter().enumerate() {
        for (b, &select_b) in NATIVE_FONT_B.iter().enumerate() {
            vga.write_port(0x3C4, 3);
            vga.write_port(0x3C5, select_a | select_b);
            assert_eq!(vga.is_512_char_mode(), a != b);
            for row in 0..32 {
                for (attr, bank) in [(0x07, b), (0x0F, a)] {
                    assert_eq!(
                        vga.get_font_row(&memory, 0xFF, row, 32, attr),
                        (bank * 32 + row) as u8,
                        "A={a}, B={b}, attr={attr}, row={row}"
                    );
                }
            }
        }
    }
}
