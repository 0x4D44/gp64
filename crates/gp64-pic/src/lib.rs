//! Programmable Interrupt Controller (8259A) emulation

use serde::{Deserialize, Serialize};

const LEGACY_IRQ_SOURCE: u16 = 0x0001;

/// Board wiring and power-on policy for a cascaded pair. No machine dependencies.
/// Implementations supply address decode and diagnostics; register logic is shared.
pub trait PicWiring: Clone + std::fmt::Debug {
    const VECTOR_OFFSETS: [u8; 2];
    const INITIAL_MASKS: [u8; 2];
    const ELCR_WRITABLE_MASKS: [u8; 2];
    const COMMAND_PORTS: [u16; 2];
    const DATA_PORTS: [u16; 2];
    const ELCR_PORTS: [u16; 2];
    /// Board policy may suppress a slave spurious vector when no handler exists.
    const DROP_SPURIOUS_CASCADE: bool = false;
    fn irq0_trace_enabled() -> bool {
        false
    }
    fn irq0_trace_slot() -> Option<u64> {
        None
    }
    fn storage_trace_enabled() -> bool {
        false
    }
    fn record_ack(_irq: u8) {}
    fn trace_irq0_transition(
        _pic: &Pic<Self>,
        _event: &'static str,
        _source: &'static str,
        _before: PicSnapshot,
        _vector: Option<u8>,
        _was_low: Option<bool>,
        _level_triggered: Option<bool>,
        _newly_latched: Option<bool>,
        _ack_while_in_service: Option<bool>,
    ) where
        Self: Sized,
    {
    }
    fn trace_completion_irq(_pic: &Pic<Self>, _event: &str, _irq: u8)
    where
        Self: Sized,
    {
    }
    fn trace_storage_irq(_pic: &Pic<Self>, _event: &'static str, _irq: u8)
    where
        Self: Sized,
    {
    }
    fn trace_eoi(_pic_num: u8, _irq: u8, _value: u8, _chip: PicChipSnapshot) {}
    fn trace_completion_eoi(
        _pic_num: u8,
        _irq: u8,
        _value: u8,
        _chip: PicChipSnapshot,
        _event: &str,
    ) {
    }
}

/// Unconfigured chip pair: all inputs masked, no board ELCR restrictions.
#[derive(Clone, Debug)]
pub struct Unconfigured;
impl PicWiring for Unconfigured {
    const VECTOR_OFFSETS: [u8; 2] = [0, 0];
    const INITIAL_MASKS: [u8; 2] = [0xff, 0xff];
    const ELCR_WRITABLE_MASKS: [u8; 2] = [0xff, 0xff];
    const COMMAND_PORTS: [u16; 2] = [0, 2];
    const DATA_PORTS: [u16; 2] = [1, 3];
    const ELCR_PORTS: [u16; 2] = [0, 1];
}

/// Opaque wired-OR source identity. Bit zero is reserved for raise/lower_irq.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IrqSource(u8);
impl IrqSource {
    /// Allocate one of the fifteen independent device source bits (1..=15).
    pub const fn new(id: u8) -> Self {
        assert!(id > 0 && id < 16);
        Self(id)
    }
    #[inline]
    const fn mask(self) -> u16 {
        1 << self.0
    }
}

/// Single 8259A PIC
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pic8259 {
    /// Interrupt mask register (IMR)
    mask: u8,
    /// In-service register (ISR)
    isr: u8,
    /// Interrupt request register (IRR)
    irr: u8,
    /// Shadow register tracking the current electrical state of each IRQ line.
    /// Used for edge-triggered detection: IRR is only set on a low-to-high
    /// transition of the corresponding bit in this register.
    irr_line: u8,
    /// Vector offset
    vector_offset: u8,
    /// Initialization state
    init_state: u8,
    /// ICW1 bit 1 (SNGL): this is the only 8259 in the system, so no ICW3.
    ///
    /// MDD-BUG-CRUCIBLE-00345. Defaults to cascaded configuration so a
    /// chip that has never seen an ICW1 sequences exactly as it did before.
    icw1_single: bool,
    /// ICW1 bit 0 (IC4): an ICW4 will follow. When clear, the 8259 defines all
    /// ICW4 features as zero and the word is never sent.
    icw1_expect_icw4: bool,
    /// Auto EOI mode
    auto_eoi: bool,
    /// Rotate on auto-EOI mode
    rotate_auto_eoi: bool,
    /// Priority base (lowest priority IRQ; the next IRQ is the highest priority)
    /// Default is 7, meaning IRQ 0 has highest priority
    priority_base: u8,
    /// Select read register: false = IRR, true = ISR
    read_isr: bool,
    /// OCW3 poll command: the next read of this chip acknowledges one request.
    #[serde(default)]
    poll_pending: bool,
    /// Edge-triggered mode (true) vs level-triggered mode (false).
    /// Set from ICW1 bit 3 during initialization.
    edge_triggered: bool,
    /// Special mask mode (OCW3 ESMM=1, SMM=1; cleared by ESMM=1, SMM=0).
    ///
    /// MDD-BUG-FLUX-00169. Per the Intel 8259A datasheet, setting a level's
    /// IMR bit while in this mode inhibits that level AND enables every other
    /// unmasked level — lower priority as well as higher. It is how a handler
    /// masks its own in-service level so a lower-priority IRQ can nest.
    ///
    /// `serde(default)` so an existing snapshot deserializes to the normal
    /// (non-special) priority behaviour it was captured with.
    #[serde(default)]
    special_mask: bool,
}

/// Read-only diagnostic snapshot of one 8259A chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PicChipSnapshot {
    pub imr: u8,
    pub isr: u8,
    pub irr: u8,
    pub irq_line: u8,
    pub vector_offset: u8,
    pub init_state: u8,
    pub auto_eoi: bool,
    pub rotate_auto_eoi: bool,
    pub priority_base: u8,
    pub read_isr: bool,
    pub edge_triggered: bool,
}

/// Read-only diagnostic snapshot of the cascaded PIC pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PicSnapshot {
    pub master: PicChipSnapshot,
    pub slave: PicChipSnapshot,
    pub elcr: u16,
    pub highest_priority_irq: Option<u8>,
}

impl PicChipSnapshot {
    fn from_chip(chip: &Pic8259) -> Self {
        Self {
            imr: chip.mask,
            isr: chip.isr,
            irr: chip.irr,
            irq_line: chip.irr_line,
            vector_offset: chip.vector_offset,
            init_state: chip.init_state,
            auto_eoi: chip.auto_eoi,
            rotate_auto_eoi: chip.rotate_auto_eoi,
            priority_base: chip.priority_base,
            read_isr: chip.read_isr,
            edge_triggered: chip.edge_triggered,
        }
    }
}

impl Pic8259 {
    fn new(vector_offset: u8) -> Self {
        Self {
            mask: 0xFF,
            isr: 0,
            irr: 0,
            irr_line: 0,
            vector_offset,
            init_state: 0,
            icw1_single: false,
            icw1_expect_icw4: true,
            auto_eoi: false,
            rotate_auto_eoi: false,
            priority_base: 7,
            read_isr: false,
            poll_pending: false,
            edge_triggered: true,
            special_mask: false,
        }
    }

    /// Get the IRQ number with the highest priority among the pending bits,
    /// respecting the current priority rotation base.
    /// Returns None if no bits are set.
    #[inline]
    fn highest_priority_bit(&self, pending: u8) -> Option<u8> {
        if pending == 0 {
            return None;
        }
        // Scan from highest priority (priority_base + 1) wrapping around
        for offset in 0..8 {
            let i = (self.priority_base.wrapping_add(1).wrapping_add(offset)) & 7;
            if pending & (1 << i) != 0 {
                return Some(i);
            }
        }
        None
    }

    /// Select the in-service IRQ a non-specific EOI should clear, respecting
    /// special-mask mode and this chip's current priority rotation.
    #[inline]
    fn highest_priority_in_service(&self) -> Option<u8> {
        let in_service = if self.special_mask {
            self.isr & !self.mask
        } else {
            self.isr
        };
        self.highest_priority_bit(in_service)
    }
}

/// Dual PIC setup (master + slave)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Pic<W: PicWiring = Unconfigured> {
    #[serde(skip)]
    wiring: std::marker::PhantomData<W>,
    master: Pic8259,
    slave: Pic8259,
    /// Board-provided Edge/Level Control Registers.
    ///
    /// A set bit marks the corresponding IRQ line as level-triggered.
    /// The wiring policy selects writable bits and register addresses.
    #[serde(default)]
    elcr: u16,
    /// Asserted device sources for each wired-OR PIC input.
    ///
    /// Bit 0 represents callers of the legacy `raise_irq`/`lower_irq` API.
    /// Independent sources use their own bits so one device cannot lower another
    /// device's asserted level.
    #[serde(default)]
    irq_sources: [u16; 16],
    /// C3 truncation seam (dispatch design §8, A-C3-1): set on every PIC
    /// mutation that may change interrupt deliverability — line raises/lowers,
    /// IMR (mask) writes, EOI/OCW2 command writes, and ICW re-initialisation.
    /// Consumed (take/clear) at the emulator's dispatch boundary; its only job
    /// is to force a batch boundary promptly after such a transition. Delivery
    /// decisions always read live PIC state, never this flag.
    #[serde(skip)]
    deliverability_dirty: bool,
}

impl<W: PicWiring> Pic<W> {
    const ELCR_MASTER_WRITABLE_MASK: u8 = W::ELCR_WRITABLE_MASKS[0];
    const ELCR_SLAVE_WRITABLE_MASK: u8 = W::ELCR_WRITABLE_MASKS[1];

    pub fn new() -> Self {
        let mut pic = Self {
            master: Pic8259::new(W::VECTOR_OFFSETS[0]),
            slave: Pic8259::new(W::VECTOR_OFFSETS[1]),
            wiring: std::marker::PhantomData,
            elcr: 0,
            irq_sources: [0; 16],
            deliverability_dirty: false,
        };
        pic.master.mask = W::INITIAL_MASKS[0];
        pic.slave.mask = W::INITIAL_MASKS[1];
        pic
    }

    /// Apply priority masking: only allow IRQs with higher priority than the
    /// highest-priority in-service IRQ. Respects the PIC's current priority base.
    #[inline]
    fn apply_priority_mask_rotated(pic: &Pic8259, pending: u8, isr: u8) -> u8 {
        // MDD-BUG-FLUX-00169: in special mask mode an in-service level that the
        // guest has masked stops blocking anything. That is the whole point of
        // the mode — a handler masks its own level so a LOWER-priority IRQ can
        // nest — so the priority resolver must not see those bits.
        let isr = if pic.special_mask {
            isr & !pic.mask
        } else {
            isr
        };
        if isr == 0 {
            return pending;
        }
        // Find the highest-priority in-service IRQ according to rotation
        if let Some(highest_is) = pic.highest_priority_bit(isr) {
            // Only allow IRQs with strictly higher priority than highest_is
            let mut allow = 0u8;
            for offset in 0..8u8 {
                let i = (pic.priority_base.wrapping_add(1).wrapping_add(offset)) & 7;
                if i == highest_is {
                    break; // This and all lower priority IRQs are blocked
                }
                allow |= 1 << i;
            }
            pending & allow
        } else {
            pending
        }
    }

    #[inline]
    fn slave_pending_for_cascade(&self) -> u8 {
        // The cascade input follows the slave's INT output, which is asserted
        // only for a request eligible under its current service and priority.
        Self::apply_priority_mask_rotated(
            &self.slave,
            self.slave.irr & !self.slave.mask,
            self.slave.isr,
        )
    }

    /// A level-triggered request is the line itself, not a captured edge:
    /// acknowledging it clears IRR for that INTA cycle only, and a line still
    /// asserted re-requests at once — QEMU's i8259 `pic_intack` leaves IRR set
    /// for an ELCR line. The ISR bit the acknowledge just set (absent AEOI)
    /// holds the request off until EOI, after which it is delivered again for
    /// as long as the device keeps the line high (MDD-BUG-KIL-01033).
    fn relatch_level_lines(&mut self) {
        let mut master_level = 0u8;
        let mut slave_level = 0u8;
        for line in 0..8u8 {
            if self.irq_is_level_triggered(line) {
                master_level |= 1 << line;
            }
            if self.irq_is_level_triggered(8 + line) {
                slave_level |= 1 << line;
            }
        }
        self.master.irr |= self.master.irr_line & master_level;
        self.slave.irr |= self.slave.irr_line & slave_level;
        self.update_cascade_line();
    }

    fn update_cascade_line(&mut self) {
        let slave_pending = self.slave_pending_for_cascade();
        if slave_pending != 0 {
            self.master.irr |= 1 << 2;
        } else {
            self.master.irr &= !(1 << 2);
        }
    }

    /// Whether `irq` behaves as a level-triggered line right now.
    ///
    /// Two independent controls can make a line level-triggered, and the
    /// assert and deassert paths must agree on both or a lowered line leaves a
    /// latched IRR nothing can clear (MDD-BUG-CRUCIBLE-00085):
    ///
    /// - **ICW1 LTIM**, which is chip-wide — it applies to all eight lines of
    ///   whichever 8259 owns this IRQ, so the master governs 0-7 and the slave
    ///   8-15.
    /// - **The chipset ELCR bit** for that individual line.
    ///
    /// Assertion used to combine the two inline while deassertion consulted
    /// only the ELCR, so a guest that programmed LTIM without matching ELCR
    /// bits could latch a request it could never lower.
    #[inline]
    fn irq_is_level_triggered(&self, irq: u8) -> bool {
        if irq >= 16 {
            return false;
        }
        let chip_is_level = if irq < 8 {
            !self.master.edge_triggered
        } else {
            !self.slave.edge_triggered
        };
        chip_is_level || (self.elcr & (1u16 << irq)) != 0
    }

    /// Mark that interrupt deliverability may have changed (C3 truncation
    /// seam, A-C3-1). Called from every state-mutating funnel: the IRQ line
    /// drivers, IMR writes, EOI/OCW2 commands, and ICW re-initialisation.
    #[inline]
    fn note_deliverability_change(&mut self) {
        self.deliverability_dirty = true;
    }

    /// Read the deliverability-dirty flag without consuming it.
    ///
    /// Consulted by `IoBus::should_break_batch()` after every interpreter
    /// IN/OUT (the shared seam both engines funnel through), so a mid-batch
    /// PIC transition ends the batch at the very next I/O boundary.
    #[inline]
    #[must_use]
    pub fn deliverability_dirty(&self) -> bool {
        self.deliverability_dirty
    }

    /// Consume the deliverability-dirty flag at a dispatch boundary.
    ///
    /// Once a batch boundary exists and the PIC has been (or is about to be)
    /// examined for pending delivery, the flag has done its job. Subsequent
    /// boundaries re-examine live PIC state regardless, so consuming here is
    /// always safe — the flag forces *a* boundary, never a delivery decision.
    #[inline]
    pub fn take_deliverability_dirty(&mut self) -> bool {
        let was = self.deliverability_dirty;
        self.deliverability_dirty = false;
        was
    }

    /// Get the interrupt request register (for debugging)
    #[inline]
    pub fn get_irr(&self) -> u8 {
        self.master.irr
    }

    /// Get the in-service register (for debugging)
    #[inline]
    pub fn get_isr(&self) -> u8 {
        self.master.isr
    }

    /// Get the interrupt mask register (for debugging)
    #[inline]
    pub fn get_mask(&self) -> u8 {
        self.master.mask
    }

    /// Capture all value-level PIC state needed by diagnostics without
    /// exposing mutable internals.
    pub fn diagnostic_snapshot(&self) -> PicSnapshot {
        PicSnapshot {
            master: PicChipSnapshot::from_chip(&self.master),
            slave: PicChipSnapshot::from_chip(&self.slave),
            elcr: self.elcr,
            highest_priority_irq: self.get_highest_priority_irq(),
        }
    }

    /// Decode the logical meaning of an I/O access to the PIC command/data
    /// ports using the pre-access state for ICW sequencing and read mode.
    pub fn decode_diagnostic_port_access(
        port: u16,
        value: u8,
        write: bool,
        before: &PicSnapshot,
    ) -> &'static str {
        let chip = match port {
            p if p == W::COMMAND_PORTS[0] || p == W::DATA_PORTS[0] => before.master,
            p if p == W::COMMAND_PORTS[1] || p == W::DATA_PORTS[1] => before.slave,
            _ => return "unknown",
        };

        if !write {
            return match port {
                p if W::COMMAND_PORTS.contains(&p) && chip.read_isr => "read_isr",
                p if W::COMMAND_PORTS.contains(&p) => "read_irr",
                p if W::DATA_PORTS.contains(&p) => "mask_read",
                _ => "unknown",
            };
        }

        match port {
            p if W::COMMAND_PORTS.contains(&p) => {
                if value & 0x10 != 0 {
                    return "icw1";
                }
                if value & 0x08 != 0 {
                    return if value & 0x02 != 0 {
                        if value & 0x01 != 0 {
                            "ocw3_read_isr"
                        } else {
                            "ocw3_read_irr"
                        }
                    } else {
                        "ocw3"
                    };
                }

                let rotate = value & 0x80 != 0;
                let specific = value & 0x40 != 0;
                let eoi = value & 0x20 != 0;
                match (rotate, specific, eoi) {
                    (false, false, true) => "non_specific_eoi",
                    (false, true, true) => "specific_eoi",
                    (true, false, true) => "rotate_non_specific_eoi",
                    (true, true, true) => "rotate_specific_eoi",
                    (true, true, false) => "set_priority",
                    (true, false, false) => "set_rotate_auto_eoi",
                    (false, false, false) => "clear_rotate_auto_eoi",
                    (false, true, false) => "ocw2_noop",
                }
            }
            p if W::DATA_PORTS.contains(&p) => match chip.init_state {
                1 => "icw2",
                2 => "icw3",
                3 => "icw4",
                _ => "mask_write",
            },
            _ => "unknown",
        }
    }

    pub fn trace_completion_irq(&self, event: &str, irq: u8) {
        W::trace_completion_irq(self, event, irq);
    }
    pub fn trace_storage_irq(&self, event: &'static str, irq: u8) {
        W::trace_storage_irq(self, event, irq);
    }

    /// Raise an IRQ (alias for request_interrupt for clarity)
    #[inline]
    pub fn raise_irq(&mut self, irq: u8) {
        self.request_interrupt(irq);
    }

    /// Set one independently driven device level on a wired-OR PIC input.
    #[inline]
    pub fn set_irq_source(&mut self, irq: u8, source: impl Into<IrqSource>, asserted: bool) {
        self.set_irq_source_mask(irq, source.into().mask(), asserted);
    }

    /// Lower (deassert) an IRQ line: drop the electrical-line shadow, and — for
    /// LEVEL-triggered lines only (per the ELCR) — clear the latched request (IRR).
    ///
    /// This is the counterpart of `raise_irq()`. Under the capture-at-assert contract
    /// (KILN-00004), an EDGE-triggered line's IRR is retained across a deassert (it is
    /// consumed only at acknowledge), matching QEMU's i8259 edge semantics; only a
    /// level-triggered line follows its wire down. The ISR is left untouched so an
    /// in-flight acknowledge/EOI sequence completes normally.
    ///
    /// For slave IRQs (8-15), the master cascade line (IRQ 2) is updated.
    #[inline]
    pub fn lower_irq(&mut self, irq: u8) {
        self.set_irq_source_mask(irq, LEGACY_IRQ_SOURCE, false);
    }

    fn set_irq_source_mask(&mut self, irq: u8, source: u16, asserted: bool) {
        let Some(sources) = self.irq_sources.get_mut(usize::from(irq)) else {
            return;
        };
        if asserted {
            *sources |= source;
            self.drive_irq_high(irq);
        } else {
            *sources &= !source;
            if *sources == 0 {
                self.drive_irq_low(irq);
            }
        }
    }

    fn drive_irq_low(&mut self, irq: u8) {
        // C3/A-C3-1: a lower can revoke a level-triggered request; mark the
        // seam conservatively (an over-set only costs one batch restart).
        self.note_deliverability_change();
        let irq0_before = (irq == 0 && W::irq0_trace_enabled()).then(|| self.diagnostic_snapshot());
        if irq < 8 {
            let bit = 1u8 << irq;
            // Capture-at-assert (KILN-00004): deasserting the line always drops the
            // electrical-line shadow, but only revokes the LATCHED request (IRR) for
            // level-triggered lines (per the ELCR). An edge-triggered request, once
            // captured, survives the deassert until it is acknowledged — matching the
            // QEMU i8259 `pic_set_irq` edge branch (which touches only `last_irr`) and
            // real 8259A silicon (the spurious-IRQ7 mechanism proves the captured edge
            // outlives an early deassert). Previously this cleared IRR unconditionally
            // (line-follows), silently dropping a completion whose line fell before the
            // deferred delivery pump ran — the root of the W2K finalization livelock.
            if self.irq_is_level_triggered(irq) {
                self.master.irr &= !bit;
            }
            self.master.irr_line &= !bit;
            tracing::debug!(
                irq,
                irr = format_args!("{:02X}", self.master.irr),
                "PIC: IRQ lower (master)"
            );
            if irq == 5 {
                W::trace_completion_irq(self, "pic_lower_irq", irq);
            }
            W::trace_storage_irq(self, "pic_lower_irq", irq);
            if let Some(before) = irq0_before {
                W::trace_irq0_transition(
                    self,
                    "pic_irq0_lower",
                    "lower_irq",
                    before,
                    None,
                    None,
                    None,
                    None,
                    None,
                );
            }
        } else if irq < 16 {
            let slave_bit = 1u8 << (irq - 8);
            // Capture-at-assert (KILN-00004): see the master branch. Edge-triggered
            // slave lines (e.g. IRQ14 IDE — confirmed edge by the W2K ELCR dump,
            // ELCR=0x0C00) retain their latched IRR across a deassert.
            if self.irq_is_level_triggered(irq) {
                self.slave.irr &= !slave_bit;
            }
            self.slave.irr_line &= !slave_bit;
            self.update_cascade_line();
            tracing::debug!(
                irq,
                slave_irr = format_args!("{:02X}", self.slave.irr),
                "PIC: IRQ lower (slave)"
            );
            W::trace_storage_irq(self, "pic_lower_irq", irq);
        }
    }

    /// Request an interrupt (raise IRQ line).
    ///
    /// In edge-triggered mode, the IRR bit is only set on a
    /// low-to-high transition of the IRQ line. If the line is already high
    /// (from a previous raise without an intervening lower), the IRR bit is
    /// NOT set again. This prevents spurious re-triggers from devices that
    /// hold their lines asserted.
    pub fn request_interrupt(&mut self, irq: u8) {
        if irq >= 16 {
            return;
        }
        self.irq_sources[usize::from(irq)] |= LEGACY_IRQ_SOURCE;
        self.drive_irq_high(irq);
    }

    fn drive_irq_high(&mut self, irq: u8) {
        // C3/A-C3-1: a raise is the canonical deliverability transition — the
        // funnel for `raise_irq`/`request_interrupt`/`set_irq_source`, which
        // covers every device raise (SB DSP block IRQs, UART IER/THRE, GUS,
        // PIT) arriving at this one PIC seam.
        self.note_deliverability_change();
        let irq0_before = (irq == 0 && W::irq0_trace_enabled()).then(|| self.diagnostic_snapshot());
        if irq < 8 {
            let bit = 1u8 << irq;
            let was_low = self.master.irr_line & bit == 0;
            let level_triggered = self.irq_is_level_triggered(irq);
            let irr_before = self.master.irr;
            self.master.irr_line |= bit;
            if was_low || level_triggered {
                self.master.irr |= bit;
            }
            let newly_latched = (irr_before & bit == 0) && (self.master.irr & bit != 0);
            // DIAG: trace IRQ5 raise
            if irq == 5 && tracing::enabled!(target: "gp64::interrupt::diag", tracing::Level::DEBUG)
            {
                static IRQ5_RAISE: std::sync::atomic::AtomicU64 =
                    std::sync::atomic::AtomicU64::new(0);
                let n = IRQ5_RAISE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if n < 5 || (n.is_multiple_of(100) && n <= 500) {
                    tracing::debug!(
                        target: "gp64::interrupt::diag",
                        irq = 5,
                        count = n,
                        was_low,
                        edge_triggered = self.master.edge_triggered,
                        irr = format_args!("{:02X}", self.master.irr),
                        isr = format_args!("{:02X}", self.master.isr),
                        irr_line = format_args!("{:02X}", self.master.irr_line),
                        vec_base = format_args!("{:02X}", self.master.vector_offset),
                        "PIC raise diagnostic"
                    );
                }
            }
            tracing::debug!(
                irq,
                irr = format_args!("{:02X}", self.master.irr),
                edge = was_low,
                "PIC: IRQ request (master)"
            );
            if irq == 5 {
                W::trace_completion_irq(self, "pic_raise_irq", irq);
            }
            W::trace_storage_irq(self, "pic_raise_irq", irq);
            if let Some(before) = irq0_before {
                W::trace_irq0_transition(
                    self,
                    "pic_irq0_raise",
                    "request_interrupt",
                    before,
                    None,
                    Some(was_low),
                    Some(level_triggered),
                    Some(newly_latched),
                    None,
                );
            }
        } else if irq < 16 {
            let slave_bit = 1u8 << (irq - 8);
            let was_low = self.slave.irr_line & slave_bit == 0;
            let level_triggered = self.irq_is_level_triggered(irq);
            self.slave.irr_line |= slave_bit;
            if was_low || level_triggered {
                self.slave.irr |= slave_bit;
            }
            // Cascade to master IRQ2
            self.update_cascade_line();
            tracing::debug!(
                irq,
                slave_irr = format_args!("{:02X}", self.slave.irr),
                edge = was_low,
                "PIC: IRQ request (slave)"
            );
            W::trace_storage_irq(self, "pic_raise_irq", irq);
        }
    }

    /// Check if there is any hardware interrupt ready to be delivered.
    ///
    /// This respects ISR priority masking (not just `irr & !mask`), so an IRQ
    /// Return the master PIC's current vector offset (ICW2 value & 0xF8).
    /// Used for keyboard flow tracing to identify IRQ1 regardless of PIC remapping.
    #[inline]
    pub fn master_vector_offset(&self) -> u8 {
        self.master.vector_offset
    }

    /// that is pending but blocked by a higher-or-equal priority in-service IRQ
    /// will not count. Slave IRQs are checked via the cascade line.
    ///
    /// This is a read-only, cheap check suitable for calling at every quantum
    /// boundary in the event-driven loop.
    #[inline]
    pub fn has_pending_irq(&self) -> bool {
        self.get_highest_priority_irq().is_some()
    }

    /// Get highest priority pending IRQ
    #[inline]
    pub fn get_highest_priority_irq(&self) -> Option<u8> {
        // Check master first - exclude IRQs already in service (ISR)
        let pending = Self::apply_priority_mask_rotated(
            &self.master,
            self.master.irr & !self.master.mask,
            self.master.isr,
        );

        // Scan in priority order (respecting rotation)
        for offset in 0..8u8 {
            let i = (self
                .master
                .priority_base
                .wrapping_add(1)
                .wrapping_add(offset))
                & 7;
            if pending & (1 << i) != 0 {
                if i == 2 {
                    // Cascade - check slave (also exclude in-service IRQs)
                    let slave_pending = Self::apply_priority_mask_rotated(
                        &self.slave,
                        self.slave.irr & !self.slave.mask,
                        self.slave.isr,
                    );
                    if let Some(j) = self.slave.highest_priority_bit(slave_pending) {
                        return Some(8 + j);
                    }
                } else {
                    return Some(i);
                }
            }
        }

        None
    }

    #[inline]
    pub fn is_irq_masked(&self, irq: u8) -> bool {
        if irq < 8 {
            (self.master.mask & (1 << irq)) != 0
        } else if irq < 16 {
            (self.slave.mask & (1 << (irq - 8))) != 0
        } else {
            true
        }
    }

    /// Check if an IRQ is currently in-service (being handled by an ISR)
    /// This is used to prevent re-entrancy - don't raise IRQ1 if INT 09h is still running
    #[inline]
    pub fn is_irq_in_service(&self, irq: u8) -> bool {
        if irq < 8 {
            (self.master.isr & (1 << irq)) != 0
        } else if irq < 16 {
            (self.slave.isr & (1 << (irq - 8))) != 0
        } else {
            false
        }
    }

    /// Acknowledge IRQ (clears IRR, sets ISR)
    pub fn acknowledge_irq(&mut self, irq: u8) {
        if irq < 8 {
            self.master.irr &= !(1 << irq);
            self.relatch_level_lines();
            if !self.master.auto_eoi {
                self.master.isr |= 1 << irq;
            }
            tracing::debug!(
                irq,
                isr = format_args!("{:02X}", self.master.isr),
                "PIC: IRQ ack (master)"
            );
        } else if irq < 16 {
            let slave_irq = irq - 8;
            self.slave.irr &= !(1 << slave_irq);
            self.relatch_level_lines();
            if !self.slave.auto_eoi {
                self.slave.isr |= 1 << slave_irq;
            }
            // Update cascade IRR and set ISR on master
            self.update_cascade_line();
            if !self.master.auto_eoi {
                self.master.isr |= 1 << 2;
            }
            // If more slave interrupts are pending, re-assert cascade bit in master IRR
            if self.slave_pending_for_cascade() != 0 {
                self.master.irr |= 1 << 2;
            }
            tracing::debug!(
                irq,
                slave_isr = format_args!("{:02X}", self.slave.isr),
                "PIC: IRQ ack (slave)"
            );
        }
    }

    /// Get the highest priority pending interrupt.
    ///
    /// On a real 8259A, if the IRQ line is deasserted between the first INTA
    /// pulse (which samples IRR) and the second INTA pulse (which reads the
    /// vector), the PIC delivers a **spurious interrupt** on IRQ 7 of that
    /// PIC chip (vector_offset + 7) without setting the ISR bit. This prevents
    /// the spurious handler from needing to send EOI.
    pub fn get_interrupt(&mut self) -> Option<u8> {
        let irq0_before = W::irq0_trace_enabled().then(|| self.diagnostic_snapshot());
        // Check master first
        let pending = Self::apply_priority_mask_rotated(
            &self.master,
            self.master.irr & !self.master.mask,
            self.master.isr,
        );
        if pending == 0 {
            return None;
        }

        // Scan in priority order (respecting rotation)
        for offset in 0..8u8 {
            let i = (self
                .master
                .priority_base
                .wrapping_add(1)
                .wrapping_add(offset))
                & 7;
            if pending & (1 << i) != 0 {
                if i == 2 {
                    // Cascade - check slave
                    let slave_pending = Self::apply_priority_mask_rotated(
                        &self.slave,
                        self.slave.irr & !self.slave.mask,
                        self.slave.isr,
                    );
                    if let Some(j) = self.slave.highest_priority_bit(slave_pending) {
                        self.slave.irr &= !(1 << j);
                        self.relatch_level_lines();
                        if self.slave.auto_eoi {
                            // Rotate on auto-EOI if enabled
                            if self.slave.rotate_auto_eoi {
                                self.slave.priority_base = j;
                            }
                        } else {
                            self.slave.isr |= 1 << j;
                        }
                        self.update_cascade_line();
                        if self.master.auto_eoi {
                            if self.master.rotate_auto_eoi {
                                self.master.priority_base = 2;
                            }
                        } else {
                            self.master.isr |= 1 << 2;
                        }
                        // If more slave interrupts are pending, re-assert cascade bit in master IRR
                        if self.slave_pending_for_cascade() != 0 {
                            self.master.irr |= 1 << 2;
                        }
                        // Real 8259A: vector = (ICW2 & 0xF8) | IRQ number
                        let irq = 8 + j;
                        if matches!(irq, 14 | 15) {
                            W::record_ack(irq);
                            W::trace_storage_irq(self, "pic_get_interrupt", irq);
                        }
                        return Some((self.slave.vector_offset & 0xF8) | j);
                    } else {
                        // The slave no longer has a request at acknowledge time.
                        // Machine policy chooses whether to suppress its IRQ7 vector.
                        self.master.irr &= !(1 << 2);
                        if !W::DROP_SPURIOUS_CASCADE {
                            // A slave spurious acknowledge services the master cascade
                            // only; the slave ISR7 remains clear.
                            if !self.master.auto_eoi {
                                self.master.isr |= 1 << 2;
                            } else if self.master.rotate_auto_eoi {
                                self.master.priority_base = 2;
                            }
                            return Some((self.slave.vector_offset & 0xf8) | 7);
                        }
                        tracing::debug!(
                            target: "gp64::hw::pic",
                            "spurious cascade dropped silently (no slave IRR)"
                        );
                        return None;
                    }
                } else {
                    self.master.irr &= !(1 << i);
                    self.relatch_level_lines();
                    if self.master.auto_eoi {
                        // Rotate on auto-EOI if enabled
                        if self.master.rotate_auto_eoi {
                            self.master.priority_base = i;
                        }
                    } else {
                        self.master.isr |= 1 << i;
                    }
                    // Real 8259A: vector = (ICW2 & 0xF8) | IRQ number
                    if i == 5 {
                        W::trace_completion_irq(self, "pic_get_interrupt", i);
                    }
                    let vector = (self.master.vector_offset & 0xF8) | i;
                    if i == 0 {
                        if let Some(before) = irq0_before {
                            let ack_while_in_service = before.master.isr & 1 != 0;
                            W::trace_irq0_transition(
                                self,
                                "pic_irq0_ack",
                                "get_interrupt",
                                before,
                                Some(vector),
                                None,
                                None,
                                None,
                                Some(ack_while_in_service),
                            );
                        }
                    }
                    return Some(vector);
                }
            }
        }

        None
    }

    /// Deliver an interrupt, handling the case where the IRQ was deasserted
    /// between the pending check and the acknowledge.
    ///
    /// On a real 8259A, if no IRR bit matches during the second INTA pulse,
    /// the master PIC delivers a **spurious interrupt** on IRQ 7
    /// (vector_offset + 7) without setting the ISR bit. This is the master-
    /// side spurious; the slave-side spurious is handled inside
    /// `get_interrupt()` when the cascade path finds an empty slave.
    ///
    /// NOTE (KILN-00004): this function has **no production caller** — the emulator
    /// dispatches via `get_interrupt()`. Under the capture-at-assert contract an edge
    /// line's IRR latch is retained across a deassert, so a raised-then-deasserted
    /// edge line delivers its **real** vector via `get_interrupt()`; a master spurious
    /// IRQ7 from an *edge deassert* is therefore intentionally NOT produced
    /// (QEMU-aligned — a documented departure from strict 8259A silicon, which
    /// delivers spurious IRQ7 on such a race). Retained for reference + unit coverage
    /// of the spurious primitive itself; do not let a future differential test flag
    /// the missing edge-deassert spurious as a bug.
    pub fn get_interrupt_or_spurious(&mut self) -> Option<u8> {
        // First, try the normal path.
        if let Some(vec) = self.get_interrupt() {
            return Some(vec);
        }
        // If get_interrupt() returned None but the caller believed there was
        // a pending IRQ (checked has_pending_irq() earlier), the IRQ was
        // deasserted between check and service. In the emulation model this
        // manifests as an IRR that was non-zero when has_pending_irq() ran
        // but is zero now. Deliver master spurious IRQ 7.
        //
        // The ISR bit for IRQ 7 is intentionally NOT set — the spurious
        // handler should not send EOI.
        tracing::debug!(
            master_vec = format_args!("{:02X}", (self.master.vector_offset & 0xF8) | 7),
            "PIC: spurious IRQ 7 (master)"
        );
        Some((self.master.vector_offset & 0xF8) | 7)
    }

    /// End of interrupt (EOI)
    pub fn end_of_interrupt(&mut self, irq: u8) {
        // C3/A-C3-1: an EOI can unblock a lower-priority pending IRQ.
        self.note_deliverability_change();
        let irq0_before = (irq == 0 && W::irq0_trace_enabled()).then(|| self.diagnostic_snapshot());
        if irq < 8 {
            self.master.isr &= !(1 << irq);
            tracing::debug!(
                irq,
                isr = format_args!("{:02X}", self.master.isr),
                "PIC: EOI (master)"
            );
        } else if irq < 16 {
            self.slave.isr &= !(1 << (irq - 8));
            // Also clear cascade on master
            self.master.isr &= !(1 << 2);
            self.update_cascade_line();
            tracing::debug!(
                irq,
                slave_isr = format_args!("{:02X}", self.slave.isr),
                "PIC: EOI (slave)"
            );
        }
        if irq == 5 {
            W::trace_completion_irq(self, "pic_end_of_interrupt", irq);
        }
        W::trace_storage_irq(self, "pic_end_of_interrupt", irq);
        if let Some(before) = irq0_before {
            W::trace_irq0_transition(
                self,
                "pic_irq0_eoi",
                "direct_end_of_interrupt",
                before,
                None,
                None,
                None,
                None,
                None,
            );
        }
    }

    /// Write command to PIC (0=master, 1=slave)
    ///
    /// Handles ICW1, OCW2 (EOI/rotation/priority), and OCW3 (register read select).
    /// OCW2 bit encoding: R(7) SL(6) EOI(5) 0 0 L2 L1 L0
    pub fn write_command(&mut self, pic_num: u8, value: u8) {
        // C3/A-C3-1: ICW1 re-initialisation and OCW2 (EOI / rotation /
        // priority) both change deliverability. OCW3 special-mask commands
        // do too; register read selects do not, so IRR/ISR polling stays quiet.
        let is_icw1 = value & 0x10 != 0;
        // MDD-BUG-FLUX-00169: the OCW2/OCW3 discriminator is bit 3 ALONE (with
        // bit 4 clear). Bit 5 is SMM in OCW3 and EOI in OCW2, so testing it
        // here sent every special-mask command down the OCW2 path.
        let is_ocw3 = !is_icw1 && value & 0x08 != 0;
        if !is_ocw3 || value & 0x40 != 0 {
            self.note_deliverability_change();
        }
        let irq0_before =
            (pic_num == 0 && W::irq0_trace_enabled()).then(|| self.diagnostic_snapshot());
        let mut irq0_eoi_source: Option<&'static str> = None;
        let named_lines = if value & 0x10 != 0 {
            let first_irq = usize::from(pic_num.min(1)) * 8;
            let mut lines = 0u8;
            for (bit, sources) in self.irq_sources[first_irq..first_irq + 8]
                .iter_mut()
                .enumerate()
            {
                *sources &= !LEGACY_IRQ_SOURCE;
                if *sources != 0 {
                    lines |= 1 << bit;
                }
            }
            lines
        } else {
            0
        };
        let pic = if pic_num == 0 {
            &mut self.master
        } else {
            &mut self.slave
        };

        if value & 0x10 != 0 {
            // ICW1
            pic.init_state = 1;
            pic.mask = 0;
            pic.isr = 0;
            pic.irr = 0;
            pic.irr_line = named_lines;
            pic.read_isr = false;
            pic.poll_pending = false;
            pic.rotate_auto_eoi = false;
            pic.priority_base = 7;
            // ICW1 bit 3: 0 = edge-triggered, 1 = level-triggered
            pic.edge_triggered = (value & 0x08) == 0;
            // MDD-BUG-CRUCIBLE-00345: ICW1 says which of the remaining
            // initialization words will actually be sent. SNGL (bit 1) means
            // this is the only 8259, so there is no cascade word; IC4 (bit 0)
            // clear means ICW4 is omitted entirely. Both used to be discarded
            // and the data port consumed ICW2, ICW3 and ICW4 unconditionally —
            // so in either configuration the guest's first OPERATIONAL write
            // (usually the IMR) was swallowed as an initialization word it
            // never sent, and the PIC came up with the wrong mask.
            pic.icw1_single = value & 0x02 != 0;
            pic.icw1_expect_icw4 = value & 0x01 != 0;
            // MDD-BUG-FLUX-00169: the 8259A initialisation sequence clears
            // special mask mode, so a re-init cannot inherit it from whatever
            // ran before.
            pic.special_mask = false;
            if !pic.icw1_expect_icw4 {
                // No ICW4 is coming, and the 8259 defines every ICW4 feature as
                // zero in that case. Auto-EOI is the one we model, so it must
                // not survive from a previous initialization.
                pic.auto_eoi = false;
            }
            tracing::debug!(
                pic = pic_num,
                value = format_args!("{:02X}", value),
                edge_triggered = pic.edge_triggered,
                "PIC: ICW1 (init start)"
            );
        } else if value & 0x08 != 0 {
            // OCW3: bit 4 = 0, bit 3 = 1. Bit 6 (ESMM) and bit 5 (SMM) belong
            // to OCW3 and must NOT be part of the discriminator — see
            // MDD-BUG-FLUX-00169, where 0x68 (enter special mask mode) was
            // decoded as OCW2 specific EOI for IRQ0 and corrupted the ISR.
            //
            // Layout: 0 ESMM SMM 0 1 P RR RIS
            //         7   6    5  4 3 2  1   0
            if value & 0x40 != 0 {
                // ESMM (bit 6) set: SMM (bit 5) is the new special-mask state.
                // So 0x68 enters the mode and 0x48 leaves it.
                pic.special_mask = value & 0x20 != 0;
                tracing::debug!(
                    pic = pic_num,
                    special_mask = pic.special_mask,
                    "PIC: OCW3 special mask mode"
                );
            }
            if value & 0x04 != 0 {
                pic.poll_pending = true;
            }
            if value & 0x02 != 0 {
                pic.read_isr = (value & 0x01) != 0;
            }
        } else {
            // OCW2: R(7) SL(6) EOI(5) 0 0 L2 L1 L0
            let r = value & 0x80 != 0;
            let sl = value & 0x40 != 0;
            let eoi = value & 0x20 != 0;
            let irq_l = value & 0x07;

            match (r, sl, eoi) {
                (false, false, true) => {
                    // 001: Non-specific EOI (0x20)
                    // Clear highest-priority in-service bit (respecting rotation)
                    if let Some(i) = pic.highest_priority_in_service() {
                        pic.isr &= !(1 << i);
                        if pic_num == 0 && i == 0 {
                            irq0_eoi_source = Some("guest_non_specific_eoi");
                        }
                        tracing::debug!(
                            pic = pic_num,
                            irq = i,
                            isr = format_args!("{:02X}", pic.isr),
                            "PIC: non-specific EOI"
                        );
                        W::trace_eoi(pic_num, i, value, PicChipSnapshot::from_chip(pic));
                        W::trace_completion_eoi(
                            pic_num,
                            i,
                            value,
                            PicChipSnapshot::from_chip(pic),
                            "pic_command_non_specific_eoi",
                        );
                    }
                }
                (false, true, true) => {
                    // 011: Specific EOI (0x60+irq)
                    if pic_num == 0 && irq_l == 0 {
                        irq0_eoi_source = Some("guest_specific_eoi");
                    }
                    pic.isr &= !(1 << irq_l);
                    tracing::debug!(
                        pic = pic_num,
                        irq = irq_l,
                        isr = format_args!("{:02X}", pic.isr),
                        "PIC: specific EOI"
                    );
                    W::trace_eoi(pic_num, irq_l, value, PicChipSnapshot::from_chip(pic));
                    W::trace_completion_eoi(
                        pic_num,
                        irq_l,
                        value,
                        PicChipSnapshot::from_chip(pic),
                        "pic_command_specific_eoi",
                    );
                }
                (true, false, true) => {
                    // 101: Rotate on non-specific EOI (0xA0)
                    // Non-specific EOI + rotate priority so the serviced IRQ becomes lowest
                    if let Some(i) = pic.highest_priority_in_service() {
                        pic.isr &= !(1 << i);
                        pic.priority_base = i;
                        if pic_num == 0 && i == 0 {
                            irq0_eoi_source = Some("guest_rotate_non_specific_eoi");
                        }
                        tracing::debug!(
                            pic = pic_num,
                            irq = i,
                            priority_base = pic.priority_base,
                            isr = format_args!("{:02X}", pic.isr),
                            "PIC: rotate on non-specific EOI"
                        );
                    }
                }
                (true, false, false) => {
                    // 100: Set rotate on auto-EOI mode (0x80)
                    pic.rotate_auto_eoi = true;
                    tracing::debug!(pic = pic_num, "PIC: set rotate on auto-EOI");
                }
                (false, false, false) => {
                    // 000: Clear rotate on auto-EOI mode (0x00)
                    pic.rotate_auto_eoi = false;
                    tracing::debug!(pic = pic_num, "PIC: clear rotate on auto-EOI");
                }
                (true, true, false) => {
                    // 110: Set priority command (0xC0+irq)
                    // Set the bottom priority to irq_l (so irq_l+1 becomes highest priority)
                    pic.priority_base = irq_l;
                    tracing::debug!(
                        pic = pic_num,
                        priority_base = irq_l,
                        "PIC: set priority command"
                    );
                }
                (true, true, true) => {
                    // 111: Rotate on specific EOI (0xE0+irq)
                    // Specific EOI + rotate so the specified IRQ becomes lowest priority
                    if pic_num == 0 && irq_l == 0 {
                        irq0_eoi_source = Some("guest_rotate_specific_eoi");
                    }
                    pic.isr &= !(1 << irq_l);
                    pic.priority_base = irq_l;
                    tracing::debug!(
                        pic = pic_num,
                        irq = irq_l,
                        priority_base = irq_l,
                        isr = format_args!("{:02X}", pic.isr),
                        "PIC: rotate on specific EOI"
                    );
                }
                (false, true, false) => {
                    // 010: No-op (not a valid 8259 command)
                    tracing::debug!(
                        pic = pic_num,
                        value = format_args!("{:02X}", value),
                        "PIC: OCW2 no-op (010)"
                    );
                }
            }
        }
        // Guest EOI, rotation, initialization and special-mask commands can
        // change slave INT without a new device edge. Keep the cascade in sync
        // here; master-only commands must retain a latched spurious cascade.
        if pic_num != 0 && (!is_ocw3 || value & 0x40 != 0) {
            self.update_cascade_line();
        }
        if let (Some(before), Some(source)) = (irq0_before, irq0_eoi_source) {
            W::trace_irq0_transition(
                self,
                "pic_irq0_eoi",
                source,
                before,
                None,
                None,
                None,
                None,
                None,
            );
        }
    }

    /// Write data to PIC (0=master, 1=slave)
    pub fn write_data(&mut self, pic_num: u8, value: u8) {
        let pic = if pic_num == 0 {
            &mut self.master
        } else {
            &mut self.slave
        };

        match pic.init_state {
            1 => {
                pic.vector_offset = value;
                // MDD-BUG-CRUCIBLE-00345: skip the words ICW1 said are absent.
                pic.init_state = if !pic.icw1_single {
                    2 // ICW3 (cascade) next
                } else if pic.icw1_expect_icw4 {
                    3 // single 8259, but ICW4 was promised
                } else {
                    0 // single 8259 with no ICW4: initialization is complete
                };
                tracing::debug!(
                    pic = pic_num,
                    vector = format_args!("{:02X}", value),
                    next_state = pic.init_state,
                    "PIC: ICW2 (vector offset)"
                );
            }
            2 => {
                // ICW3 (cascade wiring) consumed.
                pic.init_state = if pic.icw1_expect_icw4 { 3 } else { 0 };
            }
            3 => {
                pic.auto_eoi = value & 0x02 != 0;
                pic.init_state = 0;
                tracing::debug!(
                    pic = pic_num,
                    auto_eoi = pic.auto_eoi,
                    "PIC: ICW4 (init complete)"
                );
                if pic_num != 0 {
                    self.update_cascade_line();
                }
            }
            _ => {
                let old_mask = pic.mask;
                pic.mask = value;
                if pic_num == 1 {
                    self.update_cascade_line();
                }
                if old_mask != value {
                    // C3/A-C3-1: an IMR change can expose a pending IRQ —
                    // end the running slice so delivery observes the updated mask.
                    self.note_deliverability_change();
                    tracing::debug!(
                        pic = pic_num,
                        old = format_args!("{:02X}", old_mask),
                        new = format_args!("{:02X}", value),
                        "PIC: mask change"
                    );
                    if pic_num == 0 && (old_mask ^ value) & (1 << 2) != 0 {
                        W::trace_storage_irq(self, "pic_mask_change", 2);
                    }
                    if pic_num == 1 {
                        if (old_mask ^ value) & (1 << 6) != 0 {
                            W::trace_storage_irq(self, "pic_mask_change", 14);
                        }
                        if (old_mask ^ value) & (1 << 7) != 0 {
                            W::trace_storage_irq(self, "pic_mask_change", 15);
                        }
                    }
                }
            }
        }
    }

    /// Consume a chip-local poll on the next RD pulse, regardless of A0.
    /// Intel 8259A datasheet, Poll Command / Reading the 8259A Status:
    /// https://openwatcom.org/ftp/devel/docs/8259A.pdf (pages 16–18).
    /// Polling overrides register reads once; it does not acknowledge the
    /// other chip when the master's selected request is the cascade input.
    fn read_poll(&mut self, pic_num: u8) -> Option<u8> {
        let pic = if pic_num == 0 {
            &mut self.master
        } else {
            &mut self.slave
        };
        if !pic.poll_pending {
            return None;
        }
        pic.poll_pending = false;
        let pending = Self::apply_priority_mask_rotated(pic, pic.irr & !pic.mask, pic.isr);
        let Some(irq) = pic.highest_priority_bit(pending) else {
            return Some(0);
        };
        pic.irr &= !(1 << irq);
        if pic.auto_eoi {
            if pic.rotate_auto_eoi {
                pic.priority_base = irq;
            }
        } else {
            pic.isr |= 1 << irq;
        }
        // Only the acknowledged chip's level inputs are relatched. A slave
        // poll also refreshes the master's cascade input, without setting its ISR.
        let first_irq = if pic_num == 0 { 0 } else { 8 };
        let mut level_mask = 0;
        for line in 0..8 {
            if self.irq_is_level_triggered(first_irq + line) {
                level_mask |= 1 << line;
            }
        }
        let pic = if pic_num == 0 {
            &mut self.master
        } else {
            &mut self.slave
        };
        pic.irr |= pic.irr_line & level_mask;
        if pic_num != 0 {
            self.update_cascade_line();
        }
        self.note_deliverability_change();
        Some(0x80 | irq)
    }

    /// Read command from PIC (0=master, 1=slave)
    #[inline]
    pub fn read_command(&mut self, pic_num: u8) -> u8 {
        if let Some(result) = self.read_poll(pic_num) {
            return result;
        }
        let pic = if pic_num == 0 {
            &self.master
        } else {
            &self.slave
        };
        if pic.read_isr {
            pic.isr
        } else {
            pic.irr
        }
    }

    /// Read data from PIC (0=master, 1=slave)
    #[inline]
    pub fn read_data(&mut self, pic_num: u8) -> u8 {
        if let Some(result) = self.read_poll(pic_num) {
            return result;
        }
        let pic = if pic_num == 0 {
            &self.master
        } else {
            &self.slave
        };
        pic.mask
    }

    /// Read an Edge/Level Control Register byte.
    #[inline]
    pub fn read_elcr(&self, port: u16) -> u8 {
        match port {
            p if p == W::ELCR_PORTS[0] => self.elcr as u8,
            p if p == W::ELCR_PORTS[1] => (self.elcr >> 8) as u8,
            _ => 0xFF,
        }
    }

    /// Write an Edge/Level Control Register byte.
    pub fn write_elcr(&mut self, port: u16, value: u8) {
        match port {
            p if p == W::ELCR_PORTS[0] => {
                let masked = value & Self::ELCR_MASTER_WRITABLE_MASK;
                self.elcr = (self.elcr & 0xFF00) | masked as u16;
            }
            p if p == W::ELCR_PORTS[1] => {
                let masked = value & Self::ELCR_SLAVE_WRITABLE_MASK;
                self.elcr = (self.elcr & 0x00FF) | ((masked as u16) << 8);
            }
            _ => {}
        }
    }

    /// Return to a time-machine checkpoint of this PIC pair: both chips'
    /// complete register and initialization state (including an in-progress
    /// ICW sequence and special-mask mode), ELCR, and every device's asserted
    /// line (MDD-BUG-CRU-01027). The restored state may deliver differently
    /// from the live one, so the dispatcher is told to re-evaluate.
    pub fn restore_checkpoint(&mut self, checkpoint: &Self) {
        *self = checkpoint.clone();
        self.deliverability_dirty = true;
    }

    /// Save the combined ELCR state for checkpoint/time-machine snapshots.
    #[inline]
    pub fn save_elcr(&self) -> u16 {
        self.elcr
    }

    /// Restore the combined ELCR state, applying hardware reserved-bit masks.
    pub fn restore_elcr(&mut self, value: u16) {
        let master = (value as u8) & Self::ELCR_MASTER_WRITABLE_MASK;
        let slave = ((value >> 8) as u8) & Self::ELCR_SLAVE_WRITABLE_MASK;
        self.elcr = master as u16 | ((slave as u16) << 8);
    }

    /// Returns a reference to the master PIC.
    #[inline]
    pub fn master(&self) -> &Pic8259 {
        &self.master
    }

    /// Returns a reference to the slave PIC.
    #[inline]
    pub fn slave(&self) -> &Pic8259 {
        &self.slave
    }

    /// Returns a mutable reference to the master PIC.
    #[inline]
    pub fn master_mut(&mut self) -> &mut Pic8259 {
        &mut self.master
    }

    /// Returns a mutable reference to the slave PIC.
    #[inline]
    pub fn slave_mut(&mut self) -> &mut Pic8259 {
        &mut self.slave
    }
}

impl<W: PicWiring> Default for Pic<W> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    type Pic = super::Pic<TestWiring>;
    #[derive(Clone, Debug)]
    struct TestWiring;
    impl PicWiring for TestWiring {
        const VECTOR_OFFSETS: [u8; 2] = [0x08, 0x70];
        const INITIAL_MASKS: [u8; 2] = [0xfc, 0xff];
        const ELCR_WRITABLE_MASKS: [u8; 2] = [0xf8, 0xde];
        const COMMAND_PORTS: [u16; 2] = [0x20, 0xa0];
        const DATA_PORTS: [u16; 2] = [0x21, 0xa1];
        const ELCR_PORTS: [u16; 2] = [0x4d0, 0x4d1];
        const DROP_SPURIOUS_CASCADE: bool = true;
    }
    /// Independently driven device levels that may share one PIC input.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum PicIrqSource {
        S3Trio64,
        SoundBlaster,
        Opl,
        Ne2000,
    }

    impl PicIrqSource {
        const fn id(self) -> u8 {
            match self {
                Self::S3Trio64 => 1,
                Self::SoundBlaster => 2,
                Self::Opl => 3,
                Self::Ne2000 => 5,
            }
        }
    }

    impl From<PicIrqSource> for IrqSource {
        fn from(source: PicIrqSource) -> Self {
            Self::new(source.id())
        }
    }
    #[test]
    fn poll_pending_master_is_one_shot_and_preserves_register_selection() {
        for ocw3 in [0x0c, 0x0e, 0x0f] {
            let mut pic = Pic::new();
            pic.master.mask = 0;
            pic.raise_irq(1);
            pic.raise_irq(3);
            pic.take_deliverability_dirty();
            pic.write_command(0, ocw3);
            assert!(!pic.deliverability_dirty());
            assert_eq!(pic.read_command(0), 0x81);
            assert!(pic.take_deliverability_dirty());
            assert_eq!(pic.master.isr, 0x02);
            assert_eq!(pic.master.irr, 0x08);
            assert_eq!(pic.read_command(0), if ocw3 == 0x0f { 2 } else { 8 });
        }
    }

    #[test]
    fn poll_empty_and_masked_requests_return_zero_once() {
        let mut pic = Pic::new();
        pic.raise_irq(3); // Masked pending requests cannot be polled.
        pic.write_command(0, 0x0c);
        assert_eq!(pic.read_command(0), 0);
        assert_eq!(pic.master.isr, 0);
        assert_eq!(pic.read_command(0), 8);
        pic.write_command(1, 0x0c);
        assert_eq!(pic.read_command(1), 0);
    }

    #[test]
    fn poll_slave_acknowledges_only_slave_and_clears_cascade() {
        let mut pic = Pic::new();
        pic.slave.mask = 0;
        pic.raise_irq(11);
        pic.write_command(1, 0x0c);
        assert_eq!(pic.read_command(1), 0x83);
        assert_eq!(pic.slave.isr, 8);
        assert_eq!(pic.slave.irr, 0);
        assert_eq!(pic.master.isr, 0);
        assert_eq!(pic.master.irr & 4, 0);
    }

    #[test]
    fn poll_master_cascade_requires_separate_slave_poll() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.slave.mask = 0;
        pic.raise_irq(9);
        pic.write_command(0, 0x0c);
        pic.write_command(1, 0x0c);
        assert_eq!(pic.read_command(0), 0x82);
        assert_eq!(pic.master.isr, 4);
        assert_eq!(pic.slave.isr, 0);
        assert_eq!(pic.slave.irr, 2);
        assert_eq!(pic.read_command(1), 0x81);
        assert_eq!(pic.master.isr, 4);
        assert_eq!(pic.slave.isr, 2);
    }

    #[test]
    fn poll_data_read_consumes_next_chip_read_and_keeps_mask() {
        let mut pic = Pic::new();
        pic.raise_irq(1);
        pic.write_command(0, 0x0c);
        assert_eq!(pic.read_data(1), 0xff); // Other chip does not consume it.
        assert_eq!(pic.read_data(0), 0x81);
        assert_eq!(pic.read_data(0), 0xfc);
        assert_eq!(pic.read_command(0), 0);
    }

    #[test]
    fn poll_priority_blocked_and_special_mask_requests() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.raise_irq(3);
        pic.write_command(0, 0x0c);
        assert_eq!(pic.read_command(0), 0x83);
        pic.raise_irq(4);
        pic.write_command(0, 0x0c);
        assert_eq!(pic.read_command(0), 0);
        assert_eq!(pic.master.irr, 0x10);
        pic.write_data(0, 8); // Mask the in-service level.
        pic.write_command(0, 0x6c); // SMM and poll together.
        assert_eq!(pic.read_command(0), 0x84);
        assert_eq!(pic.master.isr, 0x18);
    }

    #[test]
    fn poll_auto_eoi_rotation_applies_to_each_chip() {
        for chip in [0, 1] {
            let mut pic = Pic::new();
            let controller = if chip == 0 {
                &mut pic.master
            } else {
                &mut pic.slave
            };
            controller.mask = 0;
            controller.auto_eoi = true;
            pic.write_command(chip, 0x80); // Rotate on AEOI.
            let base = chip * 8;
            pic.raise_irq(base + 1);
            pic.raise_irq(base + 3);
            pic.write_command(chip, 0x0c);
            assert_eq!(pic.read_command(chip), 0x81);
            let controller = if chip == 0 { &pic.master } else { &pic.slave };
            assert_eq!(controller.isr, 0);
            assert_eq!(controller.priority_base, 1);
            pic.lower_irq(base + 1);
            pic.raise_irq(base + 1);
            pic.write_command(chip, 0x0c);
            assert_eq!(pic.read_command(chip), 0x83);
        }
    }

    #[test]
    fn poll_level_request_relatches_until_line_is_lowered() {
        for chip in [0, 1] {
            let mut pic = Pic::new();
            let controller = if chip == 0 {
                &mut pic.master
            } else {
                &mut pic.slave
            };
            controller.mask = 0;
            controller.edge_triggered = false;
            let irq = chip * 8 + 3;
            pic.raise_irq(irq);
            pic.write_command(chip, 0x0c);
            assert_eq!(pic.read_command(chip), 0x83);
            pic.write_command(chip, 0x0c);
            assert_eq!(pic.read_command(chip), 0); // ISR blocks relatch.
            pic.write_command(chip, 0x20);
            pic.write_command(chip, 0x0c);
            assert_eq!(pic.read_command(chip), 0x83);
            pic.lower_irq(irq);
            pic.write_command(chip, 0x20);
            pic.write_command(chip, 0x0c);
            assert_eq!(pic.read_command(chip), 0);
        }
    }

    fn complete_icw1_to_icw4(pic: &mut Pic, chip: u8) {
        pic.write_data(chip, if chip == 0 { 0x20 } else { 0x28 }); // ICW2
        pic.write_data(chip, if chip == 0 { 0x04 } else { 0x02 }); // ICW3
        pic.write_data(chip, 0x01); // ICW4
    }

    fn exercise_pic_init_selector(chip: u8) {
        let irq = chip * 8 + 5;
        let pending = 1 << (irq % 8);
        let mut pic = Pic::new();

        pic.write_command(chip, 0x0b); // OCW3: select ISR.
        assert!(
            if chip == 0 {
                pic.diagnostic_snapshot().master.read_isr
            } else {
                pic.diagnostic_snapshot().slave.read_isr
            },
            "precondition: PIC {chip} selects ISR"
        );

        pic.write_command(chip, 0x11); // ICW1: cascaded, edge-triggered, ICW4 follows.
        pic.raise_irq(irq);
        assert_eq!(
            pic.read_command(chip),
            pending,
            "ICW1 must immediately select IRR for PIC {chip}"
        );
        assert!(
            !if chip == 0 {
                pic.diagnostic_snapshot().master.read_isr
            } else {
                pic.diagnostic_snapshot().slave.read_isr
            },
            "ICW1 clears ISR selection before ICW2-4 for PIC {chip}"
        );

        complete_icw1_to_icw4(&mut pic, chip);
        assert_eq!(
            pic.read_command(chip),
            pending,
            "PIC {chip} defaults to reading pending IRR after initialization"
        );

        pic.write_command(chip, 0x0b); // OCW3: select ISR again.
        assert_eq!(pic.read_command(chip), 0, "PIC {chip} ISR is empty");
        pic.write_command(chip, 0x0a); // OCW3: select IRR again.
        assert_eq!(pic.read_command(chip), pending, "PIC {chip} IRR is pending");

        pic.lower_irq(irq);
        pic.write_command(chip, 0x0b); // Re-select ISR before reinitializing.
        pic.write_command(chip, 0x11); // Repeat ICW1.
        complete_icw1_to_icw4(&mut pic, chip);
        pic.write_data(chip, !pending); // Unmask only the line under test.
        pic.raise_irq(irq);
        assert_eq!(
            pic.read_command(chip),
            pending,
            "reinitialized PIC {chip} must read a fresh unmasked request from IRR"
        );

        assert!(!if chip == 0 {
            pic.diagnostic_snapshot().master.read_isr
        } else {
            pic.diagnostic_snapshot().slave.read_isr
        });
    }

    #[test]
    fn pic_init_selector_resets_for_master() {
        exercise_pic_init_selector(0);
    }

    #[test]
    fn pic_init_selector_resets_for_slave() {
        exercise_pic_init_selector(1);
    }

    #[test]
    fn poll_checkpoint_and_serialization_preserve_and_icw1_clears_pending() {
        let mut pic = Pic::new();
        pic.slave.mask = 0;
        pic.raise_irq(1);
        pic.raise_irq(11);
        pic.write_command(0, 0x0c);
        pic.write_command(1, 0x0c);
        let saved = pic.clone();
        let mut legacy = serde_json::to_value(&saved).unwrap();
        for chip in ["master", "slave"] {
            legacy[chip].as_object_mut().unwrap().remove("poll_pending");
        }
        let mut legacy: Pic = serde_json::from_value(legacy).unwrap();
        assert_eq!(legacy.read_command(0), 6);
        assert_eq!(legacy.read_command(1), 8);
        let mut restored = Pic::new();
        restored.restore_checkpoint(&saved);
        let mut decoded: Pic =
            serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        for restored in [&mut restored, &mut decoded] {
            assert_eq!(restored.read_command(0), 0x81);
            assert_eq!(restored.read_command(1), 0x83);
            assert_eq!(restored.read_command(0), 0);
        }
        for chip in [0, 1] {
            pic.write_command(chip, 0x11);
            // Complete initialization before normal reads.
            pic.write_data(chip, 0x20);
            pic.write_data(chip, if chip == 0 { 4 } else { 2 });
            pic.write_data(chip, 1);
            pic.write_data(chip, 0);
            pic.raise_irq(chip * 8 + 3);
            assert_eq!(pic.read_command(chip), 8); // Not a poll result.
        }
    }

    #[test]
    fn test_new() {
        let pic = Pic::new();
        // IRQ 0 and 1 should be unmasked
        assert_eq!(pic.master.mask, 0xFC);
        assert_eq!(pic.master.vector_offset, 0x08);
        assert_eq!(pic.slave.vector_offset, 0x70);
    }

    #[test]
    fn test_default() {
        let pic = Pic::default();
        assert_eq!(pic.master.mask, 0xFC);
    }

    #[test]
    fn test_elcr_reserved_bits_are_forced_edge_triggered() {
        let mut pic = Pic::new();

        pic.write_elcr(0x4D0, 0xFF);
        pic.write_elcr(0x4D1, 0xFF);

        assert_eq!(pic.read_elcr(0x4D0), 0xF8);
        assert_eq!(pic.read_elcr(0x4D1), 0xDE);
        assert_eq!(pic.save_elcr(), 0xDEF8);

        pic.restore_elcr(0xFFFF);
        assert_eq!(pic.read_elcr(0x4D0), 0xF8);
        assert_eq!(pic.read_elcr(0x4D1), 0xDE);
    }

    // -- ICW1 LTIM without ELCR (MDD-BUG-CRUCIBLE-00085) --------------------
    //
    // ICW1 bit 3 puts a whole 8259 into level-triggered mode. Assertion has
    // always honoured it; deassertion consulted only the per-line ELCR, so a
    // guest using LTIM alone could latch an IRR bit that nothing could clear.
    // These deliberately leave the ELCR at zero so only LTIM is under test.

    /// ICW1: bit 4 selects ICW1, bit 3 is LTIM, bit 0 says ICW4 follows.
    const ICW1_LTIM: u8 = 0x19;

    // ---- ICW1 sequencing bits (MDD-BUG-CRUCIBLE-00345) ----

    /// Drive an initialization sequence and return the chip's state plus the
    /// mask that resulted from the first operational write after it.
    ///
    /// `sngl` is ICW1 bit 1 (single 8259, no ICW3) and `ic4` is bit 0 (an ICW4
    /// follows). The caller supplies only the words that configuration really
    /// sends — that is the whole point.
    fn init_master_and_write_imr(sngl: bool, ic4: bool, imr: u8) -> Pic {
        let mut pic = Pic::new();
        let mut icw1 = 0x10; // bit 4 marks a command byte as ICW1
        if sngl {
            icw1 |= 0x02;
        }
        if ic4 {
            icw1 |= 0x01;
        }
        pic.write_command(0, icw1);
        pic.write_data(0, 0x08); // ICW2: vector offset
        if !sngl {
            pic.write_data(0, 0x04); // ICW3: slave on IRQ2
        }
        if ic4 {
            pic.write_data(0, 0x01); // ICW4: 8086 mode, normal EOI
        }
        // Initialization is over: this is an ordinary IMR write.
        pic.write_data(0, imr);
        pic
    }

    /// All four SNGL/IC4 combinations must finish initialization after exactly
    /// the words that configuration sends, so the next data write reaches the
    /// IMR. Before the fix the state machine always consumed ICW2, ICW3 and
    /// ICW4, so in three of these four cases the guest's mask was swallowed.
    #[test]
    fn icw1_sequencing_bits_decide_which_words_are_consumed() {
        for (sngl, ic4) in [(false, true), (false, false), (true, true), (true, false)] {
            let imr = 0xAB;
            let pic = init_master_and_write_imr(sngl, ic4, imr);
            assert_eq!(
                pic.master.init_state, 0,
                "SNGL={sngl} IC4={ic4}: initialization must be complete"
            );
            assert_eq!(
                pic.master.mask, imr,
                "SNGL={sngl} IC4={ic4}: the first operational write must reach the IMR"
            );
            assert_eq!(
                pic.master.vector_offset, 0x08,
                "SNGL={sngl} IC4={ic4}: ICW2 must still land"
            );
        }
    }

    /// The classic PC path — cascaded, ICW4 present — is the one that already
    /// worked, pinned so the fix cannot regress it.
    #[test]
    fn cascaded_pc_initialization_still_consumes_all_four_words() {
        let mut pic = Pic::new();
        pic.write_command(0, 0x11); // ICW1: cascade + ICW4
        assert_eq!(pic.master.init_state, 1);
        pic.write_data(0, 0x08);
        assert_eq!(pic.master.init_state, 2, "ICW3 is expected next");
        pic.write_data(0, 0x04);
        assert_eq!(pic.master.init_state, 3, "ICW4 is expected next");
        pic.write_data(0, 0x01);
        assert_eq!(pic.master.init_state, 0);
        pic.write_data(0, 0xFF);
        assert_eq!(pic.master.mask, 0xFF);
    }

    /// A single 8259 sends no ICW3, so ICW2 hands straight to ICW4.
    #[test]
    fn single_mode_skips_icw3() {
        let mut pic = Pic::new();
        pic.write_command(0, 0x13); // ICW1: SNGL + ICW4
        pic.write_data(0, 0x08);
        assert_eq!(
            pic.master.init_state, 3,
            "with no slave there is no cascade word, so ICW4 is next"
        );
    }

    /// With IC4 clear the 8259 defines every ICW4 feature as zero, so a stale
    /// auto-EOI from an earlier initialization must not survive.
    #[test]
    fn omitted_icw4_clears_auto_eoi() {
        let mut pic = Pic::new();
        // First bring it up WITH an ICW4 that enables auto-EOI.
        pic.write_command(0, 0x11);
        pic.write_data(0, 0x08);
        pic.write_data(0, 0x04);
        pic.write_data(0, 0x03); // 8086 mode + AEOI
        assert!(pic.master.auto_eoi, "precondition: auto-EOI is on");

        // Re-initialize with IC4 clear: no ICW4 will arrive.
        pic.write_command(0, 0x12); // SNGL, no ICW4
        assert!(
            !pic.master.auto_eoi,
            "an omitted ICW4 means all ICW4 features are zero"
        );
        pic.write_data(0, 0x08);
        assert_eq!(pic.master.init_state, 0);
        pic.write_data(0, 0x5A);
        assert_eq!(pic.master.mask, 0x5A);
    }

    /// A level-triggered line still high after its handler's EOI requests
    /// again; lowering it before the EOI ends the request (MDD-BUG-KIL-01033).
    #[test]
    fn test_level_line_still_high_after_eoi_is_requested_again_master() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.slave.mask = 0;
        pic.write_elcr(0x4D0, 1 << 5);

        pic.request_interrupt(5);
        assert!(pic.get_interrupt().is_some());
        assert_eq!(pic.master.isr & (1 << 5), 1 << 5);
        assert_eq!(
            pic.get_highest_priority_irq(),
            None,
            "held off while in service"
        );

        pic.end_of_interrupt(5);
        assert_eq!(
            pic.get_highest_priority_irq(),
            Some(5),
            "the line is still high, so the request stands after EOI"
        );

        assert!(pic.get_interrupt().is_some());
        pic.lower_irq(5);
        pic.end_of_interrupt(5);
        assert_eq!(
            pic.get_highest_priority_irq(),
            None,
            "the line fell before EOI"
        );
    }

    /// The same on the slave, through `acknowledge_irq`, with the cascade
    /// re-asserted on the master (MDD-BUG-KIL-01033).
    #[test]
    fn test_level_line_still_high_after_eoi_is_requested_again_slave() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.slave.mask = 0;
        pic.write_elcr(0x4D1, 1 << 3); // IRQ11

        pic.request_interrupt(11);
        assert_eq!(pic.get_highest_priority_irq(), Some(11));
        pic.acknowledge_irq(11);
        assert_eq!(
            pic.get_highest_priority_irq(),
            None,
            "held off while in service"
        );

        pic.end_of_interrupt(11);
        assert_ne!(pic.master.irr & (1 << 2), 0, "the cascade line re-asserts");
        assert_eq!(
            pic.get_highest_priority_irq(),
            Some(11),
            "the line is still high, so the request stands after EOI"
        );

        assert!(pic.get_interrupt().is_some());
        pic.lower_irq(11);
        pic.end_of_interrupt(11);
        assert_eq!(
            pic.get_highest_priority_irq(),
            None,
            "the line fell before EOI"
        );
    }

    #[test]
    fn test_icw1_level_triggered_master_lower_clears_irr_without_elcr() {
        let mut pic = Pic::new();
        pic.write_command(0, ICW1_LTIM);
        pic.master.mask = 0;
        pic.slave.mask = 0;
        assert_eq!(pic.read_elcr(0x4D0), 0, "the ELCR must play no part here");
        assert!(!pic.master.edge_triggered, "ICW1 LTIM must take effect");

        pic.request_interrupt(3);
        assert_ne!(
            pic.master.irr & (1 << 3),
            0,
            "a level-triggered line must latch while high"
        );

        pic.lower_irq(3);
        assert_eq!(
            pic.master.irr & (1 << 3),
            0,
            "a level-triggered line that goes low must drop its latched request"
        );
        assert_eq!(
            pic.get_highest_priority_irq(),
            None,
            "no interrupt may remain pending after the line falls"
        );
    }

    #[test]
    fn test_icw1_level_triggered_slave_lower_clears_irr_without_elcr() {
        let mut pic = Pic::new();
        pic.write_command(1, ICW1_LTIM);
        pic.master.mask = 0;
        pic.slave.mask = 0;
        assert_eq!(pic.read_elcr(0x4D1), 0, "the ELCR must play no part here");
        assert!(!pic.slave.edge_triggered, "ICW1 LTIM must take effect");

        pic.request_interrupt(10);
        assert_ne!(
            pic.slave.irr & (1 << 2),
            0,
            "a level-triggered slave line must latch while high"
        );

        pic.lower_irq(10);
        assert_eq!(
            pic.slave.irr & (1 << 2),
            0,
            "a level-triggered slave line that goes low must drop its latched request"
        );
        assert_eq!(
            pic.master.irr & (1 << 2),
            0,
            "the cascade line must fall with the last slave request"
        );
        assert_eq!(
            pic.get_highest_priority_irq(),
            None,
            "no interrupt may remain pending after the line falls"
        );
    }

    /// LTIM must not break the edge-triggered default: an edge-triggered line
    /// keeps its latched request across a deassert (the capture-at-assert rule
    /// behind KILN-00004), so the shared predicate must stay false there.
    #[test]
    fn test_edge_triggered_lower_still_retains_the_latched_request() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        assert!(
            pic.master.edge_triggered,
            "the PC default is edge-triggered"
        );

        pic.request_interrupt(3);
        assert_ne!(pic.master.irr & (1 << 3), 0);

        pic.lower_irq(3);
        assert_ne!(
            pic.master.irr & (1 << 3),
            0,
            "an edge-triggered request survives the line falling"
        );
    }

    #[test]
    fn test_elcr_master_level_triggered_irq_reasserts_without_lower_edge() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.write_elcr(0x4D0, 1 << 3);

        pic.request_interrupt(3);
        assert_ne!(pic.master.irr & (1 << 3), 0);
        pic.acknowledge_irq(3);
        // The line is still high, so the acknowledge leaves the request
        // standing (MDD-BUG-KIL-01033); it used to read 0 here.
        assert_ne!(pic.master.irr & (1 << 3), 0);

        pic.request_interrupt(3);
        assert_ne!(
            pic.master.irr & (1 << 3),
            0,
            "ELCR level-triggered IRQ should reassert while line remains high"
        );
    }

    #[test]
    fn test_elcr_slave_level_triggered_irq_reasserts_without_lower_edge() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.slave.mask = 0;
        pic.write_elcr(0x4D1, 1 << 2);

        pic.request_interrupt(10);
        assert_ne!(pic.slave.irr & (1 << 2), 0);
        let _ = pic.get_interrupt();
        // Still high, so still requested through the INTA (MDD-BUG-KIL-01033).
        assert_ne!(pic.slave.irr & (1 << 2), 0);

        pic.request_interrupt(10);
        assert_ne!(
            pic.slave.irr & (1 << 2),
            0,
            "ELCR level-triggered slave IRQ should reassert while line remains high"
        );
        assert_eq!(pic.master.irr & (1 << 2), 0, "service blocks slave INT");
        pic.write_command(1, 0x20);
        assert_ne!(pic.master.irr & (1 << 2), 0);
    }

    #[test]
    fn test_ocw3_read_register_select() {
        let mut pic = Pic::new();

        pic.request_interrupt(1);
        pic.write_command(0, 0x0A); // OCW3: read IRR
        assert_eq!(pic.read_command(0), 0x02);

        pic.acknowledge_irq(1);
        pic.write_command(0, 0x0B); // OCW3: read ISR
        assert_eq!(pic.read_command(0), 0x02);
    }

    #[test]
    fn test_diagnostic_snapshot_exposes_master_and_slave_state() {
        let mut pic = Pic::new();
        pic.write_data(0, 0xF8);
        pic.write_data(1, 0xFE);
        pic.request_interrupt(8);

        let snapshot = pic.diagnostic_snapshot();

        assert_eq!(snapshot.master.imr, 0xF8);
        assert_ne!(snapshot.master.irr & 0x04, 0);
        assert_eq!(snapshot.slave.imr, 0xFE);
        assert_ne!(snapshot.slave.irr & 0x01, 0);
        assert_ne!(snapshot.slave.irq_line & 0x01, 0);
        assert_eq!(snapshot.highest_priority_irq, Some(8));
    }

    #[test]
    fn test_decode_diagnostic_port_access_command_kinds() {
        let mut pic = Pic::new();
        let before = pic.diagnostic_snapshot();

        assert_eq!(
            Pic::decode_diagnostic_port_access(0x20, 0x20, true, &before),
            "non_specific_eoi"
        );
        assert_eq!(
            Pic::decode_diagnostic_port_access(0x20, 0x63, true, &before),
            "specific_eoi"
        );
        assert_eq!(
            Pic::decode_diagnostic_port_access(0x20, 0x0A, true, &before),
            "ocw3_read_irr"
        );
        assert_eq!(
            Pic::decode_diagnostic_port_access(0x20, 0x0B, true, &before),
            "ocw3_read_isr"
        );
        assert_eq!(
            Pic::decode_diagnostic_port_access(0xA0, 0x11, true, &before),
            "icw1"
        );

        pic.write_command(0, 0x0B);
        let before_read = pic.diagnostic_snapshot();
        assert_eq!(
            Pic::decode_diagnostic_port_access(0x20, 0, false, &before_read),
            "read_isr"
        );
    }

    #[test]
    fn test_decode_diagnostic_port_access_data_kinds_follow_init_state() {
        let mut pic = Pic::new();
        let before_mask = pic.diagnostic_snapshot();
        assert_eq!(
            Pic::decode_diagnostic_port_access(0x21, 0xFE, true, &before_mask),
            "mask_write"
        );
        assert_eq!(
            Pic::decode_diagnostic_port_access(0x21, 0, false, &before_mask),
            "mask_read"
        );

        pic.write_command(0, 0x11);
        let before_icw2 = pic.diagnostic_snapshot();
        assert_eq!(
            Pic::decode_diagnostic_port_access(0x21, 0x08, true, &before_icw2),
            "icw2"
        );

        pic.write_data(0, 0x08);
        let before_icw3 = pic.diagnostic_snapshot();
        assert_eq!(
            Pic::decode_diagnostic_port_access(0x21, 0x04, true, &before_icw3),
            "icw3"
        );

        pic.write_data(0, 0x04);
        let before_icw4 = pic.diagnostic_snapshot();
        assert_eq!(
            Pic::decode_diagnostic_port_access(0x21, 0x01, true, &before_icw4),
            "icw4"
        );
    }

    #[test]
    fn test_request_interrupt_master() {
        let mut pic = Pic::new();
        pic.request_interrupt(0);
        assert_eq!(pic.master.irr, 0x01);

        pic.request_interrupt(7);
        assert_eq!(pic.master.irr, 0x81);
    }

    #[test]
    fn test_request_interrupt_slave() {
        let mut pic = Pic::new();
        pic.slave.mask = 0xFE;
        pic.request_interrupt(8);

        // Slave IRQ 0 should be set
        assert_eq!(pic.slave.irr, 0x01);
        // Cascade IRQ 2 on master should also be set
        assert_eq!(pic.master.irr, 0x04);
    }

    #[test]
    fn test_request_interrupt_out_of_range_noop() {
        let mut pic = Pic::new();
        pic.master.irr = 0x12;
        pic.slave.irr = 0x34;

        pic.request_interrupt(16);

        assert_eq!(pic.master.irr, 0x12);
        assert_eq!(pic.slave.irr, 0x34);
    }

    #[test]
    fn test_raise_irq() {
        let mut pic = Pic::new();
        pic.raise_irq(1);
        assert_eq!(pic.master.irr, 0x02);
    }

    #[test]
    fn test_get_irr() {
        let mut pic = Pic::new();
        pic.request_interrupt(3);
        assert_eq!(pic.get_irr(), 0x08);
    }

    #[test]
    fn test_is_irq_masked() {
        let pic = Pic::new();

        // IRQ 0 and 1 are unmasked (mask = 0xFC)
        assert!(!pic.is_irq_masked(0));
        assert!(!pic.is_irq_masked(1));

        // IRQ 2-7 are masked
        assert!(pic.is_irq_masked(2));
        assert!(pic.is_irq_masked(7));

        // Slave IRQs should be checked against slave mask (0xFF = all masked)
        assert!(pic.is_irq_masked(8));
        assert!(pic.is_irq_masked(15));
    }

    #[test]
    fn test_is_irq_masked_out_of_range() {
        let pic = Pic::new();
        assert!(pic.is_irq_masked(16));
        assert!(pic.is_irq_masked(255));
    }

    #[test]
    fn test_get_highest_priority_irq() {
        let mut pic = Pic::new();

        // No pending interrupts
        assert!(pic.get_highest_priority_irq().is_none());

        // Request IRQ 0 (unmasked)
        pic.request_interrupt(0);
        assert_eq!(pic.get_highest_priority_irq(), Some(0));

        // Request IRQ 1 (also unmasked, but lower priority)
        pic.request_interrupt(1);
        assert_eq!(pic.get_highest_priority_irq(), Some(0)); // Still IRQ 0

        // Clear IRQ 0
        pic.master.irr = 0x02;
        assert_eq!(pic.get_highest_priority_irq(), Some(1));
    }

    #[test]
    fn test_get_highest_priority_irq_masked() {
        let mut pic = Pic::new();

        // Request a masked IRQ
        pic.request_interrupt(3);
        // IRQ 3 is masked, so should return None
        assert!(pic.get_highest_priority_irq().is_none());
    }

    #[test]
    fn test_priority_blocks_lower_irqs() {
        let mut pic = Pic::new();
        pic.master.mask = 0xF0; // Unmask IRQ0-3

        pic.master.isr = 0x02; // IRQ1 in service
        pic.request_interrupt(3);

        assert!(pic.get_highest_priority_irq().is_none());

        pic.request_interrupt(0);
        assert_eq!(pic.get_highest_priority_irq(), Some(0));
    }

    /// The PIC-side contract that lets `int74_mouse` enable interrupts around the
    /// C207 driver far-call: with IRQ12 in service and its
    /// EOI still pending, the higher-priority IRQ0/IRQ1 must remain deliverable —
    /// that is the whole point of the STI — while IRQ12 itself and everything at
    /// or below the cascade's priority stays blocked, so a non-reentrant mouse
    /// driver cannot be re-entered on top of itself.
    #[test]
    fn in_service_irq12_admits_irq0_and_irq1_but_blocks_its_peers() {
        let mut pic = Pic::new();
        pic.master.mask = 0x00; // unmask IRQ0-7 (incl. the IRQ2 cascade)
        pic.slave.mask = 0x00; // unmask IRQ8-15

        pic.request_interrupt(12);
        assert_eq!(pic.get_highest_priority_irq(), Some(12));
        pic.acknowledge_irq(12);
        assert!(
            pic.is_irq_in_service(12),
            "acknowledge must put IRQ12 in service"
        );
        assert_eq!(
            pic.master.isr & 0x04,
            0x04,
            "a slave ack must also hold the master's cascade bit in service"
        );

        // Re-entrancy: a second mouse packet must NOT re-enter the driver.
        pic.lower_irq(12);
        pic.request_interrupt(12);
        assert_eq!(
            pic.get_highest_priority_irq(),
            None,
            "IRQ12 must not re-enter itself while its own EOI is pending"
        );

        // A lower-priority master IRQ stays behind the in-service cascade.
        pic.request_interrupt(5);
        assert_eq!(
            pic.get_highest_priority_irq(),
            None,
            "IRQ5 is below the IRQ2 cascade and must stay blocked"
        );

        // ...but the two IRQs above the cascade preempt, which is what the STI buys.
        pic.request_interrupt(1);
        assert_eq!(
            pic.get_highest_priority_irq(),
            Some(1),
            "IRQ1 outranks the cascade and must preempt an in-service IRQ12"
        );
        pic.request_interrupt(0);
        assert_eq!(
            pic.get_highest_priority_irq(),
            Some(0),
            "IRQ0 outranks IRQ1 and must be selected first"
        );
    }

    #[test]
    fn test_acknowledge_irq_master() {
        let mut pic = Pic::new();
        pic.request_interrupt(0);

        pic.acknowledge_irq(0);
        // IRR should be cleared
        assert_eq!(pic.master.irr, 0);
        // ISR should be set (auto_eoi is false by default)
        assert_eq!(pic.master.isr, 0x01);
    }

    #[test]
    fn test_acknowledge_irq_slave() {
        let mut pic = Pic::new();
        pic.request_interrupt(9);

        pic.acknowledge_irq(9);
        // Slave IRR should be cleared
        assert_eq!(pic.slave.irr, 0);
        // Master cascade IRR should be cleared
        assert_eq!(pic.master.irr & 0x04, 0);
        // Slave ISR should be set
        assert_eq!(pic.slave.isr, 0x02);
        // Master cascade ISR should also be set
        assert_eq!(pic.master.isr, 0x04);
    }

    #[test]
    fn test_acknowledge_irq_slave_auto_eoi() {
        let mut pic = Pic::new();
        pic.master.auto_eoi = true;
        pic.slave.auto_eoi = true;

        pic.request_interrupt(9);
        pic.acknowledge_irq(9);

        assert_eq!(pic.slave.isr, 0);
        assert_eq!(pic.master.isr & 0x04, 0);
    }

    #[test]
    fn test_acknowledge_irq_slave_reasserts_cascade_when_pending() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.slave.mask = 0;

        pic.request_interrupt(9);
        pic.request_interrupt(10);

        pic.acknowledge_irq(9);

        assert_eq!(pic.slave.irr, 0x04);
        assert_eq!(pic.master.irr & 0x04, 0, "IRQ9 service blocks IRQ10 INT");
        pic.write_command(0, 0x20);
        pic.write_command(1, 0x61);
        assert_eq!(pic.master.irr & 0x04, 0x04);
    }

    #[test]
    fn test_acknowledge_irq_out_of_range_noop() {
        let mut pic = Pic::new();
        pic.master.irr = 0x12;
        pic.slave.irr = 0x34;
        pic.master.isr = 0x56;
        pic.slave.isr = 0x78;

        pic.acknowledge_irq(16);

        assert_eq!(pic.master.irr, 0x12);
        assert_eq!(pic.slave.irr, 0x34);
        assert_eq!(pic.master.isr, 0x56);
        assert_eq!(pic.slave.isr, 0x78);
    }

    #[test]
    fn test_slave_irq_cascade_reasserts_for_pending() {
        let mut pic = Pic::new();
        pic.master.mask = 0x00; // Unmask all on master
        pic.slave.mask = 0x00; // Unmask all on slave

        pic.request_interrupt(9);
        pic.request_interrupt(10);

        let first = pic.get_interrupt();
        assert_eq!(first, Some(0x71)); // IRQ9 -> vector 0x70 + 1
        pic.end_of_interrupt(9);

        let second = pic.get_interrupt();
        assert_eq!(second, Some(0x72)); // IRQ10 -> vector 0x70 + 2
    }

    #[test]
    fn test_end_of_interrupt_master() {
        let mut pic = Pic::new();
        pic.master.isr = 0x01;

        pic.end_of_interrupt(0);
        assert_eq!(pic.master.isr, 0);
    }

    #[test]
    fn test_end_of_interrupt_slave() {
        let mut pic = Pic::new();
        pic.slave.isr = 0x02;
        pic.master.isr = 0x04; // Cascade

        pic.end_of_interrupt(9);
        assert_eq!(pic.slave.isr, 0);
        assert_eq!(pic.master.isr, 0); // Cascade also cleared
    }

    #[test]
    fn test_end_of_interrupt_out_of_range_noop() {
        let mut pic = Pic::new();
        pic.master.isr = 0x12;
        pic.slave.isr = 0x34;

        pic.end_of_interrupt(16);

        assert_eq!(pic.master.isr, 0x12);
        assert_eq!(pic.slave.isr, 0x34);
    }

    #[test]
    fn test_get_interrupt() {
        let mut pic = Pic::new();

        // No pending
        assert!(pic.get_interrupt().is_none());

        // Request IRQ 0
        pic.request_interrupt(0);
        let int = pic.get_interrupt();
        assert_eq!(int, Some(0x08)); // Vector offset + IRQ

        // IRR cleared, ISR set
        assert_eq!(pic.master.irr, 0);
        assert_eq!(pic.master.isr, 0x01);
    }

    #[test]
    fn test_get_interrupt_blocks_lower_priority() {
        let mut pic = Pic::new();
        pic.master.mask = 0xF0; // Unmask IRQ0-3
        pic.master.isr = 0x02; // IRQ1 in service

        pic.request_interrupt(3);
        assert!(pic.get_interrupt().is_none());

        pic.request_interrupt(0);
        assert_eq!(pic.get_interrupt(), Some(0x08));
    }

    #[test]
    fn test_get_interrupt_slave() {
        let mut pic = Pic::new();
        // Unmask cascade IRQ 2 on master
        pic.master.mask = 0xF8;
        // Unmask IRQ 8 on slave
        pic.slave.mask = 0xFE;

        pic.request_interrupt(8);

        let int = pic.get_interrupt();
        assert_eq!(int, Some(0x70)); // Slave vector offset + slave IRQ 0
        assert_eq!(pic.master.irr & 0x04, 0);
        assert_eq!(pic.master.isr & 0x04, 0x04);
    }

    #[test]
    fn test_write_command_icw1() {
        let mut pic = Pic::new();
        pic.master.mask = 0xFF;
        pic.master.isr = 0x55;
        pic.master.irr = 0xAA;

        // ICW1 (bit 4 set)
        pic.write_command(0, 0x11);

        assert_eq!(pic.master.init_state, 1);
        assert_eq!(pic.master.mask, 0);
        assert_eq!(pic.master.isr, 0);
        assert_eq!(pic.master.irr, 0);
    }

    #[test]
    fn test_write_command_eoi_nonspecific() {
        let mut pic = Pic::new();
        pic.master.isr = 0x05; // IRQ 0 and IRQ 2 in service

        // Non-specific EOI (0x20)
        pic.write_command(0, 0x20);

        // Should clear lowest priority in-service IRQ (IRQ 0)
        assert_eq!(pic.master.isr, 0x04);
    }

    #[test]
    fn test_write_command_eoi_specific() {
        let mut pic = Pic::new();
        pic.master.isr = 0x05; // IRQ 0 and IRQ 2 in service

        // Specific EOI for IRQ 2 (0x60 | 2 = 0x62)
        pic.write_command(0, 0x62);

        assert_eq!(pic.master.isr, 0x01); // Only IRQ 0 remains
    }

    #[test]
    fn test_write_data_initialization() {
        let mut pic = Pic::new();

        // Start initialization
        pic.write_command(0, 0x11);
        assert_eq!(pic.master.init_state, 1);

        // ICW2: vector offset
        pic.write_data(0, 0x20);
        assert_eq!(pic.master.vector_offset, 0x20);
        assert_eq!(pic.master.init_state, 2);

        // ICW3: cascade
        pic.write_data(0, 0x04);
        assert_eq!(pic.master.init_state, 3);

        // ICW4: mode
        pic.write_data(0, 0x01);
        assert!(!pic.master.auto_eoi);
        assert_eq!(pic.master.init_state, 0);
    }

    #[test]
    fn test_write_data_auto_eoi() {
        let mut pic = Pic::new();

        pic.write_command(0, 0x11);
        pic.write_data(0, 0x20); // ICW2
        pic.write_data(0, 0x04); // ICW3
        pic.write_data(0, 0x03); // ICW4 with auto EOI

        assert!(pic.master.auto_eoi);
    }

    #[test]
    fn test_write_data_mask() {
        let mut pic = Pic::new();

        // When not in init state, write to data port sets mask
        pic.write_data(0, 0xAB);
        assert_eq!(pic.master.mask, 0xAB);
    }

    #[test]
    fn test_write_data_mask_no_change() {
        let mut pic = Pic::new();
        pic.master.mask = 0xAA;

        pic.write_data(0, 0xAA);
        assert_eq!(pic.master.mask, 0xAA);
    }

    #[test]
    fn test_write_data_slave_updates_cascade() {
        let mut pic = Pic::new();
        pic.slave.irr = 0x01;

        // Mask all slave IRQs - cascade should NOT be asserted since all are masked
        pic.write_data(1, 0xFF);
        assert_eq!(pic.master.irr & 0x04, 0x00);

        // Unmask slave IRQ 0 - cascade SHOULD be asserted now
        pic.write_data(1, 0xFE);
        assert_eq!(pic.master.irr & 0x04, 0x04);
    }

    #[test]
    fn test_read_command() {
        let mut pic = Pic::new();
        pic.master.isr = 0x42;
        pic.slave.isr = 0x24;
        pic.master.read_isr = true;
        pic.slave.read_isr = true;

        assert_eq!(pic.read_command(0), 0x42);
        assert_eq!(pic.read_command(1), 0x24);
    }

    #[test]
    fn test_read_data() {
        let mut pic = Pic::new();
        pic.master.mask = 0x12;
        pic.slave.mask = 0x34;

        assert_eq!(pic.read_data(0), 0x12);
        assert_eq!(pic.read_data(1), 0x34);
    }

    #[test]
    fn test_auto_eoi_mode() {
        let mut pic = Pic::new();

        // Initialize with auto EOI
        pic.write_command(0, 0x11);
        pic.write_data(0, 0x08);
        pic.write_data(0, 0x04);
        pic.write_data(0, 0x03); // Auto EOI

        pic.master.mask = 0x00; // Unmask all

        pic.request_interrupt(0);
        let int = pic.get_interrupt();
        assert_eq!(int, Some(0x08));

        // With auto EOI, ISR should NOT be set
        assert_eq!(pic.master.isr, 0);
    }

    #[test]
    fn test_acknowledge_auto_eoi() {
        let mut pic = Pic::new();
        pic.master.auto_eoi = true;

        pic.request_interrupt(0);
        pic.acknowledge_irq(0);

        // With auto EOI, ISR should not be set
        assert_eq!(pic.master.isr, 0);
    }

    #[test]
    fn test_isr_blocks_lower_priority() {
        let mut pic = Pic::new();
        pic.master.mask = 0x00; // Unmask all

        // IRQ 0 in service
        pic.master.isr = 0x01;

        // Request IRQ 0 again
        pic.request_interrupt(0);

        // Should not be returned (already in service)
        let int = pic.get_highest_priority_irq();
        assert!(int.is_none());
    }

    /// MDD-BUG-FLUX-00169: 0x68 is OCW3 "enter special mask mode", not an
    /// OCW2 specific EOI.
    ///
    /// The decode tested bit 5 as part of the OCW2/OCW3 discriminator, but bit
    /// 5 is ESMM in OCW3 and EOI in OCW2 — the discriminator is bit 3 alone,
    /// with bit 4 clear. So 0x68 (0110_1000) fell through to the OCW2 arm and
    /// matched (R=0, SL=1, EOI=1) = specific EOI with L=0, silently clearing
    /// IRQ0's in-service bit.
    #[test]
    fn ocw3_enter_special_mask_is_not_decoded_as_a_specific_eoi_for_irq0() {
        let mut pic = Pic::new();
        pic.master.isr = 0b0000_1001; // IRQ0 and IRQ3 in service

        pic.write_command(0, 0x68); // OCW3: ESMM=1, SMM=1

        assert_eq!(
            pic.master.isr, 0b0000_1001,
            "0x68 is OCW3, not a specific EOI — it must not touch the ISR"
        );
        assert!(pic.master.special_mask, "0x68 must enter special mask mode");
    }

    /// MDD-BUG-FLUX-00169: 0x48 (ESMM=1, SMM=0) leaves special mask mode.
    /// It was decoded as OCW3 but its SMM bit was ignored entirely.
    #[test]
    fn ocw3_exit_special_mask_clears_the_mode() {
        let mut pic = Pic::new();
        pic.write_command(0, 0x68);
        assert!(pic.master.special_mask);

        pic.write_command(0, 0x48); // OCW3: ESMM=1, SMM=0
        assert!(
            !pic.master.special_mask,
            "0x48 must leave special mask mode"
        );
    }

    /// MDD-BUG-FLUX-00169, the behaviour the decode bug hid: the record's own
    /// repro sketch. IRQ3 is in service; the guest masks IRQ3 and enters
    /// special mask mode so a LOWER-priority IRQ4 can nest.
    ///
    /// Per the Intel 8259A datasheet, masking a level in this mode inhibits
    /// that level and enables every other unmasked level, lower priority as
    /// well as higher. Without the mode, IRQ3's in-service bit blocks IRQ4 and
    /// the guest waits indefinitely.
    #[test]
    fn special_mask_mode_lets_a_lower_priority_irq_nest_under_a_masked_in_service_level() {
        let mut pic = Pic::new();
        pic.master.mask = 0x00;
        pic.master.isr = 1 << 3; // IRQ3 in service
        pic.request_interrupt(4);

        // Normal priority: IRQ3 in service blocks the lower-priority IRQ4.
        assert_eq!(
            pic.get_highest_priority_irq(),
            None,
            "without special mask mode an in-service IRQ3 must block IRQ4"
        );

        // The guest masks its own level, then enters special mask mode.
        pic.write_data(0, 1 << 3);
        pic.write_command(0, 0x68);

        assert_eq!(
            pic.get_highest_priority_irq(),
            Some(4),
            "special mask mode must let IRQ4 nest under a MASKED in-service IRQ3"
        );

        // Leaving the mode restores normal priority blocking.
        pic.write_command(0, 0x48);
        assert_eq!(
            pic.get_highest_priority_irq(),
            None,
            "leaving special mask mode must restore normal priority blocking"
        );
    }

    /// MDD-BUG-FLUX-00169: special mask mode only unblocks levels the guest has
    /// actually masked. An UNMASKED in-service level still blocks normally, so
    /// the mode cannot be used to defeat priority wholesale.
    #[test]
    fn special_mask_mode_does_not_unblock_an_unmasked_in_service_level() {
        let mut pic = Pic::new();
        pic.master.mask = 0x00;
        pic.master.isr = 1 << 3;
        pic.request_interrupt(4);

        pic.write_command(0, 0x68); // in the mode, but IRQ3 is NOT masked

        assert_eq!(
            pic.get_highest_priority_irq(),
            None,
            "an unmasked in-service level must keep blocking even in special mask mode"
        );
    }

    /// MDD-BUG-FLUX-00169: an ICW1 re-initialisation clears special mask mode,
    /// so a re-init cannot inherit it from whatever ran before.
    #[test]
    fn icw1_clears_special_mask_mode() {
        let mut pic = Pic::new();
        pic.write_command(0, 0x68);
        assert!(pic.master.special_mask);

        pic.write_command(0, 0x11); // ICW1
        assert!(!pic.master.special_mask);
    }

    #[test]
    fn test_slave_operations() {
        let mut pic = Pic::new();

        // Write command to slave
        pic.write_command(1, 0x11);
        assert_eq!(pic.slave.init_state, 1);

        // Write data to slave
        pic.write_data(1, 0x70);
        assert_eq!(pic.slave.vector_offset, 0x70);
    }

    // Phase 4: Tests for is_irq_in_service
    #[test]
    fn test_is_irq_in_service_master() {
        let mut pic = Pic::new();

        // Initially, no IRQs in service
        assert!(!pic.is_irq_in_service(0));
        assert!(!pic.is_irq_in_service(1));
        assert!(!pic.is_irq_in_service(7));

        // Set IRQ1 in service (keyboard)
        pic.master.isr = 0x02;
        assert!(!pic.is_irq_in_service(0));
        assert!(pic.is_irq_in_service(1));
        assert!(!pic.is_irq_in_service(7));

        // Set multiple IRQs in service
        pic.master.isr = 0x83; // IRQs 0, 1, 7
        assert!(pic.is_irq_in_service(0));
        assert!(pic.is_irq_in_service(1));
        assert!(pic.is_irq_in_service(7));
        assert!(!pic.is_irq_in_service(2));
    }

    #[test]
    fn test_is_irq_in_service_slave() {
        let mut pic = Pic::new();

        // Initially, no slave IRQs in service
        assert!(!pic.is_irq_in_service(8));
        assert!(!pic.is_irq_in_service(15));

        // Set IRQ9 in service
        pic.slave.isr = 0x02;
        assert!(!pic.is_irq_in_service(8));
        assert!(pic.is_irq_in_service(9));
        assert!(!pic.is_irq_in_service(15));

        // Set multiple slave IRQs in service
        pic.slave.isr = 0x81; // IRQs 8 and 15
        assert!(pic.is_irq_in_service(8));
        assert!(pic.is_irq_in_service(15));
        assert!(!pic.is_irq_in_service(9));
    }

    #[test]
    fn test_is_irq_in_service_out_of_range() {
        let pic = Pic::new();
        // Out of range should return false
        assert!(!pic.is_irq_in_service(16));
        assert!(!pic.is_irq_in_service(255));
    }

    #[test]
    fn test_is_irq_in_service_after_acknowledge() {
        let mut pic = Pic::new();

        // Request and acknowledge IRQ1
        pic.request_interrupt(1);
        assert!(!pic.is_irq_in_service(1));

        pic.acknowledge_irq(1);
        assert!(pic.is_irq_in_service(1)); // Now in service

        // After EOI, should no longer be in service
        pic.end_of_interrupt(1);
        assert!(!pic.is_irq_in_service(1));
    }

    #[test]
    fn test_get_highest_priority_irq_master() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.master.irr = (1 << 4) | (1 << 1);
        assert_eq!(pic.get_highest_priority_irq(), Some(1));
    }

    #[test]
    fn test_get_highest_priority_irq_slave_cascade() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.slave.mask = 0;
        pic.master.irr = 1 << 2;
        pic.slave.irr = 1 << 5;
        assert_eq!(pic.get_highest_priority_irq(), Some(13));
    }

    #[test]
    fn test_acknowledge_irq_master_and_slave() {
        let mut pic = Pic::new();

        pic.request_interrupt(1);
        pic.acknowledge_irq(1);
        assert_eq!(pic.master.irr & (1 << 1), 0);
        assert_ne!(pic.master.isr & (1 << 1), 0);

        pic.request_interrupt(10);
        pic.acknowledge_irq(10);
        assert_eq!(pic.slave.irr & (1 << 2), 0);
        assert_eq!(pic.master.irr & (1 << 2), 0);
        assert_ne!(pic.slave.isr & (1 << 2), 0);
        assert_ne!(pic.master.isr & (1 << 2), 0);
    }

    #[test]
    fn test_get_interrupt_vectors_master_and_slave() {
        let mut pic = Pic::new();
        pic.master.mask = 0;

        pic.request_interrupt(1);
        assert_eq!(pic.get_interrupt(), Some(0x09));
        assert_ne!(pic.master.isr & (1 << 1), 0);
        pic.end_of_interrupt(1);

        pic.slave.mask = 0;
        pic.request_interrupt(8);
        assert_eq!(pic.get_interrupt(), Some(0x70));
        assert_ne!(pic.slave.isr & 1, 0);
        assert_ne!(pic.master.isr & (1 << 2), 0);
    }

    #[test]
    fn test_end_of_interrupt_clears_isr() {
        let mut pic = Pic::new();
        pic.master.isr = 1 << 1;
        pic.slave.isr = 1 << 3;
        pic.master.isr |= 1 << 2;

        pic.end_of_interrupt(1);
        assert_eq!(pic.master.isr & (1 << 1), 0);

        pic.end_of_interrupt(11);
        assert_eq!(pic.slave.isr & (1 << 3), 0);
        assert_eq!(pic.master.isr & (1 << 2), 0);
    }

    #[test]
    fn test_write_command_and_data_init_sequence() {
        let mut pic = Pic::new();

        pic.write_command(0, 0x11);
        assert_eq!(pic.master.init_state, 1);
        assert_eq!(pic.master.mask, 0);
        assert_eq!(pic.master.isr, 0);
        assert_eq!(pic.master.irr, 0);

        pic.write_data(0, 0x20);
        assert_eq!(pic.master.vector_offset, 0x20);
        assert_eq!(pic.master.init_state, 2);

        pic.write_data(0, 0x04);
        assert_eq!(pic.master.init_state, 3);

        pic.write_data(0, 0x03);
        assert!(pic.master.auto_eoi);
        assert_eq!(pic.master.init_state, 0);
    }

    #[test]
    fn test_write_command_eoi_variants() {
        let mut pic = Pic::new();
        pic.master.isr = 0b0001_0100;

        pic.write_command(0, 0x20);
        assert_eq!(pic.master.isr & (1 << 2), 0);
        assert_ne!(pic.master.isr & (1 << 4), 0);

        pic.write_command(0, 0x64);
        assert_eq!(pic.master.isr & (1 << 4), 0);
    }

    #[test]
    fn test_read_command_and_data() {
        let mut pic = Pic::new();
        pic.master.isr = 0x12;
        pic.master.mask = 0x34;
        pic.master.read_isr = true;
        assert_eq!(pic.read_command(0), 0x12);
        assert_eq!(pic.read_data(0), 0x34);
    }

    #[test]
    fn test_priority_mask_same_irq_blocked() {
        let mut pic = Pic::new();
        pic.master.mask = 0; // Unmask all

        // IRQ 1 in service
        pic.request_interrupt(1);
        pic.acknowledge_irq(1);
        assert_eq!(pic.master.isr, 0x02);

        // Request IRQ 1 again
        pic.request_interrupt(1);

        // Should be blocked by itself
        assert!(pic.get_highest_priority_irq().is_none());

        // Request IRQ 0 (higher priority)
        pic.request_interrupt(0);
        assert_eq!(pic.get_highest_priority_irq(), Some(0));
    }

    #[test]
    fn test_get_isr_and_mask_debugging() {
        let mut pic = Pic::new();
        pic.master.mask = 0xAA;
        pic.master.isr = 0x55;
        assert_eq!(pic.get_mask(), 0xAA);
        assert_eq!(pic.get_isr(), 0x55);
    }

    #[test]
    #[cfg(not(coverage))]
    fn test_request_interrupt_boundary_8() {
        let mut pic = Pic::new();
        pic.slave.mask = 0; // Unmask slave IRQs so cascade line is asserted

        // Test IRQ 8 specifically
        pic.request_interrupt(8);
        assert_eq!(pic.slave.irr, 0x01);
        assert_eq!(pic.master.irr, 0x04); // Cascade (only set when slave IRQ is unmasked)

        // Test IRQ 7 specifically
        pic.request_interrupt(7);
        assert_eq!(pic.master.irr, 0x84);
    }

    #[test]
    fn test_is_irq_masked_logic() {
        let mut pic = Pic::new();
        pic.master.mask = 0x80; // Only IRQ 7 masked
        pic.slave.mask = 0x01; // Only IRQ 8 masked

        assert!(!pic.is_irq_masked(0));
        assert!(pic.is_irq_masked(7));
        assert!(pic.is_irq_masked(8));
        assert!(!pic.is_irq_masked(15));
    }

    #[test]
    fn test_acknowledge_irq_boundary() {
        let mut pic = Pic::new();

        // IRQ 7
        pic.request_interrupt(7);
        pic.acknowledge_irq(7);
        assert_eq!(pic.master.isr, 0x80);

        // IRQ 15
        pic.request_interrupt(15);
        pic.acknowledge_irq(15);
        assert_eq!(pic.slave.isr, 0x80);
        assert_eq!(pic.master.isr, 0x84); // 0x80 | 0x04
    }

    #[test]
    fn test_get_interrupt_cascade_complex() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.slave.mask = 0;

        // Request slave IRQ 1 (IRQ 9) and slave IRQ 0 (IRQ 8)
        pic.request_interrupt(9);
        pic.request_interrupt(8);

        // Highest priority should be IRQ 8
        assert_eq!(pic.get_interrupt(), Some(0x70));
        assert_eq!(pic.slave.irr, 0x02); // IRQ 9 remains
        assert_eq!(pic.slave.isr, 0x01); // IRQ 8 in service

        // IRQ9 remains latched, but slave INT is blocked by IRQ8 service.
        assert_eq!(pic.master.irr & 0x04, 0);

        // However, master.isr bit 2 is ALSO set (IRQ 2 in service)
        assert_eq!(pic.master.isr & 0x04, 0x04);

        // Next get_interrupt() should return None because master IRQ 2 is already in service
        // (Blocking further interrupts of same or lower priority from the slave)
        assert_eq!(pic.get_interrupt(), None);

        // After EOI for slave IRQ 8 (which also clears master IRQ 2),
        // the master should see the pending cascade again
        pic.end_of_interrupt(8);
        assert_eq!(pic.slave.isr & 0x01, 0);
        assert_eq!(pic.master.isr & 0x04, 0);
        assert_eq!(pic.master.irr & 0x04, 0x04);
        assert_eq!(pic.get_interrupt(), Some(0x71)); // Now gets IRQ 9
    }

    #[test]
    fn test_write_command_ocw3_logic() {
        let mut pic = Pic::new();
        pic.master.irr = 0xAA;
        pic.master.isr = 0x55;

        // OCW3: Bit 3=1, Bit 1=1 (RR), Bit 0=0 (IRR)
        pic.write_command(0, 0x0A);
        assert_eq!(pic.read_command(0), 0xAA);

        // OCW3: Bit 3=1, Bit 1=1 (RR), Bit 0=1 (ISR)
        pic.write_command(0, 0x0B);
        assert_eq!(pic.read_command(0), 0x55);
    }

    #[test]
    fn test_write_command_ocw3_without_rr_does_not_toggle() {
        let mut pic = Pic::new();
        pic.master.read_isr = true;

        pic.write_command(0, 0x08);
        assert!(pic.master.read_isr);
    }

    #[test]
    fn test_write_command_noop_when_no_command_bits_set() {
        let mut pic = Pic::new();
        pic.master.read_isr = true;

        pic.write_command(0, 0x00);
        assert!(pic.master.read_isr);
    }

    #[test]
    fn test_write_command_specific_eoi_slave() {
        let mut pic = Pic::new();
        pic.slave.isr = 0x81; // IRQ 8 and 15 in service

        // Specific EOI for IRQ 15 (slave IRQ 7)
        pic.write_command(1, 0x67); // 0110 0111 -> Bit 5=1 (EOI), Bit 6=1 (Specific), bits 0-2=7
        assert_eq!(pic.slave.isr, 0x01);
    }

    #[test]
    fn test_initialization_sequence_vector_offset() {
        let mut pic = Pic::new();

        // ICW1
        pic.write_command(0, 0x11);
        // ICW2: vector offset = 0x40
        pic.write_data(0, 0x40);

        pic.request_interrupt(0);
        assert_eq!(pic.get_interrupt(), Some(0x40));
    }

    #[test]
    #[cfg(not(coverage))]
    fn test_acknowledge_irq_cascade_reset() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.slave.mask = 0; // Unmask slave IRQs so cascade line is asserted

        // Request slave IRQ 0 (IRQ 8) - sets master IRR bit 2 when slave is unmasked
        pic.request_interrupt(8);
        assert_eq!(pic.master.irr & 0x04, 0x04);

        // Acknowledge IRQ 8 (clears slave IRR bit 0)
        pic.acknowledge_irq(8);

        // Master IRR bit 2 should be cleared because no slave IRQs are pending
        assert_eq!(pic.master.irr & 0x04, 0);
    }

    #[test]
    fn test_get_interrupt_auto_eoi_logic() {
        let mut pic = Pic::new();
        pic.master.auto_eoi = true;
        pic.master.mask = 0;

        pic.request_interrupt(0);
        let int = pic.get_interrupt();
        assert_eq!(int, Some(0x08));

        // In Auto-EOI, ISR should be 0
        assert_eq!(pic.master.isr, 0);
    }

    #[test]
    fn test_get_interrupt_slave_auto_eoi() {
        let mut pic = Pic::new();
        pic.master.auto_eoi = true;
        pic.slave.auto_eoi = true;
        pic.master.mask = 0;
        pic.slave.mask = 0;

        pic.request_interrupt(9);
        let int = pic.get_interrupt();
        assert_eq!(int, Some(0x71));

        assert_eq!(pic.slave.isr, 0);
        assert_eq!(pic.master.isr & 0x04, 0);
    }

    #[test]
    fn test_write_command_icw1_logic() {
        let mut pic = Pic::new();
        pic.master.mask = 0xAA;

        // ICW1 (bit 4 must be 1)
        pic.write_command(0, 0x10); // 0001 0000
        assert_eq!(pic.master.init_state, 1);
        assert_eq!(pic.master.mask, 0);

        // Reset and try with other bits
        pic.master.mask = 0xAA;
        pic.write_command(0, 0x11); // 0001 0001
        assert_eq!(pic.master.init_state, 1);
        assert_eq!(pic.master.mask, 0);
    }

    // --- Priority rotation tests (HW-9) ---

    #[test]
    fn test_highest_priority_bit_default() {
        let pic = Pic8259::new(0x08);
        // Default priority_base=7, so scan order is 0,1,2,3,4,5,6,7
        assert_eq!(pic.highest_priority_bit(0xFF), Some(0));
        assert_eq!(pic.highest_priority_bit(0x06), Some(1)); // bits 1,2 -> IRQ1
        assert_eq!(pic.highest_priority_bit(0x80), Some(7));
        assert_eq!(pic.highest_priority_bit(0x00), None);
    }

    #[test]
    fn test_highest_priority_bit_rotated() {
        let mut pic = Pic8259::new(0x08);
        pic.priority_base = 3; // Lowest priority = IRQ3, highest = IRQ4
                               // Scan order: 4,5,6,7,0,1,2,3
        assert_eq!(pic.highest_priority_bit(0xFF), Some(4));
        assert_eq!(pic.highest_priority_bit(0x09), Some(0)); // bits 0,3 -> IRQ0 (higher than 3)
        assert_eq!(pic.highest_priority_bit(0x08), Some(3)); // Only bit 3 -> IRQ3
    }

    #[test]
    fn test_rotate_on_nonspecific_eoi() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.master.isr = 0x05; // IRQs 0 and 2 in service

        // Rotate on non-specific EOI (0xA0)
        pic.write_command(0, 0xA0);

        // Should EOI the highest-priority in-service (IRQ 0) and set priority_base = 0
        assert_eq!(pic.master.isr, 0x04); // IRQ 0 cleared
        assert_eq!(pic.master.priority_base, 0); // IRQ 0 is now lowest priority

        // Now highest priority is IRQ 1
        pic.master.irr = 0;
        pic.request_interrupt(0);
        pic.request_interrupt(1);
        // IRQ 1 should be higher priority than IRQ 0
        assert_eq!(pic.get_highest_priority_irq(), Some(1));
    }

    #[test]
    fn test_set_priority_command() {
        let mut pic = Pic::new();
        pic.master.mask = 0;

        // Set priority: IRQ 3 is lowest priority (0xC0 | 3 = 0xC3)
        pic.write_command(0, 0xC3);
        assert_eq!(pic.master.priority_base, 3);

        // Now highest priority is IRQ 4
        pic.request_interrupt(3);
        pic.request_interrupt(4);
        assert_eq!(pic.get_highest_priority_irq(), Some(4));
    }

    #[test]
    fn test_rotate_on_specific_eoi() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.master.isr = 0x06; // IRQs 1 and 2 in service

        // Rotate on specific EOI for IRQ 2 (0xE0 | 2 = 0xE2)
        pic.write_command(0, 0xE2);

        // Should clear ISR bit 2 and set priority_base = 2
        assert_eq!(pic.master.isr, 0x02); // IRQ 1 remains
        assert_eq!(pic.master.priority_base, 2); // IRQ 2 is now lowest priority
    }

    #[test]
    fn test_set_rotate_on_auto_eoi() {
        let mut pic = Pic::new();
        pic.master.auto_eoi = true;
        pic.master.mask = 0;

        // Enable rotate on auto-EOI (0x80)
        pic.write_command(0, 0x80);
        assert!(pic.master.rotate_auto_eoi);

        // Request and service IRQ 0 - should auto-rotate
        pic.request_interrupt(0);
        let int = pic.get_interrupt();
        assert_eq!(int, Some(0x08));
        assert_eq!(pic.master.isr, 0); // Auto-EOI clears ISR
        assert_eq!(pic.master.priority_base, 0); // Rotated: IRQ 0 now lowest

        // Request IRQ 0 and IRQ 1: IRQ 1 should now be higher priority
        pic.request_interrupt(0);
        pic.request_interrupt(1);
        assert_eq!(pic.get_highest_priority_irq(), Some(1));
    }

    #[test]
    fn test_clear_rotate_on_auto_eoi() {
        let mut pic = Pic::new();
        pic.master.rotate_auto_eoi = true;

        // Clear rotate on auto-EOI (0x00)
        pic.write_command(0, 0x00);
        assert!(!pic.master.rotate_auto_eoi);
    }

    #[test]
    fn test_icw1_resets_rotation_state() {
        let mut pic = Pic::new();
        pic.master.rotate_auto_eoi = true;
        pic.master.priority_base = 3;

        // ICW1 should reset rotation state
        pic.write_command(0, 0x11);
        assert!(!pic.master.rotate_auto_eoi);
        assert_eq!(pic.master.priority_base, 7);
    }

    #[test]
    fn test_priority_mask_rotated_blocks_correctly() {
        let mut pic = Pic::new();
        pic.master.mask = 0;

        // Set priority so IRQ 2 is lowest (highest = IRQ 3)
        pic.write_command(0, 0xC2);

        // IRQ 3 in service
        pic.master.isr = 1 << 3;

        // Request IRQ 4 (lower priority than 3) and IRQ 2 (also lower - it wraps around)
        pic.request_interrupt(4);
        pic.request_interrupt(2);

        // Neither should be serviced (both lower priority than IRQ 3)
        assert!(pic.get_highest_priority_irq().is_none());

        // But IRQ 3 itself re-requested should also be blocked (same priority = in-service)
        pic.request_interrupt(3);
        assert!(pic.get_highest_priority_irq().is_none());
    }

    #[test]
    fn test_non_specific_eoi_with_rotation_clears_rotated_highest() {
        let mut pic = Pic::new();
        pic.master.mask = 0;

        // Rotate priority: IRQ 4 is lowest, so IRQ 5 is highest
        pic.write_command(0, 0xC4);

        // IRQs 5 and 7 in service
        pic.master.isr = (1 << 5) | (1 << 7);

        // Non-specific EOI should clear IRQ 5 (highest priority in-service)
        pic.write_command(0, 0x20);
        assert_eq!(pic.master.isr, 1 << 7);
    }

    #[test]
    fn test_get_interrupt_vector_high_offset_no_overflow() {
        // Regression: vector_offset + irq_number must not overflow u8.
        // Real 8259A computes vector = (ICW2 & 0xF8) | irq_number.
        let mut pic = Pic::new();
        pic.master.mask = 0;

        // Set master vector offset to 0xFF (non-aligned, only valid via fuzzing)
        pic.master.vector_offset = 0xFF;
        pic.request_interrupt(3);
        // (0xFF & 0xF8) | 3 = 0xFB, not 0xFF + 3 = overflow
        assert_eq!(pic.get_interrupt(), Some(0xFB));
    }

    #[test]
    fn test_get_interrupt_slave_vector_high_offset_no_overflow() {
        // Same regression test for the slave PIC path.
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.slave.mask = 0;

        pic.slave.vector_offset = 0xFE;
        pic.request_interrupt(11); // Slave IRQ 3
                                   // (0xFE & 0xF8) | 3 = 0xFB
        assert_eq!(pic.get_interrupt(), Some(0xFB));
    }

    // --- has_pending_irq tests ---

    #[test]
    fn test_has_pending_irq_none_pending() {
        let pic = Pic::new();
        assert!(!pic.has_pending_irq());
    }

    #[test]
    fn test_has_pending_irq_raised_and_unmasked() {
        let mut pic = Pic::new();
        // IRQ 0 is unmasked by default (mask = 0xFC)
        pic.request_interrupt(0);
        assert!(pic.has_pending_irq());
    }

    #[test]
    fn test_has_pending_irq_raised_but_masked() {
        let mut pic = Pic::new();
        // IRQ 3 is masked by default (mask = 0xFC)
        pic.request_interrupt(3);
        assert!(!pic.has_pending_irq());
    }

    #[test]
    fn test_has_pending_irq_blocked_by_in_service() {
        let mut pic = Pic::new();
        pic.master.mask = 0xF0; // Unmask IRQ0-3

        // Put IRQ 1 in service
        pic.master.isr = 0x02;

        // Request IRQ 3 (lower priority than IRQ 1)
        pic.request_interrupt(3);
        assert!(!pic.has_pending_irq());

        // Request IRQ 0 (higher priority than IRQ 1) -- should be deliverable
        pic.request_interrupt(0);
        assert!(pic.has_pending_irq());
    }

    #[test]
    fn test_has_pending_irq_slave_via_cascade() {
        let mut pic = Pic::new();
        // Unmask cascade line (IRQ 2) on master and IRQ 8 on slave
        pic.master.mask = 0xF8; // Unmask IRQ 0-2
        pic.slave.mask = 0xFE; // Unmask IRQ 8

        pic.request_interrupt(8);
        assert!(pic.has_pending_irq());
    }

    #[test]
    fn test_has_pending_irq_slave_blocked_by_cascade_mask() {
        let mut pic = Pic::new();
        // Mask cascade line (IRQ 2) on master but unmask slave IRQ
        pic.master.mask = 0xFF; // All master IRQs masked (including cascade)
        pic.slave.mask = 0xFE; // Unmask IRQ 8

        pic.request_interrupt(8);
        // Slave IRQ pending, but master's cascade line (IRQ 2) is masked
        assert!(!pic.has_pending_irq());
    }

    // ── lower_irq tests ──────────────────────────────────────────────

    #[test]
    fn test_lower_irq_retains_master_irr_latch() {
        // Capture-at-assert (KILN-00004): lowering an EDGE line drops only the
        // electrical-line shadow (irr_line); the latched request (irr) is retained
        // until it is acknowledged. (Was test_lower_irq_clears_master_irr, which
        // asserted the old line-follows behaviour.)
        let mut pic = Pic::new();
        pic.raise_irq(0);
        assert_ne!(pic.master.irr & 1, 0, "IRR bit 0 should be set after raise");

        pic.lower_irq(0);
        assert_ne!(
            pic.master.irr & 1,
            0,
            "edge-line IRR latch is RETAINED across a deassert (capture-at-assert)"
        );
        assert_eq!(
            pic.master.irr_line & 1,
            0,
            "the electrical-line shadow drops on deassert"
        );
    }

    #[test]
    fn test_lower_irq_preserves_isr() {
        let mut pic = Pic::new();
        pic.raise_irq(0);
        // Simulate acknowledge: set ISR bit 0
        pic.master.isr |= 1;

        pic.lower_irq(0);
        // Capture-at-assert: the edge latch is retained; only the line shadow drops.
        assert_ne!(pic.master.irr & 1, 0, "edge IRR latch retained");
        assert_eq!(pic.master.irr_line & 1, 0, "line shadow dropped");
        assert_ne!(pic.master.isr & 1, 0, "ISR bit 0 must be preserved");
    }

    #[test]
    fn test_lower_irq_slave_updates_cascade() {
        let mut pic = Pic::new();
        pic.slave.mask = 0xFE; // Unmask slave IRQ 0 (= IRQ 8)

        pic.raise_irq(8);
        assert_ne!(pic.slave.irr & 1, 0, "Slave IRR bit 0 should be set");
        assert_ne!(
            pic.master.irr & (1 << 2),
            0,
            "Master cascade (IRQ2) should be set"
        );

        pic.lower_irq(8);
        // Capture-at-assert: the edge slave latch is retained, so the cascade
        // correctly STAYS asserted (the slave request is still pending); only the
        // slave line shadow drops.
        assert_eq!(
            pic.slave.irr_line & 1,
            0,
            "slave line shadow drops on deassert"
        );
        assert_ne!(
            pic.slave.irr & 1,
            0,
            "slave irr latch retained (capture-at-assert)"
        );
        assert_ne!(
            pic.master.irr & (1 << 2),
            0,
            "cascade stays set: the latched slave IRQ is still pending"
        );
    }

    #[test]
    fn test_lower_irq_slave_preserves_other_slave_irqs() {
        let mut pic = Pic::new();
        pic.slave.mask = 0xFC; // Unmask slave IRQ 0 and 1 (= IRQ 8, 9)

        pic.raise_irq(8);
        pic.raise_irq(9);
        assert_ne!(pic.slave.irr & 0x03, 0, "Both slave IRQ bits should be set");

        pic.lower_irq(8);
        // Capture-at-assert: lowering IRQ8 drops only its line shadow; its irr latch
        // and IRQ9's are both retained, and the cascade stays set.
        assert_eq!(pic.slave.irr_line & 1, 0, "IRQ8 line shadow drops");
        assert_ne!(
            pic.slave.irr & 1,
            0,
            "IRQ8 irr latch retained (capture-at-assert)"
        );
        assert_ne!(
            pic.slave.irr & 2,
            0,
            "Slave IRR bit 1 (IRQ 9) must remain set"
        );
        assert_ne!(
            pic.master.irr & (1 << 2),
            0,
            "Master cascade (IRQ2) remains set because IRQ 9 is still pending"
        );
    }

    #[test]
    fn test_lower_irq_edge_pulse_pattern() {
        let mut pic = Pic::new();
        pic.raise_irq(0);
        assert_ne!(pic.master.irr & 1, 0, "IRR bit 0 set after first raise");

        pic.lower_irq(0);
        // Capture-at-assert: lowering drops the line shadow (re-arming the edge
        // detector); the irr latch is retained throughout the pulse.
        assert_eq!(
            pic.master.irr_line & 1,
            0,
            "line shadow cleared after lower"
        );
        assert_ne!(
            pic.master.irr & 1,
            0,
            "irr latch retained (capture-at-assert)"
        );

        pic.raise_irq(0);
        assert_ne!(
            pic.master.irr & 1,
            0,
            "IRR bit 0 still latched after lower+raise edge pulse"
        );
        assert_ne!(
            pic.master.irr_line & 1,
            0,
            "line shadow re-asserted by the raise"
        );
    }

    // ── Edge-triggered interrupt detection tests (HW-C1) ─────────────

    #[test]
    fn test_edge_triggered_raise_already_high_does_not_retrigger() {
        // Raising an IRQ that is already high should NOT set IRR again.
        // This tests the core edge-detection behavior.
        let mut pic = Pic::new();
        pic.master.mask = 0; // Unmask all

        // First raise: should set IRR
        pic.raise_irq(0);
        assert_ne!(pic.master.irr & 1, 0, "IRR should be set on first raise");

        // Acknowledge the interrupt (clears IRR, sets ISR)
        pic.acknowledge_irq(0);
        assert_eq!(pic.master.irr & 1, 0, "IRR should be cleared after ack");

        // Raise again WITHOUT lowering first — line is still high
        // In edge-triggered mode, this should NOT set IRR
        pic.raise_irq(0);
        assert_eq!(
            pic.master.irr & 1,
            0,
            "IRR should NOT be set when raising an already-high line"
        );
    }

    #[test]
    fn test_edge_triggered_lower_then_raise_retriggers() {
        // Lowering then raising should produce a new edge and set IRR.
        let mut pic = Pic::new();
        pic.master.mask = 0;

        pic.raise_irq(0);
        pic.acknowledge_irq(0);

        // Lower the line
        pic.lower_irq(0);

        // Raise again — this is a new low-to-high transition
        pic.raise_irq(0);
        assert_ne!(
            pic.master.irr & 1,
            0,
            "IRR should be set after lower+raise edge"
        );
    }

    #[test]
    fn test_edge_triggered_lower_clears_line_state() {
        let mut pic = Pic::new();

        pic.raise_irq(3);
        assert_ne!(
            pic.master.irr_line & (1 << 3),
            0,
            "irr_line bit 3 should be set after raise"
        );

        pic.lower_irq(3);
        assert_eq!(
            pic.master.irr_line & (1 << 3),
            0,
            "irr_line bit 3 should be cleared after lower"
        );
    }

    #[test]
    fn test_edge_triggered_ack_does_not_lower_line() {
        // acknowledge_irq clears IRR but should NOT clear the line state.
        // The device must explicitly lower the line.
        let mut pic = Pic::new();
        pic.master.mask = 0;

        pic.raise_irq(1);
        assert_ne!(pic.master.irr_line & (1 << 1), 0);

        pic.acknowledge_irq(1);
        // IRR cleared, but line still high
        assert_eq!(pic.master.irr & (1 << 1), 0);
        assert_ne!(
            pic.master.irr_line & (1 << 1),
            0,
            "irr_line should still be high after ack"
        );

        // Raising again should NOT set IRR (line still high)
        pic.raise_irq(1);
        assert_eq!(
            pic.master.irr & (1 << 1),
            0,
            "IRR should NOT be set — no edge transition"
        );
    }

    #[test]
    fn test_edge_triggered_slave_raise_already_high() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.slave.mask = 0;

        // Raise slave IRQ 9
        pic.raise_irq(9);
        assert_ne!(pic.slave.irr & (1 << 1), 0);

        // Acknowledge it
        pic.acknowledge_irq(9);
        assert_eq!(pic.slave.irr & (1 << 1), 0);

        // Raise again without lowering — should NOT retrigger
        pic.raise_irq(9);
        assert_eq!(
            pic.slave.irr & (1 << 1),
            0,
            "Slave IRR should NOT be set when line is still high"
        );
    }

    #[test]
    fn test_edge_triggered_slave_lower_then_raise() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.slave.mask = 0;

        pic.raise_irq(9);
        pic.acknowledge_irq(9);

        pic.lower_irq(9);
        pic.raise_irq(9);
        assert_ne!(
            pic.slave.irr & (1 << 1),
            0,
            "Slave IRR should be set after lower+raise edge"
        );
    }

    #[test]
    fn test_edge_triggered_icw1_resets_line_state() {
        // ICW1 (re-initialization) should reset the irr_line shadow register.
        let mut pic = Pic::new();

        pic.raise_irq(0);
        assert_ne!(pic.master.irr_line, 0);

        // ICW1 resets the PIC
        pic.write_command(0, 0x11);
        assert_eq!(
            pic.master.irr_line, 0,
            "irr_line should be cleared on ICW1 reset"
        );
    }

    #[test]
    fn test_edge_triggered_save_restore_includes_irr_line() {
        let mut pic = Pic::new();
        pic.raise_irq(5);

        let saved = pic.clone();

        // Create a fresh PIC and restore
        let mut pic2 = Pic::new();
        pic2.restore_checkpoint(&saved);
        assert_eq!(pic2.master.irr_line, 1 << 5);
    }

    #[test]
    fn test_save_restore_preserves_level_triggered_mode() {
        // MDD-BUG-CRUCIBLE-00033: level-triggered (LTIM) mode must survive a
        // checkpoint round-trip. Program ICW1 with LTIM set, snapshot, restore
        // into a fresh (edge-default) PIC, and verify both the flag and the
        // relatch behaviour a level line depends on.
        let mut pic = Pic::new();
        // Master ICW1 (pic_num 0): bit4 marker | LTIM (0x08) | ICW4 (0x01) = 0x19,
        // then the ICW2/3/4 sequence.
        pic.write_command(0, 0x19); // ICW1: level-triggered
        pic.write_data(0, 0x08); // ICW2: vector offset
        pic.write_data(0, 0x04); // ICW3: cascade
        pic.write_data(0, 0x01); // ICW4
        assert!(
            !pic.master.edge_triggered,
            "precondition: LTIM should select level-triggered mode"
        );

        let saved = pic.clone();

        // Fresh PIC defaults to edge-triggered; restoring must bring back level.
        let mut restored = Pic::new();
        assert!(
            restored.master.edge_triggered,
            "fresh PIC is edge by default"
        );
        restored.restore_checkpoint(&saved);
        assert!(
            !restored.master.edge_triggered,
            "restore must recover level-triggered mode"
        );
    }

    /// MDD-BUG-CRU-01027: a checkpoint taken just after a single-mode ICW1
    /// (SNGL|IC4) resumes the sequence after restore: ICW2, then ICW4 (no ICW3
    /// in single mode), then OCW1.
    #[test]
    fn checkpoint_restore_resumes_an_in_progress_single_mode_icw_sequence() {
        let mut pic = Pic::new();
        pic.write_command(0, 0x13); // ICW1: single, ICW4 follows
        let checkpoint = pic.clone();

        let mut restored = Pic::new();
        restored.restore_checkpoint(&checkpoint);
        restored.write_data(0, 0x20); // ICW2
        restored.write_data(0, 0x01); // ICW4
        restored.write_data(0, 0xAB); // OCW1

        assert_eq!(
            restored.read_data(0),
            0xAB,
            "the mask write is OCW1, not ICW4"
        );
        assert_eq!(restored.master.vector_offset, 0x20);
    }

    /// MDD-BUG-CRU-01027: special-mask mode survives a checkpoint round trip.
    #[test]
    fn checkpoint_restore_keeps_special_mask_mode() {
        let mut pic = Pic::new();
        pic.write_command(0, 0x68); // OCW3: ESMM=1, SMM=1
        assert!(pic.master.special_mask, "precondition");
        let checkpoint = pic.clone();

        let mut restored = Pic::new();
        restored.restore_checkpoint(&checkpoint);

        assert!(restored.master.special_mask);
    }

    /// MDD-BUG-CRU-01027: two devices sharing IRQ 5 keep their separate
    /// assertions across a restore, so releasing one leaves the line high.
    #[test]
    fn checkpoint_restore_keeps_each_source_on_a_shared_line() {
        let mut pic = Pic::new();
        pic.set_irq_source(5, PicIrqSource::SoundBlaster, true);
        pic.set_irq_source(5, PicIrqSource::Ne2000, true);
        let checkpoint = pic.clone();

        let mut restored = Pic::new();
        restored.restore_checkpoint(&checkpoint);
        restored.set_irq_source(5, PicIrqSource::SoundBlaster, false);

        assert_ne!(
            restored.master.irr_line & (1 << 5),
            0,
            "the NE2000 still asserts IRQ 5"
        );
    }

    #[test]
    fn test_edge_triggered_cascade_line_exempt() {
        // The cascade line (master IRQ 2) is managed internally by
        // update_cascade_line(). It directly manipulates master.irr bit 2
        // and should NOT go through edge detection.
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.slave.mask = 0;

        // Raise a slave IRQ — this sets master IRR bit 2 via cascade
        pic.raise_irq(8);
        assert_ne!(pic.master.irr & (1 << 2), 0, "Cascade should be set");

        // Acknowledge the slave IRQ — clears slave IRR, cascade cleared
        let _vec = pic.get_interrupt();

        // Slave EOI makes a subsequent request eligible. The cascade "line"
        // was never explicitly lowered via lower_irq.
        pic.write_command(1, 0x20);
        pic.raise_irq(9);
        assert_ne!(
            pic.master.irr & (1 << 2),
            0,
            "Cascade should reassert for new slave IRQ"
        );
    }

    #[test]
    fn test_edge_triggered_level_triggered_mode_via_icw1() {
        // ICW1 bit 3 selects level-triggered mode. When set, raising
        // an already-high line SHOULD re-set IRR (no edge detection).
        let mut pic = Pic::new();
        pic.master.mask = 0;

        // Initialize master in level-triggered mode (ICW1 bit 3 = 1)
        pic.write_command(0, 0x19); // 0x11 | 0x08
        pic.write_data(0, 0x08); // ICW2
        pic.write_data(0, 0x04); // ICW3
        pic.write_data(0, 0x01); // ICW4
        pic.master.mask = 0;

        pic.raise_irq(0);
        pic.acknowledge_irq(0);

        // Raise again without lowering — in level-triggered mode,
        // this should set IRR because edge detection is disabled
        pic.raise_irq(0);
        assert_ne!(
            pic.master.irr & 1,
            0,
            "In level-triggered mode, raise should always set IRR"
        );
    }

    // ==================== get_highest_priority_irq ====================

    #[test]
    fn test_get_highest_priority_irq_none() {
        let pic = Pic::new();
        assert_eq!(pic.get_highest_priority_irq(), None);
    }

    #[test]
    fn test_get_highest_priority_irq_master_cov() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.request_interrupt(3);
        assert_eq!(pic.get_highest_priority_irq(), Some(3));
    }

    #[test]
    fn test_get_highest_priority_irq_slave() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.slave.mask = 0;
        pic.request_interrupt(10); // Slave IRQ 2
        let prio = pic.get_highest_priority_irq();
        assert_eq!(prio, Some(10));
    }

    #[test]
    fn test_get_highest_priority_irq_priority_order() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.request_interrupt(5);
        pic.request_interrupt(3);
        // IRQ 3 should have higher priority than IRQ 5
        assert_eq!(pic.get_highest_priority_irq(), Some(3));
    }

    // ==================== is_irq_masked / is_irq_in_service ====================

    #[test]
    fn test_is_irq_masked_cov() {
        let mut pic = Pic::new();
        pic.master.mask = 0xFF; // Mask all
        assert!(pic.is_irq_masked(0));
        assert!(pic.is_irq_masked(7));

        pic.master.mask = 0xFE; // Unmask IRQ 0
        assert!(!pic.is_irq_masked(0));
        assert!(pic.is_irq_masked(1));
    }

    #[test]
    fn test_is_irq_masked_slave() {
        let mut pic = Pic::new();
        pic.slave.mask = 0xFF;
        assert!(pic.is_irq_masked(8));
        assert!(pic.is_irq_masked(15));

        pic.slave.mask = 0x00;
        assert!(!pic.is_irq_masked(8));
    }

    #[test]
    fn test_is_irq_masked_out_of_range_cov() {
        let pic = Pic::new();
        assert!(pic.is_irq_masked(16));
    }

    #[test]
    fn test_is_irq_in_service() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.request_interrupt(0);
        pic.acknowledge_irq(0);
        assert!(pic.is_irq_in_service(0));
        assert!(!pic.is_irq_in_service(1));
    }

    #[test]
    fn test_is_irq_in_service_slave_cov() {
        let mut pic = Pic::new();
        pic.slave.isr = 0x01; // IRQ 8 in service
        assert!(pic.is_irq_in_service(8));
        assert!(!pic.is_irq_in_service(9));
    }

    #[test]
    fn test_is_irq_in_service_out_of_range_cov() {
        let pic = Pic::new();
        assert!(!pic.is_irq_in_service(16));
    }

    // ==================== get_interrupt ====================

    #[test]
    fn test_get_interrupt_master() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.request_interrupt(0);
        let vec = pic.get_interrupt();
        assert_eq!(vec, Some(0x08)); // Master vector offset + IRQ 0
    }

    #[test]
    fn test_get_interrupt_slave_cov() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.slave.mask = 0;
        pic.request_interrupt(8);
        let vec = pic.get_interrupt();
        assert_eq!(vec, Some(0x70)); // Slave vector offset + slave IRQ 0
    }

    #[test]
    fn test_get_interrupt_none() {
        let mut pic = Pic::new();
        assert_eq!(pic.get_interrupt(), None);
    }

    // ==================== end_of_interrupt ====================

    #[test]
    fn test_eoi_master() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.request_interrupt(0);
        pic.acknowledge_irq(0);
        assert!(pic.is_irq_in_service(0));

        pic.end_of_interrupt(0);
        assert!(!pic.is_irq_in_service(0));
    }

    #[test]
    fn test_eoi_slave() {
        let mut pic = Pic::new();
        pic.slave.isr = 0x01; // IRQ 8 in service
        pic.end_of_interrupt(8);
        assert_eq!(pic.slave.isr & 0x01, 0);
    }

    #[test]
    fn test_eoi_out_of_range() {
        let mut pic = Pic::new();
        pic.end_of_interrupt(16); // Should not panic
    }

    // ==================== write_data (OCW1 = mask register) ====================

    #[test]
    fn test_write_data_mask_master() {
        let mut pic = Pic::new();
        pic.write_data(0, 0xFF); // Mask all master IRQs
        assert_eq!(pic.get_mask(), 0xFF);
    }

    #[test]
    fn test_write_data_mask_slave() {
        let mut pic = Pic::new();
        pic.write_data(1, 0xFE);
        assert_eq!(pic.slave.mask, 0xFE);
    }

    // ==================== read_data (mask register) ====================

    #[test]
    fn test_read_data_mask() {
        let mut pic = Pic::new();
        pic.master.mask = 0xAB;
        assert_eq!(pic.read_data(0), 0xAB);
    }

    #[test]
    fn test_read_data_slave() {
        let mut pic = Pic::new();
        pic.slave.mask = 0xCD;
        assert_eq!(pic.read_data(1), 0xCD);
    }

    // ==================== OCW2: specific EOI and rotate ====================

    #[test]
    fn test_specific_eoi_via_ocw2() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.request_interrupt(3);
        pic.acknowledge_irq(3);
        assert!(pic.is_irq_in_service(3));

        // Specific EOI for IRQ 3: 0x60 | 3 = 0x63
        pic.write_command(0, 0x63);
        assert!(!pic.is_irq_in_service(3));
    }

    #[test]
    fn test_nonspecific_eoi_via_ocw2() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.request_interrupt(0);
        pic.acknowledge_irq(0);
        assert!(pic.is_irq_in_service(0));

        // Non-specific EOI: 0x20
        pic.write_command(0, 0x20);
        assert!(!pic.is_irq_in_service(0));
    }

    // ==================== master/slave accessors ====================

    #[test]
    fn test_master_slave_accessors() {
        let mut pic = Pic::new();
        assert_eq!(pic.master().vector_offset, 0x08);
        assert_eq!(pic.slave().vector_offset, 0x70);
        pic.master_mut().mask = 0xFF;
        assert_eq!(pic.master().mask, 0xFF);
        pic.slave_mut().mask = 0xAA;
        assert_eq!(pic.slave().mask, 0xAA);
    }

    // ==================== lower_irq ====================

    #[test]
    fn test_lower_irq_master() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.raise_irq(3);
        assert_ne!(pic.master.irr & 0x08, 0);

        pic.lower_irq(3);
    }

    #[test]
    fn test_lower_irq_slave() {
        let mut pic = Pic::new();
        pic.slave.mask = 0;
        pic.raise_irq(10);

        pic.lower_irq(10);
    }

    #[test]
    fn test_lower_irq_out_of_range() {
        let mut pic = Pic::new();
        pic.lower_irq(16); // Should not panic
    }

    #[test]
    fn named_irq_source_cannot_lower_a_shared_legacy_level() {
        let mut pic = Pic::new();
        pic.raise_irq(5);
        pic.set_irq_source(5, PicIrqSource::S3Trio64, true);
        pic.set_irq_source(5, PicIrqSource::S3Trio64, false);

        assert_ne!(pic.diagnostic_snapshot().master.irq_line & (1 << 5), 0);
        pic.lower_irq(5);
        assert_eq!(pic.diagnostic_snapshot().master.irq_line & (1 << 5), 0);
    }

    #[test]
    fn legacy_source_cannot_lower_a_shared_named_level() {
        let mut pic = Pic::new();
        pic.set_irq_source(7, PicIrqSource::S3Trio64, true);
        pic.raise_irq(7);
        pic.lower_irq(7);

        assert_ne!(pic.diagnostic_snapshot().master.irq_line & (1 << 7), 0);
        pic.set_irq_source(7, PicIrqSource::S3Trio64, false);
        assert_eq!(pic.diagnostic_snapshot().master.irq_line & (1 << 7), 0);
    }

    #[test]
    fn pic_initialization_preserves_named_device_levels() {
        let mut pic = Pic::new();
        pic.set_irq_source(5, PicIrqSource::S3Trio64, true);
        pic.write_command(0, 0x11);

        assert_ne!(pic.diagnostic_snapshot().master.irq_line & (1 << 5), 0);
        pic.set_irq_source(5, PicIrqSource::S3Trio64, false);
        assert_eq!(pic.diagnostic_snapshot().master.irq_line & (1 << 5), 0);
    }

    #[test]
    fn independent_named_sources_keep_a_shared_irq_asserted() {
        let mut pic = Pic::new();
        pic.write_elcr(0x4D0, 1 << 5);
        pic.set_irq_source(5, PicIrqSource::Opl, true);
        pic.set_irq_source(5, PicIrqSource::SoundBlaster, true);

        pic.set_irq_source(5, PicIrqSource::SoundBlaster, false);

        let snapshot = pic.diagnostic_snapshot();
        assert_ne!(snapshot.master.irq_line & (1 << 5), 0);
        assert_ne!(snapshot.master.irr & (1 << 5), 0);

        pic.set_irq_source(5, PicIrqSource::Opl, false);
        let snapshot = pic.diagnostic_snapshot();
        assert_eq!(snapshot.master.irq_line & (1 << 5), 0);
        assert_eq!(snapshot.master.irr & (1 << 5), 0);
    }

    // ==================== has_pending_irq ====================

    #[test]
    fn test_has_pending_irq() {
        let mut pic = Pic::new();
        assert!(!pic.has_pending_irq());

        pic.master.mask = 0;
        pic.request_interrupt(0);
        assert!(pic.has_pending_irq());
    }

    // ==================== master_vector_offset ====================

    #[test]
    fn test_master_vector_offset() {
        let pic = Pic::new();
        assert_eq!(pic.master_vector_offset(), 0x08);
    }

    // ==================== Spurious IRQ 7/15 (8259A hardware behavior) ====================

    #[test]
    fn test_edge_line_deasserted_before_ack_delivers_real_vector() {
        // KILN-00004 capture-at-assert (QEMU-aligned): an edge line raised then
        // deasserted before INTA RETAINS its latched request and delivers the REAL
        // vector — it does NOT produce a spurious IRQ 7. Spurious-IRQ7-on-edge-deassert
        // is intentionally not modelled (see get_interrupt_or_spurious's doc + the
        // KILN-00004 ledger). Was test_spurious_irq7_master_deasserted_before_acknowledge,
        // which asserted the old line-follows behaviour.
        let mut pic = Pic::new();
        pic.master.mask = 0; // Unmask all

        pic.raise_irq(3);
        assert!(pic.has_pending_irq(), "IRQ 3 should be pending");

        pic.lower_irq(3); // deassert before acknowledge

        // The captured edge survives -> get_interrupt delivers the real IRQ3 vector
        // (master offset 0x08 + 3 = 0x0B), not a spurious IRQ7.
        assert_eq!(
            pic.get_interrupt(),
            Some(0x0B),
            "retained edge delivers the real IRQ3 vector, not spurious IRQ7"
        );
        assert_ne!(
            pic.master.isr & (1 << 3),
            0,
            "ISR bit 3 is set for the real delivery"
        );
    }

    #[test]
    fn test_spurious_cascade_silent_drop() {
        // Spurious cascade: slave IRQ raised then deasserted before INTA.
        // The PC wiring silently drops this case,
        // because dispatching the spurious vector to an uninstalled IVT
        // entry would leak master ISR bit 2 forever and block IRQ 3-7.
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.slave.mask = 0;

        // Construct the spurious-cascade state directly: the master latched the
        // cascade request (IR2) but the slave IRR is empty (the slave line deasserted
        // before INTA). Under capture-at-assert (KILN-00004) lower_irq no longer
        // empties an edge slave latch, so we build the empty-slave state directly,
        // exactly as test_spurious_cascade_does_not_block_higher_priority_master_irq.
        pic.slave.irr = 0;
        pic.slave.irr_line = 0;
        pic.master.irr |= 1 << 2;

        // get_interrupt() finds cascade pending, enters slave path,
        // but slave IRR is 0 — should silently drop and return None.
        let vec = pic.get_interrupt();
        assert_eq!(
            vec, None,
            "Spurious cascade must return None (silent drop), not dispatch a vector"
        );

        // Cascade IRR bit 2 must be cleared so we don't loop on the same
        // spurious cascade on the next dispatcher pass.
        assert_eq!(
            pic.master.irr & (1 << 2),
            0,
            "Master IRR bit 2 must be cleared after spurious-cascade silent drop"
        );

        // Master ISR bit 2 must NOT be set — that was the old broken
        // behaviour that wedged all IRQ 3-7 until the (never sent) EOI.
        assert_eq!(
            pic.master.isr & (1 << 2),
            0,
            "Master ISR bit 2 must NOT be committed for a spurious cascade"
        );

        // Slave ISR bit 7 must NOT be set (no spurious vector was dispatched).
        assert_eq!(
            pic.slave.isr & (1 << 7),
            0,
            "Slave ISR bit 7 must NOT be set after silent drop"
        );
    }

    #[test]
    fn test_spurious_cascade_does_not_block_higher_priority_master_irq() {
        // With a device IRQ pending alongside a spurious cascade, the dispatcher must
        // be able to deliver IRQ 5 after the spurious cascade is dropped.
        // A spurious cascade must not commit
        // master ISR bit 2 and block IRQ 3-7 forever.
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.slave.mask = 0;

        // Stage a spurious cascade: master IRR bit 2 set, slave IRR empty.
        pic.master.irr |= 1 << 2;
        // Also raise a real SB IRQ 5 on the master.
        pic.raise_irq(5);
        assert_ne!(
            pic.master.irr & (1 << 5),
            0,
            "Master IRR bit 5 (SB) should be asserted"
        );

        // First dispatcher call: spurious cascade is silently dropped.
        let first = pic.get_interrupt();
        assert_eq!(first, None, "Spurious cascade should silently drop");

        // Second call: SB IRQ 5 should now dispatch normally.
        let second = pic.get_interrupt();
        assert_eq!(
            second,
            Some((pic.master.vector_offset & 0xF8) | 5),
            "SB IRQ 5 must be dispatchable after the spurious cascade drop"
        );
        assert_ne!(
            pic.master.isr & (1 << 5),
            0,
            "Master ISR bit 5 should be committed after dispatch"
        );
    }

    #[test]
    fn test_spurious_irq7_isr_not_set() {
        // Verify that a spurious IRQ 7 does not leave ISR bit 7 set,
        // even when called through get_interrupt_or_spurious().
        let mut pic = Pic::new();
        pic.master.mask = 0;

        // No IRQ raised at all — direct spurious call
        let vec = pic.get_interrupt_or_spurious();
        assert_eq!(vec, Some(0x0F), "Should be spurious IRQ 7");
        assert_eq!(
            pic.master.isr, 0,
            "ISR should be completely clear after spurious"
        );
    }

    #[test]
    fn test_get_interrupt_or_spurious_normal_path() {
        // When a real IRQ is pending, get_interrupt_or_spurious() should
        // behave identically to get_interrupt().
        let mut pic = Pic::new();
        pic.master.mask = 0;

        pic.raise_irq(1);
        let vec = pic.get_interrupt_or_spurious();
        assert_eq!(vec, Some(0x09), "Should deliver normal IRQ 1 vector");
        assert_ne!(
            pic.master.isr & (1 << 1),
            0,
            "ISR bit 1 should be set for real IRQ"
        );
    }

    // Synthetic guest-port sequences: a dispatch boundary may occur between
    // the two EOIs needed for a cascaded interrupt.
    fn slave_request_after_master_first_eoi(slave_eoi: u8, held_level: bool) {
        let mut pic = Pic::new();
        pic.write_data(0, 0xfb); // Unmask cascade only.
        pic.write_data(1, 0xee); // Unmask IRQ8 and IRQ12.
        if held_level {
            pic.write_elcr(0x4d1, 0x10); // IRQ12 remains asserted until lowered.
        }
        pic.raise_irq(8);
        assert_eq!(pic.get_interrupt(), Some(0x70));
        pic.raise_irq(12);
        pic.write_command(0, 0x20);
        assert_eq!(pic.get_interrupt(), None, "IRQ8 service must block IRQ12");
        pic.write_command(1, slave_eoi);
        assert_eq!(
            pic.get_interrupt(),
            Some(0x74),
            "slave EOI must expose IRQ12 after the interleaved dispatch boundary"
        );
        assert!(pic.is_irq_in_service(12));
        pic.write_command(0, 0x20);
        pic.write_command(1, 0x64);
        assert_eq!(
            pic.get_interrupt(),
            held_level.then_some(0x74),
            "edge requests are consumed once; held level requests recur after EOI"
        );
    }

    #[test]
    fn slave_cascade_master_first_nonspecific_eoi() {
        slave_request_after_master_first_eoi(0x20, false);
    }

    #[test]
    fn slave_cascade_master_first_specific_eoi() {
        slave_request_after_master_first_eoi(0x60, false);
    }

    #[test]
    fn slave_cascade_master_first_held_level_eoi() {
        slave_request_after_master_first_eoi(0x20, true);
    }

    #[test]
    fn slave_cascade_master_first_rotating_eois() {
        for command in [0xa0, 0xe0] {
            slave_request_after_master_first_eoi(command, false);
        }
    }

    #[test]
    fn slave_cascade_tracks_priority_commands() {
        let mut pic = Pic::new();
        pic.write_data(0, 0xfb);
        pic.write_data(1, 0xee);
        pic.raise_irq(8);
        assert_eq!(pic.get_interrupt(), Some(0x70));
        pic.raise_irq(12);
        pic.write_command(0, 0x20);
        assert_eq!(pic.get_interrupt(), None);
        pic.write_command(1, 0xc0); // IRQ8 becomes lowest priority.
        assert_eq!(
            pic.get_interrupt(),
            Some(0x74),
            "slave priority rotation must expose the pending IRQ12 cascade"
        );
        pic.write_command(0, 0x20);
        pic.lower_irq(8);
        pic.raise_irq(8);
        pic.write_command(1, 0xc7); // IRQ8 is highest, but still in service.
        assert_eq!(
            pic.read_command(0) & 4,
            0,
            "a priority-blocked slave must lower cascade INT without clearing its IRR"
        );
        assert_ne!(pic.read_command(1) & 1, 0);
    }

    #[test]
    fn slave_cascade_tracks_special_mask_commands() {
        let mut pic = Pic::new();
        pic.write_data(0, 0xfb);
        pic.write_data(1, 0xee);
        pic.raise_irq(8);
        assert_eq!(pic.get_interrupt(), Some(0x70));
        pic.raise_irq(12);
        pic.write_command(0, 0x20);
        assert_eq!(pic.get_interrupt(), None);
        pic.write_data(1, 0xef); // Mask the in-service IRQ8.
        assert_eq!(pic.get_interrupt(), None);
        pic.write_command(1, 0x68); // Special mask mode removes its blocker.
        assert_ne!(
            pic.read_command(0) & 4,
            0,
            "entering special mask mode must expose the pending slave cascade"
        );
        pic.write_command(1, 0x48); // Normal mode restores the blocker.
        assert_eq!(
            pic.read_command(0) & 4,
            0,
            "leaving special mask mode must lower a blocked slave cascade"
        );
        pic.write_command(1, 0x68);
        assert_eq!(pic.get_interrupt(), Some(0x74));
    }

    #[test]
    fn slave_cascade_reinitialization_clears_old_request() {
        let mut pic = Pic::new();
        pic.write_data(0, 0xfb);
        pic.write_data(1, 0xef);
        pic.raise_irq(12);
        assert_ne!(pic.read_command(0) & 4, 0);
        pic.write_command(1, 0x11);
        assert_eq!(
            pic.read_command(0) & 4,
            0,
            "slave initialization must lower cascade after clearing its pending request"
        );
    }

    // ==================== IRQ14 cascade audit (Phase 4a Stage 4a-2) ====================

    // Full cascade path for IRQ14 (primary ATA IRQ): slave IRR bit 6 → master
    // IRR bit 2 → INTA delivers vector 0x76 → slave ISR bit 6 + master ISR bit
    // 2 set → dual EOI clears both.
    #[test]
    fn test_irq14_cascade_end_to_end_through_slave() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.slave.mask = 0;

        // Raise IRQ14 — slave IRR bit 6 should latch, master cascade bit 2
        // should assert.
        pic.raise_irq(14);
        assert_ne!(
            pic.slave.irr & (1 << 6),
            0,
            "slave IRR bit 6 should be set after raise_irq(14)"
        );
        assert_ne!(
            pic.master.irr & (1 << 2),
            0,
            "master cascade (IRR bit 2) should assert when slave has a pending unmasked IRQ"
        );

        // INTA cycle: get_interrupt() returns the delivered vector, sets ISR
        // bits on both chips, and clears IRR bits.
        let vec = pic.get_interrupt();
        assert_eq!(
            vec,
            Some(0x76),
            "IRQ14 vector must be 0x70 | 6 = 0x76 (slave vector base plus input index)"
        );
        assert_ne!(
            pic.slave.isr & (1 << 6),
            0,
            "slave ISR bit 6 should be set after INTA"
        );
        assert_ne!(
            pic.master.isr & (1 << 2),
            0,
            "master ISR bit 2 should be set after INTA (cascade committed)"
        );

        // Dual EOI: slave first (port 0xA0 = pic_num=1), then master (port
        // 0x20 = pic_num=0). Non-specific EOI (0x20) clears the
        // highest-priority ISR bit.
        pic.write_command(1, 0x20); // Slave EOI → port 0xA0
        pic.write_command(0, 0x20); // Master EOI → port 0x20

        assert_eq!(
            pic.slave.isr & (1 << 6),
            0,
            "slave ISR bit 6 must be cleared after slave EOI"
        );
        assert_eq!(
            pic.master.isr & (1 << 2),
            0,
            "master ISR bit 2 must be cleared after master EOI"
        );
    }

    // Defensive test: real 8259s accept dual EOIs in any order because each
    // chip only sees its own ISR. Document the implemented semantic so future
    // refactors don't accidentally introduce an ordering requirement.
    #[test]
    fn test_irq14_reverse_eoi_order_leaves_master_stuck() {
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.slave.mask = 0;

        pic.raise_irq(14);
        let vec = pic.get_interrupt();
        assert_eq!(vec, Some(0x76));
        assert_ne!(pic.slave.isr & (1 << 6), 0);
        assert_ne!(pic.master.isr & (1 << 2), 0);

        // Reverse order: master EOI first, then slave EOI. On real hardware
        // (and in our model) each chip's EOI only touches its own ISR, so
        // this is a benign ordering. The test name is a historical devil's-
        // advocate note — the master is NOT actually stuck in our model.
        pic.write_command(0, 0x20); // Master EOI first → port 0x20
        pic.write_command(1, 0x20); // Slave EOI second → port 0xA0

        assert_eq!(
            pic.master.isr & (1 << 2),
            0,
            "master ISR bit 2 must be cleared after master EOI (regardless of ordering)"
        );
        assert_eq!(
            pic.slave.isr & (1 << 6),
            0,
            "slave ISR bit 6 must be cleared after slave EOI (regardless of ordering)"
        );
    }

    // Guard the spurious-IRQ edge case for IRQ14: if IRQ14 is raised then
    // deasserted before INTA, no real IRQ should be pending. (Mirrors
    // test_spurious_irq15_slave_deasserted_before_acknowledge but for IRQ14,
    // and doesn't manually re-assert the cascade — a clean raise-then-lower
    // leaves the PIC with no pending work.)
    #[test]
    fn test_irq14_delivered_after_early_deassert() {
        // KILN-00004 capture-at-assert: IRQ14 (edge, slave — the IDE completion line)
        // raised then deasserted before INTA is a REAL latched completion; it must
        // survive to delivery, not be silently dropped (the old line-follows loss —
        // the KILN-00004 bug class). Was test_irq14_spurious_on_deassertion.
        let mut pic = Pic::new();
        pic.master.mask = 0;
        pic.slave.mask = 0;

        pic.raise_irq(14);
        assert!(pic.has_pending_irq(), "IRQ14 should be pending after raise");

        pic.lower_irq(14); // INTRQ deasserts before INTA
        assert!(
            pic.has_pending_irq(),
            "capture-at-assert: the latched IRQ14 survives the deassert"
        );
        assert_ne!(
            pic.slave.irr & (1 << 6),
            0,
            "slave IRR bit 6 (IRQ14) is retained"
        );
        assert_ne!(
            pic.master.irr & (1 << 2),
            0,
            "cascade stays asserted for the pending slave IRQ"
        );
        // And it delivers the real IRQ14 vector (slave offset 0x70 + 6 = 0x76).
        assert_eq!(
            pic.get_interrupt(),
            Some(0x76),
            "IRQ14 delivers its real vector, not spurious"
        );
    }

    // ── C3 truncation seam: the deliverability-dirty flag (A-C3-1) ─────────

    #[test]
    fn deliverability_dirty_starts_clear_and_take_consumes() {
        let mut pic = Pic::new();
        let _ = pic.take_deliverability_dirty(); // drain construction-time marks
        assert!(!pic.deliverability_dirty());
        pic.raise_irq(3);
        assert!(pic.deliverability_dirty(), "raise marks the seam");
        assert!(pic.take_deliverability_dirty(), "take reads the mark");
        assert!(!pic.deliverability_dirty(), "take clears the mark");
        assert!(!pic.take_deliverability_dirty());
    }

    #[test]
    fn deliverability_dirty_set_by_every_transition_class() {
        // Each row: (label, mutation). After draining, the mutation must mark.
        let mut pic = Pic::new();
        let _ = pic.take_deliverability_dirty();

        pic.raise_irq(5);
        assert!(pic.take_deliverability_dirty(), "raise_irq");

        pic.lower_irq(5);
        assert!(pic.take_deliverability_dirty(), "lower_irq");

        pic.set_irq_source(4, PicIrqSource::S3Trio64, true);
        assert!(pic.take_deliverability_dirty(), "set_irq_source assert");

        pic.set_irq_source(4, PicIrqSource::S3Trio64, false);
        assert!(pic.take_deliverability_dirty(), "set_irq_source deassert");

        pic.write_data(0, 0xDF); // IMR change
        assert!(pic.take_deliverability_dirty(), "IMR mask change");

        pic.write_command(0, 0x20); // OCW2 non-specific EOI
        assert!(pic.take_deliverability_dirty(), "OCW2 EOI command");

        pic.write_command(0, 0x60 | 5); // OCW2 specific EOI
        assert!(pic.take_deliverability_dirty(), "OCW2 specific EOI");

        pic.end_of_interrupt(5);
        assert!(pic.take_deliverability_dirty(), "direct end_of_interrupt");

        pic.write_command(0, 0x11); // ICW1 re-init
        assert!(pic.take_deliverability_dirty(), "ICW1 re-initialisation");
    }

    #[test]
    fn deliverability_dirty_not_set_by_ocw3_or_unchanged_mask() {
        let mut pic = Pic::new();
        let _ = pic.take_deliverability_dirty();

        // OCW3 (register read select) is a poll, not a transition — marking it
        // would truncate ISR/IRR polling loops.
        pic.write_command(0, 0x0B); // OCW3: read ISR
        assert!(!pic.deliverability_dirty(), "OCW3 must not mark");
        pic.write_command(0, 0x0A); // OCW3: read IRR
        assert!(!pic.deliverability_dirty(), "OCW3 must not mark");

        // Rewriting the identical IMR value changes nothing.
        let current_mask = pic.master().mask;
        pic.write_data(0, current_mask);
        assert!(!pic.deliverability_dirty(), "unchanged IMR must not mark");
    }

    #[test]
    fn ocw3_special_mask_commands_mark_deliverability() {
        let mut pic = Pic::new();
        pic.take_deliverability_dirty();
        for chip in 0..2 {
            for command in [0x68, 0x48] {
                pic.write_command(chip, command);
                assert!(
                    pic.take_deliverability_dirty(),
                    "PIC {chip}, command {command:#x}"
                );
            }
        }
    }
}

#[cfg(test)]
mod wiring_tests {
    use super::*;

    #[test]
    fn unconfigured_pair_has_no_bios_defaults() {
        let pic = Pic::<Unconfigured>::new();
        let snapshot = pic.diagnostic_snapshot();
        assert_eq!(snapshot.master.imr, 0xff);
        assert_eq!(snapshot.slave.imr, 0xff);
        assert_eq!(snapshot.master.vector_offset, 0);
        assert_eq!(snapshot.slave.vector_offset, 0);
        assert!(!pic.has_pending_irq());
    }

    #[derive(Clone, Debug)]
    struct BoardWiring;
    impl PicWiring for BoardWiring {
        const VECTOR_OFFSETS: [u8; 2] = [0x30, 0x40];
        const INITIAL_MASKS: [u8; 2] = [0, 0];
        const ELCR_WRITABLE_MASKS: [u8; 2] = [0xff, 0xff];
        const COMMAND_PORTS: [u16; 2] = [0x100, 0x102];
        const DATA_PORTS: [u16; 2] = [0x101, 0x103];
        const ELCR_PORTS: [u16; 2] = [0x104, 0x105];
    }

    #[test]
    fn board_wiring_controls_vectors_ports_and_elcr() {
        let mut pic = Pic::<BoardWiring>::new();
        let before = pic.diagnostic_snapshot();
        assert_eq!(
            Pic::<BoardWiring>::decode_diagnostic_port_access(0x100, 0x11, true, &before),
            "icw1"
        );
        assert_eq!(
            Pic::<BoardWiring>::decode_diagnostic_port_access(0x20, 0x11, true, &before),
            "unknown"
        );
        pic.write_elcr(0x104, 0xff);
        pic.write_elcr(0x105, 0xff);
        assert_eq!(pic.read_elcr(0x104), 0xff);
        assert_eq!(pic.read_elcr(0x105), 0xff);
        pic.raise_irq(0);
        assert_eq!(pic.get_interrupt(), Some(0x30));
        pic.end_of_interrupt(0);
        pic.lower_irq(0);
        pic.raise_irq(15);
        assert_eq!(pic.get_interrupt(), Some(0x47));
    }

    #[test]
    fn slave_spurious_vector_is_board_policy() {
        let mut pic = Pic::<BoardWiring>::new();
        pic.master.irr |= 1 << 2;
        assert_eq!(pic.get_interrupt(), Some(0x47));
        assert!(pic.is_irq_in_service(2));
        assert!(!pic.is_irq_in_service(15));
        pic.end_of_interrupt(2);
        assert!(!pic.has_pending_irq());
    }

    #[test]
    fn opaque_sources_include_upper_bit_and_preserve_wired_or() {
        let mut pic = Pic::<BoardWiring>::new();
        pic.write_elcr(0x104, 1 << 5);
        let low = IrqSource::new(1);
        let high = IrqSource::new(15);
        pic.set_irq_source(5, low, true);
        pic.set_irq_source(5, high, true);
        pic.set_irq_source(5, low, false);
        assert!(pic.has_pending_irq());
        assert_eq!(pic.get_interrupt(), Some(0x35));
        pic.end_of_interrupt(5);
        pic.set_irq_source(5, high, false);
        assert!(!pic.has_pending_irq());
    }
}
