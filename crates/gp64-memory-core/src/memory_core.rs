//! Machine-independent x86 memory state, extracted in M0c and moved into this
//! crate in M1 (HLD 2026.10.02 R6).
//!
//! The RAM backing and the public fault latch stay on the machine type in M1:
//! engines still read them as public `Memory` fields, and M1 leaves the engines
//! unchanged. Machine address transforms and device/ROM policy stay in the
//! machine's wrappers; every predicate reaches the core
//! as an argument the machine proved (HLD R2).
//!
//! Fields retain their existing names and are ordered by alignment to avoid
//! increasing the embedding machine type; logical grouping must not add
//! per-group tail padding. Fields are crate-private: machines use the methods,
//! and engines see only `&MemoryCore` (LLD 2.3).

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gp64_types::SmcNotify;

use crate::domain::{AccessIntent, BackingOffset, DirectSpan, TlbFill};
use crate::generations::{new_page_generation_table, DirectGenerations, RamBasePtr};
use crate::tlb::{
    SoftwareTlb, TlbEntry, TlbStats, TLB_FLAG_DIRTY, TLB_FLAG_GLOBAL, TLB_FLAG_GLOBAL_LARGE,
    TLB_FLAG_USER, TLB_FLAG_WRITABLE, TLB_INVALID_TAG,
};
use crate::{
    flat_tlb, range_overlaps, INSTR_EXEC_PAGE_SHIFT, INSTR_EXEC_PAGE_SIZE, MEMORY_INSTANCE_NONCE,
};

#[repr(C)]
pub struct MemoryCore {
    /// Software TLB for paging translation cache
    pub(crate) tlb: SoftwareTlb,
    /// TLB hit/miss/invalidation counters (retired env gate; see `TlbStats`)
    pub(crate) tlb_stats: TlbStats,
    /// Epoch for memory-policy changes, including translation-policy changes
    /// such as TLB flushes.
    pub(crate) memory_policy_epoch: u64,
    /// Epoch for direct-memory assumptions that can invalidate generated host
    /// RAM/MMIO/VGA routing or backing-pointer assumptions.
    pub(crate) direct_memory_policy_epoch: u64,
    /// Epoch for paging/TLB translation identity. Unlike
    /// `memory_policy_epoch`, this is not bumped by non-translation memory
    /// policy changes.
    pub(crate) dynrec_translation_epoch: u64,
    /// History of full TLB flushes requested by PGE-capable CR4 writes.
    /// Region premises must retain this history even if CR4 returns to its old value.
    pub(crate) dynrec_global_tlb_flush_epoch: u64,
    /// Epoch for A20 identity, separate from the post-A20 physical page.
    pub(crate) alias_generation: u64,
    /// Optional self-modifying-code tracker.
    pub(crate) smc_notifier: Option<Arc<dyn SmcNotify>>,
    /// Bumped when a guest write lands in a recorded paging-structure frame,
    /// or a page walk records an unprobeable frame (an EMS overlay alias or
    /// a frame beyond the bitmap).
    pub(crate) pt_write_epoch: u64,
    /// Bumped by translation-structure events independently of page-table
    /// writes: DPMI physical-mapping changes, A20 toggles, and restore.
    pub(crate) translation_structure_epoch: u64,
    /// Process-unique identity of this `Memory` instance, taken at construction.
    /// Unlike a heap address or fresh epoch counters, it cannot confuse a
    /// replacement instance with its predecessor.
    pub(crate) instance_nonce: u64,
    /// Pages that have executed instructions (for interpreter cache invalidation).
    pub(crate) instr_exec_pages: Vec<u64>,

    pub(crate) gdt_snoop_base: usize,
    /// End of the GDT region (exclusive physical address).  0 = not set.
    pub(crate) gdt_snoop_end: usize,
    /// Start of the LDT region (physical address).  0 = not set.
    pub(crate) ldt_snoop_base: usize,
    /// End of the LDT region (exclusive physical address).  0 = not set.
    pub(crate) ldt_snoop_end: usize,
    /// Combined GDT+LDT snoop hull [snoop_min, snoop_max): lets the per-write
    /// snoop reject the overwhelmingly common non-descriptor write with two
    /// compares before the per-table range tests. Both 0 when no table is
    /// snooped; maintained solely by the three snoop-range setters.
    pub(crate) snoop_min: usize,
    pub(crate) snoop_max: usize,
    /// Monotonic descriptor-table generation. Bumped by the GDT/LDT write
    /// snoop and snoop-range (re)programming. `Cell` because the write snoop
    /// runs through `&self`.
    pub(crate) descriptor_generation: Cell<u64>,
    /// Stable-heap generation words for direct-memory proofs. Boxed so the
    /// address survives moves of `Memory`; never replaced during its lifetime.
    pub(crate) direct_generations: Box<DirectGenerations>,
    /// Per-page proof generations: one `u32` per 4 KiB of pinned RAM.
    /// Heap-pinned; replaced only by over-capacity `resize_ram_backing`
    /// under a new identity epoch.
    pub(crate) page_generation_table: Box<[Cell<u32>]>,
    /// Tables retired by over-capacity growth — kept alive (never freed)
    /// so stale pointers captured under an older identity epoch remain
    /// dereferenceable until their consumer resets.
    pub(crate) retired_page_generation_tables: Vec<Box<[Cell<u32>]>>,
    /// Identity epoch of `page_generation_table`; bumped only when the
    /// table is reallocated (over-capacity growth).
    pub(crate) page_generation_table_identity: u64,
    /// Guarded-page interest set (WI-G1): physical pages with at least one
    /// proof-slot binding since the last full consumer reset. MONOTONE;
    /// interior-mutable because binds and sweeps run under `&self`.
    pub(crate) guarded_pages: RefCell<HashSet<u32>>,
    /// Cumulative global-class sweeps (`stale_all_guarded_pages`).
    pub(crate) direct_generation_sweeps: Cell<u64>,
    /// Cumulative page words bumped by global-class sweeps.
    pub(crate) direct_swept_pages_total: Cell<u64>,
    /// Cumulative page-targeted source bumps
    /// (`bump_dynrec_source_generation_for_page`, in-table pages).
    pub(crate) direct_page_source_bumps: Cell<u64>,
    /// RAM backing capacity pinned at construction
    /// (`DOS_RAM_CEILING + FETCH_PADDING`; re-pinned larger by over-capacity
    /// `resize_ram_backing` growth, up to `MAX_RAM_SIZE + FETCH_PADDING`).
    /// Growth within this capacity does not reallocate, while every size
    /// change still retires published proofs.
    pub(crate) pinned_ram_capacity: usize,
    /// Mutable-provenance base pointer of the pinned RAM backing
    /// (DD-UNSAFE-1): captured via `RamBacking::host_base` (the mapping, not a
    /// reference) at construction and re-captured by `resize_ram_backing`.
    /// Direct page-proof pointers derive from this, never from a
    /// `&self`-borrowed slice, so stores
    /// through published write pointers are sound under the aliasing model.
    pub(crate) ram_base: RamBasePtr,
    /// Fast guard for watched code pages. Avoids vtable dispatch through
    /// `smc_notifier` on every memory write when no pages are watched.
    pub(crate) smc_has_code_pages: bool,
    /// EXP-N005: fast guard for the funnel probe; set on the first recorded
    /// (probeable) PT frame and never cleared.
    pub(crate) pt_frames_recorded: bool,
    /// EXP-N005 amendment A3: true while inside a page walker accessed/dirty
    /// write-back (`write_u32_physical_ad`). Suppresses ONLY the PT tripwire
    /// classification — SMC notification and every other write side effect
    /// are unchanged. A/D write-backs OR-in status bits no walk conclusion
    /// reads, so they cannot stale a translation premise.
    pub(crate) pt_ad_writeback: bool,
    /// Interpreter instruction cache dirty flag (self-modifying code)
    pub(crate) instr_cache_dirty: AtomicBool,
    /// Set to `true` when a write lands in the GDT region.
    pub(crate) gdt_dirty: Cell<bool>,
    /// Set to `true` when a write lands in the LDT region.
    pub(crate) ldt_dirty: Cell<bool>,
    /// True when at least one direct page proof with usable host pointers has
    /// been published since the last whole-block invalidation. RAM resize
    /// clears this latch before changing the backing allocation or length.
    pub(crate) direct_proofs_live: Cell<bool>,
    /// Bytes of raw RAM backing available for direct physical reads through
    /// the RAM base (`[0, raw_ram_limit)`). Zero means no direct reads.
    /// Machine-owned: PC `Memory` lowers it before a resize bumps any epoch
    /// and raises it only once the new backing exists. Other machines leave
    /// it zero until their backing is ready.
    pub(crate) raw_ram_limit: usize,
}

impl MemoryCore {
    /// A fresh core over a pinned RAM backing.
    ///
    /// `ram_base` is the backing's mutable-provenance base, `pinned_ram_capacity`
    /// the bytes pinned behind it, `instr_exec_pages` the initial decode-cache
    /// page bitmap, and `watch_page_count` the backing pages the SMC watch and
    /// PT-frame bitmaps cover (PC: `MAX_RAM_SIZE >> 12`).
    #[must_use]
    pub fn new(
        ram_base: RamBasePtr,
        pinned_ram_capacity: usize,
        instr_exec_pages: Vec<u64>,
        watch_page_count: usize,
    ) -> Self {
        Self {
            tlb: SoftwareTlb::new(watch_page_count),
            tlb_stats: TlbStats::from_env(),
            memory_policy_epoch: 0,
            direct_memory_policy_epoch: 0,
            dynrec_translation_epoch: 0,
            dynrec_global_tlb_flush_epoch: 0,
            alias_generation: 0,
            smc_notifier: None,
            pt_write_epoch: 0,
            translation_structure_epoch: 0,
            instance_nonce: MEMORY_INSTANCE_NONCE.fetch_add(1, Ordering::Relaxed),
            instr_exec_pages,
            gdt_snoop_base: 0,
            gdt_snoop_end: 0,
            ldt_snoop_base: 0,
            ldt_snoop_end: 0,
            snoop_min: 0,
            snoop_max: 0,
            descriptor_generation: Cell::new(0),
            direct_generations: Box::new(DirectGenerations::new()),
            page_generation_table: new_page_generation_table(pinned_ram_capacity),
            retired_page_generation_tables: Vec::new(),
            page_generation_table_identity: 0,
            guarded_pages: RefCell::new(HashSet::new()),
            direct_generation_sweeps: Cell::new(0),
            direct_swept_pages_total: Cell::new(0),
            direct_page_source_bumps: Cell::new(0),
            pinned_ram_capacity,
            ram_base,
            smc_has_code_pages: false,
            pt_frames_recorded: false,
            pt_ad_writeback: false,
            instr_cache_dirty: AtomicBool::new(false),
            gdt_dirty: Cell::new(false),
            ldt_dirty: Cell::new(false),
            direct_proofs_live: Cell::new(false),
            raw_ram_limit: 0,
        }
    }
}

impl MemoryCore {
    #[inline]
    pub fn memory_policy_epoch(&self) -> u64 {
        self.memory_policy_epoch
    }

    #[inline]
    pub fn direct_memory_policy_epoch(&self) -> u64 {
        self.direct_memory_policy_epoch
    }

    #[inline]
    pub fn dynrec_translation_epoch(&self) -> u64 {
        self.dynrec_translation_epoch
    }

    #[inline]
    pub fn dynrec_global_tlb_flush_epoch(&self) -> u64 {
        self.dynrec_global_tlb_flush_epoch
    }

    #[inline]
    pub fn alias_generation(&self) -> u64 {
        self.alias_generation
    }

    #[inline]
    pub fn dynrec_pt_write_epoch(&self) -> u64 {
        self.pt_write_epoch
    }

    #[inline]
    pub fn dynrec_instance_nonce(&self) -> u64 {
        self.instance_nonce
    }

    #[inline]
    pub fn dynrec_translation_structure_epoch(&self) -> u64 {
        self.translation_structure_epoch
    }

    #[inline]
    pub fn bump_memory_policy_epoch(&mut self) {
        self.flush_tlb_full();
        self.memory_policy_epoch = self.memory_policy_epoch.wrapping_add(1);
        self.direct_memory_policy_epoch = self.direct_memory_policy_epoch.wrapping_add(1);
        self.direct_generations
            .record_policy_epoch(self.direct_memory_policy_epoch);
        self.stale_all_guarded_pages();
    }

    #[inline]
    pub fn bump_direct_translation_policy_epoch(&mut self) {
        self.memory_policy_epoch = self.memory_policy_epoch.wrapping_add(1);
        self.direct_memory_policy_epoch = self.direct_memory_policy_epoch.wrapping_add(1);
        self.dynrec_translation_epoch = self.dynrec_translation_epoch.wrapping_add(1);
        // Physical-mapping changes alter the backing under an unchanged page
        // walk result, so they need a structure epoch bump without a PT write.
        self.translation_structure_epoch = self.translation_structure_epoch.wrapping_add(1);
        self.direct_generations
            .record_policy_epoch(self.direct_memory_policy_epoch);
        self.direct_generations
            .record_map_epoch(self.dynrec_translation_epoch);
        self.stale_all_guarded_pages();
    }

    #[inline]
    pub fn bump_translation_policy_epoch(&mut self) {
        self.memory_policy_epoch = self.memory_policy_epoch.wrapping_add(1);
        self.dynrec_translation_epoch = self.dynrec_translation_epoch.wrapping_add(1);
        self.direct_generations
            .record_map_epoch(self.dynrec_translation_epoch);
        self.stale_all_guarded_pages();
    }

    #[inline]
    pub fn bump_alias_generation(&mut self) {
        self.alias_generation = self.alias_generation.wrapping_add(1);
        self.dynrec_translation_epoch = self.dynrec_translation_epoch.wrapping_add(1);
        // An A20 flip changes physical aliasing without a PT write. Restore
        // also reaches this bump through `invalidate_all_direct_proofs`.
        self.translation_structure_epoch = self.translation_structure_epoch.wrapping_add(1);
        self.direct_generations
            .record_a20_epoch(self.alias_generation);
        self.direct_generations
            .record_map_epoch(self.dynrec_translation_epoch);
        self.stale_all_guarded_pages();
    }

    #[inline]
    pub fn flush_tlb_full(&mut self) {
        if self.tlb.global_count != 0 {
            self.dynrec_global_tlb_flush_epoch = self.dynrec_global_tlb_flush_epoch.wrapping_add(1);
        }
        self.tlb_stats.record_flush_full();
        self.tlb.flush();
    }

    #[inline]
    pub fn bind_smc(&mut self, notifier: Option<Arc<dyn SmcNotify>>) {
        self.smc_notifier = notifier;
    }

    #[inline]
    pub fn smc_snoop_pending(&self) -> bool {
        self.smc_notifier
            .as_ref()
            .is_some_and(|notifier| notifier.is_pending())
    }

    #[inline]
    pub fn has_smc_notifier(&self) -> bool {
        self.smc_notifier.is_some()
    }

    #[inline]
    pub fn take_smc_notifier(&mut self) -> Option<Arc<dyn SmcNotify>> {
        self.smc_notifier.take()
    }

    #[inline]
    pub fn set_smc_has_code_pages(&mut self, has_pages: bool) {
        self.smc_has_code_pages = has_pages;
    }

    #[inline]
    pub fn smc_has_code_pages(&self) -> bool {
        self.smc_has_code_pages
    }

    #[inline]
    pub fn gdt_snoop_base(&self) -> usize {
        self.gdt_snoop_base
    }

    #[inline]
    pub fn gdt_snoop_end(&self) -> usize {
        self.gdt_snoop_end
    }

    #[inline]
    pub fn ldt_snoop_base(&self) -> usize {
        self.ldt_snoop_base
    }

    #[inline]
    pub fn ldt_snoop_end(&self) -> usize {
        self.ldt_snoop_end
    }

    #[inline]
    pub fn has_descriptor_snoop(&self) -> bool {
        self.gdt_snoop_end != 0 || self.ldt_snoop_end != 0
    }

    #[inline]
    pub fn descriptor_snoop_overlaps(&self, phys: usize, len: usize) -> bool {
        phys < self.snoop_max && phys.saturating_add(len) > self.snoop_min
    }

    #[inline]
    pub fn set_gdt_snoop_range(&mut self, base: u32, limit: u16) {
        let new_base = base as usize;
        let new_end = base as usize + limit as usize + 1;
        let moved = self.gdt_snoop_base != new_base || self.gdt_snoop_end != new_end;
        self.gdt_snoop_base = new_base;
        self.gdt_snoop_end = new_end;
        self.gdt_dirty.set(false);
        self.bump_descriptor_generation();
        self.recompute_snoop_window();
        if moved {
            self.stale_all_guarded_pages();
        }
    }

    #[inline]
    pub fn set_ldt_snoop_range(&mut self, base: u32, limit: u32) {
        let new_base = base as usize;
        let new_end = base as usize + limit as usize + 1;
        let moved = self.ldt_snoop_base != new_base || self.ldt_snoop_end != new_end;
        self.ldt_snoop_base = new_base;
        self.ldt_snoop_end = new_end;
        self.ldt_dirty.set(false);
        // LDTR reload: same contract as set_gdt_snoop_range above.
        self.bump_descriptor_generation();
        self.recompute_snoop_window();
        if moved {
            self.stale_all_guarded_pages();
        }
    }

    #[inline]
    pub fn clear_ldt_snoop_range(&mut self) {
        self.ldt_snoop_base = 0;
        self.ldt_snoop_end = 0;
        self.ldt_dirty.set(false);
        self.bump_descriptor_generation();
        self.recompute_snoop_window();
    }

    #[inline]
    pub fn recompute_snoop_window(&mut self) {
        let (lo, hi) = self.snoop_ranges_hull();
        let hull_moved = self.snoop_min != lo || self.snoop_max != hi;
        self.snoop_min = lo;
        self.snoop_max = hi;
        if hull_moved {
            // Pair the flush with the translation-policy epoch, as every other
            // TLB-flush edge does (`invalidate_tlb`, `invalidate_tlb_page`).
            // An unmoved hull changes no translation and needs neither operation.
            self.flush_tlb_full();
            self.bump_translation_policy_epoch();
        }
    }

    /// The hull `[lo, hi)` of the programmed GDT and LDT snoop ranges; `(0,
    /// 0)` when neither is set.
    #[inline(always)]
    pub(crate) fn snoop_ranges_hull(&self) -> (usize, usize) {
        let mut lo = usize::MAX;
        let mut hi = 0usize;
        if self.gdt_snoop_end != 0 {
            lo = lo.min(self.gdt_snoop_base);
            hi = hi.max(self.gdt_snoop_end);
        }
        if self.ldt_snoop_end != 0 {
            lo = lo.min(self.ldt_snoop_base);
            hi = hi.max(self.ldt_snoop_end);
        }
        if hi == 0 {
            lo = 0;
        }
        (lo, hi)
    }

    #[inline]
    pub fn take_gdt_dirty(&self) -> bool {
        let d = self.gdt_dirty.get();
        if d {
            self.gdt_dirty.set(false);
        }
        d
    }

    #[inline]
    pub fn take_ldt_dirty(&self) -> bool {
        let d = self.ldt_dirty.get();
        if d {
            self.ldt_dirty.set(false);
        }
        d
    }

    #[inline]
    pub fn is_instr_cache_dirty(&self) -> bool {
        self.instr_cache_dirty.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn take_instr_cache_dirty(&self) -> bool {
        self.instr_cache_dirty.swap(false, Ordering::Relaxed)
    }

    #[inline]
    pub fn instr_exec_page_marked(&self, page: usize) -> bool {
        let idx = page / 64;
        if idx >= self.instr_exec_pages.len() {
            return false;
        }
        let bit = page % 64;
        (self.instr_exec_pages[idx] & (1u64 << bit)) != 0
    }

    #[inline]
    pub fn mark_instr_cache_dirty_backed_range(&self, addr: usize, end: usize) {
        let start_page = addr >> INSTR_EXEC_PAGE_SHIFT;
        let end_page = (end - 1) >> INSTR_EXEC_PAGE_SHIFT;
        for page in start_page..=end_page {
            if self.instr_exec_page_marked(page) {
                self.instr_cache_dirty.store(true, Ordering::Relaxed);
                break;
            }
        }
    }

    #[inline]
    pub fn clear_instr_exec_pages(&mut self) {
        self.instr_exec_pages.fill(0);
    }

    #[inline]
    pub fn invalidate_tlb(&mut self) {
        self.flush_tlb_full();
        self.bump_translation_policy_epoch();
    }

    #[inline]
    pub fn invalidate_tlb_global_flush(&mut self) {
        let before = self.dynrec_global_tlb_flush_epoch;
        self.invalidate_tlb();
        // A CR4 event must be recorded even when the TLB contains no globals;
        // a full flush that already retired globals records the same event once.
        if self.dynrec_global_tlb_flush_epoch == before {
            self.dynrec_global_tlb_flush_epoch = before.wrapping_add(1);
        }
    }

    #[inline]
    pub fn invalidate_tlb_non_global(&mut self) {
        self.tlb_stats.record_flush_full();
        self.tlb.flush_non_global();
        self.bump_translation_policy_epoch();
    }

    #[inline]
    pub fn tlb_global_source(&self, linear_page: u32) -> Option<(u32, bool)> {
        let entry = &self.tlb.entries[SoftwareTlb::index(linear_page)];
        if entry.linear_page != linear_page || entry.flags & TLB_FLAG_GLOBAL == 0 {
            return None;
        }
        let addr = u32::from_le_bytes([entry.pad[0], entry.pad[1], entry.pad[2], entry.pad[3]]);
        Some((addr, entry.flags & TLB_FLAG_GLOBAL_LARGE != 0))
    }

    #[inline]
    pub fn invalidate_tlb_page(&mut self, linear_addr: u32) {
        self.tlb_stats.record_flush_single();
        self.tlb.invalidate_linear(linear_addr >> 12);
        self.bump_translation_policy_epoch();
    }

    #[inline]
    pub fn tlb_hashed_slot(&self, linear_page: u32) -> &TlbEntry {
        &self.tlb.entries[SoftwareTlb::index(linear_page)]
    }

    #[inline]
    pub fn is_page_watched(&self, physical_page: u32) -> bool {
        self.tlb.flat().is_watched(physical_page)
    }

    #[inline]
    pub fn is_pt_frame_recorded(&self, physical_page: u32) -> bool {
        self.tlb.flat().is_pt_frame(physical_page)
    }

    /// Word `index` of the recorded PT-frame bitmap (physical pages
    /// `index * 64 ..`), for a caller that intersects it with its own page
    /// bitsets a word at a time.
    #[inline]
    #[must_use]
    pub fn pt_frame_word(&self, index: usize) -> u64 {
        self.tlb.flat().pt_frame_word(index)
    }

    #[inline]
    pub fn has_recorded_pt_frames(&self) -> bool {
        self.pt_frames_recorded
    }

    #[inline]
    pub fn pt_tripwire_note_physical_write(&mut self, addr: usize, size: usize) {
        if !self.pt_frames_recorded || self.pt_ad_writeback || size == 0 {
            return;
        }
        let first_page = addr >> 12;
        let last_page = addr.saturating_add(size - 1) >> 12;
        if self.tlb.flat().span_may_be_pt_frame(first_page, last_page) {
            self.pt_write_epoch = self.pt_write_epoch.wrapping_add(1);
        }
    }

    #[inline]
    pub fn tlb_lookup(&self, linear_page: u32) -> Option<(u32, bool, bool, bool)> {
        let result = self.tlb.lookup(linear_page);
        if result.is_some() {
            self.tlb_stats.record_hit();
        } else {
            self.tlb_stats.record_miss();
        }
        result
    }

    #[inline]
    pub fn tlb_entries_ptr(&self) -> *const TlbEntry {
        self.tlb.entries.as_ptr()
    }

    #[inline]
    pub fn tlb_stats(&self) -> &TlbStats {
        &self.tlb_stats
    }

    #[inline]
    pub fn tlb_stats_mut(&mut self) -> &mut TlbStats {
        &mut self.tlb_stats
    }

    #[inline]
    pub fn tlb_stats_summary(&self) -> Option<String> {
        if self.tlb_stats.is_enabled() {
            Some(self.tlb_stats.summary())
        } else {
            None
        }
    }

    #[inline]
    pub fn smc_span_may_hold_code(&self, addr: usize, size: usize) -> bool {
        if size == 0 {
            return false;
        }
        let first_page = addr >> 12;
        let last_page = addr.saturating_add(size - 1) >> 12;
        self.tlb.flat().span_may_hold_code(first_page, last_page)
    }

    #[inline]
    pub fn smc_before_write(&mut self, addr: usize, size: usize) -> bool {
        // Skip the notifier's vtable dispatch when no code pages are watched.
        if !self.smc_has_code_pages {
            return false;
        }
        if size == 0 {
            return false;
        }
        // A store in a recorded paging-structure frame bumps the PT-write
        // epoch. Recorded frames have null flat write views, so direct writes
        // must reach this funnel or call `pt_tripwire_note_physical_write`.
        // The guard above skips the tripwire when no code pages are watched.
        self.pt_tripwire_note_physical_write(addr, size);
        if !self.smc_span_may_hold_code(addr, size) {
            return false;
        }
        if let Some(ref notifier) = self.smc_notifier {
            // Safety: pointer is bound during emulator init and remains valid.
            notifier.notify_write(addr, size);
            true
        } else {
            false
        }
    }
}

impl MemoryCore {
    #[inline]
    pub fn tlb_global_backing_matches(&self, linear_page: u32, backing: u32) -> bool {
        let entry = &self.tlb.entries[SoftwareTlb::index(linear_page)];
        if entry.linear_page != linear_page || entry.flags & TLB_FLAG_GLOBAL == 0 {
            return false;
        }
        if backing & 7 != (1 | u32::from(entry.pad[4])) || backing & 0x100 == 0 {
            return false;
        }
        if entry.flags & TLB_FLAG_GLOBAL_LARGE != 0 {
            backing & 0x80 != 0
                && ((backing & 0xffc0_0000) >> 12 | (linear_page & 0x3ff)) == entry.physical_page
        } else {
            backing >> 12 == entry.physical_page
        }
    }

    #[inline]
    pub fn flat_views_ptr(&self) -> *const u64 {
        self.tlb.flat().base_ptr()
    }

    #[inline]
    pub fn flat_projection_suppressed(&self) -> bool {
        self.tlb.flat_projection_suppressed()
    }

    #[inline]
    pub fn flat_projection_counters(&self) -> (u64, u64, u64) {
        let f = self.tlb.flat();
        (
            f.watch_transitions(),
            f.watch_slots_scanned(),
            f.bias_zero_collisions(),
        )
    }

    #[inline]
    pub fn flat_mutation_epoch(&self) -> u64 {
        self.tlb.flat().mutation_epoch()
    }

    #[inline]
    pub fn flat_mutation_epoch_ptr(&self) -> *const u64 {
        self.tlb.flat().mutation_epoch_ptr()
    }

    #[inline]
    pub fn flat_read_published(&self, linear_page: u32) -> bool {
        let flat = self.tlb.flat();
        flat.entry(flat_tlb::FlatView::Read0, linear_page) != 0
            || flat.entry(flat_tlb::FlatView::Read3, linear_page) != 0
    }

    #[inline]
    pub fn mark_tlb_global(
        &mut self,
        linear_page: u32,
        backing_addr: u32,
        large: bool,
        leaf_permissions: u8,
    ) {
        let entry = &mut self.tlb.entries[SoftwareTlb::index(linear_page)];
        if entry.linear_page == linear_page {
            if entry.flags & TLB_FLAG_GLOBAL == 0 {
                self.tlb.global_count += 1;
            }
            entry.flags = (entry.flags & !TLB_FLAG_GLOBAL_LARGE)
                | TLB_FLAG_GLOBAL
                | if large { TLB_FLAG_GLOBAL_LARGE } else { 0 };
            entry.pad[..4].copy_from_slice(&backing_addr.to_le_bytes());
            entry.pad[4] = leaf_permissions;
        }
    }

    #[inline]
    pub fn tlb_mark_large(&mut self, page: u32) {
        self.tlb.mark_large_page(page);
    }

    #[inline]
    pub fn set_page_watched(&mut self, page: u32, on: bool) {
        self.tlb.set_page_watched(page, on);
    }
    pub fn record_pt_frame(&mut self, page: u32) {
        if self.tlb.record_pt_frame(page) {
            self.pt_frames_recorded = true;
        }
    }
    pub fn note_unprobeable_pt_frame(&mut self) {
        self.pt_write_epoch = self.pt_write_epoch.wrapping_add(1);
    }

    /// Decode-cache dirty duty for a commit to `[addr, addr + size)`.
    ///
    /// `private_len` yields the length of the machine's contiguous private
    /// backing from offset 0, and `backed_extents` every mounted extent for a
    /// span that leaves it (PC: `RamBacking::len` and `backed_extents`). Both
    /// are lazy so the early exits read no machine state, as before M1. The
    /// sparse layout is the machine's; the core only walks what it is given,
    /// and never inspects a no-access hole between extents.
    #[inline]
    pub fn mark_instr_cache_dirty_range<I>(
        &mut self,
        private_len: impl FnOnce() -> usize,
        backed_extents: impl FnOnce() -> I,
        addr: usize,
        size: usize,
    ) where
        I: Iterator<Item = Range<usize>>,
    {
        if size == 0 || self.instr_cache_dirty.load(Ordering::Relaxed) {
            return;
        }
        let bitmap_end = self.instr_exec_pages.len() * 64 * INSTR_EXEC_PAGE_SIZE;
        let end = addr.saturating_add(size).min(bitmap_end);
        if end <= addr {
            return;
        }
        // Private writes stay on the usual contiguous path. Sparse writes
        // inspect only mounted extents, never the no-access holes between them.
        let private_len = private_len();
        if addr < private_len && end <= private_len {
            self.mark_instr_cache_dirty_backed_range(addr, end);
            return;
        }
        let dirty = backed_extents().any(|extent| {
            let start = addr.max(extent.start);
            let end = end.min(extent.end);
            start < end
                && ((start >> INSTR_EXEC_PAGE_SHIFT)..=((end - 1) >> INSTR_EXEC_PAGE_SHIFT))
                    .any(|page| self.instr_exec_page_marked(page))
        });
        if dirty {
            self.instr_cache_dirty.store(true, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn mark_instr_exec_page(&mut self, addr: usize) {
        let page = addr >> INSTR_EXEC_PAGE_SHIFT;
        let idx = page / 64;
        if idx >= self.instr_exec_pages.len() {
            self.instr_exec_pages.resize(idx + 1, 0);
        }
        let bit = page % 64;
        self.instr_exec_pages[idx] |= 1u64 << bit;
    }

    #[inline]
    pub fn snoop_descriptor_write(&self, addr: usize, write_len: usize) -> bool {
        if self.ldt_snoop_end != 0
            && range_overlaps(
                addr,
                write_len,
                self.ldt_snoop_base,
                self.ldt_snoop_end.saturating_sub(self.ldt_snoop_base),
            )
        {
            self.ldt_dirty.set(true);
            // Descriptor bytes changed: stamp the descriptor generation only.
            // Pages in a snooped range are NonDirect at publication.
            self.bump_descriptor_generation();
        }
        if self.gdt_snoop_end != 0
            && range_overlaps(
                addr,
                write_len,
                self.gdt_snoop_base,
                self.gdt_snoop_end.saturating_sub(self.gdt_snoop_base),
            )
        {
            self.gdt_dirty.set(true);
            // Descriptor bytes changed: stamp the descriptor generation only
            // (see the LDT arm above).
            self.bump_descriptor_generation();
            true
        } else {
            false
        }
    }
}

// ── M1 machine-facing API (HLD R2, LLD 2.2) ─────────────────────────────────
impl MemoryCore {
    /// Byte offset of the hashed TLB inside the core (layout pin, LLD 2.1).
    pub const TLB_OFFSET: usize = std::mem::offset_of!(MemoryCore, tlb);
    /// Byte offset of the `smc_has_code_pages` fast guard (layout pin).
    pub const SMC_HAS_CODE_PAGES_OFFSET: usize =
        std::mem::offset_of!(MemoryCore, smc_has_code_pages);
    /// Byte offset of the direct physical RAM read limit.
    pub const RAW_RAM_LIMIT_OFFSET: usize = std::mem::offset_of!(MemoryCore, raw_ram_limit);

    /// Bytes of RAM available for direct physical reads; zero means none.
    #[inline]
    #[must_use]
    pub fn raw_ram_limit(&self) -> usize {
        self.raw_ram_limit
    }

    /// Publish the machine's direct-read RAM limit.
    ///
    /// # Safety
    /// Every byte of `[0, limit)` must be committed host backing that the
    /// engines' RAM base addresses, and must stay so until the limit is
    /// lowered again. Lower it ([`Self::lower_raw_ram_limit`]) before the
    /// backing shrinks or moves, and before the epoch bump that announces
    /// the change (LLD E4).
    #[inline]
    pub unsafe fn set_raw_ram_limit(&mut self, limit: usize) {
        self.raw_ram_limit = limit;
    }

    /// Lower the direct-read RAM limit to at most `limit`. Always safe: a
    /// lower limit only sends more reads to the checked helpers.
    #[inline]
    pub fn lower_raw_ram_limit(&mut self, limit: usize) {
        self.raw_ram_limit = self.raw_ram_limit.min(limit);
    }

    /// Publish one translation (LLD 2.2 `tlb_fill`).
    ///
    /// Host pointers derive from the core's mutable-provenance RAM base plus
    /// the proof's backing page, never from the guest physical page. The read
    /// view is published when `direct.read()`, the write view when
    /// `direct.write() && writable && dirty`; `None` publishes both null.
    ///
    /// # Safety
    /// When `fill.direct` is `Some`, the machine guarantees LLD 1.4 V1-V6 for
    /// the whole backing page at the proof's intents: the page is committed
    /// backing inside the allocation `ram_base` addresses, and reading (and,
    /// for a write proof, writing) it is equivalent to the machine's checked
    /// access minus the core duties. D2 (`backing == phys`) must hold; debug
    /// builds assert it here.
    #[inline]
    pub unsafe fn tlb_fill(&mut self, fill: TlbFill) {
        let host_page = match fill.direct {
            Some(direct) => {
                debug_assert_eq!(
                    direct.backing().0,
                    fill.phys.0,
                    "LLD D2: a TLB proof's backing page must equal its physical page"
                );
                // SAFETY: the caller proved the backing page lies inside the
                // committed allocation `ram_base` addresses (V4).
                let page = unsafe { self.ram_base.0.add((direct.backing().0 as usize) << 12) };
                (page, direct.read(), direct.write())
            }
            None => (std::ptr::null_mut(), false, false),
        };
        // SAFETY: forwarded from this function's contract.
        unsafe {
            self.tlb.insert(
                fill.linear.0,
                fill.phys.0,
                fill.perms.writable,
                fill.perms.user,
                fill.perms.dirty,
                host_page.0,
                host_page.1,
                host_page.2,
            );
        }
    }

    /// Build a direct-span proof over `[backing, backing + len)` for `intent`
    /// from the core's RAM base (LLD 2.2 `direct_span`).
    ///
    /// # Safety
    /// The machine guarantees LLD 1.4 V1-V6 and D2 for the span. In M1 the core
    /// does not own the backing allocation, so residency (V4) is entirely the
    /// machine's proof; the core checks only address arithmetic.
    #[inline]
    #[must_use]
    pub unsafe fn direct_span(
        &self,
        backing: BackingOffset,
        len: usize,
        intent: AccessIntent,
    ) -> Option<DirectSpan> {
        backing.0.checked_add(len)?;
        // SAFETY: the caller proved the span lies inside the backing.
        let ptr = unsafe { self.ram_base.0.add(backing.0) };
        // SAFETY: V1-V6 are forwarded from this function's contract.
        std::ptr::NonNull::new(ptr)
            .map(|ptr| unsafe { DirectSpan::from_raw_parts(ptr, backing, len, intent) })
    }

    /// Host pointer for a read a caller may serve with a plain host load: one
    /// 4 KiB page, a live hashed entry with a non-null `host_read`, reachable
    /// at the caller's CPL. This is the core's TLB predicate only; a machine
    /// with a content-dependent read filter refuses before it (PC:
    /// `Memory::tlb_direct_read_ptr`, the firmware MP-table filter).
    ///
    /// Callers must not cache the result: it is valid only until the next
    /// mutation of the machine.
    #[inline]
    pub fn tlb_direct_read_ptr(&self, linear: u32, width: usize, cpl3: bool) -> Option<*const u8> {
        if width == 0 || (linear as usize & 0xFFF) + width > 0x1000 {
            return None;
        }
        // SAFETY: the check above keeps `[linear, linear + width)` in one page.
        unsafe { self.tlb_direct_read_ptr_in_page(linear, cpl3) }
    }

    /// The TLB half of [`Self::tlb_direct_read_ptr`], for a machine that runs
    /// its own refusals between the one-page check and the TLB lookup (PC:
    /// the firmware MP-table filter) and must keep the predicate one inline
    /// body.
    ///
    /// # Safety
    /// The caller proved the read non-empty and inside one 4 KiB page
    /// starting at `linear`; the returned pointer is valid for that read only.
    #[inline(always)]
    pub unsafe fn tlb_direct_read_ptr_in_page(&self, linear: u32, cpl3: bool) -> Option<*const u8> {
        let linear_page = linear >> 12;
        let entry = &self.tlb().entries[SoftwareTlb::index(linear_page)];
        if entry.linear_page != linear_page {
            return None;
        }
        if cpl3 && entry.flags & TLB_FLAG_USER == 0 {
            return None;
        }
        if entry.host_read.is_null() {
            return None;
        }
        // SAFETY: a non-null `host_read` is `ram_base + (physical_page << 12)`
        // for a page proved fully inside the RAM allocation, and the caller's
        // one-page proof keeps the read inside that same page.
        Some(unsafe { entry.host_read.add((linear & 0xFFF) as usize) })
    }

    /// Whether a commit to `[addr, addr + size)` would set the decoded-
    /// instruction cache's dirty flag: the flag is clear and an exec-marked
    /// page overlaps the span. Uses the same page test as
    /// `Memory::mark_instr_cache_dirty_range`, without marking anything.
    /// Conservative over sparse holes: a marked page counts even if unbacked.
    #[inline(never)]
    pub fn store_would_dirty_instr_cache(&self, addr: usize, size: usize) -> bool {
        if size == 0 || self.is_instr_cache_dirty() {
            return false;
        }
        let bitmap_end = self.instr_exec_pages().len() * 64 * INSTR_EXEC_PAGE_SIZE;
        let end = addr.saturating_add(size).min(bitmap_end);
        if end <= addr {
            return false;
        }
        ((addr >> INSTR_EXEC_PAGE_SHIFT)..=((end - 1) >> INSTR_EXEC_PAGE_SHIFT))
            .any(|page| self.instr_exec_page_marked(page))
    }

    /// Could a recorded paging-structure frame lie in the physical page
    /// range `[first_page, end_page)`?
    ///
    /// False while no probeable frame has been recorded anywhere. Once one
    /// has, pages outside the bitmap answer true, as in
    /// `FlatTlb::span_may_be_pt_frame`. An empty range answers false.
    #[inline]
    pub fn has_recorded_pt_frame_in(&self, first_page: u32, end_page: u32) -> bool {
        self.has_recorded_pt_frames()
            && first_page < end_page
            && self
                .tlb()
                .flat()
                .span_may_be_pt_frame(first_page as usize, end_page as usize - 1)
    }

    /// Census-only: why the flat view for `linear_page` (read or write view,
    /// at the given CPL) is null, decided from the authoritative hashed entry
    /// in the same order `SoftwareTlb::insert` folds its predicates. Never a
    /// translation answer, and nothing behavioural may read it.
    pub fn flat_null_cause(
        &self,
        linear_page: u32,
        write: bool,
        cpl3: bool,
    ) -> flat_tlb::FlatNullCause {
        use flat_tlb::FlatNullCause;
        let entry = &self.tlb().entries[SoftwareTlb::index(linear_page)];
        if entry.linear_page == TLB_INVALID_TAG {
            return FlatNullCause::TlbEmpty;
        }
        if entry.linear_page != linear_page {
            return FlatNullCause::TlbConflict;
        }
        if cpl3 && entry.flags & TLB_FLAG_USER == 0 {
            return FlatNullCause::UserDenied;
        }
        if entry.host_read.is_null() {
            return FlatNullCause::NotRam;
        }
        if write {
            if entry.flags & TLB_FLAG_WRITABLE == 0 {
                return FlatNullCause::WriteReadOnly;
            }
            if entry.flags & TLB_FLAG_DIRTY == 0 {
                return FlatNullCause::WriteNotDirty;
            }
            if entry.host_write.is_null() {
                return FlatNullCause::WriteWithheld;
            }
            let flat = self.tlb().flat();
            if flat.is_watched(entry.physical_page) {
                return FlatNullCause::WriteCodeWatched;
            }
            if flat.is_pt_frame(entry.physical_page) {
                return FlatNullCause::WritePtFrame;
            }
        }
        FlatNullCause::Other
    }

    /// `Memory::tlb_direct_write_ptr` plus the span's guest physical address.
    ///
    /// Admission is identical. A caller committing through the host pointer
    /// still owes the decoded-instruction cache a `drc_note_direct_ram_write`,
    /// which is keyed by physical address. Recovering it by host-pointer
    /// subtraction would assume a RAM base this module does not publish.
    /// The returned physical address is already A20-masked by `tlb_insert`.
    #[inline]
    pub fn tlb_direct_write_span(
        &self,
        linear: u32,
        width: usize,
        cpl3: bool,
    ) -> Option<(*mut u8, usize)> {
        if width == 0 || (linear as usize & 0xFFF) + width > 0x1000 {
            return None;
        }
        let linear_page = linear >> 12;
        let entry = &self.tlb().entries[SoftwareTlb::index(linear_page)];
        if entry.linear_page != linear_page {
            return None;
        }
        if cpl3 && entry.flags & TLB_FLAG_USER == 0 {
            return None;
        }
        // Refuse recorded paging-structure frames alongside watched pages:
        // both must reach `Memory::smc_before_write` for the PT-write tripwire.
        // Separate guards are needed because `record_pt_frame` nulls only the
        // flat write views; the hashed `entry.host_write` survives refills.
        // Otherwise direct page-table edits can miss the epoch bump
        // (MDD-BUG-FLUX-00122).
        if entry.host_write.is_null()
            || self.tlb().flat().is_watched(entry.physical_page)
            || self.tlb().flat().is_pt_frame(entry.physical_page)
        {
            return None;
        }
        let page_offset = (linear & 0xFFF) as usize;
        let physical = ((entry.physical_page as usize) << 12) | page_offset;
        // SAFETY: a non-null host_write points at a complete ordinary RAM
        // page, and the single-page check keeps the whole operand inside it.
        Some((unsafe { entry.host_write.add(page_offset) }, physical))
    }

    /// Read-only view of the hashed table and its flat projection.
    #[inline]
    #[must_use]
    pub fn tlb(&self) -> &SoftwareTlb {
        &self.tlb
    }

    /// Drop one linear page with no epoch bump (a span walk whose caller owns
    /// the epoch edge).
    #[inline]
    pub fn tlb_invalidate_page_quiet(&mut self, linear_page: u32) {
        self.tlb_stats.record_flush_single();
        self.tlb.invalidate_page(linear_page);
    }

    /// Oracle O3: arm or disarm flat-projection suppression.
    #[cfg(any(test, debug_assertions, feature = "test-instrumentation"))]
    pub fn set_flat_projection_suppressed(&mut self, on: bool) {
        self.tlb.set_flat_suppressed(on);
    }

    /// Mutable-provenance base of the RAM backing (DD-UNSAFE-1).
    #[inline]
    #[must_use]
    pub fn ram_base_ptr(&self) -> *mut u8 {
        self.ram_base.0
    }

    /// Re-capture the RAM base after the machine resized its backing.
    #[inline]
    pub fn set_ram_base(&mut self, ram_base: RamBasePtr) {
        self.ram_base = ram_base;
    }

    /// Bytes of RAM backing pinned behind `ram_base`.
    #[inline]
    #[must_use]
    pub fn pinned_ram_capacity(&self) -> usize {
        self.pinned_ram_capacity
    }

    /// Record over-capacity growth of the pinned backing to `capacity` bytes.
    ///
    /// The per-page generation table is sized by the pinned capacity, so it is
    /// reallocated under a new identity epoch. The old table is RETIRED, never
    /// freed: a pointer captured under the old identity stays dereferenceable
    /// until its consumer observes the identity mismatch and resets (WI-G1).
    #[inline]
    pub fn grow_pinned_ram_capacity(&mut self, capacity: usize) {
        self.pinned_ram_capacity = capacity;
        let retired = std::mem::replace(
            &mut self.page_generation_table,
            new_page_generation_table(capacity),
        );
        self.retired_page_generation_tables.push(retired);
        self.page_generation_table_identity = self.page_generation_table_identity.wrapping_add(1);
    }

    /// Grow the physical watch and PT-frame coverage to `pages` 4 KiB pages,
    /// for a machine whose physical space gains backed pages above its pinned
    /// RAM. Never shrinks; linear view slots keep their address.
    pub fn ensure_watch_pages(&mut self, pages: usize) {
        self.tlb.ensure_watch_pages(pages);
    }

    /// Grow the per-physical-page generation table to cover `bytes` of
    /// physical space, for the same case as [`Self::ensure_watch_pages`].
    ///
    /// Like [`Self::grow_pinned_ram_capacity`], the old table is RETIRED,
    /// never freed, under a new identity epoch, so a pointer captured under
    /// the old identity stays dereferenceable until its consumer resets. The
    /// pinned RAM capacity is unchanged. Never shrinks.
    pub fn ensure_page_generation_coverage(&mut self, bytes: usize) {
        if self.page_generation_table.len() >= bytes >> 12 {
            return;
        }
        let retired = std::mem::replace(
            &mut self.page_generation_table,
            new_page_generation_table(bytes),
        );
        self.retired_page_generation_tables.push(retired);
        self.page_generation_table_identity = self.page_generation_table_identity.wrapping_add(1);
    }

    /// Bump the global source-identity word (word 5) alone.
    #[inline]
    pub fn bump_source_generation_word(&self) {
        self.direct_generations.bump_source_word();
    }

    /// Set or clear the direct-proof liveness latch.
    #[inline]
    pub fn set_direct_proofs_live(&self, live: bool) {
        self.direct_proofs_live.set(live);
    }

    /// Mark the decode cache dirty unconditionally.
    #[inline]
    pub fn set_instr_cache_dirty(&self) {
        self.instr_cache_dirty.store(true, Ordering::Relaxed);
    }

    /// The decode-cache executed-page bitmap.
    #[inline]
    #[must_use]
    pub fn instr_exec_pages(&self) -> &[u64] {
        &self.instr_exec_pages
    }

    /// Resize the executed-page bitmap to `words` 64-bit words.
    pub fn resize_instr_exec_pages(&mut self, words: usize) {
        self.instr_exec_pages.resize(words, 0);
    }

    /// Bump the PT-write tripwire epoch for an event that stales every
    /// translation premise at once (PC: physical-state swap).
    #[inline]
    pub fn bump_pt_write_epoch(&mut self) {
        self.pt_write_epoch = self.pt_write_epoch.wrapping_add(1);
    }

    /// Enter or leave a page-walker accessed/dirty write-back window, which
    /// suppresses only the PT tripwire classification (EXP-N005 A3).
    #[inline]
    pub fn set_pt_ad_writeback(&mut self, on: bool) {
        self.pt_ad_writeback = on;
    }

    /// The combined GDT+LDT snoop hull `[min, max)`; `(0, 0)` when nothing is
    /// snooped.
    #[inline]
    #[must_use]
    pub fn snoop_hull(&self) -> (usize, usize) {
        (self.snoop_min, self.snoop_max)
    }
}
