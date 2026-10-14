//! Host fault handling for projected guest-memory alias views.
//!
//! - [`FastmemFaultReason`], the census vocabulary, on every host.
//! - The flat-memory HLE layout constants the fastmem window is built on.
//! - On macOS/aarch64 only: the host fault handler, the window registry, the
//!   per-window fault queue and the fault census (`fault.rs`).
//!
//! The PC `Memory` owns the fastmem views and registers their windows here;
//! the a64 DRC emits the sites the handler decodes and drains the queue. No
//! other machine depends on this crate.

/// Flat-memory alias layout: guest linear = physical - `FASTMEM_PHYS_BIAS`
/// (the page-table mapper maps linear 0x10000 to physical
/// 0x0100_0000).
pub const FASTMEM_PHYS_BIAS: usize = 0x00FF_0000;

/// Host address space reserved per RAM backing on the stable-reservation hosts:
/// the 4 GiB linear window above [`FASTMEM_PHYS_BIAS`] plus slack, a multiple of
/// 16 MiB so every host page size up to 16 MiB divides it. It also exceeds
/// `MAX_RAM_SIZE + FETCH_PADDING`, so RAM growth never outgrows it.
#[cfg(target_pointer_width = "64")]
pub const RAM_RESERVATION_BYTES: usize = (1usize << 32) + 0x0100_0000;

/// Why a fastmem access to one guest linear address faults now (census).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FastmemFaultReason {
    /// The forced-fault oracle is on.
    Forced,
    /// The access runs past the 4 GiB linear window.
    BeyondWindow,
    /// PDE or PTE not present: unmapped, reserved, or a guard page.
    NotPresent,
    /// Present but supervisor-only.
    Supervisor,
    /// Present and user, but not the identity mapping onto ordinary RAM.
    NotRam,
    /// The guest page is readable; a stricter guest page shares its host page.
    MixedHostPage,
    /// The page is readable in the window now (e.g. a guard page consumed).
    NowReadable,
}

impl FastmemFaultReason {
    /// Stable report token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Forced => "forced",
            Self::BeyondWindow => "beyond_window",
            Self::NotPresent => "not_present",
            Self::Supervisor => "supervisor",
            Self::NotRam => "not_ram",
            Self::MixedHostPage => "mixed_host_page",
            Self::NowReadable => "now_readable",
        }
    }

    /// Every reason, in census index order (`reason as usize`).
    pub const ALL: [Self; 7] = [
        Self::Forced,
        Self::BeyondWindow,
        Self::NotPresent,
        Self::Supervisor,
        Self::NotRam,
        Self::MixedHostPage,
        Self::NowReadable,
    ];
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod fault;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub use fault::*;
