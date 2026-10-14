//! CPU CRT look: scanlines, aperture mask, vignette and barrel curvature.

use crate::scaler::{par_rows, thread_count};
use std::f32::consts::PI;

const CURVATURE: f32 = 0.03;
const SCANLINE_BASE: f32 = 0.82;
const SCANLINE_AMP: f32 = 0.18;
const MASK_DIM: u8 = 217;
const VIGNETTE_STRENGTH: f32 = 0.6;

const MASK: [(u8, u8, u8); 3] = [
    (255, MASK_DIM, MASK_DIM),
    (MASK_DIM, 255, MASK_DIM),
    (MASK_DIM, MASK_DIM, 255),
];

#[inline(always)]
fn div255(a: u32, b: u32) -> u32 {
    let t = a * b + 128;
    (t + (t >> 8)) >> 8
}

/// Lookup tables and state for the CRT look; rebuilt when the geometry changes.
#[derive(Default)]
pub struct CrtFilter {
    scanline: Vec<u8>,
    vignette_y: Vec<u8>,
    vignette_x: Vec<u8>,
    mask_phase: Vec<u8>,
    mask_active: bool,
    viewport: (usize, usize, usize, usize),
    src_dims: (usize, usize),
    /// Barrel-curvature source index per viewport pixel, or [`OUTSIDE`].
    source_index: Vec<u32>,
    /// Worker threads; 0 picks automatically from the viewport size.
    pub(crate) threads: usize,
}

/// Marker in the source index map for pixels that sample outside the frame.
const OUTSIDE: u32 = u32::MAX;

impl CrtFilter {
    fn rebuild_luts(
        &mut self,
        src_width: usize,
        src_height: usize,
        scaled_width: usize,
        scaled_height: usize,
    ) {
        let period = scaled_height as f32 / src_height as f32;

        self.scanline.resize(scaled_height, 0);
        for dy_rel in 0..scaled_height {
            let val = (SCANLINE_BASE + SCANLINE_AMP * (dy_rel as f32 * 2.0 * PI / period).sin())
                * 255.0
                + 0.5;
            self.scanline[dy_rel] = val.clamp(0.0, 255.0) as u8;
        }

        self.vignette_y.resize(scaled_height, 0);
        for dy_rel in 0..scaled_height {
            let ny = (dy_rel as f32 + 0.5) / scaled_height as f32 - 0.5;
            let val = ((1.0 - VIGNETTE_STRENGTH * ny * ny) * 255.0 + 0.5).clamp(0.0, 255.0);
            self.vignette_y[dy_rel] = val as u8;
        }

        self.vignette_x.resize(scaled_width, 0);
        for dx_rel in 0..scaled_width {
            let nx = (dx_rel as f32 + 0.5) / scaled_width as f32 - 0.5;
            let val = (VIGNETTE_STRENGTH * nx * nx * 255.0 + 0.5).clamp(0.0, 255.0);
            self.vignette_x[dx_rel] = val as u8;
        }

        self.mask_active = scaled_width / src_width >= 3;
        if self.mask_active {
            self.mask_phase.resize(scaled_width, 0);
            let inv_w = 1.0f32 / scaled_width as f32;
            for dx_rel in 0..scaled_width {
                let frac = (dx_rel as f32 * inv_w * src_width as f32).fract();
                self.mask_phase[dx_rel] = (frac * 3.0).min(2.0) as u8;
            }
        } else {
            self.mask_phase.clear();
        }

        // The barrel mapping depends only on geometry, so resolve it once.
        let inv_w = 1.0f32 / scaled_width as f32;
        let inv_h = 1.0f32 / scaled_height as f32;
        let (src_wf, src_hf) = (src_width as f32, src_height as f32);
        self.source_index.clear();
        self.source_index.reserve(scaled_width * scaled_height);
        for dy_rel in 0..scaled_height {
            let ny = 2.0 * (dy_rel as f32 * inv_h) - 1.0;
            let ny_sq = ny * ny;
            for dx_rel in 0..scaled_width {
                let nx = 2.0 * (dx_rel as f32 * inv_w) - 1.0;
                let r2 = nx * nx + ny_sq;
                let factor = 1.0 + CURVATURE * r2;
                let sx = (((nx * factor + 1.0) * 0.5) * src_wf).floor() as i32;
                let sy = (((ny * factor + 1.0) * 0.5) * src_hf).floor() as i32;
                self.source_index.push(
                    if sx < 0 || sy < 0 || sx >= src_width as i32 || sy >= src_height as i32 {
                        OUTSIDE
                    } else {
                        (sy as usize * src_width + sx as usize) as u32
                    },
                );
            }
        }
    }

    /// Apply CRT effects, replacing the nearest-neighbor scaling loop.
    /// Writes directly into the destination framebuffer (window resolution).
    /// Rebuilds LUTs lazily if dimensions have changed.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &mut self,
        src: &[u32],
        src_width: usize,
        src_height: usize,
        dst: &mut [u32],
        dst_width: usize,
        dst_height: usize,
        viewport: (usize, usize, usize, usize),
    ) {
        let (scaled_width, scaled_height, offset_x, offset_y) = viewport;

        if src_width == 0 || src_height == 0 || scaled_width == 0 || scaled_height == 0 {
            return;
        }

        debug_assert!(src.len() >= src_width * src_height);
        debug_assert!(dst.len() >= dst_width * dst_height);
        debug_assert!(offset_x + scaled_width <= dst_width);
        debug_assert!(offset_y + scaled_height <= dst_height);

        let new_src_dims = (src_width, src_height);

        if self.viewport != viewport || self.src_dims != new_src_dims {
            self.rebuild_luts(src_width, src_height, scaled_width, scaled_height);
            self.viewport = viewport;
            self.src_dims = new_src_dims;
        }

        let mask_active = self.mask_active;
        let threads = thread_count(self.threads, scaled_width * scaled_height);
        let tables = &*self;
        par_rows(
            dst,
            dst_width,
            offset_y,
            scaled_height,
            threads,
            |first, rows| {
                for (i, line) in rows.chunks_exact_mut(dst_width).enumerate() {
                    let dy_rel = first + i;
                    let s = tables.scanline[dy_rel] as u32;
                    let vy = tables.vignette_y[dy_rel];
                    let indices = &tables.source_index[dy_rel * scaled_width..][..scaled_width];
                    let out = &mut line[offset_x..][..scaled_width];
                    if mask_active {
                        let columns = tables.vignette_x.iter().zip(&tables.mask_phase);
                        for ((pixel, &index), (&vx, &phase)) in
                            out.iter_mut().zip(indices).zip(columns)
                        {
                            *pixel = if index == OUTSIDE {
                                0
                            } else {
                                let sv = div255(s, vy.saturating_sub(vx) as u32);
                                let (mr, mg, mb) = MASK[phase as usize];
                                let p = src[index as usize];
                                let r = div255(div255((p >> 16) & 0xFF, sv), mr as u32);
                                let g = div255(div255((p >> 8) & 0xFF, sv), mg as u32);
                                let b = div255(div255(p & 0xFF, sv), mb as u32);
                                (r << 16) | (g << 8) | b
                            };
                        }
                    } else {
                        for ((pixel, &index), &vx) in
                            out.iter_mut().zip(indices).zip(&tables.vignette_x)
                        {
                            *pixel = if index == OUTSIDE {
                                0
                            } else {
                                let sv = div255(s, vy.saturating_sub(vx) as u32);
                                let p = src[index as usize];
                                let r = div255((p >> 16) & 0xFF, sv);
                                let g = div255((p >> 8) & 0xFF, sv);
                                let b = div255(p & 0xFF, sv);
                                (r << 16) | (g << 8) | b
                            };
                        }
                    }
                }
            },
        );
    }
}

/// The original per-pixel floating-point implementation, kept as the oracle
/// for the table-driven, multi-threaded [`CrtFilter::apply`].
#[cfg(test)]
pub(crate) fn reference_apply(
    src: &[u32],
    (src_width, src_height): (usize, usize),
    dst: &mut [u32],
    dst_width: usize,
    (scaled_width, scaled_height, offset_x, offset_y): (usize, usize, usize, usize),
) {
    let mut filter = CrtFilter::default();
    filter.rebuild_luts(src_width, src_height, scaled_width, scaled_height);
    let inv_w = 1.0f32 / scaled_width as f32;
    let inv_h = 1.0f32 / scaled_height as f32;
    let src_wf = src_width as f32;
    let src_hf = src_height as f32;
    for dy_rel in 0..scaled_height {
        let ny = 2.0 * (dy_rel as f32 * inv_h) - 1.0;
        let ny_sq = ny * ny;
        let s = filter.scanline[dy_rel] as u32;
        let dst_row_start = (offset_y + dy_rel) * dst_width + offset_x;
        for dx_rel in 0..scaled_width {
            let nx = 2.0 * (dx_rel as f32 * inv_w) - 1.0;
            let r2 = nx * nx + ny_sq;
            let factor = 1.0 + CURVATURE * r2;
            let sx = (((nx * factor + 1.0) * 0.5) * src_wf).floor() as i32;
            let sy = (((ny * factor + 1.0) * 0.5) * src_hf).floor() as i32;
            if sx < 0 || sy < 0 || sx >= src_width as i32 || sy >= src_height as i32 {
                dst[dst_row_start + dx_rel] = 0;
                continue;
            }
            let pixel = src[sy as usize * src_width + sx as usize];
            let (src_r, src_g, src_b) = ((pixel >> 16) & 0xFF, (pixel >> 8) & 0xFF, pixel & 0xFF);
            let v = filter.vignette_y[dy_rel].saturating_sub(filter.vignette_x[dx_rel]) as u32;
            let sv = div255(s, v);
            let (r, g, b) = if filter.mask_active {
                let (mr, mg, mb) = MASK[filter.mask_phase[dx_rel] as usize];
                (
                    div255(div255(src_r, sv), mr as u32),
                    div255(div255(src_g, sv), mg as u32),
                    div255(div255(src_b, sv), mb as u32),
                )
            } else {
                (div255(src_r, sv), div255(src_g, sv), div255(src_b, sv))
            };
            dst[dst_row_start + dx_rel] = (r << 16) | (g << 8) | b;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_div255_identity() {
        assert_eq!(div255(255, 255), 255);
    }

    #[test]
    fn test_div255_zero() {
        for b in 0..=255u32 {
            assert_eq!(div255(0, b), 0);
        }
    }

    #[test]
    fn test_div255_midrange() {
        let expected = (128.0 * 128.0 / 255.0_f64).round() as u32;
        assert_eq!(div255(128, 128), expected);
    }

    #[test]
    fn test_div255_exhaustive() {
        for a in 0..=255u32 {
            for b in 0..=255u32 {
                let expected = (a as f64 * b as f64 / 255.0).round() as u32;
                assert_eq!(
                    div255(a, b),
                    expected,
                    "div255({a}, {b}) = {} but expected {expected}",
                    div255(a, b)
                );
            }
        }
    }

    #[test]
    fn test_full_brightness_passthrough() {
        // Use 4x vertical scale so scanline period = 4.0 and the peak
        // (sin = 1.0) lands exactly on integer row dy=1. Use 2x horizontal
        // scale so the phosphor mask is disabled (all channels get mr=255).
        let src_width = 320;
        let src_height = 100;
        let scaled_width = 640;
        let scaled_height = 400;
        let dst_width = 640;
        let dst_height = 400;

        let src = vec![0x00FFFFFF; src_width * src_height];
        let mut dst = vec![0u32; dst_width * dst_height];
        let viewport = (scaled_width, scaled_height, 0, 0);

        let mut filter = CrtFilter::default();
        filter.apply(
            &src, src_width, src_height, &mut dst, dst_width, dst_height, viewport,
        );

        assert!(!filter.mask_active, "mask should be disabled at 2x");

        // Scanline peak at dy=1: sin(1 * 2PI / 4) = sin(PI/2) = 1.0
        // -> scanline[1] = (0.82 + 0.18) * 255 = 255
        assert_eq!(
            filter.scanline[1], 255,
            "scanline peak should be 255 at dy=1"
        );

        // Check a pixel near the center of that row where vignette is ~255
        let cx = scaled_width / 2;
        // vignette_y[1] will be less than 255 (near top), so use a center row peak.
        // Period=4: peaks at 1, 5, 9, ..., 4k+1. Near center (200): 4*50+1 = 201.
        let peak_row = 201;
        assert_eq!(filter.scanline[peak_row], 255, "scanline peak at dy=201");

        let pixel = dst[peak_row * dst_width + cx];
        assert_eq!(
            pixel, 0x00FFFFFF,
            "center pixel at scanline peak should be full white, got {pixel:#010X}"
        );
    }

    #[test]
    fn test_mask_disabled_below_3x() {
        let mut filter = CrtFilter::default();
        filter.rebuild_luts(320, 200, 640, 400);
        assert!(!filter.mask_active, "mask should be disabled at 2x scale");
    }

    #[test]
    fn test_mask_enabled_at_3x() {
        let mut filter = CrtFilter::default();
        filter.rebuild_luts(320, 200, 960, 600);
        assert!(filter.mask_active, "mask should be enabled at 3x scale");
    }

    #[test]
    fn test_scanline_period_float() {
        let mut filter = CrtFilter::default();
        let src_height = 200;
        let scaled_height = 607;
        filter.rebuild_luts(320, src_height, 960, scaled_height);

        let period = scaled_height as f32 / src_height as f32;
        assert!(
            (period - 3.035).abs() < 0.001,
            "period should be ~3.035, got {period}"
        );

        let period_usize = period.round() as usize;
        let mut found_min = false;
        for dy in 0..scaled_height.min(period_usize * 4) {
            if filter.scanline[dy]
                == *filter.scanline[..scaled_height.min(period_usize * 4)]
                    .iter()
                    .min()
                    .unwrap()
            {
                found_min = true;
                break;
            }
        }
        assert!(
            found_min,
            "should find a scanline minimum within first few periods"
        );
    }

    #[test]
    fn test_vignette_symmetry() {
        let mut filter = CrtFilter::default();
        let h = 600;
        filter.rebuild_luts(320, 200, 960, h);
        for i in 0..h {
            assert_eq!(
                filter.vignette_y[i],
                filter.vignette_y[h - 1 - i],
                "vignette_y asymmetric at row {i}"
            );
        }
    }

    #[test]
    fn test_barrel_center_identity() {
        let src_width = 320;
        let src_height = 200;
        let scaled_width = 960;
        let scaled_height = 600;

        let inv_w = 1.0f32 / scaled_width as f32;
        let inv_h = 1.0f32 / scaled_height as f32;
        let cx = scaled_width / 2;
        let cy = scaled_height / 2;

        let nx = 2.0 * (cx as f32 * inv_w) - 1.0;
        let ny = 2.0 * (cy as f32 * inv_h) - 1.0;
        let r2 = nx * nx + ny * ny;
        let factor = 1.0 + CURVATURE * r2;
        let src_fx = ((nx * factor + 1.0) * 0.5) * src_width as f32;
        let src_fy = ((ny * factor + 1.0) * 0.5) * src_height as f32;

        let expected_sx = src_width as f32 / 2.0;
        let expected_sy = src_height as f32 / 2.0;
        assert!(
            (src_fx - expected_sx).abs() < 0.5,
            "center X: expected ~{expected_sx}, got {src_fx}"
        );
        assert!(
            (src_fy - expected_sy).abs() < 0.5,
            "center Y: expected ~{expected_sy}, got {src_fy}"
        );
    }

    #[test]
    fn test_barrel_corner_displacement() {
        let src_width = 320usize;
        let src_height = 200usize;
        let scaled_width = 960;
        let scaled_height = 600;

        let inv_w = 1.0f32 / scaled_width as f32;
        let inv_h = 1.0f32 / scaled_height as f32;

        let nx = 2.0 * (0.0 * inv_w) - 1.0;
        let ny = 2.0 * (0.0 * inv_h) - 1.0;
        let r2 = nx * nx + ny * ny;
        let factor = 1.0 + CURVATURE * r2;
        let src_fx = ((nx * factor + 1.0) * 0.5) * src_width as f32;
        let src_fy = ((ny * factor + 1.0) * 0.5) * src_height as f32;

        assert!(
            src_fx < 0.0 || src_fy < 0.0,
            "corner (0,0) should map outside source bounds: src_fx={src_fx}, src_fy={src_fy}"
        );
    }
}
