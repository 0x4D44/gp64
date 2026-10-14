//! Interrupt and exception delivery, generic over the memory and an observer.
//!
//! The architecture lives here: IDT and IVT dispatch, privilege and V86
//! stack switches, frame pushes, task gates, call gates and #DF/triple-fault
//! escalation. Machine-specific behaviour and tracing reach in through
//! [`DeliveryObserver`] hooks, which the caller monomorphises; a machine with
//! nothing to add passes [`NoObserver`] and every hook inlines to nothing.
//!
//! The helpers this code calls (`deliver_load_cs`, `deliver_try_push32`, ...)
//! are copies of the root crate's semantics, not the shared `cpu.rs` ones.
//! `helpers.rs` describes the descriptor-read contract.
mod gate;
mod helpers;
mod task;
use crate::{
    Cpu, CpuError, Flags, PageAccess, PagedMemory, Result, RingBufferEntry, RingInterruptOrigin,
    SegmentCache, TripleFaultInfo, TripleFaultOrigin,
};
use gp64_types::define_trace_checker;
define_trace_checker!(
    pub (crate) stack_fault_trace_enabled, "gp64::cpu::stack_fault",
    tracing::Level::DEBUG
);
define_trace_checker!(
    pub (crate) pm_int_if_debug_enabled, "gp64::cpu::pm_int_if", tracing::Level::DEBUG
);
define_trace_checker!(
    pub (crate) extender_trace_enabled, "gp64::dos::dpmi", tracing::Level::DEBUG
);
define_trace_checker!(
    pub (crate) selector_trace_enabled, "gp64::cpu::selector", tracing::Level::DEBUG
);
/// Source of a hardware task switch, determining busy-bit and nesting behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskSwitchSource {
    /// FAR JMP — no nesting (clear old busy, set new busy)
    Jmp,
    /// FAR CALL — nested (keep old busy, set new busy, write back-link, set NT)
    Call,
    /// INT/exception via task gate — nested (keep old busy, set new busy, write back-link, set NT)
    Int,
    /// IRET with NT=1 — un-nesting (clear old busy, keep new busy)
    Iret,
}
/// Memory capabilities delivery needs beyond [`PagedMemory`].
///
/// `P` is a tag chosen by the implementing machine (usually its observer type).
/// It exists only so a crate that owns neither this trait nor its memory type
/// can still implement it: Rust's orphan rule accepts
/// `impl DeliveryMemory<LocalTag> for ForeignMemory`. A machine that owns its
/// memory type can implement it for every `P`.
pub trait DeliveryMemory<P: ?Sized>: PagedMemory {
    /// Translate a linear address for an implicit supervisor access (IDT, GDT,
    /// LDT and TSS reads during delivery): the U/S check is bypassed whatever
    /// the CPL; present and write-protect checks still apply. A fault returns
    /// `CpuError::PageFault` with the memory's pending-fault state set.
    fn translate_linear_supervisor_checked(
        &mut self,
        cpu: &Cpu,
        linear: u32,
        access: PageAccess,
    ) -> Result<usize>;
    /// Length of the RAM buffer, used to bound reads of an LDT that extends
    /// past its declared limit.
    fn usable_len(&self) -> usize;
    /// Tell the memory where the current LDT is, after a task switch loads it.
    #[inline]
    fn set_ldt_snoop_range(&mut self, _base: u32, _limit: u32) {}
}
/// Locals of protected-mode delivery that the observer hooks read.
#[derive(Debug, Clone, Copy)]
pub struct PmGateCtx {
    pub vector: u8,
    pub error_code: Option<u32>,
    pub is_software: bool,
    pub is_hardware_irq: bool,
    /// The raw 8-byte IDT gate descriptor.
    pub gate: [u8; 8],
    pub gate_type: u8,
    pub gate_dpl: u8,
    pub is_32bit: bool,
    /// Handler code selector from the gate.
    pub selector: u16,
    pub target_offset: u32,
    pub target_base: u32,
    pub target_cpl: u8,
    pub old_cpl: u8,
    pub old_cs: u16,
    pub old_ip: u16,
    pub old_eip: u32,
    pub old_ss: u16,
    pub old_sp: u16,
    pub old_esp: u32,
}
/// The interrupted V86 state pushed on the ring-0 stack, plus that stack.
#[derive(Debug, Clone, Copy)]
pub struct V86EntryCtx {
    pub ss0_sel: u16,
    pub ss0_esp: u32,
    pub v86_cs: u16,
    pub v86_eip: u32,
    pub v86_ss: u16,
    pub v86_esp: u32,
    pub v86_ds: u16,
    pub v86_es: u16,
    pub v86_fs: u16,
    pub v86_gs: u16,
    pub pushed_eflags: u32,
}
/// The CS, EIP and EFLAGS images of the protected-mode interrupt frame.
#[derive(Debug, Clone, Copy)]
pub struct PmFrameCtx {
    pub pushed_cs: u16,
    pub pushed_eip: u32,
    pub pushed_eflags: u32,
}
/// Machine hooks on the delivery path. Every hook has a no-op default.
///
/// Trace hooks take `&Cpu` and must not change guest state. The three `pm_*`
/// behaviour hooks may: `pm_gate_policy` can end a delivery with its own
/// frame, `pm_pre_frame` runs after any privilege stack switch and before the
/// standard frame push, and `pm_post_frame` runs after that push with the token
/// `pm_pre_frame` returned.
pub trait DeliveryObserver<M> {
    /// Carried from `pm_pre_frame` to `pm_post_frame`.
    type FrameToken: Default;
    /// `exception_with_error` entry, before the diagnostic ring entry.
    #[inline(always)]
    fn exception_entry(&mut self, _cpu: &Cpu, _mem: &mut M, _vector: u8, _error_code: u32) {}
    /// Delivery is about to deliver an event it raised itself while another
    /// delivery failed (SDM Vol. 3A Table 6-5): the serial second fault after
    /// a benign first event, or the escalated #DF. Called once per such
    /// delivery, before it starts, whatever its outcome. `error_code` is the
    /// code the event pushes.
    #[inline(always)]
    fn internal_delivery(&mut self, _cpu: &Cpu, _mem: &mut M, _vector: u8, _error_code: u32) {}
    /// `interrupt_software` entry, before mode routing.
    #[inline(always)]
    fn trace_software_int_entry(&mut self, _cpu: &Cpu, _mem: &mut M, _vector: u8) {}
    /// Real-mode delivery, after the IVT read and before the pushes.
    #[inline(always)]
    fn trace_rm_int_entry(
        &mut self,
        _cpu: &Cpu,
        _mem: &mut M,
        _vector: u8,
        _new_cs: u16,
        _new_ip: u16,
    ) {
    }
    /// Real-mode delivery, after the pushes and before IF/TF are cleared.
    #[inline(always)]
    fn trace_rm_clear_if(
        &mut self,
        _cpu: &Cpu,
        _mem: &mut M,
        _vector: u8,
        _new_cs: u16,
        _new_ip: u16,
    ) {
    }
    /// V86 to ring-0 delivery, after the nine-field frame (and error code) push.
    #[inline(always)]
    fn trace_v86_frame_pushed(
        &mut self,
        _cpu: &Cpu,
        _mem: &mut M,
        _ctx: &PmGateCtx,
        _v86: &V86EntryCtx,
    ) {
    }
    /// Protected-mode delivery, after the gate and target checks and before
    /// any stack switch. `Some` ends the delivery with that result.
    #[inline(always)]
    fn pm_gate_policy(
        &mut self,
        _cpu: &mut Cpu,
        _mem: &mut M,
        _ctx: &PmGateCtx,
    ) -> Option<Result<()>> {
        None
    }
    /// Protected-mode delivery, after any privilege stack switch and before
    /// the frame push. An error ends the delivery.
    #[inline(always)]
    fn pm_pre_frame(
        &mut self,
        _cpu: &mut Cpu,
        _mem: &mut M,
        _ctx: &PmGateCtx,
    ) -> Result<Self::FrameToken> {
        Ok(Self::FrameToken::default())
    }
    /// Immediately before the protected-mode frame push.
    #[inline(always)]
    fn trace_pm_frame_pre_push(
        &mut self,
        _cpu: &Cpu,
        _mem: &mut M,
        _ctx: &PmGateCtx,
        _frame: &PmFrameCtx,
    ) {
    }
    /// Immediately after the protected-mode frame push.
    #[inline(always)]
    fn trace_pm_frame_pushed(
        &mut self,
        _cpu: &Cpu,
        _mem: &mut M,
        _ctx: &PmGateCtx,
        _frame: &PmFrameCtx,
    ) {
    }
    /// After the frame push, with the token from `pm_pre_frame`. An error
    /// ends the delivery.
    #[inline(always)]
    fn pm_post_frame(
        &mut self,
        _cpu: &mut Cpu,
        _mem: &mut M,
        _token: Self::FrameToken,
        _ctx: &PmGateCtx,
    ) -> Result<()> {
        Ok(())
    }
    /// After the EFLAGS clears, before the handler CS load.
    #[inline(always)]
    fn trace_pm_frame_complete(&mut self, _cpu: &Cpu, _mem: &mut M, _ctx: &PmGateCtx) {}
    /// After a successful protected-mode delivery through an interrupt or
    /// trap gate (CS:EIP now the handler).
    #[inline(always)]
    fn trace_pm_int_done(&mut self, _cpu: &Cpu, _mem: &mut M, _ctx: &PmGateCtx) {}
}
/// The observer for a machine with no delivery hooks.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoObserver;
impl<M> DeliveryObserver<M> for NoObserver {
    type FrameToken = ();
}
/// The SDM Vol. 3A Table 6-4 class of the event whose delivery failed (S1b (l)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FirstEventClass {
    /// Hardware interrupts, INT n, and #DB, NMI, #BP, #OF, #BR, #UD, #NM, #MF,
    /// #AC, #MC and the other non-contributory exceptions.
    Benign,
    /// #DE, #TS, #NP, #SS, #GP.
    Contributory,
    /// #PF.
    PageFault,
    /// #DF.
    DoubleFault,
}
impl FirstEventClass {
    fn of_exception(vector: u8) -> Self {
        match vector {
            8 => Self::DoubleFault,
            14 => Self::PageFault,
            v if Cpu::is_contributory_exception(v) => Self::Contributory,
            _ => Self::Benign,
        }
    }
}
impl Cpu {
    /// Check if a vector is a contributory exception per Intel spec.
    /// Contributory exceptions are: #DE(0), #TS(10), #NP(11), #SS(12), #GP(13).
    /// A contributory exception during delivery of another contributory exception
    /// escalates to #DF (double fault).
    #[inline]
    pub fn is_contributory_exception(vector: u8) -> bool {
        matches!(vector, 0 | 10 | 11 | 12 | 13)
    }
    #[inline]
    pub fn diag_current_eip(&self, fallback_ip: u16) -> u32 {
        if self.is_protected_mode() || self.is_v86_mode() {
            self.regs.eip
        } else {
            fallback_ip as u32
        }
    }
    pub fn diag_push_interrupt_with_origin(
        &mut self,
        vector: u8,
        origin: RingInterruptOrigin,
        irq: Option<u8>,
    ) {
        if !self.diag_enabled {
            return;
        }
        let eip = self.diag_current_eip(self.regs.ip());
        let entry = match origin {
            RingInterruptOrigin::SoftwareInt => RingBufferEntry::software_interrupt(
                self.regs.cs,
                eip,
                self.regs.eax,
                self.regs.esp,
                self.regs.flags.bits(),
                vector,
            ),
            RingInterruptOrigin::HardwareIrq => RingBufferEntry::hardware_irq(
                self.regs.cs,
                eip,
                self.regs.eax,
                self.regs.esp,
                self.regs.flags.bits(),
                vector,
                irq,
            ),
            _ => RingBufferEntry::interrupt_full(
                self.regs.cs,
                eip,
                self.regs.eax,
                self.regs.esp,
                self.regs.flags.bits(),
                vector,
                origin,
                irq,
            ),
        };
        self.diag_ring.push(entry);
    }
    pub fn diag_push_exception(&mut self, vector: u8, error_code: u32) {
        if !self.diag_enabled {
            return;
        }
        let entry = RingBufferEntry::exception_full(
            self.regs.cs,
            self.diag_current_eip(self.regs.ip()),
            self.regs.eax,
            self.regs.esp,
            self.regs.flags.bits(),
            vector,
            error_code,
        );
        self.diag_ring.push(entry);
    }
    #[inline(never)]
    fn triple_fault_error(
        &self,
        origin: TripleFaultOrigin,
        vector: u8,
        irq: Option<u8>,
        error_code: Option<u32>,
        df_err: &CpuError,
    ) -> CpuError {
        let info = TripleFaultInfo {
            origin,
            vector,
            irq,
            error_code,
            escalation_vector: 8,
            cs: self.regs.cs,
            eip: self.regs.eip,
            idtr_base: self.regs.idtr.base,
            idtr_limit: self.regs.idtr.limit,
            cr0: self.regs.cr0,
            cr3: self.regs.cr3,
            cpl: self.regs.cpl,
            failed_delivery: format!("{df_err:?}"),
        };
        tracing::error!(
            target : "gp64::cpu", origin = ? info.origin, original_vector =
            format_args!("{:02X}", info.vector), irq = ? info.irq, original_error = ?
            info.error_code.map(| value | format!("{value:08X}")), escalation_vector =
            format_args!("{:02X}", info.escalation_vector), df_error = ? df_err, cs =
            format_args!("{:04X}", info.cs), eip = format_args!("{:08X}", info.eip),
            idtr_base = format_args!("{:08X}", info.idtr_base), idtr_limit =
            format_args!("{:04X}", info.idtr_limit), cr0 = format_args!("{:08X}", info
            .cr0), cr3 = format_args!("{:08X}", info.cr3), cpl = info.cpl,
            "Triple fault: double fault handler failed"
        );
        CpuError::TripleFault(info)
    }
    /// What happens after the delivery of a first event failed with `err`
    /// (S1b item (l); SDM Vol. 3A §6.15, Tables 6-4 and 6-5).
    ///
    /// The second fault's class decides together with `first`:
    /// - first #DF, any second fault: triple fault;
    /// - benign first, contributory second (#TS, #NP, #SS, #GP): the second
    ///   fault is delivered serially through its own vector, and that delivery
    ///   is itself subject to the table (as a contributory first event);
    /// - benign or contributory first, #PF second: serial too, but #PF goes
    ///   back to the caller as `Err(PageFault)`, which raises it (the X12 OQ-1
    ///   contract; delivery has no CR2/#PF policy of its own);
    /// - contributory first and contributory second, or #PF first and
    ///   contributory or #PF second: #DF, and a failed #DF is a triple fault.
    ///
    /// A triple fault ending a serial chain reports the original event.
    /// Any other error goes back to the caller unchanged. The serial error
    /// code is the one delivery has: `last_exception_error_code` for #GP/#SS
    /// (the PC side channel), `last_exception_error_code | EXT` for #NP (the
    /// #NP site records its code without EXT: `selector & 0xFFFC`, or
    /// `vector * 8 + 2` for a not-present IDT gate, S1b item (k)), 0 for #TS
    /// (the #TS code plumbing is a follow-up).
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn deliver_after_failed_delivery<M: DeliveryMemory<O>, O: DeliveryObserver<M>>(
        &mut self,
        memory: &mut M,
        obs: &mut O,
        first: FirstEventClass,
        origin: TripleFaultOrigin,
        vector: u8,
        irq: Option<u8>,
        error_code: Option<u32>,
        ext: bool,
        err: CpuError,
    ) -> Result<()> {
        let (second_is_pf, second_vector, second_code) = match err {
            CpuError::GeneralProtectionFault => (false, 13, self.last_exception_error_code),
            CpuError::StackFault => (false, 12, self.last_exception_error_code),
            CpuError::SegmentNotPresent(_) => {
                (false, 11, self.last_exception_error_code | u32::from(ext))
            }
            CpuError::Interrupt(10) => (false, 10, 0),
            CpuError::PageFault(_) => (true, 14, 0),
            other => return Err(other),
        };
        match (first, second_is_pf) {
            (FirstEventClass::DoubleFault, _) => {
                return Err(self.triple_fault_error(origin, vector, irq, error_code, &err));
            }
            (FirstEventClass::Benign, false) => {
                obs.internal_delivery(self, memory, second_vector, second_code);
                return match self.exception_with_error_obs(memory, obs, second_vector, second_code)
                {
                    Err(CpuError::TripleFault(mut info)) => {
                        info.origin = origin;
                        info.vector = vector;
                        info.irq = irq;
                        info.error_code = error_code;
                        Err(CpuError::TripleFault(info))
                    }
                    other => other,
                };
            }
            (FirstEventClass::Benign, true) | (FirstEventClass::Contributory, true) => {
                return Err(err);
            }
            (FirstEventClass::Contributory, false) | (FirstEventClass::PageFault, _) => {}
        }
        if let CpuError::PageFault(info) = &err {
            self.regs.cr2 = info.linear_address;
        }
        obs.internal_delivery(self, memory, 8, 0);
        if let Err(df_err) = self.interrupt_protected_mode(memory, obs, 8, Some(0), false) {
            return Err(self.triple_fault_error(origin, vector, irq, error_code, &df_err));
        }
        Ok(())
    }
    /// Trigger an interrupt delivery (hardware/external semantics).
    /// Use `interrupt_software` for INT-instruction privilege checks.
    ///
    /// Returns a terminal triple-fault error when double-fault delivery fails
    /// or a real-mode interrupt frame cannot fit on the stack.
    ///
    /// Observer-free entry: constructs `O::default()` for this delivery. Kept out of line
    /// (`#[inline(never)]`) to restore the baseline call structure at its callers.
    #[inline(never)]
    pub fn interrupt<M: DeliveryMemory<O>, O: DeliveryObserver<M> + Default>(
        &mut self,
        memory: &mut M,
        vector: u8,
    ) -> Result<()> {
        self.interrupt_obs(memory, &mut O::default(), vector)
    }
    /// Observer-carrying form of [`Self::interrupt`].
    pub fn interrupt_obs<M: DeliveryMemory<O>, O: DeliveryObserver<M>>(
        &mut self,
        memory: &mut M,
        obs: &mut O,
        vector: u8,
    ) -> Result<()> {
        if self.is_protected_mode() || self.is_v86_mode() {
            if let Err(err) = self.interrupt_protected_mode(memory, obs, vector, None, false) {
                return self.deliver_after_failed_delivery(
                    memory,
                    obs,
                    FirstEventClass::Benign,
                    TripleFaultOrigin::ExternalInterrupt,
                    vector,
                    None,
                    None,
                    true,
                    err,
                );
            }
        } else {
            self.interrupt_real_mode(
                memory,
                obs,
                vector,
                RingInterruptOrigin::LegacyUnknown,
                None,
            )?;
        }
        Ok(())
    }
    /// Trigger a hardware IRQ interrupt delivery.
    ///
    /// Like `interrupt()`, but explicitly marks this as a hardware IRQ to prevent
    /// the DPMI exception far-call dispatch for vectors 0x08-0x0F that overlap
    /// with CPU exceptions. Hardware IRQs always use standard interrupt frame
    /// format (push EFLAGS/CS/EIP then jump to handler), never DPMI exception
    /// far-call format.
    ///
    /// Observer-free entry: constructs `O::default()` for this delivery. Kept out of line
    /// (`#[inline(never)]`) to restore the baseline call structure at its callers.
    #[inline(never)]
    pub fn interrupt_hardware_irq<M: DeliveryMemory<O>, O: DeliveryObserver<M> + Default>(
        &mut self,
        memory: &mut M,
        vector: u8,
    ) -> Result<()> {
        self.interrupt_hardware_irq_obs(memory, &mut O::default(), vector)
    }
    /// Observer-carrying form of [`Self::interrupt_hardware_irq`].
    pub fn interrupt_hardware_irq_obs<M: DeliveryMemory<O>, O: DeliveryObserver<M>>(
        &mut self,
        memory: &mut M,
        obs: &mut O,
        vector: u8,
    ) -> Result<()> {
        self.interrupt_hardware_irq_with_irq_obs(memory, obs, vector, None)
    }
    /// Trigger a hardware IRQ interrupt delivery with a decoded IRQ line for diagnostics.
    ///
    /// Observer-free entry: constructs `O::default()` for this delivery. Kept out of line
    /// (`#[inline(never)]`) to restore the baseline call structure at its callers.
    #[inline(never)]
    pub fn interrupt_hardware_irq_with_irq<
        M: DeliveryMemory<O>,
        O: DeliveryObserver<M> + Default,
    >(
        &mut self,
        memory: &mut M,
        vector: u8,
        irq: Option<u8>,
    ) -> Result<()> {
        self.interrupt_hardware_irq_with_irq_obs(memory, &mut O::default(), vector, irq)
    }
    /// Observer-carrying form of [`Self::interrupt_hardware_irq_with_irq`].
    pub fn interrupt_hardware_irq_with_irq_obs<M: DeliveryMemory<O>, O: DeliveryObserver<M>>(
        &mut self,
        memory: &mut M,
        obs: &mut O,
        vector: u8,
        irq: Option<u8>,
    ) -> Result<()> {
        if self.is_protected_mode() || self.is_v86_mode() {
            match self.interrupt_protected_mode_with_flags_and_irq(
                memory, obs, vector, None, false, true, irq,
            ) {
                Ok(()) => {}
                Err(err) => {
                    return self.deliver_after_failed_delivery(
                        memory,
                        obs,
                        FirstEventClass::Benign,
                        TripleFaultOrigin::HardwareIrq,
                        vector,
                        irq,
                        None,
                        true,
                        err,
                    );
                }
            }
        } else {
            self.interrupt_real_mode(memory, obs, vector, RingInterruptOrigin::HardwareIrq, irq)?;
        }
        Ok(())
    }
    /// Trigger a software interrupt (INT instruction).
    ///
    /// This delivers a software-origin vector through the current interrupt
    /// mechanism. Opcode-level V86 INT n privilege filtering is handled by the
    /// instruction decoder before this delivery path is reached.
    ///
    /// Observer-free entry: constructs `O::default()` for this delivery. Kept out of line
    /// (`#[inline(never)]`) to restore the baseline call structure at its callers.
    #[inline(never)]
    pub fn interrupt_software<M: DeliveryMemory<O>, O: DeliveryObserver<M> + Default>(
        &mut self,
        memory: &mut M,
        vector: u8,
    ) -> Result<()> {
        self.interrupt_software_obs(memory, &mut O::default(), vector)
    }
    /// Observer-carrying form of [`Self::interrupt_software`].
    pub fn interrupt_software_obs<M: DeliveryMemory<O>, O: DeliveryObserver<M>>(
        &mut self,
        memory: &mut M,
        obs: &mut O,
        vector: u8,
    ) -> Result<()> {
        obs.trace_software_int_entry(self, memory, vector);
        if self.is_protected_mode() || self.is_v86_mode() {
            match self.interrupt_protected_mode(memory, obs, vector, None, true) {
                Ok(()) => {}
                Err(
                    e @ (CpuError::GeneralProtectionFault
                    | CpuError::StackFault
                    | CpuError::Interrupt(10)),
                ) => return Err(e),
                Err(e @ CpuError::SegmentNotPresent(_)) => return Err(e),
                Err(err) => {
                    return self.deliver_after_failed_delivery(
                        memory,
                        obs,
                        FirstEventClass::Benign,
                        TripleFaultOrigin::SoftwareInterrupt,
                        vector,
                        None,
                        None,
                        false,
                        err,
                    );
                }
            }
        } else {
            self.interrupt_real_mode(memory, obs, vector, RingInterruptOrigin::SoftwareInt, None)?;
        }
        Ok(())
    }
    /// Resolve a real-mode vector through the architectural IDTR and memory mapping.
    /// This lookup does not construct a frame or change CPU state.
    pub fn real_mode_vector<M: crate::MemoryAccess>(
        &self,
        memory: &M,
        vector: u8,
    ) -> Result<(u16, u16)> {
        let offset = u32::from(vector) * 4;
        let base = if self.cpu_type.supports_286() {
            if offset + 3 > u32::from(self.regs.idtr.limit) {
                // The 286/386 real-address contract raises #DF for an IDT limit
                // violation; later processors specify #GP(0).
                return Err(match self.cpu_type {
                    crate::CpuType::I80286 | crate::CpuType::I80386 => CpuError::Interrupt(8),
                    _ => CpuError::GeneralProtectionFault,
                });
            }
            self.regs.idtr.base
        } else {
            0 // The 8086/8088/186 have a fixed interrupt vector table.
        };
        let bytes = std::array::from_fn::<_, 4, _>(|i| {
            memory.read_u8(base.wrapping_add(offset).wrapping_add(i as u32) as usize)
        });
        Ok((
            u16::from_le_bytes([bytes[0], bytes[1]]),
            u16::from_le_bytes([bytes[2], bytes[3]]),
        ))
    }

    /// Real-mode interrupt handling (IVT-based)
    #[inline(never)]
    pub fn interrupt_real_mode<M: DeliveryMemory<O>, O: DeliveryObserver<M>>(
        &mut self,
        memory: &mut M,
        obs: &mut O,
        vector: u8,
        origin: RingInterruptOrigin,
        irq: Option<u8>,
    ) -> Result<()> {
        self.interrupt_real_mode_with_error(memory, obs, vector, origin, irq, None)
    }

    fn interrupt_real_mode_with_error<M: DeliveryMemory<O>, O: DeliveryObserver<M>>(
        &mut self,
        memory: &mut M,
        obs: &mut O,
        vector: u8,
        origin: RingInterruptOrigin,
        irq: Option<u8>,
        error_code: Option<u32>,
    ) -> Result<()> {
        let (delivery_vector, new_ip, new_cs) = match self.real_mode_vector(memory, vector) {
            Ok((ip, cs)) => (vector, ip, cs),
            Err(CpuError::Interrupt(8)) => match self.real_mode_vector(memory, 8) {
                Ok((ip, cs)) => (8, ip, cs),
                Err(error) => {
                    let fault_origin = match origin {
                        RingInterruptOrigin::SoftwareInt => TripleFaultOrigin::SoftwareInterrupt,
                        RingInterruptOrigin::HardwareIrq => TripleFaultOrigin::HardwareIrq,
                        RingInterruptOrigin::CpuException => TripleFaultOrigin::CpuException,
                        _ => TripleFaultOrigin::ExternalInterrupt,
                    };
                    return Err(self.triple_fault_error(
                        fault_origin,
                        vector,
                        irq,
                        error_code,
                        &error,
                    ));
                }
            },
            Err(error) => {
                if matches!(error, CpuError::GeneralProtectionFault) {
                    self.last_exception_error_code = 0;
                }
                return Err(error);
            }
        };
        // Check each wrapping word before writing any part of the frame.
        // An invalid real-mode frame cannot fit the ensuing #SS/#DF frame
        // either; return the terminal CPU error. Older CPUs retain wrapping.
        let sp = self.regs.sp();
        if [2, 4, 6]
            .into_iter()
            .any(|bytes| !self.deliver_stack_range_ok(u32::from(sp.wrapping_sub(bytes)), 2))
        {
            let fault_origin = match origin {
                RingInterruptOrigin::SoftwareInt => TripleFaultOrigin::SoftwareInterrupt,
                RingInterruptOrigin::HardwareIrq => TripleFaultOrigin::HardwareIrq,
                RingInterruptOrigin::CpuException => TripleFaultOrigin::CpuException,
                _ => TripleFaultOrigin::ExternalInterrupt,
            };
            return Err(self.triple_fault_error(
                fault_origin,
                vector,
                irq,
                error_code,
                &CpuError::StackFault,
            ));
        }
        let delivery_origin = if delivery_vector == vector {
            origin
        } else {
            RingInterruptOrigin::CpuException
        };
        self.diag_push_interrupt_with_origin(delivery_vector, delivery_origin, irq);
        obs.trace_rm_int_entry(self, memory, delivery_vector, new_cs, new_ip);
        self.deliver_try_push16(memory, self.regs.flags.bits())?;
        self.deliver_try_push16(memory, self.regs.cs)?;
        self.deliver_try_push16(memory, self.regs.ip())?;
        obs.trace_rm_clear_if(self, memory, delivery_vector, new_cs, new_ip);
        self.regs.flags.set(Flags::INTERRUPT, false);
        self.regs.flags.set(Flags::TRAP, false);
        self.interrupts_enabled = false;
        self.regs.set_ip(new_ip);
        self.regs.eip = new_ip as u32;
        self.regs.cs = new_cs;
        self.regs.cs_cache.base = (new_cs as u32) << 4;
        self.cs_bits = 16;
        Ok(())
    }
    /// Protected-mode interrupt handling (IDT-based)
    /// Per HLD §8.5: Uses IDT gate descriptor to fetch target CS:EIP
    pub fn interrupt_protected_mode<M: DeliveryMemory<O>, O: DeliveryObserver<M>>(
        &mut self,
        memory: &mut M,
        obs: &mut O,
        vector: u8,
        error_code: Option<u32>,
        is_software: bool,
    ) -> Result<()> {
        self.interrupt_protected_mode_with_flags(
            memory,
            obs,
            vector,
            error_code,
            is_software,
            false,
        )
    }
    /// Protected-mode interrupt handling with hardware IRQ flag
    pub fn interrupt_protected_mode_with_flags<M: DeliveryMemory<O>, O: DeliveryObserver<M>>(
        &mut self,
        memory: &mut M,
        obs: &mut O,
        vector: u8,
        error_code: Option<u32>,
        is_software: bool,
        is_hardware_irq: bool,
    ) -> Result<()> {
        self.interrupt_protected_mode_with_flags_and_irq(
            memory,
            obs,
            vector,
            error_code,
            is_software,
            is_hardware_irq,
            None,
        )
    }
    #[inline(never)]
    pub fn interrupt_protected_mode_with_flags_and_irq<
        M: DeliveryMemory<O>,
        O: DeliveryObserver<M>,
    >(
        &mut self,
        memory: &mut M,
        obs: &mut O,
        vector: u8,
        error_code: Option<u32>,
        is_software: bool,
        is_hardware_irq: bool,
        irq: Option<u8>,
    ) -> Result<()> {
        if let Some(dispatching) = self.dispatching_exception {
            if Self::is_contributory_exception(dispatching)
                && Self::is_contributory_exception(vector)
            {
                tracing::debug!(
                    target : "gp64::cpu", dispatching_vector = dispatching, new_vector
                    = vector, cs = format_args!("{:04X}", self.regs.cs), ip =
                    format_args!("{:04X}", self.regs.ip()),
                    "Contributory exception during contributory delivery - escalating to #DF"
                );
                self.dispatching_exception = None;
                return Err(CpuError::StackFault);
            }
        }
        self.last_interrupt_had_error_code = error_code.is_some();
        if error_code.is_some() {
            self.dispatching_exception = Some(vector);
        }
        let result = self.interrupt_protected_mode_inner_with_irq(
            memory,
            obs,
            vector,
            error_code,
            is_software,
            is_hardware_irq,
            irq,
        );
        self.dispatching_exception = None;
        result
    }
    #[inline(never)]
    pub fn interrupt_protected_mode_inner_with_irq<M: DeliveryMemory<O>, O: DeliveryObserver<M>>(
        &mut self,
        memory: &mut M,
        obs: &mut O,
        vector: u8,
        error_code: Option<u32>,
        is_software: bool,
        is_hardware_irq: bool,
        irq: Option<u8>,
    ) -> Result<()> {
        let origin = if is_software {
            RingInterruptOrigin::SoftwareInt
        } else if is_hardware_irq {
            RingInterruptOrigin::HardwareIrq
        } else if error_code.is_some() || vector < 32 {
            RingInterruptOrigin::CpuException
        } else {
            RingInterruptOrigin::LegacyUnknown
        };
        self.diag_push_interrupt_with_origin(vector, origin, irq);
        let idt_entry_offset = (vector as usize) * 8;
        let entry_end = idt_entry_offset + 7;
        if entry_end > self.regs.idtr.limit as usize {
            tracing::error!(
                target : "gp64::cpu", vector, idtr_base = format_args!("{:08X}", self
                .regs.idtr.base), idtr_limit = format_args!("{:04X}", self.regs.idtr
                .limit), entry_end = format_args!("{:04X}", entry_end), cs =
                format_args!("{:04X}", self.regs.cs), ip = format_args!("{:04X}", self
                .regs.ip()), cr0 = format_args!("{:08X}", self.regs.cr0), cr3 =
                format_args!("{:08X}", self.regs.cr3), cpl = self.regs.cpl, is_software,
                is_hardware_irq, "IDT entry exceeds limit"
            );
            return Err(CpuError::GeneralProtectionFault);
        }
        let idt_entry_addr = self.regs.idtr.base as usize + idt_entry_offset;
        let mut gate = [0u8; 8];
        for (i, byte) in gate.iter_mut().enumerate() {
            let phys = memory.translate_linear_supervisor_checked(
                self,
                (idt_entry_addr + i) as u32,
                PageAccess::Read,
            )?;
            *byte = memory.read_u8(phys);
        }
        let offset_low = u16::from_le_bytes([gate[0], gate[1]]);
        let selector = u16::from_le_bytes([gate[2], gate[3]]);
        let type_attr = gate[5];
        let offset_high = u16::from_le_bytes([gate[6], gate[7]]);
        let target_offset = (offset_low as u32) | ((offset_high as u32) << 16);
        let gate_type = type_attr & 0x0F;
        let is_32bit = (type_attr & 0x08) != 0;
        let gate_dpl = (type_attr >> 5) & 0x03;
        if !matches!(gate_type, 0x05 | 0x06 | 0x07 | 0x0E | 0x0F) {
            tracing::error!(
                target : "gp64::cpu",
                "Invalid IDT gate type {gate_type:02X} for vector {vector}"
            );
            return Err(CpuError::GeneralProtectionFault);
        }
        if is_software && self.regs.cpl > gate_dpl {
            self.last_exception_error_code = ((vector as u32) << 3) | 0x02;
            tracing::error!(
                target : "gp64::cpu", "INT {vector:02X} gate DPL {gate_dpl} < CPL {}",
                self.regs.cpl
            );
            return Err(CpuError::GeneralProtectionFault);
        }
        if (type_attr & 0x80) == 0 {
            tracing::error!(
                target : "gp64::cpu", vector, idtr_base = format_args!("{:08X}", self
                .regs.idtr.base), idtr_limit = format_args!("{:04X}", self.regs.idtr
                .limit), entry_addr = format_args!("{:08X}", idt_entry_addr), gate =
                format_args!("{:02X?}", gate), selector = format_args!("{:04X}",
                selector), offset = format_args!("{:08X}", target_offset), type_attr =
                format_args!("{:02X}", type_attr), cs = format_args!("{:04X}", self.regs
                .cs), ip = format_args!("{:04X}", self.regs.ip()), cr0 =
                format_args!("{:08X}", self.regs.cr0), cr3 = format_args!("{:08X}", self
                .regs.cr3), cpl = self.regs.cpl, is_software, is_hardware_irq,
                "IDT entry not present"
            );
            let code = (u16::from(vector) << 3) | 0x02;
            self.last_exception_error_code = u32::from(code);
            return Err(CpuError::SegmentNotPresent(code));
        }
        if gate_type == 0x05 {
            return self.task_switch_obs(memory, obs, TaskSwitchSource::Int, selector, error_code);
        }
        let target_desc = match self.read_descriptor(memory, selector) {
            Some(desc) => desc,
            None => {
                tracing::error!(
                    target : "gp64::cpu",
                    "Cannot read target CS descriptor for selector {selector:04X}"
                );
                return Err(CpuError::GeneralProtectionFault);
            }
        };
        let target_cpl = if target_desc.is_conforming {
            self.regs.cpl
        } else {
            target_desc.dpl
        };
        self.deliver_check_handler_cs(selector, &target_desc)?;
        let old_cpl = self.regs.cpl;
        let old_cs = self.regs.cs;
        let old_ip = self.regs.ip();
        let _old_eip = self.regs.eip;
        let old_ss = self.regs.ss;
        let old_sp = self.regs.sp();
        let old_esp = self.regs.esp;
        let ctx = PmGateCtx {
            vector,
            error_code,
            is_software,
            is_hardware_irq,
            gate,
            gate_type,
            gate_dpl,
            is_32bit,
            selector,
            target_offset,
            target_base: target_desc.base,
            target_cpl,
            old_cpl,
            old_cs,
            old_ip,
            old_eip: _old_eip,
            old_ss,
            old_sp,
            old_esp,
        };
        tracing::debug!(
            target : "gp64::cpu::pm_int", vector = format_args!("{:02X}", vector),
            error_code = format_args!("{}", error_code.map(| value | format!("{:08X}",
            value)).unwrap_or_else(|| "none".to_string())), is_software, is_hardware_irq,
            gate = format_args!("{:02X?}", gate), gate_type = format_args!("{:02X}",
            gate_type), gate_dpl, is_32bit, handler = format_args!("{:04X}:{:08X}",
            selector, target_offset), old_cpl, target_cpl, old_cs =
            format_args!("{:04X}", old_cs), old_ip = format_args!("{:04X}", old_ip),
            old_ss = format_args!("{:04X}", old_ss), old_sp = format_args!("{:04X}",
            old_sp), old_esp = format_args!("{:08X}", old_esp), ss_big = self.regs
            .ss_cache.big, ss_limit = format_args!("{:08X}", self.regs.ss_cache
            .effective_limit()), target_base = format_args!("{:08X}", target_desc.base),
            target_limit = format_args!("{:08X}", target_desc.effective_limit()),
            target_access = format_args!("{:02X}", target_desc.access), target_flags =
            format_args!("{:02X}", target_desc.flags), target_present = target_desc
            .present, "PM interrupt delivery start"
        );
        if self.regs.is_v86_mode() {
            if gate_type != 0x0E && gate_type != 0x0F {
                tracing::error!(
                    target : "gp64::cpu", vector = format_args!("{:02X}", vector),
                    gate_type = format_args!("{:02X}", gate_type),
                    "V86 interrupt through non-32-bit gate"
                );
                return Err(CpuError::GeneralProtectionFault);
            }
            if target_cpl != 0 {
                tracing::error!(
                    target : "gp64::cpu", vector = format_args!("{:02X}", vector),
                    target_cpl, "V86 interrupt handler CPL != 0"
                );
                return Err(CpuError::GeneralProtectionFault);
            }
            let v86_gs = self.regs.gs;
            let v86_fs = self.regs.fs;
            let v86_ds = self.regs.ds;
            let v86_es = self.regs.es;
            let v86_ss = self.regs.ss;
            let v86_esp = self.regs.esp;
            let v86_cs = self.regs.cs;
            let v86_eip = self.regs.eip & 0xFFFF;
            let pushed_eflags = self.regs.eflags();
            let (ss0_esp, ss0_sel) = self.read_tss_stack(memory, 0)?;
            let v86_ss_cache = self.regs.ss_cache;
            let v86_seg_caches = [
                self.regs.ds_cache,
                self.regs.es_cache,
                self.regs.fs_cache,
                self.regs.gs_cache,
            ];
            let v86_cs_bits = self.cs_bits;
            let v86_interrupts_enabled = self.interrupts_enabled;
            let v86_entry: Result<()> = (|| {
                self.regs.set_v86_mode(false);
                self.regs.set_cpl(0);
                self.cs_bits = 32;
                if let Err(e) = self.deliver_load_ss(memory, ss0_sel) {
                    tracing::error!(
                        target : "gp64::cpu", vector = format_args!("{:02X}", vector),
                        ss0 = format_args!("{:04X}", ss0_sel), err = ? e,
                        "V86→ring-0: load_ss(SS0) failed"
                    );
                    if let Some(code) =
                        self.deliver_tss_ss_not_present_code(memory, ss0_sel, 0, !is_software)
                    {
                        self.last_exception_error_code = code;
                        return Err(CpuError::StackFault);
                    }
                    return Err(CpuError::GeneralProtectionFault);
                }
                self.regs.esp = ss0_esp;
                if !self.regs.ss_cache.big {
                    self.regs.set_sp(ss0_esp as u16);
                }
                let v86_frame_bytes = if error_code.is_some() { 40 } else { 36 };
                if !self.deliver_frame_fits(v86_frame_bytes) {
                    return Err(CpuError::StackFault);
                }
                tracing::debug!(
                    target : "gp64::cpu", vector = format_args!("{:02X}", vector),
                    v86_cs = format_args!("{:04X}", v86_cs), v86_eip =
                    format_args!("{:04X}", v86_eip), ss0 = format_args!("{:04X}",
                    ss0_sel), esp0 = format_args!("{:08X}", ss0_esp), handler =
                    format_args!("{:04X}:{:08X}", selector, target_offset),
                    "V86 interrupt: entering ring-0 monitor"
                );
                self.deliver_try_push32(memory, v86_gs as u32)?;
                self.deliver_try_push32(memory, v86_fs as u32)?;
                self.deliver_try_push32(memory, v86_ds as u32)?;
                self.deliver_try_push32(memory, v86_es as u32)?;
                self.deliver_try_push32(memory, v86_ss as u32)?;
                self.deliver_try_push32(memory, v86_esp)?;
                self.deliver_try_push32(memory, pushed_eflags)?;
                self.deliver_try_push32(memory, v86_cs as u32)?;
                self.deliver_try_push32(memory, v86_eip)?;
                if let Some(ec) = error_code {
                    self.deliver_try_push32(memory, ec)?;
                }
                obs.trace_v86_frame_pushed(
                    self,
                    memory,
                    &ctx,
                    &V86EntryCtx {
                        ss0_sel,
                        ss0_esp,
                        v86_cs,
                        v86_eip,
                        v86_ss,
                        v86_esp,
                        v86_ds,
                        v86_es,
                        v86_fs,
                        v86_gs,
                        pushed_eflags,
                    },
                );
                self.regs.ds = 0;
                self.regs.ds_cache = SegmentCache::null_flat_usable();
                self.regs.es = 0;
                self.regs.es_cache = SegmentCache::null_flat_usable();
                self.regs.fs = 0;
                self.regs.fs_cache = SegmentCache::null_flat_usable();
                self.regs.gs = 0;
                self.regs.gs_cache = SegmentCache::null_flat_usable();
                if gate_type == 0x0E {
                    self.regs.flags.set(Flags::INTERRUPT, false);
                    self.interrupts_enabled = false;
                }
                self.regs.flags.set(Flags::TRAP, false);
                self.regs.flags.set(Flags::NESTED, false);
                self.regs.eflags_high &= !0x0001;
                if let Err(err) =
                    self.deliver_load_handler_cs(memory, selector, target_desc, target_cpl)
                {
                    tracing::error!(
                        target : "gp64::cpu", vector = format_args!("{:02X}", vector),
                        cs_sel = format_args!("{:04X}", selector), err = ? err,
                        "V86→ring-0: load_cs(handler) failed"
                    );
                    return Err(err);
                }
                self.regs.eip = target_offset;
                Ok(())
            })();
            if let Err(err) = v86_entry {
                self.deliver_restore_failed_v86_entry(
                    pushed_eflags,
                    v86_interrupts_enabled,
                    v86_cs_bits,
                    (v86_ss, v86_ss_cache, v86_esp),
                    [v86_ds, v86_es, v86_fs, v86_gs],
                    v86_seg_caches,
                );
                return Err(err);
            }
            return Ok(());
        }
        if let Some(result) = obs.pm_gate_policy(self, memory, &ctx) {
            return result;
        }
        let saved_ss_cache = self.regs.ss_cache;
        let pm_entry: Result<()> = (|| {
            if target_cpl < old_cpl {
                let (new_esp, new_ss) = match self.read_tss_stack(memory, target_cpl) {
                    Ok(stack) => stack,
                    Err(err) => {
                        tracing::error!(
                            target : "gp64::cpu::pm_int", vector =
                            format_args!("{:02X}", vector), target_cpl, old_cpl, err = ?
                            err, "PM interrupt privilege switch failed reading TSS stack"
                        );
                        return Err(err);
                    }
                };
                self.regs.set_cpl(target_cpl);
                if let Err(err) = self.deliver_load_ss(memory, new_ss) {
                    tracing::error!(
                        target : "gp64::cpu::pm_int", vector = format_args!("{:02X}",
                        vector), new_ss = format_args!("{:04X}", new_ss), new_esp =
                        format_args!("{:08X}", new_esp), target_cpl, old_cpl, err = ?
                        err, "PM interrupt privilege switch failed loading SS"
                    );
                    if let Some(code) = self.deliver_tss_ss_not_present_code(
                        memory,
                        new_ss,
                        target_cpl,
                        !is_software,
                    ) {
                        self.last_exception_error_code = code;
                    }
                    self.regs.set_cpl(old_cpl);
                    return Err(err);
                }
                if self.regs.ss_cache.big {
                    self.regs.esp = new_esp;
                } else {
                    self.regs.set_sp(new_esp as u16);
                }
                let slot = if is_32bit { 4 } else { 2 };
                let slots = if error_code.is_some() { 6 } else { 5 };
                if !self.deliver_frame_fits(slot * slots) {
                    return Err(CpuError::StackFault);
                }
                let stack_switch_push_result = if is_32bit {
                    self.deliver_try_push32(memory, old_ss as u32)
                        .and_then(|()| self.deliver_try_push32(memory, old_esp))
                } else {
                    self.deliver_try_push16(memory, old_ss)
                        .and_then(|()| self.deliver_try_push16(memory, old_sp))
                };
                if let Err(err) = stack_switch_push_result {
                    tracing::error!(
                        target : "gp64::cpu::pm_int", vector = format_args!("{:02X}",
                        vector), ss = format_args!("{:04X}", self.regs.ss), sp =
                        format_args!("{:04X}", self.regs.sp()), esp =
                        format_args!("{:08X}", self.regs.esp), ss_big = self.regs
                        .ss_cache.big, ss_limit = format_args!("{:08X}", self.regs
                        .ss_cache.effective_limit()), old_ss = format_args!("{:04X}",
                        old_ss), old_sp = format_args!("{:04X}", old_sp), old_esp =
                        format_args!("{:08X}", old_esp), err = ? err,
                        "PM interrupt privilege switch failed pushing old stack"
                    );
                    return Err(err);
                }
            }
            let frame_token = obs.pm_pre_frame(self, memory, &ctx)?;
            let pushed_error_code = error_code;
            let frame_pushed_eip = if self.cs_bits == 16 {
                self.regs.ip() as u32
            } else {
                self.regs.eip
            };
            let frame_pushed_cs = self.regs.cs;
            let frame_pushed_eflags = self.regs.eflags();
            let frame = PmFrameCtx {
                pushed_cs: frame_pushed_cs,
                pushed_eip: frame_pushed_eip,
                pushed_eflags: frame_pushed_eflags,
            };
            obs.trace_pm_frame_pre_push(self, memory, &ctx, &frame);
            if is_32bit {
                let frame_push_result = self
                    .deliver_try_push32(memory, frame_pushed_eflags)
                    .and_then(|()| self.deliver_try_push32(memory, frame_pushed_cs as u32))
                    .and_then(|()| self.deliver_try_push32(memory, frame_pushed_eip))
                    .and_then(|()| {
                        if let Some(ec) = pushed_error_code {
                            self.deliver_try_push32(memory, ec)
                        } else {
                            Ok(())
                        }
                    });
                if let Err(err) = frame_push_result {
                    tracing::error!(
                        target : "gp64::cpu::pm_int", vector = format_args!("{:02X}",
                        vector), ss = format_args!("{:04X}", self.regs.ss), sp =
                        format_args!("{:04X}", self.regs.sp()), esp =
                        format_args!("{:08X}", self.regs.esp), ss_big = self.regs
                        .ss_cache.big, ss_limit = format_args!("{:08X}", self.regs
                        .ss_cache.effective_limit()), ss_base = format_args!("{:08X}",
                        self.regs.ss_cache.base), ss_access = format_args!("{:02X}", self
                        .regs.ss_cache.access), ss_flags = format_args!("{:02X}", self
                        .regs.ss_cache.flags), ss_expand_down = ! self.regs.ss_cache
                        .is_code && (self.regs.ss_cache.access & 0x04) != 0, pushed_eip =
                        format_args!("{:08X}", frame_pushed_eip), pushed_error_code =
                        format_args!("{}", pushed_error_code.map(| value |
                        format!("{:08X}", value)).unwrap_or_else(|| "none".to_string())),
                        err = ? err, "PM interrupt failed pushing 32-bit frame"
                    );
                    return Err(err);
                }
            } else {
                let frame_push_result = self
                    .deliver_try_push16(memory, self.regs.flags.bits())
                    .and_then(|()| self.deliver_try_push16(memory, self.regs.cs))
                    .and_then(|()| self.deliver_try_push16(memory, self.regs.ip()))
                    .and_then(|()| {
                        if let Some(ec) = pushed_error_code {
                            self.deliver_try_push16(memory, ec as u16)
                        } else {
                            Ok(())
                        }
                    });
                if let Err(err) = frame_push_result {
                    tracing::error!(
                        target : "gp64::cpu::pm_int", vector = format_args!("{:02X}",
                        vector), ss = format_args!("{:04X}", self.regs.ss), sp =
                        format_args!("{:04X}", self.regs.sp()), esp =
                        format_args!("{:08X}", self.regs.esp), ss_big = self.regs
                        .ss_cache.big, ss_limit = format_args!("{:08X}", self.regs
                        .ss_cache.effective_limit()), ss_base = format_args!("{:08X}",
                        self.regs.ss_cache.base), ss_access = format_args!("{:02X}", self
                        .regs.ss_cache.access), ss_flags = format_args!("{:02X}", self
                        .regs.ss_cache.flags), ss_expand_down = ! self.regs.ss_cache
                        .is_code && (self.regs.ss_cache.access & 0x04) != 0, pushed_ip =
                        format_args!("{:04X}", self.regs.ip()), pushed_error_code =
                        format_args!("{}", pushed_error_code.map(| value |
                        format!("{:08X}", value)).unwrap_or_else(|| "none".to_string())),
                        err = ? err, "PM interrupt failed pushing 16-bit frame"
                    );
                    return Err(err);
                }
            }
            obs.trace_pm_frame_pushed(self, memory, &ctx, &frame);
            obs.pm_post_frame(self, memory, frame_token, &ctx)?;
            if gate_type == 0x0E || gate_type == 0x06 {
                if pm_int_if_debug_enabled() && self.interrupts_enabled {
                    tracing::debug!(
                        target : "gp64::cpu::pm_int_if", vector =
                        format_args!("{:02X}", vector), old_cs = format_args!("{:04X}",
                        self.regs.cs), old_ip = format_args!("{:04X}", self.regs.ip()),
                        handler_sel = format_args!("{:04X}", selector), handler_off =
                        format_args!("{:08X}", target_offset), "INT gate clears IF"
                    );
                }
                self.regs.flags.set(Flags::INTERRUPT, false);
                self.interrupts_enabled = false;
            }
            self.regs.flags.set(Flags::TRAP, false);
            self.regs.flags.set(Flags::NESTED, false);
            self.regs.eflags_high &= !0x0001;
            obs.trace_pm_frame_complete(self, memory, &ctx);
            self.regs.set_cpl(target_cpl);
            if let Err(err) =
                self.deliver_load_handler_cs(memory, selector, target_desc, target_cpl)
            {
                tracing::error!(
                    target : "gp64::cpu::pm_int", vector = format_args!("{:02X}",
                    vector), selector = format_args!("{:04X}", selector), target_offset =
                    format_args!("{:08X}", target_offset), target_cpl, old_cpl, err = ?
                    err, "PM interrupt failed loading handler CS"
                );
                self.regs.set_cpl(old_cpl);
                self.regs.set_eflags(frame_pushed_eflags);
                self.interrupts_enabled = (frame_pushed_eflags & 0x200) != 0;
                return Err(err);
            }
            self.regs.eip = target_offset;
            obs.trace_pm_int_done(self, memory, &ctx);
            Ok(())
        })();
        if let Err(err) = pm_entry {
            self.deliver_restore_failed_pm_entry(old_ss, saved_ss_cache, old_esp, old_cpl);
            return Err(err);
        }
        Ok(())
    }
    /// Trigger an exception that does not push an error code (for #DE, #UD, etc.).
    ///
    /// Observer-free entry: constructs `O::default()` for this delivery. Kept out of line
    /// (`#[inline(never)]`) to restore the baseline call structure at its callers.
    #[inline(never)]
    pub fn exception<M: DeliveryMemory<O>, O: DeliveryObserver<M> + Default>(
        &mut self,
        memory: &mut M,
        vector: u8,
    ) -> Result<()> {
        self.exception_obs(memory, &mut O::default(), vector)
    }
    /// Observer-carrying form of [`Self::exception`].
    pub fn exception_obs<M: DeliveryMemory<O>, O: DeliveryObserver<M>>(
        &mut self,
        memory: &mut M,
        obs: &mut O,
        vector: u8,
    ) -> Result<()> {
        self.diag_push_exception(vector, 0);
        if self.is_protected_mode() || self.is_v86_mode() {
            match self.interrupt_protected_mode(memory, obs, vector, None, false) {
                Ok(()) => {}
                Err(err) => {
                    return self.deliver_after_failed_delivery(
                        memory,
                        obs,
                        FirstEventClass::of_exception(vector),
                        TripleFaultOrigin::CpuException,
                        vector,
                        None,
                        None,
                        true,
                        err,
                    );
                }
            }
        } else {
            self.interrupt_real_mode(memory, obs, vector, RingInterruptOrigin::CpuException, None)?;
        }
        Ok(())
    }
    /// Trigger an exception with error code (for protected mode exceptions like #PF, #GP)
    ///
    /// Observer-free entry: constructs `O::default()` for this delivery. Kept out of line
    /// (`#[inline(never)]`) to restore the baseline call structure at its callers.
    #[inline(never)]
    pub fn exception_with_error<M: DeliveryMemory<O>, O: DeliveryObserver<M> + Default>(
        &mut self,
        memory: &mut M,
        vector: u8,
        error_code: u32,
    ) -> Result<()> {
        self.exception_with_error_obs(memory, &mut O::default(), vector, error_code)
    }
    /// Observer-carrying form of [`Self::exception_with_error`].
    pub fn exception_with_error_obs<M: DeliveryMemory<O>, O: DeliveryObserver<M>>(
        &mut self,
        memory: &mut M,
        obs: &mut O,
        vector: u8,
        error_code: u32,
    ) -> Result<()> {
        obs.exception_entry(self, memory, vector, error_code);
        self.diag_push_exception(vector, error_code);
        if self.is_protected_mode() || self.is_v86_mode() {
            match self.interrupt_protected_mode(memory, obs, vector, Some(error_code), false) {
                Ok(()) => {}
                Err(err) => {
                    return self.deliver_after_failed_delivery(
                        memory,
                        obs,
                        FirstEventClass::of_exception(vector),
                        TripleFaultOrigin::CpuException,
                        vector,
                        None,
                        Some(error_code),
                        true,
                        err,
                    );
                }
            }
        } else {
            self.interrupt_real_mode_with_error(
                memory,
                obs,
                vector,
                RingInterruptOrigin::CpuException,
                None,
                Some(error_code),
            )?;
        }
        Ok(())
    }
}
