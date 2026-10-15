//! x86 CPU emulation
//!
//! Implements a 386 CPU with:
//! - All general-purpose registers (EAX, EBX, ECX, EDX, ESI, EDI, EBP, ESP)
//! - Segment registers (CS, DS, ES, FS, GS, SS)
//! - Flags register (EFLAGS)
//! - Instruction pointer (EIP)
//!
//! This crate provides the CPU state and basic instruction execution.
//! For full emulation, it requires:
//! - `gp64-memory` for memory access
//! - An `IoPort` implementation for I/O operations
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
pub mod addressing;
mod cpu;
mod deliver;
pub mod dpmi_diag;
mod flags;
pub mod fpu;
pub mod irq;
pub mod msr;
pub mod registers;
mod serde_helpers;
pub mod sse;
pub mod sse_decode;
pub mod sse_decode_cache;
pub mod sse_exec;
/// Frozen pre-port SSE executor, kept only as the migration oracle.
#[cfg(test)]
#[cfg(feature = "iced-interop")]
mod sse_exec_reference;
#[cfg(any(
    target_arch = "x86_64",
    all(target_arch = "aarch64", target_endian = "little")
))]
mod sse_host_common;
#[cfg(target_arch = "x86_64")]
mod sse_host_kernels;
#[cfg(test)]
mod test_sync;
/// The host thread's MXCSR. native SSE uses it for its debug tripwire
/// (no Rust helper may run under a guest MXCSR) and its tests pin that a run
/// leaves the host word unchanged.
#[cfg(target_arch = "x86_64")]
#[must_use]
pub fn host_mxcsr() -> u32 {
    sse_host_kernels::read_mxcsr()
}
/// Whether the host floating-point environment supports approximate Fast arithmetic.
#[cfg(all(target_arch = "aarch64", target_endian = "little"))]
#[must_use]
pub fn fast_approx_fp_environment_supported() -> bool {
    sse_host_kernels::env_ok()
}
#[cfg(all(target_arch = "aarch64", target_endian = "little"))]
#[path = "sse_host_kernels_a64.rs"]
mod sse_host_kernels;
#[cfg_attr(all(target_arch = "x86_64", not(test)), allow(dead_code))]
mod x87_common;
pub mod x87_decode;
pub mod x87_decode_cache;
mod x87_fast_core;
#[cfg(feature = "iced-interop")]
pub mod x87_iced_adapter;
#[cfg(target_arch = "x86_64")]
pub mod x87_ops;
#[cfg(not(target_arch = "x86_64"))]
#[path = "x87_soft.rs"]
pub mod x87_ops;
#[cfg(all(test, target_arch = "x86_64"))]
mod x87_oracle;
#[cfg(all(test, target_arch = "x86_64"))]
#[allow(dead_code)]
#[path = "x87_soft.rs"]
mod x87_soft_test;
pub use cpu::{
    cpuid_family_model_stepping, cpuid_features, cpuid_leaf2, cpuid_max_basic_leaf,
    cpuid_signature, ret_guard_enabled, ret_guard_should_log, ret_guard_write_dump, Cpu,
    CpuSnapshot, CpuSnapshotValidationError, MmxOperandKind, MmxProfile, RepFetchLatch,
    RetGuardEntry, RetGuardKind, WakesPendingFlag,
};
pub use deliver::{
    DeliveryMemory, DeliveryObserver, NoObserver, PmFrameCtx, PmGateCtx, TaskSwitchSource,
    V86EntryCtx,
};
pub use dpmi_diag::{DpmiExceptionDiagState, DPMI_DIAG_TARGET, DPMI_EXCEPTION_REENTRY_THRESHOLD};
pub use flags::Flags;
pub use fpu::Fpu;
use gp64_types::define_trace_checker;
pub use gp64_types::{IoPort, MemoryAccess, PageAccess, PagedMemory};
#[cfg(feature = "iced-interop")]
use iced_x86::Instruction;
pub use registers::{
    DescriptorTableRegister, Registers, SegmentCache, CR0_EM, CR0_MP, CR0_TS, CR0_X87_CONTROL_MASK,
    CR4_DE, CR4_MCE, CR4_OSFXSR, CR4_OSXMMEXCPT, CR4_PAE, CR4_PCE, CR4_PGE, CR4_PSE, CR4_PVI,
    CR4_TSD, CR4_VME,
};
pub use sse::SseState;
use std::fmt;
define_trace_checker!(
    pub kbd_flow_trace_enabled, "gp64::kbd::flow", tracing::Level::INFO
);
/// Page fault information for exception delivery
#[derive(Debug, Clone, Copy)]
pub struct PageFaultInfo {
    /// Faulting linear address (stored in CR2)
    pub linear_address: u32,
    /// Error code pushed on stack
    /// Bit 0 (P): 0 = not present, 1 = protection violation
    /// Bit 1 (W/R): 0 = read access, 1 = write access
    /// Bit 2 (U/S): 0 = supervisor mode, 1 = user mode
    pub error_code: u32,
}
impl PageFaultInfo {
    /// Create a new page fault info
    /// - `linear`: the faulting linear address
    /// - `present`: true if page was present (protection violation), false if not present
    /// - `write`: true if this was a write access
    /// - `user`: true if access was from user mode (CPL=3)
    pub fn new(linear: u32, present: bool, write: bool, user: bool) -> Self {
        let mut error_code = 0u32;
        if present {
            error_code |= 1;
        }
        if write {
            error_code |= 2;
        }
        if user {
            error_code |= 4;
        }
        Self {
            linear_address: linear,
            error_code,
        }
    }
}
impl std::fmt::Display for PageFaultInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "linear={:#010X} error={:#010X}",
            self.linear_address, self.error_code
        )
    }
}
/// V86 switch frame popped from the stack during an IRETD that was blocked
/// because the CPU is not at CPL=0.  Contains the full V86 IRETD frame
/// (EIP, CS, EFLAGS, ESP, SS, ES, DS, FS, GS) so the emulator can redirect
/// through its HLE V86 entry mechanism.
#[derive(Debug, Clone)]
pub struct V86SwitchFrame {
    pub eip: u32,
    pub cs: u16,
    pub eflags: u32,
    pub esp: u32,
    pub ss: u16,
    pub es: u16,
    pub ds: u16,
    pub fs: u16,
    pub gs: u16,
}
/// Origin of the event whose failed delivery escalated to a CPU triple fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TripleFaultOrigin {
    /// External interrupt path without a decoded PIC IRQ line.
    ExternalInterrupt,
    /// Hardware IRQ delivered by the PIC.
    HardwareIrq,
    /// Software INT/INT3/INTO instruction.
    SoftwareInterrupt,
    /// CPU exception delivery.
    CpuException,
}
impl fmt::Display for TripleFaultOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExternalInterrupt => write!(f, "external interrupt"),
            Self::HardwareIrq => write!(f, "hardware IRQ"),
            Self::SoftwareInterrupt => write!(f, "software interrupt"),
            Self::CpuException => write!(f, "CPU exception"),
        }
    }
}
/// CPU state captured when failed #DF delivery caused a triple fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TripleFaultInfo {
    pub origin: TripleFaultOrigin,
    pub vector: u8,
    pub irq: Option<u8>,
    pub error_code: Option<u32>,
    pub escalation_vector: u8,
    pub cs: u16,
    pub eip: u32,
    pub idtr_base: u32,
    pub idtr_limit: u16,
    pub cr0: u32,
    pub cr3: u32,
    pub cpl: u8,
    pub failed_delivery: String,
}
impl fmt::Display for TripleFaultInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.origin, self.irq, self.error_code) {
            (TripleFaultOrigin::HardwareIrq, Some(irq), _) => {
                write!(f, "hardware IRQ {irq} vector {:02X}", self.vector)?
            }
            (TripleFaultOrigin::HardwareIrq, None, _) => {
                write!(f, "hardware IRQ vector {:02X}", self.vector)?
            }
            (TripleFaultOrigin::SoftwareInterrupt, _, _) => {
                write!(f, "software interrupt INT {:02X}", self.vector)?
            }
            (TripleFaultOrigin::ExternalInterrupt, _, _) => {
                write!(f, "external interrupt INT {:02X}", self.vector)?
            }
            (TripleFaultOrigin::CpuException, _, Some(error_code)) => write!(
                f,
                "CPU exception vector {} error={error_code:#010X}",
                self.vector
            )?,
            (TripleFaultOrigin::CpuException, _, None) => {
                write!(f, "CPU exception vector {}", self.vector)?
            }
        }
        write!(
            f,
            " escalated to #DF vector {:02X}, #DF delivery failed with {}; \
             cs:eip={:04X}:{:08X} idtr={:08X}:{:04X} cr0={:08X} cr3={:08X} cpl={}",
            self.escalation_vector,
            self.failed_delivery,
            self.cs,
            self.eip,
            self.idtr_base,
            self.idtr_limit,
            self.cr0,
            self.cr3,
            self.cpl
        )
    }
}
/// CPU execution error
#[derive(Debug)]
pub enum CpuError {
    InvalidOpcode(u32),
    Unimplemented(String),
    DivideByZero,
    GeneralProtectionFault,
    SegmentNotPresent(u16),
    StackFault,
    PageFault(PageFaultInfo),
    Halted,
    Interrupt(u8),
    SoftwareInterrupt(u8),
    FpuStub(u8),
    TripleFault(TripleFaultInfo),
    Diagnostic(String),
    V86SwitchBlocked(V86SwitchFrame),
    TaskSwitchNeeded,
    /// load_cs() detected a TSS descriptor — caller should perform a task switch
    TssDescriptor(u16),
    TaskSwitchJmp(u16),
    TaskSwitchCall(u16),
    /// load_cs() detected a call gate descriptor — caller should perform a gate call
    CallGate(u16),
    CallGateTransfer {
        selector: u16,
        return_cs: u16,
        return_eip: u32,
    },
}
impl fmt::Display for CpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOpcode(address) => write!(f, "Invalid opcode at {address:#X}"),
            Self::Unimplemented(instruction) => {
                write!(f, "Unimplemented instruction: {instruction}")
            }
            Self::DivideByZero => f.write_str("Division by zero"),
            Self::GeneralProtectionFault => f.write_str("General protection fault"),
            Self::SegmentNotPresent(selector) => {
                write!(f, "Segment not present: selector 0x{selector:04X}")
            }
            Self::StackFault => f.write_str("Stack segment fault"),
            Self::PageFault(info) => write!(f, "Page fault: {info}"),
            Self::Halted => f.write_str("Halt instruction executed"),
            Self::Interrupt(vector) => write!(f, "CPU exception: vector {vector:#X}"),
            Self::SoftwareInterrupt(vector) => {
                write!(f, "Software interrupt: INT {vector:#X}")
            }
            Self::FpuStub(skipped) => write!(f, "FPU instruction stub (skipped {skipped} bytes)"),
            Self::TripleFault(info) => write!(f, "triple fault: {info}"),
            Self::Diagnostic(message) => write!(f, "Diagnostic: {message}"),
            Self::V86SwitchBlocked(_) => f.write_str("V86 switch blocked at CPL>0"),
            Self::TaskSwitchNeeded => f.write_str("Task switch needed (IRET with NT=1)"),
            Self::TssDescriptor(selector) => {
                write!(f, "TSS descriptor encountered (selector {selector:#06X})")
            }
            Self::TaskSwitchJmp(selector) => {
                write!(f, "TSS task switch via JMP (selector {selector:#06X})")
            }
            Self::TaskSwitchCall(selector) => {
                write!(f, "TSS task switch via CALL (selector {selector:#06X})")
            }
            Self::CallGate(selector) => {
                write!(f, "Call gate descriptor (selector {selector:#06X})")
            }
            Self::CallGateTransfer { selector, .. } => {
                write!(f, "Call gate descriptor (selector {selector:#06X})")
            }
        }
    }
}
impl std::error::Error for CpuError {}
/// Result type for CPU operations
pub type Result<T> = std::result::Result<T, CpuError>;
/// Data segment register selector for `load_data_segment`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataSegReg {
    DS,
    ES,
    FS,
    GS,
}
/// CPU type for instruction set filtering and behavior differences
///
/// `repr(u8)` fixes the in-memory byte to the declaration discriminant, so
/// native code may test `Cpu::cpu_type` against a mask built from
/// [`CpuType::ALL`] and the capability predicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum CpuType {
    /// Intel 8086 - original 16-bit processor
    I8086,
    /// Intel 8088 - 8-bit bus version of 8086 (same instruction set)
    I8088,
    /// Intel 80186 - enhanced 8086 with additional instructions
    I80186,
    /// Intel 80286 - protected mode capable
    I80286,
    /// Intel 80386 - 32-bit processor (default)
    #[default]
    I80386,
    /// Intel 80486 - integrated FPU, BSWAP, XADD, CMPXCHG
    I80486,
    /// Intel Pentium (P54C) - CPUID, RDTSC, CMPXCHG8B. No MMX.
    Pentium,
    /// Intel Pentium MMX (P55C) - Pentium plus the MMX instruction set.
    PentiumMmx,
    /// Intel Pentium Pro (P6, family 6 model 1) - CMOVcc, FCMOVcc, FCOMI.
    ///
    /// Deliberately has NO MMX: real Pentium Pro silicon shipped without it.
    PentiumPro,
    /// Intel Pentium II (Klamath, family 6 model 3) - P6 plus MMX.
    PentiumII,
    /// Intel Pentium III (Katmai, family 6 model 7) - SSE plus P6/MMX.
    PentiumIII,
    /// Intel Pentium 4 (family 15 model 2) - SSE2 plus SSE/P6/MMX.
    Pentium4,
    /// Intel Pentium III Coppermine, with implemented P6 system features.
    PentiumIIICoppermine,
}
/// Every `CpuType`, for tests that must cover the whole enum.
///
/// Built from an exhaustive match so adding a variant is a COMPILE ERROR here
/// rather than silent under-coverage in the six hand-written variant arrays
/// that used to be the only enumeration. Three separate tests were found
/// passing vacuously for exactly that reason when the P6 identities were added
/// (see `wrk_docs/2026.08.05 - HLD - P6 guest CPU identity ...`, D11).
impl CpuType {
    pub const ALL: [CpuType; 13] = {
        const fn assert_total(t: CpuType) -> CpuType {
            match t {
                CpuType::I8086
                | CpuType::I8088
                | CpuType::I80186
                | CpuType::I80286
                | CpuType::I80386
                | CpuType::I80486
                | CpuType::Pentium
                | CpuType::PentiumMmx
                | CpuType::PentiumPro
                | CpuType::PentiumII
                | CpuType::PentiumIII
                | CpuType::PentiumIIICoppermine
                | CpuType::Pentium4 => t,
            }
        }
        [
            assert_total(CpuType::I8086),
            assert_total(CpuType::I8088),
            assert_total(CpuType::I80186),
            assert_total(CpuType::I80286),
            assert_total(CpuType::I80386),
            assert_total(CpuType::I80486),
            assert_total(CpuType::Pentium),
            assert_total(CpuType::PentiumMmx),
            assert_total(CpuType::PentiumPro),
            assert_total(CpuType::PentiumII),
            assert_total(CpuType::PentiumIII),
            assert_total(CpuType::Pentium4),
            assert_total(CpuType::PentiumIIICoppermine),
        ]
    };
}
impl CpuType {
    /// The identity's nominal core clock — what the guest-visible TSC
    /// ticks at under the constant-TSC model (`Cpu::set_tsc_rate`),
    /// decoupled from the emulation speed. `None` for identities without
    /// RDTSC. Defaults follow historical emulator clock grades: P54C-100, P55C-200, PentiumPro-200,
    /// PII Deschutes-400, PIII legacy 1000 MHz default (Katmai identity), and P4 Northwood-2400
    /// (family 15 model 2 — the 2.4 GHz grade). Exhaustive so a new
    /// identity must decide its clock explicitly.
    #[inline]
    pub fn nominal_tsc_hz(&self) -> Option<u64> {
        match self {
            CpuType::I8086
            | CpuType::I8088
            | CpuType::I80186
            | CpuType::I80286
            | CpuType::I80386
            | CpuType::I80486 => None,
            CpuType::Pentium => Some(100_000_000),
            CpuType::PentiumMmx => Some(200_000_000),
            CpuType::PentiumPro => Some(200_000_000),
            CpuType::PentiumII => Some(400_000_000),
            CpuType::PentiumIII => Some(1_000_000_000),
            CpuType::PentiumIIICoppermine => Some(1_000_000_000),
            CpuType::Pentium4 => Some(2_400_000_000),
        }
    }
    /// Returns true if this CPU type supports 186+ instructions
    #[inline]
    pub fn supports_186(&self) -> bool {
        !matches!(self, CpuType::I8086 | CpuType::I8088)
    }
    /// Returns true if this CPU type supports 286+ instructions
    ///
    /// Exhaustive, like every predicate below: a `matches!` allow-list makes a
    /// new CPU variant silently *unsupported* everywhere — including for CPUID
    /// and the whole 386 estate — with no build error. Adding a guest CPU must
    /// be a compile error until each capability is decided explicitly.
    #[inline]
    pub fn supports_286(&self) -> bool {
        match self {
            CpuType::I8086 | CpuType::I8088 | CpuType::I80186 => false,
            CpuType::I80286
            | CpuType::I80386
            | CpuType::I80486
            | CpuType::Pentium
            | CpuType::PentiumMmx
            | CpuType::PentiumPro
            | CpuType::PentiumII
            | CpuType::PentiumIII
            | CpuType::PentiumIIICoppermine
            | CpuType::Pentium4 => true,
        }
    }
    /// Returns true if this CPU type supports 386+ instructions
    #[inline]
    pub fn supports_386(&self) -> bool {
        match self {
            CpuType::I8086 | CpuType::I8088 | CpuType::I80186 | CpuType::I80286 => false,
            CpuType::I80386
            | CpuType::I80486
            | CpuType::Pentium
            | CpuType::PentiumMmx
            | CpuType::PentiumPro
            | CpuType::PentiumII
            | CpuType::PentiumIII
            | CpuType::PentiumIIICoppermine
            | CpuType::Pentium4 => true,
        }
    }
    /// Returns true if this CPU type supports 486+ instructions
    #[inline]
    pub fn supports_486(&self) -> bool {
        match self {
            CpuType::I8086
            | CpuType::I8088
            | CpuType::I80186
            | CpuType::I80286
            | CpuType::I80386 => false,
            CpuType::I80486
            | CpuType::Pentium
            | CpuType::PentiumMmx
            | CpuType::PentiumPro
            | CpuType::PentiumII
            | CpuType::PentiumIII
            | CpuType::PentiumIIICoppermine
            | CpuType::Pentium4 => true,
        }
    }
    /// Returns true if this CPU type supports Pentium instructions
    #[inline]
    pub fn supports_pentium(&self) -> bool {
        match self {
            CpuType::I8086
            | CpuType::I8088
            | CpuType::I80186
            | CpuType::I80286
            | CpuType::I80386
            | CpuType::I80486 => false,
            CpuType::Pentium
            | CpuType::PentiumMmx
            | CpuType::PentiumPro
            | CpuType::PentiumII
            | CpuType::PentiumIII
            | CpuType::PentiumIIICoppermine
            | CpuType::Pentium4 => true,
        }
    }
    /// Returns true if this CPU implements and advertises baseline MMX.
    ///
    /// This predicate is the single guest-visible feature gate: CPUID,
    /// CPU feature queries, and opcode execution must agree.
    #[inline]
    pub fn supports_mmx(&self) -> bool {
        match self {
            CpuType::I8086
            | CpuType::I8088
            | CpuType::I80186
            | CpuType::I80286
            | CpuType::I80386
            | CpuType::I80486
            | CpuType::Pentium
            | CpuType::PentiumPro => false,
            CpuType::PentiumMmx
            | CpuType::PentiumII
            | CpuType::PentiumIII
            | CpuType::PentiumIIICoppermine
            | CpuType::Pentium4 => true,
        }
    }
    /// Returns true if this CPU supports P6 (Pentium Pro) instructions.
    ///
    /// Keep this match exhaustive so adding a newer guest CPU requires an
    /// explicit decision about P6-only instructions such as CMOVcc.
    #[inline]
    pub fn supports_p6(&self) -> bool {
        match self {
            CpuType::I8086
            | CpuType::I8088
            | CpuType::I80186
            | CpuType::I80286
            | CpuType::I80386
            | CpuType::I80486
            | CpuType::Pentium
            | CpuType::PentiumMmx => false,
            CpuType::PentiumPro
            | CpuType::PentiumII
            | CpuType::PentiumIII
            | CpuType::PentiumIIICoppermine
            | CpuType::Pentium4 => true,
        }
    }
    /// Returns true if this CPU implements FXSAVE/FXRSTOR and XMM state.
    #[inline]
    pub fn supports_fxsave(&self) -> bool {
        matches!(
            self,
            CpuType::PentiumIII | CpuType::PentiumIIICoppermine | CpuType::Pentium4
        )
    }
    /// Returns true if this CPU implements baseline SSE instructions.
    #[inline]
    pub fn supports_sse(&self) -> bool {
        matches!(
            self,
            CpuType::PentiumIII | CpuType::PentiumIIICoppermine | CpuType::Pentium4
        )
    }
    /// Returns true if this CPU implements SSE2 instructions.
    #[inline]
    pub fn supports_sse2(&self) -> bool {
        matches!(self, CpuType::Pentium4)
    }
    /// P6 system features implemented for the Coppermine identity only.
    #[inline]
    pub fn supports_sep(&self) -> bool {
        matches!(self, CpuType::PentiumIIICoppermine)
    }
    #[inline]
    pub fn supports_mtrr(&self) -> bool {
        matches!(self, CpuType::PentiumIIICoppermine)
    }
    #[inline]
    pub fn supports_pge(&self) -> bool {
        matches!(self, CpuType::PentiumIIICoppermine)
    }
    #[inline]
    pub fn supports_pat(&self) -> bool {
        matches!(self, CpuType::PentiumIIICoppermine)
    }
    /// CR4 bits accepted by this guest identity: the ones the identity has
    /// AND this emulator implements. MOV CR4 raises #GP and snapshot
    /// validation refuses anything else.
    ///
    /// CR4 arrived with the Pentium, so older identities accept no bits.
    /// TSD, DE, PSE and MCE match the family-5 features [`cpuid_features`]
    /// advertises; PCE arrives with RDPMC and OSFXSR/OSXMMEXCPT with XMM
    /// state. PGE is accepted only on Coppermine. VME, PVI and PAE are never accepted: none is implemented
    /// (the page walk has only the two-level 32-bit format), so setting one
    /// would silently run the guest under the wrong semantics
    /// (MDD-BUG-KIL-00765).
    ///
    /// [`cpuid_features`]: crate::cpu::cpuid_features
    #[inline]
    pub fn cr4_valid_mask(&self) -> u32 {
        use crate::registers::{
            CR4_DE, CR4_MCE, CR4_OSFXSR, CR4_OSXMMEXCPT, CR4_PCE, CR4_PGE, CR4_PSE, CR4_TSD,
        };
        if !self.supports_pentium() {
            return 0;
        }
        let mut mask = CR4_TSD | CR4_DE | CR4_PSE | CR4_MCE;
        if self.supports_pge() {
            mask |= CR4_PGE;
        }
        if self.supports_rdpmc() {
            mask |= CR4_PCE;
        }
        if self.supports_sse() {
            mask |= CR4_OSFXSR | CR4_OSXMMEXCPT;
        }
        mask
    }
    /// Returns true if this CPU implements `RDPMC`.
    ///
    /// Deliberately NOT `supports_p6()`. Per the SDM's RDPMC "IA-32
    /// Architecture Compatibility" note the instruction arrived "in the
    /// Pentium Pro processor and the Pentium processor with MMX technology" —
    /// earlier Pentiums have the counters but must read them with RDMSR. So
    /// P55C executes RDPMC, and gating it on P6 would make us wrong about a
    /// part we already model.
    #[inline]
    pub fn supports_rdpmc(&self) -> bool {
        self.supports_mmx() || self.supports_p6()
    }
    /// Returns true if this CPU type supports SSE3 instructions.
    ///
    /// Keep this match exhaustive so adding a newer guest CPU requires an
    /// explicit decision about SSE3-only instructions such as FISTTP.
    #[inline]
    pub fn supports_sse3(&self) -> bool {
        match self {
            CpuType::I8086
            | CpuType::I8088
            | CpuType::I80186
            | CpuType::I80286
            | CpuType::I80386
            | CpuType::I80486
            | CpuType::Pentium
            | CpuType::PentiumMmx
            | CpuType::PentiumPro
            | CpuType::PentiumII
            | CpuType::PentiumIII
            | CpuType::PentiumIIICoppermine
            | CpuType::Pentium4 => false,
        }
    }
    /// Returns true if PUSH SP pushes the decremented value (8086 behavior)
    /// On 8086/8088, PUSH SP pushes SP-2; on 286+, it pushes original SP
    #[inline]
    pub fn push_sp_decremented(&self) -> bool {
        matches!(self, CpuType::I8086 | CpuType::I8088)
    }
    /// Returns true if segment:offset addressing wraps at 64KB (8086 behavior)
    #[inline]
    pub fn segment_wrap(&self) -> bool {
        matches!(self, CpuType::I8086 | CpuType::I8088 | CpuType::I80186)
    }
}
/// Ring buffer size for execution history
pub const RING_BUFFER_SIZE: usize = 1024;
/// Event type for ring buffer entries
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RingBufferEventType {
    Instruction = 0,
    Interrupt = 1,
    Exception = 2,
    Dpmi = 3,
}
/// Provenance for interrupt-like ring buffer entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RingInterruptOrigin {
    None = 0,
    SoftwareInt = 1,
    HardwareIrq = 2,
    CpuException = 3,
    LegacyUnknown = 4,
}
impl RingInterruptOrigin {
    #[inline]
    pub fn from_raw(value: u8) -> Self {
        match value {
            1 => Self::SoftwareInt,
            2 => Self::HardwareIrq,
            3 => Self::CpuException,
            4 => Self::LegacyUnknown,
            _ => Self::None,
        }
    }
}
pub const RING_IRQ_NONE: u8 = 0xFF;
/// Ring buffer entry for execution history
#[derive(Clone, Copy, Debug)]
pub struct RingBufferEntry {
    /// CS:IP packed as (CS << 16) | IP
    pub cs_ip: u32,
    /// Full instruction/interrupt offset. This is authoritative in protected mode.
    pub eip: u32,
    /// EAX value
    pub eax: u32,
    /// ESP value
    pub esp: u32,
    /// FLAGS value
    pub flags: u16,
    /// Event type (instruction, interrupt, exception, dpmi)
    pub event_type: u8,
    /// Interrupt provenance for interrupt-like events.
    pub interrupt_origin: u8,
    /// Decoded hardware IRQ line, or `RING_IRQ_NONE` when not applicable/unknown.
    pub irq: u8,
    /// Extra info (vector for interrupt, error code for exception, AX for DPMI)
    pub extra: u32,
}
impl Default for RingBufferEntry {
    fn default() -> Self {
        Self {
            cs_ip: 0,
            eip: 0,
            eax: 0,
            esp: 0,
            flags: 0,
            event_type: RingBufferEventType::Instruction as u8,
            interrupt_origin: RingInterruptOrigin::None as u8,
            irq: RING_IRQ_NONE,
            extra: 0,
        }
    }
}
impl RingBufferEntry {
    /// Create a new entry for instruction execution
    pub fn instruction(cs: u16, ip: u16, eax: u32, esp: u32, flags: u16) -> Self {
        Self::instruction_full(cs, ip as u32, eax, esp, flags)
    }
    /// Create a new entry for instruction execution with a full EIP.
    pub fn instruction_full(cs: u16, eip: u32, eax: u32, esp: u32, flags: u16) -> Self {
        Self {
            cs_ip: ((cs as u32) << 16) | ((eip as u16) as u32),
            eip,
            eax,
            esp,
            flags,
            event_type: RingBufferEventType::Instruction as u8,
            interrupt_origin: RingInterruptOrigin::None as u8,
            irq: RING_IRQ_NONE,
            extra: 0,
        }
    }
    /// Create a new entry for interrupt
    pub fn interrupt(cs: u16, ip: u16, eax: u32, esp: u32, flags: u16, vector: u8) -> Self {
        Self::interrupt_full(
            cs,
            ip as u32,
            eax,
            esp,
            flags,
            vector,
            RingInterruptOrigin::LegacyUnknown,
            None,
        )
    }
    /// Create a new entry for a software interrupt.
    pub fn software_interrupt(
        cs: u16,
        eip: u32,
        eax: u32,
        esp: u32,
        flags: u16,
        vector: u8,
    ) -> Self {
        Self::interrupt_full(
            cs,
            eip,
            eax,
            esp,
            flags,
            vector,
            RingInterruptOrigin::SoftwareInt,
            None,
        )
    }
    /// Create a new entry for a hardware IRQ interrupt.
    pub fn hardware_irq(
        cs: u16,
        eip: u32,
        eax: u32,
        esp: u32,
        flags: u16,
        vector: u8,
        irq: Option<u8>,
    ) -> Self {
        Self::interrupt_full(
            cs,
            eip,
            eax,
            esp,
            flags,
            vector,
            RingInterruptOrigin::HardwareIrq,
            irq,
        )
    }
    /// Create a new entry for interrupt delivery with explicit provenance.
    pub fn interrupt_full(
        cs: u16,
        eip: u32,
        eax: u32,
        esp: u32,
        flags: u16,
        vector: u8,
        origin: RingInterruptOrigin,
        irq: Option<u8>,
    ) -> Self {
        Self {
            cs_ip: ((cs as u32) << 16) | ((eip as u16) as u32),
            eip,
            eax,
            esp,
            flags,
            event_type: RingBufferEventType::Interrupt as u8,
            interrupt_origin: origin as u8,
            irq: irq.unwrap_or(RING_IRQ_NONE),
            extra: vector as u32,
        }
    }
    /// Create a new entry for exception
    pub fn exception(
        cs: u16,
        ip: u16,
        eax: u32,
        esp: u32,
        flags: u16,
        vector: u8,
        error_code: u32,
    ) -> Self {
        Self::exception_full(cs, ip as u32, eax, esp, flags, vector, error_code)
    }
    /// Create a new entry for exception with a full EIP.
    pub fn exception_full(
        cs: u16,
        eip: u32,
        eax: u32,
        esp: u32,
        flags: u16,
        vector: u8,
        error_code: u32,
    ) -> Self {
        Self {
            cs_ip: ((cs as u32) << 16) | ((eip as u16) as u32),
            eip,
            eax,
            esp,
            flags,
            event_type: RingBufferEventType::Exception as u8,
            interrupt_origin: RingInterruptOrigin::CpuException as u8,
            irq: RING_IRQ_NONE,
            extra: ((vector as u32) << 24) | (error_code & 0x00FF_FFFF),
        }
    }
    /// Create a new entry for DPMI call
    pub fn dpmi(cs: u16, ip: u16, eax: u32, esp: u32, flags: u16, ax: u16) -> Self {
        Self::dpmi_full(cs, ip as u32, eax, esp, flags, ax)
    }
    /// Create a new entry for DPMI call with a full EIP.
    pub fn dpmi_full(cs: u16, eip: u32, eax: u32, esp: u32, flags: u16, ax: u16) -> Self {
        Self {
            cs_ip: ((cs as u32) << 16) | ((eip as u16) as u32),
            eip,
            eax,
            esp,
            flags,
            event_type: RingBufferEventType::Dpmi as u8,
            interrupt_origin: RingInterruptOrigin::None as u8,
            irq: RING_IRQ_NONE,
            extra: ax as u32,
        }
    }
    /// Extract CS from the packed `cs_ip` field
    #[inline]
    pub fn cs(&self) -> u16 {
        (self.cs_ip >> 16) as u16
    }
    /// Extract IP from the packed `cs_ip` field
    #[inline]
    pub fn ip(&self) -> u16 {
        self.cs_ip as u16
    }
    /// Full offset recorded for the event.
    #[inline]
    pub fn eip(&self) -> u32 {
        if self.eip == 0 && self.ip() != 0 {
            self.ip() as u32
        } else {
            self.eip
        }
    }
    /// Interrupt provenance for this entry.
    #[inline]
    pub fn interrupt_origin(&self) -> RingInterruptOrigin {
        RingInterruptOrigin::from_raw(self.interrupt_origin)
    }
    /// Decoded hardware IRQ line, if known.
    #[inline]
    pub fn irq(&self) -> Option<u8> {
        (self.irq != RING_IRQ_NONE).then_some(self.irq)
    }
}
/// Execution history ring buffer for debugging
pub struct ExecutionRingBuffer {
    entries: Box<[RingBufferEntry; RING_BUFFER_SIZE]>,
    head: usize,
    count: usize,
}
impl Default for ExecutionRingBuffer {
    fn default() -> Self {
        Self::new()
    }
}
impl ExecutionRingBuffer {
    /// Create a new empty ring buffer
    pub fn new() -> Self {
        Self {
            entries: vec![RingBufferEntry::default(); RING_BUFFER_SIZE]
                .into_boxed_slice()
                .try_into()
                .expect("vec length mismatch"),
            head: 0,
            count: 0,
        }
    }
    /// Push an entry to the ring buffer
    #[inline]
    pub fn push(&mut self, entry: RingBufferEntry) {
        self.entries[self.head] = entry;
        self.head = (self.head + 1) % RING_BUFFER_SIZE;
        if self.count < RING_BUFFER_SIZE {
            self.count += 1;
        }
    }
    /// Get entries in chronological order (oldest first)
    pub fn dump(&self) -> Vec<RingBufferEntry> {
        if self.count == 0 {
            return Vec::new();
        }
        let mut result = Vec::with_capacity(self.count);
        if self.count < RING_BUFFER_SIZE {
            for i in 0..self.count {
                result.push(self.entries[i]);
            }
        } else {
            for i in 0..RING_BUFFER_SIZE {
                let idx = (self.head + i) % RING_BUFFER_SIZE;
                result.push(self.entries[idx]);
            }
        }
        result
    }
    /// Get the last N entries (most recent first)
    pub fn last_n(&self, n: usize) -> Vec<RingBufferEntry> {
        let n = n.min(self.count);
        if n == 0 {
            return Vec::new();
        }
        let mut result = Vec::with_capacity(n);
        for i in 0..n {
            let idx = (self.head + RING_BUFFER_SIZE - 1 - i) % RING_BUFFER_SIZE;
            result.push(self.entries[idx]);
        }
        result
    }
    /// Get the number of entries in the buffer
    pub fn len(&self) -> usize {
        self.count
    }
    /// Check if the buffer is empty
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    /// Print the last N entries for debugging
    pub fn print_last(&self, n: usize) {
        let entries = self.last_n(n);
        eprintln!("=== Execution History (last {} events) ===", entries.len());
        for (i, entry) in entries.iter().rev().enumerate() {
            let cs = entry.cs();
            let eip = entry.eip();
            let event_name = match (
                entry.event_type,
                RingInterruptOrigin::from_raw(entry.interrupt_origin),
            ) {
                (0, _) => "INSTR",
                (1, RingInterruptOrigin::SoftwareInt) => "SWINT",
                (1, RingInterruptOrigin::HardwareIrq) => "HWIRQ",
                (1, RingInterruptOrigin::CpuException) => "EXCPT",
                (1, _) => "INT?",
                (2, _) => "EXCPT",
                (3, _) => "DPMI",
                _ => "???",
            };
            let irq = entry
                .irq()
                .map(|irq| format!(" irq={irq}"))
                .unwrap_or_default();
            eprintln!(
                "[{:4}] {:04X}:{:08X} {}{} EAX={:08X} ESP={:08X} FL={:04X} extra={:08X}",
                i, cs, eip, event_name, irq, entry.eax, entry.esp, entry.flags, entry.extra
            );
        }
        eprintln!("=== End Execution History ===");
    }
}
/// Direct-mapped instruction cache size (must be power of 2)
pub const INSTR_CACHE_SIZE: usize = 16384;
/// Mask for instruction cache indexing
pub const INSTR_CACHE_MASK: usize = INSTR_CACHE_SIZE - 1;
/// Cached instruction entry
#[cfg(feature = "iced-interop")]
#[derive(Clone, Copy, Debug)]
pub struct CachedInstruction {
    /// The decoded instruction
    pub instr: Instruction,
    /// Linear address this was cached for (`usize::MAX` = invalid)
    pub addr: usize,
}
#[cfg(feature = "iced-interop")]
impl Default for CachedInstruction {
    fn default() -> Self {
        Self {
            instr: Instruction::default(),
            addr: usize::MAX,
        }
    }
}
#[cfg(feature = "iced-interop")]
impl CachedInstruction {
    /// Create a new invalid cache entry
    pub fn new() -> Self {
        Self::default()
    }
    /// Check if this cache entry is valid for the given address
    #[inline]
    pub fn is_valid_for(&self, addr: usize) -> bool {
        self.addr == addr
    }
    /// Invalidate this cache entry
    #[inline]
    pub fn invalidate(&mut self) {
        self.addr = usize::MAX;
    }
    /// Store an instruction in this cache entry
    #[inline]
    pub fn store(&mut self, addr: usize, instr: Instruction) {
        self.addr = addr;
        self.instr = instr;
    }
}
/// Estimate CPU cycles for an instruction.
///
/// Unified timing model: every instruction costs exactly
/// 1 cycle regardless of opcode. CPU speed is controlled by `cycles_per_ms`
/// (instructions per millisecond).
/// This gives the PIT a clean conversion from instruction count to emulated
/// time and makes speed presets (`--speed pentium`, etc.) behave predictably.
///
/// Takes no operand deliberately. It used to accept `&Instruction` and ignore
/// it, which forced an `iced_x86` type through every caller for a value that is
/// always 1.
#[must_use]
pub const fn estimate_cycles() -> u64 {
    1
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    #[test]
    fn test_cpu_type_default() {
        assert_eq!(CpuType::default(), CpuType::I80386);
    }
    #[test]
    fn test_cpu_type_supports() {
        assert!(!CpuType::I8086.supports_186());
        assert!(!CpuType::I8086.supports_286());
        assert!(!CpuType::I8086.supports_386());
        assert!(CpuType::I80386.supports_186());
        assert!(CpuType::I80386.supports_286());
        assert!(CpuType::I80386.supports_386());
        assert!(!CpuType::I80386.supports_486());
        assert!(CpuType::Pentium.supports_186());
        assert!(CpuType::Pentium.supports_286());
        assert!(CpuType::Pentium.supports_386());
        assert!(CpuType::Pentium.supports_486());
        assert!(CpuType::Pentium.supports_pentium());
        assert!(!CpuType::Pentium.supports_p6());
    }
    #[test]
    fn test_push_sp_behavior() {
        assert!(CpuType::I8086.push_sp_decremented());
        assert!(CpuType::I8088.push_sp_decremented());
        assert!(!CpuType::I80186.push_sp_decremented());
        assert!(!CpuType::I80386.push_sp_decremented());
    }
    #[test]
    fn test_segment_wrap() {
        assert!(CpuType::I8086.segment_wrap());
        assert!(CpuType::I8088.segment_wrap());
        assert!(CpuType::I80186.segment_wrap());
        assert!(!CpuType::I80286.segment_wrap());
        assert!(!CpuType::I80386.segment_wrap());
    }
    #[test]
    fn test_data_seg_reg() {
        let ds = DataSegReg::DS;
        let es = DataSegReg::ES;
        assert_ne!(ds, es);
        assert_eq!(ds, DataSegReg::DS);
    }
    #[test]
    fn test_page_fault_info_new() {
        let pf = PageFaultInfo::new(0x12345678, true, true, true);
        assert_eq!(pf.linear_address, 0x12345678);
        assert_eq!(pf.error_code, 0b111);
        let pf2 = PageFaultInfo::new(0xDEADBEEF, false, false, false);
        assert_eq!(pf2.linear_address, 0xDEADBEEF);
        assert_eq!(pf2.error_code, 0b000);
        let pf3 = PageFaultInfo::new(0, true, false, true);
        assert_eq!(pf3.error_code, 0b101);
    }
    #[test]
    fn test_page_fault_info_display() {
        let pf = PageFaultInfo::new(0xABCD0000, true, true, false);
        let s = format!("{}", pf);
        assert!(s.contains("ABCD0000"));
    }
    #[test]
    fn test_cpu_error_display() {
        let err = CpuError::InvalidOpcode(0x1234);
        let s = format!("{}", err);
        assert!(s.contains("Invalid opcode"));
        let err2 = CpuError::DivideByZero;
        let s2 = format!("{}", err2);
        assert!(s2.contains("Division by zero"));
    }
    #[test]
    fn test_cpu_result_type() {
        fn ok_result() -> Result<u32> {
            Ok(42)
        }
        fn err_result() -> Result<u32> {
            Err(CpuError::Halted)
        }
        assert!(ok_result().is_ok());
        assert!(err_result().is_err());
    }
    #[test]
    fn test_ring_buffer_event_type_values() {
        assert_eq!(RingBufferEventType::Instruction as u8, 0);
        assert_eq!(RingBufferEventType::Interrupt as u8, 1);
        assert_eq!(RingBufferEventType::Exception as u8, 2);
        assert_eq!(RingBufferEventType::Dpmi as u8, 3);
    }
    #[test]
    fn test_ring_buffer_entry_instruction() {
        let entry = RingBufferEntry::instruction(0x1234, 0x5678, 0xDEAD_BEEF, 0xCAFE_BABE, 0x0202);
        assert_eq!(entry.cs(), 0x1234);
        assert_eq!(entry.ip(), 0x5678);
        assert_eq!(entry.eax, 0xDEAD_BEEF);
        assert_eq!(entry.esp, 0xCAFE_BABE);
        assert_eq!(entry.flags, 0x0202);
        assert_eq!(entry.event_type, RingBufferEventType::Instruction as u8);
        assert_eq!(entry.extra, 0);
    }
    #[test]
    fn test_ring_buffer_entry_full_eip_and_provenance() {
        // Synthetic high-bit EIPs exercise full-width storage and low-word access.
        let instr = RingBufferEntry::instruction_full(
            0x0008,
            0xABCD_1234,
            0x1111_2222,
            0x3333_4444,
            0x0202,
        );
        assert_eq!(instr.cs(), 0x0008);
        assert_eq!(instr.ip(), 0x1234);
        assert_eq!(instr.eip(), 0xABCD_1234);
        assert_eq!(instr.interrupt_origin(), RingInterruptOrigin::None);
        assert_eq!(instr.irq(), None);
        let syscall = RingBufferEntry::software_interrupt(0x0008, 0xBCDE_2345, 0, 0, 0, 0x21);
        assert_eq!(syscall.eip(), 0xBCDE_2345);
        assert_eq!(syscall.interrupt_origin(), RingInterruptOrigin::SoftwareInt);
        assert_eq!(syscall.irq(), None);
        let irq0 = RingBufferEntry::hardware_irq(0x0008, 0xCDEF_3456, 0, 0, 0, 0x30, Some(0));
        assert_eq!(irq0.eip(), 0xCDEF_3456);
        assert_eq!(irq0.interrupt_origin(), RingInterruptOrigin::HardwareIrq);
        assert_eq!(irq0.irq(), Some(0));
    }
    #[test]
    fn test_ring_buffer_entry_interrupt() {
        let entry = RingBufferEntry::interrupt(0x1234, 0x5678, 0, 0, 0, 0x21);
        assert_eq!(entry.event_type, RingBufferEventType::Interrupt as u8);
        assert_eq!(entry.extra, 0x21);
    }
    #[test]
    fn test_ring_buffer_entry_exception() {
        let entry = RingBufferEntry::exception(0x1234, 0x5678, 0, 0, 0, 0x0D, 0x1234);
        assert_eq!(entry.event_type, RingBufferEventType::Exception as u8);
        assert_eq!(entry.extra >> 24, 0x0D);
        assert_eq!(entry.extra & 0xFFFFFF, 0x1234);
    }
    #[test]
    fn test_ring_buffer_entry_dpmi() {
        let entry = RingBufferEntry::dpmi(0x1234, 0x5678, 0, 0, 0, 0x0300);
        assert_eq!(entry.event_type, RingBufferEventType::Dpmi as u8);
        assert_eq!(entry.extra, 0x0300);
    }
    #[test]
    fn test_ring_buffer_empty() {
        let rb = ExecutionRingBuffer::new();
        assert!(rb.is_empty());
        assert_eq!(rb.len(), 0);
        assert!(rb.dump().is_empty());
        assert!(rb.last_n(10).is_empty());
    }
    #[test]
    fn test_ring_buffer_push_and_dump() {
        let mut rb = ExecutionRingBuffer::new();
        rb.push(RingBufferEntry::instruction(0x1000, 0x0100, 1, 0, 0));
        rb.push(RingBufferEntry::instruction(0x1000, 0x0102, 2, 0, 0));
        rb.push(RingBufferEntry::instruction(0x1000, 0x0104, 3, 0, 0));
        assert!(!rb.is_empty());
        assert_eq!(rb.len(), 3);
        let entries = rb.dump();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].eax, 1);
        assert_eq!(entries[1].eax, 2);
        assert_eq!(entries[2].eax, 3);
    }
    #[test]
    fn test_ring_buffer_last_n() {
        let mut rb = ExecutionRingBuffer::new();
        for i in 0..5 {
            rb.push(RingBufferEntry::instruction(
                0x1000, i as u16, i as u32, 0, 0,
            ));
        }
        let last2 = rb.last_n(2);
        assert_eq!(last2.len(), 2);
        assert_eq!(last2[0].eax, 4);
        assert_eq!(last2[1].eax, 3);
        let last10 = rb.last_n(10);
        assert_eq!(last10.len(), 5);
    }
    #[test]
    fn test_ring_buffer_wrap_around() {
        let mut rb = ExecutionRingBuffer::new();
        for i in 0..RING_BUFFER_SIZE + 10 {
            rb.push(RingBufferEntry::instruction(
                0x1000, i as u16, i as u32, 0, 0,
            ));
        }
        assert_eq!(rb.len(), RING_BUFFER_SIZE);
        let entries = rb.dump();
        assert_eq!(entries.len(), RING_BUFFER_SIZE);
        assert_eq!(entries[0].eax, 10);
        assert_eq!(
            entries[RING_BUFFER_SIZE - 1].eax,
            (RING_BUFFER_SIZE + 9) as u32
        );
    }
    #[test]
    fn test_cached_instruction_constants() {
        assert!(INSTR_CACHE_SIZE.is_power_of_two());
        assert_eq!(INSTR_CACHE_MASK, INSTR_CACHE_SIZE - 1);
    }
    #[test]
    fn test_cached_instruction_default() {
        let ci = CachedInstruction::default();
        assert_eq!(ci.addr, usize::MAX);
        assert!(!ci.is_valid_for(0));
        assert!(!ci.is_valid_for(0x1000));
    }
    #[test]
    fn test_cached_instruction_new() {
        let ci = CachedInstruction::new();
        assert_eq!(ci.addr, usize::MAX);
    }
    #[test]
    fn test_cached_instruction_store_and_validate() {
        let mut ci = CachedInstruction::new();
        ci.store(0x1000, Instruction::default());
        assert!(ci.is_valid_for(0x1000));
        assert!(!ci.is_valid_for(0x1001));
        assert!(!ci.is_valid_for(0));
    }
    #[test]
    fn test_cached_instruction_invalidate() {
        let mut ci = CachedInstruction::new();
        ci.store(0x1000, Instruction::default());
        assert!(ci.is_valid_for(0x1000));
        ci.invalidate();
        assert!(!ci.is_valid_for(0x1000));
        assert_eq!(ci.addr, usize::MAX);
    }
    #[test]
    fn test_cpu_type_8088_mirrors_8086() {
        assert!(!CpuType::I8088.supports_186());
        assert!(!CpuType::I8088.supports_286());
        assert!(!CpuType::I8088.supports_386());
        assert!(!CpuType::I8088.supports_486());
        assert!(!CpuType::I8088.supports_pentium());
        assert!(CpuType::I8088.push_sp_decremented());
        assert!(CpuType::I8088.segment_wrap());
    }
    #[test]
    fn test_cpu_type_80186_supports_186_only() {
        assert!(CpuType::I80186.supports_186());
        assert!(!CpuType::I80186.supports_286());
        assert!(!CpuType::I80186.supports_386());
        assert!(!CpuType::I80186.push_sp_decremented());
        assert!(CpuType::I80186.segment_wrap());
    }
    #[test]
    fn test_cpu_type_80286_no_386_support() {
        assert!(CpuType::I80286.supports_186());
        assert!(CpuType::I80286.supports_286());
        assert!(!CpuType::I80286.supports_386());
        assert!(!CpuType::I80286.supports_486());
        assert!(!CpuType::I80286.push_sp_decremented());
        assert!(!CpuType::I80286.segment_wrap());
    }
    #[test]
    fn test_cpu_type_80486() {
        assert!(CpuType::I80486.supports_186());
        assert!(CpuType::I80486.supports_286());
        assert!(CpuType::I80486.supports_386());
        assert!(CpuType::I80486.supports_486());
        assert!(!CpuType::I80486.supports_pentium());
        assert!(!CpuType::I80486.push_sp_decremented());
        assert!(!CpuType::I80486.segment_wrap());
    }
    #[test]
    fn test_cpu_type_debug_display() {
        let dbg = format!("{:?}", CpuType::Pentium);
        assert!(dbg.contains("Pentium"));
        let dbg2 = format!("{:?}", CpuType::I80386);
        assert!(dbg2.contains("I80386"));
    }
    #[test]
    fn test_page_fault_info_write_only() {
        let pf = PageFaultInfo::new(0, false, true, false);
        assert_eq!(pf.error_code, 0b010);
    }
    #[test]
    fn test_page_fault_info_user_only() {
        let pf = PageFaultInfo::new(0, false, false, true);
        assert_eq!(pf.error_code, 0b100);
    }
    #[test]
    fn test_page_fault_info_present_only() {
        let pf = PageFaultInfo::new(0, true, false, false);
        assert_eq!(pf.error_code, 0b001);
    }
    #[test]
    fn test_page_fault_info_display_zero() {
        let pf = PageFaultInfo::new(0, false, false, false);
        let s = format!("{pf}");
        assert!(s.contains("0x00000000"));
    }
    #[test]
    fn test_cpu_error_display_all_variants() {
        let cases: Vec<(CpuError, &str)> = vec![
            (
                CpuError::InvalidOpcode(0xABCD),
                "Invalid opcode at 0xABCD",
            ),
            (
                CpuError::Unimplemented("FOO".into()),
                "Unimplemented instruction: FOO",
            ),
            (CpuError::DivideByZero, "Division by zero"),
            (CpuError::GeneralProtectionFault, "General protection fault"),
            (
                CpuError::SegmentNotPresent(0x1234),
                "Segment not present: selector 0x1234",
            ),
            (CpuError::StackFault, "Stack segment fault"),
            (
                CpuError::PageFault(PageFaultInfo::new(0x1000, true, false, false)),
                "Page fault: linear=0x00001000 error=0x00000001",
            ),
            (CpuError::Halted, "Halt instruction executed"),
            (CpuError::Interrupt(0x0D), "CPU exception: vector 0xD"),
            (
                CpuError::SoftwareInterrupt(0x21),
                "Software interrupt: INT 0x21",
            ),
            (
                CpuError::FpuStub(2),
                "FPU instruction stub (skipped 2 bytes)",
            ),
            (
                CpuError::TripleFault(TripleFaultInfo {
                    origin: TripleFaultOrigin::SoftwareInterrupt,
                    vector: 3,
                    irq: None,
                    error_code: None,
                    escalation_vector: 8,
                    cs: 0x0008,
                    eip: 0x0000_1234,
                    idtr_base: 0,
                    idtr_limit: 0,
                    cr0: 1,
                    cr3: 0,
                    cpl: 0,
                    failed_delivery: "GeneralProtectionFault".into(),
                }),
                "triple fault: software interrupt INT 03 escalated to #DF vector 08, #DF delivery failed with GeneralProtectionFault; cs:eip=0008:00001234 idtr=00000000:0000 cr0=00000001 cr3=00000000 cpl=0",
            ),
            (CpuError::Diagnostic("test".into()), "Diagnostic: test"),
            (
                CpuError::V86SwitchBlocked(V86SwitchFrame {
                    eip: 0,
                    cs: 0,
                    eflags: 0,
                    esp: 0,
                    ss: 0,
                    es: 0,
                    ds: 0,
                    fs: 0,
                    gs: 0,
                }),
                "V86 switch blocked at CPL>0",
            ),
            (
                CpuError::TaskSwitchNeeded,
                "Task switch needed (IRET with NT=1)",
            ),
            (
                CpuError::TssDescriptor(0x0028),
                "TSS descriptor encountered (selector 0x0028)",
            ),
            (
                CpuError::TaskSwitchJmp(0x0028),
                "TSS task switch via JMP (selector 0x0028)",
            ),
            (
                CpuError::TaskSwitchCall(0x0028),
                "TSS task switch via CALL (selector 0x0028)",
            ),
            (
                CpuError::CallGate(0x0030),
                "Call gate descriptor (selector 0x0030)",
            ),
            (
                CpuError::CallGateTransfer {
                    selector: 0x0030,
                    return_cs: 0x0008,
                    return_eip: 0x1234,
                },
                "Call gate descriptor (selector 0x0030)",
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(format!("{err}"), expected);
            assert_eq!(format!("{:>80.3}", err), expected);
            assert!(std::error::Error::source(&err).is_none());
        }
    }
    #[test]
    fn test_v86_switch_frame_debug() {
        let frame = V86SwitchFrame {
            eip: 0x100,
            cs: 0x1000,
            eflags: 0x20202,
            esp: 0xFFF0,
            ss: 0x2000,
            es: 0x3000,
            ds: 0x4000,
            fs: 0x5000,
            gs: 0x6000,
        };
        let dbg = format!("{frame:?}");
        assert!(dbg.contains("V86SwitchFrame"));
        assert!(dbg.contains("eip"));
    }
    #[test]
    fn test_v86_switch_frame_clone() {
        let frame = V86SwitchFrame {
            eip: 0x100,
            cs: 0x1000,
            eflags: 0,
            esp: 0,
            ss: 0,
            es: 0,
            ds: 0,
            fs: 0,
            gs: 0,
        };
        let cloned = frame.clone();
        assert_eq!(cloned.eip, 0x100);
        assert_eq!(cloned.cs, 0x1000);
    }
    #[test]
    fn test_data_seg_reg_all_variants() {
        let variants = [
            DataSegReg::DS,
            DataSegReg::ES,
            DataSegReg::FS,
            DataSegReg::GS,
        ];
        for (i, v) in variants.iter().enumerate() {
            for (j, w) in variants.iter().enumerate() {
                if i == j {
                    assert_eq!(v, w);
                } else {
                    assert_ne!(v, w);
                }
            }
        }
    }
    #[test]
    fn test_data_seg_reg_debug() {
        let dbg = format!("{:?}", DataSegReg::FS);
        assert!(dbg.contains("FS"));
    }
    #[test]
    fn test_ring_buffer_event_type_clone_debug_eq() {
        let a = RingBufferEventType::Instruction;
        let b = a;
        assert_eq!(a, b);
        let dbg = format!("{a:?}");
        assert!(dbg.contains("Instruction"));
        assert_ne!(RingBufferEventType::Instruction, RingBufferEventType::Dpmi);
    }
    #[test]
    fn test_ring_buffer_print_last() {
        let mut rb = ExecutionRingBuffer::new();
        rb.push(RingBufferEntry::instruction(
            0x1000, 0x100, 1, 0x1000, 0x202,
        ));
        rb.push(RingBufferEntry::interrupt(
            0x1000, 0x102, 2, 0x1000, 0x202, 0x21,
        ));
        rb.push(RingBufferEntry::exception(
            0x1000, 0x104, 3, 0x1000, 0x202, 0x0D, 0,
        ));
        rb.push(RingBufferEntry::dpmi(
            0x1000, 0x106, 4, 0x1000, 0x202, 0x0300,
        ));
        let mut entry = RingBufferEntry::instruction(0x1000, 0x108, 5, 0x1000, 0x202);
        entry.event_type = 99;
        rb.push(entry);
        rb.print_last(5);
    }
    #[test]
    fn test_ring_buffer_default() {
        let rb = ExecutionRingBuffer::default();
        assert!(rb.is_empty());
        assert_eq!(rb.len(), 0);
    }
    #[test]
    fn test_ring_buffer_entry_default_and_debug() {
        let entry = RingBufferEntry::default();
        assert_eq!(entry.cs_ip, 0);
        assert_eq!(entry.eax, 0);
        assert_eq!(entry.irq(), None);
        assert_eq!(entry.interrupt_origin(), RingInterruptOrigin::None);
        let dbg = format!("{entry:?}");
        assert!(dbg.contains("RingBufferEntry"));
    }
    #[test]
    fn test_cached_instruction_debug() {
        let ci = CachedInstruction::new();
        let dbg = format!("{ci:?}");
        assert!(dbg.contains("CachedInstruction"));
    }
    /// The unified timing model: every instruction costs exactly one cycle.
    ///
    /// This used to decode eighteen instruction encodings and assert each cost
    /// one cycle. Once `estimate_cycles` stopped taking an operand — it had always
    /// ignored it — that loop was asserting `1 == 1` eighteen times with the
    /// decoded instruction unused, so it is replaced by the claim it was
    /// actually making. Guest speed is set by `cycles_per_ms`, not per-opcode
    /// costs; if that ever changes, this is the test that should fail first.
    #[test]
    fn every_instruction_costs_exactly_one_cycle() {
        assert_eq!(estimate_cycles(), 1);
    }
    #[test]
    fn test_execution_ring_buffer_default() {
        let rb = ExecutionRingBuffer::default();
        assert!(rb.is_empty());
        assert_eq!(rb.len(), 0);
    }
    #[test]
    fn test_ring_buffer_print_last_v2() {
        let mut rb = ExecutionRingBuffer::new();
        rb.push(RingBufferEntry::instruction(
            0x1000, 0x0100, 1, 0x1000, 0x0202,
        ));
        rb.push(RingBufferEntry::interrupt(
            0x1000, 0x0102, 2, 0x0FFE, 0x0202, 0x21,
        ));
        rb.push(RingBufferEntry::exception(
            0x1000, 0x0104, 3, 0x0FFC, 0x0202, 0x0D, 0,
        ));
        rb.push(RingBufferEntry::dpmi(
            0x1000, 0x0106, 4, 0x0FFA, 0x0202, 0x0300,
        ));
        rb.push(RingBufferEntry {
            cs_ip: 0x1000_0108,
            eax: 5,
            esp: 0x0FF8,
            flags: 0x0202,
            event_type: 99,
            extra: 0,
            ..Default::default()
        });
        rb.print_last(5);
    }
    #[test]
    fn test_kbd_flow_trace_enabled_returns_false_without_subscriber() {
        assert!(!kbd_flow_trace_enabled());
    }
    #[test]
    fn test_v86_switch_frame_fields() {
        let frame = V86SwitchFrame {
            eip: 0x1000,
            cs: 0x0050,
            eflags: 0x0002_0202,
            esp: 0x2000,
            ss: 0x0060,
            es: 0x0070,
            ds: 0x0080,
            fs: 0x0090,
            gs: 0x00A0,
        };
        assert_eq!(frame.eip, 0x1000);
        assert_eq!(frame.cs, 0x0050);
        assert_eq!(frame.eflags, 0x0002_0202);
        assert_eq!(frame.esp, 0x2000);
        assert_eq!(frame.ss, 0x0060);
        assert_eq!(frame.es, 0x0070);
        assert_eq!(frame.ds, 0x0080);
        assert_eq!(frame.fs, 0x0090);
        assert_eq!(frame.gs, 0x00A0);
    }
    #[test]
    fn test_cpu_error_display_all_variants_v2() {
        let cases: Vec<(CpuError, &str)> = vec![
            (CpuError::InvalidOpcode(0x1234), "Invalid opcode"),
            (CpuError::Unimplemented("test".into()), "Unimplemented"),
            (CpuError::DivideByZero, "Division by zero"),
            (CpuError::GeneralProtectionFault, "General protection fault"),
            (CpuError::SegmentNotPresent(0x0010), "Segment not present"),
            (CpuError::StackFault, "Stack segment fault"),
            (
                CpuError::PageFault(PageFaultInfo::new(0, false, false, false)),
                "Page fault",
            ),
            (CpuError::Halted, "Halt"),
            (CpuError::Interrupt(0x0D), "CPU exception"),
            (CpuError::SoftwareInterrupt(0x21), "Software interrupt"),
            (CpuError::FpuStub(2), "FPU instruction stub"),
            (
                CpuError::TripleFault(TripleFaultInfo {
                    origin: TripleFaultOrigin::HardwareIrq,
                    vector: 0x09,
                    irq: Some(1),
                    error_code: None,
                    escalation_vector: 8,
                    cs: 0x0008,
                    eip: 0x0000_1234,
                    idtr_base: 0,
                    idtr_limit: 0,
                    cr0: 1,
                    cr3: 0,
                    cpl: 0,
                    failed_delivery: "GeneralProtectionFault".into(),
                }),
                "triple fault",
            ),
            (CpuError::Diagnostic("test".into()), "Diagnostic"),
            (
                CpuError::V86SwitchBlocked(V86SwitchFrame {
                    eip: 0,
                    cs: 0,
                    eflags: 0,
                    esp: 0,
                    ss: 0,
                    es: 0,
                    ds: 0,
                    fs: 0,
                    gs: 0,
                }),
                "V86 switch blocked",
            ),
            (CpuError::TaskSwitchNeeded, "Task switch"),
            (CpuError::TssDescriptor(0x0028), "TSS descriptor"),
            (CpuError::TaskSwitchJmp(0x0028), "TSS task switch via JMP"),
            (CpuError::TaskSwitchCall(0x0028), "TSS task switch via CALL"),
            (CpuError::CallGate(0x0030), "Call gate"),
            (
                CpuError::CallGateTransfer {
                    selector: 0x0030,
                    return_cs: 0x0008,
                    return_eip: 0x1000,
                },
                "Call gate",
            ),
        ];
        for (err, expected_substr) in cases {
            let s = format!("{}", err);
            assert!(
                s.contains(expected_substr),
                "Expected '{}' to contain '{}'",
                s,
                expected_substr
            );
        }
    }
    #[test]
    fn test_page_fault_info_all_combinations() {
        for &present in &[false, true] {
            for &write in &[false, true] {
                for &user in &[false, true] {
                    let pf = PageFaultInfo::new(0x1000, present, write, user);
                    assert_eq!((pf.error_code & 1) != 0, present);
                    assert_eq!((pf.error_code & 2) != 0, write);
                    assert_eq!((pf.error_code & 4) != 0, user);
                }
            }
        }
    }
    #[test]
    fn test_data_seg_reg_all_variants_v2() {
        let variants = [
            DataSegReg::DS,
            DataSegReg::ES,
            DataSegReg::FS,
            DataSegReg::GS,
        ];
        for i in 0..variants.len() {
            for j in (i + 1)..variants.len() {
                assert_ne!(variants[i], variants[j]);
            }
            let cloned = variants[i];
            assert_eq!(variants[i], cloned);
        }
    }
    #[test]
    fn test_cpu_type_186_support() {
        assert!(CpuType::I80186.supports_186());
        assert!(!CpuType::I80186.supports_286());
        assert!(!CpuType::I80186.supports_386());
        assert!(!CpuType::I8088.supports_186());
        assert!(CpuType::I8088.push_sp_decremented());
        assert!(CpuType::I8088.segment_wrap());
        assert!(CpuType::I80286.supports_186());
        assert!(CpuType::I80286.supports_286());
        assert!(!CpuType::I80286.supports_386());
        assert!(CpuType::I80486.supports_186());
        assert!(CpuType::I80486.supports_486());
        assert!(!CpuType::I80486.supports_pentium());
        assert!(!CpuType::I80486.push_sp_decremented());
        assert!(!CpuType::I80486.segment_wrap());
    }
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod pentium_mmx_tests {
    use super::{
        CpuType, CR4_DE, CR4_MCE, CR4_OSFXSR, CR4_OSXMMEXCPT, CR4_PAE, CR4_PCE, CR4_PGE, CR4_PSE,
        CR4_PVI, CR4_TSD, CR4_VME,
    };
    /// The predicates are exhaustive matches precisely so a new variant cannot
    /// default to "unsupported everywhere"; this pins what P55C actually is.
    #[test]
    fn pentium_mmx_capabilities() {
        let cpu = CpuType::PentiumMmx;
        assert!(cpu.supports_186());
        assert!(cpu.supports_286());
        assert!(cpu.supports_386(), "must not regress to sub-386");
        assert!(cpu.supports_486(), "CPUID is gated on this");
        assert!(cpu.supports_pentium(), "MMX execution is gated on this");
        assert!(cpu.supports_mmx());
        assert!(!cpu.supports_p6(), "P55C predates CMOVcc");
        assert!(!cpu.supports_sse3());
    }
    /// P54C is the same machine without MMX — the only difference.
    #[test]
    fn plain_pentium_does_not_advertise_mmx() {
        assert!(!CpuType::Pentium.supports_mmx());
        assert!(CpuType::Pentium.supports_pentium());
    }
    /// Pentium Pro: P6 WITHOUT MMX.
    ///
    /// This assertion *is* the non-monotonic-capability claim the whole
    /// two-identity design rests on. Real Pentium Pro silicon shipped without
    /// MMX while the P55C shipped with it and without CMOVcc, so neither part
    /// is a superset of the other. If someone "tidies" the enum on the
    /// assumption that later means more capable, this is what should stop
    /// them.
    #[test]
    fn pentium_pro_capabilities() {
        let cpu = CpuType::PentiumPro;
        assert!(cpu.supports_386());
        assert!(cpu.supports_486(), "CPUID is gated on this");
        assert!(cpu.supports_pentium());
        assert!(cpu.supports_p6(), "CMOVcc, FCMOVcc/FCOMI and 0F 1F /0");
        assert!(
            ! cpu.supports_mmx(),
            "real Pentium Pro has NO MMX — this is the point of having a              separate PentiumII identity, not an oversight"
        );
        assert!(cpu.supports_rdpmc());
        assert!(!cpu.supports_sse3());
    }
    /// Pentium II: the only identity that is a strict superset of P55C.
    #[test]
    fn pentium_ii_capabilities() {
        let cpu = CpuType::PentiumII;
        assert!(cpu.supports_p6());
        assert!(cpu.supports_mmx());
        assert!(cpu.supports_rdpmc());
        assert!(!cpu.supports_sse3());
    }
    #[test]
    fn pentium_iii_and_pentium_4_sse_capabilities() {
        let p3 = CpuType::PentiumIII;
        assert!(p3.supports_p6());
        assert!(p3.supports_mmx());
        assert!(p3.supports_fxsave());
        assert!(p3.supports_sse());
        assert!(!p3.supports_sse2());
        assert!(!p3.supports_sse3());
        let p4 = CpuType::Pentium4;
        assert!(p4.supports_p6());
        assert!(p4.supports_mmx());
        assert!(p4.supports_fxsave());
        assert!(p4.supports_sse());
        assert!(p4.supports_sse2());
        assert!(!p4.supports_sse3());
    }
    /// MDD-BUG-KIL-00765: CR4 accepts only what each identity has and this
    /// emulator implements. PAE in particular must not be accepted while the
    /// page walk only knows the two-level 32-bit table format.
    #[test]
    fn cr4_accepts_only_implemented_bits_per_identity() {
        let unimplemented = CR4_VME | CR4_PVI | CR4_PAE | CR4_PGE;
        let family5 = CR4_TSD | CR4_DE | CR4_PSE | CR4_MCE;
        for (cpu, expected) in [
            (CpuType::I8086, 0),
            (CpuType::I80286, 0),
            (CpuType::I80386, 0),
            (CpuType::I80486, 0),
            (CpuType::Pentium, family5),
            (CpuType::PentiumMmx, family5 | CR4_PCE),
            (CpuType::PentiumPro, family5 | CR4_PCE),
            (CpuType::PentiumII, family5 | CR4_PCE),
            (
                CpuType::PentiumIII,
                family5 | CR4_PCE | CR4_OSFXSR | CR4_OSXMMEXCPT,
            ),
            (
                CpuType::Pentium4,
                family5 | CR4_PCE | CR4_OSFXSR | CR4_OSXMMEXCPT,
            ),
        ] {
            let mask = cpu.cr4_valid_mask();
            assert_eq!(mask, expected, "{cpu:?}");
            assert_eq!(
                mask & unimplemented,
                0,
                "{cpu:?} accepts an unimplemented bit"
            );
        }
    }
    #[test]
    fn sse_cr4_bits_are_identity_gated() {
        assert_eq!(
            CpuType::PentiumII.cr4_valid_mask() & (CR4_OSFXSR | CR4_OSXMMEXCPT),
            0
        );
        assert_eq!(
            CpuType::PentiumIII.cr4_valid_mask() & (CR4_OSFXSR | CR4_OSXMMEXCPT),
            CR4_OSFXSR | CR4_OSXMMEXCPT
        );
        assert_eq!(
            CpuType::Pentium4.cr4_valid_mask() & (CR4_OSFXSR | CR4_OSXMMEXCPT),
            CR4_OSFXSR | CR4_OSXMMEXCPT
        );
    }
    /// The capability lattice is genuinely non-monotonic, stated as a property
    /// rather than left implicit in the two tests above.
    #[test]
    fn neither_pentium_mmx_nor_pentium_pro_is_a_superset_of_the_other() {
        assert!(CpuType::PentiumMmx.supports_mmx() && !CpuType::PentiumMmx.supports_p6());
        assert!(CpuType::PentiumPro.supports_p6() && !CpuType::PentiumPro.supports_mmx());
    }
    /// RDPMC is NOT P6-only: the SDM puts it on the Pentium Pro *and* the
    /// Pentium with MMX. Gating it on `supports_p6()` would make us wrong
    /// about a part we already model.
    #[test]
    fn rdpmc_predicate_covers_p55c_as_well_as_p6() {
        assert!(CpuType::PentiumMmx.supports_rdpmc(), "P55C has RDPMC");
        assert!(CpuType::PentiumPro.supports_rdpmc());
        assert!(CpuType::PentiumII.supports_rdpmc());
        assert!(
            !CpuType::Pentium.supports_rdpmc(),
            "P54C reads counters via RDMSR"
        );
        assert!(!CpuType::I80486.supports_rdpmc());
    }
    /// `CpuType::ALL` must actually contain every variant.
    ///
    /// The uniqueness loop is the load-bearing half: `assert_total` forces an arm
    /// per variant and the `[CpuType; 10]` literal length is checked by the
    /// compiler, but neither catches the same variant being listed twice —
    /// which would silently shrink coverage for everything driven off `ALL`.
    /// (The `len()` assertion below is a type-level tautology, kept only as
    /// documentation of the expected count.)
    #[test]
    fn cpu_type_all_is_complete_and_unique() {
        assert_eq!(CpuType::ALL.len(), 13);
        for (i, a) in CpuType::ALL.iter().enumerate() {
            for b in &CpuType::ALL[i + 1..] {
                assert_ne!(a, b, "duplicate entry in CpuType::ALL");
            }
        }
        assert!(CpuType::ALL.contains(&CpuType::I8086));
        assert!(CpuType::ALL.contains(&CpuType::PentiumII));
    }
    /// Exactly the P6-capable identities, driven off the totality const so a
    /// future variant cannot silently join or miss the set.
    #[test]
    fn exactly_two_identities_are_p6_capable() {
        let p6: Vec<_> = CpuType::ALL
            .iter()
            .copied()
            .filter(CpuType::supports_p6)
            .collect();
        assert_eq!(
            p6,
            vec![
                CpuType::PentiumPro,
                CpuType::PentiumII,
                CpuType::PentiumIII,
                CpuType::Pentium4,
                CpuType::PentiumIIICoppermine
            ]
        );
    }
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod sse_iced_free_guard {
    /// The SSE path must carry no `iced_x86` type or call in production code.
    ///
    /// Deliberately a SOURCE scan via `include_str!` rather than a build
    /// configuration: iced is still a genuine dependency of this crate (the
    /// x87 path, `addressing.rs`'s iced overloads, and the frozen
    /// `sse_exec_reference` oracle all use it), so nothing about the build
    /// would fail if a caller reached for it again here.
    ///
    /// Each file is scanned only as far as its `#[cfg(test)]` module marker:
    /// the decode differential and the migration oracle keep iced as their
    /// test-only ground truth, which is the point of retaining it.
    /// `sse_exec_reference.rs` is exempt in whole — the entire file is the
    /// frozen pre-port executor and is compiled only under `cfg(test)`.
    #[test]
    fn the_sse_production_path_names_no_iced_type() {
        const FILES: [(&str, &str); 6] = [
            ("sse_decode.rs", include_str!("sse_decode.rs")),
            ("sse_decode_cache.rs", include_str!("sse_decode_cache.rs")),
            ("sse_exec.rs", include_str!("sse_exec.rs")),
            ("sse_host_common.rs", include_str!("sse_host_common.rs")),
            ("sse_host_kernels.rs", include_str!("sse_host_kernels.rs")),
            (
                "sse_host_kernels_a64.rs",
                include_str!("sse_host_kernels_a64.rs"),
            ),
        ];
        const EXEMPT: [(&str, &str); 2] = [
            (
                "sse.rs",
                "register-file state only; no decode or execute logic",
            ),
            (
                "sse_exec_reference.rs",
                "the frozen pre-port oracle, compiled only under cfg(test) and \
                 exempt by the guard's own charter above",
            ),
        ];
        let mut unlisted: Vec<String> =
            std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))
                .expect("crate src dir")
                .filter_map(|entry| {
                    let name = entry.expect("dir entry").file_name();
                    let name = name.to_string_lossy().into_owned();
                    (name.starts_with("sse") && name.ends_with(".rs")).then_some(name)
                })
                .filter(|name| {
                    !FILES.iter().any(|(listed, _)| listed == name)
                        && !EXEMPT.iter().any(|(listed, _)| listed == name)
                })
                .collect();
        unlisted.sort();
        assert!(
            unlisted.is_empty(),
            "new SSE source files are outside this guard's scan — list them in \
             FILES (or EXEMPT, with a reason): {unlisted:?}"
        );
        for (name, source) in FILES {
            let marker_lines: Vec<usize> = source
                .lines()
                .enumerate()
                .filter(|(_, line)| *line == "#[cfg(test)]")
                .map(|(index, _)| index)
                .collect();
            assert_eq!(
                marker_lines.len(),
                1,
                "{name}: this guard assumes exactly one top-level \
                 `#[cfg(test)]` marker and scans everything before it — \
                 re-aim it, never delete it"
            );
            let offenders: Vec<(usize, &str)> = source
                .lines()
                .take(marker_lines[0])
                .enumerate()
                .filter(|(_, line)| line.contains("iced_x86"))
                .map(|(index, line)| (index + 1, line.trim()))
                .collect();
            assert!(
                offenders.is_empty(),
                "{name}: the production SSE path must be iced-free, but it \
                 names `iced_x86` at {offenders:?}. Decode with \
                 `sse_decode::decode`, dispatch on `SseOp`; iced stays a \
                 test-only oracle."
            );
        }
    }
}
#[cfg(test)]
mod coppermine_identity_tests {
    use super::*;
    use crate::cpu::{cpuid_features, cpuid_leaf2, cpuid_max_basic_leaf, cpuid_signature};
    #[test]
    fn coppermine_identity_and_implemented_features() {
        let identity = CpuType::PentiumIIICoppermine;
        assert_eq!(cpuid_signature(identity), 0x68A);
        assert_eq!(cpuid_max_basic_leaf(identity), 2);
        assert_eq!(cpuid_leaf2(identity), (0x03020101, 0, 0, 0x0C040841));
        assert_eq!(cpuid_features(identity), 0x0381_B9BD);
        assert!(identity.supports_sse());
        assert!(!identity.supports_sse2());
        assert_eq!(identity.cr4_valid_mask() & CR4_PGE, CR4_PGE);
        for old in CpuType::ALL {
            if old == identity {
                continue;
            }
            assert_eq!(cpuid_max_basic_leaf(old), 1);
            assert_eq!(cpuid_leaf2(old), (0, 0, 0, 0));
            assert_eq!(
                cpuid_features(old) & ((1 << 11) | (1 << 12) | (1 << 13) | (1 << 16)),
                0
            );
        }
    }
    #[test]
    fn coppermine_machine_clock_is_independent_and_continuous() {
        let mut cpu = Cpu::new();
        cpu.set_cpu_type(CpuType::PentiumIIICoppermine);
        cpu.set_tsc_clock_hz(733_333_333, 100_000);
        cpu.cycles += 100_000_000;
        let before = cpu.tsc_from_arch_cycles(cpu.cycles);
        assert!(before.abs_diff(733_333_333) <= 1);
        cpu.set_tsc_rate(200_000);
        assert_eq!(cpu.tsc_from_arch_cycles(cpu.cycles), before);
        cpu.cycles += 200_000_000;
        assert!(
            cpu.tsc_from_arch_cycles(cpu.cycles)
                .abs_diff(before + 733_333_333)
                <= 1
        );
        let mut restored = Cpu::new();
        restored.restore_state(&cpu.snapshot_state()).unwrap();
        assert_eq!(restored.tsc_clock_hz, Some(733_333_333));
        let restored_before = restored.tsc_from_arch_cycles(restored.cycles);
        restored.set_tsc_rate(100_000);
        restored.cycles += 100_000_000;
        assert!(
            restored
                .tsc_from_arch_cycles(restored.cycles)
                .abs_diff(restored_before + 733_333_333)
                <= 1
        );
        restored.reset();
        assert_eq!(restored.tsc_clock_hz, Some(733_333_333));
        restored.set_tsc_rate(100_000);
        restored.cycles = 100_000_000;
        assert!(
            restored
                .tsc_from_arch_cycles(restored.cycles)
                .abs_diff(733_333_333)
                <= 1
        );
        restored.set_reset_vector_state();
        assert_eq!(restored.tsc_clock_hz, Some(733_333_333));
        assert_eq!(Cpu::new().tsc_clock_hz, None);
    }
    /// A machine whose TSC clock
    /// equals its cycle clock reads RDTSC == cycles + offset exactly, across
    /// speed changes, reset and snapshot; the integer `cycles_per_ms` path
    /// drifts by 1 + 4.5e-7.
    #[test]
    fn exact_tsc_ratio_ticks_once_per_cycle() {
        let mut cpu = Cpu::new();
        cpu.set_cpu_type(CpuType::PentiumIIICoppermine);
        cpu.cycles = 1_000;
        cpu.set_tsc_clock_hz(733_333_333, 733_333);
        let truncated = cpu.tsc_from_arch_cycles(cpu.cycles + 1_000_000_000);
        cpu.set_tsc_exact_ratio(733_333_333, 733_333_333);
        let base = cpu.tsc_from_arch_cycles(cpu.cycles);
        let far = cpu.cycles + 1_000_000_000;
        assert_eq!(cpu.tsc_from_arch_cycles(far), base + 1_000_000_000);
        assert_ne!(truncated, base + 1_000_000_000, "the inexact path drifts");
        cpu.set_tsc_rate(100_000);
        assert_eq!(cpu.tsc_from_arch_cycles(far), base + 1_000_000_000);
        let mut restored = Cpu::new();
        restored.restore_state(&cpu.snapshot_state()).unwrap();
        assert_eq!(restored.tsc_from_arch_cycles(far), base + 1_000_000_000);
        restored.reset();
        restored.set_tsc_rate(100_000);
        restored.write_tsc_at(0, 7);
        assert_eq!(restored.tsc_from_arch_cycles(5_000_000_000), 5_000_000_007);
        assert_eq!(Cpu::new().tsc_cycle_hz, None);
    }
}
