use super::*;

#[test]
fn test_teletype_scroll_graphics_mode() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x13);
    let last_row = vga.rows - 1;
    vga.set_cursor(0, last_row, 0);
    vga.teletype(0x0A, 0, &mut memory);
    assert_eq!(vga.get_cursor(0).0, last_row);
}

#[test]
fn test_write_pixel_cga_mode04() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);
    vga.write_pixel(0, 50, 100, 3, &mut memory);
}

#[test]
fn test_write_pixel_cga_mode06() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x06);
    vga.write_pixel(0, 100, 50, 1, &mut memory);
}

#[test]
fn test_write_pixel_ega_mode0d() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0D);
    vga.write_pixel(0, 100, 50, 0x0F, &mut memory);
}

#[test]
fn test_write_pixel_ega_mode0e() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0E);
    vga.write_pixel(0, 300, 100, 0x0A, &mut memory);
}

#[test]
fn test_write_pixel_ega_mode10() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x10);
    vga.write_pixel(0, 500, 200, 0x05, &mut memory);
}

#[test]
fn test_write_pixel_vga_mode12() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x12);
    vga.write_pixel(0, 400, 300, 0x0C, &mut memory);
}

#[test]
fn test_write_pixel_out_of_bounds() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);
    vga.write_pixel(0, 500, 500, 15, &mut memory); // Out of bounds
}

#[test]
fn test_write_pixel_y_out_of_bounds() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x13);
    vga.write_pixel(0, 10, 200, 0x22, &mut memory);
    assert_eq!(memory.read_u8(0xA0000 + 10), 0);

    vga.set_mode_bios(0x04);
    vga.write_pixel(0, 10, 200, 0x03, &mut memory);
    assert_eq!(memory.read_u8(0xB8000 + 2), 0);
    vga.write_pixel(0, 320, 10, 0x03, &mut memory);
    assert_eq!(memory.read_u8(0xB8000), 0);

    vga.set_mode_bios(0x06);
    vga.write_pixel(0, 10, 200, 1, &mut memory);
    assert_eq!(memory.read_u8(0xB8000 + 1), 0);
    vga.write_pixel(0, 640, 10, 1, &mut memory);
    assert_eq!(memory.read_u8(0xB8000), 0);

    vga.set_mode_bios(0x0D);
    vga.write_pixel(0, 10, 200, 0x0F, &mut memory);
    assert_eq!(memory.plane(0)[0], 0);
}

#[test]
fn test_put_pixel_modex_out_of_bounds() {
    let mut memory = Memory::new();
    Vga::put_pixel_modex(&mut memory, 4, 0, 4, 4, 1);
    Vga::put_pixel_modex(&mut memory, 0, 4, 4, 4, 1);
    assert_eq!(memory.read_video_planar(0, 0), 0);
}

#[test]
fn test_put_pixel_mode13_bounds() {
    let mut memory = Memory::new();

    Vga::put_pixel_mode13(&mut memory, 0, 0, 0x12);
    assert_eq!(memory.read_u8(0xA0000), 0x12);

    Vga::put_pixel_mode13(&mut memory, 320, 0, 0x34);
    assert_eq!(memory.read_u8(0xA0000), 0x12);

    Vga::put_pixel_mode13(&mut memory, 0, 200, 0x56);
    assert_eq!(memory.read_u8(0xA0000), 0x12);
}

#[test]
fn test_put_pixel_modex_in_bounds() {
    let mut memory = Memory::new();

    Vga::put_pixel_modex(&mut memory, 2, 0, 320, 200, 0x7A);
    assert_eq!(memory.read_video_planar(0, 2), 0x7A);
    assert_eq!(memory.read_video_planar(0, 0), 0);
}

#[test]
fn test_read_char_attr_graphics_mode() {
    let mut vga = Vga::new();
    let memory = Memory::new();
    vga.set_mode_bios(0x13);
    let (ch, attr) = vga.read_char_attr(0, &memory);
    assert_eq!(ch, 0);
    assert_eq!(attr, 0);
}

#[test]
fn test_write_char_attr_graphics_mode() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);
    vga.write_char_attr(0, b'X', 0x70, 1, &mut memory); // No-op in graphics mode
}

#[test]
fn test_write_char_graphics_mode() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);
    vga.write_char(0, b'X', 1, &mut memory); // No-op in graphics mode
}

#[test]
fn test_write_char_attr_cga_graphics_xor() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    let ch = first_nonzero_row_char();
    vga.set_mode_bios(0x04);
    vga.set_cursor(0, 0, 0);

    let addr0 = 0xB8000;
    let addr1 = 0xB8001;
    assert_eq!(memory.read_u8(addr0), 0);
    assert_eq!(memory.read_u8(addr1), 0);

    vga.write_char_attr(0, ch, 0x81, 1, &mut memory);
    let mid0 = memory.read_u8(addr0);
    let mid1 = memory.read_u8(addr1);
    assert!((mid0 != 0) | (mid1 != 0));

    vga.write_char_attr(0, ch, 0x81, 1, &mut memory);
    assert_eq!(memory.read_u8(addr0), 0);
    assert_eq!(memory.read_u8(addr1), 0);
}

#[test]
fn test_write_char_attr_cga_graphics_column_limit() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    let ch = first_nonzero_row_char();

    vga.set_mode_bios(0x04);
    vga.set_cursor(0, 0, 39);
    vga.write_char_attr(0, ch, 0x01, 2, &mut memory);

    assert_ne!(memory.read_u8(0xB8000 + 78), 0);
}

#[test]
fn test_write_char_attr_cga_graphics_row_limit() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    let ch = first_nonzero_row_char();

    vga.set_mode_bios(0x04);
    vga.set_cursor(0, 25, 0);
    vga.write_char_attr(0, ch, 0x01, 1, &mut memory);

    assert_eq!(memory.read_u8(0xB8000), 0);
}

#[test]
fn test_write_char_cga_graphics_mode() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    let ch = first_nonzero_row_char();
    vga.set_mode_bios(0x04);
    vga.set_cursor(0, 0, 0);
    vga.write_char(0, ch, 1, &mut memory);
    let addr0 = 0xB8000;
    let addr1 = 0xB8001;
    let val0 = memory.read_u8(addr0);
    let val1 = memory.read_u8(addr1);
    assert!((val0 != 0) | (val1 != 0));
}

#[test]
fn test_teletype_cga_graphics_mode() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    let ch = first_nonzero_row_char();
    vga.set_mode_bios(0x04);
    vga.set_cursor(0, 0, 0);
    vga.teletype(ch, 0, &mut memory);
    let addr0 = 0xB8000;
    let addr1 = 0xB8001;
    let val0 = memory.read_u8(addr0);
    let val1 = memory.read_u8(addr1);
    assert!((val0 != 0) | (val1 != 0));
}

#[test]
fn test_teletype_cga_graphics_row_out_of_bounds() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    let ch = first_nonzero_row_char();

    vga.set_mode_bios(0x04);
    vga.set_cursor(0, 25, 0);
    vga.teletype(ch, 0, &mut memory);
    assert_eq!(memory.read_u8(0xB8000), 0);
}

/// CGA teletype must clear background pixels (set to color 0), not just
/// draw foreground. This prevents old text from showing through when
/// new text overwrites the same position.
#[test]
fn test_teletype_cga_clears_background_pixels() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);

    // First, fill the first character cell (columns 0, rows 0-7) with
    // color 3 (all pixels on). Two bytes cover 8 pixels (4 pixels/byte).
    for cy in 0..8usize {
        let line_offset = if (cy & 1) == 0 {
            (cy / 2) * 80
        } else {
            0x2000 + (cy / 2) * 80
        };
        // Set bytes 0 and 1 to all-color-3 (0xFF)
        memory.write_u8(0xB8000 + line_offset, 0xFF);
        memory.write_u8(0xB8000 + line_offset + 1, 0xFF);
    }

    // Now write a space character (0x20) via teletype at row 0, col 0.
    // Space has no foreground pixels, so the entire cell should be cleared.
    vga.set_cursor(0, 0, 0);
    vga.teletype(0x20, 0, &mut memory);

    // Verify: all pixels in the character cell should now be color 0.
    for cy in 0..8usize {
        let line_offset = if (cy & 1) == 0 {
            (cy / 2) * 80
        } else {
            0x2000 + (cy / 2) * 80
        };
        let b0 = memory.read_u8(0xB8000 + line_offset);
        let b1 = memory.read_u8(0xB8000 + line_offset + 1);
        assert_eq!(
            b0, 0x00,
            "CGA teletype should clear background at scanline {cy}, byte 0"
        );
        assert_eq!(
            b1, 0x00,
            "CGA teletype should clear background at scanline {cy}, byte 1"
        );
    }
}

/// write_char_for_string (AH=13h) must clear background in CGA mode,
/// unlike write_char_attr (AH=09h) which only writes foreground.
#[test]
fn test_write_char_for_string_cga_clears_background() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);

    // Fill the first char cell with solid color 3
    for cy in 0..8usize {
        let line_offset = if (cy & 1) == 0 {
            (cy / 2) * 80
        } else {
            0x2000 + (cy / 2) * 80
        };
        memory.write_u8(0xB8000 + line_offset, 0xFF);
        memory.write_u8(0xB8000 + line_offset + 1, 0xFF);
    }

    // Write a space via write_char_for_string (AH=13h path)
    vga.set_cursor(0, 0, 0);
    vga.write_char_for_string(0, 0x20, 0x03, &mut memory);

    // Background should be cleared
    for cy in 0..8usize {
        let line_offset = if (cy & 1) == 0 {
            (cy / 2) * 80
        } else {
            0x2000 + (cy / 2) * 80
        };
        assert_eq!(
            memory.read_u8(0xB8000 + line_offset),
            0x00,
            "write_char_for_string should clear bg at scanline {cy}"
        );
    }
}

/// write_char_attr (AH=09h) replaces the full character cell in CGA mode
/// (foreground + background), per IBM PC Technical Reference.
#[test]
fn test_write_char_attr_cga_replaces_full_cell() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);

    // Fill the first char cell with solid color 3
    for cy in 0..8usize {
        let line_offset = if (cy & 1) == 0 {
            (cy / 2) * 80
        } else {
            0x2000 + (cy / 2) * 80
        };
        memory.write_u8(0xB8000 + line_offset, 0xFF);
        memory.write_u8(0xB8000 + line_offset + 1, 0xFF);
    }

    // Write a space via write_char_attr (AH=09h) — replaces full cell
    vga.set_cursor(0, 0, 0);
    vga.write_char_attr(0, 0x20, 0x03, 1, &mut memory);

    // Background should now be cleared (space has no foreground pixels,
    // so entire cell becomes color 0)
    let b0 = memory.read_u8(0xB8000);
    assert_eq!(
        b0, 0x00,
        "write_char_attr should clear background pixels in CGA mode"
    );
}

#[test]
fn test_scroll_up_graphics_mode() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);
    let region = ScrollRegion {
        top: 0,
        left: 0,
        bottom: 24,
        right: 39,
    };
    vga.scroll_up(1, 0x00, region, &mut memory); // No-op in graphics mode
}

#[test]
fn test_scroll_down_graphics_mode() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);
    let region = ScrollRegion {
        top: 0,
        left: 0,
        bottom: 24,
        right: 39,
    };
    vga.scroll_down(1, 0x00, region, &mut memory);
}

#[test]
fn test_scroll_up_zero_lines() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    let region = ScrollRegion {
        top: 0,
        left: 0,
        bottom: 24,
        right: 79,
    };
    vga.scroll_up(0, 0x07, region, &mut memory); // 0 means clear entire region
}

#[test]
fn test_scroll_down_zero_lines() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    let region = ScrollRegion {
        top: 0,
        left: 0,
        bottom: 24,
        right: 79,
    };
    vga.scroll_down(0, 0x07, region, &mut memory); // 0 means clear entire region
}

#[test]
fn test_write_char_attr_count() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 0, 0);
    vga.write_char_attr(0, b'X', 0x70, 10, &mut memory); // Write 10 chars
}

#[test]
fn test_write_char_count_overflow() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 0, 75);
    vga.write_char_attr(0, b'X', 0x70, 10, &mut memory); // Should stop at column 79
}

#[test]
fn test_set_active_page_text_mode() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    vga.set_active_page(1);
    assert_eq!(vga.active_page, 1);
}

#[test]
fn test_dac_read_write_cycle() {
    let mut vga = Vga::new();

    // Write to DAC
    vga.write_port(0x3C8, 10); // Start writing at index 10
    vga.write_port(0x3C9, 63); // Red
    vga.write_port(0x3C9, 32); // Green
    vga.write_port(0x3C9, 16); // Blue

    // Read back
    vga.write_port(0x3C7, 10); // Start reading at index 10
    let r = vga.read_port(0x3C9);
    let g = vga.read_port(0x3C9);
    let b = vga.read_port(0x3C9);

    assert_eq!(r, 63);
    assert_eq!(g, 32);
    assert_eq!(b, 16);
}

#[test]
fn test_dac_auto_increment() {
    let mut vga = Vga::new();

    // Write multiple entries
    vga.write_port(0x3C8, 0);
    for i in 0..9 {
        vga.write_port(0x3C9, i);
    }

    // Read back first 3 entries
    vga.write_port(0x3C7, 0);
    let _r0 = vga.read_port(0x3C9);
    let _g0 = vga.read_port(0x3C9);
    let _b0 = vga.read_port(0x3C9);
    // Auto-increment to next entry
    let _r1 = vga.read_port(0x3C9);
}

#[test]
fn test_crtc_read_write() {
    let mut vga = Vga::new();

    // Write to CRTC via 3D4/3D5
    vga.write_port(0x3D4, 0x0C); // Start address high
    vga.write_port(0x3D5, 0x12);
    assert_eq!(vga.read_port(0x3D5), 0x12);

    // Test reading CRTC index
    assert_eq!(vga.read_port(0x3D4), 0x0C);
}

#[test]
fn test_crtc_out_of_range() {
    let mut vga = Vga::new();
    vga.write_port(0x3D4, 0xFF); // Out of range index
    assert_eq!(vga.read_port(0x3D5), 0);
}

#[test]
fn test_render_cga_mode05() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x05);
    vga.render(&mut memory);
    let (w, h) = vga.dimensions();
    assert_eq!(w, 320);
    assert_eq!(h, 200);
}

#[test]
fn test_cga_mode05_uses_cyan_red_white_palette() {
    // Mode 05 has its own fixed palette: Cyan/Red/White
    // This is different from Mode 04 Palette 1 (Cyan/Magenta/White)
    // The palette selection bit has no effect in Mode 05.
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x05);

    // Explicitly set Palette 0 via color_select_reg (bit 5 = 0)
    // This would select Green/Red/Brown in Mode 04, but Mode 05 ignores it
    vga.set_cga_color_select(0x00); // Palette 0 selected, no intensity

    // Write color index 2 to first 4 pixels (the key distinguishing color)
    // Binary: 10 10 10 10 = 0xAA
    memory.write_u8(0xB8000, 0xAA);
    vga.render(&mut memory);

    // Color index 2 in Mode 05 palette = Red (0xAA0000)
    // Color index 2 in Mode 04 Palette 1 = Magenta (0xAA00AA)
    // Color index 2 in Mode 04 Palette 0 = Red (0xAA0000)
    let fb = vga.framebuffer();
    let pixel = fb[0];

    let r = (pixel >> 16) & 0xFF;
    let g = (pixel >> 8) & 0xFF;
    let b = pixel & 0xFF;

    // Mode 05's color 2 is RED, not magenta
    assert_eq!(r, 0xAA, "Mode 05 color 2 should have AA red");
    assert_eq!(g, 0x00, "Mode 05 color 2 should have no green");
    assert_eq!(
        b, 0x00,
        "Mode 05 color 2 should have no blue (red, not magenta)"
    );
}

#[test]
fn test_cga_mode05_color1_is_cyan() {
    // Mode 05 color 1 should be Cyan
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x05);
    vga.set_cga_color_select(0x00);

    // Write color index 1 to first 4 pixels
    // Binary: 01 01 01 01 = 0x55
    memory.write_u8(0xB8000, 0x55);
    vga.render(&mut memory);

    let fb = vga.framebuffer();
    let pixel = fb[0];

    let r = (pixel >> 16) & 0xFF;
    let g = (pixel >> 8) & 0xFF;
    let b = pixel & 0xFF;

    // Mode 05's color 1 is Cyan
    assert_eq!(r, 0x00, "Mode 05 color 1 should have no red");
    assert_eq!(g, 0xAA, "Mode 05 color 1 should have AA green");
    assert_eq!(b, 0xAA, "Mode 05 color 1 should have AA blue (cyan)");
}

#[test]
fn test_cga_mode04_palette0_color2_is_red() {
    // Mode 04 Palette 0 color 2 should be Red
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);
    vga.set_cga_color_select(0x00); // Palette 0

    memory.write_u8(0xB8000, 0xAA); // Color index 2
    vga.render(&mut memory);

    let fb = vga.framebuffer();
    let pixel = fb[0];

    let r = (pixel >> 16) & 0xFF;
    let b = pixel & 0xFF;

    assert_eq!(r, 0xAA, "Mode 04 Palette 0 color 2 should be red");
    assert_eq!(b, 0x00, "Mode 04 Palette 0 color 2 should have no blue");
}

#[test]
fn test_cga_mode04_palette1_color2_is_magenta() {
    // Mode 04 Palette 1 color 2 should be Magenta
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);
    vga.set_cga_color_select(0x20); // Palette 1

    memory.write_u8(0xB8000, 0xAA); // Color index 2
    vga.render(&mut memory);

    let fb = vga.framebuffer();
    let pixel = fb[0];

    let r = (pixel >> 16) & 0xFF;
    let b = pixel & 0xFF;

    assert_eq!(r, 0xAA, "Mode 04 Palette 1 color 2 should have AA red");
    assert_eq!(
        b, 0xAA,
        "Mode 04 Palette 1 color 2 should have AA blue (magenta)"
    );
}

#[test]
fn test_cga_mode04_palette0_color1_is_green() {
    // Mode 04 Palette 0 color 1 should be Green
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);
    vga.set_cga_color_select(0x00); // Palette 0

    memory.write_u8(0xB8000, 0x55); // Color index 1
    vga.render(&mut memory);

    let fb = vga.framebuffer();
    let pixel = fb[0];

    let r = (pixel >> 16) & 0xFF;
    let g = (pixel >> 8) & 0xFF;
    let b = pixel & 0xFF;

    assert_eq!(r, 0x00, "Mode 04 Palette 0 color 1 should have no red");
    assert_eq!(g, 0xAA, "Mode 04 Palette 0 color 1 should have AA green");
    assert_eq!(b, 0x00, "Mode 04 Palette 0 color 1 should have no blue");
}

#[test]
fn test_cga_palette_variations() {
    let mut vga = Vga::new();

    // Test palette 0 low intensity
    vga.set_cga_color_select(0x00);
    assert_eq!(vga.get_cga_color_select(), 0x00);

    // Test palette 1 high intensity
    vga.set_cga_color_select(0x30);
    assert_eq!(vga.get_cga_color_select(), 0x30);
}

#[test]
fn test_input_status_retrace() {
    let mut vga = Vga::new();

    // Force deterministic cycles-per-line to exercise both paths.
    let scanlines = vga.get_total_scanlines();
    vga.timing_cycles_per_frame = scanlines * 10;
    vga.timing_retrace_cycles = 0;
    let cycles_per_line = vga.timing_cycles_per_frame / scanlines;

    // Blank now occupies the END of the line (MDD-BUG-KILN-00025): the
    // start of a line is display (bit 0 clear), the tail is blank (bit 0
    // set). Probe both to confirm bit 0 tracks horizontal position.
    vga.cycles_since_vblank = 0; // start of line -> display
    let status_display = vga.read_port(0x3DA);

    vga.cycles_since_vblank = cycles_per_line - 1; // end of line -> hblank
    let status_hblank = vga.read_port(0x3DA);

    assert_ne!(status_display & 0x01, status_hblank & 0x01);
}

#[test]
fn test_input_status_no_cycles_per_line() {
    let mut vga = Vga::new();
    vga.timing_cycles_per_frame = 0;
    vga.timing_retrace_cycles = 0;
    let status = vga.read_port(0x3DA);
    assert_eq!(status & 0x01, 0);
}

#[test]
fn test_palette_reg_bounds() {
    let mut vga = Vga::new();
    vga.set_palette_reg(15, 0x3F); // Valid
    vga.set_palette_reg(255, 0x3F); // Out of bounds, should not panic
}

#[test]
fn test_dac_reg_bounds() {
    let mut vga = Vga::new();
    vga.set_dac_reg(255, 63, 32, 16); // Valid
    vga.set_dac_reg(256, 63, 32, 16); // Out of bounds, should not panic
}

#[test]
fn test_dac_raw_bounds() {
    let mut vga = Vga::new();
    vga.set_dac_raw(0, 1, 2, 3);
    vga.set_dac_raw(256, 4, 5, 6);
    assert_eq!(vga.dac[0], (1, 2, 3));
}

#[test]
fn test_read_port_unknown() {
    let mut vga = Vga::new();
    let val = vga.read_port(0x300); // Unknown port returns 0
    assert_eq!(val, 0);
}

#[test]
fn test_write_port_unknown() {
    let mut vga = Vga::new();
    vga.write_port(0x300, 0x55); // Unknown port, should not panic
}

#[test]
fn test_render_mode_x_with_height() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x13);
    vga.fb_width = 320;
    vga.fb_height = 240;
    // Programme Mode X via registers (chain-4 cleared by default; set 256-colour).
    vga.attribute_regs[0x10] = 0x41; // 256-color + graphics
    vga.sequencer_regs[4] &= !0x08; // chain-4 cleared
    vga.crtc_regs[0x01] = 0x4F; // 80 char clocks -> 320px in 256-color
    vga.crtc_regs[0x12] = 239; // VDE low = 239
    vga.crtc_regs[0x07] = 0; // clear overflow VDE high bits
    vga.crtc_regs[0x09] = 0; // max_scan=0 → height=240
    vga.framebuffer = vec![0; 320 * 240];
    vga.render(&mut memory);

    let (w, h) = vga.dimensions();
    assert_eq!(w, 320);
    assert_eq!(h, 240);
}

#[test]
fn test_is_text_mode_all_modes() {
    let mut vga = Vga::new();

    // Text modes
    for mode in [0x00, 0x01, 0x02, 0x03, 0x07] {
        vga.set_mode_bios(mode);
        assert!(vga.is_text_mode(), "Mode {mode:02X} should be text mode");
    }

    // Graphics modes
    for mode in [0x04, 0x05, 0x06, 0x0D, 0x0E, 0x10, 0x12, 0x13] {
        vga.set_mode_bios(mode);
        assert!(
            !vga.is_text_mode(),
            "Mode {mode:02X} should be graphics mode"
        );
    }
}

#[test]
fn test_default_dac_palette() {
    let vga = Vga::new();

    // === CGA 16-color palette at entries 0-15 ===
    assert_eq!(vga.dac[0], (0, 0, 0), "Black");
    assert_eq!(vga.dac[1], (0, 0, 170), "Blue");
    assert_eq!(vga.dac[2], (0, 170, 0), "Green");
    assert_eq!(vga.dac[3], (0, 170, 170), "Cyan");
    assert_eq!(vga.dac[4], (170, 0, 0), "Red");
    assert_eq!(vga.dac[5], (170, 0, 170), "Magenta");
    assert_eq!(vga.dac[6], (170, 85, 0), "Brown");
    assert_eq!(vga.dac[7], (170, 170, 170), "Light gray");
    assert_eq!(vga.dac[8], (85, 85, 85), "Dark gray");
    assert_eq!(vga.dac[9], (85, 85, 255), "Light blue");
    assert_eq!(vga.dac[10], (85, 255, 85), "Light green");
    assert_eq!(vga.dac[11], (85, 255, 255), "Light cyan");
    assert_eq!(vga.dac[12], (255, 85, 85), "Light red");
    assert_eq!(vga.dac[13], (255, 85, 255), "Light magenta");
    assert_eq!(vga.dac[14], (255, 255, 85), "Yellow");
    assert_eq!(vga.dac[15], (255, 255, 255), "White");

    // === EGA 64-color palette at entries 16-63 ===
    // These are the entries referenced by VPT in text/16-color modes.
    // Bit layout: rgbRGB (bits 5-3=secondary, bits 2-0=primary)

    // Entry 0x14 (20): brown - VPT maps pixel 6 here
    // 0b010100 = R_primary + g_secondary = (0xAA, 0x55, 0x00)
    assert_eq!(vga.dac[0x14], (170, 85, 0), "EGA brown at 0x14");
    // Entry 0x38 (56): dark gray - VPT maps pixel 8 here
    // 0b111000 = r_sec + g_sec + b_sec = (0x55, 0x55, 0x55)
    assert_eq!(vga.dac[0x38], (85, 85, 85), "EGA dark gray at 0x38");
    // Entry 0x39 (57): light blue - VPT maps pixel 9 here
    // 0b111001 = r_sec + g_sec + b_sec + B_primary
    assert_eq!(vga.dac[0x39], (85, 85, 255), "EGA light blue at 0x39");
    // Entry 0x3E (62): yellow - VPT maps pixel 14 here
    assert_eq!(vga.dac[0x3E], (255, 255, 85), "EGA yellow at 0x3E");
    // Entry 0x3F (63): white - VPT maps pixel 15 here
    assert_eq!(vga.dac[0x3F], (255, 255, 255), "EGA white at 0x3F");

    // === Color cube (entries 64-231) ===
    // Entry 64: cube position 48 (r=1,g=2,b=0) → (48, 101, 0)
    assert_eq!(vga.dac[64], (48, 101, 0));
    // Entry 231 is the last cube entry (white)
    assert_eq!(vga.dac[231], (255, 255, 255));

    // === Grayscale ramp (entries 232-255) ===
    assert_eq!(vga.dac[232], (8, 8, 8));
    assert_eq!(vga.dac[255], (238, 238, 238));
}

#[test]
fn test_render_40x25_text() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x01); // 40x25 text (double-wide: 720×400)
    vga.render(&mut memory);
    let (w, h) = vga.dimensions();
    assert_eq!(w, 720);
    assert_eq!(h, 400);
}

#[test]
fn test_write_char_attr() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03); // Text mode
    vga.write_char_attr(0, b'A', 0x07, 1, &mut memory);
    // Verify character was written
    let (ch, attr) = vga.read_char_attr(0, &memory);
    assert_eq!(ch, b'A');
    assert_eq!(attr, 0x07);
}

#[test]
fn test_set_cursor_updates_crtc() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 5, 10);

    let pos = 5 * 80 + 10;
    assert_eq!(vga.crtc_regs[0x0E], ((pos >> 8) & 0xFF) as u8);
    assert_eq!(vga.crtc_regs[0x0F], (pos & 0xFF) as u8);
}

#[test]
fn test_write_char_multiple() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03); // Text mode
    vga.set_cursor(0, 0, 0);
    vga.write_char_attr(0, b'B', 0x0F, 3, &mut memory);
    // First character should be B
    let (ch, _) = vga.read_char_attr(0, &memory);
    assert_eq!(ch, b'B');
}

#[test]
fn test_write_char_no_attr() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03); // Text mode
    vga.write_char(0, b'C', 1, &mut memory);
    let (ch, _) = vga.read_char_attr(0, &memory);
    assert_eq!(ch, b'C');
}

#[test]
fn test_teletype_cursor_advance() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03); // Text mode
    vga.set_cursor(0, 0, 0);
    vga.teletype(b'D', 0, &mut memory);
    let (row, col) = vga.get_cursor(0);
    // Cursor should have advanced
    assert!((row > 0) | (col > 0));
}

#[test]
fn test_teletype_lf() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 5, 10);
    vga.teletype(b'\n', 0, &mut memory);
    let (row, col) = vga.get_cursor(0);
    // LF moves down one row but preserves column in DOS
    assert_eq!(row, 6);
    assert_eq!(col, 10);
}

#[test]
fn test_teletype_cr() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 5, 10);
    vga.teletype(b'\r', 0, &mut memory);
    let (row, col) = vga.get_cursor(0);
    assert_eq!(row, 5);
    assert_eq!(col, 0);
}

#[test]
fn test_teletype_bs() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 5, 10);
    vga.teletype(0x08, 0, &mut memory); // Backspace
    let (row, col) = vga.get_cursor(0);
    assert_eq!(row, 5);
    assert_eq!(col, 9);
}

#[test]
fn test_teletype_bel() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 5, 10);
    vga.teletype(0x07, 0, &mut memory); // Bell
                                        // Cursor should not move
    let (row, col) = vga.get_cursor(0);
    assert_eq!(row, 5);
    assert_eq!(col, 10);
}

#[test]
fn test_write_read_pixel_mode13h() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13); // Mode 13h
    vga.write_pixel(0, 100, 50, 0x0F, &mut memory);
    let color = vga.read_pixel(0, 100, 50, &memory);
    assert_eq!(color, 0x0F);
}

#[test]
fn test_write_pixel_cga() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04); // CGA mode
    vga.write_pixel(0, 10, 10, 0x01, &mut memory);
    // Just verify no crash
}

#[test]
fn test_get_plane_mask() {
    let vga = Vga::new();
    let mask = vga.get_plane_mask();
    // Default plane mask should be valid (0-15 for 4 planes)
    assert!(mask <= 0x0F);
}

#[test]
fn test_get_read_plane() {
    let vga = Vga::new();
    let plane = vga.get_read_plane();
    assert!(plane < 4);
}

#[test]
fn test_is_mode_x_default() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    // Mode X is NOT activated by default in mode 13h
    assert!(!vga.is_mode_x());
}

/// Regression: `get_framebuffer_dimensions()` must use register-derived
/// `mode()` when Mode 13h registers are programmed directly without
/// an INT 10h HLE mode set.
#[test]
fn test_get_framebuffer_dimensions_register_derived_for_real_boot_mode13() {
    let mut vga = Vga::new();
    // Program VGA registers directly: enable 256-colour
    // via AR[0x10] bit 6 and chain-4 via SR[4] bit 3, which makes
    // register-derived mode() return 0x13.
    vga.attribute_regs[0x10] |= 0x40; // 256-colour
    vga.sequencer_regs[4] |= 0x08; // chain-4 enabled
    assert_eq!(vga.mode(), 0x13);

    let (w, h) = vga.get_framebuffer_dimensions();
    assert_eq!(
        (w, h),
        (320, 200),
        "must return Mode-13h dims (320x200) when registers indicate 256-colour mode"
    );
}

/// Regression: `get_scanlines_per_frame()` must use register-derived
/// `mode()` when Mode 13h registers are programmed directly without
/// an INT 10h HLE mode set.
#[test]
fn test_get_scanlines_per_frame_register_derived_for_real_boot_mode13() {
    let mut vga = Vga::new();
    // Program VGA registers directly: enable 256-colour
    // via AR[0x10] bit 6 and chain-4 via SR[4] bit 3.
    vga.attribute_regs[0x10] |= 0x40; // 256-colour
    vga.sequencer_regs[4] |= 0x08; // chain-4 enabled
    assert_eq!(vga.mode(), 0x13);

    let visible = vga.get_scanlines_per_frame();
    assert_eq!(
        visible, 200,
        "must return Mode-13h scanline count (200) under register-derived mode, \
         not text-mode 400"
    );
}

#[test]
fn test_recompute_scanline_timing_register_derived_for_real_boot_mode13() {
    let mut vga = Vga::new();
    // Cache starts at the Vga::new default of 400 (text mode default).
    assert_eq!(vga.visible_scanlines_cached, 400);

    // Program VGA registers directly.
    vga.attribute_regs[0x10] |= 0x40; // 256-colour
    vga.sequencer_regs[4] |= 0x08; // chain-4 enabled
    assert_eq!(vga.mode(), 0x13);

    vga.recompute_scanline_timing();
    assert_eq!(
        vga.visible_scanlines_cached, 200,
        "recompute must use register-derived mode and produce 200 \
         for Mode 13h, not the text-mode 400 default"
    );
}

#[test]
fn test_get_scanlines_per_frame_mode_x_240_lines() {
    let mut vga = Vga::new();
    // 256-colour: AR[0x10] bit 6 set; chain-4 disabled (Mode X).
    vga.attribute_regs[0x10] |= 0x40;
    vga.sequencer_regs[4] &= !0x08; // chain-4 OFF
                                    // Programme CRTC for Mode X 320×240 single-scan. VDE = 239 (low byte
                                    // 0xEF, overflow bits 8/9 already zero). Max scan = 0 to disable
                                    // double-scan, which also satisfies `is_mode_x()`'s CR9 predicate.
    vga.crtc_regs[0x12] = 0xEF; // VDE low = 239
    vga.crtc_regs[0x07] = 0; // clear VDE overflow bits 8/9 + LC bit 8
    vga.crtc_regs[0x09] = 0; // max_scan_line = 0 (no double-scan)
    assert_eq!(vga.mode(), 0x13);
    assert!(
        vga.is_mode_x(),
        "Mode X precondition: chain-4 off + 256-colour"
    );

    let visible = vga.get_scanlines_per_frame();
    // Strict assertion: the chain-4 literal 200 is the buggy value we're
    // catching. With VDE=239 + max_scan=0 we expect exactly 240. A range
    // assertion `(200..=480)` would have passed the broken pre-fix code
    // (which returned 200 even for Mode X) — the strict equality nails it.
    assert_eq!(
        visible, 240,
        "Mode X with VDE=239, max_scan=0 must return 240, not the chain-4 literal 200"
    );
}

#[test]
fn test_cga_640x200() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x06); // 640x200 2-color
    vga.framebuffer = vec![0u32; 1];
    vga.fb_width = 1;
    vga.fb_height = 1;
    vga.render(&mut memory);
    let (w, h) = vga.dimensions();
    assert_eq!(w, 640);
    assert_eq!(h, 200);
}

#[test]
fn test_ega_640x350() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x10); // 640x350 EGA
    vga.render(&mut memory);
    let (w, h) = vga.dimensions();
    assert_eq!(w, 640);
    assert_eq!(h, 350);
}

#[test]
fn test_vga_640x480() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x12); // 640x480 VGA
    vga.render(&mut memory);
    let (w, h) = vga.dimensions();
    assert_eq!(w, 640);
    assert_eq!(h, 480);
}

#[test]
fn test_read_pixel_out_of_bounds() {
    let vga = Vga::new();
    let memory = Memory::new();
    // Reading out of bounds should return 0
    let color = vga.read_pixel(0, 5000, 5000, &memory);
    assert_eq!(color, 0);
}

#[test]
fn test_attribute_controller_ports() {
    let mut vga = Vga::new();
    // The attribute controller has complex flip-flop behavior
    // Reset flip-flop by reading input status register
    vga.read_port(0x3DA);
    // Write to attribute controller
    vga.write_port(0x3C0, 0x00); // Select register 0
    vga.write_port(0x3C0, 0x0F); // Write value
                                 // Read the attribute data port - may not match due to internal state
    let _val = vga.read_port(0x3C1);
}

#[test]
fn original_trio64_sequencer_screen_off_blanks_full_and_scanline_rendering() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.framebuffer.fill(0x00AB_CDEF);
    vga.write_port(0x3C4, 0x01);
    vga.write_port(0x3C5, vga.sequencer_regs[0x01] | 0x20);

    vga.render(&mut memory);
    assert!(vga.framebuffer.iter().all(|&pixel| pixel == 0));

    vga.framebuffer.fill(0x0012_3456);
    vga.render_scanline(1, &mut memory);
    let start = vga.fb_width;
    assert!(vga.framebuffer[start..start + vga.fb_width]
        .iter()
        .all(|&pixel| pixel == 0));
    assert_ne!(vga.framebuffer[0], 0);
}

#[test]
fn test_attribute_controller_mode_control_marks_palette16_dirty() {
    let mut vga = Vga::new();
    vga.rebuild_palette16_lut_if_dirty();
    assert!(!vga.palette16_lut_dirty);

    vga.write_port(0x3C0, 0x10);
    vga.write_port(0x3C0, 0x41);
    assert!(vga.palette16_lut_dirty);

    vga.rebuild_palette16_lut_if_dirty();
    vga.write_port(0x3C0, 0x14);
    vga.write_port(0x3C0, 0x22);
    assert!(vga.palette16_lut_dirty);
}

#[test]
fn test_attribute_controller_color_select_marks_palette16_dirty() {
    let mut vga = Vga::new();
    vga.rebuild_palette16_lut_if_dirty();
    assert!(!vga.palette16_lut_dirty);

    // Use proper 0x3C0 flip-flop: index write, then data write
    vga.read_port(0x3DA); // Reset flip-flop
    vga.write_port(0x3C0, 0x14); // Index: color select (0x14)
    vga.write_port(0x3C0, 0x22); // Data: value 0x22
    assert!(vga.palette16_lut_dirty);

    vga.rebuild_palette16_lut_if_dirty();
    vga.read_port(0x3DA); // Reset flip-flop
    vga.write_port(0x3C0, 0x10); // Index: mode control (0x10)
    vga.write_port(0x3C0, 0x11); // Data: value 0x11
    assert!(vga.palette16_lut_dirty);
}

#[test]
fn test_port_3c0_read_returns_index_with_pas() {
    let mut vga = Vga::new();
    // Reset flip-flop
    vga.read_port(0x3DA);
    // Write index 5 with PAS bit set (0x25)
    vga.write_port(0x3C0, 0x25);
    // Read back should return index with PAS bit
    assert_eq!(vga.read_port(0x3C0), 0x25);

    // Reset and write without PAS
    vga.read_port(0x3DA);
    vga.write_port(0x3C0, 0x05); // Index 5, PAS clear
    assert_eq!(vga.read_port(0x3C0), 0x05);
}

#[test]
fn test_port_3c2_input_status_0() {
    let mut vga = Vga::new();
    // When misc_output clock select bits (3-2) are clear, switch sense should be set
    vga.misc_output = 0x00;
    assert_eq!(vga.read_port(0x3C2) & 0x10, 0x10);

    // When misc_output clock select bits are set, switch sense should be clear
    vga.misc_output = 0x0C;
    assert_eq!(vga.read_port(0x3C2) & 0x10, 0x00);
}

#[test]
fn test_port_3ba_mirrors_3da() {
    let mut vga = Vga::new();
    // Both ports should reset the attribute flip-flop
    vga.read_port(0x3DA);
    vga.write_port(0x3C0, 0x10); // Set flip-flop to data state
                                 // Read 0x3BA should reset it
    let status_3ba = vga.read_port(0x3BA);
    // Read 0x3DA should return similar status
    let status_3da = vga.read_port(0x3DA);
    // Both should have similar retrace bits (timing may vary)
    // At minimum, both should have valid status register behavior
    assert!((status_3ba < 0x10) | ((status_3ba & 0x08) != 0) | ((status_3ba & 0x01) != 0));
    assert!((status_3da < 0x10) | ((status_3da & 0x08) != 0) | ((status_3da & 0x01) != 0));
}

#[test]
fn test_sequencer_write_read() {
    let mut vga = Vga::new();
    vga.write_port(0x3C4, 0x01); // Select clocking mode register
    vga.write_port(0x3C5, 0x09); // Write value
    vga.write_port(0x3C4, 0x01);
    let val = vga.read_port(0x3C5);
    assert_eq!(val, 0x09);
}

#[test]
fn test_graphics_controller_write_read() {
    let mut vga = Vga::new();
    vga.write_port(0x3CE, 0x00); // Select set/reset register
    vga.write_port(0x3CF, 0x05); // Write value
    vga.write_port(0x3CE, 0x00);
    let val = vga.read_port(0x3CF);
    assert_eq!(val, 0x05);
}

#[test]
fn test_get_plane_mask_value() {
    let mut vga = Vga::new();
    vga.write_port(0x3C4, 0x02);
    vga.write_port(0x3C5, 0x0F);
    assert_eq!(vga.get_plane_mask(), 0x0F);
}

#[test]
fn test_get_read_plane_value() {
    let mut vga = Vga::new();
    vga.write_port(0x3CE, 0x04);
    vga.write_port(0x3CF, 0x02);
    assert_eq!(vga.get_read_plane(), 0x02);
}

#[test]
fn test_scroll_up_partial_region() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    let region = ScrollRegion {
        top: 5,
        left: 10,
        bottom: 15,
        right: 50,
    };
    vga.scroll_up(3, 0x1F, region, &mut memory);
}

#[test]
fn test_scroll_down_partial_region() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    let region = ScrollRegion {
        top: 5,
        left: 10,
        bottom: 15,
        right: 50,
    };
    vga.scroll_down(3, 0x2E, region, &mut memory);
}

#[test]
fn test_crtc_cursor_registers() {
    let mut vga = Vga::new();
    vga.write_port(0x3D4, 0x0E);
    vga.write_port(0x3D5, 0x00);
    assert_eq!(vga.read_port(0x3D5), 0x00);
    vga.write_port(0x3D4, 0x0F);
    vga.write_port(0x3D5, 0x50);
    assert_eq!(vga.read_port(0x3D5), 0x50);
}

#[test]
fn test_mode_x_240_lines() {
    let mut vga = Vga::new();
    let mut mem = Memory::new();
    vga.set_mode_bios(0x13);
    vga.fb_width = 320;
    vga.fb_height = 200;
    // Programme Mode X via registers (height = 240 from CRTC).
    vga.attribute_regs[0x10] = 0x41; // 256-color + graphics
    vga.sequencer_regs[4] &= !0x08;
    vga.crtc_regs[0x01] = 0x4F; // 80 char clocks -> 320px in 256-color
    vga.crtc_regs[0x12] = 239; // VDE low = 239
    vga.crtc_regs[0x07] = 0; // clear overflow VDE high bits
    vga.crtc_regs[0x09] = 0; // max_scan=0 → height=240
    vga.render(&mut mem);
    let (_, h) = vga.dimensions();
    assert_eq!(h, 240);
}

#[test]
fn test_dac_write_read() {
    let mut vga = Vga::new();
    // Write palette index
    vga.write_port(0x3C8, 10);
    // Write RGB values
    vga.write_port(0x3C9, 63); // R
    vga.write_port(0x3C9, 32); // G
    vga.write_port(0x3C9, 16); // B
                               // Read back
    vga.write_port(0x3C7, 10);
    let r = vga.read_port(0x3C9);
    let g = vga.read_port(0x3C9);
    let b = vga.read_port(0x3C9);
    // Values are scaled 6-bit to 8-bit
    assert!(r > 0);
    assert!(g > 0);
    assert!(b > 0);
}

#[test]
fn test_ega_mode_0d() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0D); // 320x200 16-color
    vga.framebuffer = vec![0u32; 1];
    vga.fb_width = 1;
    vga.fb_height = 1;
    vga.render(&mut memory);
    let (w, h) = vga.dimensions();
    assert_eq!(w, 320);
    assert_eq!(h, 200);
}

// All bytes are generated from their aperture offsets, independently of guest data.
fn assert_graphics_scroll_down(mode: u8) {
    let (base, aperture_len, row_bytes, bytes_per_cell) = match mode {
        0x04 | 0x05 => (0xB8000, 0x4000, 80, 2),
        0x06 => (0xB8000, 0x4000, 80, 1),
        0x13 => (0xA0000, 0x10000, 320, 8),
        _ => unreachable!(),
    };
    let offset = |y: usize, x: usize| {
        if mode == 0x13 {
            y * row_bytes + x
        } else {
            (y % 2) * 0x2000 + (y / 2) * row_bytes + x
        }
    };
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(mode);
    let partial = || ScrollRegion {
        top: 2,
        left: 3,
        bottom: 7,
        right: 9,
    };
    let clipped = ScrollRegion {
        top: 23,
        left: if mode == 0x06 { 77 } else { 37 },
        bottom: 255,
        right: 255,
    };
    let cases = [
        (1, partial()),
        (2, partial()),
        (0, partial()),
        (6, partial()),
        (255, partial()),
        (1, clipped),
        (
            1,
            ScrollRegion {
                top: 8,
                bottom: 7,
                ..partial()
            },
        ),
        (
            1,
            ScrollRegion {
                left: 10,
                right: 9,
                ..partial()
            },
        ),
        (
            1,
            ScrollRegion {
                top: 25,
                bottom: 255,
                ..partial()
            },
        ),
        (
            1,
            ScrollRegion {
                left: 255,
                right: 255,
                ..partial()
            },
        ),
    ];
    for attr in [0xFC, 0xFD, 0xFE, 0xFF] {
        let fill = match mode {
            0x04 | 0x05 => (attr & 3) * 0x55,
            0x06 => {
                if attr & 1 == 0 {
                    0
                } else {
                    0xFF
                }
            }
            _ => attr,
        };
        for (lines, region) in &cases {
            let lines = *lines;
            let initial: Vec<u8> = (0..aperture_len)
                .map(|i| ((i * 29 + i / row_bytes * 7 + 19) % 256) as u8)
                .collect();
            for (i, byte) in initial.iter().enumerate() {
                memory.write_u8(base + i, *byte);
            }
            memory.write_u8(base - 1, 0x93);
            memory.write_u8(base + aperture_len, 0x6D);
            let mut expected = initial.clone();
            if region.top <= region.bottom && region.left <= region.right {
                let top = usize::from(region.top) * 8;
                let bottom = ((usize::from(region.bottom) + 1) * 8).min(200);
                let left = usize::from(region.left) * bytes_per_cell;
                let right = ((usize::from(region.right) + 1) * bytes_per_cell).min(row_bytes);
                let shift = if lines == 0 {
                    bottom.saturating_sub(top)
                } else {
                    usize::from(lines) * 8
                };
                for y in top..bottom {
                    for x in left..right {
                        expected[offset(y, x)] = if y - top >= shift {
                            initial[offset(y - shift, x)]
                        } else {
                            fill
                        };
                    }
                }
            }
            vga.scroll_down(
                lines,
                attr,
                ScrollRegion {
                    top: region.top,
                    left: region.left,
                    bottom: region.bottom,
                    right: region.right,
                },
                &mut memory,
            );
            for (i, expected_byte) in expected.iter().enumerate() {
                assert_eq!(
                    memory.read_u8(base + i),
                    *expected_byte,
                    "mode={mode:02x} lines={lines} attr={attr:02x} aperture offset={i:#x}"
                );
            }
            assert_eq!(memory.read_u8(base - 1), 0x93);
            assert_eq!(memory.read_u8(base + aperture_len), 0x6D);
        }
    }
}

#[test]
fn test_graphics_scroll_down_mode04() {
    assert_graphics_scroll_down(0x04);
}
#[test]
fn test_graphics_scroll_down_mode05() {
    assert_graphics_scroll_down(0x05);
}
#[test]
fn test_graphics_scroll_down_mode06() {
    assert_graphics_scroll_down(0x06);
}
#[test]
fn test_graphics_scroll_down_mode13() {
    assert_graphics_scroll_down(0x13);
}
