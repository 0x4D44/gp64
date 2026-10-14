//! Shared windowed-sinc resampler kernel computation.
//!
//! Provides the core DSP math for Kaiser-windowed sinc interpolation used by
//! the SC-55 and MU50 synth resamplers. Each synth crate has its own
//! `Resampler` struct with crate-specific input/output conventions, but both
//! delegate kernel computation to this module.
//!
//! The filter uses a 32-tap Kaiser window with beta=8 (~80 dB stopband
//! rejection). When downsampling, the cutoff is set at the output Nyquist
//! to prevent high-frequency energy from folding back as aliasing.

/// Half-length of the sinc kernel (total taps = 2 * SINC_HALF_LEN).
pub const SINC_HALF_LEN: usize = 16;

/// Total taps in the sinc kernel.
pub const SINC_TAPS: usize = SINC_HALF_LEN * 2;

/// Kaiser window shape parameter. beta=8 gives ~80 dB stopband attenuation.
pub const KAISER_BETA: f64 = 8.0;

/// Modified Bessel function of the first kind, order 0 (series expansion).
#[inline]
pub fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0_f64;
    let mut term = 1.0_f64;
    let half_x = x * 0.5;
    for k in 1..25_u32 {
        term *= half_x / k as f64;
        sum += term * term;
    }
    sum
}

/// Evaluate windowed sinc filter at distance `x` from center.
///
/// Returns the Kaiser-windowed sinc value with anti-alias cutoff applied.
/// `cutoff` is the ratio of output Nyquist to internal rate (clamped to 1.0).
/// `i0_inv` is the precomputed `1.0 / bessel_i0(KAISER_BETA)`.
#[inline(always)]
pub fn windowed_sinc(x: f64, cutoff: f64, i0_inv: f64) -> f64 {
    let half = SINC_HALF_LEN as f64;
    let norm_x = x / half;
    if norm_x.abs() > 1.0 {
        return 0.0;
    }

    // Kaiser window
    let w_arg = KAISER_BETA * (1.0 - norm_x * norm_x).max(0.0).sqrt();
    let w = bessel_i0(w_arg) * i0_inv;

    // Sinc with anti-alias cutoff
    if x.abs() < 1e-10 {
        cutoff * w
    } else {
        let px = std::f64::consts::PI * x * cutoff;
        (px.sin() / (std::f64::consts::PI * x)) * w
    }
}

/// Compute windowed-sinc kernel tables as `f64` for a given rate pair and
/// phase count.
///
/// Returns `num_phases` kernels, each with `SINC_TAPS` coefficients.
/// `internal_rate` and `output_rate` determine the anti-alias cutoff.
pub fn compute_sinc_kernels_f64(
    internal_rate: f64,
    output_rate: u32,
    num_phases: usize,
) -> Vec<[f64; SINC_TAPS]> {
    let cutoff = (output_rate as f64 / internal_rate).min(1.0);
    let i0_inv = 1.0 / bessel_i0(KAISER_BETA);
    let mut kernels = vec![[0.0f64; SINC_TAPS]; num_phases];
    for (p, kernel) in kernels.iter_mut().enumerate() {
        let frac = p as f64 / num_phases as f64;
        for (tap, k) in ((-(SINC_HALF_LEN as isize) + 1)..=(SINC_HALF_LEN as isize)).enumerate() {
            let x = k as f64 - frac;
            kernel[tap] = windowed_sinc(x, cutoff, i0_inv);
        }
    }
    kernels
}

/// Compute windowed-sinc kernel tables as `f32` for a given rate pair and
/// phase count.
///
/// Same as [`compute_sinc_kernels_f64`] but truncates each coefficient to
/// `f32` for crates that store kernels in single precision.
pub fn compute_sinc_kernels_f32(
    internal_rate: f64,
    output_rate: u32,
    num_phases: usize,
) -> Vec<[f32; SINC_TAPS]> {
    let cutoff = (output_rate as f64 / internal_rate).min(1.0);
    let i0_inv = 1.0 / bessel_i0(KAISER_BETA);
    let mut kernels = vec![[0.0f32; SINC_TAPS]; num_phases];
    for (p, kernel) in kernels.iter_mut().enumerate() {
        let frac = p as f64 / num_phases as f64;
        for (tap, k) in ((-(SINC_HALF_LEN as isize) + 1)..=(SINC_HALF_LEN as isize)).enumerate() {
            let x = k as f64 - frac;
            kernel[tap] = windowed_sinc(x, cutoff, i0_inv) as f32;
        }
    }
    kernels
}

/// Greatest common divisor (Euclidean algorithm). Returns at least 1.
pub fn gcd_u32(a: u32, b: u32) -> u32 {
    // The gcd never exceeds either input, so the narrowing is exact.
    gcd_u64(u64::from(a), u64::from(b)) as u32
}

/// Greatest common divisor (Euclidean algorithm). Returns at least 1.
///
/// The 64-bit form is what a rate ratio with a clock numerator needs: the
/// SC-55's is `48_000_000 / (725 * output_rate)`, whose denominator leaves
/// 32 bits at 5.9 MHz.
pub fn gcd_u64(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a.max(1)
}

#[cfg(test)]
#[cfg_attr(any(coverage, coverage_nightly), coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn bessel_i0_at_zero() {
        assert!((bessel_i0(0.0) - 1.0).abs() < 1e-15);
    }

    #[test]
    fn bessel_i0_at_eight() {
        // I0(8) ~ 427.564
        let val = bessel_i0(8.0);
        assert!((val - 427.564).abs() < 1.0, "I0(8)={val}");
    }

    #[test]
    fn windowed_sinc_at_zero() {
        let i0_inv = 1.0 / bessel_i0(KAISER_BETA);
        let val = windowed_sinc(0.0, 1.0, i0_inv);
        assert!((val - 1.0).abs() < 1e-10, "sinc(0)={val}");
    }

    #[test]
    fn windowed_sinc_outside_support() {
        let i0_inv = 1.0 / bessel_i0(KAISER_BETA);
        assert_eq!(windowed_sinc(SINC_HALF_LEN as f64 + 1.0, 1.0, i0_inv), 0.0);
    }

    #[test]
    fn gcd_basic() {
        assert_eq!(gcd_u32(48000, 44100), 300);
        assert_eq!(gcd_u32(44100, 48000), 300);
        assert_eq!(gcd_u32(0, 5), 5);
        assert_eq!(gcd_u32(0, 0), 1);
    }

    #[test]
    fn kernel_tables_f64_shape() {
        let kernels = compute_sinc_kernels_f64(66207.0, 44100, 10);
        assert_eq!(kernels.len(), 10);
        assert_eq!(kernels[0].len(), SINC_TAPS);
    }

    #[test]
    fn kernel_tables_f32_shape() {
        let kernels = compute_sinc_kernels_f32(44100.0, 48000, 10);
        assert_eq!(kernels.len(), 10);
        assert_eq!(kernels[0].len(), SINC_TAPS);
    }

    #[test]
    fn kernel_phase_zero_is_delta_like() {
        // At phase 0 (frac=0), the kernel should peak at the center tap
        let kernels = compute_sinc_kernels_f64(44100.0, 44100, 8);
        let center_tap = SINC_HALF_LEN - 1; // tap for k=0
        let peak = kernels[0][center_tap];
        assert!(peak > 0.9, "center tap should be near 1.0, got {peak}");
    }

    #[test]
    fn gcd_coprime() {
        assert_eq!(gcd_u32(7, 13), 1);
    }

    #[test]
    fn gcd_same() {
        assert_eq!(gcd_u32(42, 42), 42);
    }

    #[test]
    fn gcd_one_input_is_one() {
        assert_eq!(gcd_u32(1, 100), 1);
        assert_eq!(gcd_u32(100, 1), 1);
    }

    #[test]
    fn gcd_u64_handles_values_past_32_bits() {
        // The SC-55 ratio 48_000_000 / (725 * 44_100), whose denominator is
        // 31_972_500 - and the same pair scaled past u32 to prove no
        // truncation on the way in.
        assert_eq!(gcd_u64(48_000_000, 31_972_500), 7_500);
        assert_eq!(gcd_u64(48_000_000 << 8, 31_972_500 << 8), 7_500 << 8);
        assert_eq!(gcd_u64(0, 0), 1);
    }

    #[test]
    fn windowed_sinc_at_half_offset() {
        let i0_inv = 1.0 / bessel_i0(KAISER_BETA);
        let val = windowed_sinc(0.5, 1.0, i0_inv);
        // Should be positive and less than 1
        assert!(val > 0.0 && val < 1.0, "sinc(0.5) = {val}");
    }

    #[test]
    fn windowed_sinc_with_cutoff() {
        let i0_inv = 1.0 / bessel_i0(KAISER_BETA);
        let val = windowed_sinc(0.0, 0.5, i0_inv);
        // At center with cutoff=0.5, should be 0.5
        assert!((val - 0.5).abs() < 1e-10, "sinc(0, cutoff=0.5) = {val}");
    }

    #[test]
    fn windowed_sinc_negative_x() {
        let i0_inv = 1.0 / bessel_i0(KAISER_BETA);
        let pos = windowed_sinc(1.0, 1.0, i0_inv);
        let neg = windowed_sinc(-1.0, 1.0, i0_inv);
        // sinc is an even function (for real-valued windowed sinc)
        assert!((pos - neg).abs() < 1e-10, "pos={pos}, neg={neg}");
    }

    #[test]
    fn kernel_f32_matches_f64_approximately() {
        let f64_kernels = compute_sinc_kernels_f64(44100.0, 48000, 4);
        let f32_kernels = compute_sinc_kernels_f32(44100.0, 48000, 4);
        for (f64_k, f32_k) in f64_kernels.iter().zip(f32_kernels.iter()) {
            for (a, b) in f64_k.iter().zip(f32_k.iter()) {
                assert!((*a as f32 - *b).abs() < 1e-6, "f64={a}, f32={b}");
            }
        }
    }

    #[test]
    fn kernel_downsampling_has_lower_amplitude() {
        // When downsampling (output < internal), cutoff < 1 reduces amplitude
        let up = compute_sinc_kernels_f64(44100.0, 44100, 1);
        let down = compute_sinc_kernels_f64(96000.0, 44100, 1);
        let up_peak = up[0][SINC_HALF_LEN - 1];
        let down_peak = down[0][SINC_HALF_LEN - 1];
        assert!(
            down_peak < up_peak,
            "downsampling peak {down_peak} should be < {up_peak}"
        );
    }

    #[test]
    fn constants_consistent() {
        assert_eq!(SINC_TAPS, SINC_HALF_LEN * 2);
        assert_eq!(SINC_TAPS, 32);
        assert_eq!(SINC_HALF_LEN, 16);
    }
}
