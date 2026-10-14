//! MDD-BUG-CRU-01035 — a mapped EMS page-frame store must not touch the heap.
//!
//! Every guest byte, word or dword store into a mapped EMS frame slot works out
//! which frame windows show that backing byte, so it can invalidate each one.
//! That alias list used to be a fresh `Vec` per byte, putting the allocator on
//! an ordinary guest memory path — and a word or dword store decomposes into
//! bytes, so it paid two or four times over. Nothing fails when that happens,
//! so only an allocation count can see it.

use gp64_memory::{Memory, EMS_FRAME_BASE, EMS_FRAME_SIZE, EMS_PAGE_SIZE};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

/// Counts heap allocations made by the current thread while armed, so tests
/// running in parallel on other threads cannot disturb the count.
struct CountingAllocator;

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

fn note_allocation() {
    // `try_with` because the allocator can run while this thread's locals are
    // being torn down.
    if ARMED.try_with(Cell::get).unwrap_or(false) {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
    }
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_allocation();
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note_allocation();
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_allocation();
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

/// Heap allocations `f` makes on this thread.
fn allocations_during(f: impl FnOnce()) -> usize {
    ALLOCATIONS.with(|count| count.set(0));
    ARMED.with(|armed| armed.set(true));
    f();
    ARMED.with(|armed| armed.set(false));
    ALLOCATIONS.with(Cell::get)
}

/// An HLE EMS frame with backing page 1 mapped into slots 0 and 2, so every
/// store has two frame aliases to invalidate.
fn aliased_ems_memory() -> Memory {
    let mut mem = Memory::new();
    mem.init_ems_pool(4).unwrap();
    mem.claim_hle_ems_frame(EMS_FRAME_BASE, EMS_FRAME_SIZE)
        .unwrap();
    mem.set_ems_frame_page(0, Some(1)).unwrap();
    mem.set_ems_frame_page(2, Some(1)).unwrap();
    mem
}

const SLOT0: usize = EMS_FRAME_BASE;
const SLOT2: usize = EMS_FRAME_BASE + 2 * EMS_PAGE_SIZE;

#[test]
fn mapped_ems_byte_word_and_dword_stores_do_not_allocate() {
    let mut mem = aliased_ems_memory();

    let byte = allocations_during(|| mem.write_u8(SLOT0 + 0x10, 0x5A));
    let word = allocations_during(|| mem.write_u16(SLOT0 + 0x20, 0x1234));
    let dword = allocations_during(|| mem.write_u32(SLOT0 + 0x30, 0x89AB_CDEF));

    assert_eq!(
        (byte, word, dword),
        (0, 0, 0),
        "heap allocations per mapped EMS store (byte, word, dword)"
    );
    // The stores still reached the backing page, so the other alias sees them.
    assert_eq!(mem.read_u8(SLOT2 + 0x10), 0x5A);
    assert_eq!(mem.read_u16(SLOT2 + 0x20), 0x1234);
    assert_eq!(mem.read_u32(SLOT2 + 0x30), 0x89AB_CDEF);
}
