//! Tests for fixed-capacity collections.

use super::fixed_collections::*;
use std::cell::Cell;
use std::rc::Rc;

#[derive(Clone)]
struct PanicOnceDrop<'a> {
    id: usize,
    panic_id: usize,
    drops: &'a [Cell<usize>],
    armed: &'a Cell<bool>,
}

impl Drop for PanicOnceDrop<'_> {
    fn drop(&mut self) {
        self.drops[self.id].set(self.drops[self.id].get() + 1);
        if self.id == self.panic_id && self.armed.replace(false) {
            panic!("intentional destructor panic for element {}", self.id);
        }
    }
}

// =============================================================================
// FixedVec tests
// =============================================================================

#[test]
fn fixed_vec_new_is_empty() {
    let v: FixedVec<i32, 8> = FixedVec::new();
    assert!(v.is_empty());
    assert_eq!(v.len(), 0);
    assert!(!v.is_full());
    assert_eq!(v.capacity(), 8);
}

#[test]
fn fixed_vec_new_handles_large_nested_element_without_live_entries() {
    #[derive(Clone)]
    struct SearchLikeState {
        _pattern: FixedString<260>,
        _dir_path: FixedString<260>,
        _entries: FixedVec<(FixedString<13>, u32), 128>,
        _index: usize,
    }

    let v: FixedVec<(u32, SearchLikeState), 32> = FixedVec::new();
    assert!(v.is_empty());
    assert_eq!(v.capacity(), 32);
}

#[test]
fn fixed_vec_new_boxed_is_empty() {
    let v = FixedVec::<(FixedString<13>, u32), 4096>::new_boxed();
    assert!(v.is_empty());
    assert_eq!(v.len(), 0);
    assert_eq!(v.capacity(), 4096);
}

#[test]
fn fixed_vec_new_boxed_drops_initialized_entries() {
    #[derive(Clone)]
    struct DropSentinel(Rc<Cell<usize>>);

    impl Drop for DropSentinel {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    let drops = Rc::new(Cell::new(0));
    {
        let mut v = FixedVec::<DropSentinel, 4096>::new_boxed();
        v.push(DropSentinel(drops.clone()));
        v.push(DropSentinel(drops.clone()));
        assert_eq!(drops.get(), 0);
    }

    assert_eq!(drops.get(), 2);
}

#[test]
fn fixed_vec_default_is_empty() {
    let v: FixedVec<i32, 4> = Default::default();
    assert!(v.is_empty());
    assert_eq!(v.len(), 0);
}

#[test]
fn fixed_vec_push_pop_len() {
    let mut v: FixedVec<i32, 4> = FixedVec::new();
    v.push(10);
    v.push(20);
    v.push(30);
    assert_eq!(v.len(), 3);
    assert!(!v.is_empty());
    assert!(!v.is_full());

    assert_eq!(v.pop(), Some(30));
    assert_eq!(v.pop(), Some(20));
    assert_eq!(v.pop(), Some(10));
    assert_eq!(v.pop(), None);
    assert!(v.is_empty());
}

/// Pin the `FixedVec::pop → None on empty` invariant that the emulator's
/// shadow interrupt stack relies on for its belt-and-suspenders double-pop
/// safety argument.
///
/// The BIOS stub dispatch fix (HLD 2026.04.11 - Interrupt Stack Symmetry for
/// BIOS Stub Dispatch, Option A6) pops the shadow interrupt stack eagerly at
/// the dispatcher site when redirecting to an assembler stub at F000:F8xx.
/// For WAIT stubs that exit via IRET, the global `pop_interrupt_on_iret`
/// hook fires *after* the stub's IRET and pops a second time — against what
/// is now an empty stack. That second pop must be a silent no-op. If a
/// future refactor makes `pop` panic on empty, this test fails loudly.
#[test]
fn fixed_vec_pop_on_empty_returns_none_silently() {
    // Drain past the bottom: three extra pops on an already-empty vector
    // must all return None without panicking.
    let mut v: FixedVec<u32, 32> = FixedVec::new();
    assert!(v.pop().is_none());
    assert!(v.pop().is_none());
    assert!(v.pop().is_none());
    assert_eq!(v.len(), 0);

    // Fill, drain exactly, then attempt to over-drain: excess pops must
    // also be silent None (the emulator's double-pop pattern).
    for i in 0..32u32 {
        v.push(i);
    }
    for _ in 0..32 {
        assert!(v.pop().is_some());
    }
    assert_eq!(v.len(), 0);
    assert!(v.pop().is_none());
    assert!(v.pop().is_none());
}

#[test]
fn fixed_vec_push_to_capacity() {
    let mut v: FixedVec<u8, 3> = FixedVec::new();
    v.push(1);
    v.push(2);
    v.push(3);
    assert!(v.is_full());
    assert_eq!(v.len(), 3);
}

#[test]
#[should_panic(expected = "capacity 3 exceeded")]
fn fixed_vec_push_panics_when_full() {
    let mut v: FixedVec<u8, 3> = FixedVec::new();
    v.push(1);
    v.push(2);
    v.push(3);
    v.push(4); // should panic
}

#[test]
fn fixed_vec_clear() {
    let mut v: FixedVec<i32, 8> = FixedVec::new();
    v.push(1);
    v.push(2);
    v.push(3);
    v.clear();
    assert!(v.is_empty());
    assert_eq!(v.len(), 0);
}

#[test]
fn fixed_vec_truncate() {
    let mut v: FixedVec<i32, 8> = FixedVec::new();
    for i in 0..5 {
        v.push(i);
    }
    v.truncate(3);
    assert_eq!(v.len(), 3);
    assert_eq!(&*v, &[0, 1, 2]);

    // Truncate beyond current length is a no-op.
    v.truncate(100);
    assert_eq!(v.len(), 3);
}

#[test]
fn fixed_vec_retain() {
    let mut v: FixedVec<i32, 8> = FixedVec::new();
    for i in 0..6 {
        v.push(i);
    }
    v.retain(|x| x % 2 == 0);
    assert_eq!(&*v, &[0, 2, 4]);
}

#[test]
fn fixed_vec_retain_all() {
    let mut v: FixedVec<i32, 4> = FixedVec::new();
    v.push(1);
    v.push(2);
    v.retain(|_| true);
    assert_eq!(&*v, &[1, 2]);
}

#[test]
fn fixed_vec_retain_none() {
    let mut v: FixedVec<i32, 4> = FixedVec::new();
    v.push(1);
    v.push(2);
    v.retain(|_| false);
    assert!(v.is_empty());
}

#[test]
fn fixed_vec_extend_from_slice() {
    let mut v: FixedVec<i32, 8> = FixedVec::new();
    v.push(1);
    v.extend_from_slice(&[2, 3, 4]);
    assert_eq!(&*v, &[1, 2, 3, 4]);
}

#[test]
#[should_panic(expected = "would exceed capacity")]
fn fixed_vec_extend_from_slice_overflow() {
    let mut v: FixedVec<i32, 3> = FixedVec::new();
    v.push(1);
    v.extend_from_slice(&[2, 3, 4]); // would need 4 slots, only 3 available
}

#[test]
fn fixed_vec_deref_returns_correct_slice() {
    let mut v: FixedVec<i32, 8> = FixedVec::new();
    v.push(10);
    v.push(20);
    let slice: &[i32] = &v;
    assert_eq!(slice, &[10, 20]);
    assert_eq!(slice.len(), 2);
}

#[test]
fn fixed_vec_deref_mut_allows_modification() {
    let mut v: FixedVec<i32, 8> = FixedVec::new();
    v.push(10);
    v.push(20);
    v[0] = 99;
    assert_eq!(v[0], 99);
    assert_eq!(v[1], 20);
}

#[test]
fn fixed_vec_clone_produces_independent_copy() {
    let mut v: FixedVec<i32, 8> = FixedVec::new();
    v.push(1);
    v.push(2);
    v.push(3);
    let mut cloned = v.clone();
    cloned[0] = 99;
    assert_eq!(v[0], 1);
    assert_eq!(cloned[0], 99);
    assert_eq!(v.len(), cloned.len());
}

/// Sentinel type that sets a flag on drop.
#[derive(Clone)]
struct DropSentinel {
    dropped: Rc<Cell<bool>>,
}

impl DropSentinel {
    fn new() -> (Self, Rc<Cell<bool>>) {
        let flag = Rc::new(Cell::new(false));
        (
            DropSentinel {
                dropped: flag.clone(),
            },
            flag,
        )
    }
}

impl Drop for DropSentinel {
    fn drop(&mut self) {
        self.dropped.set(true);
    }
}

#[test]
fn fixed_vec_drop_is_called_on_elements() {
    let (s1, flag1) = DropSentinel::new();
    let (s2, flag2) = DropSentinel::new();

    {
        let mut v: FixedVec<DropSentinel, 4> = FixedVec::new();
        v.push(s1);
        v.push(s2);
        assert!(!flag1.get());
        assert!(!flag2.get());
    } // v dropped here

    assert!(flag1.get(), "element 0 was not dropped");
    assert!(flag2.get(), "element 1 was not dropped");
}

#[test]
fn fixed_vec_drop_on_clear() {
    let (s, flag) = DropSentinel::new();
    let mut v: FixedVec<DropSentinel, 4> = FixedVec::new();
    v.push(s);
    assert!(!flag.get());
    v.clear();
    assert!(flag.get(), "element was not dropped on clear");
}

#[test]
fn fixed_vec_insert_at_beginning() {
    let mut v: FixedVec<i32, 8> = FixedVec::new();
    v.push(2);
    v.push(3);
    v.insert(0, 1);
    assert_eq!(&*v, &[1, 2, 3]);
}

#[test]
fn fixed_vec_insert_in_middle() {
    let mut v: FixedVec<i32, 8> = FixedVec::new();
    v.push(1);
    v.push(3);
    v.insert(1, 2);
    assert_eq!(&*v, &[1, 2, 3]);
}

#[test]
fn fixed_vec_insert_at_end() {
    let mut v: FixedVec<i32, 8> = FixedVec::new();
    v.push(1);
    v.push(2);
    v.insert(2, 3);
    assert_eq!(&*v, &[1, 2, 3]);
}

#[test]
#[should_panic(expected = "index 5 out of bounds")]
fn fixed_vec_insert_out_of_bounds() {
    let mut v: FixedVec<i32, 8> = FixedVec::new();
    v.push(1);
    v.insert(5, 2);
}

#[test]
fn fixed_vec_remove_from_beginning() {
    let mut v: FixedVec<i32, 8> = FixedVec::new();
    v.push(1);
    v.push(2);
    v.push(3);
    assert_eq!(v.remove(0), 1);
    assert_eq!(&*v, &[2, 3]);
}

#[test]
fn fixed_vec_remove_from_middle() {
    let mut v: FixedVec<i32, 8> = FixedVec::new();
    v.push(1);
    v.push(2);
    v.push(3);
    assert_eq!(v.remove(1), 2);
    assert_eq!(&*v, &[1, 3]);
}

#[test]
fn fixed_vec_remove_from_end() {
    let mut v: FixedVec<i32, 8> = FixedVec::new();
    v.push(1);
    v.push(2);
    v.push(3);
    assert_eq!(v.remove(2), 3);
    assert_eq!(&*v, &[1, 2]);
}

#[test]
fn fixed_vec_swap_remove() {
    let mut v: FixedVec<i32, 8> = FixedVec::new();
    v.push(10);
    v.push(20);
    v.push(30);
    v.push(40);

    // Swap-remove from middle: 20 replaced by 40.
    assert_eq!(v.swap_remove(1), 20);
    assert_eq!(v.len(), 3);
    assert_eq!(v[0], 10);
    assert_eq!(v[1], 40);
    assert_eq!(v[2], 30);
}

#[test]
fn fixed_vec_swap_remove_last() {
    let mut v: FixedVec<i32, 4> = FixedVec::new();
    v.push(1);
    v.push(2);
    assert_eq!(v.swap_remove(1), 2);
    assert_eq!(&*v, &[1]);
}

#[test]
fn fixed_vec_from_iterator() {
    let v: FixedVec<i32, 8> = (1..=5).collect();
    assert_eq!(&*v, &[1, 2, 3, 4, 5]);
}

#[test]
#[should_panic(expected = "capacity 3 exceeded")]
fn fixed_vec_from_iterator_overflow() {
    let _v: FixedVec<i32, 3> = (1..=4).collect();
}

#[test]
fn fixed_vec_partial_eq() {
    let mut a: FixedVec<i32, 8> = FixedVec::new();
    let mut b: FixedVec<i32, 8> = FixedVec::new();
    a.push(1);
    a.push(2);
    b.push(1);
    b.push(2);
    assert_eq!(a, b);

    b.push(3);
    assert_ne!(a, b);
}

#[test]
fn fixed_vec_debug() {
    let mut v: FixedVec<i32, 4> = FixedVec::new();
    v.push(1);
    v.push(2);
    let dbg = format!("{v:?}");
    assert_eq!(dbg, "[1, 2]");
}

#[test]
fn fixed_vec_into_iter_ref() {
    let mut v: FixedVec<i32, 8> = FixedVec::new();
    v.push(10);
    v.push(20);
    v.push(30);
    let sum: i32 = (&v).into_iter().sum();
    assert_eq!(sum, 60);
}

#[test]
fn fixed_vec_into_iter_mut_ref() {
    let mut v: FixedVec<i32, 8> = FixedVec::new();
    v.push(1);
    v.push(2);
    for x in &mut v {
        *x *= 10;
    }
    assert_eq!(&*v, &[10, 20]);
}

// Drop correctness for truncate
#[test]
fn fixed_vec_truncate_drops_removed() {
    let (s0, f0) = DropSentinel::new();
    let (s1, f1) = DropSentinel::new();
    let (s2, f2) = DropSentinel::new();
    let mut v: FixedVec<DropSentinel, 4> = FixedVec::new();
    v.push(s0);
    v.push(s1);
    v.push(s2);
    v.truncate(1);
    assert!(!f0.get(), "kept element not dropped");
    assert!(f1.get(), "removed element 1 dropped");
    assert!(f2.get(), "removed element 2 dropped");
}

// Drop correctness for retain
#[test]
fn fixed_vec_retain_drops_removed() {
    let (s0, f0) = DropSentinel::new();
    let (s1, f1) = DropSentinel::new();
    let (s2, f2) = DropSentinel::new();
    let mut v: FixedVec<DropSentinel, 4> = FixedVec::new();
    v.push(s0);
    v.push(s1);
    v.push(s2);
    // Keep elements at even indices (0 and 2), drop element at index 1
    let mut idx = 0usize;
    v.retain(|_| {
        let keep = idx.is_multiple_of(2);
        idx += 1;
        keep
    });
    assert!(!f0.get(), "kept element 0 not dropped");
    assert!(f1.get(), "removed element 1 dropped");
    assert!(!f2.get(), "kept element 2 not dropped");
}

// Clone after pop (don't read uninit slot)
#[test]
fn fixed_vec_clone_after_pop() {
    let mut v: FixedVec<i32, 4> = FixedVec::new();
    v.push(1);
    v.push(2);
    v.push(3);
    v.pop();
    let v2 = v.clone();
    assert_eq!(v2.len(), 2);
    assert_eq!(&*v2, &[1, 2]);
}

// FixedVec owned into_iter
#[test]
fn fixed_vec_into_iter_owned() {
    let mut v: FixedVec<i32, 4> = FixedVec::new();
    v.push(1);
    v.push(2);
    v.push(3);
    let items: Vec<_> = v.into_iter().collect();
    assert_eq!(items, vec![1, 2, 3]);
}

// Owned into_iter drop correctness
#[test]
fn fixed_vec_into_iter_partial_consume_drops() {
    let flags: Vec<_> = (0..4).map(|_| Rc::new(Cell::new(false))).collect();
    {
        let mut v: FixedVec<DropSentinel, 8> = FixedVec::new();
        for f in &flags {
            v.push(DropSentinel { dropped: f.clone() });
        }
        let mut iter = v.into_iter();
        iter.next(); // consume first element
        iter.next(); // consume second element
                     // Drop iter here -- should drop elements 2 and 3
    }
    for (i, f) in flags.iter().enumerate() {
        assert!(f.get(), "element {} should be dropped", i);
    }
}

#[test]
fn fixed_vec_into_iter_drop_is_panic_safe_after_partial_consumption() {
    let drops = std::array::from_fn::<_, 3, _>(|_| Cell::new(0));
    let armed = Cell::new(false);
    let mut v: FixedVec<PanicOnceDrop<'_>, 3> = FixedVec::new();
    for id in 0..3 {
        v.push(PanicOnceDrop {
            id,
            panic_id: 1,
            drops: &drops,
            armed: &armed,
        });
    }

    let mut iter = v.into_iter();
    drop(iter.next().unwrap());
    armed.set(true);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(iter)));

    assert!(result.is_err());
    assert_eq!(std::array::from_fn(|id| drops[id].get()), [1, 1, 1]);
}

// =============================================================================
// FixedString tests
// =============================================================================

#[test]
fn fixed_string_new_is_empty() {
    let s: FixedString<32> = FixedString::new();
    assert!(s.is_empty());
    assert_eq!(s.len(), 0);
    assert_eq!(s.as_str(), "");
    assert_eq!(s.capacity(), 32);
}

#[test]
fn fixed_string_default_is_empty() {
    let s: FixedString<16> = Default::default();
    assert!(s.is_empty());
    assert_eq!(s.as_str(), "");
}

#[test]
fn fixed_string_from_str() {
    let s: FixedString<32> = FixedString::from("hello");
    assert_eq!(s.len(), 5);
    assert_eq!(s.as_str(), "hello");
    assert_eq!(s.as_bytes(), b"hello");
}

#[test]
fn fixed_string_from_str_exact_fit() {
    let s: FixedString<5> = FixedString::from("hello");
    assert_eq!(s.as_str(), "hello");
    assert_eq!(s.len(), 5);
}

#[test]
fn fixed_string_from_str_truncation() {
    let s: FixedString<3> = FixedString::from("hello world");
    assert_eq!(s.len(), 3);
    assert_eq!(s.as_str(), "hel");
}

#[test]
fn fixed_string_truncation_at_utf8_boundary() {
    // 'e' with acute = U+00E9, 2 bytes in UTF-8: 0xC3 0xA9
    // "h" = 1 byte, "e" with acute = 2 bytes, total "he" = 3 bytes
    let input = "h\u{00E9}llo"; // "hello" but with accented e

    // Capacity 2: "h" fits (1 byte), but the accented 'e' needs 2 bytes (total 3).
    // Truncation should back up to avoid splitting the codepoint.
    let s: FixedString<2> = FixedString::from(input);
    assert_eq!(s.as_str(), "h");
    assert_eq!(s.len(), 1);

    // Capacity 3: "h" + accented 'e' = 3 bytes, fits exactly.
    let s: FixedString<3> = FixedString::from(input);
    assert_eq!(s.as_str(), "h\u{00E9}");
    assert_eq!(s.len(), 3);
}

#[test]
fn fixed_string_truncation_multibyte_emoji() {
    // Snowman U+2603 is 3 bytes in UTF-8
    let input = "\u{2603}X";
    // Capacity 2: not enough for the 3-byte snowman, truncate to empty.
    let s: FixedString<2> = FixedString::from(input);
    assert_eq!(s.as_str(), "");
    assert_eq!(s.len(), 0);

    // Capacity 3: snowman fits exactly.
    let s: FixedString<3> = FixedString::from(input);
    assert_eq!(s.as_str(), "\u{2603}");
}

#[test]
fn fixed_string_deref_to_str() {
    let s: FixedString<32> = FixedString::from("test");
    let as_str: &str = &s;
    assert_eq!(as_str, "test");
    assert!(s.starts_with("te"));
}

#[test]
fn fixed_string_display() {
    let s: FixedString<32> = FixedString::from("hello");
    assert_eq!(format!("{s}"), "hello");
}

#[test]
fn fixed_string_debug() {
    let s: FixedString<32> = FixedString::from("hello");
    let dbg = format!("{s:?}");
    assert_eq!(dbg, "FixedString(\"hello\")");
}

#[test]
fn fixed_string_partial_eq_str() {
    let s: FixedString<32> = FixedString::from("hello");
    assert_eq!(s, *"hello");
    assert!(s != *"world");
}

#[test]
fn fixed_string_partial_eq_ref_str() {
    let s: FixedString<32> = FixedString::from("hello");
    assert_eq!(s, "hello");
    assert!(s != "world");
}

#[test]
fn fixed_string_partial_eq_string() {
    let s: FixedString<32> = FixedString::from("hello");
    assert_eq!(s, "hello");
    assert!(s != "world");
}

#[test]
fn fixed_string_partial_eq_self() {
    let a: FixedString<32> = FixedString::from("hello");
    let b: FixedString<32> = FixedString::from("hello");
    let c: FixedString<32> = FixedString::from("world");
    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[test]
fn fixed_string_clone_copy() {
    let s: FixedString<32> = FixedString::from("hello");
    let cloned = s;
    let copied = s; // Copy
    assert_eq!(cloned.as_str(), "hello");
    assert_eq!(copied.as_str(), "hello");
    // Original still usable after copy (proves Copy).
    assert_eq!(s.as_str(), "hello");
}

#[test]
fn fixed_string_hash() {
    use std::collections::HashSet;
    let mut set = HashSet::new();
    let s: FixedString<32> = FixedString::from("hello");
    set.insert(s);
    assert!(set.contains(&FixedString::<32>::from("hello")));
    assert!(!set.contains(&FixedString::<32>::from("world")));
}

// FixedString push_str
#[test]
fn fixed_string_push_str() {
    let mut s = FixedString::<10>::from("hello");
    let written = s.push_str(" world");
    assert_eq!(written, 5); // remaining capacity is 5, " world" is 6 bytes, so only " worl" fits
    assert_eq!(&*s, "hello worl");
}

#[test]
fn fixed_string_push_char() {
    let mut s = FixedString::<5>::from("hi");
    assert!(s.push('!'));
    assert_eq!(&*s, "hi!");
}

#[test]
fn fixed_string_clear() {
    let mut s = FixedString::<10>::from("hello");
    s.clear();
    assert!(s.is_empty());
    assert_eq!(&*s, "");
}

// FixedString From<String>
#[test]
fn fixed_string_from_string() {
    let s = String::from("hello");
    let fs = FixedString::<10>::from(s);
    assert_eq!(&*fs, "hello");
}

// FixedString ordering
#[test]
fn fixed_string_ord() {
    let a = FixedString::<10>::from("aaa");
    let b = FixedString::<10>::from("bbb");
    assert!(a < b);
    assert!(b > a);
}

// =============================================================================
// FixedRing tests
// =============================================================================

#[test]
fn fixed_ring_new_is_empty() {
    let r: FixedRing<i32, 8> = FixedRing::new();
    assert!(r.is_empty());
    assert_eq!(r.len(), 0);
    assert!(!r.is_full());
    assert_eq!(r.capacity(), 8);
}

#[test]
fn fixed_ring_default_is_empty() {
    let r: FixedRing<i32, 4> = Default::default();
    assert!(r.is_empty());
}

#[test]
fn fixed_ring_push_back_pop_front_fifo() {
    let mut r: FixedRing<i32, 8> = FixedRing::new();
    r.push_back(10);
    r.push_back(20);
    r.push_back(30);
    assert_eq!(r.len(), 3);

    assert_eq!(r.pop_front(), Some(10));
    assert_eq!(r.pop_front(), Some(20));
    assert_eq!(r.pop_front(), Some(30));
    assert_eq!(r.pop_front(), None);
}

#[test]
fn fixed_ring_push_to_capacity() {
    let mut r: FixedRing<u8, 3> = FixedRing::new();
    r.push_back(1);
    r.push_back(2);
    r.push_back(3);
    assert!(r.is_full());
}

#[test]
#[should_panic(expected = "capacity 3 exceeded")]
fn fixed_ring_push_panics_when_full() {
    let mut r: FixedRing<u8, 3> = FixedRing::new();
    r.push_back(1);
    r.push_back(2);
    r.push_back(3);
    r.push_back(4); // should panic
}

#[test]
fn fixed_ring_front_back() {
    let mut r: FixedRing<i32, 8> = FixedRing::new();
    assert_eq!(r.front(), None);
    assert_eq!(r.back(), None);

    r.push_back(10);
    assert_eq!(r.front(), Some(&10));
    assert_eq!(r.back(), Some(&10));

    r.push_back(20);
    assert_eq!(r.front(), Some(&10));
    assert_eq!(r.back(), Some(&20));

    r.push_back(30);
    assert_eq!(r.front(), Some(&10));
    assert_eq!(r.back(), Some(&30));
}

#[test]
fn fixed_ring_clear() {
    let mut r: FixedRing<i32, 8> = FixedRing::new();
    r.push_back(1);
    r.push_back(2);
    r.push_back(3);
    r.clear();
    assert!(r.is_empty());
    assert_eq!(r.len(), 0);
    assert_eq!(r.front(), None);
}

#[test]
fn fixed_ring_clear_is_panic_safe_after_wrap() {
    let drops = std::array::from_fn::<_, 5, _>(|_| Cell::new(0));
    let armed = Cell::new(false);
    let make_value = |id| PanicOnceDrop {
        id,
        panic_id: 1,
        drops: &drops,
        armed: &armed,
    };
    let mut ring: FixedRing<PanicOnceDrop<'_>, 4> = FixedRing::new();

    ring.push_back(make_value(3));
    ring.push_back(make_value(4));
    drop(ring.pop_front());
    drop(ring.pop_front());
    ring.push_back(make_value(0));
    ring.push_back(make_value(1));
    ring.push_back(make_value(2));

    armed.set(true);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ring.clear()));
    assert!(result.is_err());
    if !ring.is_empty() {
        let stale_len = ring.len();
        std::mem::forget(ring);
        panic!("clear left {stale_len} already-dropped elements modeled as live");
    }
    drop(ring);
    assert_eq!(std::array::from_fn(|id| drops[id].get()), [1, 1, 1, 1, 1]);
}

#[test]
fn fixed_ring_wrap_around() {
    let mut r: FixedRing<i32, 4> = FixedRing::new();

    // Fill the ring.
    r.push_back(1);
    r.push_back(2);
    r.push_back(3);
    r.push_back(4);

    // Pop all, advancing head past the end.
    assert_eq!(r.pop_front(), Some(1));
    assert_eq!(r.pop_front(), Some(2));
    assert_eq!(r.pop_front(), Some(3));
    assert_eq!(r.pop_front(), Some(4));
    assert!(r.is_empty());

    // Push again — head is now wrapped.
    r.push_back(10);
    r.push_back(20);
    r.push_back(30);
    r.push_back(40);

    assert_eq!(r.pop_front(), Some(10));
    assert_eq!(r.pop_front(), Some(20));
    assert_eq!(r.pop_front(), Some(30));
    assert_eq!(r.pop_front(), Some(40));
}

#[test]
fn fixed_ring_iter_fifo_order() {
    let mut r: FixedRing<i32, 8> = FixedRing::new();
    r.push_back(1);
    r.push_back(2);
    r.push_back(3);

    let items: Vec<_> = r.iter().copied().collect();
    assert_eq!(items, vec![1, 2, 3]);
}

#[test]
fn fixed_ring_iter_after_wrap() {
    let mut r: FixedRing<i32, 4> = FixedRing::new();

    // Advance head by pushing and popping.
    r.push_back(100);
    r.push_back(200);
    r.pop_front();
    r.pop_front();
    // head is now at index 2

    r.push_back(1);
    r.push_back(2);
    r.push_back(3);

    let items: Vec<_> = r.iter().copied().collect();
    assert_eq!(items, vec![1, 2, 3]);
}

#[test]
fn fixed_ring_iter_exact_size() {
    let mut r: FixedRing<i32, 8> = FixedRing::new();
    r.push_back(1);
    r.push_back(2);
    let iter = r.iter();
    assert_eq!(iter.len(), 2);
}

#[test]
fn fixed_ring_clone_produces_independent_copy() {
    let mut r: FixedRing<i32, 8> = FixedRing::new();
    r.push_back(1);
    r.push_back(2);
    r.push_back(3);

    let cloned = r.clone();

    // Mutate original.
    r.pop_front();
    assert_eq!(r.len(), 2);

    // Clone is unaffected.
    assert_eq!(cloned.len(), 3);
    let items: Vec<_> = cloned.iter().copied().collect();
    assert_eq!(items, vec![1, 2, 3]);
}

#[test]
fn fixed_ring_clone_after_wrap() {
    let mut r: FixedRing<i32, 4> = FixedRing::new();
    r.push_back(100);
    r.push_back(200);
    r.pop_front();
    r.pop_front();
    r.push_back(1);
    r.push_back(2);
    r.push_back(3);

    let cloned = r.clone();
    let items: Vec<_> = cloned.iter().copied().collect();
    assert_eq!(items, vec![1, 2, 3]);
}

#[test]
fn fixed_ring_drop_is_called_on_elements() {
    let (s1, flag1) = DropSentinel::new();
    let (s2, flag2) = DropSentinel::new();

    {
        let mut r: FixedRing<DropSentinel, 4> = FixedRing::new();
        r.push_back(s1);
        r.push_back(s2);
        assert!(!flag1.get());
        assert!(!flag2.get());
    } // r dropped here

    assert!(flag1.get(), "element 0 was not dropped");
    assert!(flag2.get(), "element 1 was not dropped");
}

#[test]
fn fixed_ring_drop_after_wrap() {
    let (s1, flag1) = DropSentinel::new();
    let (s2, flag2) = DropSentinel::new();

    {
        let mut r: FixedRing<DropSentinel, 3> = FixedRing::new();
        // Advance head to trigger wrap-around.
        let (dummy, _) = DropSentinel::new();
        r.push_back(dummy);
        r.pop_front();
        // head is now at index 1
        r.push_back(s1);
        r.push_back(s2);
        assert!(!flag1.get());
        assert!(!flag2.get());
    }

    assert!(flag1.get(), "wrapped element 0 was not dropped");
    assert!(flag2.get(), "wrapped element 1 was not dropped");
}

#[test]
fn fixed_ring_drop_on_clear() {
    let (s, flag) = DropSentinel::new();
    let mut r: FixedRing<DropSentinel, 4> = FixedRing::new();
    r.push_back(s);
    assert!(!flag.get());
    r.clear();
    assert!(flag.get(), "element was not dropped on clear");
}

#[test]
fn fixed_ring_partial_eq() {
    let mut a: FixedRing<i32, 8> = FixedRing::new();
    let mut b: FixedRing<i32, 8> = FixedRing::new();
    a.push_back(1);
    a.push_back(2);
    b.push_back(1);
    b.push_back(2);
    assert_eq!(a, b);

    b.push_back(3);
    assert_ne!(a, b);
}

#[test]
fn fixed_ring_partial_eq_after_wrap() {
    // Two rings with same logical contents but different head positions.
    let mut a: FixedRing<i32, 4> = FixedRing::new();
    a.push_back(1);
    a.push_back(2);

    let mut b: FixedRing<i32, 4> = FixedRing::new();
    // Advance head in b.
    b.push_back(99);
    b.pop_front();
    b.push_back(1);
    b.push_back(2);

    assert_eq!(a, b);
}

#[test]
fn fixed_ring_debug() {
    let mut r: FixedRing<i32, 8> = FixedRing::new();
    r.push_back(1);
    r.push_back(2);
    let dbg = format!("{r:?}");
    assert_eq!(dbg, "[1, 2]");
}

// FixedRing full with wrap and iteration
#[test]
fn fixed_ring_full_wrap_iter() {
    let mut r: FixedRing<i32, 4> = FixedRing::new();
    r.push_back(100);
    r.pop_front(); // head at 1
    r.push_back(1);
    r.push_back(2);
    r.push_back(3);
    r.push_back(4);
    assert_eq!(r.len(), 4);
    let items: Vec<_> = r.iter().copied().collect();
    assert_eq!(items, vec![1, 2, 3, 4]);
}

// FixedRing drop at exact wrap boundary
#[test]
fn fixed_ring_drop_exact_wrap() {
    let flags: Vec<_> = (0..4).map(|_| Rc::new(Cell::new(false))).collect();
    {
        let mut r: FixedRing<DropSentinel, 4> = FixedRing::new();
        // Advance head to position 3
        for _ in 0..3 {
            let (s, _) = DropSentinel::new();
            r.push_back(s);
            r.pop_front();
        }
        // Now push 4 elements starting at head=3, wrapping around
        for f in &flags {
            r.push_back(DropSentinel { dropped: f.clone() });
        }
        assert_eq!(r.len(), 4);
        // Drop r here
    }
    for (i, f) in flags.iter().enumerate() {
        assert!(f.get(), "element {} should be dropped", i);
    }
}

// FixedRing drain
#[test]
fn fixed_ring_drain_basic() {
    let mut r: FixedRing<i32, 4> = FixedRing::new();
    r.push_back(1);
    r.push_back(2);
    r.push_back(3);
    let items: Vec<_> = r.drain().collect();
    assert_eq!(items, vec![1, 2, 3]);
    assert!(r.is_empty());
}

#[test]
fn fixed_ring_drain_with_wrap() {
    let mut r: FixedRing<i32, 4> = FixedRing::new();
    r.push_back(10);
    r.pop_front(); // advance head
    r.push_back(1);
    r.push_back(2);
    r.push_back(3);
    let items: Vec<_> = r.drain().collect();
    assert_eq!(items, vec![1, 2, 3]);
    assert!(r.is_empty());
}

// FixedRing index
#[test]
fn fixed_ring_index() {
    let mut r: FixedRing<i32, 4> = FixedRing::new();
    r.push_back(10);
    r.pop_front(); // advance head
    r.push_back(1);
    r.push_back(2);
    r.push_back(3);
    assert_eq!(r[0], 1);
    assert_eq!(r[1], 2);
    assert_eq!(r[2], 3);
}

#[test]
#[should_panic(expected = "FixedRing index out of bounds")]
fn fixed_ring_index_out_of_bounds() {
    let mut r: FixedRing<i32, 4> = FixedRing::new();
    r.push_back(1);
    let _ = r[1]; // out of bounds
}

// -- Extra branch-coverage tests ---------------------------------------------

#[test]
fn fixed_string_push_str_backs_off_utf8_boundary_mid_codepoint() {
    // 'é' (U+00E9) is 2 bytes in UTF-8 (0xC3 0xA9). With 2 bytes remaining,
    // appending "éé" would try to land inside the second 'é', and the UTF-8
    // boundary loop must back up to the valid boundary at offset 2 (after
    // the first complete 'é'), leaving the buffer at "xxé".
    let mut s: FixedString<4> = FixedString::from("xx"); // 2 bytes used, 2 remaining
    let written = s.push_str("\u{00E9}\u{00E9}"); // 2 chars, 4 bytes total
    assert_eq!(
        written, 2,
        "one whole 'é' written (2 bytes), second backed off"
    );
    assert_eq!(s.as_str(), "xx\u{00E9}");
    assert_eq!(s.len(), 4);
}

#[test]
fn fixed_string_push_str_zero_remaining_keeps_string_intact() {
    let mut s: FixedString<3> = FixedString::from("abc"); // full
    let written = s.push_str("xyz");
    assert_eq!(written, 0);
    assert_eq!(s.as_str(), "abc");
}

#[test]
fn fixed_string_push_char_multibyte_returns_false_when_too_small() {
    // Snowman U+2603 is 3 bytes; buffer has only 2 free — push must return
    // false and leave the buffer unchanged.
    let mut s: FixedString<2> = FixedString::new();
    assert!(!s.push('\u{2603}'));
    assert_eq!(s.as_str(), "");
}

#[test]
fn fixed_ring_into_iter_via_reference() {
    // Exercise IntoIterator for &FixedRing (the for-loop over a shared ref
    // and the `.into_iter()` call on a shared reference).
    let mut r: FixedRing<i32, 4> = FixedRing::new();
    r.push_back(10);
    r.push_back(20);
    r.push_back(30);
    let mut out = Vec::new();
    for v in &r {
        out.push(*v);
    }
    assert_eq!(out, vec![10, 20, 30]);
    // Same via explicit into_iter on &r.
    let collected: Vec<i32> = (&r).into_iter().copied().collect();
    assert_eq!(collected, vec![10, 20, 30]);
}
