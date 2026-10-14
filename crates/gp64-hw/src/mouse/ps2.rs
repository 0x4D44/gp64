//! PS/2 mouse hardware emulation
//!
//! This module implements the PS/2 mouse protocol at the hardware level.
//! It is separate from the INT 33h driver (in `gp64-dos`) - this provides
//! the low-level interface that device drivers access directly through
//! the 8042 controller.

use gp64_types::FixedRing;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Mutex;

// ============================================================================
// SharedMouseState - Thread-safe mouse input from GUI to emulator
// ============================================================================

/// Sentinel value indicating no valid framebuffer position has been set.
const FB_POS_INVALID: u64 = u64::MAX;
const DEFAULT_MAX_MICKEYS_PER_TICK: i32 = 32;
// Native protected-mode mouse drivers may apply acceleration to larger PS/2
// deltas. Synthetic control positioning has no guest-coordinate feedback, so
// keep it at one mickey per report: the slow path is deterministic and cannot
// cross an acceleration threshold.
const SYNTHETIC_MAX_MICKEYS_PER_TICK: i32 = 1;
const DEFAULT_MOUSE_SYNC_GAIN: f64 = 1.0;
const MIN_MOUSE_SYNC_GAIN: f64 = 0.1;
const MAX_MOUSE_SYNC_GAIN: f64 = 8.0;
const MIN_EMITTED_FOR_GAIN_SAMPLE: i32 = 4;
const FIRST_OBSERVATION_CONFIDENCE: f64 = 0.40;
const MAX_OBSERVED_CONFIDENCE: f64 = 0.95;

// The widest per-axis motion one PS/2 report can carry (9-bit signed).
const MAX_REPORTABLE_MOTION: i32 = 255;
const MIN_REPORTABLE_MOTION: i32 = -256;

fn pack_fb_position(x: i32, y: i32) -> u64 {
    ((x as u32 as u64) << 32) | (y as u32 as u64)
}

fn unpack_fb_position(packed: u64) -> Option<(i32, i32)> {
    if packed == FB_POS_INVALID {
        return None;
    }
    let x = (packed >> 32) as u32 as i32;
    let y = (packed & 0xFFFFFFFF) as u32 as i32;
    Some((x, y))
}

/// Persistent absolute framebuffer target for real-boot seamless mouse input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeamlessMouseTarget {
    pub x: i32,
    pub y: i32,
    pub seq: u64,
}

#[derive(Debug, Default)]
struct SeamlessTargetState {
    target: Option<(i32, i32)>,
    seq: u64,
}

#[derive(Clone, Copy, Debug)]
struct GuestAxisRange {
    min: f64,
    max: f64,
}

impl GuestAxisRange {
    const fn new(min: f64, max: f64) -> Self {
        Self { min, max }
    }

    fn from_int33_limits(first: u16, second: u16) -> Self {
        let mut min = first as f64;
        let mut max = second as f64;
        if min > max {
            std::mem::swap(&mut min, &mut max);
        }
        Self { min, max }
    }

    fn map_fb(self, fb: i32, fb_extent: usize) -> f64 {
        if fb_extent <= 1 {
            return self.min;
        }
        let fb = fb.clamp(0, fb_extent as i32 - 1) as f64;
        self.min + fb * (self.max - self.min) / (fb_extent as f64 - 1.0)
    }

    fn clamp(self, value: f64) -> f64 {
        value.clamp(self.min, self.max)
    }

    fn same_bounds(self, other: Self) -> bool {
        self.min == other.min && self.max == other.max
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EstimateFreshness {
    None,
    SyntheticFromTarget,
    ObservedInt33,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeSource {
    ObservedInt33,
    AssumedVideoModeDefault,
    AssumedFallback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GainUpdateDecision {
    None,
    CalibrationDisabled,
    NoPreviousObservation,
    InsufficientEmission,
    BoundaryClamped,
    InvalidDelta,
    DirectionMismatch,
    SaneRangeRejected,
    Accepted,
}

/// Read-only calibration state for real-boot seamless mouse diagnostics.
#[derive(Clone, Copy, Debug)]
pub struct RealBootMouseSyncDiagnostics {
    pub freshness: EstimateFreshness,
    pub confidence: f64,
    pub gain_x: f64,
    pub gain_y: f64,
    pub last_observed_x: Option<f64>,
    pub last_observed_y: Option<f64>,
    pub emitted_since_observation_x: i32,
    pub emitted_since_observation_y: i32,
    pub last_emitted_x: i32,
    pub last_emitted_y: i32,
    pub range_x: (f64, f64),
    pub range_y: (f64, f64),
    pub range_source_x: RangeSource,
    pub range_source_y: RangeSource,
    pub residual_x: f64,
    pub residual_y: f64,
    pub gain_attribution_valid: bool,
    pub last_gain_update_x: GainUpdateDecision,
    pub last_gain_update_y: GainUpdateDecision,
}

/// Result of one estimate-only real-boot seamless mouse sync tick.
#[derive(Clone, Copy, Debug)]
pub struct RealBootMouseSyncStep {
    pub target: Option<SeamlessMouseTarget>,
    pub target_guest: Option<(f64, f64)>,
    pub emitted_x: i32,
    pub emitted_y: i32,
    pub estimate_x: f64,
    pub estimate_y: f64,
    pub residual_x: f64,
    pub residual_y: f64,
}

impl RealBootMouseSyncStep {
    pub fn emitted(self) -> (i32, i32) {
        (self.emitted_x, self.emitted_y)
    }
}

impl Default for RealBootMouseSyncStep {
    fn default() -> Self {
        Self {
            target: None,
            target_guest: None,
            emitted_x: 0,
            emitted_y: 0,
            estimate_x: 0.0,
            estimate_y: 0.0,
            residual_x: 0.0,
            residual_y: 0.0,
        }
    }
}

/// Estimate-only servo that turns an absolute framebuffer target into bounded
/// relative PS/2 mickeys for real-boot DOS mouse drivers.
pub struct RealBootMouseSync {
    target_seq_seen: Option<u64>,
    estimate_x: f64,
    estimate_y: f64,
    estimate_freshness: EstimateFreshness,
    confidence: f64,
    gain_x: f64,
    gain_y: f64,
    residual_x: f64,
    residual_y: f64,
    last_observed_x: Option<f64>,
    last_observed_y: Option<f64>,
    emitted_since_observation_x: i32,
    emitted_since_observation_y: i32,
    last_emitted_x: i32,
    last_emitted_y: i32,
    range_x: GuestAxisRange,
    range_y: GuestAxisRange,
    range_source_x: RangeSource,
    range_source_y: RangeSource,
    max_mickeys_per_tick: i32,
    gain_attribution_valid: bool,
    last_gain_update_x: GainUpdateDecision,
    last_gain_update_y: GainUpdateDecision,
}

impl RealBootMouseSync {
    pub fn new() -> Self {
        Self {
            target_seq_seen: None,
            estimate_x: 0.0,
            estimate_y: 0.0,
            estimate_freshness: EstimateFreshness::None,
            confidence: 0.0,
            gain_x: DEFAULT_MOUSE_SYNC_GAIN,
            gain_y: DEFAULT_MOUSE_SYNC_GAIN,
            residual_x: 0.0,
            residual_y: 0.0,
            last_observed_x: None,
            last_observed_y: None,
            emitted_since_observation_x: 0,
            emitted_since_observation_y: 0,
            last_emitted_x: 0,
            last_emitted_y: 0,
            range_x: GuestAxisRange::new(0.0, 639.0),
            range_y: GuestAxisRange::new(0.0, 199.0),
            range_source_x: RangeSource::AssumedFallback,
            range_source_y: RangeSource::AssumedFallback,
            max_mickeys_per_tick: DEFAULT_MAX_MICKEYS_PER_TICK,
            gain_attribution_valid: true,
            last_gain_update_x: GainUpdateDecision::None,
            last_gain_update_y: GainUpdateDecision::None,
        }
    }

    pub fn assume_video_mode_default_range(&mut self, video_mode: u8, fb_dims: (usize, usize)) {
        let Some((range_x, range_y)) = assumed_video_mode_default_ranges(video_mode, fb_dims)
        else {
            self.assume_fallback_ranges();
            return;
        };

        if self.range_source_x != RangeSource::ObservedInt33
            && (self.range_source_x != RangeSource::AssumedVideoModeDefault
                || !self.range_x.same_bounds(range_x))
        {
            self.range_x = range_x;
            self.range_source_x = RangeSource::AssumedVideoModeDefault;
            self.residual_x = 0.0;
            self.emitted_since_observation_x = 0;
        }
        if self.range_source_y != RangeSource::ObservedInt33
            && (self.range_source_y != RangeSource::AssumedVideoModeDefault
                || !self.range_y.same_bounds(range_y))
        {
            self.range_y = range_y;
            self.range_source_y = RangeSource::AssumedVideoModeDefault;
            self.residual_y = 0.0;
            self.emitted_since_observation_y = 0;
        }
    }

    pub fn reset(&mut self) {
        self.target_seq_seen = None;
        self.estimate_x = 0.0;
        self.estimate_y = 0.0;
        self.estimate_freshness = EstimateFreshness::None;
        self.confidence = 0.0;
        self.residual_x = 0.0;
        self.residual_y = 0.0;
        self.last_observed_x = None;
        self.last_observed_y = None;
        self.emitted_since_observation_x = 0;
        self.emitted_since_observation_y = 0;
        self.last_emitted_x = 0;
        self.last_emitted_y = 0;
        self.gain_attribution_valid = true;
        self.last_gain_update_x = GainUpdateDecision::None;
        self.last_gain_update_y = GainUpdateDecision::None;
    }

    pub fn full_reset(&mut self) {
        self.full_reset_inner(true);
    }

    pub fn reset_for_mode_change(&mut self) {
        self.full_reset_inner(false);
    }

    fn full_reset_inner(&mut self, reset_gains: bool) {
        self.target_seq_seen = None;
        self.estimate_x = 0.0;
        self.estimate_y = 0.0;
        self.estimate_freshness = EstimateFreshness::None;
        self.confidence = 0.0;
        if reset_gains {
            self.gain_x = DEFAULT_MOUSE_SYNC_GAIN;
            self.gain_y = DEFAULT_MOUSE_SYNC_GAIN;
        } else {
            self.gain_x = self.gain_x.clamp(MIN_MOUSE_SYNC_GAIN, MAX_MOUSE_SYNC_GAIN);
            self.gain_y = self.gain_y.clamp(MIN_MOUSE_SYNC_GAIN, MAX_MOUSE_SYNC_GAIN);
        }
        self.residual_x = 0.0;
        self.residual_y = 0.0;
        self.last_observed_x = None;
        self.last_observed_y = None;
        self.emitted_since_observation_x = 0;
        self.emitted_since_observation_y = 0;
        self.last_emitted_x = 0;
        self.last_emitted_y = 0;
        self.assume_fallback_ranges();
        self.gain_attribution_valid = true;
        self.last_gain_update_x = GainUpdateDecision::None;
        self.last_gain_update_y = GainUpdateDecision::None;
    }

    fn assume_fallback_ranges(&mut self) {
        self.range_x = GuestAxisRange::new(0.0, 639.0);
        self.range_y = GuestAxisRange::new(0.0, 199.0);
        self.range_source_x = RangeSource::AssumedFallback;
        self.range_source_y = RangeSource::AssumedFallback;
    }

    pub fn abandon_target(&mut self) {
        self.target_seq_seen = None;
        self.residual_x = 0.0;
        self.residual_y = 0.0;
        self.emitted_since_observation_x = 0;
        self.emitted_since_observation_y = 0;
        self.last_emitted_x = 0;
        self.last_emitted_y = 0;
        if self.estimate_freshness == EstimateFreshness::SyntheticFromTarget {
            self.estimate_freshness = EstimateFreshness::None;
            self.confidence = 0.0;
        }
        self.gain_attribution_valid = false;
        self.last_gain_update_x = GainUpdateDecision::CalibrationDisabled;
        self.last_gain_update_y = GainUpdateDecision::CalibrationDisabled;
    }

    pub fn invalidate_gain_attribution(&mut self) {
        self.gain_attribution_valid = false;
        self.emitted_since_observation_x = 0;
        self.emitted_since_observation_y = 0;
        self.residual_x = 0.0;
        self.residual_y = 0.0;
        self.last_gain_update_x = GainUpdateDecision::CalibrationDisabled;
        self.last_gain_update_y = GainUpdateDecision::CalibrationDisabled;
    }

    pub fn observe_horizontal_range(&mut self, min: u16, max: u16) {
        let was_synthetic = self.estimate_freshness == EstimateFreshness::SyntheticFromTarget;
        self.range_x = GuestAxisRange::from_int33_limits(min, max);
        self.range_source_x = RangeSource::ObservedInt33;
        self.invalidate_gain_attribution();
        if was_synthetic {
            self.reset();
            return;
        }
        if self.is_position_seeded() {
            self.estimate_x = self.range_x.clamp(self.estimate_x);
        }
        self.last_observed_x = self.last_observed_x.map(|x| self.range_x.clamp(x));
        self.residual_x = 0.0;
        self.emitted_since_observation_x = 0;
    }

    pub fn observe_vertical_range(&mut self, min: u16, max: u16) {
        let was_synthetic = self.estimate_freshness == EstimateFreshness::SyntheticFromTarget;
        self.range_y = GuestAxisRange::from_int33_limits(min, max);
        self.range_source_y = RangeSource::ObservedInt33;
        self.invalidate_gain_attribution();
        if was_synthetic {
            self.reset();
            return;
        }
        if self.is_position_seeded() {
            self.estimate_y = self.range_y.clamp(self.estimate_y);
        }
        self.last_observed_y = self.last_observed_y.map(|y| self.range_y.clamp(y));
        self.residual_y = 0.0;
        self.emitted_since_observation_y = 0;
    }

    pub fn observe_position(&mut self, x: u16, y: u16) {
        self.observe_position_inner(x, y, true);
    }

    pub fn observe_uncontrolled_position(&mut self, x: u16, y: u16) {
        self.observe_position_inner(x, y, false);
    }

    fn observe_position_inner(&mut self, x: u16, y: u16, allow_gain_update: bool) {
        let observed_x = self.range_x.clamp(x as f64);
        let observed_y = self.range_y.clamp(y as f64);
        let mut accepted_gain_sample = false;

        let calibration_allowed = allow_gain_update && self.gain_attribution_valid;
        if calibration_allowed {
            if let Some(previous_x) = self.last_observed_x {
                match gain_sample(
                    previous_x,
                    observed_x,
                    self.emitted_since_observation_x,
                    self.range_x,
                ) {
                    Ok(gain) => {
                        self.gain_x = smooth_gain(self.gain_x, gain);
                        self.last_gain_update_x = GainUpdateDecision::Accepted;
                        accepted_gain_sample = true;
                    }
                    Err(reason) => {
                        self.last_gain_update_x = reason;
                    }
                }
            } else {
                self.last_gain_update_x = GainUpdateDecision::NoPreviousObservation;
            }
            if let Some(previous_y) = self.last_observed_y {
                match gain_sample(
                    previous_y,
                    observed_y,
                    self.emitted_since_observation_y,
                    self.range_y,
                ) {
                    Ok(gain) => {
                        self.gain_y = smooth_gain(self.gain_y, gain);
                        self.last_gain_update_y = GainUpdateDecision::Accepted;
                        accepted_gain_sample = true;
                    }
                    Err(reason) => {
                        self.last_gain_update_y = reason;
                    }
                }
            } else {
                self.last_gain_update_y = GainUpdateDecision::NoPreviousObservation;
            }
        } else {
            self.last_gain_update_x = GainUpdateDecision::CalibrationDisabled;
            self.last_gain_update_y = GainUpdateDecision::CalibrationDisabled;
        }

        self.estimate_x = observed_x;
        self.estimate_y = observed_y;
        self.estimate_freshness = EstimateFreshness::ObservedInt33;
        self.confidence = self.confidence.max(FIRST_OBSERVATION_CONFIDENCE);
        if accepted_gain_sample {
            self.confidence = (self.confidence + 0.10).min(MAX_OBSERVED_CONFIDENCE);
        }
        self.last_observed_x = Some(observed_x);
        self.last_observed_y = Some(observed_y);
        self.emitted_since_observation_x = 0;
        self.emitted_since_observation_y = 0;
        self.residual_x = 0.0;
        self.residual_y = 0.0;
        self.gain_attribution_valid = true;
    }

    pub fn active_target_seq(&self) -> Option<u64> {
        self.target_seq_seen
    }

    pub fn is_position_seeded(&self) -> bool {
        self.estimate_freshness == EstimateFreshness::ObservedInt33
    }

    /// Give an unobserved synchronizer a low-confidence framebuffer-space
    /// starting point. This is used by control automation for native PS/2
    /// guests, whose cursor position cannot be observed through INT 33h.
    /// A real INT 33h observation always replaces this synthetic estimate.
    pub fn seed_synthetic_framebuffer_position(
        &mut self,
        fb_x: i32,
        fb_y: i32,
        fb_dims: (usize, usize),
    ) {
        if self.estimate_freshness != EstimateFreshness::None {
            return;
        }

        let (fb_w, fb_h) = fb_dims;
        if fb_w == 0 || fb_h == 0 {
            return;
        }

        self.estimate_x = self.range_x.map_fb(fb_x, fb_w);
        self.estimate_y = self.range_y.map_fb(fb_y, fb_h);
        self.estimate_freshness = EstimateFreshness::SyntheticFromTarget;
        self.confidence = 0.0;
        self.residual_x = 0.0;
        self.residual_y = 0.0;
    }

    pub fn diagnostics(&self) -> RealBootMouseSyncDiagnostics {
        RealBootMouseSyncDiagnostics {
            freshness: self.estimate_freshness,
            confidence: self.confidence,
            gain_x: self.gain_x,
            gain_y: self.gain_y,
            last_observed_x: self.last_observed_x,
            last_observed_y: self.last_observed_y,
            emitted_since_observation_x: self.emitted_since_observation_x,
            emitted_since_observation_y: self.emitted_since_observation_y,
            last_emitted_x: self.last_emitted_x,
            last_emitted_y: self.last_emitted_y,
            range_x: (self.range_x.min, self.range_x.max),
            range_y: (self.range_y.min, self.range_y.max),
            range_source_x: self.range_source_x,
            range_source_y: self.range_source_y,
            residual_x: self.residual_x,
            residual_y: self.residual_y,
            gain_attribution_valid: self.gain_attribution_valid,
            last_gain_update_x: self.last_gain_update_x,
            last_gain_update_y: self.last_gain_update_y,
        }
    }

    pub fn tick(
        &mut self,
        target: Option<SeamlessMouseTarget>,
        fb_dims: (usize, usize),
    ) -> RealBootMouseSyncStep {
        let Some(target) = target else {
            self.abandon_target();
            return self.step(None, None, 0, 0);
        };

        let (fb_w, fb_h) = fb_dims;
        if fb_w == 0 || fb_h == 0 {
            self.record_emission(0, 0);
            return self.step(Some(target), None, 0, 0);
        }

        if self.target_seq_seen != Some(target.seq) {
            // Gain samples are deliberately tied to one stable target sequence.
            // Continuous host motion still gets servo correction, but it starts
            // a fresh attribution window instead of learning from mixed targets.
            self.invalidate_gain_attribution();
            self.target_seq_seen = Some(target.seq);
            self.residual_x = 0.0;
            self.residual_y = 0.0;
        }

        let target_guest_x = self.range_x.map_fb(target.x, fb_w);
        let target_guest_y = self.range_y.map_fb(target.y, fb_h);
        if self.estimate_freshness == EstimateFreshness::None {
            // Low-confidence fallback: the first target seeds the synthetic
            // estimate without movement. Later synthetic ticks may move
            // open-loop, but only true INT 33h observations can calibrate.
            self.estimate_x = target_guest_x;
            self.estimate_y = target_guest_y;
            self.estimate_freshness = EstimateFreshness::SyntheticFromTarget;
            self.residual_x = 0.0;
            self.residual_y = 0.0;
            self.record_emission(0, 0);
            return self.step(Some(target), Some((target_guest_x, target_guest_y)), 0, 0);
        }

        let max_mickeys_per_tick =
            if self.estimate_freshness == EstimateFreshness::SyntheticFromTarget {
                SYNTHETIC_MAX_MICKEYS_PER_TICK
            } else {
                self.max_mickeys_per_tick
            };
        let (emit_x, residual_x) = quantize_axis(
            target_guest_x - self.estimate_x,
            self.gain_x,
            self.residual_x,
            max_mickeys_per_tick,
        );
        let (emit_y, residual_y) = quantize_axis(
            target_guest_y - self.estimate_y,
            self.gain_y,
            self.residual_y,
            max_mickeys_per_tick,
        );

        self.residual_x = residual_x;
        self.residual_y = residual_y;
        self.estimate_x += emit_x as f64 * self.gain_x;
        self.estimate_y += emit_y as f64 * self.gain_y;
        self.record_emission(emit_x, emit_y);

        self.step(
            Some(target),
            Some((target_guest_x, target_guest_y)),
            emit_x,
            emit_y,
        )
    }

    fn step(
        &self,
        target: Option<SeamlessMouseTarget>,
        target_guest: Option<(f64, f64)>,
        emitted_x: i32,
        emitted_y: i32,
    ) -> RealBootMouseSyncStep {
        RealBootMouseSyncStep {
            target,
            target_guest,
            emitted_x,
            emitted_y,
            estimate_x: self.estimate_x,
            estimate_y: self.estimate_y,
            residual_x: self.residual_x,
            residual_y: self.residual_y,
        }
    }

    fn record_emission(&mut self, emitted_x: i32, emitted_y: i32) {
        self.last_emitted_x = emitted_x;
        self.last_emitted_y = emitted_y;
        self.emitted_since_observation_x =
            self.emitted_since_observation_x.saturating_add(emitted_x);
        self.emitted_since_observation_y =
            self.emitted_since_observation_y.saturating_add(emitted_y);
    }

    pub fn record_gated_no_emission(&mut self) {
        self.last_emitted_x = 0;
        self.last_emitted_y = 0;
    }

    #[cfg(test)]
    fn set_gain_for_test(&mut self, gain_x: f64, gain_y: f64) {
        self.gain_x = gain_x;
        self.gain_y = gain_y;
    }
}

impl Default for RealBootMouseSync {
    fn default() -> Self {
        Self::new()
    }
}

fn assumed_video_mode_default_ranges(
    video_mode: u8,
    fb_dims: (usize, usize),
) -> Option<(GuestAxisRange, GuestAxisRange)> {
    let (x_max, y_max) = match video_mode {
        0x00 | 0x01 | 0x02 | 0x03 | 0x04 | 0x05 | 0x06 | 0x0D | 0x0E | 0x13 => (639.0, 199.0),
        0x0F | 0x10 => (639.0, 349.0),
        0x11 | 0x12 => (639.0, 479.0),
        _ => {
            let (fb_w, fb_h) = fb_dims;
            if fb_w == 0 || fb_h == 0 {
                return None;
            }
            let x_max = if fb_w <= 320 {
                fb_w.saturating_mul(2).saturating_sub(1)
            } else {
                fb_w.saturating_sub(1)
            };
            (x_max as f64, fb_h.saturating_sub(1) as f64)
        }
    };

    Some((
        GuestAxisRange::new(0.0, x_max),
        GuestAxisRange::new(0.0, y_max),
    ))
}

fn quantize_axis(error: f64, gain: f64, residual: f64, max_mickeys: i32) -> (i32, f64) {
    if !error.is_finite() || !gain.is_finite() || gain.abs() < f64::EPSILON {
        return (0, 0.0);
    }

    let desired = error / gain + residual;
    if !desired.is_finite() {
        return (0, 0.0);
    }

    let quantized = desired.trunc() as i32;
    let emitted = quantized.clamp(-max_mickeys, max_mickeys);
    let residual = (desired - quantized as f64).clamp(-0.999_999, 0.999_999);
    (emitted, residual)
}

fn gain_sample(
    previous_observed: f64,
    observed: f64,
    emitted_since_observation: i32,
    range: GuestAxisRange,
) -> Result<f64, GainUpdateDecision> {
    if emitted_since_observation.abs() < MIN_EMITTED_FOR_GAIN_SAMPLE {
        return Err(GainUpdateDecision::InsufficientEmission);
    }
    if at_range_boundary(previous_observed, range) || at_range_boundary(observed, range) {
        return Err(GainUpdateDecision::BoundaryClamped);
    }

    let observed_delta = observed - previous_observed;
    if !observed_delta.is_finite() || observed_delta == 0.0 {
        return Err(GainUpdateDecision::InvalidDelta);
    }
    if observed_delta.signum() != (emitted_since_observation as f64).signum() {
        return Err(GainUpdateDecision::DirectionMismatch);
    }

    let sample = observed_delta / emitted_since_observation as f64;
    if (MIN_MOUSE_SYNC_GAIN..=MAX_MOUSE_SYNC_GAIN).contains(&sample) {
        Ok(sample)
    } else {
        Err(GainUpdateDecision::SaneRangeRejected)
    }
}

fn at_range_boundary(value: f64, range: GuestAxisRange) -> bool {
    value <= range.min || value >= range.max
}

fn smooth_gain(old: f64, sample: f64) -> f64 {
    (old * 0.8 + sample * 0.2).clamp(MIN_MOUSE_SYNC_GAIN, MAX_MOUSE_SYNC_GAIN)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SharedMouseMotionState {
    dx: i32,
    dy: i32,
    raw_dx: i32,
    raw_dy: i32,
    movement_hle_host_activity_eligible: bool,
    raw_hle_host_activity_eligible: bool,
}

impl SharedMouseMotionState {
    fn add_movement(&mut self, dx: i32, dy: i32, hle_host_activity_eligible: bool) {
        self.dx = self.dx.wrapping_add(dx);
        self.dy = self.dy.wrapping_add(dy);
        if hle_host_activity_eligible && (dx != 0 || dy != 0) {
            self.movement_hle_host_activity_eligible = true;
        }
    }

    fn add_raw_movement(&mut self, dx: i32, dy: i32, hle_host_activity_eligible: bool) {
        self.raw_dx = self.raw_dx.wrapping_add(dx);
        self.raw_dy = self.raw_dy.wrapping_add(dy);
        if hle_host_activity_eligible && (dx != 0 || dy != 0) {
            self.raw_hle_host_activity_eligible = true;
        }
    }

    fn movement(&self) -> (i32, i32) {
        (self.dx, self.dy)
    }

    fn raw_movement(&self) -> (i32, i32) {
        (self.raw_dx, self.raw_dy)
    }

    fn has_activity(&self) -> bool {
        self.dx != 0 || self.dy != 0 || self.raw_dx != 0 || self.raw_dy != 0
    }
}

/// Thread-safe shared mouse state for communication between GUI and emulator threads.
///
/// The GUI thread updates position/buttons, the emulator thread polls and feeds
/// the data to the PS/2 mouse hardware emulation.
pub struct SharedMouseState {
    /// Pending framebuffer-space and raw motion plus source tags.
    motion: Mutex<SharedMouseMotionState>,
    /// Current button state (bit 0=left, 1=right, 2=middle)
    buttons: AtomicU8,
    /// Wheel delta (for Intellimouse)
    wheel_delta: Mutex<i8>,
    /// IRQ12 pending flag (set by emulator when packet ready)
    irq12_pending: AtomicBool,
    /// Absolute framebuffer position, packed as (fb_x:u32 << 32 | fb_y:u32).
    /// FB_POS_INVALID (u64::MAX) means no valid position.
    fb_position: AtomicU64,
    /// Persistent seamless absolute target for real-boot PS/2 synchronization.
    seamless_target: Mutex<SeamlessTargetState>,
}

impl SharedMouseState {
    pub fn new() -> Self {
        Self {
            motion: Mutex::new(SharedMouseMotionState::default()),
            buttons: AtomicU8::new(0),
            wheel_delta: Mutex::new(0),
            irq12_pending: AtomicBool::new(false),
            fb_position: AtomicU64::new(FB_POS_INVALID),
            seamless_target: Mutex::new(SeamlessTargetState::default()),
        }
    }

    /// Called by GUI thread to report mouse movement (relative, in pixels)
    pub fn add_movement(&self, dx: i32, dy: i32) {
        self.add_movement_inner(dx, dy, true);
    }

    /// Called by scripted/control input to report movement without making it
    /// a physical host-activity signal for HLE capture heuristics.
    pub fn add_scripted_movement(&self, dx: i32, dy: i32) {
        self.add_movement_inner(dx, dy, false);
    }

    fn add_movement_inner(&self, dx: i32, dy: i32, hle_host_activity_eligible: bool) {
        if let Ok(mut motion) = self.motion.lock() {
            motion.add_movement(dx, dy, hle_host_activity_eligible);
        }
    }

    /// Called by GUI thread to report raw device motion. Unlike
    /// [`Self::add_movement`], this is not tied to the host cursor's
    /// framebuffer position and can be used as an INT 33h fallback mickey
    /// source when no framebuffer-space delta is available.
    pub fn add_raw_movement(&self, dx: i32, dy: i32) {
        self.add_raw_movement_inner(dx, dy, true);
    }

    /// Scripted/control raw motion variant that does not count as physical
    /// host activity for HLE capture heuristics.
    pub fn add_scripted_raw_movement(&self, dx: i32, dy: i32) {
        self.add_raw_movement_inner(dx, dy, false);
    }

    fn add_raw_movement_inner(&self, dx: i32, dy: i32, hle_host_activity_eligible: bool) {
        if let Ok(mut motion) = self.motion.lock() {
            motion.add_raw_movement(dx, dy, hle_host_activity_eligible);
        }
    }

    /// Called by GUI thread to update button state
    pub fn set_buttons(&self, left: bool, right: bool, middle: bool) {
        let mut state = 0u8;
        if left {
            state |= 0x01;
        }
        if right {
            state |= 0x02;
        }
        if middle {
            state |= 0x04;
        }
        self.buttons.store(state, Ordering::Relaxed);
    }

    /// Called by GUI thread to add wheel movement
    pub fn add_wheel(&self, delta: i8) {
        if let Ok(mut wheel) = self.wheel_delta.lock() {
            *wheel = wheel.saturating_add(delta);
        }
    }

    /// Called by emulator thread to consume accumulated movement
    /// Returns (dx, dy) and resets the accumulators
    pub fn take_movement(&self) -> (i32, i32) {
        let Ok(mut motion) = self.motion.lock() else {
            return (0, 0);
        };
        let movement = motion.movement();
        motion.dx = 0;
        motion.dy = 0;
        motion.movement_hle_host_activity_eligible = false;
        movement
    }

    /// Called by emulator thread to consume accumulated raw host-device motion.
    pub fn take_raw_movement(&self) -> (i32, i32) {
        let Ok(mut motion) = self.motion.lock() else {
            return (0, 0);
        };
        let raw_movement = motion.raw_movement();
        motion.raw_dx = 0;
        motion.raw_dy = 0;
        motion.raw_hle_host_activity_eligible = false;
        raw_movement
    }

    /// Drain framebuffer-space and raw motion with their physical/source tags.
    ///
    /// This keeps HLE capture telemetry tied to the same drained movement the
    /// emulator consumes, instead of racing a separate source flag.
    pub fn take_motion(&self) -> ((i32, i32), bool, (i32, i32), bool) {
        let Ok(mut motion) = self.motion.lock() else {
            return ((0, 0), false, (0, 0), false);
        };
        let movement = motion.movement();
        let raw_movement = motion.raw_movement();
        let movement_eligible = motion.movement_hle_host_activity_eligible && movement != (0, 0);
        let raw_eligible = motion.raw_hle_host_activity_eligible && raw_movement != (0, 0);
        *motion = SharedMouseMotionState::default();
        (movement, movement_eligible, raw_movement, raw_eligible)
    }

    /// Clear pending positional mouse input when switching transports.
    ///
    /// This deliberately leaves buttons and wheel state intact: capture entry
    /// changes how motion is transported, not which buttons are currently down.
    pub fn clear_pending_position_and_motion(&self) {
        if let Ok(mut motion) = self.motion.lock() {
            *motion = SharedMouseMotionState::default();
        }
        self.fb_position.store(FB_POS_INVALID, Ordering::Release);
        self.clear_seamless_target();
    }

    /// Called by emulator thread to get current button state
    pub fn get_buttons(&self) -> u8 {
        self.buttons.load(Ordering::Acquire)
    }

    /// Called by emulator thread to consume wheel delta
    pub fn take_wheel(&self) -> i8 {
        if let Ok(mut wheel) = self.wheel_delta.lock() {
            let delta = *wheel;
            *wheel = 0;
            delta
        } else {
            0
        }
    }

    /// Set IRQ12 pending flag
    pub fn set_irq12_pending(&self) {
        self.irq12_pending.store(true, Ordering::Release);
    }

    /// Take IRQ12 pending flag (returns true if was pending, clears flag)
    pub fn take_irq12_pending(&self) -> bool {
        self.irq12_pending.swap(false, Ordering::AcqRel)
    }

    /// Store absolute framebuffer position (GUI thread → emulator thread).
    /// Coordinates are packed as (fb_x:u32 << 32 | fb_y:u32).
    pub fn set_fb_position(&self, x: i32, y: i32) {
        let packed = pack_fb_position(x, y);
        self.fb_position.store(packed, Ordering::Release);
    }

    /// Consume absolute framebuffer position. Returns None if no valid position.
    pub fn take_fb_position(&self) -> Option<(i32, i32)> {
        let packed = self.fb_position.swap(FB_POS_INVALID, Ordering::AcqRel);
        unpack_fb_position(packed)
    }

    /// Store a persistent seamless target distinct from one-shot fb_position.
    /// Returns the sequence number after the update.
    pub fn set_seamless_target(&self, x: i32, y: i32) -> u64 {
        let Ok(mut state) = self.seamless_target.lock() else {
            return 0;
        };
        if state.target != Some((x, y)) {
            state.target = Some((x, y));
            state.seq = state.seq.wrapping_add(1);
        }
        state.seq
    }

    /// Clear the persistent seamless target. Returns true if a target was cleared.
    pub fn clear_seamless_target(&self) -> bool {
        let Ok(mut state) = self.seamless_target.lock() else {
            return false;
        };
        if state.target.take().is_some() {
            state.seq = state.seq.wrapping_add(1);
            return true;
        }
        false
    }

    /// Read the current persistent seamless target without consuming it.
    pub fn get_seamless_target(&self) -> Option<SeamlessMouseTarget> {
        let state = self.seamless_target.lock().ok()?;
        let (x, y) = state.target?;
        let seq = state.seq;
        Some(SeamlessMouseTarget { x, y, seq })
    }

    /// Check if there's any pending movement or button change
    pub fn has_activity(&self) -> bool {
        self.motion
            .lock()
            .map(|motion| motion.has_activity())
            .unwrap_or(false)
            || self.irq12_pending.load(Ordering::Relaxed)
    }
}

impl Default for SharedMouseState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ps2MouseOutput {
    value: u8,
    kind: Ps2MouseOutputKind,
    command_generation: u32,
}

impl Ps2MouseOutput {
    fn command_response(value: u8, command_generation: u32) -> Self {
        Self {
            value,
            kind: Ps2MouseOutputKind::CommandResponse,
            command_generation,
        }
    }

    fn packet(value: u8) -> Self {
        Self {
            value,
            kind: Ps2MouseOutputKind::Packet,
            command_generation: 0,
        }
    }

    fn with_kind(value: u8, kind: Ps2MouseOutputKind, command_generation: u32) -> Self {
        let command_generation = if kind == Ps2MouseOutputKind::CommandResponse {
            command_generation
        } else {
            0
        };
        Self {
            value,
            kind,
            command_generation,
        }
    }

    pub(crate) fn value(self) -> u8 {
        self.value
    }

    pub(crate) fn is_command_response(self) -> bool {
        self.kind == Ps2MouseOutputKind::CommandResponse
    }

    pub(crate) fn command_generation(self) -> Option<u32> {
        self.is_command_response()
            .then_some(self.command_generation)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ps2MouseOutputKind {
    Packet,
    CommandResponse,
}

/// PS/2 mouse hardware state
pub struct Ps2Mouse {
    /// Output queue for command responses and movement packets.
    ///
    /// Command responses are polled by BIOS or mouse-driver code and must be able to reach
    /// OBF without AUXINT/IRQ12. Movement packets must wait until an IRQ12
    /// consumer can drain them.
    output_queue: FixedRing<Ps2MouseOutput, 32>,

    /// Mouse mode
    mode: MouseMode,

    /// Sample rate (10-200 samples/sec), default 100
    sample_rate: u8,

    /// Resolution (0-3 = 1/2/4/8 counts per mm), default 2 (4 counts/mm)
    resolution: u8,

    /// Scaling mode
    scaling: Scaling,

    /// Data reporting enabled (stream mode only)
    reporting_enabled: bool,

    /// Accumulated movement (in mouse units)
    delta_x: i32,
    delta_y: i32,

    /// Current button state (bit 0=left, 1=right, 2=middle)
    buttons: u8,

    /// Wheel delta (Intellimouse)
    wheel_delta: i8,

    /// Whether the attached physical device can enter IntelliMouse protocol.
    ///
    /// A standard PS/2 mouse accepts the sample-rate probe sequence but keeps
    /// reporting ID 00h and 3-byte packets.
    intellimouse_capable: bool,

    /// Intellimouse mode active
    intellimouse: bool,

    /// Magic sequence tracker for Intellimouse detection
    /// Stores last 3 sample rate values
    sample_rate_history: [u8; 3],
    sample_rate_history_idx: usize,

    /// Command expecting follow-up data byte
    pending_command: Option<Ps2MouseCommand>,

    /// Guest-virtual milliseconds at the last packet, for the sample-rate
    /// limiter. `None` until the first packet, which is therefore never
    /// rate-limited.
    ///
    /// GUEST time, not host time. This was an `Instant` — a host clock behind
    /// a guest-visible cadence — so report timing tracked host load and speed
    /// policy, and `--record-input` replay of mouse motion was not
    /// deterministic (MDD-BUG-FLU-01488). Every guest-visible clock in this
    /// emulator derives from `cpu.cycles`; the caller converts and passes it
    /// in, because the poll that drives this already runs on guest cycles.
    last_packet_ms: Option<u64>,

    /// Button state at last packet (for change detection)
    last_buttons: u8,

    /// Last byte delivered to the host (for RESEND handling)
    last_sent_byte: Option<u8>,

    /// Monotonic marker for commands that make recent movement attribution
    /// unsafe for real-boot seamless calibration.
    calibration_attribution_epoch: u64,

    /// Monotonic tag assigned to command-response bytes.
    command_response_generation: u32,

    /// Lifetime count used to summarize guest-controlled invalid-rate warnings.
    invalid_sample_rate_requests: u64,
}

#[derive(Clone, Copy, PartialEq, Default, Debug)]
enum MouseMode {
    #[default]
    Stream, // Send packets automatically on movement/button
    Remote, // Only send on explicit read request (0xEB)
    Wrap,   // Echo all bytes back
}

#[derive(Clone, Copy, Default, Debug)]
enum Scaling {
    #[default]
    OneToOne,
    TwoToOne,
}

#[derive(Clone, Copy, Debug)]
enum Ps2MouseCommand {
    SetSampleRate,
    SetResolution,
}

fn bounded_warning_suppressed(total: u64) -> Option<u64> {
    if total == 1 {
        Some(0)
    } else if total >= 16 && total.is_power_of_two() {
        let previous_warning = if total == 16 { 1 } else { total / 2 };
        Some(total - previous_warning - 1)
    } else {
        None
    }
}

impl Default for Ps2Mouse {
    fn default() -> Self {
        Self::new()
    }
}

impl Ps2Mouse {
    /// Report rates accepted by a PS/2 pointing device (IBM PS/2 specification).
    const VALID_SAMPLE_RATES: [u8; 7] = [10, 20, 40, 60, 80, 100, 200];

    /// Power-on / Set-Defaults (0xF6) report rate, and the cadence we fall back
    /// to when a host programs a value outside `VALID_SAMPLE_RATES`.
    const DEFAULT_SAMPLE_RATE: u8 = 100;

    fn is_valid_sample_rate(rate: u8) -> bool {
        Self::VALID_SAMPLE_RATES.contains(&rate)
    }

    /// The rate actually used to pace report generation.
    ///
    /// An out-of-spec programmed rate is reported at the power-on default.
    /// A rate index sent as a raw rate would otherwise throttle reporting.
    /// `handle_set_sample_rate` logs the anomaly at WARN.
    pub fn effective_sample_rate(&self) -> u8 {
        if Self::is_valid_sample_rate(self.sample_rate) {
            self.sample_rate
        } else {
            Self::DEFAULT_SAMPLE_RATE
        }
    }

    pub fn new() -> Self {
        Self::with_intellimouse_capability(false)
    }

    /// Create a PS/2 device that can switch to IntelliMouse protocol after
    /// the 200,100,80 sample-rate probe.
    pub fn new_intellimouse() -> Self {
        Self::with_intellimouse_capability(true)
    }

    fn with_intellimouse_capability(intellimouse_capable: bool) -> Self {
        Self {
            output_queue: FixedRing::new(),
            mode: MouseMode::Stream,
            sample_rate: Self::DEFAULT_SAMPLE_RATE,
            resolution: 2, // 4 counts/mm
            scaling: Scaling::OneToOne,
            reporting_enabled: false,
            delta_x: 0,
            delta_y: 0,
            buttons: 0,
            wheel_delta: 0,
            intellimouse_capable,
            intellimouse: false,
            sample_rate_history: [0; 3],
            sample_rate_history_idx: 0,
            pending_command: None,
            // `None` rather than a back-dated instant: the first packet is
            // never rate-limited, with no arithmetic to get wrong near zero.
            last_packet_ms: None,
            last_buttons: 0,
            last_sent_byte: None,
            calibration_attribution_epoch: 0,
            command_response_generation: 0,
            invalid_sample_rate_requests: 0,
        }
    }

    /// Called from emulator with screen-relative movement.
    ///
    /// The GUI has already mapped host-window motion into guest framebuffer
    /// pixels before this reaches the PS/2 device. Treat those deltas as the
    /// device mickeys to report. Applying the PS/2 resolution setting here
    /// would scale already-normalized emulator input a second time, making
    /// real-boot DOS mouse drivers move too fast in scaled windows.
    ///
    /// dx, dy: movement in guest screen pixels (positive = right/down)
    pub fn accumulate_movement(&mut self, dx: i32, dy: i32) {
        self.delta_x += dx;
        // PS/2 Y-axis is inverted: positive = UP (opposite of screen coords)
        self.delta_y += -dy;
    }

    /// Set current button state
    pub fn set_buttons(&mut self, buttons: u8) {
        self.buttons = buttons;
    }

    /// Add wheel movement
    pub fn add_wheel(&mut self, delta: i8) {
        if self.intellimouse {
            self.wheel_delta = self.wheel_delta.saturating_add(delta);
        }
    }

    /// Generate packet if movement/buttons changed and rate limit allows
    /// Returns true if a packet was generated (IRQ12 should be raised)
    ///
    /// `guest_now_ms` is GUEST-virtual milliseconds, derived by the caller from
    /// `cpu.cycles`. The cadence a guest observes must not depend on how fast
    /// or how loaded the host is (MDD-BUG-FLU-01488).
    pub fn maybe_generate_packet(&mut self, guest_now_ms: u64) -> bool {
        if self.mode != MouseMode::Stream || !self.reporting_enabled {
            return false;
        }

        // Check sample rate limit
        let min_interval_ms = 1000 / self.effective_sample_rate() as u64;
        if self
            .last_packet_ms
            .is_some_and(|last| guest_now_ms.saturating_sub(last) < min_interval_ms)
        {
            return false;
        }

        // Check if anything to report
        let buttons_changed = self.buttons != self.last_buttons;
        let wheel_changed = self.intellimouse && self.wheel_delta != 0;
        if self.delta_x == 0 && self.delta_y == 0 && !buttons_changed && !wheel_changed {
            return false;
        }

        if !self.generate_packet() {
            return false;
        }

        self.last_packet_ms = Some(guest_now_ms);
        self.last_buttons = self.buttons;
        true
    }

    /// Whether stream mode has input state that still needs a packet.
    ///
    /// Board schedulers can use this cheap predicate before consulting the
    /// wall-clock sample-rate limiter on otherwise unrelated CPU boundaries.
    pub fn stream_report_pending(&self) -> bool {
        self.mode == MouseMode::Stream
            && self.reporting_enabled
            && (self.delta_x != 0
                || self.delta_y != 0
                || self.buttons != self.last_buttons
                || (self.intellimouse && self.wheel_delta != 0))
    }

    fn generate_packet(&mut self) -> bool {
        self.generate_packet_with_kind(Ps2MouseOutputKind::Packet, 0)
    }

    fn generate_response_packet(&mut self) {
        let _ = self.generate_packet_with_kind(
            Ps2MouseOutputKind::CommandResponse,
            self.command_response_generation,
        );
    }

    fn generate_packet_with_kind(
        &mut self,
        kind: Ps2MouseOutputKind,
        command_generation: u32,
    ) -> bool {
        // Real PS/2 silicon drops the entire packet when its FIFO is full
        // (overrun); never put a half-packet on the wire. If we don't have
        // room for a complete 3-byte (or 4-byte intellimouse) packet, drop
        // it without consuming the deltas — the next tick will retry.
        let needed = if self.intellimouse { 4 } else { 3 };
        if self.output_queue.len() + needed > self.output_queue.capacity() {
            return false;
        }

        // One report carries one sample interval's motion, clamped to the 9-bit
        // signed range. A real device has no memory beyond that interval: it
        // reports what fits, raises the overflow bit, and drops the rest.
        // Retaining the residue instead replays it through later reports, so a
        // burst of host motion keeps arriving at the guest long after the
        // pointer stopped - 30,000 offered counts bank 29,745 and need 117
        // further reports (1.2 s at 100 Hz, 12 s at rate 10) to drain. That is
        // the same defect the Sun serial mouse fixed by discarding overflow
        // rather than replaying it (bugs/MDD-BUG-FLU-00359.md), and the
        // accumulator behind the "very sticky" real-boot pointer of
        // bugs/MDD-BUG-KILN-00139.md. Motion at or under one report is
        // unaffected: the clamp is the identity there and always was.
        let x_overflow =
            self.delta_x < MIN_REPORTABLE_MOTION || self.delta_x > MAX_REPORTABLE_MOTION;
        let y_overflow =
            self.delta_y < MIN_REPORTABLE_MOTION || self.delta_y > MAX_REPORTABLE_MOTION;
        let dx = self
            .delta_x
            .clamp(MIN_REPORTABLE_MOTION, MAX_REPORTABLE_MOTION) as i16;
        let dy = self
            .delta_y
            .clamp(MIN_REPORTABLE_MOTION, MAX_REPORTABLE_MOTION) as i16;

        // Consume the whole sample interval, overflow included.
        self.delta_x = 0;
        self.delta_y = 0;

        // The GUI path feeds screen-normalized deltas into the PS/2 device:
        // one unit here already means one guest-screen movement unit after
        // host-window scaling and letterbox correction. Applying the PS/2
        // 2:1 scaling table on top of that makes real-boot mouse drivers
        // visibly outrun the hidden host pointer. Keep tracking/reporting the
        // scaling command for status compatibility, but do not accelerate
        // already-normalized emulator input a second time.

        // Build status byte
        let mut status: u8 = 0x08; // Bit 3 always set
        if self.buttons & 0x01 != 0 {
            status |= 0x01;
        } // Left
        if self.buttons & 0x02 != 0 {
            status |= 0x02;
        } // Right
        if self.buttons & 0x04 != 0 {
            status |= 0x04;
        } // Middle
        if dx < 0 {
            status |= 0x10;
        } // X sign
        if dy < 0 {
            status |= 0x20;
        } // Y sign
        if x_overflow {
            status |= 0x40;
        } // X overflow
        if y_overflow {
            status |= 0x80;
        } // Y overflow

        // Capacity checked above — these pushes cannot fail.
        self.output_queue
            .push_back(Ps2MouseOutput::with_kind(status, kind, command_generation));
        self.output_queue.push_back(Ps2MouseOutput::with_kind(
            (dx & 0xFF) as u8,
            kind,
            command_generation,
        ));
        self.output_queue.push_back(Ps2MouseOutput::with_kind(
            (dy & 0xFF) as u8,
            kind,
            command_generation,
        ));

        if self.intellimouse {
            // Clamp wheel to 4-bit signed (-8 to +7)
            let wheel = self.wheel_delta.clamp(-8, 7);
            self.wheel_delta -= wheel;
            self.output_queue.push_back(Ps2MouseOutput::with_kind(
                (wheel & 0x0F) as u8,
                kind,
                command_generation,
            ));
        }

        true
    }

    /// Non-linear 2:1 scaling per PS/2 spec.
    #[cfg(test)]
    fn apply_2to1_scaling(&self, val: i16) -> i16 {
        match val.abs() {
            0 => 0,
            1 => val.signum(),
            2 => val.signum(),
            3 => val.signum() * 3,
            4 => val.signum() * 6,
            5 => val.signum() * 9,
            _ => val * 2,
        }
    }

    /// Handle command byte from 8042 (after 0xD4 prefix)
    ///
    /// Command responses use `try_push_back` so a full output FIFO drops the
    /// new byte (matching real silicon overrun behaviour) instead of panicking.
    /// Commands typically run with the FIFO empty/near-empty because the guest
    /// reads each response before sending the next command, so drops are rare;
    /// when they do happen the guest's 8042 driver sees a missing reply and
    /// retries.
    pub fn handle_command(&mut self, cmd: u8) {
        self.command_response_generation = self.command_response_generation.wrapping_add(1);

        // Host command traffic inhibits normal stream transfer. Any report
        // packet still sitting in this emulated device FIFO has not reached
        // OBF yet; if left ahead of the command ACK, mouse-driver or BIOS code polling sees
        // the wrong byte or times out. Keep unread command responses, but drop
        // stale movement packets before queuing the new response.
        if self.discard_pending_packets() {
            self.mark_calibration_attribution_changed();
        }

        // If we're expecting a data byte for a previous command
        if let Some(pending) = self.pending_command.take() {
            match pending {
                Ps2MouseCommand::SetSampleRate => {
                    let _ = self.handle_set_sample_rate(cmd);
                    self.queue_response(0xFA); // ACK
                }
                Ps2MouseCommand::SetResolution => {
                    self.resolution = cmd.min(3);
                    self.queue_response(0xFA); // ACK
                }
            }
            return;
        }

        // Handle wrap mode specially
        if self.mode == MouseMode::Wrap && cmd != 0xEC && cmd != 0xFF {
            // In wrap mode, echo back everything except Reset and Reset Wrap Mode
            self.queue_response(cmd);
            return;
        }

        match cmd {
            0xE6 => {
                // Set Scaling 1:1
                self.mark_calibration_attribution_changed();
                self.scaling = Scaling::OneToOne;
                self.queue_response(0xFA); // ACK
            }
            0xE7 => {
                // Set Scaling 2:1
                self.mark_calibration_attribution_changed();
                self.scaling = Scaling::TwoToOne;
                self.queue_response(0xFA); // ACK
            }
            0xE8 => {
                // Set Resolution - next byte is resolution value
                self.mark_calibration_attribution_changed();
                self.pending_command = Some(Ps2MouseCommand::SetResolution);
                self.queue_response(0xFA); // ACK
            }
            0xE9 => {
                // Status Request - return 3-byte status
                self.queue_response(0xFA); // ACK

                // Status byte 1: mode, enable, scaling, buttons
                let mut status1 = 0u8;
                if self.mode == MouseMode::Remote {
                    status1 |= 0x40;
                }
                if self.reporting_enabled {
                    status1 |= 0x20;
                }
                if matches!(self.scaling, Scaling::TwoToOne) {
                    status1 |= 0x10;
                }
                if self.buttons & 0x01 != 0 {
                    status1 |= 0x04;
                } // Left
                if self.buttons & 0x04 != 0 {
                    status1 |= 0x02;
                } // Middle
                if self.buttons & 0x02 != 0 {
                    status1 |= 0x01;
                } // Right

                self.queue_response(status1);
                self.queue_response(self.resolution);
                self.queue_response(self.sample_rate);
            }
            0xEA => {
                // Set Stream Mode
                self.mark_calibration_attribution_changed();
                self.mode = MouseMode::Stream;
                self.queue_response(0xFA); // ACK
            }
            0xEB => {
                // Read Data - send packet immediately (for remote mode)
                self.queue_response(0xFA); // ACK
                self.generate_response_packet();
            }
            0xEC => {
                // Reset Wrap Mode - return to stream mode
                self.mark_calibration_attribution_changed();
                self.mode = MouseMode::Stream;
                self.queue_response(0xFA); // ACK
            }
            0xEE => {
                // Set Wrap Mode
                self.mark_calibration_attribution_changed();
                self.mode = MouseMode::Wrap;
                self.queue_response(0xFA); // ACK
            }
            0xF0 => {
                // Set Remote Mode
                self.mark_calibration_attribution_changed();
                self.mode = MouseMode::Remote;
                self.queue_response(0xFA); // ACK
            }
            0xF2 => {
                // Read Device Type
                self.queue_response(0xFA); // ACK
                self.queue_response(self.get_device_id());
            }
            0xF3 => {
                // Set Sample Rate - next byte is rate
                self.mark_calibration_attribution_changed();
                self.pending_command = Some(Ps2MouseCommand::SetSampleRate);
                self.queue_response(0xFA); // ACK
            }
            0xF4 => {
                // Enable Data Reporting
                self.mark_calibration_attribution_changed();
                self.reporting_enabled = true;
                self.queue_response(0xFA); // ACK
            }
            0xF5 => {
                // Disable Data Reporting
                self.mark_calibration_attribution_changed();
                self.reporting_enabled = false;
                self.queue_response(0xFA); // ACK
            }
            0xF6 => {
                // Set Defaults
                self.mark_calibration_attribution_changed();
                self.sample_rate = Self::DEFAULT_SAMPLE_RATE;
                self.resolution = 2;
                self.scaling = Scaling::OneToOne;
                self.reporting_enabled = false;
                self.mode = MouseMode::Stream;
                self.queue_response(0xFA); // ACK
            }
            0xFE => {
                // Resend - resend last byte if available
                let last_byte = self.last_sent_byte.unwrap_or(0xFE);
                self.queue_response(last_byte);
            }
            0xFF => {
                // Reset - flush FIFO, perform BAT, return device ID. Real
                // silicon clears the output FIFO on reset, so the 3-byte
                // response is guaranteed to fit.
                self.mark_calibration_attribution_changed();
                self.output_queue.clear();
                self.reset();
                self.queue_response(0xFA); // ACK
                self.queue_response(0xAA); // BAT successful
                self.queue_response(self.get_device_id());
            }
            _ => {
                // Unknown command - NACK
                self.queue_response(0xFE);
            }
        }
    }

    fn queue_response(&mut self, value: u8) {
        let _ = self
            .output_queue
            .try_push_back(Ps2MouseOutput::command_response(
                value,
                self.command_response_generation,
            ));
    }

    fn mark_calibration_attribution_changed(&mut self) {
        self.calibration_attribution_epoch = self.calibration_attribution_epoch.wrapping_add(1);
    }

    fn discard_pending_packets(&mut self) -> bool {
        let mut retained = FixedRing::new();
        let mut discarded_packet = false;
        for output in self.output_queue.drain() {
            if output.is_command_response() {
                let _ = retained.try_push_back(output);
            } else {
                discarded_packet = true;
            }
        }
        self.output_queue = retained;
        discarded_packet
    }

    fn handle_set_sample_rate(&mut self, rate: u8) -> Option<(u64, u64)> {
        // Store the raw byte: Status Request (0xE9) must report back exactly what
        // the host programmed, and the IntelliMouse knock below matches on it.
        // Only the *reporting cadence* is sanitised, in `effective_sample_rate`.
        self.sample_rate = rate;

        let warning = if !Self::is_valid_sample_rate(rate) {
            self.invalid_sample_rate_requests = self.invalid_sample_rate_requests.saturating_add(1);
            let request_count = self.invalid_sample_rate_requests;
            // Real devices vary: some reject unsupported report rates and
            // some accept and clamp them. Accept the command on the wire,
            // use the default effective rate, and report the anomaly.
            if let Some(suppressed_since_previous_warning) =
                bounded_warning_suppressed(request_count)
            {
                tracing::warn!(
                    rate,
                    request_count,
                    suppressed_since_previous_warning,
                    "PS/2 mouse: host programmed an out-of-spec sample rate; \
                     reporting at {} Hz instead (valid: 10/20/40/60/80/100/200)",
                    Self::DEFAULT_SAMPLE_RATE,
                );
                Some((request_count, suppressed_since_previous_warning))
            } else {
                tracing::trace!(
                    rate,
                    request_count,
                    "PS/2 mouse: repeated out-of-spec sample rate"
                );
                None
            }
        } else {
            None
        };

        // Track for Intellimouse detection magic sequence: 200, 100, 80
        self.sample_rate_history[self.sample_rate_history_idx] = rate;
        self.sample_rate_history_idx = (self.sample_rate_history_idx + 1) % 3;

        // After storing, check if the last 3 rates were 200, 100, 80 in order
        // The idx now points to the oldest entry (will be overwritten next)
        let idx = self.sample_rate_history_idx;
        let seq = [
            self.sample_rate_history[idx],
            self.sample_rate_history[(idx + 1) % 3],
            self.sample_rate_history[(idx + 2) % 3],
        ];
        if self.intellimouse_capable && seq == [200, 100, 80] {
            self.intellimouse = true;
        }

        warning
    }

    fn get_device_id(&self) -> u8 {
        if self.intellimouse {
            0x03
        } else {
            0x00
        }
    }

    fn reset(&mut self) {
        self.mode = MouseMode::Stream;
        self.sample_rate = Self::DEFAULT_SAMPLE_RATE;
        self.resolution = 2;
        self.scaling = Scaling::OneToOne;
        self.reporting_enabled = false;
        self.delta_x = 0;
        self.delta_y = 0;
        self.wheel_delta = 0;
        self.intellimouse = false;
        self.sample_rate_history = [0; 3];
        self.sample_rate_history_idx = 0;
        self.pending_command = None;
    }

    /// Check if output buffer has data
    pub fn has_output(&self) -> bool {
        !self.output_queue.is_empty()
    }

    /// Check if the next queued output byte is a command response.
    pub(crate) fn front_output_is_command_response(&self) -> bool {
        self.output_queue
            .front()
            .is_some_and(|output| output.is_command_response())
    }

    /// Count queued command-response bytes.
    pub(crate) fn command_response_count(&self) -> usize {
        self.output_queue
            .iter()
            .filter(|output| output.is_command_response())
            .count()
    }

    /// Discard queued command-response bytes, preserving movement packets.
    pub(crate) fn discard_pending_command_responses(&mut self) -> usize {
        let mut retained = FixedRing::new();
        let mut discarded = 0;
        for output in self.output_queue.drain() {
            if output.is_command_response() {
                discarded += 1;
            } else {
                let _ = retained.try_push_back(output);
            }
        }
        self.output_queue = retained;
        discarded
    }

    /// Discard queued command-response bytes from one command generation,
    /// preserving movement packets and newer or older response generations.
    pub(crate) fn discard_command_responses_with_generation(&mut self, generation: u32) -> usize {
        let mut retained = FixedRing::new();
        let mut discarded = 0;
        for output in self.output_queue.drain() {
            if output.command_generation() == Some(generation) {
                discarded += 1;
            } else {
                let _ = retained.try_push_back(output);
            }
        }
        self.output_queue = retained;
        discarded
    }

    /// Check if at least one queued output byte is a movement/report packet.
    pub fn has_packet_output(&self) -> bool {
        self.output_queue
            .iter()
            .any(|output| !output.is_command_response())
    }

    /// Inject a byte into the output queue (used by 8042 0xD3)
    pub fn enqueue_output(&mut self, value: u8) {
        let _ = self
            .output_queue
            .try_push_back(Ps2MouseOutput::packet(value));
    }

    /// Read byte from output buffer
    pub fn read_output(&mut self) -> Option<u8> {
        self.read_output_tagged().map(Ps2MouseOutput::value)
    }

    /// Read byte from output buffer with command-response/packet tag.
    pub(crate) fn read_output_tagged(&mut self) -> Option<Ps2MouseOutput> {
        let output = self.output_queue.pop_front();
        if let Some(byte) = output {
            self.last_sent_byte = Some(byte.value());
        }
        output
    }

    /// Get output queue length (for testing/debugging)
    pub fn output_queue_len(&self) -> usize {
        self.output_queue.len()
    }

    /// Check if reporting is enabled
    pub fn is_reporting_enabled(&self) -> bool {
        self.reporting_enabled
    }

    /// Current programmed PS/2 stream sample rate in reports/sec.
    ///
    /// Real silicon only accepts {10,20,40,60,80,100,200}; the value the guest
    /// last set via the 0xF3 command. Used by BIOS conformance tests to prove
    /// INT 15h C202 translated its rate *index* to an actual rate.
    pub fn sample_rate(&self) -> u8 {
        self.sample_rate
    }

    /// Current programmed PS/2 resolution code (0-3 = 1/2/4/8 counts/mm), i.e.
    /// the value the guest last set via the 0xE8 command. Used by BIOS
    /// conformance tests to prove INT 15h C203 masked its operand with `&3`
    /// before putting it on the wire, rather than forwarding a raw out-of-domain
    /// BH that the device would then clamp to a different value.
    pub fn resolution(&self) -> u8 {
        self.resolution
    }

    /// Current accumulated movement not yet emitted as PS/2 packet bytes.
    /// Intended for diagnostics and mouse-calibration traces.
    pub fn pending_movement(&self) -> (i32, i32) {
        (self.delta_x, self.delta_y)
    }

    pub fn calibration_attribution_epoch(&self) -> u64 {
        self.calibration_attribution_epoch
    }

    /// Check if Intellimouse mode is active
    pub fn is_intellimouse(&self) -> bool {
        self.intellimouse
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::panic::AssertUnwindSafe;

    #[test]
    fn test_shared_mouse_state_movement_buttons_and_activity() {
        let state = SharedMouseState::new();
        assert!(!state.has_activity());

        state.add_movement(3, -2);
        state.set_buttons(true, false, true);

        assert!(state.has_activity());
        assert_eq!(state.get_buttons(), 0x05);

        let (dx, dy) = state.take_movement();
        assert_eq!(dx, 3);
        assert_eq!(dy, -2);
        assert!(!state.has_activity());
    }

    #[test]
    fn test_shared_mouse_state_activity_y_only_and_irq12() {
        let state = SharedMouseState::new();

        state.add_movement(0, 1);
        assert!(state.has_activity());
        let (dx, dy) = state.take_movement();
        assert_eq!(dx, 0);
        assert_eq!(dy, 1);
        assert!(!state.has_activity());

        state.set_irq12_pending();
        assert!(state.has_activity());
        assert!(state.take_irq12_pending());
        assert!(!state.has_activity());
    }

    #[test]
    fn test_shared_mouse_state_raw_movement_is_separate_activity() {
        let state = SharedMouseState::new();

        state.add_movement(3, 4);
        state.add_raw_movement(7, -9);

        assert!(state.has_activity());
        assert_eq!(state.take_raw_movement(), (7, -9));
        assert!(state.has_activity(), "regular movement is still pending");
        assert_eq!(state.take_movement(), (3, 4));
        assert!(!state.has_activity());
    }

    #[test]
    fn test_shared_mouse_scripted_movement_is_not_hle_host_activity() {
        let state = SharedMouseState::new();

        state.add_scripted_movement(3, 4);
        state.add_scripted_raw_movement(7, -9);

        let (movement, movement_eligible, raw_movement, raw_eligible) = state.take_motion();
        assert_eq!(movement, (3, 4));
        assert_eq!(raw_movement, (7, -9));
        assert!(!movement_eligible);
        assert!(!raw_eligible);

        state.add_movement(1, 0);
        state.add_raw_movement(0, -2);
        let (movement, movement_eligible, raw_movement, raw_eligible) = state.take_motion();
        assert_eq!(movement, (1, 0));
        assert_eq!(raw_movement, (0, -2));
        assert!(movement_eligible);
        assert!(raw_eligible);
        assert_eq!(state.take_motion(), ((0, 0), false, (0, 0), false));
    }

    #[test]
    fn test_shared_mouse_motion_source_tags_are_channel_specific() {
        let state = SharedMouseState::new();

        state.add_scripted_movement(3, 0);
        state.add_raw_movement(7, 0);

        let (movement, movement_eligible, raw_movement, raw_eligible) = state.take_motion();
        assert_eq!(movement, (3, 0));
        assert_eq!(raw_movement, (7, 0));
        assert!(!movement_eligible);
        assert!(raw_eligible);
    }

    #[test]
    fn test_shared_mouse_clear_pending_position_and_motion_drains_fb_position() {
        let state = SharedMouseState::new();

        state.set_fb_position(123, 45);
        state.set_seamless_target(200, 60);

        state.clear_pending_position_and_motion();

        assert_eq!(state.take_fb_position(), None);
        assert_eq!(state.get_seamless_target(), None);
    }

    #[test]
    fn test_shared_mouse_clear_pending_position_and_motion_drains_motion() {
        let state = SharedMouseState::new();

        state.add_movement(3, 4);
        state.add_raw_movement(7, -9);
        state.set_buttons(true, false, true);
        state.add_wheel(1);

        state.clear_pending_position_and_motion();

        assert_eq!(state.take_movement(), (0, 0));
        assert_eq!(state.take_raw_movement(), (0, 0));
        assert_eq!(state.take_motion(), ((0, 0), false, (0, 0), false));
        assert_eq!(state.get_buttons(), 0x05);
        assert_eq!(state.take_wheel(), 1);
    }

    #[test]
    fn test_shared_mouse_state_button_bits_all_set_and_clear() {
        let state = SharedMouseState::new();

        state.set_buttons(true, true, true);
        assert_eq!(state.get_buttons(), 0x07);

        state.set_buttons(false, false, false);
        assert_eq!(state.get_buttons(), 0x00);
    }

    #[test]
    fn test_shared_mouse_state_wheel_and_irq12() {
        let state = SharedMouseState::new();

        state.add_wheel(120);
        state.add_wheel(20);
        assert_eq!(state.take_wheel(), 127);
        assert_eq!(state.take_wheel(), 0);

        assert!(!state.take_irq12_pending());
        state.set_irq12_pending();
        assert!(state.take_irq12_pending());
        assert!(!state.take_irq12_pending());
    }

    #[test]
    fn test_shared_mouse_state_wheel_poison_returns_zero() {
        let state = SharedMouseState::new();
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _guard = state.wheel_delta.lock().unwrap();
            panic!("poison");
        }));

        state.add_wheel(5);
        assert_eq!(state.take_wheel(), 0);
    }

    #[test]
    fn test_new() {
        let mouse = Ps2Mouse::new();
        assert_eq!(mouse.sample_rate, 100);
        assert_eq!(mouse.resolution, 2);
        assert!(!mouse.reporting_enabled);
        assert!(!mouse.intellimouse_capable);
        assert!(!mouse.intellimouse);
        assert!(!mouse.has_output());
    }

    #[test]
    fn test_reset_command() {
        let mut mouse = Ps2Mouse::new();
        mouse.handle_command(0xFF); // Reset

        // Should get ACK, BAT success, device ID
        assert_eq!(mouse.read_output(), Some(0xFA)); // ACK
        assert_eq!(mouse.read_output(), Some(0xAA)); // BAT successful
        assert_eq!(mouse.read_output(), Some(0x00)); // Standard mouse ID
        assert!(!mouse.has_output());
    }

    #[test]
    fn test_read_device_type_standard() {
        let mut mouse = Ps2Mouse::new();
        mouse.handle_command(0xF2); // Read Device Type

        assert_eq!(mouse.read_output(), Some(0xFA)); // ACK
        assert_eq!(mouse.read_output(), Some(0x00)); // Standard mouse
    }

    fn send_intellimouse_probe(mouse: &mut Ps2Mouse) {
        mouse.handle_command(0xF3);
        assert_eq!(mouse.read_output(), Some(0xFA));
        mouse.handle_command(200);
        assert_eq!(mouse.read_output(), Some(0xFA));

        mouse.handle_command(0xF3);
        assert_eq!(mouse.read_output(), Some(0xFA));
        mouse.handle_command(100);
        assert_eq!(mouse.read_output(), Some(0xFA));

        mouse.handle_command(0xF3);
        assert_eq!(mouse.read_output(), Some(0xFA));
        mouse.handle_command(80);
        assert_eq!(mouse.read_output(), Some(0xFA));
    }

    fn set_sample_rate(mouse: &mut Ps2Mouse, rate: u8) {
        mouse.handle_command(0xF3);
        assert_eq!(
            mouse.read_output(),
            Some(0xFA),
            "0xF3 must be ACKed before its argument"
        );
        mouse.handle_command(rate);
        assert_eq!(
            mouse.read_output(),
            Some(0xFA),
            "a sample-rate argument is ACKed on the wire whatever its value \
             (permissive ACK policy — real devices vary between ACK and NAK)"
        );
    }

    /// Unsupported report rates retain the raw command argument for status
    /// queries while report pacing uses the power-on default. This guards
    /// against a host sending a rate index instead of a raw rate.
    #[test]
    fn out_of_spec_sample_rate_is_stored_raw_but_paced_at_the_default() {
        let mut mouse = Ps2Mouse::new();
        set_sample_rate(&mut mouse, 2); // the C202 rate-*index* the BIOS used to send

        // Status Request (0xE9) reports the programmed byte verbatim.
        mouse.handle_command(0xE9);
        assert_eq!(mouse.read_output(), Some(0xFA));
        let _status1 = mouse.read_output();
        let _resolution = mouse.read_output();
        assert_eq!(
            mouse.read_output(),
            Some(2),
            "Status Request must echo the raw programmed rate, not the sanitised one"
        );

        // ...but reporting is paced at the 100 Hz default, not at 2 Hz.  The
        // device is constructed with its last packet 100 ms in the past, so a
        // 2 Hz cadence (500 ms) would refuse this packet outright.
        mouse.handle_command(0xF4); // enable data reporting
        assert_eq!(mouse.read_output(), Some(0xFA));
        mouse.accumulate_movement(4, 0);
        assert!(
            mouse.maybe_generate_packet(1000),
            "an out-of-spec rate must not throttle the report stream to a crawl"
        );
    }

    #[test]
    fn repeated_out_of_spec_sample_rates_follow_the_bounded_warning_schedule() {
        let mut mouse = Ps2Mouse::new();

        let warnings: Vec<_> = (0..4096)
            .filter_map(|_| mouse.handle_set_sample_rate(2))
            .collect();
        assert_eq!(
            warnings,
            vec![
                (1, 0),
                (16, 14),
                (32, 15),
                (64, 31),
                (128, 63),
                (256, 127),
                (512, 255),
                (1024, 511),
                (2048, 1023),
                (4096, 2047),
            ]
        );
        assert_eq!(mouse.invalid_sample_rate_requests, 4096);
        assert_eq!(mouse.sample_rate, 2);
        assert_eq!(mouse.effective_sample_rate(), Ps2Mouse::DEFAULT_SAMPLE_RATE);
    }

    #[test]
    fn valid_sample_rate_still_paces_the_report_stream() {
        let mut mouse = Ps2Mouse::new();
        set_sample_rate(&mut mouse, 10); // valid, and the slowest rate a host may pick
        mouse.handle_command(0xF4);
        assert_eq!(mouse.read_output(), Some(0xFA));

        mouse.accumulate_movement(4, 0);
        assert!(mouse.maybe_generate_packet(1_000), "first packet is due");
        while mouse.read_output().is_some() {}

        // A second packet before the interval elapses is refused: 10 Hz means
        // 100 ms apart. The out-of-spec guard must not have turned rate
        // limiting off wholesale. GUEST milliseconds, so this is a statement
        // about the rate and not about how fast the host ran the test
        // (MDD-BUG-FLU-01488).
        mouse.accumulate_movement(4, 0);
        assert!(
            !mouse.maybe_generate_packet(1_099),
            "a valid programmed rate must still pace the stream"
        );

        // And the boundary is where the rate says it is, not a millisecond
        // later: at exactly 100 ms the next packet is due.
        assert!(
            mouse.maybe_generate_packet(1_100),
            "the interval elapsed, so the stream resumes"
        );
    }

    #[test]
    fn test_standard_mouse_ignores_intellimouse_probe() {
        let mut mouse = Ps2Mouse::new();

        send_intellimouse_probe(&mut mouse);

        assert!(!mouse.is_intellimouse());

        mouse.handle_command(0xF2);
        assert_eq!(mouse.read_output(), Some(0xFA));
        assert_eq!(mouse.read_output(), Some(0x00)); // Standard PS/2 mouse
    }

    #[test]
    fn test_standard_mouse_reports_three_byte_packets_after_probe() {
        let mut mouse = Ps2Mouse::new();

        send_intellimouse_probe(&mut mouse);
        mouse.handle_command(0xF4);
        assert_eq!(mouse.read_output(), Some(0xFA));

        mouse.accumulate_movement(127, 0);

        assert!(mouse.maybe_generate_packet(4000));
        assert_eq!(mouse.output_queue_len(), 3);
        assert_eq!(mouse.read_output(), Some(0x08));
        assert_eq!(mouse.read_output(), Some(0x7F));
        assert_eq!(mouse.read_output(), Some(0x00));
        assert_eq!(mouse.read_output(), None);
    }

    #[test]
    fn test_intellimouse_capable_detection() {
        let mut mouse = Ps2Mouse::new_intellimouse();

        send_intellimouse_probe(&mut mouse);

        assert!(mouse.is_intellimouse());

        // Now device ID should be 0x03
        mouse.handle_command(0xF2);
        assert_eq!(mouse.read_output(), Some(0xFA));
        assert_eq!(mouse.read_output(), Some(0x03)); // Intellimouse
    }

    #[test]
    fn test_enable_disable_reporting() {
        let mut mouse = Ps2Mouse::new();

        mouse.handle_command(0xF4); // Enable reporting
        mouse.read_output(); // ACK
        assert!(mouse.is_reporting_enabled());

        mouse.handle_command(0xF5); // Disable reporting
        mouse.read_output(); // ACK
        assert!(!mouse.is_reporting_enabled());
    }

    #[test]
    fn test_calibration_epoch_tracks_transport_mutating_commands() {
        let mut mouse = Ps2Mouse::new();
        let start = mouse.calibration_attribution_epoch();

        for cmd in [0xF4, 0xF5, 0xF6, 0xEA, 0xF0, 0xEE, 0xEC, 0xFF] {
            mouse.handle_command(cmd);
            while mouse.read_output().is_some() {}
        }

        assert!(mouse.calibration_attribution_epoch() >= start + 8);
    }

    #[test]
    fn test_calibration_epoch_tracks_packet_discard_for_read_only_commands() {
        for command in [0xE9, 0xEB, 0xF2, 0xFE, 0x00] {
            let mut mouse = Ps2Mouse::new();
            mouse.handle_command(0xF4);
            while mouse.read_output().is_some() {}

            mouse.accumulate_movement(8, 0);
            assert!(
                mouse.maybe_generate_packet(5000),
                "command {command:02X} setup should queue a movement packet"
            );
            assert!(
                mouse.has_packet_output(),
                "command {command:02X} setup should leave packet bytes queued"
            );

            let before = mouse.calibration_attribution_epoch();
            mouse.handle_command(command);

            assert!(
                mouse.calibration_attribution_epoch() > before,
                "command {command:02X} must invalidate attribution when it discards packet bytes"
            );
            assert!(
                !mouse.has_packet_output(),
                "command {command:02X} must not leave discarded async packet bytes queued"
            );
        }
    }

    #[test]
    fn test_set_resolution() {
        let mut mouse = Ps2Mouse::new();

        mouse.handle_command(0xE8); // Set Resolution
        mouse.read_output(); // ACK
        mouse.handle_command(3); // 8 counts/mm
        mouse.read_output(); // ACK

        // Check via status request
        mouse.handle_command(0xE9);
        mouse.read_output(); // ACK
        mouse.read_output(); // status1
        assert_eq!(mouse.read_output(), Some(3)); // resolution
    }

    #[test]
    fn test_set_scaling() {
        let mut mouse = Ps2Mouse::new();

        mouse.handle_command(0xE7); // Set Scaling 2:1
        assert_eq!(mouse.read_output(), Some(0xFA));

        mouse.handle_command(0xE6); // Set Scaling 1:1
        assert_eq!(mouse.read_output(), Some(0xFA));
    }

    #[test]
    fn test_stream_mode() {
        let mut mouse = Ps2Mouse::new();

        mouse.handle_command(0xEA); // Set Stream Mode
        assert_eq!(mouse.read_output(), Some(0xFA));
    }

    #[test]
    fn test_remote_mode() {
        let mut mouse = Ps2Mouse::new();

        mouse.handle_command(0xF0); // Set Remote Mode
        assert_eq!(mouse.read_output(), Some(0xFA));
    }

    #[test]
    fn test_wrap_mode() {
        let mut mouse = Ps2Mouse::new();

        mouse.handle_command(0xEE); // Set Wrap Mode
        assert_eq!(mouse.read_output(), Some(0xFA));

        // In wrap mode, commands should be echoed back (except 0xEC and 0xFF)
        mouse.handle_command(0x42);
        assert_eq!(mouse.read_output(), Some(0x42)); // Echoed

        // Reset wrap mode
        mouse.handle_command(0xEC);
        assert_eq!(mouse.read_output(), Some(0xFA));
    }

    #[test]
    fn test_status_request() {
        let mut mouse = Ps2Mouse::new();
        mouse.set_buttons(0x05); // Left + middle

        mouse.handle_command(0xE9); // Status Request
        assert_eq!(mouse.read_output(), Some(0xFA)); // ACK

        let status1 = mouse.read_output().unwrap();
        // Stream mode (bit 6 = 0), disabled (bit 5 = 0), 1:1 scaling (bit 4 = 0)
        // Buttons: Left (bit 2), Middle (bit 1)
        assert_eq!(status1 & 0x06, 0x06); // Left + middle in status format

        assert_eq!(mouse.read_output(), Some(2)); // Default resolution
        assert_eq!(mouse.read_output(), Some(100)); // Default sample rate
    }

    #[test]
    fn test_set_defaults() {
        let mut mouse = Ps2Mouse::new();

        // Change some settings
        mouse.handle_command(0xF4); // Enable reporting
        mouse.read_output();
        mouse.handle_command(0xE7); // 2:1 scaling
        mouse.read_output();

        // Set defaults
        mouse.handle_command(0xF6);
        assert_eq!(mouse.read_output(), Some(0xFA));

        // Verify defaults restored
        assert!(!mouse.is_reporting_enabled());
        assert_eq!(mouse.sample_rate, 100);
        assert_eq!(mouse.resolution, 2);
    }

    #[test]
    fn test_read_data_remote_mode() {
        let mut mouse = Ps2Mouse::new();

        mouse.handle_command(0xF0); // Remote mode
        mouse.read_output();

        mouse.accumulate_movement(10, 5);

        mouse.handle_command(0xEB); // Read Data
        assert!(
            !mouse.has_packet_output(),
            "0xEB reply bytes are polled command-response data, not async IRQ packets"
        );
        assert_eq!(mouse.read_output(), Some(0xFA)); // ACK

        // Should get a 3-byte packet
        assert!(mouse.has_output());
        let status = mouse.read_output().unwrap();
        assert_eq!(status & 0x08, 0x08); // Bit 3 always set
        mouse.read_output(); // X
        mouse.read_output(); // Y
    }

    #[test]
    fn test_packet_generation_buttons() {
        let mut mouse = Ps2Mouse::new();
        mouse.handle_command(0xF4); // Enable reporting
        mouse.read_output();

        mouse.set_buttons(0x01); // Left button
        mouse.accumulate_movement(1, 0); // Need some movement to trigger

        assert!(mouse.maybe_generate_packet(6000));

        let status = mouse.read_output().unwrap();
        assert_eq!(status & 0x01, 0x01); // Left button in packet
    }

    #[test]
    fn test_packet_y_axis_inversion() {
        let mut mouse = Ps2Mouse::new();
        mouse.handle_command(0xF4);
        mouse.read_output();

        // Moving down on screen (positive dy) should be negative in PS/2
        mouse.accumulate_movement(0, 10);

        assert!(mouse.maybe_generate_packet(7000));

        let status = mouse.read_output().unwrap();
        assert_eq!(status & 0x20, 0x20); // Y sign bit should be set (negative)
    }

    #[test]
    fn test_2to1_scaling() {
        let mouse = Ps2Mouse::new();

        // Test the scaling table
        assert_eq!(mouse.apply_2to1_scaling(0), 0);
        assert_eq!(mouse.apply_2to1_scaling(1), 1);
        assert_eq!(mouse.apply_2to1_scaling(2), 1);
        assert_eq!(mouse.apply_2to1_scaling(3), 3);
        assert_eq!(mouse.apply_2to1_scaling(4), 6);
        assert_eq!(mouse.apply_2to1_scaling(5), 9);
        assert_eq!(mouse.apply_2to1_scaling(6), 12);
        assert_eq!(mouse.apply_2to1_scaling(-5), -9);
    }

    #[test]
    fn test_unknown_command() {
        let mut mouse = Ps2Mouse::new();
        mouse.handle_command(0x00); // Unknown command
        assert_eq!(mouse.read_output(), Some(0xFE)); // NACK
    }

    #[test]
    fn test_resend_last_byte() {
        let mut mouse = Ps2Mouse::new();
        mouse.handle_command(0xF4); // Enable reporting
        assert_eq!(mouse.read_output(), Some(0xFA)); // ACK

        mouse.handle_command(0xFE); // Resend
        assert_eq!(mouse.read_output(), Some(0xFA)); // Resent last byte
    }

    #[test]
    fn test_wrap_mode_echoes_commands() {
        let mut mouse = Ps2Mouse::new();
        mouse.handle_command(0xEE); // Set Wrap Mode
        assert_eq!(mouse.read_output(), Some(0xFA)); // ACK

        mouse.handle_command(0xF4); // Enable reporting (echoed in wrap mode)
        assert_eq!(mouse.read_output(), Some(0xF4));
    }

    #[test]
    fn test_wrap_mode_does_not_echo_reset_or_unwrap_commands() {
        let mut mouse = Ps2Mouse::new();
        mouse.handle_command(0xEE); // Set Wrap Mode
        assert_eq!(mouse.read_output(), Some(0xFA));

        // 0xEC must be handled (exit wrap), not echoed.
        mouse.handle_command(0xEC);
        assert_eq!(mouse.read_output(), Some(0xFA));
        assert_eq!(mouse.mode, MouseMode::Stream);

        // Re-enter wrap and verify reset command is also handled, not echoed.
        mouse.handle_command(0xEE);
        assert_eq!(mouse.read_output(), Some(0xFA));
        mouse.handle_command(0xFF);
        assert_eq!(mouse.read_output(), Some(0xFA)); // ACK
        assert_eq!(mouse.read_output(), Some(0xAA)); // BAT success
    }

    #[test]
    fn test_status_request_sets_bits() {
        let mut mouse = Ps2Mouse::new();
        mouse.mode = MouseMode::Remote;
        mouse.reporting_enabled = true;
        mouse.scaling = Scaling::TwoToOne;
        mouse.buttons = 0x07;

        mouse.handle_command(0xE9); // Status request
        assert_eq!(mouse.read_output(), Some(0xFA)); // ACK
        let status = mouse.read_output().unwrap();
        assert_eq!(status & 0x70, 0x70);
        assert_eq!(status & 0x07, 0x07);
        assert_eq!(mouse.read_output(), Some(mouse.resolution));
        assert_eq!(mouse.read_output(), Some(mouse.sample_rate));
    }

    #[test]
    fn test_intellimouse_packet() {
        let mut mouse = Ps2Mouse::new_intellimouse();

        send_intellimouse_probe(&mut mouse);
        assert!(mouse.is_intellimouse());

        // Enable reporting
        mouse.handle_command(0xF4);
        assert_eq!(mouse.read_output(), Some(0xFA));

        mouse.accumulate_movement(5, 5);
        mouse.add_wheel(2);

        assert!(mouse.maybe_generate_packet(8000));

        // Should be 4-byte packet
        assert_eq!(mouse.output_queue_len(), 4);
        mouse.read_output(); // status
        mouse.read_output(); // X
        mouse.read_output(); // Y
        let wheel = mouse.read_output().unwrap();
        assert_eq!(wheel & 0x0F, 2); // Wheel delta
    }

    #[test]
    fn test_packet_status_bits_buttons_and_overflow() {
        let mut mouse = Ps2Mouse::new();
        mouse.reporting_enabled = true;
        mouse.mode = MouseMode::Stream;
        mouse.last_packet_ms = Some(0);
        mouse.buttons = 0x07;
        mouse.delta_x = -300;
        mouse.delta_y = -400;

        assert!(mouse.maybe_generate_packet(9000));

        let status = mouse.read_output().unwrap();
        assert_eq!(status & 0x07, 0x07);
        assert_eq!(status & 0xF0, 0xF0);
    }

    /// A burst of host motion must not bank a backlog for later reports to
    /// replay, or the guest keeps receiving movement long after the host
    /// pointer stopped - the "very sticky" real-boot pointer of
    /// `bugs/MDD-BUG-KILN-00139.md`. A real device reports what one sample
    /// interval can carry, flags the overrun, and drops the rest
    /// (`bugs/MDD-BUG-FLU-00359.md` settled the same question on the Sun wire).
    #[test]
    fn test_motion_beyond_one_report_is_discarded_not_replayed() {
        let mut mouse = Ps2Mouse::new();
        mouse.reporting_enabled = true;
        mouse.mode = MouseMode::Stream;

        // 100 host events of 300 guest pixels, with one report escaping the
        // sample-rate gate - the shape measured on the live real-boot path.
        for _ in 0..100 {
            mouse.accumulate_movement(300, 0);
        }
        assert_eq!(mouse.pending_movement(), (30_000, 0));

        assert!(mouse.maybe_generate_packet(10000));

        assert_eq!(
            mouse.pending_movement(),
            (0, 0),
            "one report consumes the whole interval; residue replays as lag"
        );
        let status = mouse.read_output().expect("status byte");
        assert_ne!(
            status & 0x40,
            0,
            "dropped X motion is declared as overflow, never silently"
        );
        assert_eq!(mouse.read_output(), Some(255), "X carries what fits");
    }

    /// The overflow path must not coarsen ordinary pointer movement: a sample
    /// that fits one report is still reported exactly, with no overflow bits.
    #[test]
    fn test_motion_within_one_report_is_reported_exactly() {
        let mut mouse = Ps2Mouse::new();
        mouse.reporting_enabled = true;
        mouse.mode = MouseMode::Stream;

        mouse.accumulate_movement(200, -100);
        assert!(mouse.maybe_generate_packet(11000));

        let status = mouse.read_output().expect("status byte");
        assert_eq!(status & 0xC0, 0, "a representable sample never overflows");
        assert_eq!(mouse.read_output(), Some(200));
        assert_eq!(mouse.read_output(), Some(100), "PS/2 Y is screen-inverted");
        assert_eq!(mouse.pending_movement(), (0, 0));
    }

    #[test]
    fn test_maybe_generate_packet_rate_limit_blocks() {
        let mut mouse = Ps2Mouse::new();
        mouse.reporting_enabled = true;
        mouse.mode = MouseMode::Stream;
        mouse.delta_x = 1;
        mouse.last_packet_ms = Some(0);

        // Default 100 Hz means 10 ms apart; 5 ms in, the packet is not due.
        assert!(!mouse.maybe_generate_packet(5));
    }

    #[test]
    fn test_maybe_generate_packet_reporting_disabled() {
        let mut mouse = Ps2Mouse::new();
        mouse.mode = MouseMode::Stream;
        mouse.delta_x = 1;
        mouse.last_packet_ms = Some(0);

        assert!(!mouse.maybe_generate_packet(13000));
    }

    #[test]
    fn test_maybe_generate_packet_non_stream_returns_false() {
        let mut mouse = Ps2Mouse::new();
        mouse.reporting_enabled = true;
        mouse.mode = MouseMode::Remote;
        mouse.delta_x = 1;
        mouse.last_packet_ms = Some(0);

        assert!(!mouse.maybe_generate_packet(14000));
    }

    #[test]
    fn test_intellimouse_wheel_only_packet() {
        let mut mouse = Ps2Mouse::new_intellimouse();

        send_intellimouse_probe(&mut mouse);
        assert!(mouse.is_intellimouse());

        // Enable reporting
        mouse.handle_command(0xF4);
        assert_eq!(mouse.read_output(), Some(0xFA));

        mouse.add_wheel(1);

        assert!(mouse.maybe_generate_packet(15000));
        assert_eq!(mouse.output_queue_len(), 4);
        mouse.read_output(); // status
        mouse.read_output(); // X
        mouse.read_output(); // Y
        let wheel = mouse.read_output().unwrap();
        assert_eq!(wheel & 0x0F, 1);
    }

    #[test]
    fn test_no_packet_when_no_movement() {
        let mut mouse = Ps2Mouse::new();
        mouse.handle_command(0xF4);
        mouse.read_output();

        // No movement, no button change
        assert!(!mouse.maybe_generate_packet(16000));
    }

    #[test]
    fn stream_report_pending_only_tracks_unreported_stream_input() {
        let mut mouse = Ps2Mouse::new();
        mouse.accumulate_movement(4, 0);
        assert!(!mouse.stream_report_pending());

        mouse.handle_command(0xF4);
        assert_eq!(mouse.read_output(), Some(0xFA));
        assert!(mouse.stream_report_pending());
        assert!(mouse.maybe_generate_packet(17000));
        assert!(!mouse.stream_report_pending());
    }

    #[test]
    fn test_packet_on_button_change() {
        let mut mouse = Ps2Mouse::new();
        mouse.handle_command(0xF4);
        mouse.read_output();

        // Change buttons without movement
        mouse.set_buttons(0x01);
        assert!(mouse.maybe_generate_packet(18000));
    }

    #[test]
    fn test_movement_packet_reports_current_button_state() {
        let mut mouse = Ps2Mouse::new();
        mouse.handle_command(0xF4);
        assert_eq!(mouse.read_output(), Some(0xFA));

        mouse.set_buttons(0x01);
        mouse.accumulate_movement(20, 0);
        assert!(mouse.maybe_generate_packet(19000));

        let status = mouse.read_output().unwrap();
        assert_eq!(status & 0x09, 0x09);
        assert_eq!(mouse.read_output(), Some(20));
        assert_eq!(mouse.read_output(), Some(0));
    }

    #[test]
    fn test_explicit_release_movement_packet_clears_button_bits() {
        let mut mouse = Ps2Mouse::new();
        mouse.handle_command(0xF4);
        assert_eq!(mouse.read_output(), Some(0xFA));

        mouse.set_buttons(0x01);
        assert!(mouse.maybe_generate_packet(20000));
        assert_eq!(mouse.read_output().unwrap() & 0x01, 0x01);
        assert_eq!(mouse.read_output(), Some(0));
        assert_eq!(mouse.read_output(), Some(0));

        mouse.set_buttons(0x00);
        mouse.accumulate_movement(20, 0);
        assert!(mouse.maybe_generate_packet(21000));

        let status = mouse.read_output().unwrap();
        assert_eq!(status & 0x09, 0x08);
        assert_eq!(mouse.read_output(), Some(20));
        assert_eq!(mouse.read_output(), Some(0));
    }

    #[test]
    fn test_button_change_retries_when_fifo_lacks_packet_room() {
        let mut mouse = Ps2Mouse::new();
        mouse.handle_command(0xF4);
        assert_eq!(mouse.read_output(), Some(0xFA));

        for _ in 0..30 {
            mouse.handle_command(0x00);
        }
        assert_eq!(mouse.output_queue_len(), 30);

        mouse.set_buttons(0x01);
        let last_packet_ms = mouse.last_packet_ms;

        assert!(!mouse.maybe_generate_packet(22000));
        assert_eq!(mouse.output_queue_len(), 30);
        assert_eq!(mouse.last_packet_ms, last_packet_ms);
        assert_eq!(mouse.last_buttons, 0);

        assert_eq!(mouse.read_output(), Some(0xFE));
        assert_eq!(mouse.output_queue_len(), 29);

        assert!(mouse.maybe_generate_packet(23000));
        assert_eq!(mouse.output_queue_len(), 32);
        assert_eq!(mouse.last_buttons, 0x01);

        for _ in 0..29 {
            assert_eq!(mouse.read_output(), Some(0xFE));
        }
        let status = mouse.read_output().unwrap();
        assert_eq!(status & 0x01, 0x01);
        assert_eq!(mouse.read_output(), Some(0));
        assert_eq!(mouse.read_output(), Some(0));
    }

    #[test]
    fn test_fb_position_sentinel_returns_none() {
        let state = SharedMouseState::new();
        assert_eq!(state.take_fb_position(), None);
    }

    #[test]
    fn test_fb_position_set_then_take() {
        let state = SharedMouseState::new();
        state.set_fb_position(160, 100);
        assert_eq!(state.take_fb_position(), Some((160, 100)));
    }

    #[test]
    fn test_fb_position_take_clears_value() {
        let state = SharedMouseState::new();
        state.set_fb_position(50, 75);
        assert_eq!(state.take_fb_position(), Some((50, 75)));
        assert_eq!(state.take_fb_position(), None);
    }

    #[test]
    fn test_fb_position_last_set_wins() {
        let state = SharedMouseState::new();
        state.set_fb_position(10, 20);
        state.set_fb_position(30, 40);
        state.set_fb_position(50, 60);
        assert_eq!(state.take_fb_position(), Some((50, 60)));
    }

    #[test]
    fn test_fb_position_round_trip_coordinates() {
        let state = SharedMouseState::new();

        // Origin
        state.set_fb_position(0, 0);
        assert_eq!(state.take_fb_position(), Some((0, 0)));

        // Mode 13h extents
        state.set_fb_position(319, 199);
        assert_eq!(state.take_fb_position(), Some((319, 199)));

        // Large framebuffer
        state.set_fb_position(1920, 1080);
        assert_eq!(state.take_fb_position(), Some((1920, 1080)));
    }

    #[test]
    fn test_seamless_target_is_persistent_and_distinct_from_one_shot_fb_position() {
        let state = SharedMouseState::new();

        state.add_movement(7, -4);
        state.set_fb_position(10, 20);
        let seq = state.set_seamless_target(160, 100);

        assert_eq!(state.take_movement(), (7, -4));
        assert_eq!(state.take_fb_position(), Some((10, 20)));
        assert_eq!(state.take_fb_position(), None);

        let target = state.get_seamless_target().expect("seamless target");
        assert_eq!(
            target,
            SeamlessMouseTarget {
                x: 160,
                y: 100,
                seq
            }
        );
        assert_eq!(
            state.get_seamless_target(),
            Some(SeamlessMouseTarget {
                x: 160,
                y: 100,
                seq
            }),
            "seamless target is persistent, not consumed like fb_position"
        );
    }

    #[test]
    fn test_seamless_target_sequence_changes_only_on_target_change() {
        let state = SharedMouseState::new();

        let seq1 = state.set_seamless_target(1, 2);
        let seq2 = state.set_seamless_target(1, 2);
        let seq3 = state.set_seamless_target(2, 2);

        assert_eq!(seq1, seq2);
        assert!(seq3 > seq2);
        assert!(state.clear_seamless_target());
        assert_eq!(state.get_seamless_target(), None);
        assert!(!state.clear_seamless_target());
    }

    #[test]
    fn test_seamless_target_clear_then_same_target_gets_new_sequence() {
        let state = SharedMouseState::new();

        let seq1 = state.set_seamless_target(1, 2);
        assert!(state.clear_seamless_target());
        let seq2 = state.set_seamless_target(1, 2);

        assert!(
            seq2 > seq1,
            "clearing must make a later identical target observable"
        );
        assert_eq!(
            state.get_seamless_target(),
            Some(SeamlessMouseTarget {
                x: 1,
                y: 2,
                seq: seq2
            })
        );
    }

    #[test]
    fn test_real_boot_mouse_sync_maps_framebuffer_target_to_guest_range() {
        let mut sync = RealBootMouseSync::new();
        let target = SeamlessMouseTarget {
            x: 319,
            y: 199,
            seq: 1,
        };

        let step = sync.tick(Some(target), (320, 200));

        let (guest_x, guest_y) = step.target_guest.expect("guest target");
        assert_eq!(guest_x, 639.0);
        assert_eq!(guest_y, 199.0);
    }

    #[test]
    fn test_real_boot_mouse_sync_emits_bounded_correction() {
        let mut sync = RealBootMouseSync::new();
        sync.observe_position(0, 0);
        let target = SeamlessMouseTarget {
            x: 319,
            y: 199,
            seq: 1,
        };

        let step = sync.tick(Some(target), (320, 200));

        assert_eq!(
            step.emitted(),
            (DEFAULT_MAX_MICKEYS_PER_TICK, DEFAULT_MAX_MICKEYS_PER_TICK)
        );
        assert_eq!(step.estimate_x, DEFAULT_MAX_MICKEYS_PER_TICK as f64);
        assert_eq!(step.estimate_y, DEFAULT_MAX_MICKEYS_PER_TICK as f64);
    }

    #[test]
    fn test_real_boot_mouse_sync_converges_without_overshooting_integer_target() {
        let mut sync = RealBootMouseSync::new();
        sync.observe_position(0, 0);
        let target = SeamlessMouseTarget {
            x: 64,
            y: 0,
            seq: 1,
        };

        assert_eq!(sync.tick(Some(target), (640, 200)).emitted(), (32, 0));
        assert_eq!(sync.tick(Some(target), (640, 200)).emitted(), (32, 0));
        assert_eq!(sync.tick(Some(target), (640, 200)).emitted(), (0, 0));
    }

    #[test]
    fn test_real_boot_mouse_sync_residual_accumulates_sub_mickey_error() {
        let mut sync = RealBootMouseSync::new();
        sync.observe_position(0, 0);
        let target = SeamlessMouseTarget { x: 0, y: 1, seq: 1 };

        let first = sync.tick(Some(target), (640, 400));
        let second = sync.tick(Some(target), (640, 400));
        let third = sync.tick(Some(target), (640, 400));

        assert_eq!(first.emitted(), (0, 0));
        assert_eq!(second.emitted(), (0, 0));
        assert_eq!(third.emitted(), (0, 1));
        assert!(third.residual_y.abs() < 0.5);
    }

    #[test]
    fn test_real_boot_mouse_sync_clear_target_stops_output_and_resets_residual() {
        let mut sync = RealBootMouseSync::new();
        sync.observe_position(0, 0);
        let target = SeamlessMouseTarget { x: 0, y: 1, seq: 1 };

        let first = sync.tick(Some(target), (640, 400));
        assert_eq!(first.emitted(), (0, 0));
        assert!(first.residual_y > 0.0);

        let cleared = sync.tick(None, (640, 400));
        assert_eq!(cleared.emitted(), (0, 0));
        assert_eq!(cleared.residual_y, 0.0);
    }

    #[test]
    fn test_real_boot_mouse_sync_clear_then_reacquire_zero_aligns_synthetic() {
        let mut sync = RealBootMouseSync::new();
        let first = SeamlessMouseTarget { x: 0, y: 0, seq: 1 };
        let moved = SeamlessMouseTarget {
            x: 100,
            y: 0,
            seq: 2,
        };

        assert_eq!(sync.tick(Some(first), (320, 200)).emitted(), (0, 0));
        assert_ne!(sync.tick(Some(moved), (320, 200)).emitted(), (0, 0));

        let cleared = sync.tick(None, (320, 200));
        assert_eq!(cleared.emitted(), (0, 0));
        assert_eq!(sync.diagnostics().freshness, EstimateFreshness::None);

        let reacquired = sync.tick(Some(moved), (320, 200));
        assert_eq!(reacquired.emitted(), (0, 0));
        assert_eq!(
            sync.diagnostics().freshness,
            EstimateFreshness::SyntheticFromTarget
        );
        assert_eq!(reacquired.estimate_x, reacquired.target_guest.unwrap().0);
    }

    #[test]
    fn test_real_boot_mouse_sync_abandon_target_preserves_trusted_position() {
        let mut sync = RealBootMouseSync::new();
        sync.observe_position(50, 60);
        assert_eq!(
            sync.diagnostics().freshness,
            EstimateFreshness::ObservedInt33
        );

        let cleared = sync.tick(None, (320, 200));

        assert_eq!(cleared.emitted(), (0, 0));
        assert!(sync.is_position_seeded());
        assert_eq!(sync.estimate_x, 50.0);
        assert_eq!(sync.estimate_y, 60.0);
        assert!(!sync.diagnostics().gain_attribution_valid);
    }

    #[test]
    fn test_real_boot_mouse_sync_uses_gain_estimate_for_correction() {
        let mut sync = RealBootMouseSync::new();
        sync.set_gain_for_test(2.0, 2.0);
        sync.observe_position(0, 0);
        let target = SeamlessMouseTarget {
            x: 20,
            y: 0,
            seq: 1,
        };

        let step = sync.tick(Some(target), (640, 200));

        assert_eq!(step.emitted(), (10, 0));
        assert_eq!(step.estimate_x, 20.0);
    }

    #[test]
    fn test_real_boot_mouse_sync_observes_int33_ranges_for_target_mapping() {
        let mut sync = RealBootMouseSync::new();
        sync.observe_horizontal_range(100, 10);
        sync.observe_vertical_range(5, 25);
        sync.observe_position(10, 5);
        let target = SeamlessMouseTarget {
            x: 319,
            y: 199,
            seq: 1,
        };

        let step = sync.tick(Some(target), (320, 200));

        assert_eq!(step.target_guest, Some((100.0, 25.0)));
        assert_eq!(step.emitted(), (DEFAULT_MAX_MICKEYS_PER_TICK, 20));
    }

    #[test]
    fn test_real_boot_mouse_sync_seeds_estimate_from_observed_position() {
        let mut sync = RealBootMouseSync::new();
        sync.observe_horizontal_range(0, 319);
        sync.observe_vertical_range(0, 199);
        sync.observe_position(160, 100);
        let target = SeamlessMouseTarget {
            x: 160,
            y: 100,
            seq: 1,
        };

        let step = sync.tick(Some(target), (320, 200));

        assert!(sync.is_position_seeded());
        assert_eq!(step.emitted(), (0, 0));
        assert_eq!(step.estimate_x, 160.0);
        assert_eq!(step.estimate_y, 100.0);
    }

    #[test]
    fn test_real_boot_mouse_sync_emits_no_correction_before_position_seed() {
        let mut sync = RealBootMouseSync::new();
        let target = SeamlessMouseTarget {
            x: 319,
            y: 199,
            seq: 1,
        };

        let step = sync.tick(Some(target), (320, 200));

        assert!(!sync.is_position_seeded());
        assert_eq!(step.target_guest, Some((639.0, 199.0)));
        assert_eq!(step.emitted(), (0, 0));
        assert_eq!(step.estimate_x, 639.0);
        assert_eq!(step.estimate_y, 199.0);
        assert_eq!(
            sync.estimate_freshness,
            EstimateFreshness::SyntheticFromTarget
        );
    }

    #[test]
    fn test_real_boot_mouse_sync_full_reset_clears_seed_range_and_residuals() {
        let mut sync = RealBootMouseSync::new();
        sync.observe_horizontal_range(1000, 2000);
        sync.observe_vertical_range(3000, 4000);
        sync.observe_position(1500, 3500);
        let target = SeamlessMouseTarget { x: 1, y: 1, seq: 7 };

        let before = sync.tick(Some(target), (320, 200));
        assert!(sync.is_position_seeded());
        assert_ne!(before.target_guest, Some((2.0031347962382444, 1.0)));

        sync.full_reset();

        assert!(!sync.is_position_seeded());
        assert_eq!(sync.estimate_freshness, EstimateFreshness::None);
        assert_eq!(sync.estimate_x, 0.0);
        assert_eq!(sync.estimate_y, 0.0);
        assert_eq!(sync.residual_x, 0.0);
        assert_eq!(sync.residual_y, 0.0);
        assert_eq!(sync.target_seq_seen, None);

        let after = sync.tick(Some(target), (320, 200));
        assert_eq!(after.target_guest, Some((2.0031347962382444, 1.0)));
        assert_eq!(after.emitted(), (0, 0));
        assert!(!sync.is_position_seeded());
        assert_eq!(
            sync.estimate_freshness,
            EstimateFreshness::SyntheticFromTarget
        );
    }

    #[test]
    fn test_real_boot_mouse_sync_treats_int33_values_as_unsigned() {
        let mut sync = RealBootMouseSync::new();
        sync.observe_horizontal_range(40000, 50000);
        sync.observe_vertical_range(50000, 60000);
        sync.observe_position(45000, 55000);
        let target = SeamlessMouseTarget {
            x: 319,
            y: 199,
            seq: 1,
        };

        let step = sync.tick(Some(target), (320, 200));

        assert_eq!(step.target_guest, Some((50000.0, 60000.0)));
        assert_eq!(step.estimate_x, 45032.0);
        assert_eq!(step.estimate_y, 55032.0);
        assert_eq!(
            step.emitted(),
            (DEFAULT_MAX_MICKEYS_PER_TICK, DEFAULT_MAX_MICKEYS_PER_TICK)
        );
    }

    #[test]
    fn test_real_boot_mouse_sync_first_target_fallback_avoids_top_left_overshoot() {
        let mut sync = RealBootMouseSync::new();
        let first = SeamlessMouseTarget {
            x: 200,
            y: 120,
            seq: 1,
        };
        let second = SeamlessMouseTarget {
            x: 210,
            y: 120,
            seq: 2,
        };

        let first_step = sync.tick(Some(first), (320, 200));
        let second_step = sync.tick(Some(second), (320, 200));

        assert_eq!(first_step.emitted(), (0, 0));
        assert_eq!(
            sync.estimate_freshness,
            EstimateFreshness::SyntheticFromTarget
        );
        assert!(
            !sync.is_position_seeded(),
            "synthetic fallback must remain low-confidence"
        );
        assert_eq!(
            second_step.emitted(),
            (SYNTHETIC_MAX_MICKEYS_PER_TICK, 0),
            "synthetic fallback must stay below guest acceleration thresholds"
        );
        assert_eq!(sync.diagnostics().confidence, 0.0);
        assert_eq!(second_step.emitted().1, 0);
    }

    #[test]
    fn test_real_boot_mouse_sync_synthetic_framebuffer_seed_moves_from_known_position() {
        let mut sync = RealBootMouseSync::new();
        sync.assume_video_mode_default_range(0x12, (640, 480));
        sync.seed_synthetic_framebuffer_position(320, 240, (640, 480));

        let step = sync.tick(
            Some(SeamlessMouseTarget {
                x: 100,
                y: 100,
                seq: 1,
            }),
            (640, 480),
        );

        assert_eq!(
            sync.estimate_freshness,
            EstimateFreshness::SyntheticFromTarget
        );
        assert!(!sync.is_position_seeded());
        assert_eq!(step.emitted(), (-1, -1));
        assert_eq!(sync.diagnostics().confidence, 0.0);
    }

    #[test]
    fn test_real_boot_mouse_sync_observed_position_replaces_synthetic_seed() {
        let mut sync = RealBootMouseSync::new();
        let target = SeamlessMouseTarget {
            x: 200,
            y: 120,
            seq: 1,
        };

        sync.tick(Some(target), (320, 200));
        assert_eq!(
            sync.estimate_freshness,
            EstimateFreshness::SyntheticFromTarget
        );

        sync.observe_position(10, 20);

        assert_eq!(sync.estimate_freshness, EstimateFreshness::ObservedInt33);
        assert_eq!(sync.estimate_x, 10.0);
        assert_eq!(sync.estimate_y, 20.0);
        assert!(sync.is_position_seeded());
    }

    #[test]
    fn test_real_boot_mouse_sync_range_change_after_synthetic_seed_realigns() {
        let mut sync = RealBootMouseSync::new();
        let target = SeamlessMouseTarget {
            x: 319,
            y: 199,
            seq: 1,
        };

        let synthetic = sync.tick(Some(target), (320, 200));
        assert_eq!(synthetic.target_guest, Some((639.0, 199.0)));
        assert_eq!(
            sync.diagnostics().freshness,
            EstimateFreshness::SyntheticFromTarget
        );

        sync.observe_horizontal_range(0, 319);
        sync.observe_vertical_range(0, 199);

        let realigned = sync.tick(Some(target), (320, 200));
        assert_eq!(realigned.target_guest, Some((319.0, 199.0)));
        assert_eq!(realigned.emitted(), (0, 0));
        assert_eq!(realigned.estimate_x, 319.0);
        assert_eq!(realigned.estimate_y, 199.0);
        assert_eq!(
            sync.diagnostics().freshness,
            EstimateFreshness::SyntheticFromTarget
        );
    }

    #[test]
    fn test_real_boot_mouse_sync_synthetic_only_runs_bounded_open_loop_without_calibrating() {
        let mut sync = RealBootMouseSync::new();

        let first = sync.tick(Some(SeamlessMouseTarget { x: 0, y: 0, seq: 1 }), (320, 200));
        assert_eq!(first.emitted(), (0, 0));

        for seq in 2..=64 {
            let target = SeamlessMouseTarget {
                x: (seq * 5) as i32,
                y: (seq * 3) as i32,
                seq,
            };
            let step = sync.tick(Some(target), (320, 200));
            assert!(step.emitted_x.abs() <= DEFAULT_MAX_MICKEYS_PER_TICK);
            assert!(step.emitted_y.abs() <= DEFAULT_MAX_MICKEYS_PER_TICK);
            assert!(!sync.is_position_seeded());
            assert_eq!(
                sync.estimate_freshness,
                EstimateFreshness::SyntheticFromTarget
            );
            assert_eq!(sync.diagnostics().confidence, 0.0);
        }
    }

    #[test]
    fn test_real_boot_mouse_sync_true_observation_enables_closed_loop_after_synthetic() {
        let mut sync = RealBootMouseSync::new();
        let synthetic_target = SeamlessMouseTarget {
            x: 200,
            y: 120,
            seq: 1,
        };
        assert_eq!(
            sync.tick(Some(synthetic_target), (320, 200)).emitted(),
            (0, 0)
        );

        sync.observe_position(0, 0);
        let observed_target = SeamlessMouseTarget {
            x: 319,
            y: 199,
            seq: 2,
        };
        let step = sync.tick(Some(observed_target), (320, 200));

        assert_eq!(sync.estimate_freshness, EstimateFreshness::ObservedInt33);
        assert_eq!(
            step.emitted(),
            (DEFAULT_MAX_MICKEYS_PER_TICK, DEFAULT_MAX_MICKEYS_PER_TICK)
        );
    }

    #[test]
    fn test_real_boot_mouse_sync_first_observation_sets_confidence_and_diagnostics() {
        let mut sync = RealBootMouseSync::new();

        sync.observe_horizontal_range(10, 110);
        sync.observe_vertical_range(20, 220);
        sync.observe_position(60, 120);

        let diagnostics = sync.diagnostics();
        assert_eq!(diagnostics.freshness, EstimateFreshness::ObservedInt33);
        assert_eq!(diagnostics.confidence, FIRST_OBSERVATION_CONFIDENCE);
        assert_eq!(diagnostics.gain_x, DEFAULT_MOUSE_SYNC_GAIN);
        assert_eq!(diagnostics.gain_y, DEFAULT_MOUSE_SYNC_GAIN);
        assert_eq!(diagnostics.last_observed_x, Some(60.0));
        assert_eq!(diagnostics.last_observed_y, Some(120.0));
        assert_eq!(diagnostics.emitted_since_observation_x, 0);
        assert_eq!(diagnostics.emitted_since_observation_y, 0);
        assert_eq!(diagnostics.last_emitted_x, 0);
        assert_eq!(diagnostics.last_emitted_y, 0);
        assert_eq!(diagnostics.range_x, (10.0, 110.0));
        assert_eq!(diagnostics.range_y, (20.0, 220.0));
        assert_eq!(diagnostics.range_source_x, RangeSource::ObservedInt33);
        assert_eq!(diagnostics.range_source_y, RangeSource::ObservedInt33);
        assert_eq!(diagnostics.residual_x, 0.0);
        assert_eq!(diagnostics.residual_y, 0.0);
        assert!(diagnostics.gain_attribution_valid);
    }

    #[test]
    fn test_real_boot_mouse_sync_default_range_source_is_fallback() {
        let sync = RealBootMouseSync::new();
        let diagnostics = sync.diagnostics();

        assert_eq!(diagnostics.range_x, (0.0, 639.0));
        assert_eq!(diagnostics.range_y, (0.0, 199.0));
        assert_eq!(diagnostics.range_source_x, RangeSource::AssumedFallback);
        assert_eq!(diagnostics.range_source_y, RangeSource::AssumedFallback);
    }

    #[test]
    fn test_real_boot_mouse_sync_uses_video_mode_default_ranges_before_int33_ranges() {
        let mut sync = RealBootMouseSync::new();

        sync.assume_video_mode_default_range(0x12, (640, 480));
        let video_mode_step = sync.tick(
            Some(SeamlessMouseTarget {
                x: 639,
                y: 479,
                seq: 1,
            }),
            (640, 480),
        );

        assert_eq!(video_mode_step.target_guest, Some((639.0, 479.0)));
        let diagnostics = sync.diagnostics();
        assert_eq!(
            diagnostics.range_source_x,
            RangeSource::AssumedVideoModeDefault
        );
        assert_eq!(
            diagnostics.range_source_y,
            RangeSource::AssumedVideoModeDefault
        );

        sync.observe_horizontal_range(0, 319);
        sync.observe_vertical_range(0, 239);
        let observed_step = sync.tick(
            Some(SeamlessMouseTarget {
                x: 639,
                y: 479,
                seq: 2,
            }),
            (640, 480),
        );

        assert_eq!(observed_step.target_guest, Some((319.0, 239.0)));
        let diagnostics = sync.diagnostics();
        assert_eq!(diagnostics.range_source_x, RangeSource::ObservedInt33);
        assert_eq!(diagnostics.range_source_y, RangeSource::ObservedInt33);
    }

    #[test]
    fn test_real_boot_mouse_sync_uses_mode13_default_range() {
        let mut sync = RealBootMouseSync::new();

        sync.assume_video_mode_default_range(0x13, (320, 200));
        let step = sync.tick(
            Some(SeamlessMouseTarget {
                x: 319,
                y: 199,
                seq: 1,
            }),
            (320, 200),
        );

        assert_eq!(step.target_guest, Some((639.0, 199.0)));
        assert_eq!(
            sync.diagnostics().range_source_x,
            RangeSource::AssumedVideoModeDefault
        );
    }

    #[test]
    fn test_real_boot_mouse_sync_reapplying_same_assumed_range_preserves_accounting() {
        let mut sync = RealBootMouseSync::new();
        sync.assume_video_mode_default_range(0x13, (320, 200));
        sync.observe_position(0, 0);

        let step = sync.tick(Some(SeamlessMouseTarget { x: 1, y: 0, seq: 1 }), (320, 200));
        assert_eq!(step.emitted(), (2, 0));
        let before = sync.diagnostics();
        assert_eq!(before.range_source_x, RangeSource::AssumedVideoModeDefault);
        assert_eq!(before.emitted_since_observation_x, 2);
        assert!(
            before.residual_x > 0.0,
            "mode 13h default X range over 320 pixels should leave sub-mickey residual"
        );

        sync.assume_video_mode_default_range(0x13, (320, 200));

        let after = sync.diagnostics();
        assert_eq!(after.range_source_x, RangeSource::AssumedVideoModeDefault);
        assert_eq!(
            after.emitted_since_observation_x,
            before.emitted_since_observation_x
        );
        assert_eq!(after.residual_x, before.residual_x);
    }

    #[test]
    fn test_real_boot_mouse_sync_learns_gain_from_observed_delta() {
        let mut sync = RealBootMouseSync::new();
        sync.observe_horizontal_range(0, 639);
        sync.observe_vertical_range(0, 199);
        let target = SeamlessMouseTarget {
            x: 42,
            y: 10,
            seq: 1,
        };
        assert_eq!(sync.tick(Some(target), (640, 200)).emitted(), (0, 0));
        sync.observe_position(10, 10);

        let step = sync.tick(Some(target), (640, 200));
        assert_eq!(step.emitted(), (32, 0));
        assert_eq!(sync.diagnostics().emitted_since_observation_x, 32);

        sync.observe_position(74, 10);

        let diagnostics = sync.diagnostics();
        assert!((diagnostics.gain_x - 1.2).abs() < 1.0e-12);
        assert_eq!(diagnostics.gain_y, DEFAULT_MOUSE_SYNC_GAIN);
        assert!((diagnostics.confidence - 0.50).abs() < f64::EPSILON);
        assert_eq!(diagnostics.last_observed_x, Some(74.0));
        assert_eq!(diagnostics.emitted_since_observation_x, 0);
        assert_eq!(sync.estimate_x, 74.0);
    }

    #[test]
    fn test_real_boot_mouse_sync_invalidation_makes_next_observation_uncontrolled() {
        let mut sync = RealBootMouseSync::new();
        sync.observe_horizontal_range(0, 639);
        sync.observe_vertical_range(0, 199);
        sync.observe_position(10, 10);

        let target = SeamlessMouseTarget {
            x: 42,
            y: 10,
            seq: 1,
        };
        assert_eq!(sync.tick(Some(target), (640, 200)).emitted(), (32, 0));
        assert_eq!(sync.diagnostics().emitted_since_observation_x, 32);

        sync.invalidate_gain_attribution();
        assert!(!sync.diagnostics().gain_attribution_valid);
        sync.observe_position(74, 10);

        let diagnostics = sync.diagnostics();
        assert_eq!(diagnostics.gain_x, DEFAULT_MOUSE_SYNC_GAIN);
        assert_eq!(diagnostics.gain_y, DEFAULT_MOUSE_SYNC_GAIN);
        assert_eq!(
            diagnostics.last_gain_update_x,
            GainUpdateDecision::CalibrationDisabled
        );
        assert_eq!(diagnostics.emitted_since_observation_x, 0);
        assert_eq!(diagnostics.last_observed_x, Some(74.0));
        assert!(diagnostics.gain_attribution_valid);
    }

    #[test]
    fn test_real_boot_mouse_sync_rejects_gain_across_target_sequence_churn() {
        let mut sync = RealBootMouseSync::new();
        sync.observe_horizontal_range(0, 639);
        sync.observe_vertical_range(0, 199);
        sync.observe_position(10, 10);

        assert_eq!(
            sync.tick(
                Some(SeamlessMouseTarget {
                    x: 42,
                    y: 10,
                    seq: 1,
                }),
                (640, 200),
            )
            .emitted(),
            (32, 0)
        );
        assert_eq!(
            sync.tick(
                Some(SeamlessMouseTarget {
                    x: 74,
                    y: 10,
                    seq: 2,
                }),
                (640, 200),
            )
            .emitted(),
            (32, 0)
        );
        assert_eq!(
            sync.tick(
                Some(SeamlessMouseTarget {
                    x: 106,
                    y: 10,
                    seq: 3,
                }),
                (640, 200),
            )
            .emitted(),
            (32, 0)
        );
        assert_eq!(sync.diagnostics().emitted_since_observation_x, 32);

        sync.observe_position(154, 10);

        let diagnostics = sync.diagnostics();
        assert_eq!(diagnostics.gain_x, DEFAULT_MOUSE_SYNC_GAIN);
        assert_eq!(
            diagnostics.last_gain_update_x,
            GainUpdateDecision::CalibrationDisabled
        );
        assert_eq!(diagnostics.last_observed_x, Some(154.0));
        assert_eq!(diagnostics.emitted_since_observation_x, 0);
    }

    #[test]
    fn test_real_boot_mouse_sync_rejects_bad_gain_samples() {
        let mut opposite = RealBootMouseSync::new();
        opposite.observe_horizontal_range(0, 1000);
        opposite.observe_vertical_range(0, 1000);
        opposite.observe_position(100, 100);
        opposite.tick(
            Some(SeamlessMouseTarget {
                x: 132,
                y: 100,
                seq: 1,
            }),
            (1001, 1001),
        );
        opposite.observe_position(68, 100);
        assert_eq!(opposite.diagnostics().gain_x, DEFAULT_MOUSE_SYNC_GAIN);

        let mut boundary = RealBootMouseSync::new();
        boundary.observe_horizontal_range(0, 100);
        boundary.observe_vertical_range(0, 100);
        boundary.observe_position(50, 50);
        boundary.tick(
            Some(SeamlessMouseTarget {
                x: 82,
                y: 50,
                seq: 1,
            }),
            (101, 101),
        );
        boundary.observe_position(100, 50);
        assert_eq!(boundary.diagnostics().gain_x, DEFAULT_MOUSE_SYNC_GAIN);

        let mut out_of_range = RealBootMouseSync::new();
        out_of_range.observe_horizontal_range(0, 1000);
        out_of_range.observe_vertical_range(0, 1000);
        out_of_range.observe_position(100, 100);
        out_of_range.tick(
            Some(SeamlessMouseTarget {
                x: 104,
                y: 100,
                seq: 1,
            }),
            (1001, 1001),
        );
        out_of_range.observe_position(200, 100);
        assert_eq!(out_of_range.diagnostics().gain_x, DEFAULT_MOUSE_SYNC_GAIN);
    }
}
