use super::*;

#[test]
fn test_int10_write_char_only() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x03);
    vga.set_cursor(0, 0, 0);

    cpu.regs.set_ah(0x0A); // Write char only
    cpu.regs.set_al(b'Z');
    cpu.regs.set_bh(0); // Page 0
    cpu.regs.set_cx(5); // Count

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_set_border_color() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x01); // Set border color

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_set_all_palette_regs() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x02); // Set all palette registers

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_write_string() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x03);

    // Write string "ABC" to memory
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, b'A');
    memory.write_u8(addr + 1, b'B');
    memory.write_u8(addr + 2, b'C');

    cpu.regs.set_ah(0x13); // Write string
    cpu.regs.set_al(0x01); // Mode (update cursor)
    cpu.regs.set_bh(0); // Page 0
    cpu.regs.set_bl(0x07); // Attribute
    cpu.regs.set_cx(3); // Count
    cpu.regs.set_dh(0); // Row
    cpu.regs.set_dl(0); // Column
    cpu.regs.es = 0x2000;
    cpu.regs.set_bp(0x0100);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_write_string_with_attr() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x03);

    // Write string with attrs: "AB" with individual attrs
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, b'A');
    memory.write_u8(addr + 1, 0x0F); // Attr for A
    memory.write_u8(addr + 2, b'B');
    memory.write_u8(addr + 3, 0x70); // Attr for B

    cpu.regs.set_ah(0x13); // Write string
    cpu.regs.set_al(0x02); // Mode (chars with attrs)
    cpu.regs.set_bh(0); // Page 0
    cpu.regs.set_cx(2); // Count
    cpu.regs.set_dh(0); // Row
    cpu.regs.set_dl(0); // Column
    cpu.regs.es = 0x2000;
    cpu.regs.set_bp(0x0100);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn int10_write_string_interprets_control_characters_kiln77() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    set_text_mode_03(&mut bios, &mut cpu, &mut memory, &mut vga);

    let source = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (offset, byte) in b"A\r\nB\x08C\x07D".iter().copied().enumerate() {
        memory.write_u8(source + offset, byte);
    }

    cpu.regs.set_ax(0x1301); // AH=13h, update cursor, chars only
    cpu.regs.set_bh(0);
    cpu.regs.set_bl(0x1E);
    cpu.regs.set_cx(8);
    cpu.regs.set_dh(0);
    cpu.regs.set_dl(0);
    cpu.regs.es = 0x2000;
    cpu.regs.set_bp(0x0100);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    assert_eq!(read_text_cell(&memory, 0, 0), (b'A', 0x1E));
    assert_eq!(
        read_text_cell(&memory, 0, 1),
        (b' ', 0x07),
        "control characters must not be drawn as glyphs"
    );
    assert_eq!(read_text_cell(&memory, 1, 0), (b'C', 0x1E));
    assert_eq!(read_text_cell(&memory, 1, 1), (b'D', 0x1E));
    assert_eq!(vga.get_cursor(0), (1, 2));
    assert_eq!(memory.read_u16(0x450), 0x0102);
}

#[test]
fn int10_write_string_without_cursor_update_preserves_page_cursor() {
    let (vga_cursor, bda_cursor) = write_wrapping_string_and_read_page_cursor(0x00);

    assert_eq!(vga_cursor, (7, 9));
    assert_eq!(bda_cursor, 0x0709);
}

#[test]
fn int10_write_string_with_cursor_update_syncs_page_cursor() {
    let (vga_cursor, bda_cursor) = write_wrapping_string_and_read_page_cursor(0x01);

    assert_eq!(vga_cursor, (5, 1));
    assert_eq!(bda_cursor, 0x0501);
}

#[test]
fn test_int10_set_mode_text_40x25() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x01); // 40x25 text mode

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(0x449), 0x01);
}

fn assert_int10_text_preset_roundtrips_text_words(mode: u8) {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Exercise the real AH=00 preset and its memory-pipeline publication.
    cpu.regs.set_ax(mode as u16);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    let text_base = if mode == 0x07 { 0xB0000 } else { 0xB8000 };
    memory.write_u16(text_base, 0x1E41); // Guest word: 'A', attribute 1Eh.
    let guest_word = memory.read_u16(text_base);

    // BIOS AH=09 writes a second cell and AH=08 reads that exact cell back.
    cpu.regs.set_ah(0x02);
    cpu.regs.set_bh(0);
    cpu.regs.set_dh(0);
    cpu.regs.set_dl(1);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    cpu.regs.set_ah(0x09);
    cpu.regs.set_al(b'T');
    cpu.regs.set_bh(0);
    cpu.regs.set_bl(0x4F);
    cpu.regs.set_cx(1);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    cpu.regs.set_ah(0x08);
    cpu.regs.set_bh(0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    let bios_word = cpu.regs.ax();

    assert_eq!(
        (guest_word, bios_word),
        (0x1E41, 0x4F54),
        "INT 10h mode {mode:02X} must read interleaved text words after its preset"
    );
    assert!(
        vga.build_pipeline_state().gc_odd_even_read,
        "INT 10h mode {mode:02X} must preset GC5 odd/even reads"
    );
    assert_eq!(
        vga.read_text_row(0, &memory).unwrap(),
        "AT",
        "INT 10h mode {mode:02X} text scanout input must retain both cells"
    );

    // A guest can still switch the text adapter to sequential plane access for
    // font memory. The mode preset must not override a later GC5 write.
    memory.write_u8(0xA0000, 0x99); // Raw RAM backing outside the current text map.
    for (plane, value) in [(0, 0x11), (1, 0x22), (2, 0x33), (3, 0x44)] {
        memory.write_video_planar(0, value, 1 << plane);
    }
    vga.write_port(0x3C4, 0x02);
    vga.write_port(0x3C5, 0x04); // Map Mask: plane 2.
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06); // Disable odd/even addressing for font access.
    vga.write_port(0x3CE, 0x04);
    vga.write_port(0x3CF, 0x02); // Read Map Select: plane 2.
    vga.write_port(0x3CE, 0x05);
    vga.write_port(0x3CF, 0x00); // Explicit GC5 odd/even read disable.
    vga.write_port(0x3CE, 0x06);
    vga.write_port(0x3CF, 0x04); // Text mode with A0000 aperture.
    memory.mark_vga_pipeline_dirty();
    memory.apply_vga_pipeline(&vga.build_pipeline_state());
    assert!(
        !vga.build_pipeline_state().gc_odd_even_read,
        "guest GC5 write must disable odd/even reads after a text preset"
    );
    memory.write_u8(0xA0000, 0x5A);
    assert_eq!(
        (
            memory.read_u8(0xA0000),
            memory.read_u8_physical(0xA0000),
            memory.read_video_planar(0, 0),
            memory.read_video_planar(0, 1),
            memory.read_video_planar(0, 2),
            memory.read_video_planar(0, 3),
        ),
        (0x5A, 0x99, 0x11, 0x22, 0x5A, 0x44),
        "guest GC5 reprogramming must write and read only the selected font plane"
    );

    // Graphics presets keep the GC5 odd/even-read bit clear.
    cpu.regs.set_ax(0x000D);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert!(
        !vga.build_pipeline_state().gc_odd_even_read,
        "graphics mode presets must keep GC5 odd/even reads disabled"
    );
}

#[test]
fn bios_text_preset_mode_00_roundtrips_text_words() {
    assert_int10_text_preset_roundtrips_text_words(0x00);
}

#[test]
fn bios_text_preset_mode_01_roundtrips_text_words() {
    assert_int10_text_preset_roundtrips_text_words(0x01);
}

#[test]
fn bios_text_preset_mode_02_roundtrips_text_words() {
    assert_int10_text_preset_roundtrips_text_words(0x02);
}

#[test]
fn bios_text_preset_mode_03_roundtrips_text_words() {
    assert_int10_text_preset_roundtrips_text_words(0x03);
}

#[test]
fn bios_text_preset_mode_07_roundtrips_text_words() {
    assert_int10_text_preset_roundtrips_text_words(0x07);
}

#[test]
fn test_int10_set_mode_cga_graphics() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x04); // CGA 320x200 4-color

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(0x449), 0x04);
    assert_eq!(memory.read_u8(0x465), 0x2A);
    assert_eq!(vga.get_cga_mode_control(), 0x2A);
    assert_eq!(memory.read_u8(0x466), vga.get_cga_color_select());
}

#[test]
fn test_int10_set_mode_cga_hires() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x06); // CGA 640x200 2-color

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(0x449), 0x06);
    assert_eq!(memory.read_u8(0x465), 0x1E);
    assert_eq!(vga.get_cga_mode_control(), 0x1E);
    assert_eq!(memory.read_u8(0x466), vga.get_cga_color_select());
}

#[test]
fn test_int16_read_key() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    // Put a key in the buffer
    bda::kbd_enqueue(&mut memory, 0x1E, b'a');

    cpu.regs.set_ah(0x00); // Read key

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), b'a');
    assert_eq!(cpu.regs.ah(), 0x1E);
}

#[test]
fn test_int16_check_key_present() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    // Put a key in the buffer
    bda::kbd_enqueue(&mut memory, 0x1E, b'a');

    cpu.regs.set_ah(0x01); // Check for key

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    // AH=01h always redirects to assembler peek stub
    match result.unwrap() {
        Int16Disp::Stub(offset) => assert_eq!(offset, Bios::INT16_PEEK_01),
        Int16Disp::Iret => panic!("expected Stub, got Iret"),
    }
}

#[test]
fn test_int16_extended_read_key() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    // Put a key in the buffer
    bda::kbd_enqueue(&mut memory, 0x1E, b'a');

    cpu.regs.set_ah(0x10); // Extended read key

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), b'a');
    assert_eq!(cpu.regs.ah(), 0x1E);
}

#[test]
fn test_int16_extended_shift_flags() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    // AL copies 0040:0017. AH combines left modifiers/pressed locks
    // from 0040:0018 with right modifiers from 0040:0096 and remaps SysRq.
    bda::kbd_set_shift_flags(&mut memory, 0x05);
    bda::kbd_set_extended_shift_flags(&mut memory, 0x46);
    bda::kbd_set_keyboard_flags(&mut memory, 0x8c);

    cpu.regs.set_ah(0x12); // Get extended shift flags

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x05);
    assert_eq!(cpu.regs.ah(), 0xce);
}

#[test]
fn test_int15_unsupported_function() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0xFF); // Unsupported

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
}

/// An extender kernel probe is unsupported by the BIOS. The response
/// sets CF and AH=86h and preserves ES:BX.
#[test]
fn test_int15_bf_dos4g_probe_is_unsupported() {
    for al in [0x00u8, 0x01, 0x02] {
        let mut bios = Bios::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();

        cpu.regs.set_ah(0xBF);
        cpu.regs.set_al(al);
        cpu.regs.es = 0x1234;
        cpu.regs.set_bx(0x5678);

        let result = bios.int15(&mut cpu, &mut memory, None);
        assert!(result.is_ok());
        assert_eq!(cpu.regs.ah(), 0x86, "AL={al:02X}: function not supported");
        assert!(
            cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "AL={al:02X}: CF set"
        );
        assert_eq!(cpu.regs.es, 0x1234, "AL={al:02X}: ES untouched");
        assert_eq!(cpu.regs.bx(), 0x5678, "AL={al:02X}: BX untouched");
    }
}

// PS/2 Mouse BIOS tests (INT 15h AH=C2)
#[test]
fn test_int15_c200_enable_disable_mouse() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Enable mouse
    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x00);
    cpu.regs.set_bh(0x01); // Enable

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x00);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Disable mouse
    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x00);
    cpu.regs.set_bh(0x00); // Disable

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x00);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_c201_reset_mouse() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x01);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x00);
    assert_eq!(cpu.regs.bh(), 0x00); // Standard PS/2 mouse ID
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_c202_set_sample_rate() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Valid rate
    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x02);
    cpu.regs.set_bh(0x05); // 100 samples/sec

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x00);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Invalid rate
    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x02);
    cpu.regs.set_bh(0x07); // Invalid

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x02); // Invalid parameter
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_c203_set_resolution() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Valid resolution
    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x03);
    cpu.regs.set_bh(0x02); // 4 counts/mm

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x00);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Invalid resolution
    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x03);
    cpu.regs.set_bh(0x04); // Invalid

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x02);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_c204_get_device_id() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x04);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x00);
    assert_eq!(cpu.regs.bh(), 0x00); // Standard PS/2 mouse
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_c205_initialize_interface() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Valid packet size
    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x05);
    cpu.regs.set_bh(0x03); // 3 bytes per packet

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x00);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Invalid packet size (0)
    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x05);
    cpu.regs.set_bh(0x00);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x02);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Invalid packet size (9)
    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x05);
    cpu.regs.set_bh(0x09);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x02);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_c206_extended_commands() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Get device status
    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x06);
    cpu.regs.set_bh(0x00);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x00);
    assert_eq!(cpu.regs.bl(), 0x00); // Status
    assert_eq!(cpu.regs.cl(), 0x02); // Resolution (4 counts/mm)
    assert_eq!(cpu.regs.dl(), 0x64); // Sample rate (100)
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Set scaling 1:1
    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x06);
    cpu.regs.set_bh(0x01);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x00);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Set scaling 2:1
    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x06);
    cpu.regs.set_bh(0x02);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x00);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Invalid subfunction
    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x06);
    cpu.regs.set_bh(0x03);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x02);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_c207_set_device_handler() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x07);
    cpu.regs.es = 0x1234;
    cpu.regs.set_bx(0x5678);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x00);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_c2_invalid_subfunction() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x08); // Invalid subfunction

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x01); // Invalid function
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

// APM (Advanced Power Management) tests - INT 15h AH=53h
#[test]
fn test_int15_apm_5300_installation_check() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x00);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bx(), 0x504D); // "PM"
    assert_eq!(cpu.regs.ax(), 0x0102); // APM version 1.2
                                       // No protected-mode interface is advertised, because none exists to
                                       // call. This assertion previously read 0x03 and was the false
                                       // advertisement MDD-BUG-KILN-00218 reports.
    assert_eq!(cpu.regs.cx(), 0x0000);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_apm_5301_connect_rm() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x01);
    cpu.regs.set_bx(0x0000); // System BIOS power device ID

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x00);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_apm_5302_connect_pm16() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x02);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    // APM 1.2: 06h = 16-bit protected mode interface not supported. This
    // test previously asserted only "carry clear" and so passed while we
    // returned a success whose entry point was not callable.
    assert_eq!(cpu.regs.ah(), 0x06);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_apm_5303_connect_pm32() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x03);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    // APM 1.2: 08h = 32-bit protected mode interface not supported.
    assert_eq!(cpu.regs.ah(), 0x08);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// Pins the decision behind MDD-BUG-KILN-00218: we advertise no APM
/// protected-mode interface, and both connects refuse with the status the
/// APM 1.2 spec defines.
///
/// **What this test can and cannot prove.** It cannot verify that an
/// advertised entry point is *callable* — doing that needs a protected-mode
/// guest to far-call the returned address and execute an APM function, which
/// is the integration-level test the bug asks for and which cannot be
/// written until a bridge exists. So it pins two weaker but checkable
/// things: the concrete no-interface state, and cross-consistency between
/// the capability word and what connect actually grants.
///
/// Note the consequence: this test would NOT have failed on the original
/// code, where the capability word and the connects agreed on the same
/// falsehood. The assertion that does the real work is `capabilities == 0`.
/// Anyone adding a real bridge should raise the matching bit, flip the
/// expectation here, and add the guest-level call test.
#[test]
fn apm_advertises_no_pm_interface_and_refuses_both_connects() {
    fn apm(al: u8) -> Cpu {
        let mut bios = Bios::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        cpu.regs.set_ah(0x53);
        cpu.regs.set_al(al);
        bios.int15(&mut cpu, &mut memory, None)
            .expect("INT 15h AH=53h must not error");
        cpu
    }

    let capabilities = apm(0x00).regs.cx();

    // The load-bearing assertion. Bit 0 = 16-bit PM interface supported,
    // bit 1 = 32-bit PM interface supported (APM 1.2, function 5300h).
    // Both must stay clear while no callable protected-mode bridge exists.
    assert_eq!(
        capabilities & 0x03,
        0x0000,
        "5300h must advertise no protected-mode APM interface until a callable bridge exists"
    );

    // (capability bit, connect subfunction, APM "not supported" status)
    for (bit, subfunction, unsupported_status) in [(0u16, 0x02u8, 0x06u8), (1, 0x03, 0x08)] {
        let advertised = capabilities & (1 << bit) != 0;
        let cpu = apm(subfunction);
        let connected = !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY);

        assert_eq!(
            advertised, connected,
            "5300h CX bit {bit} says advertised={advertised}, but 53{subfunction:02X}h \
             connect returned connected={connected}"
        );
        if !advertised {
            assert_eq!(
                cpu.regs.ah(),
                unsupported_status,
                "53{subfunction:02X}h must reject with the documented APM status"
            );
        }
    }
}

#[test]
fn test_int15_apm_5304_disconnect() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x04);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x00);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_apm_5305_cpu_idle() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x05);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_apm_5306_cpu_busy() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x06);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_apm_5307_set_power_state() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x07);
    cpu.regs.set_bx(0x0001); // System BIOS
    cpu.regs.set_cx(0x0000); // Ready state

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_apm_530a_get_power_status() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x0A);
    cpu.regs.set_bx(0x0001); // System BIOS power device

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bh(), 0x01); // AC power on
    assert_eq!(cpu.regs.ch(), 0x80); // No system battery
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_apm_530b_get_pm_event() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x0B);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    // Should return "no events pending"
    assert_eq!(cpu.regs.ah(), 0x80);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_apm_530e_driver_version() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x0E);
    cpu.regs.set_bx(0x0000);
    cpu.regs.set_ch(1); // Request version 1.2
    cpu.regs.set_cl(2);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 1); // Major
    assert_eq!(cpu.regs.al(), 2); // Minor
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_apm_5310_get_capabilities() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x10);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bx(), 0x0000); // No batteries
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_apm_invalid_subfunction() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0xFF); // Invalid subfunction

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x0C); // Function not supported
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_a20_unsupported_subfunction() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x24);
    cpu.regs.set_al(0x03); // Unsupported subfunction

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x86); // Not supported
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int1a_unsupported_function() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    cpu.regs.set_ah(0xFF); // Unsupported

    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    );
    assert!(result.is_ok());
}

// Font loading tests (INT 10h AH=11h)
#[test]
fn test_int10_load_8x14_font() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set text mode first
    vga.set_mode_bios(0x03);

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x01); // Load 8x14 font
    cpu.regs.set_bl(0); // Block 0

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_load_8x14_font_recalc() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x03);

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x11); // Load 8x14 font and recalculate
    cpu.regs.set_bl(0);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    // Check BDA was updated
    assert_eq!(memory.read_u8(0x485), 14); // Char height
}

#[test]
fn test_int10_load_8x8_font() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x03);

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x02); // Load 8x8 font
    cpu.regs.set_bl(0);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_load_8x8_font_recalc() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x03);

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x12); // Load 8x8 font and recalculate
    cpu.regs.set_bl(0);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(0x485), 8); // Char height
}

#[test]
fn test_int10_load_8x16_font() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x03);

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x04); // Load 8x16 font
    cpu.regs.set_bl(0);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_load_8x16_font_recalc() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x03);

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x14); // Load 8x16 font and recalculate
    cpu.regs.set_bl(0);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(0x485), 16); // Char height
}

#[test]
fn test_int10_set_font_block_specifier() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x03); // Set block specifier
    cpu.regs.set_bl(0x05); // Block spec value

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_graphics_font_8x8() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x23); // 8x8 graphics font
    cpu.regs.set_bl(0);
    cpu.regs.set_dl(25); // Rows

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_graphics_font_8x14() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x22); // 8x14 graphics font
    cpu.regs.set_bl(0);
    cpu.regs.set_dl(25);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_graphics_font_8x16() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x24); // 8x16 graphics font
    cpu.regs.set_bl(0);
    cpu.regs.set_dl(25);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_user_graphics_font() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set up a user font in memory
    let font_seg = 0x2000u16;
    let font_off = 0x0000u16;
    cpu.regs.es = font_seg;
    cpu.regs.set_bp(font_off);

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x20); // User 8x8 graphics font
    cpu.regs.set_bl(0);
    cpu.regs.set_dl(25);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_user_graphics_font_custom_height() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    let font_seg = 0x2000u16;
    let font_off = 0x0000u16;
    cpu.regs.es = font_seg;
    cpu.regs.set_bp(font_off);

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x21); // User graphics font with custom height
    cpu.regs.set_cx(12); // 12 bytes per char
    cpu.regs.set_bl(0);
    cpu.regs.set_dl(25);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

// Palette read tests (INT 10h AH=10h)
#[test]
fn test_int10_get_palette_register() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set a palette register first
    vga.set_palette_reg(5, 0x3F);

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x07); // Get individual palette register
    cpu.regs.set_bl(5); // Register 5

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bh(), 0x3F);
}

#[test]
fn test_int10_get_border_color() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set border color via attribute register 0x11
    let mut attr = vga.get_attribute_regs();
    attr[0x11] = 0x07;
    vga.set_attribute_regs(attr);

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x08); // Get border color

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bh(), 0x07);
}

#[test]
fn test_int10_get_all_palette_registers() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set some palette registers
    for i in 0..16 {
        vga.set_palette_reg(i, i + 0x10);
    }

    let table_seg = 0x2000u16;
    let table_off = 0x0000u16;
    cpu.regs.es = table_seg;
    cpu.regs.set_dx(table_off);

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x09); // Get all palette registers

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    // Verify palette values were written to table
    let table_addr = Memory::seg_offset_to_linear(table_seg, table_off);
    for i in 0..16 {
        assert_eq!(memory.read_u8(table_addr + i), (i as u8) + 0x10);
    }
}

#[test]
fn test_int10_toggle_blinking() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x03); // Toggle blinking
    cpu.regs.set_bl(0x00); // Disable blinking (enable intensity)

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    // Now enable blinking
    cpu.regs.set_bl(0x01);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_set_all_palette_with_border() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set up palette table in memory (16 palette + 1 border)
    let table_seg = 0x2000u16;
    let table_off = 0x0000u16;
    let table_addr = Memory::seg_offset_to_linear(table_seg, table_off);
    for i in 0..16 {
        memory.write_u8(table_addr + i, (i as u8) * 4);
    }
    memory.write_u8(table_addr + 16, 0x0F); // Border color

    cpu.regs.es = table_seg;
    cpu.regs.set_dx(table_off);

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x02); // Set all palette registers and border

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

// Video mode tests with BDA setup
#[test]
fn test_int10_set_mode_10h() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x10); // 640x350 EGA mode

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(0x449), 0x10); // Mode stored in BDA
    assert_eq!(memory.read_u8(0x484), 24); // 25 rows with 8x14 font (350/14)
    assert_eq!(memory.read_u8(0x485), 14); // 14-pixel char height
}

#[test]
fn test_int10_set_mode_12h() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x12); // 640x480 VGA mode

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(0x449), 0x12);
    assert_eq!(memory.read_u8(0x484), 29); // 30 rows with 8x16 font (480/16)
    assert_eq!(memory.read_u8(0x485), 16);
}

#[test]
fn test_int10_set_mode_cga_40x25() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x00); // 40x25 text mode

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(0x449), 0x00);
    assert_eq!(memory.read_u8(0x44A), 40); // 40 columns
    assert_eq!(memory.read_u16(0x44C), 0x0800); // Page size
}

#[test]
fn test_int10_set_mode_07h() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x07); // MDA mode

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(0x449), 0x07);
    assert_eq!(memory.read_u8(0x44A), 80);
    assert_eq!(memory.read_u16(0x44C), 0x1000); // Page size
}

#[test]
fn test_int10_set_mode_07h_clears_mda_vram() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    memory.write_u8(0xB8000, 0x5A);
    memory.write_u8(0xB8001, 0xA5);

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x07); // MDA mode

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    assert_eq!(memory.read_u8(0xB0000), b' ');
    assert_eq!(memory.read_u8(0xB0001), 0x07);
    assert_eq!(memory.read_u8(0xB8000), 0x5A);
    assert_eq!(memory.read_u8(0xB8001), 0xA5);
}

// CGA mode control
#[test]
fn test_cga_mode_control_from_table() {
    // Test valid CGA modes
    for mode in 0..=7u8 {
        let result = Bios::cga_mode_control_from_table(mode);
        assert!(result.is_some(), "Mode {mode} should have CGA control");
    }
    // Test invalid mode
    let result = Bios::cga_mode_control_from_table(0x10);
    assert!(result.is_none());
}

// Grayscale functions
#[test]
fn test_grayscale_triplet() {
    let (r, g, b) = Bios::grayscale_triplet(0x3F, 0x3F, 0x3F);
    assert_eq!(r, g);
    assert_eq!(g, b);
    assert!(r <= 0x3F);

    let (r, g, b) = Bios::grayscale_triplet(0, 0, 0);
    assert_eq!(r, 0);
    assert_eq!(g, 0);
    assert_eq!(b, 0);
}

#[test]
fn test_grayscale_enabled() {
    let mut memory = Memory::new();

    // Default should be disabled
    memory.write_u8(0x489, 0x00);
    assert!(!Bios::grayscale_enabled(&memory));

    // Enable grayscale
    memory.write_u8(0x489, 0x02);
    assert!(Bios::grayscale_enabled(&memory));

    memory.write_u8(0x489, 0x04);
    assert!(Bios::grayscale_enabled(&memory));

    memory.write_u8(0x489, 0x06);
    assert!(Bios::grayscale_enabled(&memory));
}

// Font info tests for different BH values (INT 10h AH=11h AL=30h)
#[test]
fn test_int10_get_font_info_bh1() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x30); // Get font info
    cpu.regs.set_bh(1); // INT 43h 8x8 font

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_get_font_info_bh2() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x30);
    cpu.regs.set_bh(2); // ROM 8x14 font

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.es, 0xF000);
}

#[test]
fn test_int10_get_font_info_bh3() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x30);
    cpu.regs.set_bh(3); // ROM 8x8 double-dot font

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.es, 0xF000);
}

#[test]
fn test_int10_get_font_info_bh4() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x30);
    cpu.regs.set_bh(4); // ROM 8x8 upper 128 chars

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.es, 0xF000);
}

#[test]
fn test_int10_get_font_info_bh5() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x30);
    cpu.regs.set_bh(5); // ROM 8x14 alternate

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.es, 0xF000);
}

#[test]
fn test_int10_get_font_info_bh6() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x30);
    cpu.regs.set_bh(6); // ROM 8x16 font

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.es, 0xF000);
}

#[test]
fn test_int10_get_font_info_bh7() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x30);
    cpu.regs.set_bh(7); // ROM 8x16 alternate

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.es, 0xF000);
}

// User font loading with count/start parameters
#[test]
fn test_int10_load_user_font() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);
    vga.set_mode_bios(0x03);

    // Set up a simple user font in memory
    let font_seg = 0x2000u16;
    let font_off = 0x0000u16;
    let font_addr = Memory::seg_offset_to_linear(font_seg, font_off);
    for i in 0..256 * 16 {
        memory.write_u8(font_addr + i, 0xAA); // Pattern
    }

    cpu.regs.es = font_seg;
    cpu.regs.set_bp(font_off);

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x00); // Load user font
    cpu.regs.set_bh(16); // Bytes per char
    cpu.regs.set_bl(0); // Block
    cpu.regs.set_cx(256); // Char count
    cpu.regs.set_dx(0); // Start char

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_int10_load_user_font_recalc() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);
    vga.set_mode_bios(0x03);

    let font_seg = 0x2000u16;
    let font_off = 0x0000u16;
    cpu.regs.es = font_seg;
    cpu.regs.set_bp(font_off);

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x10); // Load user font and recalculate
    cpu.regs.set_bh(14); // 14 bytes per char
    cpu.regs.set_bl(0);
    cpu.regs.set_cx(256);
    cpu.regs.set_dx(0);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(0x485), 14);
}

// Display combination code tests
#[test]
fn test_int10_display_combination_set() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x1A);
    cpu.regs.set_al(0x00); // Get display combination

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x1A); // Function supported
}

// Teletype with page parameter
#[test]
fn test_int10_teletype_with_page() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);

    cpu.regs.set_ah(0x0E); // Teletype output
    cpu.regs.set_al(b'X'); // Character
    cpu.regs.set_bh(1); // Page 1

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

// Get video mode
#[test]
fn test_int10_get_video_mode_after_set() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // Set mode 0x13
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x13);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // Get mode
    cpu.regs.set_ah(0x0F);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x13);
}

// Read/write pixel
#[test]
fn test_int10_write_read_pixel_mode13() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);

    // Write pixel
    cpu.regs.set_ah(0x0C);
    cpu.regs.set_al(0x0F); // Color 15
    cpu.regs.set_bh(0); // Page 0
    cpu.regs.set_cx(100); // X
    cpu.regs.set_dx(50); // Y
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // Read pixel
    cpu.regs.set_ah(0x0D);
    cpu.regs.set_bh(0);
    cpu.regs.set_cx(100);
    cpu.regs.set_dx(50);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x0F);
}

// Set cursor position on multiple pages
#[test]
fn test_int10_set_cursor_multiple_pages() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);

    for page in 0..4u8 {
        cpu.regs.set_ah(0x02);
        cpu.regs.set_bh(page);
        cpu.regs.set_dh(page + 5); // Row
        cpu.regs.set_dl(page * 10); // Column
        let result = bios.int10(&mut cpu, &mut memory, &mut vga);
        assert!(result.is_ok());

        // Verify BDA was updated
        let bda_addr = 0x450 + (page as usize) * 2;
        assert_eq!(memory.read_u8(bda_addr), page * 10); // Column
        assert_eq!(memory.read_u8(bda_addr + 1), page + 5); // Row
    }
}

// AH=12h BL=20h - Select alternate print screen
#[test]
fn test_int10_alt_func_alternate_print_screen() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x20);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x12);
}

// AH=12h BL=30h - Select scan lines (AL=1: 350 lines)
#[test]
fn test_int10_alt_func_scan_lines_350() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x30);
    cpu.regs.set_al(1); // 350 scan lines
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x12);
    // Check flags updated
    let flags = memory.read_u8(0x489);
    assert_eq!(flags & 0x9F, 0x00); // 350 lines: 0x6F mask clears bits
}

// AH=12h BL=30h - Select scan lines (AL=2: 400 lines)
#[test]
fn test_int10_alt_func_scan_lines_400() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x30);
    cpu.regs.set_al(2); // 400 scan lines
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x12);
    // Check flags updated
    let flags = memory.read_u8(0x489);
    assert_eq!(flags & 0x10, 0x10); // 400 lines sets bit 4
}

// AH=12h BL=30h - Invalid AL value (> 2)
#[test]
fn test_int10_alt_func_scan_lines_invalid() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x30);
    cpu.regs.set_al(5); // Invalid
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0); // Returns 0 on invalid
}

// Font info with default BH value (BH=8 uses default)
#[test]
fn test_int10_font_info_default_bh() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    Bios::init_rom_fonts(&mut memory);
    memory.write_u8(0x485, 16); // Set font height
    memory.write_u8(0x484, 24); // 25 rows

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x30);
    cpu.regs.set_bh(8); // Default case (>=8 falls through)
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.es, 0xF000);
    // Default returns 8x8 font offset
    assert_eq!(cpu.regs.bp(), Bios::ROM_FONT_8X8_OFFSET);
}

// Grayscale palette test
#[test]
fn test_int10_palette_grayscale_mode() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);

    // Enable grayscale mode via BDA byte 0x489 bit 1 or 2
    memory.write_u8(0x489, 0x02); // Enable grayscale

    // Set a color palette entry
    let palette_addr = 0x1000;
    cpu.regs.es = 0x0100;
    cpu.regs.set_dx(0); // Offset within segment = 0x1000 linear
    memory.write_u8(palette_addr, 0x30); // R
    memory.write_u8(palette_addr + 1, 0x20); // G
    memory.write_u8(palette_addr + 2, 0x10); // B

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x12); // Set block of DAC registers
    cpu.regs.set_bx(0); // Start register
    cpu.regs.set_cx(1); // 1 entry
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    // The color should be grayscaled
    let entries = vga.get_dac_entries();
    let (r, g, b) = entries[0];
    // Grayscale converts to same value for R, G, B
    assert_eq!(r, g);
    assert_eq!(g, b);
}

// Read DAC entry out of range (returns 0)
#[test]
fn test_int10_read_dac_out_of_range() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x15); // Read individual DAC
    cpu.regs.set_bl(255); // Entry 255 - might be out of range in some implementations
                          // Set regs to non-zero to verify they get cleared
    cpu.regs.set_dh(0xFF);
    cpu.regs.set_ch(0xFF);
    cpu.regs.set_cl(0xFF);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    // Entry 255 should exist and return valid values (not the else branch)
}

// Test CGA mode control fallback
#[test]
fn test_cga_mode_control_out_of_range() {
    // Mode 8+ returns None
    let result = Bios::cga_mode_control_from_table(0x08);
    assert!(result.is_none());

    let result = Bios::cga_mode_control_from_table(0x10);
    assert!(result.is_none());
}

// A mode number past the adapter's table is refused, not defaulted.
// (This test previously asserted the BDA took the default values, i.e. it
// pinned MDD-BUG-KILN-00641 — see
// `unsupported_vendor_mode_number_fails_the_svga_readback_probe`.)
#[test]
fn test_int10_set_mode_unusual() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x12);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    let char_height = memory.read_u8(0x485);

    // Mode 0x14 is in no mode list: nothing changes.
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x14);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(memory.read_u8(0x449), 0x12);
    assert_eq!(memory.read_u8(0x485), char_height);
    assert_eq!(vga.mode(), 0x12);
}

// AH=12h BL=36h - Video refresh control (screen off)
#[test]
fn test_int10_alt_func_video_refresh_off() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x36);
    cpu.regs.set_al(0); // Screen off
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x12);
    // Check sequencer register 1 bit 5 cleared
    let seq = vga.get_sequencer_regs();
    assert_eq!(seq[1] & 0x20, 0);
}

// AH=12h - Unhandled BL value (default case)
#[test]
fn test_int10_alt_func_unhandled_bl() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0xFF); // Unhandled
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

// AH=1Bh - Functionality state with BX != 0
#[test]
fn test_int10_functionality_state_invalid_bx() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(1); // Invalid - must be 0
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0);
}

// AH=1Ch - Video state with invalid AL
#[test]
fn test_int10_video_state_invalid_al() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(5); // Invalid AL (not 0, 1, or 2)
    cpu.regs.set_cx(0x07);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0);
}

// AH=1Ch - Video state save with state=0
#[test]
fn test_int10_video_state_save_zero_state() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(1); // Save
    cpu.regs.set_cx(0); // No state bits set
    cpu.regs.es = 0x1000;
    cpu.regs.set_bx(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0); // Fails when no state bits
}

// AH=1Ch - Video state restore with state=0
#[test]
fn test_int10_video_state_restore_zero_state() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(2); // Restore
    cpu.regs.set_cx(0); // No state bits set
    cpu.regs.es = 0x1000;
    cpu.regs.set_bx(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0); // Fails when no state bits
}

// AH=1Ah AL=01h - Set DCC with invalid code
#[test]
fn test_int10_set_dcc_invalid() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x1A);
    cpu.regs.set_al(0x01); // Set DCC
    cpu.regs.set_bx(0xFFFF); // Invalid DCC code
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    // Should return 0 on failure (line 1063-1064)
    assert_eq!(cpu.regs.al(), 0);
}

// AH=1Ah - Invalid AL (default case)
#[test]
fn test_int10_dcc_invalid_al() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x1A);
    cpu.regs.set_al(0x05); // Invalid
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

// Test functionality state with CGA modes (04, 05, 06)
#[test]
fn test_int10_functionality_state_cga_modes() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Test mode 04 (4 colors)
    vga.set_mode_bios(0x04);
    memory.write_u8(0x449, 0x04);

    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(0);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    // Check color count at offset 0x27
    let colors = memory.read_u16(0x20000 + 0x27);
    assert_eq!(colors, 4);
}

// Test functionality state with mode 06 (2 colors)
#[test]
fn test_int10_functionality_state_mode6() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x06);
    memory.write_u8(0x449, 0x06);

    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(0);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let colors = memory.read_u16(0x20000 + 0x27);
    assert_eq!(colors, 2);
}

// Test functionality state with mode 07 (mono)
#[test]
fn test_int10_functionality_state_mode7() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x07);
    memory.write_u8(0x449, 0x07);

    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(0);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let colors = memory.read_u16(0x20000 + 0x27);
    assert_eq!(colors, 1); // Mono
}

// Test functionality state with monochrome CRT port
#[test]
fn test_int10_functionality_state_mono_crt() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set mono CRT port (0x3B4)
    memory.write_u16(0x463, 0x3B4);

    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(0);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    // Check misc flags at offset 0x2D
    let flags = memory.read_u8(0x20000 + 0x2D);
    assert_eq!(flags & 0x04, 0x04); // Mono bit set
}

// Test functionality state with grayscale enabled
#[test]
fn test_int10_functionality_state_grayscale() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Enable grayscale
    memory.write_u8(0x489, 0x02);

    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(0);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let flags = memory.read_u8(0x20000 + 0x2D);
    assert_eq!(flags & 0x02, 0x02); // Grayscale bit set
}

// Test functionality state with background intensity enabled
#[test]
fn test_int10_functionality_state_blink_disabled() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set BDA 0x487 bit 0 (background intensity)
    memory.write_u8(0x487, 0x01);

    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(0);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let flags = memory.read_u8(0x20000 + 0x2D);
    assert_eq!(flags & 0x10, 0x10); // Blink disabled bit
}

// Test functionality state page calculation
#[test]
fn test_int10_functionality_state_pages() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set page size > 0 for page calculation
    memory.write_u16(0x44C, 0x1000); // 4K pages

    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(0);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let pages = memory.read_u8(0x20000 + 0x29);
    assert_eq!(pages, 8); // 0x8000 / 0x1000 = 8
}

// Unhandled INT 10h function
#[test]
fn test_int10_unhandled_function() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0xFE); // Unhandled
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

// Test video state blocks calculation
#[test]
fn test_video_state_blocks() {
    // State 0 returns 0
    assert_eq!(Bios::video_state_blocks(0), 0);
    // State with only hardware bit
    assert!(Bios::video_state_blocks(0x01) > 0);
    // State with all bits
    assert!(Bios::video_state_blocks(0x07) > 0);
}

// AH=F0h - EGA RIL read register
#[test]
fn test_int10_ega_ril_read() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Read CRTC register 0
    cpu.regs.set_ah(0xF0);
    cpu.regs.set_bl(0);
    cpu.regs.set_dx(0x00); // CRTC group
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    // Returns value in BL
}

// AH=F1h - EGA RIL write register
#[test]
fn test_int10_ega_ril_write() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Write to sequencer register 1
    cpu.regs.set_ah(0xF1);
    cpu.regs.set_bl(1); // Register
    cpu.regs.set_bh(0x55); // Value
    cpu.regs.set_dx(0x08); // Sequencer group
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    // Verify
    let seq = vga.get_sequencer_regs();
    assert_eq!(seq[1], 0x55);
}

// EGA RIL read misc output
#[test]
fn test_int10_ega_ril_read_misc() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0xF0);
    cpu.regs.set_bl(0);
    cpu.regs.set_dx(0x20); // Misc output
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

// EGA RIL read feature control
#[test]
fn test_int10_ega_ril_read_feature() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0xF0);
    cpu.regs.set_bl(0);
    cpu.regs.set_dx(0x28); // Feature control
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

// EGA RIL invalid group
#[test]
fn test_int10_ega_ril_invalid_group() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0xF0);
    cpu.regs.set_bl(0);
    cpu.regs.set_dx(0xFF); // Invalid
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bl(), 0);
}

// AH=F2h - EGA RIL read range
#[test]
fn test_int10_ega_ril_read_range() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0xF2);
    cpu.regs.set_ch(0); // Start reg
    cpu.regs.set_cl(5); // Count
    cpu.regs.es = 0x2000;
    cpu.regs.set_bx(0);
    cpu.regs.set_dx(0x00); // CRTC group
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

// AH=F3h - EGA RIL write range
#[test]
fn test_int10_ega_ril_write_range() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Write some data to buffer
    for i in 0..5 {
        memory.write_u8(0x20000 + i, (i + 1) as u8);
    }

    cpu.regs.set_ah(0xF3);
    cpu.regs.set_ch(0); // Start reg
    cpu.regs.set_cl(5); // Count
    cpu.regs.es = 0x2000;
    cpu.regs.set_bx(0);
    cpu.regs.set_dx(0x08); // Sequencer group
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

// Test toggle blinking with state > 1
#[test]
fn test_int10_toggle_blink_invalid() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x03); // Toggle blinking
    cpu.regs.set_bl(5); // Invalid state (> 1)
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

// Test mode 0D with functionality state
#[test]
fn test_int10_functionality_state_mode0d() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x0D);
    memory.write_u8(0x449, 0x0D);

    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(0);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let colors = memory.read_u16(0x20000 + 0x27);
    assert_eq!(colors, 16);
}

// Test mode 0E with functionality state
#[test]
fn test_int10_functionality_state_mode0e() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x0E);
    memory.write_u8(0x449, 0x0E);

    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(0);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let colors = memory.read_u16(0x20000 + 0x27);
    assert_eq!(colors, 16);
}

// Test mode 10 with functionality state
#[test]
fn test_int10_functionality_state_mode10() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x10);
    memory.write_u8(0x449, 0x10);

    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(0);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let colors = memory.read_u16(0x20000 + 0x27);
    assert_eq!(colors, 16);
}

// Test mode 0F (2 colors)
#[test]
fn test_int10_functionality_state_mode0f() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    memory.write_u8(0x449, 0x0F);

    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(0);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let colors = memory.read_u16(0x20000 + 0x27);
    assert_eq!(colors, 2);
}

// Test mode 11 (2 colors)
#[test]
fn test_int10_functionality_state_mode11() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    memory.write_u8(0x449, 0x11);

    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(0);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let colors = memory.read_u16(0x20000 + 0x27);
    assert_eq!(colors, 2);
}

// Test mode 12 (16 colors)
#[test]
fn test_int10_functionality_state_mode12() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x12);
    memory.write_u8(0x449, 0x12);

    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(0);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let colors = memory.read_u16(0x20000 + 0x27);
    assert_eq!(colors, 16);
}

// Test mode 13 (256 colors)
#[test]
fn test_int10_functionality_state_mode13() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x13);
    memory.write_u8(0x449, 0x13);

    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(0);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let colors = memory.read_u16(0x20000 + 0x27);
    assert_eq!(colors, 256);
}

// Test video state save and restore cycle
#[test]
fn test_int10_video_state_save_restore() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);

    // Save state
    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(1); // Save
    cpu.regs.set_cx(0x07); // All state bits
    cpu.regs.es = 0x2000;
    cpu.regs.set_bx(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x1C);

    // Restore state
    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(2); // Restore
    cpu.regs.set_cx(0x07);
    cpu.regs.es = 0x2000;
    cpu.regs.set_bx(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x1C);
}

// Test DAC page selection
#[test]
fn test_int10_dac_page_select() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x13); // Select DAC paging mode
    cpu.regs.set_bl(0); // Function 0: select paging mode
    cpu.regs.set_bh(1); // Mode 1
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

// Test read DAC page
#[test]
fn test_int10_read_dac_page() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x1A); // Get DAC paging state
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

// Test perform grayscale summing (separate test)
#[test]
fn test_int10_grayscale_summing_palette() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    vga.set_mode_bios(0x13);

    // Set some colors first
    vga.set_dac_reg(0, 0x30, 0x20, 0x10);

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x1B); // Perform grayscale summing
    cpu.regs.set_bx(0); // Start
    cpu.regs.set_cx(1); // Count
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

// Test EGA RIL group functions
#[test]
fn test_ega_ril_group() {
    // Test valid groups
    assert!(Bios::ega_ril_group(0x00).is_some()); // CRTC
    assert!(Bios::ega_ril_group(0x08).is_some()); // Sequencer
    assert!(Bios::ega_ril_group(0x10).is_some()); // Graphics
    assert!(Bios::ega_ril_group(0x18).is_some()); // Attribute
    assert!(Bios::ega_ril_group(0x20).is_some()); // Misc output
    assert!(Bios::ega_ril_group(0x28).is_some()); // Feature control
    assert!(Bios::ega_ril_group(0x30).is_some()); // Graphics 1 pos
    assert!(Bios::ega_ril_group(0x38).is_some()); // Graphics 2 pos
                                                  // Test invalid
    assert!(Bios::ega_ril_group(0x40).is_none());
    assert!(Bios::ega_ril_group(0xFF).is_none());
}

// Test grayscale triplet calculation
#[test]
fn test_grayscale_triplet_values() {
    // White (max values)
    let (r, g, b) = Bios::grayscale_triplet(0x3F, 0x3F, 0x3F);
    assert_eq!(r, g);
    assert_eq!(g, b);
    assert_eq!(r, 0x3F);

    // Black
    let (r, g, b) = Bios::grayscale_triplet(0, 0, 0);
    assert_eq!(r, 0);
    assert_eq!(g, 0);
    assert_eq!(b, 0);

    // Pure red
    let (r, g, b) = Bios::grayscale_triplet(0x3F, 0, 0);
    assert!(r > 0);
    assert_eq!(r, g);
    assert_eq!(g, b);
}

// Test EGA RIL write to CRTC group
#[test]
fn test_int10_ega_ril_write_crtc() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0xF1);
    cpu.regs.set_bl(0); // Register 0
    cpu.regs.set_bh(0x42); // Value
    cpu.regs.set_dx(0x00); // CRTC group
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let crtc = vga.get_crtc_regs();
    assert_eq!(crtc[0], 0x42);
}

// Test EGA RIL write to graphics controller
#[test]
fn test_int10_ega_ril_write_graphics() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0xF1);
    cpu.regs.set_bl(0); // Register 0
    cpu.regs.set_bh(0x33); // Value
    cpu.regs.set_dx(0x10); // Graphics controller group
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let gfx = vga.get_graphics_regs();
    assert_eq!(gfx[0], 0x33);
}

// Test EGA RIL write to attribute controller
#[test]
fn test_int10_ega_ril_write_attr() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0xF1);
    cpu.regs.set_bl(0); // Register 0
    cpu.regs.set_bh(0x22); // Value
    cpu.regs.set_dx(0x18); // Attribute controller group
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let attr = vga.get_attribute_regs();
    assert_eq!(attr[0], 0x22);
}

// Test EGA RIL write misc output
#[test]
fn test_int10_ega_ril_write_misc() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0xF1);
    cpu.regs.set_bl(0);
    cpu.regs.set_bh(0x67); // Value
    cpu.regs.set_dx(0x20); // Misc output
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(vga.get_misc_output(), 0x67);
}

// Test EGA RIL write feature control
#[test]
fn test_int10_ega_ril_write_feature() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0xF1);
    cpu.regs.set_bl(0);
    cpu.regs.set_bh(0x03); // Value
    cpu.regs.set_dx(0x28); // Feature control
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(vga.get_feature_control(), 0x03);
}

// Test EGA RIL write with out of range register
#[test]
fn test_int10_ega_ril_write_out_of_range() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0xF1);
    cpu.regs.set_bl(99); // Out of range register
    cpu.regs.set_bh(0xFF);
    cpu.regs.set_dx(0x00); // CRTC group (only 25 regs)
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

// Test video state restore with DAC state = 1
#[test]
fn test_int10_video_state_restore_dac_read() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);

    // First save state
    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(1); // Save
    cpu.regs.set_cx(0x04); // DAC state only
    cpu.regs.es = 0x2000;
    cpu.regs.set_bx(0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // Modify the saved DAC state to indicate read mode (state = 1)
    // The DAC state is saved at buffer+0x20 when state bit 2 is set
    let buffer = 0x20000;
    memory.write_u8(buffer + 0x20, 1); // dac_state = 1 (read mode)

    // Now restore
    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(2); // Restore
    cpu.regs.set_cx(0x04);
    cpu.regs.es = 0x2000;
    cpu.regs.set_bx(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

// Test functionality state with BDA bit 3 set
#[test]
fn test_int10_functionality_state_bda_bit3() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set BDA 0x489 bit 3
    memory.write_u8(0x489, 0x08);

    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(0);
    cpu.regs.es = 0x2000;
    cpu.regs.set_di(0);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let flags = memory.read_u8(0x20000 + 0x2D);
    assert_eq!(flags & 0x08, 0x08); // Bit 3 set in misc flags
}

// Test toggle blinking enable (state = 1)
#[test]
fn test_int10_toggle_blink_enable() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x03); // Toggle blinking
    cpu.regs.set_bl(1); // Enable blinking
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    // Check attribute register 0x10 bit 3
    let attr = vga.get_attribute_regs();
    assert_eq!(attr[0x10] & 0x08, 0x08);
}

// Test toggle blinking disable (state = 0)
#[test]
fn test_int10_toggle_blink_disable() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    vga.set_mode_bios(0x03);

    // First enable blinking
    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x03);
    cpu.regs.set_bl(1);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // Now disable
    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x03);
    cpu.regs.set_bl(0); // Disable blinking
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let attr = vga.get_attribute_regs();
    assert_eq!(attr[0x10] & 0x08, 0);
}

// Test EGA RIL read with out of range register
#[test]
fn test_int10_ega_ril_read_out_of_range() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0xF0);
    cpu.regs.set_bl(99); // Out of range
    cpu.regs.set_dx(0x00); // CRTC
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bl(), 0); // Returns 0 for out of range
}

// Test EGA RIL read range with invalid group
#[test]
fn test_int10_ega_ril_read_range_invalid() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0xF2);
    cpu.regs.set_ch(0);
    cpu.regs.set_cl(5);
    cpu.regs.es = 0x2000;
    cpu.regs.set_bx(0);
    cpu.regs.set_dx(0xFF); // Invalid group
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

// Test EGA RIL write range with CRTC
#[test]
fn test_int10_ega_ril_write_range_crtc() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Prepare data
    for i in 0..5 {
        memory.write_u8(0x20000 + i, (0x10 + i) as u8);
    }

    cpu.regs.set_ah(0xF3);
    cpu.regs.set_ch(0);
    cpu.regs.set_cl(5);
    cpu.regs.es = 0x2000;
    cpu.regs.set_bx(0);
    cpu.regs.set_dx(0x00); // CRTC
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    // Verify
    let crtc = vga.get_crtc_regs();
    assert_eq!(crtc[0], 0x10);
}

// Test DAC page select function 1
#[test]
fn test_int10_dac_page_select_func1() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x13);
    cpu.regs.set_bl(1); // Function 1: select page
    cpu.regs.set_bh(2); // Page 2
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

fn write_wrapping_string_and_read_page_cursor(mode: u8) -> ((u8, u8), u16) {
    const PAGE: u8 = 2;
    const BDA_CURSOR: usize = 0x450 + PAGE as usize * 2;

    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x03);
    memory.write_u8(0x484, 24); // 25 rows
    vga.set_cursor(PAGE, 7, 9);
    memory.write_u16(BDA_CURSOR, 0x0709);

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, b'A');
    memory.write_u8(addr + 1, b'B');

    cpu.regs.set_ax(0x1300 | mode as u16);
    cpu.regs.set_bh(PAGE);
    cpu.regs.set_bl(0x07);
    cpu.regs.set_cx(2);
    cpu.regs.set_dh(4);
    cpu.regs.set_dl(79);
    cpu.regs.es = 0x2000;
    cpu.regs.set_bp(0x0100);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    (vga.get_cursor(PAGE), memory.read_u16(BDA_CURSOR))
}

#[test]
fn test_int10_bda_preserves_no_clear_bit() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x86);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    assert_eq!(
        memory.read_u8(0x449),
        0x06,
        "BDA 0x449 holds the masked mode"
    );
    assert_eq!(
        memory.read_u8(0x487) & 0x80,
        0x80,
        "BDA 0x487 bit 7 holds the no-clear flag"
    );

    cpu.regs.set_ah(0x0F);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(
        cpu.regs.al(),
        0x86,
        "INT 10h AH=0Fh must recompose mode | no-clear"
    );
}

#[test]
fn test_int10_ah07_graphics_scroll_down() {
    for mode in [0x04u8, 0x05, 0x06, 0x13] {
        let mut bios = Bios::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        let mut vga = Vga::new();
        cpu.regs.set_ax(u16::from(mode));
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
        let (base, len, row_bytes, cell_bytes, fill) = match mode {
            0x04 | 0x05 => (0xB8000, 0x4000, 80, 2, 0xAA),
            0x06 => (0xB8000, 0x4000, 80, 1, 0),
            _ => (0xA0000, 0x10000, 320, 8, 0xFE),
        };
        let offset = |y: usize, x: usize| {
            if mode == 0x13 {
                y * row_bytes + x
            } else {
                (y % 2) * 0x2000 + (y / 2) * row_bytes + x
            }
        };
        let initial: Vec<u8> = (0..len)
            .map(|i| ((i * 17 + i / row_bytes + 31) % 256) as u8)
            .collect();
        for (i, byte) in initial.iter().enumerate() {
            memory.write_u8(base + i, *byte);
        }
        let mut expected = initial.clone();
        for y in 8..40 {
            for x in 2 * cell_bytes..7 * cell_bytes {
                expected[offset(y, x)] = if y < 24 {
                    fill
                } else {
                    initial[offset(y - 16, x)]
                };
            }
        }
        cpu.regs.set_ax(0x0702);
        cpu.regs.set_bh(0xFE);
        cpu.regs.set_cx(0x0102);
        cpu.regs.set_dx(0x0406);
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
        for (i, byte) in expected.iter().enumerate() {
            assert_eq!(
                memory.read_u8(base + i),
                *byte,
                "AH07 mode={mode:02x} offset={i:#x}"
            );
        }
    }
}

#[test]
fn pixel_pages_int10_services_match_selected_display() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    cpu.regs.set_ax(0x000D);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    for (page, color) in [(0, 2), (1, 5)] {
        cpu.regs.set_ah(0x0C);
        cpu.regs.set_al(color);
        cpu.regs.set_bh(page);
        cpu.regs.set_cx(0);
        cpu.regs.set_dx(0);
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    }
    for (page, color) in [(0, 2), (1, 5)] {
        cpu.regs.set_ah(0x0D);
        cpu.regs.set_bh(page);
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
        assert_eq!(cpu.regs.al(), color, "AH0D page {page}");
        cpu.regs.set_ah(0x05);
        cpu.regs.set_al(page);
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
        assert_eq!(memory.read_u16(0x44E), u16::from(page) * 0x2000);
        vga.render(&mut memory);
        assert_eq!(
            vga.framebuffer()[0],
            if color == 2 { 0x00AA00 } else { 0xAA00AA }
        );
    }
}

#[test]
fn bios_pixel_xor_ah0c_readback_and_mode13_control() {
    fn write_pixel(
        bios: &mut Bios,
        cpu: &mut Cpu,
        memory: &mut Memory,
        vga: &mut Vga,
        page: u8,
        x: u16,
        y: u16,
        color: u8,
    ) -> u8 {
        cpu.regs.set_ah(0x0C);
        cpu.regs.set_al(color);
        cpu.regs.set_bh(page);
        cpu.regs.set_cx(x);
        cpu.regs.set_dx(y);
        bios.int10(cpu, memory, vga).unwrap();
        cpu.regs.al()
    }

    fn read_pixel(
        bios: &mut Bios,
        cpu: &mut Cpu,
        memory: &mut Memory,
        vga: &mut Vga,
        page: u8,
        x: u16,
        y: u16,
    ) -> u8 {
        cpu.regs.set_ah(0x0D);
        cpu.regs.set_bh(page);
        cpu.regs.set_cx(x);
        cpu.regs.set_dx(y);
        bios.int10(cpu, memory, vga).unwrap();
        cpu.regs.al()
    }

    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    for (mode, x, y, neighbor_x, initial, neighbor, xor_color, xor_result, replacement, replaced) in [
        (0x04u8, 9, 3, 8, 1, 2, 0x83, 2, 0x7E, 2),
        (0x05, 9, 3, 8, 1, 2, 0x83, 2, 0x7E, 2),
        (0x06, 17, 3, 16, 1, 1, 0x83, 0, 0x7E, 0),
        (0x12, 10, 2, 11, 0x0A, 3, 0x85, 0x0F, 0x76, 0x06),
    ] {
        cpu.regs.set_ax(u16::from(mode));
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
        let page = 0;

        assert_eq!(
            write_pixel(
                &mut bios,
                &mut cpu,
                &mut memory,
                &mut vga,
                page,
                neighbor_x,
                y,
                neighbor
            ),
            neighbor
        );
        assert_eq!(
            write_pixel(
                &mut bios,
                &mut cpu,
                &mut memory,
                &mut vga,
                page,
                x,
                y,
                initial
            ),
            initial
        );
        let initial_pixel = read_pixel(&mut bios, &mut cpu, &mut memory, &mut vga, page, x, y);
        assert_eq!(initial_pixel, initial, "mode {mode:02x} initial pixel");

        // AL reaches the VGA write intact, then AH=0Dh reads the XOR result back.
        assert_eq!(
            write_pixel(
                &mut bios,
                &mut cpu,
                &mut memory,
                &mut vga,
                page,
                x,
                y,
                xor_color
            ),
            xor_color
        );
        assert_eq!(
            read_pixel(&mut bios, &mut cpu, &mut memory, &mut vga, page, x, y),
            xor_result,
            "mode {mode:02x} first XOR"
        );

        match mode {
            0x04 | 0x05 => {
                let address = 0xB8000 + 0x2000 + (y as usize / 2) * 80 + x as usize / 4;
                let target_shift = (3 - x as usize % 4) * 2;
                let neighbor_shift = (3 - neighbor_x as usize % 4) * 2;
                let byte = memory.read_u8(address);
                assert_eq!((byte >> target_shift) & 3, xor_result);
                assert_eq!((byte >> neighbor_shift) & 3, neighbor);
            }
            0x06 => {
                let address = 0xB8000 + 0x2000 + (y as usize / 2) * 80 + x as usize / 8;
                let target_bit = 1 << (7 - x as usize % 8);
                let neighbor_bit = 1 << (7 - neighbor_x as usize % 8);
                let byte = memory.read_u8(address);
                assert_eq!((byte & target_bit) != 0, xor_result != 0);
                assert_eq!((byte & neighbor_bit) != 0, neighbor != 0);
            }
            0x12 => {
                let offset = y as usize * 80 + x as usize / 8;
                let target_bit = 1 << (7 - x as usize % 8);
                let neighbor_bit = 1 << (7 - neighbor_x as usize % 8);
                for plane in 0..4 {
                    let byte = memory.plane(plane)[offset];
                    assert_eq!((byte & target_bit) != 0, xor_result & (1 << plane) != 0);
                    assert_eq!((byte & neighbor_bit) != 0, neighbor & (1 << plane) != 0);
                }
            }
            _ => unreachable!(),
        }

        assert_eq!(
            write_pixel(
                &mut bios,
                &mut cpu,
                &mut memory,
                &mut vga,
                page,
                x,
                y,
                xor_color
            ),
            xor_color
        );
        assert_eq!(
            read_pixel(&mut bios, &mut cpu, &mut memory, &mut vga, page, x, y),
            initial,
            "mode {mode:02x} repeated XOR"
        );
        assert_eq!(
            write_pixel(&mut bios, &mut cpu, &mut memory, &mut vga, page, x, y, 0x80),
            0x80
        );
        assert_eq!(
            read_pixel(&mut bios, &mut cpu, &mut memory, &mut vga, page, x, y),
            initial,
            "mode {mode:02x} zero XOR"
        );
        assert_eq!(
            write_pixel(
                &mut bios,
                &mut cpu,
                &mut memory,
                &mut vga,
                page,
                x,
                y,
                replacement
            ),
            replacement
        );
        assert_eq!(
            read_pixel(&mut bios, &mut cpu, &mut memory, &mut vga, page, x, y),
            replaced,
            "mode {mode:02x} replacement"
        );
    }

    cpu.regs.set_ax(0x0013);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    let (x, y) = (10, 2);
    assert_eq!(
        write_pixel(&mut bios, &mut cpu, &mut memory, &mut vga, 0, x, y, 0x83),
        0x83
    );
    assert_eq!(
        read_pixel(&mut bios, &mut cpu, &mut memory, &mut vga, 0, x, y),
        0x83
    );
    let address = 0xA0000 + y as usize * 320 + x as usize;
    assert_eq!(memory.read_u8(address), 0x83);
    assert_eq!(
        write_pixel(&mut bios, &mut cpu, &mut memory, &mut vga, 0, x, y, 0x83),
        0x83
    );
    assert_eq!(
        read_pixel(&mut bios, &mut cpu, &mut memory, &mut vga, 0, x, y),
        0x83
    );
    assert_eq!(memory.read_u8(address), 0x83);
}

// Independently synthetic BIOS graphics font patterns; no executable input.
fn graphics_teletype_foreground(mode: u8, colors: &[u8], mask: u8) {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);
    cpu.regs.set_ax(u16::from(mode));
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    // A guest-selected eight-row glyph with two foreground pixels on each row.
    // Its independently written pattern never depends on a captured font.
    let glyph: u8 = 0xA6;
    let cga = matches!(mode, 4..=6);
    cpu.set_es_real_mode(0x2500);
    cpu.regs.set_bp(0);
    let index = if cga { glyph - 128 } else { glyph };
    for row in 0..8 {
        memory.write_u8(0x25000 + usize::from(index) * 8 + row, 0x81);
    }
    cpu.regs.set_ax(if cga { 0x1120 } else { 0x1121 });
    cpu.regs.set_bx(0);
    cpu.regs.set_cx(8);
    cpu.regs.set_dx(25);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    for (column, &color) in colors.iter().enumerate() {
        cpu.regs.set_ax(0x0E00 | u16::from(glyph));
        cpu.regs.set_bx(u16::from(color));
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
        for y in 0..8 {
            for x in 0..8 {
                let expected = if x == 0 || x == 7 { color & mask } else { 0 };
                assert_eq!(
                    vga.read_pixel(0, (column * 8 + x) as u16, y, &memory),
                    expected,
                    "AH0E foreground mode={mode:02x} BL={color:02x} x={x} y={y}"
                );
            }
        }
        assert_eq!(vga.get_cursor(0), (0, (column + 1) as u8));
        assert_eq!(memory.read_u16(0x450), (column + 1) as u16);
    }
    // DOS and other callers of the original interface retain their foreground.
    vga.teletype(glyph, 0, &mut memory);
    let default = match mode {
        4 | 5 => 3,
        6 => 1,
        _ => 15,
    };
    assert_eq!(
        vga.read_pixel(0, (colors.len() * 8) as u16, 0, &memory),
        default,
        "non-BIOS teletype default mode={mode:02x}"
    );
}

#[test]
fn graphics_teletype_foreground_mode13() {
    graphics_teletype_foreground(0x13, &[2, 4, 0x82, 0x84], 0xFF);
}

#[test]
fn graphics_teletype_foreground_cga() {
    for mode in [4, 5] {
        graphics_teletype_foreground(mode, &[1, 2, 5, 6], 3);
    }
}

#[test]
fn graphics_teletype_foreground_cga_monochrome() {
    graphics_teletype_foreground(6, &[0, 1, 2, 3], 1);
}

#[test]
fn graphics_teletype_foreground_planar() {
    for mode in [0x0D, 0x0E, 0x10, 0x12] {
        graphics_teletype_foreground(mode, &[2, 4, 0x12, 0x24], 15);
    }
}

fn graphics_selected_font_output(service: u8, cga: bool) {
    let modes: &[u8] = if cga {
        &[4, 5, 6]
    } else {
        &[0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13]
    };
    for &mode in modes {
        let mut bios = Bios::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        let mut vga = Vga::new();
        bios.init_ivt(&mut memory, false);
        cpu.regs.set_ax(u16::from(mode));
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
        let glyph = 0xA5;
        let height = if cga { 8 } else { 5 };
        let pattern = [0x81, 0x42, 0x24, 0x18, 0xA5, 0x66, 0xDB, 0x3C];
        cpu.set_es_real_mode(0x2400);
        cpu.regs.set_bp(0x120);
        let font = Memory::seg_offset_to_linear(0x2400, 0x120);
        let index = if cga { glyph - 128 } else { glyph };
        for (cy, &bits) in pattern.iter().take(height).enumerate() {
            memory.write_u8(font + usize::from(index) * height + cy, bits);
        }
        cpu.regs.set_ax(if cga { 0x1120 } else { 0x1121 });
        cpu.regs.set_cx(height as u16);
        cpu.regs.set_bx(0);
        cpu.regs.set_dx(40);
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
        if !cga {
            // An unrelated upper-half table must not override INT43 glyphs.
            memory.write_u16(0x1F * 4, 0x2000);
            memory.write_u16(0x1F * 4 + 2, 0x2400);
            for cy in 0..8 {
                memory.write_u8(0x26000 + usize::from(glyph - 128) * 8 + cy, 0xFF);
            }
        }
        cpu.regs.set_ax(0x0200);
        cpu.regs.set_bx(0);
        cpu.regs.set_dx(0x0101);
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
        cpu.regs.set_ax(u16::from(service) << 8 | u16::from(glyph));
        cpu.regs.set_bx(0x000F);
        cpu.regs.set_cx(1);
        if service == 0x13 {
            cpu.regs.set_al(1);
            cpu.regs.set_dx(0x0101);
            cpu.regs.set_bp(0x100);
            memory.write_u8(Memory::seg_offset_to_linear(0x2400, 0x100), glyph);
        }
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
        let fg = match mode {
            4 | 5 => 3,
            6 => 1,
            _ => 15,
        };
        for (cy, &bits) in pattern.iter().take(height).enumerate() {
            for x in 0..8 {
                assert_eq!(
                    vga.read_pixel(0, 8 + x as u16, (height + cy) as u16, &memory),
                    if bits & (0x80 >> x) != 0 { fg } else { 0 },
                    "selected graphics font mode={mode:02x} service={service:02x} scan={cy} x={x}"
                );
            }
        }
        // CGA lower glyphs stay on the embedded ROM font; other modes
        // reset a custom selection when a new BIOS video mode is selected.
        if !cga {
            cpu.regs.set_ax(u16::from(mode));
            bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
        }
        let default_height = match mode {
            0x0F | 0x10 => 14,
            0x11 | 0x12 => 16,
            _ => 8,
        };
        let default_font = if default_height == 8 {
            gp64_hw::vga::CP437_FONT_8X8.as_slice()
        } else {
            gp64_hw::vga::CP437_FONT_8X16.as_slice()
        };
        let default_stride = if default_height == 8 { 8 } else { 16 };
        vga.set_cursor(0, 1, 2);
        vga.write_char_attr(0, b'Q', 15, 1, &mut memory);
        for cy in 0..default_height {
            for x in 0..8 {
                let bits = default_font
                    [usize::from(b'Q') * default_stride + cy + usize::from(default_height == 14)];
                assert_eq!(
                    vga.read_pixel(0, 16 + x as u16, (default_height + cy) as u16, &memory),
                    if bits & (0x80 >> x) != 0 { fg } else { 0 },
                    "default ROM glyph mode={mode:02x} scan={cy} x={x}"
                );
            }
        }
        if !cga {
            // Direct VGA users without an IVT/BDA retain mode defaults.
            memory.write_u16(0x43 * 4, 0);
            memory.write_u16(0x43 * 4 + 2, 0);
            memory.write_u16(0x485, 5);
            vga.set_cursor(0, 1, 3);
            vga.write_char_attr(0, b'Q', 15, 1, &mut memory);
            for cy in 0..default_height {
                for x in 0..8 {
                    let bits = default_font[usize::from(b'Q') * default_stride + cy];
                    assert_eq!(
                        vga.read_pixel(0, 24 + x as u16, (default_height + cy) as u16, &memory),
                        if bits & (0x80 >> x) != 0 { fg } else { 0 },
                        "null-vector fallback mode={mode:02x} scan={cy} x={x}"
                    );
                }
            }
        }
    }
}

#[test]
fn graphics_selected_font_ah09() {
    graphics_selected_font_output(9, false);
}
#[test]
fn graphics_selected_font_ah0a() {
    graphics_selected_font_output(10, false);
}
#[test]
fn graphics_selected_font_ah0e() {
    graphics_selected_font_output(14, false);
}
#[test]
fn graphics_selected_font_ah13() {
    graphics_selected_font_output(19, false);
}
#[test]
fn graphics_selected_font_cga_ah09() {
    graphics_selected_font_output(9, true);
}
#[test]
fn graphics_selected_font_cga_ah0a() {
    graphics_selected_font_output(10, true);
}
#[test]
fn graphics_selected_font_cga_ah0e() {
    graphics_selected_font_output(14, true);
}
#[test]
fn graphics_selected_font_cga_ah13() {
    graphics_selected_font_output(19, true);
}

#[test]
fn graphics_selected_font_rows_and_scroll() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);
    cpu.regs.set_ax(0x0013);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    cpu.set_es_real_mode(0x2600);
    cpu.regs.set_bp(0);
    memory.write_u8(0x26000 + usize::from(b'Z') * 5, 0x81);
    cpu.regs.set_ax(0x1121);
    cpu.regs.set_cx(5);
    cpu.regs.set_bx(0);
    cpu.regs.set_dx(40);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    cpu.regs.set_ax(0x0200);
    cpu.regs.set_dx(0x2300);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    cpu.regs.set_ax(0x0E5A);
    cpu.regs.set_bl(15);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(
        vga.read_pixel(0, 0, 175, &memory),
        15,
        "custom row35 must not clamp to mode13 row24"
    );
    cpu.regs.set_ax(0x1121);
    cpu.regs.set_bl(0);
    cpu.regs.set_dx(2);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    vga.write_pixel(0, 20, 7, 6, &mut memory);
    cpu.regs.set_ax(0x0200);
    cpu.regs.set_dx(0x0100);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    cpu.regs.set_ax(0x0E0A);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(vga.get_cursor(0), (1, 0), "custom two-row grid must scroll");
    assert_eq!(
        vga.read_pixel(0, 20, 2, &memory),
        6,
        "scroll must use selected five-row height"
    );
    // Oversized/zero accepted metrics stay bounded and keep font stride.
    for (height, stride) in [(255, 255), (0, 8)] {
        cpu.regs.set_ax(0x1121);
        cpu.regs.set_cx(height);
        cpu.regs.set_dx(0);
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
        memory.write_u8(0x26000 + usize::from(b'V') * stride, 0x81);
        vga.set_cursor(0, 0, 0);
        cpu.regs.set_ax(0x0956);
        cpu.regs.set_bx(15);
        cpu.regs.set_cx(1);
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
        assert_eq!(
            vga.read_pixel(0, 0, 0, &memory),
            15,
            "height={height} must retain selected glyph stride"
        );
        assert_eq!(
            vga.read_pixel(0, 1, 0, &memory),
            0,
            "height={height} selected glyph background"
        );
    }
    cpu.regs.set_ax(0x1121);
    cpu.regs.set_bx(0);
    cpu.regs.set_cx(5);
    cpu.regs.set_dx(2);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    vga.write_port(0x3D4, 0x09);
    vga.write_port(0x3D5, 0x40);
    vga.write_port(0x3C4, 0x04);
    vga.write_port(0x3C5, 0x06);
    assert!(vga.is_mode_x());
    vga.set_cursor(0, 0, 0);
    cpu.regs.set_ax(0x0E5A);
    cpu.regs.set_bl(15);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(memory.plane(0)[0], 15, "Mode X teletype must select INT43");
    assert_eq!(memory.plane(1)[0], 0, "Mode X selected glyph background");
    // Built-in graphics selections also replace the cached BIOS row grid.
    for (selection, height) in [(0x1122, 14), (0x1123, 8), (0x1124, 16)] {
        cpu.regs.set_ax(0x1121);
        cpu.regs.set_dx(40);
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
        cpu.regs.set_ax(selection);
        cpu.regs.set_bx(0);
        cpu.regs.set_dx(2);
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
        vga.set_cursor(0, 1, 0);
        cpu.regs.set_ax(0x0E0A);
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
        assert_eq!(
            vga.get_cursor(0),
            (1, 0),
            "ROM font selection {selection:04x} row grid"
        );
        assert_eq!(memory.read_u16(0x485), height);
    }
}
