use gp64_cpu::{Cpu, DeliveryMemory, DeliveryObserver, Flags, PageAccess};
use gp64_interp::{ExecPolicy, FetchBoundary};
use gp64_memory::Memory;

/// No BIOS address sentinels or platform diagnostics; software INT exits drive the services seam.
#[derive(Default)]
pub struct MachinePolicy;
impl ExecPolicy<Memory> for MachinePolicy {
    const ACTIVE: bool = false;
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
#[derive(Default)]
pub(crate) struct Observer;
impl DeliveryObserver<Memory> for Observer {
    type FrameToken = ();
}
impl DeliveryMemory<Observer> for Memory {
    fn translate_linear_supervisor_checked(
        &mut self,
        cpu: &Cpu,
        linear: u32,
        access: PageAccess,
    ) -> gp64_cpu::Result<usize> {
        // v1 dispatch runs in real mode (CPL 0), where every access is supervisor.
        cpu.translate_linear_checked(self, linear, access)
    }
    fn usable_len(&self) -> usize {
        Memory::usable_len(self)
    }
}
