//! Modem DSP parameter profiles for different transport media.
//!
//! A `DspProfile` captures all DSP tuning parameters that differ between
//! a clean TCP channel and a real PSTN telephone line. Selected once at
//! modem construction time; individual parameters are read during
//! component initialization.

use crate::qam::QamConfig;

/// Modem DSP parameter profile, selected at construction time.
/// TCP profile: optimized for clean channel, shorter training.
/// PSTN profile: tuned for real telephone line impairments.
#[derive(Debug, Clone, Copy)]
pub struct DspProfile {
    /// Number of taps for the T-spaced adaptive equalizer.
    pub equalizer_taps: usize,
    /// LMS step size for the adaptive equalizer.
    pub equalizer_mu: f64,
    /// Use NLMS (normalized LMS) instead of plain LMS.
    pub equalizer_nlms: bool,
    /// PLL loop bandwidth in Hz (used for initial acquisition).
    pub pll_bandwidth_hz: f64,
    /// Enable decision-directed PLL carrier tracking.
    pub pll_tracking: bool,
    /// Gardner timing recovery loop bandwidth (0.0 = disabled).
    pub timing_bandwidth: f64,
    /// AGC attack rate (fast, prevents clipping).
    pub agc_attack_alpha: f64,
    /// AGC decay rate (slow, avoids pumping).
    pub agc_decay_alpha: f64,
    /// Echo canceller tail length in samples at 16 kHz (0 = disabled).
    pub echo_tail_samples: usize,
    /// V.34 pre-emphasis filter index (0 = flat, 1-9 = V.34 Table 4).
    pub preemphasis_index: u8,
    /// Generate DTMF dialing tones before call setup.
    pub dtmf_dialing: bool,
    /// V.34 training phase duration in ticks.
    pub v34_training_ticks: u32,
    /// V.34 line probing duration in ticks.
    pub v34_probing_ticks: u32,
}

impl DspProfile {
    /// TCP profile: current defaults, no echo canceller, PLL/timing tracking off.
    pub fn tcp() -> Self {
        Self {
            equalizer_taps: 11,
            equalizer_mu: 0.03,
            equalizer_nlms: false,
            pll_bandwidth_hz: 40.0,
            pll_tracking: false,
            timing_bandwidth: 0.0,
            agc_attack_alpha: 0.002,
            agc_decay_alpha: 0.002,
            echo_tail_samples: 0,
            preemphasis_index: 0,
            dtmf_dialing: true,
            v34_training_ticks: 50,
            v34_probing_ticks: 147,
        }
    }

    /// PSTN profile: tuned for real telephone line.
    ///
    /// Tracking-loop values (to be validated in Phase 1c closed-loop tests):
    /// - `pll_bandwidth_hz: 50.0` — gut-feel acquisition bandwidth. At the
    ///   V.34 symbol rate this gives ωn ≈ 0.039 rad/sample with ζ=√2/2,
    ///   wide enough to pull in realistic crystal + VoIP resampler offsets
    ///   on the order of ±50 Hz. Will be narrowed at runtime for steady
    ///   state; retuned in Phase 1c if closed-loop BER shows lock trouble.
    /// - `timing_bandwidth: 0.007` — normalized to symbol rate. Sits in the
    ///   middle of spandsp's typical 0.005–0.02 range for Gardner TED.
    ///   Will be retuned in Phase 1c if closed-loop BER shows timing jitter.
    /// - `echo_tail_samples: 2048` — matches `preemphasis.rs` echo
    ///   canceller default (128 ms at 16 kHz), covering worst-case PSTN
    ///   hybrid echo paths.
    pub fn pstn() -> Self {
        Self {
            equalizer_taps: 21,
            equalizer_mu: 0.005,
            equalizer_nlms: true,
            pll_bandwidth_hz: 50.0, // Wide for acquisition, narrowed at runtime
            pll_tracking: true,
            timing_bandwidth: 0.007,
            agc_attack_alpha: 0.01,
            agc_decay_alpha: 0.001,
            echo_tail_samples: 2048,
            preemphasis_index: 0, // Selected during training
            dtmf_dialing: true,
            v34_training_ticks: 80,
            v34_probing_ticks: 200,
        }
    }

    /// Overlay the profile's tracking-loop parameters onto a `QamConfig`.
    ///
    /// Only fields that differ between TCP and PSTN tuning are touched:
    /// Gardner timing bandwidth, PLL loop bandwidth, and decision-directed
    /// PLL tracking. Carrier-phase correction, timing pre-alignment, RRC
    /// taps, and symbol/sample rates are preserved — those are tied to the
    /// modulation standard (V.34, V.32bis, ...) and must stay correct.
    ///
    /// Intended use: create the standard `QamConfig` for the active modem
    /// (e.g. `QamConfig::v34_16k()`), then call this to gate the tracking
    /// loops on the active profile. TCP profile freezes timing/PLL
    /// (bandwidth 0.0), matching the current hard-coded defaults; PSTN
    /// profile enables both loops so the demodulator can follow crystal
    /// drift and VoIP resampler offset on a real line.
    pub fn apply_to_qam_config(&self, cfg: &mut QamConfig) {
        cfg.timing_bandwidth = self.timing_bandwidth;
        cfg.pll_bandwidth = self.pll_bandwidth_hz;
        cfg.pll_tracking = self.pll_tracking;

        // Phase 1c.2: plumb the AGC attack/decay asymmetry and the equalizer
        // NLMS / tap-count / mu from the profile into the demodulator's
        // `QamConfig`. These are *structural* overlays: only profiles that
        // differ from the `QamConfig::*_16k()` defaults actually alter
        // behaviour, so the TCP profile stays a no-op.
        //
        // Equalizer mu/taps and AGC rates remain placeholder values from
        // Phase 1a.5 awaiting a closed-loop BER tuning pass in Stage 2
        // (HLD §5). Wiring is complete; numeric tuning is not.
        if self.equalizer_taps > 0 {
            cfg.equalizer_taps = self.equalizer_taps;
            cfg.equalizer_mu = self.equalizer_mu;
        }
        cfg.equalizer_nlms = self.equalizer_nlms;
        // Only install asymmetric AGC when the profile actually asks for
        // asymmetry — this keeps the TCP profile (attack == decay) on the
        // symmetric `agc_alpha` path that all existing tests depend on.
        if (self.agc_attack_alpha - self.agc_decay_alpha).abs() > f64::EPSILON {
            cfg.agc_attack_alpha = Some(self.agc_attack_alpha);
            cfg.agc_decay_alpha = Some(self.agc_decay_alpha);
        }

        // When the TCP profile is overlaid on the default v34_16k() config,
        // the operation must be a byte-for-byte no-op. If these fall out of
        // sync, TCP users will see a silent behaviour change. The
        // test_tcp_profile_back_to_back_unchanged test also guards this,
        // but this debug_assert catches the case where someone overlays
        // the TCP profile on an already-mutated cfg.
        #[cfg(debug_assertions)]
        if self.is_tcp_profile() {
            let default = QamConfig::v34_16k();
            debug_assert_eq!(
                cfg.timing_bandwidth, default.timing_bandwidth,
                "TCP profile overlay must leave timing_bandwidth matching v34_16k() default"
            );
            debug_assert_eq!(
                cfg.pll_bandwidth, default.pll_bandwidth,
                "TCP profile overlay must leave pll_bandwidth matching v34_16k() default"
            );
            debug_assert_eq!(
                cfg.pll_tracking, default.pll_tracking,
                "TCP profile overlay must leave pll_tracking matching v34_16k() default"
            );
            debug_assert_eq!(
                cfg.equalizer_nlms, default.equalizer_nlms,
                "TCP profile overlay must leave equalizer_nlms matching v34_16k() default"
            );
            debug_assert!(
                cfg.agc_attack_alpha.is_none() && cfg.agc_decay_alpha.is_none(),
                "TCP profile overlay must keep AGC symmetric (attack/decay = None)"
            );
        }
    }

    /// Whether this profile has the TCP tracking-loop tuning (frozen
    /// timing, DD-PLL off, 40 Hz PLL bandwidth, symmetric AGC, plain LMS).
    /// Used by `apply_to_qam_config` to gate the TCP no-op invariant check.
    #[cfg(debug_assertions)]
    pub(crate) fn is_tcp_profile(&self) -> bool {
        let tcp = Self::tcp();
        self.timing_bandwidth == tcp.timing_bandwidth
            && self.pll_bandwidth_hz == tcp.pll_bandwidth_hz
            && self.pll_tracking == tcp.pll_tracking
            && self.equalizer_nlms == tcp.equalizer_nlms
            && (self.agc_attack_alpha - self.agc_decay_alpha).abs() <= f64::EPSILON
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn test_tcp_profile_defaults() {
        let tcp = DspProfile::tcp();
        // TCP profile should match current codebase defaults.
        assert_eq!(tcp.equalizer_taps, 11);
        assert!((tcp.equalizer_mu - 0.03).abs() < 1e-10);
        assert!(!tcp.equalizer_nlms);
        assert!((tcp.pll_bandwidth_hz - 40.0).abs() < 1e-10);
        assert!(!tcp.pll_tracking);
        assert!((tcp.timing_bandwidth - 0.0).abs() < 1e-10);
        assert!((tcp.agc_attack_alpha - 0.002).abs() < 1e-10);
        assert!((tcp.agc_decay_alpha - 0.002).abs() < 1e-10);
        assert_eq!(tcp.echo_tail_samples, 0);
        assert_eq!(tcp.preemphasis_index, 0);
        assert!(tcp.dtmf_dialing);
        assert_eq!(tcp.v34_training_ticks, 50);
        assert_eq!(tcp.v34_probing_ticks, 147);
    }

    #[test]
    fn test_pstn_profile_values() {
        let pstn = DspProfile::pstn();
        // PSTN profile should have non-zero recovery loops and echo canceller.
        assert!(pstn.equalizer_taps > 11, "PSTN needs more equalizer taps");
        assert!(pstn.equalizer_nlms, "PSTN should use NLMS");
        assert!(pstn.pll_tracking, "PSTN needs PLL tracking");
        assert!(
            pstn.timing_bandwidth > 0.0,
            "PSTN needs active timing recovery"
        );
        assert!(pstn.echo_tail_samples > 0, "PSTN needs echo canceller");
        assert!(pstn.dtmf_dialing, "PSTN needs real DTMF dialing");
        assert!(
            pstn.agc_attack_alpha > pstn.agc_decay_alpha,
            "PSTN AGC attack should be faster than decay"
        );
    }

    #[test]
    fn test_apply_tcp_preserves_frozen_loops() {
        // TCP profile overlay on v34_16k must leave timing/PLL frozen,
        // exactly matching the current hard-coded defaults.
        let mut cfg = QamConfig::v34_16k();
        DspProfile::tcp().apply_to_qam_config(&mut cfg);
        assert!(
            (cfg.timing_bandwidth - 0.0).abs() < 1e-12,
            "TCP profile must keep timing_bandwidth=0.0 (was {})",
            cfg.timing_bandwidth
        );
        assert!(
            !cfg.pll_tracking,
            "TCP profile must keep decision-directed PLL tracking off"
        );
    }

    #[test]
    fn test_apply_pstn_enables_tracking_loops() {
        // PSTN profile overlay must enable Gardner timing and DD-PLL.
        // Modulation-standard fields (carrier, symbol rate, RRC geometry)
        // must remain untouched. Equalizer / AGC tuning *is* intentionally
        // overlaid — see `test_apply_pstn_wires_equalizer_and_agc_fields`.
        let mut cfg = QamConfig::v34_16k();
        let original_carrier_phase = cfg.carrier_phase_correction;
        let original_symbol_rate = cfg.symbol_rate;
        let original_rrc_taps = cfg.rrc_taps;

        DspProfile::pstn().apply_to_qam_config(&mut cfg);

        assert!(
            cfg.timing_bandwidth > 0.0,
            "PSTN profile must enable Gardner timing (got {})",
            cfg.timing_bandwidth
        );
        assert!(
            cfg.pll_tracking,
            "PSTN profile must enable decision-directed PLL tracking"
        );
        assert!(
            cfg.pll_bandwidth > 0.0,
            "PSTN profile must set a non-zero PLL bandwidth"
        );

        // Modulation-standard fields must not be perturbed.
        assert_eq!(cfg.carrier_phase_correction, original_carrier_phase);
        assert_eq!(cfg.symbol_rate, original_symbol_rate);
        assert_eq!(cfg.rrc_taps, original_rrc_taps);
    }

    #[test]
    fn test_apply_pstn_wires_equalizer_and_agc_fields() {
        // Phase 1c.2: PSTN overlay must plumb NLMS, tap count, equalizer mu,
        // and asymmetric AGC attack/decay into the QamConfig so downstream
        // `QamDemodulator::with_config` actually sees the profile's values.
        let mut cfg = QamConfig::v34_16k();
        let pstn = DspProfile::pstn();
        pstn.apply_to_qam_config(&mut cfg);

        assert!(
            cfg.equalizer_nlms,
            "PSTN overlay must enable equalizer_nlms in QamConfig"
        );
        assert_eq!(
            cfg.equalizer_taps, pstn.equalizer_taps,
            "PSTN overlay must install the profile's equalizer tap count"
        );
        assert!(
            (cfg.equalizer_mu - pstn.equalizer_mu).abs() < 1e-12,
            "PSTN overlay must install the profile's equalizer mu"
        );
        assert_eq!(
            cfg.agc_attack_alpha,
            Some(pstn.agc_attack_alpha),
            "PSTN overlay must install the asymmetric AGC attack rate"
        );
        assert_eq!(
            cfg.agc_decay_alpha,
            Some(pstn.agc_decay_alpha),
            "PSTN overlay must install the asymmetric AGC decay rate"
        );
    }

    #[test]
    fn test_apply_tcp_preserves_symmetric_agc_and_plain_lms() {
        // TCP overlay on v34_16k must keep plain LMS and symmetric AGC
        // (attack/decay = None), matching the pre-Phase-1c baseline exactly.
        let mut cfg = QamConfig::v34_16k();
        DspProfile::tcp().apply_to_qam_config(&mut cfg);
        assert!(
            !cfg.equalizer_nlms,
            "TCP profile must leave equalizer_nlms=false"
        );
        assert_eq!(
            cfg.agc_attack_alpha, None,
            "TCP profile must keep AGC attack symmetric (None)"
        );
        assert_eq!(
            cfg.agc_decay_alpha, None,
            "TCP profile must keep AGC decay symmetric (None)"
        );
    }

    #[test]
    fn test_profiles_differ() {
        let tcp = DspProfile::tcp();
        let pstn = DspProfile::pstn();

        // Key fields that must differ between profiles.
        assert_ne!(tcp.equalizer_taps, pstn.equalizer_taps);
        assert_ne!(tcp.equalizer_nlms, pstn.equalizer_nlms);
        assert_ne!(tcp.pll_tracking, pstn.pll_tracking);
        assert!(
            (tcp.timing_bandwidth - pstn.timing_bandwidth).abs() > 1e-10,
            "timing_bandwidth must differ"
        );
        assert_ne!(tcp.echo_tail_samples, pstn.echo_tail_samples);
        assert_eq!(tcp.dtmf_dialing, pstn.dtmf_dialing);
        assert!(
            (tcp.equalizer_mu - pstn.equalizer_mu).abs() > 1e-10,
            "equalizer_mu must differ"
        );
    }
}
