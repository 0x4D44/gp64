use gp64_cpu::{Cpu, Flags};
use gp64_interp::{run_ext_charged_until_cycle_budget, ExecPolicy, FetchBoundary, RunResult};
use gp64_memory::Memory;
use gp64_types::IoPort;

struct QuietIo;
impl IoPort for QuietIo {
    fn interrupt_poll_wanted(&self) -> bool {
        false
    }
    fn read_u8(&mut self, _: u16) -> u8 {
        0xff
    }
    fn read_u16(&mut self, _: u16) -> u16 {
        0xffff
    }
    fn read_u32(&mut self, _: u16) -> u32 {
        0xffff_ffff
    }
    fn write_u8(&mut self, _: u16, _: u8) {}
    fn write_u16(&mut self, _: u16, _: u16) {}
    fn write_u32(&mut self, _: u16, _: u32) {}
}

struct ActivePolicy;
impl ExecPolicy<Memory> for ActivePolicy {
    const ACTIVE: bool = true;
    fn trace_context(&mut self, _: &Cpu, _: u64) {}
    fn at_fetch_boundary(&mut self, _: &Cpu, _: bool) -> FetchBoundary {
        FetchBoundary::Continue
    }
    fn on_gdt_dirty(&mut self, _: &mut Cpu, _: &mut Memory) {}
    fn write_context_diagnostic(&self) -> bool {
        false
    }
    fn record_write_context(&mut self, _: &mut Memory, _: u16, _: u32) {}
    fn record_write_context_with_regs(&mut self, _: &mut Memory, _: &Cpu, _: u32, _: Flags) {}
    fn clear_write_context(&mut self, _: &mut Memory) {}
    fn publish_watchpoint_context(&mut self, _: &Cpu, _: u32, _: Option<[u32; 2]>) {}
}

#[test]
fn quiet_bus_cannot_override_an_active_execution_policy() {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    cpu.regs.cs = 0;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.set_ip(0x100);
    memory.ram[0x100..0x102].copy_from_slice(&[0xfb, 0xf4]); // STI; HLT.
    let result = run_ext_charged_until_cycle_budget(
        &mut cpu,
        &mut memory,
        &mut QuietIo,
        &mut ActivePolicy,
        32,
        true,
        32,
    );
    assert!(
        matches!(
            result.result,
            RunResult::InterruptPollBoundary { executed: 1 }
        ),
        "{result:?}"
    );
    assert_eq!(cpu.regs.ip(), 0x101);
}
