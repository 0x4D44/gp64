use super::*;

#[test]
fn test_bios_new() {
    let bios = Bios::new();
    assert_eq!(bios.tick_count, 0);
}

#[test]
fn test_bios_default() {
    let bios = Bios::default();
    assert_eq!(bios.tick_count, 0);
}

#[test]
fn test_bios_tick() {
    let mut bios = Bios::new();
    assert_eq!(bios.tick_count, 0);
    assert!(!bios.tick());
    assert_eq!(bios.tick_count, 1);
    assert!(!bios.tick());
    assert_eq!(bios.tick_count, 2);
}

#[test]
fn test_bios_tick_wrapping() {
    let mut bios = Bios::new();
    bios.tick_count = BIOS_TICKS_PER_DAY - 1;
    assert!(bios.tick());
    assert_eq!(bios.tick_count, 0);
}

#[test]
fn test_to_bcd() {
    assert_eq!(to_bcd(0), 0x00);
    assert_eq!(to_bcd(9), 0x09);
    assert_eq!(to_bcd(10), 0x10);
    assert_eq!(to_bcd(23), 0x23);
    assert_eq!(to_bcd(59), 0x59);
    assert_eq!(to_bcd(99), 0x99);
}

#[test]
fn test_init_ivt() {
    let bios = Bios::new();
    let mut memory = Memory::new();

    bios.init_ivt(&mut memory, false);

    // Check that IVT entries are set up correctly
    // Note: Vectors 0x40-0xBF share group stub addresses for DOS extender compatibility.
    for vector in 0u16..256 {
        let ivt_addr = (vector as usize) * 4;
        let offset = memory.read_u16(ivt_addr);
        let segment = memory.read_u16(ivt_addr + 2);

        // INT 1Dh points to video parameter table
        if vector == 0x1D {
            assert_eq!(offset, Bios::ROM_VIDEO_PARAM_TABLE_OFFSET);
            assert_eq!(segment, 0xF000);
        } else if vector == 0x1E {
            // INT 1Eh points to the diskette parameter table (MDD-BUG-ANVIL-00015)
            assert_eq!(offset, Bios::ROM_DISKETTE_PARAM_TABLE_OFFSET);
            assert_eq!(segment, 0xF000);
        } else if vector == 0x1F {
            // INT 1Fh points to upper 128 chars of 8x8 font
            assert_eq!(offset, Bios::ROM_FONT_8X8_OFFSET + 1024);
            assert_eq!(segment, 0xF000);
        } else if vector == 0x43 {
            // INT 43h points to 8x8 font
            assert_eq!(offset, Bios::ROM_FONT_8X8_OFFSET);
            assert_eq!(segment, 0xF000);
        } else if vector == 0x08 {
            // INT 08h points at the real-bytes BIOS IRQ0 ISR at F000:FEA5
            // (matches the DOS model's BIOS_DEFAULT_IRQ0_LOCATION).  The legacy
            // F000:0120 IRET byte is still written (covered by
            // test_init_ivt_legacy_int8_stub_still_iret) but no longer
            // pointed at by the IVT.
            assert_eq!(segment, 0xF000);
            assert_eq!(offset, Bios::BIOS_INT8_ISR_OFF);
        } else if vector == 0x09 {
            // INT 09h keyboard IRQ1 stub in conventional memory
            assert_eq!(segment, Bios::KBD_IRQ1_STUB_SEG);
            assert_eq!(offset, Bios::KBD_IRQ1_STUB_OFF);
        } else if vector == 0x33 {
            // INT 33h mouse driver stub in conventional memory
            assert_eq!(segment, Bios::MOUSE_STUB_SEG);
            assert_eq!(offset, Bios::MOUSE_STUB_OFF);
        } else {
            // Verify segment is always F000
            assert_eq!(segment, 0xF000);

            // Check expected offset based on vector grouping
            let expected_offset = match vector {
                0x00..=0x3F => Bios::IVT_ENTRY_BASE + vector * 4,
                0x40..=0x5F => 0x0500, // Group 1 stub
                0x67 | 0x7A => Bios::IVT_ENTRY_BASE + vector * 4, // EMS / IPX need unique addresses
                0x60..=0x7F => 0x0580, // Group 2 stub
                0x80..=0x9F => 0x0600, // Group 3 stub
                0xA0..=0xBF => 0x0680, // Group 4 stub
                0xC0..=0xFF => Bios::IVT_ENTRY_BASE + vector * 4,
                _ => unreachable!(),
            };
            assert_eq!(
                offset, expected_offset,
                "Wrong offset for vector {vector:#04X}"
            );

            // Verify IRET stub is written at the entry point
            let linear_addr = Memory::seg_offset_to_linear(0xF000, expected_offset);
            assert_eq!(
                memory.read_u8(linear_addr),
                0xCF,
                "Missing IRET at vector {vector}"
            );
        }
    }

    // Verify font data was written to ROM
    let font_8x8_addr = Memory::seg_offset_to_linear(0xF000, Bios::ROM_FONT_8X8_OFFSET);
    // Check first byte of 8x8 font is non-zero (char 0 may be 0, check char 'A' = 65)
    let char_a_offset = 65 * 8; // 'A' is ASCII 65, 8 bytes per char
    assert_ne!(
        memory.read_u8(font_8x8_addr + char_a_offset),
        0,
        "Font data not written to ROM"
    );
}

#[test]
fn rom_system_table_does_not_corrupt_8x8_font_kiln74() {
    let bios = Bios::new();
    let mut memory = Memory::new();

    bios.init_ivt(&mut memory, false);

    let font_addr = Memory::seg_offset_to_linear(0xF000, Bios::ROM_FONT_8X8_OFFSET);
    for (index, &expected) in gp64_hw::vga::CP437_FONT_8X8.iter().enumerate() {
        assert_eq!(
            memory.read_u8(font_addr + index),
            expected,
            "8x8 ROM font byte {index} was overwritten"
        );
    }
}

#[test]
fn rom_video_table_does_not_corrupt_8x16_font_kiln74() {
    let bios = Bios::new();
    let mut memory = Memory::new();

    bios.init_ivt(&mut memory, false);

    let font_addr = Memory::seg_offset_to_linear(0xF000, Bios::ROM_FONT_8X16_OFFSET);
    for (index, &expected) in gp64_hw::vga::CP437_FONT_8X16.iter().enumerate() {
        assert_eq!(
            memory.read_u8(font_addr + index),
            expected,
            "8x16 ROM font byte {index} was overwritten"
        );
    }
}

/// Constants encoding the canonical IBM PC/AT IRQ0 location matching
/// the DOS model. Regression guard: if either offset shifts, callers in
/// `hle_dispatch.rs` that compare `ip == BIOS_INT8_TICK_TRAP_OFF` will
/// silently fail to fire the BIOS-tick HLE trap.
#[test]
fn test_init_ivt_int8_constants() {
    assert_eq!(Bios::BIOS_INT8_ISR_OFF, 0xFEA5);
    assert_eq!(Bios::BIOS_INT8_TICK_TRAP_OFF, 0xFEA6);
    assert_eq!(
        Bios::BIOS_INT8_TICK_TRAP_OFF,
        Bios::BIOS_INT8_ISR_OFF + 1,
        "tick trap must be at ISR + 1 so the dispatcher catches it \
         after the STI byte has been decoded"
    );
}

/// The 16-byte BIOS INT 8 ISR at F000:FEA5..F000:FEB4 matches the DOS model's
/// CB_IRQ0 sequence (`src/cpu/callback.cpp:393-412`, `use_cb` path with
/// our 1-byte HLE trap replacing the 4-byte callback trap).  See HLD §3.1.
#[test]
fn test_init_ivt_int8_isr_bytes() {
    const EXPECTED: [u8; 16] = [
        0xFB, 0xF1, 0x1E, 0x50, 0x52, 0xCD, 0x1C, 0xFA, 0xB0, 0x20, 0xE6, 0x20, 0x5A, 0x58, 0x1F,
        0xCF,
    ];
    let bios = Bios::new();
    let mut memory = Memory::new();
    bios.init_ivt(&mut memory, false);

    let isr_base = Memory::seg_offset_to_linear(0xF000, Bios::BIOS_INT8_ISR_OFF);
    for (i, &expected) in EXPECTED.iter().enumerate() {
        let actual = memory.read_u8(isr_base + i);
        assert_eq!(
            actual,
            expected,
            "BIOS INT 8 ISR byte {i} mismatch at F000:{:04X}: \
             expected 0x{expected:02X}, got 0x{actual:02X}",
            Bios::BIOS_INT8_ISR_OFF as usize + i
        );
    }
}

/// IVT[0x08] must point at the real-bytes ISR at F000:FEA5, not at the
/// legacy F000:0120 IRET stub.  Without this, the v3 ISR is unreachable
/// from a freshly-installed INT 8 vector.
#[test]
fn test_init_ivt_int8_ivt_pointer() {
    let bios = Bios::new();
    let mut memory = Memory::new();
    bios.init_ivt(&mut memory, false);

    let ivt_off = memory.read_u16(0x08 * 4);
    let ivt_seg = memory.read_u16(0x08 * 4 + 2);
    assert_eq!(ivt_off, 0xFEA5, "IVT[0x08] offset must be 0xFEA5");
    assert_eq!(ivt_seg, 0xF000, "IVT[0x08] segment must be 0xF000");
}

/// The legacy F000:0120 single-byte IRET stub is still written by the
/// unchanged IVT-entry loop branch.  No code points at it any more, but
/// it must remain a benign IRET for any code that incidentally reads
/// the address.
#[test]
fn test_init_ivt_legacy_int8_stub_still_iret() {
    let bios = Bios::new();
    let mut memory = Memory::new();
    bios.init_ivt(&mut memory, false);

    let legacy_addr = Memory::seg_offset_to_linear(0xF000, Bios::IVT_ENTRY_BASE + 0x08 * 4);
    assert_eq!(
        memory.read_u8(legacy_addr),
        0xCF,
        "legacy F000:0120 IRET stub must remain — \
         present as a defence-in-depth no-op"
    );
}

/// IVT[0x1C] still points at F000:0170 (the IVT_ENTRY_BASE convention),
/// and that address still contains a single-byte IRET.  This is what
/// the v3 BIOS INT 8 ISR's `CD 1C` dispatches into when no user has
/// hooked INT 1Ch.  See HLD §3.2 Case A.
#[test]
fn test_init_ivt_int1c_ivt_pointer() {
    let bios = Bios::new();
    let mut memory = Memory::new();
    bios.init_ivt(&mut memory, false);

    let ivt_off = memory.read_u16(0x1C * 4);
    let ivt_seg = memory.read_u16(0x1C * 4 + 2);
    assert_eq!(ivt_off, 0x0170, "IVT[0x1C] offset must be 0x0170");
    assert_eq!(ivt_seg, 0xF000, "IVT[0x1C] segment must be 0xF000");

    let stub_addr = Memory::seg_offset_to_linear(0xF000, 0x0170);
    assert_eq!(
        memory.read_u8(stub_addr),
        0xCF,
        "F000:0170 must contain a single IRET byte"
    );
}

/// Ordering regression guard: the PM IDT stub loop writes 0xCF 0x90
/// IRET+NOP pairs across F000:FE00..FFFF, which would overwrite the
/// BIOS INT 8 ISR bytes at F000:FEA5..FEB4.  `init_ivt` must install
/// the ISR *after* the PM stub loop has run so the ISR bytes survive.
/// If a future refactor swaps the order, this test catches it.
#[test]
fn test_init_ivt_pm_stub_loop_runs_before_isr_overwrite() {
    let bios = Bios::new();
    let mut memory = Memory::new();
    bios.init_ivt(&mut memory, false);

    let first_byte = memory.read_u8(Memory::seg_offset_to_linear(
        0xF000,
        Bios::BIOS_INT8_ISR_OFF,
    ));
    assert_eq!(
        first_byte, 0xFB,
        "byte at F000:FEA5 must be 0xFB (STI) — if it is 0xCF, the PM \
         stub loop wrote AFTER the ISR install and clobbered the ISR.  \
         The PM stub loop must run FIRST, then the ISR overwrite."
    );
}

#[test]
fn test_int11_equipment_list() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();

    let result = bios.int11(&mut cpu);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ax(), Bios::EQUIPMENT_WORD);
}

#[test]
fn test_int11_equipment_word_has_fpu_bit() {
    // Bit 1 must be set to indicate math coprocessor installed.
    assert_ne!(
        Bios::EQUIPMENT_WORD & 0x02,
        0,
        "EQUIPMENT_WORD bit 1 (math coprocessor) must be set"
    );

    // Verify INT 11h returns it
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let _ = bios.int11(&mut cpu);
    assert_ne!(
        cpu.regs.ax() & 0x02,
        0,
        "INT 11h return value bit 1 (math coprocessor) must be set"
    );
}

#[test]
fn test_int11_equipment_word_bda_consistency() {
    // The equipment word stored in BDA at 0x0410 must match
    // directly instead of calling INT 11h.
    let bios = Bios::new();
    let mut memory = Memory::new();
    bios.init_ivt(&mut memory, false);

    let bda_value = memory.read_u16(0x0410);
    assert_eq!(
        bda_value,
        Bios::EQUIPMENT_WORD,
        "BDA equipment word at 0x0410 must match EQUIPMENT_WORD constant"
    );

    // Also verify the FPU bit is set in the BDA copy
    assert_ne!(
        bda_value & 0x02,
        0,
        "BDA equipment word bit 1 (math coprocessor) must be set"
    );
}

#[test]
fn test_floppy_drives_fitted_decodes_equipment_word() {
    // Bit 0 = any floppy fitted; bits 7:6 = count - 1.
    assert_eq!(Bios::floppy_drives_fitted(Bios::EQUIPMENT_WORD), 1);
    assert_eq!(Bios::floppy_drives_fitted(0x0426), 0, "bit 0 clear");
    assert_eq!(
        Bios::floppy_drives_fitted(0x00C0),
        0,
        "count bits without bit 0"
    );
    assert_eq!(Bios::floppy_drives_fitted(0x0467), 2);
    assert_eq!(Bios::floppy_drives_fitted(0x00C1), 4);
}

#[test]
fn test_bda_com_port_base_addresses() {
    let bios = Bios::new();
    let mut memory = Memory::new();
    bios.init_ivt(&mut memory, false);

    assert_eq!(
        memory.read_u16(0x400),
        0x03F8,
        "BDA COM1 base must be 0x3F8"
    );
    assert_eq!(
        memory.read_u16(0x402),
        0x02F8,
        "BDA COM2 base must be 0x2F8"
    );

    let equipment = memory.read_u16(0x410);
    let serial_count = (equipment >> 9) & 0x07;
    assert_eq!(serial_count, 2, "Equipment word must report 2 serial ports");
}

#[test]
fn test_bda_hard_disk_count() {
    let bios = Bios::new();
    let mut memory = Memory::new();
    bios.init_ivt(&mut memory, false);

    assert_eq!(
        memory.read_u8(0x475),
        1,
        "BDA hard disk count must be 1 (matches CMOS 0x12 = 0xF0)"
    );
}

#[test]
fn test_bda_conventional_memory_size_word() {
    let bios = Bios::new();
    let mut memory = Memory::new();
    bios.init_ivt(&mut memory, false);

    assert_eq!(
        memory.read_u16(0x413),
        Bios::CONVENTIONAL_MEMORY_KB,
        "BDA conventional memory word at 0040:0013 must be 640 KiB"
    );
    assert_eq!(memory.read_u8(0x413), 0x80);
    assert_eq!(memory.read_u8(0x414), 0x02);
}

#[test]
fn test_bda_conventional_memory_size_written_when_video_init_skipped() {
    let bios = Bios::new();
    let mut memory = Memory::new();
    bios.init_ivt(&mut memory, true);

    assert_eq!(
        memory.read_u16(0x413),
        Bios::CONVENTIONAL_MEMORY_KB,
        "BDA conventional memory size is not video-owned"
    );
}

#[test]
fn test_int12_memory_size() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();

    let result = bios.int12(&mut cpu);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ax(), Bios::CONVENTIONAL_MEMORY_KB);
}

#[test]
fn test_int12_memory_size_matches_bda() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    bios.init_ivt(&mut memory, false);

    let result = bios.int12(&mut cpu);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ax(), memory.read_u16(0x413));
    assert_eq!(cpu.regs.ax(), Bios::CONVENTIONAL_MEMORY_KB);
}

#[test]
fn test_int15_keyboard_intercept() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x4F);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_wait() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x86);
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(1);
    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert_eq!(result.unwrap().0, Some(1));
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_wait_non_blocking() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x86);
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(50_000); // 50ms

    let start = std::time::Instant::now();
    let result = bios.int15(&mut cpu, &mut memory, None);
    let elapsed = start.elapsed();

    assert!(result.is_ok());
    assert!(
        elapsed < Duration::from_millis(10),
        "INT 15h wait should not sleep in emulator mode"
    );
}

#[test]
fn test_int15_extended_memory_size() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    memory.ensure_ram_size(4 * 1024 * 1024);
    cpu.regs.set_ah(0x88);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    // INT 15h/88h caps at 0x3C00 (15360 KB) to match CMOS registers
    let expected_kb =
        ((memory.ram_size() as u32).saturating_sub(0x100000) / 1024).min(0x3C00) as u16;
    assert_eq!(cpu.regs.ax(), expected_kb);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_e801_extended_memory_size() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Truncate to exactly 16MB so E801 reports only 1-16MB range
    memory.resize_ram(16 * 1024 * 1024);
    cpu.regs.set_ax(0xE801);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());

    // Memory between 1MB and 16MB in KB (15MB = 15360 KB = 0x3C00)
    let expected_1_to_16_kb: u16 = 0x3C00;
    // No memory above 16MB
    let expected_above_16_blocks: u16 = 0;

    assert_eq!(cpu.regs.ax(), expected_1_to_16_kb);
    assert_eq!(cpu.regs.bx(), expected_above_16_blocks);
    assert_eq!(cpu.regs.cx(), expected_1_to_16_kb);
    assert_eq!(cpu.regs.dx(), expected_above_16_blocks);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_e801_small_memory() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Truncate to 4MB to test small memory reporting
    memory.resize_ram(4 * 1024 * 1024);
    cpu.regs.set_ax(0xE801);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());

    // Memory between 1MB and 16MB in KB (4MB - 1MB = 3MB = 3072 KB)
    let expected_1_to_16_kb: u16 = 3072;
    // No memory above 16MB
    let expected_above_16_blocks: u16 = 0;

    assert_eq!(cpu.regs.ax(), expected_1_to_16_kb);
    assert_eq!(cpu.regs.bx(), expected_above_16_blocks);
    assert_eq!(cpu.regs.cx(), expected_1_to_16_kb);
    assert_eq!(cpu.regs.dx(), expected_above_16_blocks);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_e820_memory_map() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up 16MB RAM
    memory.ensure_ram_size(16 * 1024 * 1024);

    // E820 signature "SMAP"
    const SMAP: u32 = 0x534D4150;

    // Set up buffer at ES:DI (use 0x1000:0x0000)
    let buffer_seg = 0x1000u16;
    let buffer_off = 0x0000u16;
    cpu.regs.es = buffer_seg;
    cpu.regs.set_di(buffer_off);

    // First call: EAX=E820, EBX=0 (first entry), ECX=20, EDX=SMAP
    cpu.regs.eax = 0xE820;
    cpu.regs.ebx = 0; // Continuation = 0 for first call
    cpu.regs.ecx = 20; // Buffer size
    cpu.regs.edx = SMAP;

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());

    // Verify return values
    assert_eq!(cpu.regs.eax, SMAP, "EAX should return 'SMAP'");
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "CF should be clear on success"
    );
    assert!(cpu.regs.ecx >= 20, "ECX should be >= 20 (entry size)");
    assert!(cpu.regs.ebx != 0, "EBX should be non-zero (more entries)");

    // Read first entry from buffer (all 640 KiB of conventional memory)
    let linear_addr = (buffer_seg as u32 * 16 + buffer_off as u32) as usize;
    let base = memory.read_u64(linear_addr);
    let length = memory.read_u64(linear_addr + 8);
    let mem_type = memory.read_u32(linear_addr + 16);

    assert_eq!(base, 0, "First entry base should be 0");
    assert_eq!(length, 0xA0000, "First entry length should be 640 KiB");
    assert_eq!(mem_type, 1, "First entry type should be 1 (usable)");

    // Continue iteration to get all entries (max 10 to avoid infinite loop)
    let mut entry_count = 1;
    for _ in 0..10 {
        if cpu.regs.ebx == 0 {
            break;
        }
        cpu.regs.eax = 0xE820;
        cpu.regs.ecx = 20;
        cpu.regs.edx = SMAP;

        let result = bios.int15(&mut cpu, &mut memory, None);
        assert!(result.is_ok());
        assert_eq!(cpu.regs.eax, SMAP);
        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

        entry_count += 1;
    }

    // Conventional, video/ROM, and extended memory.
    assert!(
        entry_count >= 3,
        "Should have at least 3 memory map entries, got {}",
        entry_count
    );
}

#[test]
fn test_int15_e820_invalid_signature() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Call with wrong signature
    cpu.regs.eax = 0xE820;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 20;
    cpu.regs.edx = 0x12345678; // Wrong signature

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());

    // Should fail with carry flag set
    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "CF should be set for invalid signature"
    );
}

#[test]
fn test_int15_a20_disable() {
    // Disable A20 via INT 15h AH=24h AL=00h
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x24);
    cpu.regs.set_al(0x00);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0);
    assert!(!memory.a20_enabled()); // A20 is disabled after disable call
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_a20_enable() {
    // A20 emulation removed - enable returns success, A20 always enabled
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x24);
    cpu.regs.set_al(0x01);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ah(), 0);
    assert!(memory.a20_enabled());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_a20_get_status() {
    // A20 emulation removed - status always returns 1 (enabled)
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x24);
    cpu.regs.set_al(0x02);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 1); // Always enabled
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// Test A20 functions return success (A20 emulation removed - always enabled)
#[test]
fn test_a20_functions_return_success() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Enable A20 via INT 15h - should return success
    cpu.regs.set_ah(0x24);
    cpu.regs.set_al(0x01);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert_eq!(cpu.regs.ah(), 0, "Enable should return success");
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Disable A20 via INT 15h - should return success
    cpu.regs.set_ah(0x24);
    cpu.regs.set_al(0x00);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert_eq!(cpu.regs.ah(), 0, "Disable should return success");
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // After disable, A20 should be disabled
    assert!(
        !memory.a20_enabled(),
        "A20 should be disabled after disable call"
    );

    // Query status - should return 0 (disabled)
    cpu.regs.set_ah(0x24);
    cpu.regs.set_al(0x02);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert_eq!(cpu.regs.al(), 0, "Status should return 0 (disabled)");
}

/// Test extended memory size consistency between AH=88h and AX=E801h
#[test]
fn test_memory_88h_e801_consistency() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up 8MB RAM
    memory.ensure_ram_size(8 * 1024 * 1024);
    let ram_size = memory.ram_size() as u64;

    // Get extended memory via AH=88h
    cpu.regs.set_ah(0x88);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    let ext_kb_88h = cpu.regs.ax() as u64;

    // Get extended memory via AX=E801h
    cpu.regs.set_ax(0xE801);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    let mem_1_to_16_kb = cpu.regs.ax() as u64;
    let mem_above_16_blocks = cpu.regs.bx() as u64;

    // Calculate total extended memory from E801
    // mem_1_to_16_kb is in KB, mem_above_16_blocks is in 64KB blocks
    let ext_kb_e801 = mem_1_to_16_kb + (mem_above_16_blocks * 64);

    // AH=88h is capped at 15360 KB (matching CMOS registers)
    // E801 reports the full extended memory
    let expected_ext_kb = (ram_size.saturating_sub(0x10_0000)) / 1024;
    let expected_88h = expected_ext_kb.min(0x3C00);

    assert_eq!(
        ext_kb_88h, expected_88h,
        "AH=88h should return {} KB (capped), got {} KB",
        expected_88h, ext_kb_88h
    );
    assert_eq!(
        ext_kb_e801, expected_ext_kb,
        "E801 should return {} KB for full RAM, got {} KB",
        expected_ext_kb, ext_kb_e801
    );
}

/// Test E820 vs E801 consistency for extended memory region
#[test]
fn test_memory_e820_e801_consistency() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up 16MB RAM
    memory.ensure_ram_size(16 * 1024 * 1024);

    // Get extended memory via E801
    cpu.regs.set_ax(0xE801);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    let mem_1_to_16_kb = cpu.regs.ax() as u64;
    let mem_above_16_blocks = cpu.regs.bx() as u64;
    let e801_total_bytes = (mem_1_to_16_kb * 1024) + (mem_above_16_blocks * 64 * 1024);

    // Iterate E820 to find extended memory region (base = 0x100000, type = 1)
    const SMAP: u32 = 0x534D4150;
    let buffer_seg = 0x2000u16;
    let buffer_off = 0x0000u16;

    let mut extended_memory_bytes: u64 = 0;
    let mut continuation: u32 = 0;

    loop {
        cpu.regs.es = buffer_seg;
        cpu.regs.set_di(buffer_off);
        cpu.regs.eax = 0xE820;
        cpu.regs.ebx = continuation;
        cpu.regs.ecx = 20;
        cpu.regs.edx = SMAP;

        bios.int15(&mut cpu, &mut memory, None).unwrap();

        let linear_addr = (buffer_seg as u32 * 16 + buffer_off as u32) as usize;
        let base = memory.read_u64(linear_addr);
        let length = memory.read_u64(linear_addr + 8);
        let mem_type = memory.read_u32(linear_addr + 16);

        // Extended memory region starts at 1MB (0x100000) and type = 1 (usable)
        if base == 0x10_0000 {
            assert_eq!(
                mem_type, 1,
                "extended memory region should be type 1 (usable)"
            );
            extended_memory_bytes = length;
        }

        continuation = cpu.regs.ebx;
        if continuation == 0 {
            break;
        }
    }

    // E820 extended memory length should match E801 total
    assert_eq!(
        extended_memory_bytes, e801_total_bytes,
        "E820 extended memory ({} bytes) should match E801 total ({} bytes)",
        extended_memory_bytes, e801_total_bytes
    );
}

/// Test memory size with default memory (32MB)
#[test]
fn test_memory_size_default() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Memory::new() exposes DEFAULT_RAM_SIZE usable RAM
    let ram_size = memory.ram_size() as u64;
    // AH=88h is capped at 0x3C00 (15360 KB) to match CMOS
    let expected_ext_kb = (ram_size.saturating_sub(0x10_0000) / 1024).min(0x3C00) as u16;

    // AH=88h returns extended KB, capped at 0x3C00
    cpu.regs.set_ah(0x88);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert_eq!(
        cpu.regs.ax(),
        expected_ext_kb,
        "AH=88h should return {} KB for default memory",
        expected_ext_kb
    );

    // E801 splits memory into 1-16MB (in KB) and above 16MB (in 64KB blocks)
    let expected_1_to_16_kb = ((ram_size - 0x10_0000).min(0xF0_0000) / 1024) as u16;
    let expected_above_16_blocks = ((ram_size.saturating_sub(0x100_0000)) / 0x1_0000) as u16;

    cpu.regs.set_ax(0xE801);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert_eq!(
        cpu.regs.ax(),
        expected_1_to_16_kb,
        "E801 AX should be {} KB (1-16MB range)",
        expected_1_to_16_kb
    );
    assert_eq!(
        cpu.regs.bx(),
        expected_above_16_blocks,
        "E801 BX should be {} (64KB blocks above 16MB)",
        expected_above_16_blocks
    );
}

/// Test memory size with 4MB RAM
#[test]
fn test_memory_size_4mb() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Truncate to exactly 4MB
    memory.resize_ram(4 * 1024 * 1024);
    let ram_size = memory.ram_size() as u64;

    // Expected extended memory: 4MB - 1MB = 3MB = 3072 KB
    let expected_ext_kb = ((ram_size - 0x10_0000) / 1024) as u16;

    // AH=88h
    cpu.regs.set_ah(0x88);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert_eq!(
        cpu.regs.ax(),
        expected_ext_kb,
        "AH=88h should return {} KB for 4MB",
        expected_ext_kb
    );

    // E801: for <=16MB, AX matches AH=88h
    cpu.regs.set_ax(0xE801);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert_eq!(
        cpu.regs.ax(),
        expected_ext_kb,
        "E801 AX should be {} KB",
        expected_ext_kb
    );
    assert_eq!(
        cpu.regs.bx(),
        0,
        "E801 BX should be 0 (no memory above 16MB)"
    );
}

/// Test memory size with 16MB RAM
#[test]
fn test_memory_size_16mb() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Truncate to exactly 16MB
    memory.resize_ram(16 * 1024 * 1024);

    // Expected extended memory: 16MB - 1MB = 15MB = 15360 KB (0x3C00)
    let expected_ext_kb: u16 = 0x3C00;

    // AH=88h
    cpu.regs.set_ah(0x88);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert_eq!(
        cpu.regs.ax(),
        expected_ext_kb,
        "AH=88h should return {} KB for 16MB",
        expected_ext_kb
    );

    // E801 - memory between 1-16MB in KB
    cpu.regs.set_ax(0xE801);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert_eq!(
        cpu.regs.ax(),
        expected_ext_kb,
        "E801 AX should be {} KB",
        expected_ext_kb
    );
    assert_eq!(
        cpu.regs.bx(),
        0,
        "E801 BX should be 0 (no memory above 16MB)"
    );
    // Verify CX/DX mirror AX/BX
    assert_eq!(cpu.regs.cx(), cpu.regs.ax(), "E801 CX should mirror AX");
    assert_eq!(cpu.regs.dx(), cpu.regs.bx(), "E801 DX should mirror BX");
}

#[test]
fn int15_89_refuses_a_gdt_that_cannot_describe_its_selectors_without_half_switching() {
    // MDD-BUG-TEM-00068. AH=89h committed irreversible machine state - both
    // 8259s reprogrammed with their IMRs masked to 0xFF, A20 forced on,
    // GDTR/IDTR loaded, CR0.PE set - BEFORE its only two fallible calls,
    // `load_ss(0x28)` and `load_cs(0x30)`. The GDT limit those validate
    // against comes straight from the caller's build table, so a guest could
    // declare a GDT too small to hold selector 0x28: the load returned Err,
    // `?` carried it out of `int15` and out of `dispatch_hle_bios`, and the
    // machine was left in protected mode with DS and ES loaded, no valid SS,
    // the INT frame still on the stack, and the PIC reprogramming already
    // committed. Real hardware raises #GP inside the BIOS; it does not
    // half-switch and unwind into the host.
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut pic = gp64_hw::Pic::new();

    // Five descriptors only (0x00..0x20), so selector 0x28 is past the
    // limit - exactly the record's worked example.
    let gdt = 0x0800usize;
    build_int15_89_table(&mut memory, gdt, 0x0027);

    memory.set_a20(false); // the real-mode DOS state this is called from
    cpu.regs.es = 0x0080; // ES:SI -> the build table at 0x800
    cpu.regs.set_si(0);
    cpu.regs.set_ah(0x89);
    cpu.regs.set_bh(0x50);
    cpu.regs.set_bl(0x60);
    let ss_before = cpu.regs.ss;
    let cs_before = cpu.regs.cs;
    let before = pic.diagnostic_snapshot();

    let result = bios.int15(&mut cpu, &mut memory, Some(&mut pic));

    // The error must not escape into the host.
    assert!(
        result.is_ok(),
        "a caller's bad GDT must not unwind out of the BIOS"
    );
    assert_eq!(cpu.regs.ah(), 0x86, "the arm's own failure answer");
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Nothing may have half-switched.
    assert_eq!(cpu.regs.cr0 & 1, 0, "CR0.PE must not be left set");
    assert_eq!(cpu.regs.ss, ss_before, "SS must be untouched");
    assert_eq!(cpu.regs.cs, cs_before, "CS must be untouched");
    assert!(
        !memory.a20_enabled(),
        "the caller's A20 gate must be restored"
    );

    // And the irreversible half must never have happened.
    let after = pic.diagnostic_snapshot();
    assert_eq!(
        (after.master.imr, after.slave.imr),
        (before.master.imr, before.slave.imr),
        "the 8259 IMRs must not be masked by a call that failed - this is \
         the part no guest could recover from"
    );
    assert_eq!(
        (after.master.vector_offset, after.slave.vector_offset),
        (before.master.vector_offset, before.slave.vector_offset),
        "the 8259s must not be revectored by a call that failed"
    );
}

#[test]
fn int15_89_still_switches_when_the_gdt_describes_every_selector() {
    // The other half of MDD-BUG-TEM-00068: the guarded ordering must not
    // cost the success path. A GDT that does describe 0x18/0x20/0x28/0x30
    // still switches, and the 8259s are still reprogrammed to BH/BL with
    // every line masked.
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut pic = gp64_hw::Pic::new();

    let gdt = 0x0800usize;
    build_int15_89_table(&mut memory, gdt, 0x003F); // all eight descriptors

    memory.set_a20(false);
    cpu.regs.es = 0x0080;
    cpu.regs.set_si(0);
    cpu.regs.set_ah(0x89);
    cpu.regs.set_bh(0x50);
    cpu.regs.set_bl(0x60);

    bios.int15(&mut cpu, &mut memory, Some(&mut pic)).unwrap();

    assert_eq!(cpu.regs.ax(), 0, "AX=0 signals success");
    assert_eq!(cpu.regs.cr0 & 1, 1, "CR0.PE set");
    assert_eq!(cpu.regs.cs, 0x30);
    assert_eq!(cpu.regs.ss, 0x28);
    assert!(memory.a20_enabled(), "a successful switch leaves A20 open");
    let after = pic.diagnostic_snapshot();
    assert_eq!(
        (
            after.master.vector_offset,
            after.slave.vector_offset,
            after.master.imr,
            after.slave.imr
        ),
        (0x50, 0x60, 0xFF, 0xFF),
        "the committed path still reprograms both 8259s"
    );
}

#[test]
fn test_int15_get_system_config() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0xC0);

    let result = bios.int15(&mut cpu, &mut memory, None);
    assert!(result.is_ok());
    // Now properly supported - returns ES:BX pointing to config table
    assert_eq!(cpu.regs.ah(), 0x00); // Success
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.es, 0xF000);
    assert_eq!(cpu.regs.bx(), Bios::ROM_SYSTEM_CONFIG_TABLE_OFFSET);
}

#[test]
fn test_int1a_get_system_time() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    // Set tick count in BDA
    let tick_addr = 0x0040 * 16 + 0x006C;
    memory.write_u32(tick_addr, 0x12345678);

    cpu.regs.set_ah(0x00);

    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0); // Midnight flag
    assert_eq!(cpu.regs.cx(), 0x1234);
    assert_eq!(cpu.regs.dx(), 0x5678);
}

#[test]
fn test_int1a_get_system_time_midnight_flag() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    memory.write_u32(BDA_TICK_COUNT_ADDR, 0x00000010);
    memory.write_u8(BDA_MIDNIGHT_FLAG_ADDR, 1);

    cpu.regs.set_ah(0x00);
    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 1);
    assert_eq!(memory.read_u8(BDA_MIDNIGHT_FLAG_ADDR), 0);
}

#[test]
fn test_int1a_set_system_time() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    cpu.regs.set_ah(0x01);
    cpu.regs.set_cx(0xABCD);
    cpu.regs.set_dx(0xEF01);

    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    );
    assert!(result.is_ok());

    let tick_addr = 0x0040 * 16 + 0x006C;
    assert_eq!(memory.read_u32(tick_addr), 0xABCDEF01);
    assert_eq!(bios.tick_count, 0xABCDEF01);
}

#[test]
fn test_int1a_get_rtc_time() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    cpu.regs.set_ah(0x02);

    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // Time values should be in BCD format
    // Hours: 0-23 -> 0x00-0x23
    // Minutes: 0-59 -> 0x00-0x59
    // Seconds: 0-59 -> 0x00-0x59
}

#[test]
fn test_int1a_get_rtc_date() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    cpu.regs.set_ah(0x04);

    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // Date should be in BCD format and within valid ranges
    let century = cpu.regs.ch();
    let year = cpu.regs.cl();
    let month = cpu.regs.dh();
    let day = cpu.regs.dl();
    // Century should be 19, 20, or 21 in BCD
    assert!(
        matches!(century, 0x19 | 0x20 | 0x21),
        "century BCD {century:#04X} out of range"
    );
    // Year should be 0x00-0x99 (valid BCD)
    assert!(year <= 0x99, "year BCD {year:#04X} > 0x99");
    assert!((year & 0x0F) <= 9, "year BCD low nibble > 9");
    assert!(((year >> 4) & 0x0F) <= 9, "year BCD high nibble > 9");
    // Month should be 0x01-0x12
    assert!((0x01..=0x12).contains(&month));
    // Day should be 0x01-0x31
    assert!((0x01..=0x31).contains(&day));
}

#[test]
fn test_int1a_set_rtc_time() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    // Set time to 14:30:45 in BCD
    cpu.regs.set_ah(0x03);
    cpu.regs.set_ch(0x14); // Hours (14 = 0x14 BCD)
    cpu.regs.set_cl(0x30); // Minutes (30 = 0x30 BCD)
    cpu.regs.set_dh(0x45); // Seconds (45 = 0x45 BCD)
    cpu.regs.set_dl(0x00); // DST flag (0 = standard time)

    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int1a_set_rtc_date() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    // Set date to 2026-01-17 in BCD
    cpu.regs.set_ah(0x05);
    cpu.regs.set_ch(0x20); // Century (20)
    cpu.regs.set_cl(0x26); // Year (26)
    cpu.regs.set_dh(0x01); // Month (01)
    cpu.regs.set_dl(0x17); // Day (17)

    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int1a_set_rtc_alarm_programs_cmos() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    cpu.regs.set_ah(0x06);
    cpu.regs.set_ch(0x12);
    cpu.regs.set_cl(0x34);
    cpu.regs.set_dh(0x56);

    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    cmos.write_index(gp64_hw::cmos::regs::HOURS_ALARM);
    assert_eq!(cmos.read_data(), 0x12);
    cmos.write_index(gp64_hw::cmos::regs::MINUTES_ALARM);
    assert_eq!(cmos.read_data(), 0x34);
    cmos.write_index(gp64_hw::cmos::regs::SECONDS_ALARM);
    assert_eq!(cmos.read_data(), 0x56);
    cmos.write_index(gp64_hw::cmos::regs::STATUS_B);
    assert_ne!(cmos.read_data() & 0x20, 0);
}

#[test]
fn test_int1a_set_rtc_alarm_busy_preserves_existing_alarm() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    cpu.regs.set_ah(0x06);
    cpu.regs.set_ch(0x12);
    cpu.regs.set_cl(0x34);
    cpu.regs.set_dh(0x56);
    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    cpu.regs.set_ah(0x06);
    cpu.regs.set_ch(0x21);
    cpu.regs.set_cl(0x43);
    cpu.regs.set_dh(0x65);
    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ah(), 0x06, "busy arm must preserve AH");

    cmos.write_index(gp64_hw::cmos::regs::HOURS_ALARM);
    assert_eq!(cmos.read_data(), 0x12);
    cmos.write_index(gp64_hw::cmos::regs::MINUTES_ALARM);
    assert_eq!(cmos.read_data(), 0x34);
    cmos.write_index(gp64_hw::cmos::regs::SECONDS_ALARM);
    assert_eq!(cmos.read_data(), 0x56);
}

#[test]
fn test_int1a_reset_rtc_alarm_clears_aie() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    cpu.regs.set_ah(0x06);
    cpu.regs.set_ch(0x01);
    cpu.regs.set_cl(0x02);
    cpu.regs.set_dh(0x03);
    let _ = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    );

    cpu.regs.set_ah(0x07);
    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    cmos.write_index(gp64_hw::cmos::regs::STATUS_B);
    assert_eq!(cmos.read_data() & 0x20, 0);
}

#[test]
fn test_int1a_set_system_time_clears_midnight_flag() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    // Pre-set midnight flag
    memory.write_u8(BDA_MIDNIGHT_FLAG_ADDR, 1);

    // Set system time
    cpu.regs.set_ah(0x01);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dx(0x1000);

    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    );
    assert!(result.is_ok());

    // Midnight flag should be cleared by set system time
    assert_eq!(memory.read_u8(BDA_MIDNIGHT_FLAG_ADDR), 0);
}

#[test]
fn test_int1a_get_system_time_multiple_reads_clear_flag_once() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    // Set up tick count and midnight flag
    memory.write_u32(BDA_TICK_COUNT_ADDR, 0x00001234);
    memory.write_u8(BDA_MIDNIGHT_FLAG_ADDR, 1);

    // First read should return midnight flag = 1 and clear it
    cpu.regs.set_ah(0x00);
    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 1); // Midnight occurred
    assert_eq!(memory.read_u8(BDA_MIDNIGHT_FLAG_ADDR), 0); // Flag cleared

    // Second read should return midnight flag = 0
    cpu.regs.set_ah(0x00);
    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0); // No midnight since last read
}

#[test]
fn test_int1a_get_rtc_time_returns_valid_bcd() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut cmos = Cmos::new();

    cpu.regs.set_ah(0x02);

    let result = bios.int1a(
        &mut cpu,
        &mut memory,
        &mut cmos,
        &gp64_hw::timer::Pit::new(),
        None,
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    let hours = cpu.regs.ch();
    let mins = cpu.regs.cl();
    let secs = cpu.regs.dh();
    let dst = cpu.regs.dl();

    // Hours should be valid BCD 0x00-0x23
    assert!(hours <= 0x23, "Hours {} exceeds 0x23", hours);
    assert!(
        (hours & 0x0F) <= 9,
        "Hours low nibble invalid: {:02X}",
        hours
    );
    assert!(
        ((hours >> 4) & 0x0F) <= 2,
        "Hours high nibble invalid: {:02X}",
        hours
    );

    // Minutes should be valid BCD 0x00-0x59
    assert!(mins <= 0x59, "Minutes {} exceeds 0x59", mins);
    assert!(
        (mins & 0x0F) <= 9,
        "Minutes low nibble invalid: {:02X}",
        mins
    );
    assert!(
        ((mins >> 4) & 0x0F) <= 5,
        "Minutes high nibble invalid: {:02X}",
        mins
    );

    // Seconds should be valid BCD 0x00-0x59
    assert!(secs <= 0x59, "Seconds {} exceeds 0x59", secs);
    assert!(
        (secs & 0x0F) <= 9,
        "Seconds low nibble invalid: {:02X}",
        secs
    );
    assert!(
        ((secs >> 4) & 0x0F) <= 5,
        "Seconds high nibble invalid: {:02X}",
        secs
    );

    // DST flag should be 0 or 1
    assert!(dst <= 1, "DST flag {} should be 0 or 1", dst);
}

/// Give ES a real, non-zero cache the way a guest's `MOV ES, AX` does, so
/// the interpreter's `seg_base` treats the cache as authoritative rather
/// than falling back to `selector << 4`.
fn load_stale_es(cpu: &mut Cpu, memory: &Memory) {
    cpu.load_data_segment(memory, gp64_cpu::DataSegReg::ES, 0x1234);
    assert_eq!(
        cpu.regs.es_cache.base, 0x0001_2340,
        "premise: the guest's own ES load leaves a non-zero cache base"
    );
}

/// Every HLE far-pointer return must leave the ES cache agreeing with the
/// selector it published.
fn assert_es_cache_follows(cpu: &Cpu, expected_selector: u16, what: &str) {
    assert_eq!(cpu.regs.es, expected_selector, "{what}: ES selector");
    assert_eq!(
        cpu.regs.es_cache.base,
        u32::from(expected_selector) << 4,
        "{what}: the ES descriptor CACHE must follow the selector - the              interpreter resolves es-relative addresses from it, and a bare              `regs.es =` store leaves the guest's previous segment winning"
    );
}

/// Lay out an INT 15h AH=89h build table at `gdt`: entry 1 is the caller's
/// GDT pseudo-descriptor (the one AH=89h LGDTs, so `gdt_limit` is what the
/// selector loads are validated against), entry 2 its IDT, then the fixed
/// 0x18/0x20/0x28/0x30 set the service defines.
fn build_int15_89_table(memory: &mut Memory, gdt: usize, gdt_limit: u16) {
    for k in 0..(8 * 8) {
        memory.write_u8(gdt + k, 0x00);
    }
    let mut pseudo = |idx: usize, base: u32, limit: u16| {
        let o = gdt + idx * 8;
        memory.write_u8(o, (limit & 0xFF) as u8);
        memory.write_u8(o + 1, (limit >> 8) as u8);
        memory.write_u8(o + 2, (base & 0xFF) as u8);
        memory.write_u8(o + 3, ((base >> 8) & 0xFF) as u8);
        memory.write_u8(o + 4, ((base >> 16) & 0xFF) as u8);
        memory.write_u8(o + 5, 0x93);
    };
    pseudo(1, gdt as u32, gdt_limit);
    pseudo(2, 0x0003_0000, 0x07FF);
    let mut desc = |idx: usize, access: u8| {
        let o = gdt + idx * 8;
        memory.write_u8(o, 0xFF);
        memory.write_u8(o + 1, 0xFF);
        memory.write_u8(o + 5, access);
        memory.write_u8(o + 6, 0x0F | 0xC0);
    };
    desc(3, 0x92); // DS 0x18 - flat data
    desc(4, 0x92); // ES 0x20 - flat data
    desc(5, 0x92); // SS 0x28 - flat writable data
    desc(6, 0x9A); // CS 0x30 - flat code
}

#[test]
fn hle_far_pointer_returns_resync_the_real_mode_es_cache() {
    // HLE far-pointer returns must refresh the real-mode ES descriptor cache.
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // INT 15h AH=C0h - system configuration table in ES:BX.
    load_stale_es(&mut cpu, &memory);
    cpu.regs.set_ah(0xC0);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert_es_cache_follows(&cpu, 0xF000, "INT 15h AH=C0h");
    // The pointer now resolves where the guest would read it.
    let config = (cpu.regs.es_cache.base as usize) + usize::from(cpu.regs.bx());
    assert_eq!(
        memory.read_u8(config + 2),
        0xFC,
        "es:[bx+2] must reach the AT model byte in the ROM table"
    );

    // INT 10h AH=11h AL=30h - ROM font pointer in ES:BP.
    load_stale_es(&mut cpu, &memory);
    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x30);
    cpu.regs.set_bh(0x06); // 8x16 ROM font
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_es_cache_follows(&cpu, 0xF000, "INT 10h AH=11h AL=30h");

    // INT 10h AH=FAh - RIL version table in ES:BX.
    load_stale_es(&mut cpu, &memory);
    cpu.regs.set_ah(0xFA);
    cpu.regs.set_bx(0);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_es_cache_follows(&cpu, Bios::VIDEO_ROM_SEGMENT, "INT 10h AH=FAh");
}
