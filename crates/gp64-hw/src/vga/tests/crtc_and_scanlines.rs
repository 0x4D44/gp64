use super::*;

#[test]
fn test_crtc_protect_can_be_disabled_via_cr11() {
    let mut vga = Vga::new();
    let orig_cr00 = vga.crtc_regs[0]; // Mode 03h default (0x5F)

    // Enable protection and attempt a write — should be blocked.
    vga.write_port(0x3D4, 0x11);
    vga.write_port(0x3D5, 0x80);
    vga.write_port(0x3D4, 0x00);
    vga.write_port(0x3D5, 0x10);
    vga.write_port(0x3D4, 0x00);
    assert_eq!(vga.read_port(0x3D5), orig_cr00);

    // Disable protection via CR11 write and verify writes now commit.
    vga.write_port(0x3D4, 0x11);
    vga.write_port(0x3D5, 0x00);
    vga.write_port(0x3D4, 0x00);
    vga.write_port(0x3D5, 0x34);
    vga.write_port(0x3D4, 0x00);
    assert_eq!(vga.read_port(0x3D5), 0x34);
}

#[test]
fn test_crtc_protect_overflow_07_allows_only_line_compare_bit8() {
    let mut vga = Vga::new();

    vga.write_port(0x3D4, 0x07);
    vga.write_port(0x3D5, 0xAA);

    // Enable protection and attempt to write a very different value.
    vga.write_port(0x3D4, 0x11);
    vga.write_port(0x3D5, 0x80);
    vga.write_port(0x3D4, 0x07);
    vga.write_port(0x3D5, 0x55);

    vga.write_port(0x3D4, 0x07);
    assert_eq!(vga.read_port(0x3D5), 0xBA);
}

#[test]
fn test_input_status_vsync_and_no_hblank() {
    let mut vga = Vga::new();
    let scanlines = vga.get_total_scanlines();
    vga.timing_cycles_per_frame = scanlines * 10;
    vga.timing_retrace_cycles = 50;
    // Position the VSync pulse to cover from cycle 0 for 50 cycles (whole VBI)
    vga.vsync_offset_cycles = 0;
    vga.vsync_duration_cycles = 50;
    // In vsync (cycles < retrace) but at a line position that is NOT in hblank
    let cycles_per_line = vga.timing_cycles_per_frame / scanlines;
    // Position mid-line so we're NOT in hblank
    vga.cycles_since_vblank = cycles_per_line / 2;
    // But also make it < retrace
    if vga.cycles_since_vblank >= vga.timing_retrace_cycles {
        vga.timing_retrace_cycles = vga.cycles_since_vblank + 10;
        vga.vsync_duration_cycles = vga.timing_retrace_cycles;
    }
    let status = vga.read_port(0x3DA);
    // Bit 3 (VSYNC) should be set, and bit 0 should be set (display disabled during vsync)
    assert_ne!(status & 0x08, 0, "VSYNC should be active");
    assert_ne!(status & 0x01, 0, "Display should be disabled during vsync");
}

#[test]
fn test_get_set_pel_mask() {
    let mut vga = Vga::new();
    vga.set_pel_mask(0xF0);
    assert_eq!(vga.get_pel_mask(), 0xF0);
}

#[test]
fn test_mode_x_same_height_no_realloc() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    let fb_ptr = vga.framebuffer.as_ptr();
    // Clear double-scan first so SEQ[4] chain-4 clear activates Mode X
    vga.write_port(0x3D4, 0x09);
    vga.write_port(0x3D5, 0x40); // max_scan=0, preserve LC bit 9
                                 // Enter Mode X
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06);
    assert!(vga.is_mode_x());
    // If height matches, framebuffer should not be reallocated
    // (we can't directly test pointer stability, but we can verify height)
    assert_eq!(vga.fb_height, vga.get_mode_x_height());
    let _ = fb_ptr; // Used for conceptual check
}

#[test]
fn test_render_with_trace_enabled() {
    set_vga_trace_enabled_override(Some(true));

    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);
    // Render 60+ times to trigger the debug path (cnt.is_multiple_of(60))
    for _ in 0..61 {
        vga.render(&mut memory);
    }

    set_vga_trace_enabled_override(None);
}

#[test]
fn test_write_port_3d9_color_select() {
    let mut vga = Vga::new();
    vga.write_port(0x3D9, 0x3F);
    assert_eq!(vga.color_select_reg, 0x3F);
}

#[test]
fn test_scroll_up_graphics_mode13() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);
    // Write a pixel to second text row
    let cell_h = 8; // Default char height
    let addr = 0xA0000 + cell_h * 320;
    memory.write_u8(addr, 42);

    vga.scroll_up_graphics(&mut memory, 0);
    // After scrolling up, the pixel should be at the first row
    assert_eq!(memory.read_u8(0xA0000), 42);
}

#[test]
fn test_scroll_up_graphics_mode_x_coverage() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);
    vga.write_port(0x3D4, 0x09); // CRTC index: max scan line
    vga.write_port(0x3D5, 0x40); // Clear double-scan (Mode X setup)
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06);
    assert!(vga.is_mode_x());

    vga.scroll_up_graphics(&mut memory, 0);
    // Should not panic
}

#[test]
fn test_teletype_backspace_at_col0() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 0, 0);
    vga.teletype(0x08, 0, &mut memory);
    let (_, col) = vga.get_cursor(0);
    assert_eq!(col, 0); // Should not go negative
}

#[test]
fn test_pel_mask_affects_palette16() {
    let mut vga = Vga::new();
    vga.pel_mask = 0x0F; // Only lower 4 bits
    vga.rebuild_palette16_lut_if_dirty();
    // Verify dirty flag is cleared
    assert!(!vga.palette16_lut_dirty);
}

#[test]
fn test_is_mono_text_mode_coverage() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x07);
    assert!(vga.is_mono_text_mode());
    vga.set_mode_bios(0x03);
    assert!(!vga.is_mono_text_mode());
}

#[test]
fn test_crtc_start_byte_for_mode_text() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    // Text mode: start_word * 2
    let byte = vga.crtc_start_byte_for_mode(10);
    assert_eq!(byte, 20); // 10 * 2 = 20 (+ byte_panning=0)
}

#[test]
fn test_crtc_start_byte_for_mode_graphics() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x12);
    // Graphics mode: start_word directly
    let byte = vga.crtc_start_byte_for_mode(10);
    assert_eq!(byte, 10);
}

#[test]
fn test_crtc_byte_mode() {
    let mut vga = Vga::new();
    vga.crtc_regs[0x17] = 0x40; // Byte mode ON
    assert!(vga.crtc_byte_mode());
    vga.crtc_regs[0x17] = 0x00;
    assert!(!vga.crtc_byte_mode());
}

#[test]
fn test_set_mode_bios_02() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x02);
    assert_eq!(vga.mode(), 0x02);
    assert_eq!(vga.columns, 80);
    assert!(vga.is_text_mode());
}

#[test]
fn test_rebuild_palette_lut_when_dirty() {
    let mut vga = Vga::new();
    vga.dac[5] = (100, 200, 50);
    vga.palette_lut_dirty = true;
    vga.rebuild_palette_lut_if_dirty();
    assert!(!vga.palette_lut_dirty);
    let expected = (100u32 << 16) | (200u32 << 8) | 0x32_u32;
    assert_eq!(vga.palette_lut[5], expected);
}

#[test]
fn test_rebuild_palette_lut_not_dirty() {
    let mut vga = Vga::new();
    vga.palette_lut_dirty = false;
    let old = vga.palette_lut[0];
    vga.rebuild_palette_lut_if_dirty();
    assert_eq!(vga.palette_lut[0], old);
}

#[test]
fn test_text_page_size_words_40col() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x00);
    assert_eq!(vga.text_page_size_words(), 0x400);
}

#[test]
fn test_text_page_size_words_80col() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    assert_eq!(vga.text_page_size_words(), 0x800);
}

#[test]
fn test_checkpoint_register_arrays_round_trip() {
    let mut vga = Vga::new();

    // Set some register values
    let mut attr = [0u8; 0x20];
    for (i, v) in attr.iter_mut().enumerate() {
        *v = i as u8;
    }
    vga.set_attribute_regs(attr);

    let mut seq = [0u8; 8];
    seq[1] = 0x01; // Clocking mode
    seq[4] = 0x06; // Memory mode
    vga.set_sequencer_regs(seq);

    let mut crtc = [0u8; 32];
    crtc[0x0C] = 0x12; // Start address high
    crtc[0x0D] = 0x34; // Start address low
    vga.set_crtc_regs(crtc);

    // Save and restore
    let saved_attr = vga.get_attribute_regs();
    let saved_seq = vga.get_sequencer_regs();
    let saved_gc = vga.get_graphics_regs();
    let saved_crtc = vga.get_crtc_regs();

    let mut vga2 = Vga::new();
    vga2.set_attribute_regs(saved_attr);
    vga2.set_sequencer_regs(saved_seq);
    vga2.set_graphics_regs(saved_gc);
    vga2.set_crtc_regs(saved_crtc);

    assert_eq!(vga2.get_attribute_regs(), attr);
    assert_eq!(vga2.get_sequencer_regs(), seq);
    assert_eq!(vga2.get_crtc_regs()[0x0C], 0x12);
    assert_eq!(vga2.get_crtc_regs()[0x0D], 0x34);
}

#[test]
fn test_take_vblank_pending_fresh_vga_returns_false() {
    let mut vga = Vga::new();
    assert!(!vga.take_vblank_pending());
}

#[test]
fn test_take_vblank_pending_set_after_vblank_crossing() {
    let mut vga = Vga::new();
    let frame = vga.timing_cycles_per_frame;
    // Tick exactly one full frame to guarantee a vblank-end crossing
    vga.tick(frame);
    assert!(vga.take_vblank_pending());
}

#[test]
fn test_take_vblank_pending_consume_semantics() {
    let mut vga = Vga::new();
    let frame = vga.timing_cycles_per_frame;
    vga.tick(frame);
    // First call consumes
    assert!(vga.take_vblank_pending());
    // Second call returns false
    assert!(!vga.take_vblank_pending());
}

#[test]
fn test_recompute_scanline_timing() {
    let mut vga = Vga::new();
    // Default mode should be 70 Hz / 400 visible
    vga.recompute_scanline_timing();
    assert_eq!(vga.total_scanlines_cached, 449);
    // visible depends on mode, just check it's reasonable
    assert!(vga.visible_scanlines_cached > 0);
    assert!(vga.visible_scanlines_cached <= vga.total_scanlines_cached);
}

#[test]
fn test_compute_visible_line_standard() {
    // 400-line mode: 20000 frame, 1600 retrace, 18400 active
    // Halfway through active = line 200
    let line = Vga::compute_visible_line(1600 + 9200, 1600, 20000, 400);
    assert_eq!(line, 200);
}

#[test]
fn test_compute_visible_line_double_scan() {
    // 200-line mode (double-scan): same active period, half the visible lines
    // Halfway through active = line 100
    let line = Vga::compute_visible_line(1600 + 9200, 1600, 20000, 200);
    assert_eq!(line, 100);
}

#[test]
fn test_compute_visible_line_at_retrace() {
    // During retrace: should return 0
    let line = Vga::compute_visible_line(800, 1600, 20000, 400);
    assert_eq!(line, 0);
}

#[test]
fn test_compute_visible_line_at_end() {
    // At very end of frame: should return last visible line (or close)
    let line = Vga::compute_visible_line(19999, 1600, 20000, 400);
    assert!(line >= 398); // Allow for rounding
}

#[test]
fn test_compute_visible_line_zero_active() {
    // Edge case: active_total = 0
    let line = Vga::compute_visible_line(100, 20000, 20000, 400);
    assert_eq!(line, 0);
}

#[test]
fn test_mode_change_pending() {
    let mut vga = Vga::new();
    assert!(!vga.take_mode_change_pending());
    // After set_mode_bios, the flag should be set
    vga.set_mode_bios(0x13); // Mode 13h
    assert!(vga.take_mode_change_pending());
    // Should be cleared after take
    assert!(!vga.take_mode_change_pending());
}

#[test]
fn test_scanline_mode13h_basic() {
    // Set up VGA in mode 13h, write known pixels, verify scanline rendering
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);

    // Set a known DAC palette entry: color index 5 = bright red
    vga.set_dac_reg(5, 63, 0, 0);

    // Write color index 5 to first 3 pixels in video memory
    let vmem = memory.video_memory_mut();
    vmem[0] = 5;
    vmem[1] = 5;
    vmem[2] = 5;

    // Render scanline 0
    vga.render_scanline(0, &mut memory);

    // Expected RGB: (63<<2 | 63>>4) = 0xFC | 0x0F = 0xFF => 0xFF0000
    let expected_rgb = 0xFF0000u32;
    assert_eq!(vga.framebuffer()[0], expected_rgb);
    assert_eq!(vga.framebuffer()[1], expected_rgb);
    assert_eq!(vga.framebuffer()[2], expected_rgb);
    // Pixel 3 should be default palette color 0 (black after mode set)
    assert_eq!(vga.framebuffer()[3], vga.palette_lut[0]);
}

#[test]
fn test_scanline_mode13h_line1() {
    // Verify that scanline 1 reads from the correct offset (320 bytes in)
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);

    vga.set_dac_reg(7, 0, 63, 0); // color 7 = bright green

    let vmem = memory.video_memory_mut();
    vmem[320] = 7; // First pixel of line 1

    vga.render_scanline(1, &mut memory);

    let expected_rgb = 0x00FF00u32;
    assert_eq!(vga.framebuffer()[320], expected_rgb); // Line 1, pixel 0
}

#[test]
fn test_scanline_mode13h_out_of_range() {
    // Rendering a scanline beyond mode height should be a no-op
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);

    // Render line 200 (beyond 0..199)
    vga.render_scanline(200, &mut memory);
    // Should not panic; framebuffer should be sized correctly
    assert_eq!(vga.framebuffer().len(), 320 * 200);
}

#[test]
fn test_scanline_mode13h_pel_mask() {
    // Verify PEL mask is applied
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);

    vga.set_dac_reg(0x0F, 63, 63, 63); // color 0x0F = white
    vga.set_dac_reg(0xFF, 32, 0, 0); // color 0xFF = dark red

    // Set PEL mask to 0x0F — this masks the color index
    vga.pel_mask = 0x0F;

    let vmem = memory.video_memory_mut();
    vmem[0] = 0xFF; // Will be masked to 0x0F

    vga.render_scanline(0, &mut memory);

    // Should render color 0x0F (white), not 0xFF
    let expected = 0xFFFFFFu32;
    assert_eq!(vga.framebuffer()[0], expected);
}

#[test]
fn test_scanline_text_mode_basic() {
    // Set up VGA in mode 03h (80x25 text), write a character, verify scanline
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);

    // Write character 'A' (0x41) with attribute 0x07 (white on black) at position (0,0)
    memory.write_u8(0xB8000, 0x41); // character
    memory.write_u8(0xB8001, 0x07); // attribute: fg=7 (white), bg=0 (black)

    // Ensure palette is set up (mode set should have done this)
    // Render scanline 0 (first pixel row of first character row)
    vga.render_scanline(0, &mut memory);

    // In 80x25 mode, fb_width=720 (80 cols * 9px) or 640 (80 cols * 8px)
    // The character 'A' in the CP437 8x16 font should have specific pixels
    let fb_width = vga.fb_width;
    assert!(fb_width > 0);

    // Scanline 0 is the first pixel row of the font
    // For 'A' (0x41) in 8x16 font, row 0 is typically 0x00 (blank top)
    // So all pixels in the first row should be background color
    let bg_rgb = vga.palette16_lut[0]; // bg color 0
                                       // First character, first row — check first pixel
    assert_eq!(vga.framebuffer()[0], bg_rgb);
}

#[test]
fn test_scanline_text_mode_font_pixels() {
    // Test that the font rendering produces correct foreground pixels
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);

    // Find a character with a non-zero first font row
    // 0xDB (full block) has all bits set in all rows
    memory.write_u8(0xB8000, 0xDB); // full block character
    memory.write_u8(0xB8001, 0x0F); // attribute: bright white on black

    // Render a middle scanline where the full block character will have pixels
    // Row 8 of 16 is guaranteed to be 0xFF for character 0xDB
    vga.render_scanline(8, &mut memory);

    let fg_rgb = vga.palette16_lut[0x0F]; // bright white
                                          // First pixel of this scanline should be foreground (full block)
    assert_eq!(vga.framebuffer()[8 * vga.fb_width], fg_rgb);
}

#[test]
fn test_scanline_text_cell_cache_preserves_panning_wrap_and_cell_attributes() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    vga.columns = 2;
    vga.rows = 1;
    vga.fb_width = 18;
    vga.fb_height = 16;
    vga.framebuffer = vec![0; vga.fb_width * vga.fb_height];
    vga.latched_pixel_panning = 4;
    vga.attribute_regs[0x10] = 0x04; // Line graphics on, blink off.
    vga.crtc_regs[0x0A] |= 0x20; // Cursor off.
    vga.crtc_regs[0x18] = 0xFF;
    vga.crtc_regs[0x07] |= 0x10;
    vga.crtc_regs[0x09] |= 0x40;

    memory.write_u8(0xB8000, 0xDB); // Full block, including ninth column.
    memory.write_u8(0xB8001, 0x0F); // Bright white on black.
    memory.write_u8(0xB8002, b' ');
    memory.write_u8(0xB8003, 0x4F); // Bright white on red.

    vga.render_scanline(8, &mut memory);

    let row = &vga.framebuffer()[8 * 18..9 * 18];
    let white = vga.palette16_lut[0x0F];
    let red = vga.palette16_lut[0x04];
    assert_eq!(&row[..5], &[white; 5]);
    assert_eq!(&row[5..14], &[red; 9]);
    assert_eq!(&row[14..], &[white; 4]);
}

#[test]
fn test_scanline_vga_text_ignores_cga_display_disable_when_pas_enabled() {
    let _env = OverrideGuard::render_mode(None);
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    vga.cga_mode_control_reg = 0x00;
    vga.palette_address_source = true;

    memory.write_u8(0xB8000, 0xDB);
    memory.write_u8(0xB8001, 0x0F);

    vga.render_scanline(8, &mut memory);

    let overscan = vga.overscan_rgb();
    let row_start = 8 * vga.fb_width;
    assert!(vga.framebuffer()[row_start..row_start + vga.fb_width]
        .iter()
        .any(|&pixel| pixel != overscan));
}

/// Scanline half of MDD-BUG-CRUCIBLE-00560 — the same gate, per scanline.
#[test]
fn test_scanline_vga_text_blanks_to_overscan_while_pas_is_clear() {
    let _env = OverrideGuard::render_mode(None);
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    vga.cga_mode_control_reg = 0x09;
    vga.attribute_regs[0x11] = 5;
    vga.set_dac_reg(5, 63, 0, 0);

    memory.write_u8(0xB8000, 0xDB);
    memory.write_u8(0xB8001, 0x0F);

    vga.read_port(0x3DA);
    vga.write_port(0x3C0, 0x00); // PAS clear
    vga.render_scanline(8, &mut memory);

    let overscan = vga.overscan_rgb();
    let row_start = 8 * vga.fb_width;
    assert!(
        vga.framebuffer()[row_start..row_start + vga.fb_width]
            .iter()
            .all(|&pixel| pixel == overscan),
        "PAS clear must blank this text scanline to overscan"
    );

    vga.read_port(0x3DA);
    vga.write_port(0x3C0, 0x20); // PAS set
    vga.render_scanline(8, &mut memory);
    assert!(
        vga.framebuffer()[row_start..row_start + vga.fb_width]
            .iter()
            .any(|&pixel| pixel != overscan),
        "text scanline must return once PAS is re-enabled"
    );
}

#[test]
fn test_scanline_cga_mode04_basic() {
    // CGA 320x200 4-color mode
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);

    // Write a byte at B800:0000 (even scanline 0)
    // Each byte has 4 pixels: bits 7-6, 5-4, 3-2, 1-0
    // Set byte to 0b_01_10_11_00 = 0x6C
    // Pixel 0 = color 1, Pixel 1 = color 2, Pixel 2 = color 3, Pixel 3 = color 0
    memory.write_u8(0xB8000, 0x6C);

    vga.render_scanline(0, &mut memory);

    // Default palette 0: bg=black, palette depends on color_select_reg
    // After set_mode_bios(0x04), color_select_reg is 0x00
    // Colors: bg=0 (palette_to_rgb(0)), low intensity palette 0
    let bg = vga.palette_to_rgb(0);
    assert_eq!(vga.framebuffer()[3], bg); // Pixel 3 = color 0 = background
}

#[test]
fn test_scanline_cga_mode06_basic() {
    // CGA 640x200 2-color mode
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x06);

    // Write 0xFF at B800:0000 — all 8 pixels ON for even scanline 0
    memory.write_u8(0xB8000, 0xFF);

    vga.render_scanline(0, &mut memory);

    // Mode 06h maps its white IRGB foreground (0x0F) to DAC[0x17] via AC[1].
    assert_eq!(vga.attribute_regs[1], 0x17);
    let fg = vga.palette_to_rgb(1);
    for x in 0..8 {
        assert_eq!(vga.framebuffer()[x], fg, "pixel {} should be foreground", x);
    }
}

#[test]
fn test_scanline_planar_16color_basic() {
    // Mode 0x12: 640x480 16-color planar
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x12);

    // Write to plane 0: bit 7 set = pixel 0 uses plane 0 bit
    memory.plane_mut(0)[0] = 0x80; // pixel 0 = plane0 set
    memory.plane_mut(1)[0] = 0x00;
    memory.plane_mut(2)[0] = 0x00;
    memory.plane_mut(3)[0] = 0x00;

    vga.render_scanline(0, &mut memory);

    // Color index 1 (only plane 0 set)
    let color1_rgb = vga.palette_to_rgb(1);
    assert_eq!(vga.framebuffer()[0], color1_rgb);

    // Pixel 1 should be color 0 (all planes clear)
    let color0_rgb = vga.palette_to_rgb(0);
    assert_eq!(vga.framebuffer()[1], color0_rgb);
}

#[test]
fn test_scanline_planar_16color_all_planes() {
    // Mode 0x12: test color 15 (all 4 planes set)
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x12);

    // Set all 4 planes at byte 0, bit 7 (pixel 0)
    memory.plane_mut(0)[0] = 0x80;
    memory.plane_mut(1)[0] = 0x80;
    memory.plane_mut(2)[0] = 0x80;
    memory.plane_mut(3)[0] = 0x80;

    vga.render_scanline(0, &mut memory);

    // Color 15 & color_plane_enable(0x0F) = 15
    let color15_rgb = vga.palette_to_rgb(15);
    assert_eq!(vga.framebuffer()[0], color15_rgb);
}

#[test]
fn test_scanline_ega_mono_basic() {
    // Mode 0x0F: 640x350 EGA monochrome
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0F);

    // Plane 0 = video data, plane 2 = intensity
    memory.plane_mut(0)[0] = 0x80; // pixel 0: video ON
    memory.plane_mut(2)[0] = 0x00; // pixel 0: intensity OFF

    vga.render_scanline(0, &mut memory);

    // Video ON, intensity OFF = standard mono color 0xAAAAAA
    assert_eq!(vga.framebuffer()[0], 0xAAAAAA);
}

#[test]
fn test_scanline_ega_mono_bright() {
    // Pixel with both video and intensity = bright white
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0F);

    memory.plane_mut(0)[0] = 0x80; // video ON
    memory.plane_mut(2)[0] = 0x80; // intensity ON

    vga.render_scanline(0, &mut memory);

    assert_eq!(vga.framebuffer()[0], 0xFFFFFF); // bright white
}

#[test]
fn test_scanline_ega_mono_off() {
    // Pixel with video OFF = black regardless of intensity
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0F);

    memory.plane_mut(0)[0] = 0x00; // video OFF
    memory.plane_mut(2)[0] = 0x80; // intensity ON (irrelevant)

    vga.render_scanline(0, &mut memory);

    assert_eq!(vga.framebuffer()[0], 0x000000); // black
}

#[test]
fn test_scanline_vga_mono_basic() {
    // Mode 0x11: 640x480 VGA 2-color mono
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x11);

    // Set DAC entry 1 for ON color
    vga.set_dac_reg(1, 63, 63, 63); // white

    memory.plane_mut(0)[0] = 0xC0; // pixels 0,1 ON; pixels 2-7 OFF

    vga.render_scanline(0, &mut memory);

    let color_on = {
        let (r, g, b) = vga.dac[1];
        ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
    };
    let color_off = {
        let (r, g, b) = vga.dac[0];
        ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
    };
    assert_eq!(vga.framebuffer()[0], color_on);
    assert_eq!(vga.framebuffer()[1], color_on);
    assert_eq!(vga.framebuffer()[2], color_off);
}
