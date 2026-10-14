//! Maskable-interrupt recognition at an instruction boundary, for any board
//! with an 8259-style controller.
//!
//! - [`recognize`] decides whether the CPU takes a maskable interrupt now: the
//!   interrupt shadow, then EFLAGS.IF, then a non-acknowledging INTR query,
//!   then exactly one INTA cycle. It names no board and no controller type.
//! - **The shadow is not consumed here.** STI, MOV SS, POP SS and LSS arm
//!   `interrupt_inhibit`; the engine consumes it when it runs the one
//!   protected successor (`gp64-interp` `run_ext_inner`). A caller that
//!   owns the shadow itself (the PC loop) decrements it before calling.
//! - No guest memory access, no cycle charge and no tracing: delivery
//!   (`interrupt_hardware_irq_with_irq`), charging and policy belong to the
//!   caller.

use crate::{Cpu, Flags};

/// Guest cycles charged for one architectural hardware-interrupt delivery
/// through the IVT or IDT, at the caller, after the delivery.
///
/// **Provisional: 0**, the PC's existing charge (its raw delivery path in
/// `src/emulator/hle_dispatch.rs` adds no cycles; the HLE BIOS charges there
/// apply to HLE services, not to delivery). Real P6 delivery takes core clocks
/// that no vendor document states. Replace from a measurement on a P6-family
/// part: TSC delta across an IRQ taken
/// between two RDTSCs, minus the handler's own instructions.
pub const IRQ_DELIVERY_CYCLES: u64 = 0;

/// The most maskable interrupts one boundary delivers before admitting the
/// CPU. After a trap gate IF stays 1, so the CPU may take another request
/// before the handler's first instruction; the bound only stops a stuck
/// request from livelocking the boundary. The PC's value
/// (`deliver_pending_interrupts`).
pub const MAX_IRQ_DELIVERIES_PER_BOUNDARY: u32 = 64;

/// One INTA cycle's result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IrqAck {
    /// The vector the controller drove.
    pub vector: u8,
    /// The request line, when the controller knows it (`None` for a spurious
    /// vector).
    pub line: Option<u8>,
}

/// The CPU's view of an 8259-style interrupt controller.
pub trait InterruptController {
    /// INTR: an unmasked request outranks everything in service. Must not
    /// acknowledge or change controller state.
    fn intr(&self) -> bool;
    /// One INTA cycle at guest time `now`. `None` when the controller drives
    /// no vector (the request went away after [`Self::intr`]); a controller
    /// that drives a spurious vector instead returns it with `line: None`.
    fn acknowledge(&mut self, now: u64) -> Option<IrqAck>;
}

/// What [`recognize`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recognition {
    /// The interrupt shadow is armed: no recognition at this boundary.
    Inhibited,
    /// EFLAGS.IF is clear.
    IfClear,
    /// No request is pending (or it vanished before INTA).
    Nothing,
    /// The CPU took this request; deliver it.
    Ack(IrqAck),
}

/// Bring `interrupts_enabled` back in line with EFLAGS.IF. Returns whether it
/// had drifted. [`recognize`] calls this first; a caller that traces the drift
/// (the PC) calls it before `recognize` to see the result.
#[inline]
pub fn resync_interrupt_flag(cpu: &mut Cpu) -> bool {
    let flags_if = cpu.regs.flags.contains(Flags::INTERRUPT);
    let drifted = flags_if != cpu.interrupts_enabled;
    cpu.interrupts_enabled = flags_if;
    drifted
}

/// Recognise one maskable interrupt at an instruction boundary at guest time
/// `now`. Order: resync IF, the shadow, IF, INTR, then one INTA. The
/// controller is touched only when the CPU can take an interrupt, and the
/// shadow is left for the engine.
#[inline]
pub fn recognize<C: InterruptController + ?Sized>(
    cpu: &mut Cpu,
    controller: &mut C,
    now: u64,
) -> Recognition {
    resync_interrupt_flag(cpu);
    if cpu.interrupt_inhibit > 0 {
        return Recognition::Inhibited;
    }
    if !cpu.interrupts_enabled {
        return Recognition::IfClear;
    }
    if !controller.intr() {
        return Recognition::Nothing;
    }
    controller
        .acknowledge(now)
        .map_or(Recognition::Nothing, Recognition::Ack)
}

#[cfg(test)]
#[path = "irq_tests.rs"]
mod tests;
