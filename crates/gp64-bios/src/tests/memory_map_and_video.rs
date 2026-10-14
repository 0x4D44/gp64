use super::*;

/// E820 memory map with no extended memory (ram_size <= 1MB).
/// Covers the false branch of `ram_size > 0x10_0000` at generate_e820_map.
#[test]
fn test_cov_e820_no_extended_memory() {
    let bios = Bios::new();
    let mut memory = Memory::new();
    // Shrink RAM to exactly 1MB (0x10_0000) so extended mem check fails
    memory.resize_ram(0x10_0000);

    let (entries, count) = bios.generate_e820_map(&memory);
    // Conventional and video/ROM. No EBDA or extended memory.
    assert_eq!(count, 2);
    assert_eq!(entries[0].entry_type, E820Type::AddressRangeMemory as u32);
    assert_eq!(entries[1].entry_type, E820Type::AddressRangeReserved as u32);
}

/// E801h with memory <= 1MB: both mem_1_to_16_kb and mem_above_16_blocks are 0.
/// Covers the false branches at lines 3685/3692.
#[test]
fn test_cov_e801_no_extended_memory() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    // Shrink to exactly 1MB
    memory.resize_ram(0x10_0000);

    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0x01);
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0); // No extended memory between 1-16MB
    assert_eq!(cpu.regs.bx(), 0); // No memory above 16MB
    assert_eq!(cpu.regs.cx(), 0);
    assert_eq!(cpu.regs.dx(), 0);
}

/// E881h with memory <= 1MB: same as E801h but 32-bit registers.
/// Covers the false branches at lines 3716/3723.
#[test]
fn test_cov_e881_no_extended_memory() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    memory.resize_ram(0x10_0000);

    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0x81);
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.eax, 0);
    assert_eq!(cpu.regs.ebx, 0);
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.edx, 0);
}

/// E801h with memory between 1-16MB: mem_1_to_16_kb > 0, mem_above_16_blocks = 0.
/// Covers the true branch at line 3685 and false at 3692.
#[test]
fn test_cov_e801_between_1_and_16mb() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    // Truncate to exactly 8MB to test memory between 1-16MB
    memory.resize_ram(8 * 1024 * 1024);

    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0x01);
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // 8MB - 1MB = 7MB = 7168 KB
    let expected_kb = ((memory.ram_size() as u64 - 0x10_0000).min(0xF0_0000) / 1024) as u16;
    assert_eq!(cpu.regs.ax(), expected_kb);
    assert!(expected_kb > 0);
    assert_eq!(cpu.regs.bx(), 0); // No memory above 16MB
}

/// E881h with memory between 1-16MB.
/// Covers the true branch at line 3716 and false at 3723.
#[test]
fn test_cov_e881_between_1_and_16mb() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    // Truncate to exactly 8MB to test memory between 1-16MB
    memory.resize_ram(8 * 1024 * 1024);

    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0x81);
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let expected_kb = ((memory.ram_size() as u64 - 0x10_0000).min(0xF0_0000) / 1024) as u32;
    assert_eq!(cpu.regs.eax, expected_kb);
    assert!(expected_kb > 0);
    assert_eq!(cpu.regs.ebx, 0);
}

/// INT 15h AH=88h with memory <= 1MB: returns 0 extended KB.
#[test]
fn test_cov_int15_88_no_extended() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    memory.resize_ram(0x10_0000);

    cpu.regs.set_ah(0x88);
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0);
}

/// days_to_ymd with z < 0 (before Unix epoch).
/// Covers the `else` branch at line 4595.
#[test]
fn test_days_to_ymd_before_epoch() {
    // 0 days = 1970-01-01
    let (y, m, d) = days_to_ymd(0);
    assert_eq!((y, m, d), (1970, 1, 1));

    // days_to_ymd uses u64, so we can't pass negative days directly.
    // But the z calculation: z = days as i64 + 719_468
    // For z < 0: we'd need days < -719_468, but days is u64.
    // So the z < 0 branch is unreachable with u64 input.
    // Instead, test dates that exercise mp >= 10 (months 1-2 of next year).

    // January 1, 2000: days since epoch = 10957
    let (y, m, d) = days_to_ymd(10957);
    assert_eq!((y, m, d), (2000, 1, 1));

    // February 29, 2000 (leap year): 10957 + 31 + 28 = 11016? Let's compute.
    // Actually Jan has 31 days, so Feb 1 = 10957+31 = 10988, Feb 29 = 10988+28 = 11016
    let (y, m, d) = days_to_ymd(11016);
    assert_eq!((y, m, d), (2000, 2, 29));

    // Dec 31, 1969: days = -1, but u64 so wrap around.
    // Can't test negative. Instead test November (mp=8) and January (mp=10).
    // March 1, 2000 = 10957 + 31 + 29 = 11017
    let (y, m, d) = days_to_ymd(11017);
    assert_eq!((y, m, d), (2000, 3, 1));
}

/// INT 15h wait (AH=86h) returns success via the deterministic cycle-charge
/// path (the retired real-sleep env branch was removed, KILN-00086).
#[test]
fn test_cov_int15_wait_realtime() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up a very short wait (1 microsecond)
    cpu.regs.set_ah(0x86);
    cpu.regs.set_cx(0); // high word
    cpu.regs.set_dx(1); // low word = 1 microsecond
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x00);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// INT 15h AH=BF is refused like any unknown function (extender kernel
/// API; answering it broke a DOS extender — MDD-BUG-KILN-00048).
#[test]
fn test_cov_int15_bf_refused() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0xBF);
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x86);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// E820 traversal with exactly 1MB memory (no extended entry).
/// Exercises the INT 15h E820 path where extended memory entry is absent.
#[test]
fn test_cov_e820_traversal_no_extended() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    memory.resize_ram(0x10_0000);

    let smap: u32 = 0x534D4150; // 'SMAP'
    let mut continuation = 0u32;
    let mut entry_count = 0;

    loop {
        cpu.regs.set_ah(0xE8);
        cpu.regs.set_al(0x20);
        cpu.regs.edx = smap;
        cpu.regs.ebx = continuation;
        cpu.regs.ecx = 20;
        cpu.regs.es = 0x2000;
        cpu.regs.set_di(0x0000);

        bios.int15(&mut cpu, &mut memory, None).unwrap();
        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

        entry_count += 1;
        continuation = cpu.regs.ebx;
        if continuation == 0 {
            break;
        }
        assert!(entry_count < 10, "Too many E820 entries");
    }

    // With no extended memory, EBDA, or VBE, there are exactly two entries.
    assert_eq!(entry_count, 2);
}

/// Load font to plane2 (block version) with a very short font.
/// Exercises the bounds check at line 4506 and the else branch at 4501.
#[test]
fn test_cov_load_font_to_plane2_block_tiny_font() {
    let mut memory = Memory::new();
    // 8x8 font for only 10 characters (80 bytes)
    let font: Vec<u8> = vec![0xAA; 80];
    Bios::load_font_to_plane2_block(&mut memory, &font, 8, 0, 0, 256);

    let plane2 = memory.plane_mut(2);
    // Characters 0-9 should have data
    assert_eq!(plane2[0], 0xAA);
    assert_eq!(plane2[7], 0xAA);
    // Character 10 and beyond: font ran out, so padding rows are zero
    assert_eq!(plane2[10 * 32], 0);
}

// Native plane-2 locations from IBM VGA/XGA Technical Reference (May 1992),
// p. 2-53. Check the bytes independently before comparing device lookup.
fn assert_all_loaded_font_banks(memory: &Memory, vga: &mut Vga, height: usize) {
    const OFFSETS: [usize; 8] = [
        0x0000, 0x4000, 0x8000, 0xC000, 0x2000, 0x6000, 0xA000, 0xE000,
    ];
    const SELECT_A: [u8; 8] = [0x00, 0x04, 0x08, 0x0C, 0x20, 0x24, 0x28, 0x2C];
    const SELECT_B: [u8; 8] = [0x00, 0x01, 0x02, 0x03, 0x10, 0x11, 0x12, 0x13];
    vga.custom_font_loaded = true;
    for (bank, &offset) in OFFSETS.iter().enumerate() {
        for row in 0..32 {
            let expected = if row < height { bank as u8 + 1 } else { 0 };
            assert_eq!(
                memory.plane(2)[offset + 0xFF * 32 + row],
                expected,
                "native bank {bank}, row {row}: loader address"
            );
        }
    }
    let (mut bios, mut cpu) = (Bios::new(), Cpu::new());
    let mut selection_memory = Memory::new();
    for (a, &select_a) in SELECT_A.iter().enumerate() {
        for (b, &select_b) in SELECT_B.iter().enumerate() {
            // AX=1103 reaches the same register fields as guest port writes.
            cpu.regs.set_ax(0x1103);
            cpu.regs.set_bl(select_a | select_b);
            bios.int10(&mut cpu, &mut selection_memory, vga).unwrap();
            for row in 0..32 {
                for (attr, bank) in [(0, b), (8, a)] {
                    let expected = if row < height { bank as u8 + 1 } else { 0 };
                    assert_eq!(
                        vga.get_font_row(memory, 0xFF, row, height, attr),
                        expected,
                        "A={a}, B={b}, attr={attr}, row={row}: loaded glyph"
                    );
                }
            }
        }
    }
}

#[test]
fn native_font_banks_rom_loader_preserves_all_eight() {
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    for bank in 0..8 {
        let font = vec![bank + 1; 256 * 8];
        // Bits outside the block selector remain ignored.
        Bios::load_font_to_plane2_block(&mut memory, &font, 8, bank | 0xF8, 0, 256);
    }
    assert_all_loaded_font_banks(&memory, &mut vga, 8);
}

#[test]
fn native_font_banks_user_loader_preserves_all_eight() {
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    for bank in 0..8 {
        for row in 0..16 {
            memory.write_u8(0x50000 + row, bank + 1);
        }
        cpu.regs.set_ax(0x1100);
        cpu.regs.set_bh(16);
        cpu.regs.set_bl(bank | 0xF8);
        cpu.regs.set_cx(1);
        cpu.regs.set_dx(0xFF);
        cpu.regs.es = 0x5000;
        cpu.regs.set_bp(0);
        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    }
    assert_all_loaded_font_banks(&memory, &mut vga, 16);
}

/// Load user font to plane2 with count that exceeds 256.
/// Exercises the clamping at line 4524.
#[test]
fn test_cov_load_user_font_clamp_count() {
    let mut memory = Memory::new();
    // Write some font data at a known address
    let src_addr = 0x50000;
    for i in 0..512 {
        memory.write_u8(src_addr + i, 0xBB);
    }
    // Try to load 300 chars starting at char 0 (should be clamped to 256)
    Bios::load_user_font_to_plane2(&mut memory, src_addr, 300, 8, 0, 0);

    let plane2 = memory.plane_mut(2);
    // First character should have data
    assert_eq!(plane2[0], 0xBB);
}

/// Load user font with start_char > 0 and count that would overflow.
#[test]
fn test_cov_load_user_font_start_plus_count_overflow() {
    let mut memory = Memory::new();
    let src_addr = 0x50000;
    for i in 0..2048 {
        memory.write_u8(src_addr + i, 0xCC);
    }
    // start_char=250, count=100: clamped to 6 (256-250)
    Bios::load_user_font_to_plane2(&mut memory, src_addr, 100, 8, 0, 250);

    let plane2 = memory.plane_mut(2);
    // Character 250 in plane2: offset = 250 * 32
    assert_eq!(plane2[250 * 32], 0xCC);
    // Character 255 should also be written
    assert_eq!(plane2[255 * 32], 0xCC);
}

/// INT 10h set video mode 0x07 (MDA monochrome) - tests B0000 base path.
/// Some of the mode-7 specific branches may be uncovered.
#[test]
fn test_cov_int10_set_mode_07_mda_vram_base() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    bios.init_ivt(&mut memory, false);

    // Set mode 7 (MDA)
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x07);
    // Push return address for stack trace in handler
    let ss = 0x0000u16;
    let sp = 0x1000u16;
    cpu.regs.ss = ss;
    cpu.regs.set_sp(sp);
    memory.write_u16(Memory::seg_offset_to_linear(ss, sp), 0x0100); // return IP
    memory.write_u16(Memory::seg_offset_to_linear(ss, sp) + 2, 0xF000); // return CS

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    let (mode, _, _) = vga.get_mode();
    assert_eq!(mode, 0x07);
    // MDA text memory starts at B0000
    assert_eq!(memory.read_u8(0xB0000), b' ');
    assert_eq!(memory.read_u8(0xB0001), 0x07);
}

/// INT 10h set CGA graphics mode 4 (320x200 4-color).
/// Tests the CGA mode clear path at lines 681-685.
#[test]
fn test_cov_int10_set_mode_04_cga_clear() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    bios.init_ivt(&mut memory, false);

    // Pre-fill CGA VRAM area with non-zero
    for addr in 0xB8000usize..0xBC000usize {
        memory.write_u8(addr, 0xFF);
    }

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x04);
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0xF000);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // CGA VRAM should be cleared
    assert_eq!(memory.read_u8(0xB8000), 0);
    assert_eq!(memory.read_u8(0xBBFFF), 0);
}

/// INT 10h set mode 0x0D (EGA 320x200 16-color).
/// Tests the EGA/VGA plane clear path and font loading (lines 687-702).
#[test]
fn test_cov_int10_set_mode_0d_plane_clear() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x0D);
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0xF000);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // All 4 planes should be cleared
    for plane in 0..4 {
        let p = memory.plane_mut(plane);
        // Plane 2 has font data loaded, so it won't be all zeros.
        // But the first bytes of plane 0, 1, 3 should be zero.
        if plane != 2 {
            assert_eq!(p[0], 0);
        }
    }
}

/// E801h with memory > 16MB: both mem_1_to_16_kb and mem_above_16_blocks are nonzero.
/// Covers the true branch at line 3692 (`ram_bytes > 0x0100_0000`).
#[test]
fn test_cov_e801_above_16mb() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    // Resize explicitly because the default RAM is larger than this 32MB fixture.
    memory.resize_ram(32 * 1024 * 1024);

    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0x01);
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // mem_1_to_16_kb should be 15360 (15MB = 0x3C00 KB)
    assert_eq!(cpu.regs.ax(), 0x3C00);
    // mem_above_16_blocks: (32MB - 16MB) / 64KB = 256 blocks
    assert_eq!(cpu.regs.bx(), 256);
}

/// E881h with memory > 16MB.
/// Covers the true branch at line 3723 (`ram_bytes > 0x0100_0000`).
#[test]
fn test_cov_e881_above_16mb() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    // Resize explicitly because the default RAM is larger than this 32MB fixture.
    memory.resize_ram(32 * 1024 * 1024);

    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0x81);
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.eax, 0x3C00);
    assert_eq!(cpu.regs.ebx, 256);
}

/// INT 10h set mode with no-clear bit (0x80 | mode).
/// Tests the `clear_vram == false` path.
#[test]
fn test_cov_int10_set_mode_13h_no_clear_vram() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    bios.init_ivt(&mut memory, false);

    // Set mode 0x13 first so Memory's pipeline state matches chain4.
    // This prevents apply_vga_pipeline from triggering a chain4 transition
    // that would rearrange the pre-written VRAM data.
    vga.set_mode_bios(0x13);
    memory.apply_vga_pipeline(&vga.build_pipeline_state());

    // Pre-fill mode 13h VRAM
    memory.write_u8(0xA0000, 0xAB);

    // Set mode 13h with no-clear bit
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x93); // 0x80 | 0x13
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0xF000);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // VRAM should NOT be cleared
    assert_eq!(memory.read_u8(0xA0000), 0xAB);
}

#[test]
fn test_cov_copy_memory_block_zero_length() {
    let mut memory = Memory::new();
    memory.write_u32(0x1000, 0xDEADBEEF);
    // Zero length should be a no-op
    Bios::copy_memory_block(&mut memory, 0x1000, 0x2000, 0);
    assert_eq!(memory.read_u32(0x2000), 0);
}

#[test]
fn test_cov_copy_memory_block_same_src_dst() {
    let mut memory = Memory::new();
    memory.write_u32(0x1000, 0xDEADBEEF);
    // Same source and dest should be a no-op
    Bios::copy_memory_block(&mut memory, 0x1000, 0x1000, 4);
    assert_eq!(memory.read_u32(0x1000), 0xDEADBEEF);
}

#[test]
fn test_cov_copy_memory_block_forward_non_aligned() {
    let mut memory = Memory::new();
    // Write 7 bytes
    for i in 0..7u8 {
        memory.write_u8(0x1000 + i as usize, i + 1);
    }
    // Non-overlapping forward copy with remainder bytes
    Bios::copy_memory_block(&mut memory, 0x1000, 0x2000, 7);
    for i in 0..7u8 {
        assert_eq!(memory.read_u8(0x2000 + i as usize), i + 1);
    }
}

#[test]
fn test_cov_copy_memory_block_overlapping_backward() {
    let mut memory = Memory::new();
    // Write data at 0x1000..0x1008
    for i in 0..8u8 {
        memory.write_u8(0x1000 + i as usize, i + 1);
    }
    // Overlapping: src < dst && src + len > dst
    // src=0x1000, dst=0x1004, len=8 → overlaps
    Bios::copy_memory_block(&mut memory, 0x1000, 0x1004, 8);
    // Data should be correctly copied backwards
    for i in 0..8u8 {
        assert_eq!(memory.read_u8(0x1004 + i as usize), i + 1);
    }
}

#[test]
fn test_cov_copy_memory_block_overlapping_remainder() {
    let mut memory = Memory::new();
    // Write 5 bytes
    for i in 0..5u8 {
        memory.write_u8(0x1000 + i as usize, 0x10 + i);
    }
    // Overlapping copy with remainder (5 bytes = 1 chunk of 4 + 1 remainder)
    Bios::copy_memory_block(&mut memory, 0x1000, 0x1002, 5);
    for i in 0..5u8 {
        assert_eq!(memory.read_u8(0x1002 + i as usize), 0x10 + i);
    }
}

#[test]
fn test_cov_set_dcc_index_swap_bytes() {
    let mut memory = Memory::new();
    // DCC_ENTRIES[11] = 0x0008 (VGA color, no alternate)
    // Try with swapped bytes (0x0800) - should match via swap_bytes path
    let result = Bios::set_dcc_index(&mut memory, 0x0800);
    assert!(result);
}

#[test]
fn test_cov_set_dcc_index_not_found() {
    let mut memory = Memory::new();
    // Invalid DCC code that doesn't exist
    let result = Bios::set_dcc_index(&mut memory, 0xFFFF);
    assert!(!result);
}

#[test]
fn test_cov_grayscale_triplet_clamp() {
    // Use values that produce sum > 0x3F
    let (r, g, b) = Bios::grayscale_triplet(0x3F, 0x3F, 0x3F);
    assert!(r <= 0x3F);
    assert_eq!(r, g);
    assert_eq!(g, b);
}

#[test]
fn test_cov_grayscale_triplet_no_clamp() {
    // Small values that won't exceed 0x3F
    let (r, g, _b) = Bios::grayscale_triplet(0x05, 0x05, 0x05);
    assert!(r <= 0x3F);
    assert_eq!(r, g);
}

#[test]
fn test_cov_cga_mode_control_from_table_invalid() {
    // Mode > 7 should return None
    assert!(Bios::cga_mode_control_from_table(0x08).is_none());
    assert!(Bios::cga_mode_control_from_table(0xFF).is_none());
}

#[test]
fn test_cov_cga_mode_control_from_table_valid() {
    // Modes 0-7 should return Some
    for m in 0..=7u8 {
        assert!(Bios::cga_mode_control_from_table(m).is_some());
    }
}

#[test]
fn test_cov_int10_set_mode_07_clears_mono_vram() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // Pre-fill mono text VRAM with garbage
    memory.write_u8(0xB0000, 0xFF);

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x07); // Mono text mode
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0xF000);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // Mono VRAM at B0000 should be cleared to spaces + attr 07
    assert_eq!(memory.read_u8(0xB0000), b' ');
    assert_eq!(memory.read_u8(0xB0001), 0x07);
}

// MDD-BUG-CRUCIBLE-00425: BDA 0x463 must name the mono CRTC base for the
// one monochrome mode, and the colour base everywhere else.
#[test]
fn int10_mode_07_publishes_the_mono_crtc_base() {
    assert_eq!(
        crtc_base_after_mode_set(0x07),
        0x3B4,
        "mode 07h is monochrome: its VGA path selects the mono I/O group, \
         so BDA 0x463 must be 0x3B4"
    );
}

#[test]
fn int10_mode_07_reports_monochrome_to_downstream_bios_queries() {
    // The BDA field is not the point on its own — these are the readers
    // that were being misinformed by the colour base.
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0xF000);

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x07);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // AH=12h BL=10h "get EGA info": BH = 0 colour / 1 mono.
    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x10);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(
        cpu.regs.bh(),
        1,
        "INT 10h AH=12h BL=10h must report mono in mode 07h"
    );

    // AH=1Bh functionality state: misc flags bit 2 is the mono flag.
    let buf_seg = 0x5000u16;
    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(0);
    cpu.regs.es = buf_seg;
    cpu.regs.set_di(0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    let misc_flags = memory.read_u8((buf_seg as usize) * 16 + 0x2D);
    assert_ne!(
        misc_flags & 0x04,
        0,
        "INT 10h AH=1Bh misc flags must carry the mono bit in mode 07h"
    );
}

#[test]
fn test_cov_int10_set_mode_04_clears_cga_vram() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // Pre-fill CGA VRAM
    memory.write_u8(0xB8000, 0xFF);

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x04); // CGA graphics mode
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0xF000);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // CGA VRAM should be cleared
    assert_eq!(memory.read_u8(0xB8000), 0);
}

#[test]
fn int10_text_mode_clears_full_window_with_default_cells_kiln75() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    for address in 0xB8000usize..0xC0000usize {
        memory.write_u8(address, 0xA5);
    }

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x03);
    cpu.regs.ss = 0;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0xF000);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    for address in (0xB8000usize..0xC0000usize).step_by(2) {
        assert_eq!(
            memory.read_u8(address),
            b' ',
            "text cell at {address:#06X} should contain a blank"
        );
        assert_eq!(
            memory.read_u8(address + 1),
            0x07,
            "text cell at {address:#06X} should use attribute 07h"
        );
    }
}

#[test]
fn int10_cga_mode_clears_full_window_kiln75() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    for address in 0xB8000usize..0xC0000usize {
        memory.write_u8(address, 0xA5);
    }

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x04);
    cpu.regs.ss = 0;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0xF000);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    for address in 0xB8000usize..0xC0000usize {
        assert_eq!(
            memory.read_u8(address),
            0,
            "CGA byte at {address:#06X} should be cleared"
        );
    }
}

#[test]
fn test_cov_int10_set_mode_10_clears_ega_planes() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x10); // EGA 640x350 16-color
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0xF000);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // BDA should show mode 0x10
    assert_eq!(memory.read_u8(0x449), 0x10);
}

#[test]
fn test_cov_int10_select_page_non_text_mode() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // First set mode 13h (graphics)
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x13);
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0xF000);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // Now select page 1
    cpu.regs.set_ah(0x05);
    cpu.regs.set_al(0x01);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    assert_eq!(memory.read_u8(0x462), 0x01);
}

#[test]
fn test_cov_int10_set_palette_background() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Set text mode first so BDA 0x449 = text mode
    memory.write_u8(0x449, 0x03);

    // AH=0Bh, BH=00: Set background color
    cpu.regs.set_ah(0x0B);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_bl(0x05); // Background = 5
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    let attr = vga.get_attribute_regs();
    assert_eq!(attr[0], 0x05);
}

#[test]
fn test_cov_int10_set_palette_select() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    memory.write_u8(0x449, 0x04); // CGA graphics mode
    let mut attr = vga.get_attribute_regs();
    attr[1] &= !0x01;
    attr[2] &= !0x01;
    attr[3] &= !0x01;
    vga.set_attribute_regs(attr);

    // AH=0Bh, BH=01: Select palette
    cpu.regs.set_ah(0x0B);
    cpu.regs.set_bh(0x01);
    cpu.regs.set_bl(0x01); // Palette 1 (Cyan/Magenta/White)
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    let attr = vga.get_attribute_regs();
    assert_eq!(attr[1] & 0x01, 0x01);
    assert_eq!(attr[2] & 0x01, 0x01);
    assert_eq!(attr[3] & 0x01, 0x01);
}

#[test]
fn test_cov_int10_dac_set_with_grayscale() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Enable grayscale (BDA 0x489 bits 1-2)
    memory.write_u8(0x489, 0x02);

    // AH=10h, AL=10h: Set individual DAC register with grayscale
    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x10);
    cpu.regs.set_bl(0); // Register 0
    cpu.regs.set_dh(0x20); // R
    cpu.regs.set_ch(0x10); // G
    cpu.regs.set_cl(0x08); // B
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    // Should have been converted to grayscale
    let entries = vga.get_dac_entries();
    let (r, g, b) = entries[0];
    // All channels should be equal (grayscale)
    assert_eq!(r, g);
    assert_eq!(g, b);
}

#[test]
fn test_cov_int10_dac_set_block_with_grayscale() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // Enable grayscale
    memory.write_u8(0x489, 0x04);

    // Write RGB triplets to memory
    let addr = 0x5000usize;
    memory.write_u8(addr, 0x3F); // R
    memory.write_u8(addr + 1, 0x00); // G
    memory.write_u8(addr + 2, 0x00); // B

    // AH=10h, AL=12h: Set block of DAC registers
    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x12);
    cpu.regs.set_bx(0); // Start register
    cpu.regs.set_cx(1); // Count
    cpu.regs.es = (addr / 16) as u16;
    cpu.regs.set_dx(0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    let entries = vga.get_dac_entries();
    let (r, g, b) = entries[0];
    assert_eq!(r, g);
    assert_eq!(g, b);
}

#[test]
fn test_cov_int10_write_string_with_attr() {
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

    // Write string with char+attr pairs (mode 2)
    let str_addr = 0x5000usize;
    memory.write_u8(str_addr, b'H');
    memory.write_u8(str_addr + 1, 0x0F); // attr
    memory.write_u8(str_addr + 2, b'i');
    memory.write_u8(str_addr + 3, 0x0F);

    cpu.regs.set_ah(0x13);
    cpu.regs.set_al(0x03); // mode=3 (char+attr, move cursor)
    cpu.regs.set_bh(0); // page
    cpu.regs.set_bl(0x07); // attr (unused in mode 2/3)
    cpu.regs.set_cx(2); // count
    cpu.regs.set_dh(0); // row
    cpu.regs.set_dl(0); // col
    cpu.regs.es = (str_addr / 16) as u16;
    cpu.regs.set_bp(0);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_int10_write_string_wrap_columns() {
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

    // Write string starting near end of row to trigger column wrap
    let str_addr = 0x5000usize;
    for i in 0..5u8 {
        memory.write_u8(str_addr + i as usize, b'A' + i);
    }

    cpu.regs.set_ah(0x13);
    cpu.regs.set_al(0x01); // mode=1 (attr from BL, move cursor)
    cpu.regs.set_bh(0);
    cpu.regs.set_bl(0x07);
    cpu.regs.set_cx(5);
    cpu.regs.set_dh(0); // row 0
    cpu.regs.set_dl(78); // col 78 (80-col mode, will wrap at 80)
    cpu.regs.es = (str_addr / 16) as u16;
    cpu.regs.set_bp(0);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_int10_dcc_set_valid() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // AH=1Ah, AL=01h: Set DCC
    cpu.regs.set_ah(0x1A);
    cpu.regs.set_al(0x01);
    cpu.regs.set_bx(0x0008); // BL=0x08 (VGA color active), BH=0x00 (no alternate)
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    assert_eq!(cpu.regs.al(), 0x1A); // Success
}

#[test]
fn test_cov_int10_dcc_set_invalid() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // AH=1Ah, AL=01h: Set DCC with invalid code
    cpu.regs.set_ah(0x1A);
    cpu.regs.set_al(0x01);
    cpu.regs.set_bx(0xFFFF); // Invalid
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    assert_eq!(cpu.regs.al(), 0); // Failed
}

#[test]
fn test_cov_int10_functionality_state_nonzero_bx() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0x1B);
    cpu.regs.set_bx(1); // Non-zero BX → fail
    cpu.regs.es = 0x5000;
    cpu.regs.set_di(0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    assert_eq!(cpu.regs.al(), 0); // AL=0 on failure
}

#[test]
fn test_cov_int10_ega_ril_version_bx_zero() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0xFA);
    cpu.regs.set_bx(0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    assert_eq!(cpu.regs.es, Bios::VIDEO_ROM_SEGMENT);
}

#[test]
fn test_cov_int10_ega_ril_version_bx_nonzero() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    cpu.regs.set_ah(0xFA);
    cpu.regs.set_bx(1); // Non-zero → return 0
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    assert_eq!(cpu.regs.bx(), 0);
}

#[test]
fn test_cov_int10_alternate_select_scan_lines_200() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // BL=30h, AL=0: 200 scan lines
    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x30);
    cpu.regs.set_al(0x00);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x12);
}

#[test]
fn test_cov_int10_alternate_select_scan_lines_350() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // BL=30h, AL=1: 350 scan lines
    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x30);
    cpu.regs.set_al(0x01);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x12);
}

#[test]
fn test_cov_int10_alternate_select_scan_lines_400() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // BL=30h, AL=2: 400 scan lines
    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x30);
    cpu.regs.set_al(0x02);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x12);
}

#[test]
fn test_cov_int10_alternate_select_scan_lines_invalid() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // BL=30h, AL=3: Invalid (> 2)
    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x30);
    cpu.regs.set_al(0x03);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0); // Failure
}

#[test]
fn test_cov_int10_alternate_select_default_palette() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // BL=31h, AL=1: Disable default palette loading
    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x31);
    cpu.regs.set_al(0x01);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x12);
    assert_ne!(memory.read_u8(0x489) & 0x08, 0);

    // BL=31h, AL=0: Enable default palette loading
    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x31);
    cpu.regs.set_al(0x00);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x12);
    assert_eq!(memory.read_u8(0x489) & 0x08, 0);
}

#[test]
fn test_cov_int10_alternate_select_grayscale_summing() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // BL=33h, AL=0: Enable grayscale summing
    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x33);
    cpu.regs.set_al(0x00);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x12);
    assert_ne!(memory.read_u8(0x489) & 0x02, 0);

    // BL=33h, AL=1: Disable grayscale summing
    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x33);
    cpu.regs.set_al(0x01);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x12);
    assert_eq!(memory.read_u8(0x489) & 0x02, 0);
}

#[test]
fn test_cov_int10_alternate_select_cursor_emulation() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // BL=34h, AL=1: Disable cursor emulation
    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x34);
    cpu.regs.set_al(0x01);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x12);
    assert_ne!(memory.read_u8(0x487) & 0x01, 0);
}

#[test]
fn test_cov_int10_alternate_select_screen_refresh() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();

    // BL=36h, AL=0: Enable refresh
    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x36);
    cpu.regs.set_al(0x00);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x12);

    // BL=36h, AL=1: Disable refresh
    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x36);
    cpu.regs.set_al(0x01);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.al(), 0x12);
}

#[test]
fn int10_11_20_sets_only_the_int_1f_vector() {
    let mut fresh = Memory::new();
    Bios::new().init_ivt(&mut fresh, false);
    let (memory, _) = graphics_font_call(0x20, 0x0010, 0x02, 0);
    assert_eq!(vector(&memory, 0x1F), (0x1234, 0x0010));
    assert_eq!(vector(&memory, 0x43), vector(&fresh, 0x43));
    assert_eq!(memory.read_u16(0x485), 0xBEEF, "20h leaves the BDA alone");
    assert_eq!(memory.read_u8(0x484), 0x77);
}

#[test]
fn int10_11_21_sets_int_43_height_from_cl_and_rows_from_bl() {
    // CL only: CX=012Ch stores 2Ch (and a zero high byte).
    let (memory, _) = graphics_font_call(0x21, 0x012C, 0x03, 0);
    assert_eq!(vector(&memory, 0x43), (0x1234, 0x0010));
    assert_eq!(memory.read_u16(0x485), 0x002C);
    assert_eq!(memory.read_u8(0x484), 42, "BL=3 selects 43 rows");

    let (memory, _) = graphics_font_call(0x21, 10, 0x00, 30);
    assert_eq!(memory.read_u8(0x484), 29, "BL=0 takes DL rows");
    let (memory, _) = graphics_font_call(0x21, 10, 0x01, 0);
    assert_eq!(memory.read_u8(0x484), 13, "BL=1 selects 14 rows");
    let (memory, _) = graphics_font_call(0x21, 10, 0x09, 0);
    assert_eq!(memory.read_u8(0x484), 24, "other BL values select 25 rows");
}

#[test]
fn int10_11_22_to_24_point_int_43_at_the_rom_tables() {
    for (al, offset, height) in [
        (0x22, Bios::ROM_FONT_8X14_OFFSET, 14),
        (0x23, Bios::ROM_FONT_8X8_OFFSET, 8),
        (0x24, Bios::ROM_FONT_8X16_OFFSET, 16),
    ] {
        let (memory, _) = graphics_font_call(al, 0xFFFF, 0x02, 0);
        assert_eq!(vector(&memory, 0x43), (0xF000, offset), "AL={al:02X}h");
        assert_eq!(memory.read_u16(0x485), height, "AL={al:02X}h");
        assert_eq!(memory.read_u8(0x484), 24, "AL={al:02X}h BL=2: 25 rows");
    }
}

#[test]
fn test_cov_int10_get_font_info_bh0() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // AH=11h, AL=30h, BH=0: Get INT 1Fh font pointer
    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x30);
    cpu.regs.set_bh(0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_int10_get_font_info_bh1() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // AH=11h, AL=30h, BH=1: Get INT 43h font pointer
    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x30);
    cpu.regs.set_bh(1);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_cov_int10_get_font_info_bh4() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // AH=11h, AL=30h, BH=4: Get 8x8 second half
    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x30);
    cpu.regs.set_bh(4);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(cpu.regs.es, 0xF000);
}

#[test]
fn test_cov_apm_get_power_state() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // APM 530Ch: Get Power State
    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x0C);
    cpu.regs.set_bx(0x0001);
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.cx(), 0x0000); // Ready
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_cov_apm_driver_version() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // APM 530Eh: Driver version
    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x0E);
    cpu.regs.set_bx(0);
    cpu.regs.set_ch(1); // Major = 1
    cpu.regs.set_cl(2); // Minor = 2
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ah(), 1); // Major
    assert_eq!(cpu.regs.al(), 2); // Minor
}

#[test]
fn test_cov_apm_driver_version_major_zero() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // APM 530Eh: Driver version with major=0
    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x0E);
    cpu.regs.set_bx(0);
    cpu.regs.set_ch(0); // Major = 0
    cpu.regs.set_cl(5); // Minor = 5
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0);
    assert_eq!(cpu.regs.al(), 5); // Should return requested minor
}

#[test]
fn test_cov_apm_enable_disable_power_mgmt() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // APM 5308h: Enable power management
    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x08);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // APM 5309h: Restore defaults
    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x09);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // APM 530Dh: Enable device power management
    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x0D);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // APM 530Fh: Engage power management
    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x0F);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_cov_apm_get_capabilities() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // APM 5310h: Get capabilities
    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x10);
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.bx(), 0x0000);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_cov_ega_ril_read_misc_output() {
    let vga = Vga::new();
    // DX=0x08 → group 4 (misc output), regs=0
    let val = Bios::ega_ril_read_register(&vga, 0, 0x08);
    // Should return misc output register value
    let _ = val;
}

#[test]
fn test_cov_ega_ril_read_feature_control() {
    let vga = Vga::new();
    // DX=0x28 → group 5 (feature control), regs=0
    let val = Bios::ega_ril_read_register(&vga, 0, 0x28);
    let _ = val;
}

#[test]
fn test_cov_ega_ril_write_misc_output() {
    let mut vga = Vga::new();
    // DX=0x08 → group 4 (misc output)
    Bios::ega_ril_write_register(&mut vga, 0, 0x67, 0x08);
    // Just verify it doesn't panic - the actual VGA misc output write
    // may go through different path than get_misc_output reads
}

#[test]
fn test_cov_ega_ril_write_feature_control() {
    let mut vga = Vga::new();
    // DX=0x28 → group 5 (feature control)
    Bios::ega_ril_write_register(&mut vga, 0, 0x42, 0x28);
}

#[test]
fn test_cov_ega_ril_read_register_out_of_range() {
    let vga = Vga::new();
    // Read with register index >= regs count (CRTC has 25 regs, try reg 30)
    let val = Bios::ega_ril_read_register(&vga, 30, 0x00); // group 0 (CRTC)
    assert_eq!(val, 0);
}

#[test]
fn test_cov_ega_ril_write_register_out_of_range() {
    let mut vga = Vga::new();
    // Write with register index >= regs count
    Bios::ega_ril_write_register(&mut vga, 30, 0xFF, 0x00); // CRTC, reg 30
                                                            // Should be a no-op
}

#[test]
fn test_cov_ega_ril_read_write_all_groups() {
    let mut vga = Vga::new();
    // Group 0: CRTC (DX=0x00)
    Bios::ega_ril_write_register(&mut vga, 0, 0x42, 0x00);
    let val = Bios::ega_ril_read_register(&vga, 0, 0x00);
    assert_eq!(val, 0x42);

    // Group 1: Sequencer (DX=0x18)
    Bios::ega_ril_write_register(&mut vga, 0, 0x33, 0x18);
    let val = Bios::ega_ril_read_register(&vga, 0, 0x18);
    assert_eq!(val, 0x33);

    // Group 2: Graphics (DX=0x20)
    Bios::ega_ril_write_register(&mut vga, 0, 0x11, 0x20);
    let val = Bios::ega_ril_read_register(&vga, 0, 0x20);
    assert_eq!(val, 0x11);

    // Group 3: Attribute (DX=0x10)
    Bios::ega_ril_write_register(&mut vga, 0, 0x55, 0x10);
    let val = Bios::ega_ril_read_register(&vga, 0, 0x10);
    assert_eq!(val, 0x55);
}
/// Set a video mode through INT 10h AH=00h and return the BDA 0x463 CRTC
/// base port the BIOS published for it.
fn crtc_base_after_mode_set(mode: u8) -> u16 {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(mode);
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x1000);
    memory.write_u16(0x1000, 0x0100);
    memory.write_u16(0x1002, 0xF000);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    memory.read_u16(0x463)
}

/// MDD-BUG-KIL-00852: AH=11h AL=20h-24h select GRAPHICS-mode character
/// tables. They repoint INT 1Fh / 43h and set the BDA height and rows,
/// and must not touch plane 2 - in a planar mode that is the picture's
/// red plane. The contract is the reference ROM's
fn graphics_font_call(al: u8, cx: u16, bl: u8, dl: u8) -> (Memory, Vga) {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);
    memory.plane_mut(2).fill(0x5A);
    memory.write_u16(0x485, 0xBEEF);
    memory.write_u8(0x484, 0x77);
    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(al);
    cpu.regs.es = 0x1234;
    cpu.regs.set_bp(0x0010);
    cpu.regs.set_cx(cx);
    cpu.regs.set_bl(bl);
    cpu.regs.set_dl(dl);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert!(
        memory.plane(2).iter().all(|&byte| byte == 0x5A),
        "AL={al:02X}h wrote plane 2"
    );
    assert!(
        !vga.custom_font_loaded,
        "AL={al:02X}h armed the text font latch"
    );
    (memory, vga)
}

#[test]
fn int10_colour_modes_publish_the_colour_crtc_base() {
    for mode in [0x00u8, 0x02, 0x03, 0x04, 0x06, 0x0D, 0x0F, 0x12, 0x13] {
        assert_eq!(
            crtc_base_after_mode_set(mode),
            0x3D4,
            "mode {mode:02X}h uses the colour I/O group; BDA 0x463 must be 0x3D4"
        );
    }
}

#[test]
fn native_font_int10_rom_load_at_all_native_addresses() {
    use gp64_hw::vga::{CP437_FONT_8X16, CP437_FONT_8X8};
    const OFFSETS: [usize; 8] = [
        0x0000, 0x4000, 0x8000, 0xC000, 0x2000, 0x6000, 0xA000, 0xE000,
    ];
    const SELECT_A: [u8; 8] = [0x00, 0x04, 0x08, 0x0C, 0x20, 0x24, 0x28, 0x2C];
    const SELECT_B: [u8; 8] = [0x00, 0x01, 0x02, 0x03, 0x10, 0x11, 0x12, 0x13];
    let (mut bios, mut cpu, mut memory, mut vga) =
        (Bios::new(), Cpu::new(), Memory::new(), Vga::new());
    for (ax, height, font) in [
        (0x1102, 8, &CP437_FONT_8X8[..]),
        (0x1104, 16, &CP437_FONT_8X16[..]),
    ] {
        for (bank, &offset) in OFFSETS.iter().enumerate() {
            // Sentinel makes a write to the wrong bank observable even though
            // every ROM load uses the same glyph data.
            memory.plane_mut(2).fill(0xA5);
            cpu.regs.set_ax(ax);
            cpu.regs.set_bl(bank as u8);
            bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
            assert!(vga.custom_font_loaded);
            for ch in [0, b'A', 0xFF] {
                for row in 0..32 {
                    let expected = if row < height {
                        font[ch as usize * height + row]
                    } else {
                        0
                    };
                    assert_eq!(
                        memory.plane(2)[offset + ch as usize * 32 + row],
                        expected,
                        "AX={ax:04x}, bank={bank}, glyph={ch}, row={row}: native ROM load"
                    );
                    for (spec, attr) in [(SELECT_A[bank], 8), (SELECT_B[bank], 0)] {
                        cpu.regs.set_ax(0x1103);
                        cpu.regs.set_bl(spec);
                        bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
                        assert_eq!(vga.get_font_row(&memory, ch, row, height, attr), expected);
                    }
                }
            }
            for (other, &other_offset) in OFFSETS.iter().enumerate() {
                if other != bank {
                    assert_eq!(
                        memory.plane(2)[other_offset],
                        0xA5,
                        "other bank must survive ROM load"
                    );
                }
            }
        }
    }
}
