//! Flat, direct-mapped, biased-pointer translation projection.
//!
//! This is a **derived projection** of the authoritative hashed [`SoftwareTlb`],
//! not a second translation authority: it never walks a page table, never
//! decides a permission, and is never read by Rust to answer a translation
//! query. It is a decompressed cache of decisions the authoritative table
//! already made, shaped so that emitted dyncore code can probe a translation in
//! ~9 host instructions instead of ~30.
//!
//! Translation projection layout:
//!
//! * **Flat and direct-mapped** — one `u64` per 20-bit linear page, indexed by
//!   `linear >> 12`. No hash, therefore no tag, therefore no tag compare.
//! * **Permission folded into nullity** — `0` means "take the slow path" for
//!   every reason at once: miss, no permission, MMIO/VGA, ROM-protected,
//!   not-dirty, or (write views) SMC-watched.
//! * **Biased pointers** — a nonzero entry holds
//!   `host_page_base - (linear_page << 12)`, so `entry + raw_linear` is the host
//!   address of the accessed byte. That deletes the in-page mask and lets the
//!   final address form inside the load's own addressing mode.
//! * **CPL-split** — four views (R0/W0/R3/W3) so the USER permission is
//!   evaluated once at fill time rather than on every probe. Legal because CPL
//!   is constant across a native chain (every CPL-changing shape is unadmitted).
//!
//! Layout is one contiguous heap block in this order, so the W view is a fixed
//! `disp32` from the R view in both CPL states and one pinned register serves
//! all four:
//!
//! ```text
//! offset 0x0000_0000  R0  supervisor read view
//! offset 0x0080_0000  W0  supervisor write view
//! offset 0x0100_0000  R3  user read view
//! offset 0x0180_0000  W3  user write view
//! ```
//!
//! Coherence is maintained by exactly the three mutators of the hashed table
//! (`insert` / `invalidate_page` / `flush`) plus `set_page_watched`; see the
//! `SoftwareTlb` methods in `lib.rs`. The [`SoftwareTlb::oracle_check_projection`]
//! family verifies that invariant is actually held.

/// Number of 20-bit linear pages: one entry per page in a 4 GiB linear space.
pub const FLAT_PAGE_COUNT: usize = 1 << 20;

/// Byte offset of the supervisor read view from the block base.
pub const FLAT_VIEW_R0_BYTE_OFFSET: usize = 0x0000_0000;
/// Byte offset of the supervisor write view from the block base.
pub const FLAT_VIEW_W0_BYTE_OFFSET: usize = 0x0080_0000;
/// Byte offset of the user read view from the block base.
pub const FLAT_VIEW_R3_BYTE_OFFSET: usize = 0x0100_0000;
/// Byte offset of the user write view from the block base.
pub const FLAT_VIEW_W3_BYTE_OFFSET: usize = 0x0180_0000;

/// Distance from a read view to its matching write view, in bytes.
///
/// Emitted probes rely on this being a single `disp32` addend that is identical
/// for both CPL states: `[r15 + page*8]` reads, `[r15 + page*8 + this]` writes.
pub const FLAT_VIEW_WRITE_DELTA: usize = FLAT_VIEW_W0_BYTE_OFFSET;

/// Distance from the supervisor view pair to the user view pair, in bytes.
///
/// `DynCore::run` adds this to the block base when publishing for CPL 3.
pub const FLAT_VIEW_CPL3_DELTA: usize = FLAT_VIEW_R3_BYTE_OFFSET;

/// Total number of `u64` slots across all four views.
pub const FLAT_SLOT_COUNT: usize = 4 * FLAT_PAGE_COUNT;

// Slot-index (not byte) bases for each view.
const R0: usize = 0;
const W0: usize = FLAT_PAGE_COUNT;
const R3: usize = 2 * FLAT_PAGE_COUNT;
const W3: usize = 3 * FLAT_PAGE_COUNT;

const _: () = {
    assert!(FLAT_VIEW_W0_BYTE_OFFSET == W0 * 8);
    assert!(FLAT_VIEW_R3_BYTE_OFFSET == R3 * 8);
    assert!(FLAT_VIEW_W3_BYTE_OFFSET == W3 * 8);
    // The write view must sit at the same delta from both read views, or one
    // pinned base register cannot serve all four (§3.3 of the HLD).
    assert!(FLAT_VIEW_W3_BYTE_OFFSET - FLAT_VIEW_R3_BYTE_OFFSET == FLAT_VIEW_WRITE_DELTA);
};

/// Compute the biased pointer for a page: `host_page_base - (linear_page << 12)`.
///
/// Wrapping is deliberate and load-bearing: the guest linear page can sit above
/// the host pointer, so the subtraction underflows and the later
/// `entry + raw_linear` wraps back to the correct host address.
#[inline]
pub fn bias(host_page_base: *const u8, linear_page: u32) -> u64 {
    (host_page_base as u64).wrapping_sub((linear_page as u64) << 12)
}

/// The four flat views plus the physical-page SMC watch bitmap.
///
/// Owned by `SoftwareTlb` so that the only code able to write a view is the same
/// code that maintains the authoritative hashed entries.
pub struct FlatTlbViews {
    /// `FLAT_SLOT_COUNT` entries: R0, W0, R3, W3 back to back.
    slots: Box<[u64]>,
    /// One bit per physical page: "the dyncore holds compiled code on this page".
    /// A set bit nulls that page's write views, folding the SMC fast-path
    /// predicate into write-view nullity.
    watched: Box<[u64]>,
    /// One bit per physical page: "a hardware page walk read a paging-structure
    /// entry (PDE/PTE) from this frame" — the EXP-N005 link-keep tripwire's
    /// premise set. Like `watched`, a set bit nulls the page's write views so
    /// every store to a recorded frame leaves the write fast path and reaches
    /// `Memory::smc_before_write`, where the PT-write tripwire fires. Bits
    /// accumulate and are never cleared (bounded by the guest's live paging
    /// structures — typically dozens of pages).
    pt_frames: Box<[u64]>,
    /// Number of physical pages the watch bitmap covers.
    watch_page_count: usize,
    /// Count of `set_page_watched` calls that actually flipped a bit.
    watch_transitions: u64,
    /// Total hashed slots examined by watch sweeps (the §6 sweep cost).
    watch_slots_scanned: u64,
    /// T7: legitimate biased pointers that landed on exactly 0 and were
    /// therefore published as a (correct, merely slow) miss.
    bias_zero_collisions: u64,
    /// Number of nonzero values ever written into a view. While this is 0 the
    /// whole 32 MiB block is provably still zero, so the O-P1(b) full scan is
    /// vacuous and can be skipped — which is what keeps the oracle affordable in
    /// the differential harness, where most scenarios never enable paging.
    nonzero_publishes: u64,
    /// Bumped whenever a view slot actually CHANGES value. Lets a per-instruction
    /// oracle skip re-verifying a projection that provably did not move, without
    /// weakening what it checks when it does move.
    mutation_epoch: u64,
    /// Inclusive range of page indices ever written by `publish`. A nonzero slot
    /// can only exist at a page that was published at some point, so the
    /// O-P1(b) full scan is EXACT when bounded to this range — and a scenario
    /// touching a handful of pages then costs a handful of slots instead of
    /// 4 Mi. `touched_lo > touched_hi` means nothing has been published.
    touched_lo: usize,
    touched_hi: usize,
}

impl FlatTlbViews {
    /// Allocate the views zeroed. 32 MiB of address space; resident footprint is
    /// bounded by the touched index range because the OS faults zero pages in on
    /// first touch.
    pub fn new(watch_page_count: usize) -> Self {
        Self {
            slots: vec![0u64; FLAT_SLOT_COUNT].into_boxed_slice(),
            watched: vec![0u64; watch_page_count.div_ceil(64)].into_boxed_slice(),
            pt_frames: vec![0u64; watch_page_count.div_ceil(64)].into_boxed_slice(),
            watch_page_count,
            watch_transitions: 0,
            watch_slots_scanned: 0,
            bias_zero_collisions: 0,
            nonzero_publishes: 0,
            mutation_epoch: 0,
            touched_lo: FLAT_PAGE_COUNT,
            touched_hi: 0,
        }
    }

    /// Grow the physical watch and PT-frame bitmaps to cover `count` pages.
    /// The linear view slots keep their stable address; only the bitmaps,
    /// which are never published, are reallocated. Never shrinks.
    pub fn ensure_watch_pages(&mut self, count: usize) {
        if count <= self.watch_page_count {
            return;
        }
        let words = count.div_ceil(64);
        let mut watched = std::mem::take(&mut self.watched).into_vec();
        watched.resize(words, 0);
        self.watched = watched.into_boxed_slice();
        let mut pt_frames = std::mem::take(&mut self.pt_frames).into_vec();
        pt_frames.resize(words, 0);
        self.pt_frames = pt_frames.into_boxed_slice();
        self.watch_page_count = count;
    }

    /// Number of physical pages the watch and PT-frame bitmaps cover.
    #[inline]
    #[must_use]
    pub fn watch_page_count(&self) -> usize {
        self.watch_page_count
    }

    /// Base address of the view block, for publication into `Cpu::jit_tlb_view`.
    ///
    /// Stable for the whole `Memory` lifetime: the block is allocated once and
    /// never reallocated.
    #[inline]
    pub fn base_ptr(&self) -> *const u64 {
        self.slots.as_ptr()
    }

    /// Is this physical page watched (i.e. does the dyncore hold code on it)?
    ///
    /// Out-of-range pages read as unwatched; a page that can be written through
    /// the fast path is always in range (see the fill-time assert in
    /// `SoftwareTlb::insert`).
    #[inline]
    pub fn is_watched(&self, physical_page: u32) -> bool {
        let p = physical_page as usize;
        if p >= self.watch_page_count {
            return false;
        }
        self.watched[p >> 6] & (1u64 << (p & 63)) != 0
    }

    /// Could ANY physical page in the inclusive range `[first_page, last_page]`
    /// hold dyncore-compiled code?
    ///
    /// This is the **write-side peer of the watch fold**. `is_watched` already
    /// nulls a watched page's write views so the dyncore's own emitted stores
    /// leave the fast path; this answers the same question for the write
    /// channels that do *not* read a view — the interpreter's fall-back stores
    /// and every `Memory::write_*` caller — so `Memory::smc_before_write` can
    /// skip the SMC snoop entirely on pages that do not host cached code.
    ///
    /// Conservative in the one direction that matters: pages OUTSIDE the
    /// bitmap answer `true`, because `set_watched` silently ignores an
    /// out-of-range page, so "not watched" there is "unknown", not "proven
    /// clean". An empty range (`first_page > last_page`) answers `false`.
    #[inline]
    #[must_use]
    pub fn span_may_hold_code(&self, first_page: usize, last_page: usize) -> bool {
        if last_page >= self.watch_page_count {
            return true;
        }
        (first_page..=last_page).any(|p| self.watched[p >> 6] & (1u64 << (p & 63)) != 0)
    }

    /// Is this physical page a recorded paging-structure frame (EXP-N005)?
    ///
    /// Out-of-range pages read as unrecorded here; the RECORD side treats
    /// out-of-range as unprobeable (see `Memory::record_pt_frame`), so an
    /// unrepresentable frame never depends on this probe for soundness.
    #[inline]
    pub fn is_pt_frame(&self, physical_page: u32) -> bool {
        let p = physical_page as usize;
        if p >= self.watch_page_count {
            return false;
        }
        self.pt_frames[p >> 6] & (1u64 << (p & 63)) != 0
    }

    /// Word `index` of the PT-frame bitmap: physical pages `index * 64 ..` in
    /// its bits. A word past the bitmap holds no recorded frame.
    #[inline]
    #[must_use]
    pub fn pt_frame_word(&self, index: usize) -> u64 {
        self.pt_frames.get(index).copied().unwrap_or(0)
    }

    /// Can the PT-frame bitmap represent this frame at all? A frame it cannot
    /// represent is permanently unprobeable and must take the sever-always
    /// route (EXP-N005 amendment A5).
    #[inline]
    pub fn pt_frame_representable(&self, physical_page: u32) -> bool {
        (physical_page as usize) < self.watch_page_count
    }

    /// Set the PT-frame bit. Returns `true` if the bit actually changed —
    /// the caller's signal that a write-view sweep is required. Out-of-range
    /// pages return `false` (callers must have checked representability).
    #[inline]
    pub fn set_pt_frame(&mut self, physical_page: u32) -> bool {
        let p = physical_page as usize;
        if p >= self.watch_page_count {
            return false;
        }
        let word = p >> 6;
        let mask = 1u64 << (p & 63);
        if self.pt_frames[word] & mask != 0 {
            return false;
        }
        self.pt_frames[word] |= mask;
        true
    }

    /// Could ANY physical page in the inclusive range `[first_page, last_page]`
    /// be a recorded paging-structure frame? Conservative in the same
    /// direction as `span_may_hold_code`: pages outside the bitmap answer
    /// `true` (a spurious sever, never a missed one).
    #[inline]
    #[must_use]
    pub fn span_may_be_pt_frame(&self, first_page: usize, last_page: usize) -> bool {
        if last_page >= self.watch_page_count {
            return true;
        }
        any_bit_in_span(&self.pt_frames, first_page, last_page)
    }

    /// Set or clear the watch bit. Returns `true` if the bit actually changed,
    /// which is the caller's signal that a sweep is required.
    #[inline]
    pub fn set_watched(&mut self, physical_page: u32, on: bool) -> bool {
        let p = physical_page as usize;
        if p >= self.watch_page_count {
            return false;
        }
        let word = p >> 6;
        let mask = 1u64 << (p & 63);
        let was = self.watched[word] & mask != 0;
        if was == on {
            return false;
        }
        if on {
            self.watched[word] |= mask;
        } else {
            self.watched[word] &= !mask;
        }
        self.watch_transitions += 1;
        true
    }

    /// Publish one page's four view entries from an authoritative hashed entry's
    /// decisions. `watched` nulls the write views only.
    #[inline]
    pub fn publish(
        &mut self,
        linear_page: u32,
        host_read: *const u8,
        host_write: *mut u8,
        user: bool,
        watched: bool,
    ) {
        let p = linear_page as usize;
        debug_assert!(p < FLAT_PAGE_COUNT, "linear page out of 20-bit range");

        let r = if host_read.is_null() {
            0
        } else {
            let b = bias(host_read, linear_page);
            if b == 0 {
                // T7: a legitimate bias of exactly 0 is indistinguishable from a
                // miss. Publishing 0 is correct (merely slow); count it so a
                // nonzero count on any workload is a signal to re-bias.
                self.bias_zero_collisions += 1;
            }
            b
        };
        let w = if host_write.is_null() || watched {
            0
        } else {
            let b = bias(host_write.cast_const(), linear_page);
            if b == 0 {
                self.bias_zero_collisions += 1;
            }
            b
        };

        if r != 0 || w != 0 {
            self.nonzero_publishes += 1;
        }
        self.mutation_epoch += 1;
        self.touched_lo = self.touched_lo.min(p);
        self.touched_hi = self.touched_hi.max(p);
        self.slots[R0 + p] = r;
        self.slots[W0 + p] = w;
        self.slots[R3 + p] = if user { r } else { 0 };
        self.slots[W3 + p] = if user { w } else { 0 };
    }

    /// Republish only the write views for a page (used by the watch sweep, which
    /// cannot change read permissions).
    #[inline]
    pub fn publish_write_only(
        &mut self,
        linear_page: u32,
        host_write: *mut u8,
        user: bool,
        watched: bool,
    ) {
        let p = linear_page as usize;
        debug_assert!(p < FLAT_PAGE_COUNT, "linear page out of 20-bit range");
        let w = if host_write.is_null() || watched {
            0
        } else {
            bias(host_write.cast_const(), linear_page)
        };
        if w != 0 {
            self.nonzero_publishes += 1;
        }
        self.mutation_epoch += 1;
        self.touched_lo = self.touched_lo.min(p);
        self.touched_hi = self.touched_hi.max(p);
        self.slots[W0 + p] = w;
        self.slots[W3 + p] = if user { w } else { 0 };
    }

    /// Null all four views for a page. Idempotent in its slot effect, and
    /// correct even for a page that was collision-evicted from the hashed
    /// table.
    ///
    /// The epoch bump is **unconditional**, matching `publish` and
    /// `publish_write_only` (MDD-BUG-FLUX-00086). It used to fire only when a
    /// slot had been nonzero, on the reasoning that nulling an already-null
    /// page changes nothing observable. That is true of the *projection* and
    /// false of the *translation*: a code page can legitimately sit at all-zero
    /// slots (SMC-watched W, firmware-filtered or bias-zero-collision R), and
    /// nulling it is how an INVLPG or a CR3 flush announces that its mapping is
    /// gone. Consumers read this epoch to decide whether a translation may have
    /// changed — the drc native chain link records it at patch time and
    /// re-checks it on traversal — so a silent null let a PTE rewrite remap the
    /// same linear EIP to different physical code while the link still matched,
    /// and the guest jumped to the stale physical successor.
    ///
    /// The cost is a counter increment plus, at most, a refused chain link that
    /// falls back to ordinary dispatch and re-links. It is not block
    /// invalidation, and it cannot be worse than the churn `publish` already
    /// causes on every warm.
    #[inline]
    pub fn null_page(&mut self, linear_page: u32) {
        let p = linear_page as usize;
        debug_assert!(p < FLAT_PAGE_COUNT, "linear page out of 20-bit range");
        self.mutation_epoch += 1;
        self.slots[R0 + p] = 0;
        self.slots[W0 + p] = 0;
        self.slots[R3 + p] = 0;
        self.slots[W3 + p] = 0;
    }

    /// Read a view entry, by page and view. Oracle/test use, plus warmers'
    /// idempotence probes (a nonzero entry proves a prior publish; the value
    /// itself must not be interpreted) — production Rust never reads the
    /// projection to answer a translation query.
    #[inline]
    pub fn entry(&self, view: FlatView, linear_page: u32) -> u64 {
        self.slots[view.slot_base() + linear_page as usize]
    }

    /// Total slots examined by watch sweeps so far.
    #[inline]
    pub fn watch_slots_scanned(&self) -> u64 {
        self.watch_slots_scanned
    }

    /// Number of watch-bit transitions so far.
    #[inline]
    pub fn watch_transitions(&self) -> u64 {
        self.watch_transitions
    }

    /// T7 counter: biased pointers that collided with the miss sentinel.
    #[inline]
    pub fn bias_zero_collisions(&self) -> u64 {
        self.bias_zero_collisions
    }

    /// Record sweep work for the §6 cost tripwire.
    #[inline]
    pub(crate) fn add_slots_scanned(&mut self, n: u64) {
        self.watch_slots_scanned += n;
    }

    /// Has any nonzero value ever been published into a view?
    ///
    /// When false, the block is provably still all-zero and the O-P1(b) full
    /// scan cannot find anything — the honest way to skip it, rather than
    /// sampling or thinning the oracle.
    /// Monotonic counter of real view-slot changes. Equal values across two
    /// observations prove the projection did not move in between.
    #[inline]
    pub fn mutation_epoch(&self) -> u64 {
        self.mutation_epoch
    }

    /// Stable pointer to the epoch counter, valid while `self` does not move.
    /// A native engine may capture it for one synchronous engine entry so
    /// generated code can compare the live epoch without a helper call; it
    /// must recapture on every entry (Memory can be replaced between entries).
    #[inline]
    pub fn mutation_epoch_ptr(&self) -> *const u64 {
        core::ptr::addr_of!(self.mutation_epoch)
    }

    #[inline]
    pub fn has_published_nonzero(&self) -> bool {
        self.nonzero_publishes != 0
    }

    /// Iterate every nonzero slot as `(view, page, value)`. Oracle use only —
    /// this is a full 4 Mi-slot scan.
    pub fn for_each_nonzero(&self, mut f: impl FnMut(FlatView, u32, u64)) {
        if self.touched_lo > self.touched_hi {
            return; // nothing ever published: provably all zero
        }
        for (view, base) in [
            (FlatView::Read0, R0),
            (FlatView::Write0, W0),
            (FlatView::Read3, R3),
            (FlatView::Write3, W3),
        ] {
            for page in self.touched_lo..=self.touched_hi {
                let v = self.slots[base + page];
                if v != 0 {
                    f(view, page as u32, v);
                }
            }
        }
    }
}

/// Why one flat-view slot reads null — the census-only decomposition of the
/// projection's folded nullity (see the module comment: `0` means "slow path"
/// for every reason at once). Diagnostic: nothing behavioural reads it, and
/// it is derived from the authoritative hashed entry, never from the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlatNullCause {
    /// The hashed slot for this page holds no live translation.
    TlbEmpty,
    /// The hashed slot holds a DIFFERENT live page: this one was evicted by
    /// (or never displaced) a colliding page — a capacity/conflict miss.
    TlbConflict,
    /// CPL 3 access to a supervisor-only page.
    UserDenied,
    /// No host read pointer: not ordinary RAM (MMIO, VGA, LFB, ROM image,
    /// option ROM), or withheld by the firmware MP-table read filter.
    NotRam,
    /// Write to a page whose translation is read-only.
    WriteReadOnly,
    /// Write to a writable page whose dirty bit is not yet set.
    WriteNotDirty,
    /// Write to RAM whose host write pointer is withheld (BIOS ROM segment,
    /// descriptor-table snoop hull).
    WriteWithheld,
    /// Write to a physical page holding compiled code (SMC watch).
    WriteCodeWatched,
    /// Write to a recorded paging-structure frame (EXP-N005 divert).
    WritePtFrame,
    /// None of the above: the view is non-null, or a bias-zero collision /
    /// suppressed projection nulled it.
    Other,
}

impl FlatNullCause {
    /// Every cause, in declaration order (`self as usize` indexes this).
    pub const ALL: [Self; 10] = [
        Self::TlbEmpty,
        Self::TlbConflict,
        Self::UserDenied,
        Self::NotRam,
        Self::WriteReadOnly,
        Self::WriteNotDirty,
        Self::WriteWithheld,
        Self::WriteCodeWatched,
        Self::WritePtFrame,
        Self::Other,
    ];

    /// One-word report token.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::TlbEmpty => "tlb_empty",
            Self::TlbConflict => "tlb_conflict",
            Self::UserDenied => "user_denied",
            Self::NotRam => "not_ram",
            Self::WriteReadOnly => "write_read_only",
            Self::WriteNotDirty => "write_not_dirty",
            Self::WriteWithheld => "write_withheld",
            Self::WriteCodeWatched => "write_code_watched",
            Self::WritePtFrame => "write_pt_frame",
            Self::Other => "other",
        }
    }
}

/// Which of the four views an entry belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlatView {
    /// Supervisor read.
    Read0,
    /// Supervisor write.
    Write0,
    /// User read.
    Read3,
    /// User write.
    Write3,
}

impl FlatView {
    #[inline]
    fn slot_base(self) -> usize {
        match self {
            FlatView::Read0 => R0,
            FlatView::Write0 => W0,
            FlatView::Read3 => R3,
            FlatView::Write3 => W3,
        }
    }

    /// Is this a write view?
    #[inline]
    pub fn is_write(self) -> bool {
        matches!(self, FlatView::Write0 | FlatView::Write3)
    }

    /// Is this a user (CPL 3) view?
    #[inline]
    pub fn is_user(self) -> bool {
        matches!(self, FlatView::Read3 | FlatView::Write3)
    }
}

/// Is any bit in the inclusive range `[first, last]` of `bits` set? Word-masked,
/// so a 16-page aperture probe is one or two word tests rather than one test per
/// page. An empty range (`first > last`) answers `false`. The caller has checked
/// `last` against the bitmap's length.
#[inline]
fn any_bit_in_span(bits: &[u64], first: usize, last: usize) -> bool {
    if first > last {
        return false;
    }
    let (first_word, last_word) = (first >> 6, last >> 6);
    let low_mask = u64::MAX << (first & 63);
    let high_mask = u64::MAX >> (63 - (last & 63));
    if first_word == last_word {
        return bits[first_word] & low_mask & high_mask != 0;
    }
    bits[first_word] & low_mask != 0
        || bits[first_word + 1..last_word].iter().any(|&w| w != 0)
        || bits[last_word] & high_mask != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bias_reconstructs_the_host_address_from_the_raw_linear() {
        // The whole point of the bias: no in-page mask at probe time.
        let page_store = vec![0u8; 8192];
        let host_page_base = page_store.as_ptr();
        let linear_page = 0x1_2345u32;
        let b = bias(host_page_base, linear_page);
        for offset in [0u32, 1, 0xFFF] {
            let raw_linear = (linear_page << 12) | offset;
            let reconstructed = b.wrapping_add(raw_linear as u64);
            assert_eq!(
                reconstructed,
                host_page_base as u64 + offset as u64,
                "bias + raw linear must land on the host byte, offset {offset:#x}"
            );
        }
    }

    #[test]
    fn bias_wraps_when_the_linear_page_sits_above_the_host_pointer() {
        // Guest linear can exceed the host pointer; the subtraction underflows
        // and the addition wraps back. Anything other than wrapping arithmetic
        // is a panic in debug and a wrong address in release.
        let page_store = vec![0u8; 4096];
        let host_page_base = page_store.as_ptr();
        let linear_page = 0xF_FFFFu32; // 4 GiB - 4 KiB, above any real host ptr
        let b = bias(host_page_base, linear_page);
        let raw_linear = linear_page << 12;
        assert_eq!(b.wrapping_add(raw_linear as u64), host_page_base as u64);
    }

    #[test]
    fn user_views_mirror_supervisor_views_only_when_user_is_set() {
        let mut v = FlatTlbViews::new(32768);
        let store = vec![0u8; 4096];
        let ptr = store.as_ptr();
        let page = 0x200u32;

        v.publish(page, ptr, ptr.cast_mut(), false, false);
        assert_ne!(v.entry(FlatView::Read0, page), 0);
        assert_ne!(v.entry(FlatView::Write0, page), 0);
        assert_eq!(v.entry(FlatView::Read3, page), 0, "supervisor-only page");
        assert_eq!(v.entry(FlatView::Write3, page), 0, "supervisor-only page");

        v.publish(page, ptr, ptr.cast_mut(), true, false);
        assert_eq!(
            v.entry(FlatView::Read3, page),
            v.entry(FlatView::Read0, page)
        );
        assert_eq!(
            v.entry(FlatView::Write3, page),
            v.entry(FlatView::Write0, page)
        );
    }

    #[test]
    fn watching_a_page_nulls_only_the_write_views() {
        let mut v = FlatTlbViews::new(32768);
        let store = vec![0u8; 4096];
        let ptr = store.as_ptr();
        let page = 0x300u32;

        v.publish(page, ptr, ptr.cast_mut(), true, true);
        assert_ne!(v.entry(FlatView::Read0, page), 0, "reads stay live");
        assert_ne!(v.entry(FlatView::Read3, page), 0, "reads stay live");
        assert_eq!(v.entry(FlatView::Write0, page), 0, "watched ⇒ W nulled");
        assert_eq!(v.entry(FlatView::Write3, page), 0, "watched ⇒ W nulled");
    }

    #[test]
    fn ensure_watch_pages_grows_both_bitmaps_and_keeps_bits() {
        let mut v = FlatTlbViews::new(64);
        assert!(v.set_watched(5, true));
        assert!(v.set_pt_frame(6));
        assert!(!v.set_watched(100, true), "beyond the bitmap");
        v.ensure_watch_pages(130);
        assert_eq!(v.watch_page_count(), 130);
        assert!(
            v.is_watched(5) && v.is_pt_frame(6),
            "growth keeps live bits"
        );
        assert!(v.set_watched(129, true) && v.is_watched(129));
        assert!(v.set_pt_frame(129) && v.is_pt_frame(129));
        assert!(!v.is_watched(130));
        v.ensure_watch_pages(10);
        assert_eq!(v.watch_page_count(), 130, "never shrinks");
    }

    #[test]
    fn set_watched_reports_only_real_transitions() {
        let mut v = FlatTlbViews::new(32768);
        assert!(v.set_watched(7, true), "0->1 is a transition");
        assert!(!v.set_watched(7, true), "1->1 is not");
        assert!(v.is_watched(7));
        assert!(v.set_watched(7, false), "1->0 is a transition");
        assert!(!v.set_watched(7, false), "0->0 is not");
        assert!(!v.is_watched(7));
        assert_eq!(v.watch_transitions(), 2);
    }

    #[test]
    fn watch_bits_out_of_range_are_unwatched_and_unsettable() {
        let mut v = FlatTlbViews::new(64);
        assert!(!v.is_watched(64));
        assert!(!v.set_watched(64, true));
        assert!(!v.is_watched(64));
    }

    #[test]
    fn span_may_hold_code_reports_any_watched_page_in_the_range() {
        let mut v = FlatTlbViews::new(32768);
        assert!(!v.span_may_hold_code(4, 4), "no page watched yet");
        v.set_watched(5, true);
        assert!(
            !v.span_may_hold_code(4, 4),
            "a neighbour's bit is not this page"
        );
        assert!(!v.span_may_hold_code(6, 7));
        assert!(v.span_may_hold_code(5, 5), "the watched page itself");
        assert!(
            v.span_may_hold_code(4, 5),
            "a straddle INTO the watched page"
        );
        assert!(
            v.span_may_hold_code(5, 6),
            "a straddle OUT of the watched page"
        );
        v.set_watched(5, false);
        assert!(!v.span_may_hold_code(4, 6), "un-watching re-opens the gate");
    }

    #[test]
    fn span_may_hold_code_is_conservative_outside_the_bitmap() {
        // `set_watched` silently ignores an out-of-range page, so "bit clear"
        // there means "unknown". The gate must answer yes, or a write above the
        // bitmap could skip an invalidation nothing else would catch.
        let v = FlatTlbViews::new(64);
        assert!(v.span_may_hold_code(64, 64));
        assert!(v.span_may_hold_code(63, 64), "a straddle off the end");
        assert!(!v.span_may_hold_code(62, 63), "wholly in range and clear");
    }

    #[test]
    fn span_may_hold_code_on_an_empty_range_is_false() {
        let mut v = FlatTlbViews::new(32768);
        v.set_watched(9, true);
        assert!(
            !v.span_may_hold_code(10, 9),
            "first > last is an empty span"
        );
    }

    #[test]
    fn null_page_clears_all_four_views_and_is_idempotent() {
        let mut v = FlatTlbViews::new(32768);
        let store = vec![0u8; 4096];
        let ptr = store.as_ptr();
        let page = 0x400u32;
        v.publish(page, ptr, ptr.cast_mut(), true, false);
        v.null_page(page);
        v.null_page(page);
        for view in [
            FlatView::Read0,
            FlatView::Write0,
            FlatView::Read3,
            FlatView::Write3,
        ] {
            assert_eq!(v.entry(view, page), 0);
        }
    }

    /// MDD-BUG-FLUX-00086: nulling a page whose four slots are ALREADY zero
    /// must still move the epoch. Such a page is not "unmapped and boring" — a
    /// code page sits at all-zero slots whenever its writes are SMC-watched or
    /// its read bias collides with zero, and the null is how an INVLPG or CR3
    /// flush announces the mapping is gone. Consumers (the drc native chain
    /// link) read this epoch to decide whether a translation may have changed;
    /// a silent null lets them keep a link across a PTE rewrite and jump to
    /// stale physical code.
    #[test]
    fn nulling_an_already_null_page_still_moves_the_epoch() {
        let mut v = FlatTlbViews::new(32768);
        let page = 0x600u32;

        // Never published: all four slots are zero.
        for view in [
            FlatView::Read0,
            FlatView::Write0,
            FlatView::Read3,
            FlatView::Write3,
        ] {
            assert_eq!(v.entry(view, page), 0, "precondition: slot is already 0");
        }

        let before = v.mutation_epoch();
        v.null_page(page);
        assert!(
            v.mutation_epoch() > before,
            "nulling an all-zero page must move the epoch: {before} -> {}",
            v.mutation_epoch()
        );

        // And again — every null is an announcement, so each one moves it.
        let second = v.mutation_epoch();
        v.null_page(page);
        assert!(
            v.mutation_epoch() > second,
            "a repeat null is still a translation announcement"
        );
    }

    /// MDD-BUG-FLUX-00086, the watched-write shape specifically: an SMC-watched
    /// page publishes zero into BOTH write views, so a code page under SMC
    /// watch with a bias-zero read reaches all-zero slots through the ordinary
    /// publish path rather than by never being published.
    #[test]
    fn nulling_a_watched_page_published_to_all_zero_slots_moves_the_epoch() {
        let mut v = FlatTlbViews::new(32768);
        let page = 0x601u32;

        // watched = true nulls the write views; a null host_read nulls the
        // read views. The page IS published, and lands all-zero.
        v.publish(page, std::ptr::null(), std::ptr::null_mut(), true, true);
        for view in [
            FlatView::Read0,
            FlatView::Write0,
            FlatView::Read3,
            FlatView::Write3,
        ] {
            assert_eq!(v.entry(view, page), 0, "published, but all-zero");
        }

        let before = v.mutation_epoch();
        v.null_page(page);
        assert!(
            v.mutation_epoch() > before,
            "a published-but-all-zero page must still announce its nulling"
        );
    }

    #[test]
    fn a_null_host_read_publishes_a_miss_in_both_read_views() {
        let mut v = FlatTlbViews::new(32768);
        let page = 0x500u32;
        v.publish(page, std::ptr::null(), std::ptr::null_mut(), true, false);
        assert_eq!(v.entry(FlatView::Read0, page), 0);
        assert_eq!(v.entry(FlatView::Read3, page), 0);
        assert_eq!(v.entry(FlatView::Write0, page), 0);
        assert_eq!(v.entry(FlatView::Write3, page), 0);
    }
}
