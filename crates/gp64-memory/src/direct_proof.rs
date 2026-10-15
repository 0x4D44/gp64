//! Memory-owned direct page-proof publication.
//!
//! `gp64-memory` publishes page-indexed proof entries and owns the generation
//! words against which they must be revalidated.
//!
//! # Pinning contract
//!
//! Ordinary-RAM host pointers stay stable while their proofs are valid.
//! `Memory::resize_ram` invalidates all proofs before shortening or
//! reallocating the backing.
//!
//! # Generations contract
//!
//! The boxed [`DirectGenerations`] block stays at a stable heap address for
//! the owning `Memory`'s lifetime, including moves of `Memory`. Every
//! translation, A20, memory-policy and descriptor epoch bump updates this
//! block in the same helper.

use std::cell::Cell;

use gp64_memory_core::{DirectGenerations, RamBasePtr};

use crate::{
    Memory, BIOS_BASE, EMS_FRAME_BASE, EMS_FRAME_SIZE, LOW_MEMORY_LIMIT, VIDEO_BASE, VIDEO_SIZE,
};

/// Size of one guest page.
pub const DIRECT_PROOF_PAGE_SIZE: usize = 4096;

/// Proof permission bit: page is readable.
pub const PROOF_PERM_READ: u32 = 1 << 0;
/// Proof permission bit: page is writable for the queried privilege.
pub const PROOF_PERM_WRITE: u32 = 1 << 1;
/// Proof permission bit: page is user-accessible.
pub const PROOF_PERM_USER: u32 = 1 << 2;

/// Watch flag bit: stores need SMC notification because a source-code
/// write notifier is bound to this memory.
pub const PROOF_WATCH_SOURCE_NOTIFIER: u32 = 1 << 0;

/// `physical_page_id` published for pages whose physical identity is
/// genuinely ambiguous (e.g. partially covered by a DPMI physical mapping).
/// Consumers must never key source ownership on this id; such pages are
/// always `NonDirect`.
pub const AMBIGUOUS_PHYSICAL_PAGE_ID: u32 = u32::MAX;

/// Page-indexed direct proof entry.
///
/// Only `DirectRam` and `WatchedDirectRam` entries carry usable host
/// pointers; `NonDirect` entries always have null pointers.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectPageProof {
    /// Linear page index (`linear >> 12`) this proof was published for.
    pub linear_page_tag: u32,
    /// Post-A20 physical page index (`masked_physical >> 12`). A20 aliases of
    /// the same physical page share this identity.
    pub physical_page_id: u32,
    /// Host pointer to the start of the physical page for reads, or null.
    pub read_ptr: *const u8,
    /// Host pointer to the start of the physical page for writes, or null.
    /// Only published for `DirectProofAccess::Write` queries whose required
    /// Dirty commit is already in place.
    pub write_ptr: *mut u8,
    /// `PROOF_PERM_*` bits for the translated mapping.
    pub perms: u32,
    /// `DirectGenerations::proof_generation` at publication.
    pub proof_generation: u32,
    /// `DirectGenerations::map_generation` at publication.
    pub map_generation: u32,
    /// `DirectGenerations::source_generation` at publication.
    pub source_generation: u32,
    /// `PROOF_WATCH_*` bits (non-zero only for `WatchedDirectRam`).
    pub watch_flags: u32,
}

/// Closed proof classes (HLD 11.1). There is no fourth class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectProofClass {
    /// Ordinary RAM with direct read/write pointers and no watch work.
    DirectRam,
    /// Ordinary RAM with direct pointers; stores require SMC notification
    /// as indicated by `watch_flags`.
    WatchedDirectRam,
    /// No direct pointer access (VGA/MMIO/ROM/EMS-ambiguous,
    /// descriptor-snooped, out-of-RAM or diagnostic-filtered pages).
    NonDirect,
}

/// Rejection reasons for proofs that cannot be published: the access would
/// fault or required paging side effects are not yet committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofUnavailableReason {
    /// PDE or PTE not present — the access would raise #PF.
    NotPresent,
    /// Write to a read-only mapping (user, or supervisor with CR0.WP) — the
    /// access would raise #PF.
    WriteProtected,
    /// User-mode access to a supervisor mapping — the access would raise #PF.
    UserSupervisorViolation,
    /// The required PDE/PTE Accessed (and Dirty, for write proofs) bits are
    /// not yet committed in the guest page tables. Publication never commits
    /// them itself (HLD 11.2 simple-V0 option); the caller must retire the
    /// access through a committing path first.
    AdBitsUncommitted,
}

/// Access kind a proof is requested for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectProofAccess {
    Read,
    Write,
}

impl DirectProofAccess {
    #[inline]
    fn is_write(self) -> bool {
        matches!(self, Self::Write)
    }
}

/// Paging context for a publication query. `Memory` does not own CR3/CR0;
/// the caller (CPU side) supplies the translation context and `Memory` walks
/// the guest page tables itself — they are RAM contents.
#[derive(Debug, Clone, Copy)]
pub enum DirectProofPaging {
    /// Paging disabled: linear address is the physical address (pre-A20).
    Disabled,
    /// Paging enabled with a two-level 32-bit walk (CR4.PSE honoured).
    Enabled {
        cr3: u32,
        /// True when the access is CPL3 (user).
        user: bool,
        /// CR0.WP.
        write_protect: bool,
        /// CR4.PSE.
        pse: bool,
    },
}

/// A successfully published proof entry plus its closed class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishedDirectPageProof {
    pub class: DirectProofClass,
    pub proof: DirectPageProof,
}

struct WalkResult {
    /// Physical page base (pre-A20 mask), 4 KiB aligned.
    phys_page_base: usize,
    writable: bool,
    user_allowed: bool,
}

// Test-only observation point (LLD E4): the direct-read RAM limit as the
// direct-policy epoch bump in `invalidate_all_direct_proofs` saw it. Lets a
// test prove `resize_ram` lowers the limit before the bump announces it.
#[cfg(test)]
thread_local! {
    pub(crate) static RAW_RAM_LIMIT_AT_DIRECT_EPOCH_BUMP: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

impl Memory {
    /// Raw pointer to the stable-heap [`DirectGenerations`] block.
    /// Valid for this `Memory`'s lifetime and stable across moves of `Memory`.
    #[inline]
    pub fn direct_generations_ptr(&self) -> *const DirectGenerations {
        self.core.direct_generations_ptr()
    }

    /// Borrow the generations block (host-side consumers and tests).
    #[inline]
    pub fn direct_generations(&self) -> &DirectGenerations {
        self.core.direct_generations()
    }

    /// Current memory-side descriptor-table generation.
    ///
    /// Bumped by GDT/LDT byte-write snoops, snoop-range (re)programming,
    /// and explicit [`Self::bump_descriptor_generation`] calls.
    #[inline]
    pub fn dynrec_descriptor_generation(&self) -> u64 {
        self.core.dynrec_descriptor_generation()
    }

    #[inline]
    pub fn bump_descriptor_generation(&self) {
        self.core.bump_descriptor_generation()
    }

    /// Bump global word zero and every page word in the guarded-page
    /// interest set, staling all bound proofs. Descriptor events update
    /// only the admission stamp; source events instead use
    /// [`Self::bump_dynrec_source_generation_for_page`].
    ///
    /// Registration uses `&Memory`, so the guarded set uses interior mutability.
    pub(crate) fn stale_all_guarded_pages(&self) {
        self.core.stale_all_guarded_pages()
    }

    /// Advance one physical page's generation word and the global source
    /// generation. Unlike a global sweep, this does not bump proof word zero.
    /// Interior mutability permits updates through a shared `&Memory` borrow.
    #[inline]
    pub fn bump_dynrec_source_generation_for_page(&self, physical_page_id: u32) {
        self.core
            .bump_dynrec_source_generation_for_page(physical_page_id)
    }

    /// Number of entries in the per-page generation table
    /// (`pinned_ram_capacity >> 12`).
    #[inline]
    pub fn page_generation_table_len(&self) -> usize {
        self.core.page_generation_table_len()
    }

    /// Pinned address of `physical_page_id`'s generation word, or `None`
    /// for out-of-table pages (such pages are `NonDirect` and must never
    /// bind). Valid for the lifetime of this `Memory`; replaced tables are
    /// retired, never freed, so a pointer captured under an older
    /// [`Self::page_generation_table_identity`] stays dereferenceable (its
    /// bumps land in the retired table — harmless, because the identity
    /// mismatch forces a full consumer reset before any further bind).
    #[inline]
    pub fn page_generation_word_addr(&self, physical_page_id: u32) -> Option<*const Cell<u32>> {
        self.core.page_generation_word_addr(physical_page_id)
    }

    /// Base and length of the pinned per-page generation table.
    /// Same lifetime contract as [`Self::page_generation_word_addr`].
    #[inline]
    pub fn page_generation_table_ptr(&self) -> (*const Cell<u32>, usize) {
        self.core.page_generation_table_ptr()
    }

    /// Identity epoch of the per-page generation table. Advances ONLY when
    /// the table is reallocated, which is legal only in over-capacity
    /// [`Self::resize_ram_backing`] after proof invalidation. Consumers snapshot this
    /// at cache creation and compare before native entry; a mismatch
    /// mandates a full cache + arena + guarded-set reset.
    #[inline]
    pub fn page_generation_table_identity(&self) -> u64 {
        self.core.page_generation_table_identity()
    }

    /// Current value of `physical_page_id`'s generation word (host-side
    /// inspection and tests), or `None` for out-of-table pages.
    #[inline]
    pub fn page_generation(&self, physical_page_id: u32) -> Option<u32> {
        self.core.page_generation(physical_page_id)
    }

    /// Register `physical_page_id` in the guarded-page interest set: at
    /// least one proof slot was bound to it, so global invalidation classes
    /// must bump its page word. MONOTONE — never unregistered; cleared only
    /// by [`Self::clear_guarded_pages`] at a full consumer reset. Sweeping
    /// a page with no current bindings is a harmless extra bump.
    #[inline]
    pub fn register_guarded_page(&self, physical_page_id: u32) {
        self.core.register_guarded_page(physical_page_id)
    }

    /// Clear the guarded-page interest set (full cache reset only).
    #[inline]
    pub fn clear_guarded_pages(&self) {
        self.core.clear_guarded_pages()
    }

    /// Current guarded-page interest-set size (host-side inspection).
    #[inline]
    pub fn guarded_page_count(&self) -> usize {
        self.core.guarded_page_count()
    }

    /// Cumulative global-class sweeps by [`Self::stale_all_guarded_pages`].
    #[inline]
    pub fn dynrec_generation_sweeps(&self) -> u64 {
        self.core.dynrec_generation_sweeps()
    }

    /// Cumulative page words bumped by global-class sweeps.
    #[inline]
    pub fn dynrec_swept_pages_total(&self) -> u64 {
        self.core.dynrec_swept_pages_total()
    }

    /// Cumulative page-targeted source bumps
    /// ([`Self::bump_dynrec_source_generation_for_page`], in-table pages).
    #[inline]
    pub fn dynrec_page_source_bumps(&self) -> u64 {
        self.core.dynrec_page_source_bumps()
    }

    /// True when at least one proof with usable host pointers has been
    /// published since the last whole-block invalidation.
    #[inline]
    pub fn direct_proofs_live(&self) -> bool {
        self.core.direct_proofs_live()
    }

    /// Bump every generation word so all outstanding proofs (and cached
    /// admission guards) die, and clear the proof-liveness latch. WI-G1:
    /// routes through the guarded-page sweep (several times via the
    /// sweeping epoch helpers — harmless, only equality matters to the
    /// guards), so restore stales every guarded page word as well as the
    /// global word 0.
    pub fn invalidate_all_direct_proofs(&mut self) {
        #[cfg(test)]
        RAW_RAM_LIMIT_AT_DIRECT_EPOCH_BUMP.with(|seen| seen.set(Some(self.core.raw_ram_limit())));
        // map + policy (sweep)
        self.bump_direct_translation_policy_epoch();
        // a20 + map (sweep)
        self.bump_a20_generation();
        // descriptor (admission-stamp word only; the explicit sweep below
        // covers the page words)
        self.bump_descriptor_generation();
        self.core.bump_source_generation_word();
        self.stale_all_guarded_pages();
        self.core.set_direct_proofs_live(false);
    }

    /// Restore-identity hook (WI-C1 spike, feeds WI-E1): snapshot/checkpoint
    /// restore rewrites RAM contents and page tables in place. The software
    /// TLB is flushed, and the V0 direct-proof contract is option (b) of the
    /// HLD 19 warm-restore contract: backing identity is preserved but every
    /// generation is bumped so stale translations and proofs cannot survive.
    pub fn on_snapshot_restored(&mut self) {
        self.invalidate_tlb();
        self.invalidate_all_direct_proofs();
    }

    /// Resize the RAM backing to `new_len` total bytes, including fetch
    /// padding. [`Memory::resize_ram`] retires all direct proofs and TLB host
    /// pointers before calling this allocation-moving helper.
    pub(crate) fn resize_ram_backing(&mut self, new_len: usize) {
        if new_len > self.core.pinned_ram_capacity() {
            self.ram.resize(new_len, 0);
            // WI-G1: the per-page generation table is sized by the pinned
            // capacity, so over-capacity growth reallocates it under a new
            // identity epoch; the core retires (never frees) the old table.
            self.core.grow_pinned_ram_capacity(self.ram.capacity());
            // The capacity-sized table no longer covers an attached arena.
            if self.ram.has_arena_mapping() {
                self.core
                    .ensure_page_generation_coverage(crate::arena::ARENA_PHYS_END);
            }
        } else {
            self.ram.resize(new_len, 0);
        }
        // Re-capture the mutable-provenance base (DD-UNSAFE-1). The fixed
        // reservation never moves, but the 32-bit `Vec` fallback may on
        // over-capacity growth, and a fresh `host_base` is right for both.
        self.core.set_ram_base(RamBasePtr(self.ram.host_base()));
        // Publish the new extent last, once the backing exists. `resize_ram`
        // already lowered the limit to `new_len` before its epoch bumps
        // (LLD E4), so this store can only keep or raise it.
        let ram_len = self.ram.len();
        // SAFETY: `[0, ram.len())` is the committed backing just resized,
        // addressed by the base re-captured above.
        unsafe { self.core.set_raw_ram_limit(ram_len) };
    }

    /// Publish a direct proof entry for the page containing `linear`.
    ///
    /// Publication is read-only (`&self`): it never commits A/D bits, never
    /// touches the TLB, and never mutates guest state. A paged mapping is
    /// admissible only when the required PDE/PTE Accessed (and Dirty, for
    /// write proofs) bits are already committed; otherwise the named
    /// [`ProofUnavailableReason::AdBitsUncommitted`] rejection is returned —
    /// there is no silent checked-helper fallback.
    pub fn publish_direct_page_proof(
        &self,
        linear: u32,
        access: DirectProofAccess,
        paging: DirectProofPaging,
    ) -> Result<PublishedDirectPageProof, ProofUnavailableReason> {
        let linear_page_tag = linear >> 12;
        let page_linear_base = linear & !0xFFF;

        // DPMI linear->physical mappings bypass the page walk in the root
        // translation path in both paged and unpaged modes. Pages they touch
        // are device windows and genuinely ambiguous for direct use.
        if self.page_overlaps_physical_mapping(page_linear_base) {
            let physical_page_id = self
                .map_linear_physical(page_linear_base)
                .map_or(AMBIGUOUS_PHYSICAL_PAGE_ID, |phys| {
                    (self.dynrec_mask_physical_address(phys) >> 12) as u32
                });
            return Ok(self.non_direct_entry(
                linear_page_tag,
                physical_page_id,
                PROOF_PERM_READ | PROOF_PERM_WRITE,
            ));
        }

        let walk = match paging {
            DirectProofPaging::Disabled => WalkResult {
                phys_page_base: page_linear_base as usize,
                writable: true,
                user_allowed: true,
            },
            DirectProofPaging::Enabled {
                cr3,
                user,
                write_protect,
                pse,
            } => self.direct_proof_page_walk(linear, access, cr3, user, write_protect, pse)?,
        };

        let masked_base = self.dynrec_mask_physical_address(walk.phys_page_base);
        debug_assert_eq!(masked_base & 0xFFF, 0);
        let physical_page_id = (masked_base >> 12) as u32;

        let mut perms = PROOF_PERM_READ;
        if walk.writable {
            perms |= PROOF_PERM_WRITE;
        }
        if walk.user_allowed {
            perms |= PROOF_PERM_USER;
        }

        if !self.physical_page_is_direct_candidate(masked_base) {
            return Ok(self.non_direct_entry(linear_page_tag, physical_page_id, perms));
        }

        // Time Machine write tracking records every guest store; a direct
        // write pointer would bypass it. Reads remain direct.
        if access.is_write() && self.write_tracking_enabled {
            return Ok(self.non_direct_entry(linear_page_tag, physical_page_id, perms));
        }

        // Source-watch class: when an SMC notifier is bound, guest stores
        // must reach it. Classification is publication-time conservative: a
        // bound notifier makes every ordinary-RAM page `WatchedDirectRam`
        // regardless of the current compiled-page census, because
        // `set_smc_has_code_pages(true)` does not bump generations and must
        // not be able to strand an unwatched stale proof.
        let watched = self.has_smc_notifier();
        let (class, watch_flags) = if watched {
            (
                DirectProofClass::WatchedDirectRam,
                PROOF_WATCH_SOURCE_NOTIFIER,
            )
        } else {
            (DirectProofClass::DirectRam, 0)
        };

        // SAFETY: physical_page_is_direct_candidate proved
        // masked_base + PAGE_SIZE <= usable_len(), inside the allocation.
        // Provenance (DD-UNSAFE-1): both pointers derive from `ram_base`,
        // the `host_base`-captured base with write provenance over the
        // backing — never from `&self.ram`, whose shared
        // borrow would make stores through the published pointer UB.
        let page_ptr = unsafe { self.core.ram_base_ptr().add(masked_base) };
        let read_ptr = page_ptr.cast_const();
        // Write admission was already proved by the walk for Write queries
        // (fault checks passed — including the supervisor CR0.WP=0 override
        // where the page attribute is read-only — and Dirty is committed).
        // Read queries never receive a write pointer: stores require their
        // own Write proof so Dirty-commit admissibility is always checked.
        let write_ptr = if access.is_write() {
            page_ptr
        } else {
            std::ptr::null_mut()
        };

        self.core.set_direct_proofs_live(true);
        if masked_base >= crate::arena::ARENA_PHYS_START {
            self.arena_published.set(true);
        }
        Ok(PublishedDirectPageProof {
            class,
            proof: DirectPageProof {
                linear_page_tag,
                physical_page_id,
                read_ptr,
                write_ptr,
                perms,
                proof_generation: self.core.direct_generations().proof_generation.get(),
                map_generation: self.core.direct_generations().map_generation.get(),
                source_generation: self.core.direct_generations().source_generation.get(),
                watch_flags,
            },
        })
    }

    fn non_direct_entry(
        &self,
        linear_page_tag: u32,
        physical_page_id: u32,
        perms: u32,
    ) -> PublishedDirectPageProof {
        PublishedDirectPageProof {
            class: DirectProofClass::NonDirect,
            proof: DirectPageProof {
                linear_page_tag,
                physical_page_id,
                read_ptr: std::ptr::null(),
                write_ptr: std::ptr::null_mut(),
                perms,
                proof_generation: self.core.direct_generations().proof_generation.get(),
                map_generation: self.core.direct_generations().map_generation.get(),
                source_generation: self.core.direct_generations().source_generation.get(),
                watch_flags: 0,
            },
        }
    }

    /// True when the post-A20 physical page at `masked_base` is ordinary RAM
    /// whose direct reads and writes are equivalent to the checked accessors.
    /// Anything that routes, filters, protects, or snoops accesses makes the
    /// page `NonDirect`.
    fn physical_page_is_direct_candidate(&self, masked_base: usize) -> bool {
        const PAGE: usize = DIRECT_PROOF_PAGE_SIZE;
        // Real-boot POST access tracer logs every undeclared access.
        if self.post_trace_enabled {
            return false;
        }
        // Real-boot Intel MP table read filter rewrites low-memory reads.
        if self.firmware_mp_table_suppression_enabled && masked_base < LOW_MEMORY_LIMIT {
            return false;
        }
        // Device windows and protected images.
        if self
            .mmio_regions
            .iter()
            .any(|r| crate::range_overlaps(masked_base, PAGE, r.phys_start, r.data.len()))
        {
            return false;
        }
        if crate::range_overlaps(masked_base, PAGE, VIDEO_BASE, VIDEO_SIZE) {
            return false;
        }
        if self.lfb_range_overlaps(masked_base, PAGE) {
            return false;
        }
        if self.option_rom_region_overlaps(masked_base, PAGE) {
            return false;
        }
        if self.hle_ems_frame_owned
            && crate::range_overlaps(masked_base, PAGE, EMS_FRAME_BASE, EMS_FRAME_SIZE)
        {
            return false;
        }
        if self.rom_region_overlaps(masked_base, PAGE) {
            return false;
        }

        // BIOS write-protect window: write_u8 silently drops stores here.
        if !self.allow_rom_writes && crate::range_overlaps(masked_base, PAGE, BIOS_BASE, 0x10000) {
            return false;
        }
        // Descriptor-table snoop ranges: a direct store would bypass the
        // GDT/LDT write snoop (descriptor-watched pages are NonDirect in V0).
        if self.core.gdt_snoop_end() != 0
            && crate::range_overlaps(
                masked_base,
                PAGE,
                self.core.gdt_snoop_base(),
                self.core
                    .gdt_snoop_end()
                    .saturating_sub(self.core.gdt_snoop_base()),
            )
        {
            return false;
        }
        if self.core.ldt_snoop_end() != 0
            && crate::range_overlaps(
                masked_base,
                PAGE,
                self.core.ldt_snoop_base(),
                self.core
                    .ldt_snoop_end()
                    .saturating_sub(self.core.ldt_snoop_base()),
            )
        {
            return false;
        }
        // A private page or a committed, admitted arena page may be direct.
        match masked_base.checked_add(PAGE) {
            Some(end) => {
                end <= self.usable_len()
                    || (masked_base >= crate::arena::ARENA_PHYS_START
                        && end <= crate::arena::ARENA_PHYS_END
                        && self.ram.is_backed(masked_base..end))
            }
            None => false,
        }
    }

    fn page_overlaps_physical_mapping(&self, page_linear_base: u32) -> bool {
        self.physical_mappings.iter().any(|m| {
            crate::range_overlaps(
                page_linear_base as usize,
                DIRECT_PROOF_PAGE_SIZE,
                m.linear as usize,
                m.size as usize,
            )
        })
    }

    /// Read-only two-level page walk mirroring the root crate's
    /// `translate_paging_checked` semantics (no reserved-bit checks, CR4.PSE
    /// 4 MiB pages, CR0.WP honoured) but *without* committing A/D bits:
    /// uncommitted required bits are a named rejection.
    fn direct_proof_page_walk(
        &self,
        linear: u32,
        access: DirectProofAccess,
        cr3: u32,
        user: bool,
        write_protect: bool,
        pse: bool,
    ) -> Result<WalkResult, ProofUnavailableReason> {
        const ACCESSED: u32 = 1 << 5;
        const DIRTY: u32 = 1 << 6;
        const PAGE_SIZE_BIT: u32 = 1 << 7;
        let is_write = access.is_write();

        let pde_index = (linear >> 22) as usize;
        let pde_addr = ((cr3 & 0xFFFF_F000) as usize) + pde_index * 4;
        let pde = self.read_u32_physical(pde_addr);
        if pde & 1 == 0 {
            return Err(ProofUnavailableReason::NotPresent);
        }

        if pse && (pde & PAGE_SIZE_BIT) != 0 {
            let writable = (pde & 2) != 0;
            let user_allowed = (pde & 4) != 0;
            if is_write && !writable && (user || write_protect) {
                return Err(ProofUnavailableReason::WriteProtected);
            }
            if user && !user_allowed {
                return Err(ProofUnavailableReason::UserSupervisorViolation);
            }
            if pde & ACCESSED == 0 {
                return Err(ProofUnavailableReason::AdBitsUncommitted);
            }
            if is_write && (pde & DIRTY == 0) {
                return Err(ProofUnavailableReason::AdBitsUncommitted);
            }
            let phys_page_base = ((pde & 0xFFC0_0000) | (linear & 0x003F_F000)) as usize;
            return Ok(WalkResult {
                phys_page_base,
                writable,
                user_allowed,
            });
        }

        let pte_index = ((linear >> 12) & 0x3FF) as usize;
        let pte_addr = ((pde & 0xFFFF_F000) as usize) + pte_index * 4;
        let pte = self.read_u32_physical(pte_addr);
        if pte & 1 == 0 {
            return Err(ProofUnavailableReason::NotPresent);
        }

        let writable = (pde & 2) != 0 && (pte & 2) != 0;
        let user_allowed = (pde & 4) != 0 && (pte & 4) != 0;
        if is_write && !writable && (user || write_protect) {
            return Err(ProofUnavailableReason::WriteProtected);
        }
        if user && !user_allowed {
            return Err(ProofUnavailableReason::UserSupervisorViolation);
        }
        if (pde & ACCESSED == 0) || (pte & ACCESSED == 0) {
            return Err(ProofUnavailableReason::AdBitsUncommitted);
        }
        if is_write && (pte & DIRTY == 0) {
            return Err(ProofUnavailableReason::AdBitsUncommitted);
        }

        Ok(WalkResult {
            phys_page_base: (pte & 0xFFFF_F000) as usize,
            writable,
            user_allowed,
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::DOS_RAM_CEILING;
    use std::sync::Arc;

    #[test]
    fn arena_direct_candidate_needs_commit_and_mounted_admission() {
        let (store, token) = crate::arena::ArenaStore::new().expect("section");
        let mut mem = Memory::new();
        mem.attach_arena_store(store).expect("attach");
        let phys = crate::arena::ARENA_PHYS_START + 0x40_000;
        assert!(!mem.physical_page_is_direct_candidate(phys));
        mem.give_arena_admission(token);
        assert!(!mem.physical_page_is_direct_candidate(phys));
        mem.commit_arena_host_range(phys..phys + 4096)
            .expect("commit");
        assert!(mem.physical_page_is_direct_candidate(phys));
        assert!(mem.page_generation_word_addr((phys >> 12) as u32).is_some());
        let token = mem.take_arena_admission().expect("mounted token");
        assert!(!mem.physical_page_is_direct_candidate(phys));
        drop(token);
    }

    const P: u32 = 1; // present
    const W: u32 = 1 << 1; // writable
    const U: u32 = 1 << 2; // user
    const A: u32 = 1 << 5; // accessed
    const D: u32 = 1 << 6; // dirty
    const PS: u32 = 1 << 7; // 4MB page

    const PD_BASE: u32 = 0x10000;
    const PT_BASE: u32 = 0x11000;
    const FRAME: u32 = 0x20000;
    /// Linear address mapped by PDE[1]/PTE[0] in `setup_page_tables`.
    const LIN: u32 = 0x0040_0000;

    fn paged(user: bool, write_protect: bool) -> DirectProofPaging {
        DirectProofPaging::Enabled {
            cr3: PD_BASE,
            user,
            write_protect,
            pse: false,
        }
    }

    /// Install PDE[1] -> PT_BASE and PTE[0] -> FRAME with the given flags.
    fn setup_page_tables(mem: &mut Memory, pde_flags: u32, pte_flags: u32) {
        mem.write_u32_physical(PD_BASE as usize + 4, PT_BASE | pde_flags);
        mem.write_u32_physical(PT_BASE as usize, FRAME | pte_flags);
    }

    // ── proof success and pointer stability ─────────────────────────────

    #[test]
    fn direct_ram_read_proof_publishes_stable_pointers() {
        let mem = Memory::new();
        let first = mem
            .publish_direct_page_proof(
                0x50000,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_eq!(first.class, DirectProofClass::DirectRam);
        assert_eq!(first.proof.linear_page_tag, 0x50);
        assert_eq!(first.proof.physical_page_id, 0x50);
        assert_eq!(first.proof.read_ptr, unsafe { mem.ram_ptr().add(0x50000) });
        assert!(
            first.proof.write_ptr.is_null(),
            "read proofs carry no write ptr"
        );
        assert_eq!(first.proof.watch_flags, 0);
        assert_eq!(
            first.proof.perms,
            PROOF_PERM_READ | PROOF_PERM_WRITE | PROOF_PERM_USER
        );
        assert!(mem.direct_proofs_live());

        // Publication is pure: repeated queries while generations are
        // unchanged return identical pointers and generation words.
        let again = mem
            .publish_direct_page_proof(
                0x50000,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_eq!(again.proof.read_ptr, first.proof.read_ptr);
        assert_eq!(again.proof.proof_generation, first.proof.proof_generation);
        assert_eq!(again.proof.map_generation, first.proof.map_generation);
        assert_eq!(again.proof.source_generation, first.proof.source_generation);
    }

    #[test]
    fn direct_ram_write_proof_publishes_write_pointer() {
        let mem = Memory::new();
        let proof = mem
            .publish_direct_page_proof(
                0x51000,
                DirectProofAccess::Write,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_eq!(proof.class, DirectProofClass::DirectRam);
        assert_eq!(proof.proof.write_ptr.cast_const(), proof.proof.read_ptr);
        assert!(!proof.proof.write_ptr.is_null());
    }

    #[test]
    fn pointer_stability_across_ensure_ram_size_within_capacity() {
        let mut mem = Memory::new();
        let base = mem.ram_ptr();
        let gens_before = mem.direct_generations().snapshot();
        let proof = mem
            .publish_direct_page_proof(
                0x60000,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();

        // No-op growth: already well above the requested size.
        mem.ensure_ram_size(0x20_0000);
        assert_eq!(mem.ram_ptr(), base);
        assert_eq!(mem.direct_generations().snapshot(), gens_before);
        let again = mem
            .publish_direct_page_proof(
                0x60000,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_eq!(again.proof.read_ptr, proof.proof.read_ptr);

        // Real growth within pinned capacity after a memory-owned shrink:
        // the backing buffer must not move.
        mem.resize_ram(0x10_0000);
        assert_eq!(mem.ram_ptr(), base, "shrink keeps the allocation");
        mem.ensure_ram_size(0x20_0000);
        assert_eq!(
            mem.ram_ptr(),
            base,
            "growth within capacity must not realloc"
        );
        assert_eq!(mem.usable_len(), 0x20_0000);
    }

    #[test]
    fn over_capacity_growth_without_live_proofs_invalidates_all_generations() {
        let mut mem = Memory::new();
        assert!(!mem.direct_proofs_live());
        let before = mem.direct_generations().snapshot();
        mem.expand_ram(DOS_RAM_CEILING + 0x10_0000);
        let after = mem.direct_generations().snapshot();
        assert_ne!(after.proof_generation, before.proof_generation);
        assert_ne!(after.map_generation, before.map_generation);
        assert_ne!(after.a20_generation, before.a20_generation);
        assert_ne!(after.descriptor_generation, before.descriptor_generation);
        assert_ne!(after.policy_generation, before.policy_generation);
        assert_ne!(after.source_generation, before.source_generation);
        assert!(!mem.direct_proofs_live());
        assert_eq!(mem.usable_len(), DOS_RAM_CEILING + 0x10_0000);
    }

    // ── DD-UNSAFE-1: write-pointer provenance ────────────────────────────

    /// Published pointers derive from the mutable-provenance RAM base,
    /// not a shared slice. Stores through them remain sound while a shared
    /// `&Memory` borrow is live.
    #[test]
    fn published_pointers_derive_from_ram_base_provenance() {
        let mem = Memory::new();
        let base = mem.core.ram_base_ptr() as usize;
        let published = mem
            .publish_direct_page_proof(
                0x52000,
                DirectProofAccess::Write,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_eq!(published.proof.write_ptr as usize, base + 0x52000);
        assert_eq!(published.proof.read_ptr as usize, base + 0x52000);
        // A store through the published pointer while `&Memory` is live.
        // SAFETY: the Write proof grants the whole pinned 4 KiB page.
        unsafe { published.proof.write_ptr.add(0x123).write(0xA5) };
        assert_eq!(mem.ram[0x52123], 0xA5);
    }

    /// `ram_base` follows the backing allocation: unchanged by in-capacity
    /// growth, re-captured by the over-capacity pinned-resize path.
    #[test]
    fn ram_base_tracks_the_backing_allocation_across_growth() {
        let mut mem = Memory::new();
        assert_eq!(
            mem.core.ram_base_ptr().cast_const(),
            mem.ram.host_base().cast_const()
        );
        // In-capacity growth never reallocates; the base must not move.
        mem.resize_ram(0x10_0000);
        mem.ensure_ram_size(0x20_0000);
        assert_eq!(
            mem.core.ram_base_ptr().cast_const(),
            mem.ram.host_base().cast_const()
        );
        // Over-capacity growth (no live proofs) reallocates; the base is
        // re-captured from the new allocation.
        mem.expand_ram(DOS_RAM_CEILING + 0x10_0000);
        assert_eq!(
            mem.core.ram_base_ptr().cast_const(),
            mem.ram.host_base().cast_const()
        );
    }

    #[test]
    fn over_capacity_growth_with_live_proofs_rebinds_to_current_ram() {
        let mut mem = Memory::new();
        let old_generations = mem.direct_generations().snapshot();
        let _old_proof = mem
            .publish_direct_page_proof(
                0x50000,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert!(mem.direct_proofs_live());

        mem.expand_ram(DOS_RAM_CEILING + 0x10_0000);

        assert!(!mem.direct_proofs_live());
        assert_ne!(mem.direct_generations().snapshot(), old_generations);
        let fresh_proof = mem
            .publish_direct_page_proof(
                0x50000,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_eq!(
            fresh_proof.proof.read_ptr,
            unsafe { mem.ram.host_base().cast_const().add(0x50000) },
            "new proofs must derive from the current RAM allocation"
        );
    }

    // ── class tests ──────────────────────────────────────────────────────

    fn assert_non_direct(published: &PublishedDirectPageProof) {
        assert_eq!(published.class, DirectProofClass::NonDirect);
        assert!(published.proof.read_ptr.is_null());
        assert!(published.proof.write_ptr.is_null());
        assert_eq!(published.proof.watch_flags, 0);
    }

    #[test]
    fn vga_window_page_is_non_direct_with_null_pointers() {
        let mem = Memory::new();
        let published = mem
            .publish_direct_page_proof(
                0xA0000,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_non_direct(&published);
        assert_eq!(published.proof.physical_page_id, 0xA0);
    }

    #[test]
    fn mmio_page_is_non_direct_even_above_ram() {
        let mut mem = Memory::new();
        mem.add_mmio_region(0x0400_0000, 0x2000);
        mem.add_mmio_region(0x6000_0000, 0x1000);
        let in_ram = mem
            .publish_direct_page_proof(
                0x0400_0000,
                DirectProofAccess::Write,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_non_direct(&in_ram);
        let above_ram = mem
            .publish_direct_page_proof(
                0x6000_0000,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_non_direct(&above_ram);
    }

    #[test]
    fn rom_page_is_non_direct() {
        let mut mem = Memory::new();
        mem.mark_rom_range(0xC0000..=0xC0FFF);
        let published = mem
            .publish_direct_page_proof(
                0xC0000,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_non_direct(&published);
    }

    #[test]
    fn configurable_lfb_decoder_reclassifies_and_relocates_direct_pages() {
        const FIRST_BASE: u32 = 0x0200_0000;
        const SECOND_BASE: u32 = 0x0300_0000;
        let mut mem = Memory::new();
        // FIRST_BASE is installed DRAM; SECOND_BASE is the bus above it.
        mem.resize_ram(SECOND_BASE as usize);
        mem.init_lfb(0x4000);

        let before = mem
            .publish_direct_page_proof(
                FIRST_BASE,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_eq!(before.class, DirectProofClass::DirectRam);
        let epoch = mem.direct_memory_policy_epoch();
        mem.register_guarded_page(FIRST_BASE >> 12);
        let old_generation = mem.page_generation(FIRST_BASE >> 12).unwrap();

        mem.configure_lfb_decoder(crate::LfbDecoderConfig::S3 {
            base: FIRST_BASE as usize,
            size: 0x2000,
            vram_offset: 0,
            enabled: true,
        });
        assert!(mem.direct_memory_policy_epoch() > epoch);
        assert_ne!(
            mem.page_generation(FIRST_BASE >> 12).unwrap(),
            old_generation
        );
        let first = mem
            .publish_direct_page_proof(
                FIRST_BASE,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_eq!(
            first.class,
            DirectProofClass::DirectRam,
            "a window over system DRAM never claims the page (MDD-BUG-NMI-00213)"
        );

        mem.configure_lfb_decoder(crate::LfbDecoderConfig::S3 {
            base: SECOND_BASE as usize,
            size: 0x2000,
            vram_offset: 0,
            enabled: true,
        });
        let stale = mem
            .publish_direct_page_proof(
                FIRST_BASE,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_eq!(stale.class, DirectProofClass::DirectRam);
        let relocated = mem
            .publish_direct_page_proof(
                SECOND_BASE,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_non_direct(&relocated);
    }

    #[test]
    fn option_rom_decoder_reclassifies_and_tears_down_direct_pages() {
        const FIRST_BASE: u32 = 0x000C_0000;
        const SECOND_BASE: u32 = 0x000D_0000;
        let mut mem = Memory::new();
        mem.set_option_rom_backing(&[0x55; 0x2000]);

        let before = mem
            .publish_direct_page_proof(
                FIRST_BASE,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_eq!(before.class, DirectProofClass::DirectRam);

        mem.configure_option_rom_decoder(FIRST_BASE as usize, true);
        let active = mem
            .publish_direct_page_proof(
                FIRST_BASE,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_non_direct(&active);

        mem.configure_option_rom_decoder(SECOND_BASE as usize, true);
        let stale = mem
            .publish_direct_page_proof(
                FIRST_BASE,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_eq!(stale.class, DirectProofClass::DirectRam);
        let relocated = mem
            .publish_direct_page_proof(
                SECOND_BASE,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_non_direct(&relocated);

        mem.configure_option_rom_decoder(SECOND_BASE as usize, false);
        let disabled = mem
            .publish_direct_page_proof(
                SECOND_BASE,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_eq!(disabled.class, DirectProofClass::DirectRam);
    }

    #[test]
    fn bios_write_protect_window_is_non_direct() {
        let mem = Memory::new();
        let published = mem
            .publish_direct_page_proof(
                0xF0000,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_non_direct(&published);
    }

    #[test]
    fn descriptor_snooped_page_is_non_direct() {
        let mut mem = Memory::new();
        mem.set_gdt_snoop_range(0x30000, 0xFF);
        let published = mem
            .publish_direct_page_proof(
                0x30000,
                DirectProofAccess::Write,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_non_direct(&published);
    }

    #[test]
    fn out_of_ram_page_is_non_direct() {
        let mem = Memory::new();
        let published = mem
            .publish_direct_page_proof(
                mem.usable_len() as u32,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_non_direct(&published);
    }

    #[test]
    fn paged_dpmi_physical_mapping_page_is_non_direct() {
        let mut mem = Memory::new();
        mem.add_physical_mapping(0x0030_0000, 0x000A_0000, 0x2000);
        let published = mem
            .publish_direct_page_proof(0x0030_0000, DirectProofAccess::Read, paged(false, false))
            .unwrap();
        assert_non_direct(&published);
        assert_eq!(published.proof.physical_page_id, 0xA0);
    }

    #[test]
    fn unpaged_dpmi_physical_mapping_page_is_non_direct() {
        let mut mem = Memory::new();
        mem.add_physical_mapping(0x0030_0000, 0x000A_0000, 0x2000);
        let published = mem
            .publish_direct_page_proof(
                0x0030_0000,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_non_direct(&published);
        assert_eq!(published.proof.physical_page_id, 0xA0);
    }

    #[test]
    fn smc_notifier_makes_ordinary_ram_watched() {
        let mut mem = Memory::new();
        mem.bind_smc(Some(Arc::new(())));
        let published = mem
            .publish_direct_page_proof(
                0x52000,
                DirectProofAccess::Write,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_eq!(published.class, DirectProofClass::WatchedDirectRam);
        assert_eq!(published.proof.watch_flags, PROOF_WATCH_SOURCE_NOTIFIER);
        assert!(!published.proof.read_ptr.is_null());
        assert!(!published.proof.write_ptr.is_null());
    }

    #[test]
    fn time_machine_write_tracking_makes_write_proofs_non_direct() {
        let mut mem = Memory::new();
        mem.enable_write_tracking();
        let write = mem
            .publish_direct_page_proof(
                0x53000,
                DirectProofAccess::Write,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_non_direct(&write);
        // Reads stay direct: tracking only records stores.
        let read = mem
            .publish_direct_page_proof(
                0x53000,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_eq!(read.class, DirectProofClass::DirectRam);
    }

    #[test]
    fn enabling_write_tracking_stales_preexisting_write_proof() {
        let mut mem = Memory::new();
        let write = mem
            .publish_direct_page_proof(
                0x53000,
                DirectProofAccess::Write,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_eq!(write.class, DirectProofClass::DirectRam);
        assert!(!write.proof.write_ptr.is_null());

        let page = write.proof.physical_page_id;
        mem.register_guarded_page(page);
        let policy_before = mem.direct_memory_policy_epoch();
        let page_generation_before = mem.page_generation(page).unwrap();

        mem.enable_write_tracking();

        assert_ne!(mem.direct_memory_policy_epoch(), policy_before);
        assert_ne!(mem.page_generation(page).unwrap(), page_generation_before);
        let replacement = mem
            .publish_direct_page_proof(
                0x53000,
                DirectProofAccess::Write,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_non_direct(&replacement);
    }

    // ── generations block ────────────────────────────────────────────────

    #[test]
    fn generations_block_address_is_stable_across_memory_moves() {
        let mem = Memory::new();
        let ptr = mem.direct_generations_ptr();
        let mut boxed = Box::new(mem); // moves the Memory value
        assert_eq!(boxed.direct_generations_ptr(), ptr);
        boxed.set_allow_rom_writes(true);
        // The pre-move pointer observes post-move bumps.
        let snap = unsafe { (*ptr).snapshot() };
        assert_ne!(snap.policy_generation, 0);
        assert_ne!(snap.proof_generation, 0);
    }

    #[test]
    fn translation_invalidation_bumps_map_word_before_observation() {
        let mut mem = Memory::new();
        let before = mem.direct_generations().snapshot();
        mem.invalidate_tlb(); // CR3 write / CR0.PG change path
        let after = mem.direct_generations().snapshot();
        assert_ne!(after.map_generation, before.map_generation);
        assert_ne!(after.proof_generation, before.proof_generation);
        assert_eq!(after.map_generation, mem.dynrec_translation_epoch() as u32);

        let before = after;
        mem.invalidate_tlb_page(0x1000); // INVLPG path
        let after = mem.direct_generations().snapshot();
        assert_ne!(after.map_generation, before.map_generation);
        assert_ne!(after.proof_generation, before.proof_generation);
    }

    #[test]
    fn a20_toggle_bumps_a20_map_and_policy_words() {
        let mut mem = Memory::new();
        let before = mem.direct_generations().snapshot();
        mem.set_a20(false);
        let after = mem.direct_generations().snapshot();
        assert_ne!(after.a20_generation, before.a20_generation);
        assert_ne!(after.map_generation, before.map_generation);
        assert_ne!(after.policy_generation, before.policy_generation);
        assert_ne!(after.proof_generation, before.proof_generation);
        assert_eq!(after.a20_generation, mem.dynrec_a20_generation() as u32);
    }

    #[test]
    fn policy_change_bumps_policy_word() {
        let mut mem = Memory::new();
        let before = mem.direct_generations().snapshot();
        mem.set_allow_rom_writes(true);
        let after = mem.direct_generations().snapshot();
        assert_ne!(after.policy_generation, before.policy_generation);
        assert_ne!(after.proof_generation, before.proof_generation);
        assert_eq!(
            after.policy_generation,
            mem.direct_memory_policy_epoch() as u32
        );
    }

    #[test]
    fn descriptor_generation_bumps_at_snoop_and_reload_sites() {
        let mut mem = Memory::new();
        // Snoop-range programming (GDTR reload reflection) bumps.
        let before = mem.direct_generations().snapshot();
        mem.set_gdt_snoop_range(0x30000, 0xFF);
        let after_reload = mem.direct_generations().snapshot();
        assert_ne!(
            after_reload.descriptor_generation,
            before.descriptor_generation
        );

        // A write landing in the snooped range bumps (memory-side snoop).
        let before = after_reload;
        mem.write_u32_physical(0x30010, 0xDEAD_BEEF);
        let after_write = mem.direct_generations().snapshot();
        assert_ne!(
            after_write.descriptor_generation,
            before.descriptor_generation
        );
        // Amended descriptor row: a snooped byte write stamps the
        // descriptor word only — it neither bumps word 0 nor sweeps
        // (page proofs are descriptor-independent; admissions go stale
        // via the stamp).
        assert_eq!(after_write.proof_generation, before.proof_generation);
        assert!(mem.take_gdt_dirty(), "snoop latch fired alongside the bump");

        // Root-driven explicit bump (descriptor reloads observed by the
        // engine loop) goes through the same single counter.
        let before = after_write;
        mem.bump_descriptor_generation();
        let after_explicit = mem.direct_generations().snapshot();
        assert_ne!(
            after_explicit.descriptor_generation,
            before.descriptor_generation
        );
        assert_eq!(
            u64::from(after_explicit.descriptor_generation),
            mem.dynrec_descriptor_generation()
        );

        // A write outside the snooped range does not bump.
        let before = after_explicit;
        mem.write_u32_physical(0x40000, 0x1234_5678);
        let after_unrelated = mem.direct_generations().snapshot();
        assert_eq!(
            after_unrelated.descriptor_generation,
            before.descriptor_generation
        );
    }

    /// WI-G1 source row: a page-targeted source bump advances the named
    /// page's generation word plus the global `source_generation` word —
    /// and NOTHING else (not word 0, not other pages' words).
    #[test]
    fn page_targeted_source_bump_advances_only_the_named_page_and_source_word() {
        let mem = Memory::new();
        let before = mem.direct_generations().snapshot();
        let p_before = mem.page_generation(0x50).unwrap();
        let q_before = mem.page_generation(0x51).unwrap();
        mem.bump_dynrec_source_generation_for_page(0x50);
        let after = mem.direct_generations().snapshot();
        assert_eq!(
            after.source_generation,
            before.source_generation.wrapping_add(1),
            "source word advances (the gap-cache stamp)"
        );
        assert_eq!(
            mem.page_generation(0x50).unwrap(),
            p_before.wrapping_add(1),
            "the named page's word advances"
        );
        assert_eq!(
            mem.page_generation(0x51).unwrap(),
            q_before,
            "other pages' words do not"
        );
        // Source events are per-page and leave global proof word zero unchanged.
        assert_eq!(after.proof_generation, before.proof_generation);
        assert_eq!(after.map_generation, before.map_generation);
        assert_eq!(after.a20_generation, before.a20_generation);
        assert_eq!(after.descriptor_generation, before.descriptor_generation);
        assert_eq!(after.policy_generation, before.policy_generation);
        assert_eq!(mem.dynrec_page_source_bumps(), 1);

        // Callable through a shared borrow (registration-time contract).
        let shared: &Memory = &mem;
        shared.bump_dynrec_source_generation_for_page(0x50);
        assert_eq!(
            mem.direct_generations().snapshot().source_generation,
            after.source_generation.wrapping_add(1)
        );
        assert_eq!(mem.page_generation(0x50).unwrap(), p_before.wrapping_add(2));

        // Out-of-table pages: source word still moves, no page bump counted.
        let bumps = mem.dynrec_page_source_bumps();
        mem.bump_dynrec_source_generation_for_page(u32::MAX - 1);
        assert_eq!(mem.dynrec_page_source_bumps(), bumps);
    }

    /// WI-G1 global rows: every sweeping global-class epoch helper
    /// (paging / A20 / DPMI mapping / policy rows of the invalidation
    /// matrix, plus a descriptor-table MOVE) bumps every guarded page word
    /// alongside its own generation word and word 0. Descriptor stamps
    /// (byte writes, same-range reloads) are amendment-exempt: word only.
    #[test]
    fn global_class_events_sweep_all_guarded_pages() {
        let mut mem = Memory::new();
        mem.register_guarded_page(0x50);
        mem.register_guarded_page(0x51);
        assert_eq!(mem.guarded_page_count(), 2);
        let unguarded_before = mem.page_generation(0x52).unwrap();

        let sweep_words = |mem: &Memory| {
            (
                mem.page_generation(0x50).unwrap(),
                mem.page_generation(0x51).unwrap(),
            )
        };

        let before = sweep_words(&mem);
        let sweeps_before = mem.dynrec_generation_sweeps();
        let desc_before = mem.dynrec_descriptor_generation();
        mem.bump_descriptor_generation();
        assert_eq!(sweep_words(&mem), before, "descriptor stamp must not sweep");
        assert_eq!(mem.dynrec_generation_sweeps(), sweeps_before);
        assert_eq!(mem.dynrec_descriptor_generation(), desc_before + 1);

        // Descriptor-table MOVE row: a snoop-range reload to a DIFFERENT
        // range sweeps (pages of the new range may hold write proofs)...
        // TWICE here, because this move also moves the combined snoop hull,
        // and `recompute_snoop_window`'s TLB flush now carries the
        // translation-policy epoch (MDD-BUG-FLUX-00155), which sweeps in its
        // own right. Same shape as the A20 row below — harmless for equality
        // guards, which only ever compare for inequality.
        let before = sweep_words(&mem);
        let sweeps_before = mem.dynrec_generation_sweeps();
        let swept_before = mem.dynrec_swept_pages_total();
        mem.set_gdt_snoop_range(0x0006_0000, 0xFF);
        let after = sweep_words(&mem);
        assert_eq!(after.0, before.0.wrapping_add(2), "guarded page P swept");
        assert_eq!(after.1, before.1.wrapping_add(2), "guarded page Q swept");
        assert_eq!(mem.dynrec_generation_sweeps(), sweeps_before + 2);
        let before = sweep_words(&mem);
        let sweeps_same = mem.dynrec_generation_sweeps();
        mem.set_gdt_snoop_range(0x0006_0000, 0xFF);
        assert_eq!(
            sweep_words(&mem),
            before,
            "same-range reload must not sweep"
        );
        assert_eq!(mem.dynrec_generation_sweeps(), sweeps_same);
        assert_eq!(mem.dynrec_swept_pages_total(), swept_before + 4);

        // Paging rows (CR3 write / INVLPG channel): ALL guarded pages swept.
        let before = sweep_words(&mem);
        mem.invalidate_tlb();
        assert_eq!(sweep_words(&mem).0, before.0.wrapping_add(1));
        assert_eq!(sweep_words(&mem).1, before.1.wrapping_add(1));
        let before = sweep_words(&mem);
        mem.invalidate_tlb_page(0x1000);
        assert_eq!(sweep_words(&mem).0, before.0.wrapping_add(1));
        assert_eq!(sweep_words(&mem).1, before.1.wrapping_add(1));

        // A20 row: set_a20 calls BOTH bump_memory_policy_epoch and
        // bump_a20_generation — two sweeps, harmless for equality guards.
        let before = sweep_words(&mem);
        mem.set_a20(false);
        assert_eq!(sweep_words(&mem).0, before.0.wrapping_add(2));
        assert_eq!(sweep_words(&mem).1, before.1.wrapping_add(2));

        // DPMI physical-mapping row: ALL guarded pages swept.
        let before = sweep_words(&mem);
        mem.add_physical_mapping(0x0030_0000, 0x000A_0000, 0x2000);
        assert_ne!(sweep_words(&mem).0, before.0);
        assert_ne!(sweep_words(&mem).1, before.1);

        // Memory-policy row: ALL guarded pages swept.
        let before = sweep_words(&mem);
        mem.set_allow_rom_writes(true);
        assert_eq!(sweep_words(&mem).0, before.0.wrapping_add(1));
        assert_eq!(sweep_words(&mem).1, before.1.wrapping_add(1));

        // Unguarded pages are never swept.
        assert_eq!(mem.page_generation(0x52).unwrap(), unguarded_before);

        // clear_guarded_pages empties the interest set: the next sweep
        // bumps nothing (word values hold).
        mem.clear_guarded_pages();
        assert_eq!(mem.guarded_page_count(), 0);
        let before = sweep_words(&mem);
        mem.invalidate_tlb();
        assert_eq!(sweep_words(&mem), before, "no guarded pages, no bumps");
    }

    /// Restore identity flushes cached translations, while WI-G1 routes
    /// direct-proof invalidation through the sweep so every guarded page word
    /// is stale.
    #[test]
    fn restore_invalidation_flushes_tlb_and_sweeps_guarded_pages() {
        let mut mem = Memory::new();
        mem.register_guarded_page(0x50);
        mem.tlb_insert(0x42, 0x100, true, true, false);
        let word_before = mem.page_generation(0x50).unwrap();
        assert!(mem.tlb_lookup(0x42).is_some());

        mem.on_snapshot_restored();

        assert!(
            mem.tlb_lookup(0x42).is_none(),
            "restore must evict pre-restore software-TLB translations"
        );
        assert_ne!(
            mem.page_generation(0x50).unwrap(),
            word_before,
            "restore must bump the word every P-bound guard reads"
        );
    }

    /// WI-G1 pinning discipline (same as the generations block at
    /// `generations_block_address_is_stable_across_memory_moves`): the
    /// page-generation table base is stable across `Memory` moves, and a
    /// pre-move pointer observes post-move bumps.
    #[test]
    fn page_generation_table_is_stable_across_memory_moves() {
        let mem = Memory::new();
        let (base, len) = mem.page_generation_table_ptr();
        assert_eq!(len, mem.page_generation_table_len());
        let word_50 = mem.page_generation_word_addr(0x50).unwrap();
        let boxed = Box::new(mem); // moves the Memory value
        assert_eq!(boxed.page_generation_table_ptr().0, base);
        assert_eq!(boxed.page_generation_word_addr(0x50).unwrap(), word_50);
        boxed.bump_dynrec_source_generation_for_page(0x50);
        // The pre-move pointer observes the post-move bump.
        let observed = unsafe { (*word_50).get() };
        assert_eq!(observed, 1);
        assert_eq!(boxed.page_generation(0x50), Some(1));
    }

    /// Out-of-table pages never publish a word address (they are NonDirect
    /// and must never bind).
    #[test]
    fn out_of_table_pages_have_no_generation_word() {
        let mem = Memory::new();
        let len = mem.page_generation_table_len() as u32;
        assert!(mem.page_generation_word_addr(0).is_some());
        assert!(mem.page_generation_word_addr(len - 1).is_some());
        assert!(mem.page_generation_word_addr(len).is_none());
        assert!(mem.page_generation_word_addr(u32::MAX).is_none());
        assert_eq!(mem.page_generation(len), None);
    }

    /// WI-G1 over-capacity row: growth beyond the pinned capacity retires
    /// the old table (kept alive, never freed) under a new identity epoch.
    #[test]
    fn over_capacity_growth_bumps_table_identity_and_retires_the_old_table() {
        let mut mem = Memory::new();
        let identity_before = mem.page_generation_table_identity();
        let (old_base, _) = mem.page_generation_table_ptr();
        let old_word = mem.page_generation_word_addr(0x50).unwrap();
        mem.bump_dynrec_source_generation_for_page(0x50);
        assert_eq!(mem.page_generation(0x50), Some(1));

        mem.expand_ram(DOS_RAM_CEILING + 0x10_0000);

        assert_eq!(
            mem.page_generation_table_identity(),
            identity_before.wrapping_add(1),
            "over-capacity growth advances the identity epoch"
        );
        let (new_base, new_len) = mem.page_generation_table_ptr();
        assert_ne!(new_base, old_base, "the table was reallocated");
        assert!(new_len >= mem.core.pinned_ram_capacity() >> 12);
        assert_eq!(mem.page_generation(0x50), Some(0), "fresh table words");
        // The retired table stays dereferenceable: bumps through a stale
        // pointer land harmlessly until the consumer resets.
        let stale = unsafe { (*old_word).get() };
        assert_eq!(stale, 1, "retired table retains its values, alive");

        // In-capacity growth never touches the table identity.
        let mut mem = Memory::new();
        let identity = mem.page_generation_table_identity();
        let (base, _) = mem.page_generation_table_ptr();
        mem.resize_ram(0x10_0000);
        mem.ensure_ram_size(0x20_0000);
        assert_eq!(mem.page_generation_table_identity(), identity);
        assert_eq!(mem.page_generation_table_ptr().0, base);
    }

    #[test]
    fn invalidate_all_direct_proofs_bumps_every_word() {
        let mut mem = Memory::new();
        let _ = mem
            .publish_direct_page_proof(
                0x50000,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert!(mem.direct_proofs_live());
        let before = mem.direct_generations().snapshot();
        mem.invalidate_all_direct_proofs();
        let after = mem.direct_generations().snapshot();
        assert_ne!(after.proof_generation, before.proof_generation);
        assert_ne!(after.map_generation, before.map_generation);
        assert_ne!(after.a20_generation, before.a20_generation);
        assert_ne!(after.descriptor_generation, before.descriptor_generation);
        assert_ne!(after.policy_generation, before.policy_generation);
        assert_ne!(after.source_generation, before.source_generation);
        assert!(!mem.direct_proofs_live());
    }

    // ── A20 alias identity ───────────────────────────────────────────────

    #[test]
    fn a20_aliased_linear_pages_share_physical_page_id() {
        let mut mem = Memory::new();
        mem.set_a20(false);
        let low = mem
            .publish_direct_page_proof(
                0x0000_0000,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        let alias = mem
            .publish_direct_page_proof(
                0x0010_0000,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_eq!(low.proof.physical_page_id, 0);
        assert_eq!(
            alias.proof.physical_page_id, low.proof.physical_page_id,
            "A20 aliases must share physical identity"
        );
        assert_eq!(alias.proof.read_ptr, low.proof.read_ptr);
        assert_ne!(alias.proof.linear_page_tag, low.proof.linear_page_tag);

        // Toggling A20 bumps generations before the stale alias proof can be
        // observed, and the alias resolves to its own page afterwards.
        let before = mem.direct_generations().snapshot();
        mem.set_a20(true);
        let after = mem.direct_generations().snapshot();
        assert_ne!(after.a20_generation, before.a20_generation);
        assert_ne!(after.proof_generation, before.proof_generation);
        let unaliased = mem
            .publish_direct_page_proof(
                0x0010_0000,
                DirectProofAccess::Read,
                DirectProofPaging::Disabled,
            )
            .unwrap();
        assert_eq!(unaliased.proof.physical_page_id, 0x100);
    }

    // ── A/D-commit admissibility (HLD 11.2 simple-V0) ───────────────────

    #[test]
    fn read_proof_rejected_when_accessed_bits_uncommitted() {
        let mut mem = Memory::new();
        // PDE Accessed clear.
        setup_page_tables(&mut mem, P | W | U, P | W | U | A | D);
        assert_eq!(
            mem.publish_direct_page_proof(LIN, DirectProofAccess::Read, paged(false, false)),
            Err(ProofUnavailableReason::AdBitsUncommitted)
        );
        // PTE Accessed clear.
        setup_page_tables(&mut mem, P | W | U | A, P | W | U | D);
        assert_eq!(
            mem.publish_direct_page_proof(LIN, DirectProofAccess::Read, paged(false, false)),
            Err(ProofUnavailableReason::AdBitsUncommitted)
        );
    }

    #[test]
    fn write_proof_rejected_when_dirty_uncommitted_but_read_allowed() {
        let mut mem = Memory::new();
        setup_page_tables(&mut mem, P | W | U | A, P | W | U | A);
        assert_eq!(
            mem.publish_direct_page_proof(LIN, DirectProofAccess::Write, paged(false, false)),
            Err(ProofUnavailableReason::AdBitsUncommitted)
        );
        let read = mem
            .publish_direct_page_proof(LIN, DirectProofAccess::Read, paged(false, false))
            .unwrap();
        assert_eq!(read.class, DirectProofClass::DirectRam);
        assert_eq!(read.proof.physical_page_id, FRAME >> 12);
        assert_eq!(read.proof.read_ptr, unsafe {
            mem.ram_ptr().add(FRAME as usize)
        });
    }

    #[test]
    fn write_proof_published_after_committed_walk() {
        let mut mem = Memory::new();
        setup_page_tables(&mut mem, P | W | U | A, P | W | U | A | D);
        let write = mem
            .publish_direct_page_proof(LIN, DirectProofAccess::Write, paged(false, false))
            .unwrap();
        assert_eq!(write.class, DirectProofClass::DirectRam);
        assert_eq!(write.proof.write_ptr.cast_const(), unsafe {
            mem.ram_ptr().add(FRAME as usize)
        });
    }

    #[test]
    fn user_access_to_supervisor_page_is_rejected() {
        let mut mem = Memory::new();
        setup_page_tables(&mut mem, P | W | U | A, P | W | A | D); // PTE U clear
        assert_eq!(
            mem.publish_direct_page_proof(LIN, DirectProofAccess::Read, paged(true, false)),
            Err(ProofUnavailableReason::UserSupervisorViolation)
        );
        // Supervisor access to the same page is fine.
        let read = mem
            .publish_direct_page_proof(LIN, DirectProofAccess::Read, paged(false, false))
            .unwrap();
        assert_eq!(read.class, DirectProofClass::DirectRam);
        assert_eq!(read.proof.perms & PROOF_PERM_USER, 0);
    }

    #[test]
    fn write_to_read_only_page_is_rejected_per_wp_semantics() {
        let mut mem = Memory::new();
        setup_page_tables(&mut mem, P | W | U | A, P | U | A | D); // PTE W clear
                                                                   // User write faults.
        assert_eq!(
            mem.publish_direct_page_proof(LIN, DirectProofAccess::Write, paged(true, false)),
            Err(ProofUnavailableReason::WriteProtected)
        );
        // Supervisor write with CR0.WP faults.
        assert_eq!(
            mem.publish_direct_page_proof(LIN, DirectProofAccess::Write, paged(false, true)),
            Err(ProofUnavailableReason::WriteProtected)
        );
        // Supervisor write with CR0.WP=0 is architecturally allowed (Dirty is
        // already committed here, so it is admissible).
        let write = mem
            .publish_direct_page_proof(LIN, DirectProofAccess::Write, paged(false, false))
            .unwrap();
        assert_eq!(write.class, DirectProofClass::DirectRam);
        assert!(!write.proof.write_ptr.is_null());
        assert_eq!(
            write.proof.perms & PROOF_PERM_WRITE,
            0,
            "page attribute stays RO"
        );
    }

    #[test]
    fn not_present_mappings_are_rejected() {
        let mut mem = Memory::new();
        // PTE not present.
        setup_page_tables(&mut mem, P | W | U | A, W | U | A | D);
        assert_eq!(
            mem.publish_direct_page_proof(LIN, DirectProofAccess::Read, paged(false, false)),
            Err(ProofUnavailableReason::NotPresent)
        );
        // PDE not present.
        setup_page_tables(&mut mem, W | U, P | W | U | A | D);
        assert_eq!(
            mem.publish_direct_page_proof(LIN, DirectProofAccess::Read, paged(false, false)),
            Err(ProofUnavailableReason::NotPresent)
        );
    }

    #[test]
    fn pse_large_page_ad_admissibility() {
        let mut mem = Memory::new();
        let ctx = |pse| DirectProofPaging::Enabled {
            cr3: PD_BASE,
            user: false,
            write_protect: false,
            pse,
        };
        // 4MB PDE mapping linear 0x0040_0000 to physical 0x0040_0000.
        let pde_addr = PD_BASE as usize + 4;
        mem.write_u32_physical(pde_addr, 0x0040_0000 | P | W | U | PS);
        assert_eq!(
            mem.publish_direct_page_proof(0x0040_1000, DirectProofAccess::Read, ctx(true)),
            Err(ProofUnavailableReason::AdBitsUncommitted)
        );
        mem.write_u32_physical(pde_addr, 0x0040_0000 | P | W | U | PS | A);
        let read = mem
            .publish_direct_page_proof(0x0040_1000, DirectProofAccess::Read, ctx(true))
            .unwrap();
        assert_eq!(read.class, DirectProofClass::DirectRam);
        assert_eq!(read.proof.physical_page_id, 0x401);
        // Write needs PDE Dirty on large pages.
        assert_eq!(
            mem.publish_direct_page_proof(0x0040_1000, DirectProofAccess::Write, ctx(true)),
            Err(ProofUnavailableReason::AdBitsUncommitted)
        );
        mem.write_u32_physical(pde_addr, 0x0040_0000 | P | W | U | PS | A | D);
        let write = mem
            .publish_direct_page_proof(0x0040_1000, DirectProofAccess::Write, ctx(true))
            .unwrap();
        assert_eq!(write.class, DirectProofClass::DirectRam);
    }
}
