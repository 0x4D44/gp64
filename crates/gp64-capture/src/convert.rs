//! Pixel format conversion utilities.

/// Planar 8-bit I420 (YUV 4:2:0) frame buffers.
///
/// Held across frames by the encoder thread so a capture session performs no
/// per-frame plane allocation. [`convert_0rgb_to_i420_into`] resizes the
/// planes on the first frame (and on any resolution change) and thereafter
/// overwrites them in place.
#[derive(Default)]
pub struct I420Planes {
    /// Luma plane, `width * height` bytes, tightly packed.
    pub y: Vec<u8>,
    /// Cb plane, `(width / 2) * (height / 2)` bytes, tightly packed.
    pub u: Vec<u8>,
    /// Cr plane, `(width / 2) * (height / 2)` bytes, tightly packed.
    pub v: Vec<u8>,
    /// Luma width the planes are currently sized for.
    pub width: u32,
    /// Luma height the planes are currently sized for.
    pub height: u32,
}

// BT.601 studio-range (Y 16..235, C 16..240) coefficients in 16-bit fixed
// point, rounded to nearest. This is the colour space rvp9-encoder declares in
// the VP9 uncompressed header (`ColorSpace::Bt601` + `ColorRange::Studio`), so
// a player converts them back to the original RGB — the same space openh264
// used before the VP9 switch, which keeps captures' black/white levels
// unchanged.
//
// 16-bit (rather than openh264's 8-bit) precision is what makes the primaries
// land on their canonical values: openh264's rounded-to-8-bit matrix put full
// red at Y=81.7, which its truncating cast turned into 81 by luck and a
// rounding cast would have turned into 82. Here the arithmetic is exact enough
// that the choice does not arise. With 8-bit RGB input the results are always
// inside [16, 240], so no clamp is needed.
const Y_R: i32 = 16829; // 0.256788
const Y_G: i32 = 33039; // 0.504129
const Y_B: i32 = 6416; //  0.097906
const U_R: i32 = -9714; // -0.148223
const U_G: i32 = -19070; // -0.290993
const U_B: i32 = 28784; // 0.439216
const V_R: i32 = 28784; // 0.439216
const V_G: i32 = -24103; // -0.367788
const V_B: i32 = -4681; // -0.071427
/// Half a unit in 16-bit fixed point — the round-to-nearest bias.
const HALF: i32 = 1 << 15;

#[inline]
fn rgb_to_y(r: i32, g: i32, b: i32) -> u8 {
    (((Y_R * r + Y_G * g + Y_B * b + HALF) >> 16) + 16) as u8
}

#[inline]
fn rgb_to_u(r: i32, g: i32, b: i32) -> u8 {
    (((U_R * r + U_G * g + U_B * b + HALF) >> 16) + 128) as u8
}

#[inline]
fn rgb_to_v(r: i32, g: i32, b: i32) -> u8 {
    (((V_R * r + V_G * g + V_B * b + HALF) >> 16) + 128) as u8
}

#[inline]
fn unpack(pixel: u32) -> (i32, i32, i32) {
    (
        ((pixel >> 16) & 0xFF) as i32,
        ((pixel >> 8) & 0xFF) as i32,
        (pixel & 0xFF) as i32,
    )
}

/// Convert a 0RGB (`0x00RRGGBB`) frame to planar I420, padding to
/// `dst_width` × `dst_height`.
///
/// The destination may be larger than the source: VP9 codes whole 64-pixel
/// superblock columns, so [`crate::encoder`] rounds the coded width up to a
/// multiple of 64 and the coded height up to an even number. The pad region is
/// filled by replicating the source's last column and last row rather than
/// with black — a flat extension of the picture costs the encoder almost
/// nothing, whereas a hard black edge spends bits on a high-contrast border
/// nobody sees. Viewers never see the padding at all: the muxer emits
/// Matroska `PixelCropRight` / `PixelCropBottom` so players display exactly
/// `src_width` × `src_height`.
///
/// # Panics
///
/// Panics if `dst_width`/`dst_height` are odd, are smaller than the source, or
/// if `src` is shorter than `src_width * src_height`.
pub fn convert_0rgb_to_i420_into(
    src: &[u32],
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
    planes: &mut I420Planes,
) {
    let sw = src_width as usize;
    let sh = src_height as usize;
    let dw = dst_width as usize;
    let dh = dst_height as usize;

    assert!(sw > 0 && sh > 0, "empty source frame");
    assert!(dw >= sw && dh >= sh, "destination smaller than source");
    assert!(
        dw.is_multiple_of(2) && dh.is_multiple_of(2),
        "I420 needs even destination dims"
    );
    assert!(src.len() >= sw * sh, "source shorter than its dimensions");

    let cw = dw / 2;
    let ch = dh / 2;

    if planes.width != dst_width || planes.height != dst_height {
        planes.y.resize(dw * dh, 0);
        planes.u.resize(cw * ch, 0);
        planes.v.resize(cw * ch, 0);
        planes.width = dst_width;
        planes.height = dst_height;
    }

    // --- Luma: one pass over the real rows, then replicate the last one. ---
    for sy in 0..sh {
        let src_row = &src[sy * sw..sy * sw + sw];
        let dst_row = &mut planes.y[sy * dw..sy * dw + dw];
        for (dst, &pixel) in dst_row.iter_mut().zip(src_row.iter()) {
            let (r, g, b) = unpack(pixel);
            *dst = rgb_to_y(r, g, b);
        }
        // Replicate the right-most luma sample across the pad columns.
        let edge = dst_row[sw - 1];
        dst_row[sw..].fill(edge);
    }
    // Replicate the bottom row across the pad rows.
    let last_row = (sh - 1) * dw;
    for dy in sh..dh {
        planes.y.copy_within(last_row..last_row + dw, dy * dw);
    }

    // --- Chroma: box-average each 2x2 luma block, clamped at the edges. ---
    // Averaging RGB before the matrix (rather than averaging the converted
    // chroma) is what openh264 did, and is the conventional 4:2:0 downsample.
    for cy in 0..ch {
        let sy0 = (cy * 2).min(sh - 1);
        let sy1 = (cy * 2 + 1).min(sh - 1);
        let row0 = &src[sy0 * sw..sy0 * sw + sw];
        let row1 = &src[sy1 * sw..sy1 * sw + sw];
        for cx in 0..cw {
            let sx0 = (cx * 2).min(sw - 1);
            let sx1 = (cx * 2 + 1).min(sw - 1);
            let (r0, g0, b0) = unpack(row0[sx0]);
            let (r1, g1, b1) = unpack(row0[sx1]);
            let (r2, g2, b2) = unpack(row1[sx0]);
            let (r3, g3, b3) = unpack(row1[sx1]);
            let r = (r0 + r1 + r2 + r3) / 4;
            let g = (g0 + g1 + g2 + g3) / 4;
            let b = (b0 + b1 + b2 + b3) / 4;
            planes.u[cy * cw + cx] = rgb_to_u(r, g, b);
            planes.v[cy * cw + cx] = rgb_to_v(r, g, b);
        }
    }
}

/// Nearest-neighbour integer upscale of a 0RGB u32 pixel buffer.
///
/// Writes `scale × scale` replicated pixels into `out`. A `scale` of 1 copies
/// the input as-is. Returns the output dimensions `(width, height)`.
///
/// Used before VP9 encoding for small sources (DOS Mode 13h, etc.) so the
/// file is natively at a size players won't interpolate, and so 4:2:0 chroma
/// subsampling cannot bleed between source pixels.
pub fn nearest_upscale_0rgb_into(
    src: &[u32],
    width: u32,
    height: u32,
    scale: u32,
    out: &mut Vec<u32>,
) -> (u32, u32) {
    let sw = width as usize;
    let sh = height as usize;
    let s = scale.max(1) as usize;

    if s == 1 {
        out.clear();
        out.extend_from_slice(src);
        return (width, height);
    }

    let dw = sw * s;
    let dh = sh * s;
    out.clear();
    out.resize(dw * dh, 0);

    for y in 0..sh {
        let src_row = &src[y * sw..(y + 1) * sw];
        let first_row_start = (y * s) * dw;
        // Build the first scaled row for this source row (horizontal replicate).
        for x in 0..sw {
            let p = src_row[x];
            let col_start = first_row_start + x * s;
            for k in 0..s {
                out[col_start + k] = p;
            }
        }
        // Vertically replicate that row `s - 1` more times via memcpy.
        for dy in 1..s {
            let dst_start = (y * s + dy) * dw;
            out.copy_within(first_row_start..first_row_start + dw, dst_start);
        }
    }

    (dw as u32, dh as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Convert a single pixel at 2x2 (the smallest legal I420 frame) and
    /// return `(Y, U, V)`.
    fn one_pixel(argb: u32) -> (u8, u8, u8) {
        let mut planes = I420Planes::default();
        convert_0rgb_to_i420_into(&[argb; 4], 2, 2, 2, 2, &mut planes);
        (planes.y[0], planes.u[0], planes.v[0])
    }

    #[test]
    fn black_maps_to_studio_range_floor() {
        // Studio range puts black at Y=16, chroma at the 128 midpoint.
        assert_eq!(one_pixel(0x0000_0000), (16, 128, 128));
    }

    #[test]
    fn white_maps_to_studio_range_ceiling() {
        assert_eq!(one_pixel(0x00FF_FFFF), (235, 128, 128));
    }

    #[test]
    fn primaries_land_on_the_bt601_studio_values() {
        // The canonical BT.601 studio-range triples for full-intensity
        // primaries. These pin the matrix itself: a swapped coefficient or a
        // full-range matrix moves every one of them.
        assert_eq!(one_pixel(0x00FF_0000), (81, 90, 240)); // red
        assert_eq!(one_pixel(0x0000_FF00), (145, 54, 34)); // green
        assert_eq!(one_pixel(0x0000_00FF), (41, 240, 110)); // blue
    }

    #[test]
    fn high_byte_is_ignored() {
        // The top byte is the "0" in 0RGB and must not reach the matrix.
        assert_eq!(one_pixel(0xFF00_0000), one_pixel(0x0000_0000));
    }

    #[test]
    fn plane_sizes_follow_the_destination_dimensions() {
        let mut planes = I420Planes::default();
        let src = vec![0x0012_3456u32; 320 * 200];
        convert_0rgb_to_i420_into(&src, 320, 200, 320, 200, &mut planes);
        assert_eq!(planes.y.len(), 320 * 200);
        assert_eq!(planes.u.len(), 160 * 100);
        assert_eq!(planes.v.len(), 160 * 100);
    }

    #[test]
    fn padding_replicates_the_edges_rather_than_blacking_them() {
        // A 2x2 white source padded to 8x4: every padded luma sample must be
        // the replicated white edge, not the studio-black floor of 16. Blacking
        // the pad would show up here as a 16 in the tail of each row.
        let mut planes = I420Planes::default();
        convert_0rgb_to_i420_into(&[0x00FF_FFFFu32; 4], 2, 2, 8, 4, &mut planes);
        assert!(
            planes.y.iter().all(|&y| y == 235),
            "pad columns/rows must replicate the source edge"
        );
        assert!(planes.u.iter().all(|&u| u == 128));
        assert!(planes.v.iter().all(|&v| v == 128));
    }

    #[test]
    fn padding_carries_the_edge_column_not_the_frame_average() {
        // Left half black, right half white, padded 4 columns to the right.
        // The pad must continue the WHITE right edge.
        let src = vec![
            0x0000_0000u32,
            0x0000_0000,
            0x00FF_FFFF,
            0x00FF_FFFF, // row 0
            0x0000_0000,
            0x0000_0000,
            0x00FF_FFFF,
            0x00FF_FFFF, // row 1
        ];
        let mut planes = I420Planes::default();
        convert_0rgb_to_i420_into(&src, 4, 2, 8, 2, &mut planes);
        assert_eq!(&planes.y[0..8], &[16, 16, 235, 235, 235, 235, 235, 235]);
    }

    #[test]
    fn odd_source_height_pads_by_repeating_the_last_row() {
        // 2x3 source: rows white, white, black. Padded to 2x4, the added row 3
        // must repeat row 2 (black), not wrap to row 0.
        let src = vec![
            0x00FF_FFFFu32,
            0x00FF_FFFF,
            0x00FF_FFFF,
            0x00FF_FFFF,
            0x0000_0000,
            0x0000_0000,
        ];
        let mut planes = I420Planes::default();
        convert_0rgb_to_i420_into(&src, 2, 3, 2, 4, &mut planes);
        assert_eq!(&planes.y[4..6], &[16, 16], "row 2 is the black source row");
        assert_eq!(&planes.y[6..8], &[16, 16], "row 3 repeats it");
    }

    #[test]
    fn planes_are_reused_across_frames_of_the_same_size() {
        let mut planes = I420Planes::default();
        convert_0rgb_to_i420_into(&[0x0000_0000u32; 4], 2, 2, 2, 2, &mut planes);
        let capacity = planes.y.capacity();
        let pointer = planes.y.as_ptr();

        convert_0rgb_to_i420_into(&[0x00FF_FFFFu32; 4], 2, 2, 2, 2, &mut planes);
        assert_eq!(planes.y.capacity(), capacity, "no reallocation");
        assert_eq!(planes.y.as_ptr(), pointer, "same backing buffer");
        assert_eq!(planes.y[0], 235, "and the new content landed");
    }

    #[test]
    fn upscale_scale_one_copies_input() {
        let src = vec![0x00112233, 0x00445566, 0x00778899, 0x00AABBCC];
        let mut out = Vec::new();
        let (w, h) = nearest_upscale_0rgb_into(&src, 2, 2, 1, &mut out);
        assert_eq!((w, h), (2, 2));
        assert_eq!(out, src);
    }

    #[test]
    fn upscale_2x_replicates_each_pixel_into_2x2_block() {
        // Source:
        //   A B
        //   C D
        let a = 0x00_11_11_11u32;
        let b = 0x00_22_22_22u32;
        let c = 0x00_33_33_33u32;
        let d = 0x00_44_44_44u32;
        let src = vec![a, b, c, d];
        let mut out = Vec::new();
        let (w, h) = nearest_upscale_0rgb_into(&src, 2, 2, 2, &mut out);
        assert_eq!((w, h), (4, 4));
        // Expected 4x4:
        //   A A B B
        //   A A B B
        //   C C D D
        //   C C D D
        assert_eq!(
            out,
            vec![
                a, a, b, b, //
                a, a, b, b, //
                c, c, d, d, //
                c, c, d, d, //
            ]
        );
    }

    #[test]
    fn upscale_4x_produces_correct_dimensions() {
        let src = vec![0xDEADBEEF; 320 * 200];
        let mut out = Vec::new();
        let (w, h) = nearest_upscale_0rgb_into(&src, 320, 200, 4, &mut out);
        assert_eq!((w, h), (1280, 800));
        assert_eq!(out.len(), 1280 * 800);
        // Every pixel should be the same replicated value.
        assert!(out.iter().all(|&p| p == 0xDEADBEEF));
    }

    #[test]
    fn upscale_reuses_output_buffer() {
        let mut out = Vec::new();
        let src1 = vec![0x11_22_33_44u32; 4];
        nearest_upscale_0rgb_into(&src1, 2, 2, 2, &mut out);
        let cap_after_first = out.capacity();

        // Second call with same output dims — capacity should not grow.
        let src2 = vec![0xAA_BB_CC_DDu32; 4];
        nearest_upscale_0rgb_into(&src2, 2, 2, 2, &mut out);
        assert_eq!(out.capacity(), cap_after_first);
        assert!(out.iter().all(|&p| p == 0xAA_BB_CC_DD));
    }
}
