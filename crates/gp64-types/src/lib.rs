//! Shared types, traits, and utilities for the gp64 emulator ecosystem.
//!
//! This crate provides the foundation types that other gp64 crates depend on,
//! including cross-crate traits for decoupling, statistics collection, and utilities.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

// Fixed-capacity collections for snapshot-safe state
pub mod fixed_collections;
pub use fixed_collections::{
    FixedRing, FixedRingDrain, FixedRingIter, FixedString, FixedVec, FixedVecIntoIter,
};
pub mod byte_stream_marker;
pub mod debug_texture_mode;
pub use debug_texture_mode::DebugTextureMode;
pub mod drc_env;

pub use byte_stream_marker::ByteStreamMarker;

// BIOS Data Area constants
pub mod bda;

// BIOS F000 ROM layout constants (shared between the interpreter gate and
// the HLE dispatcher pass-through for the reserved keyboard stub range).
pub mod bios_layout;

// Gremlin RTC/CMOS/PIT/keyboard replay tape (HLD Step 2).
pub mod gremlin_tape;

// Gremlin session schema (HLD Step 4a).

// A052 span-write engagement census: width buckets shared by the funnel-side
// (gp64-cpu) and outcome-side (gp64-memory) counters.
pub mod span_write_census;

// Traits for cross-crate decoupling
pub mod traits;
pub use traits::{
    InterruptInvoker, IoCpuMode, IoPort, IoTraceContext, KeyboardAccess, MemoryAccess, MidiSynth,
    MidiWriter, PageAccess, PagedMemory, SmcNotify, SpeakerAccess, SynthId, TimerAccess,
    VgaPipelineState, VideoAccess,
};

// Statistics (moved from monitor/)
pub mod stats;
pub use stats::{opcode_name, EmulatorStats, ReferenceCpu, StatsCollector, REFERENCE_CPUS};

// Windowed-sinc resampler kernel computation (shared by SC-55 and MU50)
pub mod resampler;

// Standard MIDI File parsing and transport.
pub mod midi_sequence;

// Shared half-space triangle rasterizer core.
pub mod rasterizer;

// Utilities
pub mod host_dirs;
pub mod persistence;
pub mod util;
pub use persistence::{
    is_position_valid, load_monitor_window_settings, load_named_window_settings,
    load_window_settings, save_monitor_window_settings, save_named_window_settings,
    save_window_settings, window_settings_storage_key, WindowSettings,
};

// Test utilities (feature-gated)
#[cfg(any(test, feature = "test-util"))]
pub mod test_util;

// =============================================================================
// Tracing helpers
// =============================================================================

/// Generate an `#[inline]` function that checks whether a `tracing` target is
/// enabled at a given level.  Eliminates the boilerplate of writing out
/// `fn foo() -> bool { tracing::enabled!(target: "...", Level::...) }` by hand.
///
/// Supports four visibility forms:
/// - `define_trace_checker!(name, target, level)` — private (default)
/// - `define_trace_checker!(pub(super) name, target, level)`
/// - `define_trace_checker!(pub(crate) name, target, level)`
/// - `define_trace_checker!(pub name, target, level)`
#[macro_export]
macro_rules! define_trace_checker {
    ($fn_name:ident, $target:expr, $level:expr) => {
        #[inline]
        fn $fn_name() -> bool {
            tracing::enabled!(target: $target, $level)
        }
    };
    (pub(super) $fn_name:ident, $target:expr, $level:expr) => {
        #[inline]
        pub(super) fn $fn_name() -> bool {
            tracing::enabled!(target: $target, $level)
        }
    };
    (pub(crate) $fn_name:ident, $target:expr, $level:expr) => {
        #[inline]
        pub(crate) fn $fn_name() -> bool {
            tracing::enabled!(target: $target, $level)
        }
    };
    (pub $fn_name:ident, $target:expr, $level:expr) => {
        #[inline]
        pub fn $fn_name() -> bool {
            tracing::enabled!(target: $target, $level)
        }
    };
}

// Debug tracing for memory writes
pub mod debug_trace {
    use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};

    /// Last executed instruction's CS:IP for debugging memory writes
    pub static DEBUG_LAST_CS: AtomicU16 = AtomicU16::new(0);
    pub static DEBUG_LAST_IP: AtomicU32 = AtomicU32::new(0);

    /// Set the last executed instruction location (call before each instruction)
    #[inline]
    pub fn set_last_instruction(cs: u16, ip: u32) {
        DEBUG_LAST_CS.store(cs, Ordering::Relaxed);
        DEBUG_LAST_IP.store(ip, Ordering::Relaxed);
    }

    /// Get the last executed instruction location
    #[inline]
    pub fn get_last_instruction() -> (u16, u32) {
        (
            DEBUG_LAST_CS.load(Ordering::Relaxed),
            DEBUG_LAST_IP.load(Ordering::Relaxed),
        )
    }
}

// ── Capture event types for headless debugging ──────────────────────

/// What a capture trigger watches for (pattern with optional filters).
#[derive(Clone, Debug)]
pub enum CaptureEventPattern {
    /// Any VGA mode change
    VgaModeChange,
    /// Every N frames (checked by frame number, not events)
    FrameInterval { every_n: u32 },
    /// Execution breakpoint fires (optionally filtered by bp ID)
    BreakpointHit { bp_id_filter: Option<u32> },
}

/// What actually happened (concrete values, no filter fields).
/// Note: FrameInterval is NOT here - it's checked by frame number, not by event.
#[derive(Clone, Debug)]
pub enum CaptureEventKind {
    VgaModeChange { old_mode: u8, new_mode: u8 },
    BreakpointHit { bp_id: u32 },
}

/// A fired event, queued for processing at frame boundary.
#[derive(Clone, Debug)]
pub struct FiredCaptureEvent {
    pub kind: CaptureEventKind,
}

/// Check if a capture event pattern matches a fired event.
pub fn pattern_matches(pattern: &CaptureEventPattern, event: &CaptureEventKind) -> bool {
    match (pattern, event) {
        (CaptureEventPattern::VgaModeChange, CaptureEventKind::VgaModeChange { .. }) => true,

        (
            CaptureEventPattern::BreakpointHit { bp_id_filter: None },
            CaptureEventKind::BreakpointHit { .. },
        ) => true,
        (
            CaptureEventPattern::BreakpointHit {
                bp_id_filter: Some(f),
            },
            CaptureEventKind::BreakpointHit { bp_id },
        ) => *f == *bp_id,

        // FrameInterval is handled separately (not event-driven)
        // All other mismatched pattern/event combinations
        _ => false,
    }
}

// ── Sound environment configuration (data transfer objects) ──────────

/// Top-level sound environment configuration for populating DOS environment
/// variables (BLASTER, ULTRASND). This is a data transfer object describing
/// which sound devices are enabled and their settings — it is NOT a hardware
/// device struct.
#[derive(Debug, Clone, Default)]
pub struct SoundEnvConfig {
    /// Sound Blaster configuration. `None` means SB is not enabled.
    pub sound_blaster: Option<SoundBlasterEnv>,
    /// Gravis Ultrasound configuration. `None` means GUS is not enabled.
    pub gravis_ultrasound: Option<GusEnv>,
}

/// Sound Blaster environment variable parameters for generating the `BLASTER`
/// environment string (e.g. `A220 I5 D1 H5 T6 P330`). This is a data transfer
/// object — it does not control hardware emulation directly.
#[derive(Debug, Clone)]
pub struct SoundBlasterEnv {
    /// Base I/O port (e.g. 0x220).
    pub port: u16,
    /// IRQ number (e.g. 5).
    pub irq: u8,
    /// 8-bit DMA channel (e.g. 1).
    pub dma: u8,
    /// 16-bit (high) DMA channel (e.g. 5).
    pub hdma: u8,
    /// Card type: 1=SB1.0, 2=SB1.5, 3=SB2.0, 4=SBPro, 5=SBPro2, 6=SB16.
    pub sb_type: u8,
    /// EMU8000 base port (e.g. 0x620). Use 0 to omit the `E` field from the
    /// BLASTER string. Default for AWE32 is 0x620 (SB base + 0x400).
    pub emu8000_port: u16,
    /// MPU-401 MIDI port (e.g. 0x330). Use 0 to omit the `P` field from the
    /// BLASTER string.
    pub mpu_port: u16,
}

/// Gravis Ultrasound environment variable parameters for generating the
/// `ULTRASND` environment string (e.g. `240,1,1,5,5`). This is a data transfer
/// object — it does not control hardware emulation directly.
#[derive(Debug, Clone)]
pub struct GusEnv {
    /// Base I/O port (e.g. 0x240).
    pub port: u16,
    /// DMA channel for wavetable playback.
    pub play_dma: u8,
    /// DMA channel for CODEC recording.
    pub rec_dma: u8,
    /// IRQ for wavetable playback.
    pub play_irq: u8,
    /// IRQ for MIDI / recording.
    pub rec_irq: u8,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod fixed_collections_tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::debug_trace;

    #[test]
    fn test_debug_trace_round_trip() {
        let (old_cs, old_ip) = debug_trace::get_last_instruction();

        debug_trace::set_last_instruction(0x1234, 0x56789);
        assert_eq!(debug_trace::get_last_instruction(), (0x1234, 0x56789));

        debug_trace::set_last_instruction(old_cs, old_ip);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod capture_tests {
    use super::*;

    #[test]
    fn test_pattern_matches_vga_mode_change() {
        let pattern = CaptureEventPattern::VgaModeChange;
        let event = CaptureEventKind::VgaModeChange {
            old_mode: 0x03,
            new_mode: 0x13,
        };
        assert!(pattern_matches(&pattern, &event));
    }

    #[test]
    fn capture_filter_matches_constructed_ids() {
        for id in [0, 1, 7, 42, u32::MAX] {
            let event = CaptureEventKind::BreakpointHit { bp_id: id };
            assert!(pattern_matches(
                &CaptureEventPattern::BreakpointHit { bp_id_filter: None },
                &event,
            ));
            assert!(pattern_matches(
                &CaptureEventPattern::BreakpointHit {
                    bp_id_filter: Some(id)
                },
                &event,
            ));
            assert!(!pattern_matches(
                &CaptureEventPattern::BreakpointHit {
                    bp_id_filter: Some(id.wrapping_add(1))
                },
                &event,
            ));
        }
    }

    #[test]
    fn test_pattern_matches_breakpoint_any() {
        let pattern = CaptureEventPattern::BreakpointHit { bp_id_filter: None };
        let event = CaptureEventKind::BreakpointHit { bp_id: 42 };
        assert!(pattern_matches(&pattern, &event));
    }

    #[test]
    fn test_pattern_matches_breakpoint_specific() {
        let pattern = CaptureEventPattern::BreakpointHit {
            bp_id_filter: Some(42),
        };
        assert!(pattern_matches(
            &pattern,
            &CaptureEventKind::BreakpointHit { bp_id: 42 }
        ));
        assert!(!pattern_matches(
            &pattern,
            &CaptureEventKind::BreakpointHit { bp_id: 99 }
        ));
    }

    #[test]
    fn test_pattern_frame_interval_does_not_match_events() {
        let pattern = CaptureEventPattern::FrameInterval { every_n: 10 };
        // FrameInterval is handled separately, so it should not match any event kind
        assert!(!pattern_matches(
            &pattern,
            &CaptureEventKind::VgaModeChange {
                old_mode: 3,
                new_mode: 0x13
            }
        ));
        assert!(!pattern_matches(
            &pattern,
            &CaptureEventKind::BreakpointHit { bp_id: 1 }
        ));
    }

    #[test]
    fn test_pattern_no_cross_match() {
        let pattern = CaptureEventPattern::VgaModeChange;
        let event = CaptureEventKind::BreakpointHit { bp_id: 1 };
        assert!(!pattern_matches(&pattern, &event));
    }

    // ---- BreakpointHit pattern matching ----

    #[test]
    fn test_pattern_matches_breakpoint_any_v2() {
        let pattern = CaptureEventPattern::BreakpointHit { bp_id_filter: None };
        let event = CaptureEventKind::BreakpointHit { bp_id: 42 };
        assert!(pattern_matches(&pattern, &event));
    }

    #[test]
    fn test_pattern_matches_breakpoint_specific_v2() {
        let pattern = CaptureEventPattern::BreakpointHit {
            bp_id_filter: Some(42),
        };
        assert!(pattern_matches(
            &pattern,
            &CaptureEventKind::BreakpointHit { bp_id: 42 }
        ));
        assert!(!pattern_matches(
            &pattern,
            &CaptureEventKind::BreakpointHit { bp_id: 43 }
        ));
    }

    // ---- FrameInterval pattern never matches events ----

    #[test]
    fn test_pattern_frame_interval_never_matches_events() {
        let pattern = CaptureEventPattern::FrameInterval { every_n: 10 };
        // Should not match any event kind
        assert!(!pattern_matches(
            &pattern,
            &CaptureEventKind::VgaModeChange {
                old_mode: 3,
                new_mode: 0x13
            }
        ));
        assert!(!pattern_matches(
            &pattern,
            &CaptureEventKind::BreakpointHit { bp_id: 1 }
        ));
    }

    // ---- FiredCaptureEvent ----

    #[test]
    fn test_fired_capture_event_debug_clone() {
        let event = FiredCaptureEvent {
            kind: CaptureEventKind::VgaModeChange {
                old_mode: 3,
                new_mode: 0x13,
            },
        };
        let cloned = event.clone();
        let dbg = format!("{cloned:?}");
        assert!(dbg.contains("VgaModeChange"));
    }

    // ---- CaptureEventPattern Debug/Clone coverage ----

    #[test]
    fn test_capture_event_pattern_debug() {
        let patterns = [
            CaptureEventPattern::VgaModeChange,
            CaptureEventPattern::FrameInterval { every_n: 10 },
            CaptureEventPattern::BreakpointHit { bp_id_filter: None },
        ];
        for p in &patterns {
            let dbg = format!("{p:?}");
            assert!(!dbg.is_empty());
        }
    }

    // ---- SoundEnvConfig coverage ----

    #[test]
    fn test_sound_env_config_default() {
        let config = SoundEnvConfig::default();
        assert!(config.sound_blaster.is_none());
        assert!(config.gravis_ultrasound.is_none());
    }

    #[test]
    fn test_sound_env_config_debug_clone() {
        let config = SoundEnvConfig {
            sound_blaster: Some(SoundBlasterEnv {
                port: 0x220,
                irq: 5,
                dma: 1,
                hdma: 5,
                sb_type: 6,
                emu8000_port: 0x620,
                mpu_port: 0x330,
            }),
            gravis_ultrasound: Some(GusEnv {
                port: 0x240,
                play_dma: 1,
                rec_dma: 1,
                play_irq: 5,
                rec_irq: 5,
            }),
        };
        let dbg = format!("{config:?}");
        assert!(dbg.contains("SoundBlasterEnv"));
        assert!(dbg.contains("GusEnv"));
        let cloned = config.clone();
        assert_eq!(cloned.sound_blaster.as_ref().unwrap().port, 0x220);
    }
}
