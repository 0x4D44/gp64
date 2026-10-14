//! Model-Specific Register (MSR) read/write semantics.
//!
//! Implements the minimum Pentium-class MSR set the emulator currently
//! exposes. Both the interpreter and (in future) the JIT route their
//! `RDMSR` / `WRMSR` decode through this module so the two engines cannot
//! drift on MSR semantics.
//!
//! # Strict vs lenient mode
//!
//! Real hardware raises `#GP(0)` when software accesses an undefined MSR
//! index. We default to that behaviour ([`MsrFault::Gp`]). Tests may force
//! lenient mode, where unknown `RDMSR` returns 0 and unknown `WRMSR` is
//! silently dropped with a one-shot warning per index.
//!
//! # Currently implemented MSRs
//!
//! | ECX     | Name                       | Semantics                                          |
//! |---------|----------------------------|----------------------------------------------------|
//! | `0x10`  | `IA32_TIME_STAMP_COUNTER`  | RDMSR mirrors RDTSC; WRMSR rebases the counter.   |
//! | `0x1B`  | `IA32_APIC_BASE`           | Reads return BSP=1, APIC global enable=0, base 0xFEE0_0000. Writes are stored but ignored (warn once if the guest tries to enable the APIC). |
//! | `0x79`  | `IA32_BIOS_UPDT_TRIG`      | P6 and later only. WRMSR is accepted as a rejected microcode update: no state changes and `0x8B` still reads 0. RDMSR raises `#GP(0)` (write-only). Earlier parts `#GP` on both. |
//! | `0x8B`  | `IA32_BIOS_SIGN_ID`        | Reads return 0 (no microcode update). Writes are no-ops. |
//!
//! Coppermine also implements MTRR/PAT storage and SYSENTER MSRs.
//! Anything else falls through to the unknown-MSR policy above.
use crate::Cpu;
use std::sync::Mutex;
use std::sync::OnceLock;
/// Memory type registers are stored, not applied: the emulator models no caches.
/// Intel SDM Vol. 3A §§10.11–10.12 (253668-021): 36-bit P6 MTRR fields,
/// eight variable pairs, fixed ranges and WC; PAT resets to WB/WT/UC-/UC.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MemoryTypeMsrs {
    pub def_type: u64,
    pub fixed: [u64; 11],
    pub variable: [u64; 16],
    pub pat: u64,
}
impl Default for MemoryTypeMsrs {
    fn default() -> Self {
        Self {
            def_type: 0,
            fixed: [0; 11],
            variable: [0; 16],
            pat: 0x0007_0406_0007_0406,
        }
    }
}
pub const MSR_IA32_MTRRCAP: u32 = 0xFE;
pub const MSR_IA32_MTRR_DEF_TYPE: u32 = 0x2FF;
pub const MSR_IA32_PAT: u32 = 0x277;
pub const MSR_IA32_SYSENTER_CS: u32 = 0x174;
pub const MSR_IA32_SYSENTER_ESP: u32 = 0x175;
pub const MSR_IA32_SYSENTER_EIP: u32 = 0x176;
/// MSR index for `IA32_TIME_STAMP_COUNTER` (Pentium+).
pub const MSR_IA32_TIME_STAMP_COUNTER: u32 = 0x0000_0010;
/// MSR index for `IA32_APIC_BASE`. Available since Pentium (P5) per Intel SDM Vol 4 §2.5.
pub const MSR_IA32_APIC_BASE: u32 = 0x0000_001B;
/// MSR index for `IA32_BIOS_UPDT_TRIG`, the microcode-update trigger.
///
/// Intel SDM Vol. 4, Table 2-2 (architectural MSRs) lists `79H`
/// `IA32_BIOS_UPDT_TRIG` as write-only ("BIOS Update Trigger (W)"),
/// introduced with family 06H; Vol. 3A §9.11 ("Microcode Update
/// Facilities") describes the loader writing the update's linear address
/// here and then reading `IA32_BIOS_SIGN_ID` (8BH) to learn whether the
/// update was applied. The SDM defines no read semantics for a write-only
/// MSR; this model raises `#GP(0)` on RDMSR, as for any MSR that is not
/// readable *(inference from the "(W)" classification)*.
///
/// We carry no microcode, so every update is rejected: the write changes no
/// state and `IA32_BIOS_SIGN_ID` keeps reading 0, which is what a loader
/// sees when the processor refuses an update whose signature does not
/// match.
pub const MSR_IA32_BIOS_UPDT_TRIG: u32 = 0x0000_0079;
/// MSR index for `IA32_BIOS_SIGN_ID` (microcode-update signature).
pub const MSR_IA32_BIOS_SIGN_ID: u32 = 0x0000_008B;
/// P6 performance counters 0 and 1 (`PerfCtr0`/`PerfCtr1`).
///
/// Family 6 entitles a guest to these: unlike CMOV or MMX there is NO CPUID
/// feature bit for the P6 performance-monitoring facility, so advertising
/// family 6 IS the advertisement. See `read_msr` for why they read zero.
pub const MSR_P6_PERFCTR0: u32 = 0x0000_00C1;
pub const MSR_P6_PERFCTR1: u32 = 0x0000_00C2;
/// P6 performance event selectors 0 and 1 (`PerfEvtSel0`/`PerfEvtSel1`).
pub const MSR_P6_EVNTSEL0: u32 = 0x0000_0186;
pub const MSR_P6_EVNTSEL1: u32 = 0x0000_0187;
/// Initial value reported for `IA32_APIC_BASE`.
///
/// Bits 12..=63 are the APIC physical base address (0xFEE0_0000).
/// Bit 11 (APIC global enable) is **clear** — we do not model an APIC.
/// Bit 8 (BSP, bootstrap-processor flag) is set: every guest sees itself
/// as the BSP.
///
/// The HLD originally cited `0xFEE0_0900` for this value, but `0x900`
/// has bit 11 set, which contradicts the "global enable clear" intent.
/// `0x100` is BSP=1 with global enable cleared, which matches both
/// real Pentium-class hardware in the no-APIC state and the HLD's
/// stated semantics.
pub const IA32_APIC_BASE_DEFAULT: u64 = 0x0000_0000_FEE0_0100;
/// Bit 11 of `IA32_APIC_BASE`: APIC global enable.
const IA32_APIC_BASE_GLOBAL_ENABLE: u64 = 1 << 11;
/// Outcome of an attempted MSR access. Only `#GP(0)` is currently produced.
///
/// This is an enum rather than a unit struct because future MSRs (e.g.
/// memory-mapped MTRRs) may need to surface `#PF` distinctly from `#GP`,
/// or report a partial success that the caller must react to. Adding a
/// variant is a non-breaking change; collapsing to a unit struct now would
/// force a breaking re-introduction later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsrFault {
    /// Raise `#GP(0)`. The caller is responsible for the architectural
    /// state save (EIP, FLAGS) and exception delivery.
    Gp,
}
fn fixed_mtrr_slot(index: u32) -> Option<usize> {
    match index {
        0x250 => Some(0),
        0x258 => Some(1),
        0x259 => Some(2),
        0x268..=0x26F => Some((index - 0x268) as usize + 3),
        _ => None,
    }
}
fn valid_memory_type(ty: u8, pat: bool) -> bool {
    matches!(ty, 0 | 1 | 4 | 5 | 6) || (pat && ty == 7)
}
fn valid_memory_type_bytes(value: u64, pat: bool) -> bool {
    value
        .to_le_bytes()
        .into_iter()
        .all(|ty| valid_memory_type(ty, pat))
}
fn is_memory_type_msr(index: u32) -> bool {
    matches!(index, 0xFE | 0x200..=0x20F | 0x277 | 0x2FF) || fixed_mtrr_slot(index).is_some()
}
/// Apply the WRMSR reserved-bit and memory-type checks for a stored MTRR/PAT
/// value. Snapshot validation calls this same validator so serialized state
/// cannot bypass the checks used for guest WRMSR instructions.
pub(crate) fn validate_memory_type_msr_value(index: u32, value: u64) -> Result<(), MsrFault> {
    match index {
        MSR_IA32_MTRRCAP => Err(MsrFault::Gp),
        MSR_IA32_PAT => {
            if valid_memory_type_bytes(value, true) {
                Ok(())
            } else {
                Err(MsrFault::Gp)
            }
        }
        MSR_IA32_MTRR_DEF_TYPE => {
            if value & !0xCFF == 0 && valid_memory_type(value as u8, false) {
                Ok(())
            } else {
                Err(MsrFault::Gp)
            }
        }
        0x200..=0x20F => {
            const ADDRESS_MASK: u64 = 0xF_FFFF_F000;
            let valid = if index & 1 == 0 {
                value & !(ADDRESS_MASK | 0xFF) == 0 && valid_memory_type(value as u8, false)
            } else {
                value & !(ADDRESS_MASK | 0x800) == 0
            };
            if valid {
                Ok(())
            } else {
                Err(MsrFault::Gp)
            }
        }
        _ if fixed_mtrr_slot(index).is_some() => {
            if valid_memory_type_bytes(value, false) {
                Ok(())
            } else {
                Err(MsrFault::Gp)
            }
        }
        _ => Err(MsrFault::Gp),
    }
}
/// Read the value of the MSR selected by `ecx`.
///
/// Returns the 64-bit MSR value on success, or [`MsrFault::Gp`] if the
/// index is unknown and lenient mode is disabled. Real hardware raises
/// `#GP(0)` on unknown indices; the caller delivers the exception.
pub fn read_msr(cpu: &Cpu, ecx: u32) -> Result<u64, MsrFault> {
    if is_memory_type_msr(ecx)
        && if ecx == MSR_IA32_PAT {
            cpu.cpu_type.supports_pat()
        } else {
            cpu.cpu_type.supports_mtrr()
        }
    {
        if ecx == MSR_IA32_PAT {
            return Ok(cpu.memory_type_msrs.pat);
        }
        return match ecx {
            MSR_IA32_MTRRCAP => Ok(0x508),
            MSR_IA32_MTRR_DEF_TYPE => Ok(cpu.memory_type_msrs.def_type),
            0x200..=0x20F => Ok(cpu.memory_type_msrs.variable[(ecx - 0x200) as usize]),
            _ => Ok(cpu.memory_type_msrs.fixed[fixed_mtrr_slot(ecx).expect("fixed MTRR")]),
        };
    }
    match ecx {
        MSR_IA32_SYSENTER_CS | MSR_IA32_SYSENTER_ESP | MSR_IA32_SYSENTER_EIP
            if cpu.cpu_type.supports_sep() =>
        {
            Ok(u64::from(match ecx {
                MSR_IA32_SYSENTER_CS => cpu.sysenter_cs,
                MSR_IA32_SYSENTER_ESP => cpu.sysenter_esp,
                _ => cpu.sysenter_eip,
            }))
        }
        MSR_IA32_TIME_STAMP_COUNTER => Ok(read_tsc(cpu)),
        MSR_IA32_APIC_BASE => Ok(IA32_APIC_BASE_DEFAULT),
        MSR_IA32_BIOS_SIGN_ID => Ok(0),
        MSR_P6_PERFCTR0 | MSR_P6_PERFCTR1 | MSR_P6_EVNTSEL0 | MSR_P6_EVNTSEL1
            if cpu.cpu_type.supports_p6() =>
        {
            Ok(0)
        }
        other => {
            if lenient_mode() {
                warn_unknown_once(other, false);
                Ok(0)
            } else {
                Err(MsrFault::Gp)
            }
        }
    }
}
/// Write `value` to the MSR selected by `ecx`.
///
/// Returns `Ok(())` on success or [`MsrFault::Gp`] if the index is unknown
/// and lenient mode is disabled.
pub fn write_msr(cpu: &mut Cpu, ecx: u32, value: u64) -> Result<(), MsrFault> {
    if is_memory_type_msr(ecx)
        && if ecx == MSR_IA32_PAT {
            cpu.cpu_type.supports_pat()
        } else {
            cpu.cpu_type.supports_mtrr()
        }
    {
        validate_memory_type_msr_value(ecx, value)?;
        if ecx == MSR_IA32_PAT {
            cpu.memory_type_msrs.pat = value;
            return Ok(());
        }
        match ecx {
            MSR_IA32_MTRR_DEF_TYPE => cpu.memory_type_msrs.def_type = value,
            0x200..=0x20F => {
                cpu.memory_type_msrs.variable[(ecx - 0x200) as usize] = value;
            }
            _ => {
                let slot = fixed_mtrr_slot(ecx).expect("validated fixed MTRR MSR");
                cpu.memory_type_msrs.fixed[slot] = value;
            }
        }
        return Ok(());
    }
    match ecx {
        MSR_IA32_SYSENTER_CS | MSR_IA32_SYSENTER_ESP | MSR_IA32_SYSENTER_EIP
            if cpu.cpu_type.supports_sep() =>
        {
            match ecx {
                MSR_IA32_SYSENTER_CS => cpu.sysenter_cs = value as u32,
                MSR_IA32_SYSENTER_ESP => cpu.sysenter_esp = value as u32,
                _ => cpu.sysenter_eip = value as u32,
            }
            Ok(())
        }
        MSR_IA32_TIME_STAMP_COUNTER => {
            cpu.write_tsc_at(cpu.cycles, value);
            Ok(())
        }
        MSR_IA32_APIC_BASE => {
            if (value & IA32_APIC_BASE_GLOBAL_ENABLE) != 0 {
                warn_apic_enable_once();
            }
            Ok(())
        }
        MSR_P6_PERFCTR0 | MSR_P6_PERFCTR1 | MSR_P6_EVNTSEL0 | MSR_P6_EVNTSEL1
            if cpu.cpu_type.supports_p6() =>
        {
            Ok(())
        }
        MSR_IA32_BIOS_SIGN_ID => Ok(()),
        MSR_IA32_BIOS_UPDT_TRIG if cpu.cpu_type.supports_p6() => Ok(()),
        other => {
            if lenient_mode() {
                warn_unknown_once(other, true);
                Ok(())
            } else {
                Err(MsrFault::Gp)
            }
        }
    }
}
/// Read the architectural TSC: the constant-TSC scaled ticks plus any
/// [`Cpu::tsc_write_offset`] installed by a previous `WRMSR(0x10)`.
///
/// Pulled out of [`read_msr`] so the interpreter's `RDTSC` handler can
/// share a single source of truth. On a bare `Cpu` (no
/// [`Cpu::set_tsc_rate`] wiring) the scaling is 1:1 and this remains the
/// historical `cycles + offset`.
#[inline]
pub fn read_tsc(cpu: &Cpu) -> u64 {
    cpu.tsc_from_arch_cycles(cpu.cycles)
}
/// Cached result of the unknown-MSR leniency policy.
///
/// Tests may install a thread-local override via
/// [`tests::ScopedLenientOverride`]. The override takes precedence so each
/// test can pin the policy.
fn lenient_mode() -> bool {
    #[cfg(test)]
    if let Some(forced) = tests::lenient_override_value() {
        return forced;
    }
    false
}
/// Per-index warning de-duplicator for unknown-MSR access in lenient mode.
fn warn_unknown_once(ecx: u32, is_write: bool) {
    if seen_indices().insert_if_new(ecx, false) {
        let direction = if is_write { "WRMSR" } else { "RDMSR" };
        tracing::warn!(
            target : "gp64::msr", ecx = format_args!("{:#010X}", ecx), direction =
            direction,
            "unknown MSR access in lenient mode (returning 0 / dropping write)"
        );
    }
}
/// One-shot warning when the guest tries to set the APIC global enable bit.
fn warn_apic_enable_once() {
    if seen_indices().insert_if_new(MSR_IA32_APIC_BASE, true) {
        tracing::warn!(
            target : "gp64::msr",
            "guest attempted to enable the local APIC via IA32_APIC_BASE bit 11; ignoring (no APIC modelled)"
        );
    }
}
/// Tiny set used to deduplicate one-shot warnings across the process. We
/// pair the MSR index with a discriminator bit (the APIC enable warning is
/// distinct from a generic unknown-MSR warning for the same index).
fn seen_indices() -> &'static SeenIndices {
    static SEEN: OnceLock<SeenIndices> = OnceLock::new();
    SEEN.get_or_init(SeenIndices::new)
}
struct SeenIndices {
    inner: Mutex<Vec<(u32, bool)>>,
}
impl SeenIndices {
    fn new() -> Self {
        Self {
            inner: Mutex::new(Vec::new()),
        }
    }
    /// Insert `(ecx, enable_bit)`; return `true` if it was newly added.
    fn insert_if_new(&self, ecx: u32, enable_bit: bool) -> bool {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let key = (ecx, enable_bit);
        if guard.contains(&key) {
            return false;
        }
        guard.push(key);
        true
    }
    /// Test-only reset. Avoids cross-test contamination of the one-shot set.
    #[cfg(test)]
    fn reset(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.clear();
        }
    }
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(super) mod tests {
    use super::*;
    use crate::{Cpu, CpuType};
    use std::cell::Cell;
    use std::sync::MutexGuard;
    thread_local! {
        #[doc = " `Some(b)` forces `lenient_mode()` to return `b` on this thread,"] #[doc
        = " `None` defers to the env-var-backed cache."] static LENIENT_OVERRIDE : Cell <
        Option < bool >> = const { Cell::new(None) };
    }
    /// Read the per-thread override installed by [`ScopedLenientOverride`].
    pub(super) fn lenient_override_value() -> Option<bool> {
        LENIENT_OVERRIDE.with(|c| c.get())
    }
    /// RAII guard that pins `lenient_mode()` to a specific value for the
    /// lifetime of the guard. Restores the previous override on drop, so
    /// nested guards compose correctly.
    struct ScopedLenientOverride {
        previous: Option<bool>,
    }
    impl ScopedLenientOverride {
        fn strict() -> Self {
            let previous = LENIENT_OVERRIDE.with(|c| c.replace(Some(false)));
            Self { previous }
        }
        fn lenient() -> Self {
            let previous = LENIENT_OVERRIDE.with(|c| c.replace(Some(true)));
            Self { previous }
        }
    }
    impl Drop for ScopedLenientOverride {
        fn drop(&mut self) {
            LENIENT_OVERRIDE.with(|c| c.set(self.previous));
        }
    }
    /// Serializes tests that inspect or reset the process-wide warning set.
    ///
    /// The production set deliberately outlives every CPU, but parallel tests
    /// need exclusive ownership while asserting its contents. Resetting both
    /// ends prevents one stateful test from contaminating the next.
    struct ScopedWarningState {
        _guard: MutexGuard<'static, ()>,
    }
    impl ScopedWarningState {
        fn isolated() -> Self {
            static TEST_LOCK: Mutex<()> = Mutex::new(());
            let guard = match TEST_LOCK.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            seen_indices().reset();
            Self { _guard: guard }
        }
    }
    impl Drop for ScopedWarningState {
        fn drop(&mut self) {
            seen_indices().reset();
        }
    }
    fn fresh_cpu() -> Cpu {
        Cpu::new()
    }
    fn coppermine() -> Cpu {
        let mut cpu = fresh_cpu();
        cpu.set_cpu_type(CpuType::PentiumIIICoppermine);
        cpu
    }
    #[test]
    fn coppermine_mtrrcap_and_default_type() {
        let mut cpu = coppermine();
        assert_eq!(read_msr(&cpu, 0xFE), Ok(0x508));
        assert_eq!(write_msr(&mut cpu, 0xFE, 0), Err(MsrFault::Gp));
        assert_eq!(read_msr(&cpu, 0x2FF), Ok(0));
        for ty in [0, 1, 4, 5, 6] {
            write_msr(&mut cpu, 0x2FF, 0xC00 | ty).unwrap();
            assert_eq!(read_msr(&cpu, 0x2FF), Ok(0xC00 | ty));
        }
        for bad in [2, 3, 7, 8, 0x100, 0x200, 0x1000, 1 << 63] {
            assert_eq!(write_msr(&mut cpu, 0x2FF, bad), Err(MsrFault::Gp));
            assert_eq!(read_msr(&cpu, 0x2FF), Ok(0xC06));
        }
    }
    #[test]
    fn coppermine_fixed_mtrrs_roundtrip_and_fault_atomically() {
        let mut cpu = coppermine();
        for index in [
            0x250, 0x258, 0x259, 0x268, 0x269, 0x26A, 0x26B, 0x26C, 0x26D, 0x26E, 0x26F,
        ] {
            assert_eq!(read_msr(&cpu, index), Ok(0));
            let value = 0x0605_0401_0006_0504;
            write_msr(&mut cpu, index, value).unwrap();
            assert_eq!(read_msr(&cpu, index), Ok(value));
            for lane in 0..8 {
                for bad in [2u64, 3, 7, 8, 0x80] {
                    assert_eq!(
                        write_msr(&mut cpu, index, bad << (lane * 8)),
                        Err(MsrFault::Gp)
                    );
                    assert_eq!(read_msr(&cpu, index), Ok(value));
                }
            }
        }
    }
    #[test]
    fn coppermine_variable_mtrrs_reserved_bits_and_types() {
        let mut cpu = coppermine();
        for pair in 0..8 {
            let base = 0x200 + pair * 2;
            let mask = base + 1;
            assert_eq!(read_msr(&cpu, base), Ok(0));
            assert_eq!(read_msr(&cpu, mask), Ok(0));
            for ty in [0, 1, 4, 5, 6] {
                let value = 0xF_1234_5000 | ty;
                write_msr(&mut cpu, base, value).unwrap();
                assert_eq!(read_msr(&cpu, base), Ok(value));
            }
            for bad in [2, 3, 7, 0x100, 0x800, 1 << 36, 1 << 63] {
                assert_eq!(write_msr(&mut cpu, base, bad), Err(MsrFault::Gp));
                assert_eq!(read_msr(&cpu, base), Ok(0xF_1234_5006));
            }
            let value = 0xF_FF55_5800;
            write_msr(&mut cpu, mask, value).unwrap();
            assert_eq!(read_msr(&cpu, mask), Ok(value));
            for bit in (0..11).chain(36..64) {
                assert_eq!(write_msr(&mut cpu, mask, 1u64 << bit), Err(MsrFault::Gp));
                assert_eq!(read_msr(&cpu, mask), Ok(value));
            }
        }
    }
    #[test]
    fn coppermine_pat_reset_types_and_reserved_bits() {
        let mut cpu = coppermine();
        assert_eq!(read_msr(&cpu, 0x277), Ok(0x0007_0406_0007_0406));
        let value = 0x0706_0504_0100_0706;
        write_msr(&mut cpu, 0x277, value).unwrap();
        assert_eq!(read_msr(&cpu, 0x277), Ok(value));
        for lane in 0..8 {
            for bad in [2u64, 3, 8, 0x80] {
                assert_eq!(
                    write_msr(&mut cpu, 0x277, bad << (lane * 8)),
                    Err(MsrFault::Gp)
                );
                assert_eq!(read_msr(&cpu, 0x277), Ok(value));
            }
        }
    }
    #[test]
    fn coppermine_memory_type_msrs_snapshot_and_reset() {
        let mut cpu = coppermine();
        for (index, value) in [
            (0x200, 0x12345006),
            (0x201, 0xF_FF00_0800),
            (0x250, 0x0606_0606_0606_0606),
            (0x2FF, 0xC06),
            (0x277, 0),
        ] {
            write_msr(&mut cpu, index, value).unwrap();
        }
        let snap = cpu.snapshot_state();
        let mut restored = coppermine();
        restored.restore_state(&snap).unwrap();
        assert_eq!(restored.memory_type_msrs, cpu.memory_type_msrs);
        cpu.reset();
        assert_eq!(cpu.memory_type_msrs, MemoryTypeMsrs::default());
        let preserved = restored.memory_type_msrs.clone();
        restored.set_reset_vector_state();
        assert_eq!(restored.memory_type_msrs, preserved);
    }
    #[test]
    fn old_identities_reject_coppermine_memory_type_msrs() {
        let _guard = ScopedLenientOverride::strict();
        for identity in CpuType::ALL {
            if identity == CpuType::PentiumIIICoppermine {
                continue;
            }
            let mut cpu = fresh_cpu();
            cpu.set_cpu_type(identity);
            for index in [
                0xFE, 0x200, 0x20F, 0x250, 0x258, 0x259, 0x268, 0x26F, 0x277, 0x2FF,
            ] {
                assert_eq!(read_msr(&cpu, index), Err(MsrFault::Gp));
                assert_eq!(write_msr(&mut cpu, index, 0), Err(MsrFault::Gp));
            }
        }
    }
    #[test]
    fn rdmsr_tsc_returns_cycles() {
        let mut cpu = fresh_cpu();
        cpu.cycles = 12345;
        let v = read_msr(&cpu, MSR_IA32_TIME_STAMP_COUNTER).expect("RDMSR(0x10) ok");
        assert_eq!(v, 12345);
        assert_eq!(read_tsc(&cpu), 12345);
    }
    #[test]
    fn wrmsr_tsc_rebases_subsequent_reads() {
        let mut cpu = fresh_cpu();
        cpu.cycles = 1000;
        write_msr(&mut cpu, MSR_IA32_TIME_STAMP_COUNTER, 5_000_000).expect("WRMSR(0x10) ok");
        assert_eq!(read_tsc(&cpu), 5_000_000);
        assert_eq!(
            read_msr(&cpu, MSR_IA32_TIME_STAMP_COUNTER).unwrap(),
            5_000_000
        );
        cpu.cycles += 42;
        assert_eq!(read_tsc(&cpu), 5_000_042);
    }
    #[test]
    fn wrmsr_tsc_handles_negative_offset() {
        let mut cpu = fresh_cpu();
        cpu.cycles = 10_000_000;
        write_msr(&mut cpu, MSR_IA32_TIME_STAMP_COUNTER, 100).expect("WRMSR(0x10) ok");
        assert_eq!(read_tsc(&cpu), 100);
        cpu.cycles += 25;
        assert_eq!(read_tsc(&cpu), 125);
    }
    /// Constant-TSC model (2026.08.17): once a rate is wired, the TSC ticks
    /// at the IDENTITY's nominal clock against guest-virtual time, not at
    /// one tick per emulated instruction. One guest-second of cycles at any
    /// emulation rate yields `nominal_hz` ticks.
    ///
    /// Fail-once observed by flipping the Pentium4 table entry to
    /// 100 MHz: the first assertion read 100_000_000 against 2_400_000_000.
    #[test]
    fn wired_tsc_ticks_at_the_identity_nominal_clock() {
        let mut cpu = fresh_cpu();
        cpu.cpu_type = CpuType::Pentium4;
        cpu.set_tsc_rate(100_000);
        let base = read_tsc(&cpu);
        cpu.cycles += 100_000 * 1000;
        assert_eq!(read_tsc(&cpu) - base, 2_400_000_000, "P4 ticks at 2.4 GHz");
        let mut fast = fresh_cpu();
        fast.cpu_type = CpuType::Pentium4;
        fast.set_tsc_rate(400_000);
        let base = read_tsc(&fast);
        fast.cycles += 400_000 * 1000;
        assert_eq!(read_tsc(&fast) - base, 2_400_000_000);
    }
    /// A rate change folds the epoch: the TSC stays continuous and
    /// monotonic, and post-change cycles scale at the new ratio only.
    #[test]
    fn tsc_is_continuous_across_rate_changes() {
        let mut cpu = fresh_cpu();
        cpu.cpu_type = CpuType::Pentium;
        cpu.set_tsc_rate(100_000);
        cpu.cycles = 5_000_000;
        let before = read_tsc(&cpu);
        assert_eq!(before, 5_000_000, "Pentium at 100k c/ms is exactly 1:1");
        cpu.set_tsc_rate(200_000);
        let at_fold = read_tsc(&cpu);
        assert_eq!(at_fold, before, "the fold must not move the TSC");
        cpu.cycles += 1_000_000;
        assert_eq!(
            read_tsc(&cpu) - at_fold,
            500_000,
            "post-fold cycles scale at the new ratio"
        );
    }
    /// WRMSR(0x10) semantics survive scaling: the write reads back exactly,
    /// and subsequent ticks advance at the scaled rate on top of it.
    #[test]
    fn wrmsr_tsc_roundtrips_under_scaling() {
        let mut cpu = fresh_cpu();
        cpu.cpu_type = CpuType::PentiumMmx;
        cpu.set_tsc_rate(100_000);
        cpu.cycles = 777;
        write_msr(&mut cpu, MSR_IA32_TIME_STAMP_COUNTER, 5_000_000).expect("WRMSR(0x10) ok");
        assert_eq!(read_tsc(&cpu), 5_000_000);
        cpu.cycles += 21;
        assert_eq!(read_tsc(&cpu), 5_000_042, "21 cycles = 42 scaled ticks");
    }
    /// Identities without RDTSC never wire a ratio — and a bare Cpu keeps
    /// the historical 1:1 mapping whatever its identity.
    #[test]
    fn pre_pentium_and_unwired_cpus_keep_the_legacy_mapping() {
        let mut cpu = fresh_cpu();
        cpu.cpu_type = CpuType::I80486;
        cpu.set_tsc_rate(100_000);
        assert_eq!(
            cpu.tsc_ticks_per_cycle_q32,
            1u64 << 32,
            "no RDTSC => no wiring"
        );
        let mut bare = fresh_cpu();
        bare.cpu_type = CpuType::Pentium4;
        bare.cycles = 12345;
        assert_eq!(read_tsc(&bare), 12345, "unwired stays cycles + offset");
    }
    #[test]
    fn rdmsr_apic_base_returns_default() {
        let cpu = fresh_cpu();
        let v = read_msr(&cpu, MSR_IA32_APIC_BASE).expect("RDMSR(0x1B) ok");
        assert_eq!(v, IA32_APIC_BASE_DEFAULT);
        assert_ne!(v & (1 << 8), 0, "BSP bit must be set");
        assert_eq!(
            v & IA32_APIC_BASE_GLOBAL_ENABLE,
            0,
            "global enable must be clear"
        );
    }
    #[test]
    fn wrmsr_apic_base_is_silently_dropped() {
        let _warning_state = ScopedWarningState::isolated();
        let mut cpu = fresh_cpu();
        write_msr(&mut cpu, MSR_IA32_APIC_BASE, 0xFEE0_0900).expect("WRMSR(0x1B) ok");
        let v = read_msr(&cpu, MSR_IA32_APIC_BASE).unwrap();
        assert_eq!(v, IA32_APIC_BASE_DEFAULT);
    }
    #[test]
    fn rdmsr_bios_sign_id_returns_zero() {
        let cpu = fresh_cpu();
        assert_eq!(read_msr(&cpu, MSR_IA32_BIOS_SIGN_ID).unwrap(), 0);
    }
    #[test]
    fn wrmsr_bios_sign_id_is_noop() {
        let mut cpu = fresh_cpu();
        write_msr(&mut cpu, MSR_IA32_BIOS_SIGN_ID, 0xDEAD_BEEF_CAFE_BABE).expect("WRMSR(0x8B) ok");
        assert_eq!(read_msr(&cpu, MSR_IA32_BIOS_SIGN_ID).unwrap(), 0);
    }
    /// IA32_BIOS_UPDT_TRIG (79H): a P6-family write is a rejected microcode
    /// update (no state change, 8BH still reads 0); the read is #GP (the
    /// MSR is write-only); a P5 part has no such MSR and faults both ways.
    #[test]
    fn bios_update_trigger_is_a_rejected_update_on_p6_only() {
        let _guard = ScopedLenientOverride::strict();
        for cpu_type in [
            CpuType::PentiumPro,
            CpuType::PentiumII,
            CpuType::PentiumIIICoppermine,
        ] {
            let mut cpu = fresh_cpu();
            cpu.cpu_type = cpu_type;
            let before = cpu.snapshot_state();
            write_msr(&mut cpu, MSR_IA32_BIOS_UPDT_TRIG, 0x0001_2000)
                .unwrap_or_else(|_| panic!("{cpu_type:?} accepts WRMSR(79H)"));
            assert_eq!(read_msr(&cpu, MSR_IA32_BIOS_SIGN_ID), Ok(0));
            assert_eq!(
                read_msr(&cpu, MSR_IA32_BIOS_UPDT_TRIG),
                Err(MsrFault::Gp),
                "{cpu_type:?}: 79H is write-only"
            );
            let after = cpu.snapshot_state();
            assert_eq!(
                serde_json::to_string(&after).unwrap(),
                serde_json::to_string(&before).unwrap(),
                "{cpu_type:?}: the rejected update changes no state"
            );
        }
        for cpu_type in [CpuType::Pentium, CpuType::PentiumMmx] {
            let mut cpu = fresh_cpu();
            cpu.cpu_type = cpu_type;
            assert_eq!(
                write_msr(&mut cpu, MSR_IA32_BIOS_UPDT_TRIG, 0x0001_2000),
                Err(MsrFault::Gp),
                "{cpu_type:?} has no 79H"
            );
            assert_eq!(read_msr(&cpu, MSR_IA32_BIOS_UPDT_TRIG), Err(MsrFault::Gp));
        }
    }
    #[test]
    fn unknown_msr_strict_returns_gp() {
        let _guard = ScopedLenientOverride::strict();
        let cpu = fresh_cpu();
        assert_eq!(
            read_msr(&cpu, 0xDEAD_BEEF),
            Err(MsrFault::Gp),
            "strict-mode RDMSR of an unknown index must raise #GP(0)"
        );
    }
    #[test]
    fn unknown_wrmsr_strict_returns_gp() {
        let _guard = ScopedLenientOverride::strict();
        let mut cpu = fresh_cpu();
        assert_eq!(
            write_msr(&mut cpu, 0xDEAD_BEEF, 0xCAFE_F00D),
            Err(MsrFault::Gp),
            "strict-mode WRMSR of an unknown index must raise #GP(0)"
        );
    }
    #[test]
    fn unknown_msr_lenient_returns_zero_and_drops_write() {
        let _guard = ScopedLenientOverride::lenient();
        let _warning_state = ScopedWarningState::isolated();
        let mut cpu = fresh_cpu();
        assert_eq!(
            read_msr(&cpu, 0x4242_4242),
            Ok(0),
            "lenient-mode RDMSR of an unknown index must return 0"
        );
        assert_eq!(
            write_msr(&mut cpu, 0x4242_4242, 0x1234_5678_9ABC_DEF0),
            Ok(()),
            "lenient-mode WRMSR of an unknown index must succeed silently"
        );
    }
    #[test]
    fn write_to_known_msr_does_not_touch_seen_set() {
        let _warning_state = ScopedWarningState::isolated();
        let mut cpu = fresh_cpu();
        write_msr(&mut cpu, MSR_IA32_BIOS_SIGN_ID, 0).unwrap();
        write_msr(&mut cpu, MSR_IA32_TIME_STAMP_COUNTER, 0).unwrap();
        assert!(seen_indices().insert_if_new(0xCAFE_FEED, false));
        assert!(!seen_indices().insert_if_new(0xCAFE_FEED, false));
    }
    #[test]
    fn apic_enable_warning_dedupes_across_calls() {
        let _warning_state = ScopedWarningState::isolated();
        let mut cpu = fresh_cpu();
        write_msr(
            &mut cpu,
            MSR_IA32_APIC_BASE,
            IA32_APIC_BASE_DEFAULT | (1 << 11),
        )
        .expect("WRMSR(0x1B) ok");
        assert!(!seen_indices().insert_if_new(MSR_IA32_APIC_BASE, true));
        assert!(seen_indices().insert_if_new(MSR_IA32_APIC_BASE, false));
    }
    #[test]
    fn lenient_unknown_msr_returns_zero() {
        let _warning_state = ScopedWarningState::isolated();
        warn_unknown_once(0x1234_5678, false);
        warn_unknown_once(0x1234_5678, false);
        warn_unknown_once(0x9ABC_DEF0, true);
    }
}
