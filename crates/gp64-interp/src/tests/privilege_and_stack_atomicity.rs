use super::*;

// ========================================================================
// RETF privilege transition tests (ring 0 → ring 3)
// ========================================================================

/// Set up PM GDT with ring-0 and ring-3 code/data segments at 0x10000.
/// Returns (ring0_cs, ring0_ss, ring3_cs, ring3_ss).
fn setup_pm_gdt(cpu: &mut Cpu, mem: &mut Memory) -> (u16, u16, u16, u16) {
    let gdt_base: u32 = 0x10000;
    let gdt_limit: u16 = 5 * 8 - 1;
    write_gdt_entry(mem, gdt_base as usize, [0u8; 8]);
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x08,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x08),
    ); // ring-0 code
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x10,
        make_descriptor(0, 0xFFFFF, 0x92, 0x08),
    ); // ring-0 data
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x18,
        make_descriptor(0, 0xFFFFF, 0xFA, 0x08),
    ); // ring-3 code
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x20,
        make_descriptor(0, 0xFFFFF, 0xF2, 0x08),
    ); // ring-3 data
    cpu.regs.gdtr.base = gdt_base;
    cpu.regs.gdtr.limit = gdt_limit;
    cpu.regs.cr0 |= 1;
    (0x08, 0x10, 0x1B, 0x23)
}

fn setup_ring0(cpu: &mut Cpu, mem: &Memory, cs_sel: u16, ss_sel: u16) {
    cpu.regs.cs = cs_sel;
    cpu.regs.cs_cache = cpu.read_descriptor(mem, cs_sel).unwrap();
    cpu.cs_bits = if cpu.regs.cs_cache.big { 32 } else { 16 };
    cpu.regs.cpl = 0;
    cpu.regs.ss = ss_sel;
    cpu.regs.ss_cache = cpu.read_descriptor(mem, ss_sel).unwrap();
}

#[test]
fn test_retf_16bit_privilege_transition_ring0_to_ring3() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, ring3_cs, ring3_ss) = setup_pm_gdt(&mut cpu, &mut mem);
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0xCB; // RETF 16-bit

    let stack_base: usize = 0x5000;
    cpu.regs.set_sp(stack_base as u16);
    cpu.regs.esp = stack_base as u32;
    let return_ip: u16 = 0x3000;
    let outer_sp: u16 = 0x8000;
    mem.ram[stack_base..stack_base + 2].copy_from_slice(&return_ip.to_le_bytes());
    mem.ram[stack_base + 2..stack_base + 4].copy_from_slice(&ring3_cs.to_le_bytes());
    mem.ram[stack_base + 4..stack_base + 6].copy_from_slice(&outer_sp.to_le_bytes());
    mem.ram[stack_base + 6..stack_base + 8].copy_from_slice(&ring3_ss.to_le_bytes());

    // DS/ES = ring-0 (should be nullified), FS = ring-3 (should survive)
    cpu.regs.ds = ring0_ss;
    cpu.regs.ds_cache = cpu.read_descriptor(&mem, ring0_ss).unwrap();
    cpu.regs.es = ring0_ss;
    cpu.regs.es_cache = cpu.read_descriptor(&mem, ring0_ss).unwrap();
    cpu.regs.fs = ring3_ss;
    cpu.regs.fs_cache = cpu.read_descriptor(&mem, ring3_ss).unwrap();
    cpu.regs.gs = 0;
    cpu.regs.gs_cache = SegmentCache::default();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::Retf { .. }));
    assert_eq!(cpu.regs.cs, ring3_cs);
    assert_eq!(cpu.regs.cpl, 3);
    assert_eq!(cpu.regs.ip(), return_ip);
    assert_eq!(cpu.regs.eip, return_ip as u32);
    assert_eq!(cpu.regs.ss, ring3_ss);
    assert_eq!(cpu.regs.sp(), outer_sp);
    assert_eq!(cpu.regs.ds, 0, "DS should be nullified");
    assert_eq!(cpu.regs.es, 0, "ES should be nullified");
    assert_eq!(cpu.regs.fs, ring3_ss, "FS should survive");
    assert_eq!(cpu.regs.gs, 0, "GS was already 0");
}

#[test]
fn unwatched_retf_refreshes_outer_ring_stack_base() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let (ring0_cs, ring0_ss, ring3_cs, ring3_ss) = setup_pm_gdt(&mut cpu, &mut mem);
    // The outer ring uses a different physical stack base, so a stale cached
    // SS base puts the next instruction's push in the wrong memory.
    write_gdt_entry(
        &mut mem,
        0x10000 + 0x20,
        make_descriptor(0x40000, 0xFFFF, 0xF2, 0),
    );
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);
    cpu.far_return_watch = false;
    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    cpu.regs.set_sp(0x5000);
    cpu.regs.esp = 0x5000;
    cpu.regs.set_ax(0xBEEF);
    mem.ram[0x2000] = 0xCB;
    mem.ram[0x3000] = 0x50; // PUSH AX in the caller's ring.
    mem.ram[0x5000..0x5008].copy_from_slice(&[
        0x00,
        0x30,
        ring3_cs as u8,
        (ring3_cs >> 8) as u8,
        0x00,
        0x80,
        ring3_ss as u8,
        (ring3_ss >> 8) as u8,
    ]);

    // A return that changes CPL ends the batch even unwatched (the batch
    // caches the stack size and the page-access privilege at entry); the
    // PUSH then runs in the next batch against the outer ring's stack.
    let result = run(&mut cpu, &mut mem, &mut (), 2);
    assert!(
        matches!(result, RunResult::Retf { executed: 1 }),
        "{result:?}"
    );
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "{result:?}"
    );
    assert_eq!(cpu.regs.sp(), 0x7FFE);
    assert_eq!(mem.read_u16(0x47FFE), 0xBEEF);
    assert_eq!(mem.read_u16(0x07FFE), 0);
}

/// A 32-bit ring-0 return to a ring-3 caller on a 16-bit stack must refresh
/// the cached stack size and privilege level. The returned ESP has a nonzero
/// high word, but a caller's PUSH addresses SS:SP because the new SS.B is zero.
/// Using SS:ESP instead would incorrectly raise #SS beyond the 64 KiB limit.
fn unwatched_outer_return_uses_the_outer_stack_size(opcode: u8, returned_eflags: Option<u32>) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let (ring0_cs, ring0_ss, ring3_cs, ring3_ss) = setup_pm_gdt(&mut cpu, &mut mem);
    let gdt = 0x10000;
    write_gdt_entry(
        &mut mem,
        gdt + 0x08,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        gdt + 0x10,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        gdt + 0x20,
        make_descriptor(0x40000, 0xFFFF, 0xF2, 0),
    );
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);
    assert_eq!(cpu.cs_bits, 32);
    assert!(cpu.regs.ss_cache.big);
    cpu.far_return_watch = false;
    cpu.regs.eip = 0x2000;
    cpu.regs.esp = 0x5000;
    cpu.regs.set_ax(0xBEEF);
    mem.ram[0x2000] = opcode;
    mem.ram[0x3000] = 0x50; // PUSH AX in the 16-bit ring-3 caller.
    let mut frame = vec![0x3000u32, ring3_cs as u32];
    frame.extend(returned_eflags);
    frame.extend([0x0001_8000, ring3_ss as u32]);
    for (i, value) in frame.iter().enumerate() {
        mem.ram[0x5000 + i * 4..0x5004 + i * 4].copy_from_slice(&value.to_le_bytes());
    }

    let first = run(&mut cpu, &mut mem, &mut (), 2);
    assert_eq!(cpu.regs.cpl, 3, "{first:?}");
    assert!(!cpu.regs.ss_cache.big);
    if matches!(
        first,
        RunResult::Retf { executed: 1 } | RunResult::Iret { executed: 1 }
    ) {
        let second = run(&mut cpu, &mut mem, &mut (), 1);
        assert!(
            matches!(second, RunResult::BudgetExhausted { executed: 1 }),
            "{second:?}"
        );
    } else {
        assert!(
            matches!(first, RunResult::BudgetExhausted { executed: 2 }),
            "{first:?}"
        );
    }
    assert_eq!(
        cpu.regs.esp, 0x0001_7FFE,
        "a 16-bit push decrements SP only"
    );
    assert_eq!(mem.read_u16(0x47FFE), 0xBEEF, "the push lands at SS:SP");
    assert_eq!(mem.read_u16(0x57FFE), 0, "not at SS:ESP");
}

#[test]
fn unwatched_retfd_to_a_16bit_outer_stack_pushes_through_sp() {
    unwatched_outer_return_uses_the_outer_stack_size(0xCB, None);
}

#[test]
fn unwatched_iretd_to_a_16bit_outer_stack_pushes_through_sp() {
    unwatched_outer_return_uses_the_outer_stack_size(0xCF, Some(0x0000_0002));
}

#[test]
fn test_retf_32bit_privilege_transition_ring0_to_ring3() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, ring3_cs, ring3_ss) = setup_pm_gdt(&mut cpu, &mut mem);
    // Overwrite with 32-bit (D=1) descriptors: flags = 0x0C (G=1, D=1)
    write_gdt_entry(
        &mut mem,
        0x10000 + 0x08,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        0x10000 + 0x10,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        0x10000 + 0x18,
        make_descriptor(0, 0xFFFFF, 0xFA, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        0x10000 + 0x20,
        make_descriptor(0, 0xFFFFF, 0xF2, 0x0C),
    );
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);
    assert_eq!(cpu.cs_bits, 32);

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0xCB; // RETF (32-bit due to D-bit)

    let stack_base: usize = 0x50000;
    cpu.regs.set_sp(stack_base as u16);
    cpu.regs.esp = stack_base as u32;
    let return_eip: u32 = 0x0000_3000;
    let outer_esp: u32 = 0x0008_0000;
    mem.ram[stack_base..stack_base + 4].copy_from_slice(&return_eip.to_le_bytes());
    mem.ram[stack_base + 4..stack_base + 8].copy_from_slice(&(ring3_cs as u32).to_le_bytes());
    mem.ram[stack_base + 8..stack_base + 12].copy_from_slice(&outer_esp.to_le_bytes());
    mem.ram[stack_base + 12..stack_base + 16].copy_from_slice(&(ring3_ss as u32).to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::Retf { .. }));
    assert_eq!(cpu.regs.cs, ring3_cs);
    assert_eq!(cpu.regs.cpl, 3);
    assert_eq!(cpu.regs.eip, return_eip);
    assert_eq!(cpu.regs.ss, ring3_ss);
    assert_eq!(cpu.regs.esp, outer_esp);
}

/// a RETF stack-LIMIT violation raises `#SS(0)`.
///
/// The error code must not carry whatever an earlier segment load left in
/// `last_exception_error_code`. `try_pop16`/`try_pop32` reach `StackFault`
/// only through `stack_range_ok`, i.e. a limit violation with no selector
/// involved, and the SDM specifies error code 0 for that.
///
/// The field is poisoned first so this cannot pass vacuously: before the fix
/// the poison is exactly what the guest saw pushed.
#[test]
fn retf_stack_limit_fault_reports_ss_error_code_zero_kiln565() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, _ring3_cs, _ring3_ss) = setup_pm_gdt(&mut cpu, &mut mem);
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);

    cpu.last_exception_error_code = 0xDEAD_BEEF;

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0xCB; // RETF

    // Shrink the stack segment and park SP past its limit, so the very first
    // pop of the return frame is a limit violation rather than a page fault.
    // Granularity must be cleared too: the harness's GDT entries are
    // page-granular, so a raw limit of 0x100 would still scale to 0x100FFF.
    cpu.regs.ss_cache.granularity = false;
    cpu.regs.ss_cache.limit = 0x100;
    cpu.regs.ss_cache.recompute_effective_limit();
    assert_eq!(
        cpu.regs.ss_cache.effective_limit(),
        0x100,
        "the stack limit must really be small, or this test proves nothing"
    );
    cpu.regs.set_sp(0x0FF0);
    cpu.regs.esp = 0x0000_0FF0;

    let result = run(&mut cpu, &mut mem, &mut io, 1);

    match result {
        RunResult::Exception {
            vector,
            error_code,
            fault_addr,
            ..
        } => {
            assert_eq!(vector, 12, "a stack-limit violation is #SS");
            assert_eq!(
                error_code, 0,
                "#SS from a limit violation carries error code 0, not the \
                 stale {:#X} left in last_exception_error_code",
                0xDEAD_BEEFu32
            );
            assert_eq!(fault_addr, None);
        }
        other => panic!("expected #SS from the stack-limit violation, got {other:?}"),
    }
}

#[test]
fn test_retf_same_privilege_no_stack_switch() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, _ring3_cs, _ring3_ss) = setup_pm_gdt(&mut cpu, &mut mem);
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0xCB;
    let stack_base: usize = 0x5000;
    cpu.regs.set_sp(stack_base as u16);
    cpu.regs.esp = stack_base as u32;
    let return_ip: u16 = 0x4000;
    mem.ram[stack_base..stack_base + 2].copy_from_slice(&return_ip.to_le_bytes());
    mem.ram[stack_base + 2..stack_base + 4].copy_from_slice(&ring0_cs.to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::Retf { .. }));
    assert_eq!(cpu.regs.cpl, 0);
    assert_eq!(cpu.regs.cs, ring0_cs);
    assert_eq!(cpu.regs.ip(), return_ip);
    assert_eq!(cpu.regs.ss, ring0_ss);
    assert_eq!(cpu.regs.sp(), (stack_base as u16).wrapping_add(4));
}

#[test]
fn test_retf_imm16_privilege_transition_16bit() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, ring3_cs, ring3_ss) = setup_pm_gdt(&mut cpu, &mut mem);
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0xCA;
    mem.ram[0x2001] = 0x06;
    mem.ram[0x2002] = 0x00; // RETF 6

    let stack_base: usize = 0x5000;
    cpu.regs.set_sp(stack_base as u16);
    cpu.regs.esp = stack_base as u32;
    let return_ip: u16 = 0x3000;
    let outer_sp: u16 = 0x8000;
    mem.ram[stack_base..stack_base + 2].copy_from_slice(&return_ip.to_le_bytes());
    mem.ram[stack_base + 2..stack_base + 4].copy_from_slice(&ring3_cs.to_le_bytes());
    mem.ram[stack_base + 4..stack_base + 6].copy_from_slice(&outer_sp.to_le_bytes());
    mem.ram[stack_base + 6..stack_base + 8].copy_from_slice(&ring3_ss.to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::Retf { .. }));
    assert_eq!(cpu.regs.cs, ring3_cs);
    assert_eq!(cpu.regs.cpl, 3);
    assert_eq!(cpu.regs.ip(), return_ip);
    assert_eq!(cpu.regs.ss, ring3_ss);
    // Outer SP should have immediate (6) added
    assert_eq!(cpu.regs.sp(), outer_sp.wrapping_add(6));
}

#[test]
fn test_retf_imm16_same_privilege_adjusts_current_stack() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, _ring3_cs, _ring3_ss) = setup_pm_gdt(&mut cpu, &mut mem);
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0xCA;
    mem.ram[0x2001] = 0x04;
    mem.ram[0x2002] = 0x00; // RETF 4

    let stack_base: usize = 0x5000;
    cpu.regs.set_sp(stack_base as u16);
    cpu.regs.esp = stack_base as u32;
    let return_ip: u16 = 0x4000;
    mem.ram[stack_base..stack_base + 2].copy_from_slice(&return_ip.to_le_bytes());
    mem.ram[stack_base + 2..stack_base + 4].copy_from_slice(&ring0_cs.to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::Retf { .. }));
    assert_eq!(cpu.regs.cpl, 0);
    assert_eq!(cpu.regs.ip(), return_ip);
    // SP = base + 4 (popped IP+CS) + 4 (immediate)
    assert_eq!(cpu.regs.sp(), (stack_base as u16).wrapping_add(8));
}

#[test]
fn test_retf_privilege_transition_nullifies_all_inner_segments() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, ring3_cs, ring3_ss) = setup_pm_gdt(&mut cpu, &mut mem);
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0xCB;
    let stack_base: usize = 0x5000;
    cpu.regs.set_sp(stack_base as u16);
    cpu.regs.esp = stack_base as u32;
    let return_ip: u16 = 0x3000;
    let outer_sp: u16 = 0x8000;
    mem.ram[stack_base..stack_base + 2].copy_from_slice(&return_ip.to_le_bytes());
    mem.ram[stack_base + 2..stack_base + 4].copy_from_slice(&ring3_cs.to_le_bytes());
    mem.ram[stack_base + 4..stack_base + 6].copy_from_slice(&outer_sp.to_le_bytes());
    mem.ram[stack_base + 6..stack_base + 8].copy_from_slice(&ring3_ss.to_le_bytes());

    // All data segments = ring-0 DPL (should all be nullified)
    cpu.regs.ds = ring0_ss;
    cpu.regs.ds_cache = cpu.read_descriptor(&mem, ring0_ss).unwrap();
    cpu.regs.es = ring0_ss;
    cpu.regs.es_cache = cpu.read_descriptor(&mem, ring0_ss).unwrap();
    cpu.regs.fs = ring0_ss;
    cpu.regs.fs_cache = cpu.read_descriptor(&mem, ring0_ss).unwrap();
    cpu.regs.gs = ring0_ss;
    cpu.regs.gs_cache = cpu.read_descriptor(&mem, ring0_ss).unwrap();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::Retf { .. }));
    assert_eq!(cpu.regs.cpl, 3);
    assert_eq!(cpu.regs.ds, 0, "DS should be nullified");
    assert_eq!(cpu.regs.es, 0, "ES should be nullified");
    assert_eq!(cpu.regs.fs, 0, "FS should be nullified");
    assert_eq!(cpu.regs.gs, 0, "GS should be nullified");
}

#[test]
fn test_retf_null_cs_sentinel_skips_privilege_check() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (_ring0_cs, ring0_ss, _ring3_cs, _ring3_ss) = setup_pm_gdt(&mut cpu, &mut mem);
    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x08).unwrap();
    cpu.cs_bits = 16;
    cpu.regs.cpl = 0;
    cpu.regs.ss = ring0_ss;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, ring0_ss).unwrap();
    cpu.allow_null_cs = true;

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0xCB;
    let stack_base: usize = 0x5000;
    cpu.regs.set_sp(stack_base as u16);
    cpu.regs.esp = stack_base as u32;
    let sentinel_ip: u16 = 0xFFFF;
    let null_cs: u16 = 0x0000;
    mem.ram[stack_base..stack_base + 2].copy_from_slice(&sentinel_ip.to_le_bytes());
    mem.ram[stack_base + 2..stack_base + 4].copy_from_slice(&null_cs.to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::Retf { .. }));
    assert_eq!(cpu.regs.cpl, 0);
    assert_eq!(cpu.regs.cs, 0);
    assert_eq!(cpu.regs.ss, ring0_ss);
}

// ========================================================================
// Stack atomicity tests — Fix 1: RETF privilege transition with invalid SS
// ========================================================================

#[test]
fn test_retf_16bit_priv_transition_invalid_ss_restores_state() {
    // RETF with ring-0→ring-3 transition where the outer SS selector is invalid.
    // After the fault, CS/CPL/ESP must be restored to pre-RETF values.
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, ring3_cs, _ring3_ss) = setup_pm_gdt(&mut cpu, &mut mem);
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0xCB; // RETF 16-bit

    let stack_base: usize = 0x5000;
    let orig_sp: u16 = stack_base as u16;
    let orig_esp: u32 = stack_base as u32;
    cpu.regs.set_sp(orig_sp);
    cpu.regs.esp = orig_esp;

    let return_ip: u16 = 0x3000;
    let outer_sp: u16 = 0x8000;
    let invalid_ss: u16 = 0x0003; // Null selector (index 0) with RPL=3 → load_ss fails
    mem.ram[stack_base..stack_base + 2].copy_from_slice(&return_ip.to_le_bytes());
    mem.ram[stack_base + 2..stack_base + 4].copy_from_slice(&ring3_cs.to_le_bytes());
    mem.ram[stack_base + 4..stack_base + 6].copy_from_slice(&outer_sp.to_le_bytes());
    mem.ram[stack_base + 6..stack_base + 8].copy_from_slice(&invalid_ss.to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    // A null SS selector is #GP(13), not #SS(12): the far RETF SS-load now
    // delivers the architecturally-correct vector.
    assert!(
        matches!(result, RunResult::Exception { vector: 13, .. }),
        "null SS on far RETF must #GP(13), got {result:?}"
    );
    // CS, CPL, cs_bits must be restored to ring-0 values
    assert_eq!(
        cpu.regs.cs, ring0_cs,
        "CS must be restored after RETF fault"
    );
    assert_eq!(cpu.regs.cpl, 0, "CPL must be restored after RETF fault");
    assert_eq!(cpu.cs_bits, 16, "cs_bits must be restored after RETF fault");
    // ESP/SP must be restored to pre-RETF values
    assert_eq!(
        cpu.regs.sp(),
        orig_sp,
        "SP must be restored after RETF fault"
    );
    assert_eq!(
        cpu.regs.esp, orig_esp,
        "ESP must be restored after RETF fault"
    );
    // IP must be restored to point at the RETF instruction
    assert_eq!(
        cpu.regs.ip(),
        0x2000,
        "IP must be restored to RETF instruction"
    );
}

#[test]
fn test_retf_32bit_priv_transition_invalid_ss_restores_state() {
    // 32-bit RETF with ring-0→ring-3 transition where outer SS is invalid.
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, ring3_cs, _ring3_ss) = setup_pm_gdt(&mut cpu, &mut mem);
    // Overwrite with 32-bit (D=1) descriptors
    write_gdt_entry(
        &mut mem,
        0x10000 + 0x08,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        0x10000 + 0x10,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        0x10000 + 0x18,
        make_descriptor(0, 0xFFFFF, 0xFA, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        0x10000 + 0x20,
        make_descriptor(0, 0xFFFFF, 0xF2, 0x0C),
    );
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);
    assert_eq!(cpu.cs_bits, 32);

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0xCB; // RETF (32-bit due to D-bit)

    let stack_base: usize = 0x50000;
    let orig_sp: u16 = stack_base as u16;
    let orig_esp: u32 = stack_base as u32;
    cpu.regs.set_sp(orig_sp);
    cpu.regs.esp = orig_esp;

    let return_eip: u32 = 0x0000_3000;
    let outer_esp: u32 = 0x0008_0000;
    let invalid_ss: u32 = 0x0003; // Null selector → load_ss fails
    mem.ram[stack_base..stack_base + 4].copy_from_slice(&return_eip.to_le_bytes());
    mem.ram[stack_base + 4..stack_base + 8].copy_from_slice(&(ring3_cs as u32).to_le_bytes());
    mem.ram[stack_base + 8..stack_base + 12].copy_from_slice(&outer_esp.to_le_bytes());
    mem.ram[stack_base + 12..stack_base + 16].copy_from_slice(&invalid_ss.to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    // A null SS selector is #GP(13), not #SS(12): the far RETF SS-load now
    // delivers the architecturally-correct vector.
    assert!(
        matches!(result, RunResult::Exception { vector: 13, .. }),
        "null SS on far RETF must #GP(13), got {result:?}"
    );
    assert_eq!(
        cpu.regs.cs, ring0_cs,
        "CS must be restored after RETF fault"
    );
    assert_eq!(cpu.regs.cpl, 0, "CPL must be restored after RETF fault");
    assert_eq!(cpu.cs_bits, 32, "cs_bits must be restored after RETF fault");
    assert_eq!(
        cpu.regs.esp, orig_esp,
        "ESP must be restored after RETF fault"
    );
    assert_eq!(
        cpu.regs.sp(),
        orig_sp,
        "SP must be restored after RETF fault"
    );
    assert_eq!(
        cpu.regs.eip, 0x2000,
        "EIP must be restored to RETF instruction"
    );
}

#[test]
fn test_retf_imm16_16bit_priv_transition_invalid_ss_restores_state() {
    // RETF imm16 (0xCA) with 16-bit operand, ring-0→ring-3 transition, invalid SS.
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, ring3_cs, _ring3_ss) = setup_pm_gdt(&mut cpu, &mut mem);
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    // 0xCA imm16=0x0004
    mem.ram[0x2000] = 0xCA;
    mem.ram[0x2001] = 0x04;
    mem.ram[0x2002] = 0x00;

    let stack_base: usize = 0x5000;
    let orig_sp: u16 = stack_base as u16;
    let orig_esp: u32 = stack_base as u32;
    cpu.regs.set_sp(orig_sp);
    cpu.regs.esp = orig_esp;

    let return_ip: u16 = 0x3000;
    let outer_sp: u16 = 0x8000;
    let invalid_ss: u16 = 0x0003;
    mem.ram[stack_base..stack_base + 2].copy_from_slice(&return_ip.to_le_bytes());
    mem.ram[stack_base + 2..stack_base + 4].copy_from_slice(&ring3_cs.to_le_bytes());
    mem.ram[stack_base + 4..stack_base + 6].copy_from_slice(&outer_sp.to_le_bytes());
    mem.ram[stack_base + 6..stack_base + 8].copy_from_slice(&invalid_ss.to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    // A null SS selector is #GP(13), not #SS(12): the far RETF SS-load now
    // delivers the architecturally-correct vector.
    assert!(
        matches!(result, RunResult::Exception { vector: 13, .. }),
        "null SS on far RETF must #GP(13), got {result:?}"
    );
    assert_eq!(cpu.regs.cs, ring0_cs, "CS must be restored");
    assert_eq!(cpu.regs.cpl, 0, "CPL must be restored");
    assert_eq!(cpu.regs.sp(), orig_sp, "SP must be restored");
    assert_eq!(cpu.regs.esp, orig_esp, "ESP must be restored");
    assert_eq!(
        cpu.regs.ip(),
        0x2000,
        "IP must be restored to RETF instruction"
    );
}

#[test]
fn test_retf_imm16_32bit_priv_transition_invalid_ss_restores_state() {
    // RETF imm16 (0xCA) with 32-bit operand, ring-0→ring-3 transition, invalid SS.
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, ring3_cs, _ring3_ss) = setup_pm_gdt(&mut cpu, &mut mem);
    write_gdt_entry(
        &mut mem,
        0x10000 + 0x08,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        0x10000 + 0x10,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        0x10000 + 0x18,
        make_descriptor(0, 0xFFFFF, 0xFA, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        0x10000 + 0x20,
        make_descriptor(0, 0xFFFFF, 0xF2, 0x0C),
    );
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);
    assert_eq!(cpu.cs_bits, 32);

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0xCA;
    mem.ram[0x2001] = 0x08;
    mem.ram[0x2002] = 0x00;

    let stack_base: usize = 0x50000;
    let orig_sp: u16 = stack_base as u16;
    let orig_esp: u32 = stack_base as u32;
    cpu.regs.set_sp(orig_sp);
    cpu.regs.esp = orig_esp;

    let return_eip: u32 = 0x0000_3000;
    let outer_esp: u32 = 0x0008_0000;
    let invalid_ss: u32 = 0x0003;
    mem.ram[stack_base..stack_base + 4].copy_from_slice(&return_eip.to_le_bytes());
    mem.ram[stack_base + 4..stack_base + 8].copy_from_slice(&(ring3_cs as u32).to_le_bytes());
    mem.ram[stack_base + 8..stack_base + 12].copy_from_slice(&outer_esp.to_le_bytes());
    mem.ram[stack_base + 12..stack_base + 16].copy_from_slice(&invalid_ss.to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    // A null SS selector is #GP(13), not #SS(12): the far RETF SS-load now
    // delivers the architecturally-correct vector.
    assert!(
        matches!(result, RunResult::Exception { vector: 13, .. }),
        "null SS on far RETF must #GP(13), got {result:?}"
    );
    assert_eq!(cpu.regs.cs, ring0_cs, "CS must be restored");
    assert_eq!(cpu.regs.cpl, 0, "CPL must be restored");
    assert_eq!(cpu.cs_bits, 32, "cs_bits must be restored");
    assert_eq!(cpu.regs.esp, orig_esp, "ESP must be restored");
    assert_eq!(cpu.regs.sp(), orig_sp, "SP must be restored");
    assert_eq!(
        cpu.regs.eip, 0x2000,
        "EIP must be restored to RETF instruction"
    );
}

// Test load_cs_iret failure also restores SP (pops already modified SP)
#[test]
fn test_retf_16bit_load_cs_iret_failure_restores_sp() {
    // RETF 16-bit where load_cs_iret fails because the CS selector is invalid.
    // The two pops (IP, CS) have already modified SP — SP must be restored.
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, _ring3_cs, _ring3_ss) = setup_pm_gdt(&mut cpu, &mut mem);
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0xCB; // RETF 16-bit

    let stack_base: usize = 0x5000;
    let orig_sp: u16 = stack_base as u16;
    let orig_esp: u32 = stack_base as u32;
    cpu.regs.set_sp(orig_sp);
    cpu.regs.esp = orig_esp;

    let return_ip: u16 = 0x3000;
    // Use selector 0x30 which is beyond GDT limit (5*8-1=39=0x27)
    let bad_cs: u16 = 0x30;
    mem.ram[stack_base..stack_base + 2].copy_from_slice(&return_ip.to_le_bytes());
    mem.ram[stack_base + 2..stack_base + 4].copy_from_slice(&bad_cs.to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::Exception { vector: 13, .. }));
    // SP must be restored even though two pops happened
    assert_eq!(
        cpu.regs.sp(),
        orig_sp,
        "SP must be restored after load_cs_iret failure"
    );
    assert_eq!(
        cpu.regs.esp, orig_esp,
        "ESP must be restored after load_cs_iret failure"
    );
    assert_eq!(
        cpu.regs.ip(),
        0x2000,
        "IP must be restored to RETF instruction"
    );
}

#[test]
fn test_retf_32bit_load_cs_iret_failure_restores_esp() {
    // RETF 32-bit where load_cs_iret fails. ESP must be restored.
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, _ring3_cs, _ring3_ss) = setup_pm_gdt(&mut cpu, &mut mem);
    write_gdt_entry(
        &mut mem,
        0x10000 + 0x08,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        0x10000 + 0x10,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C),
    );
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);
    assert_eq!(cpu.cs_bits, 32);

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0xCB;

    let stack_base: usize = 0x50000;
    let orig_sp: u16 = stack_base as u16;
    let orig_esp: u32 = stack_base as u32;
    cpu.regs.set_sp(orig_sp);
    cpu.regs.esp = orig_esp;

    let return_eip: u32 = 0x0000_3000;
    let bad_cs: u32 = 0x30; // Beyond GDT limit
    mem.ram[stack_base..stack_base + 4].copy_from_slice(&return_eip.to_le_bytes());
    mem.ram[stack_base + 4..stack_base + 8].copy_from_slice(&bad_cs.to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::Exception { vector: 13, .. }));
    assert_eq!(
        cpu.regs.esp, orig_esp,
        "ESP must be restored after load_cs_iret failure"
    );
    assert_eq!(
        cpu.regs.sp(),
        orig_sp,
        "SP must be restored after load_cs_iret failure"
    );
    assert_eq!(
        cpu.regs.eip, 0x2000,
        "EIP must be restored to RETF instruction"
    );
}

// ========================================================================
// Stack atomicity tests — Fix 2: POP SS with invalid selector
// ========================================================================

#[test]
fn test_pop_ss_invalid_selector_restores_sp() {
    // In PM, POP SS with an invalid selector should fault and restore SP.
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, _ring3_cs, _ring3_ss) = setup_pm_gdt(&mut cpu, &mut mem);
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0x17; // POP SS

    let stack_base: usize = 0x5000;
    let orig_sp: u16 = stack_base as u16;
    let orig_esp: u32 = stack_base as u32;
    cpu.regs.set_sp(orig_sp);
    cpu.regs.esp = orig_esp;

    // Place invalid SS selector on stack: null selector with RPL=3 (0x0003)
    // load_ss fails because (selector & 0xFFF8) == 0
    let invalid_ss: u16 = 0x0003;
    mem.ram[stack_base..stack_base + 2].copy_from_slice(&invalid_ss.to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::Exception { vector: 13, .. }));
    assert_eq!(
        cpu.regs.sp(),
        orig_sp,
        "SP must be restored after POP SS fault"
    );
    assert_eq!(
        cpu.regs.esp, orig_esp,
        "ESP must be restored after POP SS fault"
    );
    assert_eq!(
        cpu.regs.ip(),
        0x2000,
        "IP must be restored to POP SS instruction"
    );
    // SS must still be the original value
    assert_eq!(
        cpu.regs.ss, ring0_ss,
        "SS must not change after POP SS fault"
    );
}

// ========================================================================
// Stack atomicity tests — Fix 3: LSS with invalid SS selector
// ========================================================================

#[test]
fn test_lss_invalid_ss_restores_register() {
    // LSS r16, [mem] where SS selector is invalid. The destination register
    // must be restored to its pre-instruction value.
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, _ring3_cs, _ring3_ss) = setup_pm_gdt(&mut cpu, &mut mem);
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    // LSS AX, [0x6000] = 0F B2 06 00 60
    // 0F prefix, B2 opcode, modrm=0x06 (AX, [disp16])
    mem.ram[0x2000] = 0x0F;
    mem.ram[0x2001] = 0xB2;
    mem.ram[0x2002] = 0x06; // modrm: reg=AX(0), rm=6 ([disp16])
    mem.ram[0x2003] = 0x00; // disp16 lo
    mem.ram[0x2004] = 0x60; // disp16 hi = 0x6000

    // Set up source memory at DS:0x6000 with offset + invalid SS
    let src_addr = cpu.regs.ds_cache.base as usize + 0x6000;
    let offset: u16 = 0x1234;
    let invalid_ss: u16 = 0x0003; // Null selector → load_ss fails
    mem.ram[src_addr..src_addr + 2].copy_from_slice(&offset.to_le_bytes());
    mem.ram[src_addr + 2..src_addr + 4].copy_from_slice(&invalid_ss.to_le_bytes());

    // Set AX to a known value that should be restored
    let orig_ax: u16 = 0xBEEF;
    cpu.regs.eax = orig_ax as u32;

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::Exception { vector: 13, .. }));
    assert_eq!(
        cpu.regs.eax as u16, orig_ax,
        "AX must be restored after LSS fault"
    );
    assert_eq!(
        cpu.regs.ip(),
        0x2000,
        "IP must be restored to LSS instruction"
    );
    assert_eq!(cpu.regs.ss, ring0_ss, "SS must not change after LSS fault");
}
