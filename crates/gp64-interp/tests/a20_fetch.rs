use gp64_cpu::{Cpu, CpuType, SegmentCache};
use gp64_interp::{run, RunResult};
use gp64_memory::Memory;

fn boundary_fetch(a20_enabled: bool, preceding_jump: bool) {
    let mut cpu = Cpu::new();
    cpu.set_cpu_type(CpuType::I80386);
    cpu.regs.cr0 = 0;
    cpu.regs.cs = 0xffff;
    cpu.regs.cs_cache = SegmentCache::real_mode(cpu.regs.cs);
    cpu.regs.eip = if preceding_jump { 0 } else { 0x0f };
    let mut mem = Memory::new();
    // Synthetic JMP short reaches MOV AL,imm8 at the last byte below 1 MiB.
    // Its initial 15-byte direct window ends before the A20 discontinuity.
    mem.ram[0xffff0..0xffff2].copy_from_slice(&[0xeb, 0x0d]);
    mem.ram[0xfffff] = 0xb0;
    mem.ram[0] = 0x22;
    mem.ram[0x100000] = 0x33;
    mem.set_a20(a20_enabled);
    let budget = if preceding_jump { 2 } else { 1 };
    let result = run(&mut cpu, &mut mem, &mut (), budget);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed } if executed == budget),
        "both instructions must execute within one run: {result:?}"
    );
    assert_eq!(cpu.regs.eip, 0x11);
    assert_eq!(
        cpu.regs.al(),
        if a20_enabled { 0x33 } else { 0x22 },
        "instruction fetch must use the A20-selected immediate (a20_enabled={a20_enabled}, preceding_jump={preceding_jump})"
    );
}

#[test]
fn a20_fetch_first_boundary_instruction() {
    boundary_fetch(true, false);
    boundary_fetch(false, false);
}

#[test]
fn a20_fetch_decode_window_reuse_after_jump() {
    boundary_fetch(true, true);
    boundary_fetch(false, true);
}
