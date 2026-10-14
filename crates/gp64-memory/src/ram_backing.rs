//! Host storage for guest RAM with sparse backing and shared mappings.
//!
//! On every 64-bit host [`RamBacking`] lives inside one fixed host reservation
//! of [`RAM_RESERVATION_BYTES`] that never moves: the reservation is made with
//! no access, the private RAM is committed read/write at its start, and growth
//! commits more of the same reservation in place. Everything else in the
//! reservation stays reserved and no-access until a session arena section
//! replaces its arena portion. The reservation is large enough that
//! `base + FASTMEM_PHYS_BIAS + linear` is inside it for every 32-bit linear
//! address, which is what lets a second mapping of it serve as the JIT's
//! fastmem window (`fastmem.rs`).
//!
//! - Unix: one `mmap(PROT_NONE, MAP_NORESERVE)`; commit is `mprotect`.
//! - Windows: a `VirtualAlloc2` placeholder, split at [`PRIVATE_WINDOW_BYTES`].
//!   The private window becomes an ordinary reservation committed with
//!   `VirtualAlloc(MEM_COMMIT)`; the rest stays a placeholder so the arena
//!   can replace it with a section view (`MapViewOfFile3`).
//! - 32-bit hosts and wasm cannot reserve 4 GiB and keep a growable `Vec<u8>`
//!   with the same API; they never offer fastmem or the arena.
//!
//! **No whole slice.** The backing is sparse, so it never hands out a slice
//! covering the reservation, and has no `Deref`/`AsRef` to `[u8]`. Indexing
//! (`ram[phys]`, `ram[a..b]`, `ram[a..=b]`, `ram[..b]`, `ram[a..]`) validates
//! the whole requested range against the backed extents first and only then
//! builds a reference to exactly that range. Indexing into a hole panics: it is
//! a defect in trusted emulator code. Guest pointers must pass checked
//! translation (or take a modelled guest fault) before they reach indexing.
//! Raw pointers come from [`RamBacking::ptr_for`] / [`RamBacking::host_base`],
//! which read the mapping metadata and never go through a slice.
//!
//! Private RAM is `[0, len())`. Committed arena pages form further extents
//! only while this backing holds the session's unique admission token.

use gp64_types::SmcNotify;
use std::ops::{Index, IndexMut, Range, RangeFrom, RangeInclusive, RangeTo, RangeToInclusive};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::arena::{
    ArenaAdmission, ArenaStore, ARENA_PHYS_END, ARENA_PHYS_START, ARENA_TABLES_PHYS_START,
};
use crate::arena_smc::ArenaSmc;

use gp64_fastmem::FASTMEM_PHYS_BIAS;
#[cfg(target_pointer_width = "64")]
use gp64_fastmem::RAM_RESERVATION_BYTES;

/// Reservation offset where the private window ends: the physical address of
/// guest linear `0x8000_0000`, where the flat-memory shared arena starts (HLD §5.2
/// "Placement"). Private RAM always lies below it, and on Windows the
/// reservation is split here so the arena range stays a placeholder. 64 KiB
/// aligned, as `VirtualAlloc2` placeholder splits require.
pub const PRIVATE_WINDOW_BYTES: usize = FASTMEM_PHYS_BIAS + 0x8000_0000;

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(RAM_RESERVATION_BYTES >= FASTMEM_PHYS_BIAS + (1usize << 32) + 0x1_0000);
    assert!(PRIVATE_WINDOW_BYTES >= crate::MAX_RAM_SIZE + crate::FETCH_PADDING);
    assert!(PRIVATE_WINDOW_BYTES < RAM_RESERVATION_BYTES);
    assert!(PRIVATE_WINDOW_BYTES.is_multiple_of(0x1_0000));
};

/// Byte-addressable guest RAM in a sparse host reservation. Its allocation can
/// only be resized by [`crate::Memory`]; other crates index checked ranges.
///
/// It never coerces to a whole slice (HLD §10 "No whole-slice regression"):
///
/// ```compile_fail
/// fn whole(ram: &gp64_memory::RamBacking) -> &[u8] {
///     ram
/// }
/// ```
///
/// ```compile_fail
/// fn whole_mut(ram: &mut gp64_memory::RamBacking) -> &mut [u8] {
///     ram.as_mut()
/// }
/// ```
///
/// ```compile_fail
/// fn full(ram: &gp64_memory::RamBacking) -> usize {
///     ram[..].len()
/// }
/// ```
///
/// ```compile_fail
/// fn iter(ram: &gp64_memory::RamBacking) -> usize {
///     ram.iter().count()
/// }
/// ```
///
/// Checked ranges compile:
///
/// ```
/// fn part(ram: &gp64_memory::RamBacking) -> Option<&[u8]> {
///     ram.get(0x400..0x500)
/// }
/// ```
pub struct RamBacking {
    /// Host address of physical byte 0. Pointers derived from it carry the
    /// allocation's full (read/write) provenance; it is never obtained from a
    /// reference.
    base: NonNull<u8>,
    /// End of the private RAM extent: `[0, len)` is committed read/write.
    len: usize,
    storage: Storage,
    /// The shared-arena view, boxed: every access to it follows a failed
    /// private-range check, and it must not grow `Memory` (`memory_hot_layout`).
    arena: Box<ArenaAttachment>,
    /// An HLE write reached this process's own arena code; folded into
    /// `Memory::is_instr_cache_dirty` so the write-path flag stays inline.
    arena_code_dirty: AtomicBool,
}

/// [`RamBacking`]'s shared-arena state.
#[derive(Default)]
struct ArenaAttachment {
    /// Mapping stays attached while parked; admission alone governs whether
    /// safe references or published pointers may reach its shared bytes.
    store: Option<Arc<ArenaStore>>,
    admission: Option<ArenaAdmission>,
    /// Session arena SMC state and this process's PID, present only while
    /// this backing is mounted AND some process watches an arena page
    /// (`Memory::refresh_arena_hle_hook`). `None` costs a mutable arena
    /// access one test.
    hle_smc: Option<(u32, Arc<Mutex<ArenaSmc>>)>,
    /// The bound dyncore snoop, for HLE writes into this process's own
    /// arena code.
    hle_notifier: Option<Arc<dyn SmcNotify>>,
}

// SAFETY: private ranges are owned exclusively by this backing, as `Vec<u8>`
// owns its heap block. Shared arena bytes are aliased by every attached
// backing of one session, but a backing forms references to them (checked
// indexing, `get`, `ptr_for`, extents) only while it holds that session's
// `ArenaAdmission`, and exactly one token exists per `ArenaStore` (it is not
// `Clone`, `ArenaStore::new` mints it once, and a session `Clone` drops it).
// The token moves only through `&mut Memory` on the giving side and on the
// receiving side (`take_arena_admission` / `give_arena_admission`), so every
// reference the old holder produced has ended before the new holder can form
// one, whichever threads the two `Memory` objects live on. Host workers never
// touch arena bytes (HLD I3). Hence the usual borrow rules still separate
// every thread's access, arena included.
unsafe impl Send for RamBacking {}
// SAFETY: `&RamBacking` yields only shared references (checked indexing); a
// mutable arena reference needs `&mut RamBacking` of the one admitted backing,
// and no other backing can form any arena reference meanwhile (see `Send`).
unsafe impl Sync for RamBacking {}

impl RamBacking {
    pub(crate) fn set_arena_hle_smc(&mut self, hook: Option<(u32, Arc<Mutex<ArenaSmc>>)>) {
        self.arena.hle_smc = hook;
    }

    pub(crate) fn set_hle_notifier(&mut self, notifier: Option<Arc<dyn SmcNotify>>) {
        self.arena.hle_notifier = notifier;
    }

    /// Peek at the HLE arena-code dirty bit (see `arena_code_dirty`).
    #[inline]
    pub(crate) fn arena_code_dirty(&self) -> bool {
        self.arena_code_dirty.load(Ordering::Relaxed)
    }

    /// Take the HLE arena-code dirty bit.
    #[inline]
    pub(crate) fn take_arena_code_dirty(&self) -> bool {
        self.arena_code_dirty.swap(false, Ordering::Relaxed)
    }

    /// Called before a mutable reference is formed (HLD §5.4 "HLE writes").
    /// Private RAM lies wholly below the arena, so the private path pays this
    /// one compare; the arena half stays out of line.
    #[inline(always)]
    fn notify_hle_mutation(&self, start: usize, len: usize) {
        if start >= ARENA_PHYS_START {
            self.notify_arena_hle_mutation(start, len);
        }
    }

    /// No memory method is re-entered: the caller holds `&mut RamBacking`,
    /// and the snoop only records the span.
    #[cold]
    #[inline(never)]
    fn notify_arena_hle_mutation(&self, start: usize, len: usize) {
        let Some((pid, state)) = &self.arena.hle_smc else {
            return;
        };
        let end = start.saturating_add(len).min(ARENA_PHYS_END);
        let start = start.max(ARENA_PHYS_START);
        if start >= end {
            return;
        }
        let range = start as u32..end as u32;
        let local = {
            let mut state = state.lock().expect("arena SMC lock poisoned");
            let interest = state.interest_in(*pid, range.clone());
            if interest.1 {
                state.append(range);
            }
            interest.0
        };
        if local {
            self.arena_code_dirty.store(true, Ordering::Relaxed);
            if let Some(notifier) = &self.arena.hle_notifier {
                notifier.notify_write(start, end - start);
            }
        }
    }

    /// Zeroed backing of `len` bytes with at least `capacity` bytes committed
    /// (the `vec![0; capacity]` + `truncate(len)` shape).
    pub(crate) fn zeroed(capacity: usize, len: usize) -> Self {
        assert!(
            len <= capacity,
            "RAM length {len:#x} exceeds capacity {capacity:#x}"
        );
        let mut storage = Storage::zeroed(capacity, len);
        let base = storage.base();
        Self {
            base,
            len,
            storage,
            arena: Box::default(),
            arena_code_dirty: AtomicBool::new(false),
        }
    }

    /// `Vec::resize` semantics: shrinking truncates, growing appends `value`,
    /// and growth past the capacity raises it to `max(2 * capacity, new_len)`.
    pub(crate) fn resize(&mut self, new_len: usize, value: u8) {
        self.storage.resize(self.len, new_len, value);
        self.base = self.storage.base();
        self.len = new_len;
    }

    /// Logical capacity, as `Vec::capacity` would report it.
    pub(crate) fn capacity(&self) -> usize {
        self.storage.capacity()
    }

    /// End of the private RAM extent (fetch padding included): `[0, len())` is
    /// always backed. This is an extent query, not the reservation size; use
    /// [`Self::is_backed`] for a range anywhere in the reservation.
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when no RAM is backed.
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Bytes of host address space the backing spans (backed or not).
    pub fn reservation_len(&self) -> usize {
        self.storage.reservation_len()
    }

    /// The host-backed extents visible to this mounted backing, in order.
    /// Whole-RAM consumers (clear, hashing, snapshots) iterate these instead of
    /// assuming one contiguous slice.
    pub fn backed_extents(&self) -> impl Iterator<Item = Range<usize>> {
        let mut extents = Vec::new();
        if self.len != 0 {
            extents.push(0..self.len);
        }
        if let Some(admission) = &self.arena.admission {
            extents.extend(admission.store.committed_extents());
        }
        extents.into_iter()
    }

    /// True when every byte of `range` is host-backed. An empty range is
    /// backed when its start lies within or at the end of a backed extent,
    /// matching slice indexing.
    #[inline(always)]
    pub fn is_backed(&self, range: Range<usize>) -> bool {
        if range.start > range.end {
            return false;
        }
        if range.end <= self.len {
            return true;
        }
        self.is_admitted_arena(range)
    }

    /// The arena half of [`Self::is_backed`], kept out of line so the private
    /// check inlines as one compare.
    #[cold]
    #[inline(never)]
    fn is_admitted_arena(&self, range: Range<usize>) -> bool {
        self.arena
            .admission
            .as_ref()
            .is_some_and(|token| token.store.is_committed(range))
    }

    /// End of the backed extent containing `offset` (the first hole at or after
    /// it), or `None` when `offset` is not backed. `offset == extent end` counts
    /// as the (empty) tail of that extent.
    #[inline]
    pub fn backed_end(&self, offset: usize) -> Option<usize> {
        if offset <= self.len {
            return Some(self.len);
        }
        self.arena
            .admission
            .as_ref()
            .and_then(|token| token.store.committed_end(offset))
    }

    /// Map the session section without granting access to its bytes.
    pub(crate) fn attach_arena(&mut self, store: Arc<ArenaStore>) -> std::io::Result<()> {
        if let Some(existing) = &self.arena.store {
            if Arc::ptr_eq(existing, &store) {
                return Ok(());
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "another arena is attached",
            ));
        }
        self.storage.map_arena(&store)?;
        if !store.is_committed(ARENA_TABLES_PHYS_START..ARENA_PHYS_END) {
            let view = unsafe { self.base.as_ptr().add(ARENA_PHYS_START) };
            if let Err(error) = store.commit(view, ARENA_TABLES_PHYS_START..ARENA_PHYS_END) {
                self.storage.unmap_arena();
                return Err(error);
            }
        }
        self.arena.store = Some(store);
        Ok(())
    }

    /// Revoke admission before replacing or releasing the host mapping.
    pub(crate) fn take_admission(&mut self) -> Option<ArenaAdmission> {
        self.arena.admission.take()
    }

    /// Give the unique token to this mapped backing.
    pub(crate) fn give_admission(&mut self, token: ArenaAdmission) {
        let store = self
            .arena
            .store
            .as_ref()
            .expect("arena admission needs a mapping");
        assert!(
            Arc::ptr_eq(store, &token.store),
            "admission is for another arena"
        );
        assert!(
            self.arena.admission.is_none(),
            "two arena admissions in one backing"
        );
        self.arena.admission = Some(token);
    }

    pub fn has_arena_admission(&self) -> bool {
        self.arena.admission.is_some()
    }

    pub fn has_arena_mapping(&self) -> bool {
        self.arena.store.is_some()
    }

    /// Commit before U4 publishes a present arena PTE.
    pub(crate) fn commit_arena(&mut self, range: Range<usize>) -> std::io::Result<()> {
        let token = self
            .arena
            .admission
            .as_ref()
            .expect("arena commit needs admission");
        // SAFETY: the backing owns this fixed reservation and has mapped the
        // store over the arena range.
        let view = unsafe { self.base.as_ptr().add(ARENA_PHYS_START) };
        token.store.commit(view, range)
    }

    /// Remove a parked or mounted process's section view. A live admission
    /// returns to its session owner before the mapping changes.
    pub(crate) fn detach_arena(&mut self) -> Option<ArenaAdmission> {
        let token = self.arena.admission.take();
        if self.arena.store.take().is_some() {
            self.storage.unmap_arena();
        }
        token
    }

    /// Host address of physical byte 0 — the base the engines publish for
    /// `base + phys` access. Only offsets inside [`Self::backed_extents`] may be
    /// dereferenced; publishers prove each range with [`Self::is_backed`] (or a
    /// `usable_len()` bound, which lies inside the private extent). Derived from
    /// the mapping, not a reference, so it carries write provenance.
    #[inline(always)]
    pub fn host_base(&self) -> *mut u8 {
        self.base.as_ptr()
    }

    /// Host pointer to the start of `range` when the whole range is backed.
    /// Write provenance; the caller upholds Rust's aliasing rules for any
    /// access it makes through it.
    #[inline]
    pub fn ptr_for(&self, range: Range<usize>) -> Option<NonNull<u8>> {
        if self.is_backed(range.clone()) {
            // SAFETY: `is_backed` confines the pointer to a committed private
            // or admitted arena extent of this fixed mapping.
            Some(unsafe { NonNull::new_unchecked(self.base.as_ptr().add(range.start)) })
        } else {
            None
        }
    }

    /// Base of the stable host reservation, or `None` on hosts that keep the
    /// growable `Vec` (whose address may move on growth).
    pub fn stable_reservation_base(&self) -> Option<*mut u8> {
        self.storage.is_stable().then_some(self.base.as_ptr())
    }

    /// Checked access: `None` instead of a panic when the range is not backed.
    #[inline]
    pub fn get<I: RamIndex>(&self, index: I) -> Option<&I::Output> {
        let (start, len) = index.checked_bounds(self).ok()?;
        // SAFETY: `checked_bounds` proved `[start, start + len)` backed.
        Some(unsafe { I::build(self.base.as_ptr().add(start), len) })
    }

    /// Checked mutable access: `None` when the range is not backed.
    #[inline]
    pub fn get_mut<I: RamIndex>(&mut self, index: I) -> Option<&mut I::Output> {
        let (start, len) = index.checked_bounds(self).ok()?;
        self.notify_hle_mutation(start, len);
        // SAFETY: as `get`, with exclusive access through `&mut self`.
        Some(unsafe { I::build_mut(self.base.as_ptr().add(start), len) })
    }

    /// `slice::copy_within` over backed ranges: copies `src` to `dest`
    /// (overlap allowed). Panics unless both ranges are backed.
    pub fn copy_within(&mut self, src: Range<usize>, dest: usize) {
        let count = src
            .end
            .checked_sub(src.start)
            .unwrap_or_else(|| panic!("RAM copy_within source {src:?} is inverted"));
        let dest_end = dest
            .checked_add(count)
            .unwrap_or_else(|| panic!("RAM copy_within destination {dest:#x} overflows"));
        self.check_backed(src.clone());
        self.check_backed(dest..dest_end);
        self.notify_hle_mutation(dest, count);
        // SAFETY: both ranges are backed; `copy` permits overlap.
        unsafe {
            std::ptr::copy(
                self.base.as_ptr().add(src.start),
                self.base.as_ptr().add(dest),
                count,
            );
        }
    }

    #[inline(always)]
    fn check_backed(&self, range: Range<usize>) {
        if !self.is_backed(range.clone()) {
            hole_range_fail(range.start, range.end, self.len);
        }
    }
}

#[cold]
#[inline(never)]
#[track_caller]
fn hole_range_fail(start: usize, end: usize, len: usize) -> ! {
    panic!(
        "guest RAM range {start:#x}..{end:#x} is not wholly host-backed \
         (private RAM ends at {len:#x})"
    )
}

/// Index kinds [`RamBacking`] accepts: a byte offset or a range of offsets.
/// Each resolves to `Ok((start, len))` only when the whole span is backed, and
/// to `Err((start, end))` (the requested span, for the panic message) otherwise.
pub trait RamIndex: sealed::Sealed {
    /// `u8` for an offset, `[u8]` for a range.
    type Output: ?Sized;
    #[doc(hidden)]
    fn checked_bounds(self, ram: &RamBacking) -> Result<(usize, usize), (usize, usize)>;
    #[doc(hidden)]
    /// # Safety
    /// `ptr..ptr + len` must be backed and live for `'a`.
    unsafe fn build<'a>(ptr: *const u8, len: usize) -> &'a Self::Output;
    #[doc(hidden)]
    /// # Safety
    /// As `build`, and no other reference may overlap it for `'a`.
    unsafe fn build_mut<'a>(ptr: *mut u8, len: usize) -> &'a mut Self::Output;
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for usize {}
    impl Sealed for std::ops::Range<usize> {}
    impl Sealed for std::ops::RangeInclusive<usize> {}
    impl Sealed for std::ops::RangeTo<usize> {}
    impl Sealed for std::ops::RangeToInclusive<usize> {}
    impl Sealed for std::ops::RangeFrom<usize> {}
}

impl RamIndex for usize {
    type Output = u8;
    #[inline(always)]
    fn checked_bounds(self, ram: &RamBacking) -> Result<(usize, usize), (usize, usize)> {
        // Private RAM: one compare, as before the arena existed.
        if self < ram.len
            || self
                .checked_add(1)
                .is_some_and(|end| ram.is_admitted_arena(self..end))
        {
            Ok((self, 1))
        } else {
            Err((self, self.wrapping_add(1)))
        }
    }
    #[inline(always)]
    unsafe fn build<'a>(ptr: *const u8, _len: usize) -> &'a u8 {
        // SAFETY: caller contract.
        unsafe { &*ptr }
    }
    #[inline(always)]
    unsafe fn build_mut<'a>(ptr: *mut u8, _len: usize) -> &'a mut u8 {
        // SAFETY: caller contract.
        unsafe { &mut *ptr }
    }
}

/// `$bounds` yields the requested `(start, end)`, or `Err` when it cannot be
/// formed (an inclusive end of `usize::MAX`, an unbacked `a..` start).
macro_rules! range_index {
    ($ty:ty, |$r:ident, $ram:ident| $bounds:expr) => {
        impl RamIndex for $ty {
            type Output = [u8];
            #[inline(always)]
            fn checked_bounds(self, $ram: &RamBacking) -> Result<(usize, usize), (usize, usize)> {
                let $r = self;
                let (start, end): (usize, usize) = $bounds?;
                if $ram.is_backed(start..end) {
                    Ok((start, end - start))
                } else {
                    Err((start, end))
                }
            }
            #[inline(always)]
            unsafe fn build<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
                // SAFETY: caller contract.
                unsafe { std::slice::from_raw_parts(ptr, len) }
            }
            #[inline(always)]
            unsafe fn build_mut<'a>(ptr: *mut u8, len: usize) -> &'a mut [u8] {
                // SAFETY: caller contract.
                unsafe { std::slice::from_raw_parts_mut(ptr, len) }
            }
        }
    };
}

range_index!(Range<usize>, |r, _ram| Ok::<_, (usize, usize)>((
    r.start, r.end
)));
range_index!(RangeInclusive<usize>, |r, _ram| {
    let (start, last) = r.into_inner();
    last.checked_add(1)
        .map(|end| (start, end))
        .ok_or((start, last))
});
range_index!(RangeTo<usize>, |r, _ram| Ok::<_, (usize, usize)>((
    0, r.end
)));
range_index!(RangeToInclusive<usize>, |r, _ram| {
    r.end.checked_add(1).map(|end| (0, end)).ok_or((0, r.end))
});
// `ram[a..]` runs to the end of the backed extent containing `a`, never across
// a hole.
range_index!(RangeFrom<usize>, |r, ram| ram
    .backed_end(r.start)
    .map(|end| (r.start, end))
    .ok_or((r.start, r.start)));

impl<I: RamIndex> Index<I> for RamBacking {
    type Output = I::Output;

    #[inline(always)]
    #[track_caller]
    fn index(&self, index: I) -> &I::Output {
        match index.checked_bounds(self) {
            // SAFETY: `checked_bounds` proved `[start, start + len)` backed.
            Ok((start, len)) => unsafe { I::build(self.base.as_ptr().add(start), len) },
            Err((start, end)) => hole_range_fail(start, end, self.len),
        }
    }
}

impl<I: RamIndex> IndexMut<I> for RamBacking {
    #[inline(always)]
    #[track_caller]
    fn index_mut(&mut self, index: I) -> &mut I::Output {
        match index.checked_bounds(self) {
            // SAFETY: as `index`, with exclusive access through `&mut self`.
            Ok((start, len)) => {
                self.notify_hle_mutation(start, len);
                unsafe { I::build_mut(self.base.as_ptr().add(start), len) }
            }
            Err((start, end)) => hole_range_fail(start, end, self.len),
        }
    }
}

/// Host page size, in bytes.
#[cfg(unix)]
pub fn host_page_size() -> usize {
    // SAFETY: sysconf has no preconditions.
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    usize::try_from(size)
        .ok()
        .filter(|s| *s > 0)
        .unwrap_or(4096)
}

/// Host page size, in bytes.
#[cfg(not(unix))]
pub fn host_page_size() -> usize {
    4096
}

#[cfg(target_pointer_width = "64")]
use reserved::Storage;

#[cfg(not(target_pointer_width = "64"))]
use vec_backed::Storage;

#[cfg(target_pointer_width = "64")]
mod reserved {
    use super::{host_page_size, os, RAM_RESERVATION_BYTES};
    use std::ptr::NonNull;

    /// RAM inside one fixed host reservation.
    ///
    /// `[0, committed)` is read/write, the rest of the reservation has no
    /// access. Bytes at or above `high_water` have never been part of the RAM
    /// and are still the OS's zero fill, so growth only has to clear
    /// `[len, high_water)`.
    pub(super) struct Storage {
        reservation: os::Reservation,
        capacity: usize,
        committed: usize,
        high_water: usize,
    }

    impl Storage {
        pub(super) fn zeroed(capacity: usize, len: usize) -> Self {
            assert!(
                capacity <= super::PRIVATE_WINDOW_BYTES,
                "RAM capacity {capacity:#x} exceeds the private window"
            );
            let mut storage = Self {
                reservation: os::Reservation::new(),
                capacity,
                committed: 0,
                high_water: len,
            };
            storage.commit(capacity);
            storage
        }

        pub(super) fn base(&mut self) -> NonNull<u8> {
            self.reservation.base()
        }

        pub(super) fn is_stable(&self) -> bool {
            true
        }

        pub(super) fn reservation_len(&self) -> usize {
            RAM_RESERVATION_BYTES
        }

        pub(super) fn map_arena(
            &mut self,
            store: &crate::arena::ArenaStore,
        ) -> std::io::Result<()> {
            self.reservation.map_arena(store)
        }

        pub(super) fn unmap_arena(&mut self) {
            self.reservation.unmap_arena();
        }

        /// Make `[0, bytes)` (rounded up to host pages) read/write.
        fn commit(&mut self, bytes: usize) {
            let page = host_page_size();
            let want = bytes.div_ceil(page) * page;
            if want <= self.committed {
                return;
            }
            assert!(
                want <= super::PRIVATE_WINDOW_BYTES,
                "committing guest RAM to {want:#x} bytes passes the private window"
            );
            self.reservation
                .commit(self.committed, want - self.committed);
            self.committed = want;
        }

        pub(super) fn resize(&mut self, old_len: usize, new_len: usize, value: u8) {
            if new_len <= old_len {
                return;
            }
            if new_len > self.capacity {
                // `RawVec::grow_amortized`: double, or exactly what is needed.
                let grown = self.capacity.saturating_mul(2).max(new_len).max(8);
                assert!(
                    new_len <= super::PRIVATE_WINDOW_BYTES,
                    "RAM growth to {new_len:#x} exceeds the private window"
                );
                self.capacity = grown.min(super::PRIVATE_WINDOW_BYTES);
                self.commit(self.capacity);
            }
            // Bytes past the high-water mark are still zero from the OS.
            let fill_end = if value == 0 {
                new_len.min(self.high_water)
            } else {
                new_len
            };
            if fill_end > old_len {
                // SAFETY: `[old_len, fill_end)` lies below `new_len <= capacity`,
                // which is committed read/write; the caller holds `&mut`.
                unsafe {
                    std::ptr::write_bytes(
                        self.base().as_ptr().add(old_len),
                        value,
                        fill_end - old_len,
                    );
                }
            }
            self.high_water = self.high_water.max(new_len);
        }

        pub(super) fn capacity(&self) -> usize {
            self.capacity
        }
    }
}

/// OS reservation primitives for the fixed RAM reservation.
#[cfg(all(unix, target_pointer_width = "64"))]
mod os {
    use super::RAM_RESERVATION_BYTES;
    use crate::arena::{ArenaStore, ARENA_PHYS_START, ARENA_STORE_BYTES};
    use std::ptr::NonNull;

    /// `RAM_RESERVATION_BYTES` of `PROT_NONE` address space.
    pub(super) struct Reservation {
        base: NonNull<u8>,
    }

    impl Reservation {
        pub(super) fn new() -> Self {
            // SAFETY: an anonymous no-access mapping at a kernel-chosen address.
            let raw = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    RAM_RESERVATION_BYTES,
                    libc::PROT_NONE,
                    libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_NORESERVE,
                    -1,
                    0,
                )
            };
            assert!(
                raw != libc::MAP_FAILED,
                "reserving {RAM_RESERVATION_BYTES:#x} bytes for guest RAM failed: {}",
                std::io::Error::last_os_error()
            );
            Self {
                base: NonNull::new(raw.cast::<u8>()).expect("mmap returned null"),
            }
        }

        pub(super) fn base(&self) -> NonNull<u8> {
            self.base
        }

        pub(super) fn map_arena(&mut self, store: &ArenaStore) -> std::io::Result<()> {
            // SAFETY: MAP_FIXED replaces only this reservation's arena part.
            let target = unsafe { self.base.as_ptr().add(ARENA_PHYS_START) };
            let mapped = unsafe {
                libc::mmap(
                    target.cast(),
                    ARENA_STORE_BYTES,
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

        pub(super) fn unmap_arena(&mut self) {
            // Replace the section view with a no-access anonymous reservation
            // at the same address, preserving the full stable pointer span.
            let target = unsafe { self.base.as_ptr().add(ARENA_PHYS_START) };
            let mapped = unsafe {
                libc::mmap(
                    target.cast(),
                    ARENA_STORE_BYTES,
                    libc::PROT_NONE,
                    libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_FIXED | libc::MAP_NORESERVE,
                    -1,
                    0,
                )
            };
            assert_eq!(
                mapped,
                target.cast(),
                "restoring arena guard failed: {}",
                std::io::Error::last_os_error()
            );
        }

        /// Make `[offset, offset + len)` read/write (page-aligned, inside the
        /// private window).
        pub(super) fn commit(&mut self, offset: usize, len: usize) {
            // SAFETY: the range lies inside our own reservation (checked by the
            // caller against the private window).
            let rc = unsafe {
                libc::mprotect(
                    self.base.as_ptr().add(offset).cast(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE,
                )
            };
            assert!(
                rc == 0,
                "committing guest RAM [{offset:#x}, +{len:#x}) failed: {}",
                std::io::Error::last_os_error()
            );
        }
    }

    impl Drop for Reservation {
        fn drop(&mut self) {
            // SAFETY: unmaps exactly the reservation `new` created.
            unsafe {
                libc::munmap(self.base.as_ptr().cast(), RAM_RESERVATION_BYTES);
            }
        }
    }
}

#[cfg(all(windows, target_pointer_width = "64"))]
mod os {
    use super::{PRIVATE_WINDOW_BYTES, RAM_RESERVATION_BYTES};
    use crate::arena::{ArenaStore, ARENA_PHYS_END, ARENA_PHYS_START, ARENA_STORE_BYTES};
    use std::ptr::NonNull;
    use windows_sys::Win32::System::Memory::{
        VirtualAlloc, VirtualAlloc2, VirtualFree, MEM_COMMIT, MEM_RELEASE, MEM_REPLACE_PLACEHOLDER,
        MEM_RESERVE, MEM_RESERVE_PLACEHOLDER, PAGE_NOACCESS, PAGE_READWRITE,
    };

    /// `VirtualFree` flag that splits a placeholder instead of releasing it
    /// (`MEM_PRESERVE_PLACEHOLDER`; windows-sys types it for `UnmapViewOfFile2`).
    const PRESERVE_SPLIT: u32 = 0x0000_0002;

    /// `RAM_RESERVATION_BYTES` of address space in two pieces: the private
    /// window `[0, PRIVATE_WINDOW_BYTES)` as an ordinary reservation that RAM
    /// commits into, and the rest as a placeholder that U3's arena section view
    /// replaces. Needs Windows 10 1803+ (HLD D7).
    pub(super) struct Reservation {
        base: NonNull<u8>,
        arena_split: bool,
        arena_mapped: bool,
    }

    impl Reservation {
        pub(super) fn new() -> Self {
            // SAFETY: a no-access placeholder reservation at an OS-chosen
            // address in this process (null process handle = the caller).
            let raw = unsafe {
                VirtualAlloc2(
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    RAM_RESERVATION_BYTES,
                    MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
                    PAGE_NOACCESS,
                    std::ptr::null_mut(),
                    0,
                )
            };
            let base = NonNull::new(raw.cast::<u8>()).unwrap_or_else(|| {
                panic!(
                    "reserving {RAM_RESERVATION_BYTES:#x} bytes for guest RAM failed \
                     (VirtualAlloc2 needs Windows 10 1803+): {}",
                    std::io::Error::last_os_error()
                )
            });
            // Split the private window off the placeholder...
            // SAFETY: `[base, base + PRIVATE_WINDOW_BYTES)` is the head of the
            // placeholder just created; PRESERVE splits rather than frees.
            let split = unsafe {
                VirtualFree(
                    base.as_ptr().cast(),
                    PRIVATE_WINDOW_BYTES,
                    MEM_RELEASE | PRESERVE_SPLIT,
                )
            };
            assert!(
                split != 0,
                "splitting the guest RAM placeholder failed: {}",
                std::io::Error::last_os_error()
            );
            // ...and turn it into an ordinary no-access reservation.
            // SAFETY: replaces exactly the placeholder the split produced.
            let private = unsafe {
                VirtualAlloc2(
                    std::ptr::null_mut(),
                    base.as_ptr().cast_const().cast(),
                    PRIVATE_WINDOW_BYTES,
                    MEM_RESERVE | MEM_REPLACE_PLACEHOLDER,
                    PAGE_NOACCESS,
                    std::ptr::null_mut(),
                    0,
                )
            };
            assert!(
                private == base.as_ptr().cast(),
                "reserving the guest RAM private window failed: {}",
                std::io::Error::last_os_error()
            );
            Self {
                base,
                arena_split: false,
                arena_mapped: false,
            }
        }

        pub(super) fn base(&self) -> NonNull<u8> {
            self.base
        }

        pub(super) fn map_arena(&mut self, store: &ArenaStore) -> std::io::Result<()> {
            use windows_sys::Win32::System::Memory::MapViewOfFile3;
            assert!(!self.arena_mapped);
            let target = unsafe { self.base.as_ptr().add(ARENA_PHYS_START) };
            if !self.arena_split {
                // Split off exactly the section extent, leaving the rest of
                // the 4 GiB reservation protected as a placeholder.
                let ok = unsafe {
                    VirtualFree(
                        target.cast(),
                        ARENA_STORE_BYTES,
                        MEM_RELEASE | PRESERVE_SPLIT,
                    )
                };
                if ok == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                self.arena_split = true;
            }
            let view = unsafe {
                MapViewOfFile3(
                    store.handle(),
                    std::ptr::null_mut(),
                    target.cast(),
                    0,
                    ARENA_STORE_BYTES,
                    MEM_REPLACE_PLACEHOLDER,
                    PAGE_READWRITE,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if view.Value != target.cast() {
                return Err(std::io::Error::last_os_error());
            }
            self.arena_mapped = true;
            Ok(())
        }

        pub(super) fn unmap_arena(&mut self) {
            use windows_sys::Win32::System::Memory::{
                UnmapViewOfFile2, MEMORY_MAPPED_VIEW_ADDRESS, MEM_PRESERVE_PLACEHOLDER,
            };
            if !self.arena_mapped {
                return;
            }
            let target = unsafe { self.base.as_ptr().add(ARENA_PHYS_START) };
            let ok = unsafe {
                UnmapViewOfFile2(
                    -1isize as _,
                    MEMORY_MAPPED_VIEW_ADDRESS {
                        Value: target.cast(),
                    },
                    MEM_PRESERVE_PLACEHOLDER,
                )
            };
            assert_ne!(
                ok,
                0,
                "restoring arena placeholder failed: {}",
                std::io::Error::last_os_error()
            );
            self.arena_mapped = false;
        }

        /// Commit `[offset, offset + len)` read/write (inside the private
        /// window). Fresh commits read as zero.
        pub(super) fn commit(&mut self, offset: usize, len: usize) {
            // SAFETY: the range lies inside the private window reservation
            // (checked by the caller).
            let raw = unsafe {
                VirtualAlloc(
                    self.base.as_ptr().add(offset).cast_const().cast(),
                    len,
                    MEM_COMMIT,
                    PAGE_READWRITE,
                )
            };
            assert!(
                !raw.is_null(),
                "committing guest RAM [{offset:#x}, +{len:#x}) failed: {}",
                std::io::Error::last_os_error()
            );
        }
    }

    impl Drop for Reservation {
        fn drop(&mut self) {
            self.unmap_arena();
            // SAFETY: releases the two allocations `new` made: the private
            // window reservation and the remaining placeholder.
            unsafe {
                VirtualFree(self.base.as_ptr().cast(), 0, MEM_RELEASE);
                VirtualFree(
                    self.base.as_ptr().add(PRIVATE_WINDOW_BYTES).cast(),
                    0,
                    MEM_RELEASE,
                );
                if self.arena_split {
                    VirtualFree(
                        self.base.as_ptr().add(ARENA_PHYS_END).cast(),
                        0,
                        MEM_RELEASE,
                    );
                }
            }
        }
    }
}

#[cfg(not(target_pointer_width = "64"))]
mod vec_backed {
    use std::ptr::NonNull;

    /// RAM in a growable `Vec`. Growth past the capacity may move it, so these
    /// hosts report no stable reservation and never offer fastmem.
    pub(super) struct Storage(Vec<u8>);

    impl Storage {
        pub(super) fn zeroed(capacity: usize, len: usize) -> Self {
            // `vec![0; N]` then `truncate`: the allocator hands back zero pages
            // and the capacity stays pinned (see `Memory::new`).
            let mut ram = vec![0u8; capacity];
            ram.truncate(len);
            Self(ram)
        }

        pub(super) fn base(&mut self) -> NonNull<u8> {
            NonNull::new(self.0.as_mut_ptr()).expect("Vec pointer is non-null")
        }

        pub(super) fn is_stable(&self) -> bool {
            false
        }

        pub(super) fn reservation_len(&self) -> usize {
            self.0.capacity()
        }

        pub(super) fn map_arena(
            &mut self,
            _store: &crate::arena::ArenaStore,
        ) -> std::io::Result<()> {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "shared arena needs a 64-bit host",
            ))
        }

        pub(super) fn unmap_arena(&mut self) {}

        pub(super) fn resize(&mut self, _old_len: usize, new_len: usize, value: u8) {
            self.0.resize(new_len, value);
        }

        pub(super) fn capacity(&self) -> usize {
            self.0.capacity()
        }
    }
}

#[cfg(test)]
mod tests;
