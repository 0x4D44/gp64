//! The `X86Machine` contract.
//!
//! The interpreter uses this trait for memory and execution policy beyond
//! `MemoryAccess`/`PagedMemory`.
//!
//! Implementations live in other crates, so the trait is `unsafe`: bad
//! pointers or proofs can cause undefined behavior. The hidden supertrait
//! [`__audited::AuditedMachine`] and the allow-list audit in
//! `tests/config_surface.rs` pin the implementing types. Defaults never
//! license raw access: the preludes default to the checked route.

use gp64_types::{IoPort, PagedMemory};

use crate::domain::{AccessIntent, ArchPhys, BackingPage, DirectSpan, GuestPhys, PhysPage};
use crate::MemoryCore;

/// Hidden supertrait that names the audit (LLD 3.1).
#[doc(hidden)]
pub mod __audited {
    /// Implemented only by the machine types the `config_surface` test
    /// `x86_machine_implementations_are_audited` allows.
    ///
    /// # Safety
    /// The implementing type has been audited against the `X86Machine`
    /// contract (LLD 2026.10.02 §3) and added to that test's allow-list.
    pub unsafe trait AuditedMachine {}
}

/// Explicit, stable machine identity, independent of type or function addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MachineId(pub u32);

impl MachineId {
    /// PC `gp64_memory::Memory`.
    pub const PC: Self = Self(1);
    /// Synthetic non-PC machine identity used by contract tests.
    pub const GENERIC: Self = Self(2);
    /// `testing::NullMachine`.
    pub const NULL: Self = Self(0xFFFF);
}

/// The core-owned part of a machine's per-run access policy (LLD 3.2).
pub trait AccessPolicyCore: Copy + core::fmt::Debug + PartialEq + 'static {
    /// A SMC notifier was bound when the policy was captured.
    fn smc_notifier(self) -> bool;
    /// Descriptor snooping was active when the policy was captured. Monotone:
    /// engines OR it with the live `MemoryCore::has_descriptor_snoop` (R-AP3).
    fn descriptor_snoop(self) -> bool;
    /// The mask the engine ANDs into a 32-bit linear address on the unpaged
    /// route (M2 LLD §1.3). PC: the A20 mask (`0xFFFF_FFFF`, or bit 20 clear);
    /// a machine without an address-line gate: `u32::MAX`. For a `u32`
    /// address it equals [`X86Machine::phys_mask`].
    fn linear_mask(self) -> u32;

    // ── diagnostic flags (M2 LLD §8 erratum 11; never guest-visible) ────
    // Captured with the policy so the engine keeps them in registers. A
    // machine without the diagnostic leaves the default `false`, and every
    // engine use compiles out.
    /// Write tracking (PC: the Time Machine) wants register context
    /// captured at each store.
    #[inline(always)]
    fn diag_write_tracking(self) -> bool {
        false
    }
    /// An exact-address or range write trace is configured.
    #[inline(always)]
    fn diag_traced_writes(self) -> bool {
        false
    }
    /// The descriptor-scratch trace is on.
    #[inline(always)]
    fn diag_desc_trace(self) -> bool {
        false
    }
}

/// Route chosen by a write prelude (LLD 3.2, 3.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteRoute {
    /// The engine stores to `core().ram_base_ptr() + a` after the core duties.
    Raw,
    /// The machine performed the guest-visible effect (a native device
    /// commit) without republishing: the engine's `ram`/`tlb` stay valid.
    /// The engine's write observers still run.
    Handled,
    /// The machine performed the store through its own checked path (PC: the
    /// EMS frame byte and the debug Time Machine route), which may republish:
    /// the engine reloads its `ram` pointer, then runs its write observers.
    /// Kept apart from `Checked` so each engine call site keeps today's
    /// inline-versus-cold shape (M2 LLD §8).
    HandledReload,
    /// The machine discarded the write by an aperture rule (PC: the F-segment,
    /// registered ROM pages, the option-ROM window). Nothing happened, and no
    /// write observer runs (M2 LLD §8 erratum: today's funnels return early).
    Dropped,
    /// `W > 1` only: re-enter as `W` byte writes (PC ROM/EMS split).
    SplitBytes,
    /// The engine calls `MemoryAccess::write_*` (slow, side-effecting).
    Checked,
}

/// Route chosen by a read prelude (LLD 3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadRoute {
    /// The engine loads from `core().ram_base_ptr() + a`.
    Raw,
    /// The engine calls `MemoryAccess::read_*`.
    Checked,
}

/// Result of a port-write sync (LLD 3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortSync {
    None,
    /// The machine republished state: the engine refreshes its `ram`/`tlb`
    /// locals.
    Republished,
}

/// Guest cycles a native device commit charges (LLD 3.3: today's
/// `Cpu::vga_write_delay_cycles`, a `u32`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeCommitCost(pub u32);

/// Paging inputs delivered by the engines: CR0.PG, CR3, CR4.PSE and CR4.PAE.
/// The machine uses them to project the guest page tables.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PagingState {
    /// CR0.PG.
    pub pg: bool,
    /// CR3, as loaded.
    pub cr3: u32,
    /// CR4.PSE.
    pub pse: bool,
    /// CR4.PAE.
    pub pae: bool,
}

impl PagingState {
    /// The paging inputs of control registers `cr0`, `cr3` and `cr4`:
    /// CR0.PG (bit 31), CR3 as loaded, CR4.PSE (bit 4) and CR4.PAE (bit 5).
    #[inline]
    #[must_use]
    pub const fn from_control_registers(cr0: u32, cr3: u32, cr4: u32) -> Self {
        Self {
            pg: cr0 & (1 << 31) != 0,
            cr3,
            pse: cr4 & (1 << 4) != 0,
            pae: cr4 & (1 << 5) != 0,
        }
    }
}

/// The machine contract the interpreter is generic over.
///
/// # Safety
/// Implementing this trait asserts the full per-item contract of LLD
/// 2026.10.02 §3.3: every proof and route satisfies LLD 1.4 V1-V6 and D2
/// for its stated intent. `core` returns the same embedded core for the
/// machine's whole life, `CORE_OFFSET` is its byte offset in `Self`, and
/// access-policy refresh rules R-AP1-R-AP5 hold. The type must also be on
/// the audit allow-list.
pub unsafe trait X86Machine:
    PagedMemory + __audited::AuditedMachine + Sized + 'static
{
    // ── identity ────────────────────────────────────────────────────────
    /// Unique among audited machines; stable across builds.
    const MACHINE_ID: MachineId;

    // ── associated types ────────────────────────────────────────────────
    /// Per-run captured access policy (LLD 3.4). `Copy`, no references, no
    /// interior mutability.
    type AccessPolicy: AccessPolicyCore;
    /// Work a device posted during the run, returned at the boundary.
    type Posted: Default;

    // ── capability consts (monomorphisation-time; never a runtime flag) ──
    /// Seam B exactly-once dispatch, completion latch and posted work.
    const DEVICE_DISPATCH: bool;
    /// PC VGA chain-4/planar native commit (LLD 3.6).
    const NATIVE_DEVICE_COMMIT: bool;
    /// Time Machine capable; the runtime switch stays in the access policy.
    const WRITE_TRACKING: bool;
    /// `PagedMemory::map_linear_physical` can return `Some`.
    const DPMI_MAPPINGS: bool;

    // ── core view ───────────────────────────────────────────────────────
    /// The one embedded core. Hot.
    fn core(&self) -> &MemoryCore;
    /// Byte offset of the embedded core [`Self::core`] returns, from the
    /// start of `Self`. Every machine states its own layout using
    /// `core::mem::offset_of!(Self, core)`.
    const CORE_OFFSET: usize;

    // ── policy capture (R3) ─────────────────────────────────────────────
    /// Capture the access policy at run entry. Must equal what every hot item
    /// would compute live at that moment.
    fn access_policy(&self) -> Self::AccessPolicy;
    /// `ArchPhys -> GuestPhys`, a pure function of the policy. Hot.
    fn phys_mask(p: Self::AccessPolicy, a: ArchPhys) -> GuestPhys;
    /// `ArchPhys -> GuestPhys` under the live machine state: what
    /// [`Self::phys_mask`] computes under a policy captured now.
    /// Machines may override this to avoid the cost or side effects of policy
    /// capture (PC uses the bare A20 mask).
    #[inline(always)]
    fn mask_phys(&self, a: ArchPhys) -> GuestPhys {
        Self::phys_mask(self.access_policy(), a)
    }
    /// Upper bound of the raw-RAM window: `[0, raw_limit)`, minus the
    /// preludes' holes, is identity RAM. Read live. Hot.
    fn raw_limit(&self) -> usize;
    /// The bulk raw window for unpaged REP string fast paths (M2 LLD G2):
    /// `Some(limit)` when every byte of `[0, limit)` is identity RAM that no
    /// prelude effect applies to, so a whole span below it may be stored or
    /// copied raw after the core duties; `None` when no bulk span qualifies
    /// under this policy. Static and hot.
    fn bulk_raw_limit(p: Self::AccessPolicy) -> Option<usize>;

    // ── ordered access preludes (LLD 3.5) ───────────────────────────────
    /// Run the machine's write effects for a `W`-byte store, in today's order,
    /// and choose the route. Class MR: any route but `Raw` may republish.
    ///
    /// Default (M1, no consumer until M2): `Checked`, which is always correct.
    ///
    /// # Safety
    /// The caller performs a raw host store only on `Raw`, at
    /// `core().ram_base_ptr() + a`, after the core duties.
    #[inline]
    unsafe fn write_prelude<const W: usize>(
        &mut self,
        _p: Self::AccessPolicy,
        _a: GuestPhys,
        _v: u64,
    ) -> WriteRoute {
        WriteRoute::Checked
    }

    /// Choose the route for a `W`-byte load. Class PS. It reads machine state
    /// live and takes no captured policy: the interpreter's read funnels have
    /// no policy in scope, and no read route depends on a captured field (M2
    /// LLD §8 erratum to LLD 3.2).
    ///
    /// Default: `Checked`, which is always correct.
    ///
    /// # Safety
    /// The caller performs a raw host load only on `Raw`.
    #[inline]
    unsafe fn read_prelude<const W: usize>(&self, _a: GuestPhys) -> ReadRoute {
        ReadRoute::Checked
    }

    /// Diagnostics after a raw `W`-byte store of `v`; never changes
    /// guest-visible state. Default: nothing. (The value is needed by the PC
    /// selector diagnostic; M2 LLD §8 erratum to LLD 3.2's signature.)
    #[inline]
    fn write_epilogue<const W: usize>(&self, _p: Self::AccessPolicy, _a: GuestPhys, _v: u64) {}

    // ── fetch ───────────────────────────────────────────────────────────
    /// What an instruction fetch at or above `raw_limit` reads.
    /// `true`: open bus (`0xFF`) per byte with no bus access, the PC chipset's answer above
    /// installed RAM. `false`: the
    /// engine fetches through `direct_span`/`fetch_checked`, so a machine with
    /// a real ROM overlay at the top of 4 GiB decodes
    /// its bytes. A monomorphisation-time constant; never a runtime flag.
    const OPEN_BUS_FETCH_ABOVE_RAW_LIMIT: bool;
    /// The machine's fetch-only alias (PC: the reset-ROM fold). Hot.
    fn fetch_alias(&self, a: GuestPhys) -> GuestPhys;
    /// A direct-access proof for `[a, a + len)` at `intent`, or `None` when
    /// the span must take the checked path. Hot (fetch revalidation).
    fn direct_span(&self, a: GuestPhys, len: usize, intent: AccessIntent) -> Option<DirectSpan>;
    /// Byte-exact guest fetch with every side effect a fetch has. Cold.
    fn fetch_checked(&self, a: GuestPhys, dst: &mut [u8]);
    /// Host base of the machine's RAM backing. The pointer proves nothing by
    /// itself: every guest-visible access needs a `direct_span` or TLB direct
    /// proof first. Defaults to the core's published RAM base.
    #[inline]
    fn ram_base(&self) -> *const u8 {
        self.core().ram_base_ptr().cast_const()
    }
    /// Host pointer for a `width`-byte read at `linear` served by a plain
    /// host load from a warm TLB entry, or `None` when the read must take the
    /// checked path. Valid only until the next mutation of the machine.
    /// Default: the core's TLB predicate
    /// ([`MemoryCore::tlb_direct_read_ptr`]); a machine with a
    /// content-dependent read filter refuses first. Hot.
    #[inline]
    fn tlb_direct_read_ptr(&self, linear: u32, width: usize, cpl3: bool) -> Option<*const u8> {
        self.core().tlb_direct_read_ptr(linear, width, cpl3)
    }
    /// Host pointer and guest physical address for a `width`-byte store at
    /// `linear` the caller commits at once through the pointer, or `None`
    /// when the store must take the checked path (watched and PT-frame
    /// pages included). Valid only until the next mutation of the machine.
    /// Default: the core's TLB predicate
    /// ([`MemoryCore::tlb_direct_write_span`]). Hot.
    #[inline]
    fn tlb_direct_write_span(
        &self,
        linear: u32,
        width: usize,
        cpl3: bool,
    ) -> Option<(*mut u8, usize)> {
        self.core().tlb_direct_write_span(linear, width, cpl3)
    }

    // ── code-page watches and PT tripwire ───────────────────────────────
    /// The canonical backing page for a RAM-class physical page.
    fn backing_page(&self, a: PhysPage) -> Option<BackingPage>;
    /// Watch or unwatch a backing page; applies machine projections when a
    /// bit flips. Class PS.
    fn set_page_watched(&mut self, page: BackingPage, on: bool);
    /// The SMC/PT-tripwire/decode-cache core duty before a store lands.
    fn smc_before_write(&mut self, a: GuestPhys, len: usize) -> bool;
    /// Record the side effects of a direct RAM write.
    fn note_direct_write(&mut self, a: GuestPhys, len: usize);
    /// The PT-write tripwire for a store the caller commits itself: bump
    /// the PT-write epoch when `[a, a + len)` touches a recorded
    /// paging-structure frame. A core duty, routed through the machine
    /// because there is no `core_mut` (LLD 3.1).
    fn pt_tripwire_note_physical_write(&mut self, a: GuestPhys, len: usize);
    /// Bind (`Some`) or remove (`None`) the SMC notifier the store funnels
    /// report code-page writes to: the core's, plus any machine-side copy
    /// (PC: the HLE RAM writer's). Class PS. Cold.
    fn smc_bind_notifier(&mut self, notifier: Option<std::sync::Arc<dyn gp64_types::SmcNotify>>);
    /// Whether any page is watched for code, so the funnels may skip the
    /// watch test while none is: the core's flag, plus any machine-side
    /// interest derived from it (PC: `smc_interest`). Class PS. Cold.
    fn smc_set_has_code_pages(&mut self, has_pages: bool);

    // ── PC device commit (LLD 3.6) ──────────────────────────────────────
    /// Admitted native device store. Default: refuse.
    #[inline]
    fn native_device_store(&mut self, _a: GuestPhys, _v: u64, _w: u8) -> Option<NativeCommitCost> {
        None
    }
    /// Admitted native device REP store. Default: refuse.
    #[inline]
    fn native_device_rep_store(
        &mut self,
        _a: GuestPhys,
        _bytes: &[u8],
    ) -> Option<NativeCommitCost> {
        None
    }

    // ── device write charging (M2 LLD G1) ───────────────────────────────
    /// The machine charges guest cycles per device bus write (PC: the VGA
    /// write delay). `false` compiles the engine's charging arithmetic out.
    const DEVICE_WRITE_DELAY: bool;
    /// Monotone count of device bus writes the delay applies to. Read once
    /// per retired instruction when `DEVICE_WRITE_DELAY`. Hot. Default: 0.
    #[inline]
    fn device_write_count(&self) -> u64 {
        0
    }

    // ── boundary and device items (LLD 3.8) ─────────────────────────────
    /// True iff the run must stop at the next instruction boundary. Hot.
    fn device_transaction_pending(&self) -> bool;
    /// Called only when `DEVICE_DISPATCH`. Returns owned posted work.
    #[inline]
    fn complete_device_boundary(&mut self) -> Self::Posted {
        Self::Posted::default()
    }
    /// `DEVICE_DISPATCH` only.
    #[inline]
    fn set_device_instruction_context(&mut self, _eip: u32, _guest_time: u64) {}
    /// True iff a page-walk read of `[a, a + len)` dispatches to no device.
    fn physical_span_is_passive(&self, a: GuestPhys, len: usize) -> bool;
    /// The machine knows when the interrupt-poll boundary an IF 0->1
    /// transition or an interrupt shadow produces (STI, POPF/POPFD, POP SS)
    /// would find nothing to do, so the interpreter may continue the run
    /// instead of returning ([`Self::poll_boundary_wanted`]). `false` keeps
    /// every such exit (the PC: its interrupt controller is not visible from
    /// memory).
    const QUIET_POLL_BOUNDARY: bool;
    /// Whether the board wants the next interrupt-poll boundary: an interrupt
    /// may be requested, or the boundary has other work. Read only when
    /// `QUIET_POLL_BOUNDARY`, after the producing instruction retires. Hot.
    /// Default: always.
    #[inline]
    fn poll_boundary_wanted(&self) -> bool {
        true
    }

    // ── port-write sync ─────────────────────────────────────────────────
    /// Sync machine state after an OUT. Class MR. Default: nothing.
    #[inline]
    fn after_port_write<IO: IoPort>(&mut self, _io: &mut IO, _port: u16) -> PortSync {
        PortSync::None
    }

    // ── descriptor-table snoop (M2 LLD §8 erratum 9) ────────────────────
    // A core duty every x86 machine has. The reads (bases, ends,
    // `take_*_dirty`) stay on `core()`; these are the mutations, which go
    // through the machine because there is no `core_mut` (LLD 3.1).
    /// LGDT: snoop `[base, base + limit]` for descriptor writes.
    fn descriptor_snoop_set_gdt(&mut self, base: u32, limit: u16);
    /// LLDT: snoop `[base, base + limit]` for descriptor writes.
    fn descriptor_snoop_set_ldt(&mut self, base: u32, limit: u32);
    /// LLDT with a null selector: stop snooping the LDT.
    fn descriptor_snoop_clear_ldt(&mut self);
    /// The write funnels' descriptor snoop for a `write_size`-byte store of
    /// `val` at physical `addr`: marks a touched table dirty. Hot (every
    /// funnel store that the engine's cached snoop flag admits). Default: the
    /// core duty alone, behind the snoop hull.
    #[inline(always)]
    fn snoop_descriptor_write(&self, addr: usize, val: u64, write_size: u8) {
        let _ = val;
        let write_len = usize::from(write_size).max(1);
        let (lo, hi) = self.core().snoop_hull();
        if addr >= hi || addr.saturating_add(write_len) <= lo {
            return;
        }
        let _ = self.core().snoop_descriptor_write(addr, write_len);
    }

    // ── paging state ────────────────────────
    /// The engine reports the paging state: on a CR0 write that flips PG, on
    /// every MOV CR3 and MOV CR4, and once at run entry. The host reports
    /// task-switch CR3 loads and its own mode changes. Default: nothing, so a
    /// machine without a page-table projection compiles every call out.
    ///
    /// The interpreter delivers only the MOV CR0/CR3/CR4 arms, under
    /// `DEVICE_DISPATCH`. Run entry and task-switch CR3 loads are host
    /// deliveries: the interpreter performs no task switch itself (it returns
    /// a `TaskSwitch*` result and the host runs the shared delivery code), so
    /// the host delivers from `cpu.regs` before each engine entry.
    #[inline(always)]
    fn note_paging_state(&mut self, s: PagingState) {
        let _ = s;
    }

    // ── DPMI linear-to-physical mappings ────────────────────────────────
    /// True iff a linear-to-physical mapping is registered, so
    /// `PagedMemory::map_linear_physical` may answer `Some` and an unpaged
    /// translation need not be page-uniform. Read only when `DPMI_MAPPINGS`.
    /// Default: none.
    #[inline]
    fn has_linear_physical_mappings(&self) -> bool {
        false
    }

    // ── raw RAM proof for bulk stores (M2 LLD §8 erratum 10) ────────────
    /// True iff `[a, a + len)` is plain identity RAM that the engine may
    /// copy or fill raw (after its own SMC and snoop duties), as the paged
    /// REP string bulk paths need. Unlike `direct_span` with a read or write
    /// intent, it carries no access withholds. Default `false`: a machine
    /// that does not opt in takes the checked path (R2).
    #[inline]
    fn raw_ram_span(&self, a: GuestPhys, len: usize) -> bool {
        let _ = (a, len);
        false
    }

    // ── diagnostics (M2 LLD §8 erratum 9; never guest-visible) ──────────
    // `diag_*` is the allow-listed name space for machine diagnostics in
    // the engines. Defaults are empty, so they vanish where a machine has
    // none.
    /// Trace the descriptor-scratch bytes of `[start, start + len)` after a
    /// bulk store.
    #[inline(always)]
    fn diag_desc_scratch_range(&self, start: usize, len: usize) {
        let _ = (start, len);
    }
    /// Trace a bulk linear write range from `site`.
    #[inline(always)]
    fn diag_linear_write_range(&self, start: usize, len: usize, site: &'static str) {
        let _ = (start, len, site);
    }
    /// The selector diagnostic over a bulk store.
    #[inline(always)]
    fn diag_sel_bulk(&self, addr: usize, len: usize) {
        let _ = (addr, len);
    }
    /// Count a store served by reusing a TLB-held device translation.
    #[inline(always)]
    fn diag_count_tlb_device_reuse(&mut self) {}
    /// Arm the selector diagnostic's dynamic write trap at `target`.
    #[inline(always)]
    fn diag_set_sel_target(&self, target: u32) {
        let _ = target;
    }
    /// A diagnostic peek of one physical byte with no device side effect.
    /// Default: core RAM below `raw_limit`, else 0.
    #[inline]
    fn diag_peek_phys_u8(&self, addr: usize) -> u8 {
        if addr < self.raw_limit() {
            // SAFETY: `[0, raw_limit)` is backed by the core's RAM (LLD 3.3,
            // `raw_limit`), and `addr` is inside it.
            unsafe { *self.core().ram_base_ptr().add(addr) }
        } else {
            0
        }
    }
}
