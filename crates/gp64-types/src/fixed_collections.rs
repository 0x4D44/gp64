//! Fixed-capacity collections for snapshot-safe emulator state.
//!
//! These types use `#[repr(C)]` for deterministic layout and avoid heap allocation,
//! making them suitable for memory-mapped snapshots and binary serialization.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::mem::MaybeUninit;
use std::ops::{Deref, DerefMut, Index, IndexMut};
use std::ptr;

// =============================================================================
// FixedVec<T, N>
// =============================================================================

/// A fixed-capacity vector stored inline. No heap allocation.
///
/// Behaves like a `Vec<T>` but with a compile-time maximum capacity of `N`.
/// Panics on push when full rather than reallocating.
#[repr(C)]
pub struct FixedVec<T: Clone, const N: usize> {
    len: u32,
    data: [MaybeUninit<T>; N],
}

impl<T: Clone, const N: usize> FixedVec<T, N> {
    /// Create a new empty `FixedVec`.
    pub fn new() -> Self {
        const { assert!(N > 0, "FixedVec capacity must be non-zero") };
        Self {
            len: 0,
            // SAFETY: An array of MaybeUninit does not require initialization.
            data: unsafe { MaybeUninit::uninit().assume_init() },
        }
    }

    /// Create a new empty `FixedVec` directly on the heap.
    ///
    /// This avoids materializing very large inline buffers on the caller's
    /// stack before moving them into a `Box`.
    pub fn new_boxed() -> Box<Self> {
        const { assert!(N > 0, "FixedVec capacity must be non-zero") };
        let mut boxed = Box::<Self>::new_uninit();
        let ptr = boxed.as_mut_ptr();
        unsafe {
            std::ptr::addr_of_mut!((*ptr).len).write(0);
            // `data` is `[MaybeUninit<T>; N]`, so uninitialized bytes are a
            // valid representation for every slot while `len == 0`.
            boxed.assume_init()
        }
    }

    /// Returns the number of elements currently stored.
    #[inline]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Returns true if the vector contains no elements.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns true if the vector is at maximum capacity.
    #[inline]
    pub fn is_full(&self) -> bool {
        self.len as usize == N
    }

    /// Returns the maximum number of elements this vector can hold.
    #[inline]
    pub fn capacity(&self) -> usize {
        N
    }

    /// Appends an element to the back. Panics if full.
    pub fn push(&mut self, val: T) {
        assert!(
            (self.len as usize) < N,
            "FixedVec::push: capacity {} exceeded",
            N
        );
        self.data[self.len as usize] = MaybeUninit::new(val);
        self.len += 1;
    }

    /// Removes and returns the last element, or `None` if empty.
    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        // SAFETY: Element at `self.len` was initialized (it was the last valid element).
        let val = unsafe { self.data[self.len as usize].assume_init_read() };
        Some(val)
    }

    /// Removes all elements, dropping each one.
    pub fn clear(&mut self) {
        self.truncate(0);
    }

    /// Shortens the vector to `new_len`, dropping removed elements.
    /// Does nothing if `new_len >= self.len()`.
    pub fn truncate(&mut self, new_len: usize) {
        if new_len >= self.len as usize {
            return;
        }
        let old_len = self.len as usize;
        self.len = new_len as u32; // Set len first -- if a Drop panics, we don't double-free
                                   // MDD-BUG-CRUCIBLE-00285: drop the removed range THROUGH A SLICE, not in
                                   // a manual loop. A manual loop abandons every element after a panicking
                                   // destructor -- `len` has already been shortened, so nothing owns the
                                   // suffix and it leaks. Slice drop glue keeps dropping the remainder when
                                   // one element unwinds (a second panic aborts, which is the same
                                   // guarantee `Vec::truncate` gives).
                                   // SAFETY: slots new_len..old_len were initialized, are no longer covered
                                   // by `len`, and `MaybeUninit<T>` has the same layout as `T`.
        unsafe {
            let first = self.data.as_mut_ptr().add(new_len).cast::<T>();
            ptr::drop_in_place(ptr::slice_from_raw_parts_mut(first, old_len - new_len));
        }
    }

    /// Retains only elements for which the predicate returns true.
    pub fn retain(&mut self, mut f: impl FnMut(&T) -> bool) {
        let original_len = self.len as usize;
        self.len = 0; // Prevent double-free if f panics
                      // MDD-BUG-CRUCIBLE-00285: a panic from `f` or from a rejected element's
                      // destructor used to abandon everything -- the kept prefix, the element
                      // in flight, and the whole unvisited tail -- because `len` was zero and
                      // no code ran on the unwind path. The guard below backshifts the
                      // survivors over the hole and republishes `len`, the same recovery
                      // `Vec::retain` performs, so a caller that catches the unwind sees a
                      // consistent vector rather than a leak.
        struct BackshiftOnDrop<'a, T: Clone, const N: usize> {
            vec: &'a mut FixedVec<T, N>,
            read: usize,
            write: usize,
            original_len: usize,
        }
        impl<T: Clone, const N: usize> Drop for BackshiftOnDrop<'_, T, N> {
            fn drop(&mut self) {
                // `read` counts slots FULLY processed, so slots read..original_len
                // are always still live here. That is what makes one guard cover
                // both unwind sites: the predicate panics before `read` advances,
                // so its element survives; a rejected element's destructor panics
                // after `read` advances, so its slot is correctly excluded.
                let tail = self.original_len - self.read;
                if tail > 0 && self.write != self.read {
                    // SAFETY: both ranges lie inside the array, the source slots
                    // are initialized, and write < read here, so the regions may
                    // overlap -- hence `copy`, not `copy_nonoverlapping`.
                    unsafe {
                        let base = self.vec.data.as_mut_ptr().cast::<T>();
                        ptr::copy(base.add(self.read), base.add(self.write), tail);
                    }
                }
                self.vec.len = (self.write + tail) as u32;
            }
        }

        let mut guard = BackshiftOnDrop {
            vec: self,
            read: 0,
            write: 0,
            original_len,
        };
        while guard.read < original_len {
            let read = guard.read;
            // SAFETY: read < original_len, all slots 0..original_len were initialized
            let keep = unsafe { f(guard.vec.data[read].assume_init_ref()) };
            if keep {
                let write = guard.write;
                if write != read {
                    // SAFETY: read slot is initialized; move it to write position.
                    // A move is a memcpy and cannot unwind.
                    unsafe {
                        let val = guard.vec.data[read].assume_init_read();
                        guard.vec.data[write] = MaybeUninit::new(val);
                    }
                }
                guard.write += 1;
                guard.read += 1;
            } else {
                // Mark the slot consumed BEFORE running its destructor, so a
                // panicking `Drop` does not leave the guard trying to keep an
                // element that has already been dropped.
                guard.read += 1;
                // SAFETY: read slot is initialized; drop it
                unsafe {
                    guard.vec.data[read].assume_init_drop();
                }
            }
        }
        // Ran to completion: publish the final length and defuse the guard by
        // making its backshift a no-op.
        guard.vec.len = guard.write as u32;
        core::mem::forget(guard);
    }

    /// Appends all elements from a slice. Panics if there is not enough capacity.
    pub fn extend_from_slice(&mut self, slice: &[T]) {
        assert!(
            self.len as usize + slice.len() <= N,
            "FixedVec::extend_from_slice: would exceed capacity {}",
            N
        );
        for item in slice {
            self.data[self.len as usize] = MaybeUninit::new(item.clone());
            self.len += 1;
        }
    }

    /// Inserts an element at the given index, shifting subsequent elements right.
    /// Panics if full or if `index > len()`.
    pub fn insert(&mut self, index: usize, val: T) {
        let len = self.len as usize;
        assert!(
            index <= len,
            "FixedVec::insert: index {} out of bounds",
            index
        );
        assert!(len < N, "FixedVec::insert: capacity {} exceeded", N);

        // Shift elements [index..len) right by one.
        // SAFETY: All elements in [index..len) are initialized. We shift them into
        // [index+1..len+1), which is within the array bounds (len < N).
        unsafe {
            let ptr = self.data.as_mut_ptr().add(index);
            std::ptr::copy(ptr, ptr.add(1), len - index);
        }
        self.data[index] = MaybeUninit::new(val);
        self.len += 1;
    }

    /// Removes and returns the element at the given index, shifting subsequent
    /// elements left. Panics if `index >= len()`.
    pub fn remove(&mut self, index: usize) -> T {
        let len = self.len as usize;
        assert!(
            index < len,
            "FixedVec::remove: index {} out of bounds",
            index
        );

        // SAFETY: Element at `index` is initialized.
        let val = unsafe { self.data[index].assume_init_read() };

        // Shift elements [index+1..len) left by one.
        // SAFETY: All elements in [index+1..len) are initialized. We shift them
        // into [index..len-1).
        unsafe {
            let ptr = self.data.as_mut_ptr().add(index);
            std::ptr::copy(ptr.add(1), ptr, len - index - 1);
        }
        self.len -= 1;
        val
    }

    /// Removes the element at `index` by swapping it with the last element.
    /// O(1) but does not preserve order. Panics if `index >= len()`.
    pub fn swap_remove(&mut self, index: usize) -> T {
        let len = self.len as usize;
        assert!(
            index < len,
            "FixedVec::swap_remove: index {} out of bounds",
            index
        );

        self.len -= 1;
        if index == self.len as usize {
            // Removing the last element — just read it.
            // SAFETY: Element at `index` is initialized.
            unsafe { self.data[index].assume_init_read() }
        } else {
            // SAFETY: Both `index` and `self.len` (the old last element) are initialized.
            // We read both, then write the last element into the index slot.
            unsafe {
                let removed = self.data[index].assume_init_read();
                let last = self.data[self.len as usize].assume_init_read();
                self.data[index] = MaybeUninit::new(last);
                removed
            }
        }
    }
}

impl<T: Clone, const N: usize> Clone for FixedVec<T, N> {
    fn clone(&self) -> Self {
        let mut new = Self::new();
        for i in 0..self.len as usize {
            // SAFETY: Elements in [0..self.len) are initialized.
            let val = unsafe { self.data[i].assume_init_ref() };
            new.data[i] = MaybeUninit::new(val.clone());
            // MDD-BUG-CRUCIBLE-00285: publish each clone as it lands. Setting
            // `len` only after the whole loop meant a panicking `Clone` left
            // `new` claiming zero elements, so its own `Drop` skipped every
            // clone made so far and they all leaked.
            new.len = (i + 1) as u32;
        }
        new
    }
}

impl<T: Clone, const N: usize> Drop for FixedVec<T, N> {
    fn drop(&mut self) {
        // MDD-BUG-CRUCIBLE-00285: drop through a slice so a panicking element
        // destructor still drops the rest (see `truncate`).
        // SAFETY: slots 0..len are initialized and `MaybeUninit<T>` shares T's
        // layout.
        unsafe {
            let first = self.data.as_mut_ptr().cast::<T>();
            ptr::drop_in_place(ptr::slice_from_raw_parts_mut(first, self.len as usize));
        }
    }
}

impl<T: Clone, const N: usize> Default for FixedVec<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone, const N: usize> Deref for FixedVec<T, N> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        // SAFETY: Elements in [0..self.len) are initialized. MaybeUninit<T> has
        // the same layout as T, so we can safely reinterpret the slice.
        unsafe { std::slice::from_raw_parts(self.data.as_ptr() as *const T, self.len as usize) }
    }
}

impl<T: Clone, const N: usize> DerefMut for FixedVec<T, N> {
    fn deref_mut(&mut self) -> &mut [T] {
        // SAFETY: Elements in [0..self.len) are initialized. MaybeUninit<T> has
        // the same layout as T, so we can safely reinterpret the slice.
        unsafe {
            std::slice::from_raw_parts_mut(self.data.as_mut_ptr() as *mut T, self.len as usize)
        }
    }
}

impl<T: Clone, const N: usize> Index<usize> for FixedVec<T, N> {
    type Output = T;

    fn index(&self, index: usize) -> &T {
        &(**self)[index]
    }
}

impl<T: Clone, const N: usize> IndexMut<usize> for FixedVec<T, N> {
    fn index_mut(&mut self, index: usize) -> &mut T {
        &mut (**self)[index]
    }
}

impl<T: Clone + fmt::Debug, const N: usize> fmt::Debug for FixedVec<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<T: Clone + PartialEq, const N: usize> PartialEq for FixedVec<T, N> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl<T: Clone + Eq, const N: usize> Eq for FixedVec<T, N> {}

impl<'a, T: Clone, const N: usize> IntoIterator for &'a FixedVec<T, N> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, T: Clone, const N: usize> IntoIterator for &'a mut FixedVec<T, N> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<T: Clone, const N: usize> FromIterator<T> for FixedVec<T, N> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut v = Self::new();
        for item in iter {
            v.push(item);
        }
        v
    }
}

// =============================================================================
// FixedString<N>
// =============================================================================

/// A fixed-capacity UTF-8 string stored inline. No heap allocation.
///
/// The backing buffer is `N` bytes; the actual string length is stored in `len`.
/// All constructors maintain the UTF-8 invariant.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct FixedString<const N: usize> {
    len: u32,
    data: [u8; N],
}

impl<const N: usize> FixedString<N> {
    /// Create a new empty `FixedString`.
    pub fn new() -> Self {
        Self {
            len: 0,
            data: [0u8; N],
        }
    }

    /// Returns the length of the string in bytes.
    #[inline]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Returns true if the string is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the string as a `&str`.
    #[inline]
    pub fn as_str(&self) -> &str {
        // SAFETY: We maintain the UTF-8 invariant in all constructors.
        unsafe { std::str::from_utf8_unchecked(&self.data[..self.len as usize]) }
    }

    /// Returns the string content as a byte slice.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }

    /// Returns the maximum number of bytes this string can hold.
    #[inline]
    pub fn capacity(&self) -> usize {
        N
    }

    /// Append a string slice. Truncates at capacity on UTF-8 char boundary.
    /// Returns the number of bytes actually appended.
    pub fn push_str(&mut self, s: &str) -> usize {
        let remaining = N - self.len as usize;
        let bytes = s.as_bytes();
        let mut take = bytes.len().min(remaining);
        // Don't split a multi-byte UTF-8 character
        while take > 0 && !s.is_char_boundary(take) {
            take -= 1;
        }
        self.data[self.len as usize..self.len as usize + take].copy_from_slice(&bytes[..take]);
        self.len += take as u32;
        take
    }

    /// Append a single character. Returns true if it fit, false if truncated.
    pub fn push(&mut self, ch: char) -> bool {
        let mut buf = [0u8; 4];
        let s = ch.encode_utf8(&mut buf);
        let written = self.push_str(s);
        written == s.len()
    }

    /// Clear the string to empty.
    pub fn clear(&mut self) {
        self.len = 0;
    }
}

impl<const N: usize> Default for FixedString<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> From<&str> for FixedString<N> {
    fn from(s: &str) -> Self {
        let bytes = s.as_bytes();
        let mut truncated_len = bytes.len().min(N);

        // Don't cut mid-codepoint: walk backwards to find a valid UTF-8 boundary.
        while truncated_len > 0 && !s.is_char_boundary(truncated_len) {
            truncated_len -= 1;
        }

        let mut data = [0u8; N];
        data[..truncated_len].copy_from_slice(&bytes[..truncated_len]);
        Self {
            len: truncated_len as u32,
            data,
        }
    }
}

impl<const N: usize> From<String> for FixedString<N> {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

impl<const N: usize> Deref for FixedString<N> {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl<const N: usize> fmt::Display for FixedString<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<const N: usize> fmt::Debug for FixedString<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FixedString({:?})", self.as_str())
    }
}

impl<const N: usize> PartialEq for FixedString<N> {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl<const N: usize> Eq for FixedString<N> {}

impl<const N: usize> PartialEq<str> for FixedString<N> {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl<const N: usize> PartialEq<&str> for FixedString<N> {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl<const N: usize> PartialEq<String> for FixedString<N> {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl<const N: usize> PartialOrd for FixedString<N> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<const N: usize> Ord for FixedString<N> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl<const N: usize> Hash for FixedString<N> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

// =============================================================================
// FixedRing<T, N>
// =============================================================================

/// A fixed-capacity ring buffer (FIFO queue) stored inline. No heap allocation.
///
/// Elements are pushed at the back and popped from the front.
/// Panics on push when full rather than overwriting.
#[repr(C)]
pub struct FixedRing<T: Clone, const N: usize> {
    head: u32,
    len: u32,
    data: [MaybeUninit<T>; N],
}

impl<T: Clone, const N: usize> FixedRing<T, N> {
    /// Create a new empty ring buffer.
    pub fn new() -> Self {
        const { assert!(N > 0, "FixedRing capacity must be non-zero") };
        Self {
            head: 0,
            len: 0,
            // SAFETY: An array of MaybeUninit does not require initialization.
            data: unsafe { MaybeUninit::uninit().assume_init() },
        }
    }

    /// Create a new empty ring buffer directly on the heap.
    ///
    /// `Box::new(Self::new())` builds the whole `N`-element array on the
    /// stack first, and an unoptimised build keeps a copy in each frame it
    /// passes through - a 256 KiB ring costs ~790 KiB of stack and pushed
    /// emulator construction past a 2 MiB thread (MDD-BUG-CRU-01645). This
    /// writes only the two cursor words; the storage needs no initialisation.
    pub fn new_boxed() -> Box<Self> {
        const { assert!(N > 0, "FixedRing capacity must be non-zero") };
        let mut ring = Box::<Self>::new_uninit();
        let ptr = ring.as_mut_ptr();
        // SAFETY: `ptr` is a valid, uniquely owned allocation for `Self`. Both
        // cursor fields are written before `assume_init`, and `data` is
        // `[MaybeUninit<T>; N]`, which is valid uninitialised.
        unsafe {
            std::ptr::addr_of_mut!((*ptr).head).write(0);
            std::ptr::addr_of_mut!((*ptr).len).write(0);
            ring.assume_init()
        }
    }

    /// Returns the number of elements currently stored.
    #[inline]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Returns true if the ring buffer contains no elements.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns true if the ring buffer is at maximum capacity.
    #[inline]
    pub fn is_full(&self) -> bool {
        self.len as usize == N
    }

    /// Returns the maximum number of elements this ring buffer can hold.
    #[inline]
    pub fn capacity(&self) -> usize {
        N
    }

    /// Appends an element to the back. Panics if full.
    ///
    /// Use [`Self::try_push_back`] when modeling a hardware FIFO that real
    /// silicon drops on overflow (PS/2 controller, UART rx, etc.) — those
    /// callers should not panic.
    #[track_caller]
    pub fn push_back(&mut self, val: T) {
        assert!(
            (self.len as usize) < N,
            "FixedRing::push_back: capacity {} exceeded",
            N
        );
        let idx = (self.head as usize + self.len as usize) % N;
        self.data[idx] = MaybeUninit::new(val);
        self.len += 1;
    }

    /// Appends an element to the back. Returns `Err(val)` if the ring is full.
    ///
    /// Use this in producer code modeling a hardware FIFO that legitimately
    /// overflows when the consumer falls behind (e.g., PS/2 mouse output
    /// queue, UART rx FIFO). Real silicon drops the byte and sets an overrun
    /// flag; callers that don't model overrun can write
    /// `let _ = ring.try_push_back(val);`.
    #[inline]
    pub fn try_push_back(&mut self, val: T) -> Result<(), T> {
        if self.is_full() {
            Err(val)
        } else {
            self.push_back(val);
            Ok(())
        }
    }

    /// Removes and returns the front element, or `None` if empty.
    pub fn pop_front(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        let idx = self.head as usize;
        // SAFETY: Element at `head` is initialized (len > 0).
        let val = unsafe { self.data[idx].assume_init_read() };
        self.head = ((self.head as usize + 1) % N) as u32;
        self.len -= 1;
        Some(val)
    }

    /// Returns a reference to the front element, or `None` if empty.
    pub fn front(&self) -> Option<&T> {
        if self.len == 0 {
            return None;
        }
        // SAFETY: Element at `head` is initialized (len > 0).
        Some(unsafe { self.data[self.head as usize].assume_init_ref() })
    }

    /// Returns a reference to the back element, or `None` if empty.
    pub fn back(&self) -> Option<&T> {
        if self.len == 0 {
            return None;
        }
        let idx = (self.head as usize + self.len as usize - 1) % N;
        // SAFETY: The back element is initialized (len > 0).
        Some(unsafe { self.data[idx].assume_init_ref() })
    }

    /// Removes all elements, dropping each one.
    pub fn clear(&mut self) {
        for value in self.drain() {
            drop(value);
        }
    }

    /// Returns an iterator over the elements in FIFO order.
    pub fn iter(&self) -> FixedRingIter<'_, T, N> {
        FixedRingIter {
            ring: self,
            offset: 0,
        }
    }

    /// Remove and return all elements as an iterator in FIFO order.
    pub fn drain(&mut self) -> FixedRingDrain<'_, T, N> {
        let len = self.len as usize;
        let head = self.head as usize;
        self.len = 0;
        self.head = 0;
        FixedRingDrain {
            ring_data: &mut self.data,
            head,
            remaining: len,
        }
    }
}

impl<T: Clone, const N: usize> Clone for FixedRing<T, N> {
    fn clone(&self) -> Self {
        let mut new = Self::new();
        new.head = 0;
        for i in 0..self.len as usize {
            let idx = (self.head as usize + i) % N;
            // SAFETY: All elements at valid ring positions are initialized.
            let val = unsafe { self.data[idx].assume_init_ref() };
            new.data[i] = MaybeUninit::new(val.clone());
            // MDD-BUG-CRUCIBLE-00285: publish as we go, as in `FixedVec::clone`.
            new.len = (i + 1) as u32;
        }
        new
    }
}

/// Owns one contiguous run of initialized slots and drops it when it goes out
/// of scope — including when the *other* run's drop is unwinding.
///
/// MDD-BUG-CRUCIBLE-00285: dropping the two runs as two consecutive statements
/// is not enough. A panic in the first statement skips the second, so the
/// wrapped remainder leaks exactly as it did with the original per-index loop.
struct DropRun<T> {
    first: *mut T,
    len: usize,
}

impl<T> Drop for DropRun<T> {
    fn drop(&mut self) {
        // SAFETY: the constructor is the only caller and it is given a run of
        // initialized slots that nothing else will drop.
        unsafe {
            ptr::drop_in_place(ptr::slice_from_raw_parts_mut(self.first, self.len));
        }
    }
}

impl<T: Clone, const N: usize> Drop for FixedRing<T, N> {
    fn drop(&mut self) {
        // MDD-BUG-CRUCIBLE-00285: a live ring is at most two contiguous runs, so
        // drop each as a slice. The old per-index loop abandoned every remaining
        // slot -- including the wrapped ones -- once a destructor panicked.
        let head = self.head as usize;
        let len = self.len as usize;
        let first_run = len.min(N - head);
        // SAFETY: slots head..head+first_run and 0..len-first_run are exactly the
        // initialized ring positions, and `MaybeUninit<T>` shares T's layout.
        unsafe {
            let base = self.data.as_mut_ptr().cast::<T>();
            // The wrapped run is dropped by this guard whether the run below
            // finishes normally or unwinds.
            let _wrapped = DropRun {
                first: base,
                len: len - first_run,
            };
            ptr::drop_in_place(ptr::slice_from_raw_parts_mut(base.add(head), first_run));
        }
    }
}

impl<T: Clone, const N: usize> Default for FixedRing<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone + fmt::Debug, const N: usize> fmt::Debug for FixedRing<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<T: Clone + PartialEq, const N: usize> PartialEq for FixedRing<T, N> {
    fn eq(&self, other: &Self) -> bool {
        if self.len != other.len {
            return false;
        }
        self.iter().zip(other.iter()).all(|(a, b)| a == b)
    }
}

impl<T: Clone + Eq, const N: usize> Eq for FixedRing<T, N> {}

impl<T: Clone, const N: usize> Index<usize> for FixedRing<T, N> {
    type Output = T;

    fn index(&self, index: usize) -> &T {
        assert!(index < self.len as usize, "FixedRing index out of bounds");
        let physical = (self.head as usize + index) % N;
        // SAFETY: index < len, so this slot is initialized
        unsafe { self.data[physical].assume_init_ref() }
    }
}

impl<'a, T: Clone, const N: usize> IntoIterator for &'a FixedRing<T, N> {
    type Item = &'a T;
    type IntoIter = FixedRingIter<'a, T, N>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over `FixedRing` elements in FIFO order.
pub struct FixedRingIter<'a, T: Clone, const N: usize> {
    ring: &'a FixedRing<T, N>,
    offset: usize,
}

impl<'a, T: Clone, const N: usize> Iterator for FixedRingIter<'a, T, N> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset >= self.ring.len as usize {
            return None;
        }
        let idx = (self.ring.head as usize + self.offset) % N;
        self.offset += 1;
        // SAFETY: Element at this ring position is initialized (offset < len).
        Some(unsafe { self.ring.data[idx].assume_init_ref() })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.ring.len as usize - self.offset;
        (remaining, Some(remaining))
    }
}

impl<T: Clone, const N: usize> ExactSizeIterator for FixedRingIter<'_, T, N> {}

/// Draining iterator over `FixedRing` elements in FIFO order.
/// Consumes elements from the ring buffer.
pub struct FixedRingDrain<'a, T: Clone, const N: usize> {
    ring_data: &'a mut [MaybeUninit<T>; N],
    head: usize,
    remaining: usize,
}

impl<'a, T: Clone, const N: usize> Iterator for FixedRingDrain<'a, T, N> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        if self.remaining == 0 {
            return None;
        }
        // SAFETY: head points to an initialized slot (remaining > 0)
        let val = unsafe { self.ring_data[self.head].assume_init_read() };
        self.head = (self.head + 1) % N;
        self.remaining -= 1;
        Some(val)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<T: Clone, const N: usize> ExactSizeIterator for FixedRingDrain<'_, T, N> {}

impl<T: Clone, const N: usize> Drop for FixedRingDrain<'_, T, N> {
    fn drop(&mut self) {
        // MDD-BUG-CRUCIBLE-00285: drop the unconsumed remainder through slices
        // rather than by re-entering the iterator. `for _ in self.by_ref() {}`
        // dropped one value per step, so a panicking destructor abandoned every
        // element still queued behind it.
        let head = self.head;
        let remaining = self.remaining;
        self.remaining = 0;
        let first_run = remaining.min(N - head);
        // SAFETY: the drain owns slots head..head+remaining (mod N); they are
        // initialized, `remaining` is now zero so nothing drops them twice, and
        // `MaybeUninit<T>` shares T's layout.
        unsafe {
            let base = self.ring_data.as_mut_ptr().cast::<T>();
            // As in `FixedRing::drop`: the wrapped run needs an owner, or a
            // panic in the run below would skip it.
            let _wrapped = DropRun {
                first: base,
                len: remaining - first_run,
            };
            ptr::drop_in_place(ptr::slice_from_raw_parts_mut(base.add(head), first_run));
        }
    }
}

// =============================================================================
// FixedVecIntoIter (owned consuming iterator)
// =============================================================================

/// Consuming iterator over `FixedVec` elements.
pub struct FixedVecIntoIter<T: Clone, const N: usize> {
    vec: FixedVec<T, N>,
    pos: usize,
}

impl<T: Clone, const N: usize> Iterator for FixedVecIntoIter<T, N> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        if self.pos >= self.vec.len as usize {
            return None;
        }
        // SAFETY: pos < len, slot is initialized
        let val = unsafe { self.vec.data[self.pos].assume_init_read() };
        self.pos += 1;
        Some(val)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.vec.len as usize - self.pos;
        (remaining, Some(remaining))
    }
}

impl<T: Clone, const N: usize> ExactSizeIterator for FixedVecIntoIter<T, N> {}

impl<T: Clone, const N: usize> Drop for FixedVecIntoIter<T, N> {
    fn drop(&mut self) {
        let remaining = self.pos..self.vec.len as usize;
        // The consumed prefix is no longer initialized, so the backing vector
        // must relinquish ownership before any element destructor can panic.
        self.vec.len = 0;
        // SAFETY: `remaining` is exactly the initialized, unconsumed suffix.
        unsafe {
            self.vec.data[remaining].assume_init_drop();
        }
    }
}

impl<T: Clone, const N: usize> IntoIterator for FixedVec<T, N> {
    type Item = T;
    type IntoIter = FixedVecIntoIter<T, N>;

    fn into_iter(self) -> Self::IntoIter {
        FixedVecIntoIter { vec: self, pos: 0 }
    }
}

// =============================================================================
// Checkpoint serde: live elements only, in logical order
// =============================================================================

/// Serializes the stored elements as a sequence. Unused capacity is not
/// state and is never written.
impl<T: Clone + serde::Serialize, const N: usize> serde::Serialize for FixedVec<T, N> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}

/// Rebuilds the vector, refusing more elements than the fixed capacity.
impl<'de, T: Clone + serde::Deserialize<'de>, const N: usize> serde::Deserialize<'de>
    for FixedVec<T, N>
{
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let items = Vec::<T>::deserialize(deserializer)?;
        if items.len() > N {
            return Err(serde::de::Error::invalid_length(
                items.len(),
                &"at most the fixed capacity",
            ));
        }
        Ok(items.into_iter().collect())
    }
}

/// Serializes the queued elements oldest first. The storage offset of the
/// oldest element is an implementation detail and restores as zero.
impl<T: Clone + serde::Serialize, const N: usize> serde::Serialize for FixedRing<T, N> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}

/// Rebuilds the queue, refusing more elements than the fixed capacity.
impl<'de, T: Clone + serde::Deserialize<'de>, const N: usize> serde::Deserialize<'de>
    for FixedRing<T, N>
{
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let items = Vec::<T>::deserialize(deserializer)?;
        if items.len() > N {
            return Err(serde::de::Error::invalid_length(
                items.len(),
                &"at most the fixed capacity",
            ));
        }
        let mut ring = Self::new();
        for item in items {
            ring.push_back(item);
        }
        Ok(ring)
    }
}

// =========================================================================
// MDD-BUG-CRUCIBLE-00285 - panic safety
//
// Every one of these operations used to abandon initialized elements when
// user code unwound: `len` had already been shortened (or never published),
// so no owning metadata covered the survivors and they leaked. Leaking is
// safe, so nothing crashed - which is exactly why source review found this
// and the existing tests did not.
//
// `Bomb` counts live instances and can be armed to panic from `Drop` or
// `Clone`. Each test asserts the count returns to zero, so it fails on a
// leak rather than on a crash. Every one of them fails without the fix.
// =========================================================================
#[cfg(test)]
mod serde_tests {
    use super::*;

    /// A heap-built ring starts empty with its full capacity and behaves
    /// like one from `new` (MDD-BUG-CRU-01645).
    #[test]
    fn a_boxed_ring_starts_empty_and_round_trips() {
        let mut ring = FixedRing::<u8, 4>::new_boxed();
        assert!(ring.is_empty());
        assert_eq!(ring.len(), 0);
        for value in 1..=4 {
            ring.push_back(value);
        }
        assert!(ring.is_full());
        assert_eq!(ring.pop_front(), Some(1));
        ring.push_back(5);
        assert_eq!(ring.iter().copied().collect::<Vec<_>>(), [2, 3, 4, 5]);
    }

    #[test]
    fn a_wrapped_ring_round_trips_oldest_first() {
        let mut ring = FixedRing::<u8, 4>::new();
        for value in [1, 2, 3, 4] {
            ring.push_back(value);
        }
        ring.pop_front();
        ring.pop_front();
        ring.push_back(5);
        let json = serde_json::to_string(&ring).unwrap();
        assert_eq!(json, "[3,4,5]");
        let decoded: FixedRing<u8, 4> = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.iter().copied().collect::<Vec<_>>(), [3, 4, 5]);
    }

    #[test]
    fn a_vector_round_trips_its_live_elements() {
        let vector: FixedVec<u16, 8> = [7, 8, 9].into_iter().collect();
        let json = serde_json::to_string(&vector).unwrap();
        assert_eq!(json, "[7,8,9]");
        let decoded: FixedVec<u16, 8> = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, vector);
    }

    #[test]
    fn more_elements_than_the_capacity_are_refused() {
        assert!(serde_json::from_str::<FixedRing<u8, 2>>("[1,2,3]").is_err());
        assert!(serde_json::from_str::<FixedVec<u8, 2>>("[1,2,3]").is_err());
    }
}

#[cfg(test)]
mod panic_safety_tests {
    use super::*;
    use std::cell::Cell;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    thread_local! {
        static LIVE: Cell<i32> = const { Cell::new(0) };
        static PANIC_ON_DROP: Cell<i32> = const { Cell::new(-1) };
        static PANIC_ON_CLONE: Cell<i32> = const { Cell::new(-1) };
    }

    fn live() -> i32 {
        LIVE.with(Cell::get)
    }

    /// Reset the counters. Thread-local, so parallel tests cannot disturb it.
    fn reset() {
        LIVE.with(|c| c.set(0));
        PANIC_ON_DROP.with(|c| c.set(-1));
        PANIC_ON_CLONE.with(|c| c.set(-1));
    }

    struct Bomb {
        id: i32,
    }

    impl Bomb {
        fn new(id: i32) -> Self {
            LIVE.with(|c| c.set(c.get() + 1));
            Bomb { id }
        }
    }

    impl Clone for Bomb {
        fn clone(&self) -> Self {
            if PANIC_ON_CLONE.with(Cell::get) == self.id {
                panic!("clone bomb {}", self.id);
            }
            Bomb::new(self.id)
        }
    }

    impl Drop for Bomb {
        fn drop(&mut self) {
            // Decrement FIRST: this element really is gone even if the
            // destructor then unwinds, and the count must say so.
            LIVE.with(|c| c.set(c.get() - 1));
            if PANIC_ON_DROP.with(Cell::get) == self.id {
                panic!("drop bomb {}", self.id);
            }
        }
    }

    fn vec_of(n: i32) -> FixedVec<Bomb, 8> {
        let mut v = FixedVec::new();
        for id in 0..n {
            v.push(Bomb::new(id));
        }
        v
    }

    #[test]
    fn truncate_drops_the_tail_after_a_panicking_destructor() {
        reset();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut v = vec_of(5);
            PANIC_ON_DROP.with(|c| c.set(2));
            v.truncate(0);
        }));
        assert!(result.is_err(), "the armed destructor must unwind");
        assert_eq!(live(), 0, "elements 3 and 4 were abandoned behind bomb 2");
    }

    #[test]
    fn vec_drop_drops_the_tail_after_a_panicking_destructor() {
        reset();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _v = vec_of(5);
            PANIC_ON_DROP.with(|c| c.set(1));
        }));
        assert!(result.is_err(), "the armed destructor must unwind");
        assert_eq!(live(), 0, "elements after bomb 1 were abandoned");
    }

    #[test]
    fn retain_keeps_everything_when_the_predicate_panics() {
        reset();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut v = vec_of(5);
            v.retain(|b| {
                assert!(b.id != 2, "predicate bomb");
                true
            });
        }));
        assert!(result.is_err(), "the predicate must unwind");
        assert_eq!(
            live(),
            0,
            "retain published len=0, so nothing owned the elements"
        );
    }

    #[test]
    fn retain_keeps_the_survivors_when_a_rejected_destructor_panics() {
        reset();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut v = vec_of(5);
            PANIC_ON_DROP.with(|c| c.set(2));
            v.retain(|b| b.id != 2);
        }));
        assert!(
            result.is_err(),
            "the rejected element's destructor must unwind"
        );
        assert_eq!(live(), 0, "kept and unvisited elements were abandoned");
    }

    #[test]
    fn vec_clone_drops_the_clones_it_already_made() {
        reset();
        let v = vec_of(5);
        let result = catch_unwind(AssertUnwindSafe(|| {
            PANIC_ON_CLONE.with(|c| c.set(3));
            let _copy = v.clone();
        }));
        assert!(result.is_err(), "the armed Clone must unwind");
        assert_eq!(
            live(),
            5,
            "the three completed clones leaked; only the original should remain"
        );
        drop(v);
        assert_eq!(live(), 0);
    }

    /// head=2 with a wrap, so the abandoned suffix spans both runs.
    fn wrapped_ring() -> FixedRing<Bomb, 4> {
        let mut r = FixedRing::<Bomb, 4>::new();
        for id in 0..4 {
            r.push_back(Bomb::new(id));
        }
        drop(r.pop_front());
        drop(r.pop_front());
        r.push_back(Bomb::new(4));
        r.push_back(Bomb::new(5));
        r
    }

    #[test]
    fn ring_drop_drops_both_runs_after_a_panicking_destructor() {
        reset();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let r = wrapped_ring();
            PANIC_ON_DROP.with(|c| c.set(2));
            drop(r);
        }));
        assert!(result.is_err(), "the armed destructor must unwind");
        assert_eq!(
            live(),
            0,
            "the wrapped remainder was abandoned behind bomb 2"
        );
    }

    #[test]
    fn ring_clone_drops_the_clones_it_already_made() {
        reset();
        let r = wrapped_ring();
        let before = live();
        let result = catch_unwind(AssertUnwindSafe(|| {
            PANIC_ON_CLONE.with(|c| c.set(4));
            let _copy = r.clone();
        }));
        assert!(result.is_err(), "the armed Clone must unwind");
        assert_eq!(live(), before, "the completed clones leaked");
        drop(r);
        assert_eq!(live(), 0);
    }

    #[test]
    fn ring_drain_drop_drops_the_unconsumed_remainder() {
        reset();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut r = wrapped_ring();
            let mut drain = r.drain();
            drop(drain.next());
            // Bomb 3 is in the drain's FIRST run, so the wrapped run behind it
            // is only reached if that run has its own owner.
            PANIC_ON_DROP.with(|c| c.set(3));
            drop(drain);
        }));
        assert!(result.is_err(), "the armed destructor must unwind");
        assert_eq!(live(), 0, "elements queued behind bomb 4 were abandoned");
    }
}
