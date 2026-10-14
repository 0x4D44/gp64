//! Architectural SSE state.
//!
//! This module deliberately contains only persistent XMM/MXCSR state. Decode
//! and execution live in sibling modules so snapshots and native-engine offsets
//! do not depend on semantic implementation details.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Reset value of the SSE control/status register.
pub const MXCSR_RESET: u32 = 0x0000_1F80;

/// Default architectural MXCSR feature mask for the modeled SSE state.
///
/// Bit 6 (DAZ) is clear for Pentium III and earlier. `SseState` adds it only
/// for Pentium 4, so older identities continue to reject that reserved bit.
pub const MXCSR_MASK: u32 = 0x0000_FFBF;

/// Pentium 4 exposes the denormals-are-zero extension in MXCSR bit 6.
pub const MXCSR_MASK_DAZ: u32 = 0x0000_FFFF;

/// Guest software can select any of the four IEEE rounding directions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SseRounding {
    Nearest,
    Down,
    Up,
    TowardZero,
}

/// The eight architectural XMM registers and their control state.
///
/// `u128` keeps each register bit-exact and gives native-engine code a single
/// 16-byte home. The custom serde representation uses two little-endian u64
/// halves because the repository's JSON save-state paths cannot represent an
/// arbitrary u128 in `serde_json::Value`.
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SseState {
    pub xmm: [u128; 8],
    pub mxcsr: u32,
    pub mxcsr_mask: u32,
}

impl SseState {
    /// Construct architectural SSE state in reset form.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            xmm: [0; 8],
            mxcsr: MXCSR_RESET,
            mxcsr_mask: MXCSR_MASK,
        }
    }

    /// Restore the architecturally reset XMM contents and MXCSR.
    ///
    /// MXCSR_MASK is a processor capability mask rather than mutable guest
    /// state, so reset preserves it.
    pub fn reset(&mut self) {
        self.xmm = [0; 8];
        self.mxcsr = MXCSR_RESET;
    }

    /// Select the immutable MXCSR capability mask for the guest identity.
    pub fn set_daz_supported(&mut self, supported: bool) {
        self.mxcsr_mask = if supported {
            MXCSR_MASK_DAZ
        } else {
            MXCSR_MASK
        };
        self.mxcsr &= self.mxcsr_mask;
    }

    #[inline]
    pub fn rounding(self) -> SseRounding {
        SseRounding::from_mxcsr(self.mxcsr)
    }
}

impl SseRounding {
    /// Guest rounding control, decoded from MXCSR bits 14:13.
    ///
    /// Taking the bare word rather than the whole state lets the value
    /// helpers and their differential drive a synthetic MXCSR without
    /// building a CPU.
    #[inline]
    #[must_use]
    pub fn from_mxcsr(mxcsr: u32) -> Self {
        match (mxcsr >> 13) & 3 {
            0 => Self::Nearest,
            1 => Self::Down,
            2 => Self::Up,
            _ => Self::TowardZero,
        }
    }
}

impl Default for SseState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Serialize, Deserialize)]
struct SseStateWire {
    #[serde(default = "default_xmm", with = "xmm_serde")]
    xmm: [u128; 8],
    #[serde(default = "default_mxcsr")]
    mxcsr: u32,
    #[serde(default = "default_mxcsr_mask")]
    mxcsr_mask: u32,
}

fn default_xmm() -> [u128; 8] {
    [0; 8]
}

const fn default_mxcsr() -> u32 {
    MXCSR_RESET
}

const fn default_mxcsr_mask() -> u32 {
    MXCSR_MASK
}

impl Serialize for SseState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        SseStateWire {
            xmm: self.xmm,
            mxcsr: self.mxcsr,
            mxcsr_mask: self.mxcsr_mask,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SseState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SseStateWire::deserialize(deserializer)?;
        Ok(Self {
            xmm: wire.xmm,
            mxcsr: wire.mxcsr,
            mxcsr_mask: wire.mxcsr_mask,
        })
    }
}

mod xmm_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(value: &[u128; 8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let halves = value.map(|register| [register as u64, (register >> 64) as u64]);
        halves.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u128; 8], D::Error>
    where
        D: Deserializer<'de>,
    {
        let halves = <[[u64; 2]; 8]>::deserialize(deserializer)?;
        Ok(halves.map(|[low, high]| u128::from(low) | (u128::from(high) << 64)))
    }
}
