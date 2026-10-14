//! M2.4b: the interpreter runs on a machine that is not the PC.
//!
//! `NullMachine` is the audited test machine of `gp64-memory-core`: flat
//! identity RAM, no devices, no A20 gate. Driving the public `run` entry (no
//! execution policy) through it, unpaged and paged, proves the engine is
//! generic over `X86Machine` end to end, not just at the type level.

use super::*;
use gp64_memory_core::testing::NullMachine;
use gp64_types::MemoryAccess as _;

fn load(mem: &mut NullMachine, at: usize, code: &[u8]) {
    for (i, &b) in code.iter().enumerate() {
        mem.write_u8(at + i, b);
    }
}

fn flat32(cache: &mut SegmentCache, access: u8, is_code: bool) {
    cache.base = 0;
    cache.limit = 0xFFFFF;
    cache.granularity = true;
    cache.recompute_effective_limit();
    cache.access = access;
    cache.is_code = is_code;
    cache.big = true;
    cache.present = true;
}

#[test]
fn real_mode_program_and_rep_stosb_run_on_null_machine() {
    let mut mem = NullMachine::new(1 << 20);
    let mut cpu = Cpu::new();
    cpu.regs.cs = 0;
    cpu.regs.set_ip(0x100);
    cpu.regs.ss = 0;
    cpu.regs.esp = 0x1000;
    load(
        &mut mem,
        0x100,
        &[
            0xB8, 0x34, 0x12, // MOV AX, 0x1234
            0x05, 0x01, 0x00, // ADD AX, 1
            0xA3, 0x00, 0x02, // MOV [0x0200], AX
            0xB0, 0xAB, // MOV AL, 0xAB
            0xBF, 0x00, 0x03, // MOV DI, 0x0300
            0xB9, 0x10, 0x00, // MOV CX, 16
            0xF3, 0xAA, // REP STOSB
        ],
    );
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 6 + 16);

    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 22 }),
        "{result:?}"
    );
    assert_eq!(mem.read_u16(0x200), 0x1235);
    assert_eq!(cpu.regs.cx(), 0);
    assert_eq!(cpu.regs.di(), 0x310);
    for i in 0..16 {
        assert_eq!(mem.read_u8(0x300 + i), 0xAB, "STOSB byte {i}");
    }
    assert_eq!(mem.read_u8(0x310), 0, "REP STOSB overran");
}

#[test]
fn paged_program_translates_through_null_machine_page_tables() {
    let mut mem = NullMachine::new(1 << 20);
    let mut cpu = Cpu::new();
    cpu.regs.cr0 = 0x8000_0001; // PE + PG
    cpu.regs.cr3 = 0x10000;
    cpu.regs.cpl = 0;
    mem.write_u32(0x10000, 0x11007);
    for page in 0..256u32 {
        mem.write_u32(0x11000 + page as usize * 4, (page << 12) | 0x07);
    }
    // Linear 0x5000 -> physical 0x7000: a non-identity page.
    mem.write_u32(0x11000 + 5 * 4, 0x7007);

    cpu.regs.cs = 0x08;
    flat32(&mut cpu.regs.cs_cache, 0x9B, true);
    cpu.cs_bits = 32;
    for (sel, cache) in [
        (&mut cpu.regs.ds, &mut cpu.regs.ds_cache),
        (&mut cpu.regs.es, &mut cpu.regs.es_cache),
        (&mut cpu.regs.ss, &mut cpu.regs.ss_cache),
    ] {
        *sel = 0x10;
        flat32(cache, 0x93, false);
    }
    cpu.regs.esp = 0x8000;
    cpu.regs.eip = 0x100;
    load(
        &mut mem,
        0x100,
        &[
            0xB8, 0x78, 0x56, 0x34, 0x12, // MOV EAX, 0x12345678
            0xA3, 0x00, 0x50, 0x00, 0x00, // MOV [0x5000], EAX
            0x8B, 0x1D, 0x00, 0x50, 0x00, 0x00, // MOV EBX, [0x5000]
            0xBE, 0x00, 0x50, 0x00, 0x00, // MOV ESI, 0x5000
            0xBF, 0x00, 0x60, 0x00, 0x00, // MOV EDI, 0x6000
            0xB9, 0x01, 0x00, 0x00, 0x00, // MOV ECX, 1
            0xF3, 0xA5, // REP MOVSD
        ],
    );
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 7);

    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 7 }),
        "{result:?}"
    );
    assert_eq!(cpu.regs.ebx, 0x1234_5678);
    assert_eq!(
        mem.read_u32(0x7000),
        0x1234_5678,
        "store landed on the mapped frame"
    );
    assert_eq!(mem.read_u32(0x5000), 0, "identity frame untouched");
    assert_eq!(
        mem.read_u32(0x6000),
        0x1234_5678,
        "REP MOVSD read through the mapping"
    );
}
