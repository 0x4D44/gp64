use super::*;

#[test]
fn test_cov_int10_dac_select_page() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // AH=10h, AL=13h: Select DAC page
    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x13);
    cpu.regs.set_bl(0); // Function 0: select paging mode
    cpu.regs.set_bh(1); // Mode 1: 4 groups of 64
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_int10_read_dac_register() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // AH=10h, AL=15h: Read DAC register
    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x15);
    cpu.regs.set_bl(0); // Register 0
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_int10_read_dac_block() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    let buf_addr = 0x5000usize;
    // AH=10h, AL=17h: Read DAC block
    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x17);
    cpu.regs.set_bx(0); // Start register
    cpu.regs.set_cx(4); // Count
    cpu.regs.es = (buf_addr / 16) as u16;
    cpu.regs.set_dx(0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_int10_set_pel_mask() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // AH=10h, AL=18h: Set PEL mask
    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x18);
    cpu.regs.set_bl(0xF0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(vga.get_pel_mask(), 0xF0);
}

#[test]
fn test_cov_int10_get_pel_mask() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_pel_mask(0xAB);
    // AH=10h, AL=19h: Get PEL mask
    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x19);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.bl(), 0xAB);
}

#[test]
fn test_cov_int10_get_dac_paging_mode() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // AH=10h, AL=1Ah: Get DAC paging mode
    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x1A);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_int10_grayscale_summing() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // First set some DAC values
    vga.set_dac_reg(0, 0x3F, 0x00, 0x00); // Pure red

    // AH=10h, AL=1Bh: Perform grayscale summing
    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x1B);
    cpu.regs.set_bx(0); // Start
    cpu.regs.set_cx(1); // Count
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_int10_read_palette_register() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set a palette register first
    vga.set_palette_reg(5, 0x2A);

    // AH=10h, AL=07h: Read individual palette register
    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x07);
    cpu.regs.set_bl(5);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.bh(), 0x2A);
}

#[test]
fn test_cov_int10_read_border_color() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // AH=10h, AL=08h: Read border (overscan) color
    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x08);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_int10_read_all_palette_regs() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    let buf_addr = 0x5000usize;
    // AH=10h, AL=09h: Read all palette registers + border
    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x09);
    cpu.regs.es = (buf_addr / 16) as u16;
    cpu.regs.set_dx(0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_int10_toggle_blinking_via_int() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // AH=10h, AL=03h: Toggle intensity/blinking
    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x03);
    cpu.regs.set_bl(1); // Enable blinking
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_int10_write_pixel() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // Set mode 13h first
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x13);
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0xF000);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // AH=0Ch: Write pixel
    cpu.regs.set_ah(0x0C);
    cpu.regs.set_al(0x0F); // Color
    cpu.regs.set_bh(0); // Page
    cpu.regs.set_cx(100); // X
    cpu.regs.set_dx(50); // Y
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_int10_video_state_get_buffer_size() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // AH=1Ch, AL=00h: Get buffer size
    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x00);
    cpu.regs.set_cx(0x07); // All state types
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x1C);
}

#[test]
fn test_cov_int10_hercules_detection() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // AH=EFh: Hercules driver Hercules detection
    cpu.regs.set_ah(0xEF);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.dx(), 0xFFFF);
}

#[test]
fn test_cov_int10_topview_shadow_buffer() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // AH=FEh: TopView/DESQview shadow buffer
    let orig_es = cpu.regs.es;
    let orig_di = cpu.regs.di();
    cpu.regs.set_ah(0xFE);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    // ES:DI should be unchanged
    assert_eq!(cpu.regs.es, orig_es);
    assert_eq!(cpu.regs.di(), orig_di);
}

#[test]
fn test_cov_int10_alternate_select_print_screen() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // BL=20h: Select alternate print screen routine
    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x20);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x12);
}

#[test]
fn test_cov_int10_alternate_select_ega_info() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // BL=10h: Get EGA info
    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x10);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.bl(), 3); // 256K memory
}

#[test]
fn test_cov_int10_set_mode_0d_ega_graphics() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // Mode 0x0D: EGA 320x200 16-color
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x0D);
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0xF000);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(memory.read_u8(0x449), 0x0D);
}

#[test]
fn test_cov_int10_set_mode_0f_ega_mono() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // Mode 0x0F: EGA 640x350 mono
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x0F);
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0xF000);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(memory.read_u8(0x449), 0x0F);
}

#[test]
fn test_cov_int10_set_mode_11_vga_mono() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // Mode 0x11: VGA 640x480 2-color
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x11);
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0xF000);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(memory.read_u8(0x449), 0x11);
    // Mode 11/12 should have 29 rows
    assert_eq!(memory.read_u8(0x484), 29);
}

#[test]
fn test_cov_int10_set_mode_12_vga_16color() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // Mode 0x12: VGA 640x480 16-color
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x12);
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0xF000);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(memory.read_u8(0x449), 0x12);
}

#[test]
fn test_cov_int10_video_state_save() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // First get buffer size
    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x00);
    cpu.regs.set_cx(0x07);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    let _buf_size = cpu.regs.bx();

    // Now save state
    let buf_seg = 0x5000u16;
    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x01); // Save
    cpu.regs.set_cx(0x07);
    cpu.regs.es = buf_seg;
    cpu.regs.set_bx(0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x1C);

    // Restore state
    cpu.regs.set_ah(0x1C);
    cpu.regs.set_al(0x02); // Restore
    cpu.regs.set_cx(0x07);
    cpu.regs.es = buf_seg;
    cpu.regs.set_bx(0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x1C);
}

#[test]
fn test_cov_int10_read_pixel() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // Set mode 13h
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x13);
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0xF000);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // Write a pixel first
    memory.write_u8(0xA0000 + 50 * 320 + 100, 0x0F);

    // AH=0Dh: Read pixel
    cpu.regs.set_ah(0x0D);
    cpu.regs.set_bh(0); // Page
    cpu.regs.set_cx(100); // X
    cpu.regs.set_dx(50); // Y
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_int10_teletype() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // Set text mode
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x03);
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0xF000);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // AH=0Eh: Teletype output
    cpu.regs.set_ah(0x0E);
    cpu.regs.set_al(b'X');
    cpu.regs.set_bh(0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_int10_get_video_mode() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // AH=0Fh: Get video mode
    cpu.regs.set_ah(0x0F);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_apm_connect_real_mode() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // APM 5301h: Connect real mode interface
    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x01);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_cov_apm_connect_16bit() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // APM 5302h: Connect 16-bit protected mode interface
    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x02);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
}

#[test]
fn test_cov_apm_connect_32bit() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // APM 5303h: Connect 32-bit protected mode interface
    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x03);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
}

#[test]
fn test_cov_apm_disconnect() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // APM 5304h: Disconnect
    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x04);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
}

#[test]
fn test_cov_int10_set_mode_05_cga_bw() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x05); // CGA 320x200 4-color (B/W)
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0xF000);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(memory.read_u8(0x449), 0x05);
}

#[test]
fn test_cov_int10_set_mode_06_cga_hires() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x06); // CGA 640x200 2-color
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0xF000);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(memory.read_u8(0x449), 0x06);
}

#[test]
fn test_cov_grayscale_dac_set_individual() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Enable grayscale via BDA 0x489 bits 1-2
    memory.write_u8(0x489, 0x02);

    // AH=10h, AL=10h: Set individual DAC register with grayscale
    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x10);
    cpu.regs.set_bl(0); // register 0
    cpu.regs.set_dh(0x3F); // R = max
    cpu.regs.set_ch(0x3F); // G = max
    cpu.regs.set_cl(0x3F); // B = max
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_grayscale_dac_set_block() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Enable grayscale
    memory.write_u8(0x489, 0x04);

    // Write RGB triplets to memory
    let table_addr = 0x5000usize;
    for i in 0..3u8 {
        memory.write_u8(table_addr + (i as usize) * 3, 0x3F); // R
        memory.write_u8(table_addr + (i as usize) * 3 + 1, 0x3F); // G
        memory.write_u8(table_addr + (i as usize) * 3 + 2, 0x3F); // B
    }

    // AH=10h, AL=12h: Set block of DAC registers with grayscale
    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x12);
    cpu.regs.set_bx(0); // start reg
    cpu.regs.set_cx(3); // count
    cpu.regs.es = (table_addr / 16) as u16;
    cpu.regs.set_dx(0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_grayscale_triplet_clamp_both_branches() {
    // Test that grayscale_triplet clamps at 0x3F (i > 0x3F branch)
    let (r, g, b) = Bios::grayscale_triplet(0x3F, 0x3F, 0x3F);
    assert_eq!(r, 0x3F);
    assert_eq!(g, 0x3F);
    assert_eq!(b, 0x3F);

    // Low values should not clamp (i <= 0x3F branch)
    let (r, _, _) = Bios::grayscale_triplet(0x00, 0x00, 0x00);
    assert_eq!(r, 0);
}

#[test]
fn test_cov_int10_load_user_font_zero_height() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // AH=11h, AL=21h: load user graphic font with CX=0 (defaults to 8)
    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x21);
    cpu.regs.set_cx(0); // char_height = 0, should default to 8
    cpu.regs.es = 0x6000;
    cpu.regs.set_bp(0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_int10_alt_func_30_al_greater_than_2() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // AH=12h, BL=30h, AL=3: invalid scan lines value
    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x30);
    cpu.regs.set_al(3);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    // AL should be set to 0 for invalid values
    assert_eq!(cpu.regs.al(), 0);
}

#[test]
fn test_cov_int10_alt_func_31_enable_palette_loading() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // BL=31h, AL=1: enable palette loading (sets flag bit)
    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x31);
    cpu.regs.set_al(1);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x12);
    let flags = memory.read_u8(0x489);
    assert_ne!(flags & 0x08, 0);
}

#[test]
fn test_cov_int10_alt_func_33_al1() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // BL=33h, AL=1: gray-scale off
    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x33);
    cpu.regs.set_al(1);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x12);
    let flags = memory.read_u8(0x489);
    // When AL=1, bit 1 should NOT be set (gray-scale disabled)
    assert_eq!(flags & 0x02, 0);
}

#[test]
fn test_cov_int10_alt_func_36_screen_on() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // BL=36h, AL=0: screen on
    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x36);
    cpu.regs.set_al(0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x12);
    let seq = vga.get_sequencer_regs();
    assert_eq!(seq[1] & 0x20, 0); // Screen on: bit 5 cleared
}

#[test]
fn test_cov_int10_write_string_with_attrs() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set mode 3 text mode for consistent state
    vga.set_mode_bios(3);
    memory.write_u8(0x449, 3);
    memory.write_u8(0x484, 24); // 25 rows

    // Write string with mode=2 (attrs in string, no cursor update)
    let str_addr = 0x5000usize;
    // char/attr pairs
    memory.write_u8(str_addr, b'A');
    memory.write_u8(str_addr + 1, 0x07);
    memory.write_u8(str_addr + 2, b'B');
    memory.write_u8(str_addr + 3, 0x0F);

    cpu.regs.set_ah(0x13);
    cpu.regs.set_al(0x02); // mode=2: attrs in string, no cursor update
    cpu.regs.set_bh(0); // page 0
    cpu.regs.set_bl(0x07); // default attr
    cpu.regs.set_cx(2); // 2 chars
    cpu.regs.set_dh(0); // row 0
    cpu.regs.set_dl(0); // col 0
    cpu.regs.es = (str_addr / 16) as u16;
    cpu.regs.set_bp(0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_int10_write_string_with_cursor_update() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(3);
    memory.write_u8(0x449, 3);
    memory.write_u8(0x484, 24); // 25 rows

    // Write string with mode=1 (no attrs, cursor update)
    let str_addr = 0x5000usize;
    memory.write_u8(str_addr, b'X');
    memory.write_u8(str_addr + 1, b'Y');

    cpu.regs.set_ah(0x13);
    cpu.regs.set_al(0x01); // mode=1: no attrs, cursor update
    cpu.regs.set_bh(0);
    cpu.regs.set_bl(0x07);
    cpu.regs.set_cx(2);
    cpu.regs.set_dh(0);
    cpu.regs.set_dl(0);
    cpu.regs.es = (str_addr / 16) as u16;
    cpu.regs.set_bp(0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_int10_write_string_col_wrap() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(3);
    memory.write_u8(0x449, 3);
    memory.write_u8(0x484, 24); // 25 rows

    // Start at the last column to trigger column wrap
    let str_addr = 0x5000usize;
    memory.write_u8(str_addr, b'A');
    memory.write_u8(str_addr + 1, b'B');

    cpu.regs.set_ah(0x13);
    cpu.regs.set_al(0x00); // mode=0: no attrs, no cursor update
    cpu.regs.set_bh(0);
    cpu.regs.set_bl(0x07);
    cpu.regs.set_cx(2);
    cpu.regs.set_dh(0);
    cpu.regs.set_dl(79); // last column
    cpu.regs.es = (str_addr / 16) as u16;
    cpu.regs.set_bp(0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_int10_write_string_row_scroll() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    vga.set_mode_bios(3);
    memory.write_u8(0x449, 3);
    memory.write_u8(0x484, 24); // 25 rows

    // Start at last row, last column to trigger both column wrap and row scroll
    let str_addr = 0x5000usize;
    memory.write_u8(str_addr, b'Z');
    memory.write_u8(str_addr + 1, b'W');

    cpu.regs.set_ah(0x13);
    cpu.regs.set_al(0x01); // mode=1: update cursor
    cpu.regs.set_bh(0);
    cpu.regs.set_bl(0x07);
    cpu.regs.set_cx(2);
    cpu.regs.set_dh(24); // last row
    cpu.regs.set_dl(79); // last column
    cpu.regs.es = (str_addr / 16) as u16;
    cpu.regs.set_bp(0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_days_to_ymd_negative_epoch() {
    // days_to_ymd with day 0 is 1970-01-01
    let (y, m, d) = days_to_ymd(0);
    assert_eq!((y, m, d), (1970, 1, 1));
}

#[test]
fn test_cov_int15_e881_default_ram() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    // Truncate to ~1MB+64KB to test small memory E881 behavior
    let mut memory = Memory::new();
    memory.resize_ram(gp64_memory::TOTAL_SIZE);

    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0x81);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // No memory above 16MB with small RAM
    assert_eq!(cpu.regs.ebx, 0);
}

#[test]
fn test_cov_memory_size_16mb() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    // Truncate to exactly 16MB
    memory.resize_ram(16 * 1024 * 1024);

    // AH=88h: Get extended memory size
    cpu.regs.set_ah(0x88);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    let ah88_kb = cpu.regs.ax() as u64;
    // 16MB - 1MB = 15MB = 15360 KB
    assert_eq!(ah88_kb, 15360);

    // E801h: Get extended memory size
    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0x01);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    let e801_1to16_kb = cpu.regs.ax() as u64;
    let e801_above16_blocks = cpu.regs.bx() as u64;
    assert_eq!(e801_1to16_kb, 15360);
    assert_eq!(e801_above16_blocks, 0);

    // E881h: Extended memory size (32-bit)
    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0x81);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.eax, 15360); // 1-16MB in KB
    assert_eq!(cpu.regs.ebx, 0); // above 16MB blocks
}

#[test]
fn test_cov_cga_mode_control_from_table_none() {
    // cga_mode_control_from_table with mode > 7 returns None
    let result = Bios::cga_mode_control_from_table(0x08);
    assert!(result.is_none());

    // Modes 0-7 should return Some
    for mode in 0..=7 {
        assert!(Bios::cga_mode_control_from_table(mode).is_some());
    }
}

#[test]
fn test_cov_kbd_trace_allow_exhausted() {
    use tracing_subscriber::layer::SubscriberExt;

    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::filter::EnvFilter::new("gp64::kbd::int=debug"),
    );
    tracing::subscriber::with_default(subscriber, || {
        let bios = Bios::new();
        for _ in 0..5000 {
            assert!(bios.kbd_trace_allow());
        }
        assert!(!bios.kbd_trace_allow());
    });
}

#[test]
fn test_cov_days_to_ymd_negative_z() {
    // Trigger z < 0 by using a huge u64 that wraps to negative as i64
    // days = 2^64 - 719469 makes z = -719469 + 719468 = -1
    let days = u64::MAX - 719468; // as i64 = -719469
    let _ = days_to_ymd(days); // Just exercise the z < 0 branch
}

#[test]
fn test_cov_int10_1b_without_blink_flag() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Make sure attr[0x10] bit 3 is CLEAR
    let mut attr = vga.get_attribute_regs();
    attr[0x10] &= !0x08;
    vga.set_attribute_regs(attr);

    // AH=1Bh: Return functionality/state information
    let buf_seg = 0x5000u16;
    let buf_off = 0x0000u16;
    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(0);
    cpu.regs.es = buf_seg;
    cpu.regs.set_di(buf_off);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x1B);

    let buf_addr = (buf_seg as usize) * 16 + (buf_off as usize);
    let misc_flags = memory.read_u8(buf_addr + 0x2D);
    // Bit 5 should NOT be set because attr[0x10] bit 3 is clear
    assert_eq!(misc_flags & 0x20, 0);
}

// ===== Coverage: Tracing-enabled paths for pixel trace (line 922) =====
// and kbd_flow_trace (lines 2866, 2918)
#[test]
fn test_cov_tracing_enabled_pixel_and_kbd_flow() {
    use tracing_subscriber::layer::SubscriberExt;

    // Build a subscriber that enables trace targets at appropriate levels
    let subscriber = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::sink)
                .with_ansi(false),
        )
        .with(tracing_subscriber::filter::EnvFilter::new(
            "gp64::bios::pixel=trace,gp64::kbd::flow=info,gp64::kbd::int=debug",
        ));

    // Use `set_default` for thread-local (doesn't conflict with other tests)
    let _guard = tracing::subscriber::set_default(subscriber);

    let mut bios = Bios::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::Vga::new();
    let mut keyboard = gp64_hw::Keyboard::new();

    // Initialize video mode 13h for pixel writes
    vga.set_mode_bios(0x13);

    // --- Cover pixel trace True branch (line 922) and count >= 100 branch ---
    // Write 101 pixels to exceed the PX_COUNT < 100 check
    for px in 0..101u16 {
        cpu.regs.set_ah(0x0C);
        cpu.regs.set_al(0x0F); // color
        cpu.regs.set_bh(0); // page
        cpu.regs.set_cx(px); // x
        cpu.regs.set_dx(0); // y
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    }

    // --- Cover kbd_flow_trace True branches (lines 2866, 2918) ---
    // First, put a key in the BDA keyboard buffer
    gp64_hw::bda::kbd_enqueue(&mut memory, 0x1C, 0x0D); // Enter key

    // INT 16h AH=01: Check keyboard status (covers line 2918)
    cpu.regs.set_ah(0x01);
    bios.int16(&mut cpu, &mut memory, &mut keyboard).unwrap();

    // INT 16h AH=00: Read keyboard (covers line 2866)
    cpu.regs.set_ah(0x00);
    bios.int16(&mut cpu, &mut memory, &mut keyboard).unwrap();
    assert_eq!(cpu.regs.ah(), 0x1C); // scancode
    assert_eq!(cpu.regs.al(), 0x0D); // ASCII
}

#[test]
fn test_e820_type_values() {
    assert_eq!(E820Type::AddressRangeMemory as u32, 1);
    assert_eq!(E820Type::AddressRangeReserved as u32, 2);
    assert_eq!(E820Type::AddressRangeACPI as u32, 3);
    assert_eq!(E820Type::AddressRangeNVS as u32, 4);
    assert_eq!(E820Type::AddressRangeUnusable as u32, 5);
}

#[test]
fn test_e820_type_debug_clone_eq() {
    let t = E820Type::AddressRangeMemory;
    let cloned = t;
    assert_eq!(t, cloned);
    let dbg = format!("{t:?}");
    assert!(dbg.contains("AddressRangeMemory"));
}

#[test]
fn test_e820_entry_sizes() {
    assert_eq!(E820Entry::SIZE, 20);
    assert_eq!(E820Entry::SIZE_EXTENDED, 24);
}

#[test]
fn test_e820_entry_new_and_debug() {
    let entry = E820Entry::new(0x1000, 0x2000, E820Type::AddressRangeReserved);
    assert_eq!(entry.base, 0x1000);
    assert_eq!(entry.length, 0x2000);
    assert_eq!(entry.entry_type, 2);
    let dbg = format!("{entry:?}");
    assert!(dbg.contains("E820Entry"));
}

#[test]
fn test_e820_entry_clone() {
    let entry = E820Entry::new(0, 0x100000, E820Type::AddressRangeMemory);
    let cloned = entry;
    assert_eq!(cloned.base, 0);
    assert_eq!(cloned.length, 0x100000);
}

#[test]
fn test_disk_address_packet_fields() {
    let mut memory = Memory::new();
    let addr = 0x1000;
    memory.write_u8(addr, 0x10); // size
    memory.write_u8(addr + 1, 0x00); // reserved
    memory.write_u16(addr + 2, 5); // sector_count
    memory.write_u16(addr + 4, 0x0200); // buffer_offset
    memory.write_u16(addr + 6, 0x0800); // buffer_segment
    memory.write_u64(addr + 8, 0x12345678_9ABCDEF0); // lba

    let dap = DiskAddressPacket::read_from_memory(&memory, addr);
    assert_eq!(dap.size, 0x10);
    assert_eq!(dap.sector_count, 5);
    assert_eq!(dap.buffer_offset, 0x0200);
    assert_eq!(dap.buffer_segment, 0x0800);
    assert_eq!(dap.lba, 0x12345678_9ABCDEF0);
}

#[test]
fn test_to_bcd_values() {
    assert_eq!(to_bcd(0), 0x00);
    assert_eq!(to_bcd(9), 0x09);
    assert_eq!(to_bcd(10), 0x10);
    assert_eq!(to_bcd(25), 0x25);
    assert_eq!(to_bcd(59), 0x59);
    assert_eq!(to_bcd(99), 0x99);
}

#[test]
fn test_days_to_ymd_day_one() {
    let (y, m, d) = days_to_ymd(1);
    assert_eq!((y, m, d), (1970, 1, 2));
}

#[test]
fn test_parse_descriptor_base_all_bytes() {
    let mut memory = Memory::new();
    let addr = 0x2000;
    // base_low (offset +2) = 0x5678
    memory.write_u16(addr + 2, 0x5678);
    // base_mid (offset +4) = 0x34
    memory.write_u8(addr + 4, 0x34);
    // base_high (offset +7) = 0x12
    memory.write_u8(addr + 7, 0x12);

    let base = Bios::parse_descriptor_base(&memory, addr);
    assert_eq!(base, 0x12345678);
}

#[test]
fn test_e820_generate_with_extended_memory() {
    let bios = Bios::new();
    let memory = Memory::new(); // Default memory has >1MB
    let (entries, count) = bios.generate_e820_map(&memory);
    // Base, video/ROM, and extended memory.
    assert!(count >= 3, "count={count}");
    assert_eq!(entries[0].entry_type, E820Type::AddressRangeMemory as u32);
    assert_eq!(entries[0].base, 0);
    assert_eq!(entries[1].entry_type, E820Type::AddressRangeReserved as u32);
    assert_eq!(entries[2].entry_type, E820Type::AddressRangeMemory as u32);
    assert_eq!(entries[2].base, 0x100000); // Extended starts at 1MB
}

#[test]
fn test_copy_memory_block_forward_odd_length() {
    let mut memory = Memory::new();
    // Write 5 bytes at address 0x200
    for i in 0..5u8 {
        memory.write_u8(0x200 + i as usize, 0xA0 + i);
    }
    // Non-overlapping forward copy with non-4-aligned length
    Bios::copy_memory_block(&mut memory, 0x200, 0x300, 5);
    for i in 0..5u8 {
        assert_eq!(
            memory.read_u8(0x300 + i as usize),
            0xA0 + i,
            "byte at offset {i}"
        );
    }
}

#[test]
fn test_int1a_set_and_read_rtc_time() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    // AH=03h: Set RTC time
    cpu.regs.set_ah(0x03);
    cpu.regs.set_ch(0x14); // 14 hours (BCD)
    cpu.regs.set_cl(0x30); // 30 minutes (BCD)
    cpu.regs.set_dh(0x45); // 45 seconds (BCD)
    cpu.regs.set_dl(0x00); // No DST

    bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    )
    .unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify by reading back via AH=02h
    cpu.regs.set_ah(0x02);
    bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    )
    .unwrap();
    assert_eq!(cpu.regs.ch(), 0x14);
    assert_eq!(cpu.regs.cl(), 0x30);
    assert_eq!(cpu.regs.dh(), 0x45);
}

#[test]
fn test_int1a_set_and_read_rtc_date() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    // AH=05h: Set RTC date
    cpu.regs.set_ah(0x05);
    cpu.regs.set_ch(0x20); // Century 20 (BCD)
    cpu.regs.set_cl(0x26); // Year 26 (BCD)
    cpu.regs.set_dh(0x03); // Month 3 (BCD)
    cpu.regs.set_dl(0x25); // Day 25 (BCD)

    bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    )
    .unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify by reading back via AH=04h
    cpu.regs.set_ah(0x04);
    bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    )
    .unwrap();
    assert_eq!(cpu.regs.ch(), 0x20);
    assert_eq!(cpu.regs.cl(), 0x26);
    assert_eq!(cpu.regs.dh(), 0x03);
    assert_eq!(cpu.regs.dl(), 0x25);
}

#[test]
fn test_int1a_unhandled_function() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    cpu.regs.set_ah(0xFE); // Unhandled function
    bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    )
    .unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_bios_new_fields() {
    let bios = Bios::new();
    assert!(!bios.event_wait_active);
    assert_eq!(bios.event_wait_flag_addr, 0);
    assert_eq!(bios.event_wait_remaining_us, 0);
}
