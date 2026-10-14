//! X12a unit tests (LLD U2b/X12 §6.2, U1-U6): a counting fake controller
//! records every INTR query and INTA cycle.

use super::*;

#[derive(Default)]
struct Fake {
    pending: bool,
    ack: Option<IrqAck>,
    intr_calls: std::cell::Cell<u32>,
    acks: u32,
    ack_time: Option<u64>,
}

impl InterruptController for Fake {
    fn intr(&self) -> bool {
        self.intr_calls.set(self.intr_calls.get() + 1);
        self.pending
    }
    fn acknowledge(&mut self, now: u64) -> Option<IrqAck> {
        self.acks += 1;
        self.ack_time = Some(now);
        self.ack
    }
}

const ACK: IrqAck = IrqAck {
    vector: 0x20,
    line: Some(0),
};

fn pending() -> Fake {
    Fake {
        pending: true,
        ack: Some(ACK),
        ..Fake::default()
    }
}

fn cpu_with_if(enabled: bool) -> Cpu {
    let mut cpu = Cpu::new();
    cpu.regs.flags.set(Flags::INTERRUPT, enabled);
    cpu.interrupts_enabled = enabled;
    cpu.interrupt_inhibit = 0;
    cpu
}

/// U1: an armed shadow blocks recognition, is left for the engine, and the
/// controller is not acknowledged.
#[test]
fn shadow_blocks_without_consuming_or_acknowledging() {
    let mut cpu = cpu_with_if(true);
    cpu.interrupt_inhibit = 1;
    let mut pic = pending();
    assert_eq!(recognize(&mut cpu, &mut pic, 5), Recognition::Inhibited);
    assert_eq!(cpu.interrupt_inhibit, 1, "the engine consumes the shadow");
    assert_eq!(pic.acks, 0);
}

/// U2: IF clear never touches the controller.
#[test]
fn if_clear_touches_no_controller() {
    let mut cpu = cpu_with_if(false);
    let mut pic = pending();
    assert_eq!(recognize(&mut cpu, &mut pic, 5), Recognition::IfClear);
    assert_eq!((pic.intr_calls.get(), pic.acks), (0, 0));
}

/// U3: no request, no INTA cycle.
#[test]
fn no_request_no_acknowledge() {
    let mut cpu = cpu_with_if(true);
    let mut pic = Fake {
        ack: Some(ACK),
        ..Fake::default()
    };
    assert_eq!(recognize(&mut cpu, &mut pic, 5), Recognition::Nothing);
    assert_eq!((pic.intr_calls.get(), pic.acks), (1, 0));
}

/// U4: a pending request costs exactly one INTA cycle, at `now`.
#[test]
fn pending_request_takes_exactly_one_acknowledge() {
    let mut cpu = cpu_with_if(true);
    let mut pic = pending();
    assert_eq!(recognize(&mut cpu, &mut pic, 77), Recognition::Ack(ACK));
    assert_eq!((pic.acks, pic.ack_time), (1, Some(77)));
}

/// U5: a request that vanished before INTA is nothing, not vector 0.
#[test]
fn vanished_request_is_nothing() {
    let mut cpu = cpu_with_if(true);
    let mut pic = Fake {
        pending: true,
        ..Fake::default()
    };
    assert_eq!(recognize(&mut cpu, &mut pic, 5), Recognition::Nothing);
    assert_eq!(pic.acks, 1);
}

/// U6: `interrupts_enabled` drifted from EFLAGS.IF is repaired from EFLAGS
/// before the IF test, in both directions.
#[test]
fn drifted_interrupt_flag_is_resynced_from_eflags() {
    let mut cpu = cpu_with_if(true);
    cpu.interrupts_enabled = false;
    let mut pic = pending();
    assert_eq!(recognize(&mut cpu, &mut pic, 5), Recognition::Ack(ACK));
    assert!(cpu.interrupts_enabled);

    let mut cpu = cpu_with_if(false);
    cpu.interrupts_enabled = true;
    assert!(resync_interrupt_flag(&mut cpu));
    assert!(!cpu.interrupts_enabled);
    assert!(!resync_interrupt_flag(&mut cpu));
}
