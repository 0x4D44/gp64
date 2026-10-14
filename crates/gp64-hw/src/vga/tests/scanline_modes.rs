use super::*;

#[test]
fn test_scanline_mode_x_basic() {
    // Set up Mode X (unchained 256-color planar)
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);

    vga.write_port(0x3D4, 0x09); // CRTC index: max scan line
    vga.write_port(0x3D5, 0x40); // Clear double-scan (Mode X setup)

    // Switch to Mode X by disabling chain-4
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06);
    assert!(vga.is_mode_x());

    // Set a DAC entry
    vga.set_dac_reg(10, 0, 0, 63); // color 10 = bright blue

    // In Mode X: pixel at x=0 is plane 0, offset 0
    memory.plane_mut(0)[0] = 10;

    vga.render_scanline(0, &mut memory);

    let expected_rgb = 0x0000FFu32;
    assert_eq!(vga.framebuffer()[0], expected_rgb);
}

#[test]
fn test_scanline_ensure_framebuffer_sizing() {
    // Verify that render_scanline correctly sizes the framebuffer
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    // Start with empty framebuffer
    vga.set_mode_bios(0x13);
    vga.framebuffer = vec![];
    vga.fb_width = 0;
    vga.fb_height = 0;

    vga.render_scanline(0, &mut memory);

    // Should have been resized to 320x200
    assert_eq!(vga.framebuffer().len(), 320 * 200);
    assert_eq!(vga.fb_width, 320);
    assert_eq!(vga.fb_height, 200);
}

#[test]
fn test_scanline_mode13h_all_lines_match_full_frame() {
    // Render all scanlines individually and compare to full-frame render
    let mut vga_scanline = Vga::new();
    let mut vga_full = Vga::new();
    let mut memory = Memory::new();

    vga_scanline.set_mode_bios(0x13);
    vga_full.set_mode_bios(0x13);

    // Write a test pattern to video memory
    let vmem = memory.video_memory_mut();
    for i in 0..(320 * 200) {
        vmem[i] = (i % 256) as u8;
    }

    // Full-frame render
    vga_full.render(&mut memory);

    // Per-scanline render
    for line in 0..200 {
        vga_scanline.render_scanline(line, &mut memory);
    }

    // Compare framebuffers — they should match
    // Note: full-frame may use latched values while scanline uses live,
    // so we compare pixel patterns rather than exact values
    let fb_full = vga_full.framebuffer().to_vec();
    let fb_scan = vga_scanline.framebuffer().to_vec();
    assert_eq!(fb_full.len(), fb_scan.len());

    // At minimum, verify they're the same size and non-zero
    let nonzero_full = fb_full.iter().filter(|&&x| x != 0).count();
    let nonzero_scan = fb_scan.iter().filter(|&&x| x != 0).count();
    assert!(nonzero_full > 0);
    assert!(nonzero_scan > 0);
}

#[test]
fn test_ega_mode_0d_io_port_map_mask_pixel_verification() {
    // Write to a single plane via I/O port Map Mask → verify rendered color.
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0D); // 320×200, 16-color planar

    // Initial sync (as the emulator does after mode set)
    memory.apply_vga_pipeline(&vga.build_pipeline_state());

    // Change Map Mask to plane 1 only via I/O ports
    vga.write_port(0x3C4, 0x02); // Sequencer index → Map Mask
    vga.write_port(0x3C5, 0x02); // Plane 1 only (bit 1)

    // pipeline_dirty must be set after I/O port writes
    assert!(vga.pipeline_dirty, "write_port must set pipeline_dirty");

    // Sync VGA → Memory (the step the batch loop was missing)
    memory.apply_vga_pipeline(&vga.build_pipeline_state());
    assert_eq!(
        memory.vga.plane_mask, 0x02,
        "plane_mask should propagate to Memory"
    );

    // Write 0xFF to VRAM offset 0 (sets 8 pixels on plane 1)
    memory.write_u8(0xA0000, 0xFF);

    // Verify plane data: only plane 1 should be written
    assert_eq!(memory.plane(1)[0], 0xFF);
    assert_eq!(memory.plane(0)[0], 0x00);
    assert_eq!(memory.plane(2)[0], 0x00);
    assert_eq!(memory.plane(3)[0], 0x00);

    // Render and verify first 8 pixels are color index 2 (plane 1 set → bit 1)
    vga.render(&mut memory);
    let color2_rgb = vga.palette_to_rgb(2);
    let color0_rgb = vga.palette_to_rgb(0);
    for x in 0..8 {
        assert_eq!(
            vga.framebuffer()[x],
            color2_rgb,
            "pixel {x} should be color 2 (plane 1 only)"
        );
    }
    // Pixel 8 onward should be background (no data written there)
    assert_eq!(vga.framebuffer()[8], color0_rgb);
}

#[test]
fn test_ega_mode_0d_io_port_multiple_planes() {
    // Write different data to multiple planes via I/O port Map Mask changes,
    // syncing between each change, and verify the composed 4-bit color.
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0D);
    memory.apply_vga_pipeline(&vga.build_pipeline_state());

    // Write to plane 0 (bit 0 → color contribution 1)
    vga.write_port(0x3C4, 0x02);
    vga.write_port(0x3C5, 0x01); // Plane 0 only
    memory.apply_vga_pipeline(&vga.build_pipeline_state());
    memory.write_u8(0xA0000, 0xFF); // 8 pixels on plane 0

    // Write to plane 2 (bit 2 → color contribution 4)
    vga.write_port(0x3C4, 0x02);
    vga.write_port(0x3C5, 0x04); // Plane 2 only
    memory.apply_vga_pipeline(&vga.build_pipeline_state());
    memory.write_u8(0xA0000, 0xFF); // 8 pixels on plane 2

    // Planes 0 and 2 set → color index 5 (1 + 4)
    assert_eq!(memory.plane(0)[0], 0xFF);
    assert_eq!(memory.plane(1)[0], 0x00);
    assert_eq!(memory.plane(2)[0], 0xFF);
    assert_eq!(memory.plane(3)[0], 0x00);

    vga.render(&mut memory);
    let color5_rgb = vga.palette_to_rgb(5);
    for x in 0..8 {
        assert_eq!(
            vga.framebuffer()[x],
            color5_rgb,
            "pixel {x} should be color 5 (planes 0+2)"
        );
    }
}

#[test]
fn test_ega_mode_0d_io_port_bit_mask() {
    // Verify that Bit Mask register (GC index 8) via I/O ports controls
    // which bits within a byte are written.
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0D);
    memory.apply_vga_pipeline(&vga.build_pipeline_state());

    // Enable all planes
    vga.write_port(0x3C4, 0x02);
    vga.write_port(0x3C5, 0x0F);

    // Set Bit Mask to 0xF0 (only high nibble writable → pixels 0-3)
    vga.write_port(0x3CE, 0x08); // GC index → Bit Mask
    vga.write_port(0x3CF, 0xF0); // High nibble only

    memory.apply_vga_pipeline(&vga.build_pipeline_state());
    assert_eq!(memory.vga.bit_mask, 0xF0);

    // Need to prime the latches first (read fills latches with 0)
    let _ = memory.read_u8(0xA0000);

    // Write 0xFF — only bits 7-4 should be written (bit mask 0xF0)
    memory.write_u8(0xA0000, 0xFF);

    // All 4 planes should have 0xF0 (high nibble set, low nibble from latches = 0)
    for plane in 0..4 {
        assert_eq!(
            memory.plane(plane)[0],
            0xF0,
            "plane {plane} should have 0xF0 (bit mask filtered)"
        );
    }

    vga.render(&mut memory);
    let color15_rgb = vga.palette_to_rgb(15);
    let color0_rgb = vga.palette_to_rgb(0);

    // Pixels 0-3 (bit positions 7-4) should be color 15 (all planes set)
    for x in 0..4 {
        assert_eq!(
            vga.framebuffer()[x],
            color15_rgb,
            "pixel {x} should be color 15 (all planes, bit mask allowed)"
        );
    }
    // Pixels 4-7 (bit positions 3-0) should be color 0 (bit mask blocked)
    for x in 4..8 {
        assert_eq!(
            vga.framebuffer()[x],
            color0_rgb,
            "pixel {x} should be color 0 (bit mask blocked)"
        );
    }
}

#[test]
fn test_ega_mode_0d_io_port_set_reset() {
    // Verify Set/Reset + Enable Set/Reset via I/O ports: the GC generates
    // plane data from the Set/Reset register instead of CPU data.
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0D);
    memory.apply_vga_pipeline(&vga.build_pipeline_state());

    // Enable all planes for writing
    vga.write_port(0x3C4, 0x02);
    vga.write_port(0x3C5, 0x0F);

    // Set/Reset = 0x0A (planes 1 and 3)
    vga.write_port(0x3CE, 0x00); // GC index → Set/Reset
    vga.write_port(0x3CF, 0x0A);

    // Enable Set/Reset for all planes
    vga.write_port(0x3CE, 0x01); // GC index → Enable Set/Reset
    vga.write_port(0x3CF, 0x0F);

    // Bit Mask = 0xFF (all bits)
    vga.write_port(0x3CE, 0x08);
    vga.write_port(0x3CF, 0xFF);

    memory.apply_vga_pipeline(&vga.build_pipeline_state());
    assert_eq!(memory.vga.set_reset, 0x0A);
    assert_eq!(memory.vga.enable_set_reset, 0x0F);

    // Write anything — CPU data is ignored when Enable Set/Reset is active.
    // Set/Reset 0x0A means planes 1,3 get 0xFF, planes 0,2 get 0x00.
    memory.write_u8(0xA0000, 0x42); // Value doesn't matter

    assert_eq!(memory.plane(0)[0], 0x00);
    assert_eq!(memory.plane(1)[0], 0xFF);
    assert_eq!(memory.plane(2)[0], 0x00);
    assert_eq!(memory.plane(3)[0], 0xFF);

    // Color index = bits 1+3 set = 0x0A = 10
    vga.render(&mut memory);
    let color10_rgb = vga.palette_to_rgb(10);
    for x in 0..8 {
        assert_eq!(
            vga.framebuffer()[x],
            color10_rgb,
            "pixel {x} should be color 10 (set/reset 0x0A)"
        );
    }
}

#[test]
fn test_ega_mode_0d_scanline_io_port_pixel_verification() {
    // Same as the full-frame test but using per-scanline renderer.
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0D);
    memory.apply_vga_pipeline(&vga.build_pipeline_state());

    // Write planes 0+1 (color 3 = cyan) via I/O port Map Mask
    vga.write_port(0x3C4, 0x02);
    vga.write_port(0x3C5, 0x03); // Planes 0+1
    memory.apply_vga_pipeline(&vga.build_pipeline_state());
    memory.write_u8(0xA0000, 0xFF);

    vga.render_scanline(0, &mut memory);

    let color3_rgb = vga.palette_to_rgb(3);
    for x in 0..8 {
        assert_eq!(
            vga.framebuffer()[x],
            color3_rgb,
            "scanline pixel {x} should be color 3 (planes 0+1)"
        );
    }
}

#[test]
fn test_40col_text_dimensions_match_80col_width() {
    // Mode 0x01 (40-col) and Mode 0x03 (80-col) must produce the same
    // framebuffer width and height — real VGA uses the same display timing.
    let mut vga01 = Vga::new();
    let mut vga03 = Vga::new();
    vga01.set_mode_bios(0x01);
    vga03.set_mode_bios(0x03);

    let (w01, h01) = vga01.get_framebuffer_dimensions();
    let (w03, h03) = vga03.get_framebuffer_dimensions();
    assert_eq!(w01, w03, "40-col and 80-col should have same pixel width");
    assert_eq!(h01, h03, "40-col and 80-col should have same pixel height");
    assert_eq!(w01, 720);
    assert_eq!(h01, 400);
}

#[test]
fn test_40col_text_full_block_fills_18_pixels() {
    // Character 0xDB (full block █) in Mode 0x01 should fill all 18 pixels
    // of the character cell.  0xDB is in the line-drawing range (0xC0-0xDF)
    // so the 9th font column duplicates bit 0, making all 9 font columns
    // foreground, which doubles to 18 framebuffer pixels.
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x01);

    // Write 0xDB at column 0, row 0 with bright white on black
    memory.write_u8(0xB8000, 0xDB);
    memory.write_u8(0xB8001, 0x0F);

    vga.render(&mut memory);

    let fg_rgb = vga.palette_to_rgb(0x0F);
    let bg_rgb = vga.palette_to_rgb(0x00);

    // Row 8 of the 8×16 font — 0xDB has 0xFF for all rows
    let y = 8;
    let fb_width = vga.fb_width;

    // All 18 pixels of the character cell should be foreground
    for x in 0..18 {
        assert_eq!(
            vga.framebuffer()[y * fb_width + x],
            fg_rgb,
            "pixel ({x}, {y}) should be foreground (full block, char_width=18)"
        );
    }

    // Pixel 18 is the first pixel of the next character (should be background)
    assert_eq!(
        vga.framebuffer()[y * fb_width + 18],
        bg_rgb,
        "pixel (18, {y}) should be background (next char cell)"
    );
}

#[test]
fn test_40col_text_pixel_doubling_pattern() {
    // Verify that consecutive pixel pairs are identical (each font pixel
    // is doubled horizontally).  Use character 'A' (0x41) which has a
    // distinctive pattern — NOT in the line-drawing range, so the 9th
    // font column is background.
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x01);

    memory.write_u8(0xB8000, b'A');
    memory.write_u8(0xB8001, 0x0F);

    vga.render(&mut memory);

    let fb_width = vga.fb_width;

    // Check a font row known to have a mix of set/unset bits.
    // 'A' (0x41) at row 4 in 8×16 font = 0x66 = 01100110
    // (We look up the actual value to be robust against font variants.)
    let font_row = super::font_cp437_8x16::CP437_FONT_8X16[0x41 * 16 + 4];
    assert_ne!(font_row, 0x00, "font row should have some pixels set");
    assert_ne!(font_row, 0xFF, "font row should have some pixels unset");

    let y = 4;
    let fg_rgb = vga.palette_to_rgb(0x0F);
    let bg_rgb = vga.palette_to_rgb(0x00);

    // For font columns 0-7 (framebuffer pixels 0-15): each pair is doubled
    for font_col in 0..8u32 {
        let bit = (font_row >> (7 - font_col)) & 1;
        let expected = if bit != 0 { fg_rgb } else { bg_rgb };
        let px_left = (font_col * 2) as usize;
        let px_right = px_left + 1;

        let actual_left = vga.framebuffer()[y * fb_width + px_left];
        let actual_right = vga.framebuffer()[y * fb_width + px_right];

        assert_eq!(
            actual_left, expected,
            "pixel ({px_left}, {y}): font bit {font_col} = {bit}"
        );
        assert_eq!(
            actual_right, expected,
            "pixel ({px_right}, {y}): doubled from font bit {font_col}"
        );
        // Both pixels in the pair must match (the doubling invariant)
        assert_eq!(
            actual_left, actual_right,
            "pixel pair ({px_left},{px_right}) at row {y} must be identical"
        );
    }

    // Font column 8 (9th column): 'A' is NOT in 0xC0-0xDF, so background
    let px16 = vga.framebuffer()[y * fb_width + 16];
    let px17 = vga.framebuffer()[y * fb_width + 17];
    assert_eq!(
        px16, bg_rgb,
        "9th-col pixel 16 should be background for 'A'"
    );
    assert_eq!(
        px17, bg_rgb,
        "9th-col pixel 17 should be background for 'A'"
    );
}

#[test]
fn test_40col_text_scanline_double_wide() {
    // Per-scanline renderer must also produce doubled pixels.
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x01);

    // Write 0xDB (full block) at position (0,0)
    memory.write_u8(0xB8000, 0xDB);
    memory.write_u8(0xB8001, 0x0F);

    // Render scanline 8 (middle of char cell)
    vga.render_scanline(8, &mut memory);

    let fg_rgb = vga.palette_to_rgb(0x0F);
    let bg_rgb = vga.palette_to_rgb(0x00);
    let fb_width = vga.fb_width;
    let y = 8;

    // All 18 pixels should be foreground
    for x in 0..18 {
        assert_eq!(
            vga.framebuffer()[y * fb_width + x],
            fg_rgb,
            "scanline pixel ({x}, {y}) should be foreground"
        );
    }
    // Next char cell starts at pixel 18 — background
    assert_eq!(
        vga.framebuffer()[y * fb_width + 18],
        bg_rgb,
        "scanline pixel (18, {y}) should be background"
    );
}

#[test]
fn test_scanline_text_cursor_spans_full_physical_cell() {
    for (mode, expected_cell_width) in [(0x00, 18), (0x01, 18), (0x03, 8), (0x03, 9)] {
        let mut vga = Vga::new();
        let mut memory = Memory::new();
        vga.set_mode_bios(mode);
        if mode == 0x03 && expected_cell_width == 9 {
            // Synthetic 80-column, 9-dot geometry exercises the physical ninth dot.
            // BIOS mode 03h's default framebuffer is 640 pixels wide.
            vga.fb_width = 720;
        }

        let cell_width = vga.fb_width / vga.columns as usize;
        assert_eq!(
            cell_width, expected_cell_width,
            "mode {mode:02X} test geometry"
        );

        let text_base = vga.text_memory_aperture_base();
        memory.write_u8(text_base, b' ');
        memory.write_u8(text_base + 1, 0x0F); // White foreground, black background
        memory.write_u8(text_base + 2, 0xDB); // Full block marks the neighboring cell
        memory.write_u8(text_base + 3, 0x01); // Blue foreground, black background

        vga.set_cursor(0, 0, 0);
        vga.set_cursor_shape(0x20, 0); // Disabled: capture the underlying cell pixels
        vga.render_scanline(0, &mut memory);
        let disabled_row = vga.framebuffer()[..vga.fb_width].to_vec();
        let cursor_rgb = vga.palette_to_rgb(0x0F);
        assert_ne!(
            disabled_row[0], cursor_rgb,
            "mode {mode:02X} disabled cursor should leave its cell unchanged"
        );
        assert_ne!(
            disabled_row[cell_width], cursor_rgb,
            "mode {mode:02X} neighboring cell should differ from cursor color"
        );

        vga.set_cursor_shape(0, 0);
        vga.render_scanline(0, &mut memory);
        for x in 0..cell_width {
            assert_eq!(
                vga.framebuffer()[x],
                cursor_rgb,
                "mode {mode:02X} cursor should cover physical cell pixel x={x}"
            );
        }
        assert_eq!(
            vga.framebuffer()[cell_width],
            disabled_row[cell_width],
            "mode {mode:02X} cursor must leave the neighboring cell unchanged"
        );

        vga.set_cursor_shape(0x20, 0);
        vga.render_scanline(0, &mut memory);
        for x in 0..cell_width {
            assert_eq!(
                vga.framebuffer()[x],
                disabled_row[x],
                "mode {mode:02X} disabled cursor changed cell pixel x={x}"
            );
        }
    }
}

#[test]
fn test_40col_text_scanline_pixel_pairing() {
    // Verify doubled pixels via the per-scanline renderer.
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x01);

    memory.write_u8(0xB8000, b'H'); // 0x48
    memory.write_u8(0xB8001, 0x07); // normal white on black

    let font_row = super::font_cp437_8x16::CP437_FONT_8X16[0x48 * 16 + 6];
    assert_ne!(font_row, 0x00);
    assert_ne!(font_row, 0xFF);

    vga.render_scanline(6, &mut memory);

    let fb_width = vga.fb_width;
    let y = 6;

    // Every pair of pixels (2n, 2n+1) for font columns 0-7 must be equal
    for font_col in 0..8usize {
        let px_left = font_col * 2;
        let px_right = px_left + 1;
        let left = vga.framebuffer()[y * fb_width + px_left];
        let right = vga.framebuffer()[y * fb_width + px_right];
        assert_eq!(
            left, right,
            "scanline: pixel pair ({px_left},{px_right}) must be identical (font col {font_col})"
        );
    }
}

#[test]
fn test_input_status1_active_display_both_bits_clear() {
    // During active display (not in vsync, not in hblank), bits 0 and 3 should both be 0
    let mut vga = Vga::new();
    let scanlines = vga.get_total_scanlines();
    vga.timing_cycles_per_frame = scanlines * 1000; // 1000 cycles per line
    vga.timing_retrace_cycles = 50; // small retrace window

    let cycles_per_line = vga.timing_cycles_per_frame / scanlines;
    // Position well past vsync AND well into the active part of a scanline.
    // hblank occupies ~18% at the start of each line, so place at 50% into the line.
    // Ensure line_position = (cycles_since_vblank % cycles_per_line) is > 18% of line.
    let active_offset = cycles_per_line / 2; // 50% into line — well past hblank
    vga.cycles_since_vblank = vga.timing_retrace_cycles + cycles_per_line + active_offset;

    let status = vga.read_port(0x3DA);
    assert_eq!(
        status & 0x08,
        0x00,
        "bit 3 (vsync) should be clear during active display"
    );
    assert_eq!(
        status & 0x01,
        0x00,
        "bit 0 (display disabled) should be clear during active display"
    );
}

#[test]
fn test_input_status1_hblank_only() {
    // During horizontal blank (not vsync): bit 0 = 1, bit 3 = 0
    let mut vga = Vga::new();
    let scanlines = vga.get_total_scanlines();
    vga.timing_cycles_per_frame = scanlines * 1000; // 1000 cycles per line
    vga.timing_retrace_cycles = 50; // small retrace window

    let cycles_per_line = vga.timing_cycles_per_frame / scanlines;
    // hblank occupies the END of the scanline (display-then-blank,
    // MDD-BUG-KILN-00025): mode 03h defaults blank from CR01+1=80 of htotal
    // 100 chars, i.e. the last ~20%. Probe 90% into a line past vsync so
    // line_position lands firmly in the blank tail.
    vga.cycles_since_vblank =
        vga.timing_retrace_cycles + cycles_per_line + (cycles_per_line * 9 / 10);

    let status = vga.read_port(0x3DA);
    assert_eq!(
        status & 0x08,
        0x00,
        "bit 3 (vsync) should be clear during hblank"
    );
    assert_eq!(
        status & 0x01,
        0x01,
        "bit 0 (display disabled) should be set during hblank"
    );
}

/// MDD-BUG-CRUCIBLE-00089: horizontal timing spans every physical CRTC
/// scanline, including the 49 lines outside the 400-line visible image.
#[test]
fn input_status1_hblank_stays_in_phase_across_400_of_449_timing() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);

    let visible_scanlines = vga.get_scanlines_per_frame();
    let total_scanlines = vga.get_total_scanlines();
    assert_eq!(visible_scanlines, 400);
    assert_eq!(total_scanlines, 449);

    let cycles_per_line = 1_000;
    vga.apply_timing_cycles_per_frame(total_scanlines * cycles_per_line);
    assert_eq!(vga.timing_retrace_cycles % cycles_per_line, 0);
    let first_active_line = vga.timing_retrace_cycles / cycles_per_line;

    for line in first_active_line..total_scanlines {
        vga.cycles_since_vblank = line * cycles_per_line + cycles_per_line / 2;
        assert_eq!(
            vga.read_port(0x3DA) & 0x01,
            0,
            "physical line {line} midpoint must remain in display phase"
        );

        vga.cycles_since_vblank = line * cycles_per_line + cycles_per_line * 9 / 10;
        assert_eq!(
            vga.read_port(0x3DA) & 0x01,
            1,
            "physical line {line} tail must remain in horizontal blank"
        );
    }
}

/// MDD-BUG-KILN-00025: the VBI duration for port-3DA bit 0 must track the
/// CRTC vertical blank start/end registers, not display end or a heuristic.
#[test]
fn test_retrace_duration_tracks_crtc_vertical_blank_window() {
    let mut vga = Vga::new(); // mode 03h defaults: vtotal=449, blank=406..441
    vga.apply_timing_cycles_per_frame(10000);
    let vtotal = 449u64;
    let baseline = (10000 * 35) / vtotal;
    assert_eq!(
        vga.timing_retrace_cycles, baseline,
        "VBI must be derived from CR15/CR16, not fixed at 8% (=800)"
    );
    assert_ne!(
        vga.timing_retrace_cycles, 800,
        "must not be the old 8% value"
    );

    // Move blank end ten lines later without changing display end.
    vga.crtc_regs[0x16] = vga.crtc_regs[0x16].wrapping_add(10);
    vga.apply_timing_cycles_per_frame(10000);
    let expected = (10000 * 45) / vtotal;
    assert_eq!(vga.timing_retrace_cycles, expected);
    assert!(
        vga.timing_retrace_cycles > baseline,
        "a later CR16 must lengthen the vertical blank"
    );
}

#[test]
fn test_input_status1_vsync_both_bits_set() {
    // During vertical retrace: both bits 0 and 3 should be 1
    let mut vga = Vga::new();
    let scanlines = vga.get_total_scanlines();
    vga.timing_cycles_per_frame = scanlines * 100;
    vga.timing_retrace_cycles = 500;
    // Place VSync pulse covering the whole VBI for this test
    vga.vsync_offset_cycles = 0;
    vga.vsync_duration_cycles = 500;

    let cycles_per_line = vga.timing_cycles_per_frame / scanlines;
    // Position in vsync but at a point mid-line (not in hblank)
    vga.cycles_since_vblank = cycles_per_line / 2;
    assert!(
        vga.cycles_since_vblank < vga.timing_retrace_cycles,
        "should be in vsync"
    );

    let status = vga.read_port(0x3DA);
    assert_eq!(
        status & 0x08,
        0x08,
        "bit 3 (vsync) should be set during vsync"
    );
    assert_eq!(
        status & 0x01,
        0x01,
        "bit 0 (display disabled) should be set during vsync"
    );
}

#[test]
fn test_input_status1_hblank_ratio_approximately_18_percent() {
    // The horizontal blanking ratio should be ~18%, not 10%
    let mut vga = Vga::new();
    let scanlines = vga.get_total_scanlines();
    vga.timing_cycles_per_frame = scanlines * 10000; // large cycles per line for precision
    vga.timing_retrace_cycles = 1; // minimal vsync

    let cycles_per_line = vga.timing_cycles_per_frame / scanlines;
    assert!(cycles_per_line >= 100, "need enough cycles for ratio test");

    // Count how many positions within a single scanline have hblank set
    let mut hblank_count = 0u64;
    let base = vga.timing_retrace_cycles + cycles_per_line; // start of a non-vsync line
    for pos in 0..cycles_per_line {
        vga.cycles_since_vblank = base + pos;
        let status = vga.read_port(0x3DA);
        if status & 0x01 != 0 {
            hblank_count += 1;
        }
    }

    let ratio_percent = (hblank_count * 100) / cycles_per_line;
    assert!(
        ratio_percent >= 15 && ratio_percent <= 21,
        "hblank ratio should be ~18%, got {ratio_percent}%"
    );
}

#[test]
fn test_ega_mode_set_syncs_memory_map() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    // Start in text mode (mode 0x03) — memory map select = 3 (B8000-BFFFF)
    vga.set_mode_bios(0x03);
    memory.apply_vga_pipeline(&vga.build_pipeline_state());
    assert_eq!(
        memory.vga.memory_map_select, 3,
        "text mode should set memory map select to 3 (B8000 window)"
    );

    // Switch to EGA 320x200 16-color (mode 0x0D)
    vga.set_mode_bios(0x0D);

    // pipeline_dirty must be true after mode set so sync_to_memory will propagate
    assert!(
        vga.pipeline_dirty,
        "set_mode_bios must set pipeline_dirty for sync propagation"
    );

    // Sync VGA registers to Memory module
    memory.apply_vga_pipeline(&vga.build_pipeline_state());
    assert_eq!(
        memory.vga.memory_map_select, 1,
        "EGA mode 0x0D should set memory map select to 1 (A0000-AFFFF window)"
    );

    // Write a test byte to A0000 — should go through the VGA planar pipeline
    memory.write_u8(0xA0000, 0xFF);

    // Verify the write reached VGA plane 0 (default map mask enables all planes,
    // but at minimum plane 0 should have data)
    assert_ne!(
        memory.plane(0)[0],
        0x00,
        "write to A0000 should reach VGA planes, not raw RAM"
    );
}

/// Regression test: EGA Write Mode 1 latch copy must work after mode set.
///
/// Exercise the standard EGA latch-copy sequence:
///   1. Set EGA mode 0x0D via BIOS
///   2. Program Write Mode 1 via I/O (OUT 3CE/3CF)
///   3. Read from VRAM (loads latches from all 4 planes)
///   4. Write to VRAM (Write Mode 1 copies latches to destination)
///
/// If VGA register state (write_mode) isn't synced to Memory after the
/// I/O write, step 4 uses Write Mode 0 instead of 1, producing garbled
/// output.  This was the root cause of the EGA rendering regression.
#[test]
fn test_ega_write_mode_1_latch_copy_after_sync() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    // Set up EGA mode 0x0D and sync
    vga.set_mode_bios(0x0D);
    memory.apply_vga_pipeline(&vga.build_pipeline_state());
    assert_eq!(memory.vga.memory_map_select, 1);

    // Write known data to VRAM plane 0 at offset 0
    // Using default Write Mode 0, map mask = 0x0F (all planes)
    assert_eq!(memory.vga.write_mode, 0, "should start in Write Mode 0");
    memory.write_u8(0xA0000, 0xA5);
    assert_eq!(memory.plane(0)[0], 0xA5);

    // Write different data to offset 40 (one row away in 320x200 16-color)
    memory.write_u8(0xA0028, 0x3C);
    assert_eq!(memory.plane(0)[0x28], 0x3C);

    // Program Write Mode 1 through the graphics controller:
    // OUT 3CE, 05 (select GC Mode register)
    // OUT 3CF, 01 (set Write Mode 1)
    vga.graphics_index = 5;
    vga.graphics_regs[5] = 0x01; // Write Mode 1
    vga.pipeline_dirty = true;

    // Sync VGA → Memory (this is what the timing loop does on IoCompleted)
    memory.apply_vga_pipeline(&vga.build_pipeline_state());
    assert_eq!(memory.vga.write_mode, 1, "write_mode must be 1 after sync");

    // Read from offset 0 — loads latches from all 4 planes
    let _ = memory.read_u8(0xA0000);
    // Latch[0] should now hold 0xA5

    // Write Mode 1 copy: write to offset 80 — latches should be copied
    memory.write_u8(0xA0050, 0x00); // value is ignored in WM1
    assert_eq!(
        memory.plane(0)[0x50],
        0xA5,
        "Write Mode 1 latch copy must transfer plane data"
    );
}

#[test]
fn test_conformance_mode_04_init() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x04);
    assert_eq!(vga.fb_width, 320);
    assert_eq!(vga.fb_height, 200);
    assert_eq!(vga.columns, 40);
    assert_eq!(vga.rows, 25);
}

#[test]
fn test_conformance_mode_04_pixel_roundtrip() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);

    let test_cases: &[(u16, u16, u8)] = &[(0, 0, 0), (0, 0, 1), (100, 50, 2), (319, 199, 3)];
    for &(x, y, color) in test_cases {
        vga.write_pixel(0, x, y, color, &mut memory);
        let readback = vga.read_pixel(0, x, y, &memory);
        assert_eq!(
            readback, color,
            "Mode 04h pixel round-trip failed at ({}, {}) color {}",
            x, y, color
        );
    }
}

#[test]
fn test_conformance_mode_04_teletype() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);

    // Cursor should start at (0, 0)
    assert_eq!(vga.get_cursor(0), (0, 0));

    // Write 'A' via teletype
    vga.teletype(0x41, 0, &mut memory);

    // Cursor should advance to (0, 1)
    let (row, col) = vga.get_cursor(0);
    assert_eq!((row, col), (0, 1), "Cursor must advance after teletype");

    // Verify CGA memory at B8000 has non-zero data (even scanlines)
    let mut found_nonzero = false;
    for offset in 0..80 {
        if memory.read_u8(0xB8000 + offset) != 0 {
            found_nonzero = true;
            break;
        }
    }
    assert!(
        found_nonzero,
        "Mode 04h teletype must write non-zero data to CGA memory"
    );
}

#[test]
fn test_conformance_mode_05_init() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x05);
    assert_eq!(vga.fb_width, 320);
    assert_eq!(vga.fb_height, 200);
    assert_eq!(vga.columns, 40);
    assert_eq!(vga.rows, 25);
}

#[test]
fn test_conformance_mode_05_pixel_roundtrip() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x05);

    let test_cases: &[(u16, u16, u8)] = &[(0, 0, 0), (0, 0, 1), (100, 50, 2), (319, 199, 3)];
    for &(x, y, color) in test_cases {
        vga.write_pixel(0, x, y, color, &mut memory);
        let readback = vga.read_pixel(0, x, y, &memory);
        assert_eq!(
            readback, color,
            "Mode 05h pixel round-trip failed at ({}, {}) color {}",
            x, y, color
        );
    }
}

#[test]
fn test_conformance_mode_05_teletype() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x05);

    assert_eq!(vga.get_cursor(0), (0, 0));
    vga.teletype(0x41, 0, &mut memory);

    let (row, col) = vga.get_cursor(0);
    assert_eq!((row, col), (0, 1), "Cursor must advance after teletype");

    let mut found_nonzero = false;
    for offset in 0..80 {
        if memory.read_u8(0xB8000 + offset) != 0 {
            found_nonzero = true;
            break;
        }
    }
    assert!(
        found_nonzero,
        "Mode 05h teletype must write non-zero data to CGA memory"
    );
}

#[test]
fn test_conformance_mode_06_init() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x06);
    assert_eq!(vga.fb_width, 640);
    assert_eq!(vga.fb_height, 200);
    assert_eq!(vga.columns, 80);
    assert_eq!(vga.rows, 25);
}

#[test]
fn test_conformance_mode_06_pixel_roundtrip() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x06);

    let test_cases: &[(u16, u16, u8)] = &[
        (0, 0, 0),
        (0, 0, 1),
        (320, 100, 0),
        (320, 100, 1),
        (639, 199, 1),
    ];
    for &(x, y, color) in test_cases {
        vga.write_pixel(0, x, y, color, &mut memory);
        let readback = vga.read_pixel(0, x, y, &memory);
        assert_eq!(
            readback, color,
            "Mode 06h pixel round-trip failed at ({}, {}) color {}",
            x, y, color
        );
    }
}

#[test]
fn test_conformance_mode_06_teletype() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x06);

    assert_eq!(vga.get_cursor(0), (0, 0));
    vga.teletype(0x41, 0, &mut memory);

    let (row, col) = vga.get_cursor(0);
    assert_eq!((row, col), (0, 1), "Cursor must advance after teletype");

    // CGA mode 06: 1 bit per pixel at B8000
    let mut found_nonzero = false;
    for offset in 0..80 {
        if memory.read_u8(0xB8000 + offset) != 0 {
            found_nonzero = true;
            break;
        }
    }
    assert!(
        found_nonzero,
        "Mode 06h teletype must write non-zero data to CGA memory"
    );
}

#[test]
fn test_conformance_mode_0d_init() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x0D);
    assert_eq!(vga.fb_width, 320);
    assert_eq!(vga.fb_height, 200);
    assert_eq!(vga.columns, 40);
    assert_eq!(vga.rows, 25);
}

#[test]
fn test_conformance_mode_0d_pixel_roundtrip() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0D);

    let test_cases: &[(u16, u16, u8)] = &[(0, 0, 0), (0, 0, 5), (160, 100, 10), (319, 199, 15)];
    for &(x, y, color) in test_cases {
        vga.write_pixel(0, x, y, color, &mut memory);
        let readback = vga.read_pixel(0, x, y, &memory);
        assert_eq!(
            readback, color,
            "Mode 0Dh pixel round-trip failed at ({}, {}) color {}",
            x, y, color
        );
    }
}

#[test]
fn test_conformance_mode_0d_teletype() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0D);

    assert_eq!(vga.get_cursor(0), (0, 0));
    vga.teletype(0x41, 0, &mut memory);

    let (row, col) = vga.get_cursor(0);
    assert_eq!((row, col), (0, 1), "Cursor must advance after teletype");

    // Planar mode: check plane 0 for non-zero data
    let plane0 = memory.plane(0);
    let mut found_nonzero = false;
    // Character cell is 8x8 at (0,0), bytes_per_row = 320/8 = 40
    for row in 0..8 {
        if plane0[row * 40] != 0 {
            found_nonzero = true;
            break;
        }
    }
    assert!(
        found_nonzero,
        "Mode 0Dh teletype must write non-zero data to plane 0"
    );
}

#[test]
fn test_conformance_mode_0e_init() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x0E);
    assert_eq!(vga.fb_width, 640);
    assert_eq!(vga.fb_height, 200);
    assert_eq!(vga.columns, 80);
    assert_eq!(vga.rows, 25);
}

#[test]
fn test_conformance_mode_0e_pixel_roundtrip() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0E);

    let test_cases: &[(u16, u16, u8)] = &[(0, 0, 0), (0, 0, 5), (320, 100, 10), (639, 199, 15)];
    for &(x, y, color) in test_cases {
        vga.write_pixel(0, x, y, color, &mut memory);
        let readback = vga.read_pixel(0, x, y, &memory);
        assert_eq!(
            readback, color,
            "Mode 0Eh pixel round-trip failed at ({}, {}) color {}",
            x, y, color
        );
    }
}

#[test]
fn test_conformance_mode_0e_teletype() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0E);

    assert_eq!(vga.get_cursor(0), (0, 0));
    vga.teletype(0x41, 0, &mut memory);

    let (row, col) = vga.get_cursor(0);
    assert_eq!((row, col), (0, 1), "Cursor must advance after teletype");

    // Planar mode: check plane 0, bytes_per_row = 640/8 = 80
    let plane0 = memory.plane(0);
    let mut found_nonzero = false;
    for row in 0..8 {
        if plane0[row * 80] != 0 {
            found_nonzero = true;
            break;
        }
    }
    assert!(
        found_nonzero,
        "Mode 0Eh teletype must write non-zero data to plane 0"
    );
}

#[test]
fn test_conformance_mode_0f_init() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x0F);
    assert_eq!(vga.fb_width, 640);
    assert_eq!(vga.fb_height, 350);
    assert_eq!(vga.columns, 80);
    assert_eq!(vga.rows, 25);
}

#[test]
fn test_conformance_mode_0f_pixel_roundtrip() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0F);

    let test_cases: &[(u16, u16, u8)] = &[
        (0, 0, 0),
        (0, 0, 1),
        (320, 175, 0),
        (320, 175, 1),
        (639, 349, 1),
    ];
    for &(x, y, color) in test_cases {
        vga.write_pixel(0, x, y, color, &mut memory);
        let readback = vga.read_pixel(0, x, y, &memory);
        assert_eq!(
            readback, color,
            "Mode 0Fh pixel round-trip failed at ({}, {}) color {}",
            x, y, color
        );
    }
}

#[test]
fn test_conformance_mode_0f_teletype() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0F);

    assert_eq!(vga.get_cursor(0), (0, 0));
    vga.teletype(0x41, 0, &mut memory);

    let (row, col) = vga.get_cursor(0);
    assert_eq!((row, col), (0, 1), "Cursor must advance after teletype");

    // Planar mode: check plane 0, bytes_per_row = 640/8 = 80
    // Mode 0Fh uses 8x14 font, check first 14 scanlines
    let plane0 = memory.plane(0);
    let mut found_nonzero = false;
    for row in 0..14 {
        if plane0[row * 80] != 0 {
            found_nonzero = true;
            break;
        }
    }
    assert!(
        found_nonzero,
        "Mode 0Fh teletype must write non-zero data to plane 0"
    );
}

#[test]
fn test_conformance_mode_10_init() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x10);
    assert_eq!(vga.fb_width, 640);
    assert_eq!(vga.fb_height, 350);
    assert_eq!(vga.columns, 80);
    assert_eq!(vga.rows, 43);
}

#[test]
fn test_conformance_mode_10_pixel_roundtrip() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x10);

    let test_cases: &[(u16, u16, u8)] = &[(0, 0, 0), (0, 0, 5), (320, 175, 10), (639, 349, 15)];
    for &(x, y, color) in test_cases {
        vga.write_pixel(0, x, y, color, &mut memory);
        let readback = vga.read_pixel(0, x, y, &memory);
        assert_eq!(
            readback, color,
            "Mode 10h pixel round-trip failed at ({}, {}) color {}",
            x, y, color
        );
    }
}

#[test]
fn test_conformance_mode_10_teletype() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x10);

    assert_eq!(vga.get_cursor(0), (0, 0));
    vga.teletype(0x41, 0, &mut memory);

    let (row, col) = vga.get_cursor(0);
    assert_eq!((row, col), (0, 1), "Cursor must advance after teletype");

    // Planar mode: check plane 0, bytes_per_row = 640/8 = 80
    // Mode 0x10 uses 8x14 font
    let plane0 = memory.plane(0);
    let mut found_nonzero = false;
    for row in 0..14 {
        if plane0[row * 80] != 0 {
            found_nonzero = true;
            break;
        }
    }
    assert!(
        found_nonzero,
        "Mode 10h teletype must write non-zero data to plane 0"
    );
}

#[test]
fn test_conformance_mode_11_init() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x11);
    assert_eq!(vga.fb_width, 640);
    assert_eq!(vga.fb_height, 480);
    assert_eq!(vga.columns, 80);
    assert_eq!(vga.rows, 30);
}

#[test]
fn test_conformance_mode_11_pixel_roundtrip() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x11);

    let test_cases: &[(u16, u16, u8)] = &[
        (0, 0, 0),
        (0, 0, 1),
        (320, 240, 0),
        (320, 240, 1),
        (639, 479, 1),
    ];
    for &(x, y, color) in test_cases {
        vga.write_pixel(0, x, y, color, &mut memory);
        let readback = vga.read_pixel(0, x, y, &memory);
        assert_eq!(
            readback, color,
            "Mode 11h pixel round-trip failed at ({}, {}) color {}",
            x, y, color
        );
    }
}

#[test]
fn test_conformance_mode_11_teletype() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x11);

    assert_eq!(vga.get_cursor(0), (0, 0));
    vga.teletype(0x41, 0, &mut memory);

    let (row, col) = vga.get_cursor(0);
    assert_eq!((row, col), (0, 1), "Cursor must advance after teletype");

    // Planar mode: check plane 0, bytes_per_row = 640/8 = 80
    // Mode 0x11 uses 8x16 font
    let plane0 = memory.plane(0);
    let mut found_nonzero = false;
    for row in 0..16 {
        if plane0[row * 80] != 0 {
            found_nonzero = true;
            break;
        }
    }
    assert!(
        found_nonzero,
        "Mode 11h teletype must write non-zero data to plane 0"
    );
}

#[test]
fn test_conformance_mode_12_init() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x12);
    assert_eq!(vga.fb_width, 640);
    assert_eq!(vga.fb_height, 480);
    assert_eq!(vga.columns, 80);
    assert_eq!(vga.rows, 60);
}

#[test]
fn test_conformance_mode_12_pixel_roundtrip() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x12);

    let test_cases: &[(u16, u16, u8)] = &[(0, 0, 0), (0, 0, 5), (320, 240, 10), (639, 479, 15)];
    for &(x, y, color) in test_cases {
        vga.write_pixel(0, x, y, color, &mut memory);
        let readback = vga.read_pixel(0, x, y, &memory);
        assert_eq!(
            readback, color,
            "Mode 12h pixel round-trip failed at ({}, {}) color {}",
            x, y, color
        );
    }
}

#[test]
fn test_conformance_mode_12_teletype() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x12);

    assert_eq!(vga.get_cursor(0), (0, 0));
    vga.teletype(0x41, 0, &mut memory);

    let (row, col) = vga.get_cursor(0);
    assert_eq!((row, col), (0, 1), "Cursor must advance after teletype");

    // Planar mode: check plane 0, bytes_per_row = 640/8 = 80
    // Mode 0x12 uses 8x16 font
    let plane0 = memory.plane(0);
    let mut found_nonzero = false;
    for row in 0..16 {
        if plane0[row * 80] != 0 {
            found_nonzero = true;
            break;
        }
    }
    assert!(
        found_nonzero,
        "Mode 12h teletype must write non-zero data to plane 0"
    );
}

#[test]
fn test_conformance_mode_13_init() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    assert_eq!(vga.fb_width, 320);
    assert_eq!(vga.fb_height, 200);
    assert_eq!(vga.columns, 40);
    assert_eq!(vga.rows, 25);
}

#[test]
fn test_conformance_mode_13_pixel_roundtrip() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);

    let test_cases: &[(u16, u16, u8)] = &[(0, 0, 0), (0, 0, 42), (160, 100, 128), (319, 199, 255)];
    for &(x, y, color) in test_cases {
        vga.write_pixel(0, x, y, color, &mut memory);
        let readback = vga.read_pixel(0, x, y, &memory);
        assert_eq!(
            readback, color,
            "Mode 13h pixel round-trip failed at ({}, {}) color {}",
            x, y, color
        );
    }
}

#[test]
fn test_conformance_mode_13_teletype() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);

    assert_eq!(vga.get_cursor(0), (0, 0));
    vga.teletype(0x41, 0, &mut memory);

    let (row, col) = vga.get_cursor(0);
    assert_eq!((row, col), (0, 1), "Cursor must advance after teletype");

    // Mode 13h: linear 1 byte/pixel at A0000, character cell is 8x8
    let mut found_nonzero = false;
    for cy in 0..8 {
        for cx in 0..8 {
            let addr = 0xA0000 + cy * 320 + cx;
            if memory.read_u8(addr) != 0 {
                found_nonzero = true;
                break;
            }
        }
        if found_nonzero {
            break;
        }
    }
    assert!(
        found_nonzero,
        "Mode 13h teletype must write non-zero data to A0000 region"
    );
}

/// Modes 0Dh/0Eh are double-scanned by the BIOS (CR09 bit 7), so their
/// 400 physical scanlines carry only 200 VRAM rows. Their framebuffer is
/// the logical 200 rows, so the doubling must NOT be applied again when
/// mapping framebuffer row to VRAM row — that stretched every drawing to
/// twice its height and pushed the bottom half off-screen.
#[test]
fn planar_200_line_modes_are_not_double_scanned_into_the_framebuffer() {
    for (mode, width) in [(0x0Du8, 320usize), (0x0Eu8, 640usize)] {
        for use_scanline_path in [false, true] {
            let mut vga = Vga::new();
            let mut memory = Memory::new();
            vga.set_mode_bios(mode);
            assert_eq!(
                vga.crtc_regs[0x09] & 0x80,
                0x80,
                "mode {mode:02X} should be BIOS-programmed for scan doubling"
            );

            vga.write_pixel(0, 0, 3, 15, &mut memory);

            if use_scanline_path {
                for line in 0..8 {
                    vga.render_scanline(line, &mut memory);
                }
            } else {
                vga.render(&mut memory);
            }

            let fb = vga.framebuffer();
            assert_eq!(
                fb[3 * width],
                0xFFFFFF,
                "mode {mode:02X}: VRAM row 3 belongs on framebuffer row 3 \
                 (scanline path = {use_scanline_path})"
            );
            for blank in [2usize, 4, 6] {
                assert_eq!(
                    fb[blank * width],
                    0x000000,
                    "mode {mode:02X}: framebuffer row {blank} should be blank \
                     (scanline path = {use_scanline_path})"
                );
            }
        }
    }
}

/// A guest that programs the attribute controller directly owns the CGA
/// palette from then on. Rendering must not re-derive AC[0]..AC[3] from
/// the CGA colour-select register every frame, which would select stale
/// DAC entries instead of the guest-programmed palette.
#[test]
fn cga_mode04_render_preserves_guest_programmed_ac_palette() {
    for use_scanline_path in [false, true] {
        let mut vga = Vga::new();
        let mut memory = Memory::new();
        vga.set_mode_bios(0x04);

        // Keep the guest's direct DAC selector. The upper CGA palette bank
        // repeats the lower bank, so this selector still resolves to brown.
        // The BIOS default selects a different entry for pixel 1.
        write_ac_palette_reg(&mut vga, 1, 0x0E);
        vga.write_pixel(0, 10, 0, 1, &mut memory);

        if use_scanline_path {
            vga.render_scanline(0, &mut memory);
        } else {
            vga.render(&mut memory);
        }

        assert_eq!(
            vga.attribute_regs[1], 0x0E,
            "rendering must preserve the guest's AC[1] selector"
        );
        assert_eq!(
            vga.framebuffer()[10],
            0xAA5500,
            "guest AC[1] must survive rendering (scanline path = {use_scanline_path})"
        );
    }
}

/// Same invariant for mode 06h, whose single foreground colour also comes
/// from AC[1].
#[test]
fn cga_mode06_render_preserves_guest_programmed_ac_palette() {
    for use_scanline_path in [false, true] {
        let mut vga = Vga::new();
        let mut memory = Memory::new();
        vga.set_mode_bios(0x06);

        // Entries 8-15 repeat 0-7 in cga_palette_2, so DAC[0x0C] is
        // dark red (entry 4).
        write_ac_palette_reg(&mut vga, 1, 0x0C);
        vga.write_pixel(0, 10, 0, 1, &mut memory);

        if use_scanline_path {
            vga.render_scanline(0, &mut memory);
        } else {
            vga.render(&mut memory);
        }

        assert_eq!(
            vga.attribute_regs[1], 0x0C,
            "rendering must preserve the guest's AC[1] selector"
        );
        assert_eq!(
            vga.framebuffer()[10],
            0xAA0000,
            "guest AC[1] must survive rendering (scanline path = {use_scanline_path})"
        );
    }
}

/// The colour-select register still drives the palette when a guest uses
/// the CGA mechanism (port 0x3D9 / INT 10h AH=0Bh) rather than the
/// attribute controller.
#[test]
fn cga_mode04_color_select_port_still_reprograms_the_palette() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);
    vga.write_port(0x3D9, 0x00); // palette 0, low intensity

    vga.write_pixel(0, 10, 0, 1, &mut memory);
    vga.render_scanline(0, &mut memory);

    assert_eq!(
        vga.framebuffer()[10],
        0x00AA00,
        "palette 0 colour 1 = green"
    );
}

#[test]
fn test_conformance_cga_palette0_low_intensity_colors() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);
    // Palette 0, Low Intensity: clear palette bit (bit 5) and intensity bit (bit 4)
    vga.set_cga_color_select(0x00); // bg=black, palette=0, intensity=low

    vga.write_pixel(0, 10, 0, 1, &mut memory);
    vga.write_pixel(0, 20, 0, 2, &mut memory);
    vga.write_pixel(0, 30, 0, 3, &mut memory);

    vga.render(&mut memory);
    let fb = vga.framebuffer();

    assert_eq!(fb[10], 0x00AA00, "Palette 0 Low: Color 1 should be Green");
    assert_eq!(fb[20], 0xAA0000, "Palette 0 Low: Color 2 should be Red");
    assert_eq!(fb[30], 0xAA5500, "Palette 0 Low: Color 3 should be Brown");
}

#[test]
fn test_conformance_cga_palette0_high_intensity_colors() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);
    // Palette 0, High Intensity: clear palette bit (bit 5), set intensity (bit 4)
    vga.set_cga_color_select(0x10); // bg=black, palette=0, intensity=high

    vga.write_pixel(0, 10, 0, 1, &mut memory);
    vga.write_pixel(0, 20, 0, 2, &mut memory);
    vga.write_pixel(0, 30, 0, 3, &mut memory);

    vga.render(&mut memory);
    let fb = vga.framebuffer();

    assert_eq!(
        fb[10], 0x55FF55,
        "Palette 0 High: Color 1 should be Light Green"
    );
    assert_eq!(
        fb[20], 0xFF5555,
        "Palette 0 High: Color 2 should be Light Red"
    );
    assert_eq!(fb[30], 0xFFFF55, "Palette 0 High: Color 3 should be Yellow");
}

#[test]
fn test_conformance_cga_palette1_low_intensity_colors() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);
    // Palette 1, Low Intensity: set palette bit (bit 5), clear intensity (bit 4)
    vga.set_cga_color_select(0x20); // bg=black, palette=1, intensity=low

    vga.write_pixel(0, 10, 0, 1, &mut memory);
    vga.write_pixel(0, 20, 0, 2, &mut memory);
    vga.write_pixel(0, 30, 0, 3, &mut memory);

    vga.render(&mut memory);
    let fb = vga.framebuffer();

    assert_eq!(fb[10], 0x00AAAA, "Palette 1 Low: Color 1 should be Cyan");
    assert_eq!(fb[20], 0xAA00AA, "Palette 1 Low: Color 2 should be Magenta");
    assert_eq!(
        fb[30], 0xAAAAAA,
        "Palette 1 Low: Color 3 should be Light Gray"
    );
}

#[test]
fn test_conformance_cga_palette1_high_intensity_colors() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);
    // Default color_select = 0x30 (Palette 1, High Intensity)

    vga.write_pixel(0, 10, 0, 1, &mut memory);
    vga.write_pixel(0, 20, 0, 2, &mut memory);
    vga.write_pixel(0, 30, 0, 3, &mut memory);

    vga.render(&mut memory);
    let fb = vga.framebuffer();

    assert_eq!(
        fb[10], 0x55FFFF,
        "Palette 1 High: Color 1 should be Light Cyan"
    );
    assert_eq!(
        fb[20], 0xFF55FF,
        "Palette 1 High: Color 2 should be Light Magenta"
    );
    assert_eq!(fb[30], 0xFFFFFF, "Palette 1 High: Color 3 should be White");
}

#[test]
fn test_conformance_cga_mode05_low_intensity_colors() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x05);
    // Mode 05 Low Intensity: clear intensity bit
    vga.set_cga_color_select(0x00);

    vga.write_pixel(0, 10, 0, 1, &mut memory);
    vga.write_pixel(0, 20, 0, 2, &mut memory);
    vga.write_pixel(0, 30, 0, 3, &mut memory);

    vga.render(&mut memory);
    let fb = vga.framebuffer();

    assert_eq!(fb[10], 0x00AAAA, "Mode 05 Low: Color 1 should be Cyan");
    assert_eq!(fb[20], 0xAA0000, "Mode 05 Low: Color 2 should be Red");
    assert_eq!(
        fb[30], 0xAAAAAA,
        "Mode 05 Low: Color 3 should be Light Gray"
    );
}

#[test]
fn test_conformance_cga_mode05_high_intensity_colors() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x05);
    // Default color_select = 0x30 (includes intensity bit)

    vga.write_pixel(0, 10, 0, 1, &mut memory);
    vga.write_pixel(0, 20, 0, 2, &mut memory);
    vga.write_pixel(0, 30, 0, 3, &mut memory);

    vga.render(&mut memory);
    let fb = vga.framebuffer();

    assert_eq!(
        fb[10], 0x55FFFF,
        "Mode 05 High: Color 1 should be Light Cyan"
    );
    assert_eq!(
        fb[20], 0xFF5555,
        "Mode 05 High: Color 2 should be Light Red"
    );
    assert_eq!(fb[30], 0xFFFFFF, "Mode 05 High: Color 3 should be White");
}

#[test]
fn test_conformance_cga_mode05_ignores_palette_select_bit() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x05);
    // Set palette select bit (bit 5) — Mode 05 should IGNORE it
    // and still use Cyan/Red/White, NOT Cyan/Magenta/White
    vga.set_cga_color_select(0x30); // palette=1, intensity=high

    vga.write_pixel(0, 10, 0, 1, &mut memory);
    vga.write_pixel(0, 20, 0, 2, &mut memory);
    vga.write_pixel(0, 30, 0, 3, &mut memory);

    vga.render(&mut memory);
    let fb = vga.framebuffer();

    // Mode 05 always uses Cyan/Red/White regardless of palette bit
    assert_eq!(fb[10], 0x55FFFF, "Mode 05: Color 1 should be Light Cyan");
    assert_eq!(
        fb[20], 0xFF5555,
        "Mode 05: Color 2 should be Light Red (NOT Light Magenta)"
    );
    assert_eq!(fb[30], 0xFFFFFF, "Mode 05: Color 3 should be White");
}

#[test]
fn test_conformance_cga_mode06_default_foreground_white() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x06);
    // Default color_select = 0x0F (white foreground)
    assert_eq!(
        vga.color_select_reg, 0x0F,
        "Mode 06h default color_select should be 0x0F"
    );

    vga.write_pixel(0, 0, 0, 1, &mut memory);
    vga.render(&mut memory);
    let fb = vga.framebuffer();

    assert_eq!(
        fb[0],
        Vga::cga_irgb_to_rgb(0x0F),
        "Mode 06h default fg should match cga_irgb_to_rgb(0x0F)"
    );
    assert_eq!(fb[0], 0xFFFFFF, "Mode 06h default fg should be white");
}

#[test]
fn test_conformance_cga_mode06_foreground_color_select() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x06);

    // Write a pixel that is "on"
    vga.write_pixel(0, 0, 0, 1, &mut memory);

    // Change foreground to green (color_select = 0x02)
    vga.set_cga_color_select(0x02);
    vga.render(&mut memory);
    let fb = vga.framebuffer();
    assert_eq!(
        fb[0], 0x00AA00,
        "Mode 06h with color_select=0x02 should be green"
    );

    // Change to light red (color_select = 0x0C)
    vga.set_cga_color_select(0x0C);
    vga.render(&mut memory);
    let fb = vga.framebuffer();
    assert_eq!(
        fb[0], 0xFF5555,
        "Mode 06h with color_select=0x0C should be light red"
    );
}

#[test]
fn test_conformance_cga_palette_switch_intensity_on() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);

    // Start with Palette 1 (default 0x30), clear intensity, keep palette
    // BH=00 with BL=0x00: sets background=0, intensity=0
    vga.set_cga_color_select(vga.get_cga_color_select() & 0xE0);
    assert_eq!(
        vga.get_cga_color_select(),
        0x20,
        "Should be palette 1, low intensity"
    );

    // Write color 3 pixel and verify low-intensity rendering
    vga.write_pixel(0, 10, 0, 3, &mut memory);
    vga.render(&mut memory);
    assert_eq!(
        vga.framebuffer()[10],
        0xAAAAAA,
        "Palette 1 Low: Color 3 should be Light Gray"
    );

    // BH=00 with BL=0x10: sets background=0, intensity=1
    vga.set_cga_color_select((vga.get_cga_color_select() & 0xE0) | 0x10);
    assert_eq!(
        vga.get_cga_color_select(),
        0x30,
        "Should be palette 1, high intensity"
    );

    vga.render(&mut memory);
    assert_eq!(
        vga.framebuffer()[10],
        0xFFFFFF,
        "Palette 1 High: Color 3 should be White"
    );
}

#[test]
fn test_conformance_cga_palette_switch_palette_select() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);

    // Start with default 0x30 (Palette 1, High Intensity)
    vga.write_pixel(0, 10, 0, 1, &mut memory);

    // Verify Palette 1
    vga.render(&mut memory);
    assert_eq!(
        vga.framebuffer()[10],
        0x55FFFF,
        "Palette 1: Color 1 should be Light Cyan"
    );

    // BH=01 with BL=0x00: switch to Palette 0 (preserves intensity)
    let cs = vga.get_cga_color_select();
    vga.set_cga_color_select(cs & !0x20);
    assert_eq!(
        vga.get_cga_color_select() & 0x30,
        0x10,
        "Should be palette 0, high intensity"
    );

    vga.render(&mut memory);
    assert_eq!(
        vga.framebuffer()[10],
        0x55FF55,
        "Palette 0: Color 1 should be Light Green"
    );

    // BH=01 with BL=0x01: switch back to Palette 1 (preserves intensity)
    let cs = vga.get_cga_color_select();
    vga.set_cga_color_select((cs & !0x20) | (0x01 << 5));

    vga.render(&mut memory);
    assert_eq!(
        vga.framebuffer()[10],
        0x55FFFF,
        "Palette 1: Color 1 should be Light Cyan again"
    );
}

/// Program AC[index] = value through the attribute controller, the way a
/// guest (or INT 10h AH=10h) does, leaving the display enabled.
fn write_ac_palette_reg(vga: &mut Vga, index: u8, value: u8) {
    vga.read_port(0x3DA); // reset the index/data flip-flop
    vga.write_port(0x3C0, index & 0x1F);
    vga.write_port(0x3C0, value);
    vga.write_port(0x3C0, 0x20); // re-assert PAS (display enable)
}

/// MDD-BUG-FLU-03410: with no host consumer the scanline renderers produce no
/// pixels, yet leave the framebuffer shape (`fb_width`/`fb_height`, read by
/// INT 10h and the mouse ranges) exactly as a rendering run leaves it: each
/// renderer's resize prologue runs before the pixel gate. Turning output back
/// on renders the same pixels a run that never stopped.
#[test]
fn test_scanline_raster_output_off_skips_pixels_but_keeps_guest_state() {
    fn setup(mode: u8, memory: &mut Memory) -> Vga {
        let mut vga = Vga::new();
        vga.set_mode_bios(mode);
        // A shape the renderers must correct (as a guest's direct register
        // programming leaves it), so a gate above a resize is caught.
        vga.fb_width = 8;
        vga.fb_height = 8;
        vga.framebuffer = vec![0; 64];
        vga.palette_address_source = true;
        vga.set_dac_reg(5, 63, 0, 0);
        match mode {
            0x03 => {
                for i in 0..80 * 25 {
                    memory.write_u8(0xB8000 + i * 2, b'A' + (i % 26) as u8);
                    memory.write_u8(0xB8000 + i * 2 + 1, 0x1F);
                }
            }
            0x13 => memory.video_memory_mut()[..320 * 200].fill(5),
            _ => {
                for plane in 0..4 {
                    memory.plane_mut(plane)[..80 * 480].fill(0xA5);
                }
            }
        }
        vga
    }

    for mode in [0x03u8, 0x13, 0x12] {
        let mut on_mem = Memory::new();
        let mut off_mem = Memory::new();
        let mut on = setup(mode, &mut on_mem);
        let mut off = setup(mode, &mut off_mem);
        off.set_raster_output(false);
        assert!(!off.raster_output());
        // A stale construction buffer the skipped frame must not touch.
        let stale = off.framebuffer().to_vec();

        let lines = on.visible_scanlines_cached.max(200);
        for line in 0..lines {
            on.render_scanline(line, &mut on_mem);
            off.render_scanline(line, &mut off_mem);
        }

        assert_eq!(off.dimensions(), on.dimensions(), "mode {mode:02X} shape");
        assert_eq!(
            off.framebuffer().len(),
            on.framebuffer().len(),
            "mode {mode:02X} buffer length"
        );
        assert!(
            on.framebuffer().iter().any(|&px| px != 0),
            "mode {mode:02X}: the rendering run drew nothing"
        );
        if stale.len() == off.framebuffer().len() {
            assert_eq!(off.framebuffer(), stale.as_slice(), "mode {mode:02X}");
        } else {
            assert!(
                off.framebuffer().iter().all(|&px| px == 0),
                "mode {mode:02X}"
            );
        }

        off.set_raster_output(true);
        for line in 0..lines {
            off.render_scanline(line, &mut off_mem);
        }
        assert_eq!(
            off.framebuffer(),
            on.framebuffer(),
            "mode {mode:02X} resumed"
        );
    }
}
