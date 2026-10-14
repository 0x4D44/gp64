use super::*;
use gp64_memory::Memory;

fn setup(code_addr: u16, code: &[u8]) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cs = 0x0000;
    cpu.regs.set_ip(code_addr);
    cpu.regs.set_sp(0x2000);
    cpu.regs.esp = 0x2000;
    cpu.regs.ss = 0x0000;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[code_addr as usize + i] = b;
    }
    (cpu, mem)
}

#[test]
fn test_gap3_a20_enabled_no_wrap() {
    // Same as test 1, but A20 enabled — should read from 0x100010
    let (mut cpu, mut mem) = setup(0x0100, &[0xA0, 0x20, 0x00]);
    cpu.regs.ds = 0xFFFF;
    cpu.regs.ds_cache = SegmentCache::real_mode(0xFFFF);
    mem.set_a20(true); // default, but explicit

    mem.ram[0x000010] = 0xAA;
    mem.ram[0x100010] = 0xBB;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(
        cpu.regs.al(),
        0xBB,
        "With A20 enabled, should read from 0x100010 (no wrap)"
    );
}
