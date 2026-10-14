//! HLE fastmem stage 2 (HLD `2026.09.24 - HLD - HLE fastmem for the DRC`):
//! the JIT alias view of guest RAM and its host-protection projection.
//!
//! On the HLE flat-memory route guest linear = physical - [`FASTMEM_PHYS_BIAS`], so
//! a second host mapping of the whole RAM reservation, offset by that bias,
//! puts every guest linear byte at `jit_base + linear`. Rust keeps using
//! `Memory::ram` (the unprotected view); only emitted code will use the alias.
//!
//! The alias carries guest protections. They are a **derived projection** of
//! the flat-memory page tables, recomputed at exactly the page-table mutators
//! (the page-table operations `map_range*`, `protect_range`, `unmap_range`), the
//! SMC watch ([`Memory::set_page_watched`]), RAM resizes and snapshot restore.
//! Per 4 KiB guest page the rule is:
//!
//! * **none** unless the PDE and PTE are both present and user, the PTE maps
//!   the page to `linear + FASTMEM_PHYS_BIAS` (the alias only shows that
//!   physical page), and the whole physical page is ordinary RAM
//!   (`Memory::direct_host_ram_range`, the flat read view's admission);
//! * **read-write** if both entries are writable and the physical page is not
//!   SMC-watched;
//! * **read** otherwise.
//!
//! A host page (16 KiB on Apple Silicon) gets the **intersection** (the
//! strictest) of its guest pages' permissions; a host page whose guest pages
//! differ is "mixed" and will send accesses to its looser part down the slow
//! path. A projection oracle checks the host's
//! actual protections against this rule using the page tables' own reader.
//!
//! Stage 3 (the `gp64-fastmem` crate, the a64 DRC) emits guest loads against
//! [`Memory::fastmem_jit_base`]; a load the projection refuses faults into its
//! checked fallback. The alias exists only on macOS (`mach_vm_remap`); other
//! hosts report fastmem unavailable and every hook is a no-op.

use crate::Memory;
use gp64_fastmem::{FastmemFaultReason, FASTMEM_PHYS_BIAS};

/// Bytes of guest linear address space the JIT window covers.
pub const FASTMEM_LINEAR_WINDOW_BYTES: u64 = 1 << 32;

const GUEST_PAGE_SIZE: usize = 4096;
const GUEST_PAGE_COUNT: usize = 1 << 20;

const PTE_PRESENT: u32 = 1 << 0;
const PTE_RW: u32 = 1 << 1;
const PTE_USER: u32 = 1 << 2;

/// Host protection of one page of the JIT window. Ordered from strictest to
/// loosest, so `min` is the intersection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum FastmemPerm {
    /// No access: every JIT access faults to the slow path.
    None = 0,
    /// Loads may use the window; stores fault.
    Read = 1,
    /// Loads and stores may use the window.
    ReadWrite = 2,
}

impl FastmemPerm {
    fn from_bits(bits: u8) -> Self {
        match bits & STATE_PERM_MASK {
            2 => Self::ReadWrite,
            1 => Self::Read,
            _ => Self::None,
        }
    }
}

/// Fastmem projection counters. The `*_host_pages` fields describe the
/// current projection; the rest are cumulative.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FastmemStats {
    /// Host page size of the window, in bytes.
    pub host_page_size: usize,
    /// Projection updates (one per mutator, watch flip, resize or restore).
    pub projection_updates: u64,
    /// Updates that re-derived the whole window (enable, resize, restore).
    pub full_projections: u64,
    /// Host pages whose protection changed.
    pub host_pages_reprotected: u64,
    /// Host `mprotect` calls issued (contiguous runs are coalesced).
    pub protect_calls: u64,
    /// Host pages whose guest pages carry different permissions.
    pub mixed_host_pages: u64,
    /// Host pages currently read-only.
    pub read_host_pages: u64,
    /// Host pages currently read-write.
    pub read_write_host_pages: u64,
}

const STATE_PERM_MASK: u8 = 0x03;
const STATE_MIXED: u8 = 0x80;

/// The alias mapping plus the applied projection. Owned by [`Memory`].
pub(crate) struct FastmemView {
    alias: host::AliasMapping,
    page_dir_phys: u32,
    host_page: usize,
    guest_pages_per_host: usize,
    /// Applied state per host page of the linear window: the permission in
    /// bits 0-1, [`STATE_MIXED`] in bit 7.
    state: Box<[u8]>,
    stats: FastmemStats,
    /// Forced-fault oracle (stage 3 validation): project every page none, so
    /// every emitted fastmem access faults into its checked fallback.
    force_fault: bool,
}

/// `MDEM_DRC=fastmem=force_fault`: start every fastmem view in forced-fault
/// mode (the HLD's forced-fault oracle on a whole run).
fn force_fault_requested() -> bool {
    gp64_types::drc_env::drc("fastmem") == Some("force_fault")
}

impl FastmemView {
    fn window_host_pages(&self) -> usize {
        (FASTMEM_LINEAR_WINDOW_BYTES as usize) / self.host_page
    }

    /// Record `(perm, mixed)` for host page `h`; true when the host
    /// protection must change.
    fn record(&mut self, h: usize, perm: FastmemPerm, mixed: bool) -> bool {
        let old = self.state[h];
        let new = perm as u8 | if mixed { STATE_MIXED } else { 0 };
        if old == new {
            return false;
        }
        let old_perm = FastmemPerm::from_bits(old);
        match old_perm {
            FastmemPerm::Read => self.stats.read_host_pages -= 1,
            FastmemPerm::ReadWrite => self.stats.read_write_host_pages -= 1,
            FastmemPerm::None => {}
        }
        match perm {
            FastmemPerm::Read => self.stats.read_host_pages += 1,
            FastmemPerm::ReadWrite => self.stats.read_write_host_pages += 1,
            FastmemPerm::None => {}
        }
        if old & STATE_MIXED != 0 {
            self.stats.mixed_host_pages -= 1;
        }
        if mixed {
            self.stats.mixed_host_pages += 1;
        }
        self.state[h] = new;
        old_perm != perm
    }

    /// Apply `perm` to host pages `[first, first + count)` of the window.
    fn protect(&mut self, first: usize, count: usize, perm: FastmemPerm) {
        let offset = FASTMEM_PHYS_BIAS + first * self.host_page;
        self.alias.protect(offset, count * self.host_page, perm);
        self.stats.protect_calls += 1;
        self.stats.host_pages_reprotected += count as u64;
    }
}

impl Memory {
    pub(crate) fn fastmem_detach_arena_alias(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(view) = self.fastmem.as_ref() {
            view.alias.unmap_arena();
        }
    }

    /// A late macOS attachment must replace the stale arena submap in both
    /// the source reservation and the already-created JIT alias.
    pub(crate) fn fastmem_attach_arena_alias(
        &mut self,
        store: &crate::arena::ArenaStore,
    ) -> std::io::Result<()> {
        #[cfg(target_os = "macos")]
        if let Some(view) = self.fastmem.as_mut() {
            view.alias.map_arena(store)?;
            let first = 0x8000_0000usize / view.host_page;
            let last = 0xC010_0000usize / view.host_page;
            // MAP_FIXED starts the new submap read/write. Revoke it before
            // evaluating mixed permissions within 16 KiB host pages.
            view.alias.protect(
                crate::arena::ARENA_PHYS_START,
                crate::arena::ARENA_STORE_BYTES,
                FastmemPerm::None,
            );
            for h in first..last.min(view.state.len()) {
                let old = view.state[h];
                match FastmemPerm::from_bits(old) {
                    FastmemPerm::Read => view.stats.read_host_pages -= 1,
                    FastmemPerm::ReadWrite => view.stats.read_write_host_pages -= 1,
                    FastmemPerm::None => {}
                }
                if old & STATE_MIXED != 0 {
                    view.stats.mixed_host_pages -= 1;
                }
                view.state[h] = FastmemPerm::None as u8;
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = store;
        Ok(())
    }

    /// Create the JIT alias view of guest RAM (HLE flat-memory route) and project
    /// the page tables rooted at `page_dir_phys` onto it. Idempotent: a second
    /// call re-roots and re-derives the whole projection. Returns whether
    /// fastmem is available on this host.
    pub fn enable_fastmem_view(&mut self, page_dir_phys: u32) -> bool {
        if self.fastmem.is_none() {
            let Some(view) = self.create_fastmem_view(page_dir_phys) else {
                return false;
            };
            self.fastmem = Some(view);
        }
        if let Some(view) = self.fastmem.as_mut() {
            view.page_dir_phys = page_dir_phys;
        }
        self.fastmem_reproject_all();
        true
    }

    fn create_fastmem_view(&mut self, page_dir_phys: u32) -> Option<FastmemView> {
        let host_page = crate::host_page_size();
        if host_page < GUEST_PAGE_SIZE {
            return None;
        }
        if !host_page.is_multiple_of(GUEST_PAGE_SIZE)
            || !FASTMEM_PHYS_BIAS.is_multiple_of(host_page)
        {
            return None;
        }
        let base = self.ram.stable_reservation_base()?;
        let alias = host::AliasMapping::new(base)?;
        let window_host_pages = (FASTMEM_LINEAR_WINDOW_BYTES as usize) / host_page;
        Some(FastmemView {
            alias,
            page_dir_phys,
            host_page,
            guest_pages_per_host: host_page / GUEST_PAGE_SIZE,
            state: vec![FastmemPerm::None as u8; window_host_pages].into_boxed_slice(),
            stats: FastmemStats {
                host_page_size: host_page,
                ..FastmemStats::default()
            },
            force_fault: force_fault_requested(),
        })
    }

    /// True once the JIT alias view exists (HLE flat-memory route on a supported
    /// host). Stage 3 emits fastmem accesses only when this holds.
    #[inline]
    pub fn fastmem_available(&self) -> bool {
        self.fastmem.is_some()
    }

    /// `JIT_GBASE`: guest linear address `L` is at `jit_base + L` in the alias
    /// view (for every 32-bit `L`), with guest protections applied. `None`
    /// when fastmem is unavailable.
    #[inline]
    pub fn fastmem_jit_base(&self) -> Option<*mut u8> {
        self.fastmem
            .as_ref()
            .map(|view| view.alias.base().wrapping_add(FASTMEM_PHYS_BIAS))
    }

    /// Forced-fault oracle switch: when on, every page of the JIT window is
    /// projected none, so every emitted fastmem access faults into its checked
    /// fallback. No-op without a view.
    pub fn set_fastmem_force_fault(&mut self, on: bool) {
        let Some(view) = self.fastmem.as_mut() else {
            return;
        };
        if view.force_fault != on {
            view.force_fault = on;
            self.fastmem_reproject_all();
        }
    }

    /// Whether the forced-fault oracle is on.
    #[must_use]
    pub fn fastmem_force_fault(&self) -> bool {
        self.fastmem.as_ref().is_some_and(|view| view.force_fault)
    }

    /// Why a fastmem load of `width` bytes at guest `linear` faults now, by
    /// the projection rule (census classification; `None` without a view).
    #[must_use]
    pub fn fastmem_fault_reason(&self, linear: u32, width: u32) -> Option<FastmemFaultReason> {
        let view = self.fastmem.as_ref()?;
        if view.force_fault {
            return Some(FastmemFaultReason::Forced);
        }
        let end = u64::from(linear) + u64::from(width.max(1)) - 1;
        if end >= FASTMEM_LINEAR_WINDOW_BYTES {
            return Some(FastmemFaultReason::BeyondWindow);
        }
        let pd = view.page_dir_phys as usize;
        let user = PTE_PRESENT | PTE_USER;
        // The first page of the access the window refuses decides.
        let mut page = (linear >> 12) as usize;
        let last = (end >> 12) as usize;
        loop {
            let guest = self.projected_guest_page(pd, page);
            if guest == FastmemPerm::None {
                let pde = read_u32(&self.ram, pd + (page >> 10) * 4).unwrap_or(0);
                let pte = if pde & PTE_PRESENT != 0 {
                    read_u32(&self.ram, (pde & 0xFFFF_F000) as usize + (page & 0x3FF) * 4)
                        .unwrap_or(0)
                } else {
                    0
                };
                return Some(if pde & PTE_PRESENT == 0 || pte & PTE_PRESENT == 0 {
                    FastmemFaultReason::NotPresent
                } else if pde & pte & user != user {
                    FastmemFaultReason::Supervisor
                } else {
                    FastmemFaultReason::NotRam
                });
            }
            let host = page / view.guest_pages_per_host;
            if FastmemPerm::from_bits(view.state[host]) == FastmemPerm::None {
                return Some(FastmemFaultReason::MixedHostPage);
            }
            if page == last {
                return Some(FastmemFaultReason::NowReadable);
            }
            page += 1;
        }
    }

    /// The fault-registry slot of this view's JIT window: the handler queues
    /// this view's faults under it
    /// ([`gp64_fastmem::drain_window_faults`]).
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[must_use]
    pub fn fastmem_window_slot(&self) -> Option<usize> {
        self.fastmem.as_ref().map(|view| view.alias.window_slot())
    }

    /// Physical address of the page directory the projection reads.
    pub fn fastmem_page_dir_phys(&self) -> Option<u32> {
        self.fastmem.as_ref().map(|view| view.page_dir_phys)
    }

    /// Projection counters, or `None` when fastmem is unavailable.
    pub fn fastmem_stats(&self) -> Option<FastmemStats> {
        self.fastmem.as_ref().map(|view| view.stats)
    }

    /// Page-table mutator hook: re-derive the host pages covering guest
    /// linear pages `[linear_start >> 12, + num_pages)`.
    pub fn fastmem_reproject_linear(&mut self, linear_start: u32, num_pages: u32) {
        let Some(view) = self.fastmem.as_ref() else {
            return;
        };
        if num_pages == 0 {
            return;
        }
        let host_page = view.host_page as u64;
        let start = u64::from(linear_start & !0xFFF);
        let end = (start + u64::from(num_pages) * GUEST_PAGE_SIZE as u64)
            .min(FASTMEM_LINEAR_WINDOW_BYTES);
        let first = (start / host_page) as usize;
        let last = end.div_ceil(host_page) as usize;
        self.fastmem_reproject_host_pages(first, last, false);
    }

    /// SMC-watch hook: re-derive the host page showing `physical_page`.
    pub(crate) fn fastmem_reproject_physical_page(&mut self, physical_page: u32) {
        let phys = (physical_page as usize) << 12;
        let Some(linear) = phys.checked_sub(FASTMEM_PHYS_BIAS) else {
            return;
        };
        if linear as u64 >= FASTMEM_LINEAR_WINDOW_BYTES {
            return;
        }
        self.fastmem_reproject_linear(linear as u32, 1);
    }

    /// Re-derive the shared arena's linear range (admission moved).
    pub(crate) fn fastmem_reproject_arena(&mut self) {
        const ARENA_LINEAR: u32 = (crate::arena::ARENA_PHYS_START - FASTMEM_PHYS_BIAS) as u32;
        const ARENA_PAGES: u32 = 0x4000_0000 / GUEST_PAGE_SIZE as u32;
        self.fastmem_reproject_linear(ARENA_LINEAR, ARENA_PAGES);
    }

    /// Re-derive the whole window (enable, RAM resize, snapshot restore).
    pub(crate) fn fastmem_reproject_all(&mut self) {
        let Some(view) = self.fastmem.as_ref() else {
            return;
        };
        let pages = view.window_host_pages();
        self.fastmem_reproject_host_pages(0, pages, true);
    }

    fn fastmem_reproject_host_pages(&mut self, first: usize, last: usize, full: bool) {
        let Some(mut view) = self.fastmem.take() else {
            return;
        };
        view.stats.projection_updates += 1;
        if full {
            view.stats.full_projections += 1;
        }
        let per_host = view.guest_pages_per_host;
        let pd = view.page_dir_phys as usize;
        let forced = view.force_fault;
        // Pending coalesced `mprotect` run: (first host page, count, perm).
        let mut run: Option<(usize, usize, FastmemPerm)> = None;
        let mut h = first;
        while h < last {
            let g0 = h * per_host;
            // A PDE that is not present-and-user makes its whole 4 MiB none;
            // skip the per-page walk there (most of the window).
            let pde = read_u32(&self.ram, pd + (g0 >> 10) * 4).unwrap_or(0);
            let absent = forced || pde & (PTE_PRESENT | PTE_USER) != PTE_PRESENT | PTE_USER;
            let block_end = if absent {
                ((((g0 >> 10) + 1) << 10) / per_host).clamp(h + 1, last)
            } else {
                h + 1
            };
            for hp in h..block_end {
                let (perm, mixed) = if absent {
                    (FastmemPerm::None, false)
                } else {
                    self.projected_host_page(pd, hp * per_host, per_host)
                };
                if view.record(hp, perm, mixed) {
                    match run {
                        Some((start, count, p)) if p == perm && start + count == hp => {
                            run = Some((start, count + 1, p));
                        }
                        _ => {
                            if let Some((start, count, p)) = run {
                                view.protect(start, count, p);
                            }
                            run = Some((hp, 1, perm));
                        }
                    }
                }
            }
            h = block_end;
        }
        if let Some((start, count, p)) = run {
            view.protect(start, count, p);
        }
        self.fastmem = Some(view);
    }

    /// Intersection of the guest pages `[g0, g0 + count)`, and whether they
    /// differ.
    fn projected_host_page(&self, pd: usize, g0: usize, count: usize) -> (FastmemPerm, bool) {
        let first = self.projected_guest_page(pd, g0);
        let mut perm = first;
        let mut mixed = false;
        for g in g0 + 1..g0 + count {
            let p = self.projected_guest_page(pd, g);
            mixed |= p != first;
            perm = perm.min(p);
        }
        (perm, mixed)
    }

    /// The projection rule for one 4 KiB guest linear page (module docs).
    fn projected_guest_page(&self, pd: usize, linear_page: usize) -> FastmemPerm {
        debug_assert!(linear_page < GUEST_PAGE_COUNT);
        let user = PTE_PRESENT | PTE_USER;
        let Some(pde) = read_u32(&self.ram, pd + (linear_page >> 10) * 4) else {
            return FastmemPerm::None;
        };
        if pde & user != user {
            return FastmemPerm::None;
        }
        let pte_addr = (pde & 0xFFFF_F000) as usize + (linear_page & 0x3FF) * 4;
        let Some(pte) = read_u32(&self.ram, pte_addr) else {
            return FastmemPerm::None;
        };
        if pte & user != user {
            return FastmemPerm::None;
        }
        let phys = (pte & 0xFFFF_F000) as usize;
        // Ordinary RAM only: the same whole-page admission the flat read view
        // publishes behind (`direct_host_ram_range`: usable RAM, no VGA, LFB,
        // ROM, EMS frame or MMIO overlap).
        if phys != (linear_page << 12) + FASTMEM_PHYS_BIAS
            || self.direct_host_ram_range(phys, GUEST_PAGE_SIZE).is_none()
        {
            return FastmemPerm::None;
        }
        if pde & pte & PTE_RW == 0 || self.is_page_watched((phys >> 12) as u32) {
            FastmemPerm::Read
        } else {
            FastmemPerm::ReadWrite
        }
    }

    /// The host's actual protection of every host page of the JIT window, as
    /// the OS reports it (not the projection's own bookkeeping). For oracles.
    pub fn fastmem_host_protections(&self) -> Option<Vec<FastmemPerm>> {
        let view = self.fastmem.as_ref()?;
        view.alias
            .query(FASTMEM_PHYS_BIAS, view.window_host_pages(), view.host_page)
    }

    /// Query one host page for a focused arena projection oracle.
    pub fn fastmem_host_protection_at(&self, linear: u32) -> Option<FastmemPerm> {
        let view = self.fastmem.as_ref()?;
        let page = linear as usize / view.host_page;
        view.alias
            .query(FASTMEM_PHYS_BIAS + page * view.host_page, 1, view.host_page)?
            .into_iter()
            .next()
    }
}

#[inline]
fn read_u32(ram: &crate::RamBacking, addr: usize) -> Option<u32> {
    let bytes = ram.get(addr..addr.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

#[cfg(target_os = "macos")]
mod host {
    use super::FastmemPerm;
    use gp64_fastmem::RAM_RESERVATION_BYTES;

    type MachVmAddress = u64;

    const VM_FLAGS_ANYWHERE: libc::c_int = 0x0001;
    const VM_INHERIT_NONE: libc::vm_inherit_t = 2;
    const VM_REGION_BASIC_INFO_64: libc::c_int = 9;
    const KERN_SUCCESS: libc::c_int = 0;

    /// `vm_region_basic_info_64` (`#pragma pack(4)` in `<mach/vm_region.h>`).
    #[repr(C, packed(4))]
    #[derive(Default)]
    struct RegionBasicInfo64 {
        protection: libc::c_int,
        max_protection: libc::c_int,
        inheritance: libc::c_uint,
        shared: libc::c_int,
        reserved: libc::c_int,
        offset: u64,
        behavior: libc::c_int,
        user_wired_count: u16,
    }
    const REGION_BASIC_INFO_64_COUNT: u32 =
        (std::mem::size_of::<RegionBasicInfo64>() / std::mem::size_of::<libc::c_int>()) as u32;

    /// `mach_task_self()` (the libc crate's wrapper is deprecated).
    fn task_self() -> libc::mach_port_t {
        // SAFETY: a process-lifetime constant the runtime initialises before
        // `main`.
        unsafe { mach_task_self_ }
    }

    unsafe extern "C" {
        static mach_task_self_: libc::mach_port_t;

        fn mach_vm_remap(
            target_task: libc::vm_map_t,
            target_address: *mut MachVmAddress,
            size: libc::mach_vm_size_t,
            mask: libc::mach_vm_offset_t,
            flags: libc::c_int,
            src_task: libc::vm_map_t,
            src_address: MachVmAddress,
            copy: libc::c_int,
            cur_protection: *mut libc::c_int,
            max_protection: *mut libc::c_int,
            inheritance: libc::vm_inherit_t,
        ) -> libc::c_int;

        fn mach_vm_region(
            target_task: libc::vm_map_t,
            address: *mut MachVmAddress,
            size: *mut libc::mach_vm_size_t,
            flavor: libc::c_int,
            info: *mut libc::c_int,
            info_count: *mut libc::mach_msg_type_number_t,
            object_name: *mut libc::mach_port_t,
        ) -> libc::c_int;
    }

    /// A shared second mapping of the whole RAM reservation.
    pub(super) struct AliasMapping {
        base: *mut u8,
        /// Fault-handler window registration (stage 3), released before the
        /// mapping is unmapped.
        #[cfg(target_arch = "aarch64")]
        window_slot: usize,
    }

    // SAFETY: the mapping is owned exclusively by its `Memory`; the struct
    // only holds its address.
    unsafe impl Send for AliasMapping {}
    // SAFETY: as above; `&AliasMapping` performs no memory access.
    unsafe impl Sync for AliasMapping {}

    impl AliasMapping {
        pub(super) fn unmap_arena(&self) {
            let target = unsafe { self.base.add(crate::arena::ARENA_PHYS_START) };
            let mapped = unsafe {
                libc::mmap(
                    target.cast(),
                    crate::arena::ARENA_STORE_BYTES,
                    libc::PROT_NONE,
                    libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_FIXED | libc::MAP_NORESERVE,
                    -1,
                    0,
                )
            };
            assert_eq!(
                mapped,
                target.cast(),
                "restoring fastmem arena guard failed: {}",
                std::io::Error::last_os_error()
            );
        }

        pub(super) fn map_arena(&self, store: &crate::arena::ArenaStore) -> std::io::Result<()> {
            let target = unsafe { self.base.add(crate::arena::ARENA_PHYS_START) };
            // SAFETY: this subrange belongs to this alias; MAP_FIXED replaces
            // the old private remap with the same shared section as the source.
            let mapped = unsafe {
                libc::mmap(
                    target.cast(),
                    crate::arena::ARENA_STORE_BYTES,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED | libc::MAP_FIXED,
                    store.fd(),
                    0,
                )
            };
            if mapped == libc::MAP_FAILED {
                return Err(std::io::Error::last_os_error());
            }
            assert_eq!(mapped, target.cast());
            Ok(())
        }

        /// Map the reservation at `source` a second time (shared, not
        /// copied), with no access until the projection grants it.
        pub(super) fn new(source: *mut u8) -> Option<Self> {
            let mut target: MachVmAddress = 0;
            let mut cur: libc::c_int = 0;
            let mut max: libc::c_int = 0;
            // SAFETY: remaps our own reservation into a fresh kernel-chosen
            // range of the same task; `copy = 0` shares the pages.
            let kr = unsafe {
                mach_vm_remap(
                    task_self(),
                    &mut target,
                    RAM_RESERVATION_BYTES as u64,
                    0,
                    VM_FLAGS_ANYWHERE,
                    task_self(),
                    source as MachVmAddress,
                    0,
                    &mut cur,
                    &mut max,
                    VM_INHERIT_NONE,
                )
            };
            if kr != KERN_SUCCESS {
                tracing::warn!(
                    target: "gp64::memory::fastmem",
                    kr,
                    "mach_vm_remap of guest RAM failed; fastmem unavailable"
                );
                return None;
            }
            // Stage 3: emitted loads fault inside this mapping, so the fault
            // handler must know it. A full registry means no fastmem: an
            // unregistered window's faults would reach the crash handler.
            #[cfg(target_arch = "aarch64")]
            let Some(window_slot) =
                gp64_fastmem::register_window(target as usize, RAM_RESERVATION_BYTES)
            else {
                // SAFETY: unmaps exactly the alias just created.
                unsafe { libc::munmap(target as *mut libc::c_void, RAM_RESERVATION_BYTES) };
                tracing::warn!(
                    target: "gp64::memory::fastmem",
                    "fastmem window registry full; fastmem unavailable"
                );
                return None;
            };
            #[cfg(target_arch = "aarch64")]
            gp64_fastmem::install_fault_handler();
            let alias = Self {
                base: target as *mut u8,
                #[cfg(target_arch = "aarch64")]
                window_slot,
            };
            // The remap copies the source's protections (committed RAM is
            // read/write); the projection starts from none everywhere.
            alias.protect(0, RAM_RESERVATION_BYTES, FastmemPerm::None);
            Some(alias)
        }

        pub(super) fn base(&self) -> *mut u8 {
            self.base
        }

        #[cfg(target_arch = "aarch64")]
        pub(super) fn window_slot(&self) -> usize {
            self.window_slot
        }

        pub(super) fn protect(&self, offset: usize, len: usize, perm: FastmemPerm) {
            let prot = match perm {
                FastmemPerm::None => libc::PROT_NONE,
                FastmemPerm::Read => libc::PROT_READ,
                FastmemPerm::ReadWrite => libc::PROT_READ | libc::PROT_WRITE,
            };
            assert!(offset + len <= RAM_RESERVATION_BYTES);
            // SAFETY: `[offset, offset + len)` lies inside our alias mapping.
            let rc = unsafe { libc::mprotect(self.base.add(offset).cast(), len, prot) };
            assert!(
                rc == 0,
                "fastmem mprotect({offset:#x}, {len:#x}, {perm:?}) failed: {}",
                std::io::Error::last_os_error()
            );
        }

        /// OS-reported protection of `pages` host pages from `offset`.
        pub(super) fn query(
            &self,
            offset: usize,
            pages: usize,
            host_page: usize,
        ) -> Option<Vec<FastmemPerm>> {
            let start = self.base as u64 + offset as u64;
            let end = start + (pages * host_page) as u64;
            let mut out = vec![FastmemPerm::None; pages];
            let mut addr = start;
            while addr < end {
                let mut region = addr;
                let mut size: libc::mach_vm_size_t = 0;
                let mut info = RegionBasicInfo64::default();
                let mut count = REGION_BASIC_INFO_64_COUNT;
                let mut object: libc::mach_port_t = 0;
                // SAFETY: plain query of our own task's map into a local.
                let kr = unsafe {
                    mach_vm_region(
                        task_self(),
                        &mut region,
                        &mut size,
                        VM_REGION_BASIC_INFO_64,
                        (&raw mut info).cast(),
                        &mut count,
                        &mut object,
                    )
                };
                if kr != KERN_SUCCESS || region >= end {
                    // Nothing mapped from `addr` on: reads as none.
                    break;
                }
                let prot = info.protection;
                let perm = if prot & libc::PROT_WRITE != 0 {
                    FastmemPerm::ReadWrite
                } else if prot & libc::PROT_READ != 0 {
                    FastmemPerm::Read
                } else {
                    FastmemPerm::None
                };
                let lo = region.max(start);
                let hi = (region + size).min(end);
                let first = ((lo - start) / host_page as u64) as usize;
                let last = ((hi - start) / host_page as u64) as usize;
                out[first..last].fill(perm);
                addr = region + size;
            }
            Some(out)
        }
    }

    impl Drop for AliasMapping {
        fn drop(&mut self) {
            #[cfg(target_arch = "aarch64")]
            gp64_fastmem::unregister_window(self.window_slot);
            // SAFETY: unmaps exactly the alias `new` created.
            unsafe {
                libc::munmap(self.base.cast(), RAM_RESERVATION_BYTES);
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod host {
    use super::FastmemPerm;

    /// No alias mechanism on this host: fastmem is unavailable.
    pub(super) enum AliasMapping {}

    impl AliasMapping {
        pub(super) fn new(_source: *mut u8) -> Option<Self> {
            None
        }

        pub(super) fn base(&self) -> *mut u8 {
            match *self {}
        }

        pub(super) fn protect(&self, _offset: usize, _len: usize, _perm: FastmemPerm) {
            match *self {}
        }

        pub(super) fn query(
            &self,
            _offset: usize,
            _pages: usize,
            _host_page: usize,
        ) -> Option<Vec<FastmemPerm>> {
            match *self {}
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    const PD: usize = 0x00FF_F000;
    const PT: usize = 0x00F0_0000;

    /// Map linear `[0x10000, + pages * 4 KiB)` at the flat-memory identity offset
    /// with the given PTE flags, via a hand-built page table.
    fn map_identity(mem: &mut Memory, first_page: usize, pages: usize, flags: u32) {
        mem.ram[PD..PD + 4].copy_from_slice(&((PT as u32) | 0x7).to_le_bytes());
        for page in first_page..first_page + pages {
            let phys = ((page << 12) + FASTMEM_PHYS_BIAS) as u32;
            let pte = PT + page * 4;
            mem.ram[pte..pte + 4].copy_from_slice(&(phys | flags).to_le_bytes());
        }
    }

    fn expanded_memory() -> Memory {
        let mut mem = Memory::new();
        mem.expand_ram(128 * 1024 * 1024);
        mem
    }

    #[test]
    fn alias_view_shares_bytes_with_the_rust_view_in_both_directions() {
        let mut mem = expanded_memory();
        map_identity(&mut mem, 0x10, 0x10, PTE_PRESENT | PTE_RW | PTE_USER);
        assert!(mem.enable_fastmem_view(PD as u32));
        let jit = mem.fastmem_jit_base().expect("jit base");
        let linear = 0x1_0000usize;
        let phys = linear + FASTMEM_PHYS_BIAS;

        mem.ram[phys] = 0xAB;
        mem.ram[phys + 0x1FFF] = 0xCD;
        // SAFETY: linear 0x10000..0x20000 is projected read-write.
        unsafe {
            assert_eq!(jit.add(linear).read_volatile(), 0xAB);
            assert_eq!(jit.add(linear + 0x1FFF).read_volatile(), 0xCD);
            jit.add(linear + 0x10).write_volatile(0x5A);
        }
        assert_eq!(mem.ram[phys + 0x10], 0x5A);

        // RAM committed after the alias was made is shared too.
        mem.expand_ram(crate::DOS_RAM_CEILING + 0x100_0000);
        let high_page = (crate::DOS_RAM_CEILING + 0x80_0000 - FASTMEM_PHYS_BIAS) >> 12;
        let high_linear = high_page << 12;
        let pt2 = PT + 0x1000;
        let pde = PD + (high_page >> 10) * 4;
        mem.ram[pde..pde + 4].copy_from_slice(&((pt2 as u32) | 0x7).to_le_bytes());
        // Map the whole host page: the projection is the intersection.
        let per_host = crate::host_page_size() / GUEST_PAGE_SIZE;
        for page in high_page..high_page + per_host {
            let pte = pt2 + (page & 0x3FF) * 4;
            let phys = ((page << 12) + FASTMEM_PHYS_BIAS) as u32;
            mem.ram[pte..pte + 4].copy_from_slice(&(phys | 0x7).to_le_bytes());
        }
        let high_phys = (high_linear + FASTMEM_PHYS_BIAS) as u32;
        mem.fastmem_reproject_linear(high_linear as u32, per_host as u32);
        mem.ram[high_phys as usize + 3] = 0x42;
        // SAFETY: that page is now projected read-write.
        unsafe {
            assert_eq!(jit.add(high_linear + 3).read_volatile(), 0x42);
            jit.add(high_linear + 4).write_volatile(0x43);
        }
        assert_eq!(mem.ram[high_phys as usize + 4], 0x43);
    }

    #[test]
    fn projection_is_the_host_page_intersection_and_counts_mixed_pages() {
        let mut mem = expanded_memory();
        let rw = PTE_PRESENT | PTE_RW | PTE_USER;
        map_identity(&mut mem, 0x10, 0x20, rw);
        assert!(mem.enable_fastmem_view(PD as u32));
        let host = crate::host_page_size();
        let per_host = host / GUEST_PAGE_SIZE;
        let prot = mem.fastmem_host_protections().unwrap();
        let first_host = 0x10_000 / host;
        assert_eq!(prot[first_host], FastmemPerm::ReadWrite);
        assert_eq!(prot[0], FastmemPerm::None, "null page");

        // Make one guest page read-only: its host page drops to read.
        let pte = PT + 0x11 * 4;
        mem.ram[pte..pte + 4].copy_from_slice(
            &((((0x11usize << 12) + FASTMEM_PHYS_BIAS) as u32) | PTE_PRESENT | PTE_USER)
                .to_le_bytes(),
        );
        mem.fastmem_reproject_linear(0x11_000, 1);
        let prot = mem.fastmem_host_protections().unwrap();
        let h = 0x11_000 / host;
        assert_eq!(prot[h], FastmemPerm::Read);
        let stats = mem.fastmem_stats().unwrap();
        assert_eq!(stats.mixed_host_pages, u64::from(per_host > 1));

        // An SMC watch on another page of a read-write host page caps it.
        let watched_linear = 0x28_000usize;
        let watched_phys_page = ((watched_linear + FASTMEM_PHYS_BIAS) >> 12) as u32;
        let hw = watched_linear / host;
        assert_eq!(
            mem.fastmem_host_protections().unwrap()[hw],
            FastmemPerm::ReadWrite
        );
        mem.set_page_watched(watched_phys_page, true);
        assert_eq!(
            mem.fastmem_host_protections().unwrap()[hw],
            FastmemPerm::Read
        );
        mem.set_page_watched(watched_phys_page, false);
        assert_eq!(
            mem.fastmem_host_protections().unwrap()[hw],
            FastmemPerm::ReadWrite
        );
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn late_arena_attach_replaces_alias_and_reprojects_mixed_host_page() {
        use crate::arena::{ArenaStore, ARENA_PHYS_START, ARENA_TABLES_PHYS_START};

        let mut mem = expanded_memory();
        assert!(mem.enable_fastmem_view(PD as u32));
        let jit = mem.fastmem_jit_base().expect("jit base");
        let (store, token) = ArenaStore::new().expect("section");
        mem.attach_arena_store(store).expect("late attach");
        mem.give_arena_admission(token);

        let linear = 0x8001_0000usize;
        let phys = ARENA_PHYS_START + 0x1_0000;
        let host = crate::host_page_size();
        let pages = host / GUEST_PAGE_SIZE;
        mem.commit_arena_host_range(phys..phys + host)
            .expect("commit arena host page");
        mem.ram[PD + 512 * 4..PD + 512 * 4 + 4]
            .copy_from_slice(&((ARENA_TABLES_PHYS_START as u32) | 0x7).to_le_bytes());
        for i in 0..pages {
            let pte = ARENA_TABLES_PHYS_START + (0x10 + i) * 4;
            mem.ram[pte..pte + 4]
                .copy_from_slice(&((phys as u32 + (i * 4096) as u32) | 0x7).to_le_bytes());
        }
        mem.fastmem_reproject_linear(linear as u32, pages as u32);
        assert_eq!(
            mem.fastmem_host_protections().unwrap()[linear / host],
            FastmemPerm::ReadWrite
        );
        mem.ram[phys] = 0x41;
        // SAFETY: the projection above admitted this page as readable and writable.
        unsafe { assert_eq!(jit.add(linear).read_volatile(), 0x41) };

        let second_pte = ARENA_TABLES_PHYS_START + 0x11 * 4;
        mem.ram[second_pte..second_pte + 4]
            .copy_from_slice(&((phys as u32 + 4096) | PTE_PRESENT | PTE_USER).to_le_bytes());
        mem.fastmem_reproject_linear(linear as u32, pages as u32);
        assert_eq!(
            mem.fastmem_host_protections().unwrap()[linear / host],
            FastmemPerm::Read
        );
        assert_eq!(
            mem.fastmem_stats().unwrap().mixed_host_pages,
            u64::from(pages > 1)
        );

        mem.ram[second_pte..second_pte + 4]
            .copy_from_slice(&((phys as u32 + 4096) | 0x7).to_le_bytes());
        mem.fastmem_reproject_linear(linear as u32, pages as u32);
        assert_eq!(
            mem.fastmem_host_protections().unwrap()[linear / host],
            FastmemPerm::ReadWrite
        );
        // SAFETY: the projection is writable again and the alias maps this store.
        unsafe { jit.add(linear + 1).write_volatile(0x57) };
        assert_eq!(mem.ram[phys + 1], 0x57);
    }

    #[test]
    fn a_non_identity_mapping_is_never_projected() {
        let mut mem = expanded_memory();
        map_identity(&mut mem, 0x10, 0x10, PTE_PRESENT | PTE_RW | PTE_USER);
        // Point linear 0x14000.. at some other physical page.
        for page in 0x14..0x18usize {
            let pte = PT + page * 4;
            mem.ram[pte..pte + 4].copy_from_slice(&(0x0200_0000u32 | 0x7).to_le_bytes());
        }
        assert!(mem.enable_fastmem_view(PD as u32));
        let host = crate::host_page_size();
        let prot = mem.fastmem_host_protections().unwrap();
        assert_eq!(prot[0x14_000 / host], FastmemPerm::None);
    }
}
