//! Address domains and direct-access proofs (LLD 2026.10.02 §1, HLD R2).
//!
//! Guest linear, architectural physical, bus physical and canonical backing
//! addresses are distinct types. Host pointers exist only inside proof objects:
//! a [`DirectSpan`] carries one, a [`DirectPage`] names a backing page and the
//! core derives the pointer from its own RAM base.

use std::ptr::NonNull;

macro_rules! domain_type {
    ($(#[$doc:meta])* $name:ident($repr:ty)) => {
        $(#[$doc])*
        #[repr(transparent)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub $repr);
    };
}

domain_type!(
    /// Segmented-and-paged-out linear address (after segmentation, before paging).
    GuestLinear(u32)
);
domain_type!(
    /// `GuestLinear >> 12`.
    LinearPage(u32)
);
domain_type!(
    /// The CPU's physical output: a page-walk result, or the linear address when
    /// paging is off. Pre-bus: PC A20 has not been applied.
    ArchPhys(usize)
);
domain_type!(
    /// `ArchPhys >> 12`.
    ArchPhysPage(u32)
);
domain_type!(
    /// The address the machine's bus decodes: `ArchPhys` after the machine mask
    /// (PC: the A20 AND-mask; machine: identity). `usize`, because on the PC flat-memory
    /// HLE layout a guest physical address reaches past 4 GiB.
    GuestPhys(usize)
);
domain_type!(
    /// `GuestPhys >> 12`.
    PhysPage(u32)
);
domain_type!(
    /// Canonical position inside the machine's single RAM backing: one value per
    /// backing byte, however many `GuestPhys` aliases decode to it.
    BackingOffset(usize)
);
domain_type!(
    /// `BackingOffset >> 12`.
    BackingPage(u32)
);

/// What an engine intends to do with a direct span. A proof is valid ONLY for
/// its intent.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AccessIntent {
    Read = 1,
    Write = 2,
    ReadWrite = 3,
    Fetch = 4,
}

impl AccessIntent {
    /// True for intents that load through the span (`Read`, `ReadWrite`).
    #[inline]
    #[must_use]
    pub const fn reads(self) -> bool {
        matches!(self, Self::Read | Self::ReadWrite)
    }

    /// True for intents that store through the span (`Write`, `ReadWrite`).
    #[inline]
    #[must_use]
    pub const fn writes(self) -> bool {
        matches!(self, Self::Write | Self::ReadWrite)
    }
}

/// Proof that `[p, p + len)` is plain RAM for its intent, contiguous in host
/// memory and free of machine side effects (LLD 1.4 V1-V6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectSpan {
    ptr: NonNull<u8>,
    backing: BackingOffset,
    len: usize,
    intent: AccessIntent,
}

impl DirectSpan {
    /// Assemble a proof from parts a machine has already established.
    ///
    /// # Safety
    /// `ptr` is the host byte for `backing`, and LLD 1.4 V1-V6 plus D2 hold for
    /// `[backing, backing + len)` at `intent`.
    #[inline]
    #[must_use]
    pub const unsafe fn from_raw_parts(
        ptr: NonNull<u8>,
        backing: BackingOffset,
        len: usize,
        intent: AccessIntent,
    ) -> Self {
        Self {
            ptr,
            backing,
            len,
            intent,
        }
    }

    /// Host address of the span's first byte.
    #[inline]
    #[must_use]
    pub const fn ptr(&self) -> NonNull<u8> {
        self.ptr
    }

    /// Canonical backing offset of the span's first byte.
    #[inline]
    #[must_use]
    pub const fn backing(&self) -> BackingOffset {
        self.backing
    }

    /// Length of the span in bytes.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// True for a zero-length span.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The intent the proof was issued for.
    #[inline]
    #[must_use]
    pub const fn intent(&self) -> AccessIntent {
        self.intent
    }
}

/// A page-sized proof for a TLB fill: which backing page, and whether the read
/// and write views may be published. It holds no pointer; the core derives the
/// host address from its own RAM base (LLD 1.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectPage {
    backing: BackingPage,
    read: bool,
    write: bool,
}

impl DirectPage {
    /// A page proof. Its soundness obligations sit on
    /// [`crate::MemoryCore::tlb_fill`], which is `unsafe`.
    #[inline]
    #[must_use]
    pub const fn new(backing: BackingPage, read: bool, write: bool) -> Self {
        Self {
            backing,
            read,
            write,
        }
    }

    /// The backing page the views address.
    #[inline]
    #[must_use]
    pub const fn backing(&self) -> BackingPage {
        self.backing
    }

    /// Whether the read views may be published.
    #[inline]
    #[must_use]
    pub const fn read(&self) -> bool {
        self.read
    }

    /// Whether the write views may be published (subject to writable + dirty).
    #[inline]
    #[must_use]
    pub const fn write(&self) -> bool {
        self.write
    }
}

/// Page-table permissions recorded with a translation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct PagePerms {
    pub writable: bool,
    pub user: bool,
    pub dirty: bool,
}

/// One TLB publication (LLD 2.2). `direct: None` publishes both views null.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TlbFill {
    pub linear: LinearPage,
    /// Stored in `TlbEntry::physical_page`.
    pub phys: PhysPage,
    pub perms: PagePerms,
    pub direct: Option<DirectPage>,
}
