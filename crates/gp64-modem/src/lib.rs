//! Real DSP modem emulation.
//!
//! Implements Hayes-compatible AT command parsing, V.21/V.32bis/V.34/V.90
//! signal processing, and G.711 mu-law PCM downstream. TCP carries
//! 16000 Hz PCM audio — the same signal that would travel over a phone line.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod at_parser;
pub mod call_setup;
pub mod crc;
pub mod dsp;
pub mod dsp_profile;
pub mod fsk;
pub mod preemphasis;
pub mod qam;
pub mod scrambler;
pub mod state_machine;
pub mod training;
pub mod trellis;
pub mod v32bis_trellis;
pub mod v34_constellation;
pub mod v34_mapping;
pub mod v34_training;
pub mod v34_trellis;
pub mod v42_detect;
pub mod v42_lapm;
pub mod v8;
pub mod v90_pcm;
pub mod v90_training;
pub mod v90_trellis;
pub mod wire;

/// Counts this thread's heap allocations, so a test can pin an allocation-free
/// path without other tests running in parallel disturbing the count.
#[cfg(test)]
pub(crate) mod test_alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
    }

    struct Counting;

    fn count() {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
    }

    // SAFETY: every method forwards unchanged to the system allocator; the
    // only addition is a thread-local counter that itself never allocates.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            count();
            // SAFETY: forwarded with the caller's own guarantees.
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            count();
            // SAFETY: forwarded with the caller's own guarantees.
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: forwarded with the caller's own guarantees.
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            count();
            // SAFETY: forwarded with the caller's own guarantees.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    #[global_allocator]
    static GLOBAL: Counting = Counting;

    /// Heap allocations (including reallocations) this thread has made.
    pub(crate) fn allocations() -> u64 {
        ALLOCATIONS.with(Cell::get)
    }
}

pub use dsp_profile::DspProfile;
pub use qam::{Constellation, QamConfig};
pub use v32bis_trellis::{V32bisTrellisEncoder, V32bisViterbiDecoder};

// ---------------------------------------------------------------------------
// V.32bis rate profiles
// ---------------------------------------------------------------------------

/// Trellis coding mode for a V.32bis rate profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V32bisTrellisMode {
    /// No trellis coding (4800 and 9600 bps).
    None,
    /// V.32bis 8-state convolutional trellis code (14400 bps).
    V32bis,
}

/// V.32bis rate profile: maps a negotiated speed to its constellation,
/// bits-per-symbol, and trellis mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V32bisRateProfile {
    pub speed_bps: u32,
    pub constellation: Constellation,
    /// Data bits per symbol (excludes the trellis-coded bit).
    pub data_bits_per_symbol: u8,
    pub trellis: V32bisTrellisMode,
}

/// 4800 bps: QAM4, 2 data bits/symbol, no trellis.
pub const V32BIS_PROFILE_4800: V32bisRateProfile = V32bisRateProfile {
    speed_bps: 4800,
    constellation: Constellation::Qam4,
    data_bits_per_symbol: 2,
    trellis: V32bisTrellisMode::None,
};

/// 9600 bps: QAM16, 4 data bits/symbol, no trellis.
pub const V32BIS_PROFILE_9600: V32bisRateProfile = V32bisRateProfile {
    speed_bps: 9600,
    constellation: Constellation::Qam16,
    data_bits_per_symbol: 4,
    trellis: V32bisTrellisMode::None,
};

/// 14400 bps: QAM128, 6 data bits/symbol, V.32bis trellis.
pub const V32BIS_PROFILE_14400: V32bisRateProfile = V32bisRateProfile {
    speed_bps: 14400,
    constellation: Constellation::Qam128,
    data_bits_per_symbol: 6,
    trellis: V32bisTrellisMode::V32bis,
};

impl V32bisRateProfile {
    /// Look up the rate profile for a given V.32bis speed.
    /// Returns `None` for speeds without an honest data path (7200, 12000).
    pub fn from_speed(speed_bps: u32) -> Option<&'static V32bisRateProfile> {
        match speed_bps {
            4800 => Some(&V32BIS_PROFILE_4800),
            9600 => Some(&V32BIS_PROFILE_9600),
            14400 => Some(&V32BIS_PROFILE_14400),
            _ => None,
        }
    }

    /// Bytes per 10ms tick at this rate.
    /// 2400 baud * 10ms = 24 symbols/tick.
    /// bytes = 24 * data_bits_per_symbol / 8
    pub fn bytes_per_tick(&self) -> usize {
        // 24 symbols * data_bits_per_symbol / 8 bits_per_byte
        (24 * self.data_bits_per_symbol as usize) / 8
    }

    /// Symbols per 10ms tick (always 24 at 2400 baud).
    pub fn symbols_per_tick(&self) -> usize {
        24
    }
}
pub use state_machine::{
    decimate_averaging, upsample_linear, ModemConfig, ModemDsp, ModemPhase, ModemSignals,
    SelectedFamily, SessionRole, TerminalStage, V42Outcome,
};
pub use v34_constellation::V34Constellation;
pub use v34_mapping::V34DirectMapper;
pub use v34_training::{V34Training, V34TrainingRole, V34TrainingState};
pub use v34_trellis::{V34DecodedSymbol, V34TrellisEncoder, V34ViterbiDecoder};
pub use v8::NegotiatedProtocol;
pub use wire::{TcpTransport, WireTransport};
