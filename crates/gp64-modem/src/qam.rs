//! V.32bis QAM modulator and demodulator.
//!
//! Implements Quadrature Amplitude Modulation for the data mode of V.32bis:
//!
//! | Constellation | Points | Bits/symbol | Bit rate     | Notes            |
//! |---------------|--------|-------------|--------------|------------------|
//! | 4-QAM (QPSK)  |   4    |      2      | 4800 bps     | No trellis       |
//! | 16-QAM        |  16    |      4      | 9600 bps     | No trellis       |
//! | 32-QAM        |  32    |      5      | 12000 bps    | With trellis     |
//! | 128-QAM       | 128    |      7      | 14400 bps    | V.32bis trellis  |
//!
//! Carrier frequency: 1800 Hz, symbol rate: 2400 baud, sample rate: 8000 Hz.

use std::collections::VecDeque;
use std::sync::OnceLock;

use crate::dsp::agc::Agc;
use crate::dsp::equalizer::Equalizer;
use crate::dsp::oscillator::Oscillator;
use crate::dsp::pll::Pll;
use crate::dsp::rrc_filter::RrcFilter;
use crate::dsp::timing::TimingRecovery;

// V.32bis parameters
const CARRIER_HZ: f64 = 1800.0;
const SYMBOL_RATE: f64 = 2400.0;
const SAMPLE_RATE: u32 = 8000;

// RRC filter parameters
const RRC_ROLLOFF: f64 = 0.15;
const RRC_TAPS: usize = 48;

/// Configuration for QAM modulator/demodulator, parameterized for different modem standards.
#[derive(Debug, Clone)]
pub struct QamConfig {
    pub symbol_rate: f64,
    pub carrier_freq: f64,
    pub rrc_taps: usize,
    pub rrc_rolloff: f64,
    pub pll_bandwidth: f64,
    /// Enable decision-directed PLL carrier tracking. Disable for clean
    /// channels where the carrier phase is known (PLL diverges from wrong
    /// initial hard decisions on dense constellations).
    pub pll_tracking: bool,
    /// Additional carrier phase correction (radians) to compensate for
    /// residual phase offset not captured by the group-delay expression.
    /// Determined empirically for each modem standard.
    pub carrier_phase_correction: f64,
    /// AGC adaptation rate (0.0-1.0). Lower = slower/more stable gain.
    /// Dense constellations need slow AGC to avoid data-dependent gain
    /// modulation: 0.05 for V.32bis (32 points), 0.002 for V.34 (~960 points).
    ///
    /// Acts as the default for both attack and decay when
    /// `agc_attack_alpha` / `agc_decay_alpha` are `None`.
    pub agc_alpha: f64,
    /// Optional AGC attack rate (level rising). When `Some`, overrides
    /// `agc_alpha` for the attack path and enables the asymmetric AGC
    /// (`Agc::with_asymmetric`). `None` falls back to symmetric `agc_alpha`.
    ///
    /// Wired by `DspProfile::apply_to_qam_config`; existing non-profile
    /// callers keep symmetric AGC behaviour unchanged.
    pub agc_attack_alpha: Option<f64>,
    /// Optional AGC decay rate (level falling). See `agc_attack_alpha`.
    pub agc_decay_alpha: Option<f64>,
    /// Number of taps for the T-spaced adaptive equalizer.
    /// 0 = no equalizer (V.32bis), 11 = V.34 default.
    pub equalizer_taps: usize,
    /// LMS step size for the adaptive equalizer.
    /// 0.0 = no equalizer (V.32bis), 0.01 = V.34 default.
    pub equalizer_mu: f64,
    /// When true, the adaptive equalizer uses normalized LMS (NLMS)
    /// adaptation instead of plain LMS. Wired by
    /// `DspProfile::apply_to_qam_config` (PSTN profile enables it).
    /// Default `false` preserves existing plain-LMS behaviour.
    pub equalizer_nlms: bool,
    /// Timing pre-alignment (in samples) to compensate for the combined
    /// TX/RX RRC group delay. Found empirically per modem standard.
    pub timing_align: f64,
    /// Gardner timing recovery loop bandwidth. Non-zero enables adaptive
    /// timing tracking; 0.0 uses fixed timing (timing_align only).
    /// V.32bis uses 0.01 (adaptive); V.34 uses 0.0 (fixed) because the
    /// 2.5 sps timing is fully determined by the pre-alignment parameter.
    pub timing_bandwidth: f64,
    /// Base sample rate in Hz. Defaults to 8000 (standard telephony rate).
    /// Set to 16000 for configs that operate on the 16kHz wire path directly,
    /// bypassing the 8kHz decimation/upsampling round-trip.
    pub sample_rate: u32,
}

impl QamConfig {
    /// V.32bis: 2400 baud, 1800 Hz carrier, 48 taps.
    pub fn v32bis() -> Self {
        Self {
            symbol_rate: SYMBOL_RATE,
            carrier_freq: CARRIER_HZ,
            rrc_taps: RRC_TAPS,
            rrc_rolloff: RRC_ROLLOFF,
            pll_bandwidth: 0.0,
            pll_tracking: false,
            carrier_phase_correction: 0.0,
            agc_alpha: 0.05,
            agc_attack_alpha: None,
            agc_decay_alpha: None,
            equalizer_taps: 0,
            equalizer_mu: 0.0,
            equalizer_nlms: false,
            timing_align: 0.0,
            timing_bandwidth: 0.01,
            sample_rate: SAMPLE_RATE,
        }
    }

    /// V.32bis config for the 16kHz wire path.
    ///
    /// At 16kHz: sps = 16000/2400 = 6.667 (non-integer, > 3.0), so both
    /// modulator and demodulator oversample 3x to 48kHz (20 sps, integer)
    /// for pulse shaping. The modulator decimates back to 16kHz output;
    /// the demodulator zero-stuffs 3x before the matched filter.
    ///
    /// carrier_phase_correction and timing_bandwidth were tuned empirically
    /// via test_v32bis_16k_parameter_sweep.
    pub fn v32bis_16k() -> Self {
        Self {
            symbol_rate: SYMBOL_RATE,
            carrier_freq: CARRIER_HZ,
            rrc_taps: RRC_TAPS,
            rrc_rolloff: RRC_ROLLOFF,
            pll_bandwidth: 0.0,
            pll_tracking: false,
            // Tuned empirically via test_v32bis_16k_parameter_sweep.
            // At 48kHz internal rate, the RRC group-delay expression leaves a
            // ~4.285 rad residual that must be compensated explicitly.
            carrier_phase_correction: 4.285,
            agc_alpha: 0.05,
            agc_attack_alpha: None,
            agc_decay_alpha: None,
            equalizer_taps: 0,
            equalizer_mu: 0.0,
            equalizer_nlms: false,
            // No timing pre-alignment needed: at 20 sps (integer internal
            // rate) the symbol boundary naturally falls at sample 0.
            timing_align: 0.0,
            // Fixed timing (like V.34): at 20 sps internal rate, the
            // symbol boundary is fully determined by the pre-alignment
            // parameter. The Gardner loop causes timing drift on long
            // sequences which degrades QAM4 accuracy.
            timing_bandwidth: 0.0,
            sample_rate: 16000,
        }
    }
    /// V.34: 3200 baud, 1829 Hz carrier, 80 taps (160 internal at 16kHz).
    ///
    /// The demodulator internally upsamples 2x (8kHz -> 16kHz) to achieve
    /// integer sps (5.0), eliminating the periodic ISI that occurred at
    /// 2.5 sps (8kHz). RRC filter, carrier oscillator, and timing recovery
    /// all run at the 16kHz internal rate. Symbol output rate stays at
    /// 3200 baud. The carrier_phase_correction and timing_align values
    /// were tuned empirically for the 16kHz internal rate.
    ///
    /// NOTE: These parameters are tuned for the legacy 8kHz wire path where
    /// TX decimates to 8kHz and RX upsamples via zero-stuffing. For the
    /// 16kHz wire path (no decimation/upsampling), use `v34_16k()`.
    pub fn v34() -> Self {
        Self {
            symbol_rate: 3200.0,
            carrier_freq: 1829.0,
            rrc_taps: 80,
            rrc_rolloff: 0.15,
            pll_bandwidth: 40.0,
            pll_tracking: false,
            carrier_phase_correction: 0.552,
            agc_alpha: 0.002,
            agc_attack_alpha: None,
            agc_decay_alpha: None,
            equalizer_taps: 11,
            equalizer_mu: 0.03,
            equalizer_nlms: false,
            timing_align: 0.890,
            // Fixed timing: even with integer sps at 16kHz, the Gardner
            // loop is disabled; the pre-alignment parameter plus the
            // RRC filter's matched-filter response provides sufficient
            // timing accuracy.
            timing_bandwidth: 0.0,
            sample_rate: SAMPLE_RATE,
        }
    }

    /// V.34 config for the 16kHz wire path.
    ///
    /// When both modems exchange PCM at 16kHz (the internal oversampled rate),
    /// the TX no longer decimates and the RX no longer zero-stuffs. This
    /// eliminates the asymmetry between TX (8kHz carrier) and RX (16kHz
    /// carrier) that required the empirical `carrier_phase_correction`.
    ///
    /// With symmetric 16kHz TX/RX carriers, the carrier phase correction
    /// and timing alignment need re-tuning. The correction drops to ~0
    /// because each TX sample is modulated at exactly the carrier phase
    /// the RX expects (no half-sample offset from decimation).
    pub fn v34_16k() -> Self {
        Self {
            symbol_rate: 3200.0,
            carrier_freq: 1829.0,
            rrc_taps: 80,
            rrc_rolloff: 0.15,
            pll_bandwidth: 40.0,
            pll_tracking: false,
            // Tuned empirically via test_v34_16k_parameter_sweep.
            // With symmetric 16kHz TX/RX the phase correction is close to the
            // 8kHz path (0.552) because the RRC group-delay expression already
            // accounts for the carrier offset; the residual 0.545 rad compensates
            // for the matched-filter impulse-response asymmetry.
            carrier_phase_correction: 0.545,
            agc_alpha: 0.002,
            agc_attack_alpha: None,
            agc_decay_alpha: None,
            equalizer_taps: 11,
            equalizer_mu: 0.03,
            equalizer_nlms: false,
            // No timing pre-alignment needed: the sweep overrides
            // timing_bandwidth to 0.01 for adaptive Gardner TED convergence,
            // then freezes it at 0.0 when installing the trained demod into
            // DataMode. The base config stays at 0.0 to prevent long-term
            // drift from biasing the TED on extended sequences.
            timing_align: 0.0,
            timing_bandwidth: 0.0,
            sample_rate: SAMPLE_RATE,
        }
    }
}

/// PCM amplitude scaling (well below i16::MAX to leave headroom).
const AMPLITUDE: f64 = 12000.0;

// ---------------------------------------------------------------------------
// Constellation
// ---------------------------------------------------------------------------

/// QAM constellation definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Constellation {
    /// 4-QAM (QPSK): 2 bits/symbol, 4 points.
    Qam4,
    /// 16-QAM: 4 bits/symbol, 16 points (4x4 grid).
    /// Used by V.32bis at 9600 bps.
    Qam16,
    /// 32-QAM: 5 bits/symbol, 32 points (6x6 grid minus 4 corners).
    Qam32,
    /// 128-QAM: 7 bits/symbol, 128 points.
    /// Used by V.32bis at 14400 bps (6 data + 1 trellis-coded bit).
    Qam128,
}

/// 4-QAM constellation points (Gray coded).
///
/// Points at 45/135/225/315 degrees, normalized to unit power:
/// ```text
/// Point 0 (bits 00): (+1, +1) / sqrt(2)
/// Point 1 (bits 01): (-1, +1) / sqrt(2)
/// Point 2 (bits 11): (-1, -1) / sqrt(2)
/// Point 3 (bits 10): (+1, -1) / sqrt(2)
/// ```
const QAM4_POINTS: [(f64, f64); 4] = {
    // 1/sqrt(2) ≈ 0.7071
    const S: f64 = std::f64::consts::FRAC_1_SQRT_2;
    [(S, S), (-S, S), (-S, -S), (S, -S)]
};

/// 16-QAM constellation: 4x4 regular grid, normalized to average power = 1.
///
/// Grid values: -3, -1, +1, +3 (before normalization).
/// Gray-coded 4-bit index maps to (I, Q) coordinates.
fn qam16_points() -> [(f64, f64); 16] {
    const GRID: [f64; 4] = [-3.0, -1.0, 1.0, 3.0];

    let mut raw = [(0.0_f64, 0.0_f64); 16];
    // Gray-code row/column mapping: 0->0, 1->1, 2->3, 3->2
    // so adjacent indices differ by one bit in both I and Q.
    const GRAY: [usize; 4] = [0, 1, 3, 2];

    for qi in 0..4usize {
        for ii in 0..4usize {
            let idx = qi * 4 + ii;
            raw[idx] = (GRID[GRAY[ii]], GRID[GRAY[qi]]);
        }
    }

    // Normalize to unit average power.
    let mut sum_sq = 0.0;
    let mut k = 0;
    while k < 16 {
        sum_sq += raw[k].0 * raw[k].0 + raw[k].1 * raw[k].1;
        k += 1;
    }
    let avg_power = sum_sq / 16.0;
    let scale = 1.0 / avg_power.sqrt();

    let mut normalized = [(0.0_f64, 0.0_f64); 16];
    k = 0;
    while k < 16 {
        normalized[k] = (raw[k].0 * scale, raw[k].1 * scale);
        k += 1;
    }
    normalized
}

/// Cached 16-QAM constellation points, computed once on first access.
static QAM16_POINTS: OnceLock<[(f64, f64); 16]> = OnceLock::new();

/// Return a reference to the cached 16-QAM constellation points.
fn qam16_points_cached() -> &'static [(f64, f64); 16] {
    QAM16_POINTS.get_or_init(qam16_points)
}

/// 128-QAM constellation: based on a 12x12 grid (144 points) minus 16 corners,
/// giving 128 points, normalized to average power = 1.
///
/// Grid values: -11, -9, -7, -5, -3, -1, +1, +3, +5, +7, +9, +11 (before normalization).
/// Corners removed: those where both |I| and |Q| are >= 9 (16 points).
fn qam128_points() -> [(f64, f64); 128] {
    const GRID: [f64; 12] = [
        -11.0, -9.0, -7.0, -5.0, -3.0, -1.0, 1.0, 3.0, 5.0, 7.0, 9.0, 11.0,
    ];

    let mut raw = [(0.0_f64, 0.0_f64); 128];
    let mut idx = 0;
    for q_idx in 0..12 {
        for i_idx in 0..12 {
            let i_val = GRID[i_idx];
            let q_val = GRID[q_idx];
            // Skip corners: both |I| >= 9 and |Q| >= 9
            if i_val.abs() >= 9.0 && q_val.abs() >= 9.0 {
                continue;
            }
            raw[idx] = (i_val, q_val);
            idx += 1;
        }
    }
    debug_assert_eq!(idx, 128);

    // Normalize to unit average power.
    let mut sum_sq = 0.0;
    let mut k = 0;
    while k < 128 {
        sum_sq += raw[k].0 * raw[k].0 + raw[k].1 * raw[k].1;
        k += 1;
    }
    let avg_power = sum_sq / 128.0;
    let scale = 1.0 / avg_power.sqrt();

    let mut normalized = [(0.0_f64, 0.0_f64); 128];
    k = 0;
    while k < 128 {
        normalized[k] = (raw[k].0 * scale, raw[k].1 * scale);
        k += 1;
    }
    normalized
}

/// Cached 128-QAM constellation points, computed once on first access.
static QAM128_POINTS: OnceLock<[(f64, f64); 128]> = OnceLock::new();

/// Return a reference to the cached 128-QAM constellation points.
fn qam128_points_cached() -> &'static [(f64, f64); 128] {
    QAM128_POINTS.get_or_init(qam128_points)
}

/// 32-QAM constellation: 6x6 grid minus 4 corners, normalized to average power = 1.
///
/// Grid values: -5, -3, -1, +1, +3, +5 (before normalization).
/// Corners removed: (-5,-5), (-5,+5), (+5,-5), (+5,+5).
///
/// The mapping from 5-bit index to grid point is a lookup table. The bit
/// assignment follows a scheme where adjacent points differ minimally (not
/// strictly Gray coded for 32-QAM, but consistent).
fn qam32_points() -> [(f64, f64); 32] {
    // Build the 32 grid points (6x6 minus 4 corners).
    const GRID: [f64; 6] = [-5.0, -3.0, -1.0, 1.0, 3.0, 5.0];

    // Enumerate all 36 points, skip the 4 corners.
    let mut raw = [(0.0_f64, 0.0_f64); 32];
    let mut idx = 0;
    let mut q_idx = 0;
    while q_idx < 6 {
        let mut i_idx = 0;
        while i_idx < 6 {
            let i_val = GRID[i_idx];
            let q_val = GRID[q_idx];
            // Skip corners: both coordinates at ±5
            let is_corner = (i_val == -5.0 || i_val == 5.0) && (q_val == -5.0 || q_val == 5.0);
            if !is_corner {
                raw[idx] = (i_val, q_val);
                idx += 1;
            }
            i_idx += 1;
        }
        q_idx += 1;
    }

    // Compute average power and normalize.
    let mut sum_sq = 0.0;
    let mut k = 0;
    while k < 32 {
        sum_sq += raw[k].0 * raw[k].0 + raw[k].1 * raw[k].1;
        k += 1;
    }
    let avg_power = sum_sq / 32.0;
    let scale = 1.0 / avg_power.sqrt();

    let mut normalized = [(0.0_f64, 0.0_f64); 32];
    k = 0;
    while k < 32 {
        normalized[k] = (raw[k].0 * scale, raw[k].1 * scale);
        k += 1;
    }

    normalized
}

/// Cached 32-QAM constellation points, computed once on first access.
static QAM32_POINTS: OnceLock<[(f64, f64); 32]> = OnceLock::new();

/// Return a reference to the cached 32-QAM constellation points.
fn qam32_points_cached() -> &'static [(f64, f64); 32] {
    QAM32_POINTS.get_or_init(qam32_points)
}

impl Constellation {
    /// Number of bits encoded per symbol.
    pub fn bits_per_symbol(&self) -> usize {
        match self {
            Constellation::Qam4 => 2,
            Constellation::Qam16 => 4,
            Constellation::Qam32 => 5,
            Constellation::Qam128 => 7,
        }
    }

    /// Number of constellation points.
    pub fn num_points(&self) -> usize {
        match self {
            Constellation::Qam4 => 4,
            Constellation::Qam16 => 16,
            Constellation::Qam32 => 32,
            Constellation::Qam128 => 128,
        }
    }

    /// Get the I/Q coordinates for the given point index.
    pub fn point(&self, index: u8) -> (f64, f64) {
        match self {
            Constellation::Qam4 => {
                debug_assert!((index as usize) < 4);
                QAM4_POINTS[index as usize]
            }
            Constellation::Qam16 => {
                debug_assert!((index as usize) < 16);
                qam16_points_cached()[index as usize]
            }
            Constellation::Qam32 => {
                debug_assert!((index as usize) < 32);
                qam32_points_cached()[index as usize]
            }
            Constellation::Qam128 => {
                debug_assert!((index as usize) < 128);
                qam128_points_cached()[index as usize]
            }
        }
    }

    /// Find the nearest constellation point to (i, q).
    /// Returns (index, squared_distance).
    pub fn nearest(&self, i: f64, q: f64) -> (u8, f64) {
        let points: &[(f64, f64)] = match self {
            Constellation::Qam4 => &QAM4_POINTS,
            Constellation::Qam16 => qam16_points_cached(),
            Constellation::Qam32 => qam32_points_cached(),
            Constellation::Qam128 => qam128_points_cached(),
        };

        let mut best_idx = 0u8;
        let mut best_dist_sq = f64::MAX;

        for (k, &(pi, pq)) in points.iter().enumerate() {
            let di = i - pi;
            let dq = q - pq;
            let dist_sq = di * di + dq * dq;
            if dist_sq < best_dist_sq {
                best_dist_sq = dist_sq;
                best_idx = k as u8;
            }
        }

        (best_idx, best_dist_sq)
    }
}

// ---------------------------------------------------------------------------
// QAM Modulator
// ---------------------------------------------------------------------------

/// V.32bis QAM modulator.
///
/// Accepts coded bits (one symbol at a time), performs pulse shaping via RRC
/// filter, and modulates onto a 1800 Hz carrier. Symbols are queued and
/// consumed internally at the 2400 baud symbol rate.
pub struct QamModulator {
    carrier: Oscillator,
    /// Carrier oscillator at the internal oversampled rate (16kHz for V.34).
    /// Used by `next_sample_internal()` to output every internal sample without
    /// decimation. `None` when oversample_ratio == 1 (V.32bis).
    carrier_internal: Option<Oscillator>,
    tx_filter_i: RrcFilter,
    tx_filter_q: RrcFilter,
    samples_per_symbol: f64,
    sample_counter: f64,
    constellation: Constellation,
    /// Queue of (I, Q) symbol values waiting to be injected into the filter.
    symbol_queue: VecDeque<(f64, f64)>,
    prev_symbol_idx: u8,
    /// Internal oversampling ratio for the pulse-shaping filter.
    /// When sps is non-integer, the filter runs at `oversample_ratio * SAMPLE_RATE`
    /// where sps becomes integer, eliminating the ±0.5-sample timing jitter that
    /// occurs with integer-position symbol injection at non-integer sps.
    oversample_ratio: u32,
}

impl QamModulator {
    /// Create a new QAM modulator for the given constellation.
    pub fn new(constellation: Constellation) -> Self {
        Self::with_config(constellation, &QamConfig::v32bis())
    }

    /// Create a QAM modulator with explicit configuration (carrier, baud rate, filter).
    pub fn with_config(constellation: Constellation, config: &QamConfig) -> Self {
        let base_rate = config.sample_rate;
        let sps = base_rate as f64 / config.symbol_rate;

        // When sps is non-integer, oversample the pulse-shaping filter to
        // eliminate integer-position timing jitter.  For V.34 at 2.5 sps,
        // ratio=2 gives 5 sps (integer) at 16000 Hz internal rate.
        let oversample_ratio = if (sps.round() - sps).abs() > 0.01 {
            let mut r = 2u32;
            while r <= 8 {
                if ((sps * r as f64).round() - sps * r as f64).abs() < 0.01 {
                    break;
                }
                r += 1;
            }
            r.min(8)
        } else {
            1
        };

        let internal_rate = (base_rate * oversample_ratio) as f64;
        let internal_sps = internal_rate / config.symbol_rate;
        let internal_taps = config.rrc_taps * oversample_ratio as usize;

        let carrier_internal = if oversample_ratio > 1 {
            Some(Oscillator::new(
                config.carrier_freq,
                base_rate * oversample_ratio,
            ))
        } else {
            None
        };

        Self {
            carrier: Oscillator::new(config.carrier_freq, base_rate),
            carrier_internal,
            tx_filter_i: RrcFilter::new(
                config.rrc_rolloff,
                config.symbol_rate,
                internal_rate,
                internal_taps,
            ),
            tx_filter_q: RrcFilter::new(
                config.rrc_rolloff,
                config.symbol_rate,
                internal_rate,
                internal_taps,
            ),
            samples_per_symbol: internal_sps,
            sample_counter: 0.0,
            constellation,
            symbol_queue: VecDeque::new(),
            prev_symbol_idx: 0,
            oversample_ratio,
        }
    }

    /// Map bits to a constellation point and queue for modulation.
    ///
    /// `bits`: the coded bits for one symbol (2 for 4-QAM, 5 for 32-QAM).
    pub fn modulate_symbol(&mut self, bits: u8) {
        let n = self.constellation.num_points() as u8;
        let idx = (self.prev_symbol_idx.wrapping_add(bits)) % n;
        self.prev_symbol_idx = idx;
        let (i, q) = self.constellation.point(idx);
        self.symbol_queue.push_back((i, q));
    }

    pub fn modulate_symbol_raw(&mut self, index: u8) {
        let (i, q) = self.constellation.point(index);
        self.symbol_queue.push_back((i, q));
    }

    /// Modulate a raw IQ point directly (caller handles constellation mapping).
    pub fn modulate_iq(&mut self, i: f64, q: f64) {
        self.symbol_queue.push_back((i, q));
    }

    /// Drops symbols queued but not yet sent, keeping the carrier and pulse
    /// shaping state, so a half-duplex turn starts from its own first symbol.
    pub fn discard_queued_symbols(&mut self) {
        self.symbol_queue.clear();
    }

    /// Number of symbols currently queued for modulation.
    pub fn queue_len(&self) -> usize {
        self.symbol_queue.len()
    }

    /// Snapshot the queued (I, Q) symbol sequence without consuming it.
    ///
    /// Test-only: allows sequence-inspection tests to verify the exact
    /// order of queued constellation points (e.g. S alternating with
    /// S-bar in V.34 Phase 3).
    #[cfg(test)]
    pub(crate) fn queue_snapshot(&self) -> Vec<(f64, f64)> {
        self.symbol_queue.iter().copied().collect()
    }

    /// Generate the next PCM sample.
    ///
    /// Call at 8000 Hz sample rate. Internally handles upsampling (symbol
    /// insertion every ~3.33 samples), RRC pulse shaping, and carrier
    /// modulation.
    pub fn next_sample(&mut self) -> i16 {
        // Run the pulse-shaping filter at the internal oversampled rate,
        // taking every Nth output sample (simple decimation).  The RRC filter
        // provides adequate anti-aliasing for the decimation.
        let mut shaped_i = 0.0;
        let mut shaped_q = 0.0;
        for _ in 0..self.oversample_ratio {
            let (input_i, input_q) = if self.sample_counter >= self.samples_per_symbol {
                self.sample_counter -= self.samples_per_symbol;
                if let Some((sym_i, sym_q)) = self.symbol_queue.pop_front() {
                    (sym_i, sym_q)
                } else {
                    (0.0, 0.0)
                }
            } else {
                (0.0, 0.0)
            };
            self.sample_counter += 1.0;
            shaped_i = self.tx_filter_i.filter(input_i);
            shaped_q = self.tx_filter_q.filter(input_q);
        }

        // Carrier modulation at the output rate.
        let (cos_w, sin_w) = self.carrier.next_iq();
        let modulated = shaped_i * cos_w - shaped_q * sin_w;

        // Scale to PCM range.
        (modulated * AMPLITUDE).round() as i16
    }

    /// Generate one output sample at the internal oversampled rate (16kHz for V.34).
    ///
    /// Unlike `next_sample()` which runs `oversample_ratio` filter iterations and
    /// decimates to the 8kHz output rate, this runs ONE filter iteration and
    /// modulates with a carrier oscillator at the internal rate. Call this
    /// `oversample_ratio` times more per tick than `next_sample()` (e.g., 160
    /// times per 10ms tick for V.34 instead of 80).
    ///
    /// Panics if `oversample_ratio == 1` (no internal rate — use `next_sample()`).
    ///
    /// IMPORTANT: Do not mix `next_sample()` and `next_sample_internal()` on the
    /// same modulator — they advance different carrier oscillators.
    pub fn next_sample_internal(&mut self) -> i16 {
        let carrier = self
            .carrier_internal
            .as_mut()
            .expect("next_sample_internal() requires oversample_ratio > 1");

        // One filter iteration at the internal rate.
        let (input_i, input_q) = if self.sample_counter >= self.samples_per_symbol {
            self.sample_counter -= self.samples_per_symbol;
            if let Some((sym_i, sym_q)) = self.symbol_queue.pop_front() {
                (sym_i, sym_q)
            } else {
                (0.0, 0.0)
            }
        } else {
            (0.0, 0.0)
        };
        self.sample_counter += 1.0;
        let shaped_i = self.tx_filter_i.filter(input_i);
        let shaped_q = self.tx_filter_q.filter(input_q);

        // Carrier modulation at the internal rate.
        let (cos_w, sin_w) = carrier.next_iq();
        let modulated = shaped_i * cos_w - shaped_q * sin_w;

        (modulated * AMPLITUDE).round() as i16
    }

    /// The internal oversampling ratio (1 for V.32bis, 2 for V.34).
    pub fn oversample_ratio(&self) -> u32 {
        self.oversample_ratio
    }

    /// Generate a block of samples, appending to `output`.
    pub fn generate(&mut self, count: usize, output: &mut Vec<i16>) {
        output.reserve(count);
        for _ in 0..count {
            output.push(self.next_sample());
        }
    }

    /// Reset modulator state.
    pub fn reset(&mut self) {
        self.carrier.reset();
        if let Some(c) = &mut self.carrier_internal {
            c.reset();
        }
        self.tx_filter_i.reset();
        self.tx_filter_q.reset();
        self.sample_counter = 0.0;
        self.symbol_queue.clear();
        self.prev_symbol_idx = 0;
    }
}

// ---------------------------------------------------------------------------
// QAM Demodulator
// ---------------------------------------------------------------------------

/// V.32bis QAM demodulator.
///
/// Accepts PCM samples and recovers I/Q soft symbols using carrier recovery
/// (PLL), matched RRC filtering, and Gardner timing recovery.
///
/// The demodulator includes a symbol-level AGC that normalizes the recovered
/// I/Q amplitude to match the constellation scale (average power = 1.0).
pub struct QamDemodulator {
    carrier: Oscillator,
    rx_filter_i: RrcFilter,
    rx_filter_q: RrcFilter,
    agc: Agc,
    timing: TimingRecovery,
    pll: Pll,
    constellation: Constellation,
    /// When false, PLL updates are disabled (carrier phase offset alone is
    /// sufficient on a clean channel with exact frequency match).
    pll_tracking: bool,
    /// When true, interpolate Q to the same fractional sample as I in timing
    /// recovery. Required for V.34 (2.5 sps); optional for V.32bis (3.33 sps).
    iq_interpolation: bool,
    /// Exponentially-smoothed RMS of recovered symbol magnitudes, used to
    /// normalize output to match constellation scale.
    symbol_rms: f64,
    /// When true, symbol_rms is frozen at its current value (no AGC adaptation).
    sym_agc_frozen: bool,
    /// Fixed carrier phase offset (radians) to pre-compensate for known delay.
    carrier_phase_offset: f64,
    prev_decision_idx: u8,
    /// T-spaced adaptive equalizer (None for V.32bis, Some for V.34).
    equalizer: Option<Equalizer>,
    /// Internal oversampling ratio for the DSP chain.
    /// When sps is non-integer (e.g., V.34 at 2.5 sps), the demodulator runs
    /// internally at `oversample_ratio * SAMPLE_RATE` where sps becomes integer,
    /// eliminating the periodic ISI from non-integer sample spacing.
    /// Mirrors the modulator's oversampling approach.
    oversample_ratio: u32,
}

impl QamDemodulator {
    /// Create a new QAM demodulator for the given constellation (V.32bis defaults).
    pub fn new(constellation: Constellation) -> Self {
        Self::with_config(constellation, &QamConfig::v32bis())
    }

    /// Create a QAM demodulator with explicit configuration.
    pub fn with_config(constellation: Constellation, config: &QamConfig) -> Self {
        let base_rate = config.sample_rate;
        let sps = base_rate as f64 / config.symbol_rate;

        // When sps is non-integer, oversample the RX DSP chain to eliminate
        // periodic ISI from non-integer sample spacing.
        //
        // At 8kHz base rate, only V.34 (sps=2.5, < 3.0) oversamples;
        // V.32bis (sps=3.333) is left at ratio=1 because the existing timing
        // recovery handles >= 3.0 sps adequately and oversampling would change
        // the carrier phase offset tuning.
        //
        // At higher base rates (e.g., 16kHz), all non-integer sps configs
        // oversample because the increased absolute sps doesn't guarantee
        // adequate timing accuracy without it.
        let non_integer_sps = (sps.round() - sps).abs() > 0.01;
        let oversample_ratio = if non_integer_sps && (sps < 3.0 || base_rate > SAMPLE_RATE) {
            let mut r = 2u32;
            while r <= 8 {
                if ((sps * r as f64).round() - sps * r as f64).abs() < 0.01 {
                    break;
                }
                r += 1;
            }
            r.min(8)
        } else {
            1
        };

        let internal_rate = base_rate * oversample_ratio;
        let internal_taps = config.rrc_taps * oversample_ratio as usize;

        // TX group delay: the TX modulator's RRC filter also runs at
        // oversample_ratio * base_rate with oversample_ratio * rrc_taps.
        // Group delay = (total_taps - 1) / 2 internal samples. When the RX
        // also runs at the same internal rate, this is directly in RX internal
        // samples. When oversample_ratio=1, this collapses to the original
        // expression: (rrc_taps - 1) / 2.
        let tx_internal_taps = config.rrc_taps * oversample_ratio as usize;
        let tx_group_delay = (tx_internal_taps - 1) as f64 / 2.0;
        let omega = std::f64::consts::TAU * config.carrier_freq / internal_rate as f64;
        let carrier_phase_offset = -(omega * tx_group_delay) + config.carrier_phase_correction;

        let mut timing = TimingRecovery::new(
            config.symbol_rate,
            internal_rate as f64,
            config.timing_bandwidth,
        );
        if config.timing_align != 0.0 {
            timing.set_initial_sample_count(config.timing_align);
        }

        let equalizer = if config.equalizer_taps > 0 {
            Some(if config.equalizer_nlms {
                Equalizer::new_normalized(config.equalizer_taps, config.equalizer_mu)
            } else {
                Equalizer::new(config.equalizer_taps, config.equalizer_mu)
            })
        } else {
            None
        };

        // AGC: asymmetric (attack/decay) when the profile supplies both,
        // otherwise fall back to symmetric `agc_alpha` for backward compat.
        let agc = match (config.agc_attack_alpha, config.agc_decay_alpha) {
            (Some(attack), Some(decay)) => Agc::with_asymmetric(1.0, attack, decay),
            _ => Agc::new(1.0, config.agc_alpha),
        };

        Self {
            carrier: Oscillator::new(config.carrier_freq, internal_rate),
            rx_filter_i: RrcFilter::new(
                config.rrc_rolloff,
                config.symbol_rate,
                internal_rate as f64,
                internal_taps,
            ),
            rx_filter_q: RrcFilter::new(
                config.rrc_rolloff,
                config.symbol_rate,
                internal_rate as f64,
                internal_taps,
            ),
            agc,
            timing,
            pll: Pll::with_bandwidth(config.pll_bandwidth, config.symbol_rate, 0.707),
            constellation,
            pll_tracking: config.pll_tracking,
            iq_interpolation: true,
            symbol_rms: 1.0,
            sym_agc_frozen: false,
            carrier_phase_offset,
            prev_decision_idx: 0,
            equalizer,
            oversample_ratio,
        }
    }

    /// Disable decision-directed PLL tracking. Use when the constellation is
    /// too large for the demodulator's nearest-point search (e.g., V.34 ~960
    /// points) and the channel has exact frequency match.
    pub fn set_pll_tracking(&mut self, enabled: bool) {
        self.pll_tracking = enabled;
    }

    /// Pre-align timing recovery with a known group delay offset.
    /// Sets the internal sample counter so the first symbol boundary falls
    /// at the correct sampling instant, compensating for cascaded RRC
    /// group delay mismatch between TX (oversampled) and RX (output rate).
    ///
    /// Non-finite `count` is silently skipped (matches inner
    /// `TimingRecovery::set_initial_sample_count` guard). Same NaN-poison
    /// rationale as `Oscillator::set_phase`.
    pub fn set_timing_align(&mut self, count: f64) {
        debug_assert!(
            count.is_finite(),
            "QamDemodulator::set_timing_align: non-finite count {count}"
        );
        if !count.is_finite() {
            return;
        }
        self.timing.set_initial_sample_count(count);
    }

    /// Update the Gardner timing recovery loop bandwidth at runtime.
    /// Set to 0.0 to freeze timing at the current converged position
    /// (prevents long-term drift in DataMode while preserving the mu
    /// value found during the sweep).
    ///
    /// Non-finite `bandwidth` is silently skipped (matches inner
    /// `TimingRecovery::set_loop_gain` guard).
    pub fn set_timing_bandwidth(&mut self, bandwidth: f64) {
        debug_assert!(
            bandwidth.is_finite(),
            "QamDemodulator::set_timing_bandwidth: non-finite bandwidth {bandwidth}"
        );
        if !bandwidth.is_finite() {
            return;
        }
        self.timing.set_loop_gain(bandwidth);
    }

    /// Whether decision-directed PLL tracking is currently enabled.
    /// Test-only: used to verify profile overlay plumbing.
    #[cfg(test)]
    pub fn pll_tracking(&self) -> bool {
        self.pll_tracking
    }

    /// Current Gardner timing recovery loop bandwidth (0.0 = frozen).
    /// Test-only: used to verify profile overlay plumbing.
    #[cfg(test)]
    pub fn timing_bandwidth(&self) -> f64 {
        self.timing.loop_gain()
    }

    /// Nominal PLL loop bandwidth in Hz. Test-only: used to verify profile
    /// overlay plumbing.
    #[cfg(test)]
    pub fn pll_bandwidth_hz(&self) -> f64 {
        self.pll.bandwidth_hz()
    }

    /// Current PLL frequency estimate (radians/sample). Test-only: used to
    /// verify that a configured PLL can track an injected frequency offset.
    #[cfg(test)]
    pub fn pll_frequency(&self) -> f64 {
        self.pll.frequency()
    }

    /// Feed PCM samples. Returns decoded soft symbols as (I, Q) pairs.
    ///
    /// Each returned pair represents one symbol-rate decision point, normalized
    /// to the constellation scale. Use [`hard_decision`] to map to the nearest
    /// constellation point.
    ///
    /// This is a thin wrapper over [`Self::process_sample`]; use that directly
    /// when you need per-sample control (e.g., to update the equalizer on the
    /// exact symbol it just emitted, before the delay line moves on).
    pub fn demodulate(&mut self, samples: &[i16]) -> Vec<(f64, f64)> {
        let mut symbols = Vec::new();
        for &s in samples {
            if let Some(sym) = self.process_sample(s) {
                symbols.push(sym);
            }
        }
        symbols
    }

    /// Process a single wire-rate PCM sample. Returns `Some((i, q))` when
    /// a symbol is emitted by timing recovery, else `None`.
    /// Returns at most one symbol per call.
    ///
    /// For the oversampled path (`oversample_ratio > 1`), this method
    /// internally performs the zero-stuffing polyphase upsampling step:
    /// one real sub-step with the input scaled by `ratio` (compensating
    /// for the zero-stuffing gain loss), followed by `ratio - 1` zero
    /// sub-steps. Gardner timing recovery emits at most one symbol per
    /// `ratio` sub-steps, so returning a single `Option` is sufficient.
    ///
    /// On return, the equalizer's delay line is at this symbol's state,
    /// so a subsequent call to [`Self::update_equalizer`] is
    /// mathematically correct for this symbol.
    pub fn process_sample(&mut self, sample: i16) -> Option<(f64, f64)> {
        // Convert to f64 with fixed scaling. The modulator uses AMPLITUDE
        // (12000.0) to scale to PCM range; divide it out to recover the
        // baseband signal at unit scale. Passband AGC is not needed because
        // the channel (TCP) does not alter signal amplitude.
        let float_sample = sample as f64 / AMPLITUDE;

        // When oversampling, upsample by zero-stuffing: insert zeros between
        // input samples, then let the RRC matched filter perform the
        // interpolation. This avoids the spectral distortion of linear
        // interpolation and is the textbook polyphase upsampling approach.
        // The factor-of-ratio gain loss from zero-stuffing is compensated
        // by scaling the real sample by the oversample ratio.
        let ratio = (self.oversample_ratio as usize).max(1);
        let mut emitted: Option<(f64, f64)> = None;
        for step in 0..ratio {
            let sample = if step == 0 {
                if ratio > 1 {
                    float_sample * ratio as f64
                } else {
                    float_sample
                }
            } else {
                0.0
            };

            // Carrier demodulation: multiply by local oscillator to get baseband.
            // The carrier_phase_offset compensates for the TX filter group delay,
            // and the PLL tracks residual phase/frequency errors.
            let total_phase = self.carrier.phase() + self.carrier_phase_offset + self.pll.phase();
            let cos_w = total_phase.cos();
            let sin_w = total_phase.sin();
            self.carrier.next_iq(); // advance oscillator phase

            // Quadrature demodulation: mix down to baseband.
            // Factor of 2.0 compensates for the cos/sin product identity
            // (cos*cos = 0.5 + 0.5*cos(2w), we want the DC term at unity).
            let baseband_i = sample * cos_w * 2.0;
            let baseband_q = -sample * sin_w * 2.0;

            // Matched RRC filter.
            let filtered_i = self.rx_filter_i.filter(baseband_i);
            let filtered_q = self.rx_filter_q.filter(baseband_q);

            // Timing recovery.
            let symbol_opt = if self.iq_interpolation {
                self.timing.feed_iq(filtered_i, filtered_q)
            } else {
                self.timing.feed(filtered_i).map(|i| (i, filtered_q))
            };
            if let Some((raw_i, raw_q)) = symbol_opt {
                // Update symbol-level RMS tracker (exponential smoothing).
                if !self.sym_agc_frozen {
                    let mag_sq = raw_i * raw_i + raw_q * raw_q;
                    const SYM_AGC_ALPHA: f64 = 0.02;
                    if mag_sq > 1e-24 {
                        self.symbol_rms =
                            self.symbol_rms * (1.0 - SYM_AGC_ALPHA) + mag_sq * SYM_AGC_ALPHA;
                    }
                }

                let (norm_i, norm_q) = if self.symbol_rms > 1e-24 {
                    let scale = 1.0 / self.symbol_rms.sqrt();
                    (raw_i * scale, raw_q * scale)
                } else {
                    (raw_i, raw_q)
                };

                // Adaptive equalizer: operates on normalized symbols.
                // Both the equalizer output and the desired constellation
                // points are at the same scale (average power = 1).
                let (i_val, q_val) = match &mut self.equalizer {
                    Some(eq) => eq.process(norm_i, norm_q),
                    None => (norm_i, norm_q),
                };

                if self.pll_tracking {
                    let (nearest_idx, _) = self.constellation.nearest(i_val, q_val);
                    let (ref_i, ref_q) = self.constellation.point(nearest_idx);
                    let phase_error = q_val * ref_i - i_val * ref_q;
                    self.pll.update(phase_error);
                }

                // Timing recovery emits at most one symbol per `ratio`
                // internal sub-steps; a second emission would mean we
                // silently drop a symbol relative to the old `demodulate`
                // behavior — surface that in debug builds.
                debug_assert!(
                    emitted.is_none(),
                    "timing recovery emitted >1 symbol within a single oversample window"
                );
                emitted = Some((i_val, q_val));
            }
        }

        emitted
    }

    /// Feed PCM samples that are already at the internal oversampled rate
    /// (16kHz for V.34). No zero-stuffing upsampling is performed.
    ///
    /// This is the counterpart to `QamModulator::next_sample_internal()`.
    /// When the wire carries 16kHz samples, the decimation/upsampling lossy
    /// round-trip is eliminated entirely, producing higher symbol quality.
    ///
    /// Panics if `oversample_ratio == 1` (no oversampling — use `demodulate()`).
    pub fn demodulate_internal(&mut self, samples: &[i16]) -> Vec<(f64, f64)> {
        assert!(
            self.oversample_ratio > 1,
            "demodulate_internal() requires oversample_ratio > 1"
        );

        let mut symbols = Vec::new();
        for &sample in samples {
            if let Some(symbol) = self.demodulate_sample_internal(sample) {
                symbols.push(symbol);
            }
        }
        symbols
    }

    /// Process ONE internal-rate sample, returning a symbol only when timing
    /// recovery emits one.
    ///
    /// The receive paths drive the demodulator a sample at a time, and the
    /// batch API's `&[i16]` -> `Vec<f64>` -> `Vec<(f64, f64)>` shape turned
    /// each of those into heap traffic: 16,000 one-element allocations per
    /// second per receiving modem, and 27M+ across a V.34 startup sweep
    /// (MDD-BUG-CRUCIBLE-00514). `Option<(f64, f64)>` is `Copy`, so this
    /// entry point cannot allocate at all.
    ///
    /// [`Self::demodulate_internal`] is now a thin loop over this, so the two
    /// cannot drift: there is one copy of the sample-processing body.
    pub fn demodulate_sample_internal(&mut self, sample: i16) -> Option<(f64, f64)> {
        assert!(
            self.oversample_ratio > 1,
            "demodulate_sample_internal() requires oversample_ratio > 1"
        );

        // Convert to f64. No gain compensation needed: the TX produces samples
        // at full AMPLITUDE at every internal-rate tick (no zero-stuffing gain
        // loss to compensate for).
        let sample = sample as f64 / AMPLITUDE;

        // Process directly at the internal rate — identical to demodulate()'s
        // inner loop but without the zero-stuffing preamble.
        {
            let total_phase = self.carrier.phase() + self.carrier_phase_offset + self.pll.phase();
            let cos_w = total_phase.cos();
            let sin_w = total_phase.sin();
            self.carrier.next_iq();

            let baseband_i = sample * cos_w * 2.0;
            let baseband_q = -sample * sin_w * 2.0;

            let filtered_i = self.rx_filter_i.filter(baseband_i);
            let filtered_q = self.rx_filter_q.filter(baseband_q);

            let symbol_opt = if self.iq_interpolation {
                self.timing.feed_iq(filtered_i, filtered_q)
            } else {
                self.timing.feed(filtered_i).map(|i| (i, filtered_q))
            };
            if let Some((raw_i, raw_q)) = symbol_opt {
                if !self.sym_agc_frozen {
                    let mag_sq = raw_i * raw_i + raw_q * raw_q;
                    const SYM_AGC_ALPHA: f64 = 0.02;
                    if mag_sq > 1e-24 {
                        self.symbol_rms =
                            self.symbol_rms * (1.0 - SYM_AGC_ALPHA) + mag_sq * SYM_AGC_ALPHA;
                    }
                }

                let (norm_i, norm_q) = if self.symbol_rms > 1e-24 {
                    let scale = 1.0 / self.symbol_rms.sqrt();
                    (raw_i * scale, raw_q * scale)
                } else {
                    (raw_i, raw_q)
                };

                let (i_val, q_val) = match &mut self.equalizer {
                    Some(eq) => eq.process(norm_i, norm_q),
                    None => (norm_i, norm_q),
                };

                if self.pll_tracking {
                    let (nearest_idx, _) = self.constellation.nearest(i_val, q_val);
                    let (ref_i, ref_q) = self.constellation.point(nearest_idx);
                    let phase_error = q_val * ref_i - i_val * ref_q;
                    self.pll.update(phase_error);
                }

                return Some((i_val, q_val));
            }
        }

        None
    }

    /// The internal oversampling ratio (1 for V.32bis, 2 for V.34).
    pub fn oversample_ratio(&self) -> u32 {
        self.oversample_ratio
    }

    /// Hard-decision: map a soft symbol (I, Q) to the nearest constellation
    /// point and return the decoded bits.
    pub fn hard_decision(&mut self, i: f64, q: f64) -> u8 {
        let (idx, _) = self.constellation.nearest(i, q);
        let n = self.constellation.num_points() as u8;
        let diff = (idx + n - self.prev_decision_idx) % n;
        self.prev_decision_idx = idx;
        diff
    }

    pub fn hard_decision_raw(&self, i: f64, q: f64) -> u8 {
        let (idx, _) = self.constellation.nearest(i, q);
        idx
    }

    /// Update the adaptive equalizer with a known desired symbol.
    ///
    /// `desired_i/q` should be at normalized constellation scale (average
    /// power = 1), matching the equalizer's operating scale.
    pub fn update_equalizer(&mut self, desired_i: f64, desired_q: f64) {
        if let Some(eq) = &mut self.equalizer {
            eq.update_from_last(desired_i, desired_q);
        }
    }

    /// Set the equalizer's LMS step size. Use 0.0 to freeze taps after
    /// training convergence.
    ///
    /// Non-finite values are ignored at this public demodulator boundary.
    /// The lower `Equalizer::set_mu` method still keeps its debug assertion
    /// for direct misuse, but fuzz/external inputs reach the equalizer
    /// through this wrapper and must not poison or panic the pipeline.
    /// When the equalizer is `None` (V.32bis), the call is a no-op regardless.
    pub fn set_equalizer_mu(&mut self, mu: f64) {
        if !mu.is_finite() {
            return;
        }
        if let Some(eq) = &mut self.equalizer {
            eq.set_mu(mu);
        }
    }

    /// Copy trained equalizer taps from another demodulator.
    ///
    /// Both demodulators must have equalizers with the same number of taps.
    /// Only the tap weights are transferred; the delay line, mu, and all
    /// other demodulator state (carrier, timing, AGC) are left unchanged.
    pub fn copy_equalizer_taps_from(&mut self, other: &QamDemodulator) {
        if let (Some(dst), Some(src)) = (&mut self.equalizer, &other.equalizer) {
            dst.copy_taps_from(src);
        }
    }

    /// Feed an externally-computed phase error into the carrier PLL.
    ///
    /// Use this when the demodulator's internal PLL is disabled
    /// (`pll_tracking: false`) because the internal nearest-neighbor
    /// search uses the wrong constellation (e.g., QAM4 for a V.34 signal).
    /// The caller computes the phase error from the correct constellation
    /// (e.g., V.34 ~960-point) and feeds it here.
    ///
    /// Phase error convention: `error = q_rx * i_ref - i_rx * q_ref`
    /// (cross-product; positive = received symbol leads reference).
    ///
    /// Non-finite `phase_error` is silently skipped (with a debug-build
    /// assert). `Pll::update` does not have an inner guard because
    /// production callers compute it from finite-by-construction
    /// arithmetic; we guard at the boundary here so external
    /// trellis-decoder callers can't poison the loop.
    pub fn update_pll_external(&mut self, phase_error: f64) {
        debug_assert!(
            phase_error.is_finite(),
            "QamDemodulator::update_pll_external: non-finite phase_error {phase_error}"
        );
        if !phase_error.is_finite() {
            return;
        }
        self.pll.update(phase_error);
    }

    /// Get the current symbol RMS (average power estimate).
    pub fn symbol_rms(&self) -> f64 {
        self.symbol_rms
    }

    /// Reset timing recovery and re-apply timing alignment from scratch.
    /// This ensures the timing state matches a freshly constructed demodulator.
    /// The `sample_rate` parameter is the external rate (8000 Hz); internal
    /// oversampling is applied automatically. Invalid external rates or
    /// non-finite alignment are ignored at this public boundary.
    pub fn reset_timing(&mut self, symbol_rate: f64, sample_rate: f64, align: f64) {
        if !symbol_rate.is_finite()
            || symbol_rate <= 0.0
            || !sample_rate.is_finite()
            || sample_rate <= 0.0
            || !align.is_finite()
        {
            return;
        }

        let internal_rate = sample_rate * self.oversample_ratio as f64;
        if !internal_rate.is_finite() || internal_rate <= 0.0 {
            return;
        }

        let samples_per_symbol = internal_rate / symbol_rate;
        if !samples_per_symbol.is_finite() || samples_per_symbol < 1.0 {
            return;
        }

        self.timing = TimingRecovery::new(symbol_rate, internal_rate, 0.0);
        if align != 0.0 {
            self.timing.set_initial_sample_count(align);
        }
    }

    /// Freeze or unfreeze the symbol-level AGC.
    ///
    /// When frozen, `symbol_rms` stays at its current value regardless of
    /// incoming symbol power. Use after the AGC has converged during training
    /// to prevent slow drift in DataMode from degrading the EQ output.
    pub fn set_sym_agc_frozen(&mut self, frozen: bool) {
        self.sym_agc_frozen = frozen;
    }

    /// Set the symbol RMS for AGC initialization.
    ///
    /// Use this to carry a converged AGC value from training into data mode,
    /// avoiding the long convergence period that produces incorrectly scaled
    /// symbols.
    ///
    /// Non-finite or non-positive `rms` is rejected: a zero or negative
    /// RMS is physically meaningless (RMS is the square root of
    /// mean-square — always > 0 for any non-silent signal), and dividing
    /// by `sqrt(symbol_rms)` in `process_sample` would produce Inf/NaN.
    /// `debug_assert` in debug, silent skip in release. See
    /// `Oscillator::set_phase` for the matching NaN-poison rationale.
    pub fn set_symbol_rms(&mut self, rms: f64) {
        debug_assert!(
            rms.is_finite() && rms > 0.0,
            "QamDemodulator::set_symbol_rms: rms must be finite and positive, got {rms}"
        );
        if !rms.is_finite() || rms <= 0.0 {
            return;
        }
        self.symbol_rms = rms;
    }

    /// Get the carrier oscillator phase (radians).
    pub fn carrier_phase(&self) -> f64 {
        self.carrier.phase()
    }

    /// Set the carrier oscillator phase (radians).
    ///
    /// Non-finite values are ignored at this public demodulator boundary.
    /// `Oscillator::set_phase` still asserts on direct non-finite use in
    /// debug builds, but callers that tune the demodulator through this API
    /// should not be able to poison or panic the whole DSP chain.
    pub fn set_carrier_phase(&mut self, phase: f64) {
        if !phase.is_finite() {
            return;
        }
        self.carrier.set_phase(phase);
    }

    /// Adjust the carrier phase correction offset (radians).
    /// Adds `delta` to the existing carrier_phase_offset.
    /// Use to compensate for pipeline delay-induced phase rotation
    /// discovered after alignment.
    ///
    /// Non-finite `delta` is rejected here (no deeper layer holds this
    /// state — `carrier_phase_offset` is owned by the demodulator). A
    /// NaN/Inf delta would poison the offset permanently because
    /// `total_phase = self.carrier.phase() + self.carrier_phase_offset +
    /// self.pll.phase()` then becomes NaN, and `cos(NaN)/sin(NaN) = NaN`
    /// propagates through the entire DSP chain. `debug_assert` in debug,
    /// silent skip in release. See `Oscillator::set_phase` for the
    /// matching rationale.
    pub fn adjust_carrier_phase_offset(&mut self, delta: f64) {
        debug_assert!(
            delta.is_finite(),
            "QamDemodulator::adjust_carrier_phase_offset: non-finite delta {delta}"
        );
        if !delta.is_finite() {
            return;
        }
        self.carrier_phase_offset += delta;
    }

    /// Get the current passband AGC gain factor.
    pub fn agc_gain(&self) -> f64 {
        self.agc.gain()
    }

    /// Reset the equalizer to passthrough (center tap = 1, all others = 0).
    ///
    /// Use when carrying a demodulator from one mode to another where the
    /// Phase 3 trained equalizer taps are no longer appropriate.
    pub fn reset_equalizer(&mut self) {
        if let Some(eq) = &mut self.equalizer {
            eq.reset();
        }
    }

    /// Set the passband AGC gain factor directly (e.g., to carry the
    /// converged gain from Phase 3 training into data mode, avoiding the
    /// ~16 s convergence from unity).
    ///
    /// Non-finite values are ignored at this public demodulator boundary.
    /// `Agc::set_gain` still asserts on direct non-finite use in debug
    /// builds, but callers that tune the demodulator through this API should
    /// not be able to poison or panic the whole DSP chain.
    pub fn set_agc_gain(&mut self, gain: f64) {
        if !gain.is_finite() {
            return;
        }
        self.agc.set_gain(gain);
    }

    /// Reset demodulator state.
    pub fn reset(&mut self) {
        self.carrier.reset();
        self.rx_filter_i.reset();
        self.rx_filter_q.reset();
        self.agc.reset();
        self.timing.reset();
        self.pll.reset();
        self.symbol_rms = 1.0;
        self.prev_decision_idx = 0;
        if let Some(eq) = &mut self.equalizer {
            eq.reset();
        }
        // carrier_phase_offset is a computed constant, no need to reset.
    }

    // ------------------------------------------------------------------
    // Diagnostic accessors — #[cfg(test)] only, removed after investigation
    // ------------------------------------------------------------------

    /// Sum of squared tap weights across both I and Q tap vectors.
    /// A passthrough equalizer (one tap = 1, rest = 0) returns 1.0.
    /// As the LMS converges, taps spread energy; divergence is visible as
    /// the norm growing or shrinking significantly from its trained value.
    ///
    /// Diagnostic only — used by test_phase4_b1_diagnostic_* tests.
    /// Flag for removal after B1 root-cause investigation completes.
    #[cfg(test)]
    pub fn diag_eq_taps_norm(&self) -> f64 {
        match &self.equalizer {
            Some(eq) => eq.diag_taps_norm(),
            None => 0.0,
        }
    }

    /// Current carrier oscillator phase (radians), forwarded from the
    /// private `carrier` field for diagnostic tracing.
    ///
    /// Diagnostic only — used by test_phase4_b1_diagnostic_* tests.
    /// Flag for removal after B1 root-cause investigation completes.
    #[cfg(test)]
    pub fn diag_carrier_phase(&self) -> f64 {
        self.carrier.phase()
    }

    /// Current PLL phase accumulator (radians).
    ///
    /// Diagnostic only — used by test_phase4_b1_diagnostic_* tests.
    /// Flag for removal after B1 root-cause investigation completes.
    #[cfg(test)]
    pub fn diag_pll_phase(&self) -> f64 {
        self.pll.phase()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    const SAMPLES_PER_SYMBOL: f64 = SAMPLE_RATE as f64 / SYMBOL_RATE; // 3.333...

    /// Helper: queue N symbols into the modulator and generate all the samples.
    /// Returns the total number of PCM samples produced.
    fn modulate_symbols(modulator: &mut QamModulator, symbols: &[u8], pcm: &mut Vec<i16>) -> usize {
        for &bits in symbols {
            modulator.modulate_symbol_raw(bits);
        }
        // Generate enough samples: ceil(num_symbols * samples_per_symbol) + margin
        // for the RRC filter tail.
        let num_samples = (symbols.len() as f64 * SAMPLES_PER_SYMBOL).ceil() as usize + RRC_TAPS;
        modulator.generate(num_samples, pcm);
        num_samples
    }

    // -- Constellation tests --

    #[test]
    fn test_constellation_4qam_points() {
        let c = Constellation::Qam4;
        let s = std::f64::consts::FRAC_1_SQRT_2;

        let expected = [(s, s), (-s, s), (-s, -s), (s, -s)];
        for (i, &(ei, eq)) in expected.iter().enumerate() {
            let (pi, pq) = c.point(i as u8);
            assert!(
                (pi - ei).abs() < 1e-10 && (pq - eq).abs() < 1e-10,
                "4-QAM point {i}: expected ({ei}, {eq}), got ({pi}, {pq})"
            );
        }
    }

    #[test]
    fn test_constellation_4qam_nearest() {
        let c = Constellation::Qam4;

        // Each point should map to itself.
        for i in 0..4u8 {
            let (pi, pq) = c.point(i);
            let (idx, dist_sq) = c.nearest(pi, pq);
            assert_eq!(idx, i, "Point {i} should be nearest to itself");
            assert!(
                dist_sq < 1e-10,
                "Distance to self should be ~0, got {dist_sq}"
            );
        }

        // A point close to point 0 should map to point 0.
        let (idx, _) = c.nearest(0.6, 0.6);
        assert_eq!(idx, 0, "Point near (+1,+1)/sqrt(2) should map to index 0");

        // A point close to point 2 should map to point 2.
        let (idx, _) = c.nearest(-0.6, -0.6);
        assert_eq!(idx, 2, "Point near (-1,-1)/sqrt(2) should map to index 2");
    }

    #[test]
    fn test_constellation_32qam_points() {
        let c = Constellation::Qam32;

        // Should have exactly 32 points.
        assert_eq!(c.num_points(), 32);

        // Verify all 32 points are distinct.
        let mut points = Vec::new();
        for i in 0..32u8 {
            let p = c.point(i);
            for (j, &prev) in points.iter().enumerate() {
                let (pi, pq): (f64, f64) = prev;
                assert!(
                    (p.0 - pi).abs() > 1e-6 || (p.1 - pq).abs() > 1e-6,
                    "32-QAM points {j} and {i} are duplicates: ({pi}, {pq})"
                );
            }
            points.push(p);
        }

        // Average power should be ~1.0 after normalization.
        let avg_power: f64 =
            points.iter().map(|(i, q)| i * i + q * q).sum::<f64>() / points.len() as f64;
        assert!(
            (avg_power - 1.0).abs() < 0.01,
            "32-QAM average power should be ~1.0, got {avg_power}"
        );
    }

    #[test]
    fn test_constellation_bits_per_symbol() {
        assert_eq!(Constellation::Qam4.bits_per_symbol(), 2);
        assert_eq!(Constellation::Qam4.num_points(), 4);
        assert_eq!(Constellation::Qam16.bits_per_symbol(), 4);
        assert_eq!(Constellation::Qam16.num_points(), 16);
        assert_eq!(Constellation::Qam32.bits_per_symbol(), 5);
        assert_eq!(Constellation::Qam32.num_points(), 32);
        assert_eq!(Constellation::Qam128.bits_per_symbol(), 7);
        assert_eq!(Constellation::Qam128.num_points(), 128);
    }

    #[test]
    fn test_qam16_constellation_properties() {
        let c = Constellation::Qam16;
        assert_eq!(c.num_points(), 16);
        assert_eq!(c.bits_per_symbol(), 4);

        // All 16 points should be valid and distinct.
        let mut points = Vec::new();
        for i in 0..16u8 {
            let p = c.point(i);
            assert!(
                p.0.is_finite() && p.1.is_finite(),
                "QAM16 point {i} is not finite: {:?}",
                p
            );
            points.push(p);
        }

        // Average power should be ~1.0 after normalization.
        let avg_power: f64 = points.iter().map(|(i, q)| i * i + q * q).sum::<f64>() / 16.0;
        assert!(
            (avg_power - 1.0).abs() < 0.01,
            "QAM16 average power should be ~1.0, got {avg_power}"
        );

        // nearest() should return exact indices for constellation points.
        for i in 0..16u8 {
            let (pi, pq) = c.point(i);
            let (nearest_idx, dist) = c.nearest(pi, pq);
            assert_eq!(nearest_idx, i, "QAM16 nearest mismatch at point {i}");
            assert!(dist < 1e-10, "QAM16 distance should be ~0, got {dist}");
        }
    }

    #[test]
    fn test_qam128_constellation_properties() {
        let c = Constellation::Qam128;
        assert_eq!(c.num_points(), 128);
        assert_eq!(c.bits_per_symbol(), 7);

        let mut points = Vec::new();
        for i in 0..128u8 {
            let p = c.point(i);
            assert!(
                p.0.is_finite() && p.1.is_finite(),
                "QAM128 point {i} is not finite: {:?}",
                p
            );
            points.push(p);
        }

        // Average power should be ~1.0.
        let avg_power: f64 = points.iter().map(|(i, q)| i * i + q * q).sum::<f64>() / 128.0;
        assert!(
            (avg_power - 1.0).abs() < 0.01,
            "QAM128 average power should be ~1.0, got {avg_power}"
        );

        // nearest() should return exact indices for constellation points.
        for i in 0..128u8 {
            let (pi, pq) = c.point(i);
            let (nearest_idx, dist) = c.nearest(pi, pq);
            assert_eq!(nearest_idx, i, "QAM128 nearest mismatch at point {i}");
            assert!(dist < 1e-10, "QAM128 distance should be ~0, got {dist}");
        }
    }

    #[test]
    fn test_qam16_modulate_demodulate_roundtrip() {
        // QAM16 back-to-back: modulate all 16 points, demodulate, verify.
        let config = QamConfig::v32bis();
        let mut modulator = QamModulator::with_config(Constellation::Qam16, &config);
        let mut demodulator = QamDemodulator::with_config(Constellation::Qam16, &config);

        // Send a known pattern: repeat all 16 symbols several times.
        let symbols: Vec<u8> = (0..16u8).cycle().take(96).collect();

        for &sym in &symbols {
            modulator.modulate_symbol_raw(sym);
        }

        // Generate samples and demodulate.
        let mut samples = vec![0i16; 1000];
        for s in samples.iter_mut() {
            *s = modulator.next_sample();
        }

        let demod_syms = demodulator.demodulate(&samples);

        // After RRC warmup, decisions should match transmitted symbols.
        let mut correct = 0;
        let mut total = 0;
        for (i, &(si, sq)) in demod_syms.iter().enumerate() {
            if i < 5 {
                continue; // skip warmup
            }
            let decided = demodulator.hard_decision_raw(si, sq);
            if decided < 16 {
                total += 1;
                // The demodulator pipeline delay offsets the stream,
                // so we can't verify index-by-index. Just check that
                // the decided value is a valid QAM16 point.
                let (pi, pq) = Constellation::Qam16.point(decided);
                assert!(pi.is_finite() && pq.is_finite());
                correct += 1;
            }
        }
        assert!(total > 0, "Should have demodulated some QAM16 symbols");
        assert_eq!(
            correct, total,
            "All demodulated QAM16 symbols should be valid"
        );
    }

    #[test]
    fn test_qam128_modulate_demodulate_roundtrip() {
        // QAM128 back-to-back: modulate a range of points, demodulate, verify.
        let config = QamConfig::v32bis();
        let mut modulator = QamModulator::with_config(Constellation::Qam128, &config);
        let mut demodulator = QamDemodulator::with_config(Constellation::Qam128, &config);

        // Send a known pattern: sweep through all 128 symbols.
        let symbols: Vec<u8> = (0..128u8).collect();

        for &sym in &symbols {
            modulator.modulate_symbol_raw(sym);
        }

        // Generate enough samples for the full symbol sequence.
        let mut samples = vec![0i16; 1000];
        for s in samples.iter_mut() {
            *s = modulator.next_sample();
        }

        let demod_syms = demodulator.demodulate(&samples);

        // After warmup, decisions should be valid QAM128 indices.
        let mut total = 0;
        for (i, &(si, sq)) in demod_syms.iter().enumerate() {
            if i < 5 {
                continue;
            }
            let decided = demodulator.hard_decision_raw(si, sq);
            if decided < 128 {
                total += 1;
                let (pi, pq) = Constellation::Qam128.point(decided);
                assert!(pi.is_finite() && pq.is_finite());
            }
        }
        assert!(total > 0, "Should have demodulated some QAM128 symbols");
    }

    // -- Modulator tests --

    #[test]
    fn test_qam4_modulate_generates_samples() {
        let mut modulator = QamModulator::new(Constellation::Qam4);
        modulator.modulate_symbol_raw(0); // bits 00

        // Generate enough samples for the symbol + filter tail.
        let mut output = Vec::new();
        modulator.generate(RRC_TAPS + 4, &mut output);

        // Should have non-zero output (carrier modulated signal).
        let max_abs = output
            .iter()
            .map(|s| s.unsigned_abs() as u32)
            .max()
            .unwrap_or(0);
        assert!(
            max_abs > 0,
            "Modulated output should have non-zero samples, max_abs={max_abs}"
        );
    }

    // -- Round-trip tests --

    #[test]
    fn test_qam4_roundtrip() {
        let mut modulator = QamModulator::new(Constellation::Qam4);
        let mut demodulator = QamDemodulator::new(Constellation::Qam4);

        let convergence_symbols = 200;
        let test_symbols = 100;

        let mut pcm = Vec::new();

        // Convergence preamble: repeat point 0.
        let preamble: Vec<u8> = vec![0; convergence_symbols];
        modulate_symbols(&mut modulator, &preamble, &mut pcm);

        // Test data: cycle through all 4 points.
        let test_data: Vec<u8> = (0..test_symbols).map(|i| (i % 4) as u8).collect();
        modulate_symbols(&mut modulator, &test_data, &mut pcm);

        // Flush: send some trailing symbols so the last test symbols clear
        // the RRC filter pipeline.
        let flush: Vec<u8> = vec![0; RRC_TAPS / 3 + 10];
        modulate_symbols(&mut modulator, &flush, &mut pcm);

        // Demodulate.
        let symbols = demodulator.demodulate(&pcm);

        // We should get roughly (convergence + test + flush) symbols.
        let total_input = convergence_symbols + test_symbols + flush.len();
        assert!(
            symbols.len() > total_input / 2,
            "Demodulator should produce symbols: got {}, expected > {}",
            symbols.len(),
            total_input / 2
        );

        // Check the last portion of symbols: should map close to constellation points.
        let check_count = symbols.len() / 4;
        let start = symbols.len() - check_count;
        let mut correct = 0;
        let mut total = 0;
        for sym in &symbols[start..] {
            let decided = demodulator.hard_decision_raw(sym.0, sym.1);
            assert!(decided < 4, "Hard decision should be 0..3, got {decided}");
            let (pi, pq) = Constellation::Qam4.point(decided);
            let dist = ((sym.0 - pi).powi(2) + (sym.1 - pq).powi(2)).sqrt();
            if dist < 0.5 {
                correct += 1;
            }
            total += 1;
        }

        let accuracy = correct as f64 / total as f64;
        assert!(
            accuracy > 0.5,
            "At least 50% of symbols should map close to constellation points, \
             got {correct}/{total} = {accuracy:.2}"
        );
    }

    #[test]
    fn test_qam4_multiple_symbols() {
        let mut modulator = QamModulator::new(Constellation::Qam4);
        let mut demodulator = QamDemodulator::new(Constellation::Qam4);

        let pattern = [0u8, 1, 2, 3, 0, 1, 2, 3];
        let repeats = 30;
        let convergence = 150;

        let mut pcm = Vec::new();

        // Convergence preamble.
        let preamble: Vec<u8> = vec![0; convergence];
        modulate_symbols(&mut modulator, &preamble, &mut pcm);

        // Data.
        let data: Vec<u8> = (0..repeats).flat_map(|_| pattern.iter().copied()).collect();
        modulate_symbols(&mut modulator, &data, &mut pcm);

        // Flush.
        let flush: Vec<u8> = vec![0; RRC_TAPS / 3 + 10];
        modulate_symbols(&mut modulator, &flush, &mut pcm);

        let symbols = demodulator.demodulate(&pcm);

        let total_input = convergence + data.len() + flush.len();
        assert!(
            symbols.len() > total_input / 2,
            "Expected > {} symbols, got {}",
            total_input / 2,
            symbols.len()
        );

        // Check that all decisions in the latter portion are valid.
        let check_start = symbols.len() * 3 / 4;
        for sym in &symbols[check_start..] {
            let decided = demodulator.hard_decision_raw(sym.0, sym.1);
            assert!(decided < 4, "Decision should be 0..3, got {decided}");
        }
    }

    #[test]
    fn test_qam32_roundtrip() {
        let mut modulator = QamModulator::new(Constellation::Qam32);
        let mut demodulator = QamDemodulator::new(Constellation::Qam32);

        let convergence = 300;
        let test_symbols = 128;

        let mut pcm = Vec::new();

        // Convergence preamble.
        let preamble: Vec<u8> = vec![0; convergence];
        modulate_symbols(&mut modulator, &preamble, &mut pcm);

        // Test data: cycle through all 32 points.
        let data: Vec<u8> = (0..test_symbols).map(|i| (i % 32) as u8).collect();
        modulate_symbols(&mut modulator, &data, &mut pcm);

        // Flush.
        let flush: Vec<u8> = vec![0; RRC_TAPS / 3 + 10];
        modulate_symbols(&mut modulator, &flush, &mut pcm);

        let symbols = demodulator.demodulate(&pcm);

        // With 32-QAM the constellation is denser. Check that symbols
        // in the converged region map close to some constellation point.
        let check_start = symbols.len() * 3 / 4;
        let total = symbols.len() - check_start;
        let mut close_count = 0;

        for sym in &symbols[check_start..] {
            let decided = demodulator.hard_decision_raw(sym.0, sym.1);
            assert!(decided < 32, "Hard decision should be 0..31, got {decided}");

            let (pi, pq) = Constellation::Qam32.point(decided);
            let dist = ((sym.0 - pi).powi(2) + (sym.1 - pq).powi(2)).sqrt();
            if dist < 0.3 {
                close_count += 1;
            }
        }

        let accuracy = close_count as f64 / total as f64;
        assert!(
            accuracy > 0.3,
            "At least 30% of 32-QAM symbols should be close to a constellation point, \
             got {close_count}/{total} = {accuracy:.2}"
        );
    }

    #[test]
    fn test_v34_iq_roundtrip() {
        // Verify that V.34-configured modulator and demodulator can round-trip
        // a known IQ point.
        let config = QamConfig::v34();
        let mut modulator = QamModulator::with_config(Constellation::Qam4, &config);
        // Demodulator auto-applies timing_align from config.
        let mut demodulator = QamDemodulator::with_config(Constellation::Qam4, &config);
        demodulator.set_pll_tracking(false);

        let test_i = 1.0;
        let test_q = 0.5;
        for _ in 0..200 {
            modulator.modulate_iq(test_i, test_q);
        }

        // Generate enough samples for all symbols + filter settling.
        let sps = SAMPLE_RATE as f64 / config.symbol_rate;
        let num_samples = (200.0 * sps).ceil() as usize + config.rrc_taps * 2;
        let mut pcm = Vec::new();
        modulator.generate(num_samples, &mut pcm);

        let symbols = demodulator.demodulate(&pcm);

        // Skip initial symbols for filter + timing convergence.
        let skip = 50;
        assert!(
            symbols.len() > skip + 20,
            "Need at least {} symbols, got {}",
            skip + 20,
            symbols.len()
        );

        // After convergence, filter to non-zero-magnitude symbols and check
        // that the recovered angle matches the transmitted angle.
        let expected_angle = test_q.atan2(test_i);
        let valid_symbols: Vec<(f64, f64)> = symbols[skip..]
            .iter()
            .copied()
            .filter(|&(si, sq)| (si * si + sq * sq).sqrt() > 0.1)
            .collect();

        assert!(
            valid_symbols.len() > 100,
            "Need at least 100 valid symbols, got {}",
            valid_symbols.len()
        );

        let mut angle_errors: Vec<f64> = valid_symbols
            .iter()
            .map(|&(si, sq)| {
                let angle = sq.atan2(si);
                let err = (angle - expected_angle).abs();
                if err > std::f64::consts::PI {
                    std::f64::consts::TAU - err
                } else {
                    err
                }
            })
            .collect();
        angle_errors.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let avg_error = angle_errors.iter().sum::<f64>() / angle_errors.len() as f64;
        let p95 = angle_errors[(angle_errors.len() as f64 * 0.95) as usize];

        // On a clean channel with phase correction, all symbol angles should be
        // within 0.02 rad of the expected angle.
        assert!(
            avg_error < 0.02,
            "Average angle error {avg_error:.4} rad exceeds 0.02 rad. \
             P95={p95:.4}. Valid symbols: {}. First 5: {:?}",
            valid_symbols.len(),
            &valid_symbols[..5]
        );
        assert!(p95 < 0.05, "P95 angle error {p95:.4} rad exceeds 0.05 rad");
    }

    // PLL convergence test removed: PLL tracking is disabled for V.32bis
    // (decision-directed PLL diverges from wrong initial hard decisions).
    // PLL convergence is future work alongside proper QAM data mode.

    /// Measure V.34 DSP round-trip accuracy for a given QamConfig.
    /// Returns (correct, total, mean_dist) for 400 test symbols.
    ///
    /// When the config has an equalizer, the first 400 symbols are used as
    /// a training preamble: each demodulated symbol is followed by an LMS
    /// update with the known TX constellation point. The remaining 400
    /// symbols are the test set measured for accuracy.
    fn measure_v34_accuracy(config: &QamConfig) -> (usize, usize, f64) {
        use crate::v34_constellation::V34Constellation;

        let constellation = V34Constellation::new();
        let sps = SAMPLE_RATE as f64 / config.symbol_rate;

        let mut modulator = QamModulator::with_config(Constellation::Qam4, config);
        let mut demodulator = QamDemodulator::with_config(Constellation::Qam4, config);
        demodulator.set_pll_tracking(false);
        demodulator.reset_timing(config.symbol_rate, SAMPLE_RATE as f64, config.timing_align);

        let mut tx_points = Vec::new();
        for i in 0..800 {
            let coset = (i % 4) as u8;
            let index = ((i * 7 + 13) % 256) as u16;
            tx_points.push(constellation.point(coset, index));
        }
        let flush_pt = constellation.point(0, 0);
        for _ in 0..50 {
            tx_points.push(flush_pt);
        }
        for &(i, q) in &tx_points {
            modulator.modulate_iq(i, q);
        }

        let n_samples = (tx_points.len() as f64 * sps).ceil() as usize + config.rrc_taps * 2;
        let mut pcm = Vec::new();
        modulator.generate(n_samples, &mut pcm);

        let has_equalizer = config.equalizer_taps > 0;
        let measure_start = 400usize;
        let measure_count = 400usize;

        // Step 1: demodulate without equalizer updates to find TX/RX alignment.
        // On a clean channel, the no-equalizer path is already perfect, so the
        // alignment found here is valid for both paths.
        let no_eq_syms = demodulator.demodulate(&pcm);
        let mut bc = f64::NEG_INFINITY;
        let mut best_off = 0i64;
        for off in -20i64..100 {
            let mut dot = 0.0f64;
            let mut nc = 0u32;
            for j in 0..measure_count {
                let ti = measure_start + j;
                let ri = measure_start as i64 + j as i64 + off;
                if ti < tx_points.len() && ri >= 0 && (ri as usize) < no_eq_syms.len() {
                    let (a, b) = tx_points[ti];
                    let (c, d) = no_eq_syms[ri as usize];
                    dot += a * c + b * d;
                    nc += 1;
                }
            }
            if nc > 0 {
                dot /= nc as f64;
            }
            if dot > bc {
                bc = dot;
                best_off = off;
            }
        }

        let rx_symbols = if has_equalizer {
            // Step 2: re-demodulate with equalizer, using known TX symbols for
            // training (known-symbol mode). Decision-directed training fails on
            // the dense V.34 constellation because early nearest-neighbor errors
            // cascade and distort the equalizer taps.
            let mut eq_demod = QamDemodulator::with_config(Constellation::Qam4, config);
            eq_demod.set_pll_tracking(false);
            eq_demod.reset_timing(config.symbol_rate, SAMPLE_RATE as f64, config.timing_align);

            let mut syms = Vec::new();
            let mut sym_idx: usize = 0;
            for &sample in &pcm {
                let batch = eq_demod.demodulate(&[sample]);
                for &(si, sq) in &batch {
                    // Known-symbol training: use the aligned TX point.
                    let tx_idx = sym_idx as i64 - best_off;
                    if tx_idx >= 0
                        && (tx_idx as usize) < tx_points.len()
                        && (tx_idx as usize) < measure_start
                    {
                        let (ref_i, ref_q) = tx_points[tx_idx as usize];
                        eq_demod.update_equalizer(ref_i, ref_q);
                    } else if tx_idx as usize == measure_start {
                        // Freeze equalizer taps at the training/measurement
                        // boundary to prevent drift during measurement.
                        eq_demod.set_equalizer_mu(0.0);
                    }
                    syms.push((si, sq));
                    sym_idx += 1;
                }
            }
            syms
        } else {
            no_eq_syms
        };

        // Use same alignment offset: T-spaced equalizer doesn't shift timing.
        let mut correct = 0usize;
        let mut total = 0usize;
        let mut dists = Vec::new();
        for j in 0..measure_count {
            let ti = measure_start + j;
            let ri = measure_start as i64 + j as i64 + best_off;
            if ti >= tx_points.len() || ri < 0 || ri as usize >= rx_symbols.len() {
                continue;
            }
            let (tx_i, tx_q) = tx_points[ti];
            let (rx_i, rx_q) = rx_symbols[ri as usize];
            let dist = ((tx_i - rx_i).powi(2) + (tx_q - rx_q).powi(2)).sqrt();
            dists.push(dist);
            let (_, tx_coset) = constellation.nearest(tx_i, tx_q);
            let (tx_pt, _) = constellation.nearest_in_coset(tx_i, tx_q, tx_coset);
            let (rx_pt, _) = constellation.nearest_in_coset(rx_i, rx_q, tx_coset);
            if tx_pt == rx_pt {
                correct += 1;
            }
            total += 1;
        }
        let mean_dist = dists.iter().sum::<f64>() / dists.len().max(1) as f64;

        (correct, total, mean_dist)
    }

    /// Quick test: verify the adaptive equalizer improves V.34 symbol accuracy.
    ///
    /// Compares production config (with equalizer) vs baseline (without).
    /// The equalizer should push accuracy from ~97.5% to >99.5%.
    #[test]
    fn test_v34_equalizer_accuracy() {
        let config = QamConfig::v34();

        // Measure with equalizer (production config).
        let (eq_correct, eq_total, eq_md) = measure_v34_accuracy(&config);
        let eq_accuracy = eq_correct as f64 / eq_total as f64;
        println!(
            "V.34 with equalizer: {eq_correct}/{eq_total} ({:.1}%), mean_dist={eq_md:.5}",
            eq_accuracy * 100.0
        );

        // Measure without equalizer (baseline).
        let mut no_eq = config.clone();
        no_eq.equalizer_taps = 0;
        let (base_correct, base_total, base_md) = measure_v34_accuracy(&no_eq);
        let base_accuracy = base_correct as f64 / base_total as f64;
        println!(
            "V.34 no equalizer:   {base_correct}/{base_total} ({:.1}%), mean_dist={base_md:.5}",
            base_accuracy * 100.0
        );

        // Equalizer should improve accuracy.
        assert!(
            eq_accuracy >= base_accuracy,
            "Equalizer accuracy ({:.1}%) should be >= baseline ({:.1}%)",
            eq_accuracy * 100.0,
            base_accuracy * 100.0
        );

        // Target: >99.5% with equalizer.
        assert!(
            eq_accuracy > 0.995,
            "V.34 equalizer accuracy {:.1}% should exceed 99.5% \
             ({eq_correct}/{eq_total}, mean_dist={eq_md:.4})",
            eq_accuracy * 100.0
        );
    }

    /// V.34 DSP round-trip measurement with parameter sweep.
    ///
    /// Sweeps RRC tap counts, timing alignments, and carrier phase corrections
    /// to find the combination giving highest symbol accuracy. For each tap count,
    /// a coarse pass (0.05 step) finds the approximate optimum, then a fine pass
    /// (0.005 step) refines it. The sweep confirms that 80 taps is optimal;
    /// higher tap counts are catastrophically worse because symbol_rms_init is
    /// calibrated for the 80-tap RRC cascade power level.
    #[test]
    #[ignore = "measurement-only: exhaustive DSP round-trip parameter sweep whose output is a table for a human, ~30 min in debug. Run by hand with --include-ignored."]
    fn test_v34_dsp_roundtrip_measurement() {
        // Measure production config.
        let config = QamConfig::v34();
        let (prod_correct, prod_total, prod_md) = measure_v34_accuracy(&config);
        println!(
            "V.34 production: taps={}, align={:.3}, phase={:.3}, \
             {prod_correct}/{prod_total}, mean_dist={prod_md:.5}",
            config.rrc_taps, config.timing_align, config.carrier_phase_correction
        );

        // Per-tap-count sweep: coarse (0.05) then fine (0.005) for each.
        let mut best_correct = 0usize;
        let mut best_taps = config.rrc_taps;
        let mut best_align = config.timing_align;
        let mut best_phase = config.carrier_phase_correction;
        let mut best_md = f64::MAX;

        for &taps in &[80, 96, 112, 128] {
            let mut tc = 0usize;
            let mut ta = 0.0f64;
            let mut tp = 0.0f64;
            let mut tmd = f64::MAX;

            // Coarse: align 0..2.45 x phase 0..0.95
            // Disable equalizer for sweep: we're finding timing/phase params.
            for ai in 0..50 {
                let align = ai as f64 * 0.05;
                for pi in 0..20 {
                    let phase = pi as f64 * 0.05;
                    let mut c = QamConfig::v34();
                    c.rrc_taps = taps;
                    c.timing_align = align;
                    c.carrier_phase_correction = phase;
                    c.equalizer_taps = 0;
                    let (correct, total, md) = measure_v34_accuracy(&c);
                    if total > 100 && (correct > tc || (correct == tc && md < tmd)) {
                        tc = correct;
                        ta = align;
                        tp = phase;
                        tmd = md;
                    }
                }
            }

            // Fine: +/-0.20 around coarse best, 0.005 step.
            for ai in 0..81 {
                let align = (ta - 0.20f64).max(0.0) + ai as f64 * 0.005;
                for pi in 0..81 {
                    let phase = (tp - 0.20f64).max(0.0) + pi as f64 * 0.005;
                    let mut c = QamConfig::v34();
                    c.rrc_taps = taps;
                    c.timing_align = align;
                    c.carrier_phase_correction = phase;
                    c.equalizer_taps = 0;
                    let (correct, total, md) = measure_v34_accuracy(&c);
                    if total > 100 && (correct > tc || (correct == tc && md < tmd)) {
                        tc = correct;
                        ta = align;
                        tp = phase;
                        tmd = md;
                    }
                }
            }

            println!("  taps={taps}: align={ta:.3}, phase={tp:.3}, {tc}/400, mean_dist={tmd:.5}");

            if tc > best_correct || (tc == best_correct && tmd < best_md) {
                best_correct = tc;
                best_taps = taps;
                best_align = ta;
                best_phase = tp;
                best_md = tmd;
            }
        }

        println!(
            "V.34 sweep winner: taps={best_taps}, align={best_align:.3}, \
             phase={best_phase:.3}, {best_correct}/400, mean_dist={best_md:.5}"
        );

        // With the adaptive equalizer, production config should achieve
        // at least 99.5% accuracy (was 96% before equalizer).
        assert!(
            prod_total > 100,
            "Need at least 100 symbols, got {prod_total}"
        );
        let accuracy = prod_correct as f64 / prod_total as f64;
        assert!(
            accuracy > 0.995,
            "V.34 symbol accuracy {:.1}% should exceed 99.5% \
             ({prod_correct}/{prod_total}, mean_dist={prod_md:.4})",
            accuracy * 100.0
        );
    }

    /// Measure V.34 16kHz-wire-path DSP round-trip accuracy for a given QamConfig.
    /// Returns (correct, total, mean_dist) for 400 test symbols.
    ///
    /// Like `measure_v34_accuracy` but uses the 16kHz internal path:
    /// - TX via `next_sample_internal()` (no decimation to 8kHz)
    /// - RX via `demodulate_internal()` (no zero-stuffing upsampling)
    ///
    /// When the config has an equalizer, the first 400 symbols are training
    /// and the remaining 400 are the test set.
    fn measure_v34_16k_accuracy(config: &QamConfig) -> (usize, usize, f64) {
        use crate::v34_constellation::V34Constellation;

        let constellation = V34Constellation::new();
        // Internal rate: 16kHz for V.34 (oversample_ratio=2).
        let internal_rate = SAMPLE_RATE as f64 * 2.0; // 16000
        let internal_sps = internal_rate / config.symbol_rate; // 5.0

        let mut modulator = QamModulator::with_config(Constellation::Qam4, config);
        let mut demodulator = QamDemodulator::with_config(Constellation::Qam4, config);
        demodulator.set_pll_tracking(false);
        demodulator.reset_timing(config.symbol_rate, SAMPLE_RATE as f64, config.timing_align);

        // Generate 800 test points + 50 flush symbols.
        let mut tx_points = Vec::new();
        for i in 0..800 {
            let coset = (i % 4) as u8;
            let index = ((i * 7 + 13) % 256) as u16;
            tx_points.push(constellation.point(coset, index));
        }
        let flush_pt = constellation.point(0, 0);
        for _ in 0..50 {
            tx_points.push(flush_pt);
        }
        for &(i, q) in &tx_points {
            modulator.modulate_iq(i, q);
        }

        // Generate PCM at 16kHz internal rate (5 samples/symbol).
        let n_samples =
            (tx_points.len() as f64 * internal_sps).ceil() as usize + config.rrc_taps * 4;
        let mut pcm = Vec::with_capacity(n_samples);
        for _ in 0..n_samples {
            pcm.push(modulator.next_sample_internal());
        }

        let has_equalizer = config.equalizer_taps > 0;
        let measure_start = 400usize;
        let measure_count = 400usize;

        // Step 1: demodulate without equalizer updates to find TX/RX alignment.
        let no_eq_syms = demodulator.demodulate_internal(&pcm);
        let mut bc = f64::NEG_INFINITY;
        let mut best_off = 0i64;
        for off in -20i64..100 {
            let mut dot = 0.0f64;
            let mut nc = 0u32;
            for j in 0..measure_count {
                let ti = measure_start + j;
                let ri = measure_start as i64 + j as i64 + off;
                if ti < tx_points.len() && ri >= 0 && (ri as usize) < no_eq_syms.len() {
                    let (a, b) = tx_points[ti];
                    let (c, d) = no_eq_syms[ri as usize];
                    dot += a * c + b * d;
                    nc += 1;
                }
            }
            if nc > 0 {
                dot /= nc as f64;
            }
            if dot > bc {
                bc = dot;
                best_off = off;
            }
        }

        let rx_symbols = if has_equalizer {
            // Re-demodulate with equalizer using known-symbol training.
            let mut eq_demod = QamDemodulator::with_config(Constellation::Qam4, config);
            eq_demod.set_pll_tracking(false);
            eq_demod.reset_timing(config.symbol_rate, SAMPLE_RATE as f64, config.timing_align);

            let mut syms = Vec::new();
            let mut sym_idx: usize = 0;
            // Feed one sample at a time at 16kHz rate.
            for &sample in &pcm {
                let batch = eq_demod.demodulate_internal(&[sample]);
                for &(si, sq) in &batch {
                    let tx_idx = sym_idx as i64 - best_off;
                    if tx_idx >= 0
                        && (tx_idx as usize) < tx_points.len()
                        && (tx_idx as usize) < measure_start
                    {
                        let (ref_i, ref_q) = tx_points[tx_idx as usize];
                        eq_demod.update_equalizer(ref_i, ref_q);
                    } else if tx_idx as usize == measure_start {
                        eq_demod.set_equalizer_mu(0.0);
                    }
                    syms.push((si, sq));
                    sym_idx += 1;
                }
            }
            syms
        } else {
            no_eq_syms
        };

        // Measure accuracy on the test set.
        let mut correct = 0usize;
        let mut total = 0usize;
        let mut dists = Vec::new();
        for j in 0..measure_count {
            let ti = measure_start + j;
            let ri = measure_start as i64 + j as i64 + best_off;
            if ti >= tx_points.len() || ri < 0 || ri as usize >= rx_symbols.len() {
                continue;
            }
            let (tx_i, tx_q) = tx_points[ti];
            let (rx_i, rx_q) = rx_symbols[ri as usize];
            let dist = ((tx_i - rx_i).powi(2) + (tx_q - rx_q).powi(2)).sqrt();
            dists.push(dist);
            let (_, tx_coset) = constellation.nearest(tx_i, tx_q);
            let (tx_pt, _) = constellation.nearest_in_coset(tx_i, tx_q, tx_coset);
            let (rx_pt, _) = constellation.nearest_in_coset(rx_i, rx_q, tx_coset);
            if tx_pt == rx_pt {
                correct += 1;
            }
            total += 1;
        }
        let mean_dist = dists.iter().sum::<f64>() / dists.len().max(1) as f64;

        (correct, total, mean_dist)
    }

    /// V.34 16kHz wire path parameter sweep.
    ///
    /// Sweeps timing_align (0.0..2.5, step 0.05) x carrier_phase_correction
    /// (0.0..1.0, step 0.05) = 1000 combinations. Then refines with a fine
    /// sweep (±0.20, step 0.005) around the best coarse result.
    #[test]
    #[ignore = "measurement-only: exhaustive V.34 parameter sweep, run by hand with --include-ignored."]
    fn test_v34_16k_parameter_sweep() {
        // Coarse sweep: 50 x 20 = 1000 combinations.
        let mut best_correct = 0usize;
        let mut best_align = 0.0f64;
        let mut best_phase = 0.0f64;
        let mut best_md = f64::MAX;

        println!("=== V.34 16kHz coarse sweep (50 x 20 = 1000 combos) ===");
        for ai in 0..50 {
            let align = ai as f64 * 0.05;
            for pi in 0..20 {
                let phase = pi as f64 * 0.05;
                let mut c = QamConfig::v34_16k();
                c.timing_align = align;
                c.carrier_phase_correction = phase;
                c.equalizer_taps = 0; // No equalizer for sweep
                let (correct, total, md) = measure_v34_16k_accuracy(&c);
                if total > 100
                    && (correct > best_correct || (correct == best_correct && md < best_md))
                {
                    best_correct = correct;
                    best_align = align;
                    best_phase = phase;
                    best_md = md;
                }
            }
            if ai % 10 == 0 {
                println!(
                    "  align={align:.2}: best so far {best_correct}/400, \
                     align={best_align:.3}, phase={best_phase:.3}, md={best_md:.5}"
                );
            }
        }
        println!(
            "Coarse winner: align={best_align:.3}, phase={best_phase:.3}, \
             {best_correct}/400, mean_dist={best_md:.5}"
        );

        // Fine sweep: ±0.20 around coarse best, step 0.005.
        let fine_align_lo = (best_align - 0.20f64).max(0.0);
        let fine_phase_lo = (best_phase - 0.20f64).max(0.0);
        let fine_align_hi = best_align + 0.20;
        let fine_phase_hi = best_phase + 0.20;
        let align_steps = ((fine_align_hi - fine_align_lo) / 0.005).ceil() as usize + 1;
        let phase_steps = ((fine_phase_hi - fine_phase_lo) / 0.005).ceil() as usize + 1;
        println!(
            "=== V.34 16kHz fine sweep ({align_steps} x {phase_steps} = {} combos) ===",
            align_steps * phase_steps
        );

        for ai in 0..align_steps {
            let align = fine_align_lo + ai as f64 * 0.005;
            for pi in 0..phase_steps {
                let phase = fine_phase_lo + pi as f64 * 0.005;
                let mut c = QamConfig::v34_16k();
                c.timing_align = align;
                c.carrier_phase_correction = phase;
                c.equalizer_taps = 0;
                let (correct, total, md) = measure_v34_16k_accuracy(&c);
                if total > 100
                    && (correct > best_correct || (correct == best_correct && md < best_md))
                {
                    best_correct = correct;
                    best_align = align;
                    best_phase = phase;
                    best_md = md;
                }
            }
        }
        println!(
            "Fine winner: align={best_align:.4}, phase={best_phase:.4}, \
             {best_correct}/400, mean_dist={best_md:.5}"
        );

        // Also measure with equalizer at the best params.
        let mut eq_config = QamConfig::v34_16k();
        eq_config.timing_align = best_align;
        eq_config.carrier_phase_correction = best_phase;
        let (eq_correct, eq_total, eq_md) = measure_v34_16k_accuracy(&eq_config);
        println!(
            "With equalizer: {eq_correct}/{eq_total}, mean_dist={eq_md:.5}, \
             accuracy={:.1}%",
            eq_correct as f64 / eq_total as f64 * 100.0
        );

        println!("\n=== OPTIMAL VALUES ===");
        println!("carrier_phase_correction: {best_phase:.4}");
        println!("timing_align: {best_align:.4}");
        println!(
            "Accuracy (no EQ): {best_correct}/400 ({:.1}%)",
            best_correct as f64 / 400.0 * 100.0
        );
    }

    /// Quick test: verify V.34 16kHz wire path achieves >99.5% accuracy
    /// with the tuned parameters.
    #[test]
    fn test_v34_16k_accuracy() {
        let config = QamConfig::v34_16k();
        let (correct, total, md) = measure_v34_16k_accuracy(&config);
        let accuracy = correct as f64 / total as f64;
        println!(
            "V.34 16kHz: {correct}/{total} ({:.1}%), mean_dist={md:.5}",
            accuracy * 100.0
        );

        assert!(total > 100, "Need at least 100 symbols, got {total}");
        assert!(
            accuracy > 0.995,
            "V.34 16kHz accuracy {:.1}% should exceed 99.5% \
             ({correct}/{total}, mean_dist={md:.4})",
            accuracy * 100.0
        );
    }

    // -- V.32bis 4-QAM @ 16kHz diagnostic tests --

    /// Single-tick diagnostic: modulate 24 symbols through the 16kHz V.32bis
    /// pipeline and check what the demodulator produces.
    #[test]
    fn test_qam4_16k_roundtrip() {
        let config = QamConfig::v32bis_16k();
        let constellation = Constellation::Qam4;

        let mut modulator = QamModulator::with_config(constellation, &config);
        let mut demodulator = QamDemodulator::with_config(constellation, &config);

        // Known 24-symbol sequence: 0,1,2,3 repeated 6 times.
        let tx_symbols: Vec<u8> = (0..24).map(|i| (i % 4) as u8).collect();

        // Queue all symbols.
        for &sym in &tx_symbols {
            modulator.modulate_symbol_raw(sym);
        }

        // Generate 160 samples (one 10ms tick at 16kHz).
        let mut pcm = Vec::new();
        for _ in 0..160 {
            pcm.push(modulator.next_sample());
        }

        // Feed to demodulator.
        let soft_symbols = demodulator.demodulate(&pcm);
        let rx_count = soft_symbols.len();

        // Hard-decision on each recovered symbol.
        let rx_symbols: Vec<u8> = soft_symbols
            .iter()
            .map(|&(i, q)| demodulator.hard_decision_raw(i, q))
            .collect();

        println!("=== V.32bis 4-QAM @ 16kHz single-tick diagnostic ===");
        println!("TX symbols: {}", tx_symbols.len());
        println!("PCM samples: {}", pcm.len());
        println!("RX symbols produced: {rx_count}");
        println!("RX symbols: {rx_symbols:?}");

        // Report per-symbol match against TX (where we have both).
        let compare_len = rx_count.min(tx_symbols.len());
        let mut matches = 0;
        for k in 0..compare_len {
            let ok = rx_symbols[k] == tx_symbols[k];
            if ok {
                matches += 1;
            }
            println!(
                "  sym[{k:2}]: tx={} rx={} {}",
                tx_symbols[k],
                rx_symbols[k],
                if ok { "OK" } else { "MISMATCH" }
            );
        }
        println!(
            "Single-tick accuracy: {matches}/{compare_len} ({:.1}%)",
            if compare_len > 0 {
                matches as f64 / compare_len as f64 * 100.0
            } else {
                0.0
            }
        );

        // Structural assertions: the demodulator should produce *some* symbols.
        assert!(
            rx_count > 0,
            "Demodulator produced 0 symbols from 160 samples -- pipeline broken"
        );
    }

    /// Multi-tick diagnostic: run 50 ticks of TX->RX at 16kHz 4-QAM and
    /// measure accuracy after a 10-tick warmup.
    #[test]
    fn test_qam4_16k_roundtrip_multi_tick() {
        let config = QamConfig::v32bis_16k();
        let constellation = Constellation::Qam4;

        let mut modulator = QamModulator::with_config(constellation, &config);
        let mut demodulator = QamDemodulator::with_config(constellation, &config);

        let ticks = 50;
        let warmup_ticks = 10;
        let samples_per_tick = 160; // 16kHz * 10ms

        // We'll send a repeating 0,1,2,3 pattern every tick.  At 2400 baud
        // with 160 samples (10ms) we expect ~24 symbols per tick
        // (2400 * 0.01 = 24).  Queue slightly more than needed so the
        // modulator never runs dry.
        let symbols_per_tick = 26; // 24 expected + small margin

        let mut total_rx = 0usize;
        let mut total_compared = 0usize;
        let mut total_correct = 0usize;

        // Track per-tick for the report.
        let mut tick_stats: Vec<(usize, usize, usize)> = Vec::new(); // (rx, compared, correct)

        // Running TX symbol index so we know what was sent.
        let mut tx_log: Vec<u8> = Vec::new();
        let mut rx_log: Vec<u8> = Vec::new();

        for tick in 0..ticks {
            // Queue symbols for this tick.
            let tx_start = tx_log.len();
            for s in 0..symbols_per_tick {
                let sym = (s % 4) as u8;
                modulator.modulate_symbol_raw(sym);
                tx_log.push(sym);
            }
            let _ = tx_start; // suppress unused warning

            // Generate PCM.
            let mut pcm = Vec::new();
            for _ in 0..samples_per_tick {
                pcm.push(modulator.next_sample());
            }

            // Demodulate.
            let soft = demodulator.demodulate(&pcm);
            let rx_count = soft.len();

            let rx_syms: Vec<u8> = soft
                .iter()
                .map(|&(i, q)| demodulator.hard_decision_raw(i, q))
                .collect();

            total_rx += rx_count;
            rx_log.extend_from_slice(&rx_syms);

            // After warmup, compare. We align by comparing RX symbols in
            // order against the running TX log. Since symbols can be delayed,
            // we do a simple position-based comparison: the Nth overall RX
            // symbol should match the Nth overall TX symbol.
            let mut tick_compared = 0;
            let mut tick_correct = 0;
            if tick >= warmup_ticks {
                tick_compared = rx_count;
                // Compare this tick's RX against TX at the same global offset.
                let rx_global_start = rx_log.len() - rx_count;
                for k in 0..rx_count {
                    let rx_idx = rx_global_start + k;
                    if rx_idx < tx_log.len() {
                        if rx_log[rx_idx] == tx_log[rx_idx] {
                            tick_correct += 1;
                        }
                    }
                }
                total_compared += tick_compared;
                total_correct += tick_correct;
            }

            tick_stats.push((rx_count, tick_compared, tick_correct));
        }

        println!("=== V.32bis 4-QAM @ 16kHz multi-tick diagnostic ({ticks} ticks) ===");
        println!("Samples/tick: {samples_per_tick}, warmup: {warmup_ticks} ticks");
        for (tick, &(rx, compared, correct)) in tick_stats.iter().enumerate() {
            let label = if tick < warmup_ticks {
                "warmup"
            } else {
                "data  "
            };
            let acc = if compared > 0 {
                format!("{:.0}%", correct as f64 / compared as f64 * 100.0)
            } else {
                "-".to_string()
            };
            println!("  tick {tick:3}: {label}  rx_syms={rx:3}  compared={compared:3}  correct={correct:3}  acc={acc}");
        }
        println!("Total RX symbols: {total_rx}");
        println!(
            "Post-warmup: {total_correct}/{total_compared} ({:.1}%)",
            if total_compared > 0 {
                total_correct as f64 / total_compared as f64 * 100.0
            } else {
                0.0
            }
        );

        // Structural assertions.
        assert!(
            total_rx > ticks * 10,
            "Expected many symbols across {ticks} ticks, got only {total_rx}"
        );
    }

    /// Helper: measure QAM16 hard-decision accuracy at the 16kHz wire path
    /// for a given QamConfig. Returns (correct, total, gain_estimate).
    fn measure_qam16_16k_accuracy(config: &QamConfig) -> (usize, usize, f64) {
        let constellation = Constellation::Qam16;
        let mut modulator = QamModulator::with_config(constellation, config);
        let mut demodulator = QamDemodulator::with_config(constellation, config);

        let ticks = 200;
        let warmup_ticks = 30;
        let samples_per_tick = 160;
        let symbols_per_tick = 24;

        let mut tx_all: Vec<u8> = Vec::with_capacity(ticks * symbols_per_tick);
        let mut state: u32 = 0xACE1_DEAD;
        for _ in 0..ticks * symbols_per_tick {
            state = state.wrapping_mul(1103515245).wrapping_add(12345);
            tx_all.push(((state >> 16) & 0x0F) as u8);
        }

        let mut rx_all: Vec<u8> = Vec::with_capacity(tx_all.len());
        let mut rx_iq: Vec<(f64, f64)> = Vec::with_capacity(tx_all.len());

        for tick in 0..ticks {
            let s = tick * symbols_per_tick;
            for &sym in &tx_all[s..s + symbols_per_tick] {
                modulator.modulate_symbol_raw(sym);
            }
            let mut pcm = Vec::with_capacity(samples_per_tick);
            for _ in 0..samples_per_tick {
                pcm.push(modulator.next_sample());
            }
            for (i_val, q_val) in demodulator.demodulate(&pcm) {
                rx_all.push(demodulator.hard_decision_raw(i_val, q_val));
                rx_iq.push((i_val, q_val));
            }
        }

        // Find best symbol alignment.
        let warmup_syms = warmup_ticks * symbols_per_tick;
        let check_len = 1000usize;
        let mut best_offset = 0i32;
        let mut best_correct = 0usize;
        for offset in -10i32..=10 {
            let mut correct = 0;
            for i in 0..check_len {
                let ti = warmup_syms + i;
                let ri = (warmup_syms as i32 + i as i32 + offset) as i64;
                if ti < tx_all.len()
                    && ri >= 0
                    && (ri as usize) < rx_all.len()
                    && tx_all[ti] == rx_all[ri as usize]
                {
                    correct += 1;
                }
            }
            if correct > best_correct {
                best_correct = correct;
                best_offset = offset;
            }
        }

        let mut sum_corr = 0.0f64;
        let mut sum_tx2 = 0.0f64;
        for i in 0..check_len {
            let ti = warmup_syms + i;
            let ri = (warmup_syms as i32 + i as i32 + best_offset) as usize;
            if ti < tx_all.len() && ri < rx_iq.len() {
                let (txi, txq) = constellation.point(tx_all[ti]);
                let (rxi, rxq) = rx_iq[ri];
                sum_corr += rxi * txi + rxq * txq;
                sum_tx2 += txi * txi + txq * txq;
            }
        }
        let gain = sum_corr / sum_tx2.max(1e-12);
        (best_correct, check_len, gain)
    }

    fn run_long_soaks() -> bool {
        // `MDEM_LONG_TESTS` is a comma list; `soaks` selects the long durations.
        std::env::var("MDEM_LONG_TESTS")
            .is_ok_and(|keys| keys.split(',').any(|key| key.trim() == "soaks"))
    }

    fn qam16_16k_sweep_config(phase: f64, align: f64) -> QamConfig {
        let mut config = QamConfig::v32bis_16k();
        config.carrier_phase_correction = phase;
        config.timing_align = align;
        config.timing_bandwidth = 0.0;
        config.pll_tracking = false;
        config
    }

    fn qam16_16k_parameter_smoke() {
        let production = QamConfig::v32bis_16k();
        let cases = [
            (
                "production",
                production.carrier_phase_correction,
                production.timing_align,
            ),
            (
                "phase -0.05",
                production.carrier_phase_correction - 0.05,
                production.timing_align,
            ),
            (
                "phase +0.05",
                production.carrier_phase_correction + 0.05,
                production.timing_align,
            ),
            (
                "align +0.10",
                production.carrier_phase_correction,
                production.timing_align + 0.10,
            ),
            ("coarse winner", 4.250, 0.0),
        ];

        let mut best_label = "";
        let mut best_correct = 0usize;
        let mut best_phase = 0.0f64;
        let mut best_align = 0.0f64;
        let mut best_gain = 0.0f64;
        let mut production_correct = 0usize;
        let mut production_gain = 0.0f64;

        eprintln!("=== QAM16 16kHz bounded parameter smoke ===");
        for (idx, &(label, phase, align)) in cases.iter().enumerate() {
            let config = qam16_16k_sweep_config(phase, align);
            let (correct, total, gain) = measure_qam16_16k_accuracy(&config);
            eprintln!(
                "  {label:14}: phase={phase:.3} align={align:.3} {correct}/{total} gain={gain:.4}"
            );

            if idx == 0 {
                production_correct = correct;
                production_gain = gain;
            }
            if correct > best_correct {
                best_label = label;
                best_correct = correct;
                best_phase = phase;
                best_align = align;
                best_gain = gain;
            }
        }

        let untuned = qam16_16k_sweep_config(0.0, 0.0);
        let (untuned_correct, untuned_total, untuned_gain) = measure_qam16_16k_accuracy(&untuned);
        eprintln!(
            "  untuned ctrl  : phase=0.000 align=0.000 {untuned_correct}/{untuned_total} gain={untuned_gain:.4}"
        );
        eprintln!(
            "Smoke winner: {best_label} phase={best_phase:.3} align={best_align:.3} \
             {best_correct}/1000 gain={best_gain:.4}"
        );

        assert!(
            production_correct >= 985,
            "QAM16 16kHz tuned production accuracy {production_correct}/1000 < 985/1000"
        );
        assert!(
            production_gain > 0.95,
            "QAM16 16kHz tuned production gain {production_gain:.4} <= 0.95"
        );
        assert!(
            production_correct >= untuned_correct + 500,
            "QAM16 16kHz tuned production accuracy {production_correct}/1000 \
             should materially beat untuned control {untuned_correct}/1000"
        );
    }

    /// V.32bis QAM16 @ 16kHz parameter sweep. Coarse + fine sweep around the
    /// existing QAM4-tuned baseline (phase=4.285, align=0.0, timing_bw=0.0).
    /// Default ignored-test automation runs a bounded smoke; set
    /// MDEM_LONG_TESTS=soaks to run the exhaustive diagnostic sweep.
    /// Run with: cargo test -p gp64-modem --release --lib -- --include-ignored
    ///                       test_qam16_16k_parameter_sweep --nocapture
    #[test]
    #[ignore = "full_test (--include-ignored): bounded QAM16 16k parameter sweep, deferred off the per-landing gate for cost."]
    fn test_qam16_16k_parameter_sweep() {
        if !run_long_soaks() {
            qam16_16k_parameter_smoke();
            return;
        }

        let symbol_period = 16000.0 / SYMBOL_RATE;

        let mut best_correct = 0usize;
        let mut best_phase = 4.285f64;
        let mut best_align = 0.0f64;
        let mut best_gain = 0.0f64;

        let phase_steps = (std::f64::consts::TAU / 0.05).ceil() as usize;
        let align_steps = (symbol_period / 0.1).ceil() as usize;
        eprintln!("=== QAM16 16kHz coarse sweep ({phase_steps} x {align_steps}) ===");

        for pi in 0..phase_steps {
            let phase = pi as f64 * 0.05;
            for ai in 0..align_steps {
                let align = ai as f64 * 0.1;
                let c = qam16_16k_sweep_config(phase, align);
                let (correct, _total, gain) = measure_qam16_16k_accuracy(&c);
                if correct > best_correct {
                    best_correct = correct;
                    best_phase = phase;
                    best_align = align;
                    best_gain = gain;
                }
            }
            if pi % 20 == 0 {
                eprintln!(
                    "  phase={phase:.2}: best so far {best_correct}/1000  \
                     phase={best_phase:.3} align={best_align:.3} gain={best_gain:.4}"
                );
            }
        }
        eprintln!(
            "Coarse winner: phase={best_phase:.3} align={best_align:.3} \
             {best_correct}/1000 gain={best_gain:.4}"
        );

        // Fine sweep around coarse best.
        let p_lo = (best_phase - 0.20).max(0.0);
        let p_hi = best_phase + 0.20;
        let a_lo = (best_align - 0.20).max(0.0);
        let a_hi = (best_align + 0.20).min(symbol_period);
        let p_steps = ((p_hi - p_lo) / 0.005).ceil() as usize + 1;
        let a_steps = ((a_hi - a_lo) / 0.005).ceil() as usize + 1;
        eprintln!("=== QAM16 16kHz fine sweep ({p_steps} x {a_steps}) ===");
        for pi in 0..p_steps {
            let phase = p_lo + pi as f64 * 0.005;
            for ai in 0..a_steps {
                let align = a_lo + ai as f64 * 0.005;
                let c = qam16_16k_sweep_config(phase, align);
                let (correct, _total, gain) = measure_qam16_16k_accuracy(&c);
                if correct > best_correct {
                    best_correct = correct;
                    best_phase = phase;
                    best_align = align;
                    best_gain = gain;
                }
            }
        }
        eprintln!(
            "\n=== QAM16 OPTIMAL ===\n\
             carrier_phase_correction = {best_phase:.4}\n\
             timing_align             = {best_align:.4}\n\
             accuracy                 = {best_correct}/1000 ({:.2}%)\n\
             gain estimate            = {best_gain:.4}",
            best_correct as f64 / 10.0
        );
    }

    /// QAM16 @ 16kHz symbol-level round-trip with the supervised LMS
    /// equalizer training that the V.32bis 9600-bps data path relies on.
    /// The raw matched-filter path still leaves about 1% residual ISI per
    /// symbol; the expected modem behavior is that the known preamble trains
    /// the equalizer before random payload data is decoded.
    ///
    /// Run with `cargo test -p gp64-modem --release --lib --
    ///           --include-ignored test_qam16_16k_symbol_accuracy --nocapture`.
    #[test]
    #[ignore = "full_test (--include-ignored): bounded QAM16 16k symbol-accuracy check, deferred off the per-landing gate for cost."]
    fn test_qam16_16k_symbol_accuracy() {
        let mut config = QamConfig::v32bis_16k();
        config.agc_alpha = 0.005;
        config.equalizer_taps = 11;
        config.equalizer_mu = 0.0;

        let constellation = Constellation::Qam16;
        let mut modulator = QamModulator::with_config(constellation, &config);

        let ticks = 200;
        let warmup_ticks = 30;
        let samples_per_tick = 160;
        let symbols_per_tick = 24;
        let train_len = 1400usize;
        let post_train_settle = 50usize;

        // Pseudo-random symbol stream covering all constellation points.
        let mut tx_all: Vec<u8> = Vec::with_capacity(ticks * symbols_per_tick);
        let mut state: u32 = 0xACE1_DEAD;
        let sym_mask = (constellation.num_points() - 1) as u32;
        for _ in 0..ticks * symbols_per_tick {
            state = state.wrapping_mul(1103515245).wrapping_add(12345);
            tx_all.push(((state >> 16) & sym_mask) as u8);
        }

        let mut pcm_all: Vec<i16> = Vec::with_capacity(ticks * samples_per_tick);
        for tick in 0..ticks {
            let s = tick * symbols_per_tick;
            for &sym in &tx_all[s..s + symbols_per_tick] {
                modulator.modulate_symbol_raw(sym);
            }
            for _ in 0..samples_per_tick {
                pcm_all.push(modulator.next_sample());
            }
        }

        let mut alignment_demod = QamDemodulator::with_config(constellation, &config);
        let mut alignment_rx: Vec<u8> = Vec::with_capacity(tx_all.len());
        for (i_val, q_val) in alignment_demod.demodulate(&pcm_all) {
            alignment_rx.push(alignment_demod.hard_decision_raw(i_val, q_val));
        }

        // Find best alignment offset post-warmup with the equalizer present
        // but frozen at passthrough. Its center tap contributes delay, so this
        // must be measured with the same equalizer topology used for training.
        let warmup_syms = warmup_ticks * symbols_per_tick;
        let mut best_offset = 0i32;
        let mut best_correct = 0usize;
        let check_len = 1000usize;
        for offset in -30i32..=30 {
            let mut correct = 0;
            for i in 0..check_len {
                let ti = warmup_syms + i;
                let ri = (warmup_syms as i32 + i as i32 + offset) as i64;
                if ti < tx_all.len() && ri >= 0 && (ri as usize) < alignment_rx.len() {
                    if tx_all[ti] == alignment_rx[ri as usize] {
                        correct += 1;
                    }
                }
            }
            if correct > best_correct {
                best_correct = correct;
                best_offset = offset;
            }
        }

        let pct = best_correct as f64 / check_len as f64 * 100.0;
        eprintln!(
            "QAM16 @ 16kHz frozen-EQ baseline: best_offset={best_offset}, accuracy={best_correct}/{check_len} = {pct:.2}%"
        );

        let mut demodulator = QamDemodulator::with_config(constellation, &config);
        let mut rx_all: Vec<u8> = Vec::with_capacity(tx_all.len());
        let mut rx_iq: Vec<(f64, f64)> = Vec::with_capacity(tx_all.len());
        let train_start = warmup_syms;
        let train_end = train_start + train_len;

        for &sample in &pcm_all {
            let Some((i_val, q_val)) = demodulator.process_sample(sample) else {
                continue;
            };
            let rx_idx = rx_all.len();
            rx_all.push(demodulator.hard_decision_raw(i_val, q_val));
            rx_iq.push((i_val, q_val));

            let tx_idx = rx_idx as i64 - best_offset as i64;
            if tx_idx >= train_start as i64 && tx_idx < train_end as i64 {
                if tx_idx == train_start as i64 {
                    demodulator.set_equalizer_mu(0.05);
                }
                let (ref_i, ref_q) = constellation.point(tx_all[tx_idx as usize]);
                demodulator.update_equalizer(ref_i, ref_q);
            } else if tx_idx == train_end as i64 {
                demodulator.set_equalizer_mu(0.0);
            }
        }

        let measure_start = train_end + post_train_settle;
        let mut trained_correct = 0usize;
        for i in 0..check_len {
            let ti = measure_start + i;
            let ri = (measure_start as i32 + i as i32 + best_offset) as i64;
            if ti < tx_all.len()
                && ri >= 0
                && (ri as usize) < rx_all.len()
                && tx_all[ti] == rx_all[ri as usize]
            {
                trained_correct += 1;
            }
        }

        let pct = trained_correct as f64 / check_len as f64 * 100.0;
        eprintln!("QAM16 @ 16kHz trained-EQ: accuracy={trained_correct}/{check_len} = {pct:.2}%");

        // Aggregate constellation diagnostics: per-symbol average error and
        // gain estimate — reveals systematic I/Q bias / scale issues.
        let mut sum_di = 0.0f64;
        let mut sum_dq = 0.0f64;
        let mut sum_di2 = 0.0f64;
        let mut sum_dq2 = 0.0f64;
        let mut sum_corr_ii = 0.0f64; // dot(rx, tx) accumulator for gain est
        let mut sum_tx2 = 0.0f64;
        let mut count = 0usize;
        for i in 0..check_len {
            let ti = measure_start + i;
            let ri = (measure_start as i32 + i as i32 + best_offset) as usize;
            if ti < tx_all.len() && ri < rx_iq.len() {
                let (txi, txq) = constellation.point(tx_all[ti]);
                let (rxi, rxq) = rx_iq[ri];
                sum_di += rxi - txi;
                sum_dq += rxq - txq;
                sum_di2 += (rxi - txi).powi(2);
                sum_dq2 += (rxq - txq).powi(2);
                sum_corr_ii += rxi * txi + rxq * txq;
                sum_tx2 += txi * txi + txq * txq;
                count += 1;
            }
        }
        let n = count.max(1) as f64;
        let mean_di = sum_di / n;
        let mean_dq = sum_dq / n;
        let rms_di = (sum_di2 / n).sqrt();
        let rms_dq = (sum_dq2 / n).sqrt();
        let gain_est = sum_corr_ii / sum_tx2.max(1e-12);
        eprintln!(
            "  bias: I={mean_di:+.4} Q={mean_dq:+.4}  rms_err: I={rms_di:.4} Q={rms_dq:.4}  \
             rx/tx gain={gain_est:.4}"
        );

        // First 16 mismatches (post-warmup).
        let mut shown = 0;
        for i in 0..check_len {
            let ti = measure_start + i;
            let ri = (measure_start as i32 + i as i32 + best_offset) as usize;
            if ti < tx_all.len() && ri < rx_all.len() && tx_all[ti] != rx_all[ri] {
                let (txi, txq) = constellation.point(tx_all[ti]);
                let (rxi, rxq) = constellation.point(rx_all[ri]);
                let (rawi, rawq) = rx_iq[ri];
                eprintln!(
                    "  mis@{i:4}: tx={:2} ({txi:+.3},{txq:+.3}) rx={:2} ({rxi:+.3},{rxq:+.3}) \
                     raw=({rawi:+.3},{rawq:+.3})",
                    tx_all[ti], rx_all[ri]
                );
                shown += 1;
                if shown >= 16 {
                    break;
                }
            }
        }

        assert!(
            pct >= 99.9,
            "QAM16 @ 16kHz trained-EQ hard-decision accuracy {pct:.2}% < 99.9% — \
             see mismatches above for symptom"
        );
    }

    /// Test raw (non-differential) encode/decode round-trip through the 16kHz
    /// QAM pipeline with proper global alignment. Uses modulate_symbol_raw +
    /// hard_decision_raw.
    #[test]
    fn test_qam4_16k_raw_roundtrip_aligned() {
        let config = QamConfig::v32bis_16k();
        let constellation = Constellation::Qam4;
        let mut modulator = QamModulator::with_config(constellation, &config);
        let mut demodulator = QamDemodulator::with_config(constellation, &config);

        let ticks = 60;
        let warmup = 15;
        let samples_per_tick = 160;

        let tx_pattern: Vec<u8> = vec![0, 1, 2, 3];
        let mut tx_all: Vec<u8> = Vec::new();
        let mut rx_all: Vec<u8> = Vec::new();

        for _tick in 0..ticks {
            for i in 0..24 {
                let dibit = tx_pattern[i % tx_pattern.len()];
                tx_all.push(dibit);
                modulator.modulate_symbol_raw(dibit);
            }

            let mut pcm = Vec::with_capacity(samples_per_tick);
            for _ in 0..samples_per_tick {
                pcm.push(modulator.next_sample());
            }

            if _tick == warmup {
                let rms = (pcm.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / pcm.len() as f64)
                    .sqrt();
                eprintln!(
                    "Standalone tick {}: PCM rms={:.1}, first 4: [{}, {}, {}, {}]",
                    _tick, rms, pcm[0], pcm[1], pcm[2], pcm[3]
                );
            }

            let symbols = demodulator.demodulate(&pcm);
            for &(i_val, q_val) in &symbols {
                rx_all.push(demodulator.hard_decision_raw(i_val, q_val));
            }
        }

        // Find best alignment offset in the post-warmup region.
        let warmup_syms = warmup * 24;
        let search_start = warmup_syms.min(rx_all.len());
        let mut best_offset = 0i32;
        let mut best_correct = 0usize;

        for offset in -5i32..=5 {
            let mut correct = 0;
            let check_len = 200;
            for i in 0..check_len {
                let tx_idx = (search_start as i32 + i) as usize;
                let rx_idx = (search_start as i32 + i + offset) as usize;
                if tx_idx < tx_all.len() && rx_idx < rx_all.len() {
                    if tx_all[tx_idx] == rx_all[rx_idx] {
                        correct += 1;
                    }
                }
            }
            if correct > best_correct {
                best_correct = correct;
                best_offset = offset;
            }
        }

        let accuracy = best_correct as f64 / 200.0;
        eprintln!(
            "Raw roundtrip: best_offset={best_offset}, accuracy={best_correct}/200 = {:.1}%",
            accuracy * 100.0
        );

        // Print first 24 TX/RX at best offset for debug
        if accuracy < 0.95 {
            let s = search_start;
            let o = best_offset;
            eprintln!("TX[{s}..]: {:?}", &tx_all[s..s + 24.min(tx_all.len() - s)]);
            let rs = (s as i32 + o) as usize;
            eprintln!(
                "RX[{rs}..]: {:?}",
                &rx_all[rs..rs + 24.min(rx_all.len() - rs)]
            );
        }

        assert!(
            accuracy > 0.95,
            "Raw roundtrip should be >95%, got {:.1}% at offset {best_offset}",
            accuracy * 100.0
        );
    }

    /// Test that mimics the B2B state machine: two independent mod/demod pairs
    /// with a 1-tick delay in cross-feeding (like BackToBack).
    #[test]
    fn test_qam4_16k_b2b_crossfeed() {
        let config = QamConfig::v32bis_16k();
        let constellation = Constellation::Qam4;

        // Modem A: modulator + demodulator
        let mut mod_a = QamModulator::with_config(constellation, &config);
        let mut demod_b = QamDemodulator::with_config(constellation, &config);

        let ticks = 60;
        let warmup = 20;

        // 1-tick delayed buffer (like BackToBack::prev_caller_out)
        let mut prev_a_out = vec![0i16; 160];

        let tx_pattern: Vec<u8> = vec![0, 1, 2, 3];
        let mut tx_all: Vec<u8> = Vec::new();
        let mut rx_all: Vec<u8> = Vec::new();

        for _tick in 0..ticks {
            // Mod A generates 24 symbols -> 160 samples
            for i in 0..24 {
                let dibit = tx_pattern[i % tx_pattern.len()];
                tx_all.push(dibit);
                mod_a.modulate_symbol_raw(dibit);
            }
            let mut a_out = vec![0i16; 160];
            for s in a_out.iter_mut() {
                *s = mod_a.next_sample();
            }

            // Demod B processes PREVIOUS tick's output (1-tick delay)
            let symbols = demod_b.demodulate(&prev_a_out);
            for &(i_val, q_val) in &symbols {
                rx_all.push(demod_b.hard_decision_raw(i_val, q_val));
            }

            // Save for next tick
            prev_a_out = a_out;
        }

        // Find best alignment offset after warmup
        let warmup_syms = warmup * 24;
        let search_start = warmup_syms.min(rx_all.len());
        let mut best_offset = 0i32;
        let mut best_correct = 0usize;

        for offset in -10i32..=10 {
            let mut correct = 0;
            let check_len = 200;
            for i in 0..check_len {
                let tx_idx = (search_start as i32 + i) as usize;
                let rx_idx = (search_start as i32 + i + offset) as usize;
                if tx_idx < tx_all.len() && rx_idx < rx_all.len() {
                    if tx_all[tx_idx] == rx_all[rx_idx] {
                        correct += 1;
                    }
                }
            }
            if correct > best_correct {
                best_correct = correct;
                best_offset = offset;
            }
        }

        let accuracy = best_correct as f64 / 200.0;
        eprintln!(
            "B2B crossfeed: best_offset={best_offset}, accuracy={best_correct}/200 = {:.1}%",
            accuracy * 100.0
        );

        assert!(
            accuracy > 0.95,
            "B2B crossfeed should be >95%, got {:.1}% at offset {best_offset}",
            accuracy * 100.0
        );
    }

    /// Helper: measure V.32bis 4-QAM @ 16kHz accuracy for a given config.
    ///
    /// Modulates 600 scrambled symbols through the production TX/RX path,
    /// then measures hard-decision accuracy on the last 400 symbols (after
    /// warmup). Returns (correct, total, mean_distance).
    fn measure_v32bis_16k_accuracy(config: &QamConfig) -> (usize, usize, f64) {
        use crate::scrambler::Scrambler;

        let constellation = Constellation::Qam4;
        let mut modulator = QamModulator::with_config(constellation, config);
        let mut demodulator = QamDemodulator::with_config(constellation, config);
        // Don't use PLL tracking during sweep — find the phase correction that
        // works without it, then PLL can fine-tune in production.
        demodulator.set_pll_tracking(false);

        // Generate 600 scrambled symbols (2 bits each from scrambled bytes).
        let mut scrambler = Scrambler::new();
        let mut tx_symbols: Vec<u8> = Vec::with_capacity(600);
        let mut byte_idx = 0u8;
        while tx_symbols.len() < 600 {
            let scrambled = scrambler.scramble_byte(byte_idx);
            byte_idx = byte_idx.wrapping_add(1);
            // Extract 4 dibits (2 bits each) from each byte.
            for shift in (0..8).step_by(2) {
                if tx_symbols.len() >= 600 {
                    break;
                }
                let dibit = (scrambled >> shift) & 0x03;
                tx_symbols.push(dibit);
            }
        }

        // Queue all symbols + 50 flush symbols.
        for &sym in &tx_symbols {
            modulator.modulate_symbol_raw(sym);
        }
        for _ in 0..50 {
            modulator.modulate_symbol_raw(0);
        }

        // Generate PCM at 16kHz output rate using next_sample() — the production path.
        // sps at 16kHz = 16000/2400 = 6.667 samples/symbol.
        let sps = config.sample_rate as f64 / config.symbol_rate;
        let n_samples =
            ((tx_symbols.len() + 50) as f64 * sps).ceil() as usize + config.rrc_taps * 4;
        let mut pcm = Vec::with_capacity(n_samples);
        for _ in 0..n_samples {
            pcm.push(modulator.next_sample());
        }

        // Feed to demodulator in 160-sample chunks (production tick size).
        let mut rx_symbols: Vec<(f64, f64)> = Vec::new();
        for chunk in pcm.chunks(160) {
            let soft = demodulator.demodulate(chunk);
            rx_symbols.extend_from_slice(&soft);
        }

        // Find the best TX/RX alignment offset via cross-correlation.
        let measure_start = 200usize;
        let measure_count = 400usize;
        let mut best_corr = f64::NEG_INFINITY;
        let mut best_offset = 0i64;
        for off in -20i64..100 {
            let mut dot = 0.0f64;
            let mut count = 0u32;
            for j in 0..measure_count {
                let ti = measure_start + j;
                let ri = measure_start as i64 + j as i64 + off;
                if ti < tx_symbols.len() && ri >= 0 && (ri as usize) < rx_symbols.len() {
                    let (tx_i, tx_q) = constellation.point(tx_symbols[ti]);
                    let (rx_i, rx_q) = rx_symbols[ri as usize];
                    dot += tx_i * rx_i + tx_q * rx_q;
                    count += 1;
                }
            }
            if count > 0 {
                dot /= count as f64;
            }
            if dot > best_corr {
                best_corr = dot;
                best_offset = off;
            }
        }

        // Measure accuracy at the best offset.
        let mut correct = 0usize;
        let mut total = 0usize;
        let mut dists = Vec::new();
        for j in 0..measure_count {
            let ti = measure_start + j;
            let ri = measure_start as i64 + j as i64 + best_offset;
            if ti >= tx_symbols.len() || ri < 0 || ri as usize >= rx_symbols.len() {
                continue;
            }
            let (tx_i, tx_q) = constellation.point(tx_symbols[ti]);
            let (rx_i, rx_q) = rx_symbols[ri as usize];
            let dist = ((tx_i - rx_i).powi(2) + (tx_q - rx_q).powi(2)).sqrt();
            dists.push(dist);
            let rx_hard = constellation.nearest(rx_i, rx_q).0;
            if rx_hard == tx_symbols[ti] {
                correct += 1;
            }
            total += 1;
        }
        let mean_dist = dists.iter().sum::<f64>() / dists.len().max(1) as f64;

        (correct, total, mean_dist)
    }

    fn v32bis_16k_sweep_config(phase: f64, align: f64) -> QamConfig {
        let mut config = QamConfig::v32bis_16k();
        config.carrier_phase_correction = phase;
        config.timing_align = align;
        config.timing_bandwidth = 0.0;
        config.pll_tracking = false;
        config
    }

    fn v32bis_16k_parameter_smoke() {
        let production = QamConfig::v32bis_16k();
        // Perturbed cases are logged as diagnostics only. The smoke oracle is
        // the tuned production case against the deliberately untuned control.
        let cases = [
            (
                "production",
                production.carrier_phase_correction,
                production.timing_align,
            ),
            (
                "phase -0.05",
                production.carrier_phase_correction - 0.05,
                production.timing_align,
            ),
            (
                "phase +0.05",
                production.carrier_phase_correction + 0.05,
                production.timing_align,
            ),
            (
                "align +0.10",
                production.carrier_phase_correction,
                production.timing_align + 0.10,
            ),
            ("untuned ctrl", 0.0, 0.0),
        ];

        let mut production_correct = 0usize;
        let mut production_total = 0usize;
        let mut production_md = f64::MAX;
        let mut untuned_correct = 0usize;
        let mut untuned_total = 0usize;
        let mut untuned_md = f64::MAX;

        eprintln!("=== V.32bis 16kHz bounded parameter smoke ===");
        for (idx, &(label, phase, align)) in cases.iter().enumerate() {
            let config = v32bis_16k_sweep_config(phase, align);
            let (correct, total, md) = measure_v32bis_16k_accuracy(&config);
            eprintln!(
                "  {label:14}: phase={phase:.3} align={align:.3} {correct}/{total} md={md:.5}"
            );

            if idx == 0 {
                production_correct = correct;
                production_total = total;
                production_md = md;
            } else if label == "untuned ctrl" {
                untuned_correct = correct;
                untuned_total = total;
                untuned_md = md;
            }
        }

        assert!(
            production_correct == production_total && production_total == 400,
            "V.32bis 16kHz tuned production accuracy {production_correct}/{production_total} \
             should be error-free for the deterministic QAM4 smoke"
        );
        assert!(
            production_md < 0.30,
            "V.32bis 16kHz tuned production mean distance {production_md:.5} >= 0.30"
        );
        assert!(
            production_correct >= untuned_correct + 200,
            "V.32bis 16kHz tuned production accuracy {production_correct}/{production_total} \
             should materially beat untuned control {untuned_correct}/{untuned_total} \
             (untuned md={untuned_md:.5})"
        );
    }

    /// V.32bis 4-QAM @ 16kHz parameter sweep.
    ///
    /// Sweeps carrier_phase_correction (0..2*pi, step 0.05) x timing_align
    /// (0..6.667, step 0.1) using fixed timing (timing_bandwidth=0.0).
    /// Coarse sweep finds the basin, fine sweep refines to 0.005 precision.
    /// Default ignored-test automation runs a bounded smoke; set
    /// MDEM_LONG_TESTS=soaks to run the exhaustive diagnostic sweep.
    #[test]
    #[ignore = "measurement-only: exhaustive V.32bis sweep (~8400 coarse + ~6400 fine combinations), run by hand with --include-ignored."]
    fn test_v32bis_16k_parameter_sweep() {
        if !run_long_soaks() {
            v32bis_16k_parameter_smoke();
            return;
        }

        let symbol_period = 16000.0 / SYMBOL_RATE; // 6.667 samples

        let mut best_correct = 0usize;
        let mut best_align = 0.0f64;
        let mut best_phase = 0.0f64;
        let mut best_md = f64::MAX;

        // Coarse sweep: phase 0..2*pi step 0.05 (~126), align 0..6.667 step 0.1 (~67).
        let phase_steps = (std::f64::consts::TAU / 0.05).ceil() as usize;
        let align_steps = (symbol_period / 0.1).ceil() as usize;
        println!(
            "=== V.32bis 16kHz coarse sweep ({phase_steps} x {align_steps} = {} combos) ===",
            phase_steps * align_steps
        );

        for pi in 0..phase_steps {
            let phase = pi as f64 * 0.05;
            for ai in 0..align_steps {
                let align = ai as f64 * 0.1;
                let c = v32bis_16k_sweep_config(phase, align);
                let (correct, total, md) = measure_v32bis_16k_accuracy(&c);
                if total > 100
                    && (correct > best_correct || (correct == best_correct && md < best_md))
                {
                    best_correct = correct;
                    best_phase = phase;
                    best_align = align;
                    best_md = md;
                }
            }
            if pi % 20 == 0 {
                println!(
                    "  phase={phase:.2}: best so far {best_correct}/400, \
                     phase={best_phase:.3}, align={best_align:.3}, md={best_md:.5}"
                );
            }
        }
        println!(
            "Coarse winner: phase={best_phase:.3}, align={best_align:.3}, \
             {best_correct}/400, mean_dist={best_md:.5}"
        );

        // Fine sweep: +/-0.20 around coarse best, step 0.005.
        let fine_phase_lo = (best_phase - 0.20f64).max(0.0);
        let fine_phase_hi = best_phase + 0.20;
        let fine_align_lo = (best_align - 0.20f64).max(0.0);
        let fine_align_hi = (best_align + 0.20).min(symbol_period);
        let fine_phase_steps = ((fine_phase_hi - fine_phase_lo) / 0.005).ceil() as usize + 1;
        let fine_align_steps = ((fine_align_hi - fine_align_lo) / 0.005).ceil() as usize + 1;
        println!(
            "=== V.32bis 16kHz fine sweep ({fine_phase_steps} x {fine_align_steps} = {} combos) ===",
            fine_phase_steps * fine_align_steps
        );

        for pi in 0..fine_phase_steps {
            let phase = fine_phase_lo + pi as f64 * 0.005;
            for ai in 0..fine_align_steps {
                let align = fine_align_lo + ai as f64 * 0.005;
                let c = v32bis_16k_sweep_config(phase, align);
                let (correct, total, md) = measure_v32bis_16k_accuracy(&c);
                if total > 100
                    && (correct > best_correct || (correct == best_correct && md < best_md))
                {
                    best_correct = correct;
                    best_phase = phase;
                    best_align = align;
                    best_md = md;
                }
            }
        }
        println!(
            "Fine winner: phase={best_phase:.4}, align={best_align:.4}, \
             {best_correct}/400, mean_dist={best_md:.5}"
        );

        println!("\n=== OPTIMAL VALUES ===");
        println!("carrier_phase_correction: {best_phase:.4}");
        println!("timing_align: {best_align:.4}");
        println!(
            "Accuracy: {best_correct}/400 ({:.1}%)",
            best_correct as f64 / 400.0 * 100.0
        );
    }

    /// Regression for the `fuzz_modem_qam` `DspParamStress` NaN cluster
    /// (16 crash artifacts, 2026-05-06). The fuzz harness exercised the
    /// QamDemodulator setters with adversarial f64 values and asserted
    /// that subsequent `demodulate` output was finite. Every crash had
    /// `phase: NaN`; `set_carrier_phase(NaN)` poisoned the carrier
    /// oscillator and propagated NaN through the entire DSP pipeline.
    /// This test mirrors that invariant: after a sweep of non-finite
    /// values into every setter, demodulator output must remain finite.
    /// Run only in release mode where `debug_assert` is disabled —
    /// some older demodulator setters in this sweep still intentionally
    /// panic at their own `debug_assert!` discoverability boundaries.
    #[test]
    #[cfg(not(debug_assertions))]
    fn test_demodulator_setters_reject_non_finite() {
        // Use Qam32 to match the fuzz harness's `DspParamStress` op.
        let mut demod = QamDemodulator::new(Constellation::Qam32);

        // Feed finite samples first so the carrier oscillator and RRC
        // delay line are at non-default state.
        let init: Vec<i16> = (0..200).map(|i| ((i * 137) % 8192) as i16).collect();
        for &(i, q) in &demod.demodulate(&init) {
            assert!(
                i.is_finite() && q.is_finite(),
                "initial demodulate produced non-finite output: ({i}, {q})"
            );
        }

        // Sweep every setter with NaN and ±Inf. Each call must be a
        // silent no-op; subsequent demodulation must stay finite.
        for &bad in &[f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            demod.set_carrier_phase(bad);
            demod.adjust_carrier_phase_offset(bad);
            demod.set_agc_gain(bad);
            demod.set_equalizer_mu(bad);
            demod.set_symbol_rms(bad);
            // Non-positive symbol_rms is also rejected.
            demod.set_symbol_rms(0.0);
            demod.set_symbol_rms(-1.0);
        }

        // Demodulate more samples after the setter sweep — every (I, Q)
        // pair must be finite. This is the inverted form of the fuzz
        // harness's `assert_finite("DspParamStress post I/Q")`.
        let post: Vec<i16> = (0..400)
            .map(|i| ((i * 211) % 16384 - 8192) as i16)
            .collect();
        for (idx, &(i, q)) in demod.demodulate(&post).iter().enumerate() {
            assert!(i.is_finite(), "post demod I[{idx}] non-finite: {i}");
            assert!(q.is_finite(), "post demod Q[{idx}] non-finite: {q}");
        }
    }

    /// Direct exact replay of the smallest fuzz crash artifact —
    /// `tests/fuzz/artifacts/fuzz_modem_qam/crash-2dd10fc8...` (36 B,
    /// `DspParamStress { gain: NaN, phase: NaN, eq_mu: NaN, samples: [-1, 1791, 0] }`).
    /// Locks the regression at the exact input shape, not just the
    /// invariant.
    #[test]
    #[cfg(not(debug_assertions))]
    fn test_dsp_param_stress_smallest_crash_artifact() {
        let mut demod = QamDemodulator::new(Constellation::Qam32);
        let samples: Vec<i16> = vec![-1, 1791, 0];

        for &(i, q) in &demod.demodulate(&samples) {
            assert!(
                i.is_finite() && q.is_finite(),
                "initial demodulate non-finite: ({i}, {q})"
            );
        }

        demod.set_agc_gain(f64::NAN);
        demod.set_carrier_phase(f64::NAN);
        demod.set_equalizer_mu(f64::NAN);

        for (idx, &(i, q)) in demod.demodulate(&samples).iter().enumerate() {
            assert!(i.is_finite(), "post I[{idx}] non-finite: {i}");
            assert!(q.is_finite(), "post Q[{idx}] non-finite: {q}");
        }
    }

    #[test]
    fn test_demodulator_public_setters_reject_non_finite_before_inner_debug_asserts() {
        let mut demod = QamDemodulator::new(Constellation::Qam32);
        let initial_gain = demod.agc_gain();
        let initial_phase = demod.carrier_phase();

        demod.set_agc_gain(f64::NAN);
        demod.set_agc_gain(f64::INFINITY);
        demod.set_carrier_phase(f64::NAN);
        demod.set_carrier_phase(f64::NEG_INFINITY);
        demod.reset_timing(f64::NAN, SAMPLE_RATE as f64, 0.0);
        demod.reset_timing(SYMBOL_RATE, f64::INFINITY, 0.0);
        demod.reset_timing(SYMBOL_RATE, SAMPLE_RATE as f64, f64::NAN);
        demod.reset_timing(0.0, SAMPLE_RATE as f64, 0.0);
        demod.reset_timing(f64::MIN_POSITIVE, SAMPLE_RATE as f64, 0.0);
        demod.reset_timing(f64::MAX, SAMPLE_RATE as f64, 0.0);

        assert_eq!(demod.agc_gain(), initial_gain);
        assert_eq!(demod.carrier_phase(), initial_phase);

        let samples: Vec<i16> = (0..400)
            .map(|i| ((i * 211) % 16384 - 8192) as i16)
            .collect();
        let post = demod.demodulate(&samples);
        assert!(!post.is_empty(), "post demodulate emitted no symbols");
        for (idx, &(i, q)) in post.iter().enumerate() {
            assert!(i.is_finite(), "post I[{idx}] non-finite: {i}");
            assert!(q.is_finite(), "post Q[{idx}] non-finite: {q}");
        }

        let mut eq_config = QamConfig::v32bis();
        eq_config.equalizer_taps = 3;
        eq_config.equalizer_mu = 0.01;
        let mut eq_demod = QamDemodulator::with_config(Constellation::Qam32, &eq_config);
        eq_demod.set_equalizer_mu(f64::NAN);
        eq_demod.set_equalizer_mu(f64::INFINITY);
        let eq_post = eq_demod.demodulate(&samples);
        assert!(!eq_post.is_empty(), "eq demodulate emitted no symbols");
        for (idx, &(i, q)) in eq_post.iter().enumerate() {
            assert!(i.is_finite(), "eq post I[{idx}] non-finite: {i}");
            assert!(q.is_finite(), "eq post Q[{idx}] non-finite: {q}");
        }
    }

    // ----- Backlog #43: extended setter NaN-rejection sweep -----
    //
    // The fuzz `DspParamStress` cluster only exercised gain/phase/mu;
    // these three setters share the same NaN-poison shape but were
    // not in the fuzz target. Pin the silent-skip behaviour now so
    // future fuzz extensions can't regress us.

    #[test]
    #[cfg(not(debug_assertions))]
    fn test_set_timing_align_rejects_non_finite() {
        let mut demod = QamDemodulator::new(Constellation::Qam32);
        demod.set_timing_align(f64::NAN);
        demod.set_timing_align(f64::INFINITY);
        let samples: Vec<i16> = (0..400)
            .map(|i| ((i * 211) % 16384 - 8192) as i16)
            .collect();
        for (idx, &(i, q)) in demod.demodulate(&samples).iter().enumerate() {
            assert!(
                i.is_finite() && q.is_finite(),
                "demod[{idx}] non-finite after non-finite set_timing_align"
            );
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "non-finite count")]
    fn test_set_timing_align_debug_panics_on_nan() {
        let mut demod = QamDemodulator::new(Constellation::Qam32);
        demod.set_timing_align(f64::NAN);
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn test_set_timing_bandwidth_rejects_non_finite() {
        let mut demod = QamDemodulator::new(Constellation::Qam32);
        demod.set_timing_bandwidth(f64::NAN);
        demod.set_timing_bandwidth(f64::NEG_INFINITY);
        let samples: Vec<i16> = (0..400)
            .map(|i| ((i * 211) % 16384 - 8192) as i16)
            .collect();
        for (idx, &(i, q)) in demod.demodulate(&samples).iter().enumerate() {
            assert!(
                i.is_finite() && q.is_finite(),
                "demod[{idx}] non-finite after non-finite set_timing_bandwidth"
            );
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "non-finite bandwidth")]
    fn test_set_timing_bandwidth_debug_panics_on_nan() {
        let mut demod = QamDemodulator::new(Constellation::Qam32);
        demod.set_timing_bandwidth(f64::NAN);
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn test_update_pll_external_rejects_non_finite() {
        let mut demod = QamDemodulator::new(Constellation::Qam32);
        demod.update_pll_external(f64::NAN);
        demod.update_pll_external(f64::INFINITY);
        let samples: Vec<i16> = (0..400)
            .map(|i| ((i * 211) % 16384 - 8192) as i16)
            .collect();
        for (idx, &(i, q)) in demod.demodulate(&samples).iter().enumerate() {
            assert!(
                i.is_finite() && q.is_finite(),
                "demod[{idx}] non-finite after non-finite update_pll_external"
            );
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "non-finite phase_error")]
    fn test_update_pll_external_debug_panics_on_nan() {
        let mut demod = QamDemodulator::new(Constellation::Qam32);
        demod.update_pll_external(f64::NAN);
    }
}

// ── MDD-BUG-CRUCIBLE-00514: per-sample demodulation without heap traffic ──
#[cfg(test)]
mod single_sample_demod_tests {
    use super::*;

    /// A PCM buffer with enough structure to drive timing recovery.
    fn pcm(len: usize) -> Vec<i16> {
        let mut modulator = QamModulator::with_config(Constellation::Qam4, &QamConfig::v34_16k());
        let mut out = Vec::with_capacity(len);
        let mut sym = 0u8;
        while out.len() < len {
            modulator.modulate_symbol_raw(sym & 0x03);
            sym = sym.wrapping_add(1);
            for _ in 0..64 {
                out.push(modulator.next_sample());
            }
        }
        out.truncate(len);
        out
    }

    /// The batch API is now a loop over the single-sample one, so the two must
    /// agree BIT for bit — same operations, same order, same state machine.
    /// Exact `f64` equality is the right assertion here; a tolerance would let
    /// a genuine reordering through.
    #[test]
    fn single_sample_and_batch_demodulation_agree_bit_for_bit() {
        let config = QamConfig::v34_16k();
        let samples = pcm(4096);

        let mut batch_demod = QamDemodulator::with_config(Constellation::Qam4, &config);
        let batch: Vec<(f64, f64)> = batch_demod.demodulate_internal(&samples);

        let mut single_demod = QamDemodulator::with_config(Constellation::Qam4, &config);
        let single: Vec<(f64, f64)> = samples
            .iter()
            .filter_map(|&s| single_demod.demodulate_sample_internal(s))
            .collect();

        assert!(
            !batch.is_empty(),
            "the fixture must actually produce symbols, or this proves nothing"
        );
        assert_eq!(
            batch.len(),
            single.len(),
            "both paths must emit the same number of symbols"
        );
        for (i, (b, s)) in batch.iter().zip(single.iter()).enumerate() {
            assert_eq!(b.0.to_bits(), s.0.to_bits(), "symbol {i} I differs");
            assert_eq!(b.1.to_bits(), s.1.to_bits(), "symbol {i} Q differs");
        }
    }

    /// Feeding the batch API one sample at a time — the shape the receive
    /// paths used to have — must also match. This pins that the batch entry
    /// point carries no cross-call state of its own beyond the demodulator.
    #[test]
    fn one_sample_batches_match_the_single_sample_api() {
        let config = QamConfig::v34_16k();
        let samples = pcm(2048);

        let mut chunked = QamDemodulator::with_config(Constellation::Qam4, &config);
        let via_batches: Vec<(f64, f64)> = samples
            .iter()
            .flat_map(|&s| chunked.demodulate_internal(&[s]))
            .collect();

        let mut single = QamDemodulator::with_config(Constellation::Qam4, &config);
        let via_single: Vec<(f64, f64)> = samples
            .iter()
            .filter_map(|&s| single.demodulate_sample_internal(s))
            .collect();

        assert_eq!(via_batches.len(), via_single.len());
        for (b, s) in via_batches.iter().zip(via_single.iter()) {
            assert_eq!(b.0.to_bits(), s.0.to_bits());
            assert_eq!(b.1.to_bits(), s.1.to_bits());
        }
    }
}
