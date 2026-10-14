//! The hashed software TLB, its flat projection owner, and TLB statistics.
//!
//! Moved from `gp64-memory` in M1 (HLD 2026.10.02 R6). The table is the
//! sole translation authority; publication takes a machine-proved host page
//! (see [`crate::MemoryCore::tlb_fill`]) and never tests a machine's address
//! map itself (HLD R2).

use std::cell::Cell;

use crate::flat_tlb;

/// TLB size (must be power of 2 for efficient indexing)
pub const TLB_SIZE: usize = 4096;

/// Right shift in the software-TLB index hash
/// `(page ^ (page >> TLB_HASH_SHIFT)) & (TLB_SIZE - 1)`. [`SoftwareTlb::index`]
/// and the DBX emitted probe (`gp64_dyncore` `emit_tlb_slot_probe`) both
/// read this one constant, so the Rust and emitted slot choices cannot drift.
pub const TLB_HASH_SHIFT: u8 = 8;

/// TLB flag: page is writable (PDE.W & PTE.W)
pub const TLB_FLAG_WRITABLE: u8 = 0x01;
/// TLB flag: page is user-accessible (PDE.U & PTE.U)
pub const TLB_FLAG_USER: u8 = 0x02;
/// TLB flag: page dirty bit set in PTE
pub const TLB_FLAG_DIRTY: u8 = 0x04;
/// TLB flag: global page (the G bit was set with CR4.PGE enabled).
pub const TLB_FLAG_GLOBAL: u8 = 0x08;
/// TLB flag: global source is a 4 MiB PDE rather than a 4 KiB PTE.
pub(crate) const TLB_FLAG_GLOBAL_LARGE: u8 = 0x10;

/// Invalid sentinel for `linear_page` — indicates an unused TLB slot.
pub const TLB_INVALID_TAG: u32 = 0xFFFF_FFFF;

/// Software-TLB slot for `linear_page`: `(p ^ (p >> TLB_HASH_SHIFT)) & (TLB_SIZE - 1)`.
///
/// The one definition of the hash (LLD 2.1). The DBX emitted probe reads
/// [`TLB_HASH_SHIFT`]; the interpreter's raw probes adopt this in M2.
#[inline(always)]
#[must_use]
pub const fn tlb_index(linear_page: u32) -> usize {
    ((linear_page ^ (linear_page >> TLB_HASH_SHIFT)) as usize) & (TLB_SIZE - 1)
}

/// Software TLB entry for paging translation cache.
///
/// Stores precomputed host pointers so that the fast path can dereference
/// memory without composing `ram_base + (physical_page << 12) + offset`
/// on every access.  VGA pages and permission mismatches leave the
/// pointer fields null, forcing the slow path.
#[derive(Clone, Copy)]
#[repr(C, align(32))]
pub struct TlbEntry {
    /// Host pointer for reads (null = miss or VGA range).
    pub host_read: *const u8,
    /// Host pointer for writes (null = miss, read-only, not-dirty, or VGA range).
    pub host_write: *mut u8,
    /// Linear page tag (`TLB_INVALID_TAG` = empty slot).
    pub linear_page: u32,
    /// Physical page number — retained for slow-path compatibility.
    pub physical_page: u32,
    /// Packed flags: bits 0=writable, 1=user, 2=dirty, 3=global, 4=global large source.
    pub flags: u8,
    /// Bytes 0..4 retain the global backing PTE/PDE physical address;
    /// byte 4 retains its raw R/W and U/S bits. Layout stays fixed.
    pub pad: [u8; 7],
}

impl Default for TlbEntry {
    fn default() -> Self {
        Self {
            host_read: std::ptr::null(),
            host_write: std::ptr::null_mut(),
            linear_page: TLB_INVALID_TAG,
            physical_page: 0,
            flags: 0,
            pad: [0; 7],
        }
    }
}

/// Software TLB for caching page translations.
///
/// The hashed `entries` array is the **sole translation authority**. `flat` is a
/// derived projection of exactly the decisions `entries` records, kept coherent
/// by the same three mutators (`insert` / `invalidate_page` / `flush`) plus
/// `set_page_watched`. Holding the projection *inside* this struct is
/// deliberate: it makes bypass structurally impossible, because nothing outside
/// these methods can reach a view.
pub struct SoftwareTlb {
    pub entries: Box<[TlbEntry; TLB_SIZE]>,
    flat: flat_tlb::FlatTlbViews,
    /// Number of live global authority entries, maintained at every mutation.
    pub(crate) global_count: usize,
    /// **Oracle O3 (forced-miss sweep), instrumented builds only.** When set, the
    /// authoritative hashed table fills as usual but NOTHING is published into the
    /// flat projection — so every emitted probe reads a null view and every native
    /// memory operation takes its slow exit.
    ///
    /// The interpreter is unaffected (it reads the hashed table), so the guest runs
    /// exactly the same program by an entirely different route: every memory access
    /// leaves native code through the MEM-RESTART stub, is re-run by the
    /// interpreter, and re-enters. Final `Cpu` state must be bit-identical to the
    /// un-suppressed run. That is the ONLY oracle for MEM-RESTART clause 1
    /// (commit-last atomicity): a form whose slow branch sits AFTER its commit
    /// point double-commits, which the poisoning oracle O2 is structurally blind to
    /// (a post-op spill overwrites the poison with plausible-but-wrong values).
    #[cfg(any(test, debug_assertions, feature = "test-instrumentation"))]
    suppress_flat: bool,
    /// One bit per 4 MiB linear region (1024 regions): set when a 4 MiB PSE
    /// mapping filled one of the region's 4 KiB slots. A large page is cached
    /// as up to 1024 ordinary entries, and INVLPG of any address in it must
    /// drop all of them (MDD-BUG-CRU-01472). A set bit is conservative: it may
    /// outlive the slices, which costs only extra invalidation.
    large_regions: [u64; 16],
}

// SAFETY: The raw pointers in TlbEntry point into Memory::ram which is
// owned by the same thread.  The emulator is single-threaded; the TLB
// is never shared across threads.
unsafe impl Send for SoftwareTlb {}

impl SoftwareTlb {
    /// An empty TLB whose watch and PT-frame bitmaps cover `watch_page_count`
    /// backing pages (PC: `MAX_RAM_SIZE >> 12`).
    #[must_use]
    pub fn new(watch_page_count: usize) -> Self {
        let entries = vec![TlbEntry::default(); TLB_SIZE].into_boxed_slice();
        let Ok(entries) = entries.try_into() else {
            unreachable!("TLB allocation length is the TLB_SIZE constant")
        };
        Self {
            entries,
            flat: flat_tlb::FlatTlbViews::new(watch_page_count),
            global_count: 0,
            #[cfg(any(test, debug_assertions, feature = "test-instrumentation"))]
            suppress_flat: false,
            large_regions: [0; 16],
        }
    }
}

impl SoftwareTlb {
    /// Records that `linear_page` was filled from a 4 MiB PSE mapping.
    #[inline]
    pub fn mark_large_page(&mut self, linear_page: u32) {
        let region = (linear_page >> 10) as usize & 0x3FF;
        self.large_regions[region / 64] |= 1 << (region % 64);
    }

    /// INVLPG: drops `linear_page`, or every slot of its 4 MiB region when a
    /// large mapping filled that region, since any of them may be a slice of
    /// the page being invalidated (MDD-BUG-CRU-01472).
    #[inline]
    pub fn invalidate_linear(&mut self, linear_page: u32) {
        let region = (linear_page >> 10) as usize & 0x3FF;
        let bit = 1u64 << (region % 64);
        if self.large_regions[region / 64] & bit == 0 {
            self.invalidate_page(linear_page);
            return;
        }
        self.large_regions[region / 64] &= !bit;
        let base = (region as u32) << 10;
        for page in base..base + 1024 {
            self.invalidate_page(page);
        }
    }

    #[inline]
    pub fn index(linear_page: u32) -> usize {
        tlb_index(linear_page)
    }

    /// Look up a linear page in the TLB.
    /// Returns `Some((physical_page_base, writable, user, dirty))` if found, None otherwise.
    #[inline]
    pub(crate) fn lookup(&self, linear_page: u32) -> Option<(u32, bool, bool, bool)> {
        let index = Self::index(linear_page);
        let entry = &self.entries[index];
        if entry.linear_page == linear_page {
            let f = entry.flags;
            Some((
                entry.physical_page,
                f & TLB_FLAG_WRITABLE != 0,
                f & TLB_FLAG_USER != 0,
                f & TLB_FLAG_DIRTY != 0,
            ))
        } else {
            None
        }
    }

    /// Insert a translation into the TLB.
    ///
    /// `host_page` is the machine-proved host address of the backing page, or
    /// null when the page must take the slow path for every access (device,
    /// ROM, out-of-backing, ...). The table no longer decides that itself: the
    /// VGA window and RAM-bounds tests that lived here moved to the machine's
    /// proof (HLD R2), which already implied both.
    ///
    /// # Safety
    /// A non-null `host_page` must address 4 KiB of committed backing that the
    /// machine has proved side-effect free for reads (and for writes when
    /// `host_write_allowed`), and must stay valid until the next flush that
    /// precedes a backing move (LLD 1.4 V1-V5).
    #[inline]
    #[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
    pub(crate) unsafe fn insert(
        &mut self,
        linear_page: u32,
        physical_page: u32,
        writable: bool,
        user: bool,
        dirty: bool,
        host_page: *mut u8,
        host_read_allowed: bool,
        host_write_allowed: bool,
    ) {
        let flags = if writable { TLB_FLAG_WRITABLE } else { 0 }
            | if user { TLB_FLAG_USER } else { 0 }
            | if dirty { TLB_FLAG_DIRTY } else { 0 };

        let (host_read, host_write) = if host_page.is_null() {
            (std::ptr::null(), std::ptr::null_mut())
        } else {
            let read_ptr = if host_read_allowed {
                host_page.cast_const()
            } else {
                std::ptr::null()
            };
            let write_ptr = if writable && dirty && host_write_allowed {
                host_page
            } else {
                std::ptr::null_mut()
            };
            (read_ptr, write_ptr)
        };

        let index = Self::index(linear_page);

        // Projection: eviction is MIRRORED. A hashed insert that displaces a
        // different page must null that page's four view slots BEFORE
        // publishing this one — otherwise the displaced page keeps a live
        // biased pointer with no authoritative entry behind it, which is
        // exactly the "stale survivor" class O-P1(b) exists to catch.
        // Mirroring also keeps probe-hit ⟺ interpreter-fast-path-hit, so the
        // two engines observe identical TLB staleness windows.
        let displaced = self.entries[index].linear_page;
        if displaced != TLB_INVALID_TAG && self.entries[index].flags & TLB_FLAG_GLOBAL != 0 {
            self.global_count -= 1;
        }
        if displaced != TLB_INVALID_TAG && displaced != linear_page {
            self.flat.null_page(displaced);
        }

        // Subset-argument clause 2, made self-checking: a nonzero `host_write`
        // implies the page lies inside the machine's backing, which the
        // machine sizes no larger than the SMC watch bitmap (PC: `ram_len <=
        // MAX_RAM_SIZE`), hence the physical page is inside the bitmap's range. That is what makes the
        // emitted `phys < SMC_CODE_PAGE_COUNT` bound check vacuous on the hit
        // path and therefore deletable. `ram_len <= MAX_RAM_SIZE` is ENFORCED
        // by the assert in `resize_ram` (the single growth funnel), not merely
        // assumed of the flat-memory layout — MDD-BUG-FLUXHOMEARPA-00022 was exactly
        // this premise silently failing when expand_ram grew RAM past a
        // bound the bitmap didn't cover.
        debug_assert!(
            host_write.is_null() || (physical_page as usize) < self.flat.watch_page_count(),
            "writable page {physical_page:#x} lies outside the SMC watch bitmap range"
        );

        // O3: with the forced-miss sweep armed, the authoritative fill below still
        // happens (the interpreter needs it) but the projection stays null, so every
        // emitted probe misses.
        if !self.flat_projection_suppressed() {
            // Write-view nullity folds BOTH divert predicates: SMC-watched
            // code pages and recorded paging-structure frames (EXP-N005).
            // Either one must force every store onto the slow path where
            // `smc_before_write`'s combined probe lives.
            let watched =
                self.flat.is_watched(physical_page) || self.flat.is_pt_frame(physical_page);
            self.flat
                .publish(linear_page, host_read, host_write, user, watched);
        }

        self.entries[index] = TlbEntry {
            host_read,
            host_write,
            linear_page,
            physical_page,
            flags,
            pad: [0; 7],
        };
    }

    /// Invalidate all TLB entries
    pub(crate) fn flush(&mut self) {
        self.global_count = 0;
        for i in 0..TLB_SIZE {
            // The hashed table is its own "links list": nulling only the live
            // tags bounds a flush at 4096 pages, so the 32 MiB projection is
            // NEVER memset.
            let tag = self.entries[i].linear_page;
            if tag != TLB_INVALID_TAG {
                self.flat.null_page(tag);
            }
            let entry = &mut self.entries[i];
            entry.linear_page = TLB_INVALID_TAG;
            entry.host_read = std::ptr::null();
            entry.host_write = std::ptr::null_mut();
        }
        self.large_regions = [0; 16];
    }

    /// CR3 invalidation: preserve global authority entries and their four
    /// flat projections. Large-region markers may conservatively outlive the
    /// nonglobal slices, so INVLPG still removes every surviving global slice.
    #[inline]
    pub(crate) fn flush_non_global(&mut self) {
        for index in 0..TLB_SIZE {
            let entry = self.entries[index];
            if entry.linear_page != TLB_INVALID_TAG && entry.flags & TLB_FLAG_GLOBAL == 0 {
                self.invalidate_page(entry.linear_page);
            }
        }
    }

    /// Is the O3 forced-miss sweep armed?
    ///
    /// The single place the mode is read, so EVERY projection publisher honours
    /// it the same way (MDD-BUG-CRUCIBLE-00416). It used to be an inline
    /// `cfg`-split local in the fill path only, which is exactly how the
    /// watch-transition publisher came to ignore it.
    ///
    /// Always `false` where the mode cannot be armed: `suppress_flat` is an
    /// instrumented-build field, so release has nothing to read.
    #[inline]
    pub(crate) fn flat_projection_suppressed(&self) -> bool {
        #[cfg(any(test, debug_assertions, feature = "test-instrumentation"))]
        {
            self.suppress_flat
        }
        #[cfg(not(any(test, debug_assertions, feature = "test-instrumentation")))]
        {
            false
        }
    }

    /// Oracle O3: arm/disarm flat-projection suppression.
    ///
    /// Arming nulls every currently-published page (by walking the live hashed
    /// tags, the same bounded walk `flush` uses — never a 32 MiB memset) so the
    /// sweep takes effect immediately rather than only for future fills. That
    /// leaves the two halves of the TLB deliberately incoherent: the flat views
    /// are null while the authoritative hashed tags still hit.
    ///
    /// MDD-BUG-CRUCIBLE-00417: disarming therefore has to put them BACK in
    /// agreement, and clearing the flag alone did not. This comment used to
    /// claim no republish was needed because "the projection refills as the
    /// guest faults pages back in" — it cannot. Those hashed tags are still
    /// valid, so `tlb_lookup` keeps hitting, the translators return without
    /// walking the page tables, and `tlb_insert` is never reached. The flat
    /// slots stay null indefinitely: dyncore keeps taking slow exits long after
    /// suppression is nominally off, and if suppression was armed before the
    /// first fill the projection-coherence oracle can even certify the
    /// incoherent state, because `nonzero_publishes` never left zero.
    ///
    /// `flush` is exactly the missing half — it drops the hashed tags as well,
    /// so the next access re-walks and republishes both sides together. Bounded
    /// at TLB_SIZE, and this whole API is instrumented-builds-only.
    #[cfg(any(test, debug_assertions, feature = "test-instrumentation"))]
    pub(crate) fn set_flat_suppressed(&mut self, on: bool) {
        self.suppress_flat = on;
        if on {
            for i in 0..TLB_SIZE {
                let tag = self.entries[i].linear_page;
                if tag != TLB_INVALID_TAG {
                    self.flat.null_page(tag);
                }
            }
        } else {
            self.flush();
        }
    }

    /// Invalidate a specific page (for INVLPG instruction)
    pub(crate) fn invalidate_page(&mut self, linear_page: u32) {
        // Projection nulling is UNCONDITIONAL, unlike the hashed tag clear.
        // It is a no-op when the two are coherent (so it is self-healing), and
        // it is the only correct action when `linear_page` was previously
        // collision-evicted — the case where the hashed clear has nothing to do
        // but a stale projection entry could still be live.
        self.flat.null_page(linear_page);

        let index = Self::index(linear_page);
        if self.entries[index].linear_page == linear_page {
            if self.entries[index].flags & TLB_FLAG_GLOBAL != 0 {
                self.global_count -= 1;
            }
            self.entries[index].linear_page = TLB_INVALID_TAG;
            self.entries[index].host_read = std::ptr::null();
            self.entries[index].host_write = std::ptr::null_mut();
        }
    }

    /// Mark or unmark a physical page as holding dyncore-compiled code.
    ///
    /// This folds the SMC fast-path predicate into write-view nullity: a watched
    /// page publishes `0` in both write views, so the emitted store probe's
    /// single `test`/`jz` subsumes the per-store code-page map lookup. The SMC
    /// *decision authority* (the `smc_store` trampoline and the interpreter) is
    /// unchanged — only the fast-path predicate moved from probe time to fill
    /// time.
    ///
    /// A transition sweeps the 4096 hashed slots and republishes the write views
    /// of every entry mapping this physical page. Read views are untouched:
    /// watching a page never restricts reads.
    pub fn set_page_watched(&mut self, physical_page: u32, on: bool) {
        if !self.flat.set_watched(physical_page, on) {
            // No bit changed, so no projection entry can be affected.
            return;
        }
        // Un-watching must not republish a write view that something else still
        // requires null: the nullity decision is the OR of every predicate,
        // exactly as at fill time.
        //
        // - the PT-frame fold (EXP-N005);
        // - the O3 forced-miss sweep (MDD-BUG-CRUCIBLE-00416). Un-watching is
        //   ordinary dyncore cache lifecycle — eviction queues it and dispatch
        //   reconciliation performs it — so without this an unwatch during a
        //   suppressed run republished nonzero W0/W3 and let a native store hit
        //   inside a run whose whole premise is that every memory operation
        //   misses. Watch-bit bookkeeping and the slots-scanned counter below
        //   still happen; only the nonzero republish is withheld.
        let nulled =
            on || self.flat.is_pt_frame(physical_page) || self.flat_projection_suppressed();
        for i in 0..TLB_SIZE {
            let entry = self.entries[i];
            if entry.linear_page == TLB_INVALID_TAG || entry.physical_page != physical_page {
                continue;
            }
            let user = entry.flags & TLB_FLAG_USER != 0;
            self.flat
                .publish_write_only(entry.linear_page, entry.host_write, user, nulled);
        }
        self.flat.add_slots_scanned(TLB_SIZE as u64);
    }

    /// Record a physical frame as a paging-structure page (EXP-N005).
    ///
    /// A first record sweeps the hashed slots and nulls the write views of
    /// every live entry mapping this frame, exactly like
    /// `set_page_watched(_, true)` — that divert is what routes warm-TLB and
    /// emitted stores into `Memory::smc_before_write`, where the PT-write
    /// tripwire fires. Returns `true` on the first record of this frame.
    pub fn record_pt_frame(&mut self, physical_page: u32) -> bool {
        if !self.flat.set_pt_frame(physical_page) {
            return false;
        }
        for i in 0..TLB_SIZE {
            let entry = self.entries[i];
            if entry.linear_page == TLB_INVALID_TAG || entry.physical_page != physical_page {
                continue;
            }
            let user = entry.flags & TLB_FLAG_USER != 0;
            self.flat
                .publish_write_only(entry.linear_page, entry.host_write, user, true);
        }
        self.flat.add_slots_scanned(TLB_SIZE as u64);
        true
    }

    /// Persistently revoke the write view of every live entry mapping
    /// `physical_page`. Returns the
    /// number of entries revoked.
    ///
    /// For a machine-owned protection (descriptor pages, walk frames,
    /// conservative mode). Each matching hashed entry's stored `host_write`
    /// is cleared first, then its flat write views are published null.
    /// Clearing the stored pointer is what keeps the prohibition through a
    /// later `set_page_watched(_, false)`, which republishes `host_write`.
    /// Read views are untouched. Nothing re-grants here: the next fill does,
    /// through the machine's proof. Cold; the PC never calls it.
    #[cold]
    pub fn revoke_host_write(&mut self, physical_page: u32) -> usize {
        let mut revoked = 0;
        for i in 0..TLB_SIZE {
            let entry = &mut self.entries[i];
            if entry.linear_page == TLB_INVALID_TAG || entry.physical_page != physical_page {
                continue;
            }
            entry.host_write = std::ptr::null_mut();
            let (linear_page, user) = (entry.linear_page, entry.flags & TLB_FLAG_USER != 0);
            self.flat
                .publish_write_only(linear_page, std::ptr::null_mut(), user, true);
            revoked += 1;
        }
        self.flat.add_slots_scanned(TLB_SIZE as u64);
        revoked
    }

    /// Read-only access to the projection, for publication and oracles.
    #[inline]
    pub fn flat(&self) -> &flat_tlb::FlatTlbViews {
        &self.flat
    }

    /// Grow the projection's physical watch coverage to `count` pages.
    pub(crate) fn ensure_watch_pages(&mut self, count: usize) {
        self.flat.ensure_watch_pages(count);
    }

    /// **O-P1(a) + O-P4** — for every live hashed tag, recompute the four view
    /// values from the authoritative entry and assert bitwise equality with the
    /// projection, then assert each nonzero bias lands inside the current `ram`
    /// allocation.
    ///
    /// Fails if: a fill mis-biases, a fill forgets a view, the watch fold is
    /// wrong, or `ram` moved without a flush (the dangling-pointer class the raw
    /// host pointers already carry — now detectable).
    ///
    /// Cost is O(4096); safe to call after every guest instruction in a
    /// differential harness.
    pub fn oracle_check_live_tags(
        &self,
        ram_base: *const u8,
        ram_len: usize,
    ) -> std::result::Result<(), String> {
        // Oracle O3 (forced-miss sweep) DELIBERATELY empties the projection while
        // the hashed table keeps filling, so O-P1(a)'s contract — "the projection
        // holds exactly the live translation set" — does not apply. This is a
        // precondition, not a hole: under suppression the oracle asserts the
        // STRONGER, mode-specific property that every live tag projects to NULL in
        // all four views, so a suppression that silently stopped working (leaving
        // the sweep testing nothing) fails here rather than passing quietly.
        // O-P4, hoisted above every early return (MDD-BUG-CRUCIBLE-00418).
        //
        // This used to live inside the projection loop and fire only when a view
        // projected nonzero, which made it a check on the PROJECTION pointer. The
        // interpreter fast path does not read the projection - it dereferences
        // `TlbEntry::host_read` / `host_write` directly. So the bound belonged on
        // the hashed pointers all along, and putting it there also makes it total:
        //
        //   - under O3 the projection is all zeros by construction, so the old
        //     placement could never fire, and the suppressed branch returned early
        //     before reaching it anyway. O-P4 was therefore absent from precisely
        //     the mode that shifts every native miss onto the interpreter and so
        //     leans hardest on these pointers;
        //   - with no published nonzero view the `has_published_nonzero` shortcut
        //     returned early for the same reason;
        //   - a watched or PT-frame page nulls its write view deliberately, so
        //     even on the normal path a live `host_write` went unchecked.
        //
        // A RAM reallocation without a flush is what O-P4 exists to catch, and it
        // corrupts the hashed pointers in all three of those situations.
        for (i, entry) in self.entries.iter().enumerate() {
            if entry.linear_page == TLB_INVALID_TAG {
                continue;
            }
            let lo = ram_base as u64;
            let hi = lo + ram_len as u64;
            for (which, ptr) in [
                ("host_read", entry.host_read),
                ("host_write", entry.host_write.cast_const()),
            ] {
                if ptr.is_null() {
                    continue;
                }
                let host = ptr as u64;
                if host < lo || host >= hi {
                    return Err(format!(
                        "O-P4: slot {i} page {:#x} {which} {host:#x} outside ram [{lo:#x},{hi:#x})",
                        entry.linear_page
                    ));
                }
            }
        }

        #[cfg(any(test, debug_assertions, feature = "test-instrumentation"))]
        if self.suppress_flat {
            for entry in self.entries.iter() {
                if entry.linear_page == TLB_INVALID_TAG {
                    continue;
                }
                for view in [
                    flat_tlb::FlatView::Read0,
                    flat_tlb::FlatView::Write0,
                    flat_tlb::FlatView::Read3,
                    flat_tlb::FlatView::Write3,
                ] {
                    let got = self.flat.entry(view, entry.linear_page);
                    if got != 0 {
                        return Err(format!(
                            "O3 forced-miss armed but page {:#x} view {view:?} still \
                             projects {got:#x} — the sweep is not forcing misses",
                            entry.linear_page
                        ));
                    }
                }
            }
            return Ok(());
        }
        if !self.flat.has_published_nonzero() {
            // Provably vacuous, by the same argument as O-P1(b): a live tag with
            // a non-null host pointer WOULD have published a nonzero value and
            // bumped this counter. So with the counter at zero every live tag
            // expects 0 in all four views, and no view has ever been written.
            // This is what makes the per-instruction hook affordable — most
            // differential scenarios run unpaged (PG=0) and never fill a TLB
            // entry at all.
            return Ok(());
        }
        for (i, entry) in self.entries.iter().enumerate() {
            if entry.linear_page == TLB_INVALID_TAG {
                continue;
            }
            let p = entry.linear_page;
            let user = entry.flags & TLB_FLAG_USER != 0;
            // Mirror the fill-time fold: PT frames null write views exactly
            // like watched pages (EXP-N005).
            let watched = self.flat.is_watched(entry.physical_page)
                || self.flat.is_pt_frame(entry.physical_page);

            let want_r = if entry.host_read.is_null() {
                0
            } else {
                flat_tlb::bias(entry.host_read, p)
            };
            let want_w = if entry.host_write.is_null() || watched {
                0
            } else {
                flat_tlb::bias(entry.host_write.cast_const(), p)
            };

            for (view, want) in [
                (flat_tlb::FlatView::Read0, want_r),
                (flat_tlb::FlatView::Write0, want_w),
                (flat_tlb::FlatView::Read3, if user { want_r } else { 0 }),
                (flat_tlb::FlatView::Write3, if user { want_w } else { 0 }),
            ] {
                let got = self.flat.entry(view, p);
                if got != want {
                    return Err(format!(
                        "O-P1(a): slot {i} page {p:#x} view {view:?} projection={got:#x} \
                         expected={want:#x} (user={user} watched={watched} \
                         host_read={:?} host_write={:?})",
                        entry.host_read, entry.host_write
                    ));
                }
            }
        }
        Ok(())
    }

    /// **O-SMC-ANYINSTANT** — the one-directional SMC-fold invariant that must
    /// hold at *every* instant, not merely at dispatch entry like O-P1(c).
    ///
    /// `watched(phys) ⇒ every live hashed entry mapping phys has W0 == W3 == 0`.
    ///
    /// This is the direction that kills: a watched page whose write views are
    /// still permissive is a store that could commit inline over live compiled
    /// code. It holds continuously (a watch-set sweeps the write views to 0
    /// synchronously; a fill onto a watched page publishes 0), so it is safe to
    /// assert after any single operation. The converse (unwatched ⇒ nonzero) is
    /// deliberately NOT asserted — a page can be legitimately unwatched with null
    /// write views for unrelated reasons (read-only, clean, MMIO).
    ///
    /// Adopted from the D-B2 second-opinion consults (build journal D-B8): the
    /// direction O-P1(c)'s dispatch-entry equality cannot police mid-dispatch.
    /// Cost is O(4096); safe after any single mutation.
    pub fn oracle_check_smc_fold_any_instant(&self) -> std::result::Result<(), String> {
        for entry in self.entries.iter() {
            if entry.linear_page == TLB_INVALID_TAG {
                continue;
            }
            // Same one-directional invariant for both divert predicates:
            // watched (SMC) and recorded PT frame (EXP-N005) each force null
            // write views at every instant.
            if !self.flat.is_watched(entry.physical_page)
                && !self.flat.is_pt_frame(entry.physical_page)
            {
                continue;
            }
            let p = entry.linear_page;
            for view in [flat_tlb::FlatView::Write0, flat_tlb::FlatView::Write3] {
                let got = self.flat.entry(view, p);
                if got != 0 {
                    return Err(format!(
                        "O-SMC-ANYINSTANT: page {p:#x} (phys {:#x}) is watched or a recorded \
                         PT frame but view {view:?} is permissive ({got:#x}) — a store could \
                         commit inline past the divert",
                        entry.physical_page
                    ));
                }
            }
        }
        Ok(())
    }

    /// **O-P1(b)** — full 4 Mi-slot scan asserting every nonzero projection slot
    /// is claimed by a live hashed tag.
    ///
    /// This is the oracle that catches a **stale survivor**: the failure mode
    /// eviction mirroring and unconditional `invalidate_page` nulling exist to
    /// prevent. O-P1(a) cannot see it, because a stale survivor has no live tag
    /// to iterate from.
    ///
    /// Cost is O(4 Mi); call at test/scenario boundaries, not per instruction.
    pub fn oracle_check_no_stale_survivors(&self) -> std::result::Result<(), String> {
        if !self.flat.has_published_nonzero() {
            // Provably all-zero: no value has ever been written into a view, so
            // the scan cannot find a survivor. This is a proof, not a sample —
            // the oracle is skipped only where it is vacuous by construction,
            // which is what makes it affordable in the differential harness
            // (most scenarios never enable paging and so never fill the TLB).
            return Ok(());
        }
        let mut live = std::collections::HashSet::with_capacity(TLB_SIZE);
        for entry in self.entries.iter() {
            if entry.linear_page != TLB_INVALID_TAG {
                live.insert(entry.linear_page);
            }
        }
        let mut failure = None;
        self.flat.for_each_nonzero(|view, page, value| {
            if failure.is_none() && !live.contains(&page) {
                failure = Some(format!(
                    "O-P1(b): stale survivor — page {page:#x} view {view:?} holds {value:#x} \
                     but no live hashed tag claims it"
                ));
            }
        });
        match failure {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

// ============================================================================
// TLB Diagnostics
// ============================================================================

/// TLB hit/miss/invalidation counters for diagnosing collision rates.
///
/// The env gate was retired (); stats are permanently off unless a
/// test calls `enable()`. When disabled, the only cost is a single never-taken
/// branch per TLB operation.
///
/// Uses `Cell<u64>` for counters so that recording works through `&self`
/// references (the `PagedMemory::tlb_lookup` trait takes `&self`).
/// This is safe because the emulator is single-threaded.
pub struct TlbStats {
    /// Whether stats collection is enabled (checked once at init).
    enabled: bool,
    /// Number of TLB lookups that found a valid entry.
    hits: Cell<u64>,
    /// Number of TLB lookups that missed (tag mismatch or empty slot).
    misses: Cell<u64>,
    /// Number of full TLB flushes (CR3 write, CR0.PG change, RAM realloc).
    flush_full: Cell<u64>,
    /// Number of single-page invalidations (INVLPG, physical mapping change).
    flush_single: Cell<u64>,
}

impl std::fmt::Debug for TlbStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlbStats")
            .field("enabled", &self.enabled)
            .field("hits", &self.hits.get())
            .field("misses", &self.misses.get())
            .field("flush_full", &self.flush_full.get())
            .field("flush_single", &self.flush_single.get())
            .finish()
    }
}

impl TlbStats {
    /// TLB stats were env-gated (retired); permanently disabled.
    /// (Retained as `from_env` for call-site stability; use `disabled()`.)
    pub fn from_env() -> Self {
        Self {
            enabled: false,
            hits: Cell::new(0),
            misses: Cell::new(0),
            flush_full: Cell::new(0),
            flush_single: Cell::new(0),
        }
    }

    /// Create disabled stats (for tests that don't need them).
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            hits: Cell::new(0),
            misses: Cell::new(0),
            flush_full: Cell::new(0),
            flush_single: Cell::new(0),
        }
    }

    /// Whether stats collection is enabled.
    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Enable stats collection (for testing).
    pub fn enable(&mut self) {
        self.enabled = true;
    }

    /// Record a TLB lookup hit.
    #[inline]
    pub fn record_hit(&self) {
        if self.enabled {
            self.hits.set(self.hits.get() + 1);
        }
    }

    /// Record a TLB lookup miss.
    #[inline]
    pub fn record_miss(&self) {
        if self.enabled {
            self.misses.set(self.misses.get() + 1);
        }
    }

    /// Record a full TLB flush.
    #[inline]
    pub fn record_flush_full(&self) {
        if self.enabled {
            self.flush_full.set(self.flush_full.get() + 1);
        }
    }

    /// Record a single-page invalidation.
    #[inline]
    pub fn record_flush_single(&self) {
        if self.enabled {
            self.flush_single.set(self.flush_single.get() + 1);
        }
    }

    /// Total lookups (hits + misses).
    pub fn total_lookups(&self) -> u64 {
        self.hits.get() + self.misses.get()
    }

    /// Hit rate as a percentage (0.0..100.0), or 0.0 if no lookups.
    pub fn hit_rate(&self) -> f64 {
        let total = self.total_lookups();
        if total == 0 {
            0.0
        } else {
            (self.hits.get() as f64 / total as f64) * 100.0
        }
    }

    /// Miss rate as a percentage (0.0..100.0), or 0.0 if no lookups.
    pub fn miss_rate(&self) -> f64 {
        let total = self.total_lookups();
        if total == 0 {
            0.0
        } else {
            (self.misses.get() as f64 / total as f64) * 100.0
        }
    }

    /// Format a multi-line summary string.
    pub fn summary(&self) -> String {
        if !self.enabled {
            return "TLB stats: disabled".to_string();
        }
        let total = self.total_lookups();
        let hits = self.hits.get();
        let misses = self.misses.get();
        format!(
            "TLB stats:\n\
             \x20 lookups:      {total}\n\
             \x20 hits:         {hits} ({:.2}%)\n\
             \x20 misses:       {misses} ({:.2}%)\n\
             \x20 flush_full:   {}\n\
             \x20 flush_single: {}",
            self.hit_rate(),
            self.miss_rate(),
            self.flush_full.get(),
            self.flush_single.get(),
        )
    }

    /// Raw counter accessors (for testing and IPC).
    #[inline]
    pub fn hit_count(&self) -> u64 {
        self.hits.get()
    }
    #[inline]
    pub fn miss_count(&self) -> u64 {
        self.misses.get()
    }
    #[inline]
    pub fn flush_full_count(&self) -> u64 {
        self.flush_full.get()
    }
    #[inline]
    pub fn flush_single_count(&self) -> u64 {
        self.flush_single.get()
    }
}
