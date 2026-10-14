//! Stable-heap direct-proof generation words and the RAM base pointer.
//!
//! Moved from `gp64-memory::direct_proof` in M1. The words and the per-page
//! table are machine-neutral; page classification and proof publication stay
//! with the machine.

use std::cell::Cell;
use std::sync::Arc;

use crate::publication::EngineGate;

/// Allocate a fresh per-page generation table sized for `pinned_capacity`
/// bytes of pinned RAM backing (one `u32` word per 4 KiB physical page).
///
/// The table is heap-pinned for the lifetime of the owning `Memory`: emitted
/// guards hold raw per-entry addresses (via proof-slot fields) and bump them
/// with single stores. It is replaced ONLY by over-capacity
/// `Memory::resize_ram_backing`, which retires (never frees) the old table
/// and advances `Memory::page_generation_table_identity`.
pub(crate) fn new_page_generation_table(pinned_capacity: usize) -> Box<[Cell<u32>]> {
    (0..(pinned_capacity >> 12)).map(|_| Cell::new(0)).collect()
}

/// Mutable-provenance base pointer of the pinned RAM backing (DD-UNSAFE-1).
///
/// Captured via `RamBacking::host_base` (derived from the host mapping, never
/// from a reference) in `Memory::new()` and re-captured by
/// `Memory::resize_ram_backing`, so every published direct page-proof pointer
/// derives from a pointer with WRITE provenance over the backing. Deriving the
/// write pointer from a `&self`-borrowed slice instead would make every store through a
/// published proof (host-side or emitted) undefined behavior under the Rust
/// aliasing model.
#[derive(Debug, Clone, Copy)]
pub struct RamBasePtr(pub *mut u8);

// SAFETY: the pointer targets `Memory`'s own pinned RAM allocation and is
// only dereferenced through proofs published by that `Memory` (the emulator
// is single-threaded; a `Memory` moves between threads only as a whole,
// carrying its allocation with it). Mirrors `unsafe impl Send for
// SoftwareTlb`, which holds host pointers into the same allocation.
unsafe impl Send for RamBasePtr {}

/// Stable-heap generations block for emitted single-load guards.
///
/// `#[repr(C)]` with six consecutive `u32` words (each field is a
/// `Cell<u32>`, which has the same in-memory representation as `u32`).
/// Field offsets, in declaration order: 0, 4, 8, 12, 16, 20.
///
/// Address stability: the block is heap-allocated (`Box`) when `Memory` is
/// constructed and never replaced for the lifetime of the `Memory` value, so
/// the pointer returned by `Memory::direct_generations_ptr` remains valid
/// and stable even when the `Memory` struct itself is moved.
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
    /// Word 5: global source-identity generation. Bumps on whole-memory
    /// identity events (over-capacity RAM growth, snapshot restore) and on
    /// every dynrec-side compiled-source event — range
    /// registration/removal, foreign writes, and emitted SMC invalidation
    /// stubs — via `Memory::bump_dynrec_source_generation_for_page`
    /// (WI-C4/WI-G1). The per-page staleness signal lives in the
    /// `PageGenerationTable`; this word is the global source-event stamp
    /// (negative gap cache).
    pub source_generation: Cell<u32>,
    /// Engine-entry depth and the publication poison latch. Not a generation word and never read by
    /// emitted code: it
    /// trails the words so their offsets are unchanged, and it lives in this
    /// stable heap block so `MemoryCore` (and the PC `Memory` hot layout)
    /// does not grow.
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

    /// Bump the global word 0. WI-G1: word 0 has NO emitted readers — it is
    /// the Rust-side monotone staleness funnel and diagnostics counter only.
    /// Production code must reach it through
    /// `Memory::stale_all_guarded_pages`, which also bumps every guarded
    /// page word (the words emitted guards actually read).
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
