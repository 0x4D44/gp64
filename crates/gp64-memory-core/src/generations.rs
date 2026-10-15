//! Stable-heap direct-proof generation words and the RAM base pointer.
//!
//! Moved from `gp64-memory::direct_proof` in M1. The words and the per-page
//! table are machine-neutral; page classification and proof publication stay
//! with the machine.

use std::cell::Cell;
use std::sync::Arc;

use crate::publication::EngineGate;

/// Allocate one `u32` generation word per 4 KiB of pinned RAM capacity.
/// The table is heap-pinned so raw per-entry pointers stay valid. Only
/// over-capacity `Memory::resize_ram_backing` replaces it, retiring the
/// old table without freeing it and advancing its identity epoch.
pub(crate) fn new_page_generation_table(pinned_capacity: usize) -> Box<[Cell<u32>]> {
    (0..(pinned_capacity >> 12)).map(|_| Cell::new(0)).collect()
}

/// Mutable-provenance base pointer of the pinned RAM backing.
///
/// Captured from `RamBacking::host_base` at construction and resize,
/// never from a reference. Published write pointers must retain WRITE
/// provenance: deriving them from a shared slice would make stores
/// undefined behavior under Rust's aliasing model.
#[derive(Debug, Clone, Copy)]
pub struct RamBasePtr(pub *mut u8);

// SAFETY: the pointer targets `Memory`'s own pinned RAM allocation and is
// only dereferenced through proofs published by that `Memory` (the emulator
// is single-threaded; a `Memory` moves between threads only as a whole,
// carrying its allocation with it). Mirrors `unsafe impl Send for
// SoftwareTlb`, which holds host pointers into the same allocation.
unsafe impl Send for RamBasePtr {}

/// Stable-heap generations block for direct-memory proofs.
///
/// The six `Cell<u32>` generation words have offsets 0, 4, 8, 12, 16, 20.
/// The block is boxed at construction and never replaced, so its pointer
/// stays valid for the owning `Memory`'s lifetime, including moves.
#[repr(C)]
#[derive(Debug)]
pub struct DirectGenerations {
    /// Word 0: bumps on *every* proof-invalidating event (any of the words
    /// below changing). A consumer that only wants one load compares this.
    pub proof_generation: Cell<u32>,
    /// Word 1: low 32 bits of `Memory::dynrec_translation_epoch` — paging /
    /// TLB translation identity (CR3 churn, INVLPG, TLB flush, A20, DPMI
    /// physical mappings).
    pub map_generation: Cell<u32>,
    /// Word 2: low 32 bits of `Memory::dynrec_a20_generation`.
    pub a20_generation: Cell<u32>,
    /// Word 3: low 32 bits of the memory-side descriptor generation — bumped
    /// by GDT/LDT byte-write snoops, snoop-range (re)programming, and the
    /// root's descriptor-reload notification path.
    pub descriptor_generation: Cell<u32>,
    /// Word 4: low 32 bits of `Memory::direct_memory_policy_epoch` —
    /// non-translation direct-memory policy (ROM/MMIO/VGA routing, RAM
    /// backing growth, source policy).
    pub policy_generation: Cell<u32>,
    /// Word 5: global source-identity generation. Bumped by whole-memory
    /// identity changes and page-targeted source events through
    /// `Memory::bump_dynrec_source_generation_for_page`. Individual pages'
    /// staleness is tracked separately in `PageGenerationTable`.
    pub source_generation: Cell<u32>,
    /// Engine-entry depth and the publication poison latch, separate from
    /// the generation words. Kept in the stable heap block so adding it
    /// does not grow `MemoryCore` or the PC `Memory` hot layout.
    pub(crate) engine_gate: Arc<EngineGate>,
}

impl DirectGenerations {
    pub(crate) fn new() -> Self {
        Self {
            proof_generation: Cell::new(0),
            map_generation: Cell::new(0),
            a20_generation: Cell::new(0),
            descriptor_generation: Cell::new(0),
            policy_generation: Cell::new(0),
            source_generation: Cell::new(0),
            engine_gate: Arc::default(),
        }
    }

    /// Copy out all six words (for tests and host-side comparisons).
    pub fn snapshot(&self) -> DirectGenerationsSnapshot {
        DirectGenerationsSnapshot {
            proof_generation: self.proof_generation.get(),
            map_generation: self.map_generation.get(),
            a20_generation: self.a20_generation.get(),
            descriptor_generation: self.descriptor_generation.get(),
            policy_generation: self.policy_generation.get(),
            source_generation: self.source_generation.get(),
        }
    }

    /// Bump global word zero, the monotonic staleness and diagnostic counter.
    /// Production updates go through `Memory::stale_all_guarded_pages`,
    /// which also bumps every guarded page's generation word.
    #[inline]
    pub(crate) fn bump_proof_word(&self) {
        self.proof_generation
            .set(self.proof_generation.get().wrapping_add(1));
    }

    #[inline]
    pub(crate) fn record_map_epoch(&self, epoch: u64) {
        self.map_generation.set(epoch as u32);
    }

    #[inline]
    pub(crate) fn record_a20_epoch(&self, epoch: u64) {
        self.a20_generation.set(epoch as u32);
    }

    #[inline]
    pub(crate) fn record_descriptor_epoch(&self, epoch: u64) {
        self.descriptor_generation.set(epoch as u32);
    }

    #[inline]
    pub(crate) fn record_policy_epoch(&self, epoch: u64) {
        self.policy_generation.set(epoch as u32);
    }

    #[inline]
    pub(crate) fn bump_source_word(&self) {
        self.source_generation
            .set(self.source_generation.get().wrapping_add(1));
    }
}

/// Plain-data copy of the generations block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectGenerationsSnapshot {
    pub proof_generation: u32,
    pub map_generation: u32,
    pub a20_generation: u32,
    pub descriptor_generation: u32,
    pub policy_generation: u32,
    pub source_generation: u32,
}
