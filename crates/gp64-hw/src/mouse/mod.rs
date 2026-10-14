//! PS/2 and Bus Mouse hardware emulation
//!
//! This module provides hardware-level mouse emulation:
//! - PS/2 mouse (ports 0x60/0x64 through the keyboard controller)
//! - Bus mouse (ports vary by adapter type)
//!
//! Note: INT 33h DOS mouse driver is in the main crate, not here.

pub mod bus;
pub mod ps2;

pub use bus::{BusMouse, BusMouseType};
pub use ps2::{Ps2Mouse, SharedMouseState};
