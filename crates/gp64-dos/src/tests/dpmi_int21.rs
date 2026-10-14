use super::*;

/// MDD-BUG-CRU-01413: a hosted 16-bit task issues INT 21h as a 16-bit DPMI
/// client, and there AH=2Ch must read the same BDA counter AH=2Dh set -
/// not a separate (host) clock, which is what hosted-session startup once selected.
/// The expected value is exactly what the counter written by AH=2Dh
/// encodes, so a read from any other clock fails.
#[test]
fn pm16_dpmi_client_reads_back_the_time_it_set() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    dos.dos_mem.init(&mut memory);
    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);

    cpu.regs.set_ch(3);
    cpu.regs.set_cl(17);
    cpu.regs.set_dh(42);
    cpu.regs.set_dl(0);
    pm16_int21(&mut dos, &mut cpu, &mut memory, 0x2D00);
    assert_eq!(cpu.regs.al(), 0, "AH=2Dh accepts the time");
    let ticks = memory.read_u32(BDA_TICK_COUNT_ADDR);
    assert_eq!(
        ticks,
        time_to_bda_ticks(3, 17, 42, 0),
        "AH=2Dh sets the BDA counter"
    );

    pm16_int21(&mut dos, &mut cpu, &mut memory, 0x2C00);
    assert_eq!(
        (cpu.regs.ch(), cpu.regs.cl(), cpu.regs.dh(), cpu.regs.dl()),
        bda_ticks_to_time(ticks),
        "AH=2Ch reads the clock AH=2Dh set"
    );
}

/// MDD-00017: a translating DPMI host returns PM pointers from INT 21h —
#[test]
fn int21_pm16_get_indos_address_returns_a_selector() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    dos.dos_mem.init(&mut memory);
    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);

    pm16_int21(&mut dos, &mut cpu, &mut memory, 0x3400);

    let es = cpu.regs.es;
    assert_eq!(
        selector_base(&cpu, &memory, es),
        u32::from(INDOS_SEGMENT) << 4
    );
    assert_eq!(cpu.regs.es_cache.base, u32::from(INDOS_SEGMENT) << 4);
    assert_eq!(cpu.regs.bx(), INDOS_OFFSET);
}

/// MDD-00017: under a translating host a PM caller names PSPs by
/// selector — 51h/62h return one and 50h takes one back.
#[test]
fn int21_pm16_psp_calls_trade_selectors() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    dos.dos_mem.init(&mut memory);
    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);
    dos.set_current_psp(0x0742);

    pm16_int21(&mut dos, &mut cpu, &mut memory, 0x5100);
    let psp_sel = cpu.regs.bx();
    assert_eq!(selector_base(&cpu, &memory, psp_sel), 0x7420);
    pm16_int21(&mut dos, &mut cpu, &mut memory, 0x6200);
    assert_eq!(cpu.regs.bx(), psp_sel, "51h and 62h agree");

    dos.set_current_psp(0x0802);
    cpu.regs.set_bx(psp_sel);
    pm16_int21(&mut dos, &mut cpu, &mut memory, 0x5000);
    assert_eq!(dos.current_psp(), 0x0742);
}

/// MDD-00017: the default DTA EXEC sets is a real-mode segment; a PM
/// caller's 2Fh gets a selector for it (TLINK reloads the returned ES).
#[test]
fn int21_pm16_get_default_dta_returns_a_selector() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    dos.dos_mem.init(&mut memory);
    dos.set_dta(0x0742, 0x0080);
    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);

    pm16_int21(&mut dos, &mut cpu, &mut memory, 0x2F00);

    let es = cpu.regs.es;
    assert_eq!(selector_base(&cpu, &memory, es), 0x7420);
    assert_eq!(cpu.regs.bx(), 0x0080);
}

#[test]
fn int21_pm16_large_dos_memory_tracks_selector_run_through_resize_and_free() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let mut pic = Pic::new();
    dos.dos_mem.init(&mut memory);
    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);

    cpu.regs.set_ah(0x48);
    cpu.regs.set_bx(0x1001); // One paragraph over 64 KiB needs two descriptors.
    dos.int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let selector = cpu.regs.ax();
    let second_selector = selector.wrapping_add(8);
    let segment = dos
        .dpmi
        .dos_selector_to_segment(selector)
        .expect("base selector should map to the DOS allocation");
    let base = (segment as u32) << 4;
    assert_eq!(dos.dpmi.get_selector_base(selector), Some(base));
    assert_eq!(
        dos.dpmi.get_selector_base(second_selector),
        Some(base + 0x10000)
    );
    assert_eq!(dos.dpmi.get_descriptor_limit(selector), Some(0xFFFF));
    assert_eq!(
        dos.dpmi.dos_selector_to_segment(second_selector),
        Some(segment)
    );
    assert_eq!(dos.dpmi.get_descriptor_limit(second_selector), Some(0x000F));
    assert_eq!(
        dos.dos_mem
            .block_info_for_segment(segment)
            .expect("DOS allocation should exist")
            .size_paras,
        0x1001
    );

    cpu.regs.set_ah(0x4A);
    cpu.regs.es = second_selector; // Any member identifies the whole run.
    cpu.regs.set_bx(0x0800); // Shrink to 32 KiB: one descriptor remains.
    dos.int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dos.dpmi.get_descriptor_limit(selector), Some(0x7FFF));
    assert_eq!(dos.dpmi.dos_selector_to_segment(second_selector), None);
    assert_eq!(dos.dpmi.get_descriptor_limit(second_selector), None);
    assert_eq!(
        dos.dos_mem
            .block_info_for_segment(segment)
            .expect("shrunk DOS allocation should exist")
            .size_paras,
        0x0800
    );

    cpu.regs.set_ah(0x4A);
    cpu.regs.es = selector;
    cpu.regs.set_bx(0x1001); // Grow over 64 KiB: restore the run.
    dos.int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(
        dos.dpmi.dos_selector_to_segment(second_selector),
        Some(segment)
    );
    assert_eq!(dos.dpmi.get_selector_base(selector), Some(base));
    assert_eq!(
        dos.dpmi.get_selector_base(second_selector),
        Some(base + 0x10000)
    );
    assert_eq!(dos.dpmi.get_descriptor_limit(selector), Some(0xFFFF));
    assert_eq!(dos.dpmi.get_descriptor_limit(second_selector), Some(0x000F));
    assert_eq!(
        dos.dos_mem
            .block_info_for_segment(segment)
            .expect("grown DOS allocation should exist")
            .size_paras,
        0x1001
    );

    cpu.regs.set_ah(0x49);
    cpu.regs.es = second_selector; // Freeing an interior selector frees the run.
    dos.int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dos.dpmi.dos_selector_to_segment(selector), None);
    assert_eq!(dos.dpmi.dos_selector_to_segment(second_selector), None);
    assert_eq!(dos.dpmi.get_descriptor_limit(selector), None);
    assert_eq!(dos.dpmi.get_descriptor_limit(second_selector), None);
    assert!(
        dos.dos_mem
            .block_info_for_segment(segment)
            .is_none_or(|block| block.is_free),
        "freed DOS allocation must not remain owned"
    );
}

#[test]
fn int21_pm32_large_dos_memory_uses_one_full_length_selector() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let mut pic = Pic::new();
    dos.dos_mem.init(&mut memory);
    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);

    cpu.regs.set_ah(0x48);
    cpu.regs.set_bx(0x1800);
    dos.int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let selector = cpu.regs.ax();
    let segment = dos.dpmi.dos_selector_to_segment(selector).unwrap();
    assert_eq!(
        dos.dpmi.get_selector_base(selector),
        Some((segment as u32) << 4)
    );
    assert_eq!(dos.dpmi.get_descriptor_limit(selector), Some(0x17FFF));
    assert_eq!(
        dos.dpmi.dos_selector_to_segment(selector.wrapping_add(8)),
        None
    );

    cpu.regs.set_ah(0x4A);
    cpu.regs.es = selector;
    cpu.regs.set_bx(0x1A00);
    dos.int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dos.dpmi.get_descriptor_limit(selector), Some(0x19FFF));
    assert_eq!(
        dos.dpmi.dos_selector_to_segment(selector.wrapping_add(8)),
        None
    );

    cpu.regs.set_ah(0x49);
    cpu.regs.es = selector;
    dos.int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dos.dpmi.get_descriptor_limit(selector), None);
}

#[test]
fn int21_pm16_large_resize_collision_is_atomic() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let mut pic = Pic::new();
    dos.dos_mem.init(&mut memory);
    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);

    cpu.regs.set_ah(0x48);
    cpu.regs.set_bx(0x0800);
    dos.int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .unwrap();
    let selector = cpu.regs.ax();
    let segment = dos.dpmi.dos_selector_to_segment(selector).unwrap();
    let neighbor = dos.dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
    assert_eq!(neighbor, selector.wrapping_add(8));

    cpu.regs.set_ah(0x4A);
    cpu.regs.es = selector;
    cpu.regs.set_bx(0x1800);
    dos.int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 8);
    assert_eq!(cpu.regs.bx(), 0x1000);
    assert_eq!(dos.dpmi.get_descriptor_limit(selector), Some(0x7FFF));
    assert_eq!(dos.dpmi.get_descriptor_limit(neighbor), Some(0xFFFF));
    assert_eq!(
        dos.dos_mem
            .block_info_for_segment(segment)
            .expect("failed growth must preserve the DOS allocation")
            .size_paras,
        0x0800
    );
}

#[test]
fn int21_pm16_dos_memory_freed_through_int31_retires_run() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let mut pic = Pic::new();
    dos.dos_mem.init(&mut memory);
    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);

    cpu.regs.set_ah(0x48);
    cpu.regs.set_bx(0x1001);
    dos.int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .unwrap();
    let selector = cpu.regs.ax();
    let second_selector = selector.wrapping_add(8);

    cpu.regs.set_ax(0x0101);
    cpu.regs.set_dx(selector);
    dos.dpmi
        .int31_with_dosmem(&mut cpu, &mut memory, Some(&mut dos.dos_mem), 0x100)
        .unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dos.dpmi.dos_selector_to_segment(selector), None);
    assert_eq!(dos.dpmi.dos_selector_to_segment(second_selector), None);
    assert_eq!(dos.dpmi.get_descriptor_limit(selector), None);
    assert_eq!(dos.dpmi.get_descriptor_limit(second_selector), None);
}

#[test]
fn int21_pm16_umb_memory_resizes_through_int31() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let mut pic = Pic::new();
    dos.dos_mem.init(&mut memory);
    dos.dos_mem.init_umb(&mut memory);
    dos.umb_linked = true;
    dos.memory_strategy = 0x80;
    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);

    cpu.regs.set_ah(0x48);
    cpu.regs.set_bx(0x0800);
    dos.int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let selector = cpu.regs.ax();
    let segment = dos.dpmi.dos_selector_to_segment(selector).unwrap();
    assert!(dos.dos_mem.block_info_for_segment(segment).unwrap().is_umb);

    cpu.regs.set_ax(0x0102);
    cpu.regs.set_bx(0x0400);
    cpu.regs.set_dx(selector);
    dos.dpmi
        .int31_with_dosmem(&mut cpu, &mut memory, Some(&mut dos.dos_mem), 0x100)
        .unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dos.dpmi.get_descriptor_limit(selector), Some(0x3FFF));
    assert_eq!(
        dos.dos_mem
            .block_info_for_segment(segment)
            .expect("resized UMB allocation should remain tracked")
            .size_paras,
        0x0400
    );
}

/// Test dos_offset_dx returns EDX in 32-bit DPMI mode.
#[test]
fn test_cov_dos_offset_dx_32bit_dpmi() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);
    assert!(
        dos.dpmi.in_protected_mode(),
        "Should be in PM after enter_protected_mode"
    );
    assert!(dos.dpmi.is_32bit_mode(), "Should be in 32-bit mode");
    cpu.regs.edx = 0x12345678;
    let offset = dos.dos_offset_dx(&cpu);
    assert_eq!(offset, 0x12345678);
}

/// Test dos_offset_di returns EDI in 32-bit DPMI mode.
#[test]
fn test_cov_dos_offset_di_32bit_dpmi() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);
    cpu.regs.edi = 0xAABBCCDD;
    let offset = dos.dos_offset_di(&cpu);
    assert_eq!(offset, 0xAABBCCDD);
}

/// Test dos_offset_si returns ESI in 32-bit DPMI mode.
#[test]
fn test_cov_dos_offset_si_32bit_dpmi() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);
    cpu.regs.esi = 0x11223344;
    let offset = dos.dos_offset_si(&cpu);
    assert_eq!(offset, 0x11223344);
}

/// Test dos_count_cx returns ECX in 32-bit DPMI mode.
#[test]
fn test_cov_dos_count_cx_32bit_dpmi() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);
    cpu.regs.ecx = 0xDEADBEEF;
    let count = dos.dos_count_cx(&cpu);
    assert_eq!(count, 0xDEADBEEF);
}

/// Test dos_set_result_count sets EAX in 32-bit DPMI mode.
#[test]
fn test_cov_dos_set_result_count_32bit_dpmi() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);
    dos.dos_set_result_count(&mut cpu, 0x12345678);
    assert_eq!(cpu.regs.eax, 0x12345678);
}

/// Test DOS INT 21h helper width follows current CS size, not global DPMI mode.
#[test]
fn test_cov_dos_api_width_follows_current_cs_in_32bit_dpmi() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);
    force_current_cs_16bit(&mut cpu);

    cpu.regs.edx = 0x1234_5678;
    cpu.regs.edi = 0xAABB_CCDD;
    cpu.regs.esi = 0x1122_3344;
    cpu.regs.ecx = 0xDEAD_BEEF;
    cpu.regs.eax = 0xCAFE_0000;

    assert_eq!(dos.dos_offset_dx(&cpu), 0x5678);
    assert_eq!(dos.dos_offset_di(&cpu), 0xCCDD);
    assert_eq!(dos.dos_offset_si(&cpu), 0x3344);
    assert_eq!(dos.dos_count_cx(&cpu), 0xBEEF);

    dos.dos_set_result_count(&mut cpu, 0x1234_5678);
    assert_eq!(cpu.regs.eax, 0xCAFE_5678);
}

/// Test INT 21h/35h get vector in protected mode with client handler set.
#[test]
fn test_cov_ah35_get_vector_pm_client_handler() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);
    let selector = dos.dpmi.allocate_descriptors(&mut cpu, &mut memory, 1);
    assert_ne!(selector, 0);
    dos.dpmi
        .set_descriptor_base(&mut cpu, &mut memory, selector, 0x0008_8000);
    dos.dpmi
        .set_descriptor_limit(&mut cpu, &mut memory, selector, 0x12345);
    let expected_cache = cpu
        .read_descriptor(&memory, selector)
        .expect("allocated client-handler selector must be readable");
    cpu.regs.es = 0x2222;
    cpu.regs.es_cache = gp64_cpu::SegmentCache::real_mode(0x2222);
    dos.dpmi.set_client_pm_int_handler(0x21, selector, 0x1234);

    cpu.regs.set_ah(0x35);
    cpu.regs.set_al(0x21);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.es, selector);
    assert_eq!(cpu.regs.bx(), 0x1234);
    assert_eq!(
        cpu.regs.es_cache, expected_cache,
        "AH=35h client-handler arm must load the returned PM descriptor"
    );
}

/// Test INT 21h/35h get vector in 32-bit PM with client handler.
#[test]
fn test_cov_ah35_get_vector_pm_32bit_client_handler() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);
    dos.dpmi.set_client_pm_int_handler(0x10, 0x00CF, 0xAABB0000);

    cpu.regs.set_ah(0x35);
    cpu.regs.set_al(0x10);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.es, 0x00CF);
    assert_eq!(cpu.regs.ebx, 0xAABB0000);
}

/// Test INT 21h/35h get vector in 32-bit PM without client handler (fallback).
#[test]
fn test_cov_ah35_get_vector_pm_32bit_no_client_handler() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);
    let expected_cache = cpu
        .read_descriptor(&memory, PM_BIOS_CODE_SEL)
        .expect("PM BIOS code selector must exist after DPMI entry");
    cpu.regs.es = 0x2222;
    cpu.regs.es_cache = gp64_cpu::SegmentCache::real_mode(0x2222);

    cpu.regs.set_ah(0x35);
    cpu.regs.set_al(0x33); // Vector without client handler
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.es, PM_BIOS_CODE_SEL);
    let expected_offset = 0xFE00u32 + (0x33u32) * 2;
    assert_eq!(cpu.regs.ebx, expected_offset);
    assert_eq!(
        cpu.regs.es_cache, expected_cache,
        "AH=35h fallback arm must load the PM BIOS code descriptor"
    );
}

/// Saving INT 1/3 before installing a handler must preserve a target for the
/// host's default action. Chaining to that saved target must not redispatch
/// through the replacement handler and recurse.
#[test]
fn int21_pm16_ah35_debug_vectors_return_nonrecursive_chain_stubs() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    dos.dos_mem.init(&mut memory);
    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);

    for vector in [1u8, 3] {
        pm16_int21(&mut dos, &mut cpu, &mut memory, 0x3500 | u16::from(vector));
        assert_eq!(cpu.regs.es, PM_BIOS_CODE_SEL);
        assert_eq!(
            cpu.regs.bx(),
            0xFD20 + u16::from(vector) * 2,
            "vector {vector} prior target must stay on the host default-action path"
        );
    }
}

/// Test INT 21h/25h set vector in 32-bit PM mode.
#[test]
fn test_cov_ah25_set_vector_pm_32bit() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);

    cpu.regs.set_ah(0x25);
    cpu.regs.set_al(0x21);
    cpu.regs.ds = 0x0047;
    cpu.regs.edx = 0xDEAD0000;
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    let handler = dos.dpmi.client_pm_int_handler(0x21);
    assert_eq!(handler, Some((0x0047, 0xDEAD0000)));
}

/// Test INT 21h/25h uses DX, not EDX, from a 16-bit PM helper in 32-bit DPMI.
#[test]
fn test_cov_ah25_set_vector_pm_16bit_cs_inside_32bit_dpmi() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);
    force_current_cs_16bit(&mut cpu);

    cpu.regs.set_ah(0x25);
    cpu.regs.set_al(0x21);
    cpu.regs.ds = 0x0047;
    cpu.regs.edx = 0xABCD_0056;
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    let handler = dos.dpmi.client_pm_int_handler(0x21);
    assert_eq!(handler, Some((0x0047, 0x0000_0056)));
}

/// Test INT 21h/35h returns BX, not EBX, to a 16-bit PM helper in 32-bit DPMI.
#[test]
fn test_cov_ah35_get_vector_pm_16bit_cs_inside_32bit_dpmi() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);
    force_current_cs_16bit(&mut cpu);
    dos.dpmi
        .set_client_pm_int_handler(0x10, 0x00CF, 0x1234_5678);

    cpu.regs.ebx = 0xCAFE_0000;
    cpu.regs.set_ah(0x35);
    cpu.regs.set_al(0x10);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert_eq!(cpu.regs.es, 0x00CF);
    assert_eq!(cpu.regs.ebx, 0xCAFE_5678);
}

/// Test INT 21h/58h subfunction 3 - set UMB link with first-time init.
#[test]
fn test_cov_ah58_set_umb_link_first_init() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    assert!(!dos.dos_mem.is_umb_initialized());

    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x03);
    cpu.regs.set_bx(1);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(dos.umb_linked);
    // UMBs are NOT auto-created by linking — they must be
    // initialized at boot time (like real EMM386).
    assert!(!dos.dos_mem.is_umb_initialized());
}

/// Test INT 21h/58h subfunction 3 - invalid link value (BX=2).
#[test]
fn test_cov_ah58_set_umb_link_bx2_invalid() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x03);
    cpu.regs.set_bx(2);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1);
}

/// Test exec_read_param_block_cmdtail with es=0, bx=0 (null param block).
#[test]
fn test_cov_exec_read_param_block_null() {
    let dos = Dos::new();
    let memory = Memory::new();
    let (env_seg, cmdline_ptr) = dos.exec_read_param_block_cmdtail(&memory, 0, 0);
    assert_eq!(env_seg, 0);
    assert!(cmdline_ptr.is_none());
}

/// Test dos_datetime_to_systemtime with date=0 time=0.
#[test]
fn test_cov_dos_datetime_zero() {
    use std::time::UNIX_EPOCH;
    let result = Dos::dos_datetime_to_systemtime(0, 0);
    assert!(result >= UNIX_EPOCH);
}

/// Test INT 21h/3Fh (read file) in 32-bit DPMI returns count in EAX.
#[test]
fn test_cov_ah3f_read_32bit_dpmi() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let data = b"Hello World from DPMI";
    let file = Box::new(CursorFile::new(data.to_vec()));
    let handle = dos
        .file_table
        .allocate(file, "C:\\TEST.TXT".to_string(), dos.current_psp)
        .unwrap();

    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(handle);
    cpu.regs.ecx = data.len() as u32;
    cpu.regs.ds = 0x0500;
    cpu.regs.edx = 0;
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.eax, data.len() as u32);
}

/// MDD-BUG-FLUXHOMEARPA-00007: valid 32-bit DPMI reads must not allocate a
/// single ECX-sized host buffer.
#[test]
fn test_cov_ah3f_read_32bit_dpmi_large_ecx_is_chunked() {
    const MAX_CHUNK: usize = 64 * 1024;
    let count = MAX_CHUNK + 3;
    let data: Vec<u8> = (0..count).map(|i| (i & 0xFF) as u8).collect();

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(ChunkGuardReadFile::new(data.clone(), MAX_CHUNK)),
            "C:\\BIGREAD.BIN".to_string(),
            dos.current_psp,
        )
        .unwrap();

    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);
    const FLAT_DATA_SELECTOR: u16 = 0x13;
    const BUFFER_OFFSET: u32 = 0x0002_0000;
    cpu.load_data_segment(&memory, gp64_cpu::DataSegReg::DS, FLAT_DATA_SELECTOR);

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(handle);
    cpu.regs.ecx = count as u32;
    cpu.regs.edx = BUFFER_OFFSET;

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.eax, count as u32);

    let addr = cpu.translate_address(cpu.regs.ds, BUFFER_OFFSET);
    assert_eq!(memory.read_u8(addr), data[0]);
    assert_eq!(memory.read_u8(addr + MAX_CHUNK - 1), data[MAX_CHUNK - 1]);
    assert_eq!(memory.read_u8(addr + MAX_CHUNK), data[MAX_CHUNK]);
    assert_eq!(memory.read_u8(addr + count - 1), data[count - 1]);
}

#[test]
fn test_cov_ah3f_read_32bit_dpmi_short_read_returns_short_eax() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let data = b"EOF";
    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(data.to_vec())),
            "C:\\SHORTREAD.BIN".to_string(),
            dos.current_psp,
        )
        .unwrap();

    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);
    const FLAT_DATA_SELECTOR: u16 = 0x13;
    const BUFFER_OFFSET: u32 = 0x0002_1000;
    cpu.load_data_segment(&memory, gp64_cpu::DataSegReg::DS, FLAT_DATA_SELECTOR);

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(handle);
    cpu.regs.ecx = 16;
    cpu.regs.edx = BUFFER_OFFSET;

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.eax, data.len() as u32);
    let addr = cpu.translate_address(cpu.regs.ds, BUFFER_OFFSET);
    for (i, &byte) in data.iter().enumerate() {
        assert_eq!(memory.read_u8(addr + i), byte);
    }
}

/// Test INT 21h/3Fh uses DX/CX and returns AX from a 16-bit PM helper
/// inside a 32-bit DPMI client.
#[test]
fn test_cov_ah3f_read_16bit_cs_inside_32bit_dpmi() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let data = b"OVL!";
    let file = Box::new(CursorFile::new(data.to_vec()));
    let handle = dos
        .file_table
        .allocate(file, "C:\\GLIDE2X.OVL".to_string(), dos.current_psp)
        .unwrap();

    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);
    force_current_cs_16bit(&mut cpu);

    let ds = cpu.regs.ds;
    let low_addr = cpu.translate_address(ds, 0x0020);
    let high_addr = cpu.translate_address(ds, 0x1_0020);
    for i in 0..data.len() {
        memory.write_u8(low_addr + i, 0xCC);
        memory.write_u8(high_addr + i, 0xDD);
    }

    cpu.regs.eax = 0xBEEF_0000;
    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(handle);
    cpu.regs.ecx = 0x0001_0004;
    cpu.regs.edx = 0x0001_0020;

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.eax, 0xBEEF_0004);
    for (i, &b) in data.iter().enumerate() {
        assert_eq!(memory.read_u8(low_addr + i), b);
        assert_eq!(memory.read_u8(high_addr + i), 0xDD);
    }
}

/// Test INT 21h/40h (write file) in 32-bit DPMI returns count in EAX.
#[test]
fn test_cov_ah40_write_32bit_dpmi() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let file = Box::new(CursorFile::new(vec![]));
    // A real AH=3Ch/3Dh-for-write open records read-write access in the
    // SFT; AH=40h enforcement denies writes on the read-only default that
    // bare allocate() records.
    let handle = dos
        .file_table
        .allocate_with_sharing(
            file,
            "C:\\OUT.TXT".to_string(),
            dos.current_psp,
            crate::files::SHARE_COMPAT,
            crate::files::ACCESS_READWRITE,
        )
        .unwrap();

    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);

    const FLAT_DATA_SELECTOR: u16 = 0x13;
    const BUFFER_OFFSET: u32 = 0x0001_0020;

    cpu.load_data_segment(&memory, gp64_cpu::DataSegReg::DS, FLAT_DATA_SELECTOR);

    let data = b"DPMI write test";
    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(handle);
    cpu.regs.ecx = data.len() as u32;
    cpu.regs.edx = BUFFER_OFFSET;
    let buf_addr = cpu.translate_address(cpu.regs.ds, cpu.regs.edx);
    let dx_addr = cpu.translate_address(cpu.regs.ds, cpu.regs.dx() as u32);
    assert_ne!(buf_addr, dx_addr);
    for i in 0..data.len() {
        memory.write_u8(dx_addr + i, b'?');
    }
    for (i, &b) in data.iter().enumerate() {
        memory.write_u8(buf_addr + i, b);
    }

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.eax, data.len() as u32);

    let sft = dos.file_table.get_sft_mut(handle).unwrap();
    sft.file.seek(std::io::SeekFrom::Start(0)).unwrap();
    let mut out = Vec::new();
    sft.file.read_to_end(&mut out).unwrap();
    assert_eq!(out.as_slice(), data);
}

/// MDD-BUG-FLUXHOMEARPA-00007: invalid AH=40 handles must be rejected
/// before reading the guest buffer.
#[test]
fn test_cov_ah40_write_32bit_dpmi_invalid_handle_large_ecx_returns_without_copy() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);
    const FLAT_DATA_SELECTOR: u16 = 0x13;
    cpu.load_data_segment(&memory, gp64_cpu::DataSegReg::DS, FLAT_DATA_SELECTOR);

    memory.set_video_mode(0x12);
    memory.set_vga_memory_map_select(1);
    memory.plane_mut(0)[0] = 0x11;
    memory.plane_mut(1)[0] = 0x22;
    memory.plane_mut(2)[0] = 0x33;
    memory.plane_mut(3)[0] = 0x44;
    let original_latches = [0xAA, 0xBB, 0xCC, 0xDD];
    memory.set_latches(original_latches);

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(0x1234);
    cpu.regs.ecx = 0x0001_0001;
    cpu.regs.edx = 0x000A_0000;

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 6);
    assert_eq!(
        memory.get_latches(),
        original_latches,
        "invalid AH=40 handle must not read from the guest buffer"
    );
}

/// MDD-BUG-FLUXHOMEARPA-00007: valid 32-bit DPMI writes must not allocate
/// and copy a single ECX-sized guest buffer.
#[test]
fn test_cov_ah40_write_32bit_dpmi_large_ecx_is_chunked() {
    const MAX_CHUNK: usize = 64 * 1024;
    let count = MAX_CHUNK + 5;
    let data: Vec<u8> = (0..count)
        .map(|i| (i.wrapping_mul(3) & 0xFF) as u8)
        .collect();

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate_with_sharing(
            Box::new(ChunkGuardWriteFile::new(MAX_CHUNK)),
            "C:\\BIGWRITE.BIN".to_string(),
            dos.current_psp,
            crate::files::SHARE_COMPAT,
            crate::files::ACCESS_READWRITE,
        )
        .unwrap();

    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);
    const FLAT_DATA_SELECTOR: u16 = 0x13;
    const BUFFER_OFFSET: u32 = 0x0002_0000;
    cpu.load_data_segment(&memory, gp64_cpu::DataSegReg::DS, FLAT_DATA_SELECTOR);
    let addr = cpu.translate_address(cpu.regs.ds, BUFFER_OFFSET);
    for (i, &byte) in data.iter().enumerate() {
        memory.write_u8(addr + i, byte);
    }

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(handle);
    cpu.regs.ecx = count as u32;
    cpu.regs.edx = BUFFER_OFFSET;

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.eax, count as u32);

    let sft = dos.file_table.get_sft_mut(handle).unwrap();
    sft.file.seek(std::io::SeekFrom::Start(0)).unwrap();
    let mut out = Vec::new();
    sft.file.read_to_end(&mut out).unwrap();
    assert_eq!(out.len(), count);
    assert_eq!(out[0], data[0]);
    assert_eq!(out[MAX_CHUNK - 1], data[MAX_CHUNK - 1]);
    assert_eq!(out[MAX_CHUNK], data[MAX_CHUNK]);
    assert_eq!(out[count - 1], data[count - 1]);
}

#[test]
fn test_cov_ah40_write_32bit_dpmi_partial_write_returns_short_eax() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate_with_sharing(
            Box::new(PartialWriteFile::new(3)),
            "C:\\PARTWRITE.BIN".to_string(),
            dos.current_psp,
            crate::files::SHARE_COMPAT,
            crate::files::ACCESS_READWRITE,
        )
        .unwrap();

    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);
    const FLAT_DATA_SELECTOR: u16 = 0x13;
    const BUFFER_OFFSET: u32 = 0x0002_2000;
    cpu.load_data_segment(&memory, gp64_cpu::DataSegReg::DS, FLAT_DATA_SELECTOR);
    let data = b"ABCDEFGH";
    let addr = cpu.translate_address(cpu.regs.ds, BUFFER_OFFSET);
    for (i, &byte) in data.iter().enumerate() {
        memory.write_u8(addr + i, byte);
    }

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(handle);
    cpu.regs.ecx = data.len() as u32;
    cpu.regs.edx = BUFFER_OFFSET;

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.eax, 3);
    let sft = dos.file_table.get_sft_mut(handle).unwrap();
    sft.file.seek(std::io::SeekFrom::Start(0)).unwrap();
    let mut out = Vec::new();
    sft.file.read_to_end(&mut out).unwrap();
    assert_eq!(out.as_slice(), b"ABC");
}

#[test]
fn test_cov_ah40_write_32bit_dpmi_zero_count_returns_eax_zero() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate_with_sharing(
            Box::new(CursorFile::new(b"DATA".to_vec())),
            "C:\\TRUNC32.BIN".to_string(),
            dos.current_psp,
            crate::files::SHARE_COMPAT,
            crate::files::ACCESS_READWRITE,
        )
        .unwrap();

    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);
    let sft = dos.file_table.get_sft_mut(handle).unwrap();
    sft.file.seek(std::io::SeekFrom::Start(2)).unwrap();

    cpu.regs.eax = 0xCAFE_0000;
    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(handle);
    cpu.regs.ecx = 0;
    cpu.regs.ds = 0x2000;
    cpu.regs.edx = 0x0100;

    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.eax, 0);
    let sft = dos.file_table.get_sft_mut(handle).unwrap();
    sft.file.seek(std::io::SeekFrom::Start(0)).unwrap();
    let mut out = Vec::new();
    sft.file.read_to_end(&mut out).unwrap();
    assert_eq!(out.as_slice(), b"DA");
}

/// Test INT 21h/43h get attributes - archive bit.
#[test]
fn test_cov_ah43_get_attrs_with_archive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let tmp = tempfile::tempdir().unwrap();
    let test_path = tmp.path().join("archive_test.txt");
    std::fs::write(&test_path, b"test").unwrap();

    let drive_letter = 2; // C:
    let drive = crate::filesystem::HostDrive::new(tmp.path().to_path_buf());
    dos.drives.insert(drive_letter, Box::new(drive));

    let filename = "C:\\ARCHIVE_TEST.TXT\0";
    let buf_addr = 0x5000usize;
    for (i, &b) in filename.as_bytes().iter().enumerate() {
        memory.write_u8(buf_addr + i, b);
    }

    cpu.regs.set_ah(0x43);
    cpu.regs.set_al(0x00);
    cpu.regs.ds = (buf_addr >> 4) as u16;
    cpu.regs.set_dx((buf_addr & 0xF) as u16);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let attrs = cpu.regs.cx();
    assert!(
        attrs & 0x20 != 0,
        "Archive bit should be set, got {:#06X}",
        attrs
    );
}
fn selector_base(cpu: &gp64_cpu::Cpu, memory: &Memory, selector: u16) -> u32 {
    cpu.read_descriptor(memory, selector)
        .expect("a PM INT 21h pointer must name a valid selector")
        .base
}

/// Helper: emulate a 16-bit protected-mode helper stub running inside an
fn force_current_cs_16bit(cpu: &mut gp64_cpu::Cpu) {
    cpu.regs.cs_cache.big = false;
}
