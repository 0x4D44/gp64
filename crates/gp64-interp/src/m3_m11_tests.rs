use super::test_helpers::{make_descriptor, setup, write_gdt_entry};
use super::*;
use gp64_memory::Memory;

/// Helper: set up PM with GDT at 0x10000, return GDT base address.
/// Creates:
///   0x08 = ring-0 code 16-bit (0x9A, flags=0x08)
///   0x10 = ring-0 data (0x92, flags=0x08)
///   0x18 = ring-3 code 16-bit (0xFA, flags=0x08)  — RPL=3 selector = 0x1B
///   0x20 = ring-3 data (0xF2, flags=0x08)          — RPL=3 selector = 0x23
///   0x28 = system descriptor (0x82, flags=0x00)    — NOT valid for data segment
///   0x30 = not-present data (0x12, flags=0x08)     — present=0, S=1, type=data
fn setup_pm_gdt_extended(cpu: &mut Cpu, mem: &mut Memory) -> u32 {
    let gdt_base: u32 = 0x10000;
    let gdt_limit: u16 = 7 * 8 - 1;
    write_gdt_entry(mem, gdt_base as usize, [0u8; 8]); // null descriptor
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x08,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x08),
    ); // ring-0 code 16-bit
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
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x28,
        make_descriptor(0, 0xFFFFF, 0x82, 0x00),
    ); // system descriptor (invalid for data seg)
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x30,
        make_descriptor(0, 0xFFFFF, 0x12, 0x08),
    ); // not-present data
    cpu.regs.gdtr.base = gdt_base;
    cpu.regs.gdtr.limit = gdt_limit;
    cpu.regs.cr0 |= 1;
    gdt_base
}

#[test]
fn test_iretd_bad_return_cs_surfaces_selector_error_code() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    setup_pm_gdt_extended(&mut cpu, &mut mem);

    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x08).unwrap();
    cpu.cs_bits = 16;
    cpu.regs.cpl = 0;
    cpu.regs.ss = 0x10;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    cpu.regs.set_sp(0x3000);
    cpu.regs.esp = 0x3000;
    cpu.regs.flags = gp64_cpu::Flags::RESERVED1;

    mem.ram[0x2000] = 0x66; // operand-size override
    mem.ram[0x2001] = 0xCF; // IRETD
    mem.write_u32(0x3000, 0xBCDE_2345); // return EIP
    mem.write_u32(0x3004, 0x0000_000B); // return CS: GDT[1], RPL=3
    mem.write_u32(0x3008, 0x0000_0292); // EFLAGS
    mem.write_u32(0x300C, 0x0001_FFFE); // return ESP
    mem.write_u32(0x3010, 0x0000_0023); // return SS, not reached

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code,
            ..
        } => assert_eq!(error_code, 0x0008),
        other => panic!("expected IRETD #GP(0008), got {:?}", other),
    }
}

#[test]
fn test_iret_bad_return_cs_surfaces_selector_error_code() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    setup_pm_gdt_extended(&mut cpu, &mut mem);

    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x08).unwrap();
    cpu.cs_bits = 16;
    cpu.regs.cpl = 0;
    cpu.regs.ss = 0x10;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    cpu.regs.set_sp(0x3000);
    cpu.regs.esp = 0x3000;
    cpu.regs.flags = gp64_cpu::Flags::RESERVED1;

    mem.ram[0x2000] = 0xCF; // IRET
    mem.write_u16(0x3000, 0x2345); // return IP
    mem.write_u16(0x3002, 0x000B); // return CS: GDT[1], RPL=3
    mem.write_u16(0x3004, 0x0292); // FLAGS
    mem.write_u16(0x3006, 0xFFFE); // return SP
    mem.write_u16(0x3008, 0x0023); // return SS, not reached

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code,
            ..
        } => assert_eq!(error_code, 0x0008),
        other => panic!("expected IRET #GP(0008), got {:?}", other),
    }
}

// Test 1: PM MOV DS with invalid selector type → expect #GP with selector error code
#[test]
fn test_m3_mov_ds_invalid_selector_type_gp_with_error_code() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    setup_pm_gdt_extended(&mut cpu, &mut mem);

    // Set up as ring-0 code
    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x08).unwrap();
    cpu.cs_bits = 16;
    cpu.regs.cpl = 0;
    cpu.regs.ss = 0x10;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.ds = 0x10;
    cpu.regs.ds_cache = cpu.read_descriptor(&mem, 0x10).unwrap();

    // MOV DS, AX — with AX = 0x28 (system descriptor, invalid for data segment)
    // 0x8E 0xD8 = MOV DS, AX (seg_idx=3, rm=0=AX)
    let selector: u16 = 0x28;
    cpu.regs.set_ax(selector);
    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0x8E;
    mem.ram[0x2001] = 0xD8; // MOV DS, AX

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code,
            ..
        } => {
            assert_eq!(
                error_code,
                (selector & 0xFFFC) as u32,
                "error code must contain selector with RPL/TI cleared"
            );
        }
        other => panic!("expected #GP with selector error code, got {:?}", other),
    }
    // DS should NOT have been changed
    assert_eq!(cpu.regs.ds, 0x10, "DS must not be modified on fault");
}

// Test 2: PM POP ES with DPL violation → expect #GP with selector error code
#[test]
fn test_m3_pop_es_dpl_violation_gp_with_error_code() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    setup_pm_gdt_extended(&mut cpu, &mut mem);

    // Set up as ring-3 code
    cpu.regs.cs = 0x1B; // ring-3 code
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x1B).unwrap();
    cpu.cs_bits = 16;
    cpu.regs.cpl = 3;
    cpu.regs.ss = 0x23; // ring-3 data
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x23).unwrap();
    cpu.regs.es = 0x23;
    cpu.regs.es_cache = cpu.read_descriptor(&mem, 0x23).unwrap();

    // POP ES (0x07) — push ring-0 data selector on stack (DPL=0, we're ring-3 → violation)
    let ring0_data: u16 = 0x10; // DPL=0 data, RPL=0
    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0x07; // POP ES

    // Set up stack with the bad selector
    let stack_top: u16 = 0x5000;
    cpu.regs.set_sp(stack_top);
    cpu.regs.esp = stack_top as u32;
    mem.ram[stack_top as usize..stack_top as usize + 2].copy_from_slice(&ring0_data.to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code,
            ..
        } => {
            assert_eq!(
                error_code,
                (ring0_data & 0xFFFC) as u32,
                "#GP error code must contain selector"
            );
        }
        other => panic!("expected #GP with selector error code, got {:?}", other),
    }
    // SP must be restored (atomicity)
    assert_eq!(
        cpu.regs.sp(),
        stack_top,
        "SP must be restored on POP ES fault"
    );
    // ES must be unchanged
    assert_eq!(cpu.regs.es, 0x23, "ES must not be modified on fault");
}

// Test 3: PM LES with not-present segment → expect #NP (vector 11) with selector error code
#[test]
fn test_m3_les_not_present_np_with_error_code() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    setup_pm_gdt_extended(&mut cpu, &mut mem);

    // Ring-0 setup
    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x08).unwrap();
    cpu.cs_bits = 16;
    cpu.regs.cpl = 0;
    cpu.regs.ss = 0x10;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.ds = 0x10;
    cpu.regs.ds_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.es = 0x10;
    cpu.regs.es_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.eax = 0xCAFE_BABE;

    // LES AX, [0x3000] — 0xC4 0x06 (modrm: mod=00, reg=0, rm=110=disp16) then 0x00 0x30
    let not_present_sel: u16 = 0x30;
    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0xC4; // LES
    mem.ram[0x2001] = 0x06; // mod=00, reg=AX, rm=110 (disp16)
    mem.ram[0x2002] = 0x00; // disp16 low
    mem.ram[0x2003] = 0x30; // disp16 high → address 0x3000
                            // Data at [0x3000]: offset=0x1234, segment=0x0030 (not-present)
    mem.ram[0x3000] = 0x34;
    mem.ram[0x3001] = 0x12;
    mem.ram[0x3002] = not_present_sel as u8;
    mem.ram[0x3003] = (not_present_sel >> 8) as u8;

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 11,
            error_code,
            ..
        } => {
            assert_eq!(
                error_code,
                (not_present_sel & 0xFFFC) as u32,
                "#NP error code must contain selector"
            );
        }
        other => panic!(
            "expected #NP (vector 11) with selector error code, got {:?}",
            other
        ),
    }
    // ES must be unchanged
    assert_eq!(cpu.regs.es, 0x10, "ES must not be modified on #NP fault");
    assert_eq!(
        cpu.regs.eax, 0xCAFE_BABE,
        "AX must not be modified on LES descriptor-load fault"
    );
}

// Test 4: PM MOV FS with null selector → should succeed (null is allowed for FS/GS)
#[test]
fn test_m3_mov_fs_null_selector_succeeds() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    setup_pm_gdt_extended(&mut cpu, &mut mem);

    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x08).unwrap();
    cpu.cs_bits = 16;
    cpu.regs.cpl = 0;
    cpu.regs.ss = 0x10;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.fs = 0x10;
    cpu.regs.fs_cache = cpu.read_descriptor(&mem, 0x10).unwrap();

    // MOV FS, AX — 0x8E 0xE0 (seg_idx=4, rm=0=AX), AX=0
    cpu.regs.set_ax(0);
    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0x8E;
    mem.ram[0x2001] = 0xE0; // MOV FS, AX

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "MOV FS with null selector should succeed, got {:?}",
        result
    );
    assert_eq!(cpu.regs.fs, 0, "FS should be 0 after loading null selector");
}

// Test 5: PM MOV DS with valid descriptor → should succeed (regression)
#[test]
fn test_m3_mov_ds_valid_descriptor_succeeds() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    setup_pm_gdt_extended(&mut cpu, &mut mem);

    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x08).unwrap();
    cpu.cs_bits = 16;
    cpu.regs.cpl = 0;
    cpu.regs.ss = 0x10;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.ds = 0x10;
    cpu.regs.ds_cache = cpu.read_descriptor(&mem, 0x10).unwrap();

    // MOV DS, AX — 0x8E 0xD8 (seg_idx=3, rm=0=AX), AX=0x10 (ring-0 data, valid)
    cpu.regs.set_ax(0x10);
    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0x8E;
    mem.ram[0x2001] = 0xD8; // MOV DS, AX

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "MOV DS with valid selector should succeed, got {:?}",
        result
    );
    assert_eq!(cpu.regs.ds, 0x10, "DS should be loaded with the selector");
}

// Test 6: Real-mode MOV DS → should succeed unconditionally (regression)
#[test]
fn test_m3_real_mode_mov_ds_succeeds() {
    // MOV DS, AX in real mode — 0x8E 0xD8
    let (mut cpu, mut mem) = setup(&[0x8E, 0xD8]);
    let mut io = ();
    cpu.regs.set_ax(0x1234);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Real-mode MOV DS should succeed, got {:?}",
        result
    );
    assert_eq!(cpu.regs.ds, 0x1234, "DS should be set in real mode");
}

/// Test that MOV DS in real mode preserves the segment cache limit
/// (386 "unreal mode" / "big real mode" behavior). On the 386, loading
/// a segment register in real mode only updates the base address; the
/// limit, access rights, and other hidden fields are preserved from the
/// last protected-mode load.
#[test]
fn test_unreal_mode_ds_limit_preserved_on_real_mode_reload() {
    // MOV DS, AX in real mode — 0x8E 0xD8
    let (mut cpu, mut mem) = setup(&[0x8E, 0xD8]);
    let mut io = ();

    // Simulate having loaded a flat descriptor in PM: base=0, limit=4GB
    cpu.regs.ds_cache.base = 0;
    cpu.regs.ds_cache.limit = 0xFFFFF; // raw limit field
    cpu.regs.ds_cache.granularity = true; // G=1 → effective limit = 4GB
    cpu.regs.ds_cache.recompute_effective_limit();
    cpu.regs.ds_cache.present = true;
    cpu.regs.ds_cache.is_writable = true;
    cpu.regs.ds_cache.big = true;

    // Now reload DS with a real-mode segment value
    cpu.regs.set_ax(0x2000);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Real-mode MOV DS should succeed, got {:?}",
        result
    );

    // DS visible selector should be updated
    assert_eq!(cpu.regs.ds, 0x2000);
    // Base should be selector × 16
    assert_eq!(cpu.regs.ds_cache.base, 0x20000);
    // Limit should be preserved from the PM cache (4GB), not reset to 64KB.
    assert_eq!(
        cpu.regs.ds_cache.effective_limit(),
        0xFFFFFFFF,
        "DS limit should be preserved (unreal mode), not reset to 64KB"
    );
    assert!(
        cpu.regs.ds_cache.granularity,
        "Granularity bit should be preserved"
    );
    assert!(cpu.regs.ds_cache.big, "D/B bit should be preserved");
    assert!(cpu.regs.ds_cache.present, "Present bit should be preserved");
}

/// Test that MOV SS in real mode preserves unreal-mode cache fields and shadows interrupts.
#[test]
fn test_unreal_mode_ss_limit_preserved_on_real_mode_reload() {
    // MOV SS, AX — 0x8E 0xD0
    let (mut cpu, mut mem) = setup(&[0x8E, 0xD0]);
    let mut io = ();

    cpu.regs.ss_cache.base = 0;
    cpu.regs.ss_cache.limit = 0xFFFFF;
    cpu.regs.ss_cache.granularity = true;
    cpu.regs.ss_cache.recompute_effective_limit();
    cpu.regs.ss_cache.present = true;
    cpu.regs.ss_cache.is_writable = true;
    cpu.regs.ss_cache.big = true;
    cpu.interrupt_inhibit = 0;

    cpu.regs.set_ax(0x2800);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(
        result,
        RunResult::InterruptPollBoundary { executed: 1 }
    ));

    assert_eq!(cpu.regs.ss, 0x2800);
    assert_eq!(cpu.regs.ss_cache.base, 0x28000);
    assert_eq!(
        cpu.regs.ss_cache.effective_limit(),
        0xFFFFFFFF,
        "SS limit should be preserved (unreal mode)"
    );
    assert!(cpu.regs.ss_cache.granularity);
    assert!(cpu.regs.ss_cache.big);
    assert_eq!(cpu.interrupt_inhibit, 1);
}

/// Test that MOV ES in real mode also preserves the segment cache limit.
#[test]
fn test_unreal_mode_es_limit_preserved_on_real_mode_reload() {
    // MOV ES, AX — 0x8E 0xC0
    let (mut cpu, mut mem) = setup(&[0x8E, 0xC0]);
    let mut io = ();

    // Simulate flat descriptor loaded in PM
    cpu.regs.es_cache.base = 0;
    cpu.regs.es_cache.limit = 0xFFFFF;
    cpu.regs.es_cache.granularity = true;
    cpu.regs.es_cache.recompute_effective_limit();
    cpu.regs.es_cache.present = true;
    cpu.regs.es_cache.big = true;

    cpu.regs.set_ax(0x3000);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));

    assert_eq!(cpu.regs.es, 0x3000);
    assert_eq!(cpu.regs.es_cache.base, 0x30000);
    assert_eq!(
        cpu.regs.es_cache.effective_limit(),
        0xFFFFFFFF,
        "ES limit should be preserved (unreal mode)"
    );
    assert!(cpu.regs.es_cache.granularity);
    assert!(cpu.regs.es_cache.big);
}

/// Test that default real-mode segment limit (64KB) remains 64KB when
/// no PM transition has occurred.
#[test]
fn test_real_mode_default_limit_preserved() {
    // MOV DS, AX — 0x8E 0xD8
    let (mut cpu, mut mem) = setup(&[0x8E, 0xD8]);
    let mut io = ();

    // Default real-mode cache: limit=0xFFFF, present=true
    assert_eq!(cpu.regs.ds_cache.effective_limit(), 0xFFFF);

    cpu.regs.set_ax(0x4000);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));

    assert_eq!(cpu.regs.ds, 0x4000);
    assert_eq!(cpu.regs.ds_cache.base, 0x40000);
    // Preserving a normal real-mode cache leaves the same 64KB limit.
    assert_eq!(cpu.regs.ds_cache.effective_limit(), 0xFFFF);
}

/// Test that POP DS in real mode preserves unreal mode limits.
/// This is critical because POP DS uses the same segment-loading path
/// as MOV DS; both must preserve the cached limit from protected mode.
#[test]
fn test_unreal_mode_pop_ds_preserves_limit() {
    // POP DS — 0x1F
    let (mut cpu, mut mem) = setup(&[0x1F]);
    let mut io = ();

    // Simulate flat descriptor loaded in PM: base=0, limit=4GB
    cpu.regs.ds_cache.base = 0;
    cpu.regs.ds_cache.limit = 0xFFFFF;
    cpu.regs.ds_cache.granularity = true;
    cpu.regs.ds_cache.recompute_effective_limit();
    cpu.regs.ds_cache.present = true;
    cpu.regs.ds_cache.is_writable = true;

    // Push 0x2000 onto the stack for POP DS to consume
    let sp = cpu.regs.sp().wrapping_sub(2);
    cpu.regs.set_sp(sp);
    cpu.regs.esp = sp as u32;
    let ss_base = cpu.regs.ss_cache.base;
    mem.write_u16(ss_base as usize + sp as usize, 0x2000);

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "POP DS should succeed in real mode, got {:?}",
        result
    );

    assert_eq!(cpu.regs.ds, 0x2000);
    assert_eq!(cpu.regs.ds_cache.base, 0x20000);
    assert_eq!(
        cpu.regs.ds_cache.effective_limit(),
        0xFFFFFFFF,
        "POP DS in real mode must preserve unreal mode 4GB limit"
    );
}

/// Test that POP ES in real mode preserves unreal mode limits.
#[test]
fn test_unreal_mode_pop_es_preserves_limit() {
    // POP ES — 0x07
    let (mut cpu, mut mem) = setup(&[0x07]);
    let mut io = ();

    // Simulate flat descriptor loaded in PM
    cpu.regs.es_cache.base = 0;
    cpu.regs.es_cache.limit = 0xFFFFF;
    cpu.regs.es_cache.granularity = true;
    cpu.regs.es_cache.recompute_effective_limit();
    cpu.regs.es_cache.present = true;
    cpu.regs.es_cache.is_writable = true;

    // Push 0x3000 onto the stack for POP ES to consume
    let sp = cpu.regs.sp().wrapping_sub(2);
    cpu.regs.set_sp(sp);
    cpu.regs.esp = sp as u32;
    let ss_base = cpu.regs.ss_cache.base;
    mem.write_u16(ss_base as usize + sp as usize, 0x3000);

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "POP ES should succeed in real mode, got {:?}",
        result
    );

    assert_eq!(cpu.regs.es, 0x3000);
    assert_eq!(cpu.regs.es_cache.base, 0x30000);
    assert_eq!(
        cpu.regs.es_cache.effective_limit(),
        0xFFFFFFFF,
        "POP ES in real mode must preserve unreal mode 4GB limit"
    );
}

/// Test that MOV CR0 (clearing PE bit) preserves all segment descriptor caches.
/// This is the core of unreal mode: enter PM, set up flat descriptors,
/// clear PE bit, and the cached 4GB limits survive.
#[test]
fn test_unreal_mode_cr0_clear_pe_preserves_caches() {
    // MOV CR0, EAX — 0x0F 0x22 0xC0
    let (mut cpu, mut mem) = setup(&[0x0F, 0x22, 0xC0]);
    let mut io = ();

    // Start in protected mode with flat descriptors
    cpu.regs.cr0 |= 1; // PE=1
    cpu.regs.cpl = 0;

    // Set up flat 4GB descriptors on DS/ES
    cpu.regs.ds_cache.base = 0;
    cpu.regs.ds_cache.limit = 0xFFFFF;
    cpu.regs.ds_cache.granularity = true;
    cpu.regs.ds_cache.recompute_effective_limit();
    cpu.regs.ds_cache.present = true;
    cpu.regs.ds_cache.is_writable = true;

    cpu.regs.es_cache.base = 0;
    cpu.regs.es_cache.limit = 0xFFFFF;
    cpu.regs.es_cache.granularity = true;
    cpu.regs.es_cache.recompute_effective_limit();
    cpu.regs.es_cache.present = true;

    // EAX = CR0 with PE cleared (exit PM)
    cpu.regs.eax = cpu.regs.cr0 & !1;

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    // MOV CR0 returns ModeChange when PE changes
    assert!(
        matches!(result, RunResult::ModeChange { .. }),
        "MOV CR0 clearing PE should return ModeChange, got {:?}",
        result
    );

    // CPU should now be in real mode
    assert_eq!(cpu.regs.cr0 & 1, 0, "PE should be cleared");

    // Segment caches must be PRESERVED (Intel 386 manual behavior)
    assert_eq!(
        cpu.regs.ds_cache.effective_limit(),
        0xFFFFFFFF,
        "DS 4GB limit must survive PE bit clear"
    );
    assert_eq!(
        cpu.regs.es_cache.effective_limit(),
        0xFFFFFFFF,
        "ES 4GB limit must survive PE bit clear"
    );
}

// Test 7: CALL far with invalid CS → verify error code includes selector (M11)
#[test]
fn test_m11_call_far_invalid_cs_error_code() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    setup_pm_gdt_extended(&mut cpu, &mut mem);

    // Ring-3 setup
    cpu.regs.cs = 0x1B;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x1B).unwrap();
    cpu.cs_bits = 16;
    cpu.regs.cpl = 3;
    cpu.regs.ss = 0x23;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x23).unwrap();
    cpu.regs.ds = 0x23;
    cpu.regs.ds_cache = cpu.read_descriptor(&mem, 0x23).unwrap();
    cpu.regs.set_sp(0x5000);
    cpu.regs.esp = 0x5000;

    // CALL far 0x0028:0x1000 (system descriptor, invalid for CS)
    // 0x9A offset_lo offset_hi selector_lo selector_hi
    let bad_cs: u16 = 0x28; // system descriptor
    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0x9A; // CALL far ptr16:16
    mem.ram[0x2001] = 0x00;
    mem.ram[0x2002] = 0x10; // offset = 0x1000
    mem.ram[0x2003] = bad_cs as u8;
    mem.ram[0x2004] = (bad_cs >> 8) as u8;

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code,
            ..
        } => {
            assert_eq!(
                error_code,
                (bad_cs & 0xFFFC) as u32,
                "#GP error code must contain CS selector (M11)"
            );
        }
        other => panic!("expected #GP with selector error code, got {:?}", other),
    }
}

// Test 8: MOV SS error code includes selector (M11 fix)
#[test]
fn test_m11_mov_ss_error_code_includes_selector() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    setup_pm_gdt_extended(&mut cpu, &mut mem);

    // Ring-0 setup
    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x08).unwrap();
    cpu.cs_bits = 16;
    cpu.regs.cpl = 0;
    cpu.regs.ss = 0x10;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x10).unwrap();

    // MOV SS, AX — with AX = 0x08 (code segment, invalid for SS → StackFault)
    let bad_ss: u16 = 0x08;
    cpu.regs.set_ax(bad_ss);
    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0x8E;
    mem.ram[0x2001] = 0xD0; // MOV SS, AX (seg_idx=2, rm=0=AX)

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code,
            ..
        } => {
            assert_eq!(
                error_code,
                (bad_ss & 0xFFFC) as u32,
                "MOV SS #GP error code must contain selector (M11)"
            );
        }
        RunResult::Exception {
            vector: 12,
            error_code,
            ..
        } => {
            assert_eq!(
                error_code,
                (bad_ss & 0xFFFC) as u32,
                "MOV SS #SS error code must contain selector (M11)"
            );
        }
        other => panic!(
            "expected #GP or #SS with selector error code, got {:?}",
            other
        ),
    }
}

// Test 9: POP DS atomicity — SP restored on fault
#[test]
fn test_m3_pop_ds_sp_restored_on_fault() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    setup_pm_gdt_extended(&mut cpu, &mut mem);

    // Ring-3 setup
    cpu.regs.cs = 0x1B;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x1B).unwrap();
    cpu.cs_bits = 16;
    cpu.regs.cpl = 3;
    cpu.regs.ss = 0x23;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x23).unwrap();
    cpu.regs.ds = 0x23;
    cpu.regs.ds_cache = cpu.read_descriptor(&mem, 0x23).unwrap();

    // POP DS (0x1F) — push ring-0 selector on stack
    let ring0_sel: u16 = 0x10; // DPL=0 data
    let stack_top: u16 = 0x5000;
    cpu.regs.set_sp(stack_top);
    cpu.regs.esp = stack_top as u32;
    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0x1F; // POP DS
    mem.ram[stack_top as usize..stack_top as usize + 2].copy_from_slice(&ring0_sel.to_le_bytes());

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception { vector: 13, .. } => {}
        other => panic!("expected #GP, got {:?}", other),
    }
    assert_eq!(
        cpu.regs.sp(),
        stack_top,
        "SP must be restored on POP DS fault"
    );
    assert_eq!(cpu.regs.ds, 0x23, "DS must be unchanged on fault");
}

// Test 10: LFS with valid selector succeeds
#[test]
fn test_m3_lfs_valid_selector_succeeds() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    setup_pm_gdt_extended(&mut cpu, &mut mem);

    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x08).unwrap();
    cpu.cs_bits = 16;
    cpu.regs.cpl = 0;
    cpu.regs.ss = 0x10;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.ds = 0x10;
    cpu.regs.ds_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.fs = 0x10;
    cpu.regs.fs_cache = cpu.read_descriptor(&mem, 0x10).unwrap();

    // LFS AX, [0x3000] — 0x0F 0xB4 modrm=0x06 disp16=0x3000
    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0x0F;
    mem.ram[0x2001] = 0xB4;
    mem.ram[0x2002] = 0x06; // mod=00, reg=AX, rm=110 (disp16)
    mem.ram[0x2003] = 0x00;
    mem.ram[0x2004] = 0x30; // disp16 = 0x3000
                            // Data at [0x3000]: offset=0x5678, segment=0x10 (ring-0 data, valid)
    mem.ram[0x3000] = 0x78;
    mem.ram[0x3001] = 0x56;
    mem.ram[0x3002] = 0x10;
    mem.ram[0x3003] = 0x00;

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "LFS with valid selector should succeed, got {:?}",
        result
    );
    assert_eq!(cpu.regs.fs, 0x10, "FS should be loaded");
    assert_eq!(
        cpu.regs.eax as u16, 0x5678,
        "AX should be loaded with offset"
    );
}

// Test 11: LGS with invalid selector → #GP with error code
#[test]
fn test_m3_lgs_invalid_selector_gp_error_code() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();
    setup_pm_gdt_extended(&mut cpu, &mut mem);

    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x08).unwrap();
    cpu.cs_bits = 16;
    cpu.regs.cpl = 0;
    cpu.regs.ss = 0x10;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.ds = 0x10;
    cpu.regs.ds_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.gs = 0x10;
    cpu.regs.gs_cache = cpu.read_descriptor(&mem, 0x10).unwrap();

    // LGS AX, [0x3000] — 0x0F 0xB5 modrm=0x06 disp16=0x3000
    let bad_sel: u16 = 0x28; // system descriptor
    cpu.regs.set_ip(0x2000);
    cpu.regs.eip = 0x2000;
    mem.ram[0x2000] = 0x0F;
    mem.ram[0x2001] = 0xB5;
    mem.ram[0x2002] = 0x06;
    mem.ram[0x2003] = 0x00;
    mem.ram[0x2004] = 0x30;
    mem.ram[0x3000] = 0x78;
    mem.ram[0x3001] = 0x56;
    mem.ram[0x3002] = bad_sel as u8;
    mem.ram[0x3003] = (bad_sel >> 8) as u8;

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code,
            ..
        } => {
            assert_eq!(
                error_code,
                (bad_sel & 0xFFFC) as u32,
                "LGS #GP error code must contain selector"
            );
        }
        other => panic!("expected #GP with selector error code, got {:?}", other),
    }
}
