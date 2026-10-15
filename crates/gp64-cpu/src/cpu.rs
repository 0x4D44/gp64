//! CPU state and core operations
//!
//! This module defines the `Cpu` struct which holds all CPU state
//! including registers, FPU, MMX, and provides methods for instruction execution.
use crate::dpmi_diag::DpmiExceptionDiagState;
use crate::flags::Flags;
use crate::fpu::Fpu;
use crate::registers::{DescriptorTableRegister, Registers, SegmentCache};
use crate::sse::SseState;
use crate::sse_decode_cache::SseInstrCache;
#[cfg(feature = "iced-interop")]
use crate::{CachedInstruction, INSTR_CACHE_SIZE};
use crate::{CpuType, ExecutionRingBuffer};
use gp64_types::FixedVec;
#[cfg(test)]
use std::collections::HashMap;
use std::fmt;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU32, Ordering};
/// Saves the incoming host x87 control word and restores it on drop.
///
/// The host x86-64 calling conventions preserve the x87 control word across
/// calls. Helpers load a sanitized or guest-derived word with `FLDCW`, then
/// restore the caller's control word so guest precision and rounding modes
/// do not affect later host floating-point operations on the same thread.
///
/// Restoring only the control word is deliberate: `restore_host_x87_condition_codes`
/// loads a whole guest environment on purpose, and putting the old *environment*
/// back would erase the condition-code update it exists to make.
///
/// `Drop` performs one `FLDCW` and cannot panic.
#[doc(hidden)]
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
pub struct HostX87Guard {
    #[cfg(target_arch = "x86_64")]
    saved_cw: u16,
}
/// Capture the caller's x87 control word so it can be restored on exit.
///
/// The inventory oracle in `x87_ops.rs` (`every_x87_asm_site_enters_host_guard`) reads every
/// `.rs` under the crate's `src/` and `benches/` — no hand-written file list —
/// and fails any inline-asm block that is neither preceded by this call nor
/// marked `x87-inventory-exempt` with its reason, so neither a new site nor a
/// new file can silently reintroduce the leak.
#[doc(hidden)]
#[inline(always)]
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
pub fn host_x87_enter_guarded() -> HostX87Guard {
    #[cfg(target_arch = "x86_64")]
    {
        let mut saved_cw: u16 = 0;
        unsafe {
            // x87-inventory-exempt: this FNSTCW is the guard's own control-word capture; arming the guard here would recurse.
            core::arch::asm!(
                "fnstcw word ptr [{cw}]", cw = in (reg) & mut saved_cw, options(nostack),
            );
        }
        HostX87Guard { saved_cw }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        HostX87Guard {}
    }
}
impl Drop for HostX87Guard {
    #[inline(always)]
    fn drop(&mut self) {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            // x87-inventory-exempt: this FLDCW is the guard's own restore; a second guard inside Drop would recurse.
            core::arch::asm!(
                "fldcw word ptr [{cw}]", cw = in (reg) & self.saved_cw, options(nostack),
            );
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetGuardKind {
    Near,
    Far,
    Iret,
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod ret_guard_tests {
    use super::{Cpu, RetGuardEntry, RetGuardKind};
    #[test]
    fn test_ret_guard_push_and_pop() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(true);
        let mut cpu = Cpu::new();
        cpu.ret_guard_push(RetGuardEntry {
            kind: RetGuardKind::Near,
            cs: 0x1234,
            ip: 0x0100,
            ss: 0x2000,
            sp: 0xFFF0,
            tag: 0,
            caller_cs: 0x1234,
            caller_ip: 0x00F0,
        });
        cpu.ret_guard_push(RetGuardEntry {
            kind: RetGuardKind::Far,
            cs: 0x4321,
            ip: 0x0200,
            ss: 0x2000,
            sp: 0xFFEC,
            tag: 0,
            caller_cs: 0x4321,
            caller_ip: 0x00F4,
        });
        let entry = cpu.ret_guard_pop(RetGuardKind::Far).expect("entry");
        assert_eq!(entry.kind, RetGuardKind::Far);
        let entry = cpu.ret_guard_pop(RetGuardKind::Near).expect("entry");
        assert_eq!(entry.kind, RetGuardKind::Near);
        Cpu::set_ret_guard_for_tests(false);
    }
    #[test]
    fn test_ret_guard_pop_mismatch_noops() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(true);
        let mut cpu = Cpu::new();
        cpu.ret_guard_push(RetGuardEntry {
            kind: RetGuardKind::Near,
            cs: 0x1234,
            ip: 0x0100,
            ss: 0x2000,
            sp: 0xFFF0,
            tag: 0,
            caller_cs: 0x1234,
            caller_ip: 0x00F0,
        });
        assert!(cpu.ret_guard_pop(RetGuardKind::Far).is_none());
        let entry = cpu.ret_guard_pop(RetGuardKind::Near).expect("entry");
        assert_eq!(entry.kind, RetGuardKind::Near);
        Cpu::set_ret_guard_for_tests(false);
    }
}
#[derive(Clone, Copy, Debug)]
pub struct RetGuardEntry {
    pub kind: RetGuardKind,
    pub cs: u16,
    pub ip: u32,
    pub ss: u16,
    pub sp: u32,
    pub tag: u16,
    pub caller_cs: u16,
    pub caller_ip: u16,
}
#[cfg(any(test, feature = "test-util"))]
static RET_GUARD_TEST_ENABLED: AtomicBool = AtomicBool::new(false);
static RET_GUARD_LOG_COUNT: AtomicU32 = AtomicU32::new(0);
static RET_GUARD_DUMP_COUNT: AtomicU32 = AtomicU32::new(0);
pub fn ret_guard_enabled() -> bool {
    #[cfg(any(test, feature = "test-util"))]
    {
        RET_GUARD_TEST_ENABLED.load(Ordering::Relaxed)
    }
    #[cfg(not(any(test, feature = "test-util")))]
    {
        false
    }
}
fn ret_guard_max_depth() -> usize {
    1024
}
fn ret_guard_log_max() -> u32 {
    200
}
pub fn ret_guard_should_log() -> bool {
    if !ret_guard_enabled() {
        return false;
    }
    let max = ret_guard_log_max();
    let count = RET_GUARD_LOG_COUNT.fetch_add(1, Ordering::Relaxed);
    count < max
}
pub fn ret_guard_write_dump(text: &str) {
    if !ret_guard_enabled() {
        return;
    }
    let idx = RET_GUARD_DUMP_COUNT.fetch_add(1, Ordering::Relaxed);
    let _ = std::fs::create_dir_all("tmp");
    let path = format!("tmp/ret_guard_dump_{idx}.txt");
    let _ = std::fs::write(path, text);
}
/// Architectural invariant violation in a serialized CPU snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuSnapshotValidationError {
    CplOutOfRange {
        cpl: u8,
    },
    V86CplMismatch {
        cpl: u8,
    },
    V86WithoutProtectedMode,
    CsRplCplMismatch {
        cs: u16,
        cpl: u8,
    },
    IncoherentSegmentCache {
        segment: &'static str,
    },
    InvalidCsBits {
        cs_bits: u32,
    },
    InvalidCr4Bits {
        value: u32,
        invalid: u32,
        cpu_type: CpuType,
    },
    InvalidMemoryTypeMsr {
        index: u32,
    },
    UnsupportedMsrState {
        feature: &'static str,
        cpu_type: CpuType,
    },
    ZeroTscClock,
    ZeroTscRatio,
    TscEpochAheadOfCycles {
        epoch_cycles: u64,
        cycles: u64,
    },
    ForgedMxcsr {
        mxcsr: u32,
        mxcsr_mask: u32,
        allowed: u32,
        cpu_type: CpuType,
    },
}
impl fmt::Display for CpuSnapshotValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CplOutOfRange { cpl } => {
                write!(f, "invalid CPU snapshot CPL {cpl}: expected 0..=3")
            }
            Self::V86CplMismatch { cpl } => write!(
                f,
                "invalid CPU snapshot virtual-8086 state: VM=1 requires CPL=3, got {cpl}"
            ),
            Self::V86WithoutProtectedMode => {
                f.write_str("invalid CPU snapshot virtual-8086 state: VM=1 requires CR0.PE=1")
            }
            Self::CsRplCplMismatch { cs, cpl } => write!(
                f,
                "invalid CPU snapshot protected-mode CS {cs:#06x}: selector RPL {} contradicts CPL {cpl}",
                cs & 3
            ),
            Self::IncoherentSegmentCache { segment } => write!(
                f,
                "invalid CPU snapshot {segment} descriptor cache: derived fields disagree          with the canonical access/flags/limit bytes"
            ),
            Self::InvalidCsBits { cs_bits } => {
                write!(f, "invalid CPU snapshot cs_bits {cs_bits}: expected 16 or 32")
            }
            Self::InvalidCr4Bits {
                value,
                invalid,
                cpu_type,
            } => write!(
                f,
                "invalid CPU snapshot CR4 {value:#x}: unsupported bits {invalid:#x} for {cpu_type:?}"
            ),
            Self::InvalidMemoryTypeMsr { index } => {
                write!(f, "invalid CPU snapshot value for MSR {index:#x}")
            }
            Self::UnsupportedMsrState { feature, cpu_type } => write!(
                f,
                "invalid CPU snapshot: {feature} state is non-default on {cpu_type:?}"
            ),
            Self::ZeroTscClock => f.write_str(
                "invalid CPU snapshot TSC clock: an explicit clock rate must be nonzero",
            ),
            Self::ZeroTscRatio => f.write_str(
                "invalid CPU snapshot TSC ratio: a zero Q32.32 tick-per-cycle ratio freezes RDTSC",
            ),
            Self::TscEpochAheadOfCycles {
                epoch_cycles,
                cycles,
            } => write!(
                f,
                "invalid CPU snapshot TSC epoch: epoch cycles {epoch_cycles} are ahead of the          snapshot's cycle count {cycles}"
            ),
            Self::ForgedMxcsr {
                mxcsr,
                mxcsr_mask,
                allowed,
                cpu_type,
            } => write!(
                f,
                "invalid CPU snapshot MXCSR {mxcsr:#x} / MXCSR_MASK {mxcsr_mask:#x}: bits beyond          the {cpu_type:?} capability mask {allowed:#x}"
            ),
        }
    }
}
impl std::error::Error for CpuSnapshotValidationError {}
/// The immutable MXCSR capability mask a CPU identity implements. DAZ is a
/// Pentium 4 addition; [`Cpu::set_cpu_type`] selects the same mask.
fn identity_mxcsr_mask(cpu_type: CpuType) -> u32 {
    if matches!(cpu_type, CpuType::Pentium4) {
        crate::sse::MXCSR_MASK_DAZ
    } else {
        crate::sse::MXCSR_MASK
    }
}
/// Serde default for [`CpuSnapshot::tsc_ticks_per_cycle_q32`]: snapshots
/// written before the constant-TSC model load with the legacy 1:1 mapping.
fn legacy_tsc_q32() -> u64 {
    1u64 << 32
}
/// Already-decoded REP instruction bytes retained across an interpreter
/// scheduling yield. Real hardware continues the decoded instruction even if a
/// completed iteration overwrites its own instruction bytes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RepFetchLatch {
    pub valid: bool,
    pub cs: u16,
    pub eip: u32,
    pub cs_base: u32,
    pub cs_bits: u32,
    pub len: u8,
    pub bytes: [u8; 15],
}
/// Snapshot of all persistent CPU state for save/restore.
///
/// Captures everything needed to resume execution from a saved state.
/// Excludes transient runtime state (instruction cache, diagnostics ring
/// buffer, return-guard stack) that is either rebuilt on restore or is
/// purely diagnostic.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CpuSnapshot {
    pub regs: Registers,
    pub fpu: Fpu,
    /// Architectural XMM/MXCSR state. Defaults for save-states written before
    /// SSE state existed are supplied by `SseState`.
    #[serde(default)]
    pub sse: SseState,
    pub mmx: [u64; 8],
    pub cycles: u64,
    /// IA32 SYSENTER state, defaulted for older snapshots.
    #[serde(default)]
    pub sysenter_cs: u32,
    #[serde(default)]
    pub sysenter_esp: u32,
    #[serde(default)]
    pub sysenter_eip: u32,
    /// TSC rebase offset applied by WRMSR(0x10).
    #[serde(default)]
    pub tsc_write_offset: i64,
    /// Stored MTRR/PAT state; memory types do not affect this cacheless model.
    #[serde(default)]
    pub memory_type_msrs: crate::msr::MemoryTypeMsrs,
    /// Constant-TSC scaling state (2026.08.17). Pre-existing snapshots
    /// default to the legacy 1:1 mapping.
    #[serde(default)]
    pub tsc_epoch_cycles: u64,
    #[serde(default)]
    pub tsc_epoch_ticks: u64,
    #[serde(default = "legacy_tsc_q32")]
    pub tsc_ticks_per_cycle_q32: u64,
    /// Optional machine clock in Hz, independent of the CPU identity.
    #[serde(default)]
    pub tsc_clock_hz: Option<u64>,
    /// Optional exact guest cycle clock in Hz ([`Cpu::set_tsc_exact_ratio`]).
    #[serde(default)]
    pub tsc_cycle_hz: Option<u64>,
    pub halted: bool,
    pub interrupts_enabled: bool,
    pub interrupt_inhibit: u8,
    pub cpu_type: CpuType,
    pub cs_bits: u32,
    pub allow_null_cs: bool,
    pub allow_pm_real_mode_ss: bool,
    /// True while the in-process DPMI host owns protected-mode execution.
    ///
    /// Ring-3 protected-mode code can exist without DPMI (raw PM, VCPI, test
    /// programs). DPMI exception frame formatting must be gated on this bit,
    /// not just on handler CPL.
    #[serde(default)]
    pub dpmi_active: bool,
    pub hle_break_selectors: [u16; 32],
    pub hle_break_count: u8,
    /// Flat-thunk far-return sentinel. `default` so a snapshot taken before the
    /// thunk layer existed still loads — it restores as 0, which is exactly the
    /// disarmed state such a snapshot was taken in.
    #[serde(default)]
    pub thunk_return_selector: u16,
    pub last_effective_instructions: u32,
    pub last_iret: bool,
    pub last_retf: bool,
    pub dpmi_retf_cleanup: u16,
    pub last_exception_error_code: u32,
    pub last_interrupt_had_error_code: bool,
    pub dispatching_exception: Option<u8>,
    pub dos_extender_active: bool,
    /// Decoded REP bytes retained across a scheduler-only yield.
    #[serde(default)]
    pub rep_fetch_latch: RepFetchLatch,
}
impl CpuSnapshot {
    /// Validate architectural invariants before any live CPU state is mutated.
    ///
    /// Serialized snapshots are untrusted guest state. Reject impossible CPL
    /// values rather than letting them acquire supervisor paging semantics, and
    /// reject VM=1 outside ring 3 rather than silently normalizing corrupt data.
    pub fn validate(&self) -> Result<(), CpuSnapshotValidationError> {
        let cpl = self.regs.cpl;
        if cpl > 3 {
            return Err(CpuSnapshotValidationError::CplOutOfRange { cpl });
        }
        if self.regs.is_v86_mode() && cpl != 3 {
            return Err(CpuSnapshotValidationError::V86CplMismatch { cpl });
        }
        if self.regs.is_v86_mode() && self.regs.cr0 & 0x1 == 0 {
            return Err(CpuSnapshotValidationError::V86WithoutProtectedMode);
        }
        let cs = self.regs.cs;
        if self.regs.cr0 & 0x1 != 0 && !self.regs.is_v86_mode() && (cs & 3) as u8 != cpl {
            let cache = &self.regs.cs_cache;
            let pe_switch_window = cpl == 0
                && cache.dpl == 0
                && cache.base == u32::from(cs) << 4
                && cache.limit == 0xFFFF;
            if !pe_switch_window {
                return Err(CpuSnapshotValidationError::CsRplCplMismatch { cs, cpl });
            }
        }
        if !matches!(self.cs_bits, 16 | 32) {
            return Err(CpuSnapshotValidationError::InvalidCsBits {
                cs_bits: self.cs_bits,
            });
        }
        for (name, cache) in [
            ("CS", &self.regs.cs_cache),
            ("DS", &self.regs.ds_cache),
            ("ES", &self.regs.es_cache),
            ("FS", &self.regs.fs_cache),
            ("GS", &self.regs.gs_cache),
            ("SS", &self.regs.ss_cache),
        ] {
            if !cache.derived_fields_are_coherent() {
                return Err(CpuSnapshotValidationError::IncoherentSegmentCache { segment: name });
            }
        }
        let invalid_cr4 = self.regs.cr4 & !self.cpu_type.cr4_valid_mask();
        if invalid_cr4 != 0 {
            return Err(CpuSnapshotValidationError::InvalidCr4Bits {
                value: self.regs.cr4,
                invalid: invalid_cr4,
                cpu_type: self.cpu_type,
            });
        }
        if self.tsc_clock_hz == Some(0) || self.tsc_cycle_hz == Some(0) {
            return Err(CpuSnapshotValidationError::ZeroTscClock);
        }
        if !self.cpu_type.supports_sep()
            && (self.sysenter_cs != 0 || self.sysenter_esp != 0 || self.sysenter_eip != 0)
        {
            return Err(CpuSnapshotValidationError::UnsupportedMsrState {
                feature: "SEP",
                cpu_type: self.cpu_type,
            });
        }
        let validate_msr = |index, value| {
            crate::msr::validate_memory_type_msr_value(index, value)
                .map_err(|_| CpuSnapshotValidationError::InvalidMemoryTypeMsr { index })
        };
        validate_msr(0x2FF, self.memory_type_msrs.def_type)?;
        for (offset, value) in self.memory_type_msrs.variable.iter().copied().enumerate() {
            validate_msr(0x200 + offset as u32, value)?;
        }
        for (index, value) in [0x250, 0x258, 0x259]
            .into_iter()
            .chain(0x268..=0x26F)
            .zip(self.memory_type_msrs.fixed.iter().copied())
        {
            validate_msr(index, value)?;
        }
        validate_msr(0x277, self.memory_type_msrs.pat)?;
        let default_memory_types = crate::msr::MemoryTypeMsrs::default();
        if !self.cpu_type.supports_mtrr()
            && (self.memory_type_msrs.def_type != default_memory_types.def_type
                || self.memory_type_msrs.fixed != default_memory_types.fixed
                || self.memory_type_msrs.variable != default_memory_types.variable)
        {
            return Err(CpuSnapshotValidationError::UnsupportedMsrState {
                feature: "MTRR",
                cpu_type: self.cpu_type,
            });
        }
        if !self.cpu_type.supports_pat() && self.memory_type_msrs.pat != default_memory_types.pat {
            return Err(CpuSnapshotValidationError::UnsupportedMsrState {
                feature: "PAT",
                cpu_type: self.cpu_type,
            });
        }
        if self.tsc_ticks_per_cycle_q32 == 0 {
            return Err(CpuSnapshotValidationError::ZeroTscRatio);
        }
        if self.tsc_epoch_cycles > self.cycles {
            return Err(CpuSnapshotValidationError::TscEpochAheadOfCycles {
                epoch_cycles: self.tsc_epoch_cycles,
                cycles: self.cycles,
            });
        }
        let allowed = identity_mxcsr_mask(self.cpu_type);
        if self.sse.mxcsr_mask & !allowed != 0 || self.sse.mxcsr & !allowed != 0 {
            return Err(CpuSnapshotValidationError::ForgedMxcsr {
                mxcsr: self.sse.mxcsr,
                mxcsr_mask: self.sse.mxcsr_mask,
                allowed,
                cpu_type: self.cpu_type,
            });
        }
        Ok(())
    }
}
/// Operand form retired by one baseline MMX instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmxOperandKind {
    /// EMMS has no explicit register or memory operand.
    None,
    /// The instruction used only encoded registers (or an immediate shift count).
    Register,
    /// The instruction successfully accessed a checked memory operand.
    Memory,
}
/// Opt-in MMX instruction-mix diagnostics.
///
/// The interpreter updates this only when a caller installs
/// it in [`Cpu::mmx_profile`]. Production execution leaves the option empty,
/// avoiding histogram writes on the hot path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmxProfile {
    /// Retired instructions indexed by the second byte of the `0F xx` opcode.
    pub opcode_counts: [u64; 256],
    /// Register-form retirements indexed by opcode.
    pub register_operand_counts: [u64; 256],
    /// Checked-memory-form retirements indexed by opcode.
    pub memory_operand_counts: [u64; 256],
    /// Operand-less retirements indexed by opcode (currently EMMS only).
    pub no_operand_counts: [u64; 256],
    /// Immediate-shift group retirements: `0F 71..73`, then ModRM.reg.
    pub immediate_shift_counts: [[u64; 8]; 3],
}
impl Default for MmxProfile {
    fn default() -> Self {
        Self {
            opcode_counts: [0; 256],
            register_operand_counts: [0; 256],
            memory_operand_counts: [0; 256],
            no_operand_counts: [0; 256],
            immediate_shift_counts: [[0; 8]; 3],
        }
    }
}
impl MmxProfile {
    /// Record one successfully retired MMX instruction.
    #[inline]
    pub fn record(&mut self, opcode: u8, operand: MmxOperandKind) {
        let index = usize::from(opcode);
        self.opcode_counts[index] = self.opcode_counts[index].saturating_add(1);
        let counter = match operand {
            MmxOperandKind::None => &mut self.no_operand_counts[index],
            MmxOperandKind::Register => &mut self.register_operand_counts[index],
            MmxOperandKind::Memory => &mut self.memory_operand_counts[index],
        };
        *counter = counter.saturating_add(1);
    }
    /// Record one immediate packed shift, whose ModRM.reg field selects the
    /// operation rather than an ordinary operand register.
    #[inline]
    pub fn record_immediate_shift(&mut self, opcode: u8, subopcode: u8) {
        debug_assert!((0x71..=0x73).contains(&opcode));
        self.record(opcode, MmxOperandKind::Register);
        let opcode_index = usize::from(opcode.saturating_sub(0x71).min(2));
        let subopcode_index = usize::from(subopcode & 7);
        self.immediate_shift_counts[opcode_index][subopcode_index] =
            self.immediate_shift_counts[opcode_index][subopcode_index].saturating_add(1);
    }
    /// Total number of instructions represented by the histogram.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.opcode_counts
            .iter()
            .copied()
            .fold(0, u64::saturating_add)
    }
}
/// CPU state
///
/// This struct contains all CPU state needed for instruction execution.
pub struct Cpu {
    /// Register file
    pub regs: Registers,
    /// Number of cycles executed
    pub cycles: u64,
    /// Signed bias added to the SCALED TSC whenever the architectural TSC
    /// is read (RDTSC, RDMSR(0x10)). `WRMSR(0x10, V)` updates this so the
    /// next read returns `V`. Tracked here rather than mutating `cycles`
    /// so the underlying instruction-count semantics stay clean.
    /// Both engines use [`Cpu::tsc_from_arch_cycles`] to honour the offset.
    pub tsc_write_offset: i64,
    /// Constant-TSC model (2026.08.17): the guest-visible TSC advances at
    /// the CPU IDENTITY's nominal clock against guest-virtual time, not at
    /// one tick per emulated instruction. `tsc_ticks_per_cycle_q32` is a
    /// Q32.32 tick-per-cycle ratio (`nominal_hz << 32 / cycles_per_second`);
    /// the epoch pair keeps the TSC continuous when the emulation speed
    /// changes ([`Cpu::set_tsc_rate`], called from the emulator's single
    /// `set_cycles_per_ms` funnel). The default is the legacy 1:1 mapping,
    /// so a bare `Cpu` (unit tests, fuzz harnesses, conformance fixtures)
    /// behaves exactly as before until an emulator wires a rate.
    pub tsc_epoch_cycles: u64,
    /// TSC ticks accumulated at `tsc_epoch_cycles` (pre-offset).
    pub tsc_epoch_ticks: u64,
    /// Q32.32 TSC ticks per emulated cycle. `1 << 32` = legacy 1:1.
    pub tsc_ticks_per_cycle_q32: u64,
    /// Halted state (after HLT instruction)
    pub halted: bool,
    /// Interrupt flag cache for quick access
    pub interrupts_enabled: bool,
    /// Interrupt shadow count (delays IRQ delivery for N instruction boundaries)
    pub interrupt_inhibit: u8,
    /// CPU type for instruction set filtering
    pub cpu_type: CpuType,
    /// Last IP for infinite loop detection
    pub last_ip: u16,
    /// Last CS for infinite loop detection
    pub last_cs: u16,
    /// Consecutive same-instruction counter
    pub same_instr_count: u32,
    /// Direct-mapped instruction cache
    #[cfg(feature = "iced-interop")]
    pub instr_cache: Box<[CachedInstruction; INSTR_CACHE_SIZE]>,
    /// Instruction cache enabled flag
    pub instr_cache_enabled: bool,
    /// Cache hit counter for statistics
    pub cache_hits: u64,
    /// Cache miss counter for statistics
    pub cache_misses: u64,
    /// Code segment bitness (16 or 32)
    pub cs_bits: u32,
    /// Treat null CS selector as current CS for guest compatibility
    pub allow_null_cs: bool,
    /// Allow protected-mode SS loads to fall back to real-mode segment bases (DPMI)
    pub allow_pm_real_mode_ss: bool,
    /// True while the DPMI host is actively running the current client.
    pub dpmi_active: bool,
    /// HLE selector values that should cause the interpreter to break out.
    /// Entries beyond `hle_break_count` are ignored.
    pub hle_break_selectors: [u16; 32],
    /// Number of valid entries in `hle_break_selectors`.
    pub hle_break_count: u8,
    /// Far-return sentinel for a 32→16 flat-thunk transition, or 0 when no
    /// thunk window exists.
    ///
    /// A dedicated field rather than an entry in `hle_break_selectors`, because
    /// that array **fails open**: a client can occupy most of its 32 slots, `set_hle_break_selectors` replaces it wholesale, and an
    /// overflow is a `warn!` and a silent drop. A dropped sentinel here does
    /// not degrade a feature — it means the 16-bit callee never returns and the
    /// interpreter runs off into whatever the return descriptor covers. So it
    /// is immune to both replacement and capacity by construction.
    pub thunk_return_selector: u16,
    /// Host return observers need a batch boundary after RETF or IRET.
    /// The emulator refreshes this before each interpreter batch.
    pub far_return_watch: bool,
    /// x87 FPU state
    pub fpu: Fpu,
    /// Architectural XMM/MXCSR state. Kept separate from x87/MMX because XMM
    /// registers do not alias the x87 stack.
    pub sse: SseState,
    /// Byte-validated runtime decode cache for the sparse legacy SIMD path.
    /// It is not architectural state and is deliberately excluded from saves.
    pub sse_decode_cache: SseInstrCache,
    /// MMX instructions retired. Not architectural state — it exists so a
    /// measurement can prove the guest actually took an MMX path rather than
    /// inferring it from a wall-clock delta. A plain counter, not an atomic:
    /// it sits on the interpreter's hot path.
    pub mmx_retired: u64,
    /// Optional baseline-MMX opcode and operand-form histogram.
    pub mmx_profile: Option<Box<MmxProfile>>,
    /// x87 escapes (0xD8-0xDF) dispatched. Same purpose and same caveats as
    /// `mmx_retired`: it exists to size the x87 share of a workload before
    /// deciding whether a float-SIMD path is worth building.
    pub x87_retired: u64,
    /// Optional histogram of executing x87 by 1 KiB guest-EIP window.
    ///
    /// Answers a question `x87_retired` alone cannot: *where* the x87 work is.
    /// Knowing whether the hot x87 sits in code that also contains SSE decides
    /// whether an SSE implementation could displace it at all — which is far
    /// cheaper to establish than to build. `None` unless a caller opts in, so
    /// the hot path pays one null check.
    pub x87_window_hits: Option<std::collections::HashMap<u32, u64>>,
    /// x87 escapes that reach the interpreter's OUTLINED full route
    /// (`op_fpu_esc`) — i.e. escapes declined by BOTH inline lane probes at
    /// the escape arm: the register-lane probe (EXP-IQ011) and the
    /// memory-lane probe (EXP-IQ012). `x87_retired - x87_outlined` = the
    /// combined probe-admitted count. Always present (same rationale as
    /// `x87_retired`): it is the default-build engagement instrument that
    /// keeps the probes' routing testable — feature-gated census counters
    /// are compiled out of every tier that actually runs (panel finding,
    /// 2026.08.05 lever-trio HLD).
    pub x87_outlined: u64,
    /// Effective instruction count from last step().
    /// For normal instructions this is 1. For REP string instructions, this
    /// equals the iteration count (CX), since each iteration should advance
    /// PIT timing by 1 instruction (a REP MOVSB with CX=1000 is equivalent
    /// to 1000 individual memory moves for timing purposes).
    pub last_effective_instructions: u32,
    /// Last instruction was IRET (or HLE IRET) - read by Emulator for HLE dispatch
    pub last_iret: bool,
    /// Last instruction was RETF (or HLE RETF) - read by Emulator for HLE dispatch
    pub last_retf: bool,
    /// DPMI exception return cleanup (bytes to pop after RETF in control.rs)
    pub dpmi_retf_cleanup: u16,
    /// Last exception error code (side channel for #GP, #SS error codes).
    /// Read by the emulator layer for DPMI exception dispatch.
    pub last_exception_error_code: u32,
    /// True if the last interrupt frame pushed an error code (for HLE handler)
    /// This distinguishes hardware IRQs (no error code) from CPU exceptions (error code)
    pub last_interrupt_had_error_code: bool,
    /// Currently dispatching exception vector (None if not dispatching).
    /// Used to detect contributory exception during contributory exception, which escalates to #DF.
    /// Per Intel: contributory exceptions are #DE(0), #TS(10), #NP(11), #SS(12), #GP(13).
    pub dispatching_exception: Option<u8>,
    /// DPMI exception diagnostics
    pub dpmi_exception_diag: DpmiExceptionDiagState,
    /// Diagnostics enabled flag
    pub diag_enabled: bool,
    /// Diagnostics verbosity (verbose records every instruction)
    pub diag_verbose: bool,
    /// Diagnostics ring buffer sampling rate (every N instructions)
    pub diag_sample_rate: u64,
    /// Diagnostics instruction counter for sampling
    pub diag_instr_counter: u64,
    /// Diagnostics loop threshold (same CS:IP)
    pub diag_loop_threshold: u32,
    /// Diagnostics short-loop history (last 8 CS:IP pairs)
    pub diag_loop_history: [u32; 8],
    /// Next write index into diagnostics loop history
    pub diag_loop_pos: u8,
    /// Number of valid entries in diagnostics loop history
    pub diag_loop_filled: u8,
    /// Consecutive 2-instruction loop counter
    pub diag_loop2_count: u32,
    /// Consecutive 4-instruction loop counter
    pub diag_loop4_count: u32,
    /// Execution history ring buffer (diagnostics)
    pub diag_ring: ExecutionRingBuffer,
    /// Return-guard stack (call/return verification)
    ret_guard_stack: FixedVec<RetGuardEntry, 1024>,
    /// DOS protected-mode extender active flag.
    pub dos_extender_active: bool,
    /// Raw calls to the root hardware-task-switch implementation, including
    /// self-switch short-circuits and failures before CR3 is committed.
    pub task_switch_calls: u64,
    /// Task switches that reached Step 5 and reloaded the current CR3 value.
    pub task_switch_same_cr3: u64,
    /// Task switches that reached Step 5 and committed a different CR3 value.
    pub task_switch_changed_cr3: u64,
    /// Cache-line-isolated wake-pending flag for the Phase 0b coroutine
    /// scheduler. The interpreter outer loop observes this byte to bail out
    /// early when a host thread (cpal, nokhwa, RTP recv) has posted a wake into
    /// the scheduler's mailbox.
    ///
    /// Producer side: `WakeMailbox::post` stores `1` here with `Release`
    /// ordering (after pushing the record). Consumer side: the scheduler
    /// clears it (stores `0`) inside `poll_wakes` after draining.
    ///
    /// The dedicated `[repr(align(64))]` wrapper keeps this field on its
    /// own cache line — co-locating with the frequently updated `cycles`
    /// would create write-write contention on the producer side every time
    /// a host thread fires a wake. (Devil's
    /// advocate review item 2, 2026.04.25.)
    pub wakes_pending: WakesPendingFlag,
    /// Runtime-only decoded-x87 descriptor cache for the interpreter's
    /// 0xD8-0xDF arm. Entries are revalidated by exact encoded bytes on every
    /// probe, so it needs no snapshot/reset handling (same convention as
    /// `instr_cache`).
    pub x87_decode_cache: crate::x87_decode_cache::X87DecodeCache,
    /// Decoded REP bytes retained across a scheduler-only yield.
    pub rep_fetch_latch: RepFetchLatch,
    /// Per-instance opt-in for tests which exercise hypothetical SSE3-capable
    /// guest CPUs. No currently modeled production CPU supports SSE3.
    #[cfg(any(test, feature = "test-util"))]
    test_sse3_enabled: bool,
    /// IA32 SYSENTER state; reset to zero on hardware reset.
    pub sysenter_cs: u32,
    pub sysenter_esp: u32,
    pub sysenter_eip: u32,
    /// Stored MTRR/PAT state; memory types do not affect this cacheless model.
    pub memory_type_msrs: crate::msr::MemoryTypeMsrs,
    /// Explicit machine clock, preserved by reset and emulation-speed changes.
    pub tsc_clock_hz: Option<u64>,
    /// Exact guest cycle clock: when set, the TSC ratio is the rational
    /// `tsc_clock_hz / tsc_cycle_hz` and emulation-speed changes leave it be.
    pub tsc_cycle_hz: Option<u64>,
}
/// Cache-line-aligned wake flag (Phase 0b). Wraps a single
/// [`std::sync::atomic::AtomicU8`] in 64 bytes of padding so the producer's
/// `Release` store cannot invalidate the line containing frequently updated
/// interpreter state such as `Cpu::cycles`.
#[repr(C, align(64))]
pub struct WakesPendingFlag {
    /// The flag itself. `0` = no wakes pending; `non-zero` = drain the
    /// mailbox at the next yield point.
    pub flag: std::sync::atomic::AtomicU8,
    /// Padding so the struct fills a full cache line.
    _pad: [u8; 63],
}
impl Default for WakesPendingFlag {
    fn default() -> Self {
        Self::new()
    }
}
impl WakesPendingFlag {
    pub const fn new() -> Self {
        Self {
            flag: std::sync::atomic::AtomicU8::new(0),
            _pad: [0; 63],
        }
    }
    /// Producer-side: set the flag (caller did the underlying mailbox push
    /// already). `Release` ordering pairs with `Acquire` on the consumer.
    pub fn set(&self) {
        self.flag.store(1, std::sync::atomic::Ordering::Release);
    }
    /// Consumer-side: clear the flag after draining the mailbox.
    pub fn clear(&self) {
        self.flag.store(0, std::sync::atomic::Ordering::Release);
    }
    /// Cheap snapshot of the flag (used by the interpreter outer loop).
    pub fn is_set(&self) -> bool {
        self.flag.load(std::sync::atomic::Ordering::Acquire) != 0
    }
}
impl std::fmt::Debug for WakesPendingFlag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WakesPendingFlag")
            .field("flag", &self.is_set())
            .finish()
    }
}
impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}
/// Processor signature (family/model/stepping) returned in EAX by CPUID AX=1
/// and in EDX at reset per Intel SDM §9.1.
///
/// THE single source of this value. It used to be one of three hand-maintained
/// copies — here, the CPUID AX=1 handler in `gp64-interp`, and another in
/// `gp64-cpu-legacy` — kept in step by a doc comment saying "update both
/// together", which was already only two-thirds true. All three now call this.
///
/// Layout is the legacy one: bits 11:8 family, 7:4 model, 3:0 stepping.
/// Extended family/model (bits 27:20 / 19:16) are zero and correct for every
/// model we report; an identity with model >= 0x10 would have to compute
/// `DisplayModel = (ExtModel << 4) + Model` rather than copy these values.
///
/// Exhaustive on purpose: a new identity must choose a signature rather than
/// inherit a wildcard. Before this was exhaustive, a wildcard here reported
/// family 0 and a matching wildcard in the interpreter reported an i486.
pub fn cpuid_signature(cpu_type: CpuType) -> u32 {
    match cpu_type {
        CpuType::Pentium => 0x0525,
        CpuType::PentiumMmx => 0x0543,
        CpuType::PentiumPro => 0x0619,
        CpuType::PentiumII => 0x0633,
        CpuType::PentiumIII => 0x0673,
        CpuType::PentiumIIICoppermine => 0x068A,
        CpuType::Pentium4 => 0x0F24,
        CpuType::I80486 => 0x0401,
        CpuType::I80386 => 0x0300,
        CpuType::I8086 | CpuType::I8088 | CpuType::I80186 | CpuType::I80286 => 0x0000,
    }
}
/// Highest implemented basic CPUID leaf; existing identities retain leaf 1.
pub fn cpuid_max_basic_leaf(cpu_type: CpuType) -> u32 {
    if cpu_type == CpuType::PentiumIIICoppermine {
        2
    } else {
        1
    }
}
/// CPUID cache descriptors for the Pentium III Coppermine identity.
pub fn cpuid_leaf2(cpu_type: CpuType) -> (u32, u32, u32, u32) {
    if cpu_type == CpuType::PentiumIIICoppermine {
        (0x0302_0101, 0, 0, 0x0C04_0841)
    } else {
        (0, 0, 0, 0)
    }
}
/// Family / model / stepping for a guest CPU identity, DECODED from
/// [`cpuid_signature`] rather than tabulated again.
///
/// Every guest-visible CPU identity query must answer in terms of
/// family. Deriving that answer here, from the same word CPUID returns, is
/// what stops those surfaces drifting away from CPUID and from each other:
/// there is no second table to forget to update when an identity is added.
/// Hardcoding them is what MDD-BUG-ANVIL-00092 was.
///
/// The pre-CPUID identities (8086/8088/186/286) report a zero signature, so
/// they decode to family 0. Callers that need a floor must apply their own —
/// inventing a family here would be the same lie in a different place.
pub fn cpuid_family_model_stepping(cpu_type: CpuType) -> (u8, u8, u8) {
    let signature = cpuid_signature(cpu_type);
    let family = ((signature >> 8) & 0x0F) as u8;
    let model = ((signature >> 4) & 0x0F) as u8;
    let stepping = (signature & 0x0F) as u8;
    (family, model, stepping)
}
/// CPUID leaf 1 feature word (EDX) for a guest CPU identity.
///
/// The companion to [`cpuid_signature`], and likewise the single source: the
/// interpreter and the legacy oracle both call this instead of keeping their
/// own tables. They previously disagreed in *form* even while agreeing in
/// value — one wrote `0x0000_01BF` and masked VME, the other hardcoded the
/// post-mask `0x0000_01BD` and folded MMX in as `0x0080_01BD` — with nothing
/// keeping them aligned.
///
/// Feature bits are DERIVED from the capability predicates wherever one
/// exists, so "does this CPU have X" has exactly one answer that CPUID, the
/// opcode gates, and the CPU feature queries all read.
///
/// We advertise strictly less than real silicon: a real Pentium Pro reports
/// `0xFBFF`, adding PAE, APIC, SEP, MTRR, PGE and MCA. Every one of those is
/// feature-bit-gated, so under-advertising is the safe direction — a guest
/// that never tries beats a guest that faults on a capability we claimed.
pub fn cpuid_features(cpu_type: CpuType) -> u32 {
    if !cpu_type.supports_pentium() {
        return 0x0000_0001;
    }
    let mut edx = 0x0000_01BD;
    for (supported, bit) in [
        (cpu_type.supports_sep(), 11),
        (cpu_type.supports_mtrr(), 12),
        (cpu_type.supports_pge(), 13),
        (cpu_type.supports_pat(), 16),
    ] {
        if supported {
            edx |= 1 << bit;
        }
    }
    if cpu_type.supports_p6() {
        edx |= 1 << 15;
    }
    if cpu_type.supports_mmx() {
        edx |= 1 << 23;
    }
    if cpu_type.supports_fxsave() {
        edx |= 1 << 24;
    }
    if cpu_type.supports_sse() {
        edx |= 1 << 25;
    }
    if cpu_type.supports_sse2() {
        edx |= 1 << 26;
    }
    edx
}
/// A descriptor read for a segment-register load, with the Accessed-bit write
/// deferred until the caller has validated the descriptor.
///
/// Real hardware does not dirty a valid-present descriptor when the attempted
/// load fails its type or privilege checks (MDD-BUG-CRUCIBLE-00115). Keeping
/// the write-back address separate lets each loader validate first and commit
/// both the descriptor bit and the hidden cache atomically.
struct CheckedSegmentDescriptor {
    cache: SegmentCache,
    accessed_linear: Option<u32>,
}
impl Cpu {
    /// Create a new CPU in reset state
    pub fn new() -> Self {
        #[cfg(feature = "iced-interop")]
        let cache: Box<[CachedInstruction; INSTR_CACHE_SIZE]> =
            vec![CachedInstruction::default(); INSTR_CACHE_SIZE]
                .into_boxed_slice()
                .try_into()
                .unwrap();
        Self {
            regs: {
                let mut regs = Registers::new();
                regs.idtr.limit = 0x3ff;
                regs
            },
            cycles: 0,
            sysenter_cs: 0,
            sysenter_esp: 0,
            sysenter_eip: 0,
            tsc_write_offset: 0,
            memory_type_msrs: crate::msr::MemoryTypeMsrs::default(),
            tsc_epoch_cycles: 0,
            tsc_epoch_ticks: 0,
            tsc_ticks_per_cycle_q32: 1u64 << 32,
            tsc_clock_hz: None,
            tsc_cycle_hz: None,
            halted: false,
            interrupts_enabled: false,
            interrupt_inhibit: 0,
            cpu_type: CpuType::default(),
            last_ip: 0,
            last_cs: 0,
            same_instr_count: 0,
            #[cfg(feature = "iced-interop")]
            instr_cache: cache,
            instr_cache_enabled: true,
            cache_hits: 0,
            cache_misses: 0,
            cs_bits: 16,
            allow_null_cs: false,
            allow_pm_real_mode_ss: false,
            dpmi_active: false,
            hle_break_selectors: [0; 32],
            hle_break_count: 0,
            thunk_return_selector: 0,
            far_return_watch: true,
            fpu: Fpu::new(),
            sse: SseState::new(),
            sse_decode_cache: SseInstrCache::new(),
            mmx_retired: 0,
            mmx_profile: None,
            x87_retired: 0,
            x87_window_hits: None,
            x87_outlined: 0,
            last_effective_instructions: 1,
            last_iret: false,
            last_retf: false,
            dpmi_retf_cleanup: 0,
            last_exception_error_code: 0,
            last_interrupt_had_error_code: false,
            dispatching_exception: None,
            dpmi_exception_diag: DpmiExceptionDiagState::new(),
            diag_enabled: false,
            diag_verbose: false,
            diag_sample_rate: 10000,
            diag_instr_counter: 0,
            diag_loop_threshold: 100,
            diag_loop_history: [0; 8],
            diag_loop_pos: 0,
            diag_loop_filled: 0,
            diag_loop2_count: 0,
            diag_loop4_count: 0,
            diag_ring: ExecutionRingBuffer::new(),
            ret_guard_stack: FixedVec::new(),
            dos_extender_active: false,
            task_switch_calls: 0,
            task_switch_same_cr3: 0,
            task_switch_changed_cr3: 0,
            wakes_pending: WakesPendingFlag::new(),
            x87_decode_cache: crate::x87_decode_cache::X87DecodeCache::new(),
            rep_fetch_latch: RepFetchLatch::default(),
            #[cfg(any(test, feature = "test-util"))]
            test_sse3_enabled: false,
        }
    }
    /// Returns whether this CPU instance supports SSE3 instructions.
    #[inline]
    pub fn supports_sse3(&self) -> bool {
        if self.cpu_type.supports_sse3() {
            return true;
        }
        #[cfg(any(test, feature = "test-util"))]
        {
            self.test_sse3_enabled
        }
        #[cfg(not(any(test, feature = "test-util")))]
        {
            false
        }
    }
    /// Enables SSE3 for one test CPU without changing any modeled CPU type.
    #[cfg(any(test, feature = "test-util"))]
    pub fn enable_sse3_for_tests(&mut self) {
        self.test_sse3_enabled = true;
    }
    /// Reset CPU to initial state
    pub fn reset(&mut self) {
        self.fpu.reset();
        self.sse.reset();
        self.regs = Registers::new();
        self.regs.cs = 0xF000;
        self.regs.set_ip(0xFFF0);
        self.cycles = 0;
        self.sysenter_cs = 0;
        self.sysenter_esp = 0;
        self.sysenter_eip = 0;
        self.tsc_write_offset = 0;
        self.memory_type_msrs = crate::msr::MemoryTypeMsrs::default();
        self.tsc_epoch_cycles = 0;
        self.tsc_epoch_ticks = 0;
        self.halted = false;
        self.interrupts_enabled = false;
        self.interrupt_inhibit = 0;
        self.cs_bits = 16;
        self.dpmi_active = false;
        self.last_ip = 0;
        self.last_cs = 0;
        self.same_instr_count = 0;
        self.cache_hits = 0;
        self.cache_misses = 0;
        self.last_iret = false;
        self.last_retf = false;
        self.dpmi_retf_cleanup = 0;
        self.last_interrupt_had_error_code = false;
        self.dispatching_exception = None;
        self.rep_fetch_latch = RepFetchLatch::default();
        self.dpmi_exception_diag.reset();
        self.diag_loop_history = [0; 8];
        self.diag_loop_pos = 0;
        self.diag_loop_filled = 0;
        self.diag_loop2_count = 0;
        self.diag_loop4_count = 0;
        self.diag_ring = ExecutionRingBuffer::new();
        self.diag_instr_counter = 0;
        self.ret_guard_stack.clear();
        self.flush_instr_cache();
        self.allow_null_cs = false;
        self.allow_pm_real_mode_ss = false;
        self.hle_break_selectors = [0; 32];
        self.hle_break_count = 0;
        self.thunk_return_selector = 0;
        self.far_return_watch = true;
        self.dos_extender_active = false;
    }
    /// Wire the constant-TSC model to the emulation speed (2026.08.17): the
    /// guest-visible TSC ticks at the explicit machine clock, or otherwise
    /// the identity's nominal clock, against
    /// guest-virtual time, exactly like constant-TSC silicon — a P4-class
    /// CPUID no longer measures as a 100 MHz part just because the
    /// emulation rate is 100k cycles/ms. Folds the epoch so the TSC is
    /// continuous and monotonic across speed changes. Called from the
    /// emulator's single `set_cycles_per_ms` funnel; identities without
    /// RDTSC keep the (unobservable) legacy 1:1 mapping, as does any bare
    /// `Cpu` that never wires a rate — unit tests, fuzz harnesses, and
    /// conformance fixtures see the historical `cycles + offset` TSC.
    pub fn set_tsc_rate(&mut self, cycles_per_ms: u64) {
        if !self.cpu_type.supports_pentium() {
            return;
        }
        let Some(clock_hz) = self.tsc_clock_hz.or_else(|| self.cpu_type.nominal_tsc_hz()) else {
            return;
        };
        self.tsc_epoch_ticks = self.tsc_ticks_at(self.cycles);
        self.tsc_epoch_cycles = self.cycles;
        let cycles_per_second = match self.tsc_cycle_hz {
            Some(cycle_hz) => u128::from(cycle_hz),
            None => u128::from(cycles_per_ms.max(1)) * 1000,
        };
        let q32 = ((u128::from(clock_hz) << 32) / cycles_per_second).max(1);
        self.tsc_ticks_per_cycle_q32 = u64::try_from(q32).unwrap_or(u64::MAX);
    }
    /// Supply an exact machine's TSC clock independently of its CPUID identity.
    /// Reset and subsequent `set_tsc_rate` speed updates retain this machine clock.
    /// Folds the epoch so configuring the clock preserves TSC continuity.
    pub fn set_tsc_clock_hz(&mut self, clock_hz: u64, cycles_per_ms: u64) {
        if !self.cpu_type.supports_pentium() {
            return;
        }
        self.tsc_clock_hz = Some(clock_hz);
        self.set_tsc_rate(cycles_per_ms);
    }
    /// Bind the TSC to an exact machine whose guest cycles run at a fixed
    /// `cycle_hz`: the ratio is `tsc_hz / cycle_hz`, not derived from an
    /// integer `cycles_per_ms` (which truncates 733,333.33 cycles/ms). Equal
    /// clocks give exactly one tick per cycle. Folds the epoch like
    /// [`Self::set_tsc_clock_hz`]; reset, snapshots and later `set_tsc_rate`
    /// speed changes keep the ratio.
    pub fn set_tsc_exact_ratio(&mut self, tsc_hz: u64, cycle_hz: u64) {
        assert!(tsc_hz > 0 && cycle_hz > 0, "TSC clocks must be nonzero");
        if !self.cpu_type.supports_pentium() {
            return;
        }
        self.tsc_clock_hz = Some(tsc_hz);
        self.tsc_cycle_hz = Some(cycle_hz);
        self.set_tsc_rate(0);
    }
    /// Scaled TSC ticks (pre-`WRMSR` offset) at an architectural cycle
    /// count: `epoch_ticks + (arch - epoch_cycles) * ratio`.
    #[inline]
    #[must_use]
    pub fn tsc_ticks_at(&self, arch_cycles: u64) -> u64 {
        let delta = arch_cycles.saturating_sub(self.tsc_epoch_cycles);
        let scaled = (u128::from(delta) * u128::from(self.tsc_ticks_per_cycle_q32)) >> 32;
        self.tsc_epoch_ticks.wrapping_add(scaled as u64)
    }
    /// The architectural TSC at `arch_cycles` — scaled ticks plus the
    /// `WRMSR(0x10)` rebase offset. The single source of truth behind
    /// RDTSC and RDMSR(0x10) on every engine.
    #[inline]
    #[must_use]
    pub fn tsc_from_arch_cycles(&self, arch_cycles: u64) -> u64 {
        self.tsc_ticks_at(arch_cycles)
            .wrapping_add(self.tsc_write_offset as u64)
    }
    /// `WRMSR(0x10, value)` semantics at `arch_cycles`: a read at that
    /// same instant returns exactly `value`.
    pub fn write_tsc_at(&mut self, arch_cycles: u64, value: u64) {
        self.tsc_write_offset = value.wrapping_sub(self.tsc_ticks_at(arch_cycles)) as i64;
    }
    /// Installs 386 reset state per Intel SDM §9.1.
    ///
    /// Resets all descriptor caches (CS/DS/ES/FS/GS/SS), IDTR/GDTR, EFLAGS,
    /// CR0, DR7, and clears transient runtime state (`halted`,
    /// `interrupt_inhibit`, `interrupts_enabled`, last-retf/iret flags,
    /// dispatching-exception tracking, etc.). Sets `EDX` to the CPU signature
    /// we advertise via CPUID so firmware identifies the CPU correctly at POST
    /// entry.
    /// Leaves SYSENTER, MTRR and PAT MSRs intact: this path models INIT, which
    /// preserves those registers (Intel SDM Vol. 3, Table 9-1).
    ///
    /// Does NOT reset GPRs other than `EDX`, and does NOT reset FPU state —
    /// Firmware initializes the FPU separately. Safe to call on either a
    /// fresh `Cpu::new()` (cold boot) or a previously-used CPU (warm reset
    /// path), because every field the 386 reset values cover is explicitly
    /// written here rather than relying on prior contents.
    /// Re-stamp EDX with the current CPU identity, without disturbing any
    /// other reset state.
    ///
    /// MDD-BUG-CRUCIBLE-00354: startup stamps the full reset vector BEFORE the
    /// CLI-selected `--cpu` identity is applied, so EDX described whatever
    /// model happened to be configured at that moment rather than the one the
    /// guest actually runs on. Callers that change the identity after reset use
    /// this to bring the announced identity back in line; it touches only EDX,
    /// so it is safe once the reset vector itself has been established.
    pub fn stamp_reset_identity(&mut self) {
        self.regs.edx = cpuid_signature(self.cpu_type);
    }
    pub fn set_reset_vector_state(&mut self) {
        self.regs.cs = 0xF000;
        self.regs.cs_cache = SegmentCache {
            base: 0xFFFF_0000,
            limit: 0xFFFF,
            cached_effective_limit: 0xFFFF,
            access: 0x9B,
            flags: 0,
            present: true,
            dpl: 0,
            is_code: true,
            is_conforming: false,
            is_readable: true,
            is_writable: false,
            granularity: false,
            big: false,
        };
        self.regs.set_eip(0xFFF0);
        self.cs_bits = 16;
        self.regs.ds = 0;
        self.regs.es = 0;
        self.regs.fs = 0;
        self.regs.gs = 0;
        self.regs.ss = 0;
        self.regs.ds_cache = SegmentCache::real_mode(0);
        self.regs.es_cache = SegmentCache::real_mode(0);
        self.regs.fs_cache = SegmentCache::real_mode(0);
        self.regs.gs_cache = SegmentCache::real_mode(0);
        self.regs.ss_cache = SegmentCache::real_mode(0);
        self.regs.edx = cpuid_signature(self.cpu_type);
        self.regs.flags = Flags::default();
        self.regs.eflags_high = 0;
        self.regs.cr0 = 0x6000_0010;
        self.regs.gdtr = DescriptorTableRegister {
            base: 0,
            limit: 0xFFFF,
        };
        self.regs.idtr = DescriptorTableRegister {
            base: 0,
            limit: 0xFFFF,
        };
        self.mark_descriptor_tables_changed();
        self.regs.dr7 = 0x0000_0400;
        self.halted = false;
        self.interrupts_enabled = false;
        self.interrupt_inhibit = 0;
        self.dpmi_active = false;
        self.last_ip = 0;
        self.last_cs = 0;
        self.same_instr_count = 0;
        self.last_iret = false;
        self.last_retf = false;
        self.dpmi_retf_cleanup = 0;
        self.last_interrupt_had_error_code = false;
        self.dispatching_exception = None;
        self.rep_fetch_latch = RepFetchLatch::default();
    }
    /// Set the CPU type for instruction set filtering
    pub fn set_cpu_type(&mut self, cpu_type: CpuType) {
        self.cpu_type = cpu_type;
        self.sse
            .set_daz_supported(matches!(cpu_type, CpuType::Pentium4));
    }
    /// Get the current CPU type
    #[inline]
    pub fn get_cpu_type(&self) -> CpuType {
        self.cpu_type
    }
    /// Read one architectural XMM register as a bit-exact 128-bit value.
    #[inline]
    pub fn xmm_reg(&self, index: usize) -> u128 {
        self.sse.xmm[index]
    }
    /// Seed one architectural XMM register without applying any instruction
    /// semantics. Intended for state restore, diagnostics, and test setup.
    #[inline]
    pub fn set_xmm_reg(&mut self, index: usize, value: u128) {
        self.sse.xmm[index] = value;
    }
    /// Return all eight architectural XMM registers.
    #[inline]
    pub fn xmm_regs(&self) -> [u128; 8] {
        self.sse.xmm
    }
    /// Seed all eight architectural XMM registers.
    #[inline]
    pub fn set_xmm_regs(&mut self, values: [u128; 8]) {
        self.sse.xmm = values;
    }
    /// Read the guest MXCSR value.
    #[inline]
    pub fn mxcsr(&self) -> u32 {
        self.sse.mxcsr
    }
    /// Seed the guest MXCSR value. Validation belongs to LDMXCSR/FXRSTOR;
    /// this narrow accessor is intentionally a raw state hook for snapshots
    /// and tests.
    #[inline]
    pub fn set_mxcsr(&mut self, value: u32) {
        self.sse.mxcsr = value;
    }
    /// Read the architectural MXCSR feature mask.
    #[inline]
    pub fn mxcsr_mask(&self) -> u32 {
        self.sse.mxcsr_mask
    }
    /// Read one MMX register through its physical x87 alias.
    #[inline]
    pub fn mmx_reg(&self, index: usize) -> u64 {
        self.fpu.read_mmx_register(index)
    }
    /// Seed one physical MMX/x87 register alias without applying an
    /// instruction's global TOP/tag transition. Intended for state restore,
    /// diagnostics, and test setup; instruction handlers must also call
    /// `Fpu::enter_mmx_state` or `Fpu::emms` as appropriate.
    #[inline]
    pub fn set_mmx_reg(&mut self, index: usize, value: u64) {
        self.fpu.write_mmx_register(index, value);
    }
    /// Return all eight MMX register aliases.
    #[inline]
    pub fn mmx_regs(&self) -> [u64; 8] {
        self.fpu.mmx_registers()
    }
    /// Seed all eight physical MMX/x87 register aliases.
    #[inline]
    pub fn set_mmx_regs(&mut self, values: [u64; 8]) {
        for (index, value) in values.into_iter().enumerate() {
            self.fpu.write_mmx_register(index, value);
        }
    }
    /// Check if CPU is in protected mode (PE bit set in CR0, not in V86 mode)
    #[inline]
    pub fn is_protected_mode(&self) -> bool {
        (self.regs.cr0 & 1) != 0 && !self.regs.is_v86_mode()
    }
    /// Check if CPU is in Virtual 8086 mode
    #[inline]
    pub fn is_v86_mode(&self) -> bool {
        self.regs.is_v86_mode()
    }
    /// Check if CPU is in real mode (PE=0 in CR0)
    #[inline]
    pub fn is_real_mode(&self) -> bool {
        (self.regs.cr0 & 1) == 0
    }
    /// Set the HLE break selectors that the fast interpreter checks.
    ///
    /// When CS matches any of these selectors, the interpreter returns
    /// `RunResult::HleEntry` so the emulator can dispatch the HLE handler.
    /// Call this after host-service selectors are allocated.
    pub fn set_hle_break_selectors(&mut self, selectors: &[u16]) {
        let count = selectors.len().min(self.hle_break_selectors.len());
        self.hle_break_selectors[..count].copy_from_slice(&selectors[..count]);
        self.hle_break_count = count as u8;
    }
    /// Add a single HLE break selector if not already present.
    ///
    /// Used by DPMI to register PM_BIOS_CODE_SEL so the interpreter
    /// bails out on FAR JMP/CALL to BIOS HLE stubs in protected mode.
    pub fn add_hle_break_selector(&mut self, selector: u16) {
        if !self.try_add_hle_break_selector(selector) {
            tracing::warn!(
                target : "gp64::cpu", selector = format_args!("{:04X}", selector),
                "HLE break selector array full, cannot add selector"
            );
        }
    }
    /// Add a single HLE break selector, reporting whether it fits.
    ///
    /// [`Self::add_hle_break_selector`] warns and drops on overflow, and
    /// [`Self::set_hle_break_selectors`] truncates with `.min(len)` — both
    /// silent from the caller's point of view. A dropped break selector means
    /// the interpreter never exits on that CS, so the guest executes whatever
    /// bytes the descriptor points at. A caller that can still fail cleanly
    /// during service initialization uses this form and refuses instead.
    ///
    /// Returns `true` if the selector is now registered, including when it
    /// already was.
    pub fn try_add_hle_break_selector(&mut self, selector: u16) -> bool {
        let count = (self.hle_break_count as usize).min(self.hle_break_selectors.len());
        for i in 0..count {
            if self.hle_break_selectors[i] == selector {
                return true;
            }
        }
        if count < self.hle_break_selectors.len() {
            self.hle_break_selectors[count] = selector;
            self.hle_break_count = (count + 1) as u8;
            true
        } else {
            false
        }
    }
    /// How many more distinct break selectors the array can hold.
    ///
    /// Lets a caller check capacity for a whole set *before* registering any of
    /// it, so a partial registration is never observable.
    pub fn hle_break_capacity_remaining(&self) -> usize {
        self.hle_break_selectors
            .len()
            .saturating_sub((self.hle_break_count as usize).min(self.hle_break_selectors.len()))
    }
    /// Remove a single HLE break selector, preserving the order of the rest.
    ///
    /// The teardown counterpart of [`Self::try_add_hle_break_selector`]: a
    /// retained break selector can later match a recycled LDT selector, which
    /// dispatches unrelated guest code as an HLE call.
    ///
    /// Returns `true` if the selector was present.
    pub fn remove_hle_break_selector(&mut self, selector: u16) -> bool {
        let count = (self.hle_break_count as usize).min(self.hle_break_selectors.len());
        let Some(pos) = self.hle_break_selectors[..count]
            .iter()
            .position(|&s| s == selector)
        else {
            return false;
        };
        self.hle_break_selectors.copy_within(pos + 1..count, pos);
        self.hle_break_selectors[count - 1] = 0;
        self.hle_break_count = (count - 1) as u8;
        true
    }
    /// Mask of the EFLAGS bits this CPU *implements* in the upper word
    /// (EFLAGS bits 16-31, expressed as bits 0-15 of the high half).
    ///
    /// This is a positive mask of implemented bits, not a subtractive one.
    /// EFLAGS bits 22-31 are reserved on every CPU we model and read as zero
    /// on real silicon, so they must never survive a POPFD or IRETD image —
    /// a subtractive mask admitted them and let a later PUSHFD hand the guest
    /// an impossible EFLAGS value.
    ///
    /// Bit positions within the high half:
    /// - bit 0 = RF  (EFLAGS 16), 386+
    /// - bit 1 = VM  (EFLAGS 17), 386+
    /// - bit 2 = AC  (EFLAGS 18), 486+
    /// - bit 3 = VIF (EFLAGS 19), Pentium+ (VME)
    /// - bit 4 = VIP (EFLAGS 20), Pentium+ (VME)
    /// - bit 5 = ID  (EFLAGS 21), Pentium+
    ///
    /// Instruction-specific writability and preservation rules (POPFD clears
    /// RF and preserves VM; IRETD gates VM on CPL) are layered over this by
    /// the callers — this method answers only "does the part exist".
    #[inline]
    pub fn eflags_high_mask(&self) -> u16 {
        /// RF | VM — present from the 386, the first CPU with a 32-bit EFLAGS.
        const HIGH_386: u16 = 0x0003;
        /// 386 bits plus AC.
        const HIGH_486: u16 = HIGH_386 | 0x0004;
        /// 486 bits plus VIF, VIP and ID.
        const HIGH_PENTIUM: u16 = HIGH_486 | 0x0038;
        if self.cpu_type.supports_pentium() {
            HIGH_PENTIUM
        } else if self.cpu_type.supports_486() {
            HIGH_486
        } else {
            HIGH_386
        }
    }
    #[inline]
    fn canonicalize_real_mode_flags(&self, flags: crate::Flags) -> crate::Flags {
        if !self.is_real_mode() {
            return flags | crate::Flags::RESERVED1;
        }
        let mut bits = flags.bits() | crate::Flags::RESERVED1.bits();
        match self.cpu_type {
            CpuType::I8086 | CpuType::I8088 | CpuType::I80186 => {
                bits |= 0x7000;
            }
            CpuType::I80286 => {
                bits &= !0x7000;
            }
            _ => {}
        }
        crate::Flags::from_bits_truncate(bits) | crate::Flags::RESERVED1
    }
    /// Return the 16-bit FLAGS image pushed by PUSHF/INT on this CPU type.
    ///
    /// Pre-386 CPU detection relies on the real-mode fixed high FLAGS nibble:
    /// 8086/80186 read bits 12-15 as one, while 286 real mode reads them as
    /// zero. Bit 15 is not represented in `Flags`, so the pushed image needs a
    /// u16-specific helper instead of just `flags.bits()`.
    #[inline]
    pub fn pushf16_image(&self, flags: crate::Flags) -> u16 {
        let mut bits = flags.bits() | crate::Flags::RESERVED1.bits();
        if self.is_real_mode() {
            match self.cpu_type {
                CpuType::I8086 | CpuType::I8088 | CpuType::I80186 => {
                    bits |= 0xF000;
                }
                CpuType::I80286 => {
                    bits &= !0xF000;
                }
                _ => {}
            }
        }
        bits
    }
    /// Apply privilege masking to flags for POPF/POPFD/IRETD.
    ///
    /// In protected mode (CR0.PE=1), IOPL and IF are restricted based on CPL:
    /// - CPL > IOPL: IF is preserved from current flags (not popped value)
    /// - CPL != 0: IOPL is preserved from current flags, NT is cleared
    ///
    /// V86 mode (VM=1) also uses this path: V86 forces CPL=3, and 386 PRM
    /// Chapter 15 specifies that V86 POPF/POPFD/IRET/IRETD with IOPL=3
    /// preserves IOPL (they never change it regardless of stack image).
    /// With IOPL<3, these instructions are caught by a pre-check that
    /// raises #GP(0) before reaching this function. So gating on
    /// `CR0.PE` (not `is_protected_mode()` which is false in V86) lets V86
    /// traverse the same CPL=3 branch and correctly preserves IOPL/IF.
    ///
    /// The V86 CPL invariant (CPL=3 whenever VM=1) is now enforced
    /// upstream by `Registers::set_cpl` and `Registers::set_v86_mode`,
    /// so the IOPL/NT guard can rely on `regs.cpl != 0` alone. The
    /// older defensive `|| self.is_v86_mode()` term was removed once
    /// the invariant became a setter precondition.
    #[inline]
    pub fn apply_popf_privilege(&self, mut flags: crate::Flags) -> crate::Flags {
        if (self.regs.cr0 & 1) != 0 {
            if self.regs.cpl > self.regs.iopl() {
                if self.regs.flags.contains(crate::Flags::INTERRUPT) {
                    flags.insert(crate::Flags::INTERRUPT);
                } else {
                    flags.remove(crate::Flags::INTERRUPT);
                }
            }
            if self.regs.cpl != 0 {
                let iopl_bits = self.regs.flags.bits() & 0x3000;
                let merged = (flags.bits() & !0x3000) | iopl_bits;
                flags = crate::Flags::from_bits_truncate(merged) | crate::Flags::RESERVED1;
            }
        }
        self.canonicalize_real_mode_flags(flags)
    }
    /// Apply the protected-mode flag restrictions for IRET/IRETD.
    ///
    /// Intel's IRET outer-level return sequence loads the new flag image
    /// before changing CPL to the return selector's RPL. Therefore IF/IOPL
    /// gating is based on the privilege level executing IRET, not the
    /// destination CPL reached after the return.
    #[inline]
    pub fn apply_iret_privilege(
        &self,
        mut flags: crate::Flags,
        iret_cpl: u8,
        iret_iopl: u8,
        iret_flags: crate::Flags,
    ) -> crate::Flags {
        if (self.regs.cr0 & 1) != 0 {
            if iret_cpl > iret_iopl {
                if iret_flags.contains(crate::Flags::INTERRUPT) {
                    flags.insert(crate::Flags::INTERRUPT);
                } else {
                    flags.remove(crate::Flags::INTERRUPT);
                }
            }
            if iret_cpl != 0 {
                let iopl_bits = iret_flags.bits() & 0x3000;
                let merged = (flags.bits() & !0x3000) | iopl_bits;
                flags = crate::Flags::from_bits_truncate(merged) | crate::Flags::RESERVED1;
            }
        }
        self.canonicalize_real_mode_flags(flags)
    }
    #[cfg(any(test, feature = "test-util"))]
    pub fn set_ret_guard_for_tests(enabled: bool) {
        RET_GUARD_TEST_ENABLED.store(enabled, Ordering::Relaxed);
    }
    /// Returns the current depth of the ret_guard stack (for testing).
    pub fn ret_guard_depth(&self) -> usize {
        self.ret_guard_stack.len()
    }
    pub fn ret_guard_push(&mut self, entry: RetGuardEntry) {
        if !ret_guard_enabled() {
            return;
        }
        if self.ret_guard_stack.len() >= ret_guard_max_depth() {
            self.ret_guard_stack.remove(0);
        }
        self.ret_guard_stack.push(entry);
    }
    pub fn ret_guard_peek(&self) -> Option<RetGuardEntry> {
        if !ret_guard_enabled() {
            return None;
        }
        self.ret_guard_stack.last().copied()
    }
    pub fn ret_guard_pop(&mut self, kind: RetGuardKind) -> Option<RetGuardEntry> {
        if !ret_guard_enabled() {
            return None;
        }
        let entry = self.ret_guard_stack.last().copied()?;
        if entry.kind != kind {
            return None;
        }
        self.ret_guard_stack.pop()
    }
    pub fn ret_guard_pop_iret_hle(&mut self) {
        let _ = self.ret_guard_pop(RetGuardKind::Iret);
    }
    pub fn ret_guard_pop_retf_hle(&mut self) {
        let _ = self.ret_guard_pop(RetGuardKind::Far);
    }
    pub fn ret_guard_push_interrupt(&mut self, vector: u8) {
        if !ret_guard_enabled() {
            return;
        }
        let sp = if self.is_protected_mode() && self.regs.ss_cache.big {
            self.regs.esp
        } else {
            self.regs.sp() as u32
        };
        self.ret_guard_push(RetGuardEntry {
            kind: RetGuardKind::Iret,
            cs: self.regs.cs,
            ip: self.regs.ip() as u32,
            ss: self.regs.ss,
            sp,
            tag: vector as u16,
            caller_cs: self.regs.cs,
            caller_ip: self.regs.ip(),
        });
    }
    /// Check if CPU is in flat-model protected mode (CS/DS/SS bases all = 0).
    ///
    /// Flat model means offset == linear address for CS, DS, and SS.
    /// Limits are NOT checked: the Memory subsystem handles out-of-bounds access
    /// safely; a descriptor may set its limit to physical memory
    /// size (~256MB), not 4GB.
    #[inline]
    pub fn is_flat_model(&self) -> bool {
        if !self.is_protected_mode() {
            return false;
        }
        self.regs.cs_cache.base == 0 && self.regs.ds_cache.base == 0 && self.regs.ss_cache.base == 0
    }
    /// Get the segment cache for a segment selector
    ///
    /// In real mode, returns a cache with base = selector << 4 and limit = 64KB.
    /// In protected mode, returns the cached descriptor for known selectors,
    /// or falls back to real-mode calculation for unknown selectors.
    #[inline]
    pub fn get_segment_cache(&self, segment_selector: u16) -> SegmentCache {
        if !self.is_protected_mode() {
            return SegmentCache::real_mode(segment_selector);
        }
        if segment_selector == self.regs.cs {
            return self.regs.cs_cache;
        } else if segment_selector == self.regs.ds {
            return self.regs.ds_cache;
        } else if segment_selector == self.regs.es {
            return self.regs.es_cache;
        } else if segment_selector == self.regs.fs {
            return self.regs.fs_cache;
        } else if segment_selector == self.regs.gs {
            return self.regs.gs_cache;
        } else if segment_selector == self.regs.ss {
            return self.regs.ss_cache;
        }
        {
            static WARNED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                tracing::warn!(
                    selector = format_args!("{:04X}", segment_selector), cs =
                    format_args!("{:04X}", self.regs.cs), ds = format_args!("{:04X}",
                    self.regs.ds), es = format_args!("{:04X}", self.regs.es), ss =
                    format_args!("{:04X}", self.regs.ss),
                    "get_segment_cache: PM selector not in any register — using real-mode fallback (further occurrences suppressed)"
                );
            }
        }
        SegmentCache::real_mode(segment_selector)
    }
    #[inline]
    fn resolve_segment_cache<M: crate::PagedMemory>(
        &self,
        memory: &M,
        segment_selector: u16,
    ) -> SegmentCache {
        if !self.is_protected_mode() {
            return SegmentCache::real_mode(segment_selector);
        }
        if segment_selector == self.regs.cs {
            return self.regs.cs_cache;
        } else if segment_selector == self.regs.ds {
            return self.regs.ds_cache;
        } else if segment_selector == self.regs.es {
            return self.regs.es_cache;
        } else if segment_selector == self.regs.fs {
            return self.regs.fs_cache;
        } else if segment_selector == self.regs.gs {
            return self.regs.gs_cache;
        } else if segment_selector == self.regs.ss {
            return self.regs.ss_cache;
        }
        if let Some(cache) = self.read_descriptor(memory, segment_selector) {
            return cache;
        }
        {
            static WARNED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                tracing::warn!(
                    selector = format_args!("{:04X}", segment_selector), cs =
                    format_args!("{:04X}", self.regs.cs), ds = format_args!("{:04X}",
                    self.regs.ds), es = format_args!("{:04X}", self.regs.es), ss =
                    format_args!("{:04X}", self.regs.ss),
                    "resolve_segment_cache: PM selector not in any register or descriptor, using real-mode fallback (further occurrences suppressed)"
                );
            }
        }
        SegmentCache::real_mode(segment_selector)
    }
    /// Get the effective limit from a segment register's cache.
    /// Used by real-mode 32-bit addressing to check against the cached limit,
    /// which is essential for "unreal mode" (big real mode) support.
    #[cfg(feature = "iced-interop")]
    pub fn segment_cache_limit(&self, seg_reg: iced_x86::Register) -> u32 {
        use iced_x86::Register;
        match seg_reg {
            Register::CS => self.regs.cs_cache.effective_limit(),
            Register::DS => self.regs.ds_cache.effective_limit(),
            Register::ES => self.regs.es_cache.effective_limit(),
            Register::FS => self.regs.fs_cache.effective_limit(),
            Register::GS => self.regs.gs_cache.effective_limit(),
            Register::SS => self.regs.ss_cache.effective_limit(),
            _ => 0xFFFF,
        }
    }
    /// Translate segment:offset to linear address.
    ///
    /// This handles both real mode (segment * 16 + offset) and protected mode
    /// (using cached segment descriptors). Does NOT handle paging.
    ///
    /// For full translation including paging, see the main crate's `MemoryExt` trait.
    #[inline]
    pub fn translate_address(&self, segment: u16, offset: u32) -> usize {
        if self.is_real_mode() {
            ((segment as usize) << 4).wrapping_add(offset as usize)
        } else {
            let cache = self.get_segment_cache(segment);
            (cache.base as usize).wrapping_add(offset as usize)
        }
    }
    /// Translate address in V86 mode (real-mode style segmentation)
    ///
    /// In Virtual 8086 mode, addresses are translated using real-mode style
    /// segmentation (segment * 16 + offset), even though the CPU is technically
    /// in protected mode.
    #[inline]
    pub fn translate_v86(&self, segment: u16, offset: u16) -> u32 {
        ((segment as u32) << 4) + (offset as u32)
    }
    /// Translate an implicit CPU table access from linear to physical memory.
    ///
    /// Descriptor-table bases (GDTR/IDTR/LDTR/TR) are architectural linear
    /// addresses.  CPU-initiated descriptor accesses use supervisor paging
    /// semantics even when CPL=3.
    pub fn translate_system_linear_read<M: crate::PagedMemory>(
        &self,
        memory: &M,
        linear: u32,
    ) -> Option<usize> {
        self.translate_system_linear(memory, linear, false)
    }
    /// Checked translation for an implicit CPU table read.
    ///
    /// Descriptor-table accesses use linear addresses and supervisor paging
    /// semantics even when the interrupted code is running at CPL=3.
    pub fn translate_system_linear_read_checked<M: crate::PagedMemory>(
        &self,
        memory: &mut M,
        linear: u32,
    ) -> std::result::Result<usize, crate::PageFaultInfo> {
        if (self.regs.cr0 & 0x8000_0000) == 0 {
            return Ok(linear as usize);
        }
        self.translate_paging_checked_with_user(memory, linear, crate::PageAccess::Read, false)
    }
    /// Translate an implicit CPU table write from linear to physical memory.
    pub fn translate_system_linear_write<M: crate::PagedMemory>(
        &self,
        memory: &M,
        linear: u32,
    ) -> Option<usize> {
        self.translate_system_linear(memory, linear, true)
    }
    /// Translate an implicit CPU table write, preserving page faults.
    ///
    /// The `_checked` sibling of `translate_system_linear_write`: on a paging
    /// fault (page not present, or read-only under CR0.WP) it returns the
    /// `PageFaultInfo` instead of `None`, so callers can deliver a real `#PF`
    /// with the correct error code. Used for the descriptor Accessed-bit
    /// write-back, which must fault when the GDT/LDT page is mapped read-only
    /// (MDD-BUG-CRUCIBLE-00024).
    pub fn translate_system_linear_write_checked<M: crate::PagedMemory>(
        &self,
        memory: &mut M,
        linear: u32,
    ) -> std::result::Result<usize, crate::PageFaultInfo> {
        if (self.regs.cr0 & 0x8000_0000) == 0 {
            return Ok(linear as usize);
        }
        self.translate_paging_checked_with_user(memory, linear, crate::PageAccess::Write, false)
    }
    fn translate_system_linear<M: crate::PagedMemory>(
        &self,
        memory: &M,
        linear: u32,
        is_write: bool,
    ) -> Option<usize> {
        if (self.regs.cr0 & 0x8000_0000) == 0 {
            return Some(linear as usize);
        }
        let linear_page = linear >> 12;
        if let Some((physical_page, writable, _user_allowed, _dirty)) =
            memory.tlb_lookup(linear_page)
        {
            let wp = (self.regs.cr0 & 0x0001_0000) != 0;
            if is_write && !writable && wp {
                return None;
            }
            return Some(((physical_page as usize) << 12) | ((linear & 0x0FFF) as usize));
        }
        let pde_index = (linear >> 22) as usize;
        let pde_addr = ((self.regs.cr3 & 0xFFFF_F000) as usize) + pde_index * 4;
        let pde = memory.read_u32_physical(pde_addr);
        if (pde & 1) == 0 {
            return None;
        }
        let pse_enabled = (self.regs.cr4 & (1 << 4)) != 0;
        if pse_enabled && (pde & (1 << 7)) != 0 {
            let writable = (pde & 2) != 0;
            let wp = (self.regs.cr0 & 0x0001_0000) != 0;
            if is_write && !writable && wp {
                return None;
            }
            let physical = (pde & 0xFFC0_0000) | (linear & 0x003F_FFFF);
            return Some(physical as usize);
        }
        let pte_index = ((linear >> 12) & 0x03FF) as usize;
        let pte_addr = ((pde & 0xFFFF_F000) as usize) + pte_index * 4;
        let pte = memory.read_u32_physical(pte_addr);
        if (pte & 1) == 0 {
            return None;
        }
        let writable = (pde & 2) != 0 && (pte & 2) != 0;
        let wp = (self.regs.cr0 & 0x0001_0000) != 0;
        if is_write && !writable && wp {
            return None;
        }
        let physical_page = pte >> 12;
        Some(((physical_page as usize) << 12) | ((linear & 0x0FFF) as usize))
    }
    /// Read a segment descriptor from the GDT or LDT.
    ///
    /// Returns the parsed `SegmentCache` if the selector points to a valid descriptor,
    /// or None if:
    /// - The selector is null (index 0)
    /// - The selector index is outside the table limit
    pub fn read_descriptor<M: crate::PagedMemory>(
        &self,
        memory: &M,
        selector: u16,
    ) -> Option<SegmentCache> {
        let ti = (selector >> 2) & 1;
        if ti == 0 && (selector & 0xFFF8) == 0 {
            return None;
        }
        let index = (selector >> 3) as usize;
        let (table_base, table_limit) = if ti == 0 {
            (self.regs.gdtr.base as usize, self.regs.gdtr.limit as usize)
        } else {
            (self.regs.ldt_base as usize, self.regs.ldt_limit as usize)
        };
        let desc_offset = index * 8;
        if desc_offset + 7 > table_limit {
            return None;
        }
        let addr = table_base.checked_add(desc_offset)?;
        let mut desc = [0u8; 8];
        let single_page = (addr & 0xFFF) <= 0xFF8 && addr <= (u32::MAX as usize) - 7;
        #[allow(clippy::needless_range_loop)]
        for i in 0..8 {
            let linear = u32::try_from(addr + i).ok()?;
            let Some(phys) = self.translate_system_linear_read(memory, linear) else {
                tracing::debug!(
                    target : "gp64::cpu::segment", selector = format_args!("{:04X}",
                    selector), ti, index, table_base = format_args!("{:08X}",
                    table_base), table_limit = format_args!("{:04X}", table_limit),
                    desc_addr = format_args!("{:08X}", addr), failed_linear =
                    format_args!("{:08X}", linear),
                    "read_descriptor: descriptor translation failed"
                );
                return None;
            };
            if single_page {
                memory.read_run(phys, &mut desc);
                break;
            }
            desc[i] = memory.read_u8(phys);
        }
        Some(SegmentCache::from_descriptor(&desc))
    }
    fn read_descriptor_checked_for_segment_load<M: crate::PagedMemory>(
        &self,
        memory: &mut M,
        selector: u16,
    ) -> crate::Result<CheckedSegmentDescriptor> {
        let ti = (selector >> 2) & 1;
        let index = (selector >> 3) as usize;
        let (table_base, table_limit) = if ti == 0 {
            (self.regs.gdtr.base as usize, self.regs.gdtr.limit as usize)
        } else {
            (self.regs.ldt_base as usize, self.regs.ldt_limit as usize)
        };
        let desc_offset = index * 8;
        if desc_offset + 7 > table_limit {
            return Err(crate::CpuError::GeneralProtectionFault);
        }
        let Some(addr) = table_base.checked_add(desc_offset) else {
            return Err(crate::CpuError::GeneralProtectionFault);
        };
        let mut desc = [0u8; 8];
        let single_page = (addr & 0xFFF) <= 0xFF8 && addr <= (u32::MAX as usize) - 7;
        #[allow(clippy::needless_range_loop)]
        for i in 0..8 {
            let Ok(linear) = u32::try_from(addr + i) else {
                return Err(crate::CpuError::GeneralProtectionFault);
            };
            let phys = match self.translate_system_linear_read_checked(memory, linear) {
                Ok(phys) => phys,
                Err(pf) => {
                    tracing::debug!(
                        target : "gp64::cpu::segment", selector = format_args!("{:04X}",
                        selector), ti, index, table_base = format_args!("{:08X}",
                        table_base), table_limit = format_args!("{:04X}", table_limit),
                        desc_addr = format_args!("{:08X}", addr), failed_linear =
                        format_args!("{:08X}", linear), error_code = pf.error_code,
                        "read_descriptor_checked_for_segment_load: descriptor translation failed"
                    );
                    return Err(crate::CpuError::PageFault(pf));
                }
            };
            if single_page {
                memory.read_run(phys, &mut desc);
                break;
            }
            desc[i] = memory.read_u8(phys);
        }
        let access = desc[5];
        let is_app_segment = (access & 0x10) != 0;
        let present = (access & 0x80) != 0;
        let accessed_linear = if is_app_segment && present && (access & 0x01) == 0 {
            Some(u32::try_from(addr + 5).map_err(|_| crate::CpuError::GeneralProtectionFault)?)
        } else {
            None
        };
        Ok(CheckedSegmentDescriptor {
            cache: SegmentCache::from_descriptor(&desc),
            accessed_linear,
        })
    }
    /// Commit a deferred descriptor Accessed-bit write after validation.
    ///
    /// The hidden cache is updated only after the memory write succeeds, so a
    /// page fault leaves both the descriptor and architectural segment state
    /// unchanged.
    fn commit_descriptor_accessed<M: crate::PagedMemory>(
        &self,
        memory: &mut M,
        selector: u16,
        descriptor: &mut CheckedSegmentDescriptor,
    ) -> crate::Result<()> {
        let Some(linear) = descriptor.accessed_linear else {
            return Ok(());
        };
        let phys = match self.translate_system_linear_write_checked(memory, linear) {
            Ok(phys) => phys,
            Err(pf) => {
                tracing::debug!(
                    target : "gp64::cpu::segment", selector = format_args!("{:04X}",
                    selector), access_linear = format_args!("{:08X}", linear), error_code
                    = pf.error_code,
                    "commit_descriptor_accessed: accessed-bit write-back faulted"
                );
                return Err(crate::CpuError::PageFault(pf));
            }
        };
        let access = descriptor.cache.access | 0x01;
        memory.write_u8(phys, access);
        descriptor.cache.access = access;
        descriptor.accessed_linear = None;
        Ok(())
    }
    /// Check if stack access in range [offset, offset+size) is within limits
    #[inline]
    pub fn stack_range_ok(&self, offset: u32, size: u32) -> bool {
        let limits_active = self.is_protected_mode()
            || self.is_v86_mode()
            || (self.is_real_mode() && self.cpu_type.supports_286());
        if !limits_active {
            return true;
        }
        if size == 0 {
            return true;
        }
        let cache = &self.regs.ss_cache;
        let limit = cache.effective_limit();
        let expand_down = !cache.is_code && (cache.access & 0x04) != 0;
        let max = if cache.big { 0xFFFF_FFFF } else { 0xFFFF };
        let Some(end) = offset.checked_add(size - 1) else {
            return false;
        };
        if expand_down {
            offset > limit && end <= max
        } else {
            end <= limit
        }
    }
    /// Translate segment:offset to physical address for legacy no-fault callers.
    ///
    /// Production code that can architecturally page-fault must use
    /// `translate_address_full_checked` so #PF is delivered normally. This
    /// compatibility helper panics on paging faults rather than returning a
    /// fallback physical address.
    #[inline]
    pub fn translate_address_full<M: crate::PagedMemory>(
        &self,
        mem: &mut M,
        segment: u16,
        offset: u32,
    ) -> usize {
        let cache = self.resolve_segment_cache(mem, segment);
        let linear = (cache.base as usize).wrapping_add(offset as usize);
        if (self.regs.cr0 & 0x8000_0000) == 0 {
            if let Some(phys) = mem.map_linear_physical(linear as u32) {
                return phys;
            }
        }
        if (self.regs.cr0 & 0x8000_0000) != 0 {
            match self.translate_paging_checked(mem, linear as u32, crate::PageAccess::Read) {
                Ok(phys) => {
                    mem.clear_page_fault();
                    phys
                }
                Err(pf) => {
                    mem.set_page_fault(pf.linear_address, pf.error_code);
                    panic!(
                        "translate_address_full called under paging and hit #PF: linear={:08X} error={:08X}",
                        pf.linear_address, pf.error_code
                    );
                }
            }
        } else {
            linear
        }
    }
    /// Translate segment:offset to physical address with full checking.
    ///
    /// Returns an error for segment limit violations, stack faults, or page faults.
    /// This is the main translation method used by instruction execution.
    pub fn translate_address_full_checked<M: crate::PagedMemory>(
        &self,
        mem: &mut M,
        segment: u16,
        offset: u32,
        access: crate::PageAccess,
    ) -> crate::Result<usize> {
        if (self.regs.cr0 & 0x8000_0001) == 0 {
            return Ok(((segment as usize) << 4) + (offset as usize));
        }
        let linear = if self.is_protected_mode() {
            let cache = self.resolve_segment_cache(mem, segment);
            let effective_limit = cache.effective_limit();
            let expand_down = !cache.is_code && (cache.access & 0x04) != 0;
            let max = if cache.big { 0xFFFF_FFFF } else { 0xFFFF };
            let out_of_range = if expand_down {
                offset <= effective_limit || offset > max
            } else {
                offset > effective_limit
            };
            if out_of_range {
                if segment == self.regs.ss {
                    tracing::debug!(
                        target : "gp64::cpu::stack_fault", ss = format_args!("{:04X}",
                        segment), offset = format_args!("{:08X}", offset), limit =
                        format_args!("{:08X}", effective_limit), max =
                        format_args!("{:08X}", max), expand_down, access =
                        format_args!("{:02X}", cache.access), big = cache.big,
                        "Stack offset out of range"
                    );
                    return Err(crate::CpuError::StackFault);
                }
                tracing::debug!(
                    target : "gp64::cpu::segment_fault", segment = format_args!("{:04X}",
                    segment), offset = format_args!("{:08X}", offset), limit =
                    format_args!("{:08X}", effective_limit), max = format_args!("{:08X}",
                    max), expand_down, access_type = ? access, cs =
                    format_args!("{:04X}", self.regs.cs), ip = format_args!("{:04X}",
                    self.regs.ip()),
                    "Segment limit exceeded in translate_address_full_checked"
                );
                return Err(crate::CpuError::GeneralProtectionFault);
            }
            (cache.base as usize).wrapping_add(offset as usize)
        } else {
            self.translate_address(segment, offset)
        };
        if (self.regs.cr0 & 0x8000_0000) == 0 {
            if let Some(phys) = mem.map_linear_physical(linear as u32) {
                mem.clear_page_fault();
                return Ok(phys);
            }
        }
        if (self.regs.cr0 & 0x8000_0000) != 0 {
            match self.translate_paging_checked(mem, linear as u32, access) {
                Ok(phys) => {
                    mem.clear_page_fault();
                    Ok(phys)
                }
                Err(pf) => {
                    mem.set_page_fault(pf.linear_address, pf.error_code);
                    Err(crate::CpuError::PageFault(pf))
                }
            }
        } else {
            mem.clear_page_fault();
            Ok(linear)
        }
    }
    /// Translate an SS-relative offset to a physical address using
    /// `self.regs.ss_cache` directly — **unchecked** (no segment-limit
    /// check, no #SS dispatch).
    ///
    /// This helper is no-paging/legacy-only because stack access type is
    /// caller-specific. Paging-capable stack execution must use
    /// [`translate_stack_addr_full_checked`].
    ///
    /// Companion to [`translate_stack_addr_full_checked`] for the legacy
    /// fault-free `push16` / `push32` / `pop16` / `pop32` helpers. Same
    /// motivation: the selector-based `translate_address_full` path goes
    /// through `resolve_segment_cache`, which iterates CS/DS/ES/FS/GS/SS
    /// and returns the first selector match. When SS aliases another
    /// loaded selector value (e.g. V86→ring-0 entry where V86 ES holds
    /// the same numeric selector as the ring-0 SS, both 0x0010), the
    /// wrong cache is returned and the linear address picks up the wrong
    /// base. Stack ops are always SS-cache-relative — treat the cache as
    /// the source of truth and the SS selector value as advisory.
    ///
    /// Follow-up to commit 8111058c9 ("fix(cpu): stack ops bypass selector
    /// lookup (V86 SS-aliasing #SS)"), which fixed only the *checked*
    /// speckling team-lead.md` for the audit that surfaced these
    /// remaining unchecked sites.
    #[inline]
    pub fn translate_stack_addr_full<M: crate::PagedMemory>(
        &self,
        mem: &mut M,
        offset: u32,
    ) -> usize {
        let linear = if self.is_protected_mode() {
            (self.regs.ss_cache.base as usize).wrapping_add(offset as usize)
        } else {
            ((self.regs.ss as usize) << 4).wrapping_add(offset as usize)
        };
        if (self.regs.cr0 & 0x8000_0000) == 0 {
            if let Some(phys) = mem.map_linear_physical(linear as u32) {
                return phys;
            }
        }
        if (self.regs.cr0 & 0x8000_0000) == 0 {
            return linear;
        }
        match self.translate_paging_checked(mem, linear as u32, crate::PageAccess::Read) {
            Ok(_) => {
                panic!(
                    "translate_stack_addr_full called with paging enabled; use checked stack translation"
                )
            }
            Err(pf) => {
                mem.set_page_fault(pf.linear_address, pf.error_code);
                panic!(
                    "translate_stack_addr_full called under paging and hit #PF: linear={:08X} error={:08X}",
                    pf.linear_address, pf.error_code
                );
            }
        }
    }
    /// Translate an SS-relative offset to a physical address using
    /// `self.regs.ss_cache` directly.
    ///
    /// Stack operations MUST use this — never the `selector`-based
    /// `translate_address_full_checked` / `resolve_segment_cache` path,
    /// because that helper iterates CS/DS/ES/FS/GS/SS in order and returns
    /// the first selector match. When SS aliases another loaded selector
    /// value (e.g. V86→ring-0 entry where the V86 ES happens to hold the
    /// same numeric selector as the ring-0 SS, both 0x0010), the wrong
    /// cache is returned and a flat 4GB ring-0 stack push spuriously
    /// raises #SS against the 16-bit V86 ES limit.
    ///
    /// Limit violations always return `CpuError::StackFault` (never
    /// `GeneralProtectionFault`), because stack-relative accesses are
    /// always #SS by SDM Vol 3 §6.8.3.
    pub fn translate_stack_addr_full_checked<M: crate::PagedMemory>(
        &self,
        mem: &mut M,
        offset: u32,
        access: crate::PageAccess,
    ) -> crate::Result<usize> {
        if (self.regs.cr0 & 0x8000_0001) == 0 {
            return Ok(((self.regs.ss as usize) << 4) + (offset as usize));
        }
        let linear = if self.is_protected_mode() {
            let cache = &self.regs.ss_cache;
            let effective_limit = cache.effective_limit();
            let expand_down = !cache.is_code && (cache.access & 0x04) != 0;
            let max = if cache.big { 0xFFFF_FFFF } else { 0xFFFF };
            let out_of_range = if expand_down {
                offset <= effective_limit || offset > max
            } else {
                offset > effective_limit
            };
            if out_of_range {
                tracing::debug!(
                    target : "gp64::cpu::stack_fault", ss = format_args!("{:04X}", self
                    .regs.ss), offset = format_args!("{:08X}", offset), limit =
                    format_args!("{:08X}", effective_limit), max = format_args!("{:08X}",
                    max), expand_down, access = format_args!("{:02X}", cache.access), big
                    = cache.big,
                    "Stack offset out of range (translate_stack_addr_full_checked)"
                );
                return Err(crate::CpuError::StackFault);
            }
            (cache.base as usize).wrapping_add(offset as usize)
        } else {
            ((self.regs.ss as usize) << 4).wrapping_add(offset as usize)
        };
        if (self.regs.cr0 & 0x8000_0000) == 0 {
            if let Some(phys) = mem.map_linear_physical(linear as u32) {
                mem.clear_page_fault();
                return Ok(phys);
            }
        }
        if (self.regs.cr0 & 0x8000_0000) != 0 {
            match self.translate_paging_checked(mem, linear as u32, access) {
                Ok(phys) => {
                    mem.clear_page_fault();
                    Ok(phys)
                }
                Err(pf) => {
                    mem.set_page_fault(pf.linear_address, pf.error_code);
                    Err(crate::CpuError::PageFault(pf))
                }
            }
        } else {
            mem.clear_page_fault();
            Ok(linear)
        }
    }
    /// Translate a linear address to physical address with paging enforcement.
    pub fn translate_linear_checked<M: crate::PagedMemory>(
        &self,
        mem: &mut M,
        linear: u32,
        access: crate::PageAccess,
    ) -> crate::Result<usize> {
        self.translate_linear_checked_with_dirty_probe(mem, linear, access)
            .map(|(physical, _first_write)| physical)
    }
    /// As [`Self::translate_linear_checked`], but additionally reports whether
    /// this translation observed the leaf dirty bit CLEAR immediately before
    /// the walk's accessed/dirty update — i.e. whether a WRITE translation was
    /// the page's architecturally first write.
    ///
    /// The probe is captured INSIDE the page walk, from the raw PTE (or PSE
    /// PDE) value read before the A/D write-back — never inferred after the
    /// walk has already set D. It reports `false` for: unpaged translations
    /// (no D bit exists),
    /// non-write accesses, and hashed-TLB hits on an already-dirty page (no
    /// walk runs, so no D transition remains to observe).
    pub fn translate_linear_checked_with_dirty_probe<M: crate::PagedMemory>(
        &self,
        mem: &mut M,
        linear: u32,
        access: crate::PageAccess,
    ) -> crate::Result<(usize, bool)> {
        if (self.regs.cr0 & 0x8000_0000) == 0 {
            if let Some(phys) = mem.map_linear_physical(linear) {
                mem.clear_page_fault();
                return Ok((phys, false));
            }
        }
        if (self.regs.cr0 & 0x8000_0000) != 0 {
            match self.translate_paging_checked_probing(mem, linear, access, self.regs.cpl == 3) {
                Ok(result) => {
                    mem.clear_page_fault();
                    Ok(result)
                }
                Err(pf) => {
                    mem.set_page_fault(pf.linear_address, pf.error_code);
                    Err(crate::CpuError::PageFault(pf))
                }
            }
        } else {
            mem.clear_page_fault();
            Ok((linear as usize, false))
        }
    }
    /// Translate a linear address with full page fault checking.
    /// Returns the physical address on success, or `PageFaultInfo` on failure.
    /// Called from the fast interpreter's slow-path paging translation.
    pub fn translate_paging_checked<M: crate::PagedMemory>(
        &self,
        mem: &mut M,
        linear: u32,
        access: crate::PageAccess,
    ) -> std::result::Result<usize, crate::PageFaultInfo> {
        self.translate_paging_checked_with_user(mem, linear, access, self.regs.cpl == 3)
    }
    /// Prove a code fetch for a captured decode context, using the current
    /// CR3/PSE address space but that context's privilege. A compiler may
    /// validate a cached plan while the live CPU is executing another CPL;
    /// borrowing live privilege would incorrectly admit supervisor code into
    /// a user-mode artifact. This does not change the live CPU's state.
    pub fn translate_paging_fetch_checked_at_cpl<M: crate::PagedMemory>(
        &self,
        mem: &mut M,
        linear: u32,
        cpl: u8,
    ) -> std::result::Result<usize, crate::PageFaultInfo> {
        self.translate_paging_checked_with_user(mem, linear, crate::PageAccess::Fetch, cpl == 3)
    }
    fn translate_paging_checked_with_user<M: crate::PagedMemory>(
        &self,
        mem: &mut M,
        linear: u32,
        access: crate::PageAccess,
        user: bool,
    ) -> std::result::Result<usize, crate::PageFaultInfo> {
        self.translate_paging_checked_probing(mem, linear, access, user)
            .map(|(physical, _first_write)| physical)
    }
    /// The one page-walk body. The `bool` in the success value is the
    /// first-write probe: `true` iff this was a WRITE translation whose leaf
    /// dirty bit (PTE, or PSE PDE) was clear in the raw entry read by THIS
    /// walk, captured before the accessed/dirty write-back. Hashed-TLB hits return
    /// `false` for dirty entries. A clean global hit updates its retained
    /// backing entry; other clean hits fall through to the page walk.
    fn translate_paging_checked_probing<M: crate::PagedMemory>(
        &self,
        mem: &mut M,
        linear: u32,
        access: crate::PageAccess,
        user: bool,
    ) -> std::result::Result<(usize, bool), crate::PageFaultInfo> {
        let linear_page = linear >> 12;
        let wp = (self.regs.cr0 & 0x0001_0000) != 0;
        let accessed_bit = 1u32 << 5;
        let dirty_bit = 1u32 << 6;
        let page_size_bit = 1u32 << 7;
        let pse_enabled = (self.regs.cr4 & (1 << 4)) != 0;
        let reserved_pde_mask: u32 = 0x00;
        let reserved_pte_mask: u32 = 0x00;
        if let Some((physical_page, writable, user_allowed, dirty)) = mem.tlb_lookup(linear_page) {
            if access == crate::PageAccess::Write && !dirty {
                if let Some((physical, first_write)) = mem
                    .upgrade_clean_global_translation(linear, user, wp)
                    .map_err(|_| crate::PageFaultInfo::new(linear, true, true, user))?
                {
                    return Ok((physical, first_write));
                }
            } else {
                if access.is_write() && !writable && (user || wp) {
                    return Err(crate::PageFaultInfo::new(linear, true, true, user));
                }
                if user && !user_allowed {
                    return Err(crate::PageFaultInfo::new(
                        linear,
                        true,
                        access.is_write(),
                        true,
                    ));
                }
                let physical = ((physical_page as usize) << 12) | ((linear & 0xFFF) as usize);
                return Ok((physical, false));
            }
        }
        let cr3 = self.regs.cr3;
        let pde_index = (linear >> 22) as usize;
        let pde_addr = ((cr3 & 0xFFFF_F000) as usize) + (pde_index * 4);
        let pde = mem.read_u32_physical(pde_addr);
        mem.record_pt_frame((cr3 & 0xFFFF_F000) >> 12);
        if (pde & 1) == 0 {
            return Err(crate::PageFaultInfo::new(
                linear,
                false,
                access.is_write(),
                user,
            ));
        }
        if (pde & reserved_pde_mask) != 0 {
            let mut info = crate::PageFaultInfo::new(linear, true, access.is_write(), user);
            info.error_code |= 0x8;
            return Err(info);
        }
        if pse_enabled && (pde & page_size_bit) != 0 {
            let physical = (pde & 0xFFC0_0000) | (linear & 0x003F_FFFF);
            let physical_page = physical >> 12;
            let writable = (pde & 2) != 0;
            let user_allowed = (pde & 4) != 0;
            if access.is_write() && !writable && (user || wp) {
                return Err(crate::PageFaultInfo::new(linear, true, true, user));
            }
            if user && !user_allowed {
                return Err(crate::PageFaultInfo::new(
                    linear,
                    true,
                    access.is_write(),
                    true,
                ));
            }
            let is_write = access.is_write();
            let first_write = is_write && (pde & dirty_bit) == 0;
            let mut pde_updated = pde;
            if (pde & accessed_bit) == 0 {
                pde_updated |= accessed_bit;
            }
            if is_write && (pde & dirty_bit) == 0 {
                pde_updated |= dirty_bit;
            }
            if pde_updated != pde {
                mem.write_u32_physical_ad(pde_addr, pde_updated);
            }
            let dirty = is_write || (pde & dirty_bit) != 0;
            mem.cache_paging_translation(
                linear_page,
                physical_page,
                writable,
                user_allowed,
                dirty,
                gp64_types::traits::PagingTlbSource {
                    address: pde_addr as u32,
                    large: true,
                    global: self.cpu_type.supports_pge()
                        && self.regs.cr4 & crate::CR4_PGE != 0
                        && pde & 0x100 != 0,
                },
            );
            return Ok((physical as usize, first_write));
        }
        let pte_index = ((linear >> 12) & 0x3FF) as usize;
        let pte_addr = ((pde & 0xFFFF_F000) as usize) + (pte_index * 4);
        let pte = mem.read_u32_physical(pte_addr);
        mem.record_pt_frame((pde & 0xFFFF_F000) >> 12);
        if (pte & 1) == 0 {
            return Err(crate::PageFaultInfo::new(
                linear,
                false,
                access.is_write(),
                user,
            ));
        }
        if (pte & reserved_pte_mask) != 0 {
            let mut info = crate::PageFaultInfo::new(linear, true, access.is_write(), user);
            info.error_code |= 0x8;
            return Err(info);
        }
        let physical_page = pte >> 12;
        let writable = (pde & 2) != 0 && (pte & 2) != 0;
        let user_allowed = (pde & 4) != 0 && (pte & 4) != 0;
        if access.is_write() && !writable && (user || wp) {
            return Err(crate::PageFaultInfo::new(linear, true, true, user));
        }
        if user && !user_allowed {
            return Err(crate::PageFaultInfo::new(
                linear,
                true,
                access.is_write(),
                true,
            ));
        }
        let is_write = access.is_write();
        let first_write = is_write && (pte & dirty_bit) == 0;
        let mut pde_updated = pde;
        let mut pte_updated = pte;
        if (pde & accessed_bit) == 0 {
            pde_updated |= accessed_bit;
        }
        if (pte & accessed_bit) == 0 {
            pte_updated |= accessed_bit;
        }
        if is_write && (pte & dirty_bit) == 0 {
            pte_updated |= dirty_bit;
        }
        if pde_updated != pde {
            mem.write_u32_physical_ad(pde_addr, pde_updated);
        }
        if pte_updated != pte {
            mem.write_u32_physical_ad(pte_addr, pte_updated);
        }
        let dirty = is_write || (pte & dirty_bit) != 0;
        mem.cache_paging_translation(
            linear_page,
            physical_page,
            writable,
            user_allowed,
            dirty,
            gp64_types::traits::PagingTlbSource {
                address: pte_addr as u32,
                large: false,
                global: self.cpu_type.supports_pge()
                    && self.regs.cr4 & crate::CR4_PGE != 0
                    && pte & 0x100 != 0,
            },
        );
        let physical = ((physical_page as usize) << 12) | ((linear & 0xFFF) as usize);
        Ok((physical, first_write))
    }
    /// Load CS (Code Segment) register with a new selector.
    ///
    /// In protected mode, reads the descriptor from GDT/LDT and validates it.
    /// Returns an error if the segment is not present or not a code segment.
    pub fn load_cs<M: crate::PagedMemory>(
        &mut self,
        memory: &mut M,
        selector: u16,
    ) -> crate::Result<()> {
        if self.is_protected_mode() {
            if Self::is_null_selector(selector) {
                if self.allow_null_cs {
                    tracing::trace!(
                        target : "gp64::cpu::segment", selector = format_args!("{:04X}",
                        selector), "PM load CS: null selector, keeping current CS"
                    );
                    return Ok(());
                }
                tracing::debug!(
                    target : "gp64::cpu::segment", selector = format_args!("{:04X}",
                    selector), "load_cs: null selector"
                );
                return Err(crate::CpuError::GeneralProtectionFault);
            }
            {
                let mut descriptor =
                    self.read_descriptor_checked_for_segment_load(memory, selector)?;
                let cache = descriptor.cache;
                if !cache.present {
                    return Err(crate::CpuError::SegmentNotPresent(selector));
                }
                if (cache.access & 0x10) == 0 {
                    let desc_type = cache.access & 0x0F;
                    match desc_type {
                        0x09 | 0x0B => {
                            return Err(crate::CpuError::TssDescriptor(selector));
                        }
                        0x04 | 0x0C => {
                            return Err(crate::CpuError::CallGate(selector));
                        }
                        _ => {
                            let type_name = match desc_type {
                                0x4 => "16-bit call gate",
                                0x5 => "task gate",
                                0x6 => "16-bit interrupt gate",
                                0x7 => "16-bit trap gate",
                                0xC => "32-bit call gate",
                                0xE => "32-bit interrupt gate",
                                0xF => "32-bit trap gate",
                                _ => "system descriptor",
                            };
                            tracing::warn!(
                                target : "gp64::cpu", selector = format_args!("{:04X}",
                                selector), access = format_args!("{:02X}", cache.access),
                                desc_type = type_name,
                                "load_cs: selector points to {} — not supported",
                                type_name,
                            );
                            return Err(crate::CpuError::GeneralProtectionFault);
                        }
                    }
                }
                if !cache.is_code {
                    return Err(crate::CpuError::GeneralProtectionFault);
                }
                let cpl = self.regs.cpl;
                let rpl = (selector & 3) as u8;
                if cache.is_conforming {
                    if cache.dpl > cpl {
                        return Err(crate::CpuError::GeneralProtectionFault);
                    }
                } else if cache.dpl != cpl || rpl > cpl {
                    return Err(crate::CpuError::GeneralProtectionFault);
                }
                self.commit_descriptor_accessed(memory, selector, &mut descriptor)?;
                let cache = descriptor.cache;
                let new_selector = (selector & !0x3) | (cpl as u16);
                self.regs.cs = new_selector;
                self.regs.cs_cache = cache;
                let old_cs_bits = self.cs_bits;
                self.cs_bits = if cache.big { 32 } else { 16 };
                if old_cs_bits == 16 && self.cs_bits == 32 && (self.regs.eip & 0xFFFF_0000) != 0 {
                    tracing::error!(
                        target : "gp64::cpu::eip_corrupt", selector =
                        format_args!("{:04X}", selector), eip = format_args!("{:08X}",
                        self.regs.eip), ip = format_args!("{:04X}", self.regs.ip()),
                        "Switching to 32-bit CS with EIP upper bits non-zero!"
                    );
                }
                tracing::trace!(
                    target : "gp64::cpu::segment", selector = format_args!("{:04X}",
                    selector), base = format_args!("{:08X}", cache.base), limit =
                    format_args!("{:08X}", cache.effective_limit()), d_b = if cache.big {
                    32 } else { 16 }, "PM load CS"
                );
            }
        } else {
            self.regs.cs = selector;
            self.regs.cs_cache.base = (selector as u32) << 4;
            if !self.regs.is_v86_mode() {
                self.regs.set_cpl(0);
            }
            self.cs_bits = 16;
        }
        Ok(())
    }
    /// Set CS for HLE callback dispatch (no privilege checks).
    ///
    /// Used when dispatching host-service guest callbacks. Reads the
    /// descriptor from the LDT/GDT and populates cs_cache, but skips the
    /// DPL/CPL privilege checks that `load_cs` enforces. In real mode,
    /// uses real-mode segment addressing.
    pub fn set_cs_for_hle<M: crate::PagedMemory>(&mut self, memory: &M, selector: u16) {
        if self.is_protected_mode() {
            if let Some(cache) = self.read_descriptor(memory, selector) {
                self.regs.cs = selector;
                self.regs.cs_cache = cache;
                self.cs_bits = if cache.big { 32 } else { 16 };
            } else {
                let mut cache = SegmentCache::real_mode(selector);
                cache.access = 0x9B;
                cache.is_code = true;
                cache.is_writable = false;
                self.regs.cs = selector;
                self.regs.cs_cache = cache;
                self.cs_bits = 16;
            }
        } else {
            self.regs.cs = selector;
            self.regs.cs_cache.base = (selector as u32) << 4;
            self.cs_bits = 16;
        }
    }
    /// Set a segment register for HLE dispatch (no privilege checks).
    ///
    /// This compatibility helper follows the CPU's current execution mode.
    /// DOS APIs whose effective mode can differ from CR0.PE must use
    /// [`Cpu::return_data_seg_for_hle`] instead.
    #[cfg(feature = "iced-interop")]
    pub fn set_seg_for_hle<M: crate::PagedMemory>(
        &mut self,
        memory: &M,
        reg: iced_x86::Register,
        selector: u16,
    ) {
        let effective_protected_mode = self.is_protected_mode() && !self.regs.is_v86_mode();
        match reg {
            iced_x86::Register::DS => self.return_data_seg_for_hle(
                Some(memory),
                crate::DataSegReg::DS,
                selector,
                effective_protected_mode,
            ),
            iced_x86::Register::ES => self.return_data_seg_for_hle(
                Some(memory),
                crate::DataSegReg::ES,
                selector,
                effective_protected_mode,
            ),
            iced_x86::Register::FS => self.return_data_seg_for_hle(
                Some(memory),
                crate::DataSegReg::FS,
                selector,
                effective_protected_mode,
            ),
            iced_x86::Register::GS => self.return_data_seg_for_hle(
                Some(memory),
                crate::DataSegReg::GS,
                selector,
                effective_protected_mode,
            ),
            iced_x86::Register::SS => {
                self.regs.ss = selector;
                if effective_protected_mode {
                    self.regs.ss_cache = self
                        .read_descriptor(memory, selector)
                        .or_else(|| Self::is_null_selector(selector).then(SegmentCache::default))
                        .unwrap_or_else(|| SegmentCache::real_mode(selector));
                } else {
                    self.regs.ss_cache.base = (selector as u32) << 4;
                }
            }
            _ => {}
        }
    }
    /// Return a data segment register from a mode-aware HLE API (no privilege checks).
    ///
    /// `effective_protected_mode` describes the emulated API call, not merely
    /// CR0.PE: DPMI simulated-real-mode dispatch keeps the CPU mechanically in
    /// protected mode while DOS segment returns still require 386 real-mode
    /// semantics. Protected-mode returns load the descriptor. Real-mode, V86,
    /// and simulated-real-mode returns update only the cache base, preserving
    /// unreal-mode limit and access attributes.
    /// `memory` is required only for a protected-mode descriptor load.
    pub fn return_data_seg_for_hle<M: crate::PagedMemory>(
        &mut self,
        memory: Option<&M>,
        reg: crate::DataSegReg,
        selector: u16,
        effective_protected_mode: bool,
    ) {
        let protected_cache = if effective_protected_mode {
            Some(
                memory
                    .and_then(|memory| self.read_descriptor(memory, selector))
                    .unwrap_or_default(),
            )
        } else {
            None
        };
        match reg {
            crate::DataSegReg::DS => {
                self.regs.ds = selector;
                if let Some(cache) = protected_cache {
                    self.regs.ds_cache = cache;
                } else {
                    self.regs.ds_cache.base = (selector as u32) << 4;
                }
            }
            crate::DataSegReg::ES => {
                self.regs.es = selector;
                if let Some(cache) = protected_cache {
                    self.regs.es_cache = cache;
                } else {
                    self.regs.es_cache.base = (selector as u32) << 4;
                }
            }
            crate::DataSegReg::FS => {
                self.regs.fs = selector;
                if let Some(cache) = protected_cache {
                    self.regs.fs_cache = cache;
                } else {
                    self.regs.fs_cache.base = (selector as u32) << 4;
                }
            }
            crate::DataSegReg::GS => {
                self.regs.gs = selector;
                if let Some(cache) = protected_cache {
                    self.regs.gs_cache = cache;
                } else {
                    self.regs.gs_cache.base = (selector as u32) << 4;
                }
            }
        }
    }
    /// Set the ES selector in real mode (or V86), refreshing `es_cache` so the
    /// descriptor-cache base tracks `selector << 4`.
    ///
    /// HLE handlers that return a segment in ES (INT 21h/35h Get Vector, Get
    /// DTA, Get PSP, FCB services, …) must not assign `regs.es` directly: in
    /// real mode the interpreter computes the segment base from `es_cache.base`,
    /// so a bare assignment leaves the base stale and ES-overridden memory
    /// accesses read the wrong linear address.  This is the memory-free
    /// memory-free real-mode counterpart to [`Cpu::return_data_seg_for_hle`].
    pub fn set_es_real_mode(&mut self, selector: u16) {
        self.regs.es = selector;
        self.regs.es_cache.base = (selector as u32) << 4;
    }
    /// Load CS for an IRET/RETF return.
    ///
    /// IRET has different privilege checking rules than JMP/CALL:
    /// - The new CPL comes from the RPL of the new CS selector
    /// - For non-conforming segments, DPL must equal RPL (not the old CPL)
    /// - The new RPL must be >= old CPL (can only return to same or outer ring)
    /// - The return offset must be within the prospective CS limit
    pub fn load_cs_iret<M: crate::PagedMemory>(
        &mut self,
        memory: &mut M,
        selector: u16,
        offset: u32,
    ) -> crate::Result<()> {
        if self.is_protected_mode() {
            if Self::is_null_selector(selector) {
                if self.allow_null_cs {
                    tracing::trace!(
                        target : "gp64::cpu::segment", selector = format_args!("{:04X}",
                        selector), "PM load CS (IRET): null selector, keeping current CS"
                    );
                    return Ok(());
                }
                self.last_exception_error_code = Self::selector_exception_error_code(selector);
                tracing::debug!(
                    target : "gp64::cpu::segment", selector = format_args!("{:04X}",
                    selector), "load_cs_iret: null selector"
                );
                return Err(crate::CpuError::GeneralProtectionFault);
            }
            {
                let mut descriptor =
                    match self.read_descriptor_checked_for_segment_load(memory, selector) {
                        Ok(descriptor) => descriptor,
                        Err(e @ crate::CpuError::PageFault(_)) => return Err(e),
                        Err(e) => {
                            self.last_exception_error_code =
                                Self::selector_exception_error_code(selector);
                            return Err(e);
                        }
                    };
                let cache = descriptor.cache;
                if (cache.access & 0x10) == 0 || !cache.is_code {
                    self.last_exception_error_code = Self::selector_exception_error_code(selector);
                    return Err(crate::CpuError::GeneralProtectionFault);
                }
                if !cache.present {
                    self.last_exception_error_code = Self::selector_exception_error_code(selector);
                    return Err(crate::CpuError::SegmentNotPresent(selector));
                }
                let old_cpl = self.regs.cpl;
                let new_cpl = (selector & 3) as u8;
                if new_cpl < old_cpl {
                    self.last_exception_error_code = Self::selector_exception_error_code(selector);
                    return Err(crate::CpuError::GeneralProtectionFault);
                }
                if cache.is_conforming {
                    if cache.dpl > new_cpl {
                        self.last_exception_error_code =
                            Self::selector_exception_error_code(selector);
                        return Err(crate::CpuError::GeneralProtectionFault);
                    }
                } else if cache.dpl != new_cpl {
                    self.last_exception_error_code = Self::selector_exception_error_code(selector);
                    return Err(crate::CpuError::GeneralProtectionFault);
                }
                if offset > cache.effective_limit() {
                    self.last_exception_error_code = 0;
                    tracing::debug!(
                        target : "gp64::cpu::segment", selector = format_args!("{:04X}",
                        selector), offset = format_args!("{:08X}", offset), limit =
                        format_args!("{:08X}", cache.effective_limit()),
                        "IRET/RETF target offset exceeds CS limit"
                    );
                    return Err(crate::CpuError::GeneralProtectionFault);
                }
                self.commit_descriptor_accessed(memory, selector, &mut descriptor)?;
                let cache = descriptor.cache;
                self.regs.set_cpl(new_cpl);
                self.regs.cs = selector;
                self.regs.cs_cache = cache;
                self.cs_bits = if cache.big { 32 } else { 16 };
                tracing::trace!(
                    target : "gp64::cpu::segment", selector = format_args!("{:04X}",
                    selector), base = format_args!("{:08X}", cache.base), limit =
                    format_args!("{:08X}", cache.effective_limit()), old_cpl, new_cpl,
                    d_b = if cache.big { 32 } else { 16 }, "PM load CS (IRET)"
                );
            }
        } else {
            self.regs.cs = selector;
            self.regs.cs_cache.base = (selector as u32) << 4;
            self.cs_bits = 16;
        }
        Ok(())
    }
    #[inline]
    fn selector_exception_error_code(selector: u16) -> u32 {
        (selector as u32) & 0xFFFC
    }
    #[inline]
    fn is_null_selector(selector: u16) -> bool {
        selector <= 0x0003
    }
    /// Load SS (Stack Segment) register with a new selector.
    ///
    /// In protected mode, validates DPL == CPL and the segment is writable.
    pub fn load_ss<M: crate::PagedMemory>(
        &mut self,
        memory: &mut M,
        selector: u16,
    ) -> crate::Result<()> {
        if self.is_protected_mode() && Self::is_null_selector(selector) {
            return Err(crate::CpuError::GeneralProtectionFault);
        }
        if self.is_protected_mode() {
            let mut descriptor =
                match self.read_descriptor_checked_for_segment_load(memory, selector) {
                    Ok(descriptor) => descriptor,
                    Err(e @ crate::CpuError::PageFault(_)) => return Err(e),
                    Err(_) if self.allow_pm_real_mode_ss => {
                        self.regs.ss = selector;
                        self.regs.ss_cache = SegmentCache::real_mode(selector);
                        tracing::trace!(
                            target : "gp64::cpu::segment", selector = format_args!("{:04X}",
                            selector), base = format_args!("{:08X}", self.regs.ss_cache
                            .base), "PM load SS fallback to real-mode base"
                        );
                        return Ok(());
                    }
                    Err(_) => return Err(crate::CpuError::GeneralProtectionFault),
                };
            let cache = descriptor.cache;
            {
                if (cache.access & 0x10) == 0 || cache.is_code || !cache.is_writable {
                    tracing::debug!(
                        target : "gp64::cpu::segment", selector = format_args!("{:04X}",
                        selector), is_code = cache.is_code, writable = cache.is_writable,
                        "SS load type fault (#GP)"
                    );
                    return Err(crate::CpuError::GeneralProtectionFault);
                }
                let rpl = (selector & 3) as u8;
                if cache.dpl != self.regs.cpl || rpl != self.regs.cpl {
                    tracing::debug!(
                        target : "gp64::cpu::segment", selector = format_args!("{:04X}",
                        selector), dpl = cache.dpl, cpl = self.regs.cpl, rpl,
                        "SS load privilege fault (#GP)"
                    );
                    return Err(crate::CpuError::GeneralProtectionFault);
                }
                if !cache.present {
                    tracing::debug!(
                        target : "gp64::cpu::stack_fault", selector =
                        format_args!("{:04X}", selector),
                        "SS not-present stack fault (#SS)"
                    );
                    return Err(crate::CpuError::StackFault);
                }
                self.commit_descriptor_accessed(memory, selector, &mut descriptor)?;
                let cache = descriptor.cache;
                self.regs.ss = selector;
                self.regs.ss_cache = cache;
                tracing::trace!(
                    target : "gp64::cpu::segment", selector = format_args!("{:04X}",
                    selector), base = format_args!("{:08X}", cache.base), limit =
                    format_args!("{:08X}", cache.effective_limit()), "PM load SS"
                );
            }
        } else {
            self.regs.ss = selector;
            self.regs.ss_cache.base = (selector as u32) << 4;
        }
        Ok(())
    }
    /// Load a data segment register (DS, ES, FS, or GS).
    ///
    /// In protected mode, reads the descriptor and caches it.
    /// Null selectors are allowed for data segments (they become unusable).
    pub fn load_data_segment<M: crate::PagedMemory>(
        &mut self,
        memory: &M,
        reg: crate::DataSegReg,
        selector: u16,
    ) {
        match reg {
            crate::DataSegReg::DS => {
                self.regs.ds = selector;
                if self.is_protected_mode() {
                    if let Some(cache) = self.read_descriptor(memory, selector) {
                        self.regs.ds_cache = cache;
                    } else if Self::is_null_selector(selector) {
                        self.regs.ds_cache = SegmentCache::default();
                    } else {
                        self.regs.ds_cache = SegmentCache::real_mode(selector);
                    }
                } else {
                    self.regs.ds_cache.base = (selector as u32) << 4;
                }
            }
            crate::DataSegReg::ES => {
                self.regs.es = selector;
                if self.is_protected_mode() {
                    if let Some(cache) = self.read_descriptor(memory, selector) {
                        self.regs.es_cache = cache;
                    } else if Self::is_null_selector(selector) {
                        self.regs.es_cache = SegmentCache::default();
                    } else {
                        self.regs.es_cache = SegmentCache::real_mode(selector);
                    }
                } else {
                    self.regs.es_cache.base = (selector as u32) << 4;
                }
            }
            crate::DataSegReg::FS => {
                self.regs.fs = selector;
                if self.is_protected_mode() {
                    if let Some(cache) = self.read_descriptor(memory, selector) {
                        self.regs.fs_cache = cache;
                    } else if Self::is_null_selector(selector) {
                        self.regs.fs_cache = SegmentCache::default();
                    } else {
                        self.regs.fs_cache = SegmentCache::real_mode(selector);
                    }
                } else {
                    self.regs.fs_cache.base = (selector as u32) << 4;
                }
            }
            crate::DataSegReg::GS => {
                self.regs.gs = selector;
                if self.is_protected_mode() {
                    if let Some(cache) = self.read_descriptor(memory, selector) {
                        self.regs.gs_cache = cache;
                    } else if Self::is_null_selector(selector) {
                        self.regs.gs_cache = SegmentCache::default();
                    } else {
                        self.regs.gs_cache = SegmentCache::real_mode(selector);
                    }
                } else {
                    self.regs.gs_cache.base = (selector as u32) << 4;
                }
            }
        }
    }
    /// Load a data segment register (DS, ES, FS, or GS) with error checking.
    ///
    /// In protected mode, validates the descriptor before loading.
    /// Returns an error for invalid selectors or descriptor violations.
    pub fn load_data_segment_checked<M: crate::PagedMemory>(
        &mut self,
        memory: &mut M,
        reg: crate::DataSegReg,
        selector: u16,
    ) -> crate::Result<()> {
        if !self.is_protected_mode() {
            self.load_data_segment(memory, reg, selector);
            return Ok(());
        }
        if Self::is_null_selector(selector) {
            match reg {
                crate::DataSegReg::DS => {
                    self.regs.ds = selector;
                    self.regs.ds_cache = crate::registers::SegmentCache::default();
                }
                crate::DataSegReg::ES => {
                    self.regs.es = selector;
                    self.regs.es_cache = crate::registers::SegmentCache::default();
                }
                crate::DataSegReg::FS => {
                    self.regs.fs = selector;
                    self.regs.fs_cache = crate::registers::SegmentCache::default();
                }
                crate::DataSegReg::GS => {
                    self.regs.gs = selector;
                    self.regs.gs_cache = crate::registers::SegmentCache::default();
                }
            }
            return Ok(());
        }
        let mut descriptor = self.read_descriptor_checked_for_segment_load(memory, selector)?;
        let cache = descriptor.cache;
        if (cache.access & 0x10) == 0 {
            return Err(crate::CpuError::GeneralProtectionFault);
        }
        if cache.is_code && !cache.is_readable {
            return Err(crate::CpuError::GeneralProtectionFault);
        }
        let rpl = (selector & 0x03) as u8;
        let cpl = self.regs.cpl;
        let max_priv = cpl.max(rpl);
        if cache.is_code {
            if !cache.is_conforming && cache.dpl < max_priv {
                return Err(crate::CpuError::GeneralProtectionFault);
            }
        } else if cache.dpl < max_priv {
            return Err(crate::CpuError::GeneralProtectionFault);
        }
        if !cache.present {
            return Err(crate::CpuError::SegmentNotPresent(selector));
        }
        self.commit_descriptor_accessed(memory, selector, &mut descriptor)?;
        let cache = descriptor.cache;
        match reg {
            crate::DataSegReg::DS => {
                self.regs.ds = selector;
                self.regs.ds_cache = cache;
            }
            crate::DataSegReg::ES => {
                self.regs.es = selector;
                self.regs.es_cache = cache;
            }
            crate::DataSegReg::FS => {
                self.regs.fs = selector;
                self.regs.fs_cache = cache;
            }
            crate::DataSegReg::GS => {
                self.regs.gs = selector;
                self.regs.gs_cache = cache;
            }
        }
        Ok(())
    }
    /// Return the table-level descriptor generations used by dynrec proofs.
    #[inline]
    pub fn descriptor_table_generations(&self) -> (u64, u64) {
        (self.regs.gdt_generation, self.regs.ldt_generation)
    }
    /// Return the generation for the descriptor table addressed by `selector`.
    ///
    /// This is deliberately table-level, not per-descriptor. DPMI and raw GDT
    /// writes can mutate descriptor memory through several paths; until the CPU
    /// owns per-entry generations, dynrec engines invalidate conservatively
    /// when the containing table changes.
    #[inline]
    pub fn descriptor_table_generation_for_selector(&self, selector: u16) -> u64 {
        if (selector & 0x0004) != 0 {
            self.regs.ldt_generation
        } else {
            self.regs.gdt_generation
        }
    }
    /// Return the descriptor proof generation for `selector`.
    ///
    /// For D2-4 this is the same table-level generation returned by
    /// [`Self::descriptor_table_generation_for_selector`].
    #[inline]
    pub fn descriptor_generation_for_selector(&self, selector: u16) -> u64 {
        self.descriptor_table_generation_for_selector(selector)
    }
    /// Mark the GDT descriptor table as changed.
    #[inline]
    pub fn mark_gdt_changed(&mut self) {
        self.regs.gdt_generation = self.regs.gdt_generation.wrapping_add(1);
    }
    /// Mark the LDT descriptor table as changed.
    #[inline]
    pub fn mark_ldt_changed(&mut self) {
        self.regs.ldt_generation = self.regs.ldt_generation.wrapping_add(1);
    }
    /// Mark the descriptor table addressed by `selector` as changed.
    #[inline]
    pub fn mark_descriptor_table_changed_for_selector(&mut self, selector: u16) {
        if (selector & 0x0004) != 0 {
            self.mark_ldt_changed();
        } else {
            self.mark_gdt_changed();
        }
    }
    /// Mark both descriptor tables as changed.
    #[inline]
    pub fn mark_descriptor_tables_changed(&mut self) {
        self.mark_gdt_changed();
        self.mark_ldt_changed();
    }
    /// Invalidate and reload segment cache for a specific selector.
    ///
    /// Call this after modifying a descriptor in the GDT/LDT to ensure
    /// the CPU uses the updated values.
    pub fn invalidate_segment_cache<M: crate::PagedMemory>(&mut self, memory: &M, selector: u16) {
        if self.regs.cs == selector {
            if let Some(cache) = self.read_descriptor(memory, selector) {
                if cache.is_code {
                    self.regs.cs_cache = cache;
                    self.cs_bits = if cache.big { 32 } else { 16 };
                }
            }
        }
        if self.regs.ds == selector {
            if let Some(cache) = self.read_descriptor(memory, selector) {
                self.regs.ds_cache = cache;
            }
        }
        if self.regs.es == selector {
            if let Some(cache) = self.read_descriptor(memory, selector) {
                self.regs.es_cache = cache;
            }
        }
        if self.regs.fs == selector {
            if let Some(cache) = self.read_descriptor(memory, selector) {
                self.regs.fs_cache = cache;
            }
        }
        if self.regs.gs == selector {
            if let Some(cache) = self.read_descriptor(memory, selector) {
                self.regs.gs_cache = cache;
            }
        }
        if self.regs.ss == selector {
            if let Some(cache) = self.read_descriptor(memory, selector) {
                self.regs.ss_cache = cache;
            }
        }
    }
    fn translate_stack_span<const N: usize, M: crate::PagedMemory>(
        &self,
        memory: &mut M,
        offset: u32,
        wrap_16bit: bool,
        access: crate::PageAccess,
    ) -> crate::Result<[usize; N]> {
        let mut addresses = [0; N];
        let paging = (self.regs.cr0 & 0x8000_0000) != 0;
        let mut previous_offset = None;
        let mut previous_address = 0;
        for (index, address) in addresses.iter_mut().enumerate() {
            let index = index as u32;
            let byte_offset = if wrap_16bit {
                offset.wrapping_add(index) & 0xFFFF
            } else {
                offset
                    .checked_add(index)
                    .ok_or(crate::CpuError::StackFault)?
            };
            *address = if paging
                && previous_offset == Some(byte_offset.wrapping_sub(1))
                && (previous_address & 0xFFF) != 0xFFF
            {
                previous_address + 1
            } else {
                self.translate_stack_addr_full_checked(memory, byte_offset, access)?
            };
            previous_offset = Some(byte_offset);
            previous_address = *address;
        }
        Ok(addresses)
    }
    fn read_stack_bytes<const N: usize, M: crate::PagedMemory>(
        &self,
        memory: &mut M,
        offset: u32,
        wrap_16bit: bool,
    ) -> crate::Result<[u8; N]> {
        let addresses =
            self.translate_stack_span::<N, _>(memory, offset, wrap_16bit, crate::PageAccess::Read)?;
        Ok(addresses.map(|address| memory.read_u8(address)))
    }
    fn write_stack_bytes<const N: usize, M: crate::PagedMemory>(
        &self,
        memory: &mut M,
        offset: u32,
        wrap_16bit: bool,
        bytes: [u8; N],
    ) -> crate::Result<()> {
        let addresses = self.translate_stack_span::<N, _>(
            memory,
            offset,
            wrap_16bit,
            crate::PageAccess::Write,
        )?;
        for (address, byte) in addresses.into_iter().zip(bytes) {
            memory.write_u8(address, byte);
        }
        Ok(())
    }
    /// Try to pop a 16-bit value from the stack.
    ///
    /// Returns the value if successful, or an error if the stack access would
    /// violate segment limits.
    pub fn try_pop16<M: crate::PagedMemory>(&mut self, memory: &mut M) -> crate::Result<u16> {
        if self.is_protected_mode() && self.regs.ss_cache.big {
            if !self.stack_range_ok(self.regs.esp, 2) {
                return Err(crate::CpuError::StackFault);
            }
            let value = u16::from_le_bytes(self.read_stack_bytes(memory, self.regs.esp, false)?);
            self.regs.set_esp(self.regs.esp.wrapping_add(2));
            Ok(value)
        } else {
            let sp = self.regs.sp();
            if !self.stack_range_ok(sp as u32, 2) {
                return Err(crate::CpuError::StackFault);
            }
            let value = u16::from_le_bytes(self.read_stack_bytes(memory, sp as u32, true)?);
            self.regs.set_sp(sp.wrapping_add(2));
            Ok(value)
        }
    }
    /// Try to pop a 32-bit value from the stack.
    pub fn try_pop32<M: crate::PagedMemory>(&mut self, memory: &mut M) -> crate::Result<u32> {
        if self.is_protected_mode() && self.regs.ss_cache.big {
            if !self.stack_range_ok(self.regs.esp, 4) {
                return Err(crate::CpuError::StackFault);
            }
            let value = u32::from_le_bytes(self.read_stack_bytes(memory, self.regs.esp, false)?);
            self.regs.set_esp(self.regs.esp.wrapping_add(4));
            Ok(value)
        } else {
            let sp = self.regs.sp();
            if !self.stack_range_ok(sp as u32, 4) {
                return Err(crate::CpuError::StackFault);
            }
            let value = u32::from_le_bytes(self.read_stack_bytes(memory, sp as u32, true)?);
            self.regs.set_sp(sp.wrapping_add(4));
            Ok(value)
        }
    }
    /// Pop one logical stack frame, rolling the stack pointer back if any
    /// field faults. `saved_esp` is the instruction-entry value so later
    /// frame phases also undo fields popped by earlier phases.
    #[inline]
    fn try_pop_frame<T>(
        &mut self,
        saved_esp: u32,
        pop: impl FnOnce(&mut Self) -> crate::Result<T>,
    ) -> crate::Result<T> {
        match pop(self) {
            Ok(frame) => Ok(frame),
            Err(error) => {
                self.regs.esp = saved_esp;
                Err(error)
            }
        }
    }
    /// Check whether a data segment should be nullified on return to an outer
    /// privilege level (Intel SDM, 386 PRM Section 15.5).
    ///
    /// A segment is nullified if its selector is non-null AND the descriptor
    /// indicates a data segment or non-conforming code segment with
    /// `DPL < new_cpl` or `DPL < RPL(selector)`.  Conforming code segments
    /// are exempt because they are accessible from any privilege level >= DPL.
    fn should_nullify_segment(selector: u16, cache: &SegmentCache, new_cpl: u8) -> bool {
        if selector == 0 {
            return false;
        }
        if cache.is_code && cache.is_conforming {
            return false;
        }
        let rpl = (selector & 3) as u8;
        cache.dpl < new_cpl || cache.dpl < rpl
    }
    /// Execute a 16-bit IRET instruction.
    ///
    /// Pops IP, CS, and FLAGS from the stack and optionally SS:SP if returning
    /// to outer privilege level.
    pub fn iret<M: crate::PagedMemory>(&mut self, memory: &mut M) -> crate::Result<()> {
        self.last_exception_error_code = 0;
        if self.is_protected_mode() && self.regs.flags.contains(crate::Flags::NESTED) {
            return Err(crate::CpuError::TaskSwitchNeeded);
        }
        let saved_cs = self.regs.cs;
        let saved_cs_cache = self.regs.cs_cache;
        let saved_cpl = self.regs.cpl;
        let saved_cs_bits = self.cs_bits;
        let saved_ip = self.regs.ip();
        let saved_eip = self.regs.eip;
        let saved_flags = self.regs.flags;
        let saved_esp = self.regs.esp;
        let saved_sp = self.regs.sp();
        let old_cpl = self.regs.cpl;
        let old_iopl = self.regs.iopl();
        let (new_ip, new_cs, flags) = self.try_pop_frame(saved_esp, |cpu| {
            Ok((
                cpu.try_pop16(memory)?,
                cpu.try_pop16(memory)?,
                cpu.try_pop16(memory)?,
            ))
        })?;
        let new_cpl = (new_cs & 3) as u8;
        let returning_outer = self.is_protected_mode() && new_cpl > old_cpl;
        let (new_sp, new_ss) = if returning_outer {
            let (sp, ss) = self.try_pop_frame(saved_esp, |cpu| {
                Ok((cpu.try_pop16(memory)?, cpu.try_pop16(memory)?))
            })?;
            (Some(sp), Some(ss))
        } else {
            (None, None)
        };
        if self.is_real_mode() && u32::from(new_ip) > self.regs.cs_cache.effective_limit() {
            self.regs.esp = saved_esp;
            self.regs.set_sp(saved_sp);
            return Err(crate::CpuError::GeneralProtectionFault);
        }
        if let Err(e) = self.load_cs_iret(memory, new_cs, new_ip as u32) {
            self.regs.esp = saved_esp;
            self.regs.set_sp(saved_sp);
            return Err(e);
        }
        self.regs.set_ip(new_ip);
        let new_flags = crate::Flags::from_bits_truncate(flags) | crate::Flags::RESERVED1;
        self.regs.flags = self.apply_iret_privilege(new_flags, old_cpl, old_iopl, saved_flags);
        if returning_outer {
            let sp = new_sp.unwrap();
            let ss = new_ss.unwrap();
            if let Err(e) = self.load_ss(memory, ss) {
                self.last_exception_error_code = Self::selector_exception_error_code(ss);
                self.regs.cs = saved_cs;
                self.regs.cs_cache = saved_cs_cache;
                self.regs.set_cpl(saved_cpl);
                self.cs_bits = saved_cs_bits;
                self.regs.set_ip(saved_ip);
                self.regs.eip = saved_eip;
                self.regs.flags = saved_flags;
                self.regs.esp = saved_esp;
                self.regs.set_sp(saved_sp);
                return Err(e);
            }
            self.regs.set_sp(sp);
            if Self::should_nullify_segment(self.regs.ds, &self.regs.ds_cache, new_cpl) {
                self.regs.ds = 0;
                self.regs.ds_cache = SegmentCache::default();
            }
            if Self::should_nullify_segment(self.regs.es, &self.regs.es_cache, new_cpl) {
                self.regs.es = 0;
                self.regs.es_cache = SegmentCache::default();
            }
            if Self::should_nullify_segment(self.regs.fs, &self.regs.fs_cache, new_cpl) {
                self.regs.fs = 0;
                self.regs.fs_cache = SegmentCache::default();
            }
            if Self::should_nullify_segment(self.regs.gs, &self.regs.gs_cache, new_cpl) {
                self.regs.gs = 0;
                self.regs.gs_cache = SegmentCache::default();
            }
        }
        self.interrupts_enabled = self.regs.flags.contains(crate::Flags::INTERRUPT);
        self.mark_iret();
        Ok(())
    }
    /// Execute a 32-bit IRETD instruction.
    ///
    /// Handles all IRETD complexity:
    /// - V86 mode switch detection (returns CpuError::V86SwitchBlocked)
    /// - EFLAGS privilege masking
    /// - eflags_high management
    /// - Privilege-change stack reload
    /// - DS/ES/FS/GS nullification on return to outer ring
    /// - IF tracking (no interrupt_inhibit — IRET doesn't create a shadow)
    ///
    /// Caller is responsible for: mark_iret(), ret_guard_pop()
    pub fn execute_iretd<M: crate::PagedMemory>(&mut self, mem: &mut M) -> crate::Result<()> {
        self.last_exception_error_code = 0;
        if self.is_protected_mode() && self.regs.flags.contains(crate::Flags::NESTED) {
            return Err(crate::CpuError::TaskSwitchNeeded);
        }
        let old_cpl = self.regs.cpl;
        tracing::trace!(
            target : "gp64::cpu", cs = format_args!("{:04X}", self.regs.cs), eip =
            format_args!("{:08X}", self.regs.eip), cpl = old_cpl,
            "IRETD: popping from {:04X}:{:08X}, CPL={}", self.regs.cs, self.regs.eip,
            old_cpl,
        );
        let saved_cs = self.regs.cs;
        let saved_cs_cache = self.regs.cs_cache;
        let saved_cpl = self.regs.cpl;
        let saved_cs_bits = self.cs_bits;
        let saved_eip = self.regs.eip;
        let saved_ip = self.regs.ip();
        let saved_flags = self.regs.flags;
        let saved_eflags_high = self.regs.eflags_high;
        let saved_esp = self.regs.esp;
        let saved_sp = self.regs.sp();
        let old_iopl = self.regs.iopl();
        let (new_eip, new_cs, new_eflags) = self.try_pop_frame(saved_esp, |cpu| {
            Ok((
                cpu.try_pop32(mem)?,
                cpu.try_pop32(mem)? as u16,
                cpu.try_pop32(mem)?,
            ))
        })?;
        if self.is_real_mode() && new_eip > self.regs.cs_cache.effective_limit() {
            self.regs.esp = saved_esp;
            self.regs.set_sp(saved_sp);
            return Err(crate::CpuError::GeneralProtectionFault);
        }
        if self.is_protected_mode() && (new_eflags & (1 << 17)) != 0 {
            tracing::debug!(
                target : "gp64::cpu", new_cs = format_args!("{:04X}", new_cs), new_eip =
                format_args!("{:08X}", new_eip), new_eflags = format_args!("{:08X}",
                new_eflags), cpl = self.regs.cpl, "IRETD: V86 switch detected",
            );
            let (v86_esp, v86_ss, v86_es, v86_ds, v86_fs, v86_gs) =
                self.try_pop_frame(saved_esp, |cpu| {
                    Ok((
                        cpu.try_pop32(mem)?,
                        cpu.try_pop32(mem)? as u16,
                        cpu.try_pop32(mem)? as u16,
                        cpu.try_pop32(mem)? as u16,
                        cpu.try_pop32(mem)? as u16,
                        cpu.try_pop32(mem)? as u16,
                    ))
                })?;
            let new_eip = new_eip & 0xFFFF;
            if self.regs.cpl != 0 {
                self.regs.esp = saved_esp;
                self.regs.set_sp(saved_sp);
                return Err(crate::CpuError::V86SwitchBlocked(crate::V86SwitchFrame {
                    eip: new_eip,
                    cs: new_cs,
                    eflags: new_eflags,
                    esp: v86_esp,
                    ss: v86_ss,
                    es: v86_es,
                    ds: v86_ds,
                    fs: v86_fs,
                    gs: v86_gs,
                }));
            }
            self.regs.set_v86_mode(true);
            self.regs.eip = new_eip;
            self.regs.esp = v86_esp;
            self.regs.flags =
                crate::Flags::from_bits_truncate(new_eflags as u16) | crate::Flags::RESERVED1;
            let new_high = ((new_eflags >> 16) as u16) & self.eflags_high_mask();
            self.regs.eflags_high = new_high | 0x0002;
            self.regs.cs = new_cs;
            self.regs.cs_cache = SegmentCache::real_mode(new_cs);
            self.regs.ss = v86_ss;
            self.regs.ss_cache = SegmentCache::real_mode(v86_ss);
            self.regs.ds = v86_ds;
            self.regs.ds_cache = SegmentCache::real_mode(v86_ds);
            self.regs.es = v86_es;
            self.regs.es_cache = SegmentCache::real_mode(v86_es);
            self.regs.fs = v86_fs;
            self.regs.fs_cache = SegmentCache::real_mode(v86_fs);
            self.regs.gs = v86_gs;
            self.regs.gs_cache = SegmentCache::real_mode(v86_gs);
            self.cs_bits = 16;
            self.interrupts_enabled = self.regs.flags.contains(crate::Flags::INTERRUPT);
            return Ok(());
        }
        let new_cpl = (new_cs & 3) as u8;
        let returning_outer = self.is_protected_mode() && new_cpl > old_cpl;
        if returning_outer {
            tracing::debug!(
                target : "gp64::cpu", old_cpl, new_cpl, new_cs = format_args!("{:04X}",
                new_cs), new_eip = format_args!("{:08X}", new_eip),
                "IRETD: privilege transition CPL {} → {}", old_cpl, new_cpl,
            );
        }
        let (new_esp, new_ss) = if returning_outer {
            let (esp, ss) = self.try_pop_frame(saved_esp, |cpu| {
                Ok((cpu.try_pop32(mem)?, cpu.try_pop32(mem)? as u16))
            })?;
            (Some(esp), Some(ss))
        } else {
            (None, None)
        };
        if let Err(e) = self.load_cs_iret(mem, new_cs, new_eip) {
            self.regs.esp = saved_esp;
            self.regs.set_sp(saved_sp);
            return Err(e);
        }
        self.regs.eip = new_eip;
        let flags = crate::Flags::from_bits_truncate(new_eflags as u16) | crate::Flags::RESERVED1;
        self.regs.flags = self.apply_iret_privilege(flags, old_cpl, old_iopl, saved_flags);
        let mut new_high = (new_eflags >> 16) as u16;
        if self.is_real_mode() {
            // Real-address IRETD retains VM, VIF and VIP (EFLAGS mask 0x1A0000).
            const RETAINED_HIGH: u16 = 0x001A;
            new_high = (new_high & !RETAINED_HIGH) | (saved_eflags_high & RETAINED_HIGH);
        }
        new_high &= self.eflags_high_mask();
        if (self.regs.cr0 & 1) != 0 && old_cpl != 0 {
            new_high = (new_high & !0x0002) | (self.regs.eflags_high & 0x0002);
        }
        self.regs.eflags_high = new_high;
        if returning_outer {
            let esp = new_esp.unwrap();
            let ss = new_ss.unwrap();
            if let Err(e) = self.load_ss(mem, ss) {
                self.last_exception_error_code = Self::selector_exception_error_code(ss);
                self.regs.cs = saved_cs;
                self.regs.cs_cache = saved_cs_cache;
                self.regs.set_cpl(saved_cpl);
                self.cs_bits = saved_cs_bits;
                self.regs.eip = saved_eip;
                self.regs.set_ip(saved_ip);
                self.regs.flags = saved_flags;
                self.regs.eflags_high = saved_eflags_high;
                self.regs.esp = saved_esp;
                self.regs.set_sp(saved_sp);
                return Err(e);
            }
            self.regs.esp = esp;
            if Self::should_nullify_segment(self.regs.ds, &self.regs.ds_cache, new_cpl) {
                tracing::trace!(
                    target : "gp64::cpu", selector = format_args!("{:04X}", self.regs
                    .ds), dpl = self.regs.ds_cache.dpl, new_cpl,
                    "IRETD: nullified DS (DPL {} < CPL {})", self.regs.ds_cache.dpl,
                    new_cpl,
                );
                self.regs.ds = 0;
                self.regs.ds_cache = SegmentCache::default();
            }
            if Self::should_nullify_segment(self.regs.es, &self.regs.es_cache, new_cpl) {
                tracing::trace!(
                    target : "gp64::cpu", selector = format_args!("{:04X}", self.regs
                    .es), dpl = self.regs.es_cache.dpl, new_cpl,
                    "IRETD: nullified ES (DPL {} < CPL {})", self.regs.es_cache.dpl,
                    new_cpl,
                );
                self.regs.es = 0;
                self.regs.es_cache = SegmentCache::default();
            }
            if Self::should_nullify_segment(self.regs.fs, &self.regs.fs_cache, new_cpl) {
                tracing::trace!(
                    target : "gp64::cpu", selector = format_args!("{:04X}", self.regs
                    .fs), dpl = self.regs.fs_cache.dpl, new_cpl,
                    "IRETD: nullified FS (DPL {} < CPL {})", self.regs.fs_cache.dpl,
                    new_cpl,
                );
                self.regs.fs = 0;
                self.regs.fs_cache = SegmentCache::default();
            }
            if Self::should_nullify_segment(self.regs.gs, &self.regs.gs_cache, new_cpl) {
                tracing::trace!(
                    target : "gp64::cpu", selector = format_args!("{:04X}", self.regs
                    .gs), dpl = self.regs.gs_cache.dpl, new_cpl,
                    "IRETD: nullified GS (DPL {} < CPL {})", self.regs.gs_cache.dpl,
                    new_cpl,
                );
                self.regs.gs = 0;
                self.regs.gs_cache = SegmentCache::default();
            }
        }
        self.interrupts_enabled = self.regs.flags.contains(crate::Flags::INTERRUPT);
        Ok(())
    }
    /// Set up CPU state for COM file execution.
    ///
    /// COM files load at segment:0100 with all segment registers pointing
    /// to the same segment and stack at top of segment.
    pub fn setup_for_com(&mut self, segment: u16) {
        self.regs.cs = segment;
        self.regs.ds = segment;
        self.regs.es = segment;
        self.regs.ss = segment;
        let cache = SegmentCache::real_mode(segment);
        self.regs.cs_cache = cache;
        self.regs.ds_cache = cache;
        self.regs.es_cache = cache;
        self.regs.ss_cache = cache;
        self.regs.set_ip(0x0100);
        self.regs.set_sp(0xFFFE);
        self.regs.flags.insert(crate::Flags::INTERRUPT);
        self.interrupts_enabled = true;
        self.cs_bits = 16;
    }
    /// Set up CPU state for EXE file execution.
    ///
    /// EXE files have separate code, data, and stack segments specified
    /// by the MZ header.
    pub fn setup_for_exe(&mut self, cs: u16, ip: u16, ss: u16, sp: u16, ds: u16) {
        self.regs.cs = cs;
        self.regs.set_ip(ip);
        self.regs.ss = ss;
        self.regs.set_sp(sp);
        self.regs.ds = ds;
        self.regs.es = ds;
        self.regs.cs_cache = SegmentCache::real_mode(cs);
        self.regs.ss_cache = SegmentCache::real_mode(ss);
        self.regs.ds_cache = SegmentCache::real_mode(ds);
        self.regs.es_cache = SegmentCache::real_mode(ds);
        self.regs.flags.insert(crate::Flags::INTERRUPT);
        self.interrupts_enabled = true;
        self.cs_bits = 16;
    }
    /// Mutate the FLAGS word that will be restored by the current IRET frame.
    ///
    /// Stack layout after INT (as pushed by `interrupt()`):
    /// - [SS:SP+0] IP (or EIP for 32-bit)
    /// - [SS:SP+2] CS (or SS:SP+4 for 32-bit)
    /// - [SS:SP+4] FLAGS (or SS:SP+8 for 32-bit)
    ///
    /// For 32-bit DPMI applications, the frame is always 32-bit (EIP/CS/EFLAGS at +0/+4/+8)
    /// regardless of the stack segment's D/B bit. Use `force_32bit_frame=true` for these.
    pub fn with_iret_flags_mut<M: crate::PagedMemory>(
        &mut self,
        memory: &mut M,
        f: &mut dyn FnMut(&mut u16),
    ) -> crate::Result<()> {
        self.with_iret_flags_mut_ex(memory, f, false)
    }
    /// Extended version with explicit frame type control
    pub fn with_iret_flags_mut_ex<M: crate::PagedMemory>(
        &mut self,
        memory: &mut M,
        f: &mut dyn FnMut(&mut u16),
        force_32bit_frame: bool,
    ) -> crate::Result<()> {
        let use_32bit = force_32bit_frame || (self.is_protected_mode() && self.regs.ss_cache.big);
        let flags_off = if use_32bit {
            self.regs.esp.wrapping_add(8)
        } else {
            self.regs.sp().wrapping_add(4) as u32
        };
        if self.is_protected_mode() {
            let effective_limit = self.regs.ss_cache.effective_limit();
            if !self.stack_range_ok(flags_off, 2) {
                tracing::trace!(
                    target : "gp64::cpu::hle_iret", flags_off = format_args!("{:04X}",
                    flags_off), limit = format_args!("{:04X}", effective_limit),
                    "Skipping IRET flags update - would exceed SS limit"
                );
                return Ok(());
            }
        }
        let wrap_16bit = !use_32bit;
        let addresses = self.translate_stack_span::<2, _>(
            memory,
            flags_off,
            wrap_16bit,
            crate::PageAccess::Write,
        )?;
        let mut flags = u16::from_le_bytes(addresses.map(|address| memory.read_u8(address)));
        f(&mut flags);
        for (address, byte) in addresses.into_iter().zip(flags.to_le_bytes()) {
            memory.write_u8(address, byte);
        }
        Ok(())
    }
    /// Check if DPMI exception handler diagnostics should be logged.
    ///
    /// This is a stub implementation that always returns false.
    /// The full diagnostics logic is in the main crate's `hle_dispatch` module.
    pub fn dpmi_exception_diag_should_log_handler(
        &mut self,
        _vector: u8,
        _selector: u16,
        _offset: u32,
    ) -> bool {
        false
    }
    /// Return the architectural instruction offset selected by the current CS width.
    #[inline]
    pub fn instruction_offset(&self) -> u32 {
        if self.cs_bits == 32 {
            self.regs.eip
        } else {
            self.regs.ip() as u32
        }
    }
    /// Return the **architectural** linear instruction pointer: `(CS << 4) + IP`
    /// in real mode, `cs_cache.base + offset` in protected mode.
    ///
    /// This returns the address implied by the CS *register*, which is the
    /// correct value for instruction-cache indexing, breakpoint matching,
    /// disassembly, and diagnostic output.
    ///
    /// **Not** the same as the interpreter's fetch address during PM→RM
    /// transitions: after `MOV CR0` clears PE but before the mandatory far
    /// JMP reloads CS, the descriptor cache retains the protected-mode base
    /// while this function uses `(CS << 4)`.  The interpreter intentionally
    /// fetches from `cs_cache.base` during that window (matching real 386
    /// behavior).  Use [`fetch_linear()`] when you need the address the
    /// interpreter is actually executing from.
    pub fn ip_linear(&self) -> usize {
        if !self.is_protected_mode() {
            return ((self.regs.cs as usize) << 4) + (self.regs.ip() as usize);
        }
        let cs = self.get_segment_cache(self.regs.cs);
        let offset = self.instruction_offset() as usize;
        (cs.base as usize).wrapping_add(offset)
    }
    /// Return the linear address the interpreter is **actually fetching from**.
    ///
    /// Uses `cs_cache.base` unconditionally, matching the interpreter's fetch
    /// logic.  In steady-state real mode this equals `ip_linear()` because
    /// `load_cs()` sets `cs_cache.base = CS << 4`.  They diverge only during
    /// the transient PM→RM window (after `MOV CR0` clears PE, before the
    /// mandatory far JMP reloads CS).
    ///
    /// Prefer [`ip_linear()`] for cache keys, breakpoints, and UI.  Use this
    /// function only when you must match what the interpreter executes.
    pub fn fetch_linear(&self) -> usize {
        let offset = self.instruction_offset() as usize;
        (self.regs.cs_cache.base as usize).wrapping_add(offset)
    }
    /// Push a word onto the stack (unchecked)
    pub fn push16<M: crate::PagedMemory>(&mut self, memory: &mut M, value: u16) {
        if (self.regs.cr0 & 0x8000_0000) != 0 {
            panic!("legacy infallible push16 called with paging enabled; use try_push16");
        }
        if self.is_protected_mode() && self.regs.ss_cache.big {
            self.regs.set_esp(self.regs.esp.wrapping_sub(2));
            let addr = self.translate_stack_addr_full(memory, self.regs.esp);
            memory.write_u16(addr, value);
        } else {
            self.regs.set_sp(self.regs.sp().wrapping_sub(2));
            if self.regs.sp() == 0xFFFF {
                let bytes = value.to_le_bytes();
                let lo_addr = ((self.regs.ss as usize) << 4) + 0xFFFF;
                let hi_addr = (self.regs.ss as usize) << 4;
                memory.write_u8(lo_addr, bytes[0]);
                memory.write_u8(hi_addr, bytes[1]);
            } else {
                let addr = self.translate_stack_addr_full(memory, self.regs.sp() as u32);
                memory.write_u16(addr, value);
            }
        }
    }
    /// Push a dword onto the stack (unchecked)
    pub fn push32<M: crate::PagedMemory>(&mut self, memory: &mut M, value: u32) {
        if (self.regs.cr0 & 0x8000_0000) != 0 {
            panic!("legacy infallible push32 called with paging enabled; use try_push32");
        }
        if self.is_protected_mode() && self.regs.ss_cache.big {
            self.regs.set_esp(self.regs.esp.wrapping_sub(4));
            let addr = self.translate_stack_addr_full(memory, self.regs.esp);
            memory.write_u32(addr, value);
        } else {
            self.regs.set_sp(self.regs.sp().wrapping_sub(4));
            let addr = self.translate_stack_addr_full(memory, self.regs.sp() as u32);
            memory.write_u32(addr, value);
        }
    }
    /// Push a word onto the stack with limit checking
    pub fn try_push16<M: crate::PagedMemory>(
        &mut self,
        memory: &mut M,
        value: u16,
    ) -> crate::Result<()> {
        if self.is_protected_mode() && self.regs.ss_cache.big {
            let new_esp = self.regs.esp.wrapping_sub(2);
            if !self.stack_range_ok(new_esp, 2) {
                return Err(crate::CpuError::StackFault);
            }
            self.write_stack_bytes(memory, new_esp, false, value.to_le_bytes())?;
            self.regs.set_esp(new_esp);
        } else {
            let new_sp = self.regs.sp().wrapping_sub(2);
            if !self.stack_range_ok(new_sp as u32, 2) {
                return Err(crate::CpuError::StackFault);
            }
            self.write_stack_bytes(memory, new_sp as u32, true, value.to_le_bytes())?;
            self.regs.set_sp(new_sp);
        }
        Ok(())
    }
    /// Push a dword onto the stack with limit checking
    pub fn try_push32<M: crate::PagedMemory>(
        &mut self,
        memory: &mut M,
        value: u32,
    ) -> crate::Result<()> {
        if self.is_protected_mode() && self.regs.ss_cache.big {
            let new_esp = self.regs.esp.wrapping_sub(4);
            if !self.stack_range_ok(new_esp, 4) {
                return Err(crate::CpuError::StackFault);
            }
            self.write_stack_bytes(memory, new_esp, false, value.to_le_bytes())?;
            self.regs.set_esp(new_esp);
        } else {
            let new_sp = self.regs.sp().wrapping_sub(4);
            if !self.stack_range_ok(new_sp as u32, 4) {
                return Err(crate::CpuError::StackFault);
            }
            self.write_stack_bytes(memory, new_sp as u32, true, value.to_le_bytes())?;
            self.regs.set_sp(new_sp);
        }
        Ok(())
    }
    /// Push a segment selector using 32-bit operand-size semantics.
    ///
    /// x86 reserves a four-byte stack slot but writes only the two-byte selector.
    /// The complete selector span is translated before memory or SP/ESP changes,
    /// so a paging fault leaves the instruction restartable.
    pub fn try_push_segment32<M: crate::PagedMemory>(
        &mut self,
        memory: &mut M,
        value: u16,
    ) -> crate::Result<()> {
        if self.is_protected_mode() && self.regs.ss_cache.big {
            let new_esp = self.regs.esp.wrapping_sub(4);
            if !self.stack_range_ok(new_esp, 4) {
                return Err(crate::CpuError::StackFault);
            }
            self.write_stack_bytes(memory, new_esp, false, value.to_le_bytes())?;
            self.regs.set_esp(new_esp);
        } else {
            let new_sp = self.regs.sp().wrapping_sub(4);
            if !self.stack_range_ok(new_sp as u32, 4) {
                return Err(crate::CpuError::StackFault);
            }
            self.write_stack_bytes(memory, new_sp as u32, true, value.to_le_bytes())?;
            self.regs.set_sp(new_sp);
        }
        Ok(())
    }
    /// Pop a word from the stack (unchecked)
    pub fn pop16<M: crate::PagedMemory>(&mut self, memory: &mut M) -> u16 {
        if (self.regs.cr0 & 0x8000_0000) != 0 {
            panic!("legacy infallible pop16 called with paging enabled; use try_pop16");
        }
        if self.is_protected_mode() && self.regs.ss_cache.big {
            let addr = self.translate_stack_addr_full(memory, self.regs.esp);
            self.regs.set_esp(self.regs.esp.wrapping_add(2));
            memory.read_u16(addr)
        } else {
            let addr = self.translate_stack_addr_full(memory, self.regs.sp() as u32);
            self.regs.set_sp(self.regs.sp().wrapping_add(2));
            memory.read_u16(addr)
        }
    }
    /// Pop a dword from the stack (unchecked)
    pub fn pop32<M: crate::PagedMemory>(&mut self, memory: &mut M) -> u32 {
        if (self.regs.cr0 & 0x8000_0000) != 0 {
            panic!("legacy infallible pop32 called with paging enabled; use try_pop32");
        }
        if self.is_protected_mode() && self.regs.ss_cache.big {
            let addr = self.translate_stack_addr_full(memory, self.regs.esp);
            self.regs.set_esp(self.regs.esp.wrapping_add(4));
            memory.read_u32(addr)
        } else {
            let addr = self.translate_stack_addr_full(memory, self.regs.sp() as u32);
            self.regs.set_sp(self.regs.sp().wrapping_add(4));
            memory.read_u32(addr)
        }
    }
    /// Enable/disable the instruction cache (useful for SMC-sensitive tests).
    pub fn set_instr_cache_enabled(&mut self, enabled: bool) {
        self.instr_cache_enabled = enabled;
        #[cfg(feature = "iced-interop")]
        for entry in self.instr_cache.iter_mut() {
            entry.addr = usize::MAX;
        }
        self.cache_hits = 0;
        self.cache_misses = 0;
    }
    /// Invalidate all instruction cache entries.
    /// Called after snapshot restore because decoded instructions reference stale code.
    pub fn flush_instr_cache(&mut self) {
        #[cfg(feature = "iced-interop")]
        for entry in self.instr_cache.iter_mut() {
            entry.addr = usize::MAX;
        }
    }
    /// Configure CPU diagnostics (ring buffer, loop detection).
    pub fn configure_diagnostics(
        &mut self,
        enabled: bool,
        verbose: bool,
        loop_threshold: u32,
        sample_rate: u64,
    ) {
        self.diag_enabled = enabled;
        self.diag_verbose = verbose;
        self.diag_loop_threshold = loop_threshold;
        self.diag_sample_rate = sample_rate.max(1);
        self.diag_instr_counter = 0;
        if !enabled {
            self.diag_ring = crate::ExecutionRingBuffer::new();
        }
    }
    /// Get the last N events from the diagnostics ring buffer.
    pub fn diag_last_events(&self, n: usize) -> Vec<crate::RingBufferEntry> {
        self.diag_ring.last_n(n)
    }
    /// Push a DPMI entry to the diagnostics ring buffer.
    pub fn diag_push_dpmi(&mut self, ax: u16) {
        if !self.diag_enabled {
            return;
        }
        let entry = crate::RingBufferEntry::dpmi_full(
            self.regs.cs,
            self.regs.eip,
            self.regs.eax,
            self.regs.esp,
            self.regs.flags.bits(),
            ax,
        );
        self.diag_ring.push(entry);
    }
    /// Check if DPMI tracing is enabled.
    pub fn dpmi_trace_enabled(&self) -> bool {
        tracing::enabled!(target : "gp64::dos::dpmi", tracing::Level::DEBUG)
    }
    /// Clear return-instruction markers (IRET/RETF).
    #[inline]
    pub fn clear_return_flags(&mut self) {
        self.last_iret = false;
        self.last_retf = false;
    }
    /// Mark that an IRET occurred (instruction or HLE).
    #[inline]
    pub fn mark_iret(&mut self) {
        self.last_iret = true;
        if self.dpmi_exception_diag.reentry_depth > 0 {
            self.dpmi_exception_diag.reentry_depth -= 1;
        }
    }
    /// Mark that a RETF occurred (instruction or HLE).
    #[inline]
    pub fn mark_retf(&mut self) {
        self.last_retf = true;
    }
    /// Consume the last-IRET flag.
    #[inline]
    pub fn take_last_iret(&mut self) -> bool {
        let hit = self.last_iret;
        self.last_iret = false;
        hit
    }
    /// Consume the last-RETF flag.
    #[inline]
    pub fn take_last_retf(&mut self) -> bool {
        let hit = self.last_retf;
        self.last_retf = false;
        hit
    }
    /// Peek at the last-IRET flag without clearing it.
    #[inline]
    pub fn last_iret(&self) -> bool {
        self.last_iret
    }
    /// Peek at the last-RETF flag without clearing it.
    #[inline]
    pub fn last_retf(&self) -> bool {
        self.last_retf
    }
    /// Capture all persistent CPU state into a snapshot.
    ///
    /// Excludes: `instr_cache` (flushed on restore), `ret_guard_stack`
    /// (debugging aid), `diag_ring` and `dpmi_exception_diag` (diagnostics),
    /// loop-detection counters and cache hit/miss statistics.
    pub fn snapshot_state(&self) -> CpuSnapshot {
        CpuSnapshot {
            regs: self.regs.clone(),
            fpu: self.fpu.clone(),
            sse: self.sse,
            mmx: self.fpu.mmx_registers(),
            cycles: self.cycles,
            sysenter_cs: self.sysenter_cs,
            sysenter_esp: self.sysenter_esp,
            sysenter_eip: self.sysenter_eip,
            tsc_write_offset: self.tsc_write_offset,
            memory_type_msrs: self.memory_type_msrs.clone(),
            tsc_epoch_cycles: self.tsc_epoch_cycles,
            tsc_epoch_ticks: self.tsc_epoch_ticks,
            tsc_ticks_per_cycle_q32: self.tsc_ticks_per_cycle_q32,
            tsc_clock_hz: self.tsc_clock_hz,
            tsc_cycle_hz: self.tsc_cycle_hz,
            halted: self.halted,
            interrupts_enabled: self.interrupts_enabled,
            interrupt_inhibit: self.interrupt_inhibit,
            cpu_type: self.cpu_type,
            cs_bits: self.cs_bits,
            allow_null_cs: self.allow_null_cs,
            allow_pm_real_mode_ss: self.allow_pm_real_mode_ss,
            dpmi_active: self.dpmi_active,
            hle_break_selectors: self.hle_break_selectors,
            hle_break_count: self.hle_break_count,
            thunk_return_selector: self.thunk_return_selector,
            last_effective_instructions: self.last_effective_instructions,
            last_iret: self.last_iret,
            last_retf: self.last_retf,
            dpmi_retf_cleanup: self.dpmi_retf_cleanup,
            last_exception_error_code: self.last_exception_error_code,
            last_interrupt_had_error_code: self.last_interrupt_had_error_code,
            dispatching_exception: self.dispatching_exception,
            dos_extender_active: self.dos_extender_active,
            rep_fetch_latch: self.rep_fetch_latch,
        }
    }
    /// Restore persistent CPU state from a snapshot.
    ///
    /// Does NOT restore: `instr_cache` (caller should flush after restore),
    /// `ret_guard_stack`, `diag_ring`, `dpmi_exception_diag`, loop-detection
    /// counters, or cache statistics.
    ///
    /// Returns a validation error without changing any CPU field when the
    /// serialized register file violates CPL or virtual-8086 invariants.
    pub fn restore_state(&mut self, snap: &CpuSnapshot) -> Result<(), CpuSnapshotValidationError> {
        snap.validate()?;
        self.regs = snap.regs.clone();
        self.fpu = snap.fpu.clone();
        self.fpu.finish_external_restore();
        self.sse = snap.sse;
        self.sse.mxcsr_mask = identity_mxcsr_mask(snap.cpu_type);
        self.cycles = snap.cycles;
        self.sysenter_cs = snap.sysenter_cs;
        self.sysenter_esp = snap.sysenter_esp;
        self.sysenter_eip = snap.sysenter_eip;
        self.tsc_write_offset = snap.tsc_write_offset;
        self.memory_type_msrs = snap.memory_type_msrs.clone();
        self.tsc_epoch_cycles = snap.tsc_epoch_cycles;
        self.tsc_epoch_ticks = snap.tsc_epoch_ticks;
        self.tsc_ticks_per_cycle_q32 = snap.tsc_ticks_per_cycle_q32;
        self.tsc_clock_hz = snap.tsc_clock_hz;
        self.tsc_cycle_hz = snap.tsc_cycle_hz;
        self.halted = snap.halted;
        self.interrupts_enabled = snap.interrupts_enabled;
        self.interrupt_inhibit = snap.interrupt_inhibit;
        self.cpu_type = snap.cpu_type;
        self.cs_bits = snap.cs_bits;
        self.allow_null_cs = snap.allow_null_cs;
        self.allow_pm_real_mode_ss = snap.allow_pm_real_mode_ss;
        self.dpmi_active = snap.dpmi_active;
        self.hle_break_selectors = snap.hle_break_selectors;
        self.hle_break_count = snap
            .hle_break_count
            .min(self.hle_break_selectors.len() as u8);
        self.thunk_return_selector = snap.thunk_return_selector;
        self.last_effective_instructions = snap.last_effective_instructions;
        self.last_iret = snap.last_iret;
        self.last_retf = snap.last_retf;
        self.dpmi_retf_cleanup = snap.dpmi_retf_cleanup;
        self.last_exception_error_code = snap.last_exception_error_code;
        self.last_interrupt_had_error_code = snap.last_interrupt_had_error_code;
        self.dispatching_exception = snap.dispatching_exception;
        self.dos_extender_active = snap.dos_extender_active;
        self.rep_fetch_latch = snap.rep_fetch_latch;
        Ok(())
    }
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::x87_ops::F80;
    use crate::{CpuError, DataSegReg, MemoryAccess, PageAccess, PagedMemory};
    /// `set_es_real_mode` must mirror a 386 real-mode segment load: update the
    /// selector and cache BASE only, preserving the cached limit/access rights.
    /// Replacing the whole cache truncated an unreal-mode flat ES limit and
    /// sent SPEEDSYS's extended-memory `rep movsd` into an endless #GP loop.
    #[test]
    fn set_es_real_mode_preserves_unreal_flat_limit() {
        let mut cpu = Cpu::new();
        let mut flat = SegmentCache::real_mode(0x0000);
        flat.set_limit(0xFFFFF);
        flat.set_granularity(true);
        cpu.regs.es = 0x0000;
        cpu.regs.es_cache = flat;
        assert_eq!(cpu.regs.es_cache.effective_limit(), 0xFFFF_FFFF);
        cpu.set_es_real_mode(0xF000);
        assert_eq!(cpu.regs.es, 0xF000);
        assert_eq!(cpu.regs.es_cache.base, 0xF0000, "base must track selector");
        assert_eq!(
            cpu.regs.es_cache.effective_limit(),
            0xFFFF_FFFF,
            "real-mode ES load must preserve the cached (unreal) limit"
        );
    }
    impl crate::DeliveryMemory<crate::NoObserver> for TestMemory {
        fn translate_linear_supervisor_checked(
            &mut self,
            cpu: &Cpu,
            linear: u32,
            access: PageAccess,
        ) -> crate::Result<usize> {
            cpu.translate_linear_checked(self, linear, access)
        }
        fn usable_len(&self) -> usize {
            self.data.len()
        }
    }

    fn real_idtr_deliver(cpu: &mut Cpu, memory: &mut TestMemory, path: u8) -> crate::Result<()> {
        match path {
            0 => cpu.interrupt_software::<_, crate::NoObserver>(memory, 0x30),
            1 => cpu.interrupt_hardware_irq_with_irq::<_, crate::NoObserver>(memory, 0x30, Some(4)),
            _ => cpu.exception_with_error::<_, crate::NoObserver>(memory, 0x30, 0x1234),
        }
    }

    #[test]
    fn real_idtr_relocated_delivery() {
        for path in 0..3 {
            let mut cpu = Cpu::new();
            cpu.regs.idtr.base = 0x2000;
            cpu.regs.idtr.limit = 0x3ff;
            cpu.regs.set_sp(0x900);
            let mut memory = TestMemory::new(0x10000);
            memory.write_u32(0xc0, 0x0120_0340);
            memory.write_u32(0x20c0, 0x0560_0780);
            real_idtr_deliver(&mut cpu, &mut memory, path).unwrap();
            assert_eq!((cpu.regs.cs, cpu.regs.ip()), (0x560, 0x780), "path {path}");
        }
        for cpu_type in [
            CpuType::I80386,
            CpuType::I8086,
            CpuType::I8088,
            CpuType::I80186,
        ] {
            let mut cpu = Cpu::new();
            assert_eq!(cpu.regs.idtr.limit, 0x3ff);
            cpu.cpu_type = cpu_type;
            cpu.regs.set_sp(0x900);
            if cpu_type != CpuType::I80386 {
                cpu.regs.idtr.base = 0x2000;
                cpu.regs.idtr.limit = 0;
            }
            let mut memory = TestMemory::new(0x10000);
            memory.write_u32(0xc0, 0x0120_0340);
            real_idtr_deliver(&mut cpu, &mut memory, 0).unwrap();
            assert_eq!((cpu.regs.cs, cpu.regs.ip()), (0x120, 0x340));
        }
        let mut cpu = Cpu::new();
        cpu.regs.idtr.base = 0x2000;
        cpu.regs.idtr.limit = 0xc2;
        let mut memory = TestMemory::new(0x10000);
        memory.forbidden_reads = Some(0x20c0..0x20c4);
        let before_regs = format!("{:?}", cpu.regs);
        let before_memory = memory.data.clone();
        assert!(matches!(
            cpu.real_mode_vector(&memory, 0x30),
            Err(CpuError::Interrupt(8))
        ));
        assert_eq!(format!("{:?}", cpu.regs), before_regs);
        assert_eq!(memory.data, before_memory);
        cpu.cpu_type = CpuType::I80486;
        cpu.last_exception_error_code = 0x1234;
        assert!(matches!(
            real_idtr_deliver(&mut cpu, &mut memory, 0),
            Err(CpuError::GeneralProtectionFault)
        ));
        assert_eq!(cpu.last_exception_error_code, 0);
    }

    #[test]
    fn real_idtr_limit_double_fault_rescue() {
        for path in 0..3 {
            let mut cpu = Cpu::new();
            cpu.regs.idtr.base = 0x2000;
            cpu.regs.idtr.limit = 0xc2; // last byte of requested entry unavailable
            cpu.regs.cs = 0x120;
            cpu.regs.set_ip(0x456);
            cpu.regs.set_sp(0x900);
            let flags = cpu.regs.flags.bits();
            let mut memory = TestMemory::new(0x10000);
            memory.write_u32(0xc0, 0x0120_0340);
            memory.write_u32(0x20c0, 0x0560_0780);
            memory.write_u32(0x2020, 0x0320_0540);
            real_idtr_deliver(&mut cpu, &mut memory, path).unwrap();
            assert_eq!((cpu.regs.cs, cpu.regs.ip()), (0x320, 0x540), "path {path}");
            assert_eq!(cpu.regs.sp(), 0x8fa);
            assert_eq!(memory.read_u16(0x8fa), 0x456);
            assert_eq!(memory.read_u16(0x8fc), 0x120);
            assert_eq!(memory.read_u16(0x8fe), flags);
        }
    }

    #[test]
    fn real_idtr_limit_shutdown_preserves_state() {
        for path in 0..3 {
            let mut cpu = Cpu::new();
            cpu.regs.idtr.base = 0x2000;
            cpu.regs.idtr.limit = 0x22; // even vector eight is incomplete
            cpu.regs.set_sp(0x900);
            let before_regs = format!("{:?}", cpu.regs);
            let mut memory = TestMemory::new(0x10000);
            memory.data.fill(0xa5);
            let before_memory = memory.data.clone();
            let result = real_idtr_deliver(&mut cpu, &mut memory, path);
            let Err(CpuError::TripleFault(info)) = result else {
                panic!("path {path}: {result:?}")
            };
            assert_eq!(info.vector, 0x30);
            assert_eq!(info.irq, if path == 1 { Some(4) } else { None });
            assert_eq!(info.error_code, if path == 2 { Some(0x1234) } else { None });
            assert_eq!(format!("{:?}", cpu.regs), before_regs);
            assert_eq!(memory.data, before_memory);
        }
    }

    fn real_interrupt_failure_matrix(path: u8) {
        let mut failures = Vec::new();
        for (cpu_type, sp) in [
            (CpuType::I80386, 1_u16),
            (CpuType::I80386, 3),
            (CpuType::I80386, 5),
            (CpuType::I80386, 0),
            (CpuType::I80386, 2),
            (CpuType::I80386, 4),
            (CpuType::I80386, 0x100),
            (CpuType::I8086, 1),
        ] {
            let mut cpu = Cpu::new();
            cpu.cpu_type = cpu_type;
            cpu.regs.cs = 0x120;
            cpu.regs.cs_cache = SegmentCache::real_mode(0x120);
            cpu.regs.ss = 0x200;
            cpu.regs.ss_cache = SegmentCache::real_mode(0x200);
            cpu.regs.set_ip(0x456);
            cpu.regs.esp = 0xABCD_0000 | u32::from(sp);
            cpu.regs.flags.set(Flags::INTERRUPT, true);
            cpu.regs.flags.set(Flags::TRAP, true);
            cpu.interrupts_enabled = true;
            let mut memory = TestMemory::new(0x20000);
            memory.data.fill(0xA5);
            memory.write_u16(0x30 * 4, 0x789);
            memory.write_u16(0x30 * 4 + 2, 0x340);
            let before_regs = format!("{:?}", cpu.regs);
            let before_memory = memory.data.clone();
            let mut expected_regs = cpu.regs.clone();
            let mut expected_memory = before_memory.clone();
            let result = match path {
                0 => cpu.interrupt::<_, crate::NoObserver>(&mut memory, 0x30),
                1 => cpu.interrupt_software::<_, crate::NoObserver>(&mut memory, 0x30),
                2 => cpu.interrupt_hardware_irq_with_irq::<_, crate::NoObserver>(
                    &mut memory,
                    0x30,
                    Some(4),
                ),
                3 => cpu.exception::<_, crate::NoObserver>(&mut memory, 0x30),
                _ => cpu.exception_with_error::<_, crate::NoObserver>(&mut memory, 0x30, 0x1234),
            };
            if cpu_type == CpuType::I80386 && matches!(sp, 1 | 3 | 5) {
                let Err(CpuError::TripleFault(info)) = result else {
                    failures.push(format!(
                        "path {path}, SP {sp}: expected shutdown, got {result:?}"
                    ));
                    continue;
                };
                assert_eq!(
                    info.origin,
                    match path {
                        0 => crate::TripleFaultOrigin::ExternalInterrupt,
                        1 => crate::TripleFaultOrigin::SoftwareInterrupt,
                        2 => crate::TripleFaultOrigin::HardwareIrq,
                        _ => crate::TripleFaultOrigin::CpuException,
                    }
                );
                assert_eq!(info.vector, 0x30);
                assert_eq!(info.error_code, if path == 4 { Some(0x1234) } else { None });
                assert_eq!(info.irq, if path == 2 { Some(4) } else { None });
                assert_eq!(
                    format!("{:?}", cpu.regs),
                    before_regs,
                    "path {path}, SP {sp}"
                );
                assert!(cpu.interrupts_enabled);
                assert_eq!(cpu.cs_bits, 16);
                assert_eq!(
                    memory.data, before_memory,
                    "partial frame at path {path}, SP {sp}"
                );
            } else {
                result.unwrap();
                let frame = [
                    expected_regs.ip(),
                    expected_regs.cs,
                    expected_regs.flags.bits(),
                ];
                for (i, word) in frame.into_iter().enumerate() {
                    for (byte_index, byte) in word.to_le_bytes().into_iter().enumerate() {
                        let offset = sp
                            .wrapping_sub(6)
                            .wrapping_add(i as u16 * 2 + byte_index as u16);
                        expected_memory[0x2000 + usize::from(offset)] = byte;
                    }
                }
                expected_regs.set_sp(sp.wrapping_sub(6));
                expected_regs.set_ip(0x789);
                expected_regs.cs = 0x340;
                expected_regs.cs_cache.base = 0x3400;
                expected_regs.flags.set(Flags::INTERRUPT, false);
                expected_regs.flags.set(Flags::TRAP, false);
                assert_eq!(format!("{:?}", cpu.regs), format!("{expected_regs:?}"));
                assert!(!cpu.interrupts_enabled);
                assert_eq!(cpu.cs_bits, 16);
                assert_eq!(memory.data, expected_memory);
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("; "));
    }
    #[test]
    fn real_interrupt_failure_external() {
        real_interrupt_failure_matrix(0);
    }
    #[test]
    fn real_interrupt_failure_software() {
        real_interrupt_failure_matrix(1);
    }
    #[test]
    fn real_interrupt_failure_hardware() {
        real_interrupt_failure_matrix(2);
    }
    #[test]
    fn real_interrupt_failure_exception() {
        real_interrupt_failure_matrix(3);
    }
    #[test]
    fn real_interrupt_failure_exception_with_error() {
        real_interrupt_failure_matrix(4);
    }

    struct TestMemory {
        data: Vec<u8>,
        forbidden_reads: Option<std::ops::Range<usize>>,
        linear_map: HashMap<u32, usize>,
        page_fault: Option<(u32, u32)>,
        tlb: HashMap<u32, (u32, bool, bool, bool)>,
        /// EXP-N005 instrumentation: every frame the walk recorded, in order.
        recorded_pt_frames: Vec<u32>,
        /// EXP-N005 instrumentation: physical addresses of walker A/D
        /// write-backs routed through `write_u32_physical_ad`.
        ad_writebacks: Vec<usize>,
    }
    impl TestMemory {
        fn new(size: usize) -> Self {
            Self {
                data: vec![0; size],
                forbidden_reads: None,
                linear_map: HashMap::new(),
                page_fault: None,
                tlb: HashMap::new(),
                recorded_pt_frames: Vec::new(),
                ad_writebacks: Vec::new(),
            }
        }
        fn map_linear(&mut self, linear: u32, phys: usize) {
            self.linear_map.insert(linear, phys);
        }
        fn insert_tlb(
            &mut self,
            linear_page: u32,
            physical_page: u32,
            writable: bool,
            user_allowed: bool,
        ) {
            self.tlb
                .insert(linear_page, (physical_page, writable, user_allowed, false));
        }
        fn insert_tlb_dirty(
            &mut self,
            linear_page: u32,
            physical_page: u32,
            writable: bool,
            user_allowed: bool,
            dirty: bool,
        ) {
            self.tlb
                .insert(linear_page, (physical_page, writable, user_allowed, dirty));
        }
    }
    impl MemoryAccess for TestMemory {
        fn read_u8(&self, addr: usize) -> u8 {
            assert!(
                !self
                    .forbidden_reads
                    .as_ref()
                    .is_some_and(|range| range.contains(&addr)),
                "IRETD read beyond the three-dword real-mode frame at {addr:#x}"
            );
            self.data[addr]
        }
        fn read_u16(&self, addr: usize) -> u16 {
            u16::from_le_bytes([self.read_u8(addr), self.read_u8(addr + 1)])
        }
        fn read_u32(&self, addr: usize) -> u32 {
            u32::from_le_bytes([
                self.read_u8(addr),
                self.read_u8(addr + 1),
                self.read_u8(addr + 2),
                self.read_u8(addr + 3),
            ])
        }
        fn write_u8(&mut self, addr: usize, value: u8) {
            self.data[addr] = value;
        }
        fn write_u16(&mut self, addr: usize, value: u16) {
            let bytes = value.to_le_bytes();
            self.write_u8(addr, bytes[0]);
            self.write_u8(addr + 1, bytes[1]);
        }
        fn write_u32(&mut self, addr: usize, value: u32) {
            let bytes = value.to_le_bytes();
            for (offset, byte) in bytes.iter().enumerate() {
                self.write_u8(addr + offset, *byte);
            }
        }
        fn read_u64(&self, addr: usize) -> u64 {
            u64::from_le_bytes([
                self.read_u8(addr),
                self.read_u8(addr + 1),
                self.read_u8(addr + 2),
                self.read_u8(addr + 3),
                self.read_u8(addr + 4),
                self.read_u8(addr + 5),
                self.read_u8(addr + 6),
                self.read_u8(addr + 7),
            ])
        }
        fn write_u64(&mut self, addr: usize, value: u64) {
            let bytes = value.to_le_bytes();
            for (offset, byte) in bytes.iter().enumerate() {
                self.write_u8(addr + offset, *byte);
            }
        }
    }
    impl PagedMemory for TestMemory {
        fn map_linear_physical(&self, linear: u32) -> Option<usize> {
            self.linear_map.get(&linear).copied()
        }
        fn tlb_lookup(&self, linear_page: u32) -> Option<(u32, bool, bool, bool)> {
            self.tlb.get(&linear_page).copied()
        }
        fn tlb_insert(
            &mut self,
            linear_page: u32,
            physical_page: u32,
            writable: bool,
            user_allowed: bool,
            dirty: bool,
        ) {
            self.tlb
                .insert(linear_page, (physical_page, writable, user_allowed, dirty));
        }
        fn read_u32_physical(&self, addr: usize) -> u32 {
            self.read_u32(addr)
        }
        fn read_u8_physical_checked(&self, addr: usize) -> Option<u8> {
            self.data.get(addr).map(|_| self.read_u8(addr))
        }
        fn write_u32_physical(&mut self, addr: usize, value: u32) {
            self.write_u32(addr, value);
        }
        fn set_page_fault(&mut self, linear_address: u32, error_code: u32) {
            self.page_fault = Some((linear_address, error_code));
        }
        fn clear_page_fault(&mut self) {
            self.page_fault = None;
        }
        fn page_fault_pending(&self) -> bool {
            self.page_fault.is_some()
        }
        fn page_fault_address(&self) -> u32 {
            self.page_fault.map(|(addr, _)| addr).unwrap_or(0)
        }
        fn page_fault_error_code(&self) -> u32 {
            self.page_fault.map(|(_, code)| code).unwrap_or(0)
        }
        fn invalidate_tlb(&mut self) {}
        fn invalidate_tlb_page(&mut self, _: u32) {}
        fn record_pt_frame(&mut self, physical_page: u32) {
            self.recorded_pt_frames.push(physical_page);
        }
        fn write_u32_physical_ad(&mut self, addr: usize, value: u32) {
            self.ad_writebacks.push(addr);
            self.write_u32(addr, value);
        }
    }
    fn write_descriptor(mem: &mut TestMemory, addr: usize, desc: [u8; 8]) {
        for (offset, byte) in desc.iter().enumerate() {
            mem.write_u8(addr + offset, *byte);
        }
    }
    fn map_page(
        mem: &mut TestMemory,
        cr3: usize,
        page_table: usize,
        linear: u32,
        physical: usize,
        flags: u32,
    ) {
        let pde_index = (linear >> 22) as usize;
        let pte_index = ((linear >> 12) & 0x03FF) as usize;
        mem.write_u32(cr3 + pde_index * 4, page_table as u32 | flags | 1);
        mem.write_u32(page_table + pte_index * 4, physical as u32 | flags | 1);
    }
    const STACK_CR3: usize = 0x2000;
    const STACK_PT: usize = 0x3000;
    const STACK_PHYS_A: usize = 0x10000;
    const STACK_PHYS_B: usize = 0x40000;
    fn paged_stack_cpu() -> Cpu {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 0x8001_0001;
        cpu.regs.cr3 = STACK_CR3 as u32;
        cpu.regs.cpl = 0;
        cpu.regs.ss = 0x18;
        let cache = &mut cpu.regs.ss_cache;
        cache.base = 0;
        cache.set_limit(0x000F_FFFF);
        cache.set_granularity(true);
        cache.present = true;
        cache.is_code = false;
        cache.is_readable = true;
        cache.is_writable = true;
        cache.access = 0x93;
        cache.big = true;
        cpu
    }
    fn map_stack_boundary(mem: &mut TestMemory) {
        map_page(mem, STACK_CR3, STACK_PT, 0x1000, STACK_PHYS_A, 0x02);
        map_page(mem, STACK_CR3, STACK_PT, 0x2000, STACK_PHYS_B, 0x02);
    }
    #[test]
    fn try_pop32_reads_nonadjacent_physical_pages() {
        let mut cpu = paged_stack_cpu();
        let mut mem = TestMemory::new(0x5_0000);
        map_stack_boundary(&mut mem);
        cpu.regs.esp = 0x1FFE;
        mem.write_u8(STACK_PHYS_A + 0xFFE, 0x78);
        mem.write_u8(STACK_PHYS_A + 0xFFF, 0x56);
        mem.write_u8(STACK_PHYS_B, 0x34);
        mem.write_u8(STACK_PHYS_B + 1, 0x12);
        assert_eq!(cpu.try_pop32(&mut mem).unwrap(), 0x1234_5678);
        assert_eq!(cpu.regs.esp, 0x2002);
    }
    #[test]
    fn try_push32_writes_nonadjacent_physical_pages() {
        let mut cpu = paged_stack_cpu();
        let mut mem = TestMemory::new(0x5_0000);
        map_stack_boundary(&mut mem);
        cpu.regs.esp = 0x2002;
        cpu.try_push32(&mut mem, 0xA1B2_C3D4).unwrap();
        assert_eq!(cpu.regs.esp, 0x1FFE);
        assert_eq!(mem.read_u8(STACK_PHYS_A + 0xFFE), 0xD4);
        assert_eq!(mem.read_u8(STACK_PHYS_A + 0xFFF), 0xC3);
        assert_eq!(mem.read_u8(STACK_PHYS_B), 0xB2);
        assert_eq!(mem.read_u8(STACK_PHYS_B + 1), 0xA1);
    }
    #[test]
    fn page_walk_records_pt_frames_at_read_time_and_excludes_ad_writebacks() {
        let cpu = paged_stack_cpu();
        let mut mem = TestMemory::new(0x5_0000);
        map_stack_boundary(&mut mem);
        cpu.translate_paging_checked(&mut mem, 0x1000, crate::PageAccess::Read)
            .expect("mapped page translates");
        assert_eq!(
            mem.recorded_pt_frames,
            vec![(STACK_CR3 >> 12) as u32, (STACK_PT >> 12) as u32],
            "a successful walk records the directory frame at the PDE read and \
             the table frame at the PTE read"
        );
        assert_eq!(
            mem.ad_writebacks,
            vec![STACK_CR3, STACK_PT + 4],
            "accessed-bit write-backs must take the excluded A/D channel (A3)"
        );
        mem.recorded_pt_frames.clear();
        cpu.translate_paging_checked(&mut mem, 0x0080_0000, crate::PageAccess::Read)
            .expect_err("unmapped PDE faults");
        assert_eq!(
            mem.recorded_pt_frames,
            vec![(STACK_CR3 >> 12) as u32],
            "an Err(PageFault) walk still records the frames it read"
        );
        mem.recorded_pt_frames.clear();
        cpu.translate_paging_checked(&mut mem, 0x3000, crate::PageAccess::Read)
            .expect_err("unmapped PTE faults");
        assert_eq!(
            mem.recorded_pt_frames,
            vec![(STACK_CR3 >> 12) as u32, (STACK_PT >> 12) as u32],
            "a PTE-level fault records the directory AND table frames"
        );
    }
    #[test]
    fn try_pop16_faults_on_absent_second_page_without_advancing_esp() {
        let mut cpu = paged_stack_cpu();
        let mut mem = TestMemory::new(0x5_0000);
        map_page(&mut mem, STACK_CR3, STACK_PT, 0x1000, STACK_PHYS_A, 0x02);
        cpu.regs.esp = 0x1FFF;
        mem.write_u8(STACK_PHYS_A + 0xFFF, 0x5A);
        let error = cpu.try_pop16(&mut mem).unwrap_err();
        let crate::CpuError::PageFault(info) = error else {
            panic!("expected second-page #PF, got {error:?}");
        };
        assert_eq!(info.linear_address, 0x2000);
        assert_eq!(cpu.regs.esp, 0x1FFF);
    }
    #[test]
    fn try_push16_faults_atomically_on_read_only_second_page() {
        let mut cpu = paged_stack_cpu();
        let mut mem = TestMemory::new(0x5_0000);
        map_stack_boundary(&mut mem);
        let second_pte = STACK_PT + 2 * 4;
        mem.write_u32(second_pte, STACK_PHYS_B as u32 | 1);
        cpu.regs.esp = 0x2001;
        mem.write_u8(STACK_PHYS_A + 0xFFF, 0x11);
        mem.write_u8(STACK_PHYS_B, 0x22);
        let error = cpu.try_push16(&mut mem, 0xABCD).unwrap_err();
        let crate::CpuError::PageFault(info) = error else {
            panic!("expected second-page write #PF, got {error:?}");
        };
        assert_eq!(info.linear_address, 0x2000);
        assert_eq!(info.error_code & 0x03, 0x03);
        assert_eq!(cpu.regs.esp, 0x2001);
        assert_eq!(mem.read_u8(STACK_PHYS_A + 0xFFF), 0x11);
        assert_eq!(mem.read_u8(STACK_PHYS_B), 0x22);
    }
    #[test]
    fn with_iret_flags_mut_skips_wrapped_32bit_stack_span() {
        let mut cpu = paged_stack_cpu();
        let mut mem = TestMemory::new(0x5_0000);
        map_page(
            &mut mem,
            STACK_CR3,
            STACK_PT,
            0xFFFF_F000,
            STACK_PHYS_A,
            0x02,
        );
        cpu.regs.esp = 0xFFFF_FFF7;
        mem.write_u8(STACK_PHYS_A + 0xFFF, 0x02);
        mem.write_u8(STACK_PHYS_A + 0x1000, 0x00);
        let mut called = false;
        let mut mutate = |flags: &mut u16| {
            called = true;
            *flags |= 1;
        };
        cpu.with_iret_flags_mut_ex(&mut mem, &mut mutate, false)
            .unwrap();
        assert!(!called);
        assert_eq!(mem.read_u8(STACK_PHYS_A + 0xFFF), 0x02);
        assert_eq!(mem.read_u8(STACK_PHYS_A + 0x1000), 0x00);
    }
    const X87_CR3: usize = 0x2000;
    const X87_PT: usize = 0x3000;
    const X87_PHYS_A: usize = 0x10000;
    const X87_PHYS_B: usize = 0x40000;
    const X87_LIN: u32 = 0x0040_1FFC;
    /// A 32-bit protected-mode CPU with paging on and a flat, present, writable
    /// DS (selector 0x10, base 0, 4 GiB limit).
    fn x87_paged_cpu() -> Cpu {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = X87_CR3 as u32;
        cpu.regs.cpl = 0;
        cpu.cs_bits = 32;
        cpu.fpu.control_word = 0x037F;
        cpu.regs.ds = 0x10;
        let c = &mut cpu.regs.ds_cache;
        c.base = 0;
        c.set_granularity(true);
        c.set_limit(0x000F_FFFF);
        c.present = true;
        c.is_code = false;
        c.is_writable = true;
        c.is_readable = true;
        c.big = true;
        cpu
    }
    fn decode_fpu32(bytes: &[u8]) -> crate::x87_decode::DecodedX87 {
        crate::x87_decode::decode_x87(bytes, crate::x87_decode::CodeBitness::Bits32).unwrap()
    }
    #[test]
    fn test_x87_fld_m64_crosses_page_reads_correct_physical_pages() {
        let mut mem = TestMemory::new(0x5_0000);
        map_page(&mut mem, X87_CR3, X87_PT, 0x0040_1000, X87_PHYS_A, 0x02);
        map_page(&mut mem, X87_CR3, X87_PT, 0x0040_2000, X87_PHYS_B, 0x02);
        let bits = std::f64::consts::PI.to_bits();
        let le = bits.to_le_bytes();
        for (i, &b) in le[0..4].iter().enumerate() {
            mem.write_u8(X87_PHYS_A + 0xFFC + i, b);
        }
        for (i, &b) in le[4..8].iter().enumerate() {
            mem.write_u8(X87_PHYS_B + i, b);
        }
        for i in 0..4 {
            mem.write_u8(X87_PHYS_A + 0x1000 + i, 0xFF);
        }
        let mut cpu = x87_paged_cpu();
        cpu.regs.eax = X87_LIN;
        let instr = decode_fpu32(&[0xDD, 0x00]);
        crate::fpu::execute_decoded(&mut cpu, &instr, &mut mem)
            .expect("cross-page FLD must not fault");
        assert!(!mem.page_fault_pending());
        assert_eq!(
            cpu.fpu.get_st(0).to_f64().to_bits(),
            bits,
            "cross-page FLD must assemble the operand from BOTH physical pages"
        );
    }
    #[test]
    fn test_x87_fstp_m64_crosses_page_writes_correct_physical_pages() {
        let mut mem = TestMemory::new(0x5_0000);
        map_page(&mut mem, X87_CR3, X87_PT, 0x0040_1000, X87_PHYS_A, 0x02);
        map_page(&mut mem, X87_CR3, X87_PT, 0x0040_2000, X87_PHYS_B, 0x02);
        for i in 0..4 {
            mem.write_u8(X87_PHYS_A + 0x1000 + i, 0xAA);
        }
        let mut cpu = x87_paged_cpu();
        cpu.regs.eax = X87_LIN;
        cpu.fpu.push(F80::from_f64(std::f64::consts::PI));
        let instr = decode_fpu32(&[0xDD, 0x18]);
        crate::fpu::execute_decoded(&mut cpu, &instr, &mut mem)
            .expect("cross-page FSTP must not fault");
        let le = std::f64::consts::PI.to_bits().to_le_bytes();
        for (i, &b) in le[0..4].iter().enumerate() {
            assert_eq!(
                mem.read_u8(X87_PHYS_A + 0xFFC + i),
                b,
                "low byte {i} -> page A"
            );
        }
        for (i, &b) in le[4..8].iter().enumerate() {
            assert_eq!(mem.read_u8(X87_PHYS_B + i), b, "high byte {i} -> page B");
        }
        for i in 0..4 {
            assert_eq!(
                mem.read_u8(X87_PHYS_A + 0x1000 + i),
                0xAA,
                "adjacent page byte {i}"
            );
        }
    }
    #[test]
    fn test_x87_fld_m64_faults_when_second_page_absent() {
        let mut mem = TestMemory::new(0x5_0000);
        map_page(&mut mem, X87_CR3, X87_PT, 0x0040_1000, X87_PHYS_A, 0x02);
        let mut cpu = x87_paged_cpu();
        cpu.regs.eax = X87_LIN;
        let top_before = cpu.fpu.get_top();
        let instr = decode_fpu32(&[0xDD, 0x00]);
        let result = crate::fpu::execute_decoded(&mut cpu, &instr, &mut mem);
        assert!(result.is_err(), "FLD across an absent page must fault");
        assert!(
            mem.page_fault_pending(),
            "the absent second page must raise #PF"
        );
        assert_eq!(cpu.fpu.get_top(), top_before, "faulted FLD must not push");
    }
    #[test]
    fn test_x87_fstp_m64_absent_second_page_leaves_no_partial_write() {
        let mut mem = TestMemory::new(0x5_0000);
        map_page(&mut mem, X87_CR3, X87_PT, 0x0040_1000, X87_PHYS_A, 0x02);
        let mut cpu = x87_paged_cpu();
        cpu.regs.eax = X87_LIN;
        cpu.fpu.push(F80::from_f64(std::f64::consts::PI));
        let top_before = cpu.fpu.get_top();
        let instr = decode_fpu32(&[0xDD, 0x18]);
        let result = crate::fpu::execute_decoded(&mut cpu, &instr, &mut mem);
        assert!(result.is_err(), "FSTP across an absent page must fault");
        assert!(
            mem.page_fault_pending(),
            "the absent second page must raise #PF"
        );
        for i in 0..4 {
            assert_eq!(
                mem.read_u8(X87_PHYS_A + 0xFFC + i),
                0,
                "faulted FSTP must not partially write the first page (byte {i})"
            );
        }
        assert_eq!(cpu.fpu.get_top(), top_before, "faulted FSTP must not pop");
    }
    /// The funnel's second walk is gone; pass 2 replays what pass 1 recorded.
    /// So pass 1 is now the ONLY thing that can set a page's dirty bit, and
    /// this is the test that says so: a warm, READ-filled TLB entry (present,
    /// writable, NOT dirty) forces the write translation back through the
    /// page walk, which must OR D into the leaf PTE. Nothing else in this
    /// tree covers A/D end to end with an isolated page-table fixture.
    #[test]
    fn x87_store_sets_the_pte_dirty_bit_through_a_clean_warm_tlb_entry() {
        let mut mem = TestMemory::new(0x5_0000);
        map_page(&mut mem, X87_CR3, X87_PT, 0x0040_1000, X87_PHYS_A, 0x02);
        mem.insert_tlb(0x0040_1000 >> 12, (X87_PHYS_A >> 12) as u32, true, true);
        let pte_addr = X87_PT + (((0x0040_1000u32 >> 12) & 0x3FF) as usize) * 4;
        assert_eq!(
            mem.read_u32(pte_addr) & (1 << 6),
            0,
            "precondition: the leaf PTE starts with D clear"
        );
        let mut cpu = x87_paged_cpu();
        cpu.regs.eax = 0x0040_1000;
        cpu.fpu.push(F80::from_f64(std::f64::consts::PI));
        let instr = decode_fpu32(&[0xDD, 0x18]);
        crate::fpu::execute_decoded(&mut cpu, &instr, &mut mem).expect("store must not fault");
        let pte = mem.read_u32(pte_addr);
        assert_ne!(pte & (1 << 6), 0, "the write walk must set the PTE D bit");
        assert_ne!(pte & (1 << 5), 0, "...and the A bit");
        assert!(
            !mem.ad_writebacks.is_empty(),
            "the D update must go out through the A/D write-back route"
        );
        assert_eq!(
            mem.read_u64(X87_PHYS_A),
            std::f64::consts::PI.to_bits(),
            "and the operand itself must still land"
        );
    }
    /// V86 with paging: the funnel resolves the linear address through the
    /// real-mode `segment << 4` arm, not the protected-mode hidden cache.
    /// Regression armor from the panel's F2 finding — the rejected v1 design
    /// re-derived it the protected-mode way and corrupted exactly this shape.
    /// The two linear pages map to NON-adjacent physical pages, so a commit
    /// that translated once and ran on would show up immediately.
    #[test]
    fn v86_paged_x87_store_splits_across_non_adjacent_physical_pages() {
        const V86_SEG: u16 = 0x4000;
        const V86_OFF: u16 = 0x1FFC;
        let mut mem = TestMemory::new(0x5_0000);
        map_page(&mut mem, X87_CR3, X87_PT, 0x0004_1000, X87_PHYS_A, 0x06);
        map_page(&mut mem, X87_CR3, X87_PT, 0x0004_2000, X87_PHYS_B, 0x06);
        for i in 0..4 {
            mem.write_u8(X87_PHYS_A + 0x1000 + i, 0xAA);
        }
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = X87_CR3 as u32;
        cpu.regs.cpl = 3;
        cpu.cs_bits = 16;
        cpu.regs.set_v86_mode(true);
        cpu.fpu.control_word = 0x037F;
        cpu.regs.ds = V86_SEG;
        cpu.regs.ds_cache.base = u32::from(V86_SEG) << 4;
        cpu.regs.ds_cache.set_limit(0xFFFF);
        cpu.regs.ds_cache.present = true;
        cpu.regs.ds_cache.is_writable = true;
        cpu.regs.ds_cache.is_readable = true;
        cpu.regs.set_bx(V86_OFF);
        cpu.fpu.push(F80::from_f64(std::f64::consts::E));
        assert!(cpu.is_v86_mode() && !cpu.is_protected_mode());
        let instr =
            crate::x87_decode::decode_x87(&[0xDD, 0x1F], crate::x87_decode::CodeBitness::Bits16)
                .expect("FSTP qword ptr [bx]");
        crate::fpu::execute_decoded(&mut cpu, &instr, &mut mem)
            .expect("V86 paged store must not fault");
        let le = std::f64::consts::E.to_bits().to_le_bytes();
        for (i, &b) in le[0..4].iter().enumerate() {
            assert_eq!(mem.read_u8(X87_PHYS_A + 0xFFC + i), b, "low byte {i}");
        }
        for (i, &b) in le[4..8].iter().enumerate() {
            assert_eq!(mem.read_u8(X87_PHYS_B + i), b, "high byte {i}");
        }
        for i in 0..4 {
            assert_eq!(
                mem.read_u8(X87_PHYS_A + 0x1000 + i),
                0xAA,
                "physically adjacent page byte {i} must be untouched"
            );
        }
    }
    #[test]
    fn test_cpu_new() {
        let cpu = Cpu::new();
        assert_eq!(cpu.cycles, 0);
        assert!(!cpu.halted);
        assert!(!cpu.interrupts_enabled);
        assert_eq!(cpu.cpu_type, CpuType::I80386);
        assert_eq!(cpu.cs_bits, 16);
    }
    #[test]
    fn test_cpu_default() {
        let cpu = Cpu::default();
        assert_eq!(cpu.cycles, 0);
    }
    #[test]
    fn test_cpu_reset() {
        let mut cpu = Cpu::new();
        cpu.cycles = 1000;
        cpu.halted = true;
        cpu.regs.cs = 0x1234;
        cpu.regs.set_ip(0x5678);
        cpu.reset();
        assert_eq!(cpu.cycles, 0);
        assert!(!cpu.halted);
        assert_eq!(cpu.regs.cs, 0xF000);
        assert_eq!(cpu.regs.ip(), 0xFFF0);
    }
    #[test]
    fn full_reset_clears_fpu_state() {
        let mut cpu = Cpu::new();
        cpu.fpu.push(F80::from_f64(42.5));
        cpu.fpu.status_word = 0x6D41;
        cpu.fpu.control_word = 0x0272;
        cpu.fpu.tag_word = 0x1234;
        cpu.fpu.ip_offset = 0xDEAD_BEEF;
        cpu.fpu.ip_selector = 0xBEEF;
        cpu.fpu.operand_offset = 0xCAFE_BABE;
        cpu.fpu.operand_selector = 0xCAFE;
        cpu.fpu.x87_save_area.fill(0xA5);
        cpu.reset();
        let reset = Fpu::new();
        assert_eq!(cpu.fpu.regs, reset.regs);
        assert_eq!(cpu.fpu.status_word, reset.status_word);
        assert_eq!(cpu.fpu.control_word, reset.control_word);
        assert_eq!(cpu.fpu.tag_word, reset.tag_word);
        assert_eq!(cpu.fpu.ip_offset, reset.ip_offset);
        assert_eq!(cpu.fpu.ip_selector, reset.ip_selector);
        assert_eq!(cpu.fpu.operand_offset, reset.operand_offset);
        assert_eq!(cpu.fpu.operand_selector, reset.operand_selector);
        assert_eq!(cpu.fpu.x87_save_area, reset.x87_save_area);
    }
    #[test]
    fn test_is_protected_mode() {
        let mut cpu = Cpu::new();
        assert!(!cpu.is_protected_mode());
        assert!(cpu.is_real_mode());
        cpu.regs.cr0 = 1;
        assert!(cpu.is_protected_mode());
        assert!(!cpu.is_real_mode());
    }
    #[test]
    fn test_get_segment_cache_real_mode() {
        let cpu = Cpu::new();
        let cache = cpu.get_segment_cache(0x1234);
        assert_eq!(cache.base, 0x12340);
        assert_eq!(cache.limit, 0xFFFF);
    }
    #[test]
    fn test_get_segment_cache_protected_mode() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ds = 0x0010;
        cpu.regs.ds_cache.base = 0x00100000;
        cpu.regs.ds_cache.set_limit(0x0FFFF);
        let cache = cpu.get_segment_cache(0x0010);
        assert_eq!(cache.base, 0x00100000);
    }
    #[test]
    fn test_cpu_type_set_get() {
        let mut cpu = Cpu::new();
        assert_eq!(cpu.get_cpu_type(), CpuType::I80386);
        cpu.set_cpu_type(CpuType::I80486);
        assert_eq!(cpu.get_cpu_type(), CpuType::I80486);
    }
    #[test]
    fn test_stack_range_ok_real_mode() {
        let cpu = Cpu::new();
        assert!(cpu.stack_range_ok(0, 100));
        assert!(cpu.stack_range_ok(0xFFFF, 1));
    }
    /// MDD-BUG-TEM-00169: a V86 stack operand crossing 0xFFFF faults instead
    /// of wrapping to offset 0 of the same segment.
    #[test]
    fn test_stack_range_ok_v86_honours_the_64k_limit() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.set_v86_mode(true);
        cpu.regs.ss = 0x9000;
        cpu.regs.ss_cache = SegmentCache::real_mode(0x9000);
        assert!(cpu.is_v86_mode());
        assert!(cpu.stack_range_ok(0xFFFE, 2));
        assert!(!cpu.stack_range_ok(0xFFFF, 2));
        let mut mem = TestMemory::new(0x10_0000);
        cpu.regs.set_sp(1);
        assert!(matches!(
            cpu.try_push16(&mut mem, 0x1234),
            Err(crate::CpuError::StackFault)
        ));
        assert_eq!(cpu.regs.sp(), 1, "a faulting push leaves SP alone");
    }
    #[test]
    fn test_stack_range_ok_protected_mode() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.set_limit(0x1000);
        cpu.regs.ss_cache.is_code = false;
        cpu.regs.ss_cache.big = false;
        assert!(cpu.stack_range_ok(0, 0x1000));
        assert!(cpu.stack_range_ok(0x1000, 1));
        assert!(!cpu.stack_range_ok(0x1001, 1));
        assert!(!cpu.stack_range_ok(0, 0x1002));
    }
    #[test]
    fn test_stack_range_ok_expand_down() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.set_limit(0x1000);
        cpu.regs.ss_cache.is_code = false;
        cpu.regs.ss_cache.access |= 0x04;
        cpu.regs.ss_cache.big = false;
        assert!(cpu.stack_range_ok(0x1001, 2));
        assert!(!cpu.stack_range_ok(0x1000, 2));
        assert!(!cpu.stack_range_ok(0xFFFF, 2));
    }
    #[test]
    fn test_translate_address_real_mode() {
        let cpu = Cpu::new();
        assert_eq!(cpu.translate_address(0x1234, 0x5678), 0x12340 + 0x5678);
        assert_eq!(cpu.translate_address(0x0000, 0x0100), 0x100);
        assert_eq!(cpu.translate_address(0xF000, 0xFFF0), 0xFFFF0);
    }
    #[test]
    fn test_translate_address_protected_mode() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ds = 0x0010;
        cpu.regs.ds_cache.base = 0x00100000;
        cpu.regs.ds_cache.set_limit(0xFFFFF);
        assert_eq!(cpu.translate_address(0x0010, 0x1234), 0x00100000 + 0x1234);
        assert_eq!(cpu.translate_address(0x0020, 0x1234), 0x200 + 0x1234);
    }
    #[test]
    fn test_translate_address_full_checked_stack_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss = 0x2000;
        cpu.regs.ss_cache.set_limit(0x000F);
        cpu.regs.ss_cache.is_code = false;
        let err = cpu
            .translate_address_full_checked(&mut mem, cpu.regs.ss, 0x0010, PageAccess::Write)
            .unwrap_err();
        assert!(matches!(err, CpuError::StackFault));
    }
    #[test]
    fn test_translate_address_full_checked_general_protection() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.ds = 0x1234;
        cpu.regs.ds_cache.set_limit(0x000F);
        cpu.regs.ds_cache.is_code = false;
        let err = cpu
            .translate_address_full_checked(&mut mem, cpu.regs.ds, 0x0010, PageAccess::Read)
            .unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_translate_address_full_uses_linear_map() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.ds = 0x1000;
        let linear = cpu.translate_address(cpu.regs.ds, 0x0200);
        mem.map_linear(linear as u32, 0x0100);
        let phys = cpu.translate_address_full(&mut mem, cpu.regs.ds, 0x0200);
        assert_eq!(phys, 0x0100);
    }
    #[test]
    fn test_translate_address_full_uses_descriptor_for_unloaded_selector() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x40000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0010;
        let desc = [0xFF, 0xFF, 0x00, 0x30, 0x12, 0xF2, 0x40, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let phys = cpu.translate_address_full(&mut mem, 0x0008, 0x0020);
        assert_eq!(phys, 0x0012_3000 + 0x0020);
    }
    #[test]
    fn test_translate_address_full_paging_fault_guard_preserves_physical_zero() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x10000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        mem.write_u32(0x1000, 0);
        mem.write_u8(0, 0xA5);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = cpu.translate_address_full(&mut mem, 0x0000, 0x0123);
        }));
        assert!(result.is_err());
        assert_eq!(mem.page_fault_address(), 0x0000_0123);
        assert_eq!(mem.read_u8(0), 0xA5);
    }
    #[test]
    fn test_translate_address_full_checked_uses_descriptor_for_unloaded_selector() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x40000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0010;
        let desc = [0xFF, 0xFF, 0x00, 0x40, 0x23, 0xF2, 0x40, 0x01];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let phys = cpu
            .translate_address_full_checked(&mut mem, 0x0008, 0x0040, PageAccess::Read)
            .unwrap();
        assert_eq!(phys, 0x0123_4000 + 0x0040);
    }
    #[test]
    fn test_translate_linear_checked_clears_page_fault() {
        let cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        mem.set_page_fault(0xDEAD, 0xBEEF);
        mem.map_linear(0x2000, 0x1234);
        let phys = cpu
            .translate_linear_checked(&mut mem, 0x2000, PageAccess::Read)
            .unwrap();
        assert_eq!(phys, 0x1234);
        assert!(!mem.page_fault_pending());
    }
    #[test]
    fn test_translate_v86() {
        let cpu = Cpu::new();
        assert_eq!(cpu.translate_v86(0x1234, 0x5678), 0x12340 + 0x5678);
        assert_eq!(cpu.translate_v86(0x0000, 0x0100), 0x100);
        assert_eq!(cpu.translate_v86(0xF000, 0xFFF0), 0xFFFF0);
        assert_eq!(cpu.translate_v86(0xFFFF, 0x0010), 0x100000);
    }
    #[test]
    fn test_ip_linear_protected_32bit() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.cs_bits = 32;
        cpu.regs.cs = 0x0008;
        cpu.regs.cs_cache.base = 0x20000;
        cpu.regs.eip = 0x1234;
        assert_eq!(cpu.ip_linear(), 0x20000 + 0x1234);
    }
    #[test]
    fn test_instruction_offset_uses_full_eip_for_32bit_cs() {
        let mut cpu = Cpu::new();
        cpu.cs_bits = 32;
        cpu.regs.eip = 0x0001_0100;
        assert_eq!(cpu.instruction_offset(), 0x0001_0100);
    }
    #[test]
    fn test_instruction_offset_masks_upper_eip_for_16bit_cs() {
        let mut cpu = Cpu::new();
        cpu.cs_bits = 16;
        cpu.regs.eip = 0x0001_0100;
        assert_eq!(cpu.instruction_offset(), 0x0100);
    }
    #[test]
    fn test_with_iret_flags_mut_ex_skips_out_of_limit() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.set_limit(0x0001);
        cpu.regs.set_sp(0x0000);
        let mut mem = TestMemory::new(0x20000);
        let mut called = 0;
        let mut mutate = |flags: &mut u16| {
            called += 1;
            *flags |= 0x1;
        };
        cpu.with_iret_flags_mut_ex(&mut mem, &mut mutate, false)
            .unwrap();
        assert_eq!(called, 0);
    }
    #[test]
    fn test_with_iret_flags_mut_ex_updates_flags() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.ss = 0x1000;
        cpu.regs.set_sp(0x0100);
        let flags_addr = ((cpu.regs.ss as usize) << 4) + (cpu.regs.sp() as usize + 4);
        mem.write_u16(flags_addr, 0x0002);
        let mut mutate = |flags: &mut u16| {
            *flags |= 0x0040;
        };
        cpu.with_iret_flags_mut_ex(&mut mem, &mut mutate, false)
            .unwrap();
        assert_eq!(mem.read_u16(flags_addr), 0x0042);
    }
    #[test]
    fn test_push16_wraparound() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.ss = 0x1000;
        cpu.regs.set_sp(0x0001);
        cpu.regs.esp = 0x0001;
        cpu.push16(&mut mem, 0xABCD);
        let base = (cpu.regs.ss as usize) << 4;
        assert_eq!(cpu.regs.sp(), 0xFFFF);
        assert_eq!(mem.read_u8(base + 0xFFFF), 0xCD);
        assert_eq!(mem.read_u8(base), 0xAB);
    }
    #[test]
    fn test_try_push16_stack_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.set_limit(0x0000);
        cpu.regs.set_sp(0x0000);
        let err = cpu.try_push16(&mut mem, 0x1234).unwrap_err();
        assert!(matches!(err, CpuError::StackFault));
    }
    #[test]
    fn test_try_push16_paging_pde_fault_preserves_sp_and_physical_zero() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ss_cache.present = true;
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.set_sp(0x0100);
        cpu.regs.esp = 0x0100;
        mem.write_u32(0x1000, 0);
        mem.write_u8(0, 0x5A);
        let err = cpu.try_push16(&mut mem, 0x1234).unwrap_err();
        assert!(matches!(err, CpuError::PageFault(_)));
        assert_eq!(cpu.regs.sp(), 0x0100);
        assert_eq!(mem.page_fault_address(), 0x0000_00FE);
        assert_eq!(mem.read_u8(0), 0x5A);
    }
    #[test]
    fn test_legacy_push16_paging_guard_preserves_sp() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 0x8000_0000;
        cpu.regs.set_sp(0x0100);
        cpu.regs.esp = 0x0100;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cpu.push16(&mut mem, 0x1234);
        }));
        assert!(result.is_err());
        assert_eq!(cpu.regs.sp(), 0x0100);
        assert_eq!(cpu.regs.esp, 0x0100);
    }
    #[test]
    fn test_legacy_push32_paging_guard_preserves_esp() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 0x8000_0000;
        cpu.regs.ss_cache.big = true;
        cpu.regs.esp = 0x1234_5678;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cpu.push32(&mut mem, 0xDEAD_BEEF);
        }));
        assert!(result.is_err());
        assert_eq!(cpu.regs.esp, 0x1234_5678);
    }
    #[test]
    fn test_configure_diagnostics_and_dpmi_push() {
        let mut cpu = Cpu::new();
        cpu.configure_diagnostics(true, false, 10, 0);
        assert_eq!(cpu.diag_sample_rate, 1);
        assert_eq!(cpu.diag_ring.len(), 0);
        cpu.diag_push_dpmi(0x1234);
        assert_eq!(cpu.diag_ring.len(), 1);
        cpu.configure_diagnostics(false, false, 10, 1);
        assert_eq!(cpu.diag_ring.len(), 0);
    }
    #[test]
    fn test_mark_iret_reentry_depth() {
        let mut cpu = Cpu::new();
        cpu.dpmi_exception_diag.reentry_depth = 2;
        cpu.mark_iret();
        assert!(cpu.last_iret());
        assert_eq!(cpu.dpmi_exception_diag.reentry_depth, 1);
    }
    #[test]
    fn test_load_cs_protected_mode_success() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.load_cs(&mut mem, 0x08).unwrap();
        assert_eq!(cpu.regs.cs, 0x08);
        assert!(cpu.regs.cs_cache.is_code);
        assert_eq!(cpu.cs_bits, 16);
    }
    #[test]
    fn test_load_cs_retains_16bit_descriptor_size() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        cpu.dos_extender_active = true;
        // Synthetic 64 KiB code segment at a round nonzero base; D/B is clear.
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x10, 0x9B, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.load_cs(&mut mem, 0x08).unwrap();
        assert_eq!(cpu.cs_bits, 16);
        assert!(!cpu.regs.cs_cache.big);
        assert_eq!(cpu.regs.cs_cache.base, 0x0010_0000);
    }
    #[test]
    fn test_load_cs_conforming_keeps_cpl() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let user_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFA, 0x40, 0x00];
        let conf_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9E, 0x40, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, user_desc);
        write_descriptor(&mut mem, 0x1000 + 16, conf_desc);
        cpu.load_cs(&mut mem, 0x000B).unwrap();
        assert_eq!(cpu.regs.cpl, 3);
        cpu.load_cs(&mut mem, 0x0010).unwrap();
        assert_eq!(cpu.regs.cpl, 3);
        assert_eq!(cpu.regs.cs & 0x3, 3);
    }
    #[test]
    fn test_load_cs_nonconforming_rejects_dpl_mismatch() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let user_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFA, 0x40, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, user_desc);
        let err = cpu.load_cs(&mut mem, 0x0008).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_load_cs_nonconforming_rejects_rpl_above_cpl() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let kernel_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x40, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, kernel_desc);
        let err = cpu.load_cs(&mut mem, 0x000B).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_load_cs_conforming_rejects_dpl_above_cpl() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let conf_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFE, 0x40, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, conf_desc);
        let err = cpu.load_cs(&mut mem, 0x0008).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_load_cs_rejects_non_code() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_cs(&mut mem, 0x08).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_load_cs_rejects_system_descriptor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc_16 = [0x34, 0x12, 0x78, 0x56, 0x00, 0x84, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc_16);
        let err_16 = cpu.load_cs(&mut mem, 0x08).unwrap_err();
        assert!(matches!(err_16, CpuError::CallGate(0x08)));
        let desc = [0x34, 0x12, 0x78, 0x56, 0x00, 0x8C, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_cs(&mut mem, 0x08).unwrap_err();
        assert!(matches!(err, CpuError::CallGate(0x08)));
        let desc_trap = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x87, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc_trap);
        let err2 = cpu.load_cs(&mut mem, 0x08).unwrap_err();
        assert!(matches!(err2, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_load_cs_null_selector_allowed() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.allow_null_cs = true;
        cpu.regs.cs = 0x1234;
        cpu.load_cs(&mut mem, 0x0000).unwrap();
        assert_eq!(cpu.regs.cs, 0x1234);
    }
    #[test]
    fn test_load_ss_null_selector_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        let err = cpu.load_ss(&mut mem, 0x0000).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_load_ss_rejects_code_segment() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_ss(&mut mem, 0x08).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    /// MDD-BUG-TEM-00165: every SYSTEM type whose low type bits read as
    /// writable data (bit 3 clear, bit 1 set) must be #GP, not a stack.
    #[test]
    fn test_load_ss_rejects_system_descriptors() {
        for access in [0x82u8, 0x83, 0x86, 0x87] {
            let mut cpu = Cpu::new();
            let mut mem = TestMemory::new(0x20000);
            cpu.regs.cr0 = 1;
            cpu.regs.gdtr.base = 0x1000;
            cpu.regs.gdtr.limit = 0x0F;
            write_descriptor(
                &mut mem,
                0x1000 + 8,
                [0xFF, 0xFF, 0x00, 0x20, 0x00, access, 0x00, 0x00],
            );
            let before = cpu.regs.ss;
            let err = cpu.load_ss(&mut mem, 0x08).unwrap_err();
            assert!(
                matches!(err, CpuError::GeneralProtectionFault),
                "access {access:#04x}: {err:?}"
            );
            assert_eq!(cpu.regs.ss, before, "access {access:#04x} must not load SS");
        }
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        write_descriptor(
            &mut mem,
            0x1000 + 8,
            [0xFF, 0xFF, 0x00, 0x20, 0x00, 0x92, 0x00, 0x00],
        );
        cpu.load_ss(&mut mem, 0x08).unwrap();
        assert_eq!(cpu.regs.ss, 0x08);
    }
    #[test]
    fn test_load_ss_fallback_real_mode_base() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.allow_pm_real_mode_ss = true;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0000;
        cpu.load_ss(&mut mem, 0x08).unwrap();
        assert_eq!(cpu.regs.ss, 0x08);
        assert_eq!(cpu.regs.ss_cache.base, 0x0080);
    }
    #[test]
    fn test_load_ss_ldt_zero_selector_is_not_null() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.ldt_base = 0x1800;
        cpu.regs.ldt_limit = 0x0007;
        let desc = [0xFF, 0xFF, 0x00, 0x40, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1800, desc);
        cpu.load_ss(&mut mem, 0x0004).unwrap();
        assert_eq!(cpu.regs.ss, 0x0004);
        assert_eq!(cpu.regs.ss_cache.base, 0x4000);
        assert!(cpu.regs.ss_cache.present);
    }
    #[test]
    fn test_load_data_segment_checked_null_selector() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.load_data_segment_checked(&mut mem, DataSegReg::DS, 0x0000)
            .unwrap();
        assert_eq!(cpu.regs.ds, 0x0000);
        assert_eq!(cpu.regs.ds_cache.base, 0);
        assert!(!cpu.regs.ds_cache.present);
    }
    #[test]
    fn test_load_data_segment_checked_ldt_zero_is_not_null() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.ldt_base = 0x1800;
        cpu.regs.ldt_limit = 0x0007;
        let desc = [0xFF, 0xFF, 0x00, 0x20, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1800, desc);
        cpu.load_data_segment_checked(&mut mem, DataSegReg::DS, 0x0004)
            .unwrap();
        assert_eq!(cpu.regs.ds, 0x0004);
        assert_eq!(cpu.regs.ds_cache.base, 0x2000);
        assert!(cpu.regs.ds_cache.present);
    }
    #[test]
    fn test_load_data_segment_ldt_zero_is_not_null() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.ldt_base = 0x1800;
        cpu.regs.ldt_limit = 0x0007;
        let desc = [0xFF, 0xFF, 0x00, 0x30, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1800, desc);
        cpu.load_data_segment(&mem, DataSegReg::DS, 0x0006);
        assert_eq!(cpu.regs.ds, 0x0006);
        assert_eq!(cpu.regs.ds_cache.base, 0x3000);
        assert!(cpu.regs.ds_cache.present);
    }
    #[test]
    fn test_load_data_segment_checked_not_present() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x12, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu
            .load_data_segment_checked(&mut mem, DataSegReg::DS, 0x08)
            .unwrap_err();
        assert!(matches!(err, CpuError::SegmentNotPresent(_)));
    }
    #[test]
    fn test_load_data_segment_checked_rejects_overlimit_ldt() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.ldt_base = 0x10000;
        cpu.regs.ldt_limit = 7;
        let desc = [0xFF, 0xFF, 0x00, 0x50, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x10008, desc);
        let before = (cpu.regs.ds, cpu.regs.ds_cache);
        assert!(matches!(
            cpu.load_data_segment_checked(&mut mem, DataSegReg::DS, 0x0C),
            Err(CpuError::GeneralProtectionFault)
        ));
        assert_eq!(cpu.regs.ds, before.0);
        assert_eq!(cpu.regs.ds_cache, before.1);
    }
    #[test]
    fn test_load_data_segment_checked_not_present_system_descriptor_is_gp() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu
            .load_data_segment_checked(&mut mem, DataSegReg::DS, 0x08)
            .unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_load_data_segment_checked_not_present_dpl_violation_is_gp() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x12, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu
            .load_data_segment_checked(&mut mem, DataSegReg::DS, 0x0B)
            .unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_load_data_segment_not_present_carries_selector() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x12, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu
            .load_data_segment_checked(&mut mem, DataSegReg::DS, 0x08)
            .unwrap_err();
        match err {
            CpuError::SegmentNotPresent(sel) => assert_eq!(sel, 0x08),
            other => panic!("expected SegmentNotPresent with selector, got {:?}", other),
        }
    }
    #[test]
    fn test_load_data_segment_checked_code_not_readable() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x98, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu
            .load_data_segment_checked(&mut mem, DataSegReg::DS, 0x08)
            .unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_load_data_segment_checked_rejects_system_descriptor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x89, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu
            .load_data_segment_checked(&mut mem, DataSegReg::DS, 0x08)
            .unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_load_data_segment_checked_dpl_violation() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu
            .load_data_segment_checked(&mut mem, DataSegReg::DS, 0x0B)
            .unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_load_data_segment_checked_missing_descriptor_faults() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0000;
        let old_ds = cpu.regs.ds;
        let old_cache = cpu.regs.ds_cache;
        let err = cpu
            .load_data_segment_checked(&mut mem, DataSegReg::DS, 0x08)
            .unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
        assert_eq!(cpu.regs.ds, old_ds);
        assert_eq!(cpu.regs.ds_cache.base, old_cache.base);
    }
    #[test]
    fn test_load_data_segment_checked_descriptor_page_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x9000;
        cpu.regs.gdtr.base = 0x4000;
        cpu.regs.gdtr.limit = 0x000f;
        cpu.regs.ds = 0x0010;
        let old_cache = cpu.regs.ds_cache;
        let err = cpu
            .load_data_segment_checked(&mut mem, DataSegReg::DS, 0x08)
            .unwrap_err();
        match err {
            CpuError::PageFault(pf) => {
                assert_eq!(pf.linear_address, 0x4008);
                assert_eq!(pf.error_code, 0);
            }
            other => panic!("expected descriptor-table #PF, got {other:?}"),
        }
        assert_eq!(cpu.regs.ds, 0x0010);
        assert_eq!(cpu.regs.ds_cache.base, old_cache.base);
    }
    /// MDD-BUG-CRUCIBLE-00025: a page fault walking the GDT/LDT for a CS load
    /// must surface as #PF, not collapse into #GP.
    #[test]
    fn test_load_cs_descriptor_page_fault_preserves_pf() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x9000;
        cpu.regs.gdtr.base = 0x4000;
        cpu.regs.gdtr.limit = 0x00ff;
        let old_cs = cpu.regs.cs;
        let err = cpu.load_cs(&mut mem, 0x08).unwrap_err();
        match err {
            CpuError::PageFault(pf) => {
                assert_eq!(pf.linear_address, 0x4008);
                assert_eq!(pf.error_code, 0);
            }
            other => panic!("expected descriptor-table #PF, got {other:?}"),
        }
        assert_eq!(cpu.regs.cs, old_cs, "CS unchanged on faulting load");
    }
    /// MDD-BUG-CRUCIBLE-00025: same for the IRET CS-load path.
    #[test]
    fn test_load_cs_iret_descriptor_page_fault_preserves_pf() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x9000;
        cpu.regs.gdtr.base = 0x4000;
        cpu.regs.gdtr.limit = 0x00ff;
        let old_cs = cpu.regs.cs;
        let err = cpu.load_cs_iret(&mut mem, 0x08, 0).unwrap_err();
        match err {
            CpuError::PageFault(pf) => {
                assert_eq!(pf.linear_address, 0x4008);
                assert_eq!(pf.error_code, 0);
            }
            other => panic!("expected descriptor-table #PF, got {other:?}"),
        }
        assert_eq!(cpu.regs.cs, old_cs, "CS unchanged on faulting IRET load");
    }
    /// MDD-BUG-CRUCIBLE-00025: same for the SS-load path (the DPMI real-mode
    /// fallback must NOT swallow a genuine descriptor-table #PF).
    #[test]
    fn test_load_ss_descriptor_page_fault_preserves_pf() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x9000;
        cpu.regs.gdtr.base = 0x4000;
        cpu.regs.gdtr.limit = 0x00ff;
        cpu.allow_pm_real_mode_ss = true;
        let old_ss = cpu.regs.ss;
        let err = cpu.load_ss(&mut mem, 0x08).unwrap_err();
        match err {
            CpuError::PageFault(pf) => {
                assert_eq!(pf.linear_address, 0x4008);
                assert_eq!(pf.error_code, 0);
            }
            other => panic!("expected descriptor-table #PF, got {other:?}"),
        }
        assert_eq!(cpu.regs.ss, old_ss, "SS unchanged on faulting load");
    }
    #[test]
    fn test_load_data_segment_checked_descriptor_read_sets_accessed_bits() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x80000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x10000;
        cpu.regs.cpl = 3;
        cpu.regs.gdtr.base = 0x4000;
        cpu.regs.gdtr.limit = 0x000f;
        let page_table = 0x11000;
        map_page(
            &mut mem,
            cpu.regs.cr3 as usize,
            page_table,
            0x4000,
            0x6000,
            0x02,
        );
        let desc = [0xFF, 0xFF, 0x00, 0x30, 0x00, 0xF2, 0x00, 0x00];
        write_descriptor(&mut mem, 0x6000 + 8, desc);
        cpu.load_data_segment_checked(&mut mem, DataSegReg::DS, 0x000B)
            .unwrap();
        let pde_addr = cpu.regs.cr3 as usize + ((0x4000_u32 >> 22) as usize) * 4;
        let pte_addr = page_table + (((0x4000_u32 >> 12) & 0x03FF) as usize) * 4;
        assert_eq!(cpu.regs.ds, 0x000B);
        assert_eq!(cpu.regs.ds_cache.base, 0x3000);
        assert_ne!(mem.read_u32(pde_addr) & 0x20, 0, "PDE accessed bit");
        assert_ne!(mem.read_u32(pte_addr) & 0x20, 0, "PTE accessed bit");
        assert_eq!(
            mem.read_u8(0x6000 + 8 + 5),
            0xF3,
            "descriptor Accessed bit written back"
        );
        assert_ne!(
            mem.read_u32(pte_addr) & 0x40,
            0,
            "accessed-bit write-back dirties the descriptor page"
        );
    }
    #[test]
    fn test_invalidate_segment_cache_updates_data_segment() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x01, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.regs.ds = 0x08;
        cpu.regs.ds_cache.base = 0x0000;
        cpu.invalidate_segment_cache(&mem, 0x08);
        assert_eq!(cpu.regs.ds_cache.base, 0x00010000);
    }
    #[test]
    fn test_translate_linear_checked_pde_not_present() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x10000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        mem.write_u32(0x1000, 0x0000_0000);
        let err = cpu
            .translate_linear_checked(&mut mem, 0x0000_0123, PageAccess::Read)
            .unwrap_err();
        assert!(matches!(err, CpuError::PageFault(_)));
        assert!(mem.page_fault_pending());
    }
    #[test]
    fn test_translate_linear_checked_pde_bit7_accepted() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x10000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        mem.write_u32(0x1000, 0x0000_2081);
        mem.write_u32(0x2000, 0x0000_3007);
        let result = cpu.translate_linear_checked(&mut mem, 0x0000_0123, PageAccess::Read);
        assert!(result.is_ok(), "386 ignores undefined PDE bits");
    }
    #[test]
    fn test_translate_linear_checked_pte_not_present() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x10000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        mem.write_u32(0x1000, 0x0000_2001);
        mem.write_u32(0x2000, 0x0000_0000);
        let err = cpu
            .translate_linear_checked(&mut mem, 0x0000_0123, PageAccess::Read)
            .unwrap_err();
        assert!(matches!(err, CpuError::PageFault(_)));
    }
    #[test]
    fn test_translate_linear_checked_write_protect() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x10000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        cpu.regs.cpl = 3;
        mem.write_u32(0x1000, 0x0000_2001);
        mem.write_u32(0x2000, 0x0000_3001);
        let err = cpu
            .translate_linear_checked(&mut mem, 0x0000_0123, PageAccess::Write)
            .unwrap_err();
        let pf = match err {
            CpuError::PageFault(pf) => pf,
            other => panic!("Unexpected error: {other:?}"),
        };
        assert_ne!(pf.error_code & 0x1, 0);
        assert_ne!(pf.error_code & 0x2, 0);
        assert_ne!(pf.error_code & 0x4, 0);
    }
    #[test]
    fn test_translate_linear_checked_user_not_allowed() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x10000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        cpu.regs.cpl = 3;
        mem.write_u32(0x1000, 0x0000_2001);
        mem.write_u32(0x2000, 0x0000_3001);
        let err = cpu
            .translate_linear_checked(&mut mem, 0x0000_0123, PageAccess::Read)
            .unwrap_err();
        let pf = match err {
            CpuError::PageFault(pf) => pf,
            other => panic!("Unexpected error: {other:?}"),
        };
        assert_ne!(pf.error_code & 0x1, 0);
        assert_ne!(pf.error_code & 0x4, 0);
    }
    #[test]
    fn test_translate_linear_checked_success_sets_bits() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x10000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        mem.write_u32(0x1000, 0x0000_2003);
        mem.write_u32(0x2000, 0x0000_3003);
        let phys = cpu
            .translate_linear_checked(&mut mem, 0x0000_0123, PageAccess::Write)
            .unwrap();
        assert_eq!(phys, 0x3000 + 0x123);
        let pde = mem.read_u32(0x1000);
        let pte = mem.read_u32(0x2000);
        assert_ne!(pde & (1 << 5), 0);
        assert_ne!(pte & (1 << 5), 0);
        assert_ne!(pte & (1 << 6), 0);
    }
    #[test]
    fn test_read_descriptor_null_selector() {
        let cpu = Cpu::new();
        let mem = TestMemory::new(0x1000);
        assert!(cpu.read_descriptor(&mem, 0x0000).is_none());
    }
    #[test]
    fn test_read_descriptor_outside_limit() {
        let mut cpu = Cpu::new();
        let mem = TestMemory::new(0x2000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0007;
        assert!(cpu.read_descriptor(&mem, 0x08).is_none());
    }
    #[test]
    fn test_read_descriptor_uses_ldt() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x3000);
        cpu.regs.ldt_base = 0x1200;
        cpu.regs.ldt_limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x02, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1200 + 8, desc);
        let cache = cpu.read_descriptor(&mem, 0x0C).unwrap();
        assert_eq!(cache.base, 0x00020000);
    }
    #[test]
    fn test_descriptor_single_page_checked_matches_unchecked() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x8000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        cpu.regs.gdtr.base = 0xC000_1FF0;
        cpu.regs.gdtr.limit = 0x000F;
        map_page(&mut mem, 0x1000, 0x4000, 0xC000_1000, 0x3000, 0x003);
        let desc = [0x67, 0x45, 0x23, 0x01, 0x89, 0x93, 0x40, 0xAB];
        write_descriptor(&mut mem, 0x3FF8, desc);
        let unchecked = cpu.read_descriptor(&mem, 0x08).unwrap();
        let checked = cpu
            .read_descriptor_checked_for_segment_load(&mut mem, 0x08)
            .unwrap();
        assert_eq!(checked.cache, unchecked);
        assert_eq!(checked.cache.base, 0xAB89_0123);
        assert_eq!(checked.accessed_linear, None);
        let pde = mem.read_u32(0x1000 + ((0xC000_1FF8u32 >> 22) as usize) * 4);
        let pte = mem.read_u32(0x4000 + (((0xC000_1FF8u32 >> 12) & 0x3FF) as usize) * 4);
        assert_ne!(pde & 0x20, 0);
        assert_ne!(pte & 0x20, 0);
        assert_eq!(pte & 0x40, 0);
    }
    #[test]
    fn test_descriptor_straddling_missing_page_faults_at_second_page() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x8000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        cpu.regs.gdtr.base = 0xC000_1FF5;
        cpu.regs.gdtr.limit = 0x000F;
        map_page(&mut mem, 0x1000, 0x4000, 0xC000_1000, 0x3000, 0x003);
        let desc = [0x67, 0x45, 0x23, 0x01, 0x89, 0x93, 0x40, 0xAB];
        for (i, byte) in desc[..3].iter().enumerate() {
            mem.write_u8(0x3FFD + i, *byte);
        }
        assert!(cpu.read_descriptor(&mem, 0x08).is_none());
        let err = match cpu.read_descriptor_checked_for_segment_load(&mut mem, 0x08) {
            Err(err) => err,
            Ok(_) => panic!("expected #PF"),
        };
        match err {
            CpuError::PageFault(pf) => {
                assert_eq!(pf.linear_address, 0xC000_2000);
                assert_eq!(pf.error_code, 0);
            }
            other => panic!("expected #PF, got {other:?}"),
        }
    }
    #[test]
    fn test_read_descriptor_translates_high_half_gdt_with_paging() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x8000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        cpu.regs.gdtr.base = 0xC000_1000;
        cpu.regs.gdtr.limit = 0x0017;
        map_page(&mut mem, 0x1000, 0x4000, 0xC000_1000, 0x3000, 0x003);
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x34, 0x9A, 0x40, 0x12];
        write_descriptor(&mut mem, 0x3000 + 0x10, desc);
        let cache = cpu.read_descriptor(&mem, 0x10).unwrap();
        assert!(cache.is_code);
        assert!(cache.big);
        assert_eq!(cache.base, 0x1234_0000);
        cpu.load_cs(&mut mem, 0x10).unwrap();
        assert_eq!(cpu.regs.cs, 0x10);
        assert_eq!(cpu.cs_bits, 32);
        assert_eq!(cpu.regs.cs_cache.base, 0x1234_0000);
    }
    #[test]
    fn test_read_descriptor_translates_high_half_ldt_with_paging() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x9000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        cpu.regs.ldt_base = 0xC000_2000;
        cpu.regs.ldt_limit = 0x000F;
        map_page(&mut mem, 0x1000, 0x4000, 0xC000_2000, 0x5000, 0x003);
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x56, 0x92, 0x00, 0x34];
        write_descriptor(&mut mem, 0x5000 + 0x08, desc);
        let cache = cpu.read_descriptor(&mem, 0x0C).unwrap();
        assert!(!cache.is_code);
        assert!(cache.is_writable);
        assert_eq!(cache.base, 0x3456_0000);
    }
    #[test]
    fn test_read_descriptor_ignores_physical_mapping_without_paging() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x5000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0017;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x34, 0x9A, 0x40, 0x12];
        write_descriptor(&mut mem, 0x1000 + 0x10, desc);
        for offset in 0..8 {
            mem.map_linear(0x1000 + 0x10 + offset, 0x3000 + offset as usize);
        }
        let cache = cpu.read_descriptor(&mem, 0x10).unwrap();
        assert!(cache.present);
        assert!(cache.is_code);
        assert_eq!(cache.base, 0x1234_0000);
    }
    #[test]
    fn test_is_v86_mode() {
        let mut cpu = Cpu::new();
        assert!(!cpu.is_v86_mode());
        cpu.regs.set_v86_mode(true);
        assert!(cpu.is_v86_mode());
        cpu.regs.set_v86_mode(false);
        assert!(!cpu.is_v86_mode());
    }
    #[test]
    fn test_setup_for_com() {
        let mut cpu = Cpu::new();
        cpu.setup_for_com(0x1234);
        assert_eq!(cpu.regs.cs, 0x1234);
        assert_eq!(cpu.regs.ds, 0x1234);
        assert_eq!(cpu.regs.es, 0x1234);
        assert_eq!(cpu.regs.ss, 0x1234);
        assert_eq!(cpu.regs.ip(), 0x0100);
        assert_eq!(cpu.regs.eip, 0x0100);
        assert_eq!(cpu.regs.sp(), 0xFFFE);
        assert_eq!(cpu.regs.esp, 0xFFFE);
        assert!(cpu.interrupts_enabled);
        assert!(cpu.regs.flags.contains(crate::Flags::INTERRUPT));
        assert_eq!(cpu.cs_bits, 16);
        assert_eq!(cpu.regs.cs_cache.base, 0x12340);
        assert_eq!(cpu.regs.ds_cache.base, 0x12340);
        assert_eq!(cpu.regs.es_cache.base, 0x12340);
        assert_eq!(cpu.regs.ss_cache.base, 0x12340);
    }
    #[test]
    fn test_setup_for_exe() {
        let mut cpu = Cpu::new();
        cpu.setup_for_exe(0x1000, 0x0100, 0x2000, 0x1000, 0x3000);
        assert_eq!(cpu.regs.cs, 0x1000);
        assert_eq!(cpu.regs.ip(), 0x0100);
        assert_eq!(cpu.regs.eip, 0x0100);
        assert_eq!(cpu.regs.ss, 0x2000);
        assert_eq!(cpu.regs.sp(), 0x1000);
        assert_eq!(cpu.regs.esp, 0x1000);
        assert_eq!(cpu.regs.ds, 0x3000);
        assert_eq!(cpu.regs.es, 0x3000);
        assert!(cpu.interrupts_enabled);
        assert!(cpu.regs.flags.contains(crate::Flags::INTERRUPT));
        assert_eq!(cpu.cs_bits, 16);
        assert_eq!(cpu.regs.cs_cache.base, 0x10000);
        assert_eq!(cpu.regs.ss_cache.base, 0x20000);
        assert_eq!(cpu.regs.ds_cache.base, 0x30000);
        assert_eq!(cpu.regs.es_cache.base, 0x30000);
    }
    #[test]
    fn test_try_pop16_real_mode() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.ss = 0x1000;
        cpu.regs.set_sp(0x0100);
        cpu.regs.esp = 0x0100;
        let stack_addr = ((cpu.regs.ss as usize) << 4) + (cpu.regs.sp() as usize);
        mem.write_u16(stack_addr, 0xABCD);
        let value = cpu.try_pop16(&mut mem).unwrap();
        assert_eq!(value, 0xABCD);
        assert_eq!(cpu.regs.sp(), 0x0102);
        assert_eq!(cpu.regs.esp & 0xFFFF, 0x0102);
    }
    #[test]
    fn test_try_pop16_wraparound() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.cpu_type = CpuType::I8086;
        cpu.regs.ss = 0x1000;
        cpu.regs.set_sp(0xFFFF);
        cpu.regs.esp = 0xFFFF;
        let base = (cpu.regs.ss as usize) << 4;
        mem.write_u8(base + 0xFFFF, 0xCD);
        mem.write_u8(base, 0xAB);
        let value = cpu.try_pop16(&mut mem).unwrap();
        assert_eq!(value, 0xABCD);
        assert_eq!(cpu.regs.sp(), 0x0001);
    }
    #[test]
    fn test_try_pop16_386_real_mode_sp_ffff_stack_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.cpu_type = CpuType::I80386;
        cpu.regs.ss = 0x1000;
        cpu.regs.ss_cache = SegmentCache::real_mode(0x1000);
        cpu.regs.set_sp(0xFFFF);
        cpu.regs.esp = 0xFFFF;
        let result = cpu.try_pop16(&mut mem);
        assert!(matches!(result, Err(crate::CpuError::StackFault)));
        assert_eq!(cpu.regs.sp(), 0xFFFF);
        assert_eq!(cpu.regs.esp, 0xFFFF);
    }
    #[test]
    fn test_try_push16_386_real_mode_sp_0001_stack_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.cpu_type = CpuType::I80386;
        cpu.regs.ss = 0x1000;
        cpu.regs.ss_cache = SegmentCache::real_mode(0x1000);
        cpu.regs.set_sp(0x0001);
        cpu.regs.esp = 0x0001;
        let result = cpu.try_push16(&mut mem, 0xABCD);
        assert!(matches!(result, Err(crate::CpuError::StackFault)));
        assert_eq!(cpu.regs.sp(), 0x0001);
        assert_eq!(cpu.regs.esp, 0x0001);
    }
    #[test]
    fn test_try_pop32_real_mode() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.ss = 0x1000;
        cpu.regs.set_sp(0x0100);
        cpu.regs.esp = 0x0100;
        let stack_addr = ((cpu.regs.ss as usize) << 4) + (cpu.regs.sp() as usize);
        mem.write_u32(stack_addr, 0xDEADBEEF);
        let value = cpu.try_pop32(&mut mem).unwrap();
        assert_eq!(value, 0xDEADBEEF);
        assert_eq!(cpu.regs.sp(), 0x0104);
    }
    #[test]
    fn test_try_pop32_protected_mode_32bit_stack() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.esp = 0x0100;
        mem.write_u32(0x0100, 0x12345678);
        let value = cpu.try_pop32(&mut mem).unwrap();
        assert_eq!(value, 0x12345678);
        assert_eq!(cpu.regs.esp, 0x0104);
    }
    #[test]
    fn test_try_pop32_stack_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.set_limit(0x0001);
        cpu.regs.set_sp(0x0000);
        let err = cpu.try_pop32(&mut mem).unwrap_err();
        assert!(matches!(err, CpuError::StackFault));
    }
    #[test]
    fn test_push32_and_pop32() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.ss = 0x1000;
        cpu.regs.set_sp(0x1000);
        cpu.regs.esp = 0x1000;
        cpu.push32(&mut mem, 0xDEADBEEF);
        assert_eq!(cpu.regs.sp(), 0x0FFC);
        let value = cpu.pop32(&mut mem);
        assert_eq!(value, 0xDEADBEEF);
        assert_eq!(cpu.regs.sp(), 0x1000);
    }
    #[test]
    fn test_push32_protected_mode_32bit_stack() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.esp = 0x1000;
        cpu.push32(&mut mem, 0xCAFEBABE);
        assert_eq!(cpu.regs.esp, 0x0FFC);
        assert_eq!(cpu.regs.sp(), 0x0FFC_u16);
        let value = mem.read_u32(0x0FFC);
        assert_eq!(value, 0xCAFEBABE);
    }
    #[test]
    fn test_try_push32_real_mode() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.ss = 0x1000;
        cpu.regs.set_sp(0x1000);
        cpu.regs.esp = 0x1000;
        cpu.try_push32(&mut mem, 0x12345678).unwrap();
        assert_eq!(cpu.regs.sp(), 0x0FFC);
        let stack_addr = ((cpu.regs.ss as usize) << 4) + (cpu.regs.sp() as usize);
        assert_eq!(mem.read_u32(stack_addr), 0x12345678);
    }
    #[test]
    fn test_try_push32_protected_mode_32bit_stack() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.esp = 0x1000;
        cpu.try_push32(&mut mem, 0xABCDEF01).unwrap();
        assert_eq!(cpu.regs.esp, 0x0FFC);
        assert_eq!(mem.read_u32(0x0FFC), 0xABCDEF01);
    }
    #[test]
    fn test_pop16_and_pop32_protected_mode() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.esp = 0x1000;
        cpu.push16(&mut mem, 0x1234);
        let val16 = cpu.pop16(&mut mem);
        assert_eq!(val16, 0x1234);
        assert_eq!(cpu.regs.esp, 0x1000);
        cpu.push32(&mut mem, 0x56789ABC);
        let val32 = cpu.pop32(&mut mem);
        assert_eq!(val32, 0x56789ABC);
        assert_eq!(cpu.regs.esp, 0x1000);
    }
    #[test]
    fn test_set_instr_cache_enabled() {
        let mut cpu = Cpu::new();
        assert!(cpu.instr_cache_enabled);
        cpu.cache_hits = 100;
        cpu.cache_misses = 50;
        cpu.set_instr_cache_enabled(false);
        assert!(!cpu.instr_cache_enabled);
        assert_eq!(cpu.cache_hits, 0);
        assert_eq!(cpu.cache_misses, 0);
        for entry in cpu.instr_cache.iter() {
            assert_eq!(entry.addr, usize::MAX);
        }
        cpu.set_instr_cache_enabled(true);
        assert!(cpu.instr_cache_enabled);
    }
    #[test]
    fn test_diag_last_events_empty() {
        let cpu = Cpu::new();
        let events = cpu.diag_last_events(10);
        assert!(events.is_empty());
    }
    #[test]
    fn test_diag_push_dpmi_disabled() {
        let mut cpu = Cpu::new();
        cpu.diag_enabled = false;
        cpu.diag_push_dpmi(0x1234);
        assert_eq!(cpu.diag_ring.len(), 0);
    }
    #[test]
    fn test_clear_return_flags() {
        let mut cpu = Cpu::new();
        cpu.last_iret = true;
        cpu.last_retf = true;
        cpu.clear_return_flags();
        assert!(!cpu.last_iret);
        assert!(!cpu.last_retf);
    }
    #[test]
    fn test_mark_retf_and_accessors() {
        let mut cpu = Cpu::new();
        assert!(!cpu.last_retf());
        assert!(!cpu.take_last_retf());
        cpu.mark_retf();
        assert!(cpu.last_retf());
        assert!(cpu.take_last_retf());
        assert!(!cpu.last_retf());
        assert!(!cpu.take_last_retf());
    }
    #[test]
    fn test_take_last_iret() {
        let mut cpu = Cpu::new();
        assert!(!cpu.take_last_iret());
        cpu.last_iret = true;
        assert!(cpu.take_last_iret());
        assert!(!cpu.last_iret);
        assert!(!cpu.take_last_iret());
    }
    #[test]
    fn test_load_data_segment_fs_gs() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x1F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x03, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 16, desc);
        let desc2 = [0xFF, 0xFF, 0x00, 0x00, 0x04, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 24, desc2);
        cpu.load_data_segment(&mem, DataSegReg::FS, 0x10);
        assert_eq!(cpu.regs.fs, 0x10);
        assert_eq!(cpu.regs.fs_cache.base, 0x00030000);
        cpu.load_data_segment(&mem, DataSegReg::GS, 0x18);
        assert_eq!(cpu.regs.gs, 0x18);
        assert_eq!(cpu.regs.gs_cache.base, 0x00040000);
    }
    #[test]
    fn test_load_data_segment_null_protected_mode() {
        let mut cpu = Cpu::new();
        let mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.load_data_segment(&mem, DataSegReg::DS, 0x0000);
        assert_eq!(cpu.regs.ds, 0x0000);
        assert!(!cpu.regs.ds_cache.present);
        cpu.load_data_segment(&mem, DataSegReg::ES, 0x0003);
        assert_eq!(cpu.regs.es, 0x0003);
        assert!(!cpu.regs.es_cache.present);
    }
    #[test]
    fn test_load_data_segment_real_mode() {
        let mut cpu = Cpu::new();
        let mem = TestMemory::new(0x20000);
        cpu.load_data_segment(&mem, DataSegReg::DS, 0x1234);
        assert_eq!(cpu.regs.ds, 0x1234);
        assert_eq!(cpu.regs.ds_cache.base, 0x12340);
        cpu.load_data_segment(&mem, DataSegReg::ES, 0x5678);
        assert_eq!(cpu.regs.es, 0x5678);
        assert_eq!(cpu.regs.es_cache.base, 0x56780);
        cpu.load_data_segment(&mem, DataSegReg::FS, 0xABCD);
        assert_eq!(cpu.regs.fs, 0xABCD);
        assert_eq!(cpu.regs.fs_cache.base, 0xABCD0);
        cpu.load_data_segment(&mem, DataSegReg::GS, 0xEF01);
        assert_eq!(cpu.regs.gs, 0xEF01);
        assert_eq!(cpu.regs.gs_cache.base, 0xEF010);
    }
    #[test]
    fn test_load_data_segment_checked_real_mode() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.load_data_segment_checked(&mut mem, DataSegReg::DS, 0x1234)
            .unwrap();
        assert_eq!(cpu.regs.ds, 0x1234);
        assert_eq!(cpu.regs.ds_cache.base, 0x12340);
    }
    #[test]
    fn test_load_data_segment_checked_fs_gs_null() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.load_data_segment_checked(&mut mem, DataSegReg::FS, 0x0000)
            .unwrap();
        assert_eq!(cpu.regs.fs, 0x0000);
        assert!(!cpu.regs.fs_cache.present);
        cpu.load_data_segment_checked(&mut mem, DataSegReg::GS, 0x0000)
            .unwrap();
        assert_eq!(cpu.regs.gs, 0x0000);
        assert!(!cpu.regs.gs_cache.present);
    }
    #[test]
    fn test_with_iret_flags_mut_delegates() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.ss = 0x1000;
        cpu.regs.set_sp(0x0100);
        let flags_addr = ((cpu.regs.ss as usize) << 4) + (cpu.regs.sp() as usize + 4);
        mem.write_u16(flags_addr, 0x0000);
        let mut mutate = |flags: &mut u16| {
            *flags = 0x1234;
        };
        cpu.with_iret_flags_mut(&mut mem, &mut mutate).unwrap();
        assert_eq!(mem.read_u16(flags_addr), 0x1234);
    }
    #[test]
    fn test_ip_linear_real_mode() {
        let mut cpu = Cpu::new();
        cpu.regs.cs = 0x1234;
        cpu.regs.set_ip(0x5678);
        assert_eq!(cpu.ip_linear(), 0x12340 + 0x5678);
    }
    #[test]
    fn test_ip_linear_protected_16bit() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.cs_bits = 16;
        cpu.regs.cs = 0x0008;
        cpu.regs.cs_cache.base = 0x10000;
        cpu.regs.set_ip(0x1234);
        cpu.regs.eip = 0xABCD1234;
        assert_eq!(cpu.ip_linear(), 0x10000 + 0x1234);
    }
    #[test]
    fn test_load_cs_real_mode() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.load_cs(&mut mem, 0x1234).unwrap();
        assert_eq!(cpu.regs.cs, 0x1234);
        assert_eq!(cpu.regs.cs_cache.base, 0x12340);
        assert_eq!(cpu.cs_bits, 16);
    }
    #[test]
    fn test_load_cs_real_mode_resets_cpl() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cpl = 3;
        cpu.regs.cr0 &= !1;
        cpu.load_cs(&mut mem, 0x080A).unwrap();
        assert_eq!(cpu.regs.cpl, 0, "CPL must be 0 after real-mode CS load");
        assert_eq!(cpu.regs.cs, 0x080A);
        assert_eq!(cpu.cs_bits, 16);
    }
    #[test]
    fn test_load_cs_protected_mode_32bit_segment() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x40, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.load_cs(&mut mem, 0x08).unwrap();
        assert_eq!(cpu.regs.cs, 0x08);
        assert!(cpu.regs.cs_cache.big);
        assert_eq!(cpu.cs_bits, 32);
    }
    #[test]
    fn test_load_cs_protected_mode_not_present() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x1A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_cs(&mut mem, 0x08).unwrap_err();
        assert!(matches!(err, CpuError::SegmentNotPresent(_)));
    }
    #[test]
    fn test_load_cs_not_present_carries_selector() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x1A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_cs(&mut mem, 0x08).unwrap_err();
        match err {
            CpuError::SegmentNotPresent(sel) => assert_eq!(sel, 0x08),
            other => panic!("expected SegmentNotPresent with selector, got {:?}", other),
        }
    }
    #[test]
    fn test_load_cs_protected_mode_no_descriptor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0000;
        let err = cpu.load_cs(&mut mem, 0x08).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_load_ss_real_mode() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.load_ss(&mut mem, 0x2000).unwrap();
        assert_eq!(cpu.regs.ss, 0x2000);
        assert_eq!(cpu.regs.ss_cache.base, 0x20000);
    }
    #[test]
    fn test_load_ss_protected_mode_valid() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.load_ss(&mut mem, 0x08).unwrap();
        assert_eq!(cpu.regs.ss, 0x08);
        assert!(!cpu.regs.ss_cache.is_code);
        assert!(cpu.regs.ss_cache.is_writable);
    }
    #[test]
    fn test_load_ss_protected_mode_dpl_mismatch() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xF2, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_ss(&mut mem, 0x08).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_load_ss_protected_mode_not_writable() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x90, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_ss(&mut mem, 0x08).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
        let not_present = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x12, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, not_present);
        let err = cpu.load_ss(&mut mem, 0x08).unwrap_err();
        assert!(matches!(err, CpuError::StackFault));
    }
    #[test]
    fn test_try_pop16_protected_mode_32bit_stack() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.esp = 0x0100;
        cpu.regs.set_sp(0x0100);
        mem.write_u16(0x0100, 0xBEEF);
        let value = cpu.try_pop16(&mut mem).unwrap();
        assert_eq!(value, 0xBEEF);
        assert_eq!(cpu.regs.esp, 0x0102);
        assert_eq!(cpu.regs.sp(), 0x0102);
    }
    #[test]
    fn test_try_pop16_stack_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.set_limit(0x0000);
        cpu.regs.set_sp(0x0000);
        let err = cpu.try_pop16(&mut mem).unwrap_err();
        assert!(matches!(err, CpuError::StackFault));
    }
    #[test]
    fn test_try_push16_protected_mode_32bit_stack() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.esp = 0x1000;
        cpu.try_push16(&mut mem, 0xCAFE).unwrap();
        assert_eq!(cpu.regs.esp, 0x0FFE);
        assert_eq!(cpu.regs.sp(), 0x0FFE);
        assert_eq!(mem.read_u16(0x0FFE), 0xCAFE);
    }
    #[test]
    fn test_try_push16_real_mode() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.ss = 0x1000;
        cpu.regs.set_sp(0x1000);
        cpu.regs.esp = 0x1000;
        cpu.try_push16(&mut mem, 0xABCD).unwrap();
        assert_eq!(cpu.regs.sp(), 0x0FFE);
        let stack_addr = ((cpu.regs.ss as usize) << 4) + (cpu.regs.sp() as usize);
        assert_eq!(mem.read_u16(stack_addr), 0xABCD);
    }
    #[test]
    fn test_dpmi_exception_diag_should_log_handler() {
        let mut cpu = Cpu::new();
        assert!(!cpu.dpmi_exception_diag_should_log_handler(0, 0, 0));
    }
    #[test]
    fn test_get_segment_cache_all_segments_protected() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.cs = 0x08;
        cpu.regs.cs_cache.base = 0x10000;
        cpu.regs.ds = 0x10;
        cpu.regs.ds_cache.base = 0x20000;
        cpu.regs.es = 0x18;
        cpu.regs.es_cache.base = 0x30000;
        cpu.regs.fs = 0x20;
        cpu.regs.fs_cache.base = 0x40000;
        cpu.regs.gs = 0x28;
        cpu.regs.gs_cache.base = 0x50000;
        cpu.regs.ss = 0x30;
        cpu.regs.ss_cache.base = 0x60000;
        assert_eq!(cpu.get_segment_cache(0x08).base, 0x10000);
        assert_eq!(cpu.get_segment_cache(0x10).base, 0x20000);
        assert_eq!(cpu.get_segment_cache(0x18).base, 0x30000);
        assert_eq!(cpu.get_segment_cache(0x20).base, 0x40000);
        assert_eq!(cpu.get_segment_cache(0x28).base, 0x50000);
        assert_eq!(cpu.get_segment_cache(0x30).base, 0x60000);
        assert_eq!(cpu.get_segment_cache(0x38).base, 0x380);
    }
    #[test]
    fn test_translate_address_full_paging_disabled() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.ds = 0x1000;
        let addr = cpu.translate_address_full(&mut mem, 0x1000, 0x0200);
        assert_eq!(addr, 0x10200);
    }
    #[test]
    fn test_translate_address_full_checked_real_mode() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.ds = 0x1000;
        let addr = cpu
            .translate_address_full_checked(&mut mem, 0x1000, 0x0200, PageAccess::Read)
            .unwrap();
        assert_eq!(addr, 0x10200);
    }
    #[test]
    fn test_translate_address_full_expand_down() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ds = 0x1234;
        cpu.regs.ds_cache.set_limit(0x1000);
        cpu.regs.ds_cache.is_code = false;
        cpu.regs.ds_cache.access = 0x04;
        cpu.regs.ds_cache.big = false;
        let result = cpu.translate_address_full_checked(&mut mem, 0x1234, 0x1001, PageAccess::Read);
        assert!(result.is_ok());
        let result = cpu.translate_address_full_checked(&mut mem, 0x1234, 0x1000, PageAccess::Read);
        assert!(matches!(
            result.unwrap_err(),
            CpuError::GeneralProtectionFault
        ));
    }
    #[test]
    fn test_stack_range_ok_zero_size() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.set_limit(0x0000);
        assert!(cpu.stack_range_ok(0x1000, 0));
    }
    #[test]
    fn test_stack_range_ok_overflow() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.ss_cache.big = true;
        assert!(!cpu.stack_range_ok(0xFFFF_FFFF, 2));
    }
    #[test]
    fn test_iret_real_mode() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.ss = 0x1000;
        cpu.regs.set_sp(0x0100);
        cpu.regs.esp = 0x0100;
        let base = (cpu.regs.ss as usize) << 4;
        mem.write_u16(base + 0x0100, 0x1234);
        mem.write_u16(base + 0x0102, 0x5678);
        mem.write_u16(base + 0x0104, 0x0202);
        cpu.iret(&mut mem).unwrap();
        assert_eq!(cpu.regs.ip(), 0x1234);
        assert_eq!(cpu.regs.cs, 0x5678);
        assert!(cpu.regs.flags.contains(crate::Flags::INTERRUPT));
        assert!(cpu.interrupts_enabled);
        assert_eq!(cpu.regs.sp(), 0x0106);
        assert!(cpu.last_iret());
    }
    #[test]
    fn test_reset_clears_diagnostics() {
        let mut cpu = Cpu::new();
        cpu.diag_loop_history[0] = 0x12345678;
        cpu.diag_loop_pos = 5;
        cpu.diag_loop_filled = 3;
        cpu.diag_loop2_count = 100;
        cpu.diag_loop4_count = 50;
        cpu.last_iret = true;
        cpu.last_retf = true;
        cpu.dpmi_retf_cleanup = 4;
        cpu.last_interrupt_had_error_code = true;
        cpu.reset();
        assert_eq!(cpu.diag_loop_history[0], 0);
        assert_eq!(cpu.diag_loop_pos, 0);
        assert_eq!(cpu.diag_loop_filled, 0);
        assert_eq!(cpu.diag_loop2_count, 0);
        assert_eq!(cpu.diag_loop4_count, 0);
        assert!(!cpu.last_iret);
        assert!(!cpu.last_retf);
        assert_eq!(cpu.dpmi_retf_cleanup, 0);
        assert!(!cpu.last_interrupt_had_error_code);
    }
    #[test]
    fn test_reset_sets_bios_entry_point() {
        let mut cpu = Cpu::new();
        cpu.regs.cs = 0x1234;
        cpu.regs.set_ip(0x5678);
        cpu.reset();
        assert_eq!(cpu.regs.cs, 0xF000);
        assert_eq!(cpu.regs.ip(), 0xFFF0);
    }
    #[test]
    fn test_mark_iret_always_decrements_depth() {
        let mut cpu = Cpu::new();
        cpu.dpmi_exception_diag.reentry_depth = 5;
        cpu.mark_iret();
        assert!(cpu.last_iret);
        assert_eq!(cpu.dpmi_exception_diag.reentry_depth, 4);
    }
    #[test]
    fn test_mark_iret_reentry_depth_at_zero() {
        let mut cpu = Cpu::new();
        cpu.dpmi_exception_diag.reentry_depth = 0;
        cpu.mark_iret();
        assert!(cpu.last_iret);
        assert_eq!(cpu.dpmi_exception_diag.reentry_depth, 0);
    }
    #[test]
    fn test_r3_translate_paging_tlb_hit() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        mem.insert_tlb(0, 0x5, true, true);
        let phys = cpu
            .translate_paging_checked(&mut mem, 0x0100, crate::PageAccess::Read)
            .unwrap();
        assert_eq!(phys, 0x5100);
    }
    #[test]
    fn test_r3_translate_paging_pde_not_present() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        mem.write_u32(0x1000, 0x0000_0000);
        mem.write_u8(0, 0xA5);
        let err = cpu
            .translate_paging_checked(&mut mem, 0x0000, crate::PageAccess::Read)
            .unwrap_err();
        assert_eq!(err.linear_address, 0x0000);
        assert_eq!(mem.read_u8(0), 0xA5);
    }
    #[test]
    fn test_r3_translate_paging_pde_bit7_accepted() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        mem.write_u32(0x1000, 0x0000_2081);
        mem.write_u32(0x2000, 0x0000_3003);
        let phys = cpu
            .translate_paging_checked(&mut mem, 0x0000, crate::PageAccess::Read)
            .unwrap();
        assert_eq!(phys, 0x3000);
        assert!(mem.page_fault.is_none());
    }
    #[test]
    fn test_r3_translate_paging_pte_not_present() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        mem.write_u32(0x1000, 0x0000_2007);
        mem.write_u32(0x2000, 0x0000_0000);
        mem.write_u8(0, 0x5A);
        let err = cpu
            .translate_paging_checked(&mut mem, 0x0000, crate::PageAccess::Read)
            .unwrap_err();
        assert_eq!(err.linear_address, 0x0000);
        assert_eq!(mem.read_u8(0), 0x5A);
    }
    #[test]
    fn test_r3_translate_paging_pte_avl_bits_accepted() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        mem.write_u32(0x1000, 0x0000_2007);
        mem.write_u32(0x2000, 0x0000_3E03);
        let phys = cpu
            .translate_paging_checked(&mut mem, 0x0000, crate::PageAccess::Read)
            .unwrap();
        assert_eq!(phys, 0x3000);
        assert!(mem.page_fault.is_none());
    }
    #[test]
    fn test_r3_translate_paging_sets_accessed_bits() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        mem.write_u32(0x1000, 0x0000_2003);
        mem.write_u32(0x2000, 0x0000_5003);
        let _phys = cpu
            .translate_paging_checked(&mut mem, 0x0000, crate::PageAccess::Read)
            .unwrap();
        let pde_after = mem.read_u32(0x1000);
        assert_ne!(pde_after & 0x20, 0, "PDE accessed bit should be set");
        let pte_after = mem.read_u32(0x2000);
        assert_ne!(pte_after & 0x20, 0, "PTE accessed bit should be set");
    }
    #[test]
    fn test_r3_translate_paging_writable_both() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        mem.write_u32(0x1000, 0x0000_2027);
        mem.write_u32(0x2000, 0x0000_5027);
        let phys = cpu
            .translate_paging_checked(&mut mem, 0x0000, crate::PageAccess::Read)
            .unwrap();
        assert_eq!(phys, 0x5000);
    }
    #[test]
    fn test_r3_translate_paging_not_writable_pde() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        mem.write_u32(0x1000, 0x0000_2025);
        mem.write_u32(0x2000, 0x0000_5027);
        let phys = cpu
            .translate_paging_checked(&mut mem, 0x0000, crate::PageAccess::Read)
            .unwrap();
        assert_eq!(phys, 0x5000);
    }
    #[test]
    fn test_r3_paging_checked_tlb_hit_success() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        mem.insert_tlb(0, 0x5, true, true);
        let result = cpu.translate_paging_checked(&mut mem, 0x0100, crate::PageAccess::Read);
        assert_eq!(result.unwrap(), 0x5100);
    }
    #[test]
    fn test_r3_paging_checked_tlb_hit_write_not_writable_user() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cpl = 3;
        mem.insert_tlb(0, 0x5, false, true);
        let result = cpu.translate_paging_checked(&mut mem, 0x0100, crate::PageAccess::Write);
        assert!(result.is_err());
    }
    #[test]
    fn test_r3_paging_checked_tlb_hit_write_not_writable_wp() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8001_0001;
        cpu.regs.cpl = 0;
        mem.insert_tlb(0, 0x5, false, true);
        let result = cpu.translate_paging_checked(&mut mem, 0x0100, crate::PageAccess::Write);
        assert!(result.is_err());
    }
    #[test]
    fn test_r3_paging_checked_tlb_hit_user_not_allowed() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cpl = 3;
        mem.insert_tlb(0, 0x5, true, false);
        let result = cpu.translate_paging_checked(&mut mem, 0x0100, crate::PageAccess::Read);
        assert!(result.is_err());
    }
    #[test]
    fn test_r3_paging_checked_tlb_write_to_clean_page_sets_dirty() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        mem.write_u32(0x1000, 0x0000_2007);
        mem.write_u32(0x2000, 0x0000_3007);
        let result = cpu.translate_paging_checked(&mut mem, 0x0000, crate::PageAccess::Read);
        assert!(result.is_ok());
        let tlb = mem.tlb.get(&0).unwrap();
        assert!(!tlb.3, "TLB should cache dirty=false after read");
        let result = cpu.translate_paging_checked(&mut mem, 0x0000, crate::PageAccess::Write);
        assert!(result.is_ok());
        let pte = mem.read_u32(0x2000);
        assert_ne!(pte & 0x40, 0, "PTE dirty bit must be set after write");
    }
    #[test]
    fn test_r3_paging_checked_pde_not_present() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        mem.write_u32(0x1000, 0x0000_0000);
        let result = cpu.translate_paging_checked(&mut mem, 0x0000, crate::PageAccess::Read);
        assert!(result.is_err());
    }
    #[test]
    fn test_r3_paging_checked_pde_bit7_accepted() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        mem.write_u32(0x1000, 0x0000_2081);
        mem.write_u32(0x2000, 0x0000_3007);
        let result = cpu.translate_paging_checked(&mut mem, 0x0000, crate::PageAccess::Read);
        assert!(result.is_ok(), "386 ignores undefined PDE bits");
    }
    #[test]
    fn test_r3_translate_paging_pse_4mb_page() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        cpu.regs.cr4 = 1 << 4;
        let linear = 0xC010_60D0;
        let pde_addr = 0x1000 + (((linear >> 22) as usize) * 4);
        mem.write_u32(pde_addr, 0x0000_0087);
        let phys = cpu
            .translate_paging_checked(&mut mem, linear, crate::PageAccess::Read)
            .unwrap();
        assert_eq!(phys, 0x0010_60D0);
        assert_eq!(mem.read_u32(pde_addr) & 0x20, 0x20);
        assert_eq!(mem.read_u32(pde_addr) & 0x40, 0x00);
        assert!(!mem.page_fault_pending());
    }
    #[test]
    fn test_r3_paging_checked_pse_4mb_page_write_sets_dirty_on_pde() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        cpu.regs.cr4 = 1 << 4;
        let linear = 0xC010_60D0;
        let pde_addr = 0x1000 + (((linear >> 22) as usize) * 4);
        mem.write_u32(pde_addr, 0x0000_0087);
        let result = cpu.translate_paging_checked(&mut mem, linear, crate::PageAccess::Write);
        assert_eq!(result.unwrap(), 0x0010_60D0);
        assert_eq!(mem.read_u32(pde_addr) & 0x20, 0x20);
        assert_eq!(mem.read_u32(pde_addr) & 0x40, 0x40);
        assert!(!mem.page_fault_pending());
    }
    #[test]
    fn test_r3_paging_checked_pte_not_present() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        mem.write_u32(0x1000, 0x0000_2007);
        mem.write_u32(0x2000, 0x0000_0000);
        let result = cpu.translate_paging_checked(&mut mem, 0x0000, crate::PageAccess::Read);
        assert!(result.is_err());
    }
    #[test]
    fn test_r3_paging_checked_pte_avl_bits_accepted() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        mem.write_u32(0x1000, 0x0000_2007);
        mem.write_u32(0x2000, 0x0000_0E03);
        let result = cpu.translate_paging_checked(&mut mem, 0x0000, crate::PageAccess::Read);
        assert!(result.is_ok());
    }
    #[test]
    fn test_r3_paging_checked_write_not_writable() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8001_0001;
        cpu.regs.cr3 = 0x1000;
        cpu.regs.cpl = 0;
        mem.write_u32(0x1000, 0x0000_2025);
        mem.write_u32(0x2000, 0x0000_5025);
        let result = cpu.translate_paging_checked(&mut mem, 0x0000, crate::PageAccess::Write);
        assert!(result.is_err());
    }
    #[test]
    fn test_r3_paging_checked_user_not_allowed_walk() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        cpu.regs.cpl = 3;
        mem.write_u32(0x1000, 0x0000_2023);
        mem.write_u32(0x2000, 0x0000_5023);
        let result = cpu.translate_paging_checked(&mut mem, 0x0000, crate::PageAccess::Read);
        assert!(result.is_err());
    }
    #[test]
    fn test_r3_paging_checked_sets_accessed_dirty_on_write() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        cpu.regs.cpl = 0;
        mem.write_u32(0x1000, 0x0000_2007);
        mem.write_u32(0x2000, 0x0000_5007);
        let result = cpu.translate_paging_checked(&mut mem, 0x0000, crate::PageAccess::Write);
        assert!(result.is_ok());
        assert_ne!(mem.read_u32(0x1000) & 0x20, 0);
        assert_ne!(mem.read_u32(0x2000) & 0x20, 0);
        assert_ne!(mem.read_u32(0x2000) & 0x40, 0);
    }
    #[test]
    fn test_r3_paging_checked_read_no_dirty_bit() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x1000;
        cpu.regs.cpl = 0;
        mem.write_u32(0x1000, 0x0000_2007);
        mem.write_u32(0x2000, 0x0000_5007);
        let result = cpu.translate_paging_checked(&mut mem, 0x0000, crate::PageAccess::Read);
        assert!(result.is_ok());
        assert_eq!(mem.read_u32(0x2000) & 0x40, 0);
    }
    #[test]
    fn test_r3_load_cs_pm_not_present() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x1A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_cs(&mut mem, 0x08).unwrap_err();
        assert!(matches!(err, CpuError::SegmentNotPresent(_)));
    }
    #[test]
    fn test_r3_load_cs_pm_system_descriptor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x89, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_cs(&mut mem, 0x08).unwrap_err();
        assert!(matches!(err, CpuError::TssDescriptor(0x08)));
        let desc_gate_16 = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x84, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc_gate_16);
        let err_gate_16 = cpu.load_cs(&mut mem, 0x08).unwrap_err();
        assert!(matches!(err_gate_16, CpuError::CallGate(0x08)));
        let desc_gate = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x8C, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc_gate);
        let err2 = cpu.load_cs(&mut mem, 0x08).unwrap_err();
        assert!(matches!(err2, CpuError::CallGate(0x08)));
    }
    #[test]
    fn test_r3_load_cs_pm_data_segment() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_cs(&mut mem, 0x08).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r3_load_cs_pm_conforming_dpl_gt_cpl() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFE, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_cs(&mut mem, 0x08).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r3_load_cs_pm_nonconforming_dpl_ne_cpl() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xBA, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_cs(&mut mem, 0x08).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r3_load_cs_pm_nonconforming_rpl_gt_cpl() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_cs(&mut mem, 0x0B).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r3_load_cs_pm_eip_corruption_warn() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.cs_bits = 16;
        cpu.regs.eip = 0x00010100;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x40, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.load_cs(&mut mem, 0x08).unwrap();
        assert_eq!(cpu.cs_bits, 32);
    }
    #[test]
    fn test_r3_load_cs_pm_null_selector_allow() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.allow_null_cs = true;
        cpu.regs.cs = 0x08;
        cpu.load_cs(&mut mem, 0x0000).unwrap();
        assert_eq!(cpu.regs.cs, 0x08);
    }
    #[test]
    fn test_r3_load_cs_iret_not_code() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_cs_iret(&mut mem, 0x08, 0).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    /// RETF/IRET to a system descriptor whose type bit 3 is set - a 386 TSS
    /// or a 32-bit call gate - is #GP, never a code load from the descriptor's
    /// fields (MDD-BUG-TEM-00164).
    #[test]
    fn test_load_cs_iret_rejects_tss_and_gate_descriptors() {
        for (access, cpl, selector) in [(0x8B_u8, 0_u8, 0x08_u16), (0xEC, 3, 0x0B)] {
            let mut cpu = Cpu::new();
            let mut mem = TestMemory::new(0x20000);
            cpu.regs.cr0 = 1;
            cpu.regs.cpl = cpl;
            cpu.regs.cs = 0x10;
            cpu.regs.gdtr.base = 0x1000;
            cpu.regs.gdtr.limit = 0x0F;
            let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, access, 0x00, 0x00];
            write_descriptor(&mut mem, 0x1000 + 8, desc);
            let err = cpu.load_cs_iret(&mut mem, selector, 0x1000).unwrap_err();
            assert!(
                matches!(err, CpuError::GeneralProtectionFault),
                "access {access:#04x}: {err:?}"
            );
            assert_eq!(cpu.regs.cs, 0x10, "access {access:#04x}: CS left unloaded");
        }
    }
    #[test]
    fn test_rejected_cs_load_does_not_set_descriptor_accessed_bit() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_cs(&mut mem, 0x0B).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
        assert_eq!(mem.read_u8(0x1000 + 8 + 5), 0x9A);
    }
    #[test]
    fn test_rejected_iret_cs_load_does_not_set_descriptor_accessed_bit() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_cs_iret(&mut mem, 0x0B, 0).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
        assert_eq!(mem.read_u8(0x1000 + 8 + 5), 0x9A);
    }
    #[test]
    fn test_r3_load_ss_null_selector_gpf() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        let err = cpu.load_ss(&mut mem, 0x0000).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r3_load_ss_code_segment() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_ss(&mut mem, 0x08).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r3_load_ss_not_writable() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x90, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_ss(&mut mem, 0x08).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r3_load_ss_dpl_ne_cpl() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xF2, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_ss(&mut mem, 0x08).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r3_load_ss_fallback_real_mode() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.allow_pm_real_mode_ss = true;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0000;
        cpu.load_ss(&mut mem, 0x10).unwrap();
        assert_eq!(cpu.regs.ss, 0x10);
        assert_eq!(cpu.regs.ss_cache.base, 0x0100);
    }
    #[test]
    fn test_r3_load_data_segment_checked_real_mode() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.load_data_segment_checked(&mut mem, DataSegReg::DS, 0x1234)
            .unwrap();
        assert_eq!(cpu.regs.ds, 0x1234);
    }
    #[test]
    fn test_r3_load_data_segment_checked_null_fs() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.load_data_segment_checked(&mut mem, DataSegReg::FS, 0x0000)
            .unwrap();
        assert!(!cpu.regs.fs_cache.present);
    }
    #[test]
    fn test_r3_load_data_segment_checked_null_gs() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.load_data_segment_checked(&mut mem, DataSegReg::GS, 0x0000)
            .unwrap();
        assert!(!cpu.regs.gs_cache.present);
    }
    #[test]
    fn test_r3_load_data_segment_checked_not_present() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x12, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu
            .load_data_segment_checked(&mut mem, DataSegReg::DS, 0x08)
            .unwrap_err();
        assert!(matches!(err, CpuError::SegmentNotPresent(_)));
    }
    #[test]
    fn test_r3_load_data_segment_checked_system_descriptor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x89, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu
            .load_data_segment_checked(&mut mem, DataSegReg::DS, 0x08)
            .unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r3_load_data_segment_checked_nonreadable_code() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x98, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu
            .load_data_segment_checked(&mut mem, DataSegReg::DS, 0x08)
            .unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r3_load_data_segment_checked_data_dpl_lt_max_priv() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu
            .load_data_segment_checked(&mut mem, DataSegReg::DS, 0x0B)
            .unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r3_invalidate_segment_cache_no_match() {
        let mut cpu = Cpu::new();
        let mem = TestMemory::new(0x20000);
        cpu.regs.cs = 0x10;
        cpu.regs.ds = 0x18;
        cpu.regs.es = 0x20;
        cpu.regs.fs = 0x28;
        cpu.regs.gs = 0x30;
        cpu.regs.ss = 0x38;
        cpu.invalidate_segment_cache(&mem, 0x08);
    }
    #[test]
    fn test_r3_push16_pm_32bit_stack() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.esp = 0x1000;
        cpu.push16(&mut mem, 0xABCD);
        assert_eq!(cpu.regs.esp, 0x0FFE);
        assert_eq!(mem.read_u16(0x0FFE), 0xABCD);
    }
    #[test]
    fn test_r3_push32_pm_32bit_stack() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.esp = 0x1000;
        cpu.push32(&mut mem, 0xDEADBEEF);
        assert_eq!(cpu.regs.esp, 0x0FFC);
        assert_eq!(mem.read_u32(0x0FFC), 0xDEADBEEF);
    }
    #[test]
    fn test_r3_pop16_pm_32bit_stack() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.esp = 0x1000;
        mem.write_u16(0x1000, 0xBEEF);
        let val = cpu.pop16(&mut mem);
        assert_eq!(val, 0xBEEF);
        assert_eq!(cpu.regs.esp, 0x1002);
    }
    #[test]
    fn test_r3_pop32_pm_32bit_stack() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.esp = 0x1000;
        mem.write_u32(0x1000, 0xDEADCAFE);
        let val = cpu.pop32(&mut mem);
        assert_eq!(val, 0xDEADCAFE);
        assert_eq!(cpu.regs.esp, 0x1004);
    }
    #[test]
    fn test_r3_try_push16_pm_32bit_stack_success() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.esp = 0x1000;
        cpu.try_push16(&mut mem, 0x1234).unwrap();
        assert_eq!(cpu.regs.esp, 0x0FFE);
    }
    #[test]
    fn test_r3_try_push32_pm_32bit_stack_success() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.esp = 0x1000;
        cpu.try_push32(&mut mem, 0x12345678).unwrap();
        assert_eq!(cpu.regs.esp, 0x0FFC);
    }
    #[test]
    fn test_r3_try_pop16_pm_16bit_esp_update() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = false;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.set_sp(0x1000);
        cpu.regs.esp = 0xFFFF_1000;
        mem.write_u16(0x1000, 0xCAFE);
        let val = cpu.try_pop16(&mut mem).unwrap();
        assert_eq!(val, 0xCAFE);
        assert_eq!(cpu.regs.sp(), 0x1002);
        assert_eq!(cpu.regs.esp, 0xFFFF_1002);
    }
    #[test]
    fn test_r3_try_pop32_pm_16bit_esp_update() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = false;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.set_sp(0x1000);
        cpu.regs.esp = 0xFFFF_1000;
        mem.write_u32(0x1000, 0xDEADBEEF);
        let val = cpu.try_pop32(&mut mem).unwrap();
        assert_eq!(val, 0xDEADBEEF);
        assert_eq!(cpu.regs.sp(), 0x1004);
        assert_eq!(cpu.regs.esp, 0xFFFF_1004);
    }
    #[test]
    fn test_r3_iret_pm_same_privilege() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let cs_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, cs_desc);
        cpu.regs.cs = 0x08;
        cpu.regs.cs_cache = SegmentCache::from_descriptor(&cs_desc);
        cpu.regs.ss = 0x1000;
        cpu.regs.ss_cache.base = 0x10000;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.set_sp(0x0100);
        cpu.regs.esp = 0x0100;
        cpu.regs.flags = crate::Flags::RESERVED1;
        let base = 0x10000usize;
        mem.write_u16(base + 0x0100, 0x0200);
        mem.write_u16(base + 0x0102, 0x0008);
        mem.write_u16(base + 0x0104, 0x0202);
        cpu.iret(&mut mem).unwrap();
        assert_eq!(cpu.regs.cpl, 0);
        assert_eq!(cpu.regs.ip(), 0x0200);
        assert!(cpu.interrupts_enabled);
    }
    #[test]
    fn test_r3_iret_pm_outer_segments_preserved_when_dpl_ge_new_cpl() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x1FFF;
        let cs0_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, cs0_desc);
        let cs3_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFA, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 16, cs3_desc);
        let ss3_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xF2, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 24, ss3_desc);
        cpu.regs.cs = 0x08;
        cpu.regs.cs_cache = SegmentCache::from_descriptor(&cs0_desc);
        cpu.regs.ss = 0x08;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.set_sp(0x1000);
        cpu.regs.esp = 0x1000;
        cpu.regs.ds = 0x18;
        cpu.regs.ds_cache.dpl = 3;
        cpu.regs.es = 0x18;
        cpu.regs.es_cache.dpl = 3;
        cpu.regs.fs = 0x18;
        cpu.regs.fs_cache.dpl = 3;
        cpu.regs.gs = 0x18;
        cpu.regs.gs_cache.dpl = 3;
        mem.write_u16(0x1000, 0x0200);
        mem.write_u16(0x1002, 0x0013);
        mem.write_u16(0x1004, 0x0202);
        mem.write_u16(0x1006, 0x2000);
        mem.write_u16(0x1008, 0x001B);
        cpu.iret(&mut mem).unwrap();
        assert_eq!(cpu.regs.cpl, 3);
        assert_eq!(cpu.regs.ds, 0x18);
        assert_eq!(cpu.regs.es, 0x18);
        assert_eq!(cpu.regs.fs, 0x18);
        assert_eq!(cpu.regs.gs, 0x18);
    }
    #[test]
    fn test_r3_iret_pm_cpl_gt_iopl_if_clear() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let cs_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFA, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, cs_desc);
        cpu.regs.cs = 0x0B;
        cpu.regs.cs_cache = SegmentCache::from_descriptor(&cs_desc);
        cpu.regs.ss = 0x1000;
        cpu.regs.ss_cache.base = 0x10000;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.set_sp(0x0100);
        cpu.regs.esp = 0x0100;
        cpu.regs.flags = crate::Flags::RESERVED1;
        cpu.interrupts_enabled = false;
        let base = 0x10000usize;
        mem.write_u16(base + 0x0100, 0x0200);
        mem.write_u16(base + 0x0102, 0x000B);
        mem.write_u16(base + 0x0104, 0x0202);
        cpu.iret(&mut mem).unwrap();
        assert!(!cpu.regs.flags.contains(crate::Flags::INTERRUPT));
    }
    #[test]
    fn test_r3_with_iret_flags_mut_ex_pm_32bit_stack() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss = 0x1000;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.esp = 0x1000;
        mem.write_u16(0x1008, 0x0000);
        let mut mutate = |flags: &mut u16| {
            *flags = 0xABCD;
        };
        cpu.with_iret_flags_mut_ex(&mut mem, &mut mutate, false)
            .unwrap();
        assert_eq!(mem.read_u16(0x1008), 0xABCD);
    }
    #[test]
    fn test_r3_with_iret_flags_mut_ex_pm_limit_exceeded() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss = 0x1000;
        cpu.regs.ss_cache.big = false;
        cpu.regs.ss_cache.set_limit(0x0003);
        cpu.regs.set_sp(0x0000);
        mem.write_u16(0x0004, 0x1234);
        let mut mutate = |flags: &mut u16| {
            *flags = 0x0000;
        };
        cpu.with_iret_flags_mut_ex(&mut mem, &mut mutate, false)
            .unwrap();
        assert_eq!(mem.read_u16(0x0004), 0x1234);
    }
    #[test]
    fn test_r3_stack_range_ok_expand_down() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.is_code = false;
        cpu.regs.ss_cache.access = 0x96;
        cpu.regs.ss_cache.set_limit(0x1000);
        cpu.regs.ss_cache.big = false;
        assert!(cpu.stack_range_ok(0x2000, 2));
        assert!(!cpu.stack_range_ok(0x0800, 2));
    }
    #[test]
    fn test_r3_translate_address_full_checked_expand_down_out_of_range() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 1;
        cpu.regs.ds = 0x10;
        cpu.regs.ds_cache.is_code = false;
        cpu.regs.ds_cache.access = 0x96;
        cpu.regs.ds_cache.set_limit(0x1000);
        cpu.regs.ds_cache.big = false;
        cpu.regs.ds_cache.base = 0;
        let err = cpu
            .translate_address_full_checked(&mut mem, 0x10, 0x0800, crate::PageAccess::Read)
            .unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r3_translate_address_full_checked_expand_down_ss_stack_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss = 0x10;
        cpu.regs.ss_cache.is_code = false;
        cpu.regs.ss_cache.access = 0x96;
        cpu.regs.ss_cache.set_limit(0x1000);
        cpu.regs.ss_cache.big = false;
        cpu.regs.ss_cache.base = 0;
        let err = cpu
            .translate_address_full_checked(&mut mem, 0x10, 0x0800, crate::PageAccess::Read)
            .unwrap_err();
        assert!(matches!(err, CpuError::StackFault));
    }
    #[test]
    fn test_r3_translate_address_full_checked_normal_limit_exceeded() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 1;
        cpu.regs.ds = 0x10;
        cpu.regs.ds_cache.is_code = false;
        cpu.regs.ds_cache.access = 0x92;
        cpu.regs.ds_cache.set_limit(0x0100);
        cpu.regs.ds_cache.big = false;
        cpu.regs.ds_cache.base = 0;
        let err = cpu
            .translate_address_full_checked(&mut mem, 0x10, 0x0200, crate::PageAccess::Read)
            .unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r3_translate_address_full_checked_big_expand_down() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 1;
        cpu.regs.ds = 0x10;
        cpu.regs.ds_cache.is_code = false;
        cpu.regs.ds_cache.access = 0x96;
        cpu.regs.ds_cache.set_limit(0x1000);
        cpu.regs.ds_cache.big = true;
        cpu.regs.ds_cache.base = 0;
        let result =
            cpu.translate_address_full_checked(&mut mem, 0x10, 0x2000, crate::PageAccess::Read);
        assert!(result.is_ok());
    }
    #[test]
    fn test_r3_push16_sp_wraparound() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.ss = 0x1000;
        cpu.regs.set_sp(0x0001);
        cpu.regs.esp = 0x0001;
        cpu.push16(&mut mem, 0xCAFE);
        assert_eq!(cpu.regs.sp(), 0xFFFF);
        let lo = mem.read_u8(0x1_FFFF);
        let hi = mem.read_u8(0x1_0000);
        assert_eq!(u16::from_le_bytes([lo, hi]), 0xCAFE);
    }
    #[test]
    fn test_r4_invalidate_cache_cs_match_with_descriptor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x02, 0x9A, 0x40, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.regs.cs = 0x08;
        cpu.regs.cs_cache.base = 0;
        cpu.cs_bits = 16;
        cpu.invalidate_segment_cache(&mem, 0x08);
        assert_eq!(cpu.regs.cs_cache.base, 0x00020000);
        assert_eq!(cpu.cs_bits, 32);
    }
    #[test]
    fn test_r4_invalidate_cache_ds_match_with_descriptor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x03, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.regs.ds = 0x08;
        cpu.regs.ds_cache.base = 0;
        cpu.invalidate_segment_cache(&mem, 0x08);
        assert_eq!(cpu.regs.ds_cache.base, 0x00030000);
    }
    #[test]
    fn test_r4_invalidate_cache_es_match_with_descriptor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x04, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.regs.es = 0x08;
        cpu.regs.es_cache.base = 0;
        cpu.invalidate_segment_cache(&mem, 0x08);
        assert_eq!(cpu.regs.es_cache.base, 0x00040000);
    }
    #[test]
    fn test_r4_invalidate_cache_fs_match_with_descriptor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x05, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.regs.fs = 0x08;
        cpu.regs.fs_cache.base = 0;
        cpu.invalidate_segment_cache(&mem, 0x08);
        assert_eq!(cpu.regs.fs_cache.base, 0x00050000);
    }
    #[test]
    fn test_r4_invalidate_cache_gs_match_with_descriptor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x06, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.regs.gs = 0x08;
        cpu.regs.gs_cache.base = 0;
        cpu.invalidate_segment_cache(&mem, 0x08);
        assert_eq!(cpu.regs.gs_cache.base, 0x00060000);
    }
    #[test]
    fn test_r4_invalidate_cache_ss_match_with_descriptor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x07, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.regs.ss = 0x08;
        cpu.regs.ss_cache.base = 0;
        cpu.invalidate_segment_cache(&mem, 0x08);
        assert_eq!(cpu.regs.ss_cache.base, 0x00070000);
    }
    #[test]
    fn test_r4_load_data_segment_pm_ds_descriptor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x05, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.load_data_segment(&mem, DataSegReg::DS, 0x08);
        assert_eq!(cpu.regs.ds, 0x08);
        assert_eq!(cpu.regs.ds_cache.base, 0x00050000);
    }
    #[test]
    fn test_r4_load_data_segment_pm_ds_null() {
        let mut cpu = Cpu::new();
        let mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.load_data_segment(&mem, DataSegReg::DS, 0x0000);
        assert_eq!(cpu.regs.ds, 0x0000);
        assert!(!cpu.regs.ds_cache.present);
    }
    #[test]
    fn test_r4_load_data_segment_pm_ds_no_descriptor() {
        let mut cpu = Cpu::new();
        let mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0000;
        cpu.load_data_segment(&mem, DataSegReg::DS, 0x10);
        assert_eq!(cpu.regs.ds, 0x10);
        assert_eq!(cpu.regs.ds_cache.base, 0x0100);
    }
    #[test]
    fn test_r4_load_data_segment_pm_es_descriptor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x06, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.load_data_segment(&mem, DataSegReg::ES, 0x08);
        assert_eq!(cpu.regs.es, 0x08);
        assert_eq!(cpu.regs.es_cache.base, 0x00060000);
    }
    #[test]
    fn test_r4_load_data_segment_pm_es_null() {
        let mut cpu = Cpu::new();
        let mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.load_data_segment(&mem, DataSegReg::ES, 0x0000);
        assert!(!cpu.regs.es_cache.present);
    }
    #[test]
    fn test_r4_load_data_segment_pm_es_no_descriptor() {
        let mut cpu = Cpu::new();
        let mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0000;
        cpu.load_data_segment(&mem, DataSegReg::ES, 0x10);
        assert_eq!(cpu.regs.es_cache.base, 0x0100);
    }
    #[test]
    fn test_r4_load_data_segment_pm_fs_descriptor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x07, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.load_data_segment(&mem, DataSegReg::FS, 0x08);
        assert_eq!(cpu.regs.fs_cache.base, 0x00070000);
    }
    #[test]
    fn test_r4_load_data_segment_pm_fs_null() {
        let mut cpu = Cpu::new();
        let mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.load_data_segment(&mem, DataSegReg::FS, 0x0000);
        assert!(!cpu.regs.fs_cache.present);
    }
    #[test]
    fn test_r4_load_data_segment_pm_fs_no_descriptor() {
        let mut cpu = Cpu::new();
        let mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0000;
        cpu.load_data_segment(&mem, DataSegReg::FS, 0x10);
        assert_eq!(cpu.regs.fs_cache.base, 0x0100);
    }
    #[test]
    fn test_r4_load_data_segment_pm_gs_descriptor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x08, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.load_data_segment(&mem, DataSegReg::GS, 0x08);
        assert_eq!(cpu.regs.gs_cache.base, 0x00080000);
    }
    #[test]
    fn test_r4_load_data_segment_pm_gs_null() {
        let mut cpu = Cpu::new();
        let mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.load_data_segment(&mem, DataSegReg::GS, 0x0000);
        assert!(!cpu.regs.gs_cache.present);
    }
    #[test]
    fn test_r4_load_data_segment_pm_gs_no_descriptor() {
        let mut cpu = Cpu::new();
        let mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0000;
        cpu.load_data_segment(&mem, DataSegReg::GS, 0x10);
        assert_eq!(cpu.regs.gs_cache.base, 0x0100);
    }
    #[test]
    fn test_r4_load_data_segment_checked_null_ds() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.load_data_segment_checked(&mut mem, DataSegReg::DS, 0x0000)
            .unwrap();
        assert!(!cpu.regs.ds_cache.present);
    }
    #[test]
    fn test_r4_load_data_segment_checked_conforming_ok() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9E, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.load_data_segment_checked(&mut mem, DataSegReg::DS, 0x08)
            .unwrap();
        assert_eq!(cpu.regs.ds, 0x08);
    }
    #[test]
    fn test_r4_load_data_segment_checked_no_descriptor_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0000;
        let err = cpu
            .load_data_segment_checked(&mut mem, DataSegReg::DS, 0x10)
            .unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r4_load_ss_pm_valid_descriptor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.load_ss(&mut mem, 0x08).unwrap();
        assert_eq!(cpu.regs.ss, 0x08);
    }
    #[test]
    fn test_r4_load_ss_pm_fallback() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.allow_pm_real_mode_ss = true;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0000;
        cpu.load_ss(&mut mem, 0x10).unwrap();
        assert_eq!(cpu.regs.ss_cache.base, 0x0100);
    }
    #[test]
    fn test_r4_load_ss_pm_no_fallback_gpf() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.allow_pm_real_mode_ss = false;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0000;
        let err = cpu.load_ss(&mut mem, 0x10).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r4_load_cs_iret_conforming_ok() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9E, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.load_cs_iret(&mut mem, 0x08, 0).unwrap();
        assert_eq!(cpu.regs.cs, 0x08);
    }
    #[test]
    fn test_r4_load_cs_iret_32bit() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x40, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.load_cs_iret(&mut mem, 0x08, 0).unwrap();
        assert_eq!(cpu.cs_bits, 32);
    }
    #[test]
    fn test_load_cs_iret_retains_16bit_descriptor_size() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        cpu.dos_extender_active = true;
        // Synthetic 64 KiB code segment at a round nonzero base; D/B is clear.
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x10, 0x9B, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.load_cs_iret(&mut mem, 0x08, 0).unwrap();
        assert_eq!(cpu.cs_bits, 16);
        assert!(!cpu.regs.cs_cache.big);
        assert_eq!(cpu.regs.cs_cache.base, 0x0010_0000);
    }
    #[test]
    fn test_r4_load_cs_iret_null_allowed() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.allow_null_cs = true;
        cpu.regs.cs = 0x08;
        cpu.load_cs_iret(&mut mem, 0x0000, 0).unwrap();
        assert_eq!(cpu.regs.cs, 0x08);
    }
    #[test]
    fn test_r4_load_cs_iret_no_descriptor_gpf() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.allow_null_cs = false;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0000;
        let err = cpu.load_cs_iret(&mut mem, 0x08, 0).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r4_load_cs_pm_16bit_segment() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.load_cs(&mut mem, 0x08).unwrap();
        assert_eq!(cpu.cs_bits, 16);
    }
    #[test]
    fn test_r4_load_cs_pm_null_selector_no_allow() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.allow_null_cs = false;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0000;
        let err = cpu.load_cs(&mut mem, 0x0000).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r4_iret_pm_outer_segments_zero_preserved() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x1FFF;
        let cs0_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, cs0_desc);
        let cs3_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFA, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 16, cs3_desc);
        let ss3_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xF2, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 24, ss3_desc);
        cpu.regs.cs = 0x08;
        cpu.regs.cs_cache = SegmentCache::from_descriptor(&cs0_desc);
        cpu.regs.ss = 0x08;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.set_sp(0x1000);
        cpu.regs.esp = 0x1000;
        cpu.regs.ds = 0;
        cpu.regs.ds_cache.dpl = 0;
        cpu.regs.es = 0;
        cpu.regs.es_cache.dpl = 0;
        cpu.regs.fs = 0;
        cpu.regs.fs_cache.dpl = 0;
        cpu.regs.gs = 0;
        cpu.regs.gs_cache.dpl = 0;
        mem.write_u16(0x1000, 0x0200);
        mem.write_u16(0x1002, 0x0013);
        mem.write_u16(0x1004, 0x0202);
        mem.write_u16(0x1006, 0x2000);
        mem.write_u16(0x1008, 0x001B);
        cpu.iret(&mut mem).unwrap();
        assert_eq!(cpu.regs.cpl, 3);
        assert_eq!(cpu.regs.ds, 0);
        assert_eq!(cpu.regs.es, 0);
    }
    #[test]
    fn test_r4_try_pop16_pm_big_stack_success() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.set_limit(0xFFFFFFFF);
        cpu.regs.ss_cache.base = 0;
        cpu.regs.esp = 0x1000;
        mem.write_u16(0x1000, 0xBEEF);
        let val = cpu.try_pop16(&mut mem).unwrap();
        assert_eq!(val, 0xBEEF);
        assert_eq!(cpu.regs.esp, 0x1002);
    }
    #[test]
    fn test_r4_try_pop32_pm_big_stack_success() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.set_limit(0xFFFFFFFF);
        cpu.regs.ss_cache.base = 0;
        cpu.regs.esp = 0x1000;
        mem.write_u32(0x1000, 0xDEADBEEF);
        let val = cpu.try_pop32(&mut mem).unwrap();
        assert_eq!(val, 0xDEADBEEF);
        assert_eq!(cpu.regs.esp, 0x1004);
    }
    #[test]
    fn test_r4_push16_pm_big_stack() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFFFFFF);
        cpu.regs.esp = 0x2000;
        cpu.push16(&mut mem, 0x1234);
        assert_eq!(cpu.regs.esp, 0x1FFE);
        assert_eq!(mem.read_u16(0x1FFE), 0x1234);
    }
    #[test]
    fn test_r4_push32_pm_big_stack() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFFFFFF);
        cpu.regs.esp = 0x2000;
        cpu.push32(&mut mem, 0xDEADBEEF);
        assert_eq!(cpu.regs.esp, 0x1FFC);
        assert_eq!(mem.read_u32(0x1FFC), 0xDEADBEEF);
    }
    #[test]
    fn test_r4_pop16_pm_big_stack() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFFFFFF);
        cpu.regs.esp = 0x2000;
        mem.write_u16(0x2000, 0xCAFE);
        let val = cpu.pop16(&mut mem);
        assert_eq!(val, 0xCAFE);
        assert_eq!(cpu.regs.esp, 0x2002);
    }
    #[test]
    fn test_r4_pop32_pm_big_stack() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFFFFFF);
        cpu.regs.esp = 0x2000;
        mem.write_u32(0x2000, 0xDEADC0DE);
        let val = cpu.pop32(&mut mem);
        assert_eq!(val, 0xDEADC0DE);
        assert_eq!(cpu.regs.esp, 0x2004);
    }
    #[test]
    fn test_r4_try_push16_pm_big_stack_ok() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFFFFFF);
        cpu.regs.esp = 0x2000;
        cpu.try_push16(&mut mem, 0xABCD).unwrap();
        assert_eq!(cpu.regs.esp, 0x1FFE);
    }
    #[test]
    fn test_r4_try_push32_pm_big_stack_ok() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFFFFFF);
        cpu.regs.esp = 0x2000;
        cpu.try_push32(&mut mem, 0x12345678).unwrap();
        assert_eq!(cpu.regs.esp, 0x1FFC);
    }
    #[test]
    fn test_r4_try_push16_pm_16bit_limit_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = false;
        cpu.regs.ss_cache.set_limit(0x0000);
        cpu.regs.set_sp(0x0002);
        let err = cpu.try_push16(&mut mem, 0x1234).unwrap_err();
        assert!(matches!(err, CpuError::StackFault));
    }
    #[test]
    fn test_r4_try_push32_pm_16bit_limit_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = false;
        cpu.regs.ss_cache.set_limit(0x0000);
        cpu.regs.set_sp(0x0004);
        let err = cpu.try_push32(&mut mem, 0x12345678).unwrap_err();
        assert!(matches!(err, CpuError::StackFault));
    }
    #[test]
    fn test_r4_push16_16bit_sp_ffff_wrap() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.ss = 0x1000;
        cpu.regs.set_sp(0x0001);
        cpu.regs.esp = 0x0001;
        cpu.push16(&mut mem, 0xBEEF);
        assert_eq!(cpu.regs.sp(), 0xFFFF);
    }
    #[test]
    fn test_r4_iret_pm_cpl_eq_iopl_if_changes() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let cs_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, cs_desc);
        cpu.regs.cs = 0x08;
        cpu.regs.cs_cache = SegmentCache::from_descriptor(&cs_desc);
        cpu.regs.ss = 0x1000;
        cpu.regs.ss_cache.base = 0x10000;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.set_sp(0x0100);
        cpu.regs.esp = 0x0100;
        cpu.regs.flags = crate::Flags::RESERVED1;
        cpu.interrupts_enabled = false;
        let base = 0x10000usize;
        mem.write_u16(base + 0x0100, 0x0200);
        mem.write_u16(base + 0x0102, 0x0008);
        mem.write_u16(base + 0x0104, 0x0202);
        cpu.iret(&mut mem).unwrap();
        assert!(cpu.interrupts_enabled);
    }
    #[test]
    fn test_r4_iret_pm_cpl_ne_0_iopl_preserved() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let cs_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFA, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, cs_desc);
        cpu.regs.cs = 0x0B;
        cpu.regs.cs_cache = SegmentCache::from_descriptor(&cs_desc);
        cpu.regs.ss = 0x1000;
        cpu.regs.ss_cache.base = 0x10000;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.set_sp(0x0100);
        cpu.regs.esp = 0x0100;
        cpu.regs.flags = crate::Flags::RESERVED1 | crate::Flags::INTERRUPT;
        cpu.interrupts_enabled = true;
        let base = 0x10000usize;
        mem.write_u16(base + 0x0100, 0x0200);
        mem.write_u16(base + 0x0102, 0x000B);
        mem.write_u16(base + 0x0104, 0x3202);
        cpu.iret(&mut mem).unwrap();
        assert_eq!(cpu.regs.iopl(), 0);
    }
    #[test]
    fn test_iret_outer_return_uses_pre_iret_cpl_for_iopl_and_if() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        write_descriptor(
            &mut mem,
            0x1000 + 0x08,
            [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00],
        );
        write_descriptor(
            &mut mem,
            0x1000 + 0x10,
            [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x92, 0x00, 0x00],
        );
        write_descriptor(
            &mut mem,
            0x1000 + 0x18,
            [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xBA, 0x00, 0x00],
        );
        write_descriptor(
            &mut mem,
            0x1000 + 0x20,
            [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xB2, 0x00, 0x00],
        );
        cpu.load_cs(&mut mem, 0x0008).unwrap();
        cpu.load_ss(&mut mem, 0x0010).unwrap();
        cpu.regs.set_sp(0x1000);
        cpu.regs.esp = 0x1000;
        cpu.regs.flags = crate::Flags::RESERVED1;
        cpu.interrupts_enabled = false;
        mem.write_u16(0x1000, 0x0200);
        mem.write_u16(0x1002, 0x0019);
        mem.write_u16(0x1004, 0x1202);
        mem.write_u16(0x1006, 0x2000);
        mem.write_u16(0x1008, 0x0021);
        cpu.iret(&mut mem).unwrap();
        assert_eq!(cpu.regs.cpl, 1);
        assert_eq!(cpu.regs.cs, 0x0019);
        assert_eq!(cpu.regs.ss, 0x0021);
        assert_eq!(cpu.regs.iopl(), 1);
        assert!(cpu.regs.flags.contains(crate::Flags::INTERRUPT));
        assert!(cpu.interrupts_enabled);
    }
    #[test]
    fn test_iretd_outer_return_uses_pre_iret_cpl_for_iopl_and_if() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        write_descriptor(
            &mut mem,
            0x1000 + 0x08,
            [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x40, 0x00],
        );
        write_descriptor(
            &mut mem,
            0x1000 + 0x10,
            [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x92, 0x40, 0x00],
        );
        write_descriptor(
            &mut mem,
            0x1000 + 0x18,
            [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFA, 0x40, 0x00],
        );
        write_descriptor(
            &mut mem,
            0x1000 + 0x20,
            [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xF2, 0x40, 0x00],
        );
        cpu.load_cs(&mut mem, 0x0008).unwrap();
        cpu.load_ss(&mut mem, 0x0010).unwrap();
        cpu.regs.esp = 0x1000;
        cpu.regs.flags = crate::Flags::RESERVED1;
        cpu.interrupts_enabled = false;
        mem.write_u32(0x1000, 0x0000_0200);
        mem.write_u32(0x1004, 0x001B);
        mem.write_u32(0x1008, 0x0000_3202);
        mem.write_u32(0x100C, 0x0000_2000);
        mem.write_u32(0x1010, 0x0023);
        cpu.execute_iretd(&mut mem).unwrap();
        assert_eq!(cpu.regs.cpl, 3);
        assert_eq!(cpu.regs.cs, 0x001B);
        assert_eq!(cpu.regs.ss, 0x0023);
        assert_eq!(cpu.regs.iopl(), 3);
        assert!(cpu.regs.flags.contains(crate::Flags::INTERRUPT));
        assert!(cpu.interrupts_enabled);
    }
    #[test]
    fn test_r4_with_iret_flags_pm_big_stack() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss = 0x1000;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFFFFFF);
        cpu.regs.esp = 0x1000;
        mem.write_u16(0x1008, 0x0000);
        let mut mutate = |flags: &mut u16| {
            *flags = 0x5678;
        };
        cpu.with_iret_flags_mut_ex(&mut mem, &mut mutate, false)
            .unwrap();
        assert_eq!(mem.read_u16(0x1008), 0x5678);
    }
    #[test]
    fn test_r4_with_iret_flags_pm_limit_exceeded() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = false;
        cpu.regs.ss_cache.set_limit(0x0003);
        cpu.regs.set_sp(0x0000);
        mem.write_u16(0x0004, 0x1234);
        let mut mutate = |flags: &mut u16| {
            *flags = 0x0000;
        };
        cpu.with_iret_flags_mut_ex(&mut mem, &mut mutate, false)
            .unwrap();
        assert_eq!(mem.read_u16(0x0004), 0x1234);
    }
    #[test]
    fn test_r4_stack_range_ok_expand_down_pass() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.is_code = false;
        cpu.regs.ss_cache.access = 0x96;
        cpu.regs.ss_cache.set_limit(0x1000);
        cpu.regs.ss_cache.big = false;
        assert!(cpu.stack_range_ok(0x2000, 2));
    }
    #[test]
    fn test_r4_stack_range_ok_expand_down_fail() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.is_code = false;
        cpu.regs.ss_cache.access = 0x96;
        cpu.regs.ss_cache.set_limit(0x1000);
        cpu.regs.ss_cache.big = false;
        assert!(!cpu.stack_range_ok(0x0800, 2));
    }
    #[test]
    fn test_r4_try_pop16_pm_not_big_esp_mask() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = false;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.set_sp(0x1000);
        cpu.regs.esp = 0xDEAD_1000;
        mem.write_u16(0x1000, 0x4567);
        let val = cpu.try_pop16(&mut mem).unwrap();
        assert_eq!(val, 0x4567);
        assert_eq!(cpu.regs.esp, 0xDEAD_1002);
    }
    #[test]
    fn test_r4_try_pop32_pm_not_big_esp_mask() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = false;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.set_sp(0x1000);
        cpu.regs.esp = 0xDEAD_1000;
        mem.write_u32(0x1000, 0x89ABCDEF);
        let val = cpu.try_pop32(&mut mem).unwrap();
        assert_eq!(val, 0x89ABCDEF);
        assert_eq!(cpu.regs.esp, 0xDEAD_1004);
    }
    #[test]
    fn test_r5_read_descriptor_gdt_nonzero_index() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let cache = cpu.read_descriptor(&mem, 0x08);
        assert!(cache.is_some());
        assert!(cache.unwrap().is_code);
    }
    #[test]
    fn test_r5_read_descriptor_ldt_selector() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.ldt_base = 0x2000;
        cpu.regs.ldt_limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x2000 + 8, desc);
        let cache = cpu.read_descriptor(&mem, 0x0F);
        assert!(cache.is_some());
        assert!(cache.unwrap().is_writable);
    }
    #[test]
    fn test_r5_read_descriptor_ldt_null_index_valid() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.ldt_base = 0x2000;
        cpu.regs.ldt_limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x2000, desc);
        let cache = cpu.read_descriptor(&mem, 0x04);
        assert!(cache.is_some());
    }
    #[test]
    fn test_r5_read_descriptor_ldt_outside_limit() {
        let mut cpu = Cpu::new();
        let mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.ldt_base = 0;
        cpu.regs.ldt_limit = 0x07;
        let cache = cpu.read_descriptor(&mem, 0x0C);
        assert!(cache.is_none());
    }
    #[test]
    fn test_r5_stack_range_ok_not_expand_down() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.is_code = false;
        cpu.regs.ss_cache.access = 0x92;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ss_cache.big = false;
        assert!(cpu.stack_range_ok(0x1000, 2));
    }
    #[test]
    fn test_r5_stack_range_ok_not_expand_down_fail() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.is_code = false;
        cpu.regs.ss_cache.access = 0x92;
        cpu.regs.ss_cache.set_limit(0x0FFF);
        cpu.regs.ss_cache.big = false;
        assert!(!cpu.stack_range_ok(0x1000, 2));
    }
    #[test]
    fn test_r5_translate_address_full_linear_map() {
        let cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        mem.map_linear(0x5000, 0x9000);
        let phys = cpu.translate_address_full(&mut mem, 0x0000, 0x5000);
        assert_eq!(phys, 0x9000);
    }
    #[test]
    fn test_r5_translate_address_full_paging() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        let cr3: u32 = 0x100000;
        cpu.regs.cr3 = cr3;
        let pde_addr = cr3 as usize;
        let page_table_addr: u32 = 0x101000;
        mem.write_u32(pde_addr, page_table_addr | 0x07);
        let pte_addr = page_table_addr as usize;
        mem.write_u32(pte_addr, 0x50007);
        cpu.regs.cs_cache.base = 0;
        let phys = cpu.translate_address_full(&mut mem, cpu.regs.cs, 0x100);
        assert_eq!(phys, 0x50100);
    }
    #[test]
    fn test_r5_translate_address_full_checked_linear_map() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ds = 0x08;
        cpu.regs.ds_cache = SegmentCache {
            base: 0,
            limit: 0xFFFF,
            cached_effective_limit: 0xFFFF,
            access: 0x93,
            present: true,
            is_writable: true,
            is_readable: true,
            ..Default::default()
        };
        let mut mem = TestMemory::new(0x20000);
        mem.map_linear(0x5000, 0x9000);
        let phys = cpu
            .translate_address_full_checked(&mut mem, 0x08, 0x5000, PageAccess::Read)
            .unwrap();
        assert_eq!(phys, 0x9000);
    }
    #[test]
    fn test_r5_translate_address_full_checked_paging() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        let cr3: u32 = 0x100000;
        cpu.regs.cr3 = cr3;
        mem.write_u32(cr3 as usize, 0x101007);
        mem.write_u32(0x101000, 0x50007);
        let phys = cpu
            .translate_address_full_checked(&mut mem, 0x0000, 0x100, PageAccess::Read)
            .unwrap();
        assert_eq!(phys, 0x50100);
    }
    #[test]
    fn test_r5_translate_address_full_checked_ss_stack_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss = 0x10;
        cpu.regs.ss_cache = SegmentCache {
            base: 0,
            limit: 0x0FFF,
            cached_effective_limit: 0x0FFF,
            access: 0x92,
            present: true,
            is_writable: true,
            ..Default::default()
        };
        let err = cpu
            .translate_address_full_checked(&mut mem, 0x10, 0x2000, PageAccess::Read)
            .unwrap_err();
        assert!(matches!(err, CpuError::StackFault));
    }
    #[test]
    fn test_r5_translate_linear_checked_linear_map() {
        let cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        mem.map_linear(0x3000, 0x7000);
        let phys = cpu
            .translate_linear_checked(&mut mem, 0x3000, PageAccess::Read)
            .unwrap();
        assert_eq!(phys, 0x7000);
    }
    #[test]
    fn test_r5_translate_linear_checked_paging_enabled() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        let cr3: u32 = 0x100000;
        cpu.regs.cr3 = cr3;
        mem.write_u32(cr3 as usize, 0x101007);
        mem.write_u32(0x101000, 0x50007);
        let phys = cpu
            .translate_linear_checked(&mut mem, 0x100, PageAccess::Read)
            .unwrap();
        assert_eq!(phys, 0x50100);
    }
    #[test]
    fn test_r5_translate_linear_checked_no_paging() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 0;
        let phys = cpu
            .translate_linear_checked(&mut mem, 0x3000, PageAccess::Read)
            .unwrap();
        assert_eq!(phys, 0x3000);
    }
    #[test]
    fn test_r5_translate_linear_checked_page_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x100000;
        mem.write_u32(0x100000, 0);
        let err = cpu
            .translate_linear_checked(&mut mem, 0x1000, PageAccess::Read)
            .unwrap_err();
        assert!(matches!(err, CpuError::PageFault(_)));
    }
    #[test]
    fn test_r5_tlb_hit_write_writable_no_error() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cpl = 0;
        mem.insert_tlb_dirty(0, 0x50, true, true, true);
        let phys = cpu
            .translate_linear_checked(&mut mem, 0x100, PageAccess::Write)
            .unwrap();
        assert_eq!(phys, 0x50100);
    }
    #[test]
    fn test_r5_tlb_hit_user_allowed_no_error() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cpl = 3;
        mem.insert_tlb(0, 0x50, true, true);
        let phys = cpu
            .translate_linear_checked(&mut mem, 0x100, PageAccess::Read)
            .unwrap();
        assert_eq!(phys, 0x50100);
    }
    #[test]
    fn test_r5_tlb_hit_write_not_writable_user_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cpl = 3;
        mem.insert_tlb(0, 0x50, false, true);
        let err = cpu
            .translate_linear_checked(&mut mem, 0x100, PageAccess::Write)
            .unwrap_err();
        assert!(matches!(err, CpuError::PageFault(_)));
    }
    #[test]
    fn test_r5_tlb_hit_user_not_allowed_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cpl = 3;
        mem.insert_tlb(0, 0x50, true, false);
        let err = cpu
            .translate_linear_checked(&mut mem, 0x100, PageAccess::Read)
            .unwrap_err();
        assert!(matches!(err, CpuError::PageFault(_)));
    }
    #[test]
    fn test_r5_page_walk_accessed_dirty_already_set() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        let cr3: u32 = 0x100000;
        cpu.regs.cr3 = cr3;
        mem.write_u32(cr3 as usize, 0x101027);
        mem.write_u32(0x101000, 0x50067);
        let phys = cpu
            .translate_linear_checked(&mut mem, 0x100, PageAccess::Write)
            .unwrap();
        assert_eq!(phys, 0x50100);
        assert_eq!(mem.read_u32(cr3 as usize), 0x101027);
        assert_eq!(mem.read_u32(0x101000), 0x50067);
    }
    #[test]
    fn test_r5_page_walk_sets_accessed_dirty() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        let cr3: u32 = 0x100000;
        cpu.regs.cr3 = cr3;
        mem.write_u32(cr3 as usize, 0x101007);
        mem.write_u32(0x101000, 0x50007);
        let phys = cpu
            .translate_linear_checked(&mut mem, 0x100, PageAccess::Write)
            .unwrap();
        assert_eq!(phys, 0x50100);
        assert_eq!(mem.read_u32(cr3 as usize), 0x101027);
        assert_eq!(mem.read_u32(0x101000), 0x50067);
    }
    #[test]
    fn test_r5_load_cs_allow_null_cs_null_selector() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.allow_null_cs = true;
        cpu.regs.cs = 0x0008;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        cpu.load_cs(&mut mem, 0x0000).unwrap();
        assert_eq!(cpu.regs.cs, 0x0008);
    }
    #[test]
    fn test_r5_load_cs_no_descriptor_gpf() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.allow_null_cs = true;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        cpu.regs.gdtr.limit = 0x07;
        let err = cpu.load_cs(&mut mem, 0x0008).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r5_load_cs_not_present() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x1A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_cs(&mut mem, 0x0008).unwrap_err();
        assert!(matches!(err, CpuError::SegmentNotPresent(_)));
    }
    #[test]
    fn test_r5_load_cs_system_descriptor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_cs(&mut mem, 0x0008).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r5_load_cs_data_segment() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_cs(&mut mem, 0x0008).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r5_load_cs_big_sets_32bit() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x40, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.load_cs(&mut mem, 0x0008).unwrap();
        assert_eq!(cpu.cs_bits, 32);
        assert!(cpu.regs.cs_cache.big);
    }
    #[test]
    fn test_r5_load_cs_iret_no_descriptor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x07;
        let err = cpu.load_cs_iret(&mut mem, 0x0008, 0).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r5_load_cs_iret_not_present() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x1A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_cs_iret(&mut mem, 0x0008, 0).unwrap_err();
        assert!(
            matches!(err, CpuError::SegmentNotPresent(sel) if sel == 0x0008),
            "Not-present code segment should raise #NP with selector, got {:?}",
            err,
        );
    }
    #[test]
    fn test_r5_load_cs_iret_not_code() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_cs_iret(&mut mem, 0x0008, 0).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r5_load_cs_iret_new_cpl_less_than_old() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_cs_iret(&mut mem, 0x0008, 0).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r5_load_cs_iret_conforming_dpl_greater_than_new_cpl() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFE, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_cs_iret(&mut mem, 0x0008, 0).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r5_load_cs_iret_nonconforming_dpl_neq_new_cpl() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_cs_iret(&mut mem, 0x000B, 0).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r5_load_cs_iret_big_segment() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x40, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.load_cs_iret(&mut mem, 0x0008, 0).unwrap();
        assert_eq!(cpu.cs_bits, 32);
    }
    #[test]
    fn test_r5_load_cs_iret_allow_null_cs() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.allow_null_cs = true;
        cpu.regs.cs = 0x0008;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        cpu.load_cs_iret(&mut mem, 0x0000, 0).unwrap();
        assert_eq!(cpu.regs.cs, 0x0008);
    }
    #[test]
    fn test_r5_load_cs_iret_allow_null_but_nonnull_no_desc() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.allow_null_cs = true;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x07;
        let err = cpu.load_cs_iret(&mut mem, 0x0008, 0).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r5_load_ss_null_selector_pm() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        let err = cpu.load_ss(&mut mem, 0x0000).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r5_load_ss_no_descriptor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x07;
        let err = cpu.load_ss(&mut mem, 0x0008).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r5_load_ss_not_present() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x12, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_ss(&mut mem, 0x0008).unwrap_err();
        assert!(matches!(err, CpuError::StackFault));
    }
    #[test]
    fn test_r5_load_ss_code_segment() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_ss(&mut mem, 0x0008).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r5_load_ss_not_writable() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x90, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_ss(&mut mem, 0x0008).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r5_load_ss_privilege_mismatch() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xF2, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_ss(&mut mem, 0x0008).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r5_load_ss_allow_pm_real_mode() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.allow_pm_real_mode_ss = true;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x07;
        cpu.load_ss(&mut mem, 0x0008).unwrap();
        assert_eq!(cpu.regs.ss, 0x0008);
        assert_eq!(cpu.regs.ss_cache.base, 0x80);
    }
    #[test]
    fn test_r5_load_data_segment_ds_no_descriptor_nonnull() {
        let mut cpu = Cpu::new();
        let mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x07;
        cpu.load_data_segment(&mem, DataSegReg::DS, 0x0008);
        assert_eq!(cpu.regs.ds, 0x0008);
        assert_eq!(cpu.regs.ds_cache.base, 0x80);
    }
    #[test]
    fn test_r5_load_data_segment_es_null_selector() {
        let mut cpu = Cpu::new();
        let mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        cpu.load_data_segment(&mem, DataSegReg::ES, 0x0000);
        assert_eq!(cpu.regs.es, 0x0000);
        assert_eq!(cpu.regs.es_cache.base, 0);
    }
    #[test]
    fn test_r5_load_data_segment_fs_no_descriptor_nonnull() {
        let mut cpu = Cpu::new();
        let mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x07;
        cpu.load_data_segment(&mem, DataSegReg::FS, 0x0008);
        assert_eq!(cpu.regs.fs, 0x0008);
        assert_eq!(cpu.regs.fs_cache.base, 0x80);
    }
    #[test]
    fn test_r5_load_data_segment_gs_no_descriptor_nonnull() {
        let mut cpu = Cpu::new();
        let mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x07;
        cpu.load_data_segment(&mem, DataSegReg::GS, 0x0008);
        assert_eq!(cpu.regs.gs, 0x0008);
        assert_eq!(cpu.regs.gs_cache.base, 0x80);
    }
    #[test]
    fn test_r5_load_data_segment_checked_null_ds() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        cpu.load_data_segment_checked(&mut mem, DataSegReg::DS, 0x0000)
            .unwrap();
        assert_eq!(cpu.regs.ds, 0x0000);
    }
    #[test]
    fn test_r5_load_data_segment_checked_null_es() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.load_data_segment_checked(&mut mem, DataSegReg::ES, 0x0000)
            .unwrap();
        assert_eq!(cpu.regs.es, 0x0000);
    }
    #[test]
    fn test_r5_load_data_segment_checked_null_fs() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.load_data_segment_checked(&mut mem, DataSegReg::FS, 0x0000)
            .unwrap();
        assert_eq!(cpu.regs.fs, 0x0000);
    }
    #[test]
    fn test_r5_load_data_segment_checked_null_gs() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.load_data_segment_checked(&mut mem, DataSegReg::GS, 0x0000)
            .unwrap();
        assert_eq!(cpu.regs.gs, 0x0000);
    }
    #[test]
    fn test_r5_load_data_segment_checked_not_present() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x12, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu
            .load_data_segment_checked(&mut mem, DataSegReg::DS, 0x0008)
            .unwrap_err();
        assert!(matches!(err, CpuError::SegmentNotPresent(_)));
    }
    #[test]
    fn test_r5_load_data_segment_checked_system_descriptor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu
            .load_data_segment_checked(&mut mem, DataSegReg::DS, 0x0008)
            .unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r5_load_data_segment_checked_code_not_readable() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x98, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu
            .load_data_segment_checked(&mut mem, DataSegReg::DS, 0x0008)
            .unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r5_load_data_segment_checked_conforming_dpl_above_cpl_ok() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFE, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.load_data_segment_checked(&mut mem, DataSegReg::DS, 0x0008)
            .unwrap();
        assert_eq!(cpu.regs.ds, 0x0008);
        assert!(cpu.regs.ds_cache.is_conforming);
    }
    #[test]
    fn test_r5_load_data_segment_checked_nonconforming_code_dpl_lt_max_priv() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu
            .load_data_segment_checked(&mut mem, DataSegReg::DS, 0x0008)
            .unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r5_load_data_segment_checked_data_dpl_lt_max_priv() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu
            .load_data_segment_checked(&mut mem, DataSegReg::DS, 0x0008)
            .unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r5_load_data_segment_checked_no_descriptor_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x07;
        let err = cpu
            .load_data_segment_checked(&mut mem, DataSegReg::DS, 0x0008)
            .unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r5_load_data_segment_checked_conforming_code_ok() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9E, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.load_data_segment_checked(&mut mem, DataSegReg::DS, 0x000B)
            .unwrap();
    }
    #[test]
    fn test_r5_invalidate_segment_cache_no_descriptor() {
        let mut cpu = Cpu::new();
        let mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x07;
        let sel: u16 = 0x0008;
        cpu.regs.cs = sel;
        cpu.regs.ds = sel;
        cpu.regs.es = sel;
        cpu.regs.fs = sel;
        cpu.regs.gs = sel;
        cpu.regs.ss = sel;
        let orig_cs = cpu.regs.cs_cache;
        let orig_ds = cpu.regs.ds_cache;
        cpu.invalidate_segment_cache(&mem, sel);
        assert_eq!(cpu.regs.cs_cache.base, orig_cs.base);
        assert_eq!(cpu.regs.ds_cache.base, orig_ds.base);
    }
    #[test]
    fn test_r5_invalidate_segment_cache_updates() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let sel: u16 = 0x0008;
        let cs_desc = [0xFF, 0xFF, 0x00, 0x00, 0x01, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, cs_desc);
        cpu.regs.cs = sel;
        cpu.regs.ds = sel;
        cpu.regs.es = sel;
        cpu.regs.fs = sel;
        cpu.regs.gs = sel;
        cpu.regs.ss = sel;
        cpu.invalidate_segment_cache(&mem, sel);
        assert_eq!(cpu.regs.cs_cache.base, 0x10000);
        assert_eq!(cpu.regs.ds_cache.base, 0x10000);
        assert_eq!(cpu.regs.es_cache.base, 0x10000);
        assert_eq!(cpu.regs.fs_cache.base, 0x10000);
        assert_eq!(cpu.regs.gs_cache.base, 0x10000);
        assert_eq!(cpu.regs.ss_cache.base, 0x10000);
    }
    #[test]
    fn test_r5_try_pop16_pm_big_stack_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0x0FFF);
        cpu.regs.ss_cache.access = 0x92;
        cpu.regs.ss_cache.is_code = false;
        cpu.regs.esp = 0x2000;
        let err = cpu.try_pop16(&mut mem).unwrap_err();
        assert!(matches!(err, CpuError::StackFault));
    }
    #[test]
    fn test_r5_try_pop32_pm_big_stack_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0x0FFF);
        cpu.regs.ss_cache.access = 0x92;
        cpu.regs.ss_cache.is_code = false;
        cpu.regs.esp = 0x2000;
        let err = cpu.try_pop32(&mut mem).unwrap_err();
        assert!(matches!(err, CpuError::StackFault));
    }
    #[test]
    fn test_r5_try_pop16_sp_0xffff_wrap() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.cpu_type = CpuType::I8086;
        cpu.regs.ss = 0x0100;
        cpu.regs.set_sp(0xFFFF);
        let base = 0x1000usize;
        mem.write_u8(base + 0xFFFF, 0xAB);
        mem.write_u8(base, 0xCD);
        let val = cpu.try_pop16(&mut mem).unwrap();
        assert_eq!(val, 0xCDAB);
        assert_eq!(cpu.regs.sp(), 0x0001);
    }
    #[test]
    fn test_r5_with_iret_flags_mut_ex_force_32bit() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.esp = 0x1000;
        cpu.regs.set_sp(0x1000);
        mem.write_u16(0x1008, 0x1234);
        let mut mutate = |flags: &mut u16| {
            *flags = 0x5678;
        };
        cpu.with_iret_flags_mut_ex(&mut mem, &mut mutate, true)
            .unwrap();
        assert_eq!(mem.read_u16(0x1008), 0x5678);
    }
    #[test]
    fn test_r5_with_iret_flags_mut_ex_real_mode_16bit() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.set_sp(0x1000);
        cpu.regs.esp = 0x1000;
        mem.write_u16(0x1004, 0xAAAA);
        let mut mutate = |flags: &mut u16| {
            *flags = 0xBBBB;
        };
        cpu.with_iret_flags_mut_ex(&mut mem, &mut mutate, false)
            .unwrap();
        assert_eq!(mem.read_u16(0x1004), 0xBBBB);
    }
    #[test]
    fn test_r5_push16_pm_big() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFFFFFF);
        cpu.regs.esp = 0x2000;
        cpu.push16(&mut mem, 0x1234);
        assert_eq!(cpu.regs.esp, 0x1FFE);
        assert_eq!(mem.read_u16(0x1FFE), 0x1234);
    }
    #[test]
    fn test_r5_push16_real_mode_sp_wrap() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.ss = 0x0100;
        cpu.regs.set_sp(0x0001);
        cpu.regs.esp = 0x0001;
        cpu.push16(&mut mem, 0xABCD);
        assert_eq!(cpu.regs.sp(), 0xFFFF);
        let base = 0x1000usize;
        assert_eq!(mem.read_u8(base + 0xFFFF), 0xCD);
        assert_eq!(mem.read_u8(base), 0xAB);
    }
    #[test]
    fn test_r5_push32_pm_big() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFFFFFF);
        cpu.regs.esp = 0x2000;
        cpu.push32(&mut mem, 0xDEADBEEF);
        assert_eq!(cpu.regs.esp, 0x1FFC);
        assert_eq!(mem.read_u32(0x1FFC), 0xDEADBEEF);
    }
    #[test]
    fn test_r5_pop16_pm_big() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFFFFFF);
        cpu.regs.esp = 0x2000;
        mem.write_u16(0x2000, 0x5678);
        let val = cpu.pop16(&mut mem);
        assert_eq!(val, 0x5678);
        assert_eq!(cpu.regs.esp, 0x2002);
    }
    #[test]
    fn test_r5_pop16_real_mode() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.set_sp(0x1000);
        cpu.regs.esp = 0xDEAD_1000;
        mem.write_u16(0x1000, 0xABCD);
        let val = cpu.pop16(&mut mem);
        assert_eq!(val, 0xABCD);
        assert_eq!(cpu.regs.sp(), 0x1002);
        assert_eq!(cpu.regs.esp, 0xDEAD_1002);
    }
    #[test]
    fn test_r5_pop32_pm_big() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFFFFFF);
        cpu.regs.esp = 0x2000;
        mem.write_u32(0x2000, 0x12345678);
        let val = cpu.pop32(&mut mem);
        assert_eq!(val, 0x12345678);
        assert_eq!(cpu.regs.esp, 0x2004);
    }
    #[test]
    fn test_r5_pop32_real_mode() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.set_sp(0x1000);
        cpu.regs.esp = 0xDEAD_1000;
        mem.write_u32(0x1000, 0xCAFEBABE);
        let val = cpu.pop32(&mut mem);
        assert_eq!(val, 0xCAFEBABE);
        assert_eq!(cpu.regs.sp(), 0x1004);
        assert_eq!(cpu.regs.esp, 0xDEAD_1004);
    }
    #[test]
    fn test_r5_try_push16_pm_big_stack_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0x0FFF);
        cpu.regs.ss_cache.access = 0x92;
        cpu.regs.ss_cache.is_code = false;
        cpu.regs.esp = 0x2000;
        let err = cpu.try_push16(&mut mem, 0x1234).unwrap_err();
        assert!(matches!(err, CpuError::StackFault));
    }
    #[test]
    fn test_r5_try_push16_pm_16bit_stack_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = false;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0x0FFF);
        cpu.regs.ss_cache.access = 0x92;
        cpu.regs.ss_cache.is_code = false;
        cpu.regs.set_sp(0x2000);
        let err = cpu.try_push16(&mut mem, 0x1234).unwrap_err();
        assert!(matches!(err, CpuError::StackFault));
    }
    #[test]
    fn test_r5_try_push32_pm_big_stack_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0x0FFF);
        cpu.regs.ss_cache.access = 0x92;
        cpu.regs.ss_cache.is_code = false;
        cpu.regs.esp = 0x2000;
        let err = cpu.try_push32(&mut mem, 0x12345678).unwrap_err();
        assert!(matches!(err, CpuError::StackFault));
    }
    #[test]
    fn test_r5_try_push32_pm_16bit_stack_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = false;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0x0FFF);
        cpu.regs.ss_cache.access = 0x92;
        cpu.regs.ss_cache.is_code = false;
        cpu.regs.set_sp(0x2000);
        let err = cpu.try_push32(&mut mem, 0x12345678).unwrap_err();
        assert!(matches!(err, CpuError::StackFault));
    }
    #[test]
    fn test_r5_iret_returning_outer_clears_segments() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        cpu.regs.ss_cache.big = false;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.set_sp(0xF000);
        cpu.regs.esp = 0xF000;
        cpu.regs.ds = 0x0010;
        cpu.regs.ds_cache = SegmentCache {
            dpl: 0,
            ..Default::default()
        };
        cpu.regs.es = 0x0010;
        cpu.regs.es_cache = SegmentCache {
            dpl: 0,
            ..Default::default()
        };
        cpu.regs.fs = 0x0010;
        cpu.regs.fs_cache = SegmentCache {
            dpl: 0,
            ..Default::default()
        };
        cpu.regs.gs = 0x0010;
        cpu.regs.gs_cache = SegmentCache {
            dpl: 0,
            ..Default::default()
        };
        let cs_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFA, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 16, cs_desc);
        let ss_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xF2, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 24, ss_desc);
        let base = 0xF000usize;
        mem.write_u16(base, 0x0100);
        mem.write_u16(base + 2, 0x0013);
        mem.write_u16(base + 4, 0x0202);
        mem.write_u16(base + 6, 0x8000);
        mem.write_u16(base + 8, 0x001B);
        cpu.iret(&mut mem).unwrap();
        assert_eq!(cpu.regs.cpl, 3);
        assert_eq!(cpu.regs.ds, 0);
        assert_eq!(cpu.regs.es, 0);
        assert_eq!(cpu.regs.fs, 0);
        assert_eq!(cpu.regs.gs, 0);
    }
    #[test]
    fn test_r5_translate_address_full_checked_gp_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.ds = 0x10;
        cpu.regs.ds_cache = SegmentCache {
            base: 0,
            limit: 0x0FFF,
            cached_effective_limit: 0x0FFF,
            access: 0x92,
            present: true,
            is_writable: true,
            ..Default::default()
        };
        cpu.regs.ss = 0x20;
        let err = cpu
            .translate_address_full_checked(&mut mem, 0x10, 0x2000, PageAccess::Read)
            .unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_r5_stack_range_ok_size_zero() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        assert!(cpu.stack_range_ok(0x1000, 0));
    }
    #[test]
    fn test_r5_stack_range_ok_overflow() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.is_code = false;
        cpu.regs.ss_cache.access = 0x92;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ss_cache.big = false;
        assert!(!cpu.stack_range_ok(0xFFFF_FFFF, 0xFFFF_FFFF));
    }
    #[test]
    fn test_r5_translate_address_full_checked_expand_down_oor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.ss = 0x10;
        cpu.regs.ss_cache = SegmentCache {
            base: 0,
            limit: 0x1000,
            cached_effective_limit: 0x1000,
            access: 0x96,
            present: true,
            is_writable: true,
            is_code: false,
            ..Default::default()
        };
        let err = cpu
            .translate_address_full_checked(&mut mem, 0x10, 0x0500, PageAccess::Read)
            .unwrap_err();
        assert!(matches!(err, CpuError::StackFault));
    }
    #[test]
    fn test_r5_page_walk_wp_supervisor_write_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8001_0001;
        cpu.regs.cpl = 0;
        let cr3: u32 = 0x100000;
        cpu.regs.cr3 = cr3;
        mem.write_u32(cr3 as usize, 0x101005);
        mem.write_u32(0x101000, 0x50005);
        let err = cpu
            .translate_linear_checked(&mut mem, 0x100, PageAccess::Write)
            .unwrap_err();
        assert!(matches!(err, CpuError::PageFault(_)));
    }
    #[test]
    fn test_r5_page_walk_user_not_allowed() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cpl = 3;
        let cr3: u32 = 0x100000;
        cpu.regs.cr3 = cr3;
        mem.write_u32(cr3 as usize, 0x101003);
        mem.write_u32(0x101000, 0x50003);
        let err = cpu
            .translate_linear_checked(&mut mem, 0x100, PageAccess::Read)
            .unwrap_err();
        assert!(matches!(err, CpuError::PageFault(_)));
    }
    #[test]
    fn test_r5_page_walk_pde_not_present() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x100000;
        mem.write_u32(0x100000, 0);
        let err = cpu
            .translate_linear_checked(&mut mem, 0x100, PageAccess::Read)
            .unwrap_err();
        assert!(matches!(err, CpuError::PageFault(_)));
    }
    #[test]
    fn test_r5_page_walk_pte_not_present() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x100000;
        mem.write_u32(0x100000, 0x101007);
        mem.write_u32(0x101000, 0);
        let err = cpu
            .translate_linear_checked(&mut mem, 0x100, PageAccess::Read)
            .unwrap_err();
        assert!(matches!(err, CpuError::PageFault(_)));
    }
    #[test]
    fn test_r5_page_walk_pde_bit7_accepted() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x100000;
        mem.write_u32(0x100000, 0x1010C7);
        mem.write_u32(0x101000, 0x50007);
        let result = cpu.translate_linear_checked(&mut mem, 0x100, PageAccess::Read);
        assert!(result.is_ok(), "386 ignores undefined PDE bits");
    }
    #[test]
    fn test_r5_page_walk_pde_avl_bits_accepted() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x100000;
        mem.write_u32(0x100000, 0x101E07);
        mem.write_u32(0x101000, 0x50007);
        let result = cpu.translate_linear_checked(&mut mem, 0x100, PageAccess::Read);
        assert!(result.is_ok());
    }
    #[test]
    fn test_r5_page_walk_pte_avl_bits_accepted() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x100000;
        mem.write_u32(0x100000, 0x101007);
        mem.write_u32(0x101000, 0x50E67);
        let result = cpu.translate_linear_checked(&mut mem, 0x100, PageAccess::Read);
        assert!(result.is_ok());
    }
    #[test]
    fn test_cov_push16_pop16_big_ss() {
        let mut cpu = Cpu::new();
        let mut mem = gp64_memory::Memory::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.ss_cache.present = true;
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.esp = 0x8000;
        cpu.regs.set_sp(0x8000);
        cpu.push16(&mut mem, 0xBEEF);
        assert_eq!(cpu.regs.esp, 0x7FFE);
        assert_eq!(mem.read_u16(0x7FFE), 0xBEEF);
        let val = cpu.pop16(&mut mem);
        assert_eq!(val, 0xBEEF);
        assert_eq!(cpu.regs.esp, 0x8000);
    }
    #[test]
    fn test_cov_push32_pop32_big_ss() {
        let mut cpu = Cpu::new();
        let mut mem = gp64_memory::Memory::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.ss_cache.present = true;
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.esp = 0x8000;
        cpu.regs.set_sp(0x8000);
        cpu.push32(&mut mem, 0xDEAD_BEEF);
        assert_eq!(cpu.regs.esp, 0x7FFC);
        assert_eq!(mem.read_u32(0x7FFC), 0xDEAD_BEEF);
        let val = cpu.pop32(&mut mem);
        assert_eq!(val, 0xDEAD_BEEF);
        assert_eq!(cpu.regs.esp, 0x8000);
    }
    #[test]
    fn test_cov_stack_range_ok_expand_down() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.is_code = false;
        cpu.regs.ss_cache.access = 0x06;
        cpu.regs.ss_cache.set_limit(0x7FFF);
        cpu.regs.ss_cache.big = false;
        cpu.regs.ss_cache.present = true;
        assert!(cpu.stack_range_ok(0x8000, 2));
        assert!(!cpu.stack_range_ok(0x7FFF, 2));
        assert!(!cpu.stack_range_ok(0, 2));
    }
    #[test]
    fn test_cov_try_pop16_big_ss_pm() {
        let mut cpu = Cpu::new();
        let mut mem = gp64_memory::Memory::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.ss_cache.present = true;
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.esp = 0x1000;
        cpu.regs.set_sp(0x1000);
        mem.write_u16(0x1000, 0xCAFE);
        let val = cpu.try_pop16(&mut mem).unwrap();
        assert_eq!(val, 0xCAFE);
        assert_eq!(cpu.regs.esp, 0x1002);
    }
    #[test]
    fn test_cov_try_pop32_big_ss_pm() {
        let mut cpu = Cpu::new();
        let mut mem = gp64_memory::Memory::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.ss_cache.present = true;
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.esp = 0x1000;
        cpu.regs.set_sp(0x1000);
        mem.write_u32(0x1000, 0xDEADBEEF);
        let val = cpu.try_pop32(&mut mem).unwrap();
        assert_eq!(val, 0xDEADBEEF);
        assert_eq!(cpu.regs.esp, 0x1004);
    }
    #[test]
    fn test_cov_push16_sp_ffff_wrap() {
        let mut cpu = Cpu::new();
        let mut mem = gp64_memory::Memory::new();
        cpu.regs.set_sp(0x0001);
        cpu.regs.esp = 0x0001;
        cpu.regs.ss = 0;
        cpu.push16(&mut mem, 0x1234);
        assert_eq!(cpu.regs.sp(), 0xFFFF);
        assert_eq!(mem.read_u8(0xFFFF), 0x34);
        assert_eq!(mem.read_u8(0x0000), 0x12);
    }
    #[test]
    fn test_cov_push16_pop16_pm_small_ss() {
        let mut cpu = Cpu::new();
        let mut mem = gp64_memory::Memory::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = false;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ss_cache.present = true;
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.set_sp(0x8000);
        cpu.regs.esp = 0x8000;
        cpu.push16(&mut mem, 0xAAAA);
        assert_eq!(cpu.regs.sp(), 0x7FFE);
        assert_eq!(mem.read_u16(0x7FFE), 0xAAAA);
        let val = cpu.pop16(&mut mem);
        assert_eq!(val, 0xAAAA);
        assert_eq!(cpu.regs.sp(), 0x8000);
    }
    #[test]
    fn test_cov_push32_pop32_pm_small_ss() {
        let mut cpu = Cpu::new();
        let mut mem = gp64_memory::Memory::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = false;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ss_cache.present = true;
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.set_sp(0x8000);
        cpu.regs.esp = 0x8000;
        cpu.push32(&mut mem, 0xFEEDFACE);
        assert_eq!(cpu.regs.sp(), 0x7FFC);
        assert_eq!(mem.read_u32(0x7FFC), 0xFEEDFACE);
        let val = cpu.pop32(&mut mem);
        assert_eq!(val, 0xFEEDFACE);
        assert_eq!(cpu.regs.sp(), 0x8000);
    }
    #[test]
    fn test_cov_try_push16_big_ss_pm() {
        let mut cpu = Cpu::new();
        let mut mem = gp64_memory::Memory::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.ss_cache.present = true;
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.esp = 0x8000;
        cpu.regs.set_sp(0x8000);
        cpu.try_push16(&mut mem, 0x5555).unwrap();
        assert_eq!(cpu.regs.esp, 0x7FFE);
        assert_eq!(mem.read_u16(0x7FFE), 0x5555);
    }
    #[test]
    fn test_cov_try_push32_big_ss_pm() {
        let mut cpu = Cpu::new();
        let mut mem = gp64_memory::Memory::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.ss_cache.present = true;
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.esp = 0x8000;
        cpu.regs.set_sp(0x8000);
        cpu.try_push32(&mut mem, 0x77777777).unwrap();
        assert_eq!(cpu.regs.esp, 0x7FFC);
        assert_eq!(mem.read_u32(0x7FFC), 0x77777777);
    }
    #[test]
    fn test_cov_try_pop16_pm_small_ss() {
        let mut cpu = Cpu::new();
        let mut mem = gp64_memory::Memory::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = false;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ss_cache.present = true;
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.set_sp(0x1000);
        cpu.regs.esp = 0xDEAD_1000;
        mem.write_u16(0x1000, 0xBBBB);
        let val = cpu.try_pop16(&mut mem).unwrap();
        assert_eq!(val, 0xBBBB);
        assert_eq!(cpu.regs.sp(), 0x1002);
        assert_eq!(cpu.regs.esp & 0xFFFF_0000, 0xDEAD_0000);
    }
    #[test]
    fn test_cov_try_pop32_pm_small_ss() {
        let mut cpu = Cpu::new();
        let mut mem = gp64_memory::Memory::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = false;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ss_cache.present = true;
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.set_sp(0x1000);
        cpu.regs.esp = 0xDEAD_1000;
        mem.write_u32(0x1000, 0xCCCCCCCC);
        let val = cpu.try_pop32(&mut mem).unwrap();
        assert_eq!(val, 0xCCCCCCCC);
        assert_eq!(cpu.regs.sp(), 0x1004);
        assert_eq!(cpu.regs.esp & 0xFFFF_0000, 0xDEAD_0000);
    }
    #[test]
    fn test_cov_ret_guard_should_log_disabled() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(false);
        let result = ret_guard_should_log();
        let _ = result;
    }
    #[test]
    fn test_cov_ret_guard_write_dump_disabled() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(false);
        ret_guard_write_dump("test dump disabled");
    }
    #[test]
    fn test_cov_ret_guard_push_disabled() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(false);
        let mut cpu = Cpu::new();
        cpu.ret_guard_push(crate::cpu::RetGuardEntry {
            kind: crate::cpu::RetGuardKind::Near,
            cs: 0,
            ip: 0,
            ss: 0,
            sp: 0,
            caller_cs: 0,
            caller_ip: 0,
            tag: 0,
        });
        assert!(cpu.ret_guard_stack.is_empty());
    }
    #[test]
    fn test_cov_ret_guard_peek_disabled() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(false);
        let cpu = Cpu::new();
        assert!(cpu.ret_guard_peek().is_none());
    }
    #[test]
    fn test_cov_stack_range_ok_expand_down_big() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.is_code = false;
        cpu.regs.ss_cache.access = 0xF6;
        cpu.regs.ss_cache.set_limit(0x0FFF);
        cpu.regs.ss_cache.big = true;
        assert!(cpu.stack_range_ok(0x2000, 4));
        assert!(!cpu.stack_range_ok(0x0100, 4));
    }
    #[test]
    fn test_cov_page_walk_write_protected_supervisor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8001_0001;
        cpu.regs.cpl = 0;
        cpu.regs.cr3 = 0x100000;
        mem.write_u32(0x100000, 0x101007);
        mem.write_u32(0x101000, 0x50005);
        let err = cpu
            .translate_linear_checked(&mut mem, 0x100, PageAccess::Write)
            .unwrap_err();
        if let CpuError::PageFault(info) = err {
            assert!(info.error_code & 0x2 != 0, "Should be a write fault");
        } else {
            panic!("Expected PageFault, got {:?}", err);
        }
    }
    #[test]
    fn test_cov_page_walk_user_access_denied() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cpl = 3;
        cpu.regs.cr3 = 0x100000;
        mem.write_u32(0x100000, 0x101003);
        mem.write_u32(0x101000, 0x50003);
        let err = cpu
            .translate_linear_checked(&mut mem, 0x100, PageAccess::Read)
            .unwrap_err();
        if let CpuError::PageFault(info) = err {
            assert!(info.error_code & 0x4 != 0, "Should be a user-mode fault");
        } else {
            panic!("Expected PageFault, got {:?}", err);
        }
    }
    #[test]
    fn test_cov_tlb_hit_write_protected() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8001_0001;
        cpu.regs.cpl = 0;
        cpu.regs.cr3 = 0x100000;
        mem.write_u32(0x100000, 0x101007);
        mem.write_u32(0x101000, 0x50005);
        let _ = cpu.translate_linear_checked(&mut mem, 0x100, PageAccess::Read);
        let err = cpu
            .translate_linear_checked(&mut mem, 0x100, PageAccess::Write)
            .unwrap_err();
        assert!(matches!(err, CpuError::PageFault(_)));
    }
    #[test]
    fn test_cov_load_data_seg_pm_nonconforming_code_dpl_violation() {
        let mut cpu = Cpu::new();
        let mut mem = gp64_memory::Memory::new();
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        cpu.regs.gdtr.base = 0x10000;
        cpu.regs.gdtr.limit = 0xFF;
        let off = 0x10000 + 2 * 8;
        mem.write_u8(off, 0xFF);
        mem.write_u8(off + 1, 0xFF);
        mem.write_u8(off + 2, 0);
        mem.write_u8(off + 3, 0);
        mem.write_u8(off + 4, 0);
        mem.write_u8(off + 5, 0x9A);
        mem.write_u8(off + 6, 0);
        mem.write_u8(off + 7, 0);
        let result = cpu.load_data_segment_checked(&mut mem, crate::DataSegReg::DS, 0x0013);
        assert!(
            matches!(result, Err(crate::CpuError::GeneralProtectionFault)),
            "Loading DPL=0 non-conforming code into DS at CPL=3 should GPF, got {:?}",
            result
        );
    }
    #[test]
    fn test_cov_try_pop16_pm_big_ss_esp_sync() {
        let mut cpu = Cpu::new();
        let mut mem = gp64_memory::Memory::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.ss_cache.present = true;
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.set_sp(0x1000);
        cpu.regs.esp = 0x1000;
        mem.write_u16(0x1000, 0xAAAA);
        let val = cpu.try_pop16(&mut mem).unwrap();
        assert_eq!(val, 0xAAAA);
        assert_eq!(cpu.regs.sp(), 0x1002);
    }
    #[test]
    fn test_cov_try_pop32_pm_big_ss_esp_only() {
        let mut cpu = Cpu::new();
        let mut mem = gp64_memory::Memory::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.ss_cache.present = true;
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.esp = 0x1000;
        cpu.regs.set_sp(0x1000);
        mem.write_u32(0x1000, 0xDEADBEEF);
        let val = cpu.try_pop32(&mut mem).unwrap();
        assert_eq!(val, 0xDEADBEEF);
        assert_eq!(cpu.regs.esp, 0x1004);
    }
    /// Helper: set up a CPU in protected mode with flat CS/DS/SS (base=0).
    fn flat_pm_cpu(limit: u32, granularity: bool, big: bool) -> Cpu {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 |= 1;
        let cached_effective_limit = if granularity {
            (limit << 12) | 0xFFF
        } else {
            limit
        };
        let cache = SegmentCache {
            base: 0,
            limit,
            cached_effective_limit,
            granularity,
            big,
            ..Default::default()
        };
        cpu.regs.cs_cache = cache;
        cpu.regs.ds_cache = cache;
        cpu.regs.ss_cache = cache;
        cpu.regs.es_cache = SegmentCache {
            base: 0x6120,
            big: false,
            ..cache
        };
        cpu
    }
    #[test]
    fn test_is_flat_model_256mb_limit() {
        let cpu = flat_pm_cpu(0x0FFFF, true, true);
        assert!(cpu.is_flat_model());
    }
    #[test]
    fn test_is_flat_model_4gb_limit() {
        let cpu = flat_pm_cpu(0xFFFFF, true, true);
        assert!(cpu.is_flat_model());
    }
    #[test]
    fn test_is_flat_model_64kb_limit() {
        let cpu = flat_pm_cpu(0xFFFF, false, false);
        assert!(cpu.is_flat_model());
    }
    #[test]
    fn test_is_flat_model_cs_base_nonzero() {
        let mut cpu = flat_pm_cpu(0xFFFFF, true, true);
        cpu.regs.cs_cache.base = 0x1000;
        assert!(!cpu.is_flat_model());
    }
    #[test]
    fn test_is_flat_model_ds_base_nonzero() {
        let mut cpu = flat_pm_cpu(0xFFFFF, true, true);
        cpu.regs.ds_cache.base = 0x1000;
        assert!(!cpu.is_flat_model());
    }
    #[test]
    fn test_is_flat_model_ss_base_nonzero() {
        let mut cpu = flat_pm_cpu(0xFFFFF, true, true);
        cpu.regs.ss_cache.base = 0x1000;
        assert!(!cpu.is_flat_model());
    }
    #[test]
    fn test_is_flat_model_real_mode() {
        let mut cpu = flat_pm_cpu(0xFFFFF, true, true);
        cpu.regs.cr0 &= !1;
        assert!(!cpu.is_flat_model());
    }
    #[test]
    fn test_is_flat_model_es_irrelevant() {
        let cpu = flat_pm_cpu(0xFFFFF, true, true);
        assert!(cpu.is_flat_model());
    }
    #[test]
    fn test_cov_read_descriptor_ldt_selector() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.ldt_base = 0x10000;
        cpu.regs.ldt_limit = 0xFFFF;
        let desc: [u8; 8] = [0xFF, 0xFF, 0x00, 0x50, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x10008, desc);
        let cache = cpu.read_descriptor(&mem, 0x0C);
        assert!(cache.is_some());
        let c = cache.unwrap();
        assert_eq!(c.base, 0x5000);
        assert!(c.present);
    }
    #[test]
    fn test_cov_read_descriptor_ldt_out_of_range() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mem = TestMemory::new(0x20000);
        cpu.regs.ldt_base = 0;
        cpu.regs.ldt_limit = 7;
        let cache = cpu.read_descriptor(&mem, 0x0C);
        assert!(cache.is_none());
    }
    #[test]
    fn test_cov_read_descriptor_ldt_null_selector_valid() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.ldt_base = 0x10000;
        cpu.regs.ldt_limit = 0xFFFF;
        let desc: [u8; 8] = [0xFF, 0xFF, 0x00, 0x20, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x10000, desc);
        let cache = cpu.read_descriptor(&mem, 0x04);
        assert!(cache.is_some());
    }
    #[test]
    fn test_cov_load_ss_null_selector_pm() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mut mem = TestMemory::new(0x20000);
        let result = cpu.load_ss(&mut mem, 0x0000);
        assert!(result.is_err());
    }
    #[test]
    fn test_cov_load_ss_code_segment_fault() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0xFFFF;
        let desc: [u8; 8] = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1008, desc);
        let result = cpu.load_ss(&mut mem, 0x08);
        assert!(result.is_err());
    }
    #[test]
    fn test_cov_load_ss_not_writable_fault() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0xFFFF;
        let desc: [u8; 8] = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x90, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1008, desc);
        let result = cpu.load_ss(&mut mem, 0x08);
        assert!(result.is_err());
    }
    #[test]
    fn test_cov_load_ss_dpl_mismatch() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0xFFFF;
        let desc: [u8; 8] = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xF2, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1008, desc);
        let result = cpu.load_ss(&mut mem, 0x08);
        assert!(result.is_err());
    }
    #[test]
    fn test_cov_load_ss_not_present_fault() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0xFFFF;
        let desc: [u8; 8] = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x12, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1008, desc);
        let result = cpu.load_ss(&mut mem, 0x08);
        assert!(result.is_err());
    }
    #[test]
    fn test_cov_load_ss_descriptor_not_found_no_fallback() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.allow_pm_real_mode_ss = false;
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 7;
        let result = cpu.load_ss(&mut mem, 0x08);
        assert!(result.is_err());
    }
    #[test]
    fn test_cov_load_ss_descriptor_not_found_with_fallback() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.allow_pm_real_mode_ss = true;
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 7;
        let result = cpu.load_ss(&mut mem, 0x08);
        assert!(result.is_ok());
        assert_eq!(cpu.regs.ss, 0x08);
        assert_eq!(cpu.regs.ss_cache.base, 0x08 * 16);
    }
    #[test]
    fn test_cov_load_ss_valid_descriptor() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0xFFFF;
        let desc: [u8; 8] = [0xFF, 0xFF, 0x00, 0x30, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1008, desc);
        let result = cpu.load_ss(&mut mem, 0x08);
        assert!(result.is_ok());
        assert_eq!(cpu.regs.ss, 0x08);
        assert_eq!(cpu.regs.ss_cache.base, 0x3000);
    }
    #[test]
    fn test_cov_load_data_seg_checked_null_selector_ds() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mut mem = TestMemory::new(0x20000);
        let result = cpu.load_data_segment_checked(&mut mem, DataSegReg::DS, 0x0000);
        assert!(result.is_ok());
        assert_eq!(cpu.regs.ds, 0x0000);
    }
    #[test]
    fn test_cov_load_data_seg_checked_null_selector_es() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mut mem = TestMemory::new(0x20000);
        let result = cpu.load_data_segment_checked(&mut mem, DataSegReg::ES, 0x0000);
        assert!(result.is_ok());
        assert_eq!(cpu.regs.es, 0x0000);
    }
    #[test]
    fn test_cov_load_data_seg_checked_null_selector_fs() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mut mem = TestMemory::new(0x20000);
        let result = cpu.load_data_segment_checked(&mut mem, DataSegReg::FS, 0x0000);
        assert!(result.is_ok());
        assert_eq!(cpu.regs.fs, 0x0000);
    }
    #[test]
    fn test_cov_load_data_seg_checked_null_selector_gs() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mut mem = TestMemory::new(0x20000);
        let result = cpu.load_data_segment_checked(&mut mem, DataSegReg::GS, 0x0000);
        assert!(result.is_ok());
        assert_eq!(cpu.regs.gs, 0x0000);
    }
    #[test]
    fn test_cov_load_data_seg_checked_not_present() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0xFFFF;
        let desc: [u8; 8] = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x12, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1008, desc);
        let result = cpu.load_data_segment_checked(&mut mem, DataSegReg::DS, 0x08);
        assert!(matches!(result, Err(CpuError::SegmentNotPresent(_))));
    }
    #[test]
    fn test_cov_load_data_seg_checked_system_descriptor() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0xFFFF;
        let desc: [u8; 8] = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x89, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1008, desc);
        let result = cpu.load_data_segment_checked(&mut mem, DataSegReg::DS, 0x08);
        assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
    }
    #[test]
    fn test_rejected_ss_load_does_not_set_descriptor_accessed_bit() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu.load_ss(&mut mem, 0x08).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
        assert_eq!(mem.read_u8(0x1000 + 8 + 5), 0x9A);
    }
    #[test]
    fn test_cov_load_data_seg_checked_non_readable_code() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0xFFFF;
        let desc: [u8; 8] = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x98, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1008, desc);
        let result = cpu.load_data_segment_checked(&mut mem, DataSegReg::DS, 0x08);
        assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
    }
    #[test]
    fn test_cov_load_data_seg_checked_conforming_code_dpl_above_cpl_ok() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0xFFFF;
        let desc: [u8; 8] = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFE, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1008, desc);
        let result = cpu.load_data_segment_checked(&mut mem, DataSegReg::DS, 0x08);
        assert!(result.is_ok());
        assert_eq!(cpu.regs.ds, 0x08);
        assert!(cpu.regs.ds_cache.is_conforming);
    }
    #[test]
    fn test_cov_load_data_seg_checked_data_dpl_violation() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0xFFFF;
        let desc: [u8; 8] = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1008, desc);
        let result = cpu.load_data_segment_checked(&mut mem, DataSegReg::DS, 0x08);
        assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
    }
    #[test]
    fn test_cov_load_data_seg_checked_real_mode() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        let result = cpu.load_data_segment_checked(&mut mem, DataSegReg::DS, 0x1234);
        assert!(result.is_ok());
        assert_eq!(cpu.regs.ds, 0x1234);
    }
    #[test]
    fn test_cov_load_data_segment_pm_null_ds() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mem = TestMemory::new(0x20000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0xFFFF;
        cpu.load_data_segment(&mem, DataSegReg::DS, 0x0000);
        assert_eq!(cpu.regs.ds, 0x0000);
        assert!(!cpu.regs.ds_cache.present);
    }
    #[test]
    fn test_cov_load_data_segment_pm_null_es() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mem = TestMemory::new(0x20000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0xFFFF;
        cpu.load_data_segment(&mem, DataSegReg::ES, 0x0000);
        assert_eq!(cpu.regs.es, 0x0000);
    }
    #[test]
    fn test_cov_load_data_segment_pm_null_fs() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mem = TestMemory::new(0x20000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0xFFFF;
        cpu.load_data_segment(&mem, DataSegReg::FS, 0x0000);
        assert_eq!(cpu.regs.fs, 0x0000);
    }
    #[test]
    fn test_cov_load_data_segment_pm_null_gs() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mem = TestMemory::new(0x20000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0xFFFF;
        cpu.load_data_segment(&mem, DataSegReg::GS, 0x0000);
        assert_eq!(cpu.regs.gs, 0x0000);
    }
    #[test]
    fn test_cov_load_data_segment_pm_fallback_ds() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mem = TestMemory::new(0x20000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 7;
        cpu.load_data_segment(&mem, DataSegReg::DS, 0x10);
        assert_eq!(cpu.regs.ds, 0x10);
        assert_eq!(cpu.regs.ds_cache.base, 0x10 * 16);
    }
    #[test]
    fn test_cov_load_data_segment_pm_fallback_es() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mem = TestMemory::new(0x20000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 7;
        cpu.load_data_segment(&mem, DataSegReg::ES, 0x10);
        assert_eq!(cpu.regs.es, 0x10);
        assert_eq!(cpu.regs.es_cache.base, 0x10 * 16);
    }
    #[test]
    fn test_cov_load_data_segment_pm_fallback_fs() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mem = TestMemory::new(0x20000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 7;
        cpu.load_data_segment(&mem, DataSegReg::FS, 0x10);
        assert_eq!(cpu.regs.fs, 0x10);
        assert_eq!(cpu.regs.fs_cache.base, 0x10 * 16);
    }
    #[test]
    fn test_cov_load_data_segment_pm_fallback_gs() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mem = TestMemory::new(0x20000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 7;
        cpu.load_data_segment(&mem, DataSegReg::GS, 0x10);
        assert_eq!(cpu.regs.gs, 0x10);
        assert_eq!(cpu.regs.gs_cache.base, 0x10 * 16);
    }
    #[test]
    fn test_cov_invalidate_segment_cache_cs() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0xFFFF;
        let desc: [u8; 8] = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x4F, 0x00];
        write_descriptor(&mut mem, 0x1008, desc);
        cpu.regs.cs = 0x08;
        cpu.cs_bits = 16;
        cpu.invalidate_segment_cache(&mem, 0x08);
        assert_eq!(cpu.cs_bits, 32);
    }
    #[test]
    fn test_cov_invalidate_segment_cache_fs_gs_ss() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0xFFFF;
        let desc: [u8; 8] = [0xFF, 0xFF, 0x00, 0x80, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1010, desc);
        cpu.regs.fs = 0x10;
        cpu.regs.gs = 0x10;
        cpu.regs.ss = 0x10;
        cpu.invalidate_segment_cache(&mem, 0x10);
        assert_eq!(cpu.regs.fs_cache.base, 0x8000);
        assert_eq!(cpu.regs.gs_cache.base, 0x8000);
        assert_eq!(cpu.regs.ss_cache.base, 0x8000);
    }
    #[test]
    fn test_cov_translate_checked_expand_down_big_ok() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.ds = 0x10;
        let mut cache = SegmentCache::real_mode(0x10);
        cache.present = true;
        cache.is_code = false;
        cache.access = 0x96;
        cache.set_limit(0x0FFF);
        cache.big = true;
        cache.base = 0;
        cpu.regs.ds_cache = cache;
        let result = cpu.translate_address_full_checked(&mut mem, 0x10, 0x1000, PageAccess::Read);
        assert!(result.is_ok());
    }
    #[test]
    fn test_cov_translate_checked_expand_down_big_fault() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.ds = 0x10;
        let mut cache = SegmentCache::real_mode(0x10);
        cache.present = true;
        cache.is_code = false;
        cache.access = 0x96;
        cache.set_limit(0x0FFF);
        cache.big = true;
        cache.base = 0;
        cpu.regs.ds_cache = cache;
        let result = cpu.translate_address_full_checked(&mut mem, 0x10, 0x0FFF, PageAccess::Read);
        assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
    }
    #[test]
    fn test_cov_translate_checked_expand_down_ss_fault() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.ss = 0x10;
        let mut cache = SegmentCache::real_mode(0x10);
        cache.present = true;
        cache.is_code = false;
        cache.access = 0x96;
        cache.set_limit(0x0FFF);
        cache.big = false;
        cache.base = 0;
        cpu.regs.ss_cache = cache;
        let result = cpu.translate_address_full_checked(&mut mem, 0x10, 0x0500, PageAccess::Read);
        assert!(matches!(result, Err(CpuError::StackFault)));
    }
    #[test]
    fn test_cov_translate_address_full_dpmi_mapping() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        let linear = 0x10 * 16 + 0x100;
        mem.map_linear(linear as u32, 0x50000);
        cpu.regs.ds = 0x10;
        let result = cpu.translate_address_full(&mut mem, 0x10, 0x100);
        assert_eq!(result, 0x50000);
    }
    #[test]
    fn test_cov_translate_linear_checked_dpmi_mapping() {
        let cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        mem.map_linear(0x12345, 0x67890);
        let result = cpu.translate_linear_checked(&mut mem, 0x12345, PageAccess::Read);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0x67890);
    }
    #[test]
    fn test_cov_translate_linear_checked_no_paging() {
        let cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        let result = cpu.translate_linear_checked(&mut mem, 0x12345, PageAccess::Read);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0x12345);
    }
    #[test]
    fn test_cov_stack_range_ok_size_zero() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        assert!(cpu.stack_range_ok(0, 0));
    }
    #[test]
    fn test_cov_stack_range_ok_overflow() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ss_cache.big = false;
        assert!(!cpu.stack_range_ok(0xFFFF_FFFF, 2));
    }
    #[test]
    fn test_cov_stack_range_ok_real_mode() {
        let cpu = Cpu::new();
        assert!(cpu.stack_range_ok(0, 100));
        assert!(!cpu.stack_range_ok(0xFFFF, 100));
        for cpu_type in [CpuType::I8086, CpuType::I80186] {
            let mut cpu = Cpu::new();
            cpu.cpu_type = cpu_type;
            assert!(cpu.stack_range_ok(0xFFFF, 100));
        }
    }
    #[test]
    fn test_cov_stack_range_ok_expand_down_end_exceeds_max() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.is_code = false;
        cpu.regs.ss_cache.access = 0x96;
        cpu.regs.ss_cache.set_limit(0x0FFF);
        cpu.regs.ss_cache.big = false;
        assert!(!cpu.stack_range_ok(0xFFF0, 0x20));
    }
    #[test]
    fn test_cov_is_flat_model_real_mode() {
        let cpu = Cpu::new();
        assert!(!cpu.is_flat_model());
    }
    #[test]
    fn test_cov_is_flat_model_pm_not_flat() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.cs_cache.base = 0x1000;
        assert!(!cpu.is_flat_model());
    }
    #[test]
    fn test_cov_is_flat_model_pm_flat_4gb() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.cs_cache.base = 0;
        cpu.regs.cs_cache.set_limit(0xFFFFF);
        cpu.regs.cs_cache.set_granularity(true);
        cpu.regs.ds_cache.base = 0;
        cpu.regs.ds_cache.set_limit(0xFFFFF);
        cpu.regs.ds_cache.set_granularity(true);
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFFF);
        cpu.regs.ss_cache.set_granularity(true);
        assert!(cpu.is_flat_model());
    }
    #[test]
    fn test_cov_is_flat_model_pm_flat_64k() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.cs_cache.base = 0;
        cpu.regs.cs_cache.set_limit(0xFFFF);
        cpu.regs.cs_cache.set_granularity(false);
        cpu.regs.ds_cache.base = 0;
        cpu.regs.ds_cache.set_limit(0xFFFF);
        cpu.regs.ds_cache.set_granularity(false);
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ss_cache.set_granularity(false);
        assert!(cpu.is_flat_model());
    }
    #[test]
    fn test_cov_is_flat_model_pm_ds_base_nonzero() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.cs_cache.base = 0;
        cpu.regs.ds_cache.base = 0x100;
        cpu.regs.ss_cache.base = 0;
        assert!(!cpu.is_flat_model());
    }
    #[test]
    fn test_cov_is_flat_model_pm_ss_base_nonzero() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.cs_cache.base = 0;
        cpu.regs.ds_cache.base = 0;
        cpu.regs.ss_cache.base = 0x200;
        assert!(!cpu.is_flat_model());
    }
    #[test]
    fn test_cov_is_flat_model_pm_cs_base_bad() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.cs_cache.base = 0x1000;
        cpu.regs.ds_cache.base = 0;
        cpu.regs.ss_cache.base = 0;
        assert!(!cpu.is_flat_model());
    }
    #[test]
    fn test_cov_is_flat_model_pm_ds_base_bad() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.cs_cache.base = 0;
        cpu.regs.ds_cache.base = 0x1000;
        cpu.regs.ss_cache.base = 0;
        assert!(!cpu.is_flat_model());
    }
    #[test]
    fn test_cov_is_flat_model_pm_ss_base_bad() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.cs_cache.base = 0;
        cpu.regs.ds_cache.base = 0;
        cpu.regs.ss_cache.base = 0x1000;
        assert!(!cpu.is_flat_model());
    }
    #[test]
    fn test_cov_is_flat_model_pm_limit_irrelevant() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.cs_cache.base = 0;
        cpu.regs.cs_cache.set_limit(0x1000);
        cpu.regs.ds_cache.base = 0;
        cpu.regs.ds_cache.set_limit(0x1000);
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0x1000);
        assert!(cpu.is_flat_model());
    }
    #[test]
    fn test_cov_paging_pde_not_present() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cpl = 0;
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr3 = 0x100000;
        mem.write_u32(0x100000, 0);
        let result = cpu.translate_paging_checked(&mut mem, 0x1000, PageAccess::Read);
        assert!(result.is_err());
    }
    #[test]
    fn test_cov_paging_pte_not_present() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cpl = 0;
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr3 = 0x100000;
        mem.write_u32(0x100000, 0x101003);
        mem.write_u32(0x101004, 0);
        let result = cpu.translate_paging_checked(&mut mem, 0x1000, PageAccess::Read);
        assert!(result.is_err());
    }
    #[test]
    fn test_cov_paging_write_protected() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 0x8001_0001;
        cpu.regs.cpl = 0;
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr3 = 0x100000;
        mem.write_u32(0x100000, 0x101001);
        mem.write_u32(0x101000, 0x102001);
        let result = cpu.translate_paging_checked(&mut mem, 0x0000, PageAccess::Write);
        assert!(result.is_err());
    }
    #[test]
    fn test_cov_paging_user_access_denied_page_walk() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cpl = 3;
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr3 = 0x100000;
        mem.write_u32(0x100000, 0x101001);
        mem.write_u32(0x101000, 0x102001);
        let result = cpu.translate_paging_checked(&mut mem, 0x0000, PageAccess::Read);
        assert!(result.is_err());
    }
    #[test]
    fn test_cov_paging_successful() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cpl = 0;
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr3 = 0x100000;
        mem.write_u32(0x100000, 0x101003);
        mem.write_u32(0x101000, 0x102003);
        let result = cpu.translate_paging_checked(&mut mem, 0x0123, PageAccess::Read);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0x102123);
    }
    #[test]
    fn test_cov_ret_guard_push_interrupt_pm_big() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(true);
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.big = true;
        cpu.regs.esp = 0x12345678;
        cpu.regs.set_sp(0x5678);
        cpu.regs.cs = 0x08;
        cpu.regs.set_ip(0x100);
        cpu.ret_guard_push_interrupt(0x21);
        let entry = cpu.ret_guard_peek();
        assert!(entry.is_some());
        let e = entry.unwrap();
        assert_eq!(e.sp, 0x12345678);
        assert_eq!(e.tag, 0x21);
        Cpu::set_ret_guard_for_tests(false);
    }
    #[test]
    fn test_cov_ret_guard_push_interrupt_small_stack() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(true);
        let mut cpu = Cpu::new();
        cpu.regs.esp = 0xFFFF1234;
        cpu.regs.set_sp(0x1234);
        cpu.regs.cs = 0x08;
        cpu.regs.set_ip(0x200);
        cpu.ret_guard_push_interrupt(0x10);
        let entry = cpu.ret_guard_peek();
        assert!(entry.is_some());
        let e = entry.unwrap();
        assert_eq!(e.sp, 0x1234);
        Cpu::set_ret_guard_for_tests(false);
    }
    #[test]
    fn test_cov_ret_guard_push_overflow() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(true);
        let mut cpu = Cpu::new();
        for i in 0..200 {
            cpu.ret_guard_push(RetGuardEntry {
                kind: RetGuardKind::Near,
                cs: 0x1000,
                ip: i,
                ss: 0x2000,
                sp: 0xFFF0,
                tag: 0,
                caller_cs: 0x1000,
                caller_ip: 0,
            });
        }
        Cpu::set_ret_guard_for_tests(false);
    }
    #[test]
    fn test_cov_ret_guard_pop_kind_mismatch() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(true);
        let mut cpu = Cpu::new();
        cpu.ret_guard_push(RetGuardEntry {
            kind: RetGuardKind::Near,
            cs: 0x1000,
            ip: 0x100,
            ss: 0x2000,
            sp: 0xFFF0,
            tag: 0,
            caller_cs: 0x1000,
            caller_ip: 0,
        });
        let result = cpu.ret_guard_pop(RetGuardKind::Far);
        assert!(result.is_none());
        Cpu::set_ret_guard_for_tests(false);
    }
    #[test]
    fn test_cov_tlb_write_fault_user() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cpl = 3;
        let mut mem = TestMemory::new(0x200000);
        let cr3: u32 = 0x100000;
        cpu.regs.cr3 = cr3;
        mem.write_u32(cr3 as usize, 0x101007);
        mem.write_u32(0x101000, 0x000005);
        mem.tlb.insert(0, (0, false, true, false));
        let err = cpu
            .translate_linear_checked(&mut mem, 0x100, PageAccess::Write)
            .unwrap_err();
        if let CpuError::PageFault(info) = err {
            assert!(info.error_code & 0x2 != 0, "Should be write fault");
        } else {
            panic!("Expected PageFault, got {:?}", err);
        }
    }
    #[test]
    fn test_cov_tlb_user_access_supervisor_page() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cpl = 3;
        let mut mem = TestMemory::new(0x200000);
        let cr3: u32 = 0x100000;
        cpu.regs.cr3 = cr3;
        mem.write_u32(cr3 as usize, 0x101003);
        mem.write_u32(0x101000, 0x000003);
        mem.tlb.insert(0, (0, true, false, false));
        let err = cpu
            .translate_linear_checked(&mut mem, 0x100, PageAccess::Read)
            .unwrap_err();
        if let CpuError::PageFault(info) = err {
            assert!(info.error_code & 0x4 != 0, "Should be user-mode fault");
        } else {
            panic!("Expected PageFault, got {:?}", err);
        }
    }
    #[test]
    fn test_cov_pagewalk_write_fault_user() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cpl = 3;
        let mut mem = TestMemory::new(0x200000);
        let cr3: u32 = 0x100000;
        cpu.regs.cr3 = cr3;
        mem.write_u32(cr3 as usize, 0x101005);
        mem.write_u32(0x101000, 0x000007);
        let err = cpu
            .translate_linear_checked(&mut mem, 0x100, PageAccess::Write)
            .unwrap_err();
        if let CpuError::PageFault(info) = err {
            assert!(info.error_code & 0x2 != 0, "Should be write fault");
        } else {
            panic!("Expected PageFault, got {:?}", err);
        }
    }
    #[test]
    fn test_cov_pagewalk_user_access_supervisor() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cpl = 3;
        let mut mem = TestMemory::new(0x200000);
        let cr3: u32 = 0x100000;
        cpu.regs.cr3 = cr3;
        mem.write_u32(cr3 as usize, 0x101007);
        mem.write_u32(0x101000, 0x000003);
        let err = cpu
            .translate_linear_checked(&mut mem, 0x100, PageAccess::Read)
            .unwrap_err();
        if let CpuError::PageFault(info) = err {
            assert!(info.error_code & 0x4 != 0, "Should be user-mode fault");
        } else {
            panic!("Expected PageFault, got {:?}", err);
        }
    }
    #[test]
    fn test_cov_stack_range_ok_expand_down_boundary() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.ss_cache.present = true;
        cpu.regs.ss_cache.is_code = false;
        cpu.regs.ss_cache.access = 0x06;
        cpu.regs.ss_cache.big = false;
        cpu.regs.ss_cache.set_limit(0x0FFF);
        assert!(cpu.stack_range_ok(0x1000, 4));
        assert!(!cpu.stack_range_ok(0x0FFF, 4));
        assert!(!cpu.stack_range_ok(0xFFFF, 4));
    }
    #[test]
    fn test_m10_iretd_with_nt_set_raises_gpf() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.flags.insert(crate::Flags::NESTED);
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.present = true;
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.esp = 0x1000;
        cpu.regs.set_sp(0x1000);
        mem.write_u32(0x1000, 0x0200);
        mem.write_u32(0x1004, 0x0008);
        mem.write_u32(0x1008, 0x0002);
        let err = cpu.execute_iretd(&mut mem).unwrap_err();
        assert!(
            matches!(err, CpuError::TaskSwitchNeeded),
            "Expected TaskSwitchNeeded, got {:?}",
            err
        );
        assert_eq!(cpu.regs.esp, 0x1000);
    }
    #[test]
    fn test_m10_iret_16bit_with_nt_set_raises_gpf() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.cr0 = 1;
        cpu.regs.flags.insert(crate::Flags::NESTED);
        cpu.regs.ss = 0x2000;
        cpu.regs.ss_cache.base = 0x20000;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ss_cache.present = true;
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.set_sp(0x0100);
        cpu.regs.esp = 0x0100;
        let base = 0x20000usize;
        mem.write_u16(base + 0x0100, 0x1234);
        mem.write_u16(base + 0x0102, 0x5678);
        mem.write_u16(base + 0x0104, 0x0202);
        let err = cpu.iret(&mut mem).unwrap_err();
        assert!(
            matches!(err, CpuError::TaskSwitchNeeded),
            "Expected TaskSwitchNeeded, got {:?}",
            err
        );
        assert_eq!(cpu.regs.sp(), 0x0100);
    }
    #[test]
    fn test_m10_iretd_with_nt_clear_succeeds() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.flags = crate::Flags::RESERVED1;
        cpu.regs.ss = 0x1000;
        cpu.regs.ss_cache = SegmentCache::real_mode(0x1000);
        cpu.regs.set_sp(0x0100);
        cpu.regs.esp = 0x0100;
        let base = 0x10000usize;
        mem.write_u32(base + 0x0100, 0x0200);
        mem.write_u32(base + 0x0104, 0x5678);
        mem.write_u32(base + 0x0108, 0x0002);
        cpu.execute_iretd(&mut mem).unwrap();
        assert_eq!(cpu.regs.eip, 0x0200);
        assert_eq!(cpu.regs.cs, 0x5678);
    }
    #[derive(Clone, Copy)]
    struct IretArchitecturalState {
        cs: u16,
        cs_cache: SegmentCache,
        cpl: u8,
        cs_bits: u32,
        eip: u32,
        flags: crate::Flags,
        eflags_high: u16,
        ss: u16,
        ss_cache: SegmentCache,
        esp: u32,
    }
    fn capture_iret_architectural_state(cpu: &Cpu) -> IretArchitecturalState {
        IretArchitecturalState {
            cs: cpu.regs.cs,
            cs_cache: cpu.regs.cs_cache,
            cpl: cpu.regs.cpl,
            cs_bits: cpu.cs_bits,
            eip: cpu.regs.eip,
            flags: cpu.regs.flags,
            eflags_high: cpu.regs.eflags_high,
            ss: cpu.regs.ss,
            ss_cache: cpu.regs.ss_cache,
            esp: cpu.regs.esp,
        }
    }
    fn assert_segment_cache_unchanged(actual: &SegmentCache, expected: &SegmentCache, name: &str) {
        assert_eq!(actual.base, expected.base, "{name} base changed");
        assert_eq!(actual.limit, expected.limit, "{name} limit changed");
        assert_eq!(
            actual.cached_effective_limit, expected.cached_effective_limit,
            "{name} effective limit changed"
        );
        assert_eq!(actual.access, expected.access, "{name} access changed");
        assert_eq!(actual.flags, expected.flags, "{name} flags changed");
        assert_eq!(actual.present, expected.present, "{name} present changed");
        assert_eq!(actual.dpl, expected.dpl, "{name} DPL changed");
        assert_eq!(actual.is_code, expected.is_code, "{name} type changed");
        assert_eq!(
            actual.is_conforming, expected.is_conforming,
            "{name} conforming bit changed"
        );
        assert_eq!(
            actual.is_readable, expected.is_readable,
            "{name} readable bit changed"
        );
        assert_eq!(
            actual.is_writable, expected.is_writable,
            "{name} writable bit changed"
        );
        assert_eq!(
            actual.granularity, expected.granularity,
            "{name} granularity changed"
        );
        assert_eq!(actual.big, expected.big, "{name} D/B bit changed");
    }
    fn assert_iret_architectural_state_unchanged(cpu: &Cpu, expected: IretArchitecturalState) {
        assert_eq!(cpu.regs.cs, expected.cs, "CS changed");
        assert_segment_cache_unchanged(&cpu.regs.cs_cache, &expected.cs_cache, "CS cache");
        assert_eq!(cpu.regs.cpl, expected.cpl, "CPL changed");
        assert_eq!(cpu.cs_bits, expected.cs_bits, "CS bitness changed");
        assert_eq!(cpu.regs.eip, expected.eip, "EIP changed");
        assert_eq!(cpu.regs.flags, expected.flags, "FLAGS changed");
        assert_eq!(
            cpu.regs.eflags_high, expected.eflags_high,
            "upper EFLAGS changed"
        );
        assert_eq!(cpu.regs.ss, expected.ss, "SS changed");
        assert_segment_cache_unchanged(&cpu.regs.ss_cache, &expected.ss_cache, "SS cache");
        assert_eq!(cpu.regs.esp, expected.esp, "ESP/SP changed");
    }
    fn protected_iret_limit_case(
        operand_size_32: bool,
        returning_outer: bool,
    ) -> (Cpu, TestMemory, IretArchitecturalState) {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x10000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x1F;
        let code_flags = if operand_size_32 { 0x40 } else { 0x00 };
        let current_cs_desc = [0xFF, 0xFF, 0, 0, 0, 0x9A, code_flags, 0];
        write_descriptor(&mut mem, 0x1000 + 8, current_cs_desc);
        cpu.regs.cs = 0x0008;
        cpu.regs.cs_cache = SegmentCache::from_descriptor(&current_cs_desc);
        cpu.cs_bits = if operand_size_32 { 32 } else { 16 };
        cpu.regs.eip = 0x0042;
        let destination_access = if returning_outer { 0xFA } else { 0x9A };
        let destination_cs_desc = [0xFF, 0x00, 0, 0, 0, destination_access, code_flags, 0];
        write_descriptor(&mut mem, 0x1000 + 16, destination_cs_desc);
        let stack_flags = if operand_size_32 { 0x40 } else { 0x00 };
        let current_ss_desc = [0xFF, 0xFF, 0, 0, 0, 0x92, stack_flags, 0];
        cpu.regs.ss = 0x0020;
        cpu.regs.ss_cache = SegmentCache::from_descriptor(&current_ss_desc);
        cpu.regs.esp = 0x2000;
        cpu.regs.flags = crate::Flags::RESERVED1 | crate::Flags::CARRY;
        cpu.regs.eflags_high = 0x0020;
        let destination_cs = if returning_outer { 0x0013 } else { 0x0010 };
        if returning_outer {
            let destination_ss_desc = [0xFF, 0xFF, 0, 0, 0, 0xF2, stack_flags, 0];
            write_descriptor(&mut mem, 0x1000 + 24, destination_ss_desc);
        }
        if operand_size_32 {
            mem.write_u32(0x2000, 0x0000_0100);
            mem.write_u32(0x2004, destination_cs as u32);
            mem.write_u32(0x2008, 0x0000_0202);
            if returning_outer {
                mem.write_u32(0x200C, 0x0000_3000);
                mem.write_u32(0x2010, 0x0000_001B);
            }
        } else {
            mem.write_u16(0x2000, 0x0100);
            mem.write_u16(0x2002, destination_cs);
            mem.write_u16(0x2004, 0x0202);
            if returning_outer {
                mem.write_u16(0x2006, 0x3000);
                mem.write_u16(0x2008, 0x001B);
            }
        }
        let before = capture_iret_architectural_state(&cpu);
        (cpu, mem, before)
    }
    /// Real mode preserves the hidden CS limit across a protected-to-real
    /// transition, so it can sit below 0xFFFF. Build that state: a real-mode
    /// CPU whose CS cache retains a small limit, with a 16-bit IRET frame
    /// returning to `target_ip`.
    fn real_mode_iret_retained_limit_case(
        retained_limit: u32,
        target_ip: u16,
    ) -> (Cpu, TestMemory, IretArchitecturalState) {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x10000);
        cpu.regs.cr0 = 0;
        cpu.regs.cpl = 0;
        cpu.cs_bits = 16;
        cpu.regs.cs = 0x0000;
        cpu.regs.cs_cache = SegmentCache::real_mode(0x0000);
        cpu.regs.cs_cache.set_limit(retained_limit);
        cpu.regs.eip = 0x0042;
        cpu.regs.ss = 0x0000;
        cpu.regs.ss_cache = SegmentCache::real_mode(0x0000);
        cpu.regs.esp = 0x2000;
        cpu.regs.set_sp(0x2000);
        cpu.regs.flags = crate::Flags::RESERVED1 | crate::Flags::CARRY;
        mem.write_u16(0x2000, target_ip);
        mem.write_u16(0x2002, 0x0000);
        mem.write_u16(0x2004, 0x0202);
        let before = capture_iret_architectural_state(&cpu);
        (cpu, mem, before)
    }
    /// a 16-bit IRET whose popped IP exceeds the retained
    /// real-mode CS limit must #GP(0) without committing the frame. The IRETD
    /// path already did this; the 16-bit path did not.
    #[test]
    fn real_mode_iret_rejects_ip_past_the_retained_cs_limit_kiln284() {
        let (mut cpu, mut mem, before) = real_mode_iret_retained_limit_case(0x0FFF, 0x1000);
        let err = cpu.iret(&mut mem).unwrap_err();
        assert!(
            matches!(err, CpuError::GeneralProtectionFault),
            "expected #GP, got {err:?}"
        );
        assert_eq!(cpu.last_exception_error_code, 0, "limit fault is #GP(0)");
        assert_iret_architectural_state_unchanged(&cpu, before);
        assert_eq!(mem.read_u16(0x2000), 0x1000, "frame contents changed");
    }
    /// The boundary from the other side: an IP exactly AT the retained limit is
    /// within it and must still return, so the guard cannot be rejecting
    /// everything.
    #[test]
    fn real_mode_iret_accepts_ip_at_the_retained_cs_limit_kiln284() {
        let (mut cpu, mut mem, _) = real_mode_iret_retained_limit_case(0x0FFF, 0x0FFF);
        cpu.iret(&mut mem).expect("IP at the limit is within it");
        assert_eq!(cpu.regs.ip(), 0x0FFF, "the frame must have committed");
    }
    /// An ordinary real-mode CS keeps the full 0xFFFF limit, so a normal IRET
    /// anywhere in the segment is unaffected by the new check.
    #[test]
    fn real_mode_iret_unaffected_with_a_full_segment_limit_kiln284() {
        let (mut cpu, mut mem, _) = real_mode_iret_retained_limit_case(0xFFFF, 0xFFFF);
        cpu.iret(&mut mem)
            .expect("a full real-mode segment permits any IP");
        assert_eq!(cpu.regs.ip(), 0xFFFF);
    }
    #[test]
    fn iret_same_ring_rejects_target_beyond_cs_limit() {
        let (mut cpu, mut mem, before) = protected_iret_limit_case(false, false);
        let err = cpu.iret(&mut mem).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
        assert_eq!(cpu.last_exception_error_code, 0, "limit fault is #GP(0)");
        assert_iret_architectural_state_unchanged(&cpu, before);
        assert_eq!(mem.read_u16(0x2000), 0x0100, "frame contents changed");
    }
    #[test]
    fn iret_outer_ring_rejects_target_beyond_cs_limit() {
        let (mut cpu, mut mem, before) = protected_iret_limit_case(false, true);
        let err = cpu.iret(&mut mem).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
        assert_eq!(cpu.last_exception_error_code, 0, "limit fault is #GP(0)");
        assert_iret_architectural_state_unchanged(&cpu, before);
        assert_eq!(mem.read_u16(0x2000), 0x0100, "frame contents changed");
    }
    #[test]
    fn iretd_same_ring_rejects_target_beyond_cs_limit() {
        let (mut cpu, mut mem, before) = protected_iret_limit_case(true, false);
        let err = cpu.execute_iretd(&mut mem).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
        assert_eq!(cpu.last_exception_error_code, 0, "limit fault is #GP(0)");
        assert_iret_architectural_state_unchanged(&cpu, before);
        assert_eq!(mem.read_u32(0x2000), 0x0000_0100, "frame contents changed");
    }
    #[test]
    fn iretd_outer_ring_rejects_target_beyond_cs_limit() {
        let (mut cpu, mut mem, before) = protected_iret_limit_case(true, true);
        let err = cpu.execute_iretd(&mut mem).unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
        assert_eq!(cpu.last_exception_error_code, 0, "limit fault is #GP(0)");
        assert_iret_architectural_state_unchanged(&cpu, before);
        assert_eq!(mem.read_u32(0x2000), 0x0000_0100, "frame contents changed");
    }
    #[test]
    fn test_iret_base_frame_pop_fault_restores_stack() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x1000);
        cpu.regs.ss_cache = SegmentCache::real_mode(0);
        cpu.regs.ss_cache.set_limit(0x0103);
        cpu.regs.esp = 0xABCD_0100;
        mem.write_u16(0x0100, 0x0200);
        mem.write_u16(0x0102, 0x1234);
        let err = cpu.iret(&mut mem).unwrap_err();
        assert!(matches!(err, CpuError::StackFault));
        assert_eq!(cpu.regs.esp, 0xABCD_0100);
    }
    #[test]
    fn test_iret_outer_frame_pop_fault_restores_stack() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x1000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.ss_cache.set_limit(0x0107);
        cpu.regs.ss_cache.present = true;
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.esp = 0xABCD_0100;
        mem.write_u16(0x0100, 0x0200);
        mem.write_u16(0x0102, 0x0003);
        mem.write_u16(0x0104, 0x0002);
        mem.write_u16(0x0106, 0x0300);
        let err = cpu.iret(&mut mem).unwrap_err();
        assert!(matches!(err, CpuError::StackFault));
        assert_eq!(cpu.regs.esp, 0xABCD_0100);
    }
    #[test]
    fn test_iretd_base_frame_pop_fault_restores_stack() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x1000);
        cpu.regs.ss_cache = SegmentCache::real_mode(0);
        cpu.regs.ss_cache.set_limit(0x0107);
        cpu.regs.esp = 0xABCD_0100;
        mem.write_u32(0x0100, 0x0200);
        mem.write_u32(0x0104, 0x1234);
        let err = cpu.execute_iretd(&mut mem).unwrap_err();
        assert!(matches!(err, CpuError::StackFault));
        assert_eq!(cpu.regs.esp, 0xABCD_0100);
    }
    #[test]
    fn test_iretd_v86_frame_pop_fault_restores_stack() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x1000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.set_limit(0x011F);
        cpu.regs.ss_cache.present = true;
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.esp = 0x0100;
        mem.write_u32(0x0100, 0x0200);
        mem.write_u32(0x0104, 0x1234);
        mem.write_u32(0x0108, 1 << 17);
        mem.write_u32(0x010C, 0x0300);
        mem.write_u32(0x0110, 0x2000);
        mem.write_u32(0x0114, 0x3000);
        mem.write_u32(0x0118, 0x4000);
        mem.write_u32(0x011C, 0x5000);
        let err = cpu.execute_iretd(&mut mem).unwrap_err();
        assert!(matches!(err, CpuError::StackFault));
        assert_eq!(cpu.regs.esp, 0x0100);
    }
    #[test]
    fn test_iretd_outer_frame_pop_fault_restores_stack() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x1000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.set_limit(0x010F);
        cpu.regs.ss_cache.present = true;
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.esp = 0x0100;
        mem.write_u32(0x0100, 0x0200);
        mem.write_u32(0x0104, 0x0003);
        mem.write_u32(0x0108, 0x0002);
        mem.write_u32(0x010C, 0x0300);
        let err = cpu.execute_iretd(&mut mem).unwrap_err();
        assert!(matches!(err, CpuError::StackFault));
        assert_eq!(cpu.regs.esp, 0x0100);
    }
    #[test]
    fn test_iretd_privilege_change_invalid_ss_restores_cs_cpl() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x1FFF;
        let orig_cs: u16 = 0x08;
        let cs0_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, cs0_desc);
        cpu.regs.cs = orig_cs;
        cpu.regs.cs_cache = SegmentCache::from_descriptor(&cs0_desc);
        let orig_cs_cache = cpu.regs.cs_cache;
        let orig_cpl = cpu.regs.cpl;
        let orig_cs_bits = cpu.cs_bits;
        let cs3_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFA, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 16, cs3_desc);
        cpu.regs.ss = 0x08;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.ss_cache.present = true;
        cpu.regs.esp = 0x2000;
        cpu.regs.set_sp(0x2000);
        let orig_esp = cpu.regs.esp;
        let orig_sp = cpu.regs.sp();
        mem.write_u32(0x2000, 0x0400);
        mem.write_u32(0x2004, 0x0013);
        mem.write_u32(0x2008, 0x0002);
        mem.write_u32(0x200C, 0x8000);
        mem.write_u32(0x2010, 0x001B);
        let result = cpu.execute_iretd(&mut mem);
        assert!(result.is_err(), "Expected error from invalid SS");
        assert_eq!(cpu.last_exception_error_code, 0x0018);
        assert_eq!(
            cpu.regs.cs, orig_cs,
            "CS should be restored after failed IRETD"
        );
        assert_eq!(
            cpu.regs.cpl, orig_cpl,
            "CPL should be restored after failed IRETD"
        );
        assert_eq!(
            cpu.cs_bits, orig_cs_bits,
            "cs_bits should be restored after failed IRETD"
        );
        assert_eq!(
            cpu.regs.cs_cache.base, orig_cs_cache.base,
            "cs_cache should be restored after failed IRETD"
        );
        assert_eq!(
            cpu.regs.esp, orig_esp,
            "ESP should be restored after failed IRETD"
        );
        assert_eq!(
            cpu.regs.sp(),
            orig_sp,
            "SP should be restored after failed IRETD"
        );
    }
    #[test]
    fn test_r6_eflags_high_mask_386() {
        let mut cpu = Cpu::new();
        cpu.set_cpu_type(CpuType::I80386);
        let mask = cpu.eflags_high_mask();
        assert_eq!(mask & 0x0004, 0, "AC bit should be masked on 386");
        assert_eq!(mask & 0x0020, 0, "ID bit should be masked on 386");
    }
    #[test]
    fn test_r6_eflags_high_mask_486() {
        let mut cpu = Cpu::new();
        cpu.set_cpu_type(CpuType::I80486);
        let mask = cpu.eflags_high_mask();
        assert_ne!(mask & 0x0004, 0, "AC bit should be allowed on 486");
        assert_eq!(mask & 0x0020, 0, "ID bit should be masked on 486");
    }
    #[test]
    fn test_r6_eflags_high_mask_pentium() {
        let mut cpu = Cpu::new();
        cpu.set_cpu_type(CpuType::Pentium);
        let mask = cpu.eflags_high_mask();
        assert_eq!(mask, 0x003F);
    }
    #[test]
    fn test_r6_apply_popf_real_mode_no_masking() {
        let cpu = Cpu::new();
        let flags = crate::Flags::RESERVED1 | crate::Flags::INTERRUPT;
        let result = cpu.apply_popf_privilege(flags);
        assert_eq!(result, flags);
    }
    #[test]
    fn test_r6_apply_popf_pm_cpl_gt_iopl_if_preserved() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        cpu.regs.flags = crate::Flags::RESERVED1 | crate::Flags::INTERRUPT;
        let new_flags = crate::Flags::RESERVED1;
        let result = cpu.apply_popf_privilege(new_flags);
        assert!(
            result.contains(crate::Flags::INTERRUPT),
            "IF should be preserved from current flags when CPL > IOPL"
        );
    }
    #[test]
    fn test_r6_apply_popf_pm_cpl_gt_iopl_if_clear_preserved() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        cpu.regs.flags = crate::Flags::RESERVED1;
        let new_flags = crate::Flags::RESERVED1 | crate::Flags::INTERRUPT;
        let result = cpu.apply_popf_privilege(new_flags);
        assert!(
            !result.contains(crate::Flags::INTERRUPT),
            "IF should be preserved as clear when CPL > IOPL"
        );
    }
    #[test]
    fn test_r6_apply_popf_pm_cpl_ne_0_iopl_preserved_nt_loaded() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 1;
        cpu.regs.flags = crate::Flags::RESERVED1;
        let new_flags = crate::Flags::from_bits_truncate(0x7002) | crate::Flags::RESERVED1;
        let result = cpu.apply_popf_privilege(new_flags);
        let iopl = (result.bits() >> 12) & 3;
        assert_eq!(iopl, 0, "IOPL should be preserved from current flags");
        assert!(
            result.contains(crate::Flags::NESTED),
            "NT is not IOPL-gated and must load at CPL != 0"
        );
    }
    /// POPF/POPFD at ring 3 must be able to both SET and CLEAR NT: the flag
    /// carries no privilege restriction on a 386, unlike IF and IOPL
    #[test]
    fn test_apply_popf_cpl3_controls_nt_in_both_directions_kiln282() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        cpu.regs.set_iopl(3);
        cpu.regs.flags = crate::Flags::RESERVED1;
        let set = cpu.apply_popf_privilege(crate::Flags::RESERVED1 | crate::Flags::NESTED);
        assert!(
            set.contains(crate::Flags::NESTED),
            "ring 3 POPF must set NT"
        );
        cpu.regs.flags = crate::Flags::RESERVED1 | crate::Flags::NESTED;
        let cleared = cpu.apply_popf_privilege(crate::Flags::RESERVED1);
        assert!(
            !cleared.contains(crate::Flags::NESTED),
            "ring 3 POPF must clear NT"
        );
    }
    /// A same-ring IRET frame likewise loads NT at a nonzero CPL, and IF/IOPL
    /// stay gated.
    #[test]
    fn test_apply_iret_privilege_same_ring_loads_nt_kiln282() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        cpu.regs.flags = crate::Flags::RESERVED1;
        let popped = crate::Flags::from_bits_truncate(0x7002) | crate::Flags::NESTED;
        let result = cpu.apply_iret_privilege(popped, 3, 0, crate::Flags::RESERVED1);
        assert!(
            result.contains(crate::Flags::NESTED),
            "same-ring IRET must load NT at CPL != 0"
        );
        assert_eq!(
            (result.bits() >> 12) & 3,
            0,
            "IOPL is still privilege-gated at CPL != 0"
        );
    }
    /// The point of loading NT at ring 3: the NEXT protected-mode IRETD must
    /// take the nested-task return, and it must do so before mutating the
    /// stack.
    #[test]
    fn test_ring3_popf_nt_makes_the_next_iretd_switch_tasks_kiln282() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        cpu.regs.set_iopl(3);
        cpu.regs.flags = crate::Flags::RESERVED1;
        cpu.regs.esp = 0x2000;
        cpu.regs.flags = cpu.apply_popf_privilege(crate::Flags::RESERVED1 | crate::Flags::NESTED);
        assert!(cpu.regs.flags.contains(crate::Flags::NESTED));
        let err = cpu.execute_iretd(&mut mem).unwrap_err();
        assert!(
            matches!(err, crate::CpuError::TaskSwitchNeeded),
            "expected TaskSwitchNeeded, got {err:?}"
        );
        assert_eq!(
            cpu.regs.esp, 0x2000,
            "the nested-task return must be taken before any stack pop"
        );
    }
    #[test]
    fn test_r6_apply_popf_pm_cpl0_allows_all() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.flags = crate::Flags::RESERVED1;
        let new_flags = crate::Flags::from_bits_truncate(0x7202) | crate::Flags::NESTED;
        let result = cpu.apply_popf_privilege(new_flags);
        assert!(result.contains(crate::Flags::INTERRUPT));
        assert!(result.contains(crate::Flags::NESTED));
    }
    #[test]
    fn test_r6_set_cs_for_hle_pm_with_descriptor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x02, 0x9A, 0x40, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.set_cs_for_hle(&mem, 0x08);
        assert_eq!(cpu.regs.cs, 0x08);
        assert_eq!(cpu.regs.cs_cache.base, 0x00020000);
        assert_eq!(cpu.cs_bits, 32);
    }
    #[test]
    fn test_r6_set_cs_for_hle_pm_no_descriptor() {
        let mut cpu = Cpu::new();
        let mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0000;
        cpu.regs.cs = 0x1234;
        cpu.regs.cs_cache = SegmentCache {
            base: 0xDEAD_BEEF,
            limit: 0x000F_FFFF,
            cached_effective_limit: 0x000F_FFFF,
            access: 0x93,
            flags: 0x0C,
            present: true,
            dpl: 0,
            is_code: false,
            is_conforming: false,
            is_readable: true,
            is_writable: true,
            granularity: true,
            big: true,
        };
        cpu.cs_bits = 32;
        cpu.set_cs_for_hle(&mem, 0x08);
        assert_eq!(cpu.regs.cs, 0x08);
        assert_eq!(cpu.regs.cs_cache.base, 0x80);
        assert_eq!(cpu.regs.cs_cache.limit, 0xFFFF);
        assert_eq!(cpu.regs.cs_cache.cached_effective_limit, 0xFFFF);
        assert_eq!(cpu.regs.cs_cache.access, 0x9B);
        assert_eq!(cpu.regs.cs_cache.flags, 0);
        assert!(cpu.regs.cs_cache.present);
        assert_eq!(cpu.regs.cs_cache.dpl, 0);
        assert!(cpu.regs.cs_cache.is_code);
        assert!(!cpu.regs.cs_cache.is_conforming);
        assert!(cpu.regs.cs_cache.is_readable);
        assert!(!cpu.regs.cs_cache.is_writable);
        assert!(!cpu.regs.cs_cache.granularity);
        assert!(!cpu.regs.cs_cache.big);
        assert_eq!(cpu.cs_bits, 16);
    }
    #[test]
    fn test_r6_set_cs_for_hle_real_mode() {
        let mut cpu = Cpu::new();
        let mem = TestMemory::new(0x20000);
        cpu.set_cs_for_hle(&mem, 0x1234);
        assert_eq!(cpu.regs.cs, 0x1234);
        assert_eq!(cpu.regs.cs_cache.base, 0x12340);
        assert_eq!(cpu.cs_bits, 16);
    }
    #[test]
    fn test_r6_return_data_seg_for_hle_all_data_segments() {
        let mut cpu = Cpu::new();
        let mem = TestMemory::new(0x20000);
        cpu.regs.ds_cache = SegmentCache::null_flat_usable();
        let old_ds_cache = cpu.regs.ds_cache;
        cpu.return_data_seg_for_hle(Some(&mem), DataSegReg::DS, 0x1000, false);
        assert_eq!(cpu.regs.ds, 0x1000);
        let mut expected_ds_cache = old_ds_cache;
        expected_ds_cache.base = 0x10000;
        assert_eq!(cpu.regs.ds_cache, expected_ds_cache);
        cpu.return_data_seg_for_hle(Some(&mem), DataSegReg::ES, 0x2000, false);
        assert_eq!(cpu.regs.es, 0x2000);
        assert_eq!(cpu.regs.es_cache.base, 0x20000);
        cpu.return_data_seg_for_hle(Some(&mem), DataSegReg::FS, 0x4000, false);
        assert_eq!(cpu.regs.fs, 0x4000);
        assert_eq!(cpu.regs.fs_cache.base, 0x40000);
        cpu.return_data_seg_for_hle(Some(&mem), DataSegReg::GS, 0x5000, false);
        assert_eq!(cpu.regs.gs, 0x5000);
        assert_eq!(cpu.regs.gs_cache.base, 0x50000);
    }
    #[test]
    fn test_r6_set_seg_for_hle_compatibility_wrapper() {
        let mut cpu = Cpu::new();
        let mem = TestMemory::new(0x20000);
        cpu.regs.ss_cache = SegmentCache::null_flat_usable();
        let old_ss_cache = cpu.regs.ss_cache;
        cpu.set_seg_for_hle(&mem, iced_x86::Register::SS, 0x3000);
        let mut expected_ss_cache = old_ss_cache;
        expected_ss_cache.base = 0x30000;
        assert_eq!(cpu.regs.ss, 0x3000);
        assert_eq!(cpu.regs.ss_cache, expected_ss_cache);
        let old_ds = cpu.regs.ds;
        cpu.set_seg_for_hle(&mem, iced_x86::Register::EAX, 0x1234);
        assert_eq!(cpu.regs.ds, old_ds);
    }
    #[test]
    fn test_r6_return_data_seg_for_hle_pm_with_descriptor() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x03, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.return_data_seg_for_hle(Some(&mem), DataSegReg::DS, 0x08, true);
        assert_eq!(cpu.regs.ds, 0x08);
        assert_eq!(cpu.regs.ds_cache.base, 0x00030000);
    }
    #[test]
    fn test_r6_set_hle_break_selectors() {
        let mut cpu = Cpu::new();
        let selectors = [0x0008u16, 0x0010, 0x0018];
        cpu.set_hle_break_selectors(&selectors);
        assert_eq!(cpu.hle_break_count, 3);
        assert_eq!(cpu.hle_break_selectors[0], 0x0008);
        assert_eq!(cpu.hle_break_selectors[1], 0x0010);
        assert_eq!(cpu.hle_break_selectors[2], 0x0018);
    }
    #[test]
    fn test_r6_set_hle_break_selectors_truncates_at_max() {
        let mut cpu = Cpu::new();
        let selectors: Vec<u16> = (0..64).map(|i| (i * 8) as u16).collect();
        cpu.set_hle_break_selectors(&selectors);
        assert_eq!(cpu.hle_break_count, 32);
    }
    /// `set_hle_break_selectors` truncates and `add_hle_break_selector` warns
    /// and drops — both silent to the caller. A caller that can still fail
    /// cleanly needs a form that reports the overflow.
    #[test]
    fn try_add_hle_break_selector_reports_overflow_instead_of_dropping() {
        let mut cpu = Cpu::new();
        let full: Vec<u16> = (1..=32u16).map(|i| i * 8).collect();
        cpu.set_hle_break_selectors(&full);
        assert_eq!(cpu.hle_break_capacity_remaining(), 0);
        assert!(
            !cpu.try_add_hle_break_selector(0x0FF8),
            "an overflowing add must report failure, not warn and drop"
        );
        assert_eq!(cpu.hle_break_count, 32, "and must not corrupt the count");
        assert!(
            !cpu.hle_break_selectors.contains(&0x0FF8),
            "and must not have registered it"
        );
        assert!(cpu.try_add_hle_break_selector(8));
    }
    /// Teardown must be able to give capacity back, or a retained break
    /// selector can later match a recycled LDT selector (HLD D3b).
    #[test]
    fn remove_hle_break_selector_frees_capacity_and_keeps_the_rest() {
        let mut cpu = Cpu::new();
        cpu.set_hle_break_selectors(&[0x0008, 0x0010, 0x0018, 0x0020]);
        assert!(cpu.remove_hle_break_selector(0x0010));
        assert_eq!(cpu.hle_break_count, 3);
        assert_eq!(
            &cpu.hle_break_selectors[..3],
            &[0x0008, 0x0018, 0x0020],
            "the survivors must keep their order and values"
        );
        assert_eq!(cpu.hle_break_capacity_remaining(), 29);
        assert!(
            !cpu.remove_hle_break_selector(0x0010),
            "removing an absent selector must report false"
        );
    }
    #[test]
    fn test_restore_state_clamps_malformed_hle_break_count() {
        let mut cpu = Cpu::new();
        let mut snap = cpu.snapshot_state();
        snap.hle_break_count = 33;
        cpu.restore_state(&snap).unwrap();
        assert_eq!(
            cpu.hle_break_count, 32,
            "restore must clamp hle_break_count to the selector array length"
        );
        cpu.add_hle_break_selector(0x1234);
        assert_eq!(
            cpu.hle_break_count, 32,
            "array was already full; count stays clamped and no OOB occurs"
        );
    }
    #[test]
    fn cpu_snapshot_validation_error_display_contract() {
        let cases = [
            (
                CpuSnapshotValidationError::CplOutOfRange { cpl: 4 },
                "invalid CPU snapshot CPL 4: expected 0..=3",
            ),
            (
                CpuSnapshotValidationError::V86CplMismatch { cpl: 0 },
                "invalid CPU snapshot virtual-8086 state: VM=1 requires CPL=3, got 0",
            ),
            (
                CpuSnapshotValidationError::V86WithoutProtectedMode,
                "invalid CPU snapshot virtual-8086 state: VM=1 requires CR0.PE=1",
            ),
            (
                CpuSnapshotValidationError::CsRplCplMismatch { cs: 0x1237, cpl: 2 },
                "invalid CPU snapshot protected-mode CS 0x1237: selector RPL 3 contradicts CPL 2",
            ),
            (
                CpuSnapshotValidationError::IncoherentSegmentCache { segment: "DS" },
                "invalid CPU snapshot DS descriptor cache: derived fields disagree          with the canonical access/flags/limit bytes",
            ),
            (
                CpuSnapshotValidationError::InvalidCsBits { cs_bits: 64 },
                "invalid CPU snapshot cs_bits 64: expected 16 or 32",
            ),
            (
                CpuSnapshotValidationError::InvalidCr4Bits {
                    value: 0xABCD,
                    invalid: 0x1230,
                    cpu_type: crate::CpuType::I80386,
                },
                "invalid CPU snapshot CR4 0xabcd: unsupported bits 0x1230 for I80386",
            ),
            (
                CpuSnapshotValidationError::InvalidMemoryTypeMsr { index: 0x1ABCD },
                "invalid CPU snapshot value for MSR 0x1abcd",
            ),
            (
                CpuSnapshotValidationError::UnsupportedMsrState {
                    feature: "RDTSCP",
                    cpu_type: crate::CpuType::Pentium4,
                },
                "invalid CPU snapshot: RDTSCP state is non-default on Pentium4",
            ),
            (
                CpuSnapshotValidationError::ZeroTscClock,
                "invalid CPU snapshot TSC clock: an explicit clock rate must be nonzero",
            ),
            (
                CpuSnapshotValidationError::ZeroTscRatio,
                "invalid CPU snapshot TSC ratio: a zero Q32.32 tick-per-cycle ratio freezes RDTSC",
            ),
            (
                CpuSnapshotValidationError::TscEpochAheadOfCycles {
                    epoch_cycles: 11,
                    cycles: 9,
                },
                "invalid CPU snapshot TSC epoch: epoch cycles 11 are ahead of the          snapshot's cycle count 9",
            ),
            (
                CpuSnapshotValidationError::ForgedMxcsr {
                    mxcsr: 0x1F80,
                    mxcsr_mask: 0xFFFF,
                    allowed: 0xFF,
                    cpu_type: crate::CpuType::Pentium4,
                },
                "invalid CPU snapshot MXCSR 0x1f80 / MXCSR_MASK 0xffff: bits beyond          the Pentium4 capability mask 0xff",
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
            assert_eq!(format!("{:>80.3}", error), expected);
            assert!(std::error::Error::source(&error).is_none());
        }
    }
    #[test]
    fn restore_state_rejects_out_of_range_cpl_without_mutation() {
        let mut cpu = Cpu::new();
        cpu.regs.eax = 0xA1B2_C3D4;
        let before = serde_json::to_value(cpu.snapshot_state()).unwrap();
        let mut malformed = cpu.snapshot_state();
        malformed.regs.cpl = 4;
        let error = cpu.restore_state(&malformed).unwrap_err();
        assert_eq!(error, CpuSnapshotValidationError::CplOutOfRange { cpl: 4 });
        assert_eq!(serde_json::to_value(cpu.snapshot_state()).unwrap(), before);
    }
    /// MDD-BUG-KIL-00878: a protected-mode image may not pair a ring-3 CS
    /// selector with CPL 0 (or the reverse); the PE-switch window, where CS
    /// still holds its real-mode cache at CPL 0, stays loadable.
    #[test]
    fn restore_state_rejects_cs_rpl_cpl_mismatch_without_mutation() {
        let mut cpu = Cpu::new();
        cpu.regs.eax = 0xA1B2_C3D4;
        let before = serde_json::to_value(cpu.snapshot_state()).unwrap();
        let mut descriptor_cs = cpu.snapshot_state();
        descriptor_cs.regs.cr0 |= 0x1;
        descriptor_cs.regs.cs = 0x1B;
        descriptor_cs.regs.cs_cache =
            SegmentCache::from_descriptor(&[0xFF, 0xFF, 0x00, 0x00, 0x40, 0xFB, 0xCF, 0x00]);
        for (cpl, cs) in [(0u8, 0x1Bu16), (3, 0x08)] {
            let mut malformed = descriptor_cs.clone();
            malformed.regs.cpl = cpl;
            malformed.regs.cs = cs;
            assert_eq!(
                cpu.restore_state(&malformed).unwrap_err(),
                CpuSnapshotValidationError::CsRplCplMismatch { cs, cpl }
            );
            assert_eq!(serde_json::to_value(cpu.snapshot_state()).unwrap(), before);
        }
        let mut ring3 = descriptor_cs.clone();
        ring3.regs.cpl = 3;
        cpu.restore_state(&ring3).unwrap();
        assert_eq!(cpu.regs.cpl, 3);
        let mut window = Cpu::new().snapshot_state();
        window.regs.cr0 |= 0x1;
        window.regs.cs = 0x07C3;
        window.regs.cs_cache = SegmentCache::real_mode(0x07C3);
        window.regs.cpl = 0;
        cpu.restore_state(&window).unwrap();
        assert_eq!((cpu.regs.cs, cpu.regs.cpl), (0x07C3, 0));
    }
    #[test]
    fn restore_state_rejects_v86_cpl_mismatch_without_mutation() {
        let mut cpu = Cpu::new();
        cpu.regs.eax = 0xA1B2_C3D4;
        let before = serde_json::to_value(cpu.snapshot_state()).unwrap();
        let mut malformed = cpu.snapshot_state();
        malformed.regs.eflags_high |= 0x0002;
        malformed.regs.cpl = 0;
        let error = cpu.restore_state(&malformed).unwrap_err();
        assert_eq!(error, CpuSnapshotValidationError::V86CplMismatch { cpl: 0 });
        assert_eq!(serde_json::to_value(cpu.snapshot_state()).unwrap(), before);
    }
    /// MDD-BUG-KIL-00766: a snapshot may not claim an MXCSR capability its
    /// CPU identity lacks, and MXCSR must fit the identity's mask.
    #[test]
    fn restore_state_rejects_a_forged_mxcsr_capability_without_mutation() {
        let mut cpu = Cpu::new();
        cpu.set_cpu_type(CpuType::PentiumIII);
        cpu.regs.eax = 0xA1B2_C3D4;
        let before = serde_json::to_value(cpu.snapshot_state()).unwrap();
        for (mxcsr_mask, mxcsr) in [
            (crate::sse::MXCSR_MASK_DAZ, crate::sse::MXCSR_RESET),
            (crate::sse::MXCSR_MASK, crate::sse::MXCSR_RESET | 0x40),
            (crate::sse::MXCSR_MASK, crate::sse::MXCSR_RESET | 0x1_0000),
        ] {
            let mut malformed = cpu.snapshot_state();
            malformed.sse.mxcsr_mask = mxcsr_mask;
            malformed.sse.mxcsr = mxcsr;
            let error = cpu.restore_state(&malformed).unwrap_err();
            assert!(
                matches!(error, CpuSnapshotValidationError::ForgedMxcsr { .. }),
                "mask {mxcsr_mask:#x} mxcsr {mxcsr:#x}: {error:?}"
            );
            assert_eq!(serde_json::to_value(cpu.snapshot_state()).unwrap(), before);
        }
    }
    /// The valid control: a Pentium 4 image keeps DAZ, and one carrying the
    /// narrower pre-identity mask is restored with the identity's mask.
    #[test]
    fn restore_state_keeps_pentium4_daz_and_derives_the_mask_from_identity() {
        let mut cpu = Cpu::new();
        cpu.set_cpu_type(CpuType::Pentium4);
        let mut snap = cpu.snapshot_state();
        snap.sse.mxcsr = crate::sse::MXCSR_RESET | 0x40;
        cpu.restore_state(&snap).unwrap();
        assert_eq!(cpu.sse.mxcsr, crate::sse::MXCSR_RESET | 0x40);
        assert_eq!(cpu.sse.mxcsr_mask, crate::sse::MXCSR_MASK_DAZ);
        snap.sse.mxcsr = crate::sse::MXCSR_RESET;
        snap.sse.mxcsr_mask = crate::sse::MXCSR_MASK;
        cpu.restore_state(&snap).unwrap();
        assert_eq!(cpu.sse.mxcsr_mask, crate::sse::MXCSR_MASK_DAZ);
    }
    /// A zero Q32.32 ratio freezes RDTSC forever, so the image is rejected
    /// before it can touch the CPU (MDD-BUG-KIL-00767).
    ///
    /// `Cpu::set_tsc_rate` clamps the ratio with `.max(1)`, so this state is
    /// unreachable through the API — only a corrupt or hand-edited snapshot
    /// carries it, and a snapshot is untrusted input.
    #[test]
    fn restore_state_rejects_a_zero_tsc_ratio_without_mutation() {
        let mut cpu = Cpu::new();
        cpu.regs.eax = 0xA1B2_C3D4;
        let before = serde_json::to_value(cpu.snapshot_state()).unwrap();
        let mut malformed = cpu.snapshot_state();
        malformed.tsc_ticks_per_cycle_q32 = 0;
        let error = cpu.restore_state(&malformed).unwrap_err();
        assert_eq!(error, CpuSnapshotValidationError::ZeroTscRatio);
        assert_eq!(serde_json::to_value(cpu.snapshot_state()).unwrap(), before);
    }
    /// An epoch ahead of the cycle count stalls RDTSC until `cycles` catches
    /// up and then changes its slope (MDD-BUG-KIL-00767).
    ///
    /// `set_tsc_rate` always writes `tsc_epoch_cycles = self.cycles`, so this
    /// state is likewise unreachable through the API.
    #[test]
    fn restore_state_rejects_a_future_tsc_epoch_without_mutation() {
        let mut cpu = Cpu::new();
        cpu.regs.eax = 0xA1B2_C3D4;
        cpu.cycles = 1_000;
        let before = serde_json::to_value(cpu.snapshot_state()).unwrap();
        let mut malformed = cpu.snapshot_state();
        malformed.tsc_epoch_cycles = malformed.cycles + 5_000;
        let error = cpu.restore_state(&malformed).unwrap_err();
        assert_eq!(
            error,
            CpuSnapshotValidationError::TscEpochAheadOfCycles {
                epoch_cycles: 6_000,
                cycles: 1_000,
            }
        );
        assert_eq!(serde_json::to_value(cpu.snapshot_state()).unwrap(), before);
    }
    /// What the rejected images would have done, stated directly against
    /// `tsc_ticks_at` so the rejections above are not merely assertions about
    /// themselves (MDD-BUG-KIL-00767).
    #[test]
    fn the_rejected_tsc_states_are_the_ones_that_break_guest_time() {
        let mut frozen = Cpu::new();
        frozen.tsc_ticks_per_cycle_q32 = 0;
        frozen.tsc_epoch_ticks = 77;
        assert_eq!(frozen.tsc_ticks_at(0), 77);
        assert_eq!(frozen.tsc_ticks_at(1_000_000), 77, "RDTSC never advances");
        let mut warped = Cpu::new();
        warped.tsc_epoch_cycles = 1_000;
        warped.tsc_epoch_ticks = 0;
        assert_eq!(warped.tsc_ticks_at(0), 0);
        assert_eq!(warped.tsc_ticks_at(999), 0, "stalled below the epoch");
        assert_eq!(warped.tsc_ticks_at(1_000), 0);
        assert_eq!(warped.tsc_ticks_at(1_500), 500, "and only then advances");
        let healthy = Cpu::new();
        assert_eq!(healthy.tsc_ticks_at(0), 0);
        assert_eq!(healthy.tsc_ticks_at(1_500), 1_500);
    }
    /// V86 is a sub-mode of protected mode, so VM=1 with
    /// CR0.PE=0 describes a CPU that cannot exist. It must be refused before
    /// any live state is touched — restoring it makes PE-keyed and VM-keyed
    /// helpers disagree, and the guest silently gains I/O and interrupt-flag
    /// privileges it must not have.
    #[test]
    fn restore_state_rejects_v86_without_protected_mode_without_mutation() {
        let mut cpu = Cpu::new();
        cpu.regs.eax = 0xA1B2_C3D4;
        let before = serde_json::to_value(cpu.snapshot_state()).unwrap();
        let mut malformed = cpu.snapshot_state();
        malformed.regs.eflags_high |= 0x0002;
        malformed.regs.cpl = 3;
        malformed.regs.cr0 &= !0x1;
        let error = cpu.restore_state(&malformed).unwrap_err();
        assert_eq!(error, CpuSnapshotValidationError::V86WithoutProtectedMode);
        assert_eq!(
            serde_json::to_value(cpu.snapshot_state()).unwrap(),
            before,
            "a rejected snapshot must not have mutated live CPU state"
        );
    }
    /// MDD-BUG-CRUCIBLE-00581: a segment cache whose derived fields disagree
    /// with its canonical bytes must be rejected.
    ///
    /// This is the exploit the entry names: a small, non-writable segment in
    /// `limit`/`access`, but a `cached_effective_limit` of 0xFFFF_FFFF. Bounds
    /// checks read the derived field, so the restored guest would address four
    /// gigabytes through a descriptor that describes 4 KiB.
    #[test]
    fn restore_state_rejects_an_incoherent_segment_cache_without_mutation() {
        let mut cpu = Cpu::new();
        cpu.regs.eax = 0xA1B2_C3D4;
        let before = serde_json::to_value(cpu.snapshot_state()).unwrap();
        let mut malformed = cpu.snapshot_state();
        malformed.regs.ds_cache.limit = 0x0FFF;
        malformed.regs.ds_cache.granularity = false;
        malformed.regs.ds_cache.flags = 0x00;
        malformed.regs.ds_cache.cached_effective_limit = 0xFFFF_FFFF;
        let error = cpu.restore_state(&malformed).unwrap_err();
        assert_eq!(
            error,
            CpuSnapshotValidationError::IncoherentSegmentCache { segment: "DS" }
        );
        assert_eq!(
            serde_json::to_value(cpu.snapshot_state()).unwrap(),
            before,
            "a rejected snapshot must not have mutated live CPU state"
        );
    }
    /// The control that keeps the rule honest: an ordinary real-mode cache must
    /// still restore.
    ///
    /// `SegmentCache::real_mode` sets `is_readable` true for a DATA segment,
    /// while `from_descriptor` derives that field as "readable CODE" and so
    /// yields false. Comparing it would reject every real-mode snapshot - which
    /// is exactly what the first version of this validation did, and what the
    /// existing suite caught.
    #[test]
    fn restore_state_accepts_ordinary_real_mode_segment_caches() {
        let mut cpu = Cpu::new();
        let snap = cpu.snapshot_state();
        cpu.restore_state(&snap)
            .expect("a freshly constructed CPU must round-trip through validation");
    }
    /// MDD-BUG-KIL-00769: `instruction_offset` reads any `cs_bits` other than
    /// 32 as 16, so a malformed value must be rejected, not resumed as 16-bit.
    #[test]
    fn restore_state_rejects_cs_bits_other_than_16_or_32_without_mutation() {
        for cs_bits in [0, 8, 24, 64] {
            let mut cpu = Cpu::new();
            cpu.regs.eax = 0xA1B2_C3D4;
            let before = serde_json::to_value(cpu.snapshot_state()).unwrap();
            let mut malformed = cpu.snapshot_state();
            malformed.cs_bits = cs_bits;
            let error = cpu.restore_state(&malformed).unwrap_err();
            assert_eq!(error, CpuSnapshotValidationError::InvalidCsBits { cs_bits });
            assert_eq!(serde_json::to_value(cpu.snapshot_state()).unwrap(), before);
        }
        for cs_bits in [16, 32] {
            let mut cpu = Cpu::new();
            let mut snap = cpu.snapshot_state();
            snap.cs_bits = cs_bits;
            cpu.restore_state(&snap)
                .unwrap_or_else(|error| panic!("cs_bits {cs_bits} must restore: {error}"));
        }
    }
    /// MDD-BUG-KIL-00769: protected-mode reads through a code segment trust
    /// `is_readable`, so an execute-only code descriptor restored as readable
    /// would grant reads the descriptor forbids.
    #[test]
    fn restore_state_rejects_a_readable_execute_only_code_cache_without_mutation() {
        let execute_only = SegmentCache::from_descriptor(&[0xFF, 0xFF, 0, 0, 0, 0x99, 0, 0]);
        assert!(execute_only.is_code && !execute_only.is_readable);
        let mut cpu = Cpu::new();
        cpu.regs.eax = 0xA1B2_C3D4;
        let before = serde_json::to_value(cpu.snapshot_state()).unwrap();
        let mut malformed = cpu.snapshot_state();
        malformed.regs.cs_cache = SegmentCache {
            is_readable: true,
            ..execute_only
        };
        let error = cpu.restore_state(&malformed).unwrap_err();
        assert_eq!(
            error,
            CpuSnapshotValidationError::IncoherentSegmentCache { segment: "CS" }
        );
        assert_eq!(serde_json::to_value(cpu.snapshot_state()).unwrap(), before);
        let mut coherent = cpu.snapshot_state();
        coherent.regs.cs_cache = execute_only;
        coherent.regs.ds_cache = SegmentCache::real_mode(0x1234);
        cpu.restore_state(&coherent)
            .expect("a coherent execute-only code cache must restore");
    }
    /// The control: a well-formed V86 image (VM=1, CPL=3, PE=1) must still
    /// round-trip, so the new invariant rejects only the impossible state.
    #[test]
    fn restore_state_accepts_a_well_formed_v86_snapshot() {
        let mut cpu = Cpu::new();
        let mut valid = cpu.snapshot_state();
        valid.regs.eflags_high |= 0x0002;
        valid.regs.cpl = 3;
        valid.regs.cr0 |= 0x1;
        valid.regs.eax = 0x1234_5678;
        cpu.restore_state(&valid)
            .expect("a valid V86 image must restore");
        assert!(cpu.regs.is_v86_mode());
        assert_eq!(cpu.regs.cpl, 3);
        assert_eq!(cpu.regs.cr0 & 0x1, 1);
        assert_eq!(cpu.regs.eax, 0x1234_5678);
    }
    /// Real mode with PE=0 and VM=0 is of course still fine — the invariant is
    /// about the combination, not about PE on its own.
    #[test]
    fn restore_state_accepts_real_mode_with_pe_clear() {
        let mut cpu = Cpu::new();
        let mut valid = cpu.snapshot_state();
        valid.regs.eflags_high &= !0x0002;
        valid.regs.cr0 &= !0x1;
        valid.regs.cpl = 0;
        cpu.restore_state(&valid)
            .expect("plain real mode must restore");
        assert!(!cpu.regs.is_v86_mode());
    }
    #[test]
    fn test_r6_add_hle_break_selector() {
        let mut cpu = Cpu::new();
        cpu.add_hle_break_selector(0x0008);
        assert_eq!(cpu.hle_break_count, 1);
        assert_eq!(cpu.hle_break_selectors[0], 0x0008);
    }
    #[test]
    fn test_r6_add_hle_break_selector_duplicate_skipped() {
        let mut cpu = Cpu::new();
        cpu.add_hle_break_selector(0x0008);
        cpu.add_hle_break_selector(0x0008);
        assert_eq!(cpu.hle_break_count, 1);
    }
    #[test]
    fn test_r6_add_hle_break_selector_full_array() {
        let mut cpu = Cpu::new();
        for i in 0..32 {
            cpu.add_hle_break_selector((i * 8) as u16);
        }
        assert_eq!(cpu.hle_break_count, 32);
        cpu.add_hle_break_selector(0xFF00);
        assert_eq!(cpu.hle_break_count, 32);
    }
    #[test]
    fn test_r6_execute_iretd_same_privilege() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let cs_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x40, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, cs_desc);
        cpu.regs.cs = 0x08;
        cpu.regs.cs_cache = SegmentCache::from_descriptor(&cs_desc);
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.ss_cache.present = true;
        cpu.regs.esp = 0x2000;
        cpu.regs.flags = crate::Flags::RESERVED1;
        mem.write_u32(0x2000, 0x0400);
        mem.write_u32(0x2004, 0x0008);
        mem.write_u32(0x2008, 0x0202);
        cpu.execute_iretd(&mut mem).unwrap();
        assert_eq!(cpu.regs.eip, 0x0400);
        assert_eq!(cpu.regs.cs, 0x08);
        assert_eq!(cpu.regs.cpl, 0);
        assert!(cpu.interrupts_enabled);
    }
    #[test]
    fn test_r6_execute_iretd_v86_switch_blocked() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.ss_cache.present = true;
        cpu.regs.esp = 0x2000;
        cpu.regs.flags = crate::Flags::RESERVED1;
        cpu.regs.cpl = 0;
        let cs3_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFA, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, cs3_desc);
        cpu.regs.cs = 0x0B;
        cpu.regs.cs_cache = SegmentCache::from_descriptor(&cs3_desc);
        cpu.regs.cpl = 3;
        mem.write_u32(0x2000, 0x0400);
        mem.write_u32(0x2004, 0x000B);
        mem.write_u32(0x2008, 0x0002_0002);
        mem.write_u32(0x200C, 0x1000);
        mem.write_u32(0x2010, 0x2000);
        mem.write_u32(0x2014, 0x3000);
        mem.write_u32(0x2018, 0x4000);
        mem.write_u32(0x201C, 0x5000);
        mem.write_u32(0x2020, 0x6000);
        let err = cpu.execute_iretd(&mut mem).unwrap_err();
        match err {
            CpuError::V86SwitchBlocked(frame) => {
                assert_eq!(frame.eip, 0x0400);
                assert_eq!(frame.esp, 0x1000);
                assert_eq!(frame.ss, 0x2000);
                assert_eq!(frame.es, 0x3000);
                assert_eq!(frame.ds, 0x4000);
                assert_eq!(frame.fs, 0x5000);
                assert_eq!(frame.gs, 0x6000);
            }
            other => panic!("Expected V86SwitchBlocked, got {:?}", other),
        }
    }
    #[test]
    fn test_r6_execute_iretd_returning_outer_clears_segments() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x1FFF;
        let cs0_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x40, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, cs0_desc);
        let cs3_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFA, 0x40, 0x00];
        write_descriptor(&mut mem, 0x1000 + 16, cs3_desc);
        let ss3_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xF2, 0x40, 0x00];
        write_descriptor(&mut mem, 0x1000 + 24, ss3_desc);
        cpu.regs.cs = 0x08;
        cpu.regs.cs_cache = SegmentCache::from_descriptor(&cs0_desc);
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.ss_cache.present = true;
        cpu.regs.esp = 0x3000;
        cpu.regs.flags = crate::Flags::RESERVED1;
        cpu.regs.ds = 0x10;
        cpu.regs.ds_cache.dpl = 0;
        cpu.regs.es = 0x10;
        cpu.regs.es_cache.dpl = 0;
        cpu.regs.fs = 0x10;
        cpu.regs.fs_cache.dpl = 0;
        cpu.regs.gs = 0x10;
        cpu.regs.gs_cache.dpl = 0;
        mem.write_u32(0x3000, 0x0400);
        mem.write_u32(0x3004, 0x0013);
        mem.write_u32(0x3008, 0x0002);
        mem.write_u32(0x300C, 0x8000);
        mem.write_u32(0x3010, 0x001B);
        cpu.execute_iretd(&mut mem).unwrap();
        assert_eq!(cpu.regs.cpl, 3);
        assert_eq!(cpu.regs.eip, 0x0400);
        assert_eq!(cpu.regs.esp, 0x8000);
        assert_eq!(cpu.regs.ds, 0);
        assert_eq!(cpu.regs.es, 0);
        assert_eq!(cpu.regs.fs, 0);
        assert_eq!(cpu.regs.gs, 0);
    }
    #[test]
    fn test_r6_execute_iretd_eflags_high_masked_for_386() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.set_cpu_type(CpuType::I80386);
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let cs_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x40, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, cs_desc);
        cpu.regs.cs = 0x08;
        cpu.regs.cs_cache = SegmentCache::from_descriptor(&cs_desc);
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.ss_cache.present = true;
        cpu.regs.esp = 0x2000;
        cpu.regs.flags = crate::Flags::RESERVED1;
        let eflags: u32 = 0x0024_0002;
        mem.write_u32(0x2000, 0x0400);
        mem.write_u32(0x2004, 0x0008);
        mem.write_u32(0x2008, eflags);
        cpu.execute_iretd(&mut mem).unwrap();
        assert_eq!(
            cpu.regs.eflags_high & 0x0024,
            0,
            "386 should mask AC and ID bits"
        );
    }
    /// IRETD of an all-ones EFLAGS image must not store bits 22-31: they are
    /// reserved on every modeled CPU and read as zero on real silicon
    /// VM is cleared in the image so this exercises the
    /// ordinary protected-mode return rather than the V86-entry branch.
    #[test]
    fn test_execute_iretd_never_stores_reserved_eflags_bits_kiln283() {
        const CASES: [(CpuType, u16); 3] = [
            (CpuType::I80386, 0x0000),
            (CpuType::I80486, 0x0004),
            (CpuType::Pentium, 0x003C),
        ];
        for (cpu_type, expected_optional) in CASES {
            let mut cpu = Cpu::new();
            let mut mem = TestMemory::new(0x100000);
            cpu.regs.cr0 = 1;
            cpu.regs.cpl = 0;
            cpu.set_cpu_type(cpu_type);
            cpu.regs.gdtr.base = 0x1000;
            cpu.regs.gdtr.limit = 0x0FFF;
            let cs_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x40, 0x00];
            write_descriptor(&mut mem, 0x1000 + 8, cs_desc);
            cpu.regs.cs = 0x08;
            cpu.regs.cs_cache = SegmentCache::from_descriptor(&cs_desc);
            cpu.regs.ss_cache.big = true;
            cpu.regs.ss_cache.base = 0;
            cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
            cpu.regs.ss_cache.is_writable = true;
            cpu.regs.ss_cache.present = true;
            cpu.regs.esp = 0x2000;
            cpu.regs.flags = crate::Flags::RESERVED1;
            cpu.regs.eflags_high = 0;
            mem.write_u32(0x2000, 0x0400);
            mem.write_u32(0x2004, 0x0008);
            mem.write_u32(0x2008, 0xFFFD_FFFF);
            cpu.execute_iretd(&mut mem).unwrap();
            assert_eq!(
                cpu.regs.eflags_high & 0xFFC0,
                0,
                "{cpu_type:?}: EFLAGS 22-31 are reserved and must read back zero"
            );
            assert_eq!(
                cpu.regs.eflags_high & 0x003C,
                expected_optional,
                "{cpu_type:?}: AC/VIF/VIP/ID must follow the modeled generation"
            );
        }
    }
    #[test]
    fn test_r6_execute_iretd_vm_flag_cleared_at_nonzero_cpl() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let cs_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFA, 0x40, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, cs_desc);
        cpu.regs.cs = 0x0B;
        cpu.regs.cs_cache = SegmentCache::from_descriptor(&cs_desc);
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.ss_cache.present = true;
        cpu.regs.esp = 0x2000;
        cpu.regs.flags = crate::Flags::RESERVED1;
        cpu.regs.eflags_high = 0x0000;
        mem.write_u32(0x2000, 0x0400);
        mem.write_u32(0x2004, 0x000B);
        mem.write_u32(0x2008, 0x0000_0202);
        cpu.execute_iretd(&mut mem).unwrap();
        assert_eq!(
            cpu.regs.eflags_high & 0x0002,
            0,
            "VM flag should stay 0 at CPL != 0"
        );
    }
    #[test]
    fn test_r6_execute_iretd_vm_flag_modifiable_at_cpl0() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let cs_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x40, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, cs_desc);
        cpu.regs.cs = 0x08;
        cpu.regs.cs_cache = SegmentCache::from_descriptor(&cs_desc);
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.ss_cache.present = true;
        cpu.regs.esp = 0x2000;
        cpu.regs.flags = crate::Flags::RESERVED1;
        cpu.regs.eflags_high = 0x0000;
        cpu.set_cpu_type(CpuType::Pentium);
        let eflags: u32 = 0x0004_0002;
        mem.write_u32(0x2000, 0x0400);
        mem.write_u32(0x2004, 0x0008);
        mem.write_u32(0x2008, eflags);
        cpu.execute_iretd(&mut mem).unwrap();
        assert_ne!(
            cpu.regs.eflags_high & 0x0004,
            0,
            "AC bit should be set after IRETD at CPL=0"
        );
    }
    fn real_mode_iretd_fixture(old_high: u16, frame_high: u16) -> (Cpu, TestMemory) {
        let mut cpu = Cpu::new();
        cpu.set_cpu_type(CpuType::Pentium);
        cpu.regs.cr0 = 0;
        // A retained VM bit with PE clear is still a real-address return.
        // CPL3 satisfies the current Registers VM invariant in this fixture.
        cpu.regs.cpl = if old_high & 2 != 0 { 3 } else { 0 };
        cpu.regs.eflags_high = old_high;
        cpu.regs.flags = crate::Flags::RESERVED1;
        cpu.regs.ss = 0x1000;
        cpu.regs.ss_cache = SegmentCache::real_mode(cpu.regs.ss);
        cpu.regs.ds = 0x2000;
        cpu.regs.ds_cache = SegmentCache::real_mode(cpu.regs.ds);
        cpu.regs.es = 0x3000;
        cpu.regs.es_cache = SegmentCache::real_mode(cpu.regs.es);
        cpu.regs.fs = 0x4000;
        cpu.regs.fs_cache = SegmentCache::real_mode(cpu.regs.fs);
        cpu.regs.gs = 0x5000;
        cpu.regs.gs_cache = SegmentCache::real_mode(cpu.regs.gs);
        cpu.regs.esp = 0x1234_0100;
        let mut mem = TestMemory::new(0x20000);
        mem.write_u32(0x10100, 0x0200);
        mem.write_u32(0x10104, 0x1234);
        mem.write_u32(0x10108, (u32::from(frame_high) << 16) | 0x0243);
        (cpu, mem)
    }

    #[test]
    fn real_mode_iretd_reads_only_three_dwords_for_saved_vm_clear_and_set() {
        for old_vm in [0, 2] {
            for saved_vm in [0, 2] {
                let (mut cpu, mut mem) = real_mode_iretd_fixture(old_vm, saved_vm);
                let before = cpu.regs.clone();
                mem.forbidden_reads = Some(0x1010c..0x10124);
                cpu.execute_iretd(&mut mem).unwrap();
                assert_eq!((cpu.regs.cs, cpu.regs.eip), (0x1234, 0x0200));
                assert_eq!(cpu.regs.cs_cache.base, 0x12340);
                assert_eq!(cpu.regs.esp, 0x1234_010c);
                assert_eq!(cpu.regs.cpl, before.cpl);
                assert_eq!(cpu.cs_bits, 16);
                assert_eq!(cpu.regs.eflags_high & 2, old_vm);
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

    #[test]
    fn real_mode_iretd_preserves_each_vm_vif_vip_bit_from_entry_flags() {
        for (cpu_type, retained_bits, supported_high) in [
            (CpuType::I80386, &[0x0002][..], 0x0003),
            (CpuType::Pentium, &[0x0002, 0x0008, 0x0010][..], 0x003f),
        ] {
            for &retained in retained_bits {
                for old_high in [retained, 0] {
                    for saved_high in [0, retained] {
                        let frame_high = saved_high | 0x0025;
                        let (mut cpu, mut mem) = real_mode_iretd_fixture(old_high, frame_high);
                        cpu.set_cpu_type(cpu_type);
                        cpu.execute_iretd(&mut mem).unwrap();
                        assert_eq!(cpu.regs.eflags_high & 0x001a, old_high,
                            "{cpu_type:?}, retained={retained:#x}, old={old_high:#x}, frame={frame_high:#x}");
                        assert_eq!(
                            cpu.regs.eflags_high & 0x0025,
                            0x0025 & supported_high,
                            "RF, AC and ID restore according to modeled CPU support"
                        );
                    }
                }
            }
        }
        // An imported raw high-flags image must not preserve unsupported bits.
        let (mut cpu, mut mem) = real_mode_iretd_fixture(0x0018, 0x003d);
        cpu.set_cpu_type(CpuType::I80386);
        cpu.execute_iretd(&mut mem).unwrap();
        assert_eq!(cpu.regs.eflags_high, 1);
    }

    #[test]
    fn real_mode_iretd_fix_preserves_protected_mode_v86_return() {
        let mut cpu = Cpu::new();
        cpu.set_cpu_type(CpuType::Pentium);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.ss_cache = SegmentCache::real_mode(0);
        cpu.regs.ss_cache.big = true;
        cpu.regs.esp = 0x1000;
        let mut mem = TestMemory::new(0x2000);
        let frame: [u32; 9] = [
            0x0200,
            0x1234,
            0x0002_0243,
            0x3456,
            0x2000,
            0x3000,
            0x4000,
            0x5000,
            0x6000,
        ];
        for (index, value) in frame.into_iter().enumerate() {
            mem.write_u32(0x1000 + index * 4, value);
        }
        let result = cpu.execute_iretd(&mut mem);
        assert!(result.is_ok(), "protected-mode V86 return: {result:?}");
        assert_eq!(
            (cpu.regs.cs, cpu.regs.eip, cpu.regs.ss, cpu.regs.esp),
            (0x1234, 0x0200, 0x2000, 0x3456)
        );
        assert_eq!(
            (cpu.regs.es, cpu.regs.ds, cpu.regs.fs, cpu.regs.gs),
            (0x3000, 0x4000, 0x5000, 0x6000)
        );
        for (selector, cache) in [
            (cpu.regs.cs, cpu.regs.cs_cache),
            (cpu.regs.ss, cpu.regs.ss_cache),
            (cpu.regs.es, cpu.regs.es_cache),
            (cpu.regs.ds, cpu.regs.ds_cache),
            (cpu.regs.fs, cpu.regs.fs_cache),
            (cpu.regs.gs, cpu.regs.gs_cache),
        ] {
            assert_eq!(cache, SegmentCache::real_mode(selector));
        }
        assert_eq!(cpu.regs.eflags(), 0x0002_0243);
        assert_eq!(cpu.regs.cpl, 3);
        assert_eq!(cpu.cs_bits, 16);
        assert!(cpu.interrupts_enabled);
    }

    #[test]
    fn test_r6_execute_iretd_real_mode() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.regs.ss = 0x1000;
        cpu.regs.ss_cache = SegmentCache::real_mode(0x1000);
        cpu.regs.set_sp(0x0100);
        cpu.regs.esp = 0x0100;
        cpu.regs.flags = crate::Flags::RESERVED1;
        let base = 0x10000usize;
        mem.write_u32(base + 0x0100, 0x0200);
        mem.write_u32(base + 0x0104, 0x5678);
        mem.write_u32(base + 0x0108, 0x0202);
        cpu.execute_iretd(&mut mem).unwrap();
        assert_eq!(cpu.regs.eip, 0x0200);
        assert_eq!(cpu.regs.cs, 0x5678);
        assert!(cpu.interrupts_enabled);
    }
    #[test]
    fn test_execute_iretd_real_mode_invalid_eip_restores_stack() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x10000);
        // Synthetic saved state and wrapped frame; invalid EIP must roll back.
        cpu.last_exception_error_code = 0xDEAD_BEEF;
        cpu.regs.cs = 0x1234;
        cpu.regs.cs_cache = SegmentCache::real_mode(0x1234);
        cpu.regs.ss = 0;
        cpu.regs.ss_cache = SegmentCache::real_mode(0);
        cpu.regs.eip = 0x5678;
        cpu.regs.set_sp(0xFFFC);
        cpu.regs.esp = 0x0000_FFFC;
        cpu.regs.flags = crate::Flags::RESERVED1;
        mem.write_u32(0xFFFC, 0xFFFF_FFFF);
        mem.write_u32(0x0000, 0xABCD_2345);
        mem.write_u32(0x0004, 0x0000_0202);
        let err = cpu.execute_iretd(&mut mem).unwrap_err();
        assert!(matches!(err, crate::CpuError::GeneralProtectionFault));
        assert_eq!(cpu.regs.cs, 0x1234);
        assert_eq!(cpu.regs.eip, 0x5678);
        assert_eq!(cpu.regs.esp, 0x0000_FFFC);
        assert_eq!(cpu.regs.sp(), 0xFFFC);
        assert_eq!(cpu.last_exception_error_code, 0);
    }
    #[test]
    fn test_execute_iretd_outer_bad_cs_dpl_reports_selector_error_code() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        // Patterned synthetic return frame; the invalid selector determines the fault.
        cpu.last_exception_error_code = 0xDEAD_BEEF;
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x000F;
        let kernel_cs_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, kernel_cs_desc);
        cpu.regs.cs = 0x0008;
        cpu.regs.cs_cache = SegmentCache::from_descriptor(&kernel_cs_desc);
        cpu.cs_bits = 32;
        cpu.regs.eip = 0xABCD_1234;
        let orig_cs = cpu.regs.cs;
        let orig_eip = cpu.regs.eip;
        let orig_cpl = cpu.regs.cpl;
        cpu.regs.ss = 0x0010;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.ss_cache.present = true;
        cpu.regs.esp = 0x2000;
        cpu.regs.set_sp(0x2000);
        let orig_esp = cpu.regs.esp;
        let orig_sp = cpu.regs.sp();
        mem.write_u32(0x2000, 0xBCDE_2345);
        mem.write_u32(0x2004, 0x0000_000B);
        mem.write_u32(0x2008, 0x0000_0292);
        mem.write_u32(0x200C, 0x0001_FFFE);
        mem.write_u32(0x2010, 0x0000_0023);
        let err = cpu.execute_iretd(&mut mem).unwrap_err();
        assert!(matches!(err, crate::CpuError::GeneralProtectionFault));
        assert_eq!(cpu.last_exception_error_code, 0x0008);
        assert_eq!(cpu.regs.cs, orig_cs);
        assert_eq!(cpu.regs.eip, orig_eip);
        assert_eq!(cpu.regs.cpl, orig_cpl);
        assert_eq!(cpu.regs.esp, orig_esp);
        assert_eq!(cpu.regs.sp(), orig_sp);
    }
    #[test]
    fn test_iret_outer_bad_cs_dpl_reports_selector_error_code() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        // Patterned synthetic return frame; the invalid selector determines the fault.
        cpu.last_exception_error_code = 0xDEAD_BEEF;
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x000F;
        let kernel_cs_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, kernel_cs_desc);
        cpu.regs.cs = 0x0008;
        cpu.regs.cs_cache = SegmentCache::from_descriptor(&kernel_cs_desc);
        cpu.cs_bits = 16;
        cpu.regs.eip = 0x0000_1234;
        let orig_cs = cpu.regs.cs;
        let orig_eip = cpu.regs.eip;
        let orig_cpl = cpu.regs.cpl;
        cpu.regs.ss = 0x0010;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.ss_cache.present = true;
        cpu.regs.set_sp(0x2000);
        cpu.regs.esp = 0x2000;
        let orig_sp = cpu.regs.sp();
        let orig_esp = cpu.regs.esp;
        mem.write_u16(0x2000, 0x2345);
        mem.write_u16(0x2002, 0x000B);
        mem.write_u16(0x2004, 0x0292);
        mem.write_u16(0x2006, 0xFFFE);
        mem.write_u16(0x2008, 0x0023);
        let err = cpu.iret(&mut mem).unwrap_err();
        assert!(matches!(err, crate::CpuError::GeneralProtectionFault));
        assert_eq!(cpu.last_exception_error_code, 0x0008);
        assert_eq!(cpu.regs.cs, orig_cs);
        assert_eq!(cpu.regs.eip, orig_eip);
        assert_eq!(cpu.regs.cpl, orig_cpl);
        assert_eq!(cpu.regs.sp(), orig_sp);
        assert_eq!(cpu.regs.esp, orig_esp);
    }
    #[test]
    fn test_r6_execute_iretd_cs_load_failure_restores_esp() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0000;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.ss_cache.present = true;
        cpu.regs.esp = 0x2000;
        let orig_esp = cpu.regs.esp;
        mem.write_u32(0x2000, 0x0400);
        mem.write_u32(0x2004, 0x0008);
        mem.write_u32(0x2008, 0x0002);
        let err = cpu.execute_iretd(&mut mem);
        assert!(err.is_err());
        assert_eq!(cpu.regs.esp, orig_esp);
    }
    #[test]
    fn test_r6_try_push16_sp_ffff_wrap_real_mode() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x30000);
        cpu.cpu_type = CpuType::I8086;
        cpu.regs.ss = 0x1000;
        cpu.regs.ss_cache = SegmentCache::real_mode(0x1000);
        cpu.regs.set_sp(0x0001);
        cpu.regs.esp = 0x0001;
        cpu.try_push16(&mut mem, 0xCAFE).unwrap();
        assert_eq!(cpu.regs.sp(), 0xFFFF);
        let base = 0x10000usize;
        assert_eq!(mem.read_u8(base + 0xFFFF), 0xFE);
        assert_eq!(mem.read_u8(base), 0xCA);
    }
    #[test]
    fn test_r6_dpmi_trace_enabled() {
        let cpu = Cpu::new();
        let _ = cpu.dpmi_trace_enabled();
    }
    #[test]
    fn test_r6_ret_guard_pop_iret_hle() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(true);
        let mut cpu = Cpu::new();
        cpu.ret_guard_push(RetGuardEntry {
            kind: RetGuardKind::Iret,
            cs: 0x08,
            ip: 0x100,
            ss: 0x10,
            sp: 0x1000,
            tag: 0x21,
            caller_cs: 0x08,
            caller_ip: 0xF0,
        });
        assert_eq!(cpu.ret_guard_depth(), 1);
        cpu.ret_guard_pop_iret_hle();
        assert_eq!(cpu.ret_guard_depth(), 0);
        Cpu::set_ret_guard_for_tests(false);
    }
    #[test]
    fn test_r6_ret_guard_pop_retf_hle() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(true);
        let mut cpu = Cpu::new();
        cpu.ret_guard_push(RetGuardEntry {
            kind: RetGuardKind::Far,
            cs: 0x08,
            ip: 0x100,
            ss: 0x10,
            sp: 0x1000,
            tag: 0,
            caller_cs: 0x08,
            caller_ip: 0xF0,
        });
        assert_eq!(cpu.ret_guard_depth(), 1);
        cpu.ret_guard_pop_retf_hle();
        assert_eq!(cpu.ret_guard_depth(), 0);
        Cpu::set_ret_guard_for_tests(false);
    }
    #[test]
    fn test_r6_ret_guard_push_interrupt_disabled() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(false);
        let mut cpu = Cpu::new();
        cpu.ret_guard_push_interrupt(0x21);
        assert_eq!(cpu.ret_guard_depth(), 0);
    }
    #[test]
    fn test_iret16_atomicity_load_ss_failure() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x1FFF;
        let cs0_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, cs0_desc);
        cpu.regs.cs = 0x08;
        cpu.regs.cs_cache = SegmentCache::from_descriptor(&cs0_desc);
        let orig_cs = cpu.regs.cs;
        let orig_cs_cache = cpu.regs.cs_cache;
        let orig_cpl = cpu.regs.cpl;
        let orig_cs_bits = cpu.cs_bits;
        let cs3_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFA, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 16, cs3_desc);
        cpu.regs.ss = 0x08;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ss_cache.big = false;
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.ss_cache.present = true;
        cpu.regs.set_ip(0x0200);
        cpu.regs.eip = 0x0200;
        cpu.regs.set_sp(0xF000);
        cpu.regs.esp = 0xF000;
        let orig_ip = cpu.regs.ip();
        let orig_eip = cpu.regs.eip;
        let orig_sp = cpu.regs.sp();
        let orig_esp = cpu.regs.esp;
        let orig_flags = cpu.regs.flags;
        let base = 0xF000usize;
        mem.write_u16(base, 0x0400);
        mem.write_u16(base + 2, 0x0013);
        mem.write_u16(base + 4, 0x0202);
        mem.write_u16(base + 6, 0x8000);
        mem.write_u16(base + 8, 0x001B);
        let result = cpu.iret(&mut mem);
        assert!(result.is_err(), "Expected error from invalid SS");
        assert_eq!(
            cpu.regs.cs, orig_cs,
            "CS should be restored after failed IRET"
        );
        assert_eq!(
            cpu.regs.cpl, orig_cpl,
            "CPL should be restored after failed IRET"
        );
        assert_eq!(
            cpu.cs_bits, orig_cs_bits,
            "cs_bits should be restored after failed IRET"
        );
        assert_eq!(
            cpu.regs.cs_cache.base, orig_cs_cache.base,
            "cs_cache should be restored after failed IRET"
        );
        assert_eq!(
            cpu.regs.ip(),
            orig_ip,
            "IP should be restored after failed IRET"
        );
        assert_eq!(
            cpu.regs.eip, orig_eip,
            "EIP should be restored after failed IRET"
        );
        assert_eq!(
            cpu.regs.flags, orig_flags,
            "FLAGS should be restored after failed IRET"
        );
        assert_eq!(
            cpu.regs.sp(),
            orig_sp,
            "SP should be restored after failed IRET"
        );
        assert_eq!(
            cpu.regs.esp, orig_esp,
            "ESP should be restored after failed IRET"
        );
    }
    /// Regression: IRET with IF=1 must NOT set interrupt_inhibit.
    /// Only STI, MOV SS, POP SS create an interrupt shadow (Intel SDM 6.8.3).
    #[test]
    fn test_iret_does_not_set_interrupt_inhibit() {
        let _serial = crate::test_sync::serial_guard();
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.ss = 0x0000;
        cpu.regs.set_sp(0x2000);
        cpu.regs.esp = 0x2000;
        cpu.interrupts_enabled = false;
        cpu.interrupt_inhibit = 0;
        let base = 0x2000usize;
        mem.write_u16(base, 0x0100);
        mem.write_u16(base + 2, 0x0050);
        mem.write_u16(base + 4, 0x0202);
        cpu.regs.set_sp(0x2000);
        cpu.regs.esp = 0x2000;
        let result = cpu.iret(&mut mem);
        assert!(result.is_ok(), "IRET should succeed, got {:?}", result);
        assert!(cpu.interrupts_enabled, "IF should be enabled after IRET");
        assert_eq!(
            cpu.interrupt_inhibit, 0,
            "IRET must NOT set interrupt_inhibit (only STI/MOV SS/POP SS do)"
        );
    }
    /// Regression: IRETD with IF=1 must NOT set interrupt_inhibit.
    /// Only STI, MOV SS, POP SS create an interrupt shadow (Intel SDM 6.8.3).
    #[test]
    fn test_iretd_does_not_set_interrupt_inhibit() {
        let _serial = crate::test_sync::serial_guard();
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.ss = 0x0000;
        cpu.regs.set_sp(0x2000);
        cpu.regs.esp = 0x2000;
        cpu.interrupts_enabled = false;
        cpu.interrupt_inhibit = 0;
        let base = 0x2000usize;
        mem.write_u32(base, 0x0000_0100);
        mem.write_u32(base + 4, 0x0000_0050);
        mem.write_u32(base + 8, 0x0000_0202);
        cpu.regs.set_sp(0x2000);
        cpu.regs.esp = 0x2000;
        let result = cpu.execute_iretd(&mut mem);
        assert!(result.is_ok(), "IRETD should succeed, got {:?}", result);
        assert!(cpu.interrupts_enabled, "IF should be enabled after IRETD");
        assert_eq!(
            cpu.interrupt_inhibit, 0,
            "IRETD must NOT set interrupt_inhibit (only STI/MOV SS/POP SS do)"
        );
    }
    /// should_nullify_segment: data segment with DPL < new_cpl is nullified
    #[test]
    fn test_should_nullify_data_segment_dpl_lt_cpl() {
        let cache = SegmentCache {
            dpl: 0,
            is_code: false,
            is_conforming: false,
            ..Default::default()
        };
        assert!(Cpu::should_nullify_segment(0x10, &cache, 3));
    }
    /// should_nullify_segment: data segment with DPL >= new_cpl is preserved
    #[test]
    fn test_should_nullify_data_segment_dpl_ge_cpl_preserved() {
        let cache = SegmentCache {
            dpl: 3,
            is_code: false,
            is_conforming: false,
            ..Default::default()
        };
        assert!(!Cpu::should_nullify_segment(0x1B, &cache, 3));
    }
    /// should_nullify_segment: non-conforming code segment with DPL < new_cpl is nullified
    #[test]
    fn test_should_nullify_nonconforming_code_dpl_lt_cpl() {
        let cache = SegmentCache {
            dpl: 0,
            is_code: true,
            is_conforming: false,
            ..Default::default()
        };
        assert!(Cpu::should_nullify_segment(0x08, &cache, 3));
    }
    /// should_nullify_segment: conforming code segment is NEVER nullified,
    /// even when DPL < new_cpl (Intel SDM, 386 PRM Section 15.5)
    #[test]
    fn test_should_nullify_conforming_code_exempt() {
        let cache = SegmentCache {
            dpl: 0,
            is_code: true,
            is_conforming: true,
            ..Default::default()
        };
        assert!(!Cpu::should_nullify_segment(0x08, &cache, 3));
    }
    /// should_nullify_segment: null selector (0) is never nullified
    #[test]
    fn test_should_nullify_null_selector_preserved() {
        let cache = SegmentCache {
            dpl: 0,
            is_code: false,
            is_conforming: false,
            ..Default::default()
        };
        assert!(!Cpu::should_nullify_segment(0, &cache, 3));
    }
    /// should_nullify_segment: RPL-based nullification — DPL < RPL triggers nullification
    /// even when DPL >= new_cpl
    #[test]
    fn test_should_nullify_rpl_based() {
        let cache = SegmentCache {
            dpl: 1,
            is_code: false,
            is_conforming: false,
            ..Default::default()
        };
        assert!(Cpu::should_nullify_segment(0x1B, &cache, 1));
    }
    /// 16-bit IRET: conforming code segment in DS is preserved on return to
    /// outer ring (Intel SDM 386 PRM 15.5)
    #[test]
    fn test_iret16_conforming_code_segment_not_nullified() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x1FFF;
        let cs0_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, cs0_desc);
        let cs3_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFA, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 16, cs3_desc);
        let ss3_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xF2, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 24, ss3_desc);
        cpu.regs.cs = 0x08;
        cpu.regs.cs_cache = SegmentCache::from_descriptor(&cs0_desc);
        cpu.regs.ss = 0x08;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.set_sp(0x5000);
        cpu.regs.esp = 0x5000;
        cpu.regs.ds = 0x08;
        cpu.regs.ds_cache = SegmentCache {
            dpl: 0,
            is_code: true,
            is_conforming: true,
            present: true,
            ..Default::default()
        };
        cpu.regs.es = 0x08;
        cpu.regs.es_cache = SegmentCache {
            dpl: 0,
            is_code: true,
            is_conforming: false,
            present: true,
            ..Default::default()
        };
        cpu.regs.fs = 0x10;
        cpu.regs.fs_cache = SegmentCache {
            dpl: 0,
            is_code: false,
            is_conforming: false,
            present: true,
            ..Default::default()
        };
        cpu.regs.gs = 0x1B;
        cpu.regs.gs_cache = SegmentCache {
            dpl: 3,
            is_code: false,
            is_conforming: false,
            present: true,
            ..Default::default()
        };
        mem.write_u16(0x5000, 0x0200);
        mem.write_u16(0x5002, 0x0013);
        mem.write_u16(0x5004, 0x0202);
        mem.write_u16(0x5006, 0x2000);
        mem.write_u16(0x5008, 0x001B);
        cpu.iret(&mut mem).unwrap();
        assert_eq!(cpu.regs.cpl, 3);
        assert_eq!(
            cpu.regs.ds, 0x08,
            "conforming code segment must not be nullified"
        );
        assert_eq!(
            cpu.regs.es, 0,
            "non-conforming code segment must be nullified"
        );
        assert_eq!(
            cpu.regs.fs, 0,
            "data segment with DPL < new_cpl must be nullified"
        );
        assert_eq!(
            cpu.regs.gs, 0x1B,
            "data segment with DPL >= new_cpl must be preserved"
        );
    }
    /// 32-bit IRETD: conforming code segment in DS is preserved on return to
    /// outer ring (Intel SDM 386 PRM 15.5)
    #[test]
    fn test_iretd_conforming_code_segment_not_nullified() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x1FFF;
        let cs0_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x40, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, cs0_desc);
        let cs3_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFA, 0x40, 0x00];
        write_descriptor(&mut mem, 0x1000 + 16, cs3_desc);
        let ss3_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xF2, 0x40, 0x00];
        write_descriptor(&mut mem, 0x1000 + 24, ss3_desc);
        cpu.regs.cs = 0x08;
        cpu.regs.cs_cache = SegmentCache::from_descriptor(&cs0_desc);
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.ss_cache.present = true;
        cpu.regs.esp = 0x3000;
        cpu.regs.flags = crate::Flags::RESERVED1;
        cpu.regs.ds = 0x08;
        cpu.regs.ds_cache = SegmentCache {
            dpl: 0,
            is_code: true,
            is_conforming: true,
            present: true,
            ..Default::default()
        };
        cpu.regs.es = 0x08;
        cpu.regs.es_cache = SegmentCache {
            dpl: 0,
            is_code: true,
            is_conforming: false,
            present: true,
            ..Default::default()
        };
        cpu.regs.fs = 0x1B;
        cpu.regs.fs_cache = SegmentCache {
            dpl: 3,
            is_code: false,
            is_conforming: false,
            present: true,
            ..Default::default()
        };
        cpu.regs.gs = 0x08;
        cpu.regs.gs_cache = SegmentCache {
            dpl: 0,
            is_code: false,
            is_conforming: false,
            present: true,
            ..Default::default()
        };
        mem.write_u32(0x3000, 0x0400);
        mem.write_u32(0x3004, 0x0013);
        mem.write_u32(0x3008, 0x0002);
        mem.write_u32(0x300C, 0x8000);
        mem.write_u32(0x3010, 0x001B);
        cpu.execute_iretd(&mut mem).unwrap();
        assert_eq!(cpu.regs.cpl, 3);
        assert_eq!(
            cpu.regs.ds, 0x08,
            "conforming code segment must not be nullified"
        );
        assert_eq!(
            cpu.regs.es, 0,
            "non-conforming code segment must be nullified"
        );
        assert_eq!(
            cpu.regs.fs, 0x1B,
            "data segment with DPL >= new_cpl must be preserved"
        );
        assert_eq!(
            cpu.regs.gs, 0,
            "data segment with DPL < new_cpl must be nullified"
        );
    }
    /// should_nullify_segment: RPL-based nullification in a full 16-bit IRET
    #[test]
    fn test_iret16_rpl_based_nullification() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x1FFF;
        let cs0_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, cs0_desc);
        let cs1_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xBA, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 16, cs1_desc);
        let ss1_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xB2, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 24, ss1_desc);
        cpu.regs.cs = 0x08;
        cpu.regs.cs_cache = SegmentCache::from_descriptor(&cs0_desc);
        cpu.regs.ss = 0x08;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF);
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.set_sp(0x5000);
        cpu.regs.esp = 0x5000;
        cpu.regs.ds = 0x1B;
        cpu.regs.ds_cache = SegmentCache {
            dpl: 1,
            is_code: false,
            is_conforming: false,
            present: true,
            ..Default::default()
        };
        cpu.regs.es = 0x19;
        cpu.regs.es_cache = SegmentCache {
            dpl: 1,
            is_code: false,
            is_conforming: false,
            present: true,
            ..Default::default()
        };
        cpu.regs.fs = 0;
        cpu.regs.gs = 0;
        mem.write_u16(0x5000, 0x0200);
        mem.write_u16(0x5002, 0x0011);
        mem.write_u16(0x5004, 0x0202);
        mem.write_u16(0x5006, 0x2000);
        mem.write_u16(0x5008, 0x0019);
        cpu.iret(&mut mem).unwrap();
        assert_eq!(cpu.regs.cpl, 1);
        assert_eq!(cpu.regs.ds, 0, "segment with DPL < RPL must be nullified");
        assert_eq!(
            cpu.regs.es, 0x19,
            "segment with DPL >= RPL and DPL >= new_cpl must be preserved"
        );
    }
    /// V86SwitchBlocked must restore ESP to its pre-IRETD value so the caller
    /// sees no side-effects on the CPU register state.
    #[test]
    fn test_iretd_v86_switch_blocked_restores_esp() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x100000);
        cpu.regs.cr0 = 1;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0FFF;
        let cs3_desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFA, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, cs3_desc);
        cpu.regs.cs = 0x0B;
        cpu.regs.cs_cache = SegmentCache::from_descriptor(&cs3_desc);
        cpu.regs.cpl = 3;
        cpu.regs.ss_cache.big = true;
        cpu.regs.ss_cache.base = 0;
        cpu.regs.ss_cache.set_limit(0xFFFF_FFFF);
        cpu.regs.ss_cache.is_writable = true;
        cpu.regs.ss_cache.present = true;
        let original_esp: u32 = 0x4000;
        cpu.regs.esp = original_esp;
        cpu.regs.flags = crate::Flags::RESERVED1;
        mem.write_u32(0x4000, 0x0400);
        mem.write_u32(0x4004, 0x000B);
        mem.write_u32(0x4008, 0x0002_0002);
        mem.write_u32(0x400C, 0x1000);
        mem.write_u32(0x4010, 0x2000);
        mem.write_u32(0x4014, 0x3000);
        mem.write_u32(0x4018, 0x4000);
        mem.write_u32(0x401C, 0x5000);
        mem.write_u32(0x4020, 0x6000);
        let err = cpu.execute_iretd(&mut mem).unwrap_err();
        match &err {
            CpuError::V86SwitchBlocked(frame) => {
                assert_eq!(frame.eip, 0x0400);
                assert_eq!(frame.esp, 0x1000);
            }
            other => panic!("Expected V86SwitchBlocked, got {:?}", other),
        }
        assert_eq!(
            cpu.regs.esp, original_esp,
            "ESP must be restored to pre-IRETD value after V86SwitchBlocked"
        );
    }
    #[test]
    fn test_ret_guard_should_log_disabled() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(false);
        assert!(!ret_guard_should_log());
    }
    #[test]
    fn test_ret_guard_should_log_enabled() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(true);
        RET_GUARD_LOG_COUNT.store(0, Ordering::Relaxed);
        assert!(ret_guard_should_log());
        Cpu::set_ret_guard_for_tests(false);
    }
    #[test]
    fn test_ret_guard_write_dump_disabled() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(false);
        ret_guard_write_dump("test content");
    }
    #[test]
    fn test_ret_guard_write_dump_enabled() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(true);
        RET_GUARD_DUMP_COUNT.store(0, Ordering::Relaxed);
        ret_guard_write_dump("test dump content");
        let path = "tmp/ret_guard_dump_0.txt";
        assert!(std::path::Path::new(path).exists());
        let _ = std::fs::remove_file(path);
        Cpu::set_ret_guard_for_tests(false);
    }
    #[test]
    fn test_ret_guard_log_max_default() {
        let _serial = crate::test_sync::serial_guard();
        let max = ret_guard_log_max();
        assert_eq!(max, 200);
    }
    #[test]
    fn test_ret_guard_peek_disabled() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(false);
        let cpu = Cpu::new();
        assert!(cpu.ret_guard_peek().is_none());
    }
    #[test]
    fn test_ret_guard_peek_enabled() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(true);
        let mut cpu = Cpu::new();
        cpu.ret_guard_push(RetGuardEntry {
            kind: RetGuardKind::Near,
            cs: 0x1000,
            ip: 0x100,
            ss: 0x2000,
            sp: 0xFFF0,
            tag: 0,
            caller_cs: 0x1000,
            caller_ip: 0x00F0,
        });
        let entry = cpu.ret_guard_peek().unwrap();
        assert_eq!(entry.kind, RetGuardKind::Near);
        assert_eq!(entry.cs, 0x1000);
        Cpu::set_ret_guard_for_tests(false);
    }
    #[test]
    fn test_ret_guard_push_interrupt() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(true);
        let mut cpu = Cpu::new();
        cpu.regs.cs = 0x1000;
        cpu.regs.set_ip(0x0200);
        cpu.regs.ss = 0x2000;
        cpu.regs.esp = 0xFFF0;
        cpu.ret_guard_push_interrupt(0x21);
        let entry = cpu.ret_guard_peek().unwrap();
        assert_eq!(entry.kind, RetGuardKind::Iret);
        assert_eq!(entry.tag, 0x21);
        Cpu::set_ret_guard_for_tests(false);
    }
    #[test]
    fn test_ret_guard_pop_iret_hle() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(true);
        let mut cpu = Cpu::new();
        cpu.ret_guard_push(RetGuardEntry {
            kind: RetGuardKind::Iret,
            cs: 0x1000,
            ip: 0x100,
            ss: 0x2000,
            sp: 0xFFF0,
            tag: 0x10,
            caller_cs: 0x1000,
            caller_ip: 0x00F0,
        });
        cpu.ret_guard_pop_iret_hle();
        assert!(cpu.ret_guard_peek().is_none());
        Cpu::set_ret_guard_for_tests(false);
    }
    #[test]
    fn test_ret_guard_pop_retf_hle() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(true);
        let mut cpu = Cpu::new();
        cpu.ret_guard_push(RetGuardEntry {
            kind: RetGuardKind::Far,
            cs: 0x1000,
            ip: 0x100,
            ss: 0x2000,
            sp: 0xFFF0,
            tag: 0,
            caller_cs: 0x1000,
            caller_ip: 0x00F0,
        });
        cpu.ret_guard_pop_retf_hle();
        assert!(cpu.ret_guard_peek().is_none());
        Cpu::set_ret_guard_for_tests(false);
    }
    #[test]
    fn test_ret_guard_max_depth_eviction() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(true);
        let mut cpu = Cpu::new();
        for i in 0..1025u32 {
            cpu.ret_guard_push(RetGuardEntry {
                kind: RetGuardKind::Near,
                cs: 0x1000,
                ip: i,
                ss: 0x2000,
                sp: 0xFFF0,
                tag: 0,
                caller_cs: 0x1000,
                caller_ip: 0x00F0,
            });
        }
        assert_eq!(cpu.ret_guard_depth(), 1024);
        Cpu::set_ret_guard_for_tests(false);
    }
    #[test]
    fn test_segment_cache_limit_all_regs() {
        use iced_x86::Register;
        let cpu = Cpu::new();
        assert_eq!(cpu.segment_cache_limit(Register::CS), 0xFFFF);
        assert_eq!(cpu.segment_cache_limit(Register::DS), 0xFFFF);
        assert_eq!(cpu.segment_cache_limit(Register::ES), 0xFFFF);
        assert_eq!(cpu.segment_cache_limit(Register::FS), 0xFFFF);
        assert_eq!(cpu.segment_cache_limit(Register::GS), 0xFFFF);
        assert_eq!(cpu.segment_cache_limit(Register::SS), 0xFFFF);
        assert_eq!(cpu.segment_cache_limit(Register::EAX), 0xFFFF);
    }
    #[test]
    fn test_eflags_high_mask_386() {
        let mut cpu = Cpu::new();
        cpu.set_cpu_type(CpuType::I80386);
        let mask = cpu.eflags_high_mask();
        assert_eq!(mask & 0x0004, 0);
        assert_eq!(mask & 0x0020, 0);
    }
    #[test]
    fn test_eflags_high_mask_486() {
        let mut cpu = Cpu::new();
        cpu.set_cpu_type(CpuType::I80486);
        let mask = cpu.eflags_high_mask();
        assert_ne!(mask & 0x0004, 0);
        assert_eq!(mask & 0x0020, 0);
    }
    #[test]
    fn test_eflags_high_mask_pentium() {
        let mut cpu = Cpu::new();
        cpu.set_cpu_type(CpuType::Pentium);
        let mask = cpu.eflags_high_mask();
        assert_eq!(mask, 0x003F);
    }
    #[test]
    fn test_apply_popf_privilege_real_mode() {
        let cpu = Cpu::new();
        let flags = crate::Flags::from_bits_truncate(0x0202);
        let result = cpu.apply_popf_privilege(flags);
        assert_eq!(result, flags);
    }
    #[test]
    fn test_apply_popf_privilege_cpl0() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 |= 1;
        cpu.regs.cpl = 0;
        cpu.regs.cs_cache.present = true;
        cpu.regs.cs_cache.is_code = true;
        let flags = crate::Flags::from_bits_truncate(0x3202);
        let result = cpu.apply_popf_privilege(flags);
        assert_eq!(result, flags);
    }
    #[test]
    fn test_apply_popf_privilege_cpl3_preserves_iopl_and_if() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 |= 1;
        cpu.regs.cpl = 3;
        cpu.regs.cs_cache.present = true;
        cpu.regs.cs_cache.is_code = true;
        cpu.regs.flags = crate::Flags::from_bits_truncate(0x0202);
        let new_flags = crate::Flags::from_bits_truncate(0x3002);
        let result = cpu.apply_popf_privilege(new_flags);
        assert!(result.contains(crate::Flags::INTERRUPT));
        assert_eq!(result.bits() & 0x3000, 0);
    }
    #[test]
    fn test_set_hle_break_selectors() {
        let mut cpu = Cpu::new();
        let selectors = [0x0008, 0x0010, 0x0018];
        cpu.set_hle_break_selectors(&selectors);
        assert_eq!(cpu.hle_break_count, 3);
        assert_eq!(cpu.hle_break_selectors[0], 0x0008);
        assert_eq!(cpu.hle_break_selectors[1], 0x0010);
        assert_eq!(cpu.hle_break_selectors[2], 0x0018);
    }
    #[test]
    fn test_add_hle_break_selector_new() {
        let mut cpu = Cpu::new();
        cpu.add_hle_break_selector(0x0008);
        assert_eq!(cpu.hle_break_count, 1);
        assert_eq!(cpu.hle_break_selectors[0], 0x0008);
    }
    #[test]
    fn test_add_hle_break_selector_duplicate() {
        let mut cpu = Cpu::new();
        cpu.add_hle_break_selector(0x0008);
        cpu.add_hle_break_selector(0x0008);
        assert_eq!(cpu.hle_break_count, 1);
    }
    #[test]
    fn test_get_segment_cache_pm_unknown_selector() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 |= 1;
        cpu.regs.cs = 0x0008;
        cpu.regs.ds = 0x0010;
        cpu.regs.es = 0x0010;
        cpu.regs.fs = 0x0000;
        cpu.regs.gs = 0x0000;
        cpu.regs.ss = 0x0018;
        let cache = cpu.get_segment_cache(0x0020);
        assert_eq!(cache.base, 0x0020_u32 << 4);
    }
    #[test]
    fn test_cpu_set_get_type_coverage() {
        let mut cpu = Cpu::new();
        cpu.set_cpu_type(CpuType::Pentium);
        assert_eq!(cpu.get_cpu_type(), CpuType::Pentium);
        cpu.set_cpu_type(CpuType::I80486);
        assert_eq!(cpu.get_cpu_type(), CpuType::I80486);
    }
    #[test]
    fn test_ret_guard_push_interrupt_pm_big_ss() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(true);
        let mut cpu = Cpu::new();
        cpu.regs.cr0 |= 1;
        cpu.regs.cs_cache.present = true;
        cpu.regs.cs_cache.is_code = true;
        cpu.regs.ss_cache.big = true;
        cpu.regs.esp = 0x0001_FFF0;
        cpu.regs.cs = 0x0008;
        cpu.regs.set_ip(0x0200);
        cpu.ret_guard_push_interrupt(0x21);
        let entry = cpu.ret_guard_peek().unwrap();
        assert_eq!(entry.sp, 0x0001_FFF0);
        Cpu::set_ret_guard_for_tests(false);
    }
    #[test]
    fn test_fetch_linear_16bit() {
        let mut cpu = Cpu::new();
        cpu.cs_bits = 16;
        cpu.regs.cs_cache.base = 0x10000;
        cpu.regs.set_ip(0x0100);
        assert_eq!(cpu.fetch_linear(), 0x10100);
    }
    #[test]
    fn test_fetch_linear_32bit() {
        let mut cpu = Cpu::new();
        cpu.cs_bits = 32;
        cpu.regs.cs_cache.base = 0x0;
        cpu.regs.eip = 0x0040_1000;
        assert_eq!(cpu.fetch_linear(), 0x0040_1000);
    }
    #[test]
    fn test_try_push16_32bit_stack() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 |= 1;
        cpu.regs.ss = 0x0018;
        cpu.regs.ss_cache = SegmentCache {
            base: 0,
            limit: 0xFFFF,
            cached_effective_limit: 0xFFFF,
            access: 0x93,
            flags: 0x40,
            present: true,
            dpl: 0,
            is_code: false,
            is_conforming: false,
            is_readable: true,
            is_writable: true,
            granularity: false,
            big: true,
        };
        cpu.regs.cs_cache.present = true;
        cpu.regs.cs_cache.is_code = true;
        cpu.regs.esp = 0x1000;
        let mut mem = TestMemory::new(0x10000);
        let result = cpu.try_push16(&mut mem, 0x1234);
        assert!(result.is_ok());
        assert_eq!(cpu.regs.esp, 0x0FFE);
        assert_eq!(mem.read_u16(0x0FFE), 0x1234);
    }
    #[test]
    fn test_try_push32_32bit_stack() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 |= 1;
        cpu.regs.ss = 0x0018;
        cpu.regs.ss_cache = SegmentCache {
            base: 0,
            limit: 0xFFFF,
            cached_effective_limit: 0xFFFF,
            access: 0x93,
            flags: 0x40,
            present: true,
            dpl: 0,
            is_code: false,
            is_conforming: false,
            is_readable: true,
            is_writable: true,
            granularity: false,
            big: true,
        };
        cpu.regs.cs_cache.present = true;
        cpu.regs.cs_cache.is_code = true;
        cpu.regs.esp = 0x1000;
        let mut mem = TestMemory::new(0x10000);
        let result = cpu.try_push32(&mut mem, 0xDEAD_BEEF);
        assert!(result.is_ok());
        assert_eq!(cpu.regs.esp, 0x0FFC);
        assert_eq!(mem.read_u32(0x0FFC), 0xDEAD_BEEF);
    }
    #[test]
    fn test_try_push16_16bit_stack() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 |= 1;
        cpu.regs.ss = 0x0018;
        cpu.regs.ss_cache = SegmentCache {
            base: 0,
            limit: 0xFFFF,
            cached_effective_limit: 0xFFFF,
            access: 0x93,
            flags: 0,
            present: true,
            dpl: 0,
            is_code: false,
            is_conforming: false,
            is_readable: true,
            is_writable: true,
            granularity: false,
            big: false,
        };
        cpu.regs.cs_cache.present = true;
        cpu.regs.cs_cache.is_code = true;
        cpu.regs.esp = 0x1000;
        let mut mem = TestMemory::new(0x10000);
        let result = cpu.try_push16(&mut mem, 0x5678);
        assert!(result.is_ok());
        assert_eq!(cpu.regs.sp(), 0x0FFE);
    }
    #[test]
    fn test_try_push32_16bit_stack() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 |= 1;
        cpu.regs.ss = 0x0018;
        cpu.regs.ss_cache = SegmentCache {
            base: 0,
            limit: 0xFFFF,
            cached_effective_limit: 0xFFFF,
            access: 0x93,
            flags: 0,
            present: true,
            dpl: 0,
            is_code: false,
            is_conforming: false,
            is_readable: true,
            is_writable: true,
            granularity: false,
            big: false,
        };
        cpu.regs.cs_cache.present = true;
        cpu.regs.cs_cache.is_code = true;
        cpu.regs.esp = 0x1000;
        let mut mem = TestMemory::new(0x10000);
        let result = cpu.try_push32(&mut mem, 0xCAFE_BABE);
        assert!(result.is_ok());
        assert_eq!(cpu.regs.sp(), 0x0FFC);
    }
    #[test]
    fn test_try_pop16_32bit_stack() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 |= 1;
        cpu.regs.ss = 0x0018;
        cpu.regs.ss_cache = SegmentCache {
            base: 0,
            limit: 0xFFFF,
            cached_effective_limit: 0xFFFF,
            access: 0x93,
            flags: 0x40,
            present: true,
            dpl: 0,
            is_code: false,
            is_conforming: false,
            is_readable: true,
            is_writable: true,
            granularity: false,
            big: true,
        };
        cpu.regs.cs_cache.present = true;
        cpu.regs.cs_cache.is_code = true;
        cpu.regs.esp = 0x0FFE;
        let mut mem = TestMemory::new(0x10000);
        mem.write_u16(0x0FFE, 0xABCD);
        let result = cpu.try_pop16(&mut mem);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0xABCD);
        assert_eq!(cpu.regs.esp, 0x1000);
    }
    #[test]
    fn test_try_pop32_32bit_stack() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 |= 1;
        cpu.regs.ss = 0x0018;
        cpu.regs.ss_cache = SegmentCache {
            base: 0,
            limit: 0xFFFF,
            cached_effective_limit: 0xFFFF,
            access: 0x93,
            flags: 0x40,
            present: true,
            dpl: 0,
            is_code: false,
            is_conforming: false,
            is_readable: true,
            is_writable: true,
            granularity: false,
            big: true,
        };
        cpu.regs.cs_cache.present = true;
        cpu.regs.cs_cache.is_code = true;
        cpu.regs.esp = 0x0FFC;
        let mut mem = TestMemory::new(0x10000);
        mem.write_u32(0x0FFC, 0x1234_5678);
        let result = cpu.try_pop32(&mut mem);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0x1234_5678);
        assert_eq!(cpu.regs.esp, 0x1000);
    }
    #[test]
    fn test_translate_paging_checked_tlb_hit_read() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 |= 1 | 0x8000_0000;
        cpu.regs.cs_cache.present = true;
        cpu.regs.cs_cache.is_code = true;
        cpu.regs.cpl = 0;
        let mut mem = TestMemory::new(0x10000);
        mem.insert_tlb_dirty(1, 2, true, false, true);
        let result = cpu.translate_paging_checked(&mut mem, 0x1000, PageAccess::Read);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0x2000);
    }
    #[test]
    fn test_translate_paging_checked_tlb_write_not_writable() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 |= 1 | 0x8000_0000 | 0x0001_0000;
        cpu.regs.cs_cache.present = true;
        cpu.regs.cs_cache.is_code = true;
        cpu.regs.cpl = 0;
        let mut mem = TestMemory::new(0x10000);
        mem.insert_tlb_dirty(1, 2, false, false, true);
        let result = cpu.translate_paging_checked(&mut mem, 0x1000, PageAccess::Write);
        assert!(result.is_err());
    }
    #[test]
    fn test_translate_paging_checked_tlb_user_not_allowed() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 |= 1 | 0x8000_0000;
        cpu.regs.cs_cache.present = true;
        cpu.regs.cs_cache.is_code = true;
        cpu.regs.cpl = 3;
        let mut mem = TestMemory::new(0x10000);
        mem.insert_tlb_dirty(1, 2, true, false, true);
        let result = cpu.translate_paging_checked(&mut mem, 0x1000, PageAccess::Read);
        assert!(result.is_err());
    }
    #[test]
    fn test_translate_system_linear_read_checked_uses_supervisor_semantics_at_cpl3() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x10000;
        cpu.regs.cpl = 3;
        let mut mem = TestMemory::new(0x30000);
        let linear = 0x8003_656D;
        let physical_base = 0x20000;
        let page_table = 0x11000;
        map_page(
            &mut mem,
            cpu.regs.cr3 as usize,
            page_table,
            linear & !0x0FFF,
            physical_base,
            0x02,
        );
        let normal = cpu.translate_paging_checked(&mut mem, linear, PageAccess::Read);
        assert!(
            normal.is_err(),
            "normal CPL-aware data reads must reject supervisor-only pages at CPL3"
        );
        let physical = cpu
            .translate_system_linear_read_checked(&mut mem, linear)
            .expect("implicit CPU table read should ignore U/S at CPL3");
        assert_eq!(physical, physical_base + 0x56D);
        let pde_addr = cpu.regs.cr3 as usize + ((linear >> 22) as usize) * 4;
        let pte_addr = page_table + (((linear >> 12) & 0x03FF) as usize) * 4;
        assert_ne!(mem.read_u32(pde_addr) & 0x20, 0, "PDE accessed bit");
        assert_ne!(mem.read_u32(pte_addr) & 0x20, 0, "PTE accessed bit");
        assert_eq!(mem.read_u32(pte_addr) & 0x40, 0, "read must not set dirty");
    }
    #[test]
    fn test_translate_system_linear_read_checked_unmapped_fault_is_supervisor_read() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cr3 = 0x10000;
        cpu.regs.cpl = 3;
        let mut mem = TestMemory::new(0x20000);
        let linear = 0x8003_656D;
        let err = cpu
            .translate_system_linear_read_checked(&mut mem, linear)
            .expect_err("unmapped system read should return #PF information");
        assert_eq!(err.linear_address, linear);
        assert_eq!(
            err.error_code, 0,
            "missing IDT page is a supervisor read not-present fault"
        );
    }
    #[test]
    fn test_translate_system_linear_read_checked_tlb_hit_ignores_user_bit_at_cpl3() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cpl = 3;
        let mut mem = TestMemory::new(0x30000);
        let linear = 0x8003_656D;
        let linear_page = linear >> 12;
        mem.insert_tlb_dirty(linear_page, 0x20, true, false, false);
        let normal = cpu.translate_paging_checked(&mut mem, linear, PageAccess::Read);
        assert!(
            normal.is_err(),
            "normal CPL-aware TLB reads must reject supervisor-only pages at CPL3"
        );
        let physical = cpu
            .translate_system_linear_read_checked(&mut mem, linear)
            .expect("implicit CPU table read should accept supervisor-only TLB entries");
        assert_eq!(physical, 0x2056D);
    }
    #[test]
    fn test_add_hle_break_selector_full() {
        let mut cpu = Cpu::new();
        for i in 0..32u16 {
            cpu.add_hle_break_selector(i + 1);
        }
        assert_eq!(cpu.hle_break_count, 32);
        cpu.add_hle_break_selector(0x9999);
        assert_eq!(cpu.hle_break_count, 32);
    }
    #[test]
    fn test_mark_iret_and_retf() {
        let mut cpu = Cpu::new();
        assert!(!cpu.last_iret());
        assert!(!cpu.last_retf());
        cpu.mark_iret();
        assert!(cpu.last_iret());
        cpu.mark_retf();
        assert!(cpu.last_retf());
        assert!(cpu.take_last_iret());
        assert!(!cpu.last_iret());
        assert!(cpu.take_last_retf());
        assert!(!cpu.last_retf());
    }
    fn with_trace_subscriber<F: FnOnce()>(f: F) {
        use tracing_subscriber::fmt;
        use tracing_subscriber::prelude::*;
        let subscriber = fmt::layer()
            .with_writer(std::io::sink)
            .with_filter(tracing_subscriber::filter::LevelFilter::TRACE);
        let _guard = tracing_subscriber::registry()
            .with(subscriber)
            .set_default();
        f();
    }
    #[test]
    fn test_get_segment_cache_pm_unknown_with_tracing() {
        with_trace_subscriber(|| {
            let mut cpu = Cpu::new();
            cpu.regs.cr0 |= 1;
            cpu.regs.cs = 0x0008;
            cpu.regs.ds = 0x0010;
            cpu.regs.es = 0x0010;
            cpu.regs.fs = 0x0000;
            cpu.regs.gs = 0x0000;
            cpu.regs.ss = 0x0018;
            let _cache = cpu.get_segment_cache(0x0020);
        });
    }
    #[test]
    fn test_translate_address_full_checked_limit_exceeded_with_tracing() {
        with_trace_subscriber(|| {
            let mut cpu = Cpu::new();
            cpu.regs.cr0 |= 1;
            cpu.regs.ds = 0x0010;
            cpu.regs.ds_cache = SegmentCache {
                base: 0,
                limit: 0x00FF,
                cached_effective_limit: 0x00FF,
                access: 0x93,
                flags: 0,
                present: true,
                dpl: 0,
                is_code: false,
                is_conforming: false,
                is_readable: true,
                is_writable: true,
                granularity: false,
                big: false,
            };
            cpu.regs.cs_cache.present = true;
            cpu.regs.cs_cache.is_code = true;
            let mut mem = TestMemory::new(0x10000);
            let result =
                cpu.translate_address_full_checked(&mut mem, 0x0010, 0x200, PageAccess::Read);
            assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
        });
    }
    #[test]
    fn test_translate_address_full_checked_ss_limit_exceeded_with_tracing() {
        with_trace_subscriber(|| {
            let mut cpu = Cpu::new();
            cpu.regs.cr0 |= 1;
            cpu.regs.ss = 0x0018;
            cpu.regs.ss_cache = SegmentCache {
                base: 0,
                limit: 0x00FF,
                cached_effective_limit: 0x00FF,
                access: 0x93,
                flags: 0,
                present: true,
                dpl: 0,
                is_code: false,
                is_conforming: false,
                is_readable: true,
                is_writable: true,
                granularity: false,
                big: false,
            };
            cpu.regs.cs_cache.present = true;
            cpu.regs.cs_cache.is_code = true;
            let mut mem = TestMemory::new(0x10000);
            let result =
                cpu.translate_address_full_checked(&mut mem, 0x0018, 0x200, PageAccess::Read);
            assert!(matches!(result, Err(CpuError::StackFault)));
        });
    }
    #[test]
    fn test_load_cs_real_mode_with_tracing() {
        with_trace_subscriber(|| {
            let mut cpu = Cpu::new();
            let mut mem = TestMemory::new(0x10000);
            cpu.load_cs(&mut mem, 0x1234).unwrap();
            assert_eq!(cpu.regs.cs, 0x1234);
        });
    }
    #[test]
    fn test_add_hle_break_selector_full_with_tracing() {
        with_trace_subscriber(|| {
            let mut cpu = Cpu::new();
            for i in 0..32u16 {
                cpu.add_hle_break_selector(i + 1);
            }
            cpu.add_hle_break_selector(0x9999);
        });
    }
    #[test]
    fn test_load_ss_pm_invalid_descriptor_with_tracing() {
        with_trace_subscriber(|| {
            let mut cpu = Cpu::new();
            cpu.regs.cr0 |= 1;
            cpu.regs.cs_cache.present = true;
            cpu.regs.cs_cache.is_code = true;
            cpu.regs.cpl = 0;
            cpu.regs.gdtr.base = 0;
            cpu.regs.gdtr.limit = 0xFFFF;
            let mut mem = TestMemory::new(0x10000);
            let idx = 2 * 8;
            mem.write_u8(idx + 5, 0x9A);
            mem.write_u8(idx + 6, 0x00);
            let result = cpu.load_ss(&mut mem, 0x0010);
            assert!(result.is_err());
        });
    }
    #[test]
    fn test_load_ss_pm_dpl_mismatch_with_tracing() {
        with_trace_subscriber(|| {
            let mut cpu = Cpu::new();
            cpu.regs.cr0 |= 1;
            cpu.regs.cs_cache.present = true;
            cpu.regs.cs_cache.is_code = true;
            cpu.regs.cpl = 0;
            cpu.regs.gdtr.base = 0;
            cpu.regs.gdtr.limit = 0xFFFF;
            let mut mem = TestMemory::new(0x10000);
            let idx = 2 * 8;
            mem.write_u8(idx + 5, 0xF3);
            mem.write_u8(idx + 6, 0x00);
            let result = cpu.load_ss(&mut mem, 0x0010);
            assert!(result.is_err());
        });
    }
    #[test]
    fn test_load_cs_pm_no_descriptor_with_tracing() {
        with_trace_subscriber(|| {
            let mut cpu = Cpu::new();
            cpu.regs.cr0 |= 1;
            cpu.regs.cs_cache.present = true;
            cpu.regs.cs_cache.is_code = true;
            cpu.regs.cpl = 0;
            cpu.regs.gdtr.base = 0;
            cpu.regs.gdtr.limit = 0x0F;
            let mut mem = TestMemory::new(0x10000);
            let result = cpu.load_cs(&mut mem, 0x0010);
            assert!(result.is_err());
        });
    }
    #[test]
    fn test_try_pop16_16bit_stack_normal() {
        let mut cpu = Cpu::new();
        cpu.regs.ss = 0x0000;
        cpu.regs.esp = 0x1000;
        let mut mem = TestMemory::new(0x10000);
        mem.write_u16(0x1000, 0xBEEF);
        let result = cpu.try_pop16(&mut mem);
        assert_eq!(result.unwrap(), 0xBEEF);
        assert_eq!(cpu.regs.sp(), 0x1002);
    }
    #[test]
    fn test_try_pop32_16bit_stack_normal() {
        let mut cpu = Cpu::new();
        cpu.regs.ss = 0x0000;
        cpu.regs.esp = 0x1000;
        let mut mem = TestMemory::new(0x10000);
        mem.write_u32(0x1000, 0xDEAD_BEEF);
        let result = cpu.try_pop32(&mut mem);
        assert_eq!(result.unwrap(), 0xDEAD_BEEF);
        assert_eq!(cpu.regs.sp(), 0x1004);
    }
    #[test]
    fn test_try_pop16_sp_wrap_0xffff() {
        let mut cpu = Cpu::new();
        cpu.cpu_type = CpuType::I8086;
        cpu.regs.ss = 0x0000;
        cpu.regs.esp = 0xFFFF;
        let mut mem = TestMemory::new(0x10000);
        mem.write_u8(0xFFFF, 0x34);
        mem.write_u8(0x0000, 0x12);
        let result = cpu.try_pop16(&mut mem);
        assert_eq!(result.unwrap(), 0x1234);
    }
    #[test]
    fn test_try_push16_sp_wrap_0x0001() {
        let mut cpu = Cpu::new();
        cpu.cpu_type = CpuType::I8086;
        cpu.regs.ss = 0x0000;
        cpu.regs.esp = 0x0001;
        let mut mem = TestMemory::new(0x10000);
        let result = cpu.try_push16(&mut mem, 0x5678);
        assert!(result.is_ok());
        assert_eq!(cpu.regs.sp(), 0xFFFF);
        let lo = mem.read_u8(0xFFFF);
        let hi = mem.read_u8(0x0000);
        assert_eq!(u16::from_le_bytes([lo, hi]), 0x5678);
    }
    #[test]
    fn test_translate_paging_walk_present_pde_pte() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 |= 1 | 0x8000_0000;
        cpu.regs.cr3 = 0x1000;
        cpu.regs.cs_cache.present = true;
        cpu.regs.cs_cache.is_code = true;
        cpu.regs.cpl = 0;
        let mut mem = TestMemory::new(0x10000);
        mem.write_u32(0x1000, 0x2003);
        mem.write_u32(0x2000, 0x3003);
        let result = cpu.translate_paging_checked(&mut mem, 0x0000_0100, PageAccess::Read);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0x3100);
    }
    #[test]
    fn test_translate_paging_pde_not_present() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 |= 1 | 0x8000_0000;
        cpu.regs.cr3 = 0x1000;
        cpu.regs.cs_cache.present = true;
        cpu.regs.cs_cache.is_code = true;
        cpu.regs.cpl = 0;
        let mut mem = TestMemory::new(0x10000);
        mem.write_u32(0x1000, 0x0000);
        let result = cpu.translate_paging_checked(&mut mem, 0x0000_0100, PageAccess::Read);
        assert!(result.is_err());
    }
    #[test]
    fn test_translate_paging_pte_not_present() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 |= 1 | 0x8000_0000;
        cpu.regs.cr3 = 0x1000;
        cpu.regs.cs_cache.present = true;
        cpu.regs.cs_cache.is_code = true;
        cpu.regs.cpl = 0;
        let mut mem = TestMemory::new(0x10000);
        mem.write_u32(0x1000, 0x2003);
        mem.write_u32(0x2000, 0x0000);
        let result = cpu.translate_paging_checked(&mut mem, 0x0000_0100, PageAccess::Read);
        assert!(result.is_err());
    }
    #[test]
    fn test_translate_paging_write_sets_dirty() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 |= 1 | 0x8000_0000;
        cpu.regs.cr3 = 0x1000;
        cpu.regs.cs_cache.present = true;
        cpu.regs.cs_cache.is_code = true;
        cpu.regs.cpl = 0;
        let mut mem = TestMemory::new(0x10000);
        mem.write_u32(0x1000, 0x2003);
        mem.write_u32(0x2000, 0x3003);
        let result = cpu.translate_paging_checked(&mut mem, 0x0000_0000, PageAccess::Write);
        assert!(result.is_ok());
        let pte = mem.read_u32(0x2000);
        assert_ne!(pte & 0x40, 0, "Dirty bit should be set after write");
    }
    #[test]
    fn test_translate_paging_tlb_write_to_clean_falls_through() {
        let mut cpu = Cpu::new();
        cpu.regs.cr0 |= 1 | 0x8000_0000;
        cpu.regs.cr3 = 0x1000;
        cpu.regs.cs_cache.present = true;
        cpu.regs.cs_cache.is_code = true;
        cpu.regs.cpl = 0;
        let mut mem = TestMemory::new(0x10000);
        mem.insert_tlb_dirty(0, 3, true, true, false);
        mem.write_u32(0x1000, 0x2003);
        mem.write_u32(0x2000, 0x3003);
        let result = cpu.translate_paging_checked(&mut mem, 0x0000_0000, PageAccess::Write);
        assert!(result.is_ok());
    }
    #[test]
    fn test_load_data_segment_checked_nonconforming_code_dpl_violation() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu
            .load_data_segment_checked(&mut mem, DataSegReg::DS, 0x0B)
            .unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
    }
    #[test]
    fn test_rejected_data_segment_load_does_not_set_descriptor_accessed_bit() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 3;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let err = cpu
            .load_data_segment_checked(&mut mem, DataSegReg::DS, 0x0B)
            .unwrap_err();
        assert!(matches!(err, CpuError::GeneralProtectionFault));
        assert_eq!(mem.read_u8(0x1000 + 8 + 5), 0x92);
    }
    #[test]
    fn test_load_data_segment_checked_conforming_code_dpl_gt_cpl_ok() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x0F;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFE, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.load_data_segment_checked(&mut mem, DataSegReg::DS, 0x08)
            .unwrap();
        assert_eq!(cpu.regs.ds, 0x08);
        assert!(cpu.regs.ds_cache.is_conforming);
    }
    #[test]
    fn test_ret_guard_pop_returns_none_when_disabled() {
        let _serial = crate::test_sync::serial_guard();
        Cpu::set_ret_guard_for_tests(false);
        let mut cpu = Cpu::new();
        assert!(cpu.ret_guard_pop(RetGuardKind::Near).is_none());
    }
    #[test]
    fn test_translate_address_full_checked_paging_page_fault() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x200000);
        cpu.regs.cr0 = 0x8000_0001;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x17;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x92, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        cpu.regs.cr3 = 0x100000;
        map_page(&mut mem, 0x100000, 0x101000, 0x1000, 0x1000, 0x02);
        cpu.load_data_segment(&mem, DataSegReg::DS, 0x08);
        let result =
            cpu.translate_address_full_checked(&mut mem, cpu.regs.ds, 0x0000, PageAccess::Read);
        assert!(result.is_err());
        match result.unwrap_err() {
            CpuError::PageFault(pf) => {
                assert_eq!(pf.linear_address, 0);
                assert_eq!(pf.error_code & 1, 0);
            }
            other => panic!("Expected PageFault, got {:?}", other),
        }
        assert!(mem.page_fault.is_some());
    }
    #[test]
    fn test_load_cs_unsupported_gate_types() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 |= 1;
        cpu.regs.cpl = 0;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x2F;
        let types_and_names: &[(u8, &str)] = &[
            (0x04, "16-bit call gate"),
            (0x05, "task gate"),
            (0x06, "16-bit interrupt gate"),
            (0x07, "16-bit trap gate"),
            (0x0E, "32-bit interrupt gate"),
            (0x0F, "32-bit trap gate"),
        ];
        for (i, &(desc_type, _name)) in types_and_names.iter().enumerate() {
            let selector = (i as u16 + 1) * 8;
            let gdt_offset = 0x1000 + (selector as usize);
            let access = 0x80 | desc_type;
            let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, access, 0x00, 0x00];
            write_descriptor(&mut mem, gdt_offset, desc);
            let result = cpu.load_cs(&mut mem, selector);
            assert!(
                result.is_err(),
                "Expected GPF for desc_type {:#x}",
                desc_type
            );
        }
    }
    #[test]
    fn test_load_data_segment_checked_non_conforming_code_dpl_mismatch() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 |= 1;
        cpu.regs.cpl = 3;
        cpu.regs.gdtr.base = 0x1000;
        cpu.regs.gdtr.limit = 0x17;
        let desc = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0x00, 0x00];
        write_descriptor(&mut mem, 0x1000 + 8, desc);
        let result = cpu.load_data_segment_checked(&mut mem, DataSegReg::DS, 0x08);
        assert!(matches!(result, Err(CpuError::GeneralProtectionFault)));
    }
    #[test]
    fn test_set_reset_vector_state_i386() {
        let mut cpu = Cpu::new();
        cpu.set_cpu_type(CpuType::I80386);
        cpu.set_reset_vector_state();
        assert_eq!(cpu.regs.cs, 0xF000);
        assert_eq!(cpu.regs.cs_cache.base, 0xFFFF_0000);
        assert_eq!(cpu.regs.cs_cache.limit, 0xFFFF);
        assert_eq!(cpu.regs.cs_cache.cached_effective_limit, 0xFFFF);
        assert!(cpu.regs.cs_cache.present);
        assert!(cpu.regs.cs_cache.is_code, "CS must be a code segment");
        assert!(cpu.regs.cs_cache.is_readable, "CS must be readable");
        assert!(
            !cpu.regs.cs_cache.is_writable,
            "CS must not be writable — code segments are R/X, never W"
        );
        assert_eq!(cpu.regs.eip, 0xFFF0);
        for (name, sel, cache) in [
            ("DS", cpu.regs.ds, &cpu.regs.ds_cache),
            ("ES", cpu.regs.es, &cpu.regs.es_cache),
            ("FS", cpu.regs.fs, &cpu.regs.fs_cache),
            ("GS", cpu.regs.gs, &cpu.regs.gs_cache),
            ("SS", cpu.regs.ss, &cpu.regs.ss_cache),
        ] {
            assert_eq!(sel, 0, "{name} selector must be 0 after reset");
            assert_eq!(cache.base, 0, "{name}.base must be 0 after reset");
            assert_eq!(
                cache.limit, 0xFFFF,
                "{name}.limit must be 0xFFFF after reset"
            );
            assert!(!cache.is_code, "{name} must not be a code segment");
        }
        assert_eq!(cpu.regs.edx, 0x0000_0300);
        assert_eq!(cpu.regs.eflags(), 0x0000_0002);
        assert_eq!(cpu.regs.cr0, 0x6000_0010);
        assert_eq!(cpu.regs.gdtr.base, 0);
        assert_eq!(cpu.regs.gdtr.limit, 0xFFFF);
        assert_eq!(cpu.regs.idtr.base, 0);
        assert_eq!(cpu.regs.idtr.limit, 0xFFFF);
        assert_eq!(cpu.regs.dr7, 0x0000_0400);
        assert!(!cpu.halted);
        assert_eq!(cpu.interrupt_inhibit, 0);
        assert_eq!(cpu.regs.eax, 0);
        assert_eq!(cpu.regs.ebx, 0);
        assert_eq!(cpu.regs.ecx, 0);
        assert_eq!(cpu.regs.esi, 0);
        assert_eq!(cpu.regs.edi, 0);
        assert_eq!(cpu.regs.ebp, 0);
        assert_eq!(cpu.regs.esp, 0);
        assert_eq!(cpu.cs_bits, 16);
    }
    /// Reset EDX must describe the model the guest actually runs on
    /// (MDD-BUG-CRUCIBLE-00354). Every non-Pentium model used to report the
    /// i486 family, so a 386 announced an instruction set it does not have.
    #[test]
    fn reset_edx_matches_every_cpu_identity() {
        for (cpu_type, expected, why) in [
            (
                CpuType::I80386,
                0x0000_0300u32,
                "80386 component id, family 3",
            ),
            (
                CpuType::I80486,
                0x0000_0401,
                "family 4, model 0, stepping 1",
            ),
            (
                CpuType::Pentium,
                0x0000_0525,
                "family 5, model 2, stepping 5",
            ),
            (
                CpuType::PentiumMmx,
                0x0000_0543,
                "family 5, model 4, stepping 3",
            ),
        ] {
            let mut cpu = Cpu::new();
            cpu.set_cpu_type(cpu_type);
            cpu.set_reset_vector_state();
            assert_eq!(cpu.regs.edx, expected, "{cpu_type:?}: {why}");
        }
    }
    /// The ordering half of the same bug: startup stamps the reset vector
    /// BEFORE applying the CLI `--cpu` identity, so EDX described the previous
    /// model. Re-stamping must bring it back in line without disturbing any
    /// other reset state.
    #[test]
    fn stamp_reset_identity_refreshes_edx_after_a_late_identity_change() {
        let mut cpu = Cpu::new();
        cpu.set_cpu_type(CpuType::I80386);
        cpu.set_reset_vector_state();
        assert_eq!(cpu.regs.edx, 0x0000_0300);
        cpu.set_cpu_type(CpuType::Pentium);
        assert_eq!(
            cpu.regs.edx, 0x0000_0300,
            "changing the identity alone must not silently rewrite EDX"
        );
        cpu.stamp_reset_identity();
        assert_eq!(cpu.regs.edx, 0x0000_0525, "EDX must follow the identity");
        assert_eq!(cpu.regs.cs, 0xF000);
        assert_eq!(cpu.regs.cs_cache.base, 0xFFFF_0000);
        assert_eq!(cpu.regs.eip, 0xFFF0);
        assert_eq!(cpu.regs.cr0, 0x6000_0010);
        assert_eq!(cpu.regs.eflags(), 0x0000_0002);
    }
    #[test]
    fn test_set_reset_vector_state_clears_stale_transient_state() {
        let mut cpu = Cpu::new();
        cpu.halted = true;
        cpu.interrupt_inhibit = 3;
        cpu.interrupts_enabled = true;
        cpu.last_iret = true;
        cpu.last_retf = true;
        cpu.dispatching_exception = Some(13);
        cpu.regs.ds = 0x1234;
        cpu.regs.ds_cache.base = 0xDEAD_BEEF;
        cpu.regs.ds_cache.limit = 0x0FFF;
        cpu.regs.fs_cache.is_code = true;
        cpu.set_reset_vector_state();
        assert!(!cpu.halted);
        assert_eq!(cpu.interrupt_inhibit, 0);
        assert!(!cpu.interrupts_enabled);
        assert!(!cpu.last_iret);
        assert!(!cpu.last_retf);
        assert!(cpu.dispatching_exception.is_none());
        assert_eq!(cpu.regs.ds, 0);
        assert_eq!(cpu.regs.ds_cache.base, 0);
        assert_eq!(cpu.regs.ds_cache.limit, 0xFFFF);
        assert!(!cpu.regs.fs_cache.is_code);
    }
    #[test]
    fn init_reset_vector_preserves_p6_msrs_but_full_reset_clears_them() {
        use crate::msr::{write_msr, MemoryTypeMsrs};
        let mut cpu = Cpu::new();
        cpu.set_cpu_type(CpuType::PentiumIIICoppermine);
        write_msr(&mut cpu, crate::msr::MSR_IA32_SYSENTER_CS, 0x1234).unwrap();
        write_msr(&mut cpu, crate::msr::MSR_IA32_SYSENTER_ESP, 0x1234_5678).unwrap();
        write_msr(&mut cpu, crate::msr::MSR_IA32_SYSENTER_EIP, 0x8765_4321).unwrap();
        write_msr(&mut cpu, crate::msr::MSR_IA32_MTRR_DEF_TYPE, 0xC06).unwrap();
        write_msr(&mut cpu, crate::msr::MSR_IA32_PAT, 0).unwrap();
        let sysenter = (cpu.sysenter_cs, cpu.sysenter_esp, cpu.sysenter_eip);
        let memory_types = cpu.memory_type_msrs.clone();
        cpu.set_reset_vector_state();
        assert_eq!(
            (cpu.sysenter_cs, cpu.sysenter_esp, cpu.sysenter_eip),
            sysenter,
            "INIT/reset-vector entry preserves SYSENTER MSRs"
        );
        assert_eq!(
            cpu.memory_type_msrs, memory_types,
            "INIT/reset-vector entry preserves MTRR and PAT MSRs"
        );
        cpu.reset();
        assert_eq!(
            (cpu.sysenter_cs, cpu.sysenter_esp, cpu.sysenter_eip),
            (0, 0, 0)
        );
        assert_eq!(cpu.memory_type_msrs, MemoryTypeMsrs::default());
    }
    #[test]
    fn test_set_reset_vector_state_preserves_fpu_state() {
        let mut cpu = Cpu::new();
        cpu.fpu.push(F80::from_f64(42.5));
        cpu.fpu.status_word = 0x6D41;
        cpu.fpu.control_word = 0x0272;
        cpu.fpu.tag_word = 0x1234;
        cpu.fpu.ip_offset = 0xDEAD_BEEF;
        cpu.fpu.ip_selector = 0xBEEF;
        cpu.fpu.operand_offset = 0xCAFE_BABE;
        cpu.fpu.operand_selector = 0xCAFE;
        cpu.fpu.x87_save_area.fill(0xA5);
        let before = serde_json::to_value(&cpu.fpu).expect("FPU state must serialize");
        cpu.set_reset_vector_state();
        assert_eq!(
            serde_json::to_value(&cpu.fpu).expect("FPU state must serialize"),
            before
        );
    }
    #[test]
    fn test_set_reset_vector_state_pentium_edx() {
        let mut cpu = Cpu::new();
        cpu.set_cpu_type(CpuType::Pentium);
        cpu.set_reset_vector_state();
        assert_eq!(cpu.regs.edx, 0x0000_0525);
    }
    #[test]
    fn test_set_reset_vector_state_preserves_zeroed_gprs() {
        let mut cpu = Cpu::new();
        cpu.regs.eax = 0xDEAD_BEEF;
        cpu.regs.ecx = 0x1111_2222;
        cpu.regs.esi = 0x3333_4444;
        cpu.regs.edi = 0x5555_6666;
        cpu.regs.ebp = 0x7777_8888;
        cpu.regs.esp = 0x9999_AAAA;
        cpu.regs.ebx = 0xBBBB_CCCC;
        cpu.set_reset_vector_state();
        assert_eq!(cpu.regs.eax, 0xDEAD_BEEF);
        assert_eq!(cpu.regs.ecx, 0x1111_2222);
        assert_eq!(cpu.regs.esi, 0x3333_4444);
        assert_eq!(cpu.regs.edi, 0x5555_6666);
        assert_eq!(cpu.regs.ebp, 0x7777_8888);
        assert_eq!(cpu.regs.esp, 0x9999_AAAA);
        assert_eq!(cpu.regs.ebx, 0xBBBB_CCCC);
        assert_eq!(cpu.regs.edx, 0x0000_0300);
    }
    /// V86 far JMP via load_cs takes the real-mode branch and previously
    /// called `set_cpl(0)`, violating the SDM §20.1 invariant that VM=1
    /// always implies CPL=3 — and tripping the setter's debug assertion.
    /// The fix skips the CPL update entirely when VM=1, leaving CPL=3
    /// (already forced by `set_v86_mode(true)` on entry).
    #[test]
    fn test_v86_far_jmp_keeps_cpl_3() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.set_v86_mode(true);
        assert_eq!(cpu.regs.cpl, 3, "set_v86_mode(true) must force CPL=3");
        cpu.load_cs(&mut mem, 0x0123).unwrap();
        assert_eq!(
            cpu.regs.cpl, 3,
            "V86 far JMP via load_cs must keep CPL=3 (SDM §20.1)"
        );
        assert!(cpu.is_v86_mode(), "V86 mode must still be active");
    }
    /// Same invariant via the IRET return path. The real-mode branch of
    /// `load_cs_iret` itself does not touch CPL today, but a V86 IRET
    /// frame can include other helpers that do — and the saved-state
    /// rollback path used to write `regs.cpl = saved_cpl` directly.
    /// This test exercises the rollback path: a load_ss failure inside
    /// `iret` triggers the restore branch, which now goes through the
    /// setter. With VM=1 and any saved_cpl, the result must be 3.
    #[test]
    fn test_v86_iret_keeps_cpl_3() {
        let mut cpu = Cpu::new();
        let mut mem = TestMemory::new(0x20000);
        cpu.regs.cr0 = 1;
        cpu.regs.set_v86_mode(true);
        assert_eq!(cpu.regs.cpl, 3);
        cpu.regs.set_cpl(3);
        assert_eq!(cpu.regs.cpl, 3);
        cpu.load_cs_iret(&mut mem, 0x0F00, 0).unwrap();
        assert_eq!(cpu.regs.cpl, 3, "V86 IRET must keep CPL=3 (SDM §20.1)");
        assert!(cpu.is_v86_mode());
    }
    /// `set_v86_mode(true)` must enforce SDM §20.1 by forcing CPL=3,
    /// even if the caller starts at CPL=0 (e.g. PM kernel transitioning
    /// into a V86 task).
    #[test]
    fn test_set_v86_mode_forces_cpl_3() {
        let mut regs = Registers {
            cpl: 0,
            cr0: 1,
            ..Default::default()
        };
        assert!(!regs.is_v86_mode());
        regs.set_v86_mode(true);
        assert!(regs.is_v86_mode());
        assert_eq!(
            regs.cpl, 3,
            "set_v86_mode(true) must enforce CPL=3 per SDM §20.1"
        );
        regs.set_v86_mode(false);
        assert!(!regs.is_v86_mode());
        assert_eq!(
            regs.cpl, 3,
            "set_v86_mode(false) must NOT alter CPL (caller restores)"
        );
    }
    /// In debug builds, attempting to call `set_cpl(0)` while VM=1 must
    /// panic — this turns latent invariant violations into immediate
    /// test failures rather than silent IOPL corruption later.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "VM=1 requires CPL=3")]
    fn test_set_cpl_in_v86_panics() {
        let mut regs = Registers::default();
        regs.set_v86_mode(true);
        regs.set_cpl(0);
    }
    /// Architectural register state survives JSON and restore for boundary values.
    #[test]
    fn architectural_registers_round_trip_through_json_and_restore() {
        for value in [
            0,
            1,
            0x7FFF,
            0x8000,
            0xFFFF,
            0x1_0000,
            0x8000_0000,
            u32::MAX,
        ] {
            let mut cpu = Cpu::new();
            cpu.regs.eax = value;
            cpu.regs.ebx = value.rotate_left(8);
            cpu.regs.ecx = !value;
            cpu.regs.edx = value.wrapping_add(1);
            let expected = serde_json::to_value(cpu.snapshot_state()).unwrap();
            let mut json = expected.clone();
            json.as_object_mut().unwrap().insert(
                "retired_diagnostic".to_owned(),
                serde_json::Value::Bool(true),
            );
            let snapshot: CpuSnapshot = serde_json::from_value(json).unwrap();
            let mut restored = Cpu::new();
            restored.restore_state(&snapshot).unwrap();
            assert_eq!(restored.regs.eax, value);
            assert_eq!(restored.regs.ebx, value.rotate_left(8));
            assert_eq!(restored.regs.ecx, !value);
            assert_eq!(restored.regs.edx, value.wrapping_add(1));
            assert_eq!(
                serde_json::to_value(restored.snapshot_state()).unwrap(),
                expected
            );
        }
    }
    #[test]
    fn mmx_alias_state_round_trips_through_snapshot_and_restore() {
        let mut cpu = Cpu::new();
        cpu.fpu.enter_mmx_state();
        cpu.set_mmx_reg(0, 0x0123_4567_89AB_CDEF);
        cpu.set_mmx_reg(7, 0xFEDC_BA98_7654_3210);
        let snap = cpu.snapshot_state();
        assert_eq!(snap.mmx, cpu.fpu.mmx_registers());
        cpu.fpu.emms();
        cpu.set_mmx_regs([0; 8]);
        cpu.restore_state(&snap).unwrap();
        assert_eq!(cpu.mmx_reg(0), 0x0123_4567_89AB_CDEF);
        assert_eq!(cpu.mmx_reg(7), 0xFEDC_BA98_7654_3210);
        assert_eq!(cpu.fpu.get_top(), 0);
        assert_eq!(cpu.fpu.tag_word, 0);
        assert_eq!(&cpu.fpu.regs[0].bytes()[8..], &[0xFF, 0xFF]);
        assert_eq!(&cpu.fpu.regs[7].bytes()[8..], &[0xFF, 0xFF]);
    }
    #[test]
    fn sse_state_resets_and_round_trips_through_snapshot() {
        let mut cpu = Cpu::new();
        let values = [
            0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210u128,
            0xFFFF_0000_1111_2222_3333_4444_5555_6666u128,
            0x8000_0000_0000_0000_0000_0000_0000_0001u128,
            0x7FFF_FFFF_FFFF_FFFF_0000_0000_0000_0000u128,
            0xDEAD_BEEF_CAFE_BABE_1357_2468_9ABC_DEF0u128,
            0,
            u128::MAX,
            0xA5A5_A5A5_A5A5_A5A5_5A5A_5A5A_5A5A_5A5Au128,
        ];
        cpu.set_xmm_regs(values);
        cpu.set_mxcsr(0x0000_9F80);
        let snapshot = cpu.snapshot_state();
        assert_eq!(snapshot.sse.xmm, values);
        assert_eq!(snapshot.sse.mxcsr, 0x0000_9F80);
        assert_eq!(snapshot.sse.mxcsr_mask, crate::sse::MXCSR_MASK);
        cpu.set_xmm_regs([0; 8]);
        cpu.set_mxcsr(0);
        cpu.restore_state(&snapshot).unwrap();
        assert_eq!(cpu.xmm_regs(), values);
        assert_eq!(cpu.mxcsr(), 0x0000_9F80);
        cpu.set_xmm_reg(0, u128::MAX);
        cpu.set_mxcsr(0);
        cpu.reset();
        assert_eq!(cpu.xmm_regs(), [0; 8]);
        assert_eq!(cpu.mxcsr(), crate::sse::MXCSR_RESET);
    }
    #[test]
    fn older_cpu_snapshot_json_defaults_sse_state() {
        let mut json = serde_json::to_value(Cpu::new().snapshot_state()).unwrap();
        let object = json.as_object_mut().unwrap();
        assert!(object.remove("sse").is_some());
        let snapshot: CpuSnapshot = serde_json::from_value(json).unwrap();
        assert_eq!(snapshot.sse, SseState::new());
    }
    #[test]
    fn snapshot_rejects_sse_cr4_bits_on_older_identity() {
        let cpu = Cpu::new();
        let mut snapshot = cpu.snapshot_state();
        snapshot.regs.cr4 = crate::registers::CR4_OSFXSR;
        let error = snapshot.validate().unwrap_err();
        assert!(matches!(
            error,
            CpuSnapshotValidationError::InvalidCr4Bits {
                value: crate::registers::CR4_OSFXSR,
                invalid: crate::registers::CR4_OSFXSR,
                cpu_type: CpuType::I80386,
            }
        ));
    }
    #[test]
    fn snapshot_rejects_invalid_coppermine_memory_type_msrs() {
        let mut cpu = Cpu::new();
        cpu.set_cpu_type(CpuType::PentiumIIICoppermine);
        let valid = cpu.snapshot_state();
        let mut invalid = valid.clone();
        invalid.memory_type_msrs.def_type = 0xC02;
        assert!(
            invalid.validate().is_err(),
            "MTRR default type 2 is reserved"
        );
        let mut invalid = valid.clone();
        invalid.memory_type_msrs.fixed[0] = 0x0202_0202_0202_0202;
        assert!(invalid.validate().is_err(), "fixed MTRR type 2 is reserved");
        let mut invalid = valid.clone();
        invalid.memory_type_msrs.variable[0] = 0x1234_5007;
        assert!(invalid.validate().is_err(), "MTRR type 7 is reserved");
        let mut invalid = valid.clone();
        invalid.memory_type_msrs.variable[1] = 1;
        assert!(
            invalid.validate().is_err(),
            "variable mask bit 0 is reserved"
        );
        let mut invalid = valid;
        invalid.memory_type_msrs.pat = 0x0207_0406_0007_0406;
        assert!(invalid.validate().is_err(), "PAT type 2 is reserved");
    }
    #[test]
    fn snapshot_rejects_new_msr_state_without_identity_feature() {
        let cpu = Cpu::new();
        let valid = cpu.snapshot_state();
        let mut unsupported = valid.clone();
        unsupported.sysenter_cs = 1;
        assert!(unsupported.validate().is_err(), "I80386 has no SEP state");
        let mut unsupported = valid.clone();
        unsupported.memory_type_msrs.def_type = 0xC06;
        assert!(unsupported.validate().is_err(), "I80386 has no MTRR state");
        let mut unsupported = valid;
        unsupported.memory_type_msrs.pat = 0;
        assert!(unsupported.validate().is_err(), "I80386 has no PAT state");
    }
    #[test]
    fn snapshot_rejects_zero_explicit_tsc_clock() {
        let cpu = Cpu::new();
        let mut snapshot = cpu.snapshot_state();
        snapshot.tsc_clock_hz = Some(0);
        assert!(snapshot.validate().is_err());
        let mut snapshot = cpu.snapshot_state();
        snapshot.tsc_cycle_hz = Some(0);
        assert!(snapshot.validate().is_err());
    }
}
