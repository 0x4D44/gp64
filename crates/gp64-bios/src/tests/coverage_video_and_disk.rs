use super::*;

// --- DCC set_dcc_index with swap_bytes match (line 479) ---
#[test]
fn test_set_dcc_index_swap_bytes_match() {
    let mut memory = Memory::new();
    // DCC_ENTRIES[1] = 0x0001 (MDA, no alternate)
    // Passing 0x0100 should match via swap_bytes path
    let result = Bios::set_dcc_index(&mut memory, 0x0100);
    assert!(result);
    assert_eq!(memory.read_u8(0x48A), 1); // Index 1
}

// --- grayscale_triplet overflow (line 501) ---
#[test]
fn test_grayscale_triplet_overflow_clamped() {
    // With r=0x3F, g=0x3F, b=0x3F: 77*63+151*63+28*63 = 16128
    // (16128 + 128) >> 8 = 63 = 0x3F (exactly at boundary)
    let (r, g, b) = Bios::grayscale_triplet(0x3F, 0x3F, 0x3F);
    assert_eq!(r, 0x3F);
    assert_eq!(g, 0x3F);
    assert_eq!(b, 0x3F);
}

#[test]
fn test_grayscale_triplet_over_clamp() {
    // With large green (high weight=151): r=0x10, g=0x3F, b=0x10
    // 77*16 + 151*63 + 28*16 = 1232 + 9513 + 448 = 11193
    // (11193+128)>>8 = 44 = 0x2C, under 0x3F - not enough
    // Need all max: r=0x3F g=0x3F b=0x3F = 63 as shown above
    // To get > 0x3F, we'd need inputs > 0x3F which are u8 clamped
    // Actually the function takes u8 inputs. Max sum with 0x3F = 63.
    // But wait - the grayscale function in set_all_palette uses DAC 6-bit values
    // which go up to 0x3F. If someone passes 0xFF:
    // 77*255 + 151*255 + 28*255 = 65280 → (65280+128)>>8 = 255 > 0x3F → clamp!
    let (r, g, b) = Bios::grayscale_triplet(0xFF, 0xFF, 0xFF);
    assert_eq!(r, 0x3F);
    assert_eq!(g, 0x3F);
    assert_eq!(b, 0x3F);
}

// --- INT 10h AH=0Bh: CGA palette set bh==0x01 (line 898) ---
#[test]
fn test_int10_cga_palette_set_bh01() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x0B);
    cpu.regs.set_bh(0x01);
    cpu.regs.set_bl(0x01);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let color_select = vga.get_cga_color_select();
    assert_ne!(color_select & 0x20, 0);
}

// --- INT 10h AH=10h AL=01: set border color (line 985) ---
#[test]
fn test_int10_set_border_color_via_ah10_al01() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x01);
    cpu.regs.set_bh(0x07);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let attr = vga.get_attribute_regs();
    assert!(attr.len() > 0x11, "VGA should have enough attribute regs");
    assert_eq!(attr[0x11], 0x07);
}

// --- INT 10h AH=10h AL=02: set all palette registers (line 999) ---
#[test]
fn test_int10_set_all_palette_regs_b2() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    let table_addr = 0x5000;
    cpu.regs.es = 0x0500;
    cpu.regs.set_dx(0x0000);
    for i in 0..16u8 {
        memory.write_u8(table_addr + i as usize, i + 1);
    }
    memory.write_u8(table_addr + 16, 0x0A);

    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x02);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    let attr = vga.get_attribute_regs();
    assert!(attr.len() > 0x11, "VGA should have enough attribute regs");
    assert_eq!(attr[0x11], 0x0A);
}

// --- INT 10h AH=13h: write string that wraps and scrolls (line 1395) ---
#[test]
fn test_int10_write_string_wrap_and_scroll() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x03);
    memory.write_u8(0x449, 0x03);
    memory.write_u8(0x484, 24);

    const PAGE0: usize = 0xB8000;
    const PAGE1: usize = PAGE0 + 0x1000;
    memory.write_u8(PAGE0, b'A');
    memory.write_u8(PAGE0 + 1, 0x1A);
    memory.write_u8(PAGE0 + 80 * 2, b'B');
    memory.write_u8(PAGE0 + 80 * 2 + 1, 0x2B);

    cpu.regs.set_ah(0x13);
    cpu.regs.set_al(0x01);
    cpu.regs.set_bh(0x01);
    cpu.regs.set_bl(0x0F);
    cpu.regs.set_cx(81);
    cpu.regs.set_dh(24);
    cpu.regs.set_dl(0);

    let string_addr = 0x6000;
    cpu.regs.es = 0x0600;
    cpu.regs.set_bp(0x0000);
    for i in 0..81 {
        memory.write_u8(string_addr + i, b'X');
    }

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(
        (memory.read_u8(PAGE0), memory.read_u8(PAGE0 + 1)),
        (b'A', 0x1A),
        "inactive-page write must not scroll active page row 0"
    );
    assert_eq!(
        (
            memory.read_u8(PAGE0 + 80 * 2),
            memory.read_u8(PAGE0 + 80 * 2 + 1),
        ),
        (b'B', 0x2B),
        "inactive-page write must not scroll active page row 1"
    );
    for col in 0..80 {
        let addr = PAGE1 + (23 * 80 + col) * 2;
        assert_eq!(
            (memory.read_u8(addr), memory.read_u8(addr + 1)),
            (b'X', 0x0F),
            "requested page's former bottom row must move to row 23"
        );
    }
    assert_eq!(
        (
            memory.read_u8(PAGE1 + 24 * 80 * 2),
            memory.read_u8(PAGE1 + 24 * 80 * 2 + 1)
        ),
        (b'X', 0x0F),
        "81st character must start the cleared bottom row"
    );
    assert_eq!(
        (
            memory.read_u8(PAGE1 + (24 * 80 + 1) * 2),
            memory.read_u8(PAGE1 + (24 * 80 + 1) * 2 + 1),
        ),
        (b' ', 0x07),
        "remaining bottom-row cells must use the default teletype fill attribute"
    );
    assert_eq!(vga.get_cursor(1), (24, 1));
}

// --- Video state function info: attr blink flag (line 1603) ---
#[test]
fn test_int10_video_state_func_info_blink() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    let mut attr = vga.get_attribute_regs();
    assert!(attr.len() > 0x10);
    attr[0x10] |= 0x08;
    vga.set_attribute_regs(attr);

    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(0x0000);
    cpu.regs.es = 0x0800;
    cpu.regs.set_di(0x0000);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x1B);
    let buffer_addr = 0x8000;
    let misc_flags = memory.read_u8(buffer_addr + 0x2D);
    assert_ne!(misc_flags & 0x20, 0, "Blink flag should be set");
}

// --- Video state save with DAC state == 0 (line 1759) ---
#[test]
fn test_int10_video_state_save_dac_state0() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x01);
    cpu.regs.set_cx(0x04);
    cpu.regs.es = 0x0900;
    cpu.regs.set_bx(0x0000);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x1C);
}

// --- INT 16h AH=01: peek with E0 replacement (line 2928) ---
#[test]
fn test_int16_peek_e0_replacement() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    // Set up IRET frame on stack
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100); // Return IP
    memory.write_u16(0x1002, 0x0000); // Return CS
    memory.write_u16(0x1004, 0x0200); // Flags

    // Enqueue a key with ASCII=0xE0 (extended key marker)
    bda::kbd_enqueue(&mut memory, 0x48, 0xE0); // Up arrow

    // AH=01h: Always redirects to assembler peek stub
    // (E0 replacement is now handled by the stub, not HLE)
    cpu.regs.set_ah(0x01);
    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    match result.unwrap() {
        Int16Disp::Stub(offset) => assert_eq!(offset, Bios::INT16_PEEK_01),
        Int16Disp::Iret => panic!("expected Stub, got Iret"),
    }
}

// --- INT 16h AH=05: store keycode success (line 2959) ---
#[test]
fn test_int16_store_keycode_success() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    // Set up IRET frame
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0x0000);
    memory.write_u16(0x1004, 0x0200);

    // AH=05h: Store keycode
    cpu.regs.set_ah(0x05);
    cpu.regs.set_ch(0x1E); // Scancode 'A'
    cpu.regs.set_cl(0x41); // ASCII 'A'

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0); // Success
}

// --- INT 16h AH=13: enhanced write success (line 2970) ---
#[test]
fn test_int16_enhanced_write_success() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0x0000);
    memory.write_u16(0x1004, 0x0200);

    // AH=13h: Enhanced write to keyboard buffer
    cpu.regs.set_ah(0x13);
    cpu.regs.set_ch(0x1E);
    cpu.regs.set_cl(0x41);

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x00);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

// --- INT 15h AH=86h: wait nonzero (line 3601 - false branch of env var check) ---
#[test]
fn test_int15_wait_nonzero_cycles() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up IRET frame
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0x0000);
    memory.write_u16(0x1004, 0x0200);

    // AH=86h: Wait
    cpu.regs.set_ah(0x86);
    cpu.regs.set_cx(0x0001); // High word of microseconds
    cpu.regs.set_dx(0x0000); // Low word

    let cycles_before = cpu.cycles;
    let result = bios.int15(&mut cpu, &mut memory, None);
    assert_eq!(result.unwrap().0, Some(0x1_0000));
    assert_eq!(cpu.regs.ah(), 0x00);
    assert_eq!(cpu.cycles, cycles_before);
}

// --- INT 15h AX=E801h branches (lines 3685, 3692) ---
#[test]
fn test_int15_e801_extended_memory() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Truncate to ~1.1MB to test E801 with small extended memory
    memory.resize_ram(gp64_memory::TOTAL_SIZE);

    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0x0000);
    memory.write_u16(0x1004, 0x0200);

    // AH=E8h, AL=01h: Get extended memory size
    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0x01);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    // Memory is ~1.1MB, so mem_1_to_16_kb should be nonzero
    // 0x110000 - 0x100000 = 0x10000 = 64KB = 64 KB
    let mem_1_to_16_kb = cpu.regs.ax();
    assert!(
        mem_1_to_16_kb > 0,
        "Should have extended memory between 1-16MB"
    );
    // No memory above 16MB
    assert_eq!(cpu.regs.bx(), 0);
}

// --- INT 15h AX=E881h branches (lines 3716, 3723) ---
#[test]
fn test_int15_e881_extended_memory_32bit() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0x0000);
    memory.write_u16(0x1004, 0x0200);

    // AH=E8h, AL=81h: Get extended memory size (32-bit)
    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0x81);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert!(cpu.regs.eax > 0);
}

// --- INT 15h AH=BF: extender kernel API — unsupported on a real BIOS ---
#[test]
fn test_int15_bf_unsupported_like_real_bios() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0x0000);
    memory.write_u16(0x1004, 0x0200);

    cpu.regs.set_ah(0xBF);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0x86);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

// --- Font loading: load_font_to_plane2_block boundary (lines 4501, 4506) ---
#[test]
fn test_load_font_to_plane2_block_boundaries() {
    let mut memory = Memory::new();
    // Create a small font (8 chars, height 16)
    let font = vec![0xAA; 8 * 16];

    // Load to block 0, starting at char 0, count 8
    Bios::load_font_to_plane2_block(&mut memory, &font, 16, 0, 0, 8);

    let plane2 = memory.plane_mut(2);
    // First char should have font data
    assert_eq!(plane2[0], 0xAA);
    // Rows beyond font height should be 0
    assert_eq!(plane2[16], 0x00);
}

#[test]
fn test_load_font_to_plane2_block_with_block_offset() {
    let mut memory = Memory::new();
    let font = vec![0xBB; 256 * 16]; // Full 256-char font, height 16

    // Load to block 1
    Bios::load_font_to_plane2_block(&mut memory, &font, 16, 1, 0, 256);

    let plane2 = memory.plane_mut(2);
    // Native selector 1 starts at 0x4000, not the adjacent 8KB bank.
    assert_eq!(plane2[0x4000], 0xBB);
    assert_eq!(plane2[0x2000], 0);
}

// --- Font loading: load_font_to_plane2 with short font data (line 4464) ---
#[test]
fn test_load_font_to_plane2_short_font() {
    let mut memory = Memory::new();
    // Font with only 2 bytes (less than char_height * 256)
    let font = vec![0xCC; 2];

    Bios::load_font_to_plane2_block(&mut memory, &font, 16, 0, 0, 256);

    let plane2 = memory.plane_mut(2);
    assert_eq!(plane2[0], 0xCC);
    assert_eq!(plane2[1], 0xCC);
    // Beyond font data should be untouched (0)
}

// --- load_user_font_to_plane2 with boundary check (line 4544) ---
#[test]
fn test_load_user_font_boundary_check() {
    let mut memory = Memory::new();
    let src_addr = 0x5000;

    // Write font data for 2 chars, height 8
    for i in 0..16 {
        memory.write_u8(src_addr + i, 0xDD);
    }

    Bios::load_user_font_to_plane2(&mut memory, src_addr, 2, 8, 0, 0);

    let plane2 = memory.plane_mut(2);
    assert_eq!(plane2[0], 0xDD);
    // Rows 8-31 should be 0 (padded)
    assert_eq!(plane2[8], 0x00);
}

// --- EGA RIL read_range with start >= regs (line 2056 false branch) ---
#[test]
fn test_ega_ril_read_range_start_beyond_regs_b2() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0xF1);
    cpu.regs.set_ch(0xFF);
    cpu.regs.set_cl(0x01);
    cpu.regs.set_dx(0x03D4);
    cpu.regs.es = 0x0C00;

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

// --- EGA RIL define_defaults (line 2114) ---
#[test]
fn test_ega_ril_define_defaults_b2() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    let buffer_addr = 0x7000;
    cpu.regs.es = 0x0700;
    cpu.regs.set_bx(0x0000);
    for i in 0..25u8 {
        memory.write_u8(buffer_addr + i as usize, i + 10);
    }

    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0xF3);
    cpu.regs.set_dx(0x03D4);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

// --- EGA RIL revert_defaults (line 2006 area) ---
#[test]
fn test_ega_ril_revert_defaults_b2() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    let buffer_addr = 0x7000;
    cpu.regs.es = 0x0700;
    cpu.regs.set_bx(0x0000);
    for i in 0..25u8 {
        memory.write_u8(buffer_addr + i as usize, i + 10);
    }
    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0xF3);
    cpu.regs.set_dx(0x03D4);
    let _ = bios.int10(&mut cpu, &mut memory, &mut vga);

    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0xF4);
    cpu.regs.set_dx(0x03D4);
    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

// --- EGA RIL read/write register set (lines 2091, 2102) ---
#[test]
fn test_ega_ril_read_register_set_b2() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    let buffer_addr = 0x7000;
    cpu.regs.es = 0x0700;
    cpu.regs.set_bx(0x0000);
    memory.write_u16(buffer_addr, 0x03D4);
    memory.write_u8(buffer_addr + 2, 0x00);
    memory.write_u8(buffer_addr + 3, 0x00);

    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0xF5);
    cpu.regs.set_cx(1);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

#[test]
fn test_ega_ril_write_register_set_b2() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    let buffer_addr = 0x7000;
    cpu.regs.es = 0x0700;
    cpu.regs.set_bx(0x0000);
    memory.write_u16(buffer_addr, 0x03D4);
    memory.write_u8(buffer_addr + 2, 0x00);
    memory.write_u8(buffer_addr + 3, 0x42);

    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0xF6);
    cpu.regs.set_cx(1);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

// --- INT 10h AH=12h BL=31h: enable/disable ---
#[test]
fn test_int10_bl31_enable_default_palette() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x31);
    cpu.regs.set_al(0x00);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x12);
}

// --- INT 10h AH=12h BL=33h: enable/disable grayscale ---
#[test]
fn test_int10_bl33_enable_grayscale() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x33);
    cpu.regs.set_al(0x00);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x12);
}

// --- INT 10h AH=12h BL=34h: cursor emulation ---
#[test]
fn test_int10_bl34_cursor_emulation() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x34);
    cpu.regs.set_al(0x01);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x12);
}

// --- INT 10h AH=12h BL=36h: screen refresh control ---
#[test]
fn test_int10_bl36_screen_refresh_disable() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x36);
    cpu.regs.set_al(0x01);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x12);
}

// --- INT 10h AH=13h write string with attr-in-string mode ---
#[test]
fn test_int10_write_string_mode2_with_attrs() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(0x03);
    memory.write_u8(0x449, 0x03);
    memory.write_u8(0x484, 24);

    cpu.regs.set_ah(0x13);
    cpu.regs.set_al(0x02);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_bl(0x07);
    cpu.regs.set_cx(2);
    cpu.regs.set_dh(0);
    cpu.regs.set_dl(0);

    let string_addr = 0x6000;
    cpu.regs.es = 0x0600;
    cpu.regs.set_bp(0x0000);
    memory.write_u8(string_addr, b'H');
    memory.write_u8(string_addr + 1, 0x0E);
    memory.write_u8(string_addr + 2, b'i');
    memory.write_u8(string_addr + 3, 0x0A);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
}

// --- INT 16h store keycode buffer full (line 2961) ---
#[test]
fn test_int16_store_keycode_buffer_full_b3() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    bios.init_ivt(&mut memory, false);

    // Fill the keyboard buffer (16 entries max)
    for _ in 0..16 {
        bda::kbd_enqueue(&mut memory, 0x1E, 0x61);
    }

    cpu.regs.set_ah(0x05);
    cpu.regs.set_ch(0x1E);
    cpu.regs.set_cl(0x61);

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 1); // Buffer full
}

// --- INT 16h enhanced write buffer full (line 2974) ---
#[test]
fn test_int16_enhanced_write_buffer_full_b3() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    bios.init_ivt(&mut memory, false);

    // Fill the keyboard buffer
    for _ in 0..16 {
        bda::kbd_enqueue(&mut memory, 0x1E, 0x61);
    }

    cpu.regs.set_ah(0x13);
    cpu.regs.set_ch(0x48);
    cpu.regs.set_cl(0x00);

    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x01); // Buffer full
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

// --- INT 10h write string that scrolls screen (line 1395) ---
#[test]
fn test_int10_write_string_scrolls_up_b3() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set text mode
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x03);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // Write 2000 chars (25 rows * 80 cols = 2000, fills entire screen)
    // Start at row 24, col 79 - writing 3 chars forces wrap + scroll
    let str_seg = 0x3000u16;
    let str_off = 0x0000u16;
    let str_addr = Memory::seg_offset_to_linear(str_seg, str_off);
    for i in 0..3 {
        memory.write_u8(str_addr + i, b'Z');
    }

    cpu.regs.set_ah(0x13);
    cpu.regs.set_al(0x00);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_bl(0x07);
    cpu.regs.set_cx(3);
    cpu.regs.set_dh(24); // last row
    cpu.regs.set_dl(79); // last col
    cpu.regs.es = str_seg;
    cpu.regs.set_bp(str_off);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

// --- days_to_ymd with edge date (line 4595: z < 0 path) ---
#[test]
fn test_days_to_ymd_epoch_b3() {
    // Day 0 = 1970-01-01
    let (y, m, d) = super::days_to_ymd(0);
    assert_eq!((y, m, d), (1970, 1, 1));
}

// --- EGA RIL write_register with out-of-range reg (line 2017) ---
#[test]
fn test_ega_ril_write_register_out_of_range_b3() {
    let mut vga = Vga::new();
    // CRTC has 25 regs; reg 30 is out of range - should be no-op
    Bios::ega_ril_write_register(&mut vga, 30, 0xFF, 0x00);
    // Should not panic or corrupt
}

// --- EGA RIL read_range with start >= regs (line 2056 second condition) ---
#[test]
fn test_ega_ril_read_range_start_past_end_b3() {
    let cpu = Cpu::new();
    let mut memory = Memory::new();
    let vga = Vga::new();

    // CRTC has 25 regs; start=30 is past end
    Bios::ega_ril_read_range(&cpu, &mut memory, &vga, 30, 2, 0x00);
}

// --- EGA RIL write_range with start >= regs (line 2079 second condition) ---
#[test]
fn test_ega_ril_write_range_start_past_end_b3() {
    let cpu = Cpu::new();
    let memory = Memory::new();
    let mut vga = Vga::new();

    Bios::ega_ril_write_range(&cpu, &memory, &mut vga, 30, 2, 0x00);
}

// --- EGA RIL define_defaults with invalid group (line 2114) ---
#[test]
fn test_ega_ril_define_defaults_invalid_group_b3() {
    let _cpu = Cpu::new();
    let memory = Memory::new();
    let mut vga = Vga::new();

    Bios::ega_ril_define_defaults(&Cpu::new(), &memory, &mut vga);
    // With dx=0 (default), this should work. Test with invalid:
    let mut cpu2 = Cpu::new();
    cpu2.regs.set_dx(0xFF); // Invalid group
    Bios::ega_ril_define_defaults(&cpu2, &memory, &mut vga);
}

// --- INT 10h video_state_save with DAC read state (line 1759: dac_state != 0) ---
#[test]
fn test_int10_video_state_save_dac_read_state_b3() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set mode 3 first
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x03);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // Set DAC read index to trigger dac_state == 1 path
    vga.set_dac_read_index(42);

    // Save state with DAC flag (0x04)
    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x01); // save
    cpu.regs.set_cx(0x04); // DAC state only
    cpu.regs.es = 0x0900;
    cpu.regs.set_bx(0x0000);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x1C);
}

// --- Font loading: short font (line 4464: font_offset + row >= font.len()) ---
#[test]
fn test_load_font_to_plane2_short_font_b3() {
    let mut memory = Memory::new();

    // Font with only 4 rows per char instead of 8
    let char_height = 8;
    let font = vec![0xCC; 256 * 4]; // Only 4 bytes per char, but char_height=8
    Bios::load_font_to_plane2_block(&mut memory, &font, char_height, 0, 0, 256);

    let plane2 = memory.plane_mut(2);
    // First 4 rows should be 0xCC, rows 4-7 should be 0x00 (not written)
    assert_eq!(plane2[0], 0xCC);
    assert_eq!(plane2[3], 0xCC);
    // Row 4 stays at whatever was there (0 if plane was zeroed)
}

// --- load_font_to_plane2_block: font_offset + row >= font.len() (line 4501) ---
#[test]
fn test_load_font_to_plane2_block_short_font_b3() {
    let mut memory = Memory::new();
    let font = vec![0xDD; 10]; // Very short font
    Bios::load_font_to_plane2_block(&mut memory, &font, 16, 0, 0, 1);
}

// --- update_text_metrics_for_font: adjust_layout with graphics mode (line 4595) ---
#[test]
fn test_update_text_metrics_adjust_in_mode13h_b3() {
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    memory.write_u8(0x449, 0x13); // Mode 13h
    Bios::update_text_metrics_for_font(&mut memory, &mut vga, 14, true);
    assert_eq!(memory.read_u8(0x485), 14);
}

// --- INT 10h 0x0B color select: BDA mode > 0x07 (line 907 else) ---
#[test]
fn test_int10_color_select_no_bda_update_b3() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set BDA mode to > 0x07
    memory.write_u8(0x449, 0x13);

    cpu.regs.set_ah(0x0B);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_bl(0x05);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    // Should NOT update BDA 0x466 for modes > 0x07
}

// --- INT 10h functionality state: no blink flag (line 1603 false) ---
// Attr reg 0x10 bit 3 = 0 means blinking disabled
#[test]
fn test_int10_functionality_state_no_blink_b3() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Ensure blink bit is clear
    let mut attr = vga.get_attribute_regs();
    attr[0x10] &= !0x08;
    vga.set_attribute_regs(attr);

    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(0x0000);
    cpu.regs.es = 0x0800;
    cpu.regs.set_di(0x0000);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    let buf_addr = Memory::seg_offset_to_linear(0x0800, 0x0000);
    let misc = memory.read_u8(buf_addr + 0x2D);
    assert_eq!(misc & 0x20, 0); // No blink
}

// --- INT 10h AH=0Bh: bh >= 2 (neither 0x00 nor 0x01, line 898 else) ---
#[test]
fn test_int10_cga_bh_other_b4() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x0B);
    cpu.regs.set_bh(0x02); // Neither background nor palette
    cpu.regs.set_bl(0x05);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

// --- INT 10h video_state_save with DAC in write mode (line 1759 dac_state==0) ---
// Default DAC state is write mode (dac_state = 0). The test already covers this.
// The uncovered branch is dac_state != 0 (read mode).
#[test]
fn test_int10_video_state_save_dac_state1_b4() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set mode 3 to initialize BDA
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x03);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // Force DAC into read mode (dac_state = 1)
    vga.set_dac_state(1);
    vga.set_dac_read_index(10);

    // Save state with DAC + full state
    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x01); // save
    cpu.regs.set_cx(0x07); // all state flags
    cpu.regs.es = 0x0900;
    cpu.regs.set_bx(0x0000);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x1C);
}

// --- font loading: row < height but font_offset + row >= font.len() (line 4501) ---
#[test]
fn test_load_font_to_plane2_block_very_short_b4() {
    let mut memory = Memory::new();
    let font = vec![0xEE; 5]; // Only 5 bytes total
                              // char_height=16, count=1. For char 0: font_offset=0, rows 0-4 have data, 5-15 don't
    Bios::load_font_to_plane2_block(&mut memory, &font, 16, 0, 0, 1);

    let plane2 = memory.plane_mut(2);
    assert_eq!(plane2[0], 0xEE);
    assert_eq!(plane2[4], 0xEE);
    assert_eq!(plane2[5], 0); // Beyond font data, should be 0
}

#[test]
fn test_cov_e801_with_max_ram() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    memory.ensure_ram_size(16 * 1024 * 1024);
    let ram_bytes = memory.ram_size() as u64;
    assert!(ram_bytes > 0x0010_0000);

    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0x01); // E801h
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let expected_1_to_16 = ((ram_bytes - 0x0010_0000).min(0x00F0_0000) / 1024) as u16;
    assert_eq!(cpu.regs.ax(), expected_1_to_16);
    assert_eq!(cpu.regs.cx(), expected_1_to_16);
}

#[test]
fn test_cov_e881_with_max_ram() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    memory.ensure_ram_size(16 * 1024 * 1024);
    let ram_bytes = memory.ram_size() as u64;

    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0x81); // E881h
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let expected_1_to_16 = ((ram_bytes - 0x0010_0000).min(0x00F0_0000) / 1024) as u32;
    assert_eq!(cpu.regs.eax, expected_1_to_16);
}

#[test]
fn test_cov_int16_peek_e0_ah11_no_replacement() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    // Set up IRET frame
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0x0000);
    memory.write_u16(0x1004, 0x0200);

    bda::kbd_enqueue(&mut memory, 0x48, 0xE0);

    // AH=11h: Always redirects to assembler peek stub
    // (E0 pass-through is now handled by the stub, not HLE)
    cpu.regs.set_ah(0x11);
    let result = bios.int16(&mut cpu, &mut memory, &mut keyboard);
    assert!(result.is_ok());
    match result.unwrap() {
        Int16Disp::Stub(offset) => assert_eq!(offset, Bios::INT16_PEEK_11),
        Int16Disp::Iret => panic!("expected Stub, got Iret"),
    }
}

#[test]
fn test_cov_int10_set_mode_13h_no_cga() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set mode 0x13 (mode 13h) - mode > 7, so CGA control register branch is skipped
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x13);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());
    // Mode 13h should have been set
    assert_eq!(memory.read_u8(0x449), 0x13);
}

#[test]
fn test_cov_e820_with_max_ram() {
    let bios = Bios::new();
    let mut memory = Memory::new();
    memory.ensure_ram_size(16 * 1024 * 1024);

    let (entries, count) = bios.generate_e820_map(&memory);
    assert!(count >= 3); // base, video/ROM, extended
    let ext = &entries[2];
    assert_eq!(ext.base, 0x0010_0000);
    assert!(ext.length > 0); // Extended memory present
}

#[test]
fn test_cov_e820_traversal_max_ram() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    memory.ensure_ram_size(16 * 1024 * 1024);

    let buffer_seg = 0x1000u16;
    let buffer_off = 0x0000u16;
    let smap = 0x534D4150u32;
    let mut continuation = 0u32;
    let mut found_extended = false;
    let mut entry_count = 0;

    loop {
        cpu.regs.set_ah(0xE8);
        cpu.regs.set_al(0x20); // E820h
        cpu.regs.ebx = continuation;
        cpu.regs.ecx = 20;
        cpu.regs.edx = smap;
        cpu.regs.es = buffer_seg;
        cpu.regs.set_di(buffer_off);

        bios.int15(&mut cpu, &mut memory, None).unwrap();

        let linear_addr = (buffer_seg as u32 * 16 + buffer_off as u32) as usize;
        let base = memory.read_u64(linear_addr);
        let _length = memory.read_u64(linear_addr + 8);
        let mem_type = memory.read_u32(linear_addr + 16);

        if base == 0x10_0000 {
            assert_eq!(
                mem_type, 1,
                "extended memory region should be type 1 (usable)"
            );
            found_extended = true;
        }

        continuation = cpu.regs.ebx;
        entry_count += 1;
        if continuation == 0 {
            break;
        }
    }

    assert!(found_extended, "Should find extended memory region");
    assert!(entry_count >= 3);
}

#[test]
fn test_cov_ega_ril_write_invalid_group() {
    let mut vga = Vga::new();
    // DX=0x1234 doesn't map to any valid RIL group
    Bios::ega_ril_write_register(&mut vga, 0, 0xFF, 0x1234);
    // Should return without crash (None from ega_ril_group)
}

#[test]
fn test_cov_int10_set_border_color() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // AH=10h, AL=01h: Set border color
    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x01);
    cpu.regs.set_bh(0x05); // Border color = 5

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    let attr = vga.get_attribute_regs();
    assert_eq!(attr[0x11], 0x05);
}

#[test]
fn test_cov_int10_set_all_palette_and_border() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set up palette table in memory
    let table_addr = 0x5000usize;
    for i in 0..16u8 {
        memory.write_u8(table_addr + i as usize, i + 1);
    }
    memory.write_u8(table_addr + 16, 0x0A); // border color

    // AH=10h, AL=02h: Set all palette registers and border
    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x02);
    cpu.regs.es = (table_addr / 16) as u16;
    cpu.regs.set_dx(0);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    let attr = vga.get_attribute_regs();
    assert_eq!(attr[0x11], 0x0A);
}

#[test]
fn test_cov_toggle_blinking() {
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Enable blinking
    Bios::toggle_blinking(&mut memory, &mut vga, 1);
    let attr = vga.get_attribute_regs();
    assert_ne!(attr[0x10] & 0x08, 0);

    // Disable blinking
    Bios::toggle_blinking(&mut memory, &mut vga, 0);
    let attr = vga.get_attribute_regs();
    assert_eq!(attr[0x10] & 0x08, 0);

    // Invalid state > 1: should do nothing
    Bios::toggle_blinking(&mut memory, &mut vga, 2);
}

#[test]
fn test_cov_select_dac_page_functions() {
    let mut vga = Vga::new();

    // Function 0, mode 0: clear paging mode
    Bios::select_dac_page(&mut vga, 0, 0);
    let (mode, _page) = Bios::get_dac_page(&vga);
    assert_eq!(mode, 0);

    // Function 0, mode 1: enable paging
    Bios::select_dac_page(&mut vga, 0, 1);
    let (mode, _page) = Bios::get_dac_page(&vga);
    assert_eq!(mode, 1);

    // Function 1: set page directly (paging mode enabled)
    Bios::select_dac_page(&mut vga, 1, 3);
    let (_mode, page) = Bios::get_dac_page(&vga);
    assert_eq!(page, 3);

    // Function 1: set page with paging mode disabled (page shifted left 2)
    Bios::select_dac_page(&mut vga, 0, 0); // Clear paging
    Bios::select_dac_page(&mut vga, 1, 2);
    let (_mode, page) = Bios::get_dac_page(&vga);
    assert_eq!(page, 2); // Page is shifted >> 2 on read when paging disabled
}

#[test]
fn test_cov_save_restore_functionality_state() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set up attr reg 0x10 bit 3 for P10 blinking mode
    let mut attr = vga.get_attribute_regs();
    attr[0x10] |= 0x08;
    vga.set_attribute_regs(attr);

    // AH=1Bh: Return functionality/state information
    let buf_seg = 0x5000u16;
    let buf_off = 0x0000u16;
    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(0);
    cpu.regs.es = buf_seg;
    cpu.regs.set_di(buf_off);

    let result = bios.int10(&mut cpu, &mut memory, &mut vga);
    assert!(result.is_ok());

    let buf_addr = (buf_seg as usize) * 16 + (buf_off as usize);
    let misc_flags = memory.read_u8(buf_addr + 0x2D);
    // Bit 5 should be set because attr reg 0x10 bit 3 is set
    assert_ne!(misc_flags & 0x20, 0);
}

#[test]
fn test_cov_load_user_font_to_plane2() {
    let mut memory = Memory::new();

    // Set up a font in memory
    let src_addr = 0x6000usize;
    for i in 0..16u8 {
        memory.write_u8(src_addr + i as usize, 0xAA + i);
    }

    // Load 1 character at position 0
    Bios::load_user_font_to_plane2(&mut memory, src_addr, 1, 16, 0, 0);

    let plane2 = memory.plane_mut(2);
    assert_eq!(plane2[0], 0xAA);
    assert_eq!(plane2[15], 0xAA + 15);
    assert_eq!(plane2[16], 0); // Beyond height, padded with 0
}

#[test]
fn test_cov_memory_reporting_invariant_max_ram() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Truncate to exactly 16MB so cross-check (AH=88h == E801 1-16MB) holds
    memory.resize_ram(16 * 1024 * 1024);
    let ram_bytes = memory.ram_size() as u64;
    let extended_bytes = ram_bytes - 0x10_0000;

    // AH=88h
    cpu.regs.set_ah(0x88);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    let ah88_kb = cpu.regs.ax() as u64;

    // E801h
    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0x01);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    let e801_1to16_kb = cpu.regs.ax() as u64;

    let expected_1to16_kb = (extended_bytes.min(0xF00000)) / 1024;
    assert_eq!(e801_1to16_kb, expected_1to16_kb);

    // For RAM <= 16MB, AH=88h and E801h 1-16MB should agree
    assert_eq!(ah88_kb, e801_1to16_kb);
}

#[test]
fn test_cov_generate_e820_max_ram() {
    let bios = Bios::new();
    let mut memory = Memory::new();
    memory.ensure_ram_size(16 * 1024 * 1024);

    let (entries, count) = bios.generate_e820_map(&memory);

    assert_eq!(count, 3);
    assert_eq!(entries[2].base, 0x0010_0000);
    let expected_ext = memory.ram_size() as u64 - 0x0010_0000;
    assert_eq!(entries[2].length, expected_ext);
}

#[test]
fn test_cov_e881_max_ram_assertions() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    memory.ensure_ram_size(16 * 1024 * 1024);
    let ram_bytes = memory.ram_size() as u64;

    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0x81);
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let expected_1_to_16_kb = ((ram_bytes - 0x0010_0000).min(0x00F0_0000) / 1024) as u32;
    assert_eq!(cpu.regs.eax, expected_1_to_16_kb);
    assert!(expected_1_to_16_kb > 0);
}
