use super::*;

#[test]
fn test_port_write_graphics_controller() {
    let mut vga = Vga::new();
    vga.write_port(0x3CE, 0x05); // Index
    assert_eq!(vga.graphics_index, 0x05);

    vga.write_port(0x3CF, 0x40); // Data - write mode
    assert_eq!(vga.graphics_regs[0x05], 0x40);
    assert!(vga.pipeline_dirty);
}

#[test]
fn test_port_write_crtc() {
    let mut vga = Vga::new();
    vga.write_port(0x3D4, 0x0C); // CRTC index
    assert_eq!(vga.crtc_index, 0x0C);
}

#[test]
fn test_port_write_3c1_read_only() {
    let mut vga = Vga::new();
    vga.attribute_regs[0] = 0xAA;
    vga.write_port(0x3C1, 0x55); // Should be ignored
    assert_eq!(vga.attribute_regs[0], 0xAA);
}

#[test]
fn test_mode_x_detection_via_seq4() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    assert!(!vga.is_mode_x());

    // Clear double-scan first (Mode X requires CRTC reg 9 != Mode 13h default)
    vga.write_port(0x3D4, 0x09);
    vga.write_port(0x3D5, 0x40); // max_scan=0, preserve LC bit 9

    // Disable chain-4 by clearing bit 3 of sequencer reg 4
    vga.write_port(0x3C4, 4); // Index 4
    vga.write_port(0x3C5, 0x06); // bit 3 clear = unchained
    assert!(vga.is_mode_x());

    // Re-enable chain-4
    vga.write_port(0x3C5, 0x0E); // bit 3 set = chain4
    assert!(!vga.is_mode_x());
}

#[test]
fn test_set_cursor_shape_cov() {
    let mut vga = Vga::new();
    vga.set_cursor_shape(13, 14);
    // Cursor shape stored in CRTC registers 0x0A (start) and 0x0B (end)
    assert_eq!(vga.get_crtc_regs()[0x0A] & 0x1F, 13);
    assert_eq!(vga.get_crtc_regs()[0x0B] & 0x1F, 14);
}

#[test]
fn test_set_active_page_cov() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    vga.set_active_page(2);
    assert_eq!(vga.active_page, 2);
}

#[test]
fn test_set_palette_reg_cov() {
    let mut vga = Vga::new();
    vga.set_palette_reg(5, 0x15);
    assert_eq!(vga.attribute_regs[5], 0x15);
}

#[test]
fn test_set_dac_reg_cov() {
    let mut vga = Vga::new();
    vga.set_dac_reg(10, 0x3F, 0x20, 0x00);
    let (r, g, b) = vga.dac[10];
    assert_eq!(r, 0xFF); // 63<<2 | 63>>4
    assert_eq!(g, 0x82); // 32<<2 | 32>>4
    assert_eq!(b, 0);
}

#[test]
fn test_set_dac_raw() {
    let mut vga = Vga::new();
    vga.set_dac_raw(20, 100, 200, 50);
    let (r, g, b) = vga.dac[20];
    assert_eq!(r, 100);
    assert_eq!(g, 200);
    assert_eq!(b, 50);
}

#[test]
fn test_pipeline_state_getters() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x12); // planar 16-color

    assert!(vga.is_planar_16color_mode());
    assert!(!vga.is_256_color_mode());
    assert!(!vga.is_mode_x());
    let _state = vga.build_pipeline_state();
}

#[test]
fn test_font_block_spec_cov2() {
    let mut vga = Vga::new();
    vga.set_font_block_spec(0x12);
    assert_eq!(vga.get_font_block_spec(), 0x12);
}

#[test]
fn test_ril_defaults_cov() {
    let mut vga = Vga::new();
    vga.set_ril_defaults(0, vec![1, 2, 3]);
    assert_eq!(vga.get_ril_defaults(0), Some(&[1u8, 2, 3][..]));
    assert_eq!(vga.get_ril_defaults(1), None);
}

#[test]
fn test_get_set_register_accessors() {
    let mut vga = Vga::new();

    vga.set_sequencer_index(3);
    assert_eq!(vga.get_sequencer_index(), 3);

    vga.set_graphics_index(7);
    assert_eq!(vga.get_graphics_index(), 7);

    vga.set_crtc_index(0x0C);
    assert_eq!(vga.get_crtc_index(), 0x0C);

    vga.set_attribute_index(0x15);
    assert_eq!(vga.get_attribute_index(), 0x15);
}

#[test]
fn test_misc_output_accessor() {
    let mut vga = Vga::new();
    vga.set_misc_output(0x67);
    assert_eq!(vga.get_misc_output(), 0x67);
}

#[test]
fn test_feature_control_accessor() {
    let mut vga = Vga::new();
    vga.set_feature_control(0x03);
    assert_eq!(vga.get_feature_control(), 0x03);
}

#[test]
fn test_dac_accessors() {
    let mut vga = Vga::new();

    vga.set_dac_state(0x03);
    assert_eq!(vga.get_dac_state(), 0x03);

    vga.set_dac_read_index(42);
    assert_eq!(vga.get_dac_read_index(), 42);

    vga.set_dac_write_index(10);
    assert_eq!(vga.get_dac_write_index(), 10);

    vga.set_dac_component(2);
    assert_eq!(vga.get_dac_component(), 2);
}

#[test]
fn test_dac_entries_accessor() {
    let mut vga = Vga::new();
    let mut entries = [(0u8, 0u8, 0u8); 256];
    entries[5] = (10, 20, 30);
    vga.set_dac_entries(&entries);
    assert_eq!(vga.get_dac_entries()[5], (10, 20, 30));
}

#[test]
fn test_attribute_regs_accessor() {
    let mut vga = Vga::new();
    let mut regs = vga.get_attribute_regs();
    regs[0x10] = 0x41;
    vga.set_attribute_regs(regs);
    assert_eq!(vga.get_attribute_regs()[0x10], 0x41);
}

#[test]
fn test_sequencer_regs_accessor() {
    let mut vga = Vga::new();
    let mut regs = vga.get_sequencer_regs();
    regs[2] = 0x0F;
    vga.set_sequencer_regs(regs);
    assert_eq!(vga.get_sequencer_regs()[2], 0x0F);
}

#[test]
fn test_graphics_regs_accessor() {
    let mut vga = Vga::new();
    let mut regs = vga.get_graphics_regs();
    regs[5] = 0x10;
    vga.set_graphics_regs(regs);
    assert_eq!(vga.get_graphics_regs()[5], 0x10);
}

#[test]
fn test_crtc_regs_accessor() {
    let mut vga = Vga::new();
    let mut regs = vga.get_crtc_regs();
    regs[0x12] = 0xDF;
    vga.set_crtc_regs(regs);
    assert_eq!(vga.get_crtc_regs()[0x12], 0xDF);
}

#[test]
fn test_palette_regs_accessor() {
    let mut vga = Vga::new();
    let mut regs = vga.get_palette_regs();
    regs[0] = 0x3F;
    vga.set_palette_regs(regs);
    assert_eq!(vga.get_palette_regs()[0], 0x3F);
}

#[test]
fn test_attribute_flip_flop_accessor() {
    let mut vga = Vga::new();
    assert!(!vga.get_attribute_flip_flop());
    vga.set_attribute_flip_flop(true);
    assert!(vga.get_attribute_flip_flop());
    vga.reset_attribute_flip_flop();
    assert!(!vga.get_attribute_flip_flop());
}

#[test]
fn test_palette_address_source_accessor() {
    let vga = Vga::new();
    assert!(vga.get_palette_address_source()); // Default is true
}

#[test]
fn test_cga_mode_control_accessor() {
    let mut vga = Vga::new();
    vga.set_cga_mode_control(0x1E);
    assert_eq!(vga.get_cga_mode_control(), 0x1E);
}

#[test]
fn test_hw_cursor_pos_cov2() {
    let mut vga = Vga::new();
    vga.set_hw_cursor_pos(10, 20);
    assert_eq!(vga.get_hw_cursor_pos(), (10, 20));
}

#[test]
fn test_hw_cursor_enabled_cov2() {
    let mut vga = Vga::new();
    vga.set_hw_cursor_enabled(false);
    assert!(!vga.is_hw_cursor_enabled());
    vga.set_hw_cursor_enabled(true);
    assert!(vga.is_hw_cursor_enabled());
}

#[test]
fn test_get_scanlines_per_frame_cov() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    let scanlines = vga.get_scanlines_per_frame();
    assert!(scanlines > 0);
}

#[test]
fn test_get_total_scanlines_cov2() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    let total = vga.get_total_scanlines();
    assert!(total > 0);
}

#[test]
fn test_is_in_active_display_and_vblank() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    // After mode set, timing state should be valid
    let _active = vga.is_in_active_display();
    let _vblank = vga.is_in_vblank();
}

#[test]
fn test_text_page_size_words_40col_cov() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x00); // 40x25
    assert_eq!(vga.text_page_size_words(), 0x400);
}

#[test]
fn test_text_page_size_words_80col_cov() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03); // 80x25
    assert_eq!(vga.text_page_size_words(), 0x800);
}

#[test]
fn test_text_page_size_words_mode07() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x07); // Mono 80x25
    assert_eq!(vga.text_page_size_words(), 0x800);
}

#[test]
fn test_render_text_mode03() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);

    // Write some text to video memory
    let base = 0xB8000;
    for i in 0..80u8 {
        memory.write_u8(base + (i as usize) * 2, b'A' + (i % 26));
        memory.write_u8(base + (i as usize) * 2 + 1, 0x07);
    }

    vga.render(&mut memory);
    // Framebuffer should have non-zero pixels
    assert!(vga.framebuffer().iter().any(|&p| p != 0));
}

#[test]
fn test_render_mode13h_cov() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);

    // Write some pixels to video memory
    for i in 0..320 {
        memory.video_memory_mut()[i] = (i % 256) as u8;
    }

    vga.render(&mut memory);
    assert!(vga.framebuffer().iter().any(|&p| p != 0));
}

#[test]
fn test_render_cga_mode04() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);

    // Write some CGA 4-color data to B8000
    for i in 0..80 {
        memory.write_u8(0xB8000 + i, 0xFF);
    }

    vga.render(&mut memory);
    assert!(vga.framebuffer().iter().any(|&p| p != 0));
}

#[test]
fn test_render_cga_mode06() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x06);

    // Write some CGA 2-color data
    for i in 0..80 {
        memory.write_u8(0xB8000 + i, 0xAA);
    }

    vga.render(&mut memory);
    assert!(vga.framebuffer().iter().any(|&p| p != 0));
}

#[test]
fn test_render_ega_mode0d() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0D); // 320x200 16-color

    vga.render(&mut memory);
    // Even with no data, render should complete without panic
    assert!(!vga.framebuffer().is_empty());
}

#[test]
fn test_render_ega_mode10() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x10); // 640x350 16-color

    vga.render(&mut memory);
    assert!(!vga.framebuffer().is_empty());
}

#[test]
fn test_render_vga_mode12() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x12); // 640x480 16-color

    vga.render(&mut memory);
    assert!(!vga.framebuffer().is_empty());
}

#[test]
fn test_render_vga_mode11() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x11); // 640x480 2-color

    vga.render(&mut memory);
    assert!(!vga.framebuffer().is_empty());
}

#[test]
fn test_render_text_mode00_40col() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x00); // 40x25 text

    let base = 0xB8000;
    memory.write_u8(base, b'X');
    memory.write_u8(base + 1, 0x1F);

    vga.render(&mut memory);
    assert!(!vga.framebuffer().is_empty());
}

#[test]
fn test_render_mode_x() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);

    // Enable Mode X
    vga.write_port(0x3C4, 4);
    vga.write_port(0x3C5, 0x06); // Disable chain-4

    vga.render(&mut memory);
    assert!(!vga.framebuffer().is_empty());
}

#[test]
fn test_render_cga_video_output_disabled() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);
    vga.cga_mode_control_reg &= !0x08; // Disable video output

    vga.render(&mut memory);
    // Should blank display
}

#[test]
fn test_render_scanline_mode04() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);

    memory.write_u8(0xB8000, 0xFF);
    vga.latch_frame_start_registers();
    vga.scanline_counter = 0;
    vga.last_rendered_line = -1;
    vga.render_scanline(0, &mut memory);
}

#[test]
fn test_render_scanline_mode06() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x06);

    memory.write_u8(0xB8000, 0xAA);
    vga.latch_frame_start_registers();
    vga.scanline_counter = 0;
    vga.last_rendered_line = -1;
    vga.render_scanline(0, &mut memory);
}

#[test]
fn test_render_scanline_text_mode() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);

    let base = 0xB8000;
    memory.write_u8(base, b'H');
    memory.write_u8(base + 1, 0x07);

    vga.latch_frame_start_registers();
    vga.scanline_counter = 0;
    vga.last_rendered_line = -1;
    vga.render_scanline(0, &mut memory);
}

#[test]
fn test_render_scanline_mode_x() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);
    vga.write_port(0x3C4, 4);
    vga.write_port(0x3C5, 0x06);

    vga.latch_frame_start_registers();
    vga.scanline_counter = 0;
    vga.last_rendered_line = -1;
    vga.render_scanline(0, &mut memory);
}

#[test]
fn test_render_scanline_ega_mono() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0F); // EGA mono

    vga.latch_frame_start_registers();
    vga.scanline_counter = 0;
    vga.last_rendered_line = -1;
    vga.render_scanline(0, &mut memory);
}

#[test]
fn test_render_scanline_vga_mono() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x11); // VGA mono

    vga.latch_frame_start_registers();
    vga.scanline_counter = 0;
    vga.last_rendered_line = -1;
    vga.render_scanline(0, &mut memory);
}

#[test]
fn test_render_scanline_planar_16color() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x12); // 640x480 16-color

    vga.latch_frame_start_registers();
    vga.scanline_counter = 0;
    vga.last_rendered_line = -1;
    vga.render_scanline(0, &mut memory);
}

#[test]
fn test_set_get_cga_color_select() {
    let mut vga = Vga::new();
    vga.set_cga_color_select(0x20);
    assert_eq!(vga.get_cga_color_select(), 0x20);
}

#[test]
fn test_set_crtc_start_words_cov2() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    vga.set_crtc_start_words(0x0800);
    // Verify via CRTC register reads
    let regs = vga.get_crtc_regs();
    assert_eq!(regs[0x0C], 0x08);
    assert_eq!(regs[0x0D], 0x00);
}

#[test]
fn test_set_timing_cycles_per_frame_cov() {
    let mut vga = Vga::new();
    vga.set_timing_cycles_per_frame(100000);
}

#[test]
fn test_sync_bda_state() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);

    // Write BDA values
    memory.write_u8(0x449, 0x03); // Current video mode
    memory.write_u16(0x44A, 80); // Number of columns
    memory.write_u16(0x44C, 0x1000); // Page size
    memory.write_u16(0x44E, 0); // Current page offset
    memory.write_u8(0x484, 24); // Rows - 1
    memory.write_u8(0x462, 0); // Active page

    vga.sync_bda_state(&memory);
}

#[test]
fn test_vga_tick() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    vga.set_timing_cycles_per_frame(100000);

    // Tick enough cycles to trigger a vblank
    vga.tick(100001);
    assert!(vga.take_vblank_pending());
}

#[test]
fn test_diag_display_state_cov() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    let diag = vga.diag_display_state();
    assert!(diag.contains("mode=0x13"));
}

#[test]
fn test_snapshot_stable_registers_cov() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    let snap = vga.snapshot_stable_registers();
    assert_eq!(snap.mode, 0x03);
}

#[test]
fn test_scroll_up_graphics_mode13_cov() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);

    let region = ScrollRegion {
        top: 0,
        bottom: 24,
        left: 0,
        right: 39,
    };
    vga.scroll_up(1, 0x00, region, &mut memory);
}

#[test]
fn test_scroll_down_graphics_mode04() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);

    let region = ScrollRegion {
        top: 0,
        bottom: 24,
        left: 0,
        right: 39,
    };
    vga.scroll_down(1, 0x00, region, &mut memory);
}

const CGA_VRAM_START: usize = 0xB8000;
const CGA_VRAM_SIZE: usize = 0x4000;
const CGA_BYTES_PER_SCANLINE: usize = 80;
const CGA_TEST_SENTINEL: u8 = 0x69;

fn cga_scanline_byte_offset(y: usize, byte_x: usize) -> usize {
    let bank_offset = if y & 1 == 0 { 0 } else { 0x2000 };
    bank_offset + (y / 2) * CGA_BYTES_PER_SCANLINE + byte_x
}

fn initialize_cga_vram(memory: &mut Memory) -> Vec<u8> {
    for offset in 0..CGA_VRAM_SIZE {
        memory.write_u8(CGA_VRAM_START + offset, CGA_TEST_SENTINEL);
    }
    memory.write_u8(CGA_VRAM_START - 1, CGA_TEST_SENTINEL);
    memory.write_u8(CGA_VRAM_START + CGA_VRAM_SIZE, CGA_TEST_SENTINEL);
    vec![CGA_TEST_SENTINEL; CGA_VRAM_SIZE]
}

fn assert_cga_vram_matches(memory: &Memory, expected: &[u8], mode: u8, operation: &str) {
    for (offset, expected_byte) in expected.iter().enumerate() {
        assert_eq!(
            memory.read_u8(CGA_VRAM_START + offset),
            *expected_byte,
            "mode {mode:#04x} {operation}, CGA VRAM offset {offset:#06x}"
        );
    }
    assert_eq!(
        memory.read_u8(CGA_VRAM_START - 1),
        CGA_TEST_SENTINEL,
        "mode {mode:#04x} {operation} changed the byte before CGA VRAM"
    );
    assert_eq!(
        memory.read_u8(CGA_VRAM_START + CGA_VRAM_SIZE),
        CGA_TEST_SENTINEL,
        "mode {mode:#04x} {operation} changed the byte after CGA VRAM"
    );
}

fn assert_cga_graphics_scroll_fill(
    mode: u8,
    attr_cases: &[(u8, u8)],
    columns: usize,
    bytes_per_cell: usize,
) {
    let region = || ScrollRegion {
        top: 1,
        bottom: 2,
        left: 1,
        right: (columns - 2) as u8,
    };
    let first_byte = bytes_per_cell;
    let after_last_byte = (columns - 1) * bytes_per_cell;
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(mode);

    for &(attr, packed_fill) in attr_cases {
        let mut expected = initialize_cga_vram(&mut memory);
        for y in 8..24 {
            for byte_x in first_byte..after_last_byte {
                expected[cga_scanline_byte_offset(y, byte_x)] = packed_fill;
            }
        }
        vga.scroll_up(0, attr, region(), &mut memory);
        assert_cga_vram_matches(&memory, &expected, mode, "up clear");

        let mut expected = initialize_cga_vram(&mut memory);
        for y in 8..24 {
            for byte_x in first_byte..after_last_byte {
                expected[cga_scanline_byte_offset(y, byte_x)] = packed_fill;
            }
        }
        vga.scroll_down(0, attr, region(), &mut memory);
        assert_cga_vram_matches(&memory, &expected, mode, "down clear");

        let mut expected = initialize_cga_vram(&mut memory);
        for y in 0..32 {
            let row_value = 0x20 + y as u8;
            for byte_x in 0..CGA_BYTES_PER_SCANLINE {
                let offset = cga_scanline_byte_offset(y, byte_x);
                expected[offset] = row_value;
                memory.write_u8(CGA_VRAM_START + offset, row_value);
            }
        }
        for y in 8..16 {
            for byte_x in first_byte..after_last_byte {
                expected[cga_scanline_byte_offset(y, byte_x)] = 0x20 + (y + 8) as u8;
            }
        }
        for y in 16..24 {
            for byte_x in first_byte..after_last_byte {
                expected[cga_scanline_byte_offset(y, byte_x)] = packed_fill;
            }
        }
        vga.scroll_up(1, attr, region(), &mut memory);
        assert_cga_vram_matches(&memory, &expected, mode, "up scroll");
    }
}

#[test]
fn test_cga_scroll_fill_honors_attr_mode04() {
    assert_cga_graphics_scroll_fill(
        0x04,
        &[(0xFC, 0x00), (0xFD, 0x55), (0xFE, 0xAA), (0xFF, 0xFF)],
        40,
        2,
    );
}

#[test]
fn test_cga_scroll_fill_honors_attr_mode05() {
    assert_cga_graphics_scroll_fill(
        0x05,
        &[(0xFC, 0x00), (0xFD, 0x55), (0xFE, 0xAA), (0xFF, 0xFF)],
        40,
        2,
    );
}

#[test]
fn test_cga_scroll_fill_honors_attr_mode06() {
    assert_cga_graphics_scroll_fill(0x06, &[(0xA4, 0x00), (0xA5, 0xFF)], 80, 1);
}

#[test]
fn test_teletype_graphics_mode13() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);
    vga.set_cursor(0, 0, 0);

    vga.teletype(b'X', 0, &mut memory);
    let (_, col) = vga.get_cursor(0);
    assert_eq!(col, 1);
}

#[test]
fn test_teletype_graphics_mode_x() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);
    // Enable Mode X
    vga.write_port(0x3C4, 4);
    vga.write_port(0x3C5, 0x06);
    vga.set_cursor(0, 0, 0);

    vga.teletype(b'Y', 0, &mut memory);
}

#[test]
fn test_teletype_graphics_mode12_planar() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x12);
    vga.set_cursor(0, 0, 0);

    vga.teletype(b'Z', 0, &mut memory);
}

#[test]
fn test_teletype_graphics_mode04_cga() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);
    vga.set_cursor(0, 0, 0);

    vga.teletype(b'C', 0, &mut memory);
}

#[test]
fn test_teletype_graphics_mode06_cga() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x06);
    vga.set_cursor(0, 0, 0);

    vga.teletype(b'D', 0, &mut memory);
}

#[test]
fn test_vga_stable_snapshot_describe_changes() {
    let mut vga1 = Vga::new();
    vga1.set_mode_bios(0x03);
    let snap1 = vga1.snapshot_stable_registers();

    let mut vga2 = Vga::new();
    vga2.set_mode_bios(0x13);
    let snap2 = vga2.snapshot_stable_registers();

    let desc = VgaStableSnapshot::describe_changes(&snap1, &snap2);
    assert!(desc.is_some());
}

#[test]
fn test_vga_stable_snapshot_no_changes() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    let snap1 = vga.snapshot_stable_registers();
    let snap2 = vga.snapshot_stable_registers();

    let desc = VgaStableSnapshot::describe_changes(&snap1, &snap2);
    assert!(desc.is_none());
}

#[test]
fn test_ensure_framebuffer_for_mode_cov() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    vga.ensure_framebuffer_for_mode();
    assert_eq!(vga.dimensions(), (320, 200));
}

#[test]
fn test_increment_blink_frame_count_cov() {
    let mut vga = Vga::new();
    vga.increment_blink_frame_count();
}

#[test]
fn test_rebuild_palette_lut_if_needed_calls_rebuild() {
    let mut vga = Vga::new();
    vga.palette16_lut_dirty = true;
    vga.rebuild_palette_lut_if_needed();
    // rebuild_palette_lut_if_needed rebuilds 16-color palette LUT
    assert!(!vga.palette16_lut_dirty);
}

#[test]
fn test_compute_crtc_refresh_all_standard_modes() {
    let mut vga = Vga::new();

    // (mode, expected_millihz, tolerance)
    let modes: [(u8, u32, u32); 12] = [
        (0x00, 70086, 100), // 40x25 text
        (0x01, 70086, 100), // 40x25 text
        (0x02, 70086, 100), // 80x25 text
        (0x03, 70086, 100), // 80x25 text
        (0x04, 70086, 200), // CGA 320x200 (clock/2 rounding)
        (0x06, 70086, 100), // CGA 640x200
        (0x0D, 70086, 200), // EGA 320x200 (clock/2 rounding)
        (0x0E, 70086, 100), // EGA 640x200
        (0x0F, 70086, 100), // EGA 640x350 mono
        (0x10, 70086, 100), // EGA 640x350 16-color
        (0x11, 59940, 100), // VGA 640x480 mono
        (0x12, 59940, 100), // VGA 640x480 16-color
    ];

    for (mode, expected_mhz, tolerance) in modes {
        vga.set_mode_bios(mode);
        let (_, refresh_mhz) = vga.compute_crtc_refresh();
        let diff = (refresh_mhz as i64 - expected_mhz as i64).unsigned_abs() as u32;
        assert!(
            diff <= tolerance,
            "Mode 0x{:02X}: expected {}±{} millihz, got {} (diff={})",
            mode,
            expected_mhz,
            tolerance,
            refresh_mhz,
            diff
        );
    }
}

#[test]
fn test_compute_crtc_refresh_mode_13h() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    let (_, refresh_mhz) = vga.compute_crtc_refresh();
    let diff = (refresh_mhz as i64 - 70086i64).unsigned_abs() as u32;
    assert!(
        diff <= 100,
        "Mode 13h: expected ~70086 millihz, got {}",
        refresh_mhz
    );
}

#[test]
fn test_compute_crtc_refresh_mode_x() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);

    // Standard Mode X CRTC reprogramming: change vtotal to 525 lines
    // Unlock CRTC (clear CR11 bit 7)
    vga.write_port(0x3D4, 0x11);
    vga.write_port(0x3D5, 0x2C);
    // CR06 = 0x0D (vtotal low byte for 525-2=523: low 8 bits = 0x0B)
    vga.write_port(0x3D4, 0x06);
    vga.write_port(0x3D5, 0x0B);
    // CR07 overflow: vtotal bit 8 = 1, bit 9 = 1 → bits 0 and 5 set
    // Keep other overflow bits for VDE etc. Use 0x3E
    vga.write_port(0x3D4, 0x07);
    vga.write_port(0x3D5, 0x3E);

    let (_, refresh_mhz) = vga.compute_crtc_refresh();
    let diff = (refresh_mhz as i64 - 59940i64).unsigned_abs() as u32;
    assert!(
        diff <= 200,
        "Mode X: expected ~59940 millihz, got {}",
        refresh_mhz
    );
}

#[test]
fn test_sr01_bit3_clock_divide() {
    let mut vga = Vga::new();

    // Mode 00h has SR01 bit 3 set (clock divide by 2)
    vga.set_mode_bios(0x00);
    let (_, refresh_with_div) = vga.compute_crtc_refresh();

    // Should be ~70 Hz, NOT ~140 Hz
    assert!(
        refresh_with_div < 85000,
        "Mode 00h refresh {} should be < 85 kHz",
        refresh_with_div
    );
    assert!(
        refresh_with_div > 60000,
        "Mode 00h refresh {} should be > 60 kHz",
        refresh_with_div
    );
}

#[test]
fn test_crtc_write_triggers_recompute() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    vga.set_emulated_ips(25_200_000);

    let original_cpf = vga.timing_cycles_per_frame;

    // Write a different vtotal via port — should trigger recompute
    vga.write_port(0x3D4, 0x06); // Select CR06
    vga.write_port(0x3D5, 0x0B); // vtotal low = 0x0B (different from mode 03h default 0xBF)

    // Timing should have changed (or be pending)
    let new_cpf = vga.timing_cycles_per_frame;
    let pending = vga.pending_timing_cycles_per_frame;
    assert!(
        new_cpf != original_cpf || pending.is_some(),
        "CRTC write should trigger timing recompute"
    );
}

#[test]
fn test_deferred_timing_when_scanline_active() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    vga.set_emulated_ips(25_200_000);

    let original_cpf = vga.timing_cycles_per_frame;

    // Simulate mid-frame: set scanline counter > 0
    vga.scanline_counter = 1;

    // Trigger recompute via IPS change
    vga.set_emulated_ips(50_000_000);

    // Should be deferred, not applied immediately
    assert_eq!(
        vga.timing_cycles_per_frame, original_cpf,
        "Timing should be deferred when scanline_counter > 0"
    );
    assert!(
        vga.pending_timing_cycles_per_frame.is_some(),
        "Pending timing should be set"
    );

    // Clear scanline counter and apply
    vga.scanline_counter = 0;
    if let Some(cpf) = vga.pending_timing_cycles_per_frame.take() {
        vga.apply_timing_cycles_per_frame(cpf);
    }
    assert_ne!(
        vga.timing_cycles_per_frame, original_cpf,
        "Timing should update after applying pending"
    );
}

/// EXP-N007a (M5) regression: with the VBI coalesced, `scanline_counter`
/// stays 0 for the whole vertical blank, so the chain-active FLAG must
/// carry the deferral decision — immediate application under a queued
/// scanline deadline is the black-screen hazard the deferral prevents.
/// Fails without the flag term in `set_timing_cycles_per_frame`.
#[test]
fn timing_update_defers_on_the_chain_flag_even_at_counter_zero() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    vga.set_emulated_ips(25_200_000);
    let original_cpf = vga.timing_cycles_per_frame;

    // Coalesced VBI: chain live, counter still 0.
    vga.scanline_chain_active = true;
    vga.scanline_counter = 0;

    vga.set_emulated_ips(50_000_000);
    assert_eq!(
        vga.timing_cycles_per_frame, original_cpf,
        "timing must defer while the event chain is live"
    );
    assert!(vga.pending_timing_cycles_per_frame.is_some());
}

/// EXP-N007b: port-space sensitivity classification (the beam gate is
/// tested separately below).
#[test]
fn raster_write_sensitivity_classifies_the_vga_window() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    assert!(
        vga.raster_write_is_sensitive(0x3C9),
        "DAC data is live-read"
    );
    assert!(
        vga.raster_write_is_sensitive(0x3C6),
        "pel mask is live-read"
    );
    assert!(
        vga.raster_write_is_sensitive(0x3C0),
        "attribute is live-read"
    );
    assert!(
        !vga.raster_write_is_sensitive(0x3D4),
        "CRTC index write alters no rendered state"
    );
    assert!(
        !vga.raster_write_is_sensitive(0x3C8),
        "DAC write index only"
    );
    // CRTC data: start-address bytes are frame-latched; line compare is
    // live-read.
    vga.crtc_index = 0x0C;
    assert!(!vga.raster_write_is_sensitive(0x3D5));
    vga.crtc_index = 0x0D;
    assert!(!vga.raster_write_is_sensitive(0x3D5));
    vga.crtc_index = 0x18;
    assert!(vga.raster_write_is_sensitive(0x3D5), "line compare is live");
}

/// EXP-N007b: the latch-or-apply beam gate. A sensitive write defers
/// only when the event-grid beam has passed lines the frame has not yet
/// rendered: immediate in vblank (nothing to render — the vblank
/// exemption), immediate within an already-rendered line (one boundary
/// per scanline for bursts), deferred when catch-up would render.
/// Order preservation: anything behind a latched write queues too.
#[test]
fn raster_write_latch_defers_only_when_catchup_would_render() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    // Deterministic synthetic frame: 201 lines, 1000 cycles, retrace 100.
    vga.total_scanlines_cached = 201;
    vga.visible_scanlines_cached = 200;
    vga.timing_cycles_per_frame = 1000;
    vga.timing_retrace_cycles = 100;
    vga.frame_start_cycle = 0;

    // now=50 → grid index 10 → grid elapsed 49 < retrace: vblank.
    vga.last_rendered_line = -1;
    vga.latch_or_apply_raster_write(0x3C9, 0x3F, 50);
    assert!(!vga.has_pending_raster_writes());

    // now=500 → grid elapsed 497 → visible line 88; last rendered 10:
    // catch-up would render 11..=88 → defers.
    vga.last_rendered_line = 10;
    vga.latch_or_apply_raster_write(0x3C9, 0x3F, 500);
    assert!(vga.has_pending_raster_writes());
    // A write behind a latched one queues too, even an insensitive one.
    vga.latch_or_apply_raster_write(0x3D4, 0x0C, 500);
    assert_eq!(vga.raster_write_count, 2);
    vga.raster_write_count = 0;

    // Same line already rendered (88): burst continues immediately.
    vga.last_rendered_line = 88;
    vga.latch_or_apply_raster_write(0x3C9, 0x10, 500);
    assert!(!vga.has_pending_raster_writes());
}

/// EXP-N007b: the service drains the queue in arrival order with the
/// catch-up render first, and clears it.
#[test]
fn service_raster_writes_applies_in_order_and_clears() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);
    vga.total_scanlines_cached = 201;
    vga.visible_scanlines_cached = 200;
    vga.timing_cycles_per_frame = 1000;
    vga.timing_retrace_cycles = 100;
    vga.frame_start_cycle = 0;
    vga.last_rendered_line = 10;

    // Two DAC data writes for palette index 0: red component then green.
    vga.latch_or_apply_raster_write(0x3C8, 0x00, 500); // immediate: index
    vga.latch_or_apply_raster_write(0x3C9, 0x2A, 500); // defers
    vga.latch_or_apply_raster_write(0x3C9, 0x15, 500); // queues behind
    assert_eq!(vga.raster_write_count, 2);

    vga.service_raster_writes(500, &mut memory);
    assert!(!vga.has_pending_raster_writes());
    assert_eq!(
        vga.last_rendered_line, 88,
        "catch-up must have rendered to the grid line before applying"
    );
    vga.write_port(0x3C7, 0x00);
    let r = vga.read_port(0x3C9);
    let g = vga.read_port(0x3C9);
    assert_eq!((r, g), (0x2A, 0x15), "writes applied in arrival order");
}

/// EXP-N007b: the catch-up grid-inverse expression — largest n with
/// cycle(n) <= now — brute-force-pinned (the naive inverse is off-by-one
/// almost everywhere against the floor-form forward expression).
#[test]
fn catch_up_grid_index_matches_brute_force() {
    for &(cpf, total) in &[(1_426_818u64, 449u64), (1000, 201), (999_983, 448)] {
        for elapsed in [0u64, 1, 99, 100, 101, cpf / 4, cpf / 2, cpf - 1] {
            let brute = (0..total)
                .rev()
                .find(|&n| (n * cpf) / total <= elapsed)
                .unwrap();
            let expression = elapsed
                .saturating_add(1)
                .saturating_mul(total)
                .div_ceil(cpf)
                .saturating_sub(1)
                .min(total - 1);
            assert_eq!(
                expression, brute,
                "cpf={cpf} total={total} elapsed={elapsed}"
            );
        }
    }
}

#[test]
fn test_pathological_crtc_values_no_crash() {
    let mut vga = Vga::new();
    vga.set_emulated_ips(25_200_000);

    // All zeros — total_pixels would be 0, should hit fallback
    vga.crtc_regs[0x00] = 0;
    vga.crtc_regs[0x06] = 0;
    vga.crtc_regs[0x07] = 0;
    let (cpf, mhz) = vga.compute_crtc_refresh();
    assert!(cpf > 0, "Zero CRTC regs should produce valid cpf");
    assert!(mhz > 0, "Zero CRTC regs should produce valid millihz");

    // All 0xFF — very large vtotal
    vga.crtc_regs[0x00] = 0xFF;
    vga.crtc_regs[0x06] = 0xFF;
    vga.crtc_regs[0x07] = 0xFF;
    let (cpf2, mhz2) = vga.compute_crtc_refresh();
    assert!(cpf2 > 0, "Max CRTC regs should produce valid cpf");
    assert!(mhz2 >= 50000, "Max CRTC regs should be clamped to >= 50 Hz");
    assert!(mhz2 <= 85000, "Max CRTC regs should be clamped to <= 85 Hz");
}

#[test]
fn test_get_total_scanlines_matches_crtc() {
    let mut vga = Vga::new();

    vga.set_mode_bios(0x03);
    assert_eq!(
        vga.get_total_scanlines(),
        449,
        "Mode 03h should have 449 total scanlines"
    );

    vga.set_mode_bios(0x12);
    assert_eq!(
        vga.get_total_scanlines(),
        525,
        "Mode 12h should have 525 total scanlines"
    );

    vga.set_mode_bios(0x13);
    assert_eq!(
        vga.get_total_scanlines(),
        449,
        "Mode 13h should have 449 total scanlines"
    );

    // 200-line mode
    vga.set_mode_bios(0x04);
    assert_eq!(
        vga.get_total_scanlines(),
        449,
        "Mode 04h should have 449 total scanlines"
    );
}

#[test]
fn test_mode_default_is_text03() {
    // Vga::new() defaults must yield mode() == 0x03 (80x25 colour text)
    // purely from register state — no `set_mode_bios` calls.
    let vga = Vga::new();
    assert_eq!(
        vga.mode(),
        0x03,
        "fresh Vga::new() must report mode 0x03 from registers alone"
    );
}

#[test]
fn test_mode_register_derived_mode13_via_port_sequence() {
    // Program registers in CRTC→SR→GC→AR order
    // for Mode 13h. Do NOT call set_mode_bios — the cache stays at 0x03.
    let mut vga = Vga::new();
    // Mode 13h CRTC defaults
    vga.crtc_regs[0x01] = 0x4F; // HDE = 79 (320 px / 4 ppc in 256-colour)
                                // Sequencer: chain4 enabled
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x0E);
    // Attribute Controller mode register: 256-colour bit (bit 6) + graphics (bit 0)
    vga.write_port(0x3C0, 0x10);
    vga.write_port(0x3C0, 0x41);
    assert_eq!(
        vga.mode(),
        0x13,
        "AR[0x10] bit 6 set must yield mode 0x13 from registers alone"
    );
}

#[test]
fn test_mode_x_register_derived() {
    // Mode X = 256-colour AND chain4 cleared. Both bits are register-only.
    let mut vga = Vga::new();
    // Programme Mode 13h registers via the port path so SR[4] handler runs.
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x0E); // chain4 enabled
    vga.write_port(0x3C0, 0x10);
    vga.write_port(0x3C0, 0x41); // 256-colour
    assert_eq!(vga.mode(), 0x13);
    assert!(!vga.is_mode_x(), "chain4=1 → not Mode X");

    // Now clear chain4 → Mode X. After Stage 3 the result is purely
    // register-derived: `is_256_color_mode() && !is_chain4()` plus the
    // strict CRTC commitment (CR9 max_scan_line=0 or CR17 bit 6) — see
    // Vga::is_mode_x() doc comment for the mode-set transient rationale.
    vga.write_port(0x3D4, 0x09);
    vga.write_port(0x3D5, 0x40); // CR9 := max_scan_line=0
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06); // chain4 cleared, odd/even cleared
    assert_eq!(
        vga.mode(),
        0x13,
        "Mode X still reports 0x13 (Mode X is a sibling of 13h)"
    );
    assert!(
        vga.is_mode_x(),
        "AR[0x10] bit 6 set + chain4 cleared + CR9 max_scan=0 → is_mode_x() derives true"
    );
}

/// Stage 3 acceptance: toggling chain-4 directly on the sequencer register
/// (without going through `write_port`, so no handler/cache update can run)
/// changes `is_mode_x()` immediately. Confirms the predicate is purely
/// register-derived with no hidden cache state.
#[test]
fn test_mode_x_no_cache_pure_register_derivation() {
    let mut vga = Vga::new();
    // Bare-metal register state: 256-colour bit set, CR9 max_scan_line=0
    // (Mode-X CRTC commitment, LC bit 9 preserved — see Vga::is_mode_x()
    // doc comment for the mode-set transient rationale), chain-4
    // enabled (so we start NOT in Mode X).
    vga.attribute_regs[0x10] = 0x41;
    vga.crtc_regs[0x09] = 0x40;
    vga.sequencer_regs[4] = 0x0E;
    assert!(!vga.is_mode_x(), "chain-4 enabled → not Mode X");

    // Toggle chain-4 off via direct register mutation (no port path,
    // no handler runs). `is_mode_x()` must flip on the next call.
    vga.sequencer_regs[4] = 0x06;
    assert!(vga.is_mode_x(), "chain-4 cleared → Mode X (no cache)");

    // Toggle 256-colour off via direct register mutation.
    vga.attribute_regs[0x10] = 0x01;
    assert!(!vga.is_mode_x(), "256-colour cleared → not Mode X");

    // Restore both → Mode X again.
    vga.attribute_regs[0x10] = 0x41;
    assert!(vga.is_mode_x(), "256-colour + chain-4 cleared → Mode X");
}

#[test]
fn test_mode_planar_16color_640x480() {
    // AR[0x10] bit 0 set + CRTC VDE = 479 → mode 0x12 (colour).
    // misc_output defaults to bit 0 = 1 (colour I/O range).
    let mut vga = Vga::new();
    // Set graphics bit, clear 256-colour bit
    vga.attribute_regs[0x10] = 0x01;
    // VDE = 479 (480 visible scanlines)
    set_crtc_vde(&mut vga, 479);
    // HDE = 79 (640 px) - already default
    assert_eq!(vga.mode(), 0x12);

    // Mono disambiguation: misc_output bit 0 = 0 → mono I/O range → 0x11.
    // Note: for 640x480, both 0x11 and 0x12 use misc=0xE3 on real VGA,
    // so this test verifies the fallback behaviour (default to 0x12
    // when bit 0 is set, which covers the real-hardware case).
    vga.misc_output &= !0x01;
    assert_eq!(
        vga.mode(),
        0x11,
        "misc_output bit 0 = 0 disambiguates to mono planar"
    );
}

#[test]
fn test_mode_planar_16color_640x350() {
    // AR[0x10] bit 0 set + CRTC VDE = 349 → mode 0x10 (colour).
    // misc_output defaults to bit 0 = 1 (colour I/O range).
    let mut vga = Vga::new();
    vga.attribute_regs[0x10] = 0x01;
    set_crtc_vde(&mut vga, 349);
    // HDE = 79 default
    assert_eq!(vga.mode(), 0x10);

    // Mono disambiguation: misc_output bit 0 = 0 → mono I/O range → 0x0F.
    // For 640x350, misc_output genuinely differs: 0x0F uses misc=0xA2
    // (bit 0 = 0) and 0x10 uses misc=0xA3 (bit 0 = 1).
    vga.misc_output &= !0x01;
    assert_eq!(
        vga.mode(),
        0x0F,
        "misc_output bit 0 = 0 disambiguates to mono planar"
    );
}

#[test]
fn test_mode_mono_text_via_misc_output() {
    // Default text-mode register state + Misc Output bit 0 == 0 → mode 0x07.
    let mut vga = Vga::new();
    // Defaults already place us in text mode; clear MOR bit 0.
    vga.misc_output &= !0x01;
    assert_eq!(vga.mode(), 0x07);
}

#[test]
fn test_mode_cga_graphics_06() {
    // CGA mode-control bit 1 (graphics) + bit 4 (640) → mode 0x06.
    let mut vga = Vga::new();
    vga.cga_mode_control_reg = 0x1A; // bits 1 + 3 + 4
    vga.attribute_regs[0x10] = 0x01;
    vga.graphics_regs[0x06] = 0x0F;
    assert_eq!(vga.mode(), 0x06);
}

#[test]
fn test_mode_0e_doublescan_not_misclassified_as_10() {
    // Mode 0x0E (640x200, double-scan) and mode 0x10 (640x350) both have
    // HDE=79. Mode 0x0E uses double-scan (CRTC[0x09] bit 7) so its
    // physical VDE is ~399, but logically it's 200 lines. The naive
    // "VDE >= 349 → 0x10" priority would misclassify; mode() checks
    // double-scan first.
    let mut vga = Vga::new();
    vga.attribute_regs[0x10] = 0x01; // graphics
    vga.crtc_regs[0x01] = 0x4F; // HDE = 79
    vga.crtc_regs[0x09] = 0xC0; // double-scan + max scan = 1
    set_crtc_vde(&mut vga, 399);
    assert_eq!(
        vga.mode(),
        0x0E,
        "double-scan VDE=399 must be 0x0E, not 0x10"
    );
}

#[test]
fn test_mode_round_trip_text_bw_variants() {
    // After set_mode_bios(0xN), vga.mode() must return 0xN for all
    // standard text/CGA mode tags. Specifically: modes 0x00 (40-col b/w)
    // and 0x02 (80-col b/w) require the CGA b/w bit be programmed in
    // cga_mode_control_reg, otherwise register-derived mode() can't
    // distinguish them from the colour variants.
    for tag in [0x00u8, 0x01, 0x02, 0x03, 0x07] {
        let mut vga = Vga::new();
        vga.set_mode_bios(tag);
        assert_eq!(vga.mode(), tag, "round-trip failed for mode 0x{:02X}", tag);
    }
}

#[test]
fn test_is_text_mode_register_derived() {
    // Default Vga state is text mode 0x03.
    let mut vga = Vga::new();
    assert!(
        vga.is_text_mode(),
        "fresh Vga should be text mode (AR bit 0 = 0)"
    );

    // Switch to Mode 13h via raw register writes (bypass the cache).
    vga.write_port(0x3C0, 0x10);
    vga.write_port(0x3C0, 0x41);
    assert!(
        !vga.is_text_mode(),
        "256-colour mode must NOT report as text"
    );
}

#[test]
fn test_is_planar_16color_mode_register_derived() {
    // Register-state Mode 0x12: AR[0x10] bit 0 set, bit 6 clear.
    let mut vga = Vga::new();
    vga.attribute_regs[0x10] = 0x01;
    set_crtc_vde(&mut vga, 479);
    assert!(
        vga.is_planar_16color_mode(),
        "AR bit 0 set, bit 6 clear → planar 16-colour"
    );

    // Now flip to 256-colour (bit 6 set): no longer planar 16-colour.
    vga.attribute_regs[0x10] = 0x41;
    assert!(
        !vga.is_planar_16color_mode(),
        "256-colour mode must NOT report as planar 16-colour"
    );
}

/// Helper: set CRTC VDE to the given visible scanline count.
/// VDE is "last visible scanline", so for 200 visible lines, VDE = 199.
fn set_crtc_vde(vga: &mut Vga, vde: u16) {
    vga.crtc_regs[0x12] = (vde & 0xFF) as u8;
    // Overflow bit 1 = VDE bit 8, bit 6 = VDE bit 9
    let overflow = vga.crtc_regs[0x07] & !0x42;
    let bit8 = ((vde >> 8) & 1) as u8;
    let bit9 = ((vde >> 9) & 1) as u8;
    vga.crtc_regs[0x07] = overflow | (bit8 << 1) | (bit9 << 6);
}
