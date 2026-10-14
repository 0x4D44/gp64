//! PC board adapter for the shared Intel 8253/8254 timer core.
//!
//! The public re-export deliberately preserves the existing `gp64-hw`
//! timer API. PC I/O decode and scheduler wiring continue to use `Pit::new`,
//! whose defaults remain the historical mdpc defaults.

pub use gp64_pit::*;
