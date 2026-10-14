use super::test_helpers::{make_descriptor, setup, write_gdt_entry};
use super::*;
use gp64_cpu::{Cpu, CpuType, Flags};
use gp64_memory::Memory;

// ========================================================================
// Helper: setup protected-mode 32-bit (same pattern as coverage_batch4)
// ========================================================================
fn setup_pm32(code: &[u8]) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let gdt_base: u32 = 0x10000;
    let gdt_limit: u16 = 8 * 8 - 1;
    write_gdt_entry(&mut mem, gdt_base as usize, [0u8; 8]);
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x08,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x10,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x18,
        make_descriptor(0, 0xFFFFF, 0xFA, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x20,
        make_descriptor(0, 0xFFFFF, 0xF2, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x28,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x08),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x30,
        make_descriptor(0x1000, 0xFFFFF, 0x92, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x38,
        make_descriptor(0x2000, 0xFFFFF, 0x92, 0x0C),
    );
    cpu.regs.gdtr.base = gdt_base;
    cpu.regs.gdtr.limit = gdt_limit;
    cpu.regs.cr0 |= 1;
    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x08).unwrap();
    cpu.cs_bits = 32;
    cpu.regs.cpl = 0;
    cpu.regs.ss = 0x10;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.ds = 0x10;
    cpu.regs.ds_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.es = 0x10;
    cpu.regs.es_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.fs = 0x30;
    cpu.regs.fs_cache = cpu.read_descriptor(&mem, 0x30).unwrap();
    cpu.regs.gs = 0x38;
    cpu.regs.gs_cache = cpu.read_descriptor(&mem, 0x38).unwrap();
    cpu.regs.set_eip(0x0100);
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }
    (cpu, mem)
}

// Helper: setup paging (identity map first 1MB)
fn setup_paging(code: &[u8]) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cr0 = 0x8000_0001; // PE + PG
    cpu.regs.cpl = 0;
    cpu.regs.cr3 = 0x10000;
    mem.write_u32_physical(0x10000, 0x11007);
    for page in 0..256u32 {
        let pte_addr = 0x11000 + (page as usize) * 4;
        mem.write_u32_physical(pte_addr, (page << 12) | 0x07);
    }
    cpu.regs.cs = 0x0008;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.cs_cache.limit = 0xFFFFF;
    cpu.regs.cs_cache.granularity = true;
    cpu.regs.cs_cache.recompute_effective_limit();
    cpu.regs.cs_cache.access = 0x9B;
    cpu.regs.cs_cache.is_code = true;
    cpu.regs.cs_cache.big = true;
    cpu.regs.cs_cache.present = true;
    cpu.cs_bits = 32;
    cpu.regs.ds = 0x0010;
    cpu.regs.ds_cache.base = 0;
    cpu.regs.ds_cache.limit = 0xFFFFF;
    cpu.regs.ds_cache.granularity = true;
    cpu.regs.ds_cache.recompute_effective_limit();
    cpu.regs.ds_cache.access = 0x93;
    cpu.regs.ds_cache.is_code = false;
    cpu.regs.ds_cache.big = true;
    cpu.regs.ds_cache.present = true;
    cpu.regs.ss = 0x0018;
    cpu.regs.ss_cache.base = 0;
    cpu.regs.ss_cache.limit = 0xFFFFF;
    cpu.regs.ss_cache.granularity = true;
    cpu.regs.ss_cache.recompute_effective_limit();
    cpu.regs.ss_cache.access = 0x93;
    cpu.regs.ss_cache.is_code = false;
    cpu.regs.ss_cache.big = true;
    cpu.regs.ss_cache.present = true;
    cpu.regs.es = 0x0020;
    cpu.regs.es_cache.base = 0;
    cpu.regs.es_cache.limit = 0xFFFFF;
    cpu.regs.es_cache.granularity = true;
    cpu.regs.es_cache.recompute_effective_limit();
    cpu.regs.es_cache.access = 0x93;
    cpu.regs.es_cache.is_code = false;
    cpu.regs.es_cache.big = true;
    cpu.regs.es_cache.present = true;
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }
    (cpu, mem)
}

// Helper: setup Pentium MMX (P55C) in real mode.
fn setup_pentium(code: &[u8]) -> (Cpu, Memory) {
    let (mut cpu, mem) = setup(code);
    cpu.cpu_type = CpuType::PentiumMmx;
    (cpu, mem)
}

// Helper: setup 486-class CPU in PM32 (for BSWAP, CMPXCHG, XADD)
fn setup_486_pm32(code: &[u8]) -> (Cpu, Memory) {
    let (mut cpu, mem) = setup_pm32(code);
    cpu.cpu_type = CpuType::I80486;
    (cpu, mem)
}

// Helper: setup Pentium MMX (P55C) in PM32.
fn setup_pentium_pm32(code: &[u8]) -> (Cpu, Memory) {
    let (mut cpu, mem) = setup_pm32(code);
    cpu.cpu_type = CpuType::PentiumMmx;
    (cpu, mem)
}

// Helper: setup Pentium MMX (P55C) paging.
fn setup_pentium_paging(code: &[u8]) -> (Cpu, Memory) {
    let (mut cpu, mem) = setup_paging(code);
    cpu.cpu_type = CpuType::PentiumMmx;
    (cpu, mem)
}

// Helper: setup 486 paging
fn setup_486_paging(code: &[u8]) -> (Cpu, Memory) {
    let (mut cpu, mem) = setup_paging(code);
    cpu.cpu_type = CpuType::I80486;
    (cpu, mem)
}

/// MDD-BUG-CRUCIBLE-00331: a cold destination TLB entry must not disable
/// the paged REP bulk route for the whole remaining slice.
///
/// `setup_paging` builds a fresh `Memory`, so the hashed TLB starts EMPTY
/// and the fill's first bulk lookup necessarily misses. Before the fix that
/// miss `break`s the bulk loop, handing the entire REP to the per-element
/// path — so ZERO bulk chunks commit. After it, the miss warms the entry
/// through `translate_slow` and the chunk is retried, and the fill crosses
/// into a second page, so at least two chunks commit.
///
/// The chunk count is the ONLY observable difference: both routes leave
/// identical guest memory. The fill and CX are asserted too, so a change
/// that bumped the counter while corrupting the fill cannot pass.
#[test]
fn paged_rep_stosb_resumes_bulk_after_a_cold_tlb_miss() {
    let code = [0xF3, 0xAA]; // REP STOSB
    let (mut cpu, mut mem) = setup_paging(&code);

    // Page-aligned, spanning two pages, inside the identity-mapped 1 MB
    // and well below VIDEO_BASE so no VGA bail applies.
    let dst = 0x3000usize;
    let count = 0x1800u32;
    cpu.regs.edi = dst as u32;
    cpu.regs.ecx = count;
    cpu.regs.eax = (cpu.regs.eax & !0xFF) | 0xA5;

    paged_rep_bulk_chunks_reset();
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 100_000);

    let chunks = paged_rep_bulk_chunks();
    assert!(
        chunks >= 2,
        "cold-start REP STOSB committed {chunks} bulk chunks; a cold TLB \
         miss disabled the bulk route for the whole slice \
         (MDD-BUG-CRUCIBLE-00331)"
    );

    assert!(
        mem.ram[dst..dst + count as usize]
            .iter()
            .all(|&b| b == 0xA5),
        "REP STOSB must still fill the whole range exactly"
    );
    assert_eq!(cpu.regs.ecx, 0, "REP must run CX to zero");
}

// ========================================================================
// 1. FPU instructions — FADD, FSUB, FMUL, FDIV, FCOM, FCHS, FABS
// ========================================================================

#[test]
fn test_fpu_fadd_st0_st1() {
    // FLD dword [0x200], FLD dword [0x204], FADD ST(0),ST(1), FSTP dword [0x210]
    // D9 06 00 02 = FLD [0x200]
    // D9 06 04 02 = FLD [0x204]
    // D8 C1       = FADD ST(0), ST(1)
    // D9 1E 10 02 = FSTP [0x210]
    let code = [
        0xD9, 0x06, 0x00, 0x02, // FLD dword [0x200]
        0xD9, 0x06, 0x04, 0x02, // FLD dword [0x204]
        0xD8, 0xC1, // FADD ST(0), ST(1)
        0xD9, 0x1E, 0x10, 0x02, // FSTP dword [0x210]
    ];
    let (mut cpu, mut mem) = setup(&code);
    let a: f32 = 2.5;
    let b: f32 = 3.5;
    mem.ram[0x200..0x204].copy_from_slice(&a.to_le_bytes());
    mem.ram[0x204..0x208].copy_from_slice(&b.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 4);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    assert!((result - 6.0).abs() < 0.001);
}

#[test]
fn test_fpu_fsub_st0_st1() {
    // FLD [0x200], FLD [0x204], FSUB ST(0),ST(1) = D8 E1, FSTP [0x210]
    let code = [
        0xD9, 0x06, 0x00, 0x02, 0xD9, 0x06, 0x04, 0x02, 0xD8, 0xE1, 0xD9, 0x1E, 0x10, 0x02,
    ];
    let (mut cpu, mut mem) = setup(&code);
    let a: f32 = 2.0;
    let b: f32 = 7.0;
    mem.ram[0x200..0x204].copy_from_slice(&a.to_le_bytes());
    mem.ram[0x204..0x208].copy_from_slice(&b.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 4);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    assert!((result - 5.0).abs() < 0.001); // 7.0 - 2.0 = 5.0
}

#[test]
fn test_fpu_fmul_st0_st1() {
    // FLD [0x200], FLD [0x204], FMUL ST(0),ST(1) = D8 C9, FSTP [0x210]
    let code = [
        0xD9, 0x06, 0x00, 0x02, 0xD9, 0x06, 0x04, 0x02, 0xD8, 0xC9, 0xD9, 0x1E, 0x10, 0x02,
    ];
    let (mut cpu, mut mem) = setup(&code);
    let a: f32 = 3.0;
    let b: f32 = 4.0;
    mem.ram[0x200..0x204].copy_from_slice(&a.to_le_bytes());
    mem.ram[0x204..0x208].copy_from_slice(&b.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 4);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    assert!((result - 12.0).abs() < 0.001);
}

#[test]
fn test_fpu_fdiv_st0_st1() {
    // FLD [0x200], FLD [0x204], FDIV ST(0),ST(1) = D8 F1, FSTP [0x210]
    let code = [
        0xD9, 0x06, 0x00, 0x02, 0xD9, 0x06, 0x04, 0x02, 0xD8, 0xF1, 0xD9, 0x1E, 0x10, 0x02,
    ];
    let (mut cpu, mut mem) = setup(&code);
    let a: f32 = 2.0;
    let b: f32 = 10.0;
    mem.ram[0x200..0x204].copy_from_slice(&a.to_le_bytes());
    mem.ram[0x204..0x208].copy_from_slice(&b.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 4);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    assert!((result - 5.0).abs() < 0.001); // 10.0 / 2.0 = 5.0
}

#[test]
fn test_fpu_fchs() {
    // FLD [0x200], FCHS = D9 E0, FSTP [0x210]
    let code = [
        0xD9, 0x06, 0x00, 0x02, // FLD [0x200]
        0xD9, 0xE0, // FCHS
        0xD9, 0x1E, 0x10, 0x02, // FSTP [0x210]
    ];
    let (mut cpu, mut mem) = setup(&code);
    let a: f32 = 42.0;
    mem.ram[0x200..0x204].copy_from_slice(&a.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    assert!((result + 42.0).abs() < 0.001);
}

#[test]
fn test_fpu_fabs() {
    // FLD [0x200], FABS = D9 E1, FSTP [0x210]
    let code = [0xD9, 0x06, 0x00, 0x02, 0xD9, 0xE1, 0xD9, 0x1E, 0x10, 0x02];
    let (mut cpu, mut mem) = setup(&code);
    let a: f32 = -99.5;
    mem.ram[0x200..0x204].copy_from_slice(&a.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    assert!((result - 99.5).abs() < 0.001);
}

/// FCHS and FABS set C1 to 0 (Intel SDM), so a C1 left by an earlier
/// instruction must not survive them (MDD-BUG-FLU-01967).
#[test]
fn test_fpu_fchs_and_fabs_clear_c1() {
    for (name, op) in [("FCHS", 0xE0u8), ("FABS", 0xE1)] {
        // FLD [0x200], then the operation
        let code = [0xD9, 0x06, 0x00, 0x02, 0xD9, op];
        let (mut cpu, mut mem) = setup(&code);
        mem.ram[0x200..0x204].copy_from_slice(&(-3.5f32).to_le_bytes());
        let mut io = ();
        let _ = run(&mut cpu, &mut mem, &mut io, 1);
        cpu.fpu.status_word |= 0x0200; // stale C1, e.g. an inexact round-up
        let _ = run(&mut cpu, &mut mem, &mut io, 1);
        assert_eq!(cpu.fpu.status_word & 0x0200, 0, "{name} must clear C1");
    }
}

/// FSIN and FCOS tag the result, not the operand: FCOS(+0) = 1.0 must
/// read back tagged Valid (00), not the operand's Zero (MDD-BUG-FLU-01966).
#[test]
fn test_fpu_fsin_fcos_retag_the_result() {
    for (name, op, tag) in [("FCOS", 0xFFu8, 0b00u16), ("FSIN", 0xFE, 0b01)] {
        // FLDZ, then the operation
        let code = [0xD9, 0xEE, 0xD9, op];
        let (mut cpu, mut mem) = setup(&code);
        let mut io = ();
        let _ = run(&mut cpu, &mut mem, &mut io, 2);
        let top = cpu.fpu.get_top();
        assert_eq!(
            (cpu.fpu.tag_word >> (2 * top)) & 0b11,
            tag,
            "{name}(+0) tags its result"
        );
    }
}

#[test]
fn test_fpu_fsqrt() {
    // FLD [0x200], FSQRT = D9 FA, FSTP [0x210]
    let code = [0xD9, 0x06, 0x00, 0x02, 0xD9, 0xFA, 0xD9, 0x1E, 0x10, 0x02];
    let (mut cpu, mut mem) = setup(&code);
    let a: f32 = 25.0;
    mem.ram[0x200..0x204].copy_from_slice(&a.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    assert!((result - 5.0).abs() < 0.01);
}

#[test]
fn test_fpu_fild_fistp_16bit() {
    // FILD word [0x200] = DF 06 00 02
    // FISTP word [0x210] = DF 1E 10 02
    let code = [0xDF, 0x06, 0x00, 0x02, 0xDF, 0x1E, 0x10, 0x02];
    let (mut cpu, mut mem) = setup(&code);
    let val: i16 = -42;
    mem.ram[0x200..0x202].copy_from_slice(&val.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    let result = i16::from_le_bytes(mem.ram[0x210..0x212].try_into().unwrap());
    assert_eq!(result, -42);
}

#[test]
fn test_fpu_fild_fistp_32bit() {
    // FILD dword [0x200] = DB 06 00 02
    // FISTP dword [0x210] = DB 1E 10 02
    let code = [0xDB, 0x06, 0x00, 0x02, 0xDB, 0x1E, 0x10, 0x02];
    let (mut cpu, mut mem) = setup(&code);
    let val: i32 = 123456;
    mem.ram[0x200..0x204].copy_from_slice(&val.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    let result = i32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    assert_eq!(result, 123456);
}

#[test]
fn test_fpu_fcom_st1() {
    // FLD [0x200], FLD [0x204], FCOM ST(1) = D8 D1, then FNSTSW AX = DF E0
    let code = [
        0xD9, 0x06, 0x00, 0x02, // FLD [0x200] -> ST(1)
        0xD9, 0x06, 0x04, 0x02, // FLD [0x204] -> ST(0)
        0xD8, 0xD1, // FCOM ST(1)
        0xDF, 0xE0, // FNSTSW AX
    ];
    let (mut cpu, mut mem) = setup(&code);
    let a: f32 = 1.0; // ST(1) = 1.0
    let b: f32 = 2.0; // ST(0) = 2.0
    mem.ram[0x200..0x204].copy_from_slice(&a.to_le_bytes());
    mem.ram[0x204..0x208].copy_from_slice(&b.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 4);
    // ST(0) > ST(1): C0=0, C3=0
    let sw = cpu.regs.eax as u16;
    assert_eq!(sw & 0x4500, 0x0000); // ST(0) > ST(1)
}

#[test]
fn test_fpu_fldz_fld1() {
    // FLDZ = D9 EE, FLD1 = D9 E8, FADD = D8 C1, FSTP [0x210]
    let code = [
        0xD9, 0xEE, // FLDZ -> push 0.0
        0xD9, 0xE8, // FLD1 -> push 1.0
        0xD8, 0xC1, // FADD ST(0), ST(1)
        0xD9, 0x1E, 0x10, 0x02, // FSTP [0x210]
    ];
    let (mut cpu, mut mem) = setup(&code);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 4);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    assert!((result - 1.0).abs() < 0.001); // 0.0 + 1.0 = 1.0
}

#[test]
fn test_fpu_fldpi() {
    // FLDPI = D9 EB, FSTP [0x210]
    let code = [0xD9, 0xEB, 0xD9, 0x1E, 0x10, 0x02];
    let (mut cpu, mut mem) = setup(&code);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    assert!((result - std::f32::consts::PI).abs() < 0.001);
}

#[test]
fn test_fpu_fxch_st1() {
    // FLD [0x200], FLD [0x204], FXCH ST(1) = D9 C9, FSTP [0x210]
    let code = [
        0xD9, 0x06, 0x00, 0x02, 0xD9, 0x06, 0x04, 0x02, 0xD9, 0xC9, 0xD9, 0x1E, 0x10, 0x02,
    ];
    let (mut cpu, mut mem) = setup(&code);
    let a: f32 = 10.0;
    let b: f32 = 20.0;
    mem.ram[0x200..0x204].copy_from_slice(&a.to_le_bytes());
    mem.ram[0x204..0x208].copy_from_slice(&b.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 4);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    // After FXCH, ST(0) should be old ST(1)=10.0
    assert!((result - 10.0).abs() < 0.001);
}

#[test]
fn test_fpu_fnstcw_fldcw() {
    // FNINIT = DB E3, FNSTCW [0x200] = D9 3E 00 02, FLDCW [0x200] = D9 2E 00 02
    let code = [
        0xDB, 0xE3, // FNINIT
        0xD9, 0x3E, 0x00, 0x02, // FNSTCW [0x200]
        0xD9, 0x2E, 0x00, 0x02, // FLDCW [0x200]
    ];
    let (mut cpu, mut mem) = setup(&code);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    let cw = u16::from_le_bytes(mem.ram[0x200..0x202].try_into().unwrap());
    // Default CW after FNINIT: 0x037F or 0x033F (stack fault mask varies)
    assert_eq!(cw & 0x0F3F, 0x033F);
}

#[test]
fn test_fpu_frndint() {
    // FLD [0x200], FRNDINT = D9 FC, FSTP [0x210]
    let code = [0xD9, 0x06, 0x00, 0x02, 0xD9, 0xFC, 0xD9, 0x1E, 0x10, 0x02];
    let (mut cpu, mut mem) = setup(&code);
    let a: f32 = 3.7;
    mem.ram[0x200..0x204].copy_from_slice(&a.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    assert!((result - 4.0).abs() < 0.001); // default rounding = nearest
}

#[test]
fn test_fpu_faddp() {
    // FLD [0x200], FLD [0x204], FADDP ST(1),ST(0) = DE C1, FSTP [0x210]
    let code = [
        0xD9, 0x06, 0x00, 0x02, 0xD9, 0x06, 0x04, 0x02, 0xDE, 0xC1, 0xD9, 0x1E, 0x10, 0x02,
    ];
    let (mut cpu, mut mem) = setup(&code);
    let a: f32 = 100.0;
    let b: f32 = 200.0;
    mem.ram[0x200..0x204].copy_from_slice(&a.to_le_bytes());
    mem.ram[0x204..0x208].copy_from_slice(&b.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 4);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    assert!((result - 300.0).abs() < 0.01);
}

#[test]
fn test_fpu_fmulp() {
    // FLD [0x200], FLD [0x204], FMULP ST(1),ST(0) = DE C9, FSTP [0x210]
    let code = [
        0xD9, 0x06, 0x00, 0x02, 0xD9, 0x06, 0x04, 0x02, 0xDE, 0xC9, 0xD9, 0x1E, 0x10, 0x02,
    ];
    let (mut cpu, mut mem) = setup(&code);
    let a: f32 = 5.0;
    let b: f32 = 6.0;
    mem.ram[0x200..0x204].copy_from_slice(&a.to_le_bytes());
    mem.ram[0x204..0x208].copy_from_slice(&b.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 4);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    assert!((result - 30.0).abs() < 0.01);
}

#[test]
fn test_fpu_fsubp() {
    // FLD [0x200], FLD [0x204], FSUBP ST(1),ST(0) = DE E9, FSTP [0x210]
    let code = [
        0xD9, 0x06, 0x00, 0x02, 0xD9, 0x06, 0x04, 0x02, 0xDE, 0xE9, 0xD9, 0x1E, 0x10, 0x02,
    ];
    let (mut cpu, mut mem) = setup(&code);
    let a: f32 = 50.0;
    let b: f32 = 30.0;
    mem.ram[0x200..0x204].copy_from_slice(&a.to_le_bytes());
    mem.ram[0x204..0x208].copy_from_slice(&b.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 4);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    // FSUBP ST(1),ST(0) = ST(1) - ST(0) = 50.0 - 30.0 = 20.0
    assert!((result - 20.0).abs() < 0.01);
}

#[test]
fn test_fpu_fdivp() {
    // FLD [0x200], FLD [0x204], FDIVP ST(1),ST(0) = DE F9, FSTP [0x210]
    let code = [
        0xD9, 0x06, 0x00, 0x02, 0xD9, 0x06, 0x04, 0x02, 0xDE, 0xF9, 0xD9, 0x1E, 0x10, 0x02,
    ];
    let (mut cpu, mut mem) = setup(&code);
    let a: f32 = 100.0;
    let b: f32 = 4.0;
    mem.ram[0x200..0x204].copy_from_slice(&a.to_le_bytes());
    mem.ram[0x204..0x208].copy_from_slice(&b.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 4);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    // FDIVP ST(1),ST(0) = ST(1) / ST(0) = 100.0 / 4.0 = 25.0
    assert!((result - 25.0).abs() < 0.01);
}

#[test]
fn test_fpu_ftst() {
    // FLD [0x200], FTST = D9 E4, FNSTSW AX = DF E0
    let code = [
        0xD9, 0x06, 0x00, 0x02, // FLD [0x200]
        0xD9, 0xE4, // FTST
        0xDF, 0xE0, // FNSTSW AX
    ];
    let (mut cpu, mut mem) = setup(&code);
    let a: f32 = 0.0;
    mem.ram[0x200..0x204].copy_from_slice(&a.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    // 0.0 == 0: C3=1, C0=0, C2=0 -> SW & 0x4500 = 0x4000
    let sw = cpu.regs.eax as u16;
    assert_eq!(sw & 0x4500, 0x4000);
}

#[test]
fn test_fpu_fscale() {
    // FLD [0x200], FLD [0x204], FSCALE = D9 FD, FSTP [0x210]
    // FSCALE: ST(0) = ST(0) * 2^trunc(ST(1))
    let code = [
        0xD9, 0x06, 0x00, 0x02, // FLD [0x200] -> ST(1) = exponent
        0xD9, 0x06, 0x04, 0x02, // FLD [0x204] -> ST(0) = mantissa
        0xD9, 0xFD, // FSCALE
        0xD9, 0x1E, 0x10, 0x02, // FSTP [0x210]
    ];
    let (mut cpu, mut mem) = setup(&code);
    let exp: f32 = 3.0; // 2^3 = 8
    let mant: f32 = 1.5;
    mem.ram[0x200..0x204].copy_from_slice(&exp.to_le_bytes());
    mem.ram[0x204..0x208].copy_from_slice(&mant.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 4);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    assert!((result - 12.0).abs() < 0.01); // 1.5 * 8 = 12.0
}

#[test]
fn test_fpu_fld_double() {
    // FLD qword [0x200] = DD 06 00 02, FSTP qword [0x210] = DD 1E 10 02
    let code = [0xDD, 0x06, 0x00, 0x02, 0xDD, 0x1E, 0x10, 0x02];
    let (mut cpu, mut mem) = setup(&code);
    let val: f64 = 3.141592653589793;
    mem.ram[0x200..0x208].copy_from_slice(&val.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    let result = f64::from_le_bytes(mem.ram[0x210..0x218].try_into().unwrap());
    assert!((result - val).abs() < 1e-10);
}

#[test]
fn test_fpu_fiadd_word() {
    // FLD [0x200], FIADD word [0x204] = DE 06 04 02, FSTP [0x210]
    let code = [
        0xD9, 0x06, 0x00, 0x02, // FLD dword [0x200]
        0xDE, 0x06, 0x04, 0x02, // FIADD word [0x204]
        0xD9, 0x1E, 0x10, 0x02, // FSTP dword [0x210]
    ];
    let (mut cpu, mut mem) = setup(&code);
    let a: f32 = 10.0;
    let b: i16 = 5;
    mem.ram[0x200..0x204].copy_from_slice(&a.to_le_bytes());
    mem.ram[0x204..0x206].copy_from_slice(&b.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    assert!((result - 15.0).abs() < 0.01);
}

#[test]
fn test_fpu_fninit() {
    // FNINIT = DB E3
    let (mut cpu, mut mem) = setup(&[0xDB, 0xE3]);
    cpu.fpu.control_word = 0x0000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.fpu.control_word, 0x037F);
}

// ========================================================================
// 2. MMX instructions
// ========================================================================

// SSE/SSE2 smoke tests intentionally sit beside the MMX coverage because
// the mandatory-prefix encodings share much of the same 0F opcode space.
// A missing early SIMD route can otherwise execute an SSE2 instruction as
// its unprefixed MMX cousin and appear to retire successfully.
#[test]
fn mov_cr4_rejects_sse_control_bits_on_older_cpu() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x22, 0xE0]); // MOV CR4,EAX
    cpu.set_cpu_type(gp64_cpu::CpuType::PentiumII);
    cpu.regs.eax = gp64_cpu::CR4_OSFXSR;
    let old_cr4 = cpu.regs.cr4;

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        }
    ));
    assert_eq!(cpu.regs.cr4, old_cr4);
    assert_eq!(cpu.regs.ip(), 0x100);
}

/// MDD-BUG-KIL-00765: the page walk only knows two-level 32-bit tables,
/// so enabling PAE (or any other unimplemented mode) must fault rather
/// than translate through the wrong table format. PSE, which is
/// implemented, still loads.
#[test]
fn mov_cr4_rejects_unimplemented_modes_and_keeps_implemented_ones() {
    for bit in [
        gp64_cpu::CR4_PAE,
        gp64_cpu::CR4_PGE,
        gp64_cpu::CR4_VME,
        gp64_cpu::CR4_PVI,
    ] {
        let (mut cpu, mut mem) = setup(&[0x0F, 0x22, 0xE0]); // MOV CR4,EAX
        cpu.set_cpu_type(gp64_cpu::CpuType::Pentium4);
        cpu.regs.eax = bit | gp64_cpu::CR4_PSE;

        let result = run(&mut cpu, &mut mem, &mut (), 1);

        assert!(
            matches!(
                result,
                RunResult::Exception {
                    vector: 13,
                    error_code: 0,
                    ..
                }
            ),
            "CR4 bit {bit:#x}: {result:?}"
        );
        assert_eq!(cpu.regs.cr4, 0, "CR4 bit {bit:#x} must not load");
    }

    let (mut cpu, mut mem) = setup(&[0x0F, 0x22, 0xE0]);
    cpu.set_cpu_type(gp64_cpu::CpuType::Pentium4);
    cpu.regs.eax = gp64_cpu::CR4_PSE;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(!matches!(result, RunResult::Exception { .. }), "{result:?}");
    assert_eq!(cpu.regs.cr4, gp64_cpu::CR4_PSE);
}

#[test]
fn sse_addps_executes_on_pentium_iii() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x58, 0xC1]);
    cpu.set_cpu_type(gp64_cpu::CpuType::PentiumIII);
    cpu.regs.cr4 = gp64_cpu::CR4_OSFXSR;
    cpu.set_xmm_reg(
        0,
        u128::from_le_bytes([
            0, 0, 0x80, 0x3F, 0, 0, 0, 0x40, 0, 0, 0x40, 0x40, 0, 0, 0x80, 0x40,
        ]),
    );
    cpu.set_xmm_reg(
        1,
        u128::from_le_bytes([
            0, 0, 0, 0x40, 0, 0, 0x40, 0x40, 0, 0, 0x80, 0x40, 0, 0, 0xA0, 0x40,
        ]),
    );

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(!matches!(result, RunResult::Exception { .. }), "{result:?}");
    assert_eq!(
        cpu.xmm_reg(0),
        u128::from_le_bytes([
            0, 0, 0x40, 0x40, 0, 0, 0xA0, 0x40, 0, 0, 0xE0, 0x40, 0, 0, 0x10, 0x41,
        ])
    );
    assert_eq!(cpu.regs.eip, 0x0103);
}

#[test]
fn sse_scalar_add_preserves_upper_lanes() {
    let (mut cpu, mut mem) = setup(&[0xF3, 0x0F, 0x58, 0xC1]);
    cpu.set_cpu_type(gp64_cpu::CpuType::PentiumIII);
    cpu.regs.cr4 = gp64_cpu::CR4_OSFXSR;
    let upper = 0xA1A2_A3A4_B1B2_B3B4_C1C2_C3C4u128 << 32;
    cpu.set_xmm_reg(0, upper | u128::from(1.5f32.to_bits()));
    cpu.set_xmm_reg(1, u128::from(2.25f32.to_bits()));

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(!matches!(result, RunResult::Exception { .. }), "{result:?}");
    assert_eq!(cpu.xmm_reg(0) >> 32, upper >> 32);
    assert_eq!(cpu.xmm_reg(0) as u32, 3.75f32.to_bits());
}

#[test]
fn sse2_paddd_uses_xmm_not_the_mmx_alias() {
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xFE, 0xC1]);
    cpu.set_cpu_type(gp64_cpu::CpuType::Pentium4);
    cpu.regs.cr4 = gp64_cpu::CR4_OSFXSR;
    let words = |lanes: [u32; 4]| {
        u128::from_le_bytes(lanes.map(u32::to_le_bytes).concat().try_into().unwrap())
    };
    cpu.set_xmm_reg(0, words([u32::MAX, 20, 30, 40]));
    cpu.set_xmm_reg(1, words([2, 3, 4, 5]));
    let mmx_before = cpu.mmx_regs();

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(!matches!(result, RunResult::Exception { .. }), "{result:?}");
    assert_eq!(cpu.xmm_reg(0), words([1, 23, 34, 45]));
    assert_eq!(cpu.mmx_regs(), mmx_before, "SSE2 must not enter MMX state");
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn sse_pavgb_mmx_form_uses_x87_alias_registers() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0xE0, 0xC1]); // PAVGB MM0,MM1
    cpu.set_cpu_type(gp64_cpu::CpuType::PentiumIII);
    // MMX-register forms do not require the OS to enable XMM save/restore.
    cpu.regs.cr4 = 0;
    cpu.set_mmx_reg(0, 0x0010_20FE_4050_6070);
    cpu.set_mmx_reg(1, 0x010F_2101_3F51_6171);

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(!matches!(result, RunResult::Exception { .. }), "{result:?}");
    assert_eq!(cpu.mmx_reg(0), 0x0110_2180_4051_6171);
}

/// The unprefixed pre-filter must admit every form the SSE table owns, or
/// that form falls to the compact `0F` switch and raises #UD. PMINUB and
/// PMAXUB on MMX registers (`0F DA`, `0F DE`) were missing.
#[test]
fn legacy_simd_prefilter_admits_every_unprefixed_table_form() {
    for opcode in 0..=u8::MAX {
        for modrm in [0xC1u8, 0x06] {
            let mut bytes = [0u8; 15];
            bytes[..3].copy_from_slice(&[0x0F, opcode, modrm]);
            if let gp64_cpu::sse_decode::SseDecode::Decoded(_) =
                gp64_cpu::sse_decode::decode(&bytes, 32)
            {
                assert!(
                    crate::may_be_legacy_simd(opcode, false),
                    "0F {opcode:02X} {modrm:02X} is a table form the pre-filter drops"
                );
            }
        }
    }
}

#[test]
fn sse_pminub_and_pmaxub_mmx_forms_execute_on_pentium_iii() {
    for (opcode, expected) in [
        (0xDAu8, 0x000F_2001_3F50_6070),
        (0xDE, 0x0110_21FE_4051_6171),
    ] {
        let (mut cpu, mut mem) = setup(&[0x0F, opcode, 0xC1]); // PMINUB/PMAXUB MM0,MM1
        cpu.set_cpu_type(gp64_cpu::CpuType::PentiumIII);
        cpu.regs.cr4 = 0;
        cpu.set_mmx_reg(0, 0x0010_20FE_4050_6070);
        cpu.set_mmx_reg(1, 0x010F_2101_3F51_6171);

        let result = run(&mut cpu, &mut mem, &mut (), 1);

        assert!(
            !matches!(result, RunResult::Exception { .. }),
            "{opcode:02X}: {result:?}"
        );
        assert_eq!(cpu.mmx_reg(0), expected, "{opcode:02X}");
    }
}

#[test]
fn sse_mmx_forms_ignore_osfxsr_but_honor_em_and_ts() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0xE7, 0x06, 0x00, 0x02]); // MOVNTQ [200],MM0
    cpu.set_cpu_type(gp64_cpu::CpuType::PentiumIII);
    cpu.regs.cr4 = 0;
    cpu.set_mmx_reg(0, 0x0123_4567_89AB_CDEF);

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(!matches!(result, RunResult::Exception { .. }), "{result:?}");
    assert_eq!(
        u64::from_le_bytes(mem.ram[0x200..0x208].try_into().unwrap()),
        0x0123_4567_89AB_CDEF
    );

    for (name, cr0, vector) in [("EM", CR0_EM, 6), ("TS", CR0_TS, 7)] {
        let (mut cpu, mut mem) = setup(&[0x0F, 0xE0, 0xC1]); // PAVGB MM0,MM1
        cpu.set_cpu_type(gp64_cpu::CpuType::PentiumIII);
        cpu.regs.cr0 = cr0;
        cpu.regs.cr4 = 0;
        cpu.set_mmx_reg(0, 0x0010_20FE_4050_6070);
        cpu.set_mmx_reg(1, 0x010F_2101_3F51_6171);
        let before = cpu.mmx_reg(0);

        let result = run(&mut cpu, &mut mem, &mut (), 1);

        assert!(
            matches!(result, RunResult::Exception { vector: got, .. } if got == vector),
            "{name}: expected vector {vector}, got {result:?}"
        );
        assert_eq!(cpu.mmx_reg(0), before, "{name}: changed MMX state");
        assert_eq!(cpu.regs.eip, 0x0100, "{name}: fault did not restart");
    }
}

#[test]
fn sse_non_xmm_instructions_ignore_osfxsr() {
    let cases = [
        (
            "PREFETCHNTA",
            gp64_cpu::CpuType::PentiumIII,
            vec![0x0F, 0x18, 0x06, 0x00, 0x02],
        ),
        (
            "PREFETCHT0",
            gp64_cpu::CpuType::PentiumIII,
            vec![0x0F, 0x18, 0x0E, 0x00, 0x02],
        ),
        (
            "PREFETCHT1",
            gp64_cpu::CpuType::PentiumIII,
            vec![0x0F, 0x18, 0x16, 0x00, 0x02],
        ),
        (
            "PREFETCHT2",
            gp64_cpu::CpuType::PentiumIII,
            vec![0x0F, 0x18, 0x1E, 0x00, 0x02],
        ),
        (
            "LFENCE",
            gp64_cpu::CpuType::Pentium4,
            vec![0x0F, 0xAE, 0xE8],
        ),
        (
            "MFENCE",
            gp64_cpu::CpuType::Pentium4,
            vec![0x0F, 0xAE, 0xF0],
        ),
        (
            "SFENCE",
            gp64_cpu::CpuType::PentiumIII,
            vec![0x0F, 0xAE, 0xF8],
        ),
        (
            "MOVNTI",
            gp64_cpu::CpuType::Pentium4,
            vec![0x0F, 0xC3, 0x06, 0x00, 0x02],
        ),
    ];

    for (name, cpu_type, code) in cases {
        let (mut cpu, mut mem) = setup(&code);
        cpu.set_cpu_type(cpu_type);
        cpu.regs.cr4 = 0;
        cpu.regs.eax = 0x1234_5678;

        let result = run(&mut cpu, &mut mem, &mut (), 1);

        assert!(
            matches!(result, RunResult::BudgetExhausted { executed: 1 }),
            "{name} with OSFXSR clear should execute, got {result:?}"
        );
        assert_eq!(
            cpu.regs.eip,
            0x0100 + code.len() as u32,
            "{name}: instruction did not retire"
        );
    }
}

#[test]
fn sse2_maskmovdqu_uses_edi_and_preserves_unselected_bytes() {
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xF7, 0xC1]);
    cpu.set_cpu_type(gp64_cpu::CpuType::Pentium4);
    cpu.regs.cr4 = gp64_cpu::CR4_OSFXSR;
    cpu.regs.edi = 0x300;
    cpu.set_xmm_reg(
        0,
        u128::from_le_bytes(core::array::from_fn(|index| index as u8)),
    );
    cpu.set_xmm_reg(
        1,
        u128::from_le_bytes(core::array::from_fn(
            |index| if index % 2 == 0 { 0x80 } else { 0 },
        )),
    );
    mem.ram[0x300..0x310].fill(0xAA);

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(!matches!(result, RunResult::Exception { .. }), "{result:?}");
    assert_eq!(
        &mem.ram[0x300..0x310],
        &[0, 0xAA, 2, 0xAA, 4, 0xAA, 6, 0xAA, 8, 0xAA, 10, 0xAA, 12, 0xAA, 14, 0xAA]
    );
}

#[test]
fn sse2_movntdq_requires_a_sixteen_byte_aligned_destination() {
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xE7, 0x06, 0x03, 0x02]);
    cpu.set_cpu_type(gp64_cpu::CpuType::Pentium4);
    cpu.regs.cr4 = gp64_cpu::CR4_OSFXSR;
    cpu.set_xmm_reg(0, u128::MAX);
    let before = mem.ram[0x203..0x213].to_vec();

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        }
    ));
    assert_eq!(&mem.ram[0x203..0x213], before);
    assert_eq!(cpu.regs.ip(), 0x100);
}

#[test]
fn pentium_iii_rejects_sse2_without_side_effects() {
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xFE, 0xC1]);
    cpu.set_cpu_type(gp64_cpu::CpuType::PentiumIII);
    cpu.regs.cr4 = gp64_cpu::CR4_OSFXSR;
    cpu.set_xmm_regs([1, 2, 3, 4, 5, 6, 7, 8]);
    let before = cpu.xmm_regs();

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(matches!(result, RunResult::Exception { vector: 6, .. }));
    assert_eq!(cpu.xmm_regs(), before);
    assert_eq!(cpu.regs.eip, 0x0100);
}

#[test]
fn sse_gate_orders_ud_before_nm_and_is_restartable() {
    for (name, ty, cr0, cr4, vector) in [
        (
            "unsupported before TS",
            gp64_cpu::CpuType::PentiumII,
            CR0_TS,
            gp64_cpu::CR4_OSFXSR,
            6,
        ),
        (
            "EM before TS",
            gp64_cpu::CpuType::PentiumIII,
            CR0_EM | CR0_TS,
            gp64_cpu::CR4_OSFXSR,
            6,
        ),
        (
            "OSFXSR before TS",
            gp64_cpu::CpuType::PentiumIII,
            CR0_TS,
            0,
            6,
        ),
        (
            "TS",
            gp64_cpu::CpuType::PentiumIII,
            CR0_TS,
            gp64_cpu::CR4_OSFXSR,
            7,
        ),
    ] {
        let (mut cpu, mut mem) = setup(&[0x0F, 0x57, 0xC0]);
        cpu.set_cpu_type(ty);
        cpu.regs.cr0 = cr0;
        cpu.regs.cr4 = cr4;
        cpu.set_xmm_reg(0, u128::MAX);
        let result = run(&mut cpu, &mut mem, &mut (), 1);
        assert!(
            matches!(result, RunResult::Exception { vector: got, .. } if got == vector),
            "{name}: expected vector {vector}, got {result:?}"
        );
        assert_eq!(cpu.xmm_reg(0), u128::MAX, "{name}: changed XMM state");
        assert_eq!(cpu.regs.eip, 0x0100, "{name}: fault did not restart");
    }
}

#[test]
fn sse_memory_load_and_alignment_faults_are_restartable() {
    let payload = 0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210u128;
    let (mut cpu, mut mem) = setup(&[0x0F, 0x10, 0x06, 0x00, 0x20]); // MOVUPS XMM0,[2000]
    cpu.set_cpu_type(gp64_cpu::CpuType::PentiumIII);
    cpu.regs.cr4 = gp64_cpu::CR4_OSFXSR;
    mem.ram[0x2000..0x2010].copy_from_slice(&payload.to_le_bytes());
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(!matches!(result, RunResult::Exception { .. }), "{result:?}");
    assert_eq!(cpu.xmm_reg(0), payload);

    let (mut cpu, mut mem) = setup(&[0x0F, 0x28, 0x06, 0x01, 0x20]); // MOVAPS XMM0,[2001]
    cpu.set_cpu_type(gp64_cpu::CpuType::PentiumIII);
    cpu.regs.cr4 = gp64_cpu::CR4_OSFXSR;
    cpu.set_xmm_reg(0, payload);
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(result, RunResult::Exception { vector: 13, .. }));
    assert_eq!(cpu.xmm_reg(0), payload);
    assert_eq!(cpu.regs.eip, 0x0100);
}

#[test]
fn sse2_shuffle_shift_and_compare_use_correct_lanes() {
    let (mut cpu, mut mem) = setup(&[
        0x66, 0x0F, 0x70, 0xC1, 0x1B, // PSHUFD XMM0,XMM1,1Bh (reverse)
        0x66, 0x0F, 0x72, 0xF0, 0x01, // PSLLD XMM0,1
        0x0F, 0x2F, 0xC2, // COMISS XMM0,XMM2
    ]);
    cpu.set_cpu_type(gp64_cpu::CpuType::Pentium4);
    cpu.regs.cr4 = gp64_cpu::CR4_OSFXSR;
    let words = |lanes: [u32; 4]| {
        u128::from_le_bytes(lanes.map(u32::to_le_bytes).concat().try_into().unwrap())
    };
    cpu.set_xmm_reg(1, words([1, 2, 3, 4]));
    cpu.set_xmm_reg(2, 8);

    let first = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(!matches!(first, RunResult::Exception { .. }), "{first:?}");
    assert_eq!(cpu.xmm_reg(0), words([4, 3, 2, 1]));
    let second = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(!matches!(second, RunResult::Exception { .. }), "{second:?}");
    assert_eq!(cpu.xmm_reg(0), words([8, 6, 4, 2]));
    let third = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(!matches!(third, RunResult::Exception { .. }), "{third:?}");
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert!(!cpu.regs.flags.intersects(Flags::CARRY | Flags::PARITY));
}

#[test]
fn fxsave_fxrstor_round_trip_x87_xmm_and_mxcsr() {
    let code = [
        0x0F, 0xAE, 0x06, 0x00, 0x20, // FXSAVE [2000]
        0x0F, 0xAE, 0x0E, 0x00, 0x20, // FXRSTOR [2000]
    ];
    let (mut cpu, mut mem) = setup(&code);
    cpu.set_cpu_type(gp64_cpu::CpuType::Pentium4);
    cpu.regs.cr4 = gp64_cpu::CR4_OSFXSR;
    cpu.fpu.control_word = 0x027F;
    cpu.fpu.status_word = 0x1800;
    cpu.fpu.tag_word = 0xFFFC;
    cpu.fpu.regs[0] = gp64_cpu::x87_ops::F80::from_f64(3.25);
    cpu.set_xmm_reg(7, 0xDEAD_BEEF_CAFE_BABE_0123_4567_89AB_CDEF);
    cpu.set_mxcsr(0x3F80);
    let first = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(!matches!(first, RunResult::Exception { .. }), "{first:?}");

    cpu.fpu.reset();
    cpu.set_xmm_regs([0; 8]);
    cpu.set_mxcsr(gp64_cpu::sse::MXCSR_RESET);
    let second = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(!matches!(second, RunResult::Exception { .. }), "{second:?}");
    assert_eq!(cpu.fpu.control_word, 0x027F);
    assert_eq!(cpu.fpu.status_word, 0x1800);
    assert_eq!(cpu.fpu.tag_word, 0xFFFC);
    assert_eq!(cpu.fpu.regs[0].to_f64(), 3.25);
    assert_eq!(cpu.xmm_reg(7), 0xDEAD_BEEF_CAFE_BABE_0123_4567_89AB_CDEF);
    assert_eq!(cpu.mxcsr(), 0x3F80);
}

#[test]
fn mxcsr_rounding_and_unmasked_exception_control_float_execution() {
    let (mut cpu, mut mem) = setup(&[0xF3, 0x0F, 0x58, 0xC1]); // ADDSS
    cpu.set_cpu_type(gp64_cpu::CpuType::PentiumIII);
    cpu.regs.cr4 = gp64_cpu::CR4_OSFXSR | gp64_cpu::CR4_OSXMMEXCPT;
    cpu.set_mxcsr(gp64_cpu::sse::MXCSR_RESET | (2 << 13)); // round upward
    cpu.set_xmm_reg(0, u128::from(1.0f32.to_bits()));
    cpu.set_xmm_reg(1, u128::from((2.0f32.powi(-24)).to_bits()));
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(!matches!(result, RunResult::Exception { .. }), "{result:?}");
    assert_eq!(cpu.xmm_reg(0) as u32, 1.0f32.to_bits() + 1);
    assert_ne!(cpu.mxcsr() & (1 << 5), 0, "inexact status was not recorded");

    let (mut cpu, mut mem) = setup(&[0xF3, 0x0F, 0x5E, 0xC1]); // DIVSS
    cpu.set_cpu_type(gp64_cpu::CpuType::PentiumIII);
    cpu.regs.cr4 = gp64_cpu::CR4_OSFXSR | gp64_cpu::CR4_OSXMMEXCPT;
    cpu.set_mxcsr(gp64_cpu::sse::MXCSR_RESET & !(1 << 9)); // unmask divide-by-zero
    cpu.set_xmm_reg(0, u128::from(1.0f32.to_bits()));
    cpu.set_xmm_reg(1, u128::from(0.0f32.to_bits()));
    let before = cpu.xmm_reg(0);
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(result, RunResult::Exception { vector: 19, .. }));
    assert_eq!(cpu.xmm_reg(0), before, "faulting SIMD arithmetic committed");
    assert_ne!(cpu.mxcsr() & (1 << 2), 0, "divide-by-zero status missing");
    assert_eq!(cpu.regs.eip, 0x0100);
}

#[test]
fn pentium4_mxcsr_daz_and_ftz_control_denormal_execution() {
    let (mut cpu, mut mem) = setup(&[0xF3, 0x0F, 0x58, 0xC1]); // ADDSS
    cpu.set_cpu_type(gp64_cpu::CpuType::Pentium4);
    cpu.regs.cr4 = gp64_cpu::CR4_OSFXSR;
    assert_ne!(
        cpu.mxcsr_mask() & (1 << 6),
        0,
        "Pentium 4 must advertise DAZ"
    );
    cpu.set_mxcsr(gp64_cpu::sse::MXCSR_RESET | (1 << 6));
    cpu.set_xmm_reg(0, 1); // smallest positive subnormal
    cpu.set_xmm_reg(1, 0);

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(!matches!(result, RunResult::Exception { .. }), "{result:?}");
    assert_eq!(cpu.xmm_reg(0) as u32, 0, "DAZ did not zero the input");
    assert_eq!(cpu.mxcsr() & (1 << 1), 0, "DAZ raised denormal status");

    let (mut cpu, mut mem) = setup(&[0xF3, 0x0F, 0x5E, 0xC1]); // DIVSS
    cpu.set_cpu_type(gp64_cpu::CpuType::Pentium4);
    cpu.regs.cr4 = gp64_cpu::CR4_OSFXSR;
    cpu.set_mxcsr(gp64_cpu::sse::MXCSR_RESET | (1 << 15));
    cpu.set_xmm_reg(0, u128::from(f32::MIN_POSITIVE.to_bits()));
    cpu.set_xmm_reg(1, u128::from(3.0f32.to_bits()));

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(!matches!(result, RunResult::Exception { .. }), "{result:?}");
    assert_eq!(cpu.xmm_reg(0) as u32, 0, "FTZ did not zero the result");
    assert_eq!(
        cpu.mxcsr() & ((1 << 4) | (1 << 5)),
        (1 << 4) | (1 << 5),
        "FTZ did not record underflow and inexact status"
    );

    let mut pentium_iii = Cpu::new();
    pentium_iii.set_cpu_type(gp64_cpu::CpuType::PentiumIII);
    assert_eq!(pentium_iii.mxcsr_mask() & (1 << 6), 0);
}

#[test]
fn mxcsr_invalid_exceptions_cover_compare_and_conversion() {
    for (name, code) in [
        ("COMISS", &[0x0F, 0x2F, 0xC1][..]),
        ("CVTTPS2DQ", &[0xF3, 0x0F, 0x5B, 0xC1][..]),
    ] {
        let (mut cpu, mut mem) = setup(code);
        cpu.set_cpu_type(gp64_cpu::CpuType::Pentium4);
        cpu.regs.cr4 = gp64_cpu::CR4_OSFXSR | gp64_cpu::CR4_OSXMMEXCPT;
        cpu.set_mxcsr(gp64_cpu::sse::MXCSR_RESET & !(1 << 7));
        cpu.set_xmm_reg(0, 0xA5A5_A5A5_A5A5_A5A5_A5A5_A5A5_A5A5_A5A5);
        cpu.set_xmm_reg(1, u128::from(f32::NAN.to_bits()));
        let destination_before = cpu.xmm_reg(0);
        let flags_before = cpu.regs.flags;

        let result = run(&mut cpu, &mut mem, &mut (), 1);

        assert!(
            matches!(result, RunResult::Exception { vector: 19, .. }),
            "{name}: {result:?}"
        );
        assert_eq!(
            cpu.xmm_reg(0),
            destination_before,
            "{name}: committed result"
        );
        assert_eq!(cpu.regs.flags, flags_before, "{name}: committed EFLAGS");
        assert_ne!(cpu.mxcsr() & 1, 0, "{name}: invalid status missing");
        assert_eq!(cpu.regs.eip, 0x0100, "{name}: did not restart");
    }
}

#[test]
fn test_mmx_emms() {
    // 0F 77 = EMMS
    let (mut cpu, mut mem) = setup_pentium(&[0x0F, 0x77]);
    cpu.fpu.tag_word = 0x0000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.fpu.tag_word, 0xFFFF);
}

#[test]
fn test_mmx_movd_reg_to_mm() {
    // MOVD MM0, EAX = 0F 6E C0
    let (mut cpu, mut mem) = setup_pentium(&[0x0F, 0x6E, 0xC0]);
    cpu.regs.eax = 0xDEADBEEF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.mmx_reg(0), 0xDEADBEEF);
}

#[test]
fn test_mmx_movd_mm_to_reg() {
    // MOVD EAX, MM1 = 0F 7E C8
    let (mut cpu, mut mem) = setup_pentium(&[0x0F, 0x7E, 0xC8]);
    cpu.set_mmx_reg(1, 0x0000_0000_CAFE_BABE);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xCAFEBABE);
}

#[test]
fn test_mmx_movq_mm_to_mm() {
    // MOVQ MM0, MM1 = 0F 6F C1
    let (mut cpu, mut mem) = setup_pentium(&[0x0F, 0x6F, 0xC1]);
    cpu.set_mmx_reg(1, 0x1234_5678_ABCD_EF01);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.mmx_reg(0), 0x1234_5678_ABCD_EF01);
}

#[test]
fn test_mmx_movd_mem_to_mm() {
    // MOVD MM0, [0x200] = 0F 6E 06 00 02
    let (mut cpu, mut mem) = setup_pentium(&[0x0F, 0x6E, 0x06, 0x00, 0x02]);
    mem.ram[0x200..0x204].copy_from_slice(&0xAABBCCDDu32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.mmx_reg(0), 0xAABBCCDD);
}

#[test]
fn test_mmx_movd_mm_to_mem() {
    // MOVD [0x200], MM0 = 0F 7E 06 00 02
    let (mut cpu, mut mem) = setup_pentium(&[0x0F, 0x7E, 0x06, 0x00, 0x02]);
    cpu.set_mmx_reg(0, 0x11223344);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes(mem.ram[0x200..0x204].try_into().unwrap());
    assert_eq!(val, 0x11223344);
}

#[test]
fn test_mmx_movq_mem_to_mm() {
    // MOVQ MM0, [0x200] = 0F 6F 06 00 02
    let (mut cpu, mut mem) = setup_pentium(&[0x0F, 0x6F, 0x06, 0x00, 0x02]);
    mem.ram[0x200..0x208].copy_from_slice(&0x0102030405060708u64.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.mmx_reg(0), 0x0102030405060708);
}

#[test]
fn test_mmx_movq_mm_to_mem() {
    // MOVQ [0x200], MM2 = 0F 7F 16 00 02
    let (mut cpu, mut mem) = setup_pentium(&[0x0F, 0x7F, 0x16, 0x00, 0x02]);
    cpu.set_mmx_reg(2, 0xFEDCBA9876543210);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u64::from_le_bytes(mem.ram[0x200..0x208].try_into().unwrap());
    assert_eq!(val, 0xFEDCBA9876543210);
}

fn assert_mmx_movq_cross_page_fault_is_atomic(high_page_entry: u32, enable_wp: bool) {
    // MOVQ [0x5FFC], MM0 = 0F 7F /r with a disp32 memory operand.
    let code = [0x0F, 0x7F, 0x05, 0xFC, 0x5F, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_pentium_paging(&code);
    if enable_wp {
        cpu.regs.cr0 |= 1 << 16;
    }
    mem.write_u32_physical(0x11000 + 6 * 4, high_page_entry);

    let original = 0x8877_6655_4433_2211u64.to_le_bytes();
    mem.ram[0x5FFC..0x6004].copy_from_slice(&original);
    cpu.set_mmx_reg(0, 0x0123_4567_89AB_CDEF);
    cpu.fpu.set_top(5);
    cpu.fpu.tag_word = 0xA55A;
    let saved_eip = cpu.regs.eip;
    let saved_mmx_retired = cpu.mmx_retired;

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(
        matches!(result, RunResult::Exception { vector: 14, .. }),
        "expected #PF from the high page, got {result:?}"
    );
    assert_eq!(cpu.regs.eip, saved_eip);
    assert_eq!(cpu.fpu.get_top(), 5, "faulting MOVQ must not reset x87 TOP");
    assert_eq!(
        cpu.fpu.tag_word, 0xA55A,
        "faulting MOVQ must not enter MMX tag state"
    );
    assert_eq!(
        cpu.mmx_retired, saved_mmx_retired,
        "faulting MOVQ must not retire"
    );
    assert_eq!(
        &mem.ram[0x5FFC..0x6004],
        &original,
        "MOVQ must validate both pages before committing either dword"
    );
}

#[test]
fn test_mmx_movq_store_is_atomic_when_high_page_is_absent() {
    assert_mmx_movq_cross_page_fault_is_atomic(0, false);
}

#[test]
fn test_mmx_movq_store_is_atomic_when_high_page_is_read_only() {
    assert_mmx_movq_cross_page_fault_is_atomic((6 << 12) | 0x05, true);
}

#[test]
fn test_cmpxchg8b_store_is_atomic_when_high_page_is_read_only() {
    // CMPXCHG8B [0x5FFC] = 0F C7 /1 with a disp32 memory operand.
    let code = [0x0F, 0xC7, 0x0D, 0xFC, 0x5F, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_pentium_paging(&code);
    cpu.regs.cr0 |= 1 << 16; // CR0.WP: supervisor writes honor read-only PTEs.
    mem.write_u32_physical(0x11000 + 6 * 4, (6 << 12) | 0x05);

    let original = 0x8877_6655_4433_2211u64;
    mem.ram[0x5FFC..0x6004].copy_from_slice(&original.to_le_bytes());
    cpu.regs.eax = original as u32;
    cpu.regs.edx = (original >> 32) as u32;
    cpu.regs.ebx = 0x89AB_CDEF;
    cpu.regs.ecx = 0x0123_4567;
    let saved_eip = cpu.regs.eip;

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 14,
                executed: 0,
                ..
            }
        ),
        "expected precise #PF from the high page, got {result:?}"
    );
    assert_eq!(cpu.regs.eip, saved_eip);
    assert_eq!(
        &mem.ram[0x5FFC..0x6004],
        &original.to_le_bytes(),
        "CMPXCHG8B must validate both pages before committing either dword"
    );
}

#[test]
fn test_mmx_pand() {
    // MOVD MM0, EAX; MOVD MM1, EBX; PAND MM0, MM1 = 0F DB C1
    let code = [
        0x0F, 0x6E, 0xC0, // MOVD MM0, EAX
        0x0F, 0x6E, 0xCB, // MOVD MM1, EBX
        0x0F, 0xDB, 0xC1, // PAND MM0, MM1
    ];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0xFF00FF00;
    cpu.regs.ebx = 0x0F0F0F0F;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    assert_eq!(cpu.mmx_reg(0) as u32, 0x0F000F00);
}

#[test]
fn test_mmx_pandn() {
    // MOVD MM0, EAX; MOVD MM1, EBX; PANDN MM0, MM1 = 0F DF C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0xDF, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0xFF00FF00;
    cpu.regs.ebx = 0x0F0F0F0F;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    // PANDN: ~MM0 & MM1 = ~0xFF00FF00 & 0x0F0F0F0F = 0x00FF00FF & 0x0F0F0F0F = 0x000F000F
    assert_eq!(cpu.mmx_reg(0) as u32, 0x000F000F);
}

#[test]
fn test_mmx_por() {
    // MOVD MM0, EAX; MOVD MM1, EBX; POR MM0, MM1 = 0F EB C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0xEB, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0xAA000000;
    cpu.regs.ebx = 0x00BB0000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    assert_eq!(cpu.mmx_reg(0) as u32, 0xAABB0000);
}

#[test]
fn test_mmx_pxor() {
    // MOVD MM0, EAX; MOVD MM1, EBX; PXOR MM0, MM1 = 0F EF C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0xEF, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0xFF00FF00;
    cpu.regs.ebx = 0xFF00FF00;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    assert_eq!(cpu.mmx_reg(0) as u32, 0x00000000);
}

#[test]
fn test_mmx_paddb() {
    // PADDB MM0, MM1 = 0F FC C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0xFC, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0x01020304; // bytes: 04,03,02,01
    cpu.regs.ebx = 0x10203040; // bytes: 40,30,20,10
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    // Byte-wise add: 04+40=44, 03+30=33, 02+20=22, 01+10=11
    assert_eq!(cpu.mmx_reg(0) as u32, 0x11223344);
}

#[test]
fn test_mmx_psubb() {
    // PSUBB MM0, MM1 = 0F F8 C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0xF8, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0x44332211;
    cpu.regs.ebx = 0x01020304;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    // Byte-wise sub
    assert_eq!(cpu.mmx_reg(0) as u32, 0x43311F0D);
}

#[test]
fn test_mmx_paddw() {
    // PADDW MM0, MM1 = 0F FD C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0xFD, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0x00010002;
    cpu.regs.ebx = 0x00030004;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    assert_eq!(cpu.mmx_reg(0) as u32, 0x00040006);
}

#[test]
fn test_mmx_paddd() {
    // PADDD MM0, MM1 = 0F FE C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0xFE, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0x10000000;
    cpu.regs.ebx = 0x20000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    assert_eq!(cpu.mmx_reg(0) as u32, 0x30000000);
}

#[test]
fn test_mmx_pcmpeqb() {
    // PCMPEQB MM0, MM1 = 0F 74 C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0x74, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0x01020304;
    cpu.regs.ebx = 0x01000304;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    // Byte 0: 04==04 -> FF, byte 1: 03==03 -> FF, byte 2: 02!=00 -> 00, byte 3: 01==01 -> FF
    assert_eq!(cpu.mmx_reg(0) as u32, 0xFF00FFFF);
}

#[test]
fn test_mmx_pcmpeqw() {
    // PCMPEQW MM0, MM1 = 0F 75 C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0x75, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0x00010002;
    cpu.regs.ebx = 0x00010003;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    // word 0: 0002 != 0003 -> 0000, word 1: 0001 == 0001 -> FFFF
    assert_eq!(cpu.mmx_reg(0) as u32, 0xFFFF0000);
}

#[test]
fn test_mmx_pcmpeqd() {
    // PCMPEQD MM0, MM1 = 0F 76 C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0x76, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0xAAAAAAAA;
    cpu.regs.ebx = 0xAAAAAAAA;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    assert_eq!(cpu.mmx_reg(0) as u32, 0xFFFFFFFF);
}

#[test]
fn test_mmx_pmullw() {
    // PMULLW MM0, MM1 = 0F D5 C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0xD5, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0x00030004; // words: 0004, 0003
    cpu.regs.ebx = 0x00050002; // words: 0002, 0005
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    // word 0: 4*2=8, word 1: 3*5=15
    assert_eq!(cpu.mmx_reg(0) as u32, 0x000F0008);
}

#[test]
fn test_mmx_emms_on_non_pentium_raises_ud() {
    // EMMS on 386 should raise #UD
    let (mut cpu, mut mem) = setup(&[0x0F, 0x77]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::Exception { vector: 6, .. }));
}

// ========================================================================
// 3. BCD instructions
// ========================================================================

#[test]
fn test_daa_cf_set() {
    // DAA with AL > 0x99 sets CF
    let (mut cpu, mut mem) = setup(&[0x27]);
    cpu.regs.set_al(0xA5);
    cpu.regs.flags.remove(Flags::AUX_CARRY);
    cpu.regs.flags.remove(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_das_cf_set() {
    // DAS with borrow sets CF
    let (mut cpu, mut mem) = setup(&[0x2F]);
    cpu.regs.set_al(0xFF);
    cpu.regs.flags.insert(Flags::AUX_CARRY);
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_aaa_adjust_carry() {
    // AAA = 0x37
    let (mut cpu, mut mem) = setup(&[0x37]);
    cpu.regs.set_al(0x0A); // low nibble >= 10 => adjust
    cpu.regs.eax = (cpu.regs.eax & 0xFFFF_FF00) | 0x0A;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x00);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_aas_adjust_borrow() {
    // AAS = 0x3F
    let (mut cpu, mut mem) = setup(&[0x3F]);
    cpu.regs.eax = 0x00FF; // AH=0, AL=0xFF -> AF should trigger
    cpu.regs.flags.insert(Flags::AUX_CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_aam_custom_base() {
    // AAM 16 = D4 10
    let (mut cpu, mut mem) = setup(&[0xD4, 0x10]);
    cpu.regs.set_al(0x20); // 32 decimal
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // 32 / 16 = 2, 32 % 16 = 0 => AH=2, AL=0
    assert_eq!((cpu.regs.eax >> 8) & 0xFF, 2);
    assert_eq!(cpu.regs.al(), 0);
}

/// MDD-BUG-CRU-01385: on the 386, `AAM 0` latches SZP from `AL >> 1` and
/// clears CF/OF/AF before raising #DE, leaving AX alone. The rows are the 12
/// SingleStep 80386 `aam 0` vectors (`v1_ex_real_mode/D4`): initial AL,
/// initial EFLAGS, and the arithmetic bits of the FLAGS image pushed by #DE.
#[test]
fn aam_zero_on_386_latches_szp_from_half_al_before_divide_error() {
    const ARITH: u16 = 0x08D5; // OF SF ZF AF PF CF
    const VECTORS: [(u8, u16, u16); 12] = [
        (0xE3, 0x0042, 0x004), // idx 56
        (0xCD, 0x0843, 0x004), // idx 722
        (0x06, 0x0047, 0x004), // idx 787
        (0x50, 0x0C82, 0x004), // idx 1049
        (0x1B, 0x04D6, 0x000), // idx 1220: odd parity, PF cleared
        (0xB5, 0x0806, 0x004), // idx 1409
        (0x1F, 0x04C6, 0x004), // idx 1492
        (0x49, 0x0446, 0x004), // idx 1699
        (0xB4, 0x0C17, 0x004), // idx 1706
        (0x52, 0x08C6, 0x000), // idx 1813
        (0xFF, 0x04D3, 0x000), // idx 1905
        (0xE3, 0x00C3, 0x004), // idx 2024: same AL as idx 56, same result
    ];
    for (al, initial, expected) in VECTORS {
        let (mut cpu, mut mem) = setup(&[0xD4, 0x00]);
        cpu.cpu_type = CpuType::I80386;
        cpu.regs.eax = 0x1234_5600 | u32::from(al);
        cpu.regs.flags = Flags::from_bits_truncate(initial | 0x0002);
        let mut io = ();
        let result = run(&mut cpu, &mut mem, &mut io, 1);
        assert!(
            matches!(result, RunResult::Exception { vector: 0, .. }),
            "AL={al:02X}: {result:?}"
        );
        assert_eq!(
            cpu.regs.eax,
            0x1234_5600 | u32::from(al),
            "AX is not committed"
        );
        assert_eq!(
            cpu.regs.flags.bits() & ARITH,
            expected,
            "AL={al:02X} initial={initial:04X}"
        );
        assert_eq!(
            cpu.regs.flags.bits() & 0x0400,
            initial & 0x0400,
            "DF is not an arithmetic flag and survives"
        );
    }
}

/// MDD-BUG-CRU-01380: SHLD/SHRD OF, for every count, is the overflow of the
/// final one-bit step (sign of the count-1 intermediate XOR sign of the
/// result). Rows are SingleStep 80386 `v1_ex_real_mode` vectors on which both
/// "preserve OF for count > 1" and "sign(dst) XOR sign(result)" are wrong.
#[test]
fn double_shift_of_is_the_final_single_step_overflow() {
    struct Row {
        code: &'static [u8],
        dst: (usize, u32),
        src: (usize, u32),
        initial_of: bool,
        result: u32,
        of: bool,
    }
    const EDX: usize = 2;
    const EAX: usize = 0;
    const ESP: usize = 4;
    const EDI: usize = 7;
    const ECX: usize = 1;
    let rows = [
        // 0FA4 idx 16: shld dx,ax,0Dh
        Row {
            code: &[0x0F, 0xA4, 0xC2, 0x0D],
            dst: (EDX, 0xC523),
            src: (EAX, 0x4FFA),
            initial_of: true,
            result: 0x69FF,
            of: false,
        },
        // 0FAC idx 15: shrd sp,di,02h
        Row {
            code: &[0x0F, 0xAC, 0xFC, 0x02],
            dst: (ESP, 0x0C5A),
            src: (EDI, 0xB9C5),
            initial_of: false,
            result: 0x4316,
            of: true,
        },
        // 660FA4 idx 16: shld edx,eax,0Dh
        Row {
            code: &[0x66, 0x0F, 0xA4, 0xC2, 0x0D],
            dst: (EDX, 0xAAC3_C523),
            src: (EAX, 0xCDB1_4FFA),
            initial_of: true,
            result: 0x78A4_79B6,
            of: false,
        },
        // 660FAC idx 11: shrd esp,ecx,19h
        Row {
            code: &[0x66, 0x0F, 0xAC, 0xCC, 0x19],
            dst: (ESP, 0x0000_FFFE),
            src: (ECX, 0xF78D_2410),
            initial_of: true,
            result: 0xC692_0800,
            of: false,
        },
    ];
    for row in rows {
        let (mut cpu, mut mem) = setup(row.code);
        let wide = row.code[0] == 0x66;
        let set = |cpu: &mut Cpu, index: usize, value: u32| {
            let reg = match index {
                0 => &mut cpu.regs.eax,
                1 => &mut cpu.regs.ecx,
                2 => &mut cpu.regs.edx,
                4 => &mut cpu.regs.esp,
                7 => &mut cpu.regs.edi,
                _ => unreachable!(),
            };
            *reg = if wide {
                value
            } else {
                (*reg & 0xFFFF_0000) | value
            };
        };
        set(&mut cpu, row.dst.0, row.dst.1);
        set(&mut cpu, row.src.0, row.src.1);
        cpu.regs.flags.set(Flags::OVERFLOW, row.initial_of);
        let mut io = ();
        let _ = run(&mut cpu, &mut mem, &mut io, 1);
        let dst = match row.dst.0 {
            2 => cpu.regs.edx,
            4 => cpu.regs.esp,
            _ => unreachable!(),
        };
        let dst = if wide { dst } else { dst & 0xFFFF };
        assert_eq!(dst, row.result, "{:02X?}", row.code);
        assert_eq!(
            cpu.regs.flags.contains(Flags::OVERFLOW),
            row.of,
            "{:02X?} OF",
            row.code
        );
    }
}

#[test]
fn test_aad_custom_base() {
    // AAD 16 = D5 10
    let (mut cpu, mut mem) = setup(&[0xD5, 0x10]);
    cpu.regs.eax = (cpu.regs.eax & 0xFFFF_0000) | 0x0305; // AH=3, AL=5
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // result = AH*16 + AL = 48 + 5 = 53
    assert_eq!(cpu.regs.al(), 53);
    assert_eq!((cpu.regs.eax >> 8) & 0xFF, 0);
}

// ========================================================================
// 4. Bit manipulation — BSF, BSR, BT, BTS, BTR, BTC in PM32
// ========================================================================

#[test]
fn test_bsf_r32_pm() {
    // BSF EAX, EBX = 0F BC C3
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xBC, 0xC3]);
    cpu.regs.ebx = 0x0000_0100; // bit 8 set
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 8);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_bsf_r32_zero_pm() {
    // BSF EAX, EBX = 0F BC C3 with EBX=0
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xBC, 0xC3]);
    cpu.regs.ebx = 0;
    cpu.regs.eax = 0xDEAD;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_bsr_r32_pm() {
    // BSR EAX, EBX = 0F BD C3
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xBD, 0xC3]);
    cpu.regs.ebx = 0x8000_0000; // bit 31
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 31);
}

#[test]
fn test_bt_r32_imm8_pm() {
    // BT EAX, 16 = 0F BA E0 10
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xBA, 0xE0, 0x10]);
    cpu.regs.eax = 0x0001_0000; // bit 16 set
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_bts_r32_imm8_pm() {
    // BTS EAX, 4 = 0F BA E8 04
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xBA, 0xE8, 0x04]);
    cpu.regs.eax = 0;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x10); // bit 4 set
}

#[test]
fn test_btr_r32_imm8_pm() {
    // BTR EAX, 8 = 0F BA F0 08
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xBA, 0xF0, 0x08]);
    cpu.regs.eax = 0x100;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0); // bit 8 cleared
    assert!(cpu.regs.flags.contains(Flags::CARRY)); // old bit was 1
}

#[test]
fn test_btc_r32_imm8_pm() {
    // BTC EAX, 0 = 0F BA F8 00
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xBA, 0xF8, 0x00]);
    cpu.regs.eax = 0;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 1); // bit 0 toggled on
    assert!(!cpu.regs.flags.contains(Flags::CARRY)); // was 0
}

#[test]
fn test_bt_r32_r32_pm() {
    // BT ECX, EAX = 0F A3 C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xA3, 0xC1]);
    cpu.regs.ecx = 0x0000_0008; // bit 3 set
    cpu.regs.eax = 3; // test bit 3
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_bts_r32_r32_pm() {
    // BTS ECX, EAX = 0F AB C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xAB, 0xC1]);
    cpu.regs.ecx = 0;
    cpu.regs.eax = 5; // set bit 5
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx, 0x20);
}

// ========================================================================
// 5. SHLD/SHRD in PM32
// ========================================================================

#[test]
fn test_shld_r32_imm8_pm() {
    // SHLD EAX, EBX, 4 = 0F A4 D8 04
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xA4, 0xD8, 0x04]);
    cpu.regs.eax = 0x12345678;
    cpu.regs.ebx = 0xABCDEF01;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // SHLD shifts EAX left by 4, filling with top 4 bits of EBX (0xA)
    assert_eq!(cpu.regs.eax, 0x2345678A);
}

#[test]
fn test_shrd_r32_imm8_pm() {
    // SHRD EAX, EBX, 4 = 0F AC D8 04
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xAC, 0xD8, 0x04]);
    cpu.regs.eax = 0x12345678;
    cpu.regs.ebx = 0xABCDEF01;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // SHRD shifts EAX right by 4, filling with bottom 4 bits of EBX (1)
    assert_eq!(cpu.regs.eax, 0x11234567);
}

#[test]
fn test_shld_r32_cl_pm() {
    // SHLD EAX, EBX, CL = 0F A5 D8
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xA5, 0xD8]);
    cpu.regs.eax = 0x12345678;
    cpu.regs.ebx = 0xF0000000;
    cpu.regs.ecx = 8;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x345678F0);
}

#[test]
fn test_shrd_r32_cl_pm() {
    // SHRD EAX, EBX, CL = 0F AD D8
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xAD, 0xD8]);
    cpu.regs.eax = 0x12345678;
    cpu.regs.ebx = 0x0000000F;
    cpu.regs.ecx = 8;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x0F123456);
}

// ========================================================================
// 6. SETcc instructions in PM32
// ========================================================================

#[test]
fn test_sete_r8_pm() {
    // SETE AL = 0F 94 C0
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0x94, 0xC0]);
    cpu.regs.flags.insert(Flags::ZERO);
    cpu.regs.eax = 0;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 1);
}

#[test]
fn test_setne_r8_pm() {
    // SETNE AL = 0F 95 C0
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0x95, 0xC0]);
    cpu.regs.flags.remove(Flags::ZERO);
    cpu.regs.eax = 0;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 1);
}

#[test]
fn test_setg_r8_pm() {
    // SETG AL = 0F 9F C0 (ZF=0 and SF=OF)
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0x9F, 0xC0]);
    cpu.regs.flags.remove(Flags::ZERO);
    cpu.regs.flags.remove(Flags::SIGN);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    cpu.regs.eax = 0;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 1);
}

#[test]
fn test_setl_r8_pm() {
    // SETL AL = 0F 9C C0 (SF != OF)
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0x9C, 0xC0]);
    cpu.regs.flags.insert(Flags::SIGN);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    cpu.regs.eax = 0;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 1);
}

// ========================================================================
// 7. CMOVcc instructions in PM32
// ========================================================================

#[test]
fn test_cmovz_r32_pm_taken_is_ud_on_pentium() {
    // CMOVZ EAX, EBX = 0F 44 C3 (P6+)
    let (mut cpu, mut mem) = setup_pentium_pm32(&[0x0F, 0x44, 0xC3]);
    cpu.regs.flags.insert(Flags::ZERO);
    cpu.regs.eax = 0xAAAA;
    cpu.regs.ebx = 0xBBBB;
    let saved_flags = cpu.regs.flags;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 6,
            error_code: 0,
            fault_addr: None,
            executed: 0
        }
    ));
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.eax, 0xAAAA);
    assert_eq!(cpu.regs.ebx, 0xBBBB);
    assert_eq!(cpu.regs.flags, saved_flags);
}

#[test]
fn test_cmovz_r32_pm_not_taken_is_still_ud_on_pentium() {
    // CMOVZ EAX, EBX = 0F 44 C3 (P6+)
    let (mut cpu, mut mem) = setup_pentium_pm32(&[0x0F, 0x44, 0xC3]);
    cpu.regs.flags.remove(Flags::ZERO);
    cpu.regs.eax = 0xAAAA;
    cpu.regs.ebx = 0xBBBB;
    let saved_flags = cpu.regs.flags;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 6,
            error_code: 0,
            fault_addr: None,
            executed: 0
        }
    ));
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.eax, 0xAAAA);
    assert_eq!(cpu.regs.ebx, 0xBBBB);
    assert_eq!(cpu.regs.flags, saved_flags);
}

#[test]
fn test_cmovb_r32_pm_is_ud_on_pentium() {
    // CMOVB EAX, ECX = 0F 42 C1 (P6+)
    let (mut cpu, mut mem) = setup_pentium_pm32(&[0x0F, 0x42, 0xC1]);
    cpu.regs.flags.insert(Flags::CARRY);
    cpu.regs.eax = 0;
    cpu.regs.ecx = 0x12345678;
    let saved_flags = cpu.regs.flags;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 6,
            error_code: 0,
            fault_addr: None,
            executed: 0
        }
    ));
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.eax, 0);
    assert_eq!(cpu.regs.ecx, 0x12345678);
    assert_eq!(cpu.regs.flags, saved_flags);
}

#[test]
fn test_cmova_r32_pm_is_ud_on_pentium() {
    // CMOVA EAX, ECX = 0F 47 C1 (P6+, CF=0 and ZF=0)
    let (mut cpu, mut mem) = setup_pentium_pm32(&[0x0F, 0x47, 0xC1]);
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.flags.remove(Flags::ZERO);
    cpu.regs.eax = 0;
    cpu.regs.ecx = 0xDEAD;
    let saved_flags = cpu.regs.flags;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 6,
            error_code: 0,
            fault_addr: None,
            executed: 0
        }
    ));
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.eax, 0);
    assert_eq!(cpu.regs.ecx, 0xDEAD);
    assert_eq!(cpu.regs.flags, saved_flags);
}

// ========================================================================
// 8. Protected mode string operations
// ========================================================================

#[test]
fn test_rep_movsb_pm32() {
    // REP MOVSB = F3 A4
    let (mut cpu, mut mem) = setup_pm32(&[0xF3, 0xA4]);
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x3000;
    cpu.regs.ecx = 8;
    for i in 0..8u8 {
        mem.ram[0x2000 + i as usize] = 0x10 + i;
    }
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 64);
    for i in 0..8u8 {
        assert_eq!(mem.ram[0x3000 + i as usize], 0x10 + i);
    }
    assert_eq!(cpu.regs.ecx, 0);
}

#[test]
fn test_rep_stosb_pm32() {
    // REP STOSB = F3 AA
    let (mut cpu, mut mem) = setup_pm32(&[0xF3, 0xAA]);
    cpu.regs.set_al(0x42);
    cpu.regs.edi = 0x4000;
    cpu.regs.ecx = 16;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 64);
    for i in 0..16 {
        assert_eq!(mem.ram[0x4000 + i], 0x42);
    }
    assert_eq!(cpu.regs.ecx, 0);
}

#[test]
fn test_rep_stosd_pm32() {
    // REP STOSD = F3 AB
    let (mut cpu, mut mem) = setup_pm32(&[0xF3, 0xAB]);
    cpu.regs.eax = 0xDEADBEEF;
    cpu.regs.edi = 0x5000;
    cpu.regs.ecx = 4;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 64);
    for i in 0..4 {
        let val = u32::from_le_bytes(mem.ram[0x5000 + i * 4..0x5004 + i * 4].try_into().unwrap());
        assert_eq!(val, 0xDEADBEEF);
    }
}

#[test]
fn test_rep_movsd_pm32() {
    // REP MOVSD = F3 A5
    let (mut cpu, mut mem) = setup_pm32(&[0xF3, 0xA5]);
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x3000;
    cpu.regs.ecx = 4;
    for i in 0..4u32 {
        let addr = 0x2000 + (i * 4) as usize;
        mem.ram[addr..addr + 4].copy_from_slice(&(0xAA00_0000 + i).to_le_bytes());
    }
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 64);
    for i in 0..4u32 {
        let val = u32::from_le_bytes(
            mem.ram[0x3000 + (i * 4) as usize..0x3004 + (i * 4) as usize]
                .try_into()
                .unwrap(),
        );
        assert_eq!(val, 0xAA00_0000 + i);
    }
}

#[test]
fn test_repe_cmpsb_pm32_equal() {
    // REPE CMPSB = F3 A6
    let (mut cpu, mut mem) = setup_pm32(&[0xF3, 0xA6]);
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x3000;
    cpu.regs.ecx = 4;
    for i in 0..4u8 {
        mem.ram[0x2000 + i as usize] = 0x41 + i;
        mem.ram[0x3000 + i as usize] = 0x41 + i;
    }
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 64);
    assert_eq!(cpu.regs.ecx, 0);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_repe_cmpsb_pm32_mismatch() {
    // REPE CMPSB = F3 A6
    let (mut cpu, mut mem) = setup_pm32(&[0xF3, 0xA6]);
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x3000;
    cpu.regs.ecx = 4;
    for i in 0..4u8 {
        mem.ram[0x2000 + i as usize] = 0x41 + i;
        mem.ram[0x3000 + i as usize] = 0x41 + i;
    }
    mem.ram[0x3002] = 0xFF; // mismatch at index 2
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 64);
    assert_eq!(cpu.regs.ecx, 1); // stopped after comparing index 2
}

#[test]
fn test_repne_scasb_pm32() {
    // REPNE SCASB = F2 AE
    let (mut cpu, mut mem) = setup_pm32(&[0xF2, 0xAE]);
    cpu.regs.set_al(0x42);
    cpu.regs.edi = 0x4000;
    cpu.regs.ecx = 8;
    mem.ram[0x4000] = 0x41;
    mem.ram[0x4001] = 0x41;
    mem.ram[0x4002] = 0x42; // match here
    mem.ram[0x4003] = 0x43;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 64);
    assert_eq!(cpu.regs.ecx, 5); // 8 - 3 = 5
}

#[test]
fn test_rep_lodsb_pm32() {
    // REP LODSB = F3 AC
    let (mut cpu, mut mem) = setup_pm32(&[0xF3, 0xAC]);
    cpu.regs.esi = 0x2000;
    cpu.regs.ecx = 3;
    mem.ram[0x2000] = 0xAA;
    mem.ram[0x2001] = 0xBB;
    mem.ram[0x2002] = 0xCC;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 64);
    assert_eq!(cpu.regs.al(), 0xCC); // last loaded byte
    assert_eq!(cpu.regs.ecx, 0);
}

// ========================================================================
// 9. Paging — instruction variants that exercise paged code paths
// ========================================================================

#[test]
fn test_paging_add_r32_mem() {
    // ADD EAX, [0x2000] = 03 05 00 20 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x03, 0x05, 0x00, 0x20, 0x00, 0x00]);
    cpu.regs.eax = 0x100;
    mem.ram[0x2000..0x2004].copy_from_slice(&0x200u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x300);
}

#[test]
fn test_paging_sub_r32_mem() {
    // SUB EAX, [0x2000] = 2B 05 00 20 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x2B, 0x05, 0x00, 0x20, 0x00, 0x00]);
    cpu.regs.eax = 0x500;
    mem.ram[0x2000..0x2004].copy_from_slice(&0x200u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x300);
}

#[test]
fn test_paging_and_r32_mem() {
    // AND EAX, [0x2000] = 23 05 00 20 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x23, 0x05, 0x00, 0x20, 0x00, 0x00]);
    cpu.regs.eax = 0xFF00FF00;
    mem.ram[0x2000..0x2004].copy_from_slice(&0x0F0F0F0Fu32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x0F000F00);
}

#[test]
fn test_paging_or_r32_mem() {
    // OR EAX, [0x2000] = 0B 05 00 20 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x0B, 0x05, 0x00, 0x20, 0x00, 0x00]);
    cpu.regs.eax = 0xAA000000;
    mem.ram[0x2000..0x2004].copy_from_slice(&0x00BB0000u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xAABB0000);
}

#[test]
fn test_paging_xor_r32_mem() {
    // XOR EAX, [0x2000] = 33 05 00 20 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x33, 0x05, 0x00, 0x20, 0x00, 0x00]);
    cpu.regs.eax = 0xFF00FF00;
    mem.ram[0x2000..0x2004].copy_from_slice(&0xFF00FF00u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0);
}

#[test]
fn test_paging_cmp_r32_mem() {
    // CMP EAX, [0x2000] = 3B 05 00 20 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x3B, 0x05, 0x00, 0x20, 0x00, 0x00]);
    cpu.regs.eax = 0x100;
    mem.ram[0x2000..0x2004].copy_from_slice(&0x100u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.eax, 0x100); // CMP doesn't modify
}

#[test]
fn test_paging_mov_r32_mem_write() {
    // MOV [0x3000], EAX = 89 05 00 30 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x89, 0x05, 0x00, 0x30, 0x00, 0x00]);
    cpu.regs.eax = 0xCAFEBABE;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes(mem.ram[0x3000..0x3004].try_into().unwrap());
    assert_eq!(val, 0xCAFEBABE);
}

#[test]
fn test_paging_mov_r32_mem_read() {
    // MOV EAX, [0x3000] = 8B 05 00 30 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x8B, 0x05, 0x00, 0x30, 0x00, 0x00]);
    mem.ram[0x3000..0x3004].copy_from_slice(&0x12345678u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x12345678);
}

#[test]
fn test_paging_lea_r32() {
    // LEA EAX, [EBX+ECX*4+0x100] = 8D 84 8B 00 01 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x8D, 0x84, 0x8B, 0x00, 0x01, 0x00, 0x00]);
    cpu.regs.ebx = 0x2000;
    cpu.regs.ecx = 0x10;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x2000 + 0x10 * 4 + 0x100);
}

#[test]
fn test_paging_push_pop_r32() {
    // PUSH EBX = 53, POP EAX = 58
    let (mut cpu, mut mem) = setup_paging(&[0x53, 0x58]);
    cpu.regs.ebx = 0xBAADF00D;
    cpu.regs.eax = 0;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eax, 0xBAADF00D);
    assert_eq!(cpu.regs.esp, 0x8000);
}

#[test]
fn test_paging_xchg_r32_mem() {
    // XCHG EAX, [0x2000] = 87 05 00 20 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x87, 0x05, 0x00, 0x20, 0x00, 0x00]);
    cpu.regs.eax = 0x11111111;
    mem.ram[0x2000..0x2004].copy_from_slice(&0x22222222u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x22222222);
    let mem_val = u32::from_le_bytes(mem.ram[0x2000..0x2004].try_into().unwrap());
    assert_eq!(mem_val, 0x11111111);
}

#[test]
fn test_paging_mul_r32() {
    // MUL EBX = F7 E3
    let (mut cpu, mut mem) = setup_paging(&[0xF7, 0xE3]);
    cpu.regs.eax = 0x10000;
    cpu.regs.ebx = 0x10000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // 0x10000 * 0x10000 = 0x1_0000_0000
    assert_eq!(cpu.regs.eax, 0x00000000);
    assert_eq!(cpu.regs.edx, 0x00000001);
}

#[test]
fn test_paging_imul_r32_r32_imm8() {
    // IMUL EAX, EBX, 10 = 6B C3 0A
    let (mut cpu, mut mem) = setup_paging(&[0x6B, 0xC3, 0x0A]);
    cpu.regs.ebx = 7;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 70);
}

#[test]
fn test_paging_div_r32() {
    // DIV EBX = F7 F3
    let (mut cpu, mut mem) = setup_paging(&[0xF7, 0xF3]);
    cpu.regs.eax = 100;
    cpu.regs.edx = 0;
    cpu.regs.ebx = 7;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 14); // quotient
    assert_eq!(cpu.regs.edx, 2); // remainder
}

#[test]
fn test_paging_not_r32() {
    // NOT EAX = F7 D0
    let (mut cpu, mut mem) = setup_paging(&[0xF7, 0xD0]);
    cpu.regs.eax = 0x0F0F0F0F;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xF0F0F0F0);
}

#[test]
fn test_paging_neg_r32() {
    // NEG EAX = F7 D8
    let (mut cpu, mut mem) = setup_paging(&[0xF7, 0xD8]);
    cpu.regs.eax = 42;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax as i32, -42);
}

#[test]
fn test_paging_inc_dec_r32() {
    // INC EAX = 40, DEC EBX = 4B
    let (mut cpu, mut mem) = setup_paging(&[0x40, 0x4B]);
    cpu.regs.eax = 99;
    cpu.regs.ebx = 50;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eax, 100);
    assert_eq!(cpu.regs.ebx, 49);
}

#[test]
fn test_paging_movzx_r32_r8() {
    // MOVZX EAX, BL = 0F B6 C3
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xB6, 0xC3]);
    cpu.regs.eax = 0xFFFFFFFF;
    cpu.regs.ebx = 0x42;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x42);
}

#[test]
fn test_paging_movsx_r32_r8() {
    // MOVSX EAX, BL = 0F BE C3
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xBE, 0xC3]);
    cpu.regs.eax = 0;
    cpu.regs.ebx = 0x80; // -128 as i8
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax as i32, -128);
}

#[test]
fn test_paging_movzx_r32_r16() {
    // MOVZX EAX, BX = 0F B7 C3
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xB7, 0xC3]);
    cpu.regs.eax = 0xFFFFFFFF;
    cpu.regs.ebx = 0x1234;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x1234);
}

#[test]
fn test_paging_movsx_r32_r16() {
    // MOVSX EAX, BX = 0F BF C3
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xBF, 0xC3]);
    cpu.regs.eax = 0;
    cpu.regs.ebx = 0x8000; // -32768
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax as i32, -32768);
}

#[test]
fn test_paging_test_r32_r32() {
    // TEST EAX, EBX = 85 C3
    let (mut cpu, mut mem) = setup_paging(&[0x85, 0xC3]);
    cpu.regs.eax = 0xFF;
    cpu.regs.ebx = 0x0F;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_paging_test_r32_r32_zero() {
    // TEST EAX, EBX = 85 C3
    let (mut cpu, mut mem) = setup_paging(&[0x85, 0xC3]);
    cpu.regs.eax = 0xFF00;
    cpu.regs.ebx = 0x00FF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_paging_call_near_ret() {
    // CALL +5 = E8 05 00 00 00 (call to 0x010A)
    // NOP NOP NOP NOP NOP (padding at 0x105-0x109)
    // RET = C3 (at 0x010A)
    let code = [
        0xE8, 0x05, 0x00, 0x00, 0x00, // CALL rel32
        0x90, 0x90, 0x90, 0x90, 0x90, // NOPs
        0xC3, // RET
    ];
    let (mut cpu, mut mem) = setup_paging(&code);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    // After CALL+RET, EIP should be at 0x105 (return address)
    assert_eq!(cpu.regs.eip, 0x0105);
}

#[test]
fn test_paging_jmp_near_rel32() {
    // JMP +5 = E9 05 00 00 00 (jump to 0x010A)
    let code = [0xE9, 0x05, 0x00, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_paging(&code);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x010A);
}

#[test]
fn test_paging_conditional_jcc_rel32() {
    // JZ +5 = 0F 84 05 00 00 00
    let code = [0x0F, 0x84, 0x05, 0x00, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_paging(&code);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x010B); // 0x100 + 6 + 5
}

#[test]
fn test_paging_rep_stosb() {
    // REP STOSB = F3 AA
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xAA]);
    cpu.regs.set_al(0xCC);
    cpu.regs.edi = 0x6000;
    cpu.regs.ecx = 32;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 128);
    for i in 0..32 {
        assert_eq!(mem.ram[0x6000 + i], 0xCC);
    }
}

#[test]
fn test_paging_rep_movsb() {
    // REP MOVSB = F3 A4
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA4]);
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x3000;
    cpu.regs.ecx = 16;
    for i in 0..16u8 {
        mem.ram[0x2000 + i as usize] = 0xA0 + i;
    }
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 128);
    for i in 0..16u8 {
        assert_eq!(mem.ram[0x3000 + i as usize], 0xA0 + i);
    }
}

// ========================================================================
// 10. LFS/LGS in protected mode
// ========================================================================

#[test]
fn test_lfs_r32_pm() {
    // LFS EAX, [0x3000] = 0F B4 05 00 30 00 00
    // Memory layout at 0x3000: dword offset (4 bytes) + word selector (2 bytes)
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xB4, 0x05, 0x00, 0x30, 0x00, 0x00]);
    // Write offset=0xDEAD at 0x3000, selector=0x10 at 0x3004
    mem.ram[0x3000..0x3004].copy_from_slice(&0xDEADu32.to_le_bytes());
    mem.ram[0x3004..0x3006].copy_from_slice(&0x10u16.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xDEAD);
    assert_eq!(cpu.regs.fs, 0x10);
}

#[test]
fn test_lgs_r32_pm() {
    // LGS EAX, [0x3000] = 0F B5 05 00 30 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xB5, 0x05, 0x00, 0x30, 0x00, 0x00]);
    mem.ram[0x3000..0x3004].copy_from_slice(&0xBEEFu32.to_le_bytes());
    mem.ram[0x3004..0x3006].copy_from_slice(&0x10u16.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xBEEF);
    assert_eq!(cpu.regs.gs, 0x10);
}

// ========================================================================
// 11. ENTER/LEAVE in PM32
// ========================================================================

#[test]
fn test_enter_level0_pm32() {
    // ENTER 0x10, 0 = C8 10 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0xC8, 0x10, 0x00, 0x00]);
    let old_esp = cpu.regs.esp;
    let old_ebp = cpu.regs.ebp;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // ENTER pushes EBP, sets EBP=ESP, then ESP-=0x10
    let expected_ebp = old_esp - 4;
    assert_eq!(cpu.regs.ebp, expected_ebp);
    assert_eq!(cpu.regs.esp, expected_ebp - 0x10);
    // Old EBP should be at [new EBP]
    let stored = u32::from_le_bytes(
        mem.ram[expected_ebp as usize..(expected_ebp + 4) as usize]
            .try_into()
            .unwrap(),
    );
    assert_eq!(stored, old_ebp);
}

#[test]
fn test_leave_pm32() {
    // Set up a stack frame, then LEAVE = C9
    let (mut cpu, mut mem) = setup_pm32(&[0xC9]);
    // Simulate a stack frame: EBP points to saved EBP on stack
    cpu.regs.ebp = 0x7FF0;
    mem.ram[0x7FF0..0x7FF4].copy_from_slice(&0x7000u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.esp, 0x7FF4); // ESP = old EBP + 4
    assert_eq!(cpu.regs.ebp, 0x7000); // EBP = [old EBP]
}

// ========================================================================
// 12. CWDE/CDQ in PM32
// ========================================================================

#[test]
fn test_cwde_pm32_positive() {
    // CWDE = 98 (in 32-bit mode)
    let (mut cpu, mut mem) = setup_pm32(&[0x98]);
    cpu.regs.eax = 0xFFFF_007F; // AX = 0x007F (positive)
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x0000_007F);
}

#[test]
fn test_cwde_pm32_negative() {
    // CWDE = 98
    let (mut cpu, mut mem) = setup_pm32(&[0x98]);
    cpu.regs.eax = 0x0000_FF80; // AX = 0xFF80 (-128)
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFFFF_FF80);
}

#[test]
fn test_cdq_pm32_positive() {
    // CDQ = 99
    let (mut cpu, mut mem) = setup_pm32(&[0x99]);
    cpu.regs.eax = 0x7FFF_FFFF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.edx, 0x00000000);
}

#[test]
fn test_cdq_pm32_negative() {
    // CDQ = 99
    let (mut cpu, mut mem) = setup_pm32(&[0x99]);
    cpu.regs.eax = 0x8000_0000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.edx, 0xFFFFFFFF);
}

// ========================================================================
// 13. LAHF/SAHF in PM32
// ========================================================================

#[test]
fn test_lahf_pm32() {
    // LAHF = 9F
    let (mut cpu, mut mem) = setup_pm32(&[0x9F]);
    cpu.regs.flags.insert(Flags::CARRY);
    cpu.regs.flags.insert(Flags::ZERO);
    cpu.regs.flags.insert(Flags::SIGN);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let ah = (cpu.regs.eax >> 8) as u8;
    assert!(ah & 0x01 != 0); // CF
    assert!(ah & 0x40 != 0); // ZF
    assert!(ah & 0x80 != 0); // SF
}

#[test]
fn test_sahf_pm32() {
    // SAHF = 9E
    let (mut cpu, mut mem) = setup_pm32(&[0x9E]);
    cpu.regs.eax = (cpu.regs.eax & 0xFFFF_00FF) | (0xC1 << 8); // AH=0xC1: SF,ZF,CF
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.flags.remove(Flags::ZERO);
    cpu.regs.flags.remove(Flags::SIGN);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert!(cpu.regs.flags.contains(Flags::SIGN));
}

// ========================================================================
// 14. XLAT in PM32
// ========================================================================

#[test]
fn test_xlat_pm32() {
    // XLATB = D7
    let (mut cpu, mut mem) = setup_pm32(&[0xD7]);
    cpu.regs.ebx = 0x5000;
    cpu.regs.set_al(0x10);
    mem.ram[0x5010] = 0x42;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x42);
}

// ========================================================================
// 15. ROL/ROR/RCL/RCR/SHL/SHR/SAR in PM32
// ========================================================================

#[test]
fn test_rol_r32_1_pm() {
    // ROL EAX, 1 = D1 C0
    let (mut cpu, mut mem) = setup_pm32(&[0xD1, 0xC0]);
    cpu.regs.eax = 0x80000001;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00000003);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_ror_r32_1_pm() {
    // ROR EAX, 1 = D1 C8
    let (mut cpu, mut mem) = setup_pm32(&[0xD1, 0xC8]);
    cpu.regs.eax = 0x00000001;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x80000000);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_rcl_r32_1_pm() {
    // RCL EAX, 1 = D1 D0
    let (mut cpu, mut mem) = setup_pm32(&[0xD1, 0xD0]);
    cpu.regs.eax = 0x80000000;
    cpu.regs.flags.remove(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00000000);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_rcr_r32_1_pm() {
    // RCR EAX, 1 = D1 D8
    let (mut cpu, mut mem) = setup_pm32(&[0xD1, 0xD8]);
    cpu.regs.eax = 0x00000001;
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x80000000);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_shl_r32_cl_pm() {
    // SHL EAX, CL = D3 E0
    let (mut cpu, mut mem) = setup_pm32(&[0xD3, 0xE0]);
    cpu.regs.eax = 0x01;
    cpu.regs.ecx = 16;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x10000);
}

#[test]
fn test_shr_r32_cl_pm() {
    // SHR EAX, CL = D3 E8
    let (mut cpu, mut mem) = setup_pm32(&[0xD3, 0xE8]);
    cpu.regs.eax = 0x10000;
    cpu.regs.ecx = 8;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x100);
}

#[test]
fn test_sar_r32_cl_pm() {
    // SAR EAX, CL = D3 F8
    let (mut cpu, mut mem) = setup_pm32(&[0xD3, 0xF8]);
    cpu.regs.eax = 0x80000000; // negative
    cpu.regs.ecx = 4;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xF8000000);
}

#[test]
fn test_shl_r32_imm8_pm() {
    // SHL EAX, 8 = C1 E0 08
    let (mut cpu, mut mem) = setup_pm32(&[0xC1, 0xE0, 0x08]);
    cpu.regs.eax = 0xFF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFF00);
}

#[test]
fn test_shr_r32_imm8_pm() {
    // SHR EAX, 8 = C1 E8 08
    let (mut cpu, mut mem) = setup_pm32(&[0xC1, 0xE8, 0x08]);
    cpu.regs.eax = 0xFF00;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFF);
}

// ========================================================================
// 16. PUSHF/POPF in PM32
// ========================================================================

#[test]
fn test_pushfd_popfd_pm32() {
    // PUSHFD = 9C, POPFD = 9D
    let (mut cpu, mut mem) = setup_pm32(&[0x9C, 0x9D]);
    cpu.regs.flags.insert(Flags::CARRY);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1); // PUSHFD
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.flags.remove(Flags::ZERO);
    let _ = run(&mut cpu, &mut mem, &mut io, 1); // POPFD
    assert!(cpu.regs.flags.contains(Flags::CARRY));
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// 17. LOOP/JECXZ in PM32
// ========================================================================

#[test]
fn test_loop_pm32() {
    // At 0x100: INC EAX = 40
    // At 0x101: LOOP -2 = E2 FD (back to 0x100)
    let (mut cpu, mut mem) = setup_pm32(&[0x40, 0xE2, 0xFD]);
    cpu.regs.eax = 0;
    cpu.regs.ecx = 5;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 20);
    assert_eq!(cpu.regs.eax, 5);
    assert_eq!(cpu.regs.ecx, 0);
}

#[test]
fn test_jecxz_pm32_taken() {
    // JECXZ +5 = E3 05 (jump if ECX==0)
    let (mut cpu, mut mem) = setup_pm32(&[0xE3, 0x05]);
    cpu.regs.ecx = 0;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x107); // 0x100 + 2 + 5
}

#[test]
fn test_jecxz_pm32_not_taken() {
    // JECXZ +5 = E3 05
    let (mut cpu, mut mem) = setup_pm32(&[0xE3, 0x05]);
    cpu.regs.ecx = 1;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x102); // fall through
}

// ========================================================================
// 18. CMPXCHG in PM32
// ========================================================================

#[test]
fn test_cmpxchg_r32_equal_pm() {
    // CMPXCHG EBX, ECX = 0F B1 CB (486+)
    let (mut cpu, mut mem) = setup_486_pm32(&[0x0F, 0xB1, 0xCB]);
    cpu.regs.eax = 0x1234;
    cpu.regs.ebx = 0x1234;
    cpu.regs.ecx = 0x5678;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ebx, 0x5678); // exchange happened
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_cmpxchg_r32_not_equal_pm() {
    // CMPXCHG EBX, ECX = 0F B1 CB (486+)
    let (mut cpu, mut mem) = setup_486_pm32(&[0x0F, 0xB1, 0xCB]);
    cpu.regs.eax = 0x1234;
    cpu.regs.ebx = 0x5678;
    cpu.regs.ecx = 0x9999;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x5678); // EAX loaded with EBX
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// 19. XADD in PM32
// ========================================================================

#[test]
fn test_xadd_r32_pm() {
    // XADD EBX, EAX = 0F C1 C3 (486+)
    let (mut cpu, mut mem) = setup_486_pm32(&[0x0F, 0xC1, 0xC3]);
    cpu.regs.ebx = 10;
    cpu.regs.eax = 20;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ebx, 30); // 10 + 20
    assert_eq!(cpu.regs.eax, 10); // old EBX
}

// ========================================================================
// 20. BSWAP in PM32
// ========================================================================

#[test]
fn test_bswap_eax_pm() {
    // BSWAP EAX = 0F C8 (486+)
    let (mut cpu, mut mem) = setup_486_pm32(&[0x0F, 0xC8]);
    cpu.regs.eax = 0x12345678;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x78563412);
}

#[test]
fn test_bswap_ecx_pm() {
    // BSWAP ECX = 0F C9 (486+)
    let (mut cpu, mut mem) = setup_486_pm32(&[0x0F, 0xC9]);
    cpu.regs.ecx = 0xAABBCCDD;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx, 0xDDCCBBAA);
}

// ========================================================================
// 21. IMUL r32,r32 (two-operand) in PM32
// ========================================================================

#[test]
fn test_imul_r32_r32_pm() {
    // IMUL EAX, EBX = 0F AF C3
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xAF, 0xC3]);
    cpu.regs.eax = 7;
    cpu.regs.ebx = 11;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 77);
}

#[test]
fn test_imul_r32_r32_negative_pm() {
    // IMUL EAX, EBX = 0F AF C3
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xAF, 0xC3]);
    cpu.regs.eax = -3i32 as u32;
    cpu.regs.ebx = 5;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax as i32, -15);
}

// ========================================================================
// 22. IMUL r32, r32, imm32 (three-operand) in PM32
// ========================================================================

#[test]
fn test_imul_r32_r32_imm32_pm() {
    // IMUL EAX, EBX, 0x100 = 69 C3 00 01 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x69, 0xC3, 0x00, 0x01, 0x00, 0x00]);
    cpu.regs.ebx = 5;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x500);
}

// ========================================================================
// 23. Paging string ops with direction flag
// ========================================================================

#[test]
fn test_paging_rep_stosb_backward() {
    // STD; REP STOSB = FD F3 AA
    let (mut cpu, mut mem) = setup_paging(&[0xFD, 0xF3, 0xAA]);
    cpu.regs.set_al(0xEE);
    cpu.regs.edi = 0x500F; // start from end
    cpu.regs.ecx = 16;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 128);
    for i in 0..16 {
        assert_eq!(mem.ram[0x5000 + i], 0xEE);
    }
}

// ========================================================================
// 24. TEST AL/EAX,imm in PM32
// ========================================================================

#[test]
fn test_test_al_imm8_pm() {
    // TEST AL, 0x0F = A8 0F
    let (mut cpu, mut mem) = setup_pm32(&[0xA8, 0x0F]);
    cpu.regs.set_al(0x10);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_test_eax_imm32_pm() {
    // TEST EAX, 0xFF = A9 FF 00 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0xA9, 0xFF, 0x00, 0x00, 0x00]);
    cpu.regs.eax = 0x100;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// 25. ADC/SBB in PM32
// ========================================================================

#[test]
fn test_adc_r32_r32_pm() {
    // ADC EAX, EBX = 11 D8
    let (mut cpu, mut mem) = setup_pm32(&[0x11, 0xD8]);
    cpu.regs.eax = 0x10;
    cpu.regs.ebx = 0x20;
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x31); // 0x10 + 0x20 + CF(1)
}

#[test]
fn test_sbb_r32_r32_pm() {
    // SBB EAX, EBX = 19 D8
    let (mut cpu, mut mem) = setup_pm32(&[0x19, 0xD8]);
    cpu.regs.eax = 0x30;
    cpu.regs.ebx = 0x10;
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x1F); // 0x30 - 0x10 - CF(1)
}

// ========================================================================
// 26. PUSHA/POPA in PM32
// ========================================================================

#[test]
fn test_pushad_popad_pm32() {
    // PUSHAD = 60, POPAD = 61
    let (mut cpu, mut mem) = setup_pm32(&[0x60, 0x61]);
    cpu.regs.eax = 0x11;
    cpu.regs.ecx = 0x22;
    cpu.regs.edx = 0x33;
    cpu.regs.ebx = 0x44;
    cpu.regs.ebp = 0x55;
    cpu.regs.esi = 0x66;
    cpu.regs.edi = 0x77;
    let old_esp = cpu.regs.esp;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eax, 0x11);
    assert_eq!(cpu.regs.ecx, 0x22);
    assert_eq!(cpu.regs.edx, 0x33);
    assert_eq!(cpu.regs.ebx, 0x44);
    assert_eq!(cpu.regs.ebp, 0x55);
    assert_eq!(cpu.regs.esi, 0x66);
    assert_eq!(cpu.regs.edi, 0x77);
    assert_eq!(cpu.regs.esp, old_esp);
}

// ========================================================================
// 27. PUSH imm8/imm32 in PM32
// ========================================================================

#[test]
fn test_push_imm8_pm32() {
    // PUSH 0x42 = 6A 42 (sign-extended to 32-bit)
    // POP EAX = 58
    let (mut cpu, mut mem) = setup_pm32(&[0x6A, 0x42, 0x58]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eax, 0x42);
}

#[test]
fn test_push_imm8_negative_pm32() {
    // PUSH -1 = 6A FF (sign-extended to 0xFFFFFFFF)
    // POP EAX = 58
    let (mut cpu, mut mem) = setup_pm32(&[0x6A, 0xFF, 0x58]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eax, 0xFFFFFFFF);
}

#[test]
fn test_push_imm32_pm32() {
    // PUSH 0xDEADBEEF = 68 EF BE AD DE, POP EAX = 58
    let (mut cpu, mut mem) = setup_pm32(&[0x68, 0xEF, 0xBE, 0xAD, 0xDE, 0x58]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eax, 0xDEADBEEF);
}

// ========================================================================
// 28. MOV segment register in PM32
// ========================================================================

#[test]
fn test_mov_es_r16_pm32() {
    // MOV ES, AX = 8E C0
    let (mut cpu, mut mem) = setup_pm32(&[0x8E, 0xC0]);
    cpu.regs.eax = 0x10; // valid data segment selector
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.es, 0x10);
}

#[test]
fn test_mov_ds_r16_pm32() {
    // MOV DS, AX = 8E D8
    let (mut cpu, mut mem) = setup_pm32(&[0x8E, 0xD8]);
    cpu.regs.eax = 0x10;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ds, 0x10);
}

// ========================================================================
// 29. MMX packed operations (more)
// ========================================================================

#[test]
fn test_mmx_pcmpgtb() {
    // PCMPGTB MM0, MM1 = 0F 64 C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0x64, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0x05040302; // bytes: 02,03,04,05
    cpu.regs.ebx = 0x03030303; // bytes: 03,03,03,03
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    // byte 0: 02 > 03? No -> 00, byte 1: 03 > 03? No -> 00
    // byte 2: 04 > 03? Yes -> FF, byte 3: 05 > 03? Yes -> FF
    assert_eq!(cpu.mmx_reg(0) as u32, 0xFFFF0000);
}

#[test]
fn test_mmx_pcmpgtw() {
    // PCMPGTW MM0, MM1 = 0F 65 C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0x65, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0x00050003; // words: 0003, 0005
    cpu.regs.ebx = 0x00040004; // words: 0004, 0004
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    // word 0: 0003 > 0004? No -> 0000, word 1: 0005 > 0004? Yes -> FFFF
    assert_eq!(cpu.mmx_reg(0) as u32, 0xFFFF0000);
}

#[test]
fn test_mmx_pcmpgtd() {
    // PCMPGTD MM0, MM1 = 0F 66 C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0x66, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0x00000010;
    cpu.regs.ebx = 0x00000005;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    // 0x10 > 0x05 -> FFFFFFFF
    assert_eq!(cpu.mmx_reg(0) as u32, 0xFFFFFFFF);
}

#[test]
fn test_mmx_psubw() {
    // PSUBW MM0, MM1 = 0F F9 C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0xF9, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0x000A0014; // words: 0x0014=20, 0x000A=10
    cpu.regs.ebx = 0x00030005; // words: 0x0005=5, 0x0003=3
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    // word 0: 20-5=15, word 1: 10-3=7
    assert_eq!(cpu.mmx_reg(0) as u32, 0x0007000F);
}

#[test]
fn test_mmx_psubd() {
    // PSUBD MM0, MM1 = 0F FA C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0xFA, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0x00000064; // 100
    cpu.regs.ebx = 0x00000019; // 25
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    assert_eq!(cpu.mmx_reg(0) as u32, 75);
}

#[test]
fn test_mmx_psllw_reg() {
    // PSLLW MM0, MM1 = 0F F1 C1 (shift by count in MM1)
    let code = [
        0x0F, 0x6E, 0xC0, // MOVD MM0, EAX
        0x0F, 0x6E, 0xCB, // MOVD MM1, EBX
        0x0F, 0xF1, 0xC1, // PSLLW MM0, MM1
    ];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0x00010002; // words: 0x0002, 0x0001
    cpu.regs.ebx = 4; // shift count
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    // word 0: 2<<4=32, word 1: 1<<4=16
    assert_eq!(cpu.mmx_reg(0) as u32, 0x00100020);
}

#[test]
fn test_mmx_psrlw_reg() {
    // PSRLW MM0, MM1 = 0F D1 C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0xD1, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0x00100020; // words: 0x0020=32, 0x0010=16
    cpu.regs.ebx = 4; // shift count
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    assert_eq!(cpu.mmx_reg(0) as u32, 0x00010002);
}

#[test]
fn test_mmx_pslld_reg() {
    // PSLLD MM0, MM1 = 0F F2 C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0xF2, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0x00000001;
    cpu.regs.ebx = 8; // shift count
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    assert_eq!(cpu.mmx_reg(0) as u32, 0x00000100);
}

#[test]
fn test_mmx_psrld_reg() {
    // PSRLD MM0, MM1 = 0F D2 C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0xD2, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0x00000100;
    cpu.regs.ebx = 8; // shift count
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    assert_eq!(cpu.mmx_reg(0) as u32, 0x00000001);
}

#[test]
fn test_mmx_psllq_reg() {
    // PSLLQ MM0, MM1 = 0F F3 C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0xF3, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0x00000001;
    cpu.regs.ebx = 16; // shift count
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    assert_eq!(cpu.mmx_reg(0), 0x00010000);
}

#[test]
fn test_mmx_psrlq_reg() {
    // PSRLQ MM0, MM1 = 0F D3 C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0xD3, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0x00010000;
    cpu.regs.ebx = 16; // shift count
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    assert_eq!(cpu.mmx_reg(0), 0x00000001);
}

// ========================================================================
// 30. MMX in PM32 (Pentium protected mode)
// ========================================================================

#[test]
fn test_mmx_movd_reg_to_mm_pm32() {
    // MOVD MM0, EAX = 0F 6E C0
    let (mut cpu, mut mem) = setup_pentium_pm32(&[0x0F, 0x6E, 0xC0]);
    cpu.regs.eax = 0xBEEFCAFE;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.mmx_reg(0), 0xBEEFCAFE);
}

#[test]
fn test_mmx_pand_pm32() {
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0xDB, 0xC1];
    let (mut cpu, mut mem) = setup_pentium_pm32(&code);
    cpu.regs.eax = 0xF0F0F0F0;
    cpu.regs.ebx = 0x0F0F0F0F;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    assert_eq!(cpu.mmx_reg(0) as u32, 0x00000000);
}

// ========================================================================
// 31. FPU in PM32
// ========================================================================

#[test]
fn test_fpu_fld_fstp_pm32() {
    // FLD dword [0x2000] = D9 05 00 20 00 00
    // FSTP dword [0x2010] = D9 1D 10 20 00 00
    let code = [
        0xD9, 0x05, 0x00, 0x20, 0x00, 0x00, 0xD9, 0x1D, 0x10, 0x20, 0x00, 0x00,
    ];
    let (mut cpu, mut mem) = setup_pm32(&code);
    let val: f32 = 2.718;
    mem.ram[0x2000..0x2004].copy_from_slice(&val.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    let result = f32::from_le_bytes(mem.ram[0x2010..0x2014].try_into().unwrap());
    assert!((result - 2.718).abs() < 0.01);
}

#[test]
fn test_fpu_fadd_mem_pm32() {
    // FLD dword [0x2000] = D9 05 00 20 00 00
    // FADD dword [0x2004] = D8 05 04 20 00 00
    // FSTP dword [0x2010] = D9 1D 10 20 00 00
    let code = [
        0xD9, 0x05, 0x00, 0x20, 0x00, 0x00, // FLD [0x2000]
        0xD8, 0x05, 0x04, 0x20, 0x00, 0x00, // FADD [0x2004]
        0xD9, 0x1D, 0x10, 0x20, 0x00, 0x00, // FSTP [0x2010]
    ];
    let (mut cpu, mut mem) = setup_pm32(&code);
    let a: f32 = 10.0;
    let b: f32 = 25.0;
    mem.ram[0x2000..0x2004].copy_from_slice(&a.to_le_bytes());
    mem.ram[0x2004..0x2008].copy_from_slice(&b.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    let result = f32::from_le_bytes(mem.ram[0x2010..0x2014].try_into().unwrap());
    assert!((result - 35.0).abs() < 0.01);
}

// ========================================================================
// 32. Paging: BSF/BSR/BT with paging
// ========================================================================

#[test]
fn test_paging_bsf_r32() {
    // BSF EAX, EBX = 0F BC C3
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xBC, 0xC3]);
    cpu.regs.ebx = 0x0000_0010;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 4);
}

#[test]
fn test_paging_bsr_r32() {
    // BSR EAX, EBX = 0F BD C3
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xBD, 0xC3]);
    cpu.regs.ebx = 0x0000_0100;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 8);
}

#[test]
fn test_paging_bt_r32_imm8() {
    // BT EAX, 24 = 0F BA E0 18
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xBA, 0xE0, 0x18]);
    cpu.regs.eax = 0x0100_0000; // bit 24
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

// ========================================================================
// 33. Paging: MOVSX/MOVZX from memory
// ========================================================================

#[test]
fn test_paging_movzx_r32_mem8() {
    // MOVZX EAX, byte [0x3000] = 0F B6 05 00 30 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xB6, 0x05, 0x00, 0x30, 0x00, 0x00]);
    mem.ram[0x3000] = 0x80;
    cpu.regs.eax = 0xFFFFFFFF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x80);
}

#[test]
fn test_paging_movsx_r32_mem8() {
    // MOVSX EAX, byte [0x3000] = 0F BE 05 00 30 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xBE, 0x05, 0x00, 0x30, 0x00, 0x00]);
    mem.ram[0x3000] = 0x80;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax as i32, -128);
}

// ========================================================================
// 34. Paging: SHLD/SHRD
// ========================================================================

#[test]
fn test_paging_shld_r32_imm8() {
    // SHLD EAX, EBX, 8 = 0F A4 D8 08
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xA4, 0xD8, 0x08]);
    cpu.regs.eax = 0x12345678;
    cpu.regs.ebx = 0xAB000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x345678AB);
}

#[test]
fn test_paging_shrd_r32_imm8() {
    // SHRD EAX, EBX, 8 = 0F AC D8 08
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xAC, 0xD8, 0x08]);
    cpu.regs.eax = 0x12345678;
    cpu.regs.ebx = 0x000000AB;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xAB123456);
}

// ========================================================================
// 35. Paging: BSWAP, XADD, CMPXCHG
// ========================================================================

#[test]
fn test_paging_bswap() {
    // BSWAP EAX = 0F C8 (486+)
    let (mut cpu, mut mem) = setup_486_paging(&[0x0F, 0xC8]);
    cpu.regs.eax = 0xAABBCCDD;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xDDCCBBAA);
}

#[test]
fn test_paging_xadd() {
    // XADD EBX, EAX = 0F C1 C3 (486+)
    let (mut cpu, mut mem) = setup_486_paging(&[0x0F, 0xC1, 0xC3]);
    cpu.regs.ebx = 100;
    cpu.regs.eax = 50;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ebx, 150);
    assert_eq!(cpu.regs.eax, 100);
}

#[test]
fn test_paging_cmpxchg_equal() {
    // CMPXCHG EBX, ECX = 0F B1 CB (486+)
    let (mut cpu, mut mem) = setup_486_paging(&[0x0F, 0xB1, 0xCB]);
    cpu.regs.eax = 42;
    cpu.regs.ebx = 42;
    cpu.regs.ecx = 99;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ebx, 99);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// 36. Paging: SETcc, CMOVcc
// ========================================================================

#[test]
fn test_paging_sete_r8() {
    // SETE AL = 0F 94 C0
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0x94, 0xC0]);
    cpu.regs.flags.insert(Flags::ZERO);
    cpu.regs.eax = 0;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 1);
}

#[test]
fn test_paging_cmovz_r32_is_ud_on_pentium() {
    // CMOVZ EAX, EBX = 0F 44 C3 (P6+)
    let (mut cpu, mut mem) = setup_pentium_paging(&[0x0F, 0x44, 0xC3]);
    cpu.regs.flags.insert(Flags::ZERO);
    cpu.regs.eax = 0;
    cpu.regs.ebx = 0x12345678;
    let saved_flags = cpu.regs.flags;
    let saved_cr3 = cpu.regs.cr3;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 6,
            error_code: 0,
            fault_addr: None,
            executed: 0
        }
    ));
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.eax, 0);
    assert_eq!(cpu.regs.ebx, 0x12345678);
    assert_eq!(cpu.regs.flags, saved_flags);
    assert_eq!(cpu.regs.cr3, saved_cr3);
}

// ========================================================================
// 37. Paging: IMUL variants
// ========================================================================

#[test]
fn test_paging_imul_r32_r32() {
    // IMUL EAX, EBX = 0F AF C3
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xAF, 0xC3]);
    cpu.regs.eax = 13;
    cpu.regs.ebx = 7;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 91);
}

#[test]
fn test_paging_imul_r32_imm8() {
    // IMUL EAX, EBX, 3 = 6B C3 03
    let (mut cpu, mut mem) = setup_paging(&[0x6B, 0xC3, 0x03]);
    cpu.regs.ebx = 100;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 300);
}

// ========================================================================
// 38. Paging: FPU
// ========================================================================

#[test]
fn test_paging_fpu_fld_fstp() {
    // FLD dword [0x2000] = D9 05 00 20 00 00
    // FSTP dword [0x2010] = D9 1D 10 20 00 00
    let code = [
        0xD9, 0x05, 0x00, 0x20, 0x00, 0x00, 0xD9, 0x1D, 0x10, 0x20, 0x00, 0x00,
    ];
    let (mut cpu, mut mem) = setup_paging(&code);
    let val: f32 = 1.414;
    mem.ram[0x2000..0x2004].copy_from_slice(&val.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    let result = f32::from_le_bytes(mem.ram[0x2010..0x2014].try_into().unwrap());
    assert!((result - 1.414).abs() < 0.01);
}

// ========================================================================
// 39. MOV r32, mem with SIB in paging mode
// ========================================================================

#[test]
fn test_paging_mov_r32_sib() {
    // MOV EAX, [EBX+ECX*4] = 8B 04 8B
    let (mut cpu, mut mem) = setup_paging(&[0x8B, 0x04, 0x8B]);
    cpu.regs.ebx = 0x3000;
    cpu.regs.ecx = 0x10;
    mem.ram[0x3040..0x3044].copy_from_slice(&0xABCD1234u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xABCD1234);
}

// ========================================================================
// 40. ADC/SBB with memory in paging mode
// ========================================================================

#[test]
fn test_paging_adc_r32_mem() {
    // ADC EAX, [0x2000] = 13 05 00 20 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x13, 0x05, 0x00, 0x20, 0x00, 0x00]);
    cpu.regs.eax = 10;
    mem.ram[0x2000..0x2004].copy_from_slice(&20u32.to_le_bytes());
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 31); // 10 + 20 + 1
}

#[test]
fn test_paging_sbb_r32_mem() {
    // SBB EAX, [0x2000] = 1B 05 00 20 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x1B, 0x05, 0x00, 0x20, 0x00, 0x00]);
    cpu.regs.eax = 100;
    mem.ram[0x2000..0x2004].copy_from_slice(&30u32.to_le_bytes());
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 69); // 100 - 30 - 1
}

// ========================================================================
// 41. Paging: ROL/ROR memory operands
// ========================================================================

#[test]
fn test_paging_rol_mem32_1() {
    // ROL dword [0x2000], 1 = D1 05 00 20 00 00
    let (mut cpu, mut mem) = setup_paging(&[0xD1, 0x05, 0x00, 0x20, 0x00, 0x00]);
    mem.ram[0x2000..0x2004].copy_from_slice(&0x80000001u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes(mem.ram[0x2000..0x2004].try_into().unwrap());
    assert_eq!(val, 0x00000003);
}

#[test]
fn test_paging_ror_mem32_1() {
    // ROR dword [0x2000], 1 = D1 0D 00 20 00 00
    let (mut cpu, mut mem) = setup_paging(&[0xD1, 0x0D, 0x00, 0x20, 0x00, 0x00]);
    mem.ram[0x2000..0x2004].copy_from_slice(&0x00000001u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes(mem.ram[0x2000..0x2004].try_into().unwrap());
    assert_eq!(val, 0x80000000);
}

// ========================================================================
// 42. Paging: INC/DEC memory
// ========================================================================

#[test]
fn test_paging_inc_mem32() {
    // INC dword [0x2000] = FF 05 00 20 00 00
    let (mut cpu, mut mem) = setup_paging(&[0xFF, 0x05, 0x00, 0x20, 0x00, 0x00]);
    mem.ram[0x2000..0x2004].copy_from_slice(&99u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes(mem.ram[0x2000..0x2004].try_into().unwrap());
    assert_eq!(val, 100);
}

#[test]
fn test_paging_dec_mem32() {
    // DEC dword [0x2000] = FF 0D 00 20 00 00
    let (mut cpu, mut mem) = setup_paging(&[0xFF, 0x0D, 0x00, 0x20, 0x00, 0x00]);
    mem.ram[0x2000..0x2004].copy_from_slice(&100u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes(mem.ram[0x2000..0x2004].try_into().unwrap());
    assert_eq!(val, 99);
}

// ========================================================================
// 43. Paging: NOT/NEG memory
// ========================================================================

#[test]
fn test_paging_not_mem32() {
    // NOT dword [0x2000] = F7 15 00 20 00 00
    let (mut cpu, mut mem) = setup_paging(&[0xF7, 0x15, 0x00, 0x20, 0x00, 0x00]);
    mem.ram[0x2000..0x2004].copy_from_slice(&0x0F0F0F0Fu32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes(mem.ram[0x2000..0x2004].try_into().unwrap());
    assert_eq!(val, 0xF0F0F0F0);
}

#[test]
fn test_paging_neg_mem32() {
    // NEG dword [0x2000] = F7 1D 00 20 00 00
    let (mut cpu, mut mem) = setup_paging(&[0xF7, 0x1D, 0x00, 0x20, 0x00, 0x00]);
    mem.ram[0x2000..0x2004].copy_from_slice(&42u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = i32::from_le_bytes(mem.ram[0x2000..0x2004].try_into().unwrap());
    assert_eq!(val, -42);
}

// ========================================================================
// 44. Paging: ADD/SUB/AND/OR/XOR to memory
// ========================================================================

#[test]
fn test_paging_add_mem32_r32() {
    // ADD [0x2000], EAX = 01 05 00 20 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x01, 0x05, 0x00, 0x20, 0x00, 0x00]);
    cpu.regs.eax = 5;
    mem.ram[0x2000..0x2004].copy_from_slice(&10u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes(mem.ram[0x2000..0x2004].try_into().unwrap());
    assert_eq!(val, 15);
}

#[test]
fn test_paging_sub_mem32_r32() {
    // SUB [0x2000], EAX = 29 05 00 20 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x29, 0x05, 0x00, 0x20, 0x00, 0x00]);
    cpu.regs.eax = 3;
    mem.ram[0x2000..0x2004].copy_from_slice(&10u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes(mem.ram[0x2000..0x2004].try_into().unwrap());
    assert_eq!(val, 7);
}

#[test]
fn test_paging_and_mem32_r32() {
    // AND [0x2000], EAX = 21 05 00 20 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x21, 0x05, 0x00, 0x20, 0x00, 0x00]);
    cpu.regs.eax = 0x0F0F0F0F;
    mem.ram[0x2000..0x2004].copy_from_slice(&0xFF00FF00u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes(mem.ram[0x2000..0x2004].try_into().unwrap());
    assert_eq!(val, 0x0F000F00);
}

#[test]
fn test_paging_or_mem32_r32() {
    // OR [0x2000], EAX = 09 05 00 20 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x09, 0x05, 0x00, 0x20, 0x00, 0x00]);
    cpu.regs.eax = 0x00FF;
    mem.ram[0x2000..0x2004].copy_from_slice(&0xFF00u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes(mem.ram[0x2000..0x2004].try_into().unwrap());
    assert_eq!(val, 0xFFFF);
}

#[test]
fn test_paging_xor_mem32_r32() {
    // XOR [0x2000], EAX = 31 05 00 20 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x31, 0x05, 0x00, 0x20, 0x00, 0x00]);
    cpu.regs.eax = 0xFFFFFFFF;
    mem.ram[0x2000..0x2004].copy_from_slice(&0xAA55AA55u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes(mem.ram[0x2000..0x2004].try_into().unwrap());
    assert_eq!(val, 0x55AA55AA);
}

// ========================================================================
// 45. Paging: ENTER/LEAVE
// ========================================================================

#[test]
fn test_paging_enter_level0() {
    // ENTER 0x20, 0 = C8 20 00 00
    let (mut cpu, mut mem) = setup_paging(&[0xC8, 0x20, 0x00, 0x00]);
    let old_esp = cpu.regs.esp;
    let old_ebp = cpu.regs.ebp;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let expected_ebp = old_esp - 4;
    assert_eq!(cpu.regs.ebp, expected_ebp);
    assert_eq!(cpu.regs.esp, expected_ebp - 0x20);
    let stored = u32::from_le_bytes(
        mem.ram[expected_ebp as usize..(expected_ebp + 4) as usize]
            .try_into()
            .unwrap(),
    );
    assert_eq!(stored, old_ebp);
}

#[test]
fn test_paging_leave() {
    // LEAVE = C9
    let (mut cpu, mut mem) = setup_paging(&[0xC9]);
    cpu.regs.ebp = 0x7FF0;
    mem.ram[0x7FF0..0x7FF4].copy_from_slice(&0x6000u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.esp, 0x7FF4);
    assert_eq!(cpu.regs.ebp, 0x6000);
}

// ========================================================================
// 46. Paging: LOOP/JECXZ
// ========================================================================

#[test]
fn test_paging_loop() {
    // INC EAX = 40, LOOP -2 = E2 FD
    let (mut cpu, mut mem) = setup_paging(&[0x40, 0xE2, 0xFD]);
    cpu.regs.eax = 0;
    cpu.regs.ecx = 3;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 20);
    assert_eq!(cpu.regs.eax, 3);
}

#[test]
fn test_paging_jecxz_taken() {
    // JECXZ +3 = E3 03
    let (mut cpu, mut mem) = setup_paging(&[0xE3, 0x03]);
    cpu.regs.ecx = 0;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x105);
}

// ========================================================================
// 47. Paging: LAHF/SAHF
// ========================================================================

#[test]
fn test_paging_lahf() {
    // LAHF = 9F
    let (mut cpu, mut mem) = setup_paging(&[0x9F]);
    cpu.regs.flags.insert(Flags::CARRY);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let ah = (cpu.regs.eax >> 8) as u8;
    assert!(ah & 0x01 != 0); // CF
    assert!(ah & 0x40 != 0); // ZF
}

// ========================================================================
// 48. PM32: ALU imm8 sign-extended to 32
// ========================================================================

#[test]
fn test_add_r32_imm8_sx_pm() {
    // ADD EAX, -1 = 83 C0 FF (sign-extend 0xFF to 0xFFFFFFFF)
    let (mut cpu, mut mem) = setup_pm32(&[0x83, 0xC0, 0xFF]);
    cpu.regs.eax = 10;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 9);
}

#[test]
fn test_sub_r32_imm8_sx_pm() {
    // SUB EAX, 5 = 83 E8 05
    let (mut cpu, mut mem) = setup_pm32(&[0x83, 0xE8, 0x05]);
    cpu.regs.eax = 100;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 95);
}

#[test]
fn test_cmp_r32_imm8_sx_pm() {
    // CMP EAX, 0 = 83 F8 00
    let (mut cpu, mut mem) = setup_pm32(&[0x83, 0xF8, 0x00]);
    cpu.regs.eax = 0;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_and_r32_imm8_sx_pm() {
    // AND EAX, 0x0F = 83 E0 0F
    let (mut cpu, mut mem) = setup_pm32(&[0x83, 0xE0, 0x0F]);
    cpu.regs.eax = 0xFF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x0F);
}

#[test]
fn test_or_r32_imm8_sx_pm() {
    // OR EAX, -1 = 83 C8 FF
    let (mut cpu, mut mem) = setup_pm32(&[0x83, 0xC8, 0xFF]);
    cpu.regs.eax = 0;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFFFFFFFF);
}

#[test]
fn test_xor_r32_imm8_sx_pm() {
    // XOR EAX, -1 = 83 F0 FF
    let (mut cpu, mut mem) = setup_pm32(&[0x83, 0xF0, 0xFF]);
    cpu.regs.eax = 0;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFFFFFFFF);
}

// ========================================================================
// 49. PM32: MOV EAX, moffs32 / MOV moffs32, EAX
// ========================================================================

#[test]
fn test_mov_eax_moffs32_pm() {
    // MOV EAX, [0x00004000] = A1 00 40 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0xA1, 0x00, 0x40, 0x00, 0x00]);
    mem.ram[0x4000..0x4004].copy_from_slice(&0x99887766u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x99887766);
}

#[test]
fn test_mov_moffs32_eax_pm() {
    // MOV [0x00004000], EAX = A3 00 40 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0xA3, 0x00, 0x40, 0x00, 0x00]);
    cpu.regs.eax = 0xBAADF00D;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes(mem.ram[0x4000..0x4004].try_into().unwrap());
    assert_eq!(val, 0xBAADF00D);
}

// ========================================================================
// 50. Paging: XLAT
// ========================================================================

#[test]
fn test_paging_xlat() {
    // XLATB = D7
    let (mut cpu, mut mem) = setup_paging(&[0xD7]);
    cpu.regs.ebx = 0x4000;
    cpu.regs.set_al(0x20);
    mem.ram[0x4020] = 0x99;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x99);
}

// ========================================================================
// 51. Paging: PUSHF/POPF roundtrip
// ========================================================================

#[test]
fn test_paging_pushfd_popfd() {
    // PUSHFD=9C, POPFD=9D
    let (mut cpu, mut mem) = setup_paging(&[0x9C, 0x9D]);
    cpu.regs.flags.insert(Flags::CARRY);
    cpu.regs.flags.insert(Flags::SIGN);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1); // PUSHFD
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.flags.remove(Flags::SIGN);
    let _ = run(&mut cpu, &mut mem, &mut io, 1); // POPFD
    assert!(cpu.regs.flags.contains(Flags::CARRY));
    assert!(cpu.regs.flags.contains(Flags::SIGN));
}

// ========================================================================
// 52. Paging: PUSHAD/POPAD roundtrip
// ========================================================================

#[test]
fn test_paging_pushad_popad_roundtrip() {
    // PUSHAD=60, POPAD=61
    let (mut cpu, mut mem) = setup_paging(&[0x60, 0x61]);
    cpu.regs.eax = 0xAA;
    cpu.regs.ebx = 0xBB;
    cpu.regs.ecx = 0xCC;
    cpu.regs.edx = 0xDD;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eax, 0xAA);
    assert_eq!(cpu.regs.ebx, 0xBB);
    assert_eq!(cpu.regs.ecx, 0xCC);
    assert_eq!(cpu.regs.edx, 0xDD);
}

// ========================================================================
// 53. Paging: CWDE/CDQ
// ========================================================================

#[test]
fn test_paging_cwde_negative() {
    // CWDE = 98
    let (mut cpu, mut mem) = setup_paging(&[0x98]);
    cpu.regs.eax = 0x0000_8000; // AX = -32768
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFFFF_8000);
}

#[test]
fn test_paging_cdq_negative() {
    // CDQ = 99
    let (mut cpu, mut mem) = setup_paging(&[0x99]);
    cpu.regs.eax = 0x80000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.edx, 0xFFFFFFFF);
}

// ========================================================================
// 54. MMX: PADDSB (saturating signed byte add)
// ========================================================================

#[test]
fn test_mmx_paddsb() {
    // PADDSB MM0, MM1 = 0F EC C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0xEC, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0x7F017F01; // bytes: 01,7F,01,7F
    cpu.regs.ebx = 0x01010101;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    // byte 0: 01+01=02, byte 1: 7F+01=saturate to 7F, byte 2: 01+01=02, byte 3: 7F+01=7F
    assert_eq!(cpu.mmx_reg(0) as u32, 0x7F027F02);
}

// ========================================================================
// 55. MMX: PSUBUSB (saturating unsigned byte subtract)
// ========================================================================

#[test]
fn test_mmx_psubusb() {
    // PSUBUSB MM0, MM1 = 0F D8 C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0xD8, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0x05000A02; // bytes: 02, 0A, 00, 05
    cpu.regs.ebx = 0x03000503; // bytes: 03, 05, 00, 03
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    // byte 0: max(2-3,0)=0, byte 1: 0A-05=05, byte 2: 0-0=0, byte 3: max(5-3,0)=2
    assert_eq!(cpu.mmx_reg(0) as u32, 0x02000500);
}

// ========================================================================
// 56. MMX: PADDUSB (saturating unsigned byte add)
// ========================================================================

#[test]
fn test_mmx_paddusb() {
    // PADDUSB MM0, MM1 = 0F DC C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0xDC, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0xFF000102; // bytes: 02, 01, 00, FF
    cpu.regs.ebx = 0x01000101; // bytes: 01, 01, 00, 01
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    // byte 0: min(2+1,255)=3, byte 1: 1+1=2, byte 2: 0+0=0, byte 3: min(255+1,255)=255
    assert_eq!(cpu.mmx_reg(0) as u32, 0xFF000203);
}

// ========================================================================
// 57. MMX: PMULHW (packed signed multiply high)
// ========================================================================

#[test]
fn test_mmx_pmulhw() {
    // PMULHW MM0, MM1 = 0F E5 C1
    let code = [0x0F, 0x6E, 0xC0, 0x0F, 0x6E, 0xCB, 0x0F, 0xE5, 0xC1];
    let (mut cpu, mut mem) = setup_pentium(&code);
    cpu.regs.eax = 0x01000100; // words: 0x0100=256, 0x0100=256
    cpu.regs.ebx = 0x01000100;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    // 256*256=65536 -> high word = 1
    assert_eq!(cpu.mmx_reg(0) as u32, 0x00010001);
}

// ========================================================================
// 58. FPU: FSIN
// ========================================================================

#[test]
fn test_fpu_fsin() {
    // FLDPI, FSIN = D9 FE, FSTP [0x210]
    let code = [
        0xD9, 0xEB, // FLDPI
        0xD9, 0xFE, // FSIN
        0xD9, 0x1E, 0x10, 0x02, // FSTP [0x210]
    ];
    let (mut cpu, mut mem) = setup(&code);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    // sin(pi) should be ~0.0
    assert!(result.abs() < 0.001);
}

// ========================================================================
// 59. FPU: FCOS
// ========================================================================

#[test]
fn test_fpu_fcos() {
    // FLDZ, FCOS = D9 FF, FSTP [0x210]
    let code = [
        0xD9, 0xEE, // FLDZ
        0xD9, 0xFF, // FCOS
        0xD9, 0x1E, 0x10, 0x02, // FSTP [0x210]
    ];
    let (mut cpu, mut mem) = setup(&code);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    // cos(0) = 1.0
    assert!((result - 1.0).abs() < 0.001);
}

// ========================================================================
// 60. FPU: F2XM1
// ========================================================================

#[test]
fn test_fpu_f2xm1() {
    // FLD1, F2XM1 = D9 F0, FSTP [0x210]
    // 2^1 - 1 = 1.0
    let code = [
        0xD9, 0xE8, // FLD1
        0xD9, 0xF0, // F2XM1
        0xD9, 0x1E, 0x10, 0x02, // FSTP [0x210]
    ];
    let (mut cpu, mut mem) = setup(&code);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    assert!((result - 1.0).abs() < 0.001);
}

// ========================================================================
// 61. FPU: FXAM
// ========================================================================

#[test]
fn test_fpu_fxam_zero() {
    // FLDZ, FXAM = D9 E5, FNSTSW AX = DF E0
    let code = [
        0xD9, 0xEE, // FLDZ
        0xD9, 0xE5, // FXAM
        0xDF, 0xE0, // FNSTSW AX
    ];
    let (mut cpu, mut mem) = setup(&code);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    // FXAM: zero => C3=1, C2=0, C0=0 (class 4 = zero)
    let sw = cpu.regs.eax as u16;
    assert_eq!(sw & 0x4500, 0x4000); // C3=1
}

// ========================================================================
// 62. FPU: FDECSTP / FINCSTP
// ========================================================================

#[test]
fn test_fpu_fdecstp_fincstp() {
    // FDECSTP = D9 F6, FINCSTP = D9 F7
    let code = [0xD9, 0xF6, 0xD9, 0xF7];
    let (mut cpu, mut mem) = setup(&code);
    let initial_top = (cpu.fpu.status_word >> 11) & 7;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1); // FDECSTP
    let after_dec = (cpu.fpu.status_word >> 11) & 7;
    assert_eq!(after_dec, (initial_top.wrapping_sub(1)) & 7);
    let _ = run(&mut cpu, &mut mem, &mut io, 1); // FINCSTP
    let after_inc = (cpu.fpu.status_word >> 11) & 7;
    assert_eq!(after_inc, initial_top);
}

// ========================================================================
// 63. FPU: FLDL2E
// ========================================================================

#[test]
fn test_fpu_fldl2e() {
    // FLDL2E = D9 EA, FSTP [0x210]
    let code = [0xD9, 0xEA, 0xD9, 0x1E, 0x10, 0x02];
    let (mut cpu, mut mem) = setup(&code);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    // log2(e) ~ 1.4427
    assert!((result - std::f64::consts::LOG2_E as f32).abs() < 0.001);
}

// ========================================================================
// 64. FPU: FLDL2T
// ========================================================================

#[test]
fn test_fpu_fldl2t() {
    // FLDL2T = D9 E9, FSTP [0x210]
    let code = [0xD9, 0xE9, 0xD9, 0x1E, 0x10, 0x02];
    let (mut cpu, mut mem) = setup(&code);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    // log2(10) ~ 3.3219
    assert!((result - (10.0f64.log2() as f32)).abs() < 0.01);
}

// ========================================================================
// 65. FPU: FLDLG2
// ========================================================================

#[test]
fn test_fpu_fldlg2() {
    // FLDLG2 = D9 EC, FSTP [0x210]
    let code = [0xD9, 0xEC, 0xD9, 0x1E, 0x10, 0x02];
    let (mut cpu, mut mem) = setup(&code);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    // log10(2) ~ 0.30103
    assert!((result - std::f64::consts::LOG10_2 as f32).abs() < 0.001);
}

// ========================================================================
// 66. FPU: FLDLN2
// ========================================================================

#[test]
fn test_fpu_fldln2() {
    // FLDLN2 = D9 ED, FSTP [0x210]
    let code = [0xD9, 0xED, 0xD9, 0x1E, 0x10, 0x02];
    let (mut cpu, mut mem) = setup(&code);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    let result = f32::from_le_bytes(mem.ram[0x210..0x214].try_into().unwrap());
    // ln(2) ~ 0.6931
    assert!((result - std::f64::consts::LN_2 as f32).abs() < 0.001);
}
