use gp64_cpu::{
    x87_decode_cache::X87DecodeCache, x87_ops::F80, Cpu, CpuType, Flags, SegmentCache, CR4_OSFXSR,
};
use gp64_interp::RunResult;
use gp64_memory::Memory;

const CODE_BASE: usize = 0x1_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpectedTerminal {
    Budget,
    Halt,
}

fn sync_real_mode_segment_caches(cpu: &mut Cpu) {
    cpu.regs.cs_cache = SegmentCache::real_mode(cpu.regs.cs);
    cpu.regs.ds_cache = SegmentCache::real_mode(cpu.regs.ds);
    cpu.regs.es_cache = SegmentCache::real_mode(cpu.regs.es);
    cpu.regs.ss_cache = SegmentCache::real_mode(cpu.regs.ss);
    cpu.cs_bits = 16;
}

fn base_cpu() -> Cpu {
    let mut cpu = Cpu::new();
    cpu.regs.cs = 0x1000;
    cpu.regs.set_ip(0);
    cpu.regs.eip = 0;
    cpu.regs.ss = 0x2000;
    cpu.regs.esp = 0xfffe;
    cpu.regs.set_sp(0xfffe);
    cpu.regs.ds = 0x3000;
    sync_real_mode_segment_caches(&mut cpu);
    cpu
}

fn install(mem: &mut Memory, code: &[u8]) {
    for (i, &byte) in code.iter().enumerate() {
        mem.write_u8(CODE_BASE + i, byte);
    }
}

pub fn run_bounded(
    cpu: &mut Cpu,
    mem: &mut Memory,
    expected_retired: u32,
    terminal: ExpectedTerminal,
) -> u32 {
    assert!(
        expected_retired > 0,
        "benchmark retirement bound must be positive"
    );
    let mut retired = 0u32;
    loop {
        let remaining = expected_retired
            .checked_sub(retired)
            .expect("benchmark retired beyond its architectural bound");
        assert!(
            remaining > 0,
            "benchmark reached {expected_retired} instructions without expected {terminal:?} exit at {:04x}:{:04x}",
            cpu.regs.cs,
            cpu.regs.ip()
        );
        let budget = remaining.min(4096);
        match gp64_interp::run(cpu, mem, &mut (), budget) {
            RunResult::BudgetExhausted { executed } => {
                assert!(executed > 0, "benchmark made zero progress");
                retired = retired
                    .checked_add(executed)
                    .expect("benchmark retirement count overflowed");
                assert!(
                    retired <= expected_retired,
                    "benchmark retired {retired}, beyond bound {expected_retired}"
                );
                if retired == expected_retired {
                    assert_eq!(
                        terminal,
                        ExpectedTerminal::Budget,
                        "benchmark exhausted its retirement bound without HLT"
                    );
                    return retired;
                }
            }
            RunResult::Halt { executed } => {
                assert!(executed > 0, "HLT exit made zero progress");
                retired = retired
                    .checked_add(executed)
                    .expect("benchmark retirement count overflowed");
                assert_eq!(terminal, ExpectedTerminal::Halt, "unexpected HLT");
                assert_eq!(
                    retired, expected_retired,
                    "HLT retired {retired}, expected {expected_retired}"
                );
                return retired;
            }
            other => panic!(
                "unexpected interpreter exit after {retired}/{expected_retired} instructions at {:04x}:{:04x}: {other:?}",
                cpu.regs.cs,
                cpu.regs.ip()
            ),
        }
    }
}

pub fn setup_add_cpu() -> Cpu {
    let mut cpu = base_cpu();
    cpu.regs.eax = 0;
    cpu
}

pub fn install_add(mem: &mut Memory, iterations: u16) {
    let [lo, hi] = iterations.to_le_bytes();
    install(
        mem,
        &[
            0x05, 0x01, 0x00, // ADD AX,1
            0x3d, lo, hi, // CMP AX,limit
            0x75, 0xf8, // JNE 0000
        ],
    );
}

pub fn preflight_add(mem: &mut Memory, iterations: u16) {
    install_add(mem, iterations);
    let mut cpu = setup_add_cpu();
    assert_eq!(
        run_bounded(
            &mut cpu,
            mem,
            u32::from(iterations) * 3,
            ExpectedTerminal::Budget,
        ),
        u32::from(iterations) * 3
    );
    assert_eq!(cpu.regs.ax(), iterations);
    assert_eq!(cpu.regs.ip(), 8);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

pub fn setup_rep_stosw_cpu(count: u16) -> Cpu {
    let mut cpu = base_cpu();
    cpu.regs.es = 0x5000;
    cpu.regs.ecx = u32::from(count);
    cpu.regs.eax = 0x1234;
    cpu.regs.edi = 0;
    sync_real_mode_segment_caches(&mut cpu);
    cpu.regs.flags.remove(Flags::DIRECTION);
    cpu
}

pub fn install_rep_stosw(mem: &mut Memory) {
    install(mem, &[0xf3, 0xab, 0xf4]);
}

pub fn preflight_rep_stosw(mem: &mut Memory, count: u16) {
    install_rep_stosw(mem);
    let mut cpu = setup_rep_stosw_cpu(count);

    // Sentinels immediately either side of the destination. Checking only the
    // intended words let an inclusive-loop overrun write one extra word - or a
    // biased base write one short - with every assertion still green
    // (MDD-BUG-KIL-01057).
    let span = usize::from(count) * 2;
    mem.write_u16(0x5_0000 - 2, 0xfeed);
    mem.write_u16(0x5_0000 + span, 0xbeef);

    run_bounded(&mut cpu, mem, u32::from(count) + 1, ExpectedTerminal::Halt);
    assert_eq!(cpu.regs.cx(), 0);
    assert_eq!(cpu.regs.di(), count.wrapping_mul(2));
    assert_eq!(cpu.regs.ip(), 3);
    for offset in (0..span).step_by(2) {
        assert_eq!(mem.read_u16(0x5_0000 + offset), 0x1234);
    }
    assert_eq!(
        mem.read_u16(0x5_0000 - 2),
        0xfeed,
        "REP STOSW wrote BELOW its destination"
    );
    assert_eq!(
        mem.read_u16(0x5_0000 + span),
        0xbeef,
        "REP STOSW overran its destination by at least one word"
    );
}

pub fn setup_shifts_cpu() -> Cpu {
    let mut cpu = base_cpu();
    cpu.regs.eax = 0x1234;
    cpu.regs.ecx = 0;
    cpu.regs.ebx = 0;
    cpu
}

pub fn install_shifts(mem: &mut Memory, iterations: u16) {
    let [lo, hi] = iterations.to_le_bytes();
    install(
        mem,
        &[
            0xd1, 0xe0, // SHL AX,1
            0xd1, 0xe8, // SHR AX,1
            0x41, // INC CX
            0x81, 0xf9, lo, hi, // CMP CX,limit
            0x75, 0xf5, // JNE 0000
        ],
    );
}

pub fn preflight_shifts(mem: &mut Memory, iterations: u16) {
    install_shifts(mem, iterations);
    let mut cpu = setup_shifts_cpu();
    run_bounded(
        &mut cpu,
        mem,
        u32::from(iterations) * 5,
        ExpectedTerminal::Budget,
    );
    assert_eq!(cpu.regs.ax(), 0x1234);
    assert_eq!(cpu.regs.cx(), iterations);
    assert_eq!(cpu.regs.ip(), 11);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

pub fn setup_push_pop_cpu() -> Cpu {
    let mut cpu = base_cpu();
    cpu.regs.esp = 0xfff0;
    cpu.regs.set_sp(0xfff0);
    cpu.regs.eax = 0x1234;
    cpu.regs.ebx = 0x5678;
    cpu.regs.ecx = 0;
    cpu
}

pub fn install_push_pop(mem: &mut Memory, iterations: u16) {
    let [lo, hi] = iterations.to_le_bytes();
    install(
        mem,
        &[
            0x50, // PUSH AX
            0x53, // PUSH BX
            0x5b, // POP BX
            0x58, // POP AX
            0x41, // INC CX
            0x81, 0xf9, lo, hi, // CMP CX,limit
            0x75, 0xf5, // JNE 0000
        ],
    );
}

pub fn preflight_push_pop(mem: &mut Memory, iterations: u16) {
    install_push_pop(mem, iterations);
    let mut cpu = setup_push_pop_cpu();

    // SS = 0x2000 and SP starts at 0xfff0, so the two pushes own exactly
    // linear 0x2_ffec..0x2_ffef: BX at 0x2_ffec, AX at 0x2_ffee. Registers,
    // flags and a restored SP alone cannot see a shared address bias that
    // pushes to the wrong place and pops from that same wrong place
    // (MDD-BUG-KIL-01057), so the footprint and its guards are checked too.
    const STACK_BX: usize = 0x2_ffec;
    const STACK_AX: usize = 0x2_ffee;
    const GUARD_BELOW: usize = 0x2_ffea;
    const GUARD_ABOVE: usize = 0x2_fff0;
    mem.write_u16(STACK_BX, 0);
    mem.write_u16(STACK_AX, 0);
    mem.write_u16(GUARD_BELOW, 0xfeed);
    mem.write_u16(GUARD_ABOVE, 0xbeef);

    run_bounded(
        &mut cpu,
        mem,
        u32::from(iterations) * 7,
        ExpectedTerminal::Budget,
    );
    assert_eq!(cpu.regs.ax(), 0x1234);
    assert_eq!(cpu.regs.bx(), 0x5678);
    assert_eq!(cpu.regs.cx(), iterations);
    assert_eq!(cpu.regs.sp(), 0xfff0);
    assert_eq!(cpu.regs.ip(), 11);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(
        mem.read_u16(STACK_AX),
        0x1234,
        "PUSH AX must land at SS:0xffee"
    );
    assert_eq!(
        mem.read_u16(STACK_BX),
        0x5678,
        "PUSH BX must land at SS:0xffec"
    );
    assert_eq!(
        mem.read_u16(GUARD_BELOW),
        0xfeed,
        "the pushes ran below their own footprint"
    );
    assert_eq!(
        mem.read_u16(GUARD_ABOVE),
        0xbeef,
        "the pushes ran above their own footprint"
    );
}

pub fn setup_x87_cpu(iterations: u16) -> Cpu {
    let mut cpu = base_cpu();
    cpu.regs.ecx = u32::from(iterations);
    cpu.fpu.control_word &= !(0x3 << 8);
    cpu.fpu.push(F80::from_f32(1.0));
    cpu
}

pub fn install_x87(mem: &mut Memory) {
    let data_base = 0x3_0000usize;
    mem.write_u32(data_base, 1.0001f32.to_bits());
    mem.write_u32(data_base + 4, 0.0001f32.to_bits());
    install(
        mem,
        &[
            0xd8, 0x0e, 0x00, 0x00, // FMUL m32 [0000]
            0xd8, 0x06, 0x04, 0x00, // FADD m32 [0004]
            0x49, // DEC CX
            0x75, 0xf5, // JNZ 0000
            0xf4, // HLT
        ],
    );
}

fn expected_x87_result(iterations: u16) -> [u8; 10] {
    match iterations {
        10 => [0x00, 0x00, 0x00, 0x00, 0x00, 0x94, 0x41, 0x80, 0xff, 0x3f],
        100 => [0x00, 0x00, 0x00, 0x00, 0x00, 0xbb, 0x92, 0x82, 0xff, 0x3f],
        1000 => [0x00, 0x00, 0x00, 0x00, 0x00, 0x58, 0xed, 0x9a, 0xff, 0x3f],
        10_000 => [0x00, 0x00, 0x00, 0x00, 0x00, 0x42, 0xf7, 0x8d, 0x01, 0x40],
        _ => panic!("missing independent x87 golden for {iterations} iterations"),
    }
}

/// Verify the x87 loop and return the decode cache it warmed, for
/// [`setup_warm_x87_cpu`] to hand to every timed CPU.
pub fn preflight_x87(mem: &mut Memory, iterations: u16) -> X87DecodeCache {
    install_x87(mem);
    let mut cpu = setup_x87_cpu(iterations);
    run_bounded(
        &mut cpu,
        mem,
        u32::from(iterations) * 4 + 1,
        ExpectedTerminal::Halt,
    );
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.ip(), 12);
    assert_eq!(cpu.fpu.control_word, 0x007f);
    assert_eq!(
        cpu.fpu.status_word, 0x3a20,
        "x87 status golden for {iterations} iterations"
    );
    assert_eq!(cpu.fpu.tag_word, 0x3fff);
    assert_eq!(cpu.fpu.get_st(0).0, expected_x87_result(iterations));
    std::mem::take(&mut cpu.x87_decode_cache)
}

/// A fresh x87 CPU carrying a copy of `warm`, with zeroed hit and miss
/// counters. The decode cache belongs to the `Cpu`, so a CPU built by
/// [`setup_x87_cpu`] alone starts every timed run cold (MDD-BUG-KIL-00812).
pub fn setup_warm_x87_cpu(iterations: u16, warm: &X87DecodeCache) -> Cpu {
    let mut cpu = setup_x87_cpu(iterations);
    cpu.x87_decode_cache = warm.clone();
    cpu.x87_decode_cache.hits = 0;
    cpu.x87_decode_cache.misses = 0;
    cpu
}

pub fn setup_sse2_cpu(iterations: u16) -> Cpu {
    let mut cpu = base_cpu();
    cpu.set_cpu_type(CpuType::Pentium4);
    cpu.regs.cr4 = CR4_OSFXSR;
    cpu.regs.ecx = u32::from(iterations);
    cpu.set_xmm_reg(1, 0x0000_0001_0000_0001_0000_0001_0000_0001u128);
    cpu
}

pub fn install_sse2(mem: &mut Memory) {
    install(
        mem,
        &[
            0x66, 0x0f, 0xfe, 0xc1, // PADDD XMM0,XMM1
            0x49, // DEC CX
            0x75, 0xf9, // JNZ 0000
            0xf4, // HLT
        ],
    );
}

pub fn preflight_sse2(mem: &mut Memory, iterations: u16) {
    install_sse2(mem);
    let mut cpu = setup_sse2_cpu(iterations);
    run_bounded(
        &mut cpu,
        mem,
        u32::from(iterations) * 3 + 1,
        ExpectedTerminal::Halt,
    );
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.ip(), 8);
    let expected = u128::from(iterations) * 0x0000_0001_0000_0001_0000_0001_0000_0001u128;
    assert_eq!(cpu.xmm_reg(0), expected);
}

/// The four packed u32 lanes the memory-form PADDD adds each iteration.
const PADDD_ADDEND: u128 = 0x0000_0001_0000_0001_0000_0001_0000_0001;

/// Where the memory-form benchmarks keep their operand: DS:0000, i.e. linear
/// 0x30000 with the `base_cpu` selectors, and 16-byte aligned.
const SSE_DATA_BASE: usize = 0x3_0000;

pub fn setup_sse2_mem_cpu(iterations: u16) -> Cpu {
    let mut cpu = base_cpu();
    cpu.set_cpu_type(CpuType::Pentium4);
    cpu.regs.cr4 = CR4_OSFXSR;
    cpu.regs.ecx = u32::from(iterations);
    cpu
}

/// PADDD with a *memory* source. The interpreter's inline SSE2 fast lane only
/// takes ModRM >= 0xC0, so this form is the one that actually routes through
/// the SSE decode cache and `sse_exec::execute`.
pub fn install_sse2_mem(mem: &mut Memory) {
    for (lane, chunk) in PADDD_ADDEND
        .to_le_bytes()
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
    {
        mem.write_u32(SSE_DATA_BASE + lane * 4, u32::from_le_bytes(*chunk));
    }
    install(
        mem,
        &[
            0x66, 0x0f, 0xfe, 0x06, 0x00, 0x00, // PADDD XMM0,[0000]
            0x49, // DEC CX
            0x75, 0xf7, // JNZ 0000
            0xf4, // HLT
        ],
    );
}

pub fn preflight_sse2_mem(mem: &mut Memory, iterations: u16) {
    install_sse2_mem(mem);
    let mut cpu = setup_sse2_mem_cpu(iterations);
    run_bounded(
        &mut cpu,
        mem,
        u32::from(iterations) * 3 + 1,
        ExpectedTerminal::Halt,
    );
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.ip(), 10);
    assert_eq!(cpu.xmm_reg(0), u128::from(iterations) * PADDD_ADDEND);
}

/// 1.0f32 broadcast to all four lanes: the ADDPS addend.
const ONE_F32X4: u128 = 0x3f80_0000_3f80_0000_3f80_0000_3f80_0000;

/// A marker in the three upper lanes of XMM0, so the scalar benchmark's
/// preflight can prove ADDSS merged rather than overwrote them.
const ADDSS_UPPER_MARKER: u128 = 0x4000_0000_4000_0000_4000_0000_0000_0000;

fn setup_float_cpu(iterations: u16, xmm0: u128, xmm1: u128) -> Cpu {
    let mut cpu = base_cpu();
    cpu.set_cpu_type(CpuType::Pentium4);
    cpu.regs.cr4 = CR4_OSFXSR;
    cpu.regs.ecx = u32::from(iterations);
    cpu.set_xmm_reg(0, xmm0);
    cpu.set_xmm_reg(1, xmm1);
    cpu
}

/// Every iteration adds exactly 1.0 to each lane. A `u16` count can never
/// reach f32's 2^24 integer limit, so the running sum is exact at every step
/// and the benchmark raises no sticky MXCSR bit — which the preflights assert.
fn expected_float_sum(iterations: u16) -> u32 {
    f32::from(iterations).to_bits()
}

pub fn setup_sse_addps_cpu(iterations: u16) -> Cpu {
    setup_float_cpu(iterations, 0, ONE_F32X4)
}

pub fn install_sse_addps(mem: &mut Memory) {
    install(
        mem,
        &[
            0x0f, 0x58, 0xc1, // ADDPS XMM0,XMM1
            0x49, // DEC CX
            0x75, 0xfa, // JNZ 0000
            0xf4, // HLT
        ],
    );
}

pub fn preflight_sse_addps(mem: &mut Memory, iterations: u16) {
    install_sse_addps(mem);
    let mut cpu = setup_sse_addps_cpu(iterations);
    run_bounded(
        &mut cpu,
        mem,
        u32::from(iterations) * 3 + 1,
        ExpectedTerminal::Halt,
    );
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.ip(), 7);
    let lane = u128::from(expected_float_sum(iterations));
    assert_eq!(
        cpu.xmm_reg(0),
        lane | (lane << 32) | (lane << 64) | (lane << 96)
    );
    assert_eq!(cpu.sse.mxcsr & 0x3f, 0, "packed sum was not exact");
}

pub fn setup_sse_addss_cpu(iterations: u16) -> Cpu {
    setup_float_cpu(iterations, ADDSS_UPPER_MARKER, ONE_F32X4)
}

pub fn install_sse_addss(mem: &mut Memory) {
    install(
        mem,
        &[
            0xf3, 0x0f, 0x58, 0xc1, // ADDSS XMM0,XMM1
            0x49, // DEC CX
            0x75, 0xf9, // JNZ 0000
            0xf4, // HLT
        ],
    );
}

pub fn preflight_sse_addss(mem: &mut Memory, iterations: u16) {
    install_sse_addss(mem);
    let mut cpu = setup_sse_addss_cpu(iterations);
    run_bounded(
        &mut cpu,
        mem,
        u32::from(iterations) * 3 + 1,
        ExpectedTerminal::Halt,
    );
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.ip(), 8);
    assert_eq!(
        cpu.xmm_reg(0),
        ADDSS_UPPER_MARKER | u128::from(expected_float_sum(iterations)),
        "ADDSS must merge, leaving the upper three lanes alone"
    );
    assert_eq!(cpu.sse.mxcsr & 0x3f, 0, "scalar sum was not exact");
}
