use super::*;

#[test]
fn test_set_exception_handler_0203_trace_linear_overflow() {
    let _tracing = init_tracing();
    // Lines 2548-2564: base.checked_add(offset) overflows
    let _lock = env_lock();

    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handler_sel = cpu.regs.ax();

    // Set base to near u32::MAX so base + offset overflows
    let handler_base = 0xFFFF_FFF0u32;
    if let Some(s) = dpmi
        .selectors
        .iter_mut()
        .find(|s| s.selector == handler_sel)
    {
        s.access = 0xFA; // code, present, DPL=3
        s.base = handler_base;
        s.limit = 0xFFFF_FFFF; // huge limit
        s.flags = 0xC0; // G=1, D=1
    }
    dpmi.write_selector_descriptor(
        &mut cpu,
        &mut memory,
        handler_sel,
        handler_base,
        0xFFFF_FFFF,
        0xFA,
        0xC0,
    );

    cpu.regs.set_ax(0x0203);
    cpu.regs.set_bl(0x00); // exception #0
    cpu.regs.set_cx(handler_sel);
    cpu.regs.edx = 0x20; // offset that causes base+offset to overflow u32
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Should succeed (handler is set) - the trace path handles the overflow gracefully
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_set_exception_handler_0203_null_selector_index() {
    // DPMI 0.9 / 1.0 §5.2.2: handler selector with index 0 (NULL) is a
    // clear-handler request, not an error. Lenient accept (matches
    // the DOS model). See HLD 2026.05.10 - HLD - generic DPMI 8001
    // unhandled.md.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x0203);
    cpu.regs.set_bl(0x0D);
    cpu.regs.set_cx(0x0000); // Null selector
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(dpmi.exception_handler_entry(0x0D).is_none());
}

#[test]
fn test_0203_null_selector_clears_existing_handler() {
    // Install a valid handler, then NULL it out, and verify the slot
    // is cleared (present=false). DPMI 0.9 / 1.0 §5.2.2.
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate a code descriptor as the handler.
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handler_sel = cpu.regs.ax();

    // Configure as 32-bit code segment, present, DPL=3.
    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(handler_sel);
    cpu.regs.set_cx(0x40FA);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Install handler at exception 0.
    cpu.regs.set_ax(0x0203);
    cpu.regs.set_bl(0x00);
    cpu.regs.set_cx(handler_sel);
    cpu.regs.edx = 0x100;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(dpmi.exception_handler_entry(0).is_some());

    // Now NULL the handler: this must clear the slot, with CF=0.
    cpu.regs.set_ax(0x0203);
    cpu.regs.set_bl(0x00);
    cpu.regs.set_cx(0x0000);
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(dpmi.exception_handler_entry(0).is_none());
}

#[test]
fn test_0205_null_selector_clears_pm_idt_entry() {
    // For 0205h, NULL selector clears pm_idt[v] but leaves the IDT
    // gate as-is (matches the DOS model — see HLD §5.1 / supervisor
    // adjustment #1). The slot retains whatever was last written;
    // exception dispatch falls back through the host's HLE stub.
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate and configure a handler code segment.
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handler_sel = cpu.regs.ax();
    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(handler_sel);
    cpu.regs.set_cx(0x40FA); // 32-bit code, DPL=3, present
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Install at INT 0x21.
    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x21);
    cpu.regs.set_cx(handler_sel);
    cpu.regs.set_dx(0x0100);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(dpmi.pm_idt[0x21].present);
    assert_eq!(dpmi.pm_idt[0x21].selector, handler_sel);

    // Now NULL the vector.
    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x21);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dx(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(!dpmi.pm_idt[0x21].present);
}

#[test]
fn test_0213_null_selector_clears_extended_handler() {
    // Same as 0x0203 but via the extended (32-bit-offset) entrypoint.
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handler_sel = cpu.regs.ax();
    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(handler_sel);
    cpu.regs.set_cx(0x40FA);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Install via 0x0213.
    cpu.regs.set_ax(0x0213);
    cpu.regs.set_bl(0x0D);
    cpu.regs.set_cx(handler_sel);
    cpu.regs.edx = 0x0100;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(dpmi.exception_handler_entry(0x0D).is_some());

    // NULL via 0x0213.
    cpu.regs.set_ax(0x0213);
    cpu.regs.set_bl(0x0D);
    cpu.regs.set_cx(0x0000);
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(dpmi.exception_handler_entry(0x0D).is_none());
}

#[test]
fn test_0203_null_selector_does_not_affect_other_exceptions() {
    // NULL'ing exception 0 must leave other installed exception handlers
    // (e.g. exception 5) intact. Catches accidental array-wide clears.
    let _tracing = init_tracing();
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let handler_sel = cpu.regs.ax();
    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(handler_sel);
    cpu.regs.set_cx(0x40FA);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Install handler at exception 5.
    cpu.regs.set_ax(0x0203);
    cpu.regs.set_bl(0x05);
    cpu.regs.set_cx(handler_sel);
    cpu.regs.edx = 0x0100;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(dpmi.exception_handler_entry(0x05).is_some());

    // NULL exception 0 (which has no installed user handler).
    cpu.regs.set_ax(0x0203);
    cpu.regs.set_bl(0x00);
    cpu.regs.set_cx(0x0000);
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Exception 5's handler must still be installed.
    let entry5 = dpmi.exception_handler_entry(0x05).expect("exc 5 still set");
    assert_eq!(entry5.selector, handler_sel);
    assert_eq!(entry5.offset, 0x0100);
}

#[test]
fn test_0203_invalid_exception_number_rejected_even_with_null_selector() {
    // Pins precedence: BL >= 32 is rejected with DPMI_ERR_INVALID_VALUE
    // even when CX is NULL (i.e. exception-number validation runs
    // first, before the lenient NULL-selector branch).
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x0203);
    cpu.regs.set_bl(0x20); // exception >= 32 → invalid
    cpu.regs.set_cx(0x0000); // NULL selector
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_exception_handler_0203_vector_target_cache_returns_none() {
    // Lines 2487-2490: vector_target_cache returns None
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Use a GDT selector that has no descriptor written
    // GDT selector 0x0008 (index=1, TI=0) - it might exist from PM setup
    // Use a higher GDT selector that doesn't exist
    let gdt_sel: u16 = 0x0040; // GDT index=8, RPL=0
                               // Expand the GDT limit to include this selector's index
    let new_limit = ((8 + 1) * 8 - 1) as u16;
    if new_limit > cpu.regs.gdtr.limit {
        cpu.regs.gdtr.limit = new_limit;
    }
    // But don't write any descriptor there - read_descriptor will return non-present

    cpu.regs.set_ax(0x0203);
    cpu.regs.set_bl(0x0C); // exception #12
    cpu.regs.set_cx(gdt_sel);
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_call_rm_int_0300_stack_copy_error() {
    // Lines 3062-3065: copy_pm_stack_to_rm fails in 0x0300
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Set up a RealModeCallStruct at a known location
    let struct_addr = 0x80000usize;
    let rm_struct = RealModeCallStruct {
        edi: 0,
        esi: 0,
        ebp: 0,
        reserved: 0,
        ebx: 0,
        edx: 0,
        ecx: 0,
        eax: 0,
        flags: 0x0200,
        es: 0,
        ds: 0,
        fs: 0,
        gs: 0,
        ip: 0x0184,
        cs: 0x0100, // Non-HLE target
        sp: 0x0000,
        ss: 0x0000, // SS:SP = 0:0 — the host supplies the stack
    };
    rm_struct.write_to_memory(&mut memory, struct_addr);

    // Point ES:DI at the struct
    let es_sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = struct_addr as u32;
        s.limit = 0xFFFF;
    }
    cpu.regs.es = es_sel;
    cpu.regs.set_di(0);

    // Set up IVT entry for INT 21h so the RM handler resolves
    let int_num = 0x21u8;
    memory.write_u16((int_num as usize) * 4, 0x0100);
    memory.write_u16((int_num as usize) * 4 + 2, 0x0100);

    cpu.regs.set_ax(0x0300);
    cpu.regs.set_bl(int_num);
    cpu.regs.set_bh(0x00);
    // CX deeper than the HOST stack holds. A client stack has no such
    // budget — real-mode SP wraps — so the error branch these three tests
    // cover is now reached through SS:SP = 0:0 (MDD-BUG-CRU-01389).
    cpu.regs.set_cx(200); // 200 words = 400 bytes; the host stack holds 256
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_call_rm_far_0301_stack_copy_error() {
    // Lines 3165-3168: copy_pm_stack_to_rm fails in 0x0301
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let struct_addr = 0x80000usize;
    let rm_struct = RealModeCallStruct {
        edi: 0,
        esi: 0,
        ebp: 0,
        reserved: 0,
        ebx: 0,
        edx: 0,
        ecx: 0,
        eax: 0,
        flags: 0x0200,
        es: 0,
        ds: 0,
        fs: 0,
        gs: 0,
        ip: 0x0100,
        cs: 0x0100, // Non-HLE target
        sp: 0x0000,
        ss: 0x0000, // SS:SP = 0:0 — the host supplies the stack
    };
    rm_struct.write_to_memory(&mut memory, struct_addr);

    let es_sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = struct_addr as u32;
        s.limit = 0xFFFF;
    }
    cpu.regs.es = es_sel;
    cpu.regs.set_di(0);

    cpu.regs.set_ax(0x0301);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_cx(200); // Deeper than the 256-byte host stack holds
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_call_rm_iret_0302_stack_copy_error() {
    // Lines 3304-3307: copy_pm_stack_to_rm fails in 0x0302
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let struct_addr = 0x80000usize;
    let rm_struct = RealModeCallStruct {
        edi: 0,
        esi: 0,
        ebp: 0,
        reserved: 0,
        ebx: 0,
        edx: 0,
        ecx: 0,
        eax: 0,
        flags: 0x0200,
        es: 0,
        ds: 0,
        fs: 0,
        gs: 0,
        ip: 0x0100,
        cs: 0x0100, // Non-HLE target
        sp: 0x0000,
        ss: 0x0000, // SS:SP = 0:0 — the host supplies the stack
    };
    rm_struct.write_to_memory(&mut memory, struct_addr);

    let es_sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = struct_addr as u32;
        s.limit = 0xFFFF;
    }
    cpu.regs.es = es_sel;
    cpu.regs.set_di(0);

    cpu.regs.set_ax(0x0302);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_cx(200); // Deeper than the 256-byte host stack holds
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_call_rm_iret_0302_trace_non_hle_target() {
    // Lines 3251-3267: DPMI trace with non-HLE target in 0x0302
    let _lock = env_lock();

    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    let struct_addr = 0x80000usize;
    let rm_struct = RealModeCallStruct {
        edi: 0,
        esi: 0,
        ebp: 0,
        reserved: 0,
        ebx: 0,
        edx: 0,
        ecx: 0,
        eax: 0,
        flags: 0x0200,
        es: 0,
        ds: 0,
        fs: 0,
        gs: 0,
        ip: 0x0100,
        cs: 0x0200, // Non-HLE, non-F000 segment
        sp: 0x1000,
        ss: 0x2000,
    };
    rm_struct.write_to_memory(&mut memory, struct_addr);

    let es_sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = struct_addr as u32;
        s.limit = 0xFFFF;
    }
    cpu.regs.es = es_sel;
    cpu.regs.edi = 0;

    cpu.regs.set_ax(0x0302);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_cx(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Should succeed and enter V86 mode
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_allocate_rm_callback_0303_trace_paths() {
    let _tracing = init_tracing();
    // Lines 3336-3341, 3382-3385: trace output during callback allocation
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ax(0x0303);
    cpu.regs.ds = cpu.regs.cs; // PM procedure in DS:SI
    cpu.regs.set_si(0x0100);
    // ES:DI points to register structure
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_enter_protected_mode_with_psp_env_program_name() {
    let _tracing = init_tracing();
    // Lines 5955-5976: env selector allocation with valid PSP and env
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Set up real-mode state
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0xFFFE);
    let stack_base = 0x10000usize;
    memory.write_u16(stack_base + 0xFFFC, 0x0100); // Return IP
    memory.write_u16(stack_base + 0xFFFE, 0x2000); // Return CS
    cpu.regs.set_sp(0xFFFC);
    cpu.regs.set_ax(0); // 16-bit mode
    cpu.regs.ds = 0x3000;
    cpu.regs.es = 0x4000;

    // Set up PSP at 0x0200 with environment segment at PSP[0x2C]
    let psp_segment = 0x0200u16;
    let env_segment = 0x0210u16;
    let psp_base = (psp_segment as usize) << 4;
    memory.write_u16(psp_base + 0x2C, env_segment);

    // Set up environment: "PATH=C:\\\0\0" (double null terminated)
    // with program name after
    let env_base = (env_segment as usize) << 4;
    // Write MCB header before the env segment
    let mcb_seg = env_segment - 1;
    let mcb_addr = (mcb_seg as usize) << 4;
    memory.write_u8(mcb_addr, 0x4D); // MCB tag
    memory.write_u16(mcb_addr + 1, psp_segment); // owner
    memory.write_u16(mcb_addr + 3, 0x0010); // 16 paragraphs
                                            // Write env string: "A=B\0\0" (double-null terminated) then program name
    memory.write_u8(env_base, b'A');
    memory.write_u8(env_base + 1, b'=');
    memory.write_u8(env_base + 2, b'B');
    memory.write_u8(env_base + 3, 0x00);
    memory.write_u8(env_base + 4, 0x00); // double-null
                                         // DOS 3.0+ program name: word count = 1, then null-terminated string
    memory.write_u16(env_base + 5, 1); // word count > 0
    memory.write_u8(env_base + 7, b'T');
    memory.write_u8(env_base + 8, b'S');
    memory.write_u8(env_base + 9, b'T');
    memory.write_u8(env_base + 10, 0x00); // null terminator for program name

    dpmi.enter_protected_mode_with_psp(&mut cpu, &mut memory, psp_segment, env_segment)
        .unwrap();

    assert!(dpmi.protected_mode);
    assert_ne!(dpmi.env_selector, 0);
}

#[test]
fn test_enter_protected_mode_32bit_mode() {
    let _tracing = init_tracing();
    // Lines 5908-5915, 6022-6053: 32-bit mode entry
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0xFFFE);
    let stack_base = 0x10000usize;
    memory.write_u16(stack_base + 0xFFFC, 0x0100); // Return IP
    memory.write_u16(stack_base + 0xFFFE, 0x2000); // Return CS
    cpu.regs.set_sp(0xFFFC);
    cpu.regs.set_ax(1); // 32-bit mode
    cpu.regs.ds = 0x3000;
    cpu.regs.es = 0x4000;

    dpmi.enter_protected_mode(&mut cpu, &mut memory).unwrap();

    assert!(dpmi.protected_mode);
    assert_eq!(dpmi.cpu_mode, 1);
}

#[test]
fn test_unsupported_05xx_subfunction() {
    // Lines 5568-5571: unknown 0x05XX subfunction
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x050C); // Non-existent subfunction
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_UNSUPPORTED);
}

#[test]
fn test_get_memory_block_size_050a_16bit_client() {
    // Lines 5000-5004: 0x050A for 16-bit client uses BX:CX and SI:DI
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false); // 16-bit

    // Directly insert a linear memory block to exercise the 16-bit path
    // (0x0504 rejects 16-bit clients, so we need to manually set this up)
    let handle = 42u32;
    let block_base = 0x200000u32;
    let block_size = 0x10000u32;
    dpmi.linear_memory_blocks
        .push(LinearMemoryBlock::new(handle, block_base, block_size, true));

    // Call 050A with the handle in SI:DI (16-bit format)
    cpu.regs.set_ax(0x050A);
    cpu.regs.set_si((handle >> 16) as u16);
    cpu.regs.set_di((handle & 0xFFFF) as u16);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // In 16-bit mode: BX:CX = base, SI:DI = size
    let returned_base = ((cpu.regs.bx() as u32) << 16) | (cpu.regs.cx() as u32);
    let returned_size = ((cpu.regs.si() as u32) << 16) | (cpu.regs.di() as u32);
    assert_eq!(returned_base, block_base);
    assert_eq!(returned_size, block_size);
}

#[test]
fn test_map_device_0508_base_plus_offset_overflow() {
    // Lines 5447-5453: block base.checked_add(offset) returns None
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Directly insert a linear block with base near u32::MAX
    let handle = 99u32;
    dpmi.linear_memory_blocks.push(LinearMemoryBlock::new(
        handle,
        0xFFFF_F000, // base near u32::MAX
        0x2000,      // 2 pages
        false,
    ));
    dpmi.next_mem_handle = 100;

    // Try to map device at offset 0x1000 which causes base + offset overflow
    cpu.regs.set_ax(0x0508);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0x1000; // offset 0x1000 + base 0xFFFF_F000 overflows u32
    cpu.regs.ecx = 1;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_map_conventional_0509_base_plus_offset_overflow() {
    // Lines 5533-5539: block base.checked_add(offset) returns None
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Directly insert a linear block with base near u32::MAX
    let handle = 99u32;
    dpmi.linear_memory_blocks
        .push(LinearMemoryBlock::new(handle, 0xFFFF_F000, 0x2000, false));
    dpmi.next_mem_handle = 100;

    // offset 0x1000 + base 0xFFFF_F000 overflows u32
    cpu.regs.set_ax(0x0509);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0x1000;
    cpu.regs.ecx = 1;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_page_attributes_0507_attr_type_invalid() {
    // Lines 5359-5362: attr_type > 1 is rejected
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate a linear block
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x1000; // 1 page
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    // Set up ES:EDX pointing to a buffer with invalid attr_type
    let buf_addr = 0x90000usize;
    let es_sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = 0;
        s.limit = 0xFFFF_FFFF;
        s.flags = 0xC0;
    }
    dpmi.write_selector_descriptor(&mut cpu, &mut memory, es_sel, 0, 0xFFFF_FFFF, 0xF2, 0xC0);
    cpu.regs.es = es_sel;
    cpu.regs.edx = buf_addr as u32;

    // Write an invalid page attribute (type=3, which is > 1)
    memory.write_u16(buf_addr, 0x0003);

    cpu.regs.set_ax(0x0507);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0; // offset 0
    cpu.regs.ecx = 1; // 1 page
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_page_attributes_0507_page_count_exceeds_block() {
    // Lines 5336-5341: rel_page + page_count > block.page_count
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate a 1-page block
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x1000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    // Set up buffer for attributes
    let buf_addr = 0x90000usize;
    let es_sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = 0;
        s.limit = 0xFFFF_FFFF;
        s.flags = 0xC0;
    }
    dpmi.write_selector_descriptor(&mut cpu, &mut memory, es_sel, 0, 0xFFFF_FFFF, 0xF2, 0xC0);
    cpu.regs.es = es_sel;
    cpu.regs.edx = buf_addr as u32;
    memory.write_u16(buf_addr, 0x0001);
    memory.write_u16(buf_addr + 2, 0x0001);

    cpu.regs.set_ax(0x0507);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0; // offset 0
    cpu.regs.ecx = 5; // 5 pages but block has only 1
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_page_attributes_0507_buffer_end_overflow() {
    // Lines 5311-5316: buffer.checked_add(total_bytes) overflows
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate a block
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x1000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    // Set up ES:EDX pointing near end of RAM
    let es_sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = 0;
        s.limit = 0xFFFF_FFFF;
        s.flags = 0xC0;
    }
    dpmi.write_selector_descriptor(&mut cpu, &mut memory, es_sel, 0, 0xFFFF_FFFF, 0xF2, 0xC0);
    cpu.regs.es = es_sel;
    cpu.regs.edx = (memory.ram_size() - 1) as u32; // near end of RAM

    cpu.regs.set_ax(0x0507);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_LINEAR);
}

#[test]
fn test_get_page_attributes_0506_buffer_end_overflow() {
    // Lines 5217-5228: buffer.checked_add or end > ram_size for get page attrs
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate a block
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x1000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    // Point ES:EDX near end of memory
    let es_sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = 0;
        s.limit = 0xFFFF_FFFF;
        s.flags = 0xC0;
    }
    dpmi.write_selector_descriptor(&mut cpu, &mut memory, es_sel, 0, 0xFFFF_FFFF, 0xF2, 0xC0);
    cpu.regs.es = es_sel;
    cpu.regs.edx = (memory.ram_size() - 1) as u32;

    cpu.regs.set_ax(0x0506);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_LINEAR);
}

#[test]
fn test_get_page_attributes_0506_offset_not_page_aligned() {
    // Lines 5194-5197: offset (EBX) not page-aligned
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate a block
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    // Set up buffer
    let buf_addr = 0x90000usize;
    let es_sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = 0;
        s.limit = 0xFFFF_FFFF;
        s.flags = 0xC0;
    }
    dpmi.write_selector_descriptor(&mut cpu, &mut memory, es_sel, 0, 0xFFFF_FFFF, 0xF2, 0xC0);
    cpu.regs.es = es_sel;
    cpu.regs.edx = buf_addr as u32;

    cpu.regs.set_ax(0x0506);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0x0100; // Not page-aligned (not a multiple of 0x1000)
    cpu.regs.ecx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_env_limit_from_mcb_with_program_name() {
    // Lines 4076-4082: env block with double-null followed by program name
    let mut memory = Memory::new();

    let env_segment = 0x0300u16;
    let mcb_seg = env_segment - 1;
    let mcb_addr = (mcb_seg as usize) << 4;
    let env_base = (env_segment as usize) << 4;

    // Write MCB header
    memory.write_u8(mcb_addr, 0x4D);
    memory.write_u16(mcb_addr + 1, 0x0200); // owner
    memory.write_u16(mcb_addr + 3, 0x0020); // 32 paragraphs = 512 bytes

    // Write environment: "X=Y\0\0" then program name "synthetic-program\0"
    let env_str = b"X=Y\0\0";
    for (i, &b) in env_str.iter().enumerate() {
        memory.write_u8(env_base + i, b);
    }
    // After double-null: word count = 1
    memory.write_u16(env_base + 5, 1);
    // Program name "abc\0"
    memory.write_u8(env_base + 7, b'a');
    memory.write_u8(env_base + 8, b'b');
    memory.write_u8(env_base + 9, b'c');
    memory.write_u8(env_base + 10, 0x00);

    let limit = Dpmi::env_limit_from_mcb(&memory, env_segment);
    assert!(limit.is_some());
    // The limit should extend past the double-null to include the program name
    let limit_val = limit.unwrap();
    assert!(
        limit_val >= 10,
        "limit should include program name, got {}",
        limit_val
    );
}

#[test]
fn test_push_descriptor_ranges_after_pm_entry() {
    // Lines 4099-4108: push GDT, LDT, IDT ranges
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let mut ranges = Vec::new();
    dpmi.push_descriptor_ranges(&mut ranges);

    // After PM entry, gdt/ldt/idt bases and sizes should be set
    assert!(
        !ranges.is_empty(),
        "should have at least one descriptor range"
    );
}

#[test]
fn test_dos_memory_allocate_0100_creates_extra_selectors_16bit() {
    // Lines 2079-2087: large allocation in 16-bit mode needs >1 selector
    // dos_memory_selector_count for 16-bit mode: paragraphs.div_ceil(0x1000)
    // 0x1000 paragraphs = 64KB, so >64KB needs multiple selectors
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false); // 16-bit

    // Allocate 0x2000 paragraphs = 128KB = 2 selectors needed
    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(0x2000); // 128KB
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Without DosMem, the fallback path is used
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // Check that the block was created with selector_count > 1
    let block = dpmi.dos_memory_blocks.last().unwrap();
    assert_eq!(block.selector_count, 2);
}

#[test]
fn test_dos_memory_allocate_0100_with_dosmem_insufficient() {
    // Lines 2221-2224: DosMem allocation fails with InsufficientMemory
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let mut dos_mem = init_dos_mem(&mut memory);

    // Try to allocate more memory than available
    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(0xFFFF); // Request max paragraphs
    dpmi.dos_memory_services(&mut cpu, &mut memory, Some(&mut dos_mem), 0x0200)
        .unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0x0008); // DOS_ERR_INSUFFICIENT_MEMORY
}

#[test]
fn test_dos_memory_free_0101_not_found() {
    // Lines 2299-2301: free with selector not in dos_memory_blocks
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate a regular selector (not DOS memory)
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let sel = cpu.regs.ax();

    // Try to free it as DOS memory
    cpu.regs.set_ax(0x0101);
    cpu.regs.set_dx(sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0x0009); // DOS_ERR_INVALID_MEMORY_BLOCK
}

#[test]
fn test_dos_memory_free_0101_selector_not_in_ldt_range() {
    // Lines 2267-2270: free with selector not in LDT range
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Use a valid LDT selector but with too high an index
    cpu.regs.set_ax(0x0101);
    cpu.regs.set_dx(0xFFF7); // LDT, RPL=3, very high index
                             // Set a restrictive LDT limit
    cpu.regs.ldt_limit = 0x003F; // only 8 entries
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0x0009); // DOS_ERR_INVALID_MEMORY_BLOCK
}

#[test]
fn test_dos_memory_resize_0102_selector_not_in_ldt_range() {
    // Lines 2312-2315: resize with selector not in LDT range
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ax(0x0102);
    cpu.regs.set_dx(0xFFF7);
    cpu.regs.set_bx(0x0100);
    cpu.regs.ldt_limit = 0x003F;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0x0009); // DOS_ERR_INVALID_MEMORY_BLOCK
}

#[test]
fn test_dos_memory_resize_0102_without_dosmem_grow() {
    // Lines 2376-2393: resize without DosMem (fallback path)
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false); // 16-bit

    // Allocate via DPMI (without DosMem)
    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(0x0100); // 256 paragraphs = 4KB
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel = cpu.regs.dx();

    // Resize to a larger size
    cpu.regs.set_ax(0x0102);
    cpu.regs.set_dx(sel);
    cpu.regs.set_bx(0x0200); // Grow to 512 paragraphs
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_dos_memory_resize_0102_shrink_without_dosmem() {
    // Lines 2159-2168: resize to smaller, freeing extra selectors
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false); // 16-bit

    // Allocate a large block (needs 2 selectors in 16-bit mode)
    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(0x2000); // 128KB = 2 selectors
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel = cpu.regs.dx();
    assert_eq!(dpmi.dos_memory_blocks.last().unwrap().selector_count, 2);

    // Resize down to 1 selector
    cpu.regs.set_ax(0x0102);
    cpu.regs.set_dx(sel);
    cpu.regs.set_bx(0x0100); // 4KB = 1 selector
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_dos_memory_free_0101_without_dosmem_success() {
    // Lines 2294-2298: free without DosMem (fallback path)
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate via DPMI (without DosMem)
    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(0x0100);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel = cpu.regs.dx();

    let blocks_before = dpmi.dos_memory_blocks.len();

    // Free it
    cpu.regs.set_ax(0x0101);
    cpu.regs.set_dx(sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dpmi.dos_memory_blocks.len(), blocks_before - 1);
}

#[test]
fn test_set_page_attributes_0507_commit_pages() {
    // Lines 5366-5389: successful set page attributes - commit pages
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate a 2-page block (uncommitted)
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 0; // uncommitted
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    // Set up buffer with commit attributes
    let buf_addr = 0x90000usize;
    let es_sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = 0;
        s.limit = 0xFFFF_FFFF;
        s.flags = 0xC0;
    }
    dpmi.write_selector_descriptor(&mut cpu, &mut memory, es_sel, 0, 0xFFFF_FFFF, 0xF2, 0xC0);
    cpu.regs.es = es_sel;
    cpu.regs.edx = buf_addr as u32;

    // Write commit attributes for both pages
    memory.write_u16(buf_addr, 0x0001); // committed
    memory.write_u16(buf_addr + 2, 0x0001); // committed

    cpu.regs.set_ax(0x0507);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 2;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_get_page_attributes_0506_success() {
    // Lines 5248-5267: successful get page attributes
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate a 2-page block (committed)
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1; // committed
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    // Set up buffer
    let buf_addr = 0x90000usize;
    let es_sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = 0;
        s.limit = 0xFFFF_FFFF;
        s.flags = 0xC0;
    }
    dpmi.write_selector_descriptor(&mut cpu, &mut memory, es_sel, 0, 0xFFFF_FFFF, 0xF2, 0xC0);
    cpu.regs.es = es_sel;
    cpu.regs.edx = buf_addr as u32;

    cpu.regs.set_ax(0x0506);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 2;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // Both pages should be committed (attr = 0x0001)
    assert_eq!(memory.read_u16(buf_addr), 0x0001);
    assert_eq!(memory.read_u16(buf_addr + 2), 0x0001);
}

#[test]
fn test_map_device_0508_success() {
    // Lines 5467-5479: successful device mapping
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate a 2-page block
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 0; // uncommitted
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    // Map device at offset 0 (1 page)
    cpu.regs.set_ax(0x0508);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0; // offset 0
    cpu.regs.ecx = 1; // 1 page
    cpu.regs.edx = 0xA0000; // VGA physical address
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_map_conventional_0509_success() {
    // Lines 5553-5565: successful conventional memory mapping
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate a 2-page block
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    // Map conventional memory
    cpu.regs.set_ax(0x0509);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 1;
    cpu.regs.edx = 0x00000; // Physical address 0
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_map_device_0508_page_count_mul_overflow() {
    // Lines 5420-5426: page_count * 0x1000 overflows u32
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate a block
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x1000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    // page_count * 0x1000 that overflows u32
    cpu.regs.set_ax(0x0508);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 0xFFFFF; // Very large page count
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_map_conventional_0509_page_count_mul_overflow() {
    // Lines 5506-5511: page_count * 0x1000 overflows u32
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x1000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    cpu.regs.set_ax(0x0509);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 0xFFFFF;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_map_device_0508_phys_end_exceeds_max_ram() {
    // Lines 5456-5459: phys_end > DOS_RAM_CEILING
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    // Physical address near end of u32 range
    cpu.regs.set_ax(0x0508);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 1;
    cpu.regs.edx = 0xFFF00000; // Very high physical address
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_LINEAR);
}

#[test]
fn test_map_conventional_0509_phys_end_exceeds_max_ram() {
    // Lines 5542-5545: phys_end > DOS_RAM_CEILING
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x2000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    cpu.regs.set_ax(0x0509);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 1;
    cpu.regs.edx = 0xFFF00000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_LINEAR);
}

#[test]
fn test_map_device_0508_page_range_exceeds_block() {
    // Lines 5441-5444: page range exceeds block
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Allocate 1 page
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x1000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    // Try to map 2 pages (block only has 1)
    cpu.regs.set_ax(0x0508);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 2;
    cpu.regs.edx = 0xA0000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_map_conventional_0509_page_range_exceeds_block() {
    // Lines 5527-5530: page range exceeds block
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 0x1000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 1;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let handle = cpu.regs.esi;

    cpu.regs.set_ax(0x0509);
    cpu.regs.esi = handle;
    cpu.regs.ebx = 0;
    cpu.regs.ecx = 2;
    cpu.regs.edx = 0;
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_call_rm_int_0300_32bit_stack_words_high_bits_set() {
    // ECX high bits are now ignored per DPMI spec — CX=0 is valid
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true); // 32-bit

    let struct_addr = 0x80000usize;
    let rm_struct = RealModeCallStruct {
        edi: 0,
        esi: 0,
        ebp: 0,
        reserved: 0,
        ebx: 0,
        edx: 0,
        ecx: 0,
        eax: 0,
        flags: 0x0200,
        es: 0,
        ds: 0,
        fs: 0,
        gs: 0,
        ip: 0x0100,
        cs: 0x0100,
        sp: 0x1000,
        ss: 0x2000,
    };
    rm_struct.write_to_memory(&mut memory, struct_addr);

    let es_sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = struct_addr as u32;
        s.limit = 0xFFFF;
    }
    cpu.regs.es = es_sel;
    cpu.regs.edi = 0;

    cpu.regs.set_ax(0x0301);
    cpu.regs.set_bh(0x00);
    cpu.regs.ecx = 0x00010000; // High bits set, CX=0 (valid)
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // High ECX bits are ignored — CX=0 means zero stack words, which is valid
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_call_rm_far_0301_stack_words_too_large() {
    // Lines 3112-3115: stack_words > 0x7FFF
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let struct_addr = 0x80000usize;
    let rm_struct = RealModeCallStruct {
        edi: 0,
        esi: 0,
        ebp: 0,
        reserved: 0,
        ebx: 0,
        edx: 0,
        ecx: 0,
        eax: 0,
        flags: 0x0200,
        es: 0,
        ds: 0,
        fs: 0,
        gs: 0,
        ip: 0x0100,
        cs: 0x0100,
        sp: 0x1000,
        ss: 0x2000,
    };
    rm_struct.write_to_memory(&mut memory, struct_addr);

    let es_sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = struct_addr as u32;
        s.limit = 0xFFFF;
    }
    cpu.regs.es = es_sel;
    cpu.regs.set_di(0);

    cpu.regs.set_ax(0x0301);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_cx(0x8000); // > 0x7FFF
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_call_rm_far_0301_invalid_bh_flags() {
    // Lines 3096-3099: BH has bits other than bit 0 set
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ax(0x0301);
    cpu.regs.set_bh(0x02); // Invalid - only bit 0 allowed
    cpu.regs.set_cx(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_call_rm_iret_0302_invalid_bh_flags() {
    // Lines 3195-3198: BH has bits other than bit 0 set
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ax(0x0302);
    cpu.regs.set_bh(0x04); // Invalid
    cpu.regs.set_cx(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_call_rm_iret_0302_stack_words_too_large() {
    // Lines 3210-3213: stack_words > 0x7FFF
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ax(0x0302);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_cx(0x8000); // > 0x7FFF
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_call_rm_iret_0302_32bit_ecx_high_bits() {
    // ECX high bits are now ignored per DPMI spec — CX=0 is valid
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true); // 32-bit

    cpu.regs.set_ax(0x0302);
    cpu.regs.set_bh(0x00);
    cpu.regs.ecx = 0x00010000; // High bits set, CX=0 (valid)
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // High ECX bits are ignored — CX=0 means zero stack words, which is valid
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_call_rm_far_0301_hle_target() {
    // Lines 3136-3152: 0x0301 with HLE target (CS=F000)
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let struct_addr = 0x80000usize;
    let rm_struct = RealModeCallStruct {
        edi: 0,
        esi: 0,
        ebp: 0,
        reserved: 0,
        ebx: 0,
        edx: 0,
        ecx: 0,
        eax: 0,
        flags: 0x0200,
        es: 0,
        ds: 0,
        fs: 0,
        gs: 0,
        ip: 0x0184,
        cs: 0xF000, // HLE stub
        sp: 0,
        ss: 0,
    };
    rm_struct.write_to_memory(&mut memory, struct_addr);

    let es_sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = struct_addr as u32;
        s.limit = 0xFFFF;
    }
    cpu.regs.es = es_sel;
    cpu.regs.set_di(0);

    cpu.regs.set_ax(0x0301);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_cx(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(dpmi.pending_hle_simulation.is_some());
}

#[test]
fn test_call_rm_iret_0302_hle_target_low_ip() {
    // Lines 3280-3282: HLE target with IP < 0x0100
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    let struct_addr = 0x80000usize;
    let rm_struct = RealModeCallStruct {
        edi: 0,
        esi: 0,
        ebp: 0,
        reserved: 0,
        ebx: 0,
        edx: 0,
        ecx: 0,
        eax: 0,
        flags: 0x0200,
        es: 0,
        ds: 0,
        fs: 0,
        gs: 0,
        ip: 0x0050,
        cs: 0xF000, // HLE stub with low IP
        sp: 0,
        ss: 0,
    };
    rm_struct.write_to_memory(&mut memory, struct_addr);

    let es_sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = struct_addr as u32;
        s.limit = 0xFFFF;
    }
    cpu.regs.es = es_sel;
    cpu.regs.edi = 0;

    cpu.regs.set_ax(0x0302);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_cx(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let hle = dpmi.pending_hle_simulation.as_ref().unwrap();
    assert_eq!(hle.int_num, 0); // IP < 0x0100, so int_num = 0
}

#[test]
fn test_dos_memory_free_0101_with_dosmem_invalid_block() {
    // Lines 2289-2292: DosMem::free returns error (segment not found)
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let mut dos_mem = init_dos_mem(&mut memory);

    // Allocate a block normally
    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(0x0010);
    dpmi.dos_memory_services(&mut cpu, &mut memory, Some(&mut dos_mem), 0x0200)
        .unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel = cpu.regs.dx();

    // Corrupt the DPMI's internal segment to one that DosMem doesn't know about.
    // DosMem::free(memory, 0) returns InvalidBlock because segment == 0.
    dpmi.dos_memory_blocks[0].segment = 0;

    // Now free via DPMI 0x0101 - DosMem::free will fail with InvalidBlock
    cpu.regs.set_ax(0x0101);
    cpu.regs.set_dx(sel);
    dpmi.dos_memory_services(&mut cpu, &mut memory, Some(&mut dos_mem), 0x0200)
        .unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0x0007); // DOS_ERR_MCB_DESTROYED
}

#[test]
fn test_dos_memory_resize_0102_with_dosmem_success() {
    // Lines 2344-2363: successful resize with DosMem
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let mut dos_mem = init_dos_mem(&mut memory);

    // Allocate a block
    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(0x0100);
    dpmi.dos_memory_services(&mut cpu, &mut memory, Some(&mut dos_mem), 0x0200)
        .unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel = cpu.regs.dx();

    // Resize it smaller
    cpu.regs.set_ax(0x0102);
    cpu.regs.set_dx(sel);
    cpu.regs.set_bx(0x0080); // Shrink to 128 paragraphs
    dpmi.dos_memory_services(&mut cpu, &mut memory, Some(&mut dos_mem), 0x0200)
        .unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_dos_memory_free_0101_with_dosmem_success() {
    // Lines 2278-2287: successful free with DosMem
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let mut dos_mem = init_dos_mem(&mut memory);

    // Allocate a block
    cpu.regs.set_ax(0x0100);
    cpu.regs.set_bx(0x0100);
    dpmi.dos_memory_services(&mut cpu, &mut memory, Some(&mut dos_mem), 0x0200)
        .unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel = cpu.regs.dx();

    // Free it
    cpu.regs.set_ax(0x0101);
    cpu.regs.set_dx(sel);
    dpmi.dos_memory_services(&mut cpu, &mut memory, Some(&mut dos_mem), 0x0200)
        .unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_call_rm_far_0301_with_reset_if() {
    // Lines 3127-3128: 0x0301 with BH bit 0 set (reset IF)
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let struct_addr = 0x80000usize;
    let rm_struct = RealModeCallStruct {
        edi: 0,
        esi: 0,
        ebp: 0,
        reserved: 0,
        ebx: 0,
        edx: 0,
        ecx: 0,
        eax: 0,
        flags: 0x0200,
        es: 0,
        ds: 0,
        fs: 0,
        gs: 0,
        ip: 0x0184,
        cs: 0xF000, // HLE stub
        sp: 0,
        ss: 0,
    };
    rm_struct.write_to_memory(&mut memory, struct_addr);

    let es_sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = struct_addr as u32;
        s.limit = 0xFFFF;
    }
    cpu.regs.es = es_sel;
    cpu.regs.set_di(0);

    cpu.regs.set_ax(0x0301);
    cpu.regs.set_bh(0x01); // reset IF
    cpu.regs.set_cx(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_exit_v86_with_trace() {
    let _tracing = init_tracing();
    // Lines 4664-4679: exit_v86 with DPMI trace enabled
    let _lock = env_lock();

    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    // Enter V86 via 0x0302 with a non-HLE target
    let struct_addr = 0x80000usize;
    let rm_struct = RealModeCallStruct {
        edi: 0,
        esi: 0,
        ebp: 0,
        reserved: 0,
        ebx: 0,
        edx: 0,
        ecx: 0,
        eax: 0,
        flags: 0x0200,
        es: 0,
        ds: 0,
        fs: 0,
        gs: 0,
        ip: 0x0100,
        cs: 0x0200,
        sp: 0x1000,
        ss: 0x2000,
    };
    rm_struct.write_to_memory(&mut memory, struct_addr);

    let es_sel = dpmi.selectors[0].selector;
    if let Some(s) = dpmi.selectors.iter_mut().find(|s| s.selector == es_sel) {
        s.base = struct_addr as u32;
        s.limit = 0xFFFF;
    }
    cpu.regs.es = es_sel;
    cpu.regs.edi = 0;

    cpu.regs.set_ax(0x0302);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_cx(0);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!dpmi.v86_saved_state.is_empty());

    // Now exit V86
    dpmi.exit_v86(&mut cpu, &mut memory);
    assert!(dpmi.v86_saved_state.is_empty());
}

#[test]
fn test_set_pm_int_vector_null_selector() {
    // DPMI 0.9 / 1.0 §5.2.2: NULL selector (selector & 0xFFFC == 0)
    // for 0205h is a clear-handler request. Lenient accept (matches
    // the DOS model — leaves IDT slot as-is, only clears pm_idt[v]). See
    // HLD 2026.05.10 - HLD - generic DPMI 8001 unhandled.md.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x21); // vector
    cpu.regs.set_cx(0x0000); // NULL selector
    cpu.regs.set_dx(0x0100); // offset
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(!dpmi.pm_idt[0x21].present);
}

#[test]
fn test_set_pm_int_vector_null_selector_0003() {
    // Selector=0x0003 has bits 0-1 set but (0x0003 & 0xFFFC) == 0, so
    // it is a NULL selector and the lenient-accept path applies.
    // See HLD 2026.05.10 - HLD - generic DPMI 8001 unhandled.md.
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x21);
    cpu.regs.set_cx(0x0003); // (0x0003 & 0xFFFC) == 0 -> NULL
    cpu.regs.set_dx(0x0100);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(!dpmi.pm_idt[0x21].present);
}

#[test]
fn test_set_pm_int_vector_ldt_out_of_range() {
    // Lines 2826-2829: LDT selector out of range
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // A very large LDT selector (TI=1, high index, RPL=3)
    let large_ldt_sel: u16 = 0xFFF7; // index=0x1FFE, TI=1, RPL=3
    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x21);
    cpu.regs.set_cx(large_ldt_sel);
    cpu.regs.set_dx(0x0100);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_pm_int_vector_gdt_selector_not_resolvable() {
    // Lines 2832-2835: GDT selector that doesn't resolve via vector_target_cache
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Use a GDT selector beyond the GDT limit so read_descriptor returns None
    // GDT index 100 = 0x320, with RPL=3 = 0x0323
    let gdt_sel: u16 = 0x0323;
    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x21);
    cpu.regs.set_cx(gdt_sel);
    cpu.regs.set_dx(0x0100);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_pm_int_vector_not_present() {
    // Lines 2837-2840: Selector not present
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate a descriptor, then mark it not-present in GDT
    // We can write a not-present descriptor directly into the GDT
    let gdt_base = cpu.regs.gdtr.base as usize;
    // Write a not-present code descriptor at GDT index 6 (selector = 6*8+3 = 0x33)
    let index = 6usize;
    let sel: u16 = (index as u16 * 8) | 3; // GDT selector with RPL=3
                                           // Make sure GDT limit covers this entry
    cpu.regs.gdtr.limit = ((index + 1) * 8 - 1) as u16;
    // Write descriptor: base=0, limit=0xFFFF, access=0x7A (not present code: P=0, DPL=3, S=1, type=1010)
    dpmi.write_descriptor_raw(&mut memory, gdt_base, index, 0, 0xFFFF, 0x7A, 0x00);

    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x21);
    cpu.regs.set_cx(sel);
    cpu.regs.set_dx(0x0100);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_pm_int_vector_not_code_segment() {
    // Lines 2842-2845: Selector is present but not a code segment
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let gdt_base = cpu.regs.gdtr.base as usize;
    let index = 6usize;
    let sel: u16 = (index as u16 * 8) | 3;
    cpu.regs.gdtr.limit = ((index + 1) * 8 - 1) as u16;
    // Write a present DATA descriptor: access=0xF2 (P=1, DPL=3, S=1, type=0010 = data r/w)
    dpmi.write_descriptor_raw(&mut memory, gdt_base, index, 0, 0xFFFF, 0xF2, 0x00);

    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x21);
    cpu.regs.set_cx(sel);
    cpu.regs.set_dx(0x0100);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}

#[test]
fn test_set_pm_int_vector_offset_beyond_limit() {
    // Lines 2847-2850: Offset exceeds segment limit
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let gdt_base = cpu.regs.gdtr.base as usize;
    let index = 6usize;
    let sel: u16 = (index as u16 * 8) | 3;
    cpu.regs.gdtr.limit = ((index + 1) * 8 - 1) as u16;
    // Code segment with limit=0x00FF (very small)
    dpmi.write_descriptor_raw(&mut memory, gdt_base, index, 0, 0x00FF, 0xFA, 0x00);

    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x21);
    cpu.regs.set_cx(sel);
    cpu.regs.set_dx(0x0200); // offset 0x200 > limit 0xFF
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_LINEAR);
}

#[test]
fn test_set_pm_int_vector_success_16bit_non_hw_irq() {
    let _tracing = init_tracing();
    // Lines 2865-2867: 16-bit client, non-HW IRQ -> gate_type 0x07 (trap gate)
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Use an existing valid LDT selector (the CS selector from PM setup)
    let cs_sel = cpu.regs.cs;
    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x21); // vector 0x21, not a HW IRQ
    cpu.regs.set_cx(cs_sel);
    cpu.regs.set_dx(0x0000); // offset 0
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // Verify pm_idt entry was set
    assert_eq!(dpmi.pm_idt[0x21].selector, cs_sel);
    assert_eq!(dpmi.pm_idt[0x21].gate_type, 0x07); // 16-bit trap gate
}

#[test]
fn test_set_pm_int_vector_success_16bit_hw_irq() {
    let _tracing = init_tracing();
    // Lines 2860-2861: 16-bit client, HW IRQ -> gate_type 0x06 (interrupt gate)
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    let cs_sel = cpu.regs.cs;
    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x08); // IRQ0 = vector 0x08, is HW IRQ
    cpu.regs.set_cx(cs_sel);
    cpu.regs.set_dx(0x0000);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dpmi.pm_idt[0x08].gate_type, 0x06); // 16-bit interrupt gate
}

#[test]
fn test_set_pm_int_vector_success_32bit_hw_irq() {
    let _tracing = init_tracing();
    // Lines 2858-2859: 32-bit client, HW IRQ -> gate_type 0x0E
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    let cs_sel = cpu.regs.cs;
    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x70); // IRQ8 = vector 0x70, is HW IRQ
    cpu.regs.set_cx(cs_sel);
    cpu.regs.edx = 0x0000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dpmi.pm_idt[0x70].gate_type, 0x0E); // 32-bit interrupt gate
}

#[test]
fn test_set_pm_int_vector_success_32bit_non_hw() {
    let _tracing = init_tracing();
    // Lines 2863-2864: 32-bit client, non-HW IRQ -> gate_type 0x0F (trap gate)
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, true);

    let cs_sel = cpu.regs.cs;
    cpu.regs.set_ax(0x0205);
    cpu.regs.set_bl(0x21); // non-HW IRQ
    cpu.regs.set_cx(cs_sel);
    cpu.regs.edx = 0x0000;
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dpmi.pm_idt[0x21].gate_type, 0x0F); // 32-bit trap gate
}

#[test]
fn test_set_access_rights_dpl_mismatch() {
    // Lines 1532-1537: DPL != CPL -> error
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // After setup_protected_mode, cpu.regs.cpl == 3
    // Allocate a selector
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel = cpu.regs.ax();

    // Set access rights with DPL=0 (bits 5-6 = 0), but CPL=3
    // access byte: P=1, DPL=0, S=1, type=0010 (data r/w) = 0x92
    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(sel);
    cpu.regs.set_cl(0x92); // access: DPL=0 != CPL=3
    cpu.regs.set_ch(0x00);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_VALUE);
}

#[test]
fn test_set_access_rights_selector_not_found() {
    // Lines 1597-1599: Selector not in selectors list
    let mut dpmi = Dpmi::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    setup_protected_mode(&mut dpmi, &mut cpu, &mut memory, false);

    // Allocate then free a selector to get a valid LDT selector that's not in the list
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    let sel = cpu.regs.ax();

    // Free it
    cpu.regs.set_ax(0x0001);
    cpu.regs.set_bx(sel);
    dpmi.int31(&mut cpu, &mut memory).unwrap();

    // Now try to set access rights on the freed selector
    // access: P=1, DPL=3, S=1, type=0010 = 0xF2
    cpu.regs.set_ax(0x0009);
    cpu.regs.set_bx(sel);
    cpu.regs.set_cl(0xF2);
    cpu.regs.set_ch(0x00);
    dpmi.int31(&mut cpu, &mut memory).unwrap();
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), DPMI_ERR_INVALID_SELECTOR);
}
