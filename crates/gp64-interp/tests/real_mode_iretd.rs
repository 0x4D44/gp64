use gp64_cpu::{Cpu, CpuType, SegmentCache};
use gp64_interp::{run, RunResult};
use gp64_memory::Memory;

#[test]
fn real_mode_iretd_operand_override_uses_three_dwords_and_retains_flags() {
    for old_vm in [0, 2] {
        for saved_vm in [0, 2] {
            let mut cpu = Cpu::new();
            cpu.set_cpu_type(CpuType::Pentium);
            cpu.regs.cr0 = 0;
            // CPL3 satisfies the current Registers VM invariant when old VM is set.
            cpu.regs.cpl = if old_vm == 0 { 0 } else { 3 };
            cpu.regs.eflags_high = old_vm | 0x0018;
            cpu.regs.cs = 0;
            cpu.regs.cs_cache = SegmentCache::real_mode(0);
            cpu.regs.eip = 0x100;
            cpu.regs.ss = 0x1000;
            cpu.regs.ss_cache = SegmentCache::real_mode(0x1000);
            cpu.regs.esp = 0x1234_0200;
            cpu.regs.ds = 0x2000;
            cpu.regs.ds_cache = SegmentCache::real_mode(cpu.regs.ds);
            cpu.regs.es = 0x3000;
            cpu.regs.es_cache = SegmentCache::real_mode(cpu.regs.es);
            cpu.regs.fs = 0x4000;
            cpu.regs.fs_cache = SegmentCache::real_mode(cpu.regs.fs);
            cpu.regs.gs = 0x5000;
            cpu.regs.gs_cache = SegmentCache::real_mode(cpu.regs.gs);
            let before = cpu.regs.clone();
            let mut mem = Memory::new();
            // Independently assembled operand-size override + IRET.
            mem.ram[0x100..0x102].copy_from_slice(&[0x66, 0xcf]);
            let frame: [u32; 9] = [
                0x0200,
                0x1234,
                (u32::from(saved_vm as u16) << 16) | 0x0243,
                0x3456,
                0x6000,
                0x7000,
                0x8000,
                0x9000,
                0xa000,
            ];
            for (index, value) in frame.into_iter().enumerate() {
                mem.ram[0x10200 + index * 4..0x10204 + index * 4]
                    .copy_from_slice(&value.to_le_bytes());
            }
            let result = run(&mut cpu, &mut mem, &mut (), 1);
            assert!(
                matches!(
                    result,
                    RunResult::Iret { executed: 1 } | RunResult::BudgetExhausted { executed: 1 }
                ),
                "{result:?}"
            );
            assert_eq!((cpu.regs.cs, cpu.regs.eip), (0x1234, 0x0200));
            assert_eq!(cpu.regs.cs_cache.base, 0x12340);
            assert_eq!(cpu.regs.esp, 0x1234_020c);
            assert_eq!(cpu.regs.cpl, before.cpl);
            assert_eq!(cpu.cs_bits, 16);
            assert_eq!(cpu.regs.eflags_high & 0x001a, old_vm | 0x0018);
            assert_eq!(cpu.regs.flags.bits(), 0x0243);
            assert!(cpu.interrupts_enabled);
            assert_eq!(
                (
                    cpu.regs.ss,
                    cpu.regs.ds,
                    cpu.regs.es,
                    cpu.regs.fs,
                    cpu.regs.gs
                ),
                (before.ss, before.ds, before.es, before.fs, before.gs)
            );
            assert_eq!(cpu.regs.ss_cache, before.ss_cache);
            assert_eq!(cpu.regs.ds_cache, before.ds_cache);
            assert_eq!(cpu.regs.es_cache, before.es_cache);
            assert_eq!(cpu.regs.fs_cache, before.fs_cache);
            assert_eq!(cpu.regs.gs_cache, before.gs_cache);
        }
    }
}
