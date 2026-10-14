use super::*;

#[test]
fn test_int15_e820_wrong_signature() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0x20);
    cpu.regs.edx = 0x12345678; // Wrong signature (not SMAP)
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 20;

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x86);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_e820_small_buffer() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0x20);
    cpu.regs.edx = 0x534D_4150; // SMAP
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 4; // Too small (minimum is 20)

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x86);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_e8_unhandled_subfunction() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ah(0xE8);
    cpu.regs.set_al(0xFF); // Unknown sub-function

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x86);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_ps2_mouse_set_handler() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // C207h: Set device handler address
    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0x07);
    cpu.regs.es = 0x1234;
    cpu.regs.set_bx(0x5678);

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x00);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_ps2_mouse_unhandled() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // C2FFh: Unknown PS/2 mouse sub-function
    cpu.regs.set_ah(0xC2);
    cpu.regs.set_al(0xFF);

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x01);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_bf_refused_after_init_ivt() {
    // AH=BF is the extender kernel API, not a BIOS function; it must be
    // refused like any unknown function (MDD-BUG-KILN-00048).
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    bios.init_ivt(&mut memory, false);

    cpu.regs.set_ah(0xBF);
    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x86);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_apm_set_power_state() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // 5307h: Set power state
    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x07);
    cpu.regs.set_bx(0x0001); // System BIOS device
    cpu.regs.set_cx(0x0001); // Ready state

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_apm_driver_version_negotiation() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // 530Eh: Request APM version higher than supported
    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x0E);
    cpu.regs.set_ch(0x05); // Major version 5 (higher than supported 1)
    cpu.regs.set_cl(0x09); // Minor version 9

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // Should be capped at 1.2
    assert_eq!(cpu.regs.ah(), 1);
    assert_eq!(cpu.regs.al(), 2);
}

#[test]
fn test_int15_apm_driver_version_zero_major() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // 530Eh with major=0: special case
    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0x0E);
    cpu.regs.set_ch(0x00); // Major version 0
    cpu.regs.set_cl(0x09); // Minor version 9

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ah(), 0);
    assert_eq!(cpu.regs.al(), 9);
}

#[test]
fn test_int15_apm_unsupported() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // 53FFh: Unknown APM function
    cpu.regs.set_ah(0x53);
    cpu.regs.set_al(0xFF);

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x0C);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// MDD-BUG-CRUCIBLE-00600: AH=C9h must report the CONFIGURED identity.
///
/// The previous test built a default `Cpu::new()` (a 386) and asserted the
/// hardcoded CH=3, so it passed no matter what the handler did with any
/// other identity — it masked the defect rather than catching it. This
/// covers each family the handler can be asked about.
#[test]
fn int15_get_cpu_type_reports_the_configured_family() {
    for (cpu_type, expected_ch) in [
        (gp64_cpu::CpuType::I80386, 0x03u8),
        (gp64_cpu::CpuType::I80486, 0x04),
        (gp64_cpu::CpuType::Pentium, 0x05),
        (gp64_cpu::CpuType::PentiumMmx, 0x05),
        (gp64_cpu::CpuType::PentiumPro, 0x06),
        (gp64_cpu::CpuType::PentiumII, 0x06),
    ] {
        let (ah, ch, cl, carry) = run_int15_get_cpu_type(cpu_type);

        assert_eq!(ah, 0x00, "{cpu_type:?}: AH=C9h must succeed");
        assert_eq!(
            ch, expected_ch,
            "{cpu_type:?}: BIOS family code contradicts the configured identity"
        );
        assert_eq!(cl, 0x00, "{cpu_type:?}: stepping is deliberately 0");
        assert!(!carry, "{cpu_type:?}: success must clear carry");
    }
}

/// The declared pre-386 behaviour: AH=C9h is a 386+-era call, so an
/// identity with no family code in its vocabulary gets this handler's
/// standard "function not supported" answer rather than a fabricated one.
#[test]
fn int15_get_cpu_type_is_unsupported_on_a_pre_386_identity() {
    for cpu_type in [
        gp64_cpu::CpuType::I8086,
        gp64_cpu::CpuType::I8088,
        gp64_cpu::CpuType::I80186,
        gp64_cpu::CpuType::I80286,
    ] {
        let (ah, _ch, _cl, carry) = run_int15_get_cpu_type(cpu_type);

        assert_eq!(
            ah, 0x86,
            "{cpu_type:?}: a pre-386 identity must report function-not-supported"
        );
        assert!(
            carry,
            "{cpu_type:?}: an unsupported function must set carry"
        );
    }
}

#[test]
fn test_int15_topview_not_supported() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // 10h: TopView API
    cpu.regs.set_ah(0x10);

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x86);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int15_unhandled() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // FEh: Unknown function
    cpu.regs.set_ah(0xFE);

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x86);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int16_write_to_buffer_enhanced() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    // AH=13h: Enhanced write to keyboard buffer
    cpu.regs.set_ah(0x13);
    cpu.regs.set_ch(0x1C); // Enter scancode
    cpu.regs.set_cl(0x0D); // CR ASCII

    let disp = bios.int16(&mut cpu, &mut memory, &mut keyboard).unwrap();
    match disp {
        Int16Disp::Iret => {} // expected for AH=13h
        Int16Disp::Stub(_) => panic!("expected Iret for AH=13h, got Stub"),
    }

    assert_eq!(cpu.regs.al(), 0x00); // Success
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify key is in buffer by reading (AH=10h dequeues in HLE when key available)
    cpu.regs.set_ah(0x10);
    let disp = bios.int16(&mut cpu, &mut memory, &mut keyboard).unwrap();
    match disp {
        Int16Disp::Iret => {} // expected — key was available
        Int16Disp::Stub(_) => panic!("expected Iret for AH=10h with key, got Stub"),
    }

    assert_eq!(cpu.regs.ah(), 0x1C);
    assert_eq!(cpu.regs.al(), 0x0D);
}

#[test]
fn test_int16_enhanced_write_buffer_full() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    // Fill the buffer (16 slots in standard BDA buffer)
    for i in 0..16 {
        bda::kbd_enqueue(&mut memory, i as u8, i as u8);
    }

    // AH=13h: Should fail (buffer full)
    cpu.regs.set_ah(0x13);
    cpu.regs.set_ch(0x01);
    cpu.regs.set_cl(0x41);

    bios.int16(&mut cpu, &mut memory, &mut keyboard).unwrap();

    assert_eq!(cpu.regs.al(), 0x01); // Buffer full
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int16_unhandled() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut keyboard = Keyboard::new();

    cpu.regs.set_ah(0xFE); // Unknown function
    bios.int16(&mut cpu, &mut memory, &mut keyboard).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int14_extended_init_even_parity() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    bios.init_ivt(&mut memory, false);
    let mut serial = gp64_hw::serial::SerialPorts::new();
    let mut tick = |_: &mut gp64_hw::serial::SerialPorts, _: usize| {};

    // AH=04h: Extended initialize with even parity
    cpu.regs.set_ah(0x04);
    cpu.regs.set_dx(0); // COM1
    cpu.regs.set_cl(7); // Baud rate code 7 = 9600
    cpu.regs.set_ch(0x03); // 8-bit word length
    cpu.regs.set_bl(0x00); // 1 stop bit
    cpu.regs.set_bh(0x02); // Even parity
    cpu.regs.set_al(0x00); // Break on

    bios.int14(&mut cpu, &mut memory, &mut serial, &mut tick)
        .unwrap();

    // Should return status
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int14_extended_init_stick_odd_parity() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    bios.init_ivt(&mut memory, false);
    let mut serial = gp64_hw::serial::SerialPorts::new();
    let mut tick = |_: &mut gp64_hw::serial::SerialPorts, _: usize| {};

    // AH=04h: Extended initialize with stick odd parity
    cpu.regs.set_ah(0x04);
    cpu.regs.set_dx(0);
    cpu.regs.set_cl(8); // 19200 baud
    cpu.regs.set_ch(0x03);
    cpu.regs.set_bl(0x01); // 2 stop bits
    cpu.regs.set_bh(0x03); // Stick odd parity
    cpu.regs.set_al(0x01); // Break off

    bios.int14(&mut cpu, &mut memory, &mut serial, &mut tick)
        .unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int14_extended_init_stick_even() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    bios.init_ivt(&mut memory, false);
    let mut serial = gp64_hw::serial::SerialPorts::new();
    let mut tick = |_: &mut gp64_hw::serial::SerialPorts, _: usize| {};

    // AH=04h: Extended initialize with stick even parity
    cpu.regs.set_ah(0x04);
    cpu.regs.set_dx(0);
    cpu.regs.set_cl(0); // 110 baud
    cpu.regs.set_ch(0x00);
    cpu.regs.set_bl(0x00);
    cpu.regs.set_bh(0x04); // Stick even parity
    cpu.regs.set_al(0x00);

    bios.int14(&mut cpu, &mut memory, &mut serial, &mut tick)
        .unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int14_unhandled_ah_fe() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    bios.init_ivt(&mut memory, false);
    let mut serial = gp64_hw::serial::SerialPorts::new();
    let mut tick = |_: &mut gp64_hw::serial::SerialPorts, _: usize| {};

    cpu.regs.set_ah(0xFE);
    cpu.regs.set_dx(0); // COM1

    bios.int14(&mut cpu, &mut memory, &mut serial, &mut tick)
        .unwrap();

    assert_eq!(cpu.regs.ah(), 0x80);
}

#[test]
fn test_int14_no_port_at_index() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    // Don't call init_ivt - BDA COM ports will be 0 (no ports)
    let mut serial = gp64_hw::serial::SerialPorts::new();
    let mut tick = |_: &mut gp64_hw::serial::SerialPorts, _: usize| {};

    cpu.regs.set_ah(0x00);
    cpu.regs.set_dx(0); // COM1 but BDA says port_base=0

    bios.int14(&mut cpu, &mut memory, &mut serial, &mut tick)
        .unwrap();

    assert_eq!(cpu.regs.ah(), 0x80);
    assert_eq!(cpu.regs.al(), 0x00);
}

#[test]
fn test_int14_all_baud_rates_via_init() {
    // Exercise all baud rate codes (0-7) through AH=00h
    let mut bios = Bios::new();
    let mut memory = Memory::new();
    bios.init_ivt(&mut memory, false);
    let mut serial = gp64_hw::serial::SerialPorts::new();
    let mut tick = |_: &mut gp64_hw::serial::SerialPorts, _: usize| {};

    for code in 0u8..=7 {
        let mut cpu = Cpu::new();
        cpu.regs.set_ah(0x00);
        cpu.regs.set_dx(0); // COM1
                            // Baud code is in bits 7-5 of AL
        cpu.regs.set_al(code << 5 | 0x03); // code + 8N1

        bios.int14(&mut cpu, &mut memory, &mut serial, &mut tick)
            .unwrap();
    }
}

#[test]
fn test_int14_bda_timeout_zero_uses_default() {
    // When BDA timeout is 0, bda_timeout_iterations returns 0xFFFF
    // We verify this indirectly by setting timeout to 0 and calling write (AH=01h)
    // with no serial connection - it should timeout quickly with 0xFFFF iterations
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    bios.init_ivt(&mut memory, false);
    // Set COM1 timeout to 0
    memory.write_u8(0x47C, 0);

    let mut serial = gp64_hw::serial::SerialPorts::new();
    // Mark as external device so null-modem simulation doesn't auto-set CTS/DSR
    serial.ports[0].external_tx = true;
    let mut tick = |_: &mut gp64_hw::serial::SerialPorts, _: usize| {};

    // AH=01h: Write char - will timeout since no modem signals asserted
    cpu.regs.set_ah(0x01);
    cpu.regs.set_dx(0); // COM1
    cpu.regs.set_al(b'X');

    int14_to_completion(&mut bios, &mut cpu, &mut memory, &mut serial, &mut tick);

    // Should have timeout bit set (bit 7)
    assert_ne!(cpu.regs.ah() & 0x80, 0);
}

#[test]
fn test_int17_unhandled_ah05() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();

    // AH=05h: Unknown function
    cpu.regs.set_ah(0x05);
    cpu.regs.set_dx(0); // LPT1

    bios.int17(&mut cpu).unwrap();

    assert_eq!(cpu.regs.ah(), 0x01); // Timeout
}

#[test]
fn test_int17_print_char_ah00() {
    let bios = Bios::new();
    let mut cpu = Cpu::new();

    // AH=00h: Print character
    cpu.regs.set_ah(0x00);
    cpu.regs.set_al(0x41); // 'A'
    cpu.regs.set_dx(0); // LPT1

    bios.int17(&mut cpu).unwrap();

    assert_eq!(cpu.regs.ah(), 0x01); // Timeout - no printer
}

#[test]
fn test_int10_load_8x14_font_al01() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // AH=11h, AL=01h: Load 8x14 font
    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x01);
    cpu.regs.set_bl(0x00); // Block 0

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    assert!(vga.custom_font_loaded);
}

#[test]
fn test_int10_load_8x16_font_al04() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // AH=11h, AL=04h: Load 8x16 font
    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0x04);
    cpu.regs.set_bl(0x00);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    assert!(vga.custom_font_loaded);
}

#[test]
fn test_int10_char_gen_unhandled_subfunction() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // AH=11h, AL=FFh: Unhandled subfunction
    cpu.regs.set_ah(0x11);
    cpu.regs.set_al(0xFF);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
    // Should not panic
}

#[test]
fn test_int10_unhandled() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // AH=FFh: Unhandled
    cpu.regs.set_ah(0xFF);
    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

#[test]
fn test_int10_topview_shadow_buffer() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // AH=FEh: TopView shadow buffer - ES:DI unchanged
    let orig_es = 0xB800u16;
    let orig_di = 0x0000u16;
    cpu.regs.es = orig_es;
    cpu.regs.set_di(orig_di);
    cpu.regs.set_ah(0xFE);

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();

    assert_eq!(cpu.regs.es, orig_es);
    assert_eq!(cpu.regs.di(), orig_di);
}

#[test]
fn test_int15_move_memory_trace() {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up GDT with source and destination descriptors
    let gdt_segment = 0x1000u16;
    let gdt_offset = 0x0000u16;
    let gdt_addr = Memory::seg_offset_to_linear(gdt_segment, gdt_offset);

    // Source descriptor at offset +0x10: base = 0x200000
    memory.write_u16(gdt_addr + 0x10, 0xFFFF); // limit
    memory.write_u16(gdt_addr + 0x12, 0x0000); // base low
    memory.write_u8(gdt_addr + 0x14, 0x20); // base mid (0x20)
    memory.write_u8(gdt_addr + 0x17, 0x00); // base high

    // Destination descriptor at offset +0x18: base = 0x300000
    memory.write_u16(gdt_addr + 0x18, 0xFFFF); // limit
    memory.write_u16(gdt_addr + 0x1A, 0x0000); // base low
    memory.write_u8(gdt_addr + 0x1C, 0x30); // base mid (0x30)
    memory.write_u8(gdt_addr + 0x1F, 0x00); // base high

    // Write test data at source
    for i in 0..8 {
        memory.write_u8(0x200000 + i, (i + 1) as u8);
    }

    cpu.regs.set_ah(0x87);
    cpu.regs.set_cx(4); // 4 words = 8 bytes
    cpu.regs.es = gdt_segment;
    cpu.regs.set_si(gdt_offset);

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    assert_eq!(cpu.regs.ah(), 0x00);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify data was copied
    for i in 0..8 {
        assert_eq!(memory.read_u8(0x300000 + i), (i + 1) as u8);
    }
}

#[test]
fn test_int10_ega_ril_read_attribute_fallthrough() {
    // Cover the _ => 0 branch in ega_ril_read_register (group > 3, with regs > 0)
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = Vga::new();
    bios.init_ivt(&mut memory, false);

    // EGA RIL uses AH=12h BL=34h with DX for register groups
    // Read register: AH=12h BL=34h DX=port BH=register
    // This tests the read_register path
    // Use AH=12h BL=34h to trigger EGA RIL read_register
    // DX=0x3CE group 2 (Graphics), reg 0
    cpu.regs.set_ah(0x12);
    cpu.regs.set_bl(0x34);
    cpu.regs.set_dx(0x3CE); // Graphics Controller
    cpu.regs.set_bh(0x00); // Register 0

    bios.int10(&mut cpu, &mut memory, &mut vga).unwrap();
}

/// Issue INT 15h AH=C9h against a given configured CPU identity.
fn run_int15_get_cpu_type(cpu_type: gp64_cpu::CpuType) -> (u8, u8, u8, bool) {
    let mut bios = Bios::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.set_cpu_type(cpu_type);
    cpu.regs.set_ah(0xC9);

    bios.int15(&mut cpu, &mut memory, None).unwrap();

    (
        cpu.regs.ah(),
        cpu.regs.ch(),
        cpu.regs.cl(),
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
    )
}
