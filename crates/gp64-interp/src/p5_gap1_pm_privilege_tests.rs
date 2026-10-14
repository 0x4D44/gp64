use super::test_helpers::{make_descriptor, write_gdt_entry};
use super::*;
use gp64_memory::Memory;

/// Set up GDT at 0x10000 with ring-0 and ring-3 code/data segments.
/// Returns (ring0_cs, ring0_ss, ring3_cs, ring3_ss).
fn setup_pm_gdt(cpu: &mut Cpu, mem: &mut Memory) -> (u16, u16, u16, u16) {
    let gdt_base: u32 = 0x10000;
    let gdt_limit: u16 = 5 * 8 - 1;
    write_gdt_entry(mem, gdt_base as usize, [0u8; 8]); // null
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x08,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x08),
    ); // ring-0 code, 16-bit
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x10,
        make_descriptor(0, 0xFFFFF, 0x92, 0x08),
    ); // ring-0 data, 16-bit
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x18,
        make_descriptor(0, 0xFFFFF, 0xFA, 0x08),
    ); // ring-3 code, 16-bit
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x20,
        make_descriptor(0, 0xFFFFF, 0xF2, 0x08),
    ); // ring-3 data, 16-bit
    cpu.regs.gdtr.base = gdt_base;
    cpu.regs.gdtr.limit = gdt_limit;
    cpu.regs.cr0 |= 1;
    (0x08, 0x10, 0x1B, 0x23)
}

/// Set up GDT with 32-bit (D=1) segments.
fn setup_pm_gdt_32bit(cpu: &mut Cpu, mem: &mut Memory) -> (u16, u16, u16, u16) {
    let gdt_base: u32 = 0x10000;
    let gdt_limit: u16 = 5 * 8 - 1;
    write_gdt_entry(mem, gdt_base as usize, [0u8; 8]);
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x08,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x0C),
    ); // ring-0 code, 32-bit
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x10,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C),
    ); // ring-0 data, 32-bit
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x18,
        make_descriptor(0, 0xFFFFF, 0xFA, 0x0C),
    ); // ring-3 code, 32-bit
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x20,
        make_descriptor(0, 0xFFFFF, 0xF2, 0x0C),
    ); // ring-3 data, 32-bit
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

// ---- Test 1: RETF same-privilege (CPL 0 -> CPL 0) ----

#[test]
fn test_gap1_retf_same_privilege_ring0_to_ring0() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, _, _) = setup_pm_gdt(&mut cpu, &mut mem);
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0xCB; // RETF

    let stack_base: usize = 0x5000;
    cpu.regs.set_sp(stack_base as u16);
    cpu.regs.esp = stack_base as u32;
    let return_ip: u16 = 0x4000;
    // Push CS:IP for same-privilege return
    mem.ram[stack_base..stack_base + 2].copy_from_slice(&return_ip.to_le_bytes());
    mem.ram[stack_base + 2..stack_base + 4].copy_from_slice(&ring0_cs.to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Retf { .. }),
        "Expected Retf, got {:?}",
        result
    );
    assert_eq!(cpu.regs.cpl, 0, "CPL should remain 0");
    assert_eq!(cpu.regs.cs, ring0_cs, "CS should be ring-0 code");
    assert_eq!(cpu.regs.ip(), return_ip, "IP should be return address");
    // No SS:SP pop for same-privilege
    assert_eq!(cpu.regs.ss, ring0_ss, "SS unchanged for same-privilege");
    assert_eq!(
        cpu.regs.sp(),
        (stack_base as u16).wrapping_add(4),
        "SP should advance by 4 (popped IP+CS)"
    );
}

// ---- Test 2: RETF outer-ring (CPL 0 -> CPL 3) with segment nullification ----

#[test]
fn test_gap1_retf_outer_ring_with_nullification() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, ring3_cs, ring3_ss) = setup_pm_gdt(&mut cpu, &mut mem);
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0xCB; // RETF

    let stack_base: usize = 0x5000;
    cpu.regs.set_sp(stack_base as u16);
    cpu.regs.esp = stack_base as u32;
    let return_ip: u16 = 0x3000;
    let outer_sp: u16 = 0x8000;
    // Stack: IP, CS, SP, SS
    mem.ram[stack_base..stack_base + 2].copy_from_slice(&return_ip.to_le_bytes());
    mem.ram[stack_base + 2..stack_base + 4].copy_from_slice(&ring3_cs.to_le_bytes());
    mem.ram[stack_base + 4..stack_base + 6].copy_from_slice(&outer_sp.to_le_bytes());
    mem.ram[stack_base + 6..stack_base + 8].copy_from_slice(&ring3_ss.to_le_bytes());

    // Set DS/ES = ring-0 (should be nullified on transition to ring 3)
    cpu.regs.ds = ring0_ss;
    cpu.regs.ds_cache = cpu.read_descriptor(&mem, ring0_ss).unwrap();
    cpu.regs.es = ring0_ss;
    cpu.regs.es_cache = cpu.read_descriptor(&mem, ring0_ss).unwrap();
    // FS = ring-3 (should survive)
    cpu.regs.fs = ring3_ss;
    cpu.regs.fs_cache = cpu.read_descriptor(&mem, ring3_ss).unwrap();
    // GS already 0
    cpu.regs.gs = 0;

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Retf { .. }),
        "Expected Retf, got {:?}",
        result
    );
    assert_eq!(cpu.regs.cpl, 3, "CPL should be 3 after outer-ring return");
    assert_eq!(cpu.regs.cs, ring3_cs, "CS should be ring-3 code");
    assert_eq!(cpu.regs.ip(), return_ip, "IP should be return address");
    assert_eq!(cpu.regs.ss, ring3_ss, "SS should be loaded from stack");
    assert_eq!(cpu.regs.sp(), outer_sp, "SP should be loaded from stack");
    assert_eq!(cpu.regs.ds, 0, "DS (ring-0 DPL) should be nullified");
    assert_eq!(cpu.regs.es, 0, "ES (ring-0 DPL) should be nullified");
    assert_eq!(cpu.regs.fs, ring3_ss, "FS (ring-3 DPL) should survive");
}

// ---- Test 3: RETF with immediate (0xCA) same-privilege ----

#[test]
fn test_gap1_retf_imm16_same_privilege() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, _, _) = setup_pm_gdt(&mut cpu, &mut mem);
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    // RETF 8 (0xCA 0x08 0x00)
    mem.ram[0x2000] = 0xCA;
    mem.ram[0x2001] = 0x08;
    mem.ram[0x2002] = 0x00;

    let stack_base: usize = 0x5000;
    cpu.regs.set_sp(stack_base as u16);
    cpu.regs.esp = stack_base as u32;
    let return_ip: u16 = 0x4000;
    mem.ram[stack_base..stack_base + 2].copy_from_slice(&return_ip.to_le_bytes());
    mem.ram[stack_base + 2..stack_base + 4].copy_from_slice(&ring0_cs.to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Retf { .. }),
        "Expected Retf, got {:?}",
        result
    );
    assert_eq!(cpu.regs.cpl, 0);
    assert_eq!(cpu.regs.ip(), return_ip);
    // SP = base + 4 (IP+CS pops) + 8 (immediate)
    assert_eq!(
        cpu.regs.sp(),
        (stack_base as u16).wrapping_add(4 + 8),
        "SP should advance by 4 (pops) + 8 (immediate)"
    );
}

// ---- Test 3b: RETF with immediate (0xCA) different privilege ----

#[test]
fn test_gap1_retf_imm16_privilege_transition() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, ring3_cs, ring3_ss) = setup_pm_gdt(&mut cpu, &mut mem);
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    // RETF 10 (0xCA 0x0A 0x00)
    mem.ram[0x2000] = 0xCA;
    mem.ram[0x2001] = 0x0A;
    mem.ram[0x2002] = 0x00;

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
    assert!(
        matches!(result, RunResult::Retf { .. }),
        "Expected Retf, got {:?}",
        result
    );
    assert_eq!(cpu.regs.cpl, 3);
    assert_eq!(cpu.regs.ip(), return_ip);
    assert_eq!(cpu.regs.ss, ring3_ss);
    // Outer SP should have immediate (10) added
    assert_eq!(cpu.regs.sp(), outer_sp.wrapping_add(10));
}

// ---- Test 4: IRETD same-privilege (flags restoration) ----

#[test]
fn test_gap1_iretd_same_privilege_restores_flags() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, _, _) = setup_pm_gdt_32bit(&mut cpu, &mut mem);
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    // 0xCF is IRETD when op_size_32 (32-bit segments)
    mem.ram[0x2000] = 0xCF;

    let stack_base: usize = 0x50000;
    cpu.regs.set_sp(stack_base as u16);
    cpu.regs.esp = stack_base as u32;
    let return_eip: u32 = 0x4000;
    let return_cs: u32 = ring0_cs as u32; // same ring
                                          // Set CF, ZF, SF in EFLAGS
    let eflags: u32 = 0x0202 | 0x01 | 0x40 | 0x80; // IF | CF | ZF | SF
    mem.ram[stack_base..stack_base + 4].copy_from_slice(&return_eip.to_le_bytes());
    mem.ram[stack_base + 4..stack_base + 8].copy_from_slice(&return_cs.to_le_bytes());
    mem.ram[stack_base + 8..stack_base + 12].copy_from_slice(&eflags.to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Iret { .. }),
        "Expected Iret, got {:?}",
        result
    );
    assert_eq!(cpu.regs.eip, return_eip, "EIP should be restored");
    assert_eq!(cpu.regs.cs, ring0_cs, "CS should remain ring-0");
    assert_eq!(cpu.regs.cpl, 0, "CPL should remain 0");
    // Verify individual flags
    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "CF should be set from EFLAGS"
    );
    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::ZERO),
        "ZF should be set from EFLAGS"
    );
    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::SIGN),
        "SF should be set from EFLAGS"
    );
}

// ---- Test 5: IRETD outer-ring (CPL 0 -> CPL 3) ----

#[test]
fn test_gap1_iretd_outer_ring_privilege_transition() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, ring3_cs, ring3_ss) = setup_pm_gdt_32bit(&mut cpu, &mut mem);
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0xCF; // IRETD

    let stack_base: usize = 0x50000;
    cpu.regs.set_sp(stack_base as u16);
    cpu.regs.esp = stack_base as u32;
    let return_eip: u32 = 0x3000;
    let eflags: u32 = 0x0202; // IF
    let outer_esp: u32 = 0x80000;
    // Stack for outer-ring IRETD: EIP, CS, EFLAGS, ESP, SS
    mem.ram[stack_base..stack_base + 4].copy_from_slice(&return_eip.to_le_bytes());
    mem.ram[stack_base + 4..stack_base + 8].copy_from_slice(&(ring3_cs as u32).to_le_bytes());
    mem.ram[stack_base + 8..stack_base + 12].copy_from_slice(&eflags.to_le_bytes());
    mem.ram[stack_base + 12..stack_base + 16].copy_from_slice(&outer_esp.to_le_bytes());
    mem.ram[stack_base + 16..stack_base + 20].copy_from_slice(&(ring3_ss as u32).to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Iret { .. }),
        "Expected Iret, got {:?}",
        result
    );
    assert_eq!(cpu.regs.eip, return_eip, "EIP should be restored");
    assert_eq!(cpu.regs.cs, ring3_cs, "CS should be ring-3");
    assert_eq!(cpu.regs.cpl, 3, "CPL should be 3");
    assert_eq!(cpu.regs.esp, outer_esp, "ESP should be from stack");
    assert_eq!(cpu.regs.ss, ring3_ss, "SS should be from stack");
}

// ---- Test 6a: RETF to not-present segment ----

#[test]
fn test_gap1_retf_to_not_present_segment_raises_np() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, _, _) = setup_pm_gdt(&mut cpu, &mut mem);
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);

    // Add a not-present code segment at GDT index 5 (selector 0x28)
    // Access 0x1A = P=0, S=1, code+read
    write_gdt_entry(
        &mut mem,
        0x10000 + 0x28,
        make_descriptor(0, 0xFFFFF, 0x1A, 0x08),
    );
    // Extend GDT limit
    cpu.regs.gdtr.limit = 6 * 8 - 1;

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0xCB; // RETF

    let stack_base: usize = 0x5000;
    cpu.regs.set_sp(stack_base as u16);
    cpu.regs.esp = stack_base as u32;
    let np_sel: u16 = 0x28; // not-present selector
    mem.ram[stack_base..stack_base + 2].copy_from_slice(&0x4000u16.to_le_bytes());
    mem.ram[stack_base + 2..stack_base + 4].copy_from_slice(&np_sel.to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    // Should raise #NP (vector 11)
    assert!(
        matches!(result, RunResult::Exception { vector: 11, .. }),
        "Expected #NP exception (vector 11), got {:?}",
        result
    );
}

// ---- Test 6b: RETF to non-code segment (data segment) ----

#[test]
fn test_gap1_retf_to_data_segment_raises_gp() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let (ring0_cs, ring0_ss, _, _) = setup_pm_gdt(&mut cpu, &mut mem);
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0xCB; // RETF

    let stack_base: usize = 0x5000;
    cpu.regs.set_sp(stack_base as u16);
    cpu.regs.esp = stack_base as u32;
    // Return CS = ring0_ss (a data segment, not code)
    mem.ram[stack_base..stack_base + 2].copy_from_slice(&0x4000u16.to_le_bytes());
    mem.ram[stack_base + 2..stack_base + 4].copy_from_slice(&ring0_ss.to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    // Should raise #GP (vector 13) — loading a data segment as CS
    assert!(
        matches!(result, RunResult::Exception { vector: 13, .. }),
        "Expected #GP exception (vector 13), got {:?}",
        result
    );
}

// ---- Test 6c: RETF with DPL violation ----

#[test]
fn test_gap1_retf_dpl_violation_raises_gp() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    // Set up ring-3 as current, try to RETF to ring-0 (privilege escalation)
    let (ring0_cs, _ring0_ss, ring3_cs, ring3_ss) = setup_pm_gdt(&mut cpu, &mut mem);
    // Start at ring-3
    cpu.regs.cs = ring3_cs;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, ring3_cs).unwrap();
    cpu.cs_bits = if cpu.regs.cs_cache.big { 32 } else { 16 };
    cpu.regs.cpl = 3;
    cpu.regs.ss = ring3_ss;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, ring3_ss).unwrap();

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0xCB; // RETF

    let stack_base: usize = 0x5000;
    cpu.regs.set_sp(stack_base as u16);
    cpu.regs.esp = stack_base as u32;
    // Try to return to ring-0 CS (DPL violation: returning to inner ring)
    mem.ram[stack_base..stack_base + 2].copy_from_slice(&0x4000u16.to_le_bytes());
    mem.ram[stack_base + 2..stack_base + 4].copy_from_slice(&ring0_cs.to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    // Should raise #GP — cannot return to inner privilege level
    assert!(
        matches!(result, RunResult::Exception { vector: 13, .. }),
        "Expected #GP for DPL violation, got {:?}",
        result
    );
}

/// A ring-0 extender can return to a ring-2 client with a 16-bit RETF.
/// The return CS selector carries RPL=2, so this must use
/// the IRET/RETF privilege rules rather than the CALL/JMP load rules.
#[test]
fn test_retf_outer_ring2_return_is_accepted() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    let gdt_base = 0usize;
    let ring0_cs: u16 = 0x08;
    let ring0_ss: u16 = 0x10;
    let ring2_cs: u16 = 0x7002; // GDT[0xE00], DPL=2, RPL=2
    let ring2_ss: u16 = 0x700A; // GDT[0xE01], DPL=2, RPL=2
    write_gdt_entry(&mut mem, gdt_base, [0; 8]);
    write_gdt_entry(
        &mut mem,
        gdt_base + 0x08,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x08),
    ); // ring-0 code, 16-bit
    write_gdt_entry(
        &mut mem,
        gdt_base + 0x10,
        make_descriptor(0, 0xFFFFF, 0x92, 0x08),
    ); // ring-0 data, 16-bit
    write_gdt_entry(
        &mut mem,
        gdt_base + 0x7000,
        make_descriptor(0, 0xFFFFF, 0xDA, 0x08),
    ); // ring-2 code, 16-bit
    write_gdt_entry(
        &mut mem,
        gdt_base + 0x7008,
        make_descriptor(0, 0xFFFFF, 0xD2, 0x08),
    ); // ring-2 data, 16-bit
    cpu.regs.gdtr.base = gdt_base as u32;
    cpu.regs.gdtr.limit = 0x700F;
    cpu.regs.cr0 |= 1;
    setup_ring0(&mut cpu, &mem, ring0_cs, ring0_ss);

    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0xCA; // Synthetic RETF 0x20 at a round code address
    mem.ram[0x2001] = 0x20;
    mem.ram[0x2002] = 0x00;

    let stack_base = 0x5000usize;
    cpu.regs.set_sp(stack_base as u16);
    cpu.regs.esp = stack_base as u32;
    mem.ram[stack_base..stack_base + 2].copy_from_slice(&0x3000u16.to_le_bytes());
    mem.ram[stack_base + 2..stack_base + 4].copy_from_slice(&ring2_cs.to_le_bytes());
    mem.ram[stack_base + 4..stack_base + 6].copy_from_slice(&0x8000u16.to_le_bytes());
    mem.ram[stack_base + 6..stack_base + 8].copy_from_slice(&ring2_ss.to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Retf { .. }),
        "ring-0 to ring-2 RETF should complete, got {:?}",
        result
    );
    assert_eq!(cpu.regs.cs, ring2_cs);
    assert_eq!(cpu.regs.cpl, 2);
    assert_eq!(cpu.regs.ip(), 0x3000);
    assert_eq!(cpu.regs.ss, ring2_ss);
    assert_eq!(cpu.regs.sp(), 0x8020);
}
