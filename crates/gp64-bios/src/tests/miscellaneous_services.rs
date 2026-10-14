use super::*;

/// Cover INT 15h E820 invalid continuation value path (lines 317-318, 328).
#[test]
fn test_int15_e820_bad_continuation_value() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0x20);
    cpu.regs.edx = 0x534D_4150; // SMAP
    cpu.regs.ebx = 99; // Invalid continuation (> entry_count)
    cpu.regs.ecx = 24;
    cpu.regs.es = 0x9000;
    cpu.regs.set_di(0x0000);

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x86);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// Cover INT 15h PS/2 mouse C205h invalid packet size (line 564 region).
#[test]
fn test_int15_ps2_mouse_init_interface_invalid() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x05);
    cpu.regs.set_bh(0); // Invalid packet size (must be 1-8)

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x02);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// Cover INT 15h PS/2 mouse C206h extended commands - subfunction 0x00/0x01/0x02 and invalid.
#[test]
fn test_int15_ps2_mouse_extended_commands() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // C206h BH=00h: Get device status
    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x06);
    cpu.regs.set_bh(0x00);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert_eq!(cpu.regs.ah(), 0x00);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // C206h BH=01h: Set scaling 1:1
    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x06);
    cpu.regs.set_bh(0x01);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert_eq!(cpu.regs.ah(), 0x00);

    // C206h BH=02h: Set scaling 2:1
    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x06);
    cpu.regs.set_bh(0x02);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert_eq!(cpu.regs.ah(), 0x00);

    // C206h BH=FFh: Invalid subfunction
    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x06);
    cpu.regs.set_bh(0xFF);
    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert_eq!(cpu.regs.ah(), 0x02);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// Cover APM version negotiation with major > 1 (line 713 edge case).
#[test]
fn test_int15_apm_version_major_gt_1() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // 530Eh: Request version with major=2, minor=5
    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x0E);
    cpu.regs.set_ch(0x02); // Major 2
    cpu.regs.set_cl(0x05); // Minor 5

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    // Capped to 1.2
    assert_eq!(cpu.regs.ah(), 1);
    assert_eq!(cpu.regs.al(), 2);
}

/// Cover INT 10h AH=11h AL=11h: Load 8x14 font with recalculate flag (cov variant).
#[test]
fn test_int10_load_8x14_font_al11_recalc() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x11); // AL=11h loads 8x14 and recalculates page length
    cpu.regs.set_bl(0x00);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert!(vga.custom_font_loaded);
}

/// Cover INT 10h AH=11h AL=14h: Load 8x16 font with recalculate flag (cov variant).
#[test]
fn test_int10_load_8x16_font_al14_recalc() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x14); // AL=14h loads 8x16 and recalculates page length
    cpu.regs.set_bl(0x00);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert!(vga.custom_font_loaded);
}

/// Cover INT 10h AH=0Fh: Get video mode (exercises mode trace at lines 43-46).
#[test]
fn test_int10_get_mode_trace() {
    use tracing_subscriber::layer::SubscriberExt;

    let subscriber = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::sink)
                .with_ansi(false),
        )
        .with(tracing_subscriber::filter::EnvFilter::new("gp64=debug"));

    let _guard = tracing::subscriber::set_default(subscriber);

    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // AH=0Fh: Get current video mode (exercises the mode trace path)
    cpu.regs.set_ah(0x0F);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

/// Cover INT 10h DAC set-block trace path (line 521-527).
#[test]
fn test_int10_dac_set_block_trace() {
    use tracing_subscriber::layer::SubscriberExt;

    let subscriber = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::sink)
                .with_ansi(false),
        )
        .with(tracing_subscriber::filter::EnvFilter::new("gp64=trace"));

    let _guard = tracing::subscriber::set_default(subscriber);

    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // Write palette data to buffer
    let buf_seg = 0x5000u16;
    let buf_off = 0x0000u16;
    let buf_addr = Memory::seg_offset_to_linear(buf_seg, buf_off);
    for i in 0..3 {
        memory.write_u8(buf_addr + i, (i * 21) as u8);
    }

    // AH=10h AL=12h: Set block of DAC registers
    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x12);
    cpu.regs.set_bx(0); // start register
    cpu.regs.set_cx(1); // count
    cpu.regs.es = buf_seg;
    cpu.regs.set_dx(buf_off);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

/// Cover INT 10h AH=11h AL=12h: Load 8x8 font with recalculate.
#[test]
fn test_int10_load_8x8_font_al12_recalc() {
    use tracing_subscriber::layer::SubscriberExt;

    let subscriber = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::sink)
                .with_ansi(false),
        )
        .with(tracing_subscriber::filter::EnvFilter::new("gp64=debug"));

    let _guard = tracing::subscriber::set_default(subscriber);

    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // AL=12h loads 8x8 and recalculates page
    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x12);
    cpu.regs.set_bl(0x00);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert!(vga.custom_font_loaded);
}

/// Cover INT 16h with kbd_trace_allow enabled (int16 lines 23-32, 53-57, 71, 105-109).
#[test]
fn test_int16_with_kbd_trace() {
    use tracing_subscriber::layer::SubscriberExt;

    let subscriber = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::sink)
                .with_ansi(false),
        )
        .with(tracing_subscriber::filter::EnvFilter::new(
            "gp64::kbd::int=debug,gp64::kbd::flow=info",
        ));

    let _guard = tracing::subscriber::set_default(subscriber);

    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    // Enqueue a key so reads succeed
    bda::kbd_enqueue(&mut memory, 0x1E, b'a');

    // Set up IRET stack frame
    cpu.regs.set_sp(0xFFF0);
    cpu.regs.ss = 0x0000;
    memory.write_u16(0xFFF0, 0x0000);
    memory.write_u16(0xFFF2, 0x0000);
    memory.write_u16(0xFFF4, cpu.regs.flags.bits());

    // AH=11h: Peek (covers trace at lines 105-109)
    cpu.regs.set_ah(0x11);
    bios.int16(&mut cpu, &mut memory, &mut keyboard).unwrap();

    // AH=10h: Read (covers trace at lines 53-57)
    cpu.regs.set_ah(0x10);
    bios.int16(&mut cpu, &mut memory, &mut keyboard).unwrap();
    assert_eq!(cpu.regs.ah(), 0x1E);
    assert_eq!(cpu.regs.al(), b'a');

    // AH=10h: Read with empty buffer (covers trace at line 71)
    // Set up fresh stack frame
    cpu.regs.set_sp(0xFFF0);
    memory.write_u16(0xFFF0, 0x0000);
    memory.write_u16(0xFFF2, 0x0000);
    memory.write_u16(0xFFF4, cpu.regs.flags.bits());
    cpu.regs.set_ah(0x10);
    bios.int16(&mut cpu, &mut memory, &mut keyboard).unwrap();
    // Should loop back (return IP decremented)
}

/// AH=01h reaching phase 2 with THRE already asserted: the write SUCCEEDS.
///
/// MDD-BUG-KILN-00278: this was named `test_int14_write_thre_timeout` and
/// asserted NOTHING — it called `int14` and returned, so it could only fail
/// on a panic. Worse, the name claimed the timeout path while the fixture
/// starts with THRE asserted, so phase 2 exits on its first poll and the
/// timeout is never exercised. Renamed to what it covers, and given the
/// result assertions it lacked. The phase-2 TIMEOUT path is still
/// uncovered and is not reachable from a unit fixture today — see the
/// 2026-08-05 entry in `bugs/MDD-BUG-KILN-00278.md` for why and what would
/// unlock it.
#[test]
fn int14_write_succeeds_when_thre_is_already_asserted() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    bios.init_ivt(&mut memory, false);
    let mut serial = gp64_hw::serial::SerialPorts::new();
    let mut tick_count = 0u32;
    let mut tick = |s: &mut gp64_hw::serial::SerialPorts, _: usize| {
        tick_count += 1;
        // Force DSR+CTS to be set (so modem_ready becomes true)
        // by writing to MSR - but we can't directly set MSR, so
        // the serial port in loopback mode would set it.
        // Instead, set MCR loopback bit which feeds back to MSR.
        s.write(0x3F8 + 4, 0x1B); // MCR: loopback + DTR + RTS + OUT2
    };

    // AH=01h: Write char - with loopback, DSR+CTS will be set
    cpu.regs.set_ah(0x01);
    cpu.regs.set_dx(0);
    cpu.regs.set_al(b'Z');

    bios.int14(&mut cpu, &mut memory, &mut serial, &mut tick)
        .unwrap();

    // The assertions this test never had. Bit 7 of AH is the BIOS timeout
    // flag; with THRE asserted the write completes, so it must be CLEAR.
    assert_eq!(
        cpu.regs.ah() & 0x80,
        0,
        "AH bit 7 (timeout) must be clear when THRE was ready: AH={:#04x}",
        cpu.regs.ah()
    );
    assert_ne!(
        cpu.regs.ah() & 0x20,
        0,
        "AH must carry the LSR it observed, including THRE: AH={:#04x}",
        cpu.regs.ah()
    );
    assert!(tick_count > 0, "the serial pump must have been ticked");
}

/// The identification block must not run into the Verite board record at
/// C000:0100 (nor, beyond it, the 4F0A PMI table at C000:0200).
#[test]
fn c0000_reserved_regions_do_not_overlap() {
    // Pairwise, over the WHOLE map. The predecessor of this test compared the
    // VBE block against two hand-picked neighbours and missed the Verite PCIR
    // sharing its exact offset (MDD-BUG-FLUX-00297) — a check that enumerates
    // cannot make that mistake.
    let map = Bios::C0000_RESERVED;
    for (i, &(name_a, start_a, len_a)) in map.iter().enumerate() {
        assert!(len_a > 0, "{name_a} has zero length");
        for &(name_b, start_b, len_b) in &map[i + 1..] {
            let overlap = start_a < start_b + len_b && start_b < start_a + len_a;
            assert!(
                !overlap,
                "C0000 layout: {name_a} [{start_a:#06X}..{:#06X}) overlaps \
                 {name_b} [{start_b:#06X}..{:#06X})",
                start_a + len_a,
                start_b + len_b
            );
        }
    }
}

/// Cover misc.rs serial read path (AH=02h).
#[test]
fn test_int14_read_timeout() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    bios.init_ivt(&mut memory, false);
    let mut serial = gp64_hw::serial::SerialPorts::new();
    let mut tick = |_: &mut gp64_hw::serial::SerialPorts, _: usize| {};

    // AH=02h: Read char - will timeout
    cpu.regs.set_ah(0x02);
    cpu.regs.set_dx(0);

    int14_to_completion(&mut bios, &mut cpu, &mut memory, &mut serial, &mut tick);

    // Should have timeout bit set
    assert_ne!(cpu.regs.ah() & 0x80, 0);
}

/// Cover INT 15h APM version negotiation with major=1, minor=1 (line 713: minor capped).
#[test]
fn test_int15_apm_version_exact_1_1() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x0E);
    cpu.regs.set_ch(0x01);
    cpu.regs.set_cl(0x01);

    bios.int15(&mut cpu, &mut memory, None).unwrap();
    assert_eq!(cpu.regs.ah(), 1);
    assert_eq!(cpu.regs.al(), 1);
}

/// EXP-N007b (unit E) regression: an INT 10h palette mutation mid-frame
/// must render the raster up to the event grid BEFORE the mutation
/// applies (the render-before-mutate contract shared with deferred port
/// writes). Fails without the catch-up gate at the top of `int10`.
#[test]
fn int10_palette_mutation_renders_catch_up_before_applying() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    set_text_mode_03(&mut bios, &mut cpu, &mut memory, &mut vga);

    // Deterministic synthetic frame: 201 lines, 1000 cycles, retrace
    // 100; beam mid-frame with lines 11..=88 not yet rendered.
    vga.total_scanlines_cached = 201;
    vga.visible_scanlines_cached = 200;
    vga.timing_cycles_per_frame = 1000;
    vga.timing_retrace_cycles = 100;
    vga.frame_start_cycle = 0;
    vga.last_rendered_line = 10;
    cpu.cycles = 500; // event-grid line 88

    // AH=10h AL=10h: set one DAC register — raster-mutating.
    cpu.regs.set_ah(0x10);
    cpu.regs.set_al(0x10);
    cpu.regs.set_bx(0x0001);
    cpu.regs.set_dh(63); // R
    cpu.regs.set_ch(0); // G
    cpu.regs.set_cl(0); // B
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    assert_eq!(
        vga.last_rendered_line, 88,
        "int10 must catch the raster up to the grid line before mutating"
    );

    // A non-raster service (AH=0Fh get mode) must NOT trigger catch-up.
    vga.last_rendered_line = 10;
    cpu.regs.set_ah(0x0F);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    assert_eq!(vga.last_rendered_line, 10);
}
