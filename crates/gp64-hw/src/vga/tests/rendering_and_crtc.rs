use super::*;

#[test]
fn test_render_text_blink_forces_background_when_blink_is_off() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    vga.attribute_regs[0x10] |= 0x08; // Enable blink interpretation
                                      // Disable split screen: set line_compare > fb_height (400)
                                      // Default crtc_regs[0x18]=0 gives line_compare=0, activating split screen
    vga.crtc_regs[0x18] = 0xFF;
    vga.crtc_regs[0x07] |= 0x10; // line_compare bit 8
    vga.crtc_regs[0x09] |= 0x40; // line_compare bit 9 → 0x3FF = 1023

    let ch = first_nonzero_row_char();
    memory.write_u8(0xB8000, ch);
    memory.write_u8(0xB8001, 0x8F); // Blink bit set + bright foreground

    vga.palette16_lut_dirty = true;
    vga.rebuild_palette16_lut_if_dirty();
    let bg = vga.palette_to_rgb(0);

    let char_w = vga.fb_width / vga.columns as usize;
    let char_h = vga.fb_height / vga.rows as usize;
    let fb_w = vga.fb_width;

    // Helper: check if any pixel in the first character cell differs from bg
    let cell_has_fg =
        |fb: &[u32]| -> bool { (0..char_h).any(|y| (0..char_w).any(|x| fb[y * fb_w + x] != bg)) };

    vga.blink_frame_count = 8; // Blink OFF phase
    vga.render(&mut memory);
    assert!(!cell_has_fg(&vga.framebuffer));

    vga.blink_frame_count = 0; // Blink ON phase
    vga.render(&mut memory);
    assert!(cell_has_fg(&vga.framebuffer));
}

#[test]
fn test_sync_to_memory_chain4_transition_when_old_mode_is_13h() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x03);
    memory.vga.video_mode = 0x13;
    memory.vga.chain4 = !vga.is_chain4();

    memory.plane_mut(0)[0] = 0x11;
    memory.plane_mut(1)[0] = 0x22;
    memory.plane_mut(2)[0] = 0x33;
    memory.plane_mut(3)[0] = 0x44;

    memory.apply_vga_pipeline(&vga.build_pipeline_state());
    assert_eq!(memory.vga.video_mode, vga.mode());
    assert_eq!(memory.vga.chain4, vga.is_chain4());
}

#[test]
fn test_mode_x_height_update_via_overflow_register_path() {
    // CRTC writes to VDE-affecting indices (0x12, 0x07, 0x09) change the
    // value reported by `get_mode_x_height()`. Writes to unrelated indices
    // do not.
    let mut vga = Vga::new();
    // Programme Mode X via registers so the CRTC handler's `is_mode_x()`
    // gate fires when sizing the framebuffer.
    vga.attribute_regs[0x10] |= 0x40;
    vga.sequencer_regs[4] &= !0x08;
    vga.fb_width = 320;
    vga.fb_height = 200;
    vga.framebuffer = vec![0u32; 320 * 200];

    vga.write_port(0x3D4, 0x12);
    vga.write_port(0x3D5, 0xC7); // VDE low
    let height_after_12 = vga.get_mode_x_height();

    vga.write_port(0x3D4, 0x07);
    vga.write_port(0x3D5, 0x42); // overflow bits for VDE high bits
    assert!(vga.get_mode_x_height() >= height_after_12);

    let before_unrelated = vga.get_mode_x_height();
    vga.write_port(0x3D4, 0x06);
    vga.write_port(0x3D5, 0x00);
    assert_eq!(vga.get_mode_x_height(), before_unrelated);
}

#[test]
fn test_get_font_row_out_of_bounds_falls_back_to_builtin() {
    let vga = Vga::new();
    let memory = Memory::new();
    let row = memory.plane(2).len();
    let val = vga.get_font_row(&memory, b'A', row, 16, 0);
    assert_eq!(val, 0);
}

#[test]
fn test_tick_crossed_vblank_end_with_full_frame_advance() {
    let mut vga = Vga::new();
    vga.set_timing_cycles_per_frame(100);
    vga.tick(100);
    assert!(vga.take_vblank_pending());
}

#[test]
fn test_vga_port_trace_emit_limit_guard_branch() {
    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tmp");
    path.push("test-traces");
    path.push(format!("gp64_vga_ports_limit_{}.log", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let path_text = path.to_string_lossy().into_owned();
    let _guard = EnvGuard::set("vga_port_trace", &path_text);
    set_vga_port_trace_override(Some(true));
    for _ in 0..2105 {
        vga_port_trace_emit("limit-branch");
    }
    set_vga_port_trace_override(None);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_trace_debug_text_sampling_space_and_limit_paths() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    set_vga_trace_enabled_override(Some(true));
    vga.set_mode_bios(0x13);

    for i in 0..80usize {
        let ch = if i == 10 { b' ' } else { b'A' };
        memory.write_u8(0xB8000 + i * 2, ch);
        memory.write_u8(0xB8000 + i * 2 + 1, 0x07);
    }

    for _ in 0..61 {
        vga.render(&mut memory);
    }

    set_vga_trace_enabled_override(None);
}

#[test]
fn test_mode13h_crtc_init_addressing_mode() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    assert_eq!(vga.crtc_regs[0x13], 40, "Mode 13h CRTC offset should be 40");
    assert_eq!(
        vga.crtc_regs[0x14], 0x40,
        "Mode 13h CRTC 0x14 should be 0x40 (DW mode ON)"
    );
    assert_eq!(
        vga.crtc_regs[0x17], 0xA3,
        "Mode 13h CRTC 0x17 should be 0xA3"
    );
}

#[test]
fn test_mode12h_crtc_init_addressing_mode() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x12);
    assert_eq!(vga.crtc_regs[0x13], 40, "Mode 12h CRTC offset should be 40");
    assert_eq!(
        vga.crtc_regs[0x14], 0x00,
        "Mode 12h CRTC 0x14 should be 0x00 (DW mode OFF)"
    );
    assert_eq!(
        vga.crtc_regs[0x17], 0xE3,
        "Mode 12h CRTC 0x17 should be 0xE3 (byte mode ON)"
    );
}

#[test]
fn test_mode03_crtc_init_addressing_mode() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    assert_eq!(vga.crtc_regs[0x13], 40, "Mode 03h CRTC offset should be 40");
    assert_eq!(
        vga.crtc_regs[0x14], 0x1F,
        "Mode 03h CRTC 0x14 should be 0x1F"
    );
    assert_eq!(
        vga.crtc_regs[0x17], 0xA3,
        "Mode 03h CRTC 0x17 should be 0xA3 (word mode)"
    );
}

#[test]
fn test_mode0d_crtc_init_addressing_mode() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x0D);
    assert_eq!(vga.crtc_regs[0x13], 20, "Mode 0Dh CRTC offset should be 20");
    assert_eq!(
        vga.crtc_regs[0x14], 0x00,
        "Mode 0Dh CRTC 0x14 should be 0x00 (DW mode OFF)"
    );
    assert_eq!(
        vga.crtc_regs[0x17], 0xE3,
        "Mode 0Dh CRTC 0x17 should be 0xE3 (byte mode ON)"
    );
}

#[test]
fn test_mode07_crtc_init_addressing_mode() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x07);
    assert_eq!(vga.crtc_regs[0x13], 40, "Mode 07h CRTC offset should be 40");
    assert_eq!(
        vga.crtc_regs[0x14], 0x1F,
        "Mode 07h CRTC 0x14 should be 0x1F"
    );
    assert_eq!(
        vga.crtc_regs[0x17], 0xA3,
        "Mode 07h CRTC 0x17 should be 0xA3 (word mode)"
    );
}

#[test]
fn test_mode00_crtc_init_addressing_mode() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x00);
    assert_eq!(vga.crtc_regs[0x13], 20, "Mode 00h CRTC offset should be 20");
    assert_eq!(
        vga.crtc_regs[0x14], 0x1F,
        "Mode 00h CRTC 0x14 should be 0x1F"
    );
    assert_eq!(
        vga.crtc_regs[0x17], 0xA3,
        "Mode 00h CRTC 0x17 should be 0xA3 (word mode)"
    );
}

#[test]
fn test_compute_bytes_per_row_byte_mode() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x12); // Sets byte mode ON, CRTC 0x13 = 40
                             // Byte mode: K=2, stride = 40 * 2 = 80
    assert_eq!(vga.compute_bytes_per_row(640, false), 80);
}

#[test]
fn test_compute_bytes_per_row_word_mode() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03); // Text mode: word mode
                             // Word mode: K=4, stride = 40 * 4 = 160
    assert_eq!(vga.compute_bytes_per_row(640, false), 160);
}

#[test]
fn test_compute_bytes_per_row_dword_mode() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13); // Mode 13h: DW mode ON
                             // DW mode: K=8, stride = 40 * 8 = 320
    assert_eq!(vga.compute_bytes_per_row(320, false), 320);
}

#[test]
fn test_compute_bytes_per_row_mode_x_after_transition() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    // Simulate Mode X transition: clear DW, set byte
    vga.crtc_regs[0x14] &= !0x40; // DW mode OFF
    vga.crtc_regs[0x17] |= 0x40; // Byte mode ON
                                 // CRTC 0x13 stays at 40 (not reprogrammed in Mode X transition)
                                 // Byte mode: K=2, stride = 40 * 2 = 80
    assert_eq!(vga.compute_bytes_per_row(320, true), 80);
}

#[test]
fn test_tick_basic_advance() {
    let mut vga = Vga::new();
    vga.timing_cycles_per_frame = 1000;
    vga.timing_retrace_cycles = 100;
    vga.cycles_since_vblank = 0;
    vga.tick(50);
    assert_eq!(vga.cycles_since_vblank, 50);
}

#[test]
fn test_tick_wrap_around_frame() {
    let mut vga = Vga::new();
    vga.timing_cycles_per_frame = 1000;
    vga.timing_retrace_cycles = 100;
    vga.cycles_since_vblank = 900;
    // Advancing 200 cycles wraps: (900+200) % 1000 = 100
    vga.tick(200);
    assert_eq!(vga.cycles_since_vblank, 100);
}

#[test]
fn test_tick_large_cycles_multi_frame() {
    let mut vga = Vga::new();
    vga.timing_cycles_per_frame = 1000;
    vga.timing_retrace_cycles = 100;
    vga.cycles_since_vblank = 0;
    // Cycles >= frame_cycles: always crosses vblank end
    vga.tick(2500);
    assert_eq!(vga.cycles_since_vblank, 500);
}

#[test]
fn test_tick_crosses_retrace_no_wrap() {
    let mut vga = Vga::new();
    vga.timing_cycles_per_frame = 1000;
    vga.timing_retrace_cycles = 100;
    vga.cycles_since_vblank = 50;
    // prev=50 < retrace=100, new_cycles=150 >= retrace: crosses vblank end
    vga.tick(100);
    assert_eq!(vga.cycles_since_vblank, 150);
}

#[test]
fn test_tick_no_cross_retrace_no_wrap() {
    let mut vga = Vga::new();
    vga.timing_cycles_per_frame = 1000;
    vga.timing_retrace_cycles = 100;
    vga.cycles_since_vblank = 200;
    // prev=200 >= retrace=100 and new=300 >= retrace: no crossing
    vga.tick(100);
    assert_eq!(vga.cycles_since_vblank, 300);
}

#[test]
fn test_tick_wrap_crosses_retrace() {
    let mut vga = Vga::new();
    vga.timing_cycles_per_frame = 1000;
    vga.timing_retrace_cycles = 100;
    vga.cycles_since_vblank = 950;
    // Wrap: total=950+200=1150, new=150. prev=950 >= retrace || new=150 >= retrace: crosses
    vga.tick(200);
    assert_eq!(vga.cycles_since_vblank, 150);
}

#[test]
fn test_set_timing_cycles_per_frame_zero() {
    let mut vga = Vga::new();
    let original = vga.timing_cycles_per_frame;
    vga.set_timing_cycles_per_frame(0);
    // Zero should be ignored
    assert_eq!(vga.timing_cycles_per_frame, original);
}

#[test]
fn test_set_timing_cycles_per_frame_small() {
    let mut vga = Vga::new();
    vga.set_timing_cycles_per_frame(10);
    assert_eq!(vga.timing_cycles_per_frame, 10);
    // Retrace should be at least 1 but less than frame cycles
    assert!(vga.timing_retrace_cycles >= 1);
    assert!(vga.timing_retrace_cycles < 10);
}

#[test]
fn test_set_timing_cycles_per_frame_large() {
    let mut vga = Vga::new();
    vga.set_timing_cycles_per_frame(100000);
    assert_eq!(vga.timing_cycles_per_frame, 100000);
    // Retrace is CRTC-derived (MDD-BUG-KILN-00025): mode 03h programs
    // vertical blank from line 406 through line 441.
    let expected_retrace = (100000 * 35) / 449;
    assert_eq!(vga.timing_retrace_cycles, expected_retrace);
}

#[test]
fn test_set_timing_cycles_per_frame_resets_cycles() {
    let mut vga = Vga::new();
    vga.timing_cycles_per_frame = 1000;
    vga.cycles_since_vblank = 500;
    vga.set_timing_cycles_per_frame(200);
    // cycles_since_vblank should be wrapped: 500 % 200 = 100
    assert_eq!(vga.cycles_since_vblank, 100);
}

#[test]
fn test_is_planar_16color_mode_true() {
    let mut vga = Vga::new();
    for mode in [0x0D, 0x0E, 0x10, 0x12] {
        vga.set_mode_bios(mode);
        assert!(
            vga.is_planar_16color_mode(),
            "Mode 0x{mode:02X} should be planar"
        );
    }
}

#[test]
fn test_is_planar_16color_mode_false() {
    let mut vga = Vga::new();
    for mode in [0x03, 0x04, 0x06, 0x13] {
        vga.set_mode_bios(mode);
        assert!(
            !vga.is_planar_16color_mode(),
            "Mode 0x{mode:02X} should not be planar"
        );
    }
}

#[test]
fn test_is_chain4_mode13() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    assert!(vga.is_chain4());
}

#[test]
fn test_is_chain4_mode_x() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06); // Clear Chain-4 bit
    assert!(!vga.is_chain4());
}

#[test]
fn test_get_write_mode() {
    let mut vga = Vga::new();
    vga.write_port(0x3CE, 0x05);
    vga.write_port(0x3CF, 0x02); // Write mode 2
    assert_eq!(vga.get_write_mode(), 2);
}

#[test]
fn test_get_bit_mask() {
    let mut vga = Vga::new();
    vga.write_port(0x3CE, 0x08);
    vga.write_port(0x3CF, 0xAA);
    assert_eq!(vga.get_bit_mask(), 0xAA);
}

#[test]
fn test_get_set_reset() {
    let mut vga = Vga::new();
    vga.write_port(0x3CE, 0x00);
    vga.write_port(0x3CF, 0x0F);
    assert_eq!(vga.get_set_reset(), 0x0F);
}

#[test]
fn test_get_enable_set_reset() {
    let mut vga = Vga::new();
    vga.write_port(0x3CE, 0x01);
    vga.write_port(0x3CF, 0x0A);
    assert_eq!(vga.get_enable_set_reset(), 0x0A);
}

#[test]
fn test_get_data_rotate() {
    let mut vga = Vga::new();
    vga.write_port(0x3CE, 0x03);
    vga.write_port(0x3CF, 0x18);
    assert_eq!(vga.get_data_rotate(), 0x18);
}

#[test]
fn test_get_read_mode() {
    let mut vga = Vga::new();
    vga.write_port(0x3CE, 0x05);
    vga.write_port(0x3CF, 0x08); // Read mode 1
    assert_eq!(vga.get_read_mode(), 1);
}

#[test]
fn test_get_color_compare() {
    let mut vga = Vga::new();
    vga.write_port(0x3CE, 0x02);
    vga.write_port(0x3CF, 0x05);
    assert_eq!(vga.get_color_compare(), 0x05);
}

#[test]
fn test_get_color_dont_care() {
    let mut vga = Vga::new();
    vga.write_port(0x3CE, 0x07);
    vga.write_port(0x3CF, 0x03);
    assert_eq!(vga.get_color_dont_care(), 0x03);
}

#[test]
fn test_get_font_row_builtin_8x16_coverage() {
    let vga = Vga::new();
    let memory = Memory::new();
    // Fallback to built-in 8x16 font for char 'A'
    let row = vga.get_font_row(&memory, b'A', 0, 16, 0x07);
    // CP437 'A' has non-zero bits in row 0
    // Just verify it returns some data
    let _ = row; // May be 0 for row 0 of 'A', depends on font
}

#[test]
fn test_get_font_row_builtin_8x8_coverage() {
    let vga = Vga::new();
    let memory = Memory::new();
    // Use 8x8 font (char_height < 16)
    let row = vga.get_font_row(&memory, b'A', 0, 8, 0x07);
    let _ = row;
}

#[test]
fn test_get_font_row_custom_font_in_plane2() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    // Flag must be set for `get_font_row` to read plane 2 — the latch is
    // a one-way flag (set by HLE AH=11h or by observation of a guest
    // plane-2 font upload).
    vga.custom_font_loaded = true;
    // Write custom font data for char 0 into plane 2
    for r in 0..16 {
        memory.plane_mut(2)[r] = 0xFF; // All bits set for every row of char 0
    }
    let row = vga.get_font_row(&memory, 0, 0, 16, 0x07);
    assert_eq!(row, 0xFF);
}

#[test]
fn test_write_port_3d8_switch_to_graphics_640() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x04);
    // bit 1=1 (graphics), bit 4=1 (640 mode) => mode 0x06
    vga.write_port(0x3D8, 0x12);
    assert_eq!(vga.mode(), 0x06);
}

#[test]
fn test_write_port_3d8_switch_to_graphics_bw() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x04);
    // bit 1=1 (graphics), bit 2=1 (b/w), bit 4=0 => mode 0x05
    vga.write_port(0x3D8, 0x06);
    assert_eq!(vga.mode(), 0x05);
}

#[test]
fn test_write_port_3d8_switch_to_graphics_color() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x06);
    // bit 1=1 (graphics), bit 2=0, bit 4=0 => mode 0x04
    vga.write_port(0x3D8, 0x02);
    assert_eq!(vga.mode(), 0x04);
}

#[test]
fn test_write_port_3d8_switch_to_text_80col() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x04);
    // bit 0=1 (80col), bit 1=0 (text) => mode 0x03
    vga.write_port(0x3D8, 0x01);
    assert_eq!(vga.mode(), 0x03);
}

#[test]
fn test_write_port_3d8_switch_to_text_40col() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x04);
    // bit 0=0 (40col), bit 1=0 (text) => mode 0x01
    vga.write_port(0x3D8, 0x00);
    assert_eq!(vga.mode(), 0x01);
}

#[test]
fn test_write_port_3d8_ignored_in_ega_mode() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x12); // EGA/VGA mode
    vga.write_port(0x3D8, 0x02); // Should not switch mode
    assert_eq!(vga.mode(), 0x12);
}

#[test]
fn test_write_port_3d8_does_not_replace_vga_text_timing() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    let crtc_before = vga.get_crtc_regs();

    // VGA does not expose the CGA Mode Control register as a hardware
    // mode-set mechanism. Writing the CGA-compatible display-enable value
    // must preserve the VGA's 80-column mode and CRTC timing registers.
    vga.write_port(0x3D8, 0x08);

    assert_eq!(vga.mode(), 0x03);
    assert_eq!(vga.get_crtc_regs(), crtc_before);
}

#[test]
fn test_write_port_3d8_same_mode_no_switch() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    // 80col text with video enable = mode 0x03 already
    vga.write_port(0x3D8, 0x09);
    assert_eq!(vga.mode(), 0x03);
}

#[test]
fn test_write_port_pel_mask() {
    let mut vga = Vga::new();
    vga.write_port(0x3C6, 0xF0);
    assert_eq!(vga.pel_mask, 0xF0);
    assert!(vga.palette16_lut_dirty);
}

#[test]
fn test_write_port_3c1_read_only() {
    let mut vga = Vga::new();
    let before = vga.attribute_regs[0];
    vga.write_port(0x3C1, 0xFF);
    // Writing to 0x3C1 should be ignored
    assert_eq!(vga.attribute_regs[0], before);
}

#[test]
fn test_write_port_3da_feature_control() {
    let mut vga = Vga::new();
    vga.write_port(0x3DA, 0x42);
    assert_eq!(vga.feature_control, 0x42);
}

#[test]
fn test_write_port_3ba_feature_control() {
    let mut vga = Vga::new();
    vga.write_port(0x3BA, 0x33);
    assert_eq!(vga.feature_control, 0x33);
}

#[test]
fn test_read_port_feature_control() {
    let mut vga = Vga::new();
    vga.feature_control = 0x55;
    assert_eq!(vga.read_port(0x3CA), 0x55);
}

#[test]
fn test_read_port_pel_mask() {
    let mut vga = Vga::new();
    vga.pel_mask = 0xAB;
    assert_eq!(vga.read_port(0x3C6), 0xAB);
}

#[test]
fn test_read_port_dac_state() {
    let mut vga = Vga::new();
    vga.dac_state = 0x03;
    assert_eq!(vga.read_port(0x3C7), 0x03);
}

#[test]
fn test_read_port_dac_write_index() {
    let mut vga = Vga::new();
    vga.write_port(0x3C8, 42);
    assert_eq!(vga.read_port(0x3C8), 42);
}

#[test]
fn test_read_port_cga_mode_control() {
    let mut vga = Vga::new();
    vga.cga_mode_control_reg = 0x2A;
    assert_eq!(vga.read_port(0x3D8), 0x2A);
}

#[test]
fn test_read_port_cga_color_select() {
    let mut vga = Vga::new();
    vga.color_select_reg = 0x35;
    assert_eq!(vga.read_port(0x3D9), 0x35);
}

#[test]
fn test_ril_defaults_set_and_get() {
    let mut vga = Vga::new();
    vga.set_ril_defaults(0, vec![1, 2, 3]);
    let result = vga.get_ril_defaults(0);
    assert_eq!(result, Some([1, 2, 3].as_slice()));
}

#[test]
fn test_ril_defaults_get_unset() {
    let vga = Vga::new();
    assert!(vga.get_ril_defaults(0).is_none());
}

#[test]
fn test_ril_defaults_out_of_bounds() {
    let vga = Vga::new();
    assert!(vga.get_ril_defaults(255).is_none());
}

#[test]
fn test_font_block_spec() {
    let mut vga = Vga::new();
    vga.set_font_block_spec(0x3F);
    assert_eq!(vga.get_font_block_spec(), 0x3F);
}

#[test]
fn test_set_cursor_shape() {
    let mut vga = Vga::new();
    vga.set_cursor_shape(0x0D, 0x0E);
    // Cursor shape is stored in CRTC regs 0x0A and 0x0B
    assert_eq!(vga.crtc_regs[0x0A], 0x0D);
    assert_eq!(vga.crtc_regs[0x0B], 0x0E);
}

#[test]
fn test_hw_cursor_pos() {
    let mut vga = Vga::new();
    vga.set_hw_cursor_pos(100, 200);
    assert_eq!(vga.get_hw_cursor_pos(), (100, 200));
}

#[test]
fn test_hw_cursor_enabled() {
    let mut vga = Vga::new();
    vga.set_hw_cursor_enabled(true);
    assert!(vga.is_hw_cursor_enabled());
    vga.set_hw_cursor_enabled(false);
    assert!(!vga.is_hw_cursor_enabled());
}

#[test]
fn test_read_pixel_cga_mode04_roundtrip() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);
    vga.write_pixel(0, 10, 10, 3, &mut memory);
    let color = vga.read_pixel(0, 10, 10, &memory);
    assert_eq!(color, 3);
}

#[test]
fn test_read_pixel_cga_mode04_oob() {
    let vga = Vga::new();
    let memory = Memory::new();
    // Out of bounds for CGA
    assert_eq!(vga.read_pixel(0, 400, 0, &memory), 0);
    assert_eq!(vga.read_pixel(0, 0, 250, &memory), 0);
}

#[test]
fn test_read_pixel_cga_mode06_roundtrip() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x06);
    vga.write_pixel(0, 100, 50, 1, &mut memory);
    let color = vga.read_pixel(0, 100, 50, &memory);
    assert_eq!(color, 1);
}

#[test]
fn test_read_pixel_cga_mode06_oob() {
    let vga = Vga::new();
    let memory = Memory::new();
    assert_eq!(vga.read_pixel(0, 700, 0, &memory), 0);
    assert_eq!(vga.read_pixel(0, 0, 250, &memory), 0);
}

#[test]
fn test_read_pixel_ega_modes() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0D);
    vga.write_pixel(0, 50, 50, 0x0A, &mut memory);
    let color = vga.read_pixel(0, 50, 50, &memory);
    assert_eq!(color, 0x0A);
}

#[test]
fn test_read_pixel_ega_oob() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x0D);
    let memory = Memory::new();
    assert_eq!(vga.read_pixel(0, 400, 0, &memory), 0);
}

#[test]
fn test_read_pixel_mode_0e() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0E);
    vga.write_pixel(0, 300, 100, 0x05, &mut memory);
    assert_eq!(vga.read_pixel(0, 300, 100, &memory), 0x05);
}

#[test]
fn test_read_pixel_mode_10() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x10);
    vga.write_pixel(0, 500, 200, 0x07, &mut memory);
    assert_eq!(vga.read_pixel(0, 500, 200, &memory), 0x07);
}

#[test]
fn test_read_pixel_mode_12() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x12);
    vga.write_pixel(0, 400, 300, 0x0C, &mut memory);
    assert_eq!(vga.read_pixel(0, 400, 300, &memory), 0x0C);
}

#[test]
fn test_write_pixel_cga_odd_row() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);
    // Write to odd row (y=1) to exercise bank_offset = 0x2000
    vga.write_pixel(0, 0, 1, 2, &mut memory);
    let color = vga.read_pixel(0, 0, 1, &memory);
    assert_eq!(color, 2);
}

#[test]
fn test_write_pixel_cga_mode06_odd_row() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x06);
    vga.write_pixel(0, 0, 1, 1, &mut memory);
    let color = vga.read_pixel(0, 0, 1, &memory);
    assert_eq!(color, 1);
}

#[test]
fn test_write_pixel_mode06_clear_bit() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x06);
    vga.write_pixel(0, 0, 0, 1, &mut memory);
    assert_eq!(vga.read_pixel(0, 0, 0, &memory), 1);
    vga.write_pixel(0, 0, 0, 0, &mut memory);
    assert_eq!(vga.read_pixel(0, 0, 0, &memory), 0);
}

#[test]
fn test_set_crtc_start_words() {
    let mut vga = Vga::new();
    vga.set_crtc_start_words(0x1234);
    assert_eq!(vga.crtc_regs[0x0C], 0x12);
    assert_eq!(vga.crtc_regs[0x0D], 0x34);
}

#[test]
fn test_sync_to_memory() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    memory.apply_vga_pipeline(&vga.build_pipeline_state());
    // Verify memory state is synced
    assert_eq!(memory.read_u8(0x449), 0x03); // Video mode
}

#[test]
fn test_render_text_cursor_disabled_coverage() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    // Disable cursor via CRTC reg 0x0A bit 5
    vga.crtc_regs[0x0A] = 0x20;
    vga.render(&mut memory); // Should not crash with cursor disabled
}

#[test]
fn test_render_text_cursor_end_lt_start_coverage() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    // Set cursor start > end (invalid shape, cursor not drawn)
    vga.crtc_regs[0x0A] = 0x0F; // Start = 15
    vga.crtc_regs[0x0B] = 0x01; // End = 1
    vga.render(&mut memory);
}

#[test]
fn test_mode_x_crtc_vde_update() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    // Disable chain-4 (mode_x deferred until CRTC reg 9 changes)
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06);
    assert!(!vga.is_mode_x()); // Not yet — CRTC reg 9 still has Mode 13h default
                               // Disable CRTC write protect (CR11 bit 7) so overflow register is writable
    vga.write_port(0x3D4, 0x11);
    vga.write_port(0x3D5, 0x0E);
    // Clear double-scan: set max_scan_line=0 (keep LC bit 9)
    // This triggers deferred Mode X activation.
    vga.write_port(0x3D4, 0x09);
    vga.write_port(0x3D5, 0x40);
    assert!(vga.is_mode_x());

    // Update VDE via CRTC register 0x12 (low byte of VDE)
    vga.write_port(0x3D4, 0x12);
    vga.write_port(0x3D5, 0xDF); // VDE low = 223
                                 // Overflow register sets bits 8/9
    vga.write_port(0x3D4, 0x07);
    vga.write_port(0x3D5, 0x00); // No overflow bits
                                 // VDE = 223, height = 224
    assert_eq!(vga.get_mode_x_height(), 224);
    assert_eq!(vga.fb_height, 224);
}

#[test]
fn test_is_256_color_mode() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    assert!(vga.is_256_color_mode());
    vga.set_mode_bios(0x03);
    assert!(!vga.is_256_color_mode());
}

#[test]
fn test_get_byte_panning_coverage() {
    let mut vga = Vga::new();
    vga.crtc_regs[0x08] = 0x20; // Byte panning = bits 6:5 of CRTC reg 0x08 = 01
    assert_eq!(vga.get_byte_panning(), 1);
}

#[test]
fn test_get_pixel_panning_value() {
    let mut vga = Vga::new();
    let mut regs = vga.get_attribute_regs();
    regs[0x13] = 0x03;
    vga.set_attribute_regs(regs);
    assert_eq!(vga.get_pixel_panning(), 0x03);
}

#[test]
fn test_get_vertical_display_end() {
    let mut vga = Vga::new();
    vga.crtc_regs[0x12] = 0xDF; // VDE low byte = 223
    vga.crtc_regs[0x07] = 0x02; // Bit 1 = 1 (VDE bit 8)
    let vde = vga.get_vertical_display_end();
    assert_eq!(vde, 0xDF | (1 << 8)); // 479
}

#[test]
fn test_set_attribute_regs_marks_dirty() {
    let mut vga = Vga::new();
    vga.rebuild_palette16_lut_if_dirty();
    assert!(!vga.palette16_lut_dirty);
    let regs = vga.get_attribute_regs();
    vga.set_attribute_regs(regs);
    assert!(vga.palette16_lut_dirty);
}

#[test]
fn test_set_sequencer_regs() {
    let mut vga = Vga::new();
    let mut regs = vga.get_sequencer_regs();
    regs[2] = 0x0F;
    vga.set_sequencer_regs(regs);
    assert_eq!(vga.sequencer_regs[2], 0x0F);
}

#[test]
fn test_set_graphics_regs() {
    let mut vga = Vga::new();
    let mut regs = vga.get_graphics_regs();
    regs[5] = 0x40;
    vga.set_graphics_regs(regs);
    assert_eq!(vga.graphics_regs[5], 0x40);
}

#[test]
fn test_get_set_misc_output() {
    let mut vga = Vga::new();
    vga.set_misc_output(0x63);
    assert_eq!(vga.get_misc_output(), 0x63);
}

#[test]
fn test_get_set_feature_control() {
    let mut vga = Vga::new();
    vga.set_feature_control(0x55);
    assert_eq!(vga.get_feature_control(), 0x55);
}

#[test]
fn test_get_set_dac_state() {
    let mut vga = Vga::new();
    vga.set_dac_state(0x03);
    assert_eq!(vga.get_dac_state(), 0x03);
}

#[test]
fn test_get_set_dac_read_index() {
    let mut vga = Vga::new();
    vga.set_dac_read_index(100);
    assert_eq!(vga.get_dac_read_index(), 100);
}

#[test]
fn test_get_set_dac_write_index() {
    let mut vga = Vga::new();
    vga.set_dac_write_index(200);
    assert_eq!(vga.get_dac_write_index(), 200);
}

#[test]
fn test_get_set_dac_component() {
    let mut vga = Vga::new();
    vga.set_dac_component(2);
    assert_eq!(vga.get_dac_component(), 2);
}

#[test]
fn test_get_set_dac_entries() {
    let mut vga = Vga::new();
    let mut entries = *vga.get_dac_entries();
    entries[0] = (255, 128, 64);
    vga.set_dac_entries(&entries);
    assert_eq!(vga.dac[0], (255, 128, 64));
}

#[test]
fn test_get_set_cga_mode_control() {
    let mut vga = Vga::new();
    vga.set_cga_mode_control(0x2A);
    assert_eq!(vga.get_cga_mode_control(), 0x2A);
}

#[test]
fn test_get_set_sequencer_index() {
    let mut vga = Vga::new();
    vga.set_sequencer_index(5);
    assert_eq!(vga.get_sequencer_index(), 5);
}

#[test]
fn test_get_set_graphics_index() {
    let mut vga = Vga::new();
    vga.set_graphics_index(8);
    assert_eq!(vga.get_graphics_index(), 8);
}

#[test]
fn test_get_set_crtc_index() {
    let mut vga = Vga::new();
    vga.set_crtc_index(0x1F);
    assert_eq!(vga.get_crtc_index(), 0x1F);
}

#[test]
fn test_get_set_attribute_index() {
    let mut vga = Vga::new();
    vga.set_attribute_index(0x12);
    assert_eq!(vga.get_attribute_index(), 0x12);
}

#[test]
fn test_reset_attribute_flip_flop() {
    let mut vga = Vga::new();
    vga.attribute_flip_flop = true;
    vga.reset_attribute_flip_flop();
    assert!(!vga.attribute_flip_flop);
}

#[test]
fn test_get_set_palette_regs() {
    let mut vga = Vga::new();
    let mut regs = vga.get_palette_regs();
    regs[0] = 0x3F;
    vga.set_palette_regs(regs);
    assert_eq!(vga.attribute_regs[0], 0x3F);
    assert!(vga.palette16_lut_dirty);
}

#[test]
fn test_get_scanlines_per_frame() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    let scanlines = vga.get_scanlines_per_frame();
    assert!(scanlines > 0);
}

#[test]
fn test_get_total_scanlines() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    let total = vga.get_total_scanlines();
    assert!(total > 0);
}

#[test]
fn test_is_in_active_display_coverage() {
    let mut vga = Vga::new();
    vga.timing_cycles_per_frame = 1000;
    vga.timing_retrace_cycles = 100;
    // In active display (after retrace)
    vga.cycles_since_vblank = 500;
    assert!(vga.is_in_active_display());
}

#[test]
fn test_is_in_vblank_coverage() {
    let mut vga = Vga::new();
    vga.timing_cycles_per_frame = 1000;
    vga.timing_retrace_cycles = 100;
    // In vblank (before retrace cycles end)
    vga.cycles_since_vblank = 50;
    assert!(vga.is_in_vblank());
}

fn setup_text_split_case(mode: u8) -> (Vga, Memory, u32, u32) {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(mode);

    let aperture_base = if mode == 0x07 { 0xB0000 } else { 0xB8000 };
    let start_word = 0x0800;
    let selected_page_base = aperture_base + start_word * 2;

    // Give aperture zero and the selected display page visibly different
    // backgrounds, independent of the mode's default palette setup.
    vga.attribute_regs[1] = 1;
    vga.attribute_regs[2] = 2;
    vga.attribute_regs[0x14] = 0;
    vga.set_dac_reg(1, 63, 0, 0);
    vga.set_dac_reg(2, 0, 63, 0);
    vga.rebuild_palette16_lut_if_dirty();
    let aperture_rgb = vga.palette16_lut[1];
    let selected_page_rgb = vga.palette16_lut[2];
    assert_ne!(aperture_rgb, selected_page_rgb);

    for col in 0..vga.columns as usize {
        memory.write_u8(aperture_base + col * 2, b' ');
        memory.write_u8(aperture_base + col * 2 + 1, 0x10);
        memory.write_u8(selected_page_base + col * 2, b' ');
        memory.write_u8(selected_page_base + col * 2 + 1, 0x20);
    }

    let mut crtc = vga.get_crtc_regs();
    crtc[0x18] = 16;
    crtc[0x07] &= !0x10;
    crtc[0x09] &= !0x40;
    crtc[0x0C] = (start_word >> 8) as u8;
    crtc[0x0D] = start_word as u8;
    vga.set_crtc_regs(crtc);
    vga.latch_frame_start_registers();

    (vga, memory, aperture_rgb, selected_page_rgb)
}

#[test]
fn test_render_text_split_uses_aperture_zero_for_color_text() {
    let (mut vga, mut memory, aperture_rgb, selected_page_rgb) = setup_text_split_case(0x03);

    vga.render(&mut memory);

    assert_eq!(
        vga.framebuffer[0], selected_page_rgb,
        "selected page above split"
    );
    assert_eq!(
        vga.framebuffer[16 * vga.fb_width],
        aperture_rgb,
        "aperture zero below split"
    );
}

#[test]
fn test_render_text_split_uses_aperture_zero_for_mono_text() {
    let (mut vga, mut memory, aperture_rgb, selected_page_rgb) = setup_text_split_case(0x07);

    vga.render(&mut memory);

    assert_eq!(
        vga.framebuffer[0], selected_page_rgb,
        "selected page above split"
    );
    assert_eq!(
        vga.framebuffer[16 * vga.fb_width],
        aperture_rgb,
        "aperture zero below split"
    );
}

#[test]
fn test_render_text_scanline_split_uses_aperture_zero_for_color_text() {
    let (mut vga, mut memory, aperture_rgb, selected_page_rgb) = setup_text_split_case(0x03);

    vga.render_scanline(0, &mut memory);
    vga.render_scanline(16, &mut memory);

    assert_eq!(
        vga.framebuffer[0], selected_page_rgb,
        "selected page above split"
    );
    assert_eq!(
        vga.framebuffer[16 * vga.fb_width],
        aperture_rgb,
        "aperture zero below split"
    );
}

#[test]
fn test_render_text_scanline_split_uses_aperture_zero_for_mono_text() {
    let (mut vga, mut memory, aperture_rgb, selected_page_rgb) = setup_text_split_case(0x07);

    vga.render_scanline(0, &mut memory);
    vga.render_scanline(16, &mut memory);

    assert_eq!(
        vga.framebuffer[0], selected_page_rgb,
        "selected page above split"
    );
    assert_eq!(
        vga.framebuffer[16 * vga.fb_width],
        aperture_rgb,
        "aperture zero below split"
    );
}

#[test]
fn test_render_mode13_split_screen() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x13);

    // Set split screen line at y=100
    let mut regs = vga.get_crtc_regs();
    regs[0x18] = 100;
    regs[0x07] &= !(1 << 4); // Clear VDE bit 8
    regs[0x09] &= !(1 << 6); // Clear VDE bit 9
                             // Set start address so top half shows different data
    regs[0x0C] = 0x00;
    regs[0x0D] = 0x10; // Start at word 16 (offset 64)
    vga.set_crtc_regs(regs);

    memory.video_memory_mut()[64] = 5; // First pixel after start offset
    memory.video_memory_mut()[0] = 10; // First pixel in split region (address 0)

    vga.render(&mut memory);
    // Verify split screen renders pixel from address 0 at scanline >= 100
    let split_pixel = vga.framebuffer[100 * 320];
    assert_eq!(split_pixel, vga.palette_lut[10]);
}

#[test]
fn test_render_mode13_vde_blanks_partial() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x13);
    vga.set_dac_reg(1, 63, 0, 0);
    let mut attrs = vga.get_attribute_regs();
    attrs[0x11] = 1; // Overscan color index
    vga.set_attribute_regs(attrs);

    // Set VDE to 49 (50 visible lines)
    let mut crtc = vga.get_crtc_regs();
    crtc[0x12] = 49;
    crtc[0x07] &= !(1 << 1);
    crtc[0x07] &= !(1 << 6);
    vga.set_crtc_regs(crtc);

    memory.video_memory_mut().fill(2);
    vga.render(&mut memory);

    // Line 50+ should be overscan
    let overscan = vga.overscan_rgb();
    assert_eq!(vga.framebuffer[60 * 320], overscan);
}

#[test]
fn test_render_cga_disabled_modes_04_to_06() {
    let _env = OverrideGuard::render_mode(None);
    for (mode, disabled_control) in [(0x04, 0x02), (0x05, 0x06), (0x06, 0x12)] {
        let mut vga = Vga::new();
        let mut memory = Memory::new();
        vga.set_mode_bios(mode);
        // Disable CGA video output (bit 3 = 0), preserving mode-select bits.
        vga.cga_mode_control_reg = disabled_control;
        vga.palette_address_source = true;
        vga.attribute_regs[0x11] = 5;
        vga.set_dac_reg(5, 63, 0, 0);

        memory.write_u8(0xB8000, 0xFF);
        vga.render(&mut memory);

        let overscan = vga.overscan_rgb();
        assert!(
            vga.framebuffer().iter().all(|&pixel| pixel == overscan),
            "mode {mode:02X} should blank to overscan"
        );
    }
}

#[test]
fn test_compute_bytes_per_row_zero_offset_mode_x() {
    let mut vga = Vga::new();
    vga.crtc_regs[0x13] = 0; // Zero CRTC offset
    assert_eq!(vga.compute_bytes_per_row(320, true), 320 / 4);
}

#[test]
fn test_compute_bytes_per_row_zero_offset_normal() {
    let mut vga = Vga::new();
    vga.crtc_regs[0x13] = 0; // Zero CRTC offset
    assert_eq!(vga.compute_bytes_per_row(640, false), 640 / 8);
}

#[test]
fn test_vga_trace_enabled_override() {
    set_vga_trace_enabled_override(Some(true));
    assert!(vga_trace_enabled());
    set_vga_trace_enabled_override(Some(false));
    assert!(!vga_trace_enabled());
    set_vga_trace_enabled_override(None);
}

#[test]
fn test_get_current_scanline() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    vga.timing_cycles_per_frame = 10000;
    vga.timing_retrace_cycles = 100;
    vga.cycles_since_vblank = 5000;
    let scanline = vga.get_current_scanline();
    assert!(scanline > 0);
}

#[test]
fn test_mode_07_text() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x07);
    assert!(vga.is_text_mode());
    assert!(vga.is_mono_text_mode());
    vga.render(&mut memory);
}

#[test]
fn test_text_page_base_mono() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x07);
    let base = vga.text_page_base(0);
    assert_eq!(base, 0xB0000);
}

#[test]
fn test_text_page_base_color() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    let base = vga.text_page_base(0);
    assert_eq!(base, 0xB8000);
}

#[test]
fn test_write_port_3c7_sets_dac_read_mode() {
    let mut vga = Vga::new();
    vga.write_port(0x3C7, 42);
    assert_eq!(vga.dac_read_index, 42);
    assert_eq!(vga.dac_component, 0);
    assert_eq!(vga.dac_state, 0x03);
}

#[test]
fn test_write_port_3c8_sets_dac_write_mode() {
    let mut vga = Vga::new();
    vga.write_port(0x3C8, 100);
    assert_eq!(vga.dac_write_index, 100);
    assert_eq!(vga.dac_component, 0);
    assert_eq!(vga.dac_state, 0);
}

#[test]
fn test_dac_write_auto_increment() {
    let mut vga = Vga::new();
    vga.write_port(0x3C8, 0); // Start at index 0
                              // Write first entry (R, G, B)
    vga.write_port(0x3C9, 63); // R
    vga.write_port(0x3C9, 32); // G
    vga.write_port(0x3C9, 16); // B
                               // Auto-increments to index 1
    assert_eq!(vga.dac_write_index, 1);
    // Write second entry
    vga.write_port(0x3C9, 10);
    vga.write_port(0x3C9, 20);
    vga.write_port(0x3C9, 30);
    assert_eq!(vga.dac_write_index, 2);
}

#[test]
fn test_attr_palette_write_marks_dirty() {
    let mut vga = Vga::new();
    vga.rebuild_palette16_lut_if_dirty();
    assert!(!vga.palette16_lut_dirty);

    // Write to palette register 0 via attribute controller
    vga.read_port(0x3DA); // Reset flip-flop
    vga.write_port(0x3C0, 0x00); // Index 0
    vga.write_port(0x3C0, 0x3F); // Data
    assert!(vga.palette16_lut_dirty);
}

#[test]
fn test_crtc_mono_port_read_write() {
    let mut vga = Vga::new();
    vga.write_port(0x3B4, 0x0C);
    assert_eq!(vga.read_port(0x3B4), 0x0C);
    vga.write_port(0x3B5, 0x42);
    assert_eq!(vga.read_port(0x3B5), 0x42);
}

#[test]
fn test_crtc_protect_blocks_writes_to_00_through_06() {
    let mut vga = Vga::new();

    for idx in 0x00..=0x06 {
        vga.write_port(0x3D4, idx);
        vga.write_port(0x3D5, 0x12);
    }

    // Enable CRTC write protection (CR11 bit 7).
    vga.write_port(0x3D4, 0x11);
    vga.write_port(0x3D5, 0x80);

    for idx in 0x00..=0x06 {
        vga.write_port(0x3D4, idx);
        vga.write_port(0x3D5, 0xA5);
    }

    for idx in 0x00..=0x06 {
        vga.write_port(0x3D4, idx);
        assert_eq!(
            vga.read_port(0x3D5),
            0x12,
            "CRTC register 0x{idx:02X} should be protected"
        );
    }
}
