//! Machine-neutral x86 memory core and the `X86Machine` contract.
//!
//! HLD `2026.10.02 - x86 memory core and machine-generic engines` (step M1)
//! and its binding contract, LLD `2026.10.02 - X86Machine trait and safety
//! contract`. Every x86 machine including PC `Memory` embeds one
//! [`MemoryCore`] and implements [`X86Machine`]; the x86 engines become generic
//! over that trait in M2/M3.
//!
//! The core names no VGA, ROM, EMS, A20, DPMI or machine: each predicate
//! arrives as an argument the machine proved (HLD R2). Its only normal
//! workspace dependency is `gp64-types` (HLD §2), which
//! `tests/config_surface.rs` enforces.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod core_direct;
pub mod domain;
pub mod flat_tlb;
pub mod generations;
pub mod machine;
mod memory_core;
mod publication;
pub mod tlb;

#[cfg(any(test, feature = "test-util"))]
pub mod testing;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod publication_tests;

use std::sync::atomic::AtomicU64;

pub use domain::{
    AccessIntent, ArchPhys, ArchPhysPage, BackingOffset, BackingPage, DirectPage, DirectSpan,
    GuestLinear, GuestPhys, LinearPage, PagePerms, PhysPage, TlbFill,
};
pub use generations::{DirectGenerations, DirectGenerationsSnapshot, RamBasePtr};
pub use machine::{
    AccessPolicyCore, MachineId, NativeCommitCost, PagingState, PortSync, ReadRoute, WriteRoute,
    X86Machine,
};
pub use memory_core::MemoryCore;
pub use publication::{
    DescriptorReprojection, DescriptorTable, EngineEntry, KindSet, Publication, PublicationPlan,
    PublicationReceipt, PublicationStage,
};
pub use tlb::{
    tlb_index, SoftwareTlb, TlbEntry, TlbStats, TLB_FLAG_DIRTY, TLB_FLAG_GLOBAL, TLB_FLAG_USER,
    TLB_FLAG_WRITABLE, TLB_HASH_SHIFT, TLB_INVALID_TAG, TLB_SIZE,
};

/// Decode-cache executed-page granularity (4 KiB).
pub const INSTR_EXEC_PAGE_SHIFT: usize = 12;
/// Decode-cache executed-page size in bytes.
pub const INSTR_EXEC_PAGE_SIZE: usize = 1 << INSTR_EXEC_PAGE_SHIFT;

/// Process-unique identity source for [`MemoryCore`] instances (EXP-N006 N2).
static MEMORY_INSTANCE_NONCE: AtomicU64 = AtomicU64::new(1);

/// True when the non-empty ranges `[a_start, a_start + a_len)` and
/// `[b_start, b_start + b_len)` overlap (saturating ends).
#[inline(always)]
#[must_use]
pub(crate) fn range_overlaps(a_start: usize, a_len: usize, b_start: usize, b_len: usize) -> bool {
    if a_len == 0 || b_len == 0 {
        return false;
    }
    let a_end = a_start.saturating_add(a_len);
    let b_end = b_start.saturating_add(b_len);
    a_start < b_end && b_start < a_end
}
