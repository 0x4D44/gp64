//! Transactional physical-map publication.
//!
//! A machine that changes its physical decode between engine entries opens a
//! [`Publication`] with [`MemoryCore::begin_publication`], installs its new map,
//! and commits a [`PublicationPlan`]. The commit revokes every cached view
//! first, records the revoked pages for sever delivery, and only then moves
//! each observable word, each exactly once:
//!
//! 1. snapshot the global-entry count (in `begin_publication`);
//! 2. the machine installs its map (between begin and commit);
//! 3. revoke: the hashed TLB and its flat views, the direct-proof latch;
//!    collect the pending bumps without applying any;
//! 4. sever: hand the revoked linear pages to [`MemoryCore`]'s sever stage
//!    (currently a no-op whose position is pinned by the counter test);
//! 5. the pending bumps: per-page generation words, then the proof, source,
//!    descriptor and PT-write words once each;
//! 6. the epoch set for the [`KindSet`], once;
//! 7. the global TLB-flush epoch, once, if the pre-flush count was non-zero;
//! 8. the decode-cache clear, then the SMC notifier over the changed ranges;
//! 9. the receipt.
//!
//! A panic between begin and the receipt poisons the core: every later
//! [`MemoryCore::enter_engine`] and [`MemoryCore::begin_publication`] panics, so
//! the machine never resumes on half-applied state.

use std::ops::BitOr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use crate::memory_core::MemoryCore;
use crate::tlb::{TLB_INVALID_TAG, TLB_SIZE};

/// Which publication kinds one commit covers (LLD §5.2 table).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KindSet(u8);

impl KindSet {
    /// No kind: nothing to publish.
    pub const EMPTY: Self = Self(0);
    /// The backing under unchanged walks moved (a BAR move or enable).
    pub const DIRECT_TRANSLATION: Self = Self(1);
    /// Bytes changed with no write (the MCPX overlay flip).
    pub const ALIAS: Self = Self(2);
    /// Both at once (an NV2A BAR1 alias move): the union of the bumps.
    pub const BOTH: Self = Self(3);

    #[inline]
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[inline]
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for KindSet {
    type Output = Self;

    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// A descriptor table, for [`MemoryCore::mark_descriptor_dirty`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DescriptorTable {
    Gdt,
    Ldt,
}

/// What the machine changed: the input to [`Publication::commit`].
#[derive(Clone, Copy, Debug, Default)]
pub struct PublicationPlan<'a> {
    /// Canonical page ids whose decode changed (old and new backing pages of
    /// every page whose decode differs), ascending without duplicates. Their
    /// per-page generation words move.
    pub affected_pages: &'a [u32],
    /// Canonical byte ranges `[start, end)` whose decode changed, old and new.
    /// The descriptor snoop and the SMC notifier see these.
    pub changed_ranges: &'a [(usize, usize)],
}

/// What one commit did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PublicationReceipt {
    /// The kinds this commit published; [`KindSet::EMPTY`] when the machine
    /// published nothing.
    pub kinds: KindSet,
    /// Hashed-TLB linear pages revoked and handed to the sever stage.
    pub revoked_linear_pages: usize,
    /// Per-page generation words bumped (guarded pages and affected pages,
    /// each once).
    pub page_words_bumped: usize,
    /// Whether a descriptor table's bytes changed, marking it dirty.
    pub descriptor_tables_marked: bool,
    /// Whether the global TLB-flush epoch moved.
    pub global_flush_epoch_bumped: bool,
}

/// The descriptor reprojection the commit applies. The machine computes it
/// inside the transaction, after the revoke and before
/// the sever, and moves no observable word itself. Step 5 applies this:
/// the core snoop ranges, the moved tables' dirty flags, and one descriptor
/// generation bump shared with the changed-range test.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DescriptorReprojection {
    /// The GDT's new core snoop range `[start, end)`, when it changed.
    pub gdt_range: Option<(usize, usize)>,
    /// The LDT's new core snoop range `[start, end)`, when it changed.
    pub ldt_range: Option<(usize, usize)>,
    /// The bytes behind the linear GDT changed: mark it dirty.
    pub gdt_moved: bool,
    /// The bytes behind the linear LDT changed: mark it dirty.
    pub ldt_moved: bool,
}

/// The commit's stages, in order, for the counter test's probe.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PublicationStage {
    Revoked,
    Severed,
    PendingBumpsApplied,
    EpochSetApplied,
    GlobalFlushEpoch,
    DecodeCacheCleared,
    SmcNotified,
}

/// Engine-entry depth and the poison latch, shared with every live
/// [`EngineEntry`]. Behind an `Arc` so an entry guard borrows nothing: an
/// engine holds `&mut M` for the whole run, and a guard borrowing the core
/// would make the P2 assert unreachable. It trails the core's boxed
/// `DirectGenerations` block, so the core's own layout is unchanged.
#[derive(Debug, Default)]
pub(crate) struct EngineGate {
    depth: AtomicU32,
    poisoned: AtomicBool,
}

/// RAII engine entry (LLD §11.3 P2). While one is alive,
/// [`MemoryCore::begin_publication`] panics.
#[derive(Debug)]
#[must_use = "dropping the entry leaves the engine at once"]
pub struct EngineEntry {
    gate: Arc<EngineGate>,
}

impl Drop for EngineEntry {
    fn drop(&mut self) {
        self.gate.depth.fetch_sub(1, Ordering::Relaxed);
    }
}

/// An open publication. Commit it with [`Publication::commit`]; dropping it
/// uncommitted (including by unwinding) poisons the core.
#[must_use = "an uncommitted publication poisons the core"]
pub struct Publication<'a> {
    core: &'a mut MemoryCore,
    kinds: KindSet,
    global_count_before: usize,
    committed: bool,
}

impl MemoryCore {
    /// Enter an interpreter run.
    ///
    /// Take this once per `run_ext*` entry, never per instruction. The guard
    /// clones an `Arc` and increments a depth counter, with atomics on both
    /// entry and drop.
    ///
    /// # Panics
    /// If the core is poisoned by an unwound publication.
    pub fn enter_engine(&self) -> EngineEntry {
        assert!(
            !self.publication_poisoned(),
            "fatal invariant: engine entry on a core poisoned by an unwound publication"
        );
        self.direct_generations
            .engine_gate
            .depth
            .fetch_add(1, Ordering::Relaxed);
        EngineEntry {
            gate: Arc::clone(&self.direct_generations.engine_gate),
        }
    }

    /// Live engine entries.
    #[inline]
    #[must_use]
    pub fn engine_depth(&self) -> u32 {
        self.direct_generations
            .engine_gate
            .depth
            .load(Ordering::Relaxed)
    }

    /// True once a publication unwound before its receipt.
    #[inline]
    #[must_use]
    pub fn publication_poisoned(&self) -> bool {
        self.direct_generations
            .engine_gate
            .poisoned
            .load(Ordering::Relaxed)
    }

    /// Open a publication of `kinds` and snapshot the global-entry count
    /// (step 1). The machine installs its map next, then commits.
    ///
    /// # Panics
    /// In every build (amendment A-M5-2): if an engine entry is live, if the
    /// core is poisoned, or if `kinds` is empty.
    pub fn begin_publication(&mut self, kinds: KindSet) -> Publication<'_> {
        assert!(
            !self.publication_poisoned(),
            "fatal invariant: publication on a poisoned core"
        );
        assert_eq!(
            self.engine_depth(),
            0,
            "P2: physical-map publication inside an engine entry"
        );
        assert!(!kinds.is_empty(), "a publication needs at least one kind");
        let global_count_before = self.tlb.global_count;
        Publication {
            core: self,
            kinds,
            global_count_before,
            committed: false,
        }
    }

    /// Bump the per-page generation words of `pages` only (A-M5-9): no global
    /// word moves. Pages past the table are skipped.
    pub fn bump_page_generation_words(&self, pages: &[u32]) -> usize {
        let mut bumped = 0;
        for &page in pages {
            if let Some(word) = self.page_generation_table.get(page as usize) {
                word.set(word.get().wrapping_add(1));
                bumped += 1;
            }
        }
        bumped
    }

    /// The deduplicated epoch set for `kinds` (LLD §11.3 table). Each counter
    /// and each `DirectGenerations` record moves once. Guarded pages are
    /// staled by the commit's pending bumps, not here (§13.2).
    fn apply_epoch_set(&mut self, kinds: KindSet) {
        if kinds.contains(KindSet::DIRECT_TRANSLATION) {
            self.memory_policy_epoch = self.memory_policy_epoch.wrapping_add(1);
            self.direct_memory_policy_epoch = self.direct_memory_policy_epoch.wrapping_add(1);
            self.direct_generations
                .record_policy_epoch(self.direct_memory_policy_epoch);
        }
        self.dynrec_translation_epoch = self.dynrec_translation_epoch.wrapping_add(1);
        self.translation_structure_epoch = self.translation_structure_epoch.wrapping_add(1);
        self.direct_generations
            .record_map_epoch(self.dynrec_translation_epoch);
        if kinds.contains(KindSet::ALIAS) {
            self.alias_generation = self.alias_generation.wrapping_add(1);
            self.direct_generations
                .record_a20_epoch(self.alias_generation);
        }
    }

    /// Step 5 of a reprojecting commit: install the reprojected snoop ranges
    /// and the hull, and set the moved tables' dirty flags, moving no word.
    /// Unlike `set_gdt_snoop_range`, this is not a reload: a pending dirty
    /// flag survives, nothing bumps, and the hull change needs no flush or
    /// policy bump of its own (step 3 already flushed every translation; the
    /// epoch set moves in step 6). Returns whether a table was marked, for the
    /// commit's single descriptor-generation bump.
    fn apply_descriptor_reprojection(&mut self, r: &DescriptorReprojection) -> bool {
        if let Some((start, end)) = r.gdt_range {
            self.gdt_snoop_base = start;
            self.gdt_snoop_end = end;
        }
        if let Some((start, end)) = r.ldt_range {
            self.ldt_snoop_base = start;
            self.ldt_snoop_end = end;
        }
        (self.snoop_min, self.snoop_max) = self.snoop_ranges_hull();
        if r.gdt_moved {
            self.gdt_dirty.set(true);
        }
        if r.ldt_moved {
            self.ldt_dirty.set(true);
        }
        r.gdt_moved || r.ldt_moved
    }

    /// Clear the hashed TLB and null its flat views, returning the revoked
    /// linear pages. Bumps no observable epoch.
    fn revoke_translations(&mut self) -> Vec<u32> {
        let revoked: Vec<u32> = (0..TLB_SIZE)
            .map(|i| self.tlb.entries[i].linear_page)
            .filter(|&tag| tag != TLB_INVALID_TAG)
            .collect();
        self.tlb_stats.record_flush_full();
        self.tlb.flush();
        revoked
    }

    /// Sever stage for revoked linear pages. Currently a no-op; publication
    /// runs at engine depth zero.
    fn deliver_severs(&self, _revoked: &[u32]) {}

    /// Mark `table` dirty and bump the descriptor generation: what `snoop_descriptor_write` does on
    /// a hit, with no
    /// range test. For a machine whose descriptor watch is not the core's
    /// linear range (the machine canonical spans). Policy-free and cold; the PC
    /// never calls it.
    #[cold]
    pub fn mark_descriptor_dirty(&self, table: DescriptorTable) {
        match table {
            DescriptorTable::Gdt => self.gdt_dirty.set(true),
            DescriptorTable::Ldt => self.ldt_dirty.set(true),
        }
        self.bump_descriptor_generation();
    }

    /// True when `[start, end)` overlaps a programmed descriptor snoop range,
    /// setting that table's dirty flag only (§13.2: the commit bumps the
    /// descriptor generation once, afterwards).
    fn set_descriptor_dirty_flags_in(&self, start: usize, end: usize) -> bool {
        let mut marked = false;
        for (base, table_end, flag) in [
            (self.gdt_snoop_base, self.gdt_snoop_end, &self.gdt_dirty),
            (self.ldt_snoop_base, self.ldt_snoop_end, &self.ldt_dirty),
        ] {
            if table_end > base && start < table_end && base < end {
                flag.set(true);
                marked = true;
            }
        }
        marked
    }
}

impl Publication<'_> {
    /// The kinds this publication covers.
    #[inline]
    #[must_use]
    pub fn kinds(&self) -> KindSet {
        self.kinds
    }

    /// Commit (steps 3-9) and return the receipt.
    ///
    /// # Panics
    /// If `plan.affected_pages` is not strictly ascending. The publication is
    /// then uncommitted, so the core is poisoned.
    pub fn commit(self, plan: &PublicationPlan<'_>) -> PublicationReceipt {
        self.commit_inner(plan, &mut no_reprojection, &mut |_, _| {})
    }

    /// [`commit`](Self::commit), calling `probe` after each stage.
    #[doc(hidden)]
    pub fn commit_probed(
        self,
        plan: &PublicationPlan<'_>,
        probe: &mut dyn FnMut(PublicationStage, &MemoryCore),
    ) -> PublicationReceipt {
        self.commit_inner(plan, &mut no_reprojection, probe)
    }

    /// [`commit`](Self::commit) with the machine's descriptor reprojection
    /// inside the transaction.
    ///
    /// `reproject` runs once, after the revoke (every translation, flat view
    /// and direct proof is already gone) and before the sever stage, with
    /// the newly installed map. It may read guest RAM and record
    /// paging-structure frames, and must move no observable word: no
    /// generation, epoch or snoop-range setter. What it returns is applied
    /// in step 5, under the commit's single descriptor-generation bump. A
    /// panic inside it poisons the core like any other step.
    ///
    /// # Panics
    /// As [`commit`](Self::commit).
    pub fn commit_reprojecting(
        self,
        plan: &PublicationPlan<'_>,
        reproject: &mut dyn FnMut(&mut MemoryCore) -> DescriptorReprojection,
    ) -> PublicationReceipt {
        self.commit_inner(plan, reproject, &mut |_, _| {})
    }

    /// [`commit_reprojecting`](Self::commit_reprojecting), calling `probe`
    /// after each stage.
    #[doc(hidden)]
    pub fn commit_reprojecting_probed(
        self,
        plan: &PublicationPlan<'_>,
        reproject: &mut dyn FnMut(&mut MemoryCore) -> DescriptorReprojection,
        probe: &mut dyn FnMut(PublicationStage, &MemoryCore),
    ) -> PublicationReceipt {
        self.commit_inner(plan, reproject, probe)
    }

    /// Steps 3-9 (see the module docs).
    fn commit_inner(
        mut self,
        plan: &PublicationPlan<'_>,
        reproject: &mut dyn FnMut(&mut MemoryCore) -> DescriptorReprojection,
        probe: &mut dyn FnMut(PublicationStage, &MemoryCore),
    ) -> PublicationReceipt {
        assert!(
            plan.affected_pages.windows(2).all(|w| w[0] < w[1]),
            "PublicationPlan::affected_pages must ascend without duplicates"
        );
        let kinds = self.kinds;
        let global_count_before = self.global_count_before;
        let core = &mut *self.core;

        // 3. Revoke, and collect the pending bumps without applying any.
        let revoked = core.revoke_translations();
        core.set_direct_proofs_live(false);
        let mut page_words: Vec<u32> = core.guarded_pages.borrow().iter().copied().collect();
        let guarded = page_words.len() as u64;
        page_words.extend_from_slice(plan.affected_pages);
        // Guarded ∪ affected, each page once. No sort: a `<[u32]>::sort_unstable`
        // monomorphization made here is shared with the engines, which sort
        // `u32`s too, and changes PC codegen.
        page_words.retain(|page| plan.affected_pages.binary_search(page).is_err());
        page_words.extend_from_slice(plan.affected_pages);
        // The machine's reprojection over the installed map, with no bump.
        let reprojection = reproject(core);
        probe(PublicationStage::Revoked, core);

        // 4. Sever (deferred delivery at depth 0).
        core.deliver_severs(&revoked);
        probe(PublicationStage::Severed, core);

        // 5. The pending bumps: per-page words, then each global word once.
        let page_words_bumped = core.bump_page_generation_words(&page_words);
        let mut descriptor_tables_marked = core.apply_descriptor_reprojection(&reprojection);
        // The X5 stand-in for the projection: a changed range over a snooped
        // range marks that table. Kept with the reprojection (LLD §14.3 as
        // built); it can only over-mark. X7d/X7f decide whether to drop it.
        for &(start, end) in plan.changed_ranges {
            descriptor_tables_marked |= core.set_descriptor_dirty_flags_in(start, end);
        }
        core.direct_generations.bump_proof_word();
        core.direct_generation_sweeps
            .set(core.direct_generation_sweeps.get().wrapping_add(1));
        core.direct_swept_pages_total
            .set(core.direct_swept_pages_total.get().wrapping_add(guarded));
        core.direct_generations.bump_source_word();
        core.direct_page_source_bumps.set(
            core.direct_page_source_bumps
                .get()
                .wrapping_add(plan.affected_pages.len() as u64),
        );
        if descriptor_tables_marked {
            core.bump_descriptor_generation();
        }
        core.bump_pt_write_epoch();
        probe(PublicationStage::PendingBumpsApplied, core);

        // 6. The epoch set, once.
        core.apply_epoch_set(kinds);
        probe(PublicationStage::EpochSetApplied, core);

        // 7. The global flush epoch, from the pre-flush snapshot.
        let global_flush_epoch_bumped = global_count_before != 0;
        if global_flush_epoch_bumped {
            core.dynrec_global_tlb_flush_epoch = core.dynrec_global_tlb_flush_epoch.wrapping_add(1);
        }
        probe(PublicationStage::GlobalFlushEpoch, core);

        // 8. The decode cache, then the SMC notifier.
        core.clear_instr_exec_pages();
        core.set_instr_cache_dirty();
        probe(PublicationStage::DecodeCacheCleared, core);
        if let Some(notifier) = core.smc_notifier.clone() {
            for &(start, end) in plan.changed_ranges {
                notifier.notify_write_range(start, end);
            }
        }
        probe(PublicationStage::SmcNotified, core);

        // 9. The receipt.
        self.committed = true;
        PublicationReceipt {
            kinds,
            revoked_linear_pages: revoked.len(),
            page_words_bumped,
            descriptor_tables_marked,
            global_flush_epoch_bumped,
        }
    }
}

/// The reprojection of a machine without a descriptor projection.
fn no_reprojection(_: &mut MemoryCore) -> DescriptorReprojection {
    DescriptorReprojection::default()
}

impl Drop for Publication<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.core
                .direct_generations
                .engine_gate
                .poisoned
                .store(true, Ordering::Relaxed);
        }
    }
}
