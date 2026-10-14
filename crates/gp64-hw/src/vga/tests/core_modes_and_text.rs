use super::*;

#[test]
fn test_vga_new() {
    let vga = Vga::new();
    assert_eq!(vga.mode(), 0x03);
    assert_eq!(vga.columns, 80);
    assert_eq!(vga.rows, 25);
    assert_eq!(vga.active_page, 0);
}

#[test]
fn test_vga_default() {
    let vga = Vga::default();
    assert_eq!(vga.mode(), 0x03);
}

#[test]
fn test_set_mode_bios_vga_mode_trace_enabled() {
    set_vga_mode_trace_override(Some(true));
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    assert_eq!(vga.mode(), 0x13);
    set_vga_mode_trace_override(None);
}

#[test]
fn test_vga_mode_trace_enabled_override() {
    set_vga_mode_trace_override(Some(true));
    assert!(vga_mode_trace_enabled());
    set_vga_mode_trace_override(Some(false));
    assert!(!vga_mode_trace_enabled());
    set_vga_mode_trace_override(None);
}

#[test]
fn test_vga_port_trace_enabled_override() {
    set_vga_port_trace_override(Some(true));
    assert!(vga_port_trace_enabled());
    set_vga_port_trace_override(Some(false));
    assert!(!vga_port_trace_enabled());
    set_vga_port_trace_override(None);
}

#[test]
fn test_trace_path_from_env_variants() {
    assert_eq!(
        trace_path_from_env(None, "tmp/default.log"),
        std::path::PathBuf::from("tmp/default.log")
    );
    assert_eq!(
        trace_path_from_env(Some(String::new()), "tmp/default.log"),
        std::path::PathBuf::from("tmp/default.log")
    );
    assert_eq!(
        trace_path_from_env(Some("   ".to_string()), "tmp/default.log"),
        std::path::PathBuf::from("tmp/default.log")
    );
    assert_eq!(
        trace_path_from_env(Some("1".to_string()), "tmp/default.log"),
        std::path::PathBuf::from("tmp/default.log")
    );
    assert_eq!(
        trace_path_from_env(
            Some(" logs/custom_trace.log ".to_string()),
            "tmp/default.log"
        ),
        std::path::PathBuf::from("logs/custom_trace.log")
    );
}

#[test]
fn test_render_force_render_mode_hex() {
    let _env = OverrideGuard::render_mode(Some("0x13"));
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);

    memory.video_memory_mut()[0] = 0x03;
    vga.render(&mut memory);
    assert_eq!(vga.framebuffer()[0], vga.palette_to_rgb(0x03));
}

#[test]
fn test_render_force_render_mode_decimal() {
    let _env = OverrideGuard::render_mode(Some("19"));
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);

    memory.video_memory_mut()[0] = 0x05;
    vga.render(&mut memory);
    assert_eq!(vga.framebuffer()[0], vga.palette_to_rgb(0x05));
}

#[test]
fn test_render_mode_x_forced_mode_skips_mode_x() {
    let _env = OverrideGuard::render_mode(Some("0x13"));
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);

    vga.write_port(0x3D4, 0x09);
    vga.write_port(0x3D5, 0x40);
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06);
    assert!(vga.is_mode_x());

    vga.render(&mut memory);
    assert_eq!(vga.fb_width, 320);
}

#[test]
fn test_render_vga_text_ignores_cga_display_disable_when_pas_enabled() {
    let _env = OverrideGuard::render_mode(None);
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x03);
    vga.cga_mode_control_reg = 0x00;
    vga.palette_address_source = true;

    memory.write_u8(0xB8000, 0xDB);
    memory.write_u8(0xB8001, 0x0F);
    vga.render(&mut memory);
    let overscan = vga.overscan_rgb();
    assert!(vga.framebuffer().iter().any(|&pixel| pixel != overscan));
}

/// MDD-BUG-CRUCIBLE-00560: PAS is the display-enable gate in every mode, so
/// a guest clearing it in text mode blanks to overscan exactly as it does in
/// graphics mode — and re-enabling it brings the text straight back. Driven
/// through port 0x3C0 rather than by poking the field, so the test also
/// covers the index-write path that carries the bit.
#[test]
fn test_render_vga_text_blanks_to_overscan_while_pas_is_clear() {
    let _env = OverrideGuard::render_mode(None);
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x03);
    vga.cga_mode_control_reg = 0x09;
    vga.attribute_regs[0x11] = 5;
    vga.set_dac_reg(5, 63, 0, 0);

    memory.write_u8(0xB8000, 0xDB);
    memory.write_u8(0xB8001, 0x0F);

    // Reset the attribute flip-flop to the index phase, then write an index
    // with bit 5 clear: PAS off.
    vga.read_port(0x3DA);
    vga.write_port(0x3C0, 0x00);
    assert!(!vga.palette_address_source, "index write must clear PAS");

    vga.render(&mut memory);
    let overscan = vga.overscan_rgb();
    assert!(
        vga.framebuffer().iter().all(|&pixel| pixel == overscan),
        "PAS clear must blank the whole text frame to overscan"
    );

    // Bit 5 set: display re-enabled, the glyph is visible again.
    vga.read_port(0x3DA);
    vga.write_port(0x3C0, 0x20);
    assert!(vga.palette_address_source, "index write must set PAS");

    vga.render(&mut memory);
    assert!(
        vga.framebuffer().iter().any(|&pixel| pixel != overscan),
        "text must be visible again once PAS is re-enabled"
    );
}

#[test]
fn test_render_mode_13h_exercise() {
    // Exercise VGA rendering in mode 13h with various memory patterns
    // vga_trace_enabled() now uses tracing::enabled!() instead of env vars
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x13);
    memory.video_memory_mut()[0] = 1;
    memory.video_memory_mut()[1] = 64;
    memory.video_memory_mut()[2] = 100;
    for i in 0..52usize {
        memory.write_u8(0xB8000 + i * 2, b'A');
    }
    memory.write_u8(0xB8000 + 2 * 10, b' ');
    memory.write_u8(0xB8000 + 1, 1);
    memory.write_u8(0x90000 + 100, 1);
    memory.plane_mut(0)[0] = 1;
    memory.plane_mut(1)[0] = 1;
    memory.plane_mut(2)[0] = 1;
    memory.plane_mut(3)[0] = 1;

    vga.render(&mut memory);
    assert_eq!(vga.framebuffer()[0], vga.palette_to_rgb(1));
    for _ in 0..60 {
        vga.render(&mut memory);
    }
}

#[test]
fn test_render_planar_mode_12h() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x12);
    for plane in 0..4 {
        memory.plane_mut(plane).fill(0);
    }

    vga.render(&mut memory);
    assert_eq!(vga.framebuffer()[0], vga.palette_to_rgb(0));
    vga.render(&mut memory);
}

#[test]
fn test_write_port_attribute_controller() {
    let mut vga = Vga::new();
    vga.write_port(0x3C0, 0x20);
    assert!(vga.palette_address_source);
}

#[test]
fn test_set_mode_bios_text() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);
    assert_eq!(vga.mode(), 0x03);
    assert_eq!(vga.columns, 80);
    assert_eq!(vga.rows, 25);
}

#[test]
fn test_set_mode_bios_40x25() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x01);
    assert_eq!(vga.mode(), 0x01);
    assert_eq!(vga.columns, 40);
    assert_eq!(vga.rows, 25);
}

#[test]
fn test_set_mode_bios_mode13h() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    assert_eq!(vga.mode(), 0x13);
    assert_eq!(vga.fb_width, 320);
    assert_eq!(vga.fb_height, 200);
    assert!(!vga.is_text_mode());
}

#[test]
fn test_mode13_default_line_compare_disabled() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    assert_eq!(vga.get_line_compare(), 0x3FF);
}

#[test]
fn test_set_mode_bios_enables_pas() {
    // Test that BIOS mode set enables PAS (Palette Address Source) for display
    let mut vga = Vga::new();

    // First, clear PAS via attribute controller write
    vga.read_port(0x3DA); // Reset flip-flop
    vga.write_port(0x3C0, 0x00); // Index 0, PAS=0 (display blanked)
    assert!(!vga.palette_address_source, "PAS should be cleared");

    // Now set a video mode - this should re-enable PAS
    vga.set_mode_bios(0x13);
    assert!(
        vga.palette_address_source,
        "BIOS mode set should enable PAS (display)"
    );

    // Repeat for text mode
    vga.read_port(0x3DA);
    vga.write_port(0x3C0, 0x00); // Clear PAS again
    assert!(!vga.palette_address_source, "PAS should be cleared");

    vga.set_mode_bios(0x03);
    assert!(
        vga.palette_address_source,
        "BIOS mode set to text mode should enable PAS (display)"
    );
}

#[test]
fn test_get_pixel_panning_masks_high_bits() {
    let mut vga = Vga::new();
    let mut regs = vga.get_attribute_regs();
    regs[0x13] = 0xAF;
    vga.set_attribute_regs(regs);
    assert_eq!(vga.get_pixel_panning(), 0x0F);
}

#[test]
fn test_get_line_compare_assembles_bits() {
    let mut vga = Vga::new();
    let mut regs = vga.get_crtc_regs();
    regs[0x18] = 0x55;
    regs[0x07] |= 1 << 4;
    regs[0x09] |= 1 << 6;
    vga.set_crtc_regs(regs);
    assert_eq!(vga.get_line_compare(), 0x355);
}

#[test]
fn test_mode13_line_compare_zero_ignores_start_offset() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);

    let mut regs = vga.get_crtc_regs();
    regs[0x0C] = 0x00;
    regs[0x0D] = 0x01; // start_words = 1 -> start_offset = 4
    regs[0x18] = 0x00;
    regs[0x07] &= !(1 << 4);
    regs[0x09] &= !(1 << 6);
    vga.set_crtc_regs(regs);

    let video_mem = memory.video_memory_mut();
    video_mem.fill(0);
    video_mem[4] = 0x02;

    vga.render(&mut memory);
    assert_eq!(vga.framebuffer[0], vga.palette_to_rgb(0));
}

#[test]
fn test_mode13_line_compare_disabled_uses_start_offset() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);

    let mut regs = vga.get_crtc_regs();
    regs[0x0C] = 0x00;
    regs[0x0D] = 0x01; // start_words = 1 -> start_offset = 4
    regs[0x18] = 0xFF;
    regs[0x07] |= 1 << 4;
    regs[0x09] |= 1 << 6;
    vga.set_crtc_regs(regs);

    let video_mem = memory.video_memory_mut();
    video_mem.fill(0);
    video_mem[4] = 0x02;

    vga.render(&mut memory);
    assert_eq!(vga.framebuffer[0], vga.palette_to_rgb(2));
}

#[test]
fn test_mode13_vde_blanks_beyond_visible_lines() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);

    vga.set_dac_reg(1, 63, 0, 0);
    vga.set_dac_reg(2, 0, 0, 63);
    let mut attrs = vga.get_attribute_regs();
    attrs[0x11] = 1;
    vga.set_attribute_regs(attrs);

    let mut crtc = vga.get_crtc_regs();
    crtc[0x12] = 99; // 100 visible lines
    crtc[0x07] &= !(1 << 1);
    crtc[0x07] &= !(1 << 6);
    vga.set_crtc_regs(crtc);

    memory.video_memory_mut().fill(2);

    vga.render(&mut memory);
    let overscan = vga.overscan_rgb();
    assert_eq!(vga.framebuffer[0], vga.palette_lut[2]);
    assert_eq!(vga.framebuffer[150 * 320], overscan);
}

#[test]
fn test_mode13_split_oob_start_offset() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);

    let mut regs = vga.get_crtc_regs();
    regs[0x0C] = 0xFF;
    regs[0x0D] = 0xFF;
    regs[0x18] = 0x01;
    regs[0x07] &= !(1 << 4);
    regs[0x09] &= !(1 << 6);
    vga.set_crtc_regs(regs);

    vga.render(&mut memory);
    assert_eq!(vga.framebuffer[0], vga.palette_lut[0]);
}

#[test]
fn test_mode_x_line_compare_disabled_uses_start_offset() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);

    vga.write_port(0x3D4, 0x09); // CRTC index: max scan line
    vga.write_port(0x3D5, 0x40); // Clear double-scan (Mode X setup)

    // Enable Mode X (unchained).
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06);

    let mut regs = vga.get_crtc_regs();
    regs[0x0C] = 0x00;
    regs[0x0D] = 0x01; // start_word = 1 -> start_offset = 1 (graphics)
    regs[0x18] = 0xFF;
    regs[0x07] |= 1 << 4;
    regs[0x09] |= 1 << 6;
    vga.set_crtc_regs(regs);

    vga.set_dac_reg(5, 10, 20, 30);
    for plane in 0..4 {
        memory.plane_mut(plane).fill(0);
    }
    memory.plane_mut(0)[1] = 5;

    let to_8bit = |val: u8| (val & 0x3F) << 2 | (val & 0x3F) >> 4;
    let expected =
        ((to_8bit(10) as u32) << 16) | ((to_8bit(20) as u32) << 8) | (to_8bit(30) as u32);

    vga.render(&mut memory);
    assert_eq!(vga.framebuffer[0], expected);
}

#[test]
fn test_sync_bda_state_updates_cga_regs() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    memory.write_u8(0x449, 0x04);
    memory.write_u8(0x44A, 40);
    memory.write_u8(0x484, 24);
    memory.write_u8(0x462, 0);
    memory.write_u8(0x465, 0x2A);
    memory.write_u8(0x466, 0x35);

    vga.sync_bda_state(&memory);

    assert_eq!(vga.get_cga_mode_control(), 0x2A);
    assert_eq!(vga.get_cga_color_select(), 0x35);
}

#[test]
fn test_sync_bda_state_text_mode_zero_cga_control_uses_bios_default() {
    let _env = OverrideGuard::render_mode(None);
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x03);
    memory.write_u8(0x449, 0x03);
    memory.write_u8(0x44A, 80);
    memory.write_u8(0x484, 24);
    memory.write_u8(0x462, 0);
    memory.write_u8(0x465, 0x00);
    memory.write_u8(0x466, 0x00);
    memory.write_u8(0xB8000, 0xDB);
    memory.write_u8(0xB8001, 0x0F);

    vga.sync_bda_state(&memory);

    assert_eq!(vga.get_cga_mode_control(), 0x09);
    vga.render(&mut memory);
    let overscan = vga.overscan_rgb();
    assert!(vga.framebuffer().iter().any(|&pixel| pixel != overscan));
}

#[test]
fn test_sync_bda_state_graphics_mode_zero_cga_control_stays_disabled() {
    let _env = OverrideGuard::render_mode(None);
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x04);
    vga.attribute_regs[0x11] = 5;
    vga.set_dac_reg(5, 63, 0, 0);
    memory.write_u8(0x449, 0x04);
    memory.write_u8(0x44A, 40);
    memory.write_u8(0x484, 24);
    memory.write_u8(0x462, 0);
    memory.write_u8(0x465, 0x00);
    memory.write_u8(0x466, 0x00);
    memory.write_u8(0xB8000, 0xFF);
    memory.write_u8(0xB8001, 0xFF);

    vga.sync_bda_state(&memory);

    assert_eq!(vga.get_cga_mode_control(), 0x02);
    assert_eq!(vga.mode(), 0x04);
    vga.render(&mut memory);
    let overscan = vga.overscan_rgb();
    assert_ne!(overscan, 0);
    assert!(vga.framebuffer().iter().all(|&pixel| pixel == overscan));
}

#[test]
fn test_sync_bda_state_mode_x_height_from_crtc() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    // Programme registers so `is_mode_x()` is register-true:
    //   chain-4 disabled (SR[4] bit 3 clear) AND 256-colour (AR[0x10] bit 6 set).
    vga.sequencer_regs[4] &= !0x08;
    vga.attribute_regs[0x10] |= 0x40;
    let mut regs = vga.get_crtc_regs();
    // VDE = 0x10 | bit 8 | bit 9 = 784. With max_scan=0 → visible_height=785,
    // clamped to 480 by `get_mode_x_height()`.
    regs[0x12] = 0x10;
    regs[0x07] |= (1 << 1) | (1 << 6);
    regs[0x09] = 0x40; // Clear max_scan_line (keep LC bit 9)
    vga.set_crtc_regs(regs);

    memory.write_u8(0x449, 0x13);
    memory.write_u8(0x44A, 40);
    memory.write_u8(0x484, 24);
    memory.write_u8(0x462, 0);

    vga.sync_bda_state(&memory);

    assert!(vga.is_mode_x());
    // With the cache deleted, height is clamped to 480 via `mode_x_visible_height().min(480)`.
    assert_eq!(vga.get_mode_x_height(), 480);
    assert_eq!(vga.fb_height, vga.get_mode_x_height());
}

#[test]
fn test_sync_bda_state_high_res_mode() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    memory.write_u8(0x449, 0x12);
    memory.write_u8(0x44A, 80);
    memory.write_u8(0x484, 59);
    memory.write_u8(0x462, 0);

    vga.sync_bda_state(&memory);

    // mode() is register-derived; sync_bda_state doesn't program VGA
    // registers, so we verify framebuffer dimensions only.
    assert_eq!(vga.fb_width, 640);
    assert_eq!(vga.fb_height, 480);
    assert_eq!(vga.framebuffer.len(), 640 * 480);
}

#[test]
fn test_sync_bda_state_zero_columns_keeps_default() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    memory.write_u8(0x449, 0x03);
    memory.write_u8(0x44A, 0);
    memory.write_u8(0x484, 9);
    memory.write_u8(0x462, 0);

    vga.sync_bda_state(&memory);

    assert_eq!(vga.columns, 80);
    assert_eq!(vga.rows, 10);
}

#[test]
fn test_sync_bda_state_mode13_chain4_keeps_mode_x_false() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.sequencer_regs[4] |= 0x08;
    memory.write_u8(0x449, 0x13);
    memory.write_u8(0x44A, 40);
    memory.write_u8(0x484, 24);
    memory.write_u8(0x462, 0);

    vga.sync_bda_state(&memory);

    assert!(!vga.is_mode_x());
    assert_eq!(vga.fb_height, 200);
}

#[test]
fn test_set_mode_cga_graphics() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x04);
    assert_eq!(vga.mode(), 0x04);
    assert_eq!(vga.fb_width, 320);
    assert_eq!(vga.fb_height, 200);
}

#[test]
fn test_set_mode_cga_hires() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x06);
    assert_eq!(vga.mode(), 0x06);
    assert_eq!(vga.fb_width, 640);
    assert_eq!(vga.fb_height, 200);
}

#[test]
fn test_set_mode_ega_vga_16color() {
    let mut vga = Vga::new();

    vga.set_mode_bios(0x0D);
    assert_eq!(vga.mode(), 0x0D);
    assert_eq!(vga.fb_width, 320);

    vga.set_mode_bios(0x0E);
    assert_eq!(vga.mode(), 0x0E);
    assert_eq!(vga.fb_width, 640);

    vga.set_mode_bios(0x10);
    assert_eq!(vga.mode(), 0x10);
    assert_eq!(vga.fb_height, 350);

    vga.set_mode_bios(0x12);
    assert_eq!(vga.mode(), 0x12);
    assert_eq!(vga.fb_height, 480);
}

#[test]
fn test_set_mode_mono_modes() {
    let mut vga = Vga::new();

    vga.set_mode_bios(0x0F);
    assert_eq!(vga.mode(), 0x0F);
    assert_eq!(vga.fb_height, 350);
    assert_eq!(vga.sequencer_regs[2], 0x05);
    assert_ne!(vga.attribute_regs[0x10] & 0x01, 0);

    vga.set_mode_bios(0x11);
    // Mode 0x11 and 0x12 both set misc_output = 0xE3 (bit 0 = 1),
    // making them register-indistinguishable. mode() defaults to 0x12
    // (the colour variant) which is the safe choice — we deliberately
    // avoid using SR[2] for disambiguation because apps in mode 0x12
    // routinely mask planes during writes.
    assert_eq!(
        vga.mode(),
        0x12,
        "0x11 and 0x12 are register-indistinguishable; defaults to 0x12"
    );
    assert_eq!(vga.fb_height, 480);
    assert_eq!(vga.sequencer_regs[2], 0x01);
    assert_ne!(vga.attribute_regs[0x10] & 0x01, 0);
}

#[test]
fn test_get_mode() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    let (mode, cols, page) = vga.get_mode();
    assert_eq!(mode, 0x13);
    assert_eq!(cols, 40);
    assert_eq!(page, 0);
}

#[test]
fn test_cursor_position() {
    let mut vga = Vga::new();
    vga.set_cursor(0, 10, 20);
    let (row, col) = vga.get_cursor(0);
    assert_eq!(row, 10);
    assert_eq!(col, 20);
}

#[test]
fn test_cursor_position_multiple_pages() {
    let mut vga = Vga::new();
    vga.set_cursor(0, 5, 10);
    vga.set_cursor(1, 12, 25);
    vga.set_cursor(2, 0, 79);

    assert_eq!(vga.get_cursor(0), (5, 10));
    assert_eq!(vga.get_cursor(1), (12, 25));
    assert_eq!(vga.get_cursor(2), (0, 79));
}

#[test]
fn test_cursor_invalid_page() {
    let mut vga = Vga::new();
    vga.set_cursor(0, 7, 19);
    assert_eq!(
        vga.get_cursor(8),
        (7, 19),
        "the first invalid BIOS page must fall back to page zero"
    );
    assert_eq!(vga.get_cursor(255), (7, 19));
}

#[test]
fn test_set_active_page() {
    let mut vga = Vga::new();
    vga.set_active_page(2);
    let (_, _, page) = vga.get_mode();
    assert_eq!(page, 2);
}

#[test]
fn test_set_active_page_graphics_mode_skips_crtc_update() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    vga.crtc_regs[0x0C] = 0xAA;
    vga.crtc_regs[0x0D] = 0xBB;

    vga.set_active_page(1);
    assert_eq!(vga.crtc_regs[0x0C], 0xAA);
    assert_eq!(vga.crtc_regs[0x0D], 0xBB);
}

#[test]
fn test_set_cursor_graphics_mode_skips_hw_cursor() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    vga.crtc_regs[0x0E] = 0x11;
    vga.crtc_regs[0x0F] = 0x22;

    vga.set_cursor(0, 5, 10);
    assert_eq!(vga.cursor_pos[0], (5, 10));
    assert_eq!(vga.crtc_regs[0x0E], 0x11);
    assert_eq!(vga.crtc_regs[0x0F], 0x22);
}

#[test]
fn test_set_palette_reg() {
    let mut vga = Vga::new();
    vga.set_palette_reg(0, 0x3F);
}

#[test]
fn test_set_dac_reg() {
    let mut vga = Vga::new();
    vga.set_dac_reg(0, 63, 32, 16);
}

#[test]
fn test_palette_to_rgb_color_select_pas0() {
    let mut vga = Vga::new();
    let mut regs = vga.get_attribute_regs();
    regs[0] = 0x2A;
    regs[0x10] &= !0x80;
    regs[0x14] = 0x0C;
    vga.set_attribute_regs(regs);

    vga.set_dac_reg(0x2A, 10, 11, 12);
    vga.set_dac_reg(0xEA, 1, 2, 3);

    // Rebuild the 16-color palette LUT before testing
    vga.rebuild_palette16_lut_if_dirty();

    let to_8bit = |val: u8| (val & 0x3F) << 2 | (val & 0x3F) >> 4;
    let expected = ((to_8bit(1) as u32) << 16) | ((to_8bit(2) as u32) << 8) | (to_8bit(3) as u32);

    assert_eq!(vga.palette_to_rgb(0), expected);
}

#[test]
fn pas0_color_select_uses_bits_2_3_for_all_64_color_pages() {
    let mut vga = Vga::new();
    let mut regs = vga.get_attribute_regs();
    regs[0] = 0x2A;
    regs[0x10] &= !0x80;
    vga.set_attribute_regs(regs);

    let page_colors = [(1, 2, 3), (4, 5, 6), (7, 8, 9), (10, 11, 12)];
    for (page, &(r, g, b)) in page_colors.iter().enumerate() {
        vga.set_dac_reg((0x2A + page * 0x40) as u16, r, g, b);
    }

    let to_8bit = |val: u8| (val & 0x3F) << 2 | (val & 0x3F) >> 4;
    for (page, &(r, g, b)) in page_colors.iter().enumerate() {
        for low_bits in 0..4 {
            let mut regs = vga.get_attribute_regs();
            regs[0x14] = ((page as u8) << 2) | low_bits;
            vga.set_attribute_regs(regs);
            vga.rebuild_palette16_lut_if_dirty();

            let expected = (u32::from(to_8bit(r)) << 16)
                | (u32::from(to_8bit(g)) << 8)
                | u32::from(to_8bit(b));
            assert_eq!(
                vga.palette_to_rgb(0),
                expected,
                "PAS=0 DAC page {page} must ignore AR14 low bits {low_bits}"
            );
        }
    }
}

#[test]
fn test_palette_to_rgb_color_select_pas1() {
    let mut vga = Vga::new();
    let to_8bit = |val: u8| (val & 0x3F) << 2 | (val & 0x3F) >> 4;
    for page in 0..16u8 {
        let mut regs = vga.get_attribute_regs();
        regs[0] = 0x0B;
        regs[0x10] |= 0x80;
        regs[0x14] = page;
        vga.set_attribute_regs(regs);

        let r = page * 3 + 1;
        let g = page * 3 + 2;
        let b = page * 3 + 3;
        vga.set_dac_reg(u16::from((page << 4) | 0x0B), r, g, b);
        vga.rebuild_palette16_lut_if_dirty();

        let expected =
            (u32::from(to_8bit(r)) << 16) | (u32::from(to_8bit(g)) << 8) | u32::from(to_8bit(b));
        assert_eq!(
            vga.palette_to_rgb(0),
            expected,
            "PAS=1 DAC page {page} must use the full AR14 nibble"
        );
    }
}

#[test]
fn test_cga_color_select() {
    let mut vga = Vga::new();
    vga.set_cga_color_select(0x3F);
    assert_eq!(vga.get_cga_color_select(), 0x3F);
}

#[test]
fn test_is_text_mode() {
    let mut vga = Vga::new();

    vga.set_mode_bios(0x03);
    assert!(vga.is_text_mode());

    vga.set_mode_bios(0x13);
    assert!(!vga.is_text_mode());
}

#[test]
fn test_text_memory_operations() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 0, 0);
    vga.write_char_attr(0, b'A', 0x07, 1, &mut memory);
    let (ch, attr) = vga.read_char_attr(0, &memory);
    assert_eq!(ch, b'A');
    assert_eq!(attr, 0x07);
}

#[test]
fn test_write_char_only() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 0, 0);
    vga.write_char_attr(0, b'X', 0x70, 1, &mut memory);
    vga.write_char(0, b'Y', 1, &mut memory);

    let (ch, attr) = vga.read_char_attr(0, &memory);
    assert_eq!(ch, b'Y');
    assert_eq!(attr, 0x70);
}

#[test]
fn test_scroll_up() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 1, 0);
    vga.write_char_attr(0, b'A', 0x07, 1, &mut memory);

    let region = ScrollRegion {
        top: 0,
        left: 0,
        bottom: 24,
        right: 79,
    };
    vga.scroll_up(1, 0x07, region, &mut memory);

    vga.set_cursor(0, 0, 0);
    let (ch, _) = vga.read_char_attr(0, &memory);
    assert_eq!(ch, b'A');
}

#[test]
fn test_scroll_down() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 0, 0);
    vga.write_char_attr(0, b'B', 0x07, 1, &mut memory);

    let region = ScrollRegion {
        top: 0,
        left: 0,
        bottom: 24,
        right: 79,
    };
    vga.scroll_down(1, 0x07, region, &mut memory);

    vga.set_cursor(0, 1, 0);
    let (ch, _) = vga.read_char_attr(0, &memory);
    assert_eq!(ch, b'B');
}

#[test]
fn test_teletype_basic() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 0, 0);
    vga.teletype(b'X', 0, &mut memory);

    let (row, col) = vga.get_cursor(0);
    assert_eq!(row, 0);
    assert_eq!(col, 1);
}

#[test]
fn test_teletype_sets_default_attr() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 0, 0);
    vga.teletype(b'X', 0, &mut memory);

    let addr = 0xB8000;
    assert_eq!(memory.read_u8(addr), b'X');
    assert_eq!(memory.read_u8(addr + 1), 0x07);
}

#[test]
fn test_teletype_preserves_existing_attr() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 0, 0);
    let addr = 0xB8000;
    memory.write_u8(addr + 1, 0x1E);

    vga.teletype(b'Y', 0, &mut memory);
    assert_eq!(memory.read_u8(addr + 1), 0x1E);
}

#[test]
fn test_teletype_newline() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 5, 10);

    vga.teletype(0x0D, 0, &mut memory);
    let (row, col) = vga.get_cursor(0);
    assert_eq!(row, 5);
    assert_eq!(col, 0);

    vga.teletype(0x0A, 0, &mut memory);
    let (row, col) = vga.get_cursor(0);
    assert_eq!(row, 6);
    assert_eq!(col, 0);
}

#[test]
fn teletype_survives_an_off_screen_cursor_from_int10_ah02() {
    // AH=0Eh control codes and an ordinary character, each from the
    // maximum out-of-range cursor AH=02h will happily store.
    for ch in [0x0Au8, 0x09, b'A', 0x0D, 0x08] {
        let mut vga = Vga::new();
        let mut memory = Memory::new();
        vga.set_mode_bios(0x03);

        // INT 10h AH=02h with DH=DL=0xFF. This is stored verbatim, as a
        // real BIOS does — the raw value must survive for AH=03h.
        vga.set_cursor(0, 0xFF, 0xFF);
        assert_eq!(
            vga.get_cursor(0),
            (0xFF, 0xFF),
            "AH=02h must store the raw off-screen pair for AH=03h to read back"
        );

        // INT 10h AH=0Eh. Must not unwind.
        vga.teletype(ch, 0, &mut memory);

        // Deterministic, in-range cursor afterwards.
        let (row, col) = vga.get_cursor(0);
        assert!(row < 25, "char {ch:#04X}: row {row} left off-screen");
        assert!(col < 80, "char {ch:#04X}: col {col} left off-screen");

        // Nothing written outside page 0.
        let stray = strays_outside_page0(&memory);
        assert!(
            stray.is_empty(),
            "char {ch:#04X}: wrote outside the active page at {:X?}",
            &stray[..stray.len().min(4)]
        );
    }
}

#[test]
fn write_char_survives_an_off_screen_cursor() {
    // The same trust boundary reached through AH=09h/0Ah instead of 0Eh.
    //
    // col MUST be in range here: `write_char_attr`'s inner loop breaks on
    // `c >= columns`, so an off-screen COLUMN makes it write nothing and
    // the test would pass for the wrong reason. The off-screen ROW is the
    // uncovered half — nothing bounds it before the address is formed.
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 0xFF, 0);

    vga.write_char_attr(0, b'Z', 0x07, 8, &mut memory);
    vga.write_char(0, b'Y', 8, &mut memory);

    let stray = strays_outside_page0(&memory);
    assert!(
        stray.is_empty(),
        "wrote outside the active page at {:X?}",
        &stray[..stray.len().min(4)]
    );
}

#[test]
fn an_in_range_cursor_is_untouched_by_containment() {
    // The clamp must not perturb ordinary output.
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 5, 10);

    vga.teletype(b'Q', 0, &mut memory);

    assert_eq!(vga.get_cursor(0), (5, 11));
    let addr = 0xB8000 + (5 * 80 + 10) * 2;
    assert_eq!(memory.read_u8(addr), b'Q');
}

#[test]
fn test_teletype_backspace() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 0, 10);
    vga.teletype(0x08, 0, &mut memory);
    let (_, col) = vga.get_cursor(0);
    assert_eq!(col, 9);
}

#[test]
fn test_write_pixel_mode13h() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x13);
    vga.write_pixel(0, 100, 50, 15, &mut memory);
    let color = vga.read_pixel(0, 100, 50, &memory);
    assert_eq!(color, 15);
}

#[test]
fn test_dimensions() {
    let mut vga = Vga::new();

    vga.set_mode_bios(0x13);
    let (w, h) = vga.dimensions();
    assert_eq!(w, 320);
    assert_eq!(h, 200);

    vga.set_mode_bios(0x12);
    let (w, h) = vga.dimensions();
    assert_eq!(w, 640);
    assert_eq!(h, 480);
}

#[test]
fn test_framebuffer() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    assert_eq!(vga.framebuffer().len(), 320 * 200);
}

#[test]
fn test_port_io_dac() {
    let mut vga = Vga::new();
    vga.write_port(0x3C8, 0);
    vga.write_port(0x3C9, 63);
    vga.write_port(0x3C9, 32);
    vga.write_port(0x3C9, 16);

    vga.write_port(0x3C7, 0);
    let r = vga.read_port(0x3C9);
    let g = vga.read_port(0x3C9);
    let b = vga.read_port(0x3C9);

    assert_eq!(r, 63);
    assert_eq!(g, 32);
    assert_eq!(b, 16);
}

#[test]
fn test_port_io_sequencer() {
    let mut vga = Vga::new();
    // Sequencer index register - write and read back
    vga.write_port(0x3C4, 0x02); // Select Map Mask register
    assert_eq!(vga.read_port(0x3C4), 0x02);

    // Sequencer data register - write and read back
    vga.write_port(0x3C5, 0x0F); // Enable all planes
    assert_eq!(vga.read_port(0x3C5), 0x0F);

    // Test Mode X detection via sequencer register 4
    vga.set_mode_bios(0x13); // Need to be in mode 13h for Mode X
    vga.write_port(0x3D4, 0x09); // CRTC index: max scan line
    vga.write_port(0x3D5, 0x40); // Clear double-scan (Mode X setup)
    vga.write_port(0x3C4, 0x04); // Select Memory Mode register
    vga.write_port(0x3C5, 0x06); // Clear Chain-4 bit (bit 3)
    assert!(vga.is_mode_x());
    assert_eq!(vga.read_port(0x3C5), 0x06); // Verify read-back

    // Disable Mode X
    vga.write_port(0x3C5, 0x0E); // Set Chain-4 bit
    assert!(!vga.is_mode_x());
    assert_eq!(vga.read_port(0x3C5), 0x0E); // Verify read-back
}

#[test]
fn test_port_io_graphics() {
    let mut vga = Vga::new();
    // Graphics index register - write and read back
    vga.write_port(0x3CE, 0x05); // Select Mode register
    assert_eq!(vga.read_port(0x3CE), 0x05);

    // Graphics data register - write and read back
    vga.write_port(0x3CF, 0x40); // Set write mode
    assert_eq!(vga.read_port(0x3CF), 0x40);
}

#[test]
fn test_port_io_sequencer_out_of_range() {
    let mut vga = Vga::new();
    // Out of range sequencer index should return 0
    vga.write_port(0x3C4, 0xFF); // Out of range index
    assert_eq!(vga.read_port(0x3C5), 0); // Should return 0 for invalid index
}

#[test]
fn test_port_io_graphics_out_of_range() {
    let mut vga = Vga::new();
    // Out of range graphics index should return 0
    vga.write_port(0x3CE, 0xFF); // Out of range index
    assert_eq!(vga.read_port(0x3CF), 0); // Should return 0 for invalid index
}

#[test]
fn test_port_io_crtc() {
    let mut vga = Vga::new();
    vga.write_port(0x3D4, 0x0C);
    vga.write_port(0x3D5, 0x00);
    assert_eq!(vga.read_port(0x3D5), 0x00);
}

#[test]
fn test_input_status_register() {
    let mut vga = Vga::new();
    let val1 = vga.read_port(0x3DA);
    let val2 = vga.read_port(0x3DA);
    // Just verify we can read the port without panic
    let _ = val1;
    let _ = val2;
}

#[test]
fn test_render_text_mode() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    vga.render(&mut memory);
    assert!(!vga.framebuffer().is_empty());
}

#[test]
fn test_render_mode13h() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);
    vga.render(&mut memory);
    let (w, h) = vga.dimensions();
    assert_eq!(w, 320);
    assert_eq!(h, 200);
}

#[test]
fn test_render_cga_mode() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x04);
    vga.framebuffer = vec![0u32; 1];
    vga.fb_width = 1;
    vga.fb_height = 1;
    vga.render(&mut memory);
    let (w, h) = vga.dimensions();
    assert_eq!(w, 320);
    assert_eq!(h, 200);
}

#[test]
fn test_render_cga_header_bytes_offset() {
    let _env = OverrideGuard::cga_header_bytes(Some("2"));
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x04);
    vga.set_cga_color_select(0x30);
    memory.write_u8(0xB8000 + 2, 0xC0);

    vga.render(&mut memory);
    assert_eq!(vga.framebuffer[0], 0xFFFFFF);
}

#[test]
fn test_render_cga_mode05_intensity_palette() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();

    vga.set_mode_bios(0x05);
    vga.set_cga_color_select(0x10);
    memory.write_u8(0xB8000, 0x40);

    vga.render(&mut memory);
    assert_eq!(vga.framebuffer[0], 0x55FFFF);
}

#[test]
fn test_mode_x_setup() {
    let mut vga = Vga::new();
    let mut mem = Memory::new();

    vga.set_mode_bios(0x13);
    vga.fb_width = 320;
    vga.fb_height = 200;
    // 256-colour mode bit must be set for Mode X detection (register-derived).
    vga.attribute_regs[0x10] = 0x41;
    // CR9 max_scan_line = 0 (Mode-X CRTC commitment, LC bit 9 preserved):
    // see Vga::is_mode_x() doc comment — disabling chain-4 alone is not
    // sufficient under the strict predicate.
    vga.crtc_regs[0x09] = 0x40;
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06);
    assert!(vga.is_mode_x());

    for plane in 0..4 {
        vga.write_port(0x3C4, 0x02);
        vga.write_port(0x3C5, 1 << plane);
    }
    vga.render(&mut mem);
}

#[test]
fn test_mode_x_rendering() {
    let mut vga = Vga::new();
    let mut mem = Memory::new();

    vga.set_mode_bios(0x13);
    vga.fb_width = 320;
    vga.fb_height = 200;
    // Programme Mode X via registers: 256-colour + chain-4 cleared.
    vga.attribute_regs[0x10] = 0x41; // 256-color + graphics (required for Mode X)
    vga.sequencer_regs[4] &= !0x08; // chain-4 cleared
    vga.crtc_regs[0x01] = 0x4F; // 80 char clocks -> 320px in 256-color
    vga.crtc_regs[0x12] = 199; // VDE low = 199
    vga.crtc_regs[0x07] = 0; // clear overflow VDE high bits
    vga.crtc_regs[0x09] = 0; // max_scan=0 → height=200
    vga.render(&mut mem);

    let (w, h) = vga.dimensions();
    assert_eq!(w, 320);
    assert_eq!(h, 200);
    assert_eq!(vga.framebuffer().len(), 320 * 200);
}

#[test]
fn test_mode_13h_rendering() {
    let mut vga = Vga::new();
    let mut mem = Memory::new();

    vga.set_mode_bios(0x13);
    vga.fb_width = 320;
    vga.fb_height = 200;
    // Mode 13h (linear, chain-4 enabled) — must not be Mode X.
    vga.sequencer_regs[4] |= 0x08;
    vga.render(&mut mem);

    let (w, h) = vga.dimensions();
    assert_eq!(w, 320);
    assert_eq!(h, 200);
    assert_eq!(vga.framebuffer().len(), 320 * 200);
}

#[test]
fn test_ega_planar_mode() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x10);
    vga.render(&mut memory);
    let (w, h) = vga.dimensions();
    assert_eq!(w, 640);
    assert_eq!(h, 350);
}

#[test]
fn test_scroll_region_struct() {
    let region = ScrollRegion {
        top: 5,
        left: 10,
        bottom: 20,
        right: 70,
    };
    assert_eq!(region.top, 5);
    assert_eq!(region.left, 10);
    assert_eq!(region.bottom, 20);
    assert_eq!(region.right, 70);
}

#[test]
fn test_misc_output_register() {
    let mut vga = Vga::new();
    vga.write_port(0x3C2, 0x63);
    assert_eq!(vga.read_port(0x3CC), 0x63);
}

#[test]
fn test_attribute_controller() {
    let mut vga = Vga::new();
    vga.read_port(0x3DA);
    vga.write_port(0x3C0, 0x00);
    vga.write_port(0x3C0, 0x00);
}

#[test]
fn test_render_mode06() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x06);
    vga.framebuffer = vec![0u32; 640 * 200];
    vga.fb_width = 640;
    vga.fb_height = 200;
    vga.render(&mut memory);
    let (w, h) = vga.dimensions();
    assert_eq!(w, 640);
    assert_eq!(h, 200);
}

#[test]
fn test_render_mode0d() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0D);
    vga.render(&mut memory);
    let (w, h) = vga.dimensions();
    assert_eq!(w, 320);
    assert_eq!(h, 200);
}

#[test]
fn test_render_mode0e() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0E);
    vga.render(&mut memory);
    let (w, h) = vga.dimensions();
    assert_eq!(w, 640);
    assert_eq!(h, 200);
}

#[test]
fn test_render_mode12() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x12);
    vga.render(&mut memory);
    let (w, h) = vga.dimensions();
    assert_eq!(w, 640);
    assert_eq!(h, 480);
}

#[test]
fn test_render_ega_mono_split_intensity() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0F);
    vga.framebuffer = vec![0u32; 1];
    vga.fb_width = 1;
    vga.fb_height = 1;
    vga.crtc_regs[0x18] = 1;
    vga.crtc_regs[0x07] = 0;
    vga.crtc_regs[0x09] = 0;

    memory.plane_mut(0)[0] = 0xC0;
    memory.plane_mut(2)[0] = 0x80;

    vga.render(&mut memory);
    assert_eq!(vga.framebuffer[0], 0xFFFFFF);
}

#[test]
fn test_render_ega_mono_offset_out_of_bounds() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x0F);
    vga.crtc_regs[0x0C] = 0xFF;
    vga.crtc_regs[0x0D] = 0xFF;

    vga.render(&mut memory);
    assert_eq!(vga.framebuffer[0], 0);
}

#[test]
fn test_render_vga_mono_split() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x11);
    vga.framebuffer = vec![0u32; 1];
    vga.fb_width = 1;
    vga.fb_height = 1;
    vga.crtc_regs[0x18] = 1;
    vga.crtc_regs[0x07] = 0;
    vga.crtc_regs[0x09] = 0;

    memory.plane_mut(0)[0] = 0x80;

    vga.render(&mut memory);
    // Mode 11h is monochrome: pixel 1 → AC[1]=0x0F → DAC[15]=(255,255,255) white
    assert_eq!(vga.framebuffer[0], 0xFFFFFF);
}

#[test]
fn test_render_vga_mono_offset_out_of_bounds() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x11);
    vga.crtc_regs[0x0C] = 0xFF;
    vga.crtc_regs[0x0D] = 0xFF;

    vga.render(&mut memory);
    assert_eq!(vga.framebuffer[0], 0);
}

#[test]
fn test_set_mode_unknown() {
    // A mode number the adapter has no entry for is refused outright; it
    // does NOT fall back to a text-mode default. (This test previously
    // asserted the fallback, i.e. it pinned MDD-BUG-KILN-00641.)
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);
    assert!(!vga.set_mode_bios(0xFF));
    assert_eq!(vga.mode(), 0x13);
    assert_eq!((vga.fb_width, vga.fb_height), (320, 200));
}

/// The gate in `is_supported_bios_mode` and the geometry table in
/// `set_mode_internal` are two hand-written lists of the same set. Pin them
/// to each other so adding an arm to one without the other is a test
/// failure rather than a silently unreachable mode (or a silently
/// half-configured adapter).
#[test]
fn modes_the_geometry_table_configures_are_exactly_the_supported_set() {
    // Every standard VGA BIOS mode; 0x0B/0x0C are EGA-internal and are not
    // settable modes on any adapter we model.
    let expected: Vec<u8> = (0x00..=0x13u8)
        .filter(|m| !matches!(m, 0x0B | 0x0C))
        .collect();

    let accepted: Vec<u8> = (0x00..=0x7Fu8)
        .filter(|&m| {
            let mut vga = Vga::new();
            let ok = vga.set_mode_bios(m);
            assert_eq!(
                ok,
                Vga::is_supported_bios_mode(m),
                "set_mode_bios({m:#04X}) disagrees with is_supported_bios_mode"
            );
            if ok {
                // An accepted mode must have configured a real surface.
                assert_eq!(
                    vga.framebuffer.len(),
                    vga.fb_width * vga.fb_height,
                    "mode {m:#04X} left an inconsistent framebuffer"
                );
            }
            ok
        })
        .collect();

    assert_eq!(accepted, expected);
}

#[test]
fn test_set_mode_05() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x05); // CGA b/w graphics mode
    assert_eq!(vga.mode(), 0x05);
    assert_eq!(vga.fb_width, 320);
    assert_eq!(vga.fb_height, 200);
}

#[test]
fn test_set_mode_00() {
    let mut vga = Vga::new();
    vga.set_mode_bios(0x00); // 40x25 text mode
    assert_eq!(vga.mode(), 0x00);
    assert_eq!(vga.columns, 40);
}

#[test]
fn test_set_cursor_invalid_page() {
    let mut vga = Vga::new();
    vga.set_cursor(255, 10, 20);
    assert_eq!(
        vga.get_cursor(0),
        (10, 20),
        "BIOS cursor access aliases an invalid page to page zero"
    );
}

#[test]
fn test_teletype_bell() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 5, 10);
    vga.teletype(0x07, 0, &mut memory); // Bell character
    let (row, col) = vga.get_cursor(0);
    assert_eq!(row, 5);
    assert_eq!(col, 10); // Cursor should not move
}

#[test]
fn test_teletype_tab() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 0, 3);
    vga.teletype(0x09, 0, &mut memory); // Tab character
    let (_, col) = vga.get_cursor(0);
    assert_eq!(col, 8); // Should jump to column 8
}

#[test]
fn test_teletype_wrap() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 0, 79);
    vga.teletype(b'X', 0, &mut memory); // At end of line
    let (row, col) = vga.get_cursor(0);
    assert_eq!(row, 1); // Should wrap to next line
    assert_eq!(col, 0);
}

#[test]
fn test_teletype_scroll() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 24, 0); // Last row
    vga.teletype(0x0A, 0, &mut memory); // Line feed should trigger scroll
    let (row, _) = vga.get_cursor(0);
    assert_eq!(row, 24); // Should stay at row 24 after scroll
}

#[test]
fn test_teletype_graphics_mode13h() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    vga.set_mode_bios(0x13);
    vga.set_cursor(0, 0, 0);
    vga.teletype(b'A', 0, &mut memory); // Character in graphics mode
    let (row, col) = vga.get_cursor(0);
    assert_eq!(row, 0);
    assert_eq!(col, 1);
}

#[test]
fn test_teletype_graphics_mode_modex_writes_planes() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    let ch = first_nonzero_row_char();

    vga.set_mode_bios(0x13);
    vga.write_port(0x3D4, 0x09); // CRTC index: max scan line
    vga.write_port(0x3D5, 0x40); // Clear double-scan (Mode X setup)
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06);
    vga.set_cursor(0, 0, 0);
    vga.teletype(ch, 0, &mut memory);

    let found =
        (0..64).any(|offset| (0..4).any(|plane| memory.read_video_planar(offset, plane) != 0));
    assert!(found);
}

#[test]
fn test_teletype_graphics_mode_planar_writes_pixels() {
    let mut vga = Vga::new();
    let mut memory = Memory::new();
    let ch = first_nonzero_row_char();

    vga.set_mode_bios(0x0D);
    vga.set_cursor(0, 0, 0);
    vga.teletype(ch, 0, &mut memory);
    let wrote = memory.plane(0)[0..8].iter().any(|&b| b != 0);
    assert!(wrote);
}

/// Every non-zero byte outside page 0's own 4000 bytes, scanned WELL past
/// the 32 KiB text aperture.
///
/// The window deliberately runs to 0xD0000, not 0xC0000: an unclamped
/// `row = 0xFF` forms `0xB8000 + (255 * 80 + col) * 2`, which lands near
/// 0xC1F60 — *beyond* the aperture. A scan that stopped at the aperture
/// edge would never see the very write this test exists to catch, and
/// would pass against the unfixed code.
fn strays_outside_page0(memory: &Memory) -> Vec<(usize, u8)> {
    (0xB8000..0xD0000)
        .filter(|a| *a >= 0xB8000 + 80 * 25 * 2)
        .map(|a| (a, memory.read_u8(a)))
        .filter(|(_, v)| *v != 0)
        .collect()
}
