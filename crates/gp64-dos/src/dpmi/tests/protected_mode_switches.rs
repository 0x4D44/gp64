use super::*;

#[test]
fn test_int31_state_save_restore_addresses() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.protected_mode = true;
    dpmi.cpu_mode = 0; // 16-bit mode

    cpu.regs.set_ax(0x0305);
    dpmi.translation_services(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 54); // Buffer size (DPMI 0.9: 54 bytes)
    assert_eq!(cpu.regs.bx(), 0xF000); // RM segment
    assert_eq!(cpu.regs.cx(), 0xFD40); // RM offset
    assert_eq!(cpu.regs.si(), PM_BIOS_CODE_SEL); // PM code selector
    assert_eq!(cpu.regs.di(), 0xFD40); // PM offset (16-bit mode)
}

#[test]
fn test_int31_raw_mode_switch_addresses() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.protected_mode = true;
    dpmi.cpu_mode = 0; // 16-bit mode

    cpu.regs.set_ax(0x0306);
    dpmi.translation_services(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.bx(), 0xF000); // RM-to-PM segment
    assert_eq!(cpu.regs.cx(), 0xFD50); // RM-to-PM offset
    assert_eq!(cpu.regs.si(), PM_BIOS_CODE_SEL); // PM-to-RM selector (BIOS code segment)
    assert_eq!(cpu.regs.di(), 0xFD58); // PM-to-RM offset (16-bit mode)
}

#[test]
fn test_int31_state_save_restore_addresses_32bit() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.protected_mode = true;
    dpmi.cpu_mode = 1; // 32-bit mode

    cpu.regs.set_ax(0x0305);
    dpmi.translation_services(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 54); // Buffer size (DPMI 0.9: 54 bytes)
    assert_eq!(cpu.regs.bx(), 0xF000); // RM segment
    assert_eq!(cpu.regs.cx(), 0xFD40); // RM offset
    assert_eq!(cpu.regs.si(), PM_BIOS_CODE_SEL); // PM code selector
    assert_eq!(cpu.regs.edi, 0x000FD40); // PM offset (32-bit mode uses EDI)
}

#[test]
fn test_int31_raw_mode_switch_addresses_32bit() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.protected_mode = true;
    dpmi.cpu_mode = 1; // 32-bit mode

    cpu.regs.set_ax(0x0306);
    dpmi.translation_services(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.bx(), 0xF000); // RM-to-PM segment
    assert_eq!(cpu.regs.cx(), 0xFD50); // RM-to-PM offset
    assert_eq!(cpu.regs.si(), PM_BIOS_CODE_SEL); // PM-to-RM selector (BIOS code segment)
    assert_eq!(cpu.regs.edi, 0x000FD58); // PM-to-RM offset (32-bit mode uses EDI)
}

#[test]
fn test_raw_switch_to_pm_uses_full_32bit_regs() {
    let mut dpmi = Dpmi::new();
    dpmi.set_cpu_mode(1); // a 32-bit client passes EBX/EDI
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ldt_base = 0x20000;
    cpu.regs.ldt_limit = 0x001F; // 4 entries
    let ldt_base = cpu.regs.ldt_base as usize;

    let cs_sel = 0x000F; // LDT index 1, RPL=3
    let ds_sel = 0x0017; // LDT index 2, RPL=3
    let ss_sel = 0x001F; // LDT index 3, RPL=3

    dpmi.write_descriptor_raw(&mut memory, ldt_base, 1, 0x00000, 0xFFFF, 0xFA, 0x0);
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 2, 0x10000, 0xFFFF, 0xF2, 0x0);
    dpmi.write_descriptor_raw(&mut memory, ldt_base, 3, 0x20000, 0xFFFF, 0xF2, 0x0);

    cpu.regs.set_ax(ds_sel);
    cpu.regs.set_cx(ds_sel);
    cpu.regs.set_dx(ss_sel);
    cpu.regs.ebx = 0x1234_5678;
    cpu.regs.set_si(cs_sel);
    cpu.regs.edi = 0xABCD_1234;

    dpmi.handle_raw_switch_to_pm(&mut cpu, &mut memory).unwrap();

    assert!(cpu.is_protected_mode());
    assert_eq!(cpu.regs.cs, cs_sel);
    assert_eq!(cpu.regs.ds, ds_sel);
    assert_eq!(cpu.regs.es, ds_sel);
    assert_eq!(cpu.regs.ss, ss_sel);
    assert_eq!(cpu.regs.eip, 0xABCD_1234);
    assert_eq!(cpu.regs.esp, 0x1234_5678);
    assert_eq!(cpu.regs.ip(), 0x1234);
    assert_eq!(cpu.regs.sp(), 0x5678);
}

#[test]
fn test_raw_switch_to_rm_uses_full_32bit_regs() {
    let mut dpmi = Dpmi::new();
    dpmi.set_cpu_mode(1); // a 32-bit client passes EBX/EDI
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.cr0 |= 0x0000_0001;
    dpmi.protected_mode = true;

    cpu.regs.set_ax(0x1111);
    cpu.regs.set_cx(0x2222);
    cpu.regs.set_dx(0x3333);
    cpu.regs.ebx = 0xCAFE_D00D;
    cpu.regs.set_si(0x4444);
    cpu.regs.edi = 0xDEAD_BEEF;

    dpmi.handle_raw_switch_to_rm(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.is_protected_mode());
    assert_eq!(cpu.regs.cs, 0x4444);
    assert_eq!(cpu.regs.ds, 0x1111);
    assert_eq!(cpu.regs.es, 0x2222);
    assert_eq!(cpu.regs.ss, 0x3333);
    assert_eq!(cpu.regs.eip, 0xDEAD_BEEF);
    assert_eq!(cpu.regs.esp, 0xCAFE_D00D);
    assert_eq!(cpu.regs.ip(), 0xBEEF);
    assert_eq!(cpu.regs.sp(), 0xD00D);
}

#[test]
fn test_load_rm_registers_sets_stack_when_non_zero() {
    let dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let rm = RealModeCallStruct {
        ss: 0x2000,
        sp: 0xFFFC,
        ..RealModeCallStruct::default()
    };

    dpmi.load_rm_registers(&mut cpu, &mut memory, &rm).unwrap();
    assert_eq!(cpu.regs.ss, 0x2000);
    assert_eq!(cpu.regs.sp(), 0xFFFC);
}

#[test]
fn test_enter_v86_for_interrupt_custom_stack() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    memory.write_u16(0x20 * 4, 0x1234);
    memory.write_u16(0x20 * 4 + 2, 0x1000);

    let rm = RealModeCallStruct {
        ss: 0x2000,
        sp: 0xFFFC,
        ..RealModeCallStruct::default()
    };

    dpmi.enter_v86_for_interrupt(&mut cpu, &memory, &rm, 0x50000, 0x20)
        .unwrap();
    assert_eq!(cpu.regs.ss, 0x2000);
    assert_eq!(cpu.regs.sp(), 0xFFFC);
}

#[test]
fn test_enter_v86_for_interrupt_default_stack_when_zero() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    memory.write_u16(0x10 * 4, 0x1234);
    memory.write_u16(0x10 * 4 + 2, 0x1000);

    let rm = RealModeCallStruct {
        ss: 0,
        sp: 0,
        ..RealModeCallStruct::default()
    };

    dpmi.enter_v86_for_interrupt(&mut cpu, &memory, &rm, 0x50000, 0x10)
        .unwrap();
    // When SS:SP = 0:0 the host supplies the stack. It sits at the TOP of
    // the EBDA, not at 9FC0:0000 whose SP=0 made the very first push wrap
    // to 0xAFBFE inside the VGA aperture (MDD-BUG-CRU-01386).
    assert_eq!(cpu.regs.ss, Dpmi::V86_CALLBACK_STACK_SEG);
    assert_eq!(cpu.regs.sp(), Dpmi::V86_CALLBACK_STACK_TOP);
    // This function performs no push, so the property is where the first
    // one WOULD land: inside the EBDA rather than in video memory.
    let first_push = usize::from(cpu.regs.ss) * 16 + usize::from(cpu.regs.sp()) - 2;
    assert!(
        (0x9_FC00..=0x9_FFFF).contains(&first_push),
        "first push at {first_push:#X} must be inside the EBDA"
    );
}

#[test]
fn test_enter_v86_for_interrupt_zero_ss_nonzero_sp() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    memory.write_u16(0x11 * 4, 0x4321);
    memory.write_u16(0x11 * 4 + 2, 0x1000);

    let rm = RealModeCallStruct {
        ss: 0,
        sp: 0xFFFC,
        ..RealModeCallStruct::default()
    };

    dpmi.enter_v86_for_interrupt(&mut cpu, &memory, &rm, 0x50000, 0x11)
        .unwrap();
    assert_eq!(cpu.regs.ss, 0x0000);
    assert_eq!(cpu.regs.sp(), 0xFFFC);
}

#[test]
fn test_enter_protected_mode_sets_eip() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Both 16-bit and 32-bit clients return to 16-bit stub code with segment offsets.
    // The stub is responsible for setting up flat addressing before jumping to 32-bit code.
    // EIP/ESP are segment offsets (ret_ip = 0x100, sp = 0x0000 after pops)
    assert_eq!(cpu.regs.ip(), 0x0100);
    assert_eq!(cpu.regs.eip, 0x0100);
    // SP after popping ret_ip and ret_cs (4 bytes) wraps from 0xFFFC to 0x0000
    assert_eq!(cpu.regs.sp(), 0);
    assert_eq!(cpu.regs.esp, 0);
}

#[test]
fn test_enter_protected_mode_sets_descriptor_table_sizes() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    assert_eq!(dpmi.gdt_base, 0x100000);
    assert_eq!(dpmi.gdt_size, 0x1000);
    assert_eq!(dpmi.ldt_base, 0x101000);
    assert_eq!(dpmi.ldt_size, 0x8000);
    assert_eq!(dpmi.idt_base, 0x109000);
    assert_eq!(dpmi.idt_size, 0x1000);
    assert_eq!(cpu.regs.ldt_limit, 0x7FFF);
}

#[test]
fn test_enter_protected_mode_sets_ldt_descriptor_and_ldtr() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    assert_eq!(cpu.regs.ldtr, PM_LDT_SEL);

    let gdt_base = dpmi.gdt_base as usize;
    let desc_addr = gdt_base + 4 * 8;
    let limit_low = memory.read_u16(desc_addr);
    let base_low = memory.read_u16(desc_addr + 2);
    let base_mid = memory.read_u8(desc_addr + 4);
    let access = memory.read_u8(desc_addr + 5);
    let limit_flags = memory.read_u8(desc_addr + 6);
    let base_high = memory.read_u8(desc_addr + 7);

    let base = (base_low as u32) | ((base_mid as u32) << 16) | ((base_high as u32) << 24);
    let limit = (limit_low as u32) | (((limit_flags & 0x0F) as u32) << 16);

    assert_eq!(base, dpmi.ldt_base);
    assert_eq!(limit, dpmi.ldt_size - 1);
    assert_eq!(access, 0x82);
}

/// **The TSS `SS0` must be a ring-0 stack selector.**
///
/// A privilege transition to CPL 0 loads `SS:ESP` from the TSS, and the CPU
/// requires the new stack selector's DPL *and* RPL both to equal the new
/// CPL (Intel SDM Vol. 3 §6.4.1; `gp64_cpu::Cpu::load_ss` enforces
/// `cache.dpl == cpl && rpl == cpl`). `SS0` used to be `PM_FLAT_DATA_SEL`
/// (0x13) — GDT[2], a DPL=3 descriptor referenced at RPL=3 — which fails
/// BOTH checks, so every interrupt gate that actually changed privilege
/// raised #GP instead of switching to the ring-0 stack. DPL=3 gates masked
/// the defect because they perform no transition at all.
///
/// The assertions are on the descriptor the selector resolves to, not on
/// the selector value alone: a selector pointing at the wrong descriptor
/// would satisfy a bare `assert_eq!(ss0, 0x30)` and still fault.
#[test]
fn test_tss_ss0_resolves_to_a_ring0_writable_stack_descriptor() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    let gdt_base = dpmi.gdt_base as usize;
    // `enter_protected_mode` lays the TSS immediately after the IDT.
    let tss_base = (dpmi.idt_base + dpmi.idt_size) as usize;
    let ss0 = memory.read_u16(tss_base + 0x08);

    assert_eq!(
        ss0 & 3,
        0,
        "SS0 {ss0:#06x} has RPL={}, but a CPL-0 stack load requires RPL=0",
        ss0 & 3
    );
    assert_eq!(
        ss0 & 4,
        0,
        "SS0 {ss0:#06x} must come from the GDT, not the LDT"
    );

    let index = (ss0 >> 3) as usize;
    assert!(index != 0, "SS0 must not be the null selector");
    let descriptor = gdt_base + index * 8;
    assert!(
        descriptor + 8 <= gdt_base + dpmi.gdt_size as usize,
        "SS0 {ss0:#06x} indexes past the GDT limit"
    );

    let access = memory.read_u8(descriptor + 5);
    let present = access & 0x80 != 0;
    let dpl = (access >> 5) & 3;
    let is_system = access & 0x10 == 0;
    let is_code = access & 0x08 != 0;
    let writable = access & 0x02 != 0;

    assert!(present, "SS0 descriptor {access:#04x} is not present");
    assert!(
        !is_system,
        "SS0 descriptor {access:#04x} is a system segment"
    );
    assert!(!is_code, "SS0 descriptor {access:#04x} is a code segment");
    assert!(
        writable,
        "SS0 descriptor {access:#04x} is not writable — a stack segment must be"
    );
    assert_eq!(
        dpl, 0,
        "SS0 descriptor {access:#04x} has DPL={dpl}, but a CPL-0 stack load requires DPL=0"
    );

    // And it is the flat segment the ring-0 stack pointer lives in, so
    // ESP0 is addressable through it.
    let limit_low = memory.read_u16(descriptor);
    let limit_flags = memory.read_u8(descriptor + 6);
    let granularity = limit_flags & 0x80 != 0;
    let raw_limit = u32::from(limit_low) | (u32::from(limit_flags & 0x0F) << 16);
    let limit = if granularity {
        (raw_limit << 12) | 0xFFF
    } else {
        raw_limit
    };
    let esp0 = memory.read_u32(tss_base + 0x04);
    assert!(
        esp0 <= limit,
        "ESP0 {esp0:#010x} lies outside the SS0 segment limit {limit:#010x}"
    );
}

/// Test that enter_protected_mode_with_psp sets ES to PSP in 32-bit mode.
///
/// Both 16-bit and 32-bit clients initially receive 16-bit segmented model descriptors.
/// The 32-bit stub code is responsible for setting up flat descriptors via INT 31h.
#[test]
fn test_enter_protected_mode_32bit_sets_es_to_psp_base() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up PSP at segment 0x1234
    let psp_segment = 0x1234u16;
    let env_segment = 0x5000u16;
    let psp_base = (psp_segment as u32) << 4;

    // Write something recognizable to PSP
    memory.write_u16((psp_base + 0x2C) as usize, env_segment); // Real-mode env segment

    // Set up stack for protected mode entry
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0xFFFE);
    let stack_base = 0x10000usize;
    memory.write_u16(stack_base + 0xFFFC, 0x0100); // Return IP
    memory.write_u16(stack_base + 0xFFFE, 0x2000); // Return CS
    cpu.regs.set_sp(0xFFFC);
    cpu.regs.set_ax(1); // 32-bit mode
    cpu.regs.ds = 0x3000;
    cpu.regs.es = 0x4000; // This should NOT be used

    dpmi.enter_protected_mode_with_psp(&mut cpu, &mut memory, psp_segment, env_segment)
        .unwrap();

    // ES should be 0x27 (LDT[4] with TI=1, RPL=3)
    assert_eq!(cpu.regs.es, 0x27);

    // LDT[4] (selector 0x27) should match PSP base with 0x100 limit
    let expected_base = psp_base;
    let ldt_base = dpmi.ldt_base as usize;
    let desc_addr = ldt_base + 4 * 8; // LDT descriptor 4
    let base_low = memory.read_u16(desc_addr + 2);
    let base_mid = memory.read_u8(desc_addr + 4);
    let base_high = memory.read_u8(desc_addr + 7);
    let actual_base = (base_low as u32) | ((base_mid as u32) << 16) | ((base_high as u32) << 24);
    assert_eq!(actual_base, expected_base, "ES should match PSP base");

    // Limit should be 0xFF (0x100 bytes) for PSP
    let limit_low = memory.read_u16(desc_addr);
    let limit_high = memory.read_u8(desc_addr + 6) & 0x0F;
    let actual_limit = (limit_low as u32) | ((limit_high as u32) << 16);
    assert_eq!(actual_limit, 0x00FF, "ES should have PSP 0x100 limit");

    let selector_entry = dpmi
        .selectors
        .iter()
        .find(|entry| entry.selector == 0x27)
        .expect("ES selector should be tracked");
    assert_eq!(selector_entry.base, expected_base);
    assert_eq!(selector_entry.limit, 0x00FF);
}

/// Test that environment selector is allocated and written to PSP[0x2C]
#[test]
fn test_enter_protected_mode_with_psp_allocates_env_selector() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let psp_segment = 0x1234u16;
    let env_segment = 0x5000u16;
    let psp_base = (psp_segment as u32) << 4;
    let env_base = (env_segment as u32) << 4;
    let env_size_paras = 0x20u16;
    // Per DPMI 0.9 spec, 16-bit DPMI env selector limit is floored at 64 KB.
    let env_limit = (env_size_paras as u32)
        .saturating_mul(16)
        .saturating_sub(1)
        .max(0xFFFF);

    // Write original environment segment to PSP[0x2C]
    memory.write_u16((psp_base + 0x2C) as usize, env_segment);
    let env_mcb_addr = (env_segment.wrapping_sub(1) as usize) * 16;
    memory.write_u8(env_mcb_addr, 0x4D); // MCB type
    memory.write_u16(env_mcb_addr + 1, psp_segment);
    memory.write_u16(env_mcb_addr + 3, env_size_paras);
    let env_size_bytes = (env_size_paras as usize) * 16;
    for i in 0..env_size_bytes.saturating_sub(2) {
        memory.write_u8(env_base as usize + i, b'A');
    }
    memory.write_u8(env_base as usize + env_size_bytes.saturating_sub(2), 0);
    memory.write_u8(env_base as usize + env_size_bytes.saturating_sub(1), 0);

    // Set up stack for protected mode entry
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0xFFFE);
    let stack_base = 0x10000usize;
    memory.write_u16(stack_base + 0xFFFC, 0x0100);
    memory.write_u16(stack_base + 0xFFFE, 0x2000);
    cpu.regs.set_sp(0xFFFC);
    cpu.regs.set_ax(1);
    cpu.regs.ds = 0x3000;
    cpu.regs.es = 0x4000;

    dpmi.enter_protected_mode_with_psp(&mut cpu, &mut memory, psp_segment, env_segment)
        .unwrap();

    // PSP[0x2C] should now contain an LDT selector (not the original segment)
    let env_selector = memory.read_u16((psp_base + 0x2C) as usize);

    // LDT selectors have TI bit set (bit 2) and should be non-zero
    assert_ne!(env_selector, 0);
    assert_ne!(env_selector, env_segment); // Should not be the original segment
    assert!(
        env_selector & 0x04 != 0,
        "Environment selector should be LDT (TI=1)"
    );

    // The selector should be tracked in dpmi.env_selector
    assert_eq!(dpmi.env_selector, env_selector);

    // Verify the descriptor base points to env_base
    if let Some(s) = dpmi.selectors.iter().find(|s| s.selector == env_selector) {
        assert_eq!(s.base, env_base);
        assert_eq!(s.limit, env_limit);
    } else {
        panic!("Environment selector not found in dpmi.selectors");
    }
}

#[test]
fn test_environment_selector_limit_uses_double_null() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let psp_segment = 0x1100u16;
    let env_segment = 0x1200u16;
    let psp_base = (psp_segment as u32) << 4;
    let env_base = (env_segment as u32) << 4;

    // Write environment block shorter than MCB size.
    let env_bytes = b"PATH=C:\\DOS\0\0";
    for (i, &byte) in env_bytes.iter().enumerate() {
        memory.write_u8(env_base as usize + i, byte);
    }

    memory.write_u16((psp_base + 0x2C) as usize, env_segment);
    let env_mcb_addr = (env_segment.wrapping_sub(1) as usize) * 16;
    memory.write_u8(env_mcb_addr, 0x4D);
    memory.write_u16(env_mcb_addr + 1, psp_segment);
    memory.write_u16(env_mcb_addr + 3, 0x20); // 512 bytes MCB

    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0xFFFE);
    let stack_base = 0x10000usize;
    memory.write_u16(stack_base + 0xFFFC, 0x0100);
    memory.write_u16(stack_base + 0xFFFE, 0x2000);
    cpu.regs.set_sp(0xFFFC);
    cpu.regs.set_ax(1);
    cpu.regs.ds = 0x3000;
    cpu.regs.es = 0x4000;

    dpmi.enter_protected_mode_with_psp(&mut cpu, &mut memory, psp_segment, env_segment)
        .unwrap();

    let env_selector = memory.read_u16((psp_base + 0x2C) as usize);
    let tracked = dpmi
        .selectors
        .iter()
        .find(|s| s.selector == env_selector)
        .expect("Environment selector not tracked");
    // Environment selector limit is based on MCB paragraph size (0x20 * 16 = 512 bytes),
    // floored at 64 KB per DPMI 0.9 spec ("Descriptor Usage Rules") for 16-bit DPMI.
    let mcb_size_paragraphs = 0x20u32;
    let expected_limit = (mcb_size_paragraphs * 16).saturating_sub(1).max(0xFFFF);
    assert_eq!(tracked.limit, expected_limit);
}

/// Test backward compatibility: when psp_segment=0 in 32-bit mode, ES matches DS.
/// Both 16-bit and 32-bit clients initially receive 16-bit segmented model descriptors.
#[test]
fn test_enter_protected_mode_without_psp_32bit_uses_ds_base() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up stack for protected mode entry
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0xFFFE);
    let stack_base = 0x10000usize;
    memory.write_u16(stack_base + 0xFFFC, 0x0100);
    memory.write_u16(stack_base + 0xFFFE, 0x2000);
    cpu.regs.set_sp(0xFFFC);
    cpu.regs.set_ax(1); // 32-bit mode
    cpu.regs.ds = 0x3000;
    cpu.regs.es = 0x4000; // Caller's ES (should NOT be used - ES matches DS)

    // Call without PSP (backward compatible mode)
    dpmi.enter_protected_mode(&mut cpu, &mut memory).unwrap();

    // ES selector 0x27 (LDT[4]) should match DS base for 16-bit stub compatibility
    let ds_base = (0x3000u32) << 4; // 0x30000
    let ldt_base = dpmi.ldt_base as usize;
    let desc_addr = ldt_base + 4 * 8; // LDT descriptor 4
    let base_low = memory.read_u16(desc_addr + 2);
    let base_mid = memory.read_u8(desc_addr + 4);
    let base_high = memory.read_u8(desc_addr + 7);
    let actual_base = (base_low as u32) | ((base_mid as u32) << 16) | ((base_high as u32) << 24);
    assert_eq!(
        actual_base, ds_base,
        "ES should match DS base for 16-bit stub"
    );

    // Limit should be 0xFFFF (64KB) for 16-bit segmented model
    let limit_low = memory.read_u16(desc_addr);
    let limit_high = memory.read_u8(desc_addr + 6) & 0x0F;
    let actual_limit = (limit_low as u32) | ((limit_high as u32) << 16);
    assert_eq!(
        actual_limit, 0xFFFF,
        "ES should have 64KB limit in 32-bit mode"
    );

    // No environment selector should be allocated
    assert_eq!(dpmi.env_selector, 0);
}

/// Test 16-bit mode: when psp_segment=0, ES points to caller's ES with 64KB limit
#[test]
fn test_enter_protected_mode_without_psp_16bit_uses_callers_es() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up stack for protected mode entry
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0xFFFE);
    let stack_base = 0x10000usize;
    memory.write_u16(stack_base + 0xFFFC, 0x0100);
    memory.write_u16(stack_base + 0xFFFE, 0x2000);
    cpu.regs.set_sp(0xFFFC);
    cpu.regs.set_ax(0); // 16-bit mode
    cpu.regs.ds = 0x3000;
    cpu.regs.es = 0x4000; // Caller's ES

    // Call without PSP (backward compatible mode)
    dpmi.enter_protected_mode(&mut cpu, &mut memory).unwrap();

    // ES selector 0x27 (LDT[4]) should point to base 0x40000 (caller's ES << 4)
    let ldt_base = dpmi.ldt_base as usize;
    let desc_addr = ldt_base + 4 * 8; // LDT descriptor 4
    let base_low = memory.read_u16(desc_addr + 2);
    let base_mid = memory.read_u8(desc_addr + 4);
    let base_high = memory.read_u8(desc_addr + 7);
    let actual_base = (base_low as u32) | ((base_mid as u32) << 16) | ((base_high as u32) << 24);
    assert_eq!(
        actual_base, 0x40000,
        "ES should point to caller's ES in 16-bit mode"
    );

    // Limit should be 0xFFFF (64KB)
    let limit_low = memory.read_u16(desc_addr);
    let limit_high = memory.read_u8(desc_addr + 6) & 0x0F;
    let actual_limit = (limit_low as u32) | ((limit_high as u32) << 16);
    assert_eq!(
        actual_limit, 0xFFFF,
        "ES should have 64KB limit in 16-bit mode"
    );

    // No environment selector should be allocated
    assert_eq!(dpmi.env_selector, 0);
}

/// Test that environment selector has correct base when accessed
#[test]
fn test_environment_selector_has_correct_base() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    let psp_segment = 0x0800u16;
    let env_segment = 0x0700u16;
    let psp_base = (psp_segment as u32) << 4;
    let env_base = (env_segment as u32) << 4;

    // Write test environment string
    let env_str = b"PATH=C:\\DOS\0COMSPEC=C:\\COMMAND.COM\0\0";
    for (i, &byte) in env_str.iter().enumerate() {
        memory.write_u8(env_base as usize + i, byte);
    }

    // Set up stack
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0xFFFE);
    let stack_base = 0x10000usize;
    memory.write_u16(stack_base + 0xFFFC, 0x0100);
    memory.write_u16(stack_base + 0xFFFE, 0x2000);
    cpu.regs.set_sp(0xFFFC);
    cpu.regs.set_ax(1);
    cpu.regs.ds = 0x3000;
    cpu.regs.es = 0x4000;

    dpmi.enter_protected_mode_with_psp(&mut cpu, &mut memory, psp_segment, env_segment)
        .unwrap();

    // Get environment selector from PSP[0x2C]
    let env_selector = memory.read_u16((psp_base + 0x2C) as usize);

    // Verify the selector's base in our tracking
    let tracked = dpmi.selectors.iter().find(|s| s.selector == env_selector);
    assert!(tracked.is_some());
    assert_eq!(tracked.unwrap().base, env_base);
}

#[test]
fn test_canonicalize_vector_selector_prefers_ldt_when_gdt_missing() {
    let dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.gdtr.base = 0x1000;
    cpu.regs.gdtr.limit = 0x0000;
    cpu.regs.ldt_base = 0x2000;
    cpu.regs.ldt_limit = 0x0FFF;

    let index = 3usize;
    let gdt_like = ((index as u16) << 3) | 0x3;
    let ldt_like = ((index as u16) << 3) | 0x7;

    dpmi.write_descriptor_raw(
        &mut memory,
        cpu.regs.ldt_base as usize,
        index,
        0x1234_0000,
        0xFFFF,
        0xFA,
        0x0,
    );

    let canonical = dpmi.canonicalize_vector_selector(&cpu, &memory, gdt_like);
    assert_eq!(canonical, ldt_like);
}

#[test]
fn test_canonicalize_vector_selector_rejects_null() {
    let dpmi = Dpmi::new();
    let cpu = Cpu::new();
    let memory = Memory::new();

    let canonical = dpmi.canonicalize_vector_selector(&cpu, &memory, 0);
    assert_eq!(canonical, 0);
}

#[test]
fn test_canonicalize_vector_selector_preserves_valid_gdt() {
    let dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.gdtr.base = 0x1000;
    cpu.regs.gdtr.limit = 0x0047;
    cpu.regs.ldt_base = 0x2000;
    cpu.regs.ldt_limit = 0x0FFF;

    let index = 3usize;
    let gdt_like = ((index as u16) << 3) | 0x3;

    dpmi.write_descriptor_raw(
        &mut memory,
        cpu.regs.gdtr.base as usize,
        index,
        0,
        0xFFFF,
        0xFA,
        0x0,
    );

    let canonical = dpmi.canonicalize_vector_selector(&cpu, &memory, gdt_like);
    assert_eq!(canonical, gdt_like);
}

#[test]
fn test_canonicalize_vector_selector_prefers_ldt_over_gdt() {
    let dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.gdtr.base = 0x1000;
    cpu.regs.gdtr.limit = 0x0017;
    cpu.regs.ldt_base = 0x2000;
    cpu.regs.ldt_limit = 0x0FFF;

    let index = 6usize;
    let gdt_like = ((index as u16) << 3) | 0x3;
    let ldt_like = ((index as u16) << 3) | 0x7;

    dpmi.write_descriptor_raw(
        &mut memory,
        cpu.regs.gdtr.base as usize,
        index,
        0,
        0xFFFF,
        0xFA,
        0x0,
    );
    dpmi.write_descriptor_raw(
        &mut memory,
        cpu.regs.ldt_base as usize,
        index,
        0x1234_0000,
        0xFFFF,
        0xFA,
        0x0,
    );

    let canonical = dpmi.canonicalize_vector_selector(&cpu, &memory, gdt_like);
    assert_eq!(canonical, ldt_like);
}

#[test]
fn test_enter_protected_mode_stack_fault_propagates() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let a20_before = memory.a20_enabled();

    cpu.regs.cr0 = 1;
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0x0000);
    cpu.regs.ss_cache = SegmentCache {
        base: 0,
        limit: 0,
        granularity: false,
        present: true,
        is_writable: true,
        is_readable: true,
        ..SegmentCache::default()
    };

    let result = dpmi.enter_protected_mode(&mut cpu, &mut memory);
    match result {
        Err(err) => match err.downcast_ref::<CpuError>() {
            Some(CpuError::StackFault) => {}
            other => panic!("expected stack fault, got {other:?}"),
        },
        Ok(_) => panic!("expected stack fault"),
    }

    assert!(!dpmi.protected_mode);
    assert_eq!(dpmi.cpu_mode, 0);
    assert_eq!(memory.a20_enabled(), a20_before);
    assert_eq!(cpu.regs.cr0, 1);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_enter_protected_mode_uses_ax_bit0_only() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up stack for protected mode entry
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0xFFFE);
    let stack_base = 0x10000usize;
    memory.write_u16(stack_base + 0xFFFC, 0x0100);
    memory.write_u16(stack_base + 0xFFFE, 0x2000);
    cpu.regs.set_sp(0xFFFC);
    cpu.regs.set_ax(0x0002); // bit 0 clear => 16-bit mode
    cpu.regs.ds = 0x3000;
    cpu.regs.es = 0x4000;

    dpmi.enter_protected_mode(&mut cpu, &mut memory).unwrap();

    assert_eq!(dpmi.cpu_mode, 0);
}

#[test]
fn test_state_save_restore_restores_ip_sp() {
    // State save/restore saves DPMI host state, NOT client CPU registers.
    // Restore (AL=1) is a NOP for registers — EIP, ESP should be unchanged.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Use 16-bit mode
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let offset = 0x0200u16;
    let buffer = (0x4000usize << 4) + offset as usize;
    cpu.regs.set_al(1); // Restore operation
    cpu.regs.set_di(offset);

    // Write values to buffer (not used by restore, but validates no crash)
    memory.write_u32(buffer + 0x2A, 0x0000_1234); // EIP
    memory.write_u32(buffer + 0x30, 0x0000_5678); // ESP

    cpu.regs.eip = 0x9999;
    cpu.regs.set_sp(0xAAAA);

    dpmi.handle_state_save_restore(&mut cpu, &mut memory)
        .unwrap();

    // Registers must NOT be modified by restore
    assert_eq!(cpu.regs.eip, 0x9999);
    assert_eq!(cpu.regs.sp(), 0xAAAA);
}

#[test]
fn test_handle_state_save_restore_invalid_buffer() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.protected_mode = true;
    cpu.regs.cr0 = 1;

    cpu.regs.set_al(0);
    cpu.regs.es = 0x0017; // LDT selector, not allocated
    cpu.regs.set_di(0);
    dpmi.handle_state_save_restore(&mut cpu, &mut memory)
        .unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_handle_state_save_restore_buffer_overflow_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    install_flat_es_selector(&mut dpmi, &mut cpu, &mut memory);

    cpu.regs.set_al(0); // Save operation

    let buffer_offset = memory
        .ram_size()
        .saturating_sub(STATE_BUFFER_SIZE_32 as usize)
        .saturating_add(1);
    let buffer_offset =
        u32::try_from(buffer_offset).expect("buffer offset fits 32-bit address space");
    cpu.regs.edi = buffer_offset;

    let buffer_linear = buffer_offset as usize;
    memory.write_u8(buffer_linear, 0xCC);

    dpmi.handle_state_save_restore(&mut cpu, &mut memory)
        .unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_LINEAR);
    assert_eq!(memory.read_u8(buffer_linear), 0xCC);
}

#[test]
fn test_raw_switch_to_pm_syncs_ip_sp() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ip(0x1111);
    cpu.regs.eip = 0x1111;
    cpu.regs.set_sp(0x2222);
    cpu.regs.set_ax(0x17); // DS = LDT[2] with TI=1, RPL=3
    cpu.regs.set_cx(0x27); // ES = LDT[4] with TI=1, RPL=3
    cpu.regs.set_dx(0x1F); // SS = LDT[3] with TI=1, RPL=3
    cpu.regs.ebx = 0x0000_4444; // (E)SP
    cpu.regs.set_si(0x0F); // CS = LDT[1] with TI=1, RPL=3
    cpu.regs.edi = 0x0000_3333; // (E)IP

    dpmi.handle_raw_switch_to_pm(&mut cpu, &mut memory).unwrap();

    assert_eq!(cpu.regs.eip, 0x0000_3333);
    assert_eq!(cpu.regs.ip(), 0x3333);
    assert_eq!(cpu.regs.esp, 0x0000_4444);
    assert_eq!(cpu.regs.sp(), 0x4444);
}

#[test]
fn test_raw_switch_to_pm_invalid_selector_no_state_change() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);
    dpmi.protected_mode = false;
    cpu.regs.cr0 &= !0x0000_0001;

    cpu.regs.set_ax(0x17);
    cpu.regs.set_cx(0x27);
    cpu.regs.set_dx(0x1F);
    cpu.regs.ebx = 0x0000_4444;
    cpu.regs.set_si(0x3333); // invalid CS selector
    cpu.regs.edi = 0x0000_2222;

    let result = dpmi.handle_raw_switch_to_pm(&mut cpu, &mut memory);
    match result {
        Err(err) => match err.downcast_ref::<gp64_cpu::CpuError>() {
            Some(gp64_cpu::CpuError::GeneralProtectionFault) => {}
            other => panic!("expected GPF, got {other:?}"),
        },
        Ok(_) => panic!("expected raw switch failure"),
    }

    assert!(!dpmi.protected_mode);
    assert_eq!(cpu.regs.cr0 & 0x0000_0001, 0);
}

/// MDD-BUG-CRUCIBLE-00648: the raw switch paths used to `self.clone()` the
/// whole `Dpmi` — a boxed 65,536-entry selector map and every live
/// collection — purely to roll back one bool. The clone is gone; this pins
/// what it used to be responsible for, so narrowing it cannot quietly lose
/// state: a failed switch must leave the selector table exactly as it was.
#[test]
fn test_failed_raw_switch_preserves_selector_state_without_a_full_clone() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);
    dpmi.protected_mode = false;
    cpu.regs.cr0 &= !0x0000_0001;

    let selectors_before = dpmi.selectors.len();
    let next_selector_before = dpmi.next_selector;

    cpu.regs.set_ax(0x17);
    cpu.regs.set_cx(0x27);
    cpu.regs.set_dx(0x1F);
    cpu.regs.ebx = 0x0000_4444;
    cpu.regs.set_si(0x3333); // invalid CS selector — forces the failure leg
    cpu.regs.edi = 0x0000_2222;

    assert!(dpmi.handle_raw_switch_to_pm(&mut cpu, &mut memory).is_err());

    assert!(!dpmi.protected_mode, "protected_mode must roll back");
    assert_eq!(
        dpmi.selectors.len(),
        selectors_before,
        "a failed switch must not add or drop selectors"
    );
    assert_eq!(
        dpmi.next_selector, next_selector_before,
        "a failed switch must not advance the selector allocator"
    );
}

#[test]
fn test_raw_switch_to_rm_syncs_ip_sp() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ip(0x1111);
    cpu.regs.eip = 0x1111;
    cpu.regs.set_sp(0x2222);
    cpu.regs.set_ax(0x1234);
    cpu.regs.set_cx(0x2345);
    cpu.regs.set_dx(0x3456);
    cpu.regs.ebx = 0x1234_5678;
    cpu.regs.set_si(0x5555);
    cpu.regs.edi = 0x8765_4321;

    dpmi.handle_raw_switch_to_rm(&mut cpu, &mut memory).unwrap();

    // A 16-bit client passes BX and DI; the register high words are not
    assert_eq!(cpu.regs.eip, 0x4321);
    assert_eq!(cpu.regs.esp, 0x5678);
}

#[test]
fn test_raw_switch_to_rm_in_real_mode_rejected() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.protected_mode = false;
    cpu.regs.cr0 &= !0x0000_0001;

    cpu.regs.set_ax(0x1234);
    cpu.regs.set_cx(0x2345);
    cpu.regs.set_dx(0x3456);
    cpu.regs.ebx = 0x1234_5678;
    cpu.regs.set_si(0x5555);
    cpu.regs.edi = 0x8765_4321;

    let result = dpmi.handle_raw_switch_to_rm(&mut cpu, &mut memory);
    match result {
        Err(err) => match err.downcast_ref::<gp64_cpu::CpuError>() {
            Some(gp64_cpu::CpuError::GeneralProtectionFault) => {}
            other => panic!("expected GPF, got {other:?}"),
        },
        Ok(_) => panic!("expected raw switch failure"),
    }

    assert!(!dpmi.protected_mode);
    assert_eq!(cpu.regs.cr0 & 0x0000_0001, 0);
}

#[test]
fn test_raw_switch_to_rm_rollback_on_failure() {
    // Verify that a failed PM-to-RM switch leaves all state unchanged.
    // Currently the only reachable failure path is calling from real mode
    // (the guard rejects before the snapshot). The snapshot/rollback closure
    // is a safety net for future changes where segment loads could fail
    // after PE is cleared.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Start in real mode (not PM) so the guard rejects.
    dpmi.protected_mode = false;
    cpu.regs.cr0 &= !0x0000_0001;

    // Set up distinctive CPU state to verify nothing changes.
    cpu.regs.cs = 0xAAAA;
    cpu.regs.ds = 0xBBBB;
    cpu.regs.es = 0xCCCC;
    cpu.regs.ss = 0xDDDD;
    cpu.regs.set_sp(0xEEEE);
    cpu.regs.eip = 0x1111_2222;
    cpu.regs.fs = 0x1111;
    cpu.regs.gs = 0x2222;

    // Raw switch registers (these should NOT be consumed on failure).
    cpu.regs.set_ax(0x1234); // new DS
    cpu.regs.set_cx(0x2345); // new ES
    cpu.regs.set_dx(0x3456); // new SS
    cpu.regs.ebx = 0x1234_5678; // new ESP
    cpu.regs.set_si(0x5555); // new CS
    cpu.regs.edi = 0x8765_4321; // new EIP

    let old_cr0 = cpu.regs.cr0;

    let result = dpmi.handle_raw_switch_to_rm(&mut cpu, &mut memory);
    assert!(result.is_err(), "should fail when not in protected mode");

    // Verify all state is unchanged.
    assert!(!dpmi.protected_mode, "DPMI PM flag should be unchanged");
    assert_eq!(cpu.regs.cr0, old_cr0, "CR0 should be unchanged");
    assert_eq!(cpu.regs.cs, 0xAAAA, "CS should be unchanged");
    assert_eq!(cpu.regs.ds, 0xBBBB, "DS should be unchanged");
    assert_eq!(cpu.regs.es, 0xCCCC, "ES should be unchanged");
    assert_eq!(cpu.regs.ss, 0xDDDD, "SS should be unchanged");
    assert_eq!(cpu.regs.sp(), 0xEEEE, "SP should be unchanged");
    assert_eq!(cpu.regs.eip, 0x1111_2222, "EIP should be unchanged");
    assert_eq!(cpu.regs.fs, 0x1111, "FS should be unchanged");
    assert_eq!(cpu.regs.gs, 0x2222, "GS should be unchanged");
}

#[test]
fn test_int31_physical_address_mapping() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0800); // Map physical address
    cpu.regs.set_bx(0x000A); // Physical address A0000h (video memory)
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_si(0x0001); // Size: 64KB (0x10000)
    cpu.regs.set_di(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // Returns a linear address window
    let linear = ((cpu.regs.bx() as u32) << 16) | (cpu.regs.cx() as u32);
    assert!(linear >= 0x100000);
}

#[test]
fn test_int31_physical_address_mapping_mmio_above_max_ram() {
    // DPMI 0x0800 must allow mapping MMIO regions above DOS_RAM_CEILING.
    // The 3dfx LFB at 0x60000000 needs this to work.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Register an MMIO region at 0x60000000, 4MB (3dfx LFB)
    memory.add_mmio_region(0x6000_0000, 4 * 1024 * 1024);

    cpu.regs.set_ax(0x0800);
    cpu.regs.set_bx(0x6000); // Physical address 0x60000000
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_si(0x0040); // Size: 4MB (0x00400000)
    cpu.regs.set_di(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Should succeed (no CARRY)
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "DPMI 0x0800 should allow MMIO addresses above DOS_RAM_CEILING"
    );
    let linear = ((cpu.regs.bx() as u32) << 16) | (cpu.regs.cx() as u32);
    assert!(linear >= 0x100000, "linear address should be above 1MB");

    // Verify physical mapping was registered
    assert_eq!(dpmi.physical_mappings.len(), 1);
    assert_eq!(dpmi.physical_mappings[0].physical, 0x6000_0000);
    assert_eq!(dpmi.physical_mappings[0].size, 0x0040_0000);

    // Verify RAM was NOT expanded (MMIO owns its own buffer)
    assert!(
        memory.ram_size() < 0x6000_0000,
        "RAM should not be expanded for MMIO regions"
    );
}

#[test]
fn test_int31_physical_address_mapping_non_mmio_above_max_ram_rejected() {
    // Non-MMIO addresses above DOS_RAM_CEILING should still be rejected.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0800);
    cpu.regs.set_bx(0x6000); // Physical address 0x60000000 (NO MMIO registered)
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_si(0x0040); // Size: 4MB
    cpu.regs.set_di(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "Non-MMIO addresses above DOS_RAM_CEILING should be rejected"
    );
}

#[test]
fn test_int31_physical_address_unsupported() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x08FF); // Unsupported subfunction
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int31_get_disable_interrupt_state() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Virtual interrupts start enabled in Dpmi::new()
    cpu.regs.set_ax(0x0900); // Get and disable interrupt state
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert_eq!(cpu.regs.al(), 1); // Was enabled
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify disabled via query
    cpu.regs.set_ax(0x0902);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert_eq!(cpu.regs.al(), 0); // Now disabled
}

#[test]
fn test_int31_get_enable_interrupt_state() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // First disable virtual interrupts
    cpu.regs.set_ax(0x0900);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Now enable
    cpu.regs.set_ax(0x0901); // Get and enable interrupt state
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert_eq!(cpu.regs.al(), 0); // Was disabled
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify enabled via query
    cpu.regs.set_ax(0x0902);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert_eq!(cpu.regs.al(), 1); // Now enabled
}

#[test]
fn test_int31_get_interrupt_state() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Virtual interrupts start enabled in Dpmi::new()
    cpu.regs.set_ax(0x0902); // Get interrupt state
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert_eq!(cpu.regs.al(), 1); // Enabled (default state)
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int31_interrupt_unsupported_subfunc() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x09FF); // Unsupported subfunction
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert_eq!(cpu.regs.ax(), DPMI_ERR_UNSUPPORTED);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int31_unsupported_function() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0xFF00); // Completely unsupported
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert_eq!(cpu.regs.ax(), 0x8001); // Unsupported function
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_allocate_descriptor() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0000); // Allocate descriptor
    cpu.regs.set_cx(1); // Count = 1
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0x0107); // First selector
    assert_eq!(dpmi.selectors.len(), 1);
}

#[test]
fn test_allocate_multiple_descriptors() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0000); // Allocate descriptor
    cpu.regs.set_cx(3); // Count = 3
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0x0107); // First selector
    assert_eq!(dpmi.selectors.len(), 3);
}

#[test]
fn test_allocate_zero_descriptors() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x0000); // Allocate descriptor
    cpu.regs.set_cx(0); // Count = 0
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_allocate_descriptor_out_of_ldt_range_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ldt_limit = 0x0007; // One LDT entry max
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_DESC_UNAVAILABLE);
}

#[test]
fn test_allocate_descriptor_overflow_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.next_selector = 0xFFFC;
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_DESC_UNAVAILABLE);
}

#[test]
fn test_allocate_descriptor_large_count_overflow_fails() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dpmi.next_selector = 0x0007;
    cpu.regs.ldt_limit = 0xFFFF; // Max LDT range
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(0x2000); // count * 8 = 0x10000
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_DESC_UNAVAILABLE);
}

#[test]
fn test_ldt_entry_count_max_limit() {
    let dpmi = Dpmi::new();
    let mut cpu = Cpu::new();

    cpu.regs.ldt_limit = 0xFFFF;
    assert_eq!(dpmi.ldt_entry_count(&cpu), 0x10000 / 8);
}

#[test]
fn test_free_descriptor() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // First allocate a descriptor
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    // Now free it
    cpu.regs.set_ax(0x0001); // Free descriptor
    cpu.regs.set_bx(selector);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_set_segment_base() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate a descriptor first
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    // Set segment base
    cpu.regs.set_ax(0x0007); // Set segment base
    cpu.regs.set_bx(selector);
    cpu.regs.set_cx(0x0010); // Base high word
    cpu.regs.set_dx(0x0000); // Base low word
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify base was set
    let base = dpmi.get_selector_base(selector);
    assert_eq!(base, Some(0x00100000));
}

#[test]
fn test_set_segment_limit() {
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Allocate a descriptor first
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let selector = cpu.regs.ax();

    // Set segment limit
    cpu.regs.set_ax(0x0008); // Set segment limit
    cpu.regs.set_bx(selector);
    cpu.regs.set_cx(0x0001); // Limit high word
    cpu.regs.set_dx(0xFFFF); // Limit low word
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_linear_address_for_selector_rejects_offset_past_limit() {
    let mut dpmi = Dpmi::new();
    let cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.protected_mode = true;

    let selector = 0x0017;
    dpmi.selectors.push(Selector {
        selector,
        base: 0x1000,
        limit: 0x00FF,
        access: 0x92,
        flags: 0,
        lock_count: 0,
    });

    let result = dpmi.linear_address_for_selector(&cpu, &mut memory, selector, 0x0200);
    assert_eq!(result, Err(DPMI_ERR_INVALID_SELECTOR));
}

#[test]
fn test_linear_address_for_selector_requires_present() {
    let mut dpmi = Dpmi::new();
    let cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.protected_mode = true;

    let selector = 0x0017;
    dpmi.selectors.push(Selector {
        selector,
        base: 0x1000,
        limit: 0x00FF,
        access: 0x12,
        flags: 0,
        lock_count: 0,
    });

    let result = dpmi.linear_address_for_selector(&cpu, &mut memory, selector, 0x0010);
    assert_eq!(result, Err(DPMI_ERR_INVALID_SELECTOR));
}

#[test]
fn test_linear_address_for_selector_enforces_gdt_limit() {
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    dpmi.protected_mode = true;

    cpu.regs.cr0 = 1;
    let selector = 0x0008;
    cpu.regs.ds = selector;
    cpu.regs.ds_cache = SegmentCache {
        base: 0x2000,
        limit: 0x00FF,
        cached_effective_limit: 0x00FF,
        present: true,
        ..SegmentCache::default()
    };

    let result = dpmi.linear_address_for_selector(&cpu, &mut memory, selector, 0x0200);
    assert_eq!(result, Err(DPMI_ERR_INVALID_SELECTOR));
}

/// A CLIENT-supplied real-mode stack has no byte budget counted up from
/// offset 0: `SP` wraps within its segment and hardware services the copy.
/// This replaces `test_copy_pm_stack_to_rm_underflow_guard`, which asserted
/// `Err(8021h)` for exactly these arguments and so pinned the invented
/// guard this record removes (MDD-BUG-CRU-01389).
#[test]
fn copy_pm_stack_to_rm_wraps_a_client_stack_instead_of_refusing_it() {
    let dpmi = Dpmi::new();
    let cpu = Cpu::new();
    let mut memory = Memory::new();

    assert_eq!(
        dpmi.copy_pm_stack_to_rm(
            &cpu,
            &mut memory,
            0x1000,
            4,
            4,
            mode_switch::RealModeStack::ClientSupplied,
        ),
        Ok(0xFFFC),
        "eight bytes below SP=4 wraps to the top of the segment"
    );

    // The record's own case: SP = 0x0100, CX = 0x81 is 258 bytes, which the
    // old guard called 258 > 256 and refused.
    assert_eq!(
        dpmi.copy_pm_stack_to_rm(
            &cpu,
            &mut memory,
            0x1000,
            0x0100,
            0x81,
            mode_switch::RealModeStack::ClientSupplied,
        ),
        Ok(0xFFFE)
    );
}

/// A HOST-supplied stack is the `SS:SP = 0:0` form. Every `CX > 0` used to
/// be refused with a spurious 8021h because the guard measured the offset
/// the host had just substituted; the copy now proceeds, and the only
/// budget that really exists — the host stack's own size — still refuses a
/// copy deeper than it.
#[test]
fn copy_pm_stack_to_rm_serves_a_host_stack_and_bounds_it_by_its_own_size() {
    let dpmi = Dpmi::new();
    let cpu = Cpu::new();
    let mut memory = Memory::new();

    assert_eq!(
        dpmi.copy_pm_stack_to_rm(
            &cpu,
            &mut memory,
            Dpmi::V86_CALLBACK_STACK_SEG,
            Dpmi::V86_CALLBACK_STACK_TOP,
            4,
            mode_switch::RealModeStack::HostSupplied,
        ),
        Ok(0x00F8),
        "four words fit the host stack and land inside the EBDA"
    );

    let over = Dpmi::V86_CALLBACK_STACK_TOP / 2 + 1;
    assert_eq!(
        dpmi.copy_pm_stack_to_rm(
            &cpu,
            &mut memory,
            Dpmi::V86_CALLBACK_STACK_SEG,
            Dpmi::V86_CALLBACK_STACK_TOP,
            over,
            mode_switch::RealModeStack::HostSupplied,
        ),
        Err(DPMI_ERR_INVALID_VALUE),
        "one word deeper than the host stack holds cannot be honoured"
    );
}

#[test]
fn test_get_selector_base_nonexistent() {
    let dpmi = Dpmi::new();
    let base = dpmi.get_selector_base(0xFFFF);
    assert!(base.is_none());
}

#[test]
fn test_selector_struct() {
    let selector = Selector {
        selector: 0x0107,
        base: 0x12345678,
        limit: 0x0000FFFF,
        access: 0x92,
        flags: 0x40,
        lock_count: 0,
    };
    assert_eq!(selector.selector, 0x0107);
    assert_eq!(selector.base, 0x12345678);
    assert_eq!(selector.limit, 0x0000FFFF);
    assert_eq!(selector.access, 0x92);
    assert_eq!(selector.flags, 0x40);
}

#[test]
fn test_memory_block_struct() {
    let block = MemoryBlock::new(1, 0x00100000, 0x10000);
    assert_eq!(block.handle, 1);
    assert_eq!(block.address, 0x00100000);
    assert_eq!(block.size, 0x10000);
}

#[test]
fn test_protected_mode_flag() {
    let mut dpmi = Dpmi::new();
    assert!(!dpmi.protected_mode);
    dpmi.protected_mode = true;
    assert!(dpmi.protected_mode);
}
