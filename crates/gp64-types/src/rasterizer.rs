//! Shared half-space triangle rasterizer core.
//!
//! Provides edge function setup, bounding box clipping, and pixel iteration
//! with barycentric coordinates. Backend-specific pixel pipelines plug in
//! via a closure passed to [`TriangleSetup::rasterize`].

/// Clip rectangle for bounding box clipping.
/// All coordinates are in pixel space. `max_x` and `max_y` are **exclusive**
/// (i.e., valid pixels are in `[min_x, max_x)` and `[min_y, max_y)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipRect {
    pub min_x: i32,
    pub min_y: i32,
    /// Exclusive upper bound for X.
    pub max_x: i32,
    /// Exclusive upper bound for Y.
    pub max_y: i32,
}

impl ClipRect {
    /// Create a clip rect covering `[0, width) x [0, height)`.
    pub fn from_size(width: i32, height: i32) -> Self {
        Self {
            min_x: 0,
            min_y: 0,
            max_x: width,
            max_y: height,
        }
    }
}

/// Subpixel precision of the snapped coordinate grid (28.4 fixed point).
///
/// Coverage is decided on vertices snapped to this grid with EXACT i64 edge
/// functions, which is what makes the rasterizer watertight: two triangles
/// sharing an edge derive it from the same snapped endpoints, so their edge
/// functions are exact negations and every boundary pixel is claimed by
/// exactly one of them under the top-left fill rule (MDD-BUG-FLUX-00124).
/// The old f32 edge functions rounded each side independently, and pixels
/// within rounding error of a shared edge could be rejected by BOTH sides —
/// flickering black hairline gaps along shared diagonals.
const SUBPIXEL_BITS: u32 = 4;
const SUBPIXEL_SCALE: f64 = (1i64 << SUBPIXEL_BITS) as f64; // 16.0
/// Snapped coordinates are clamped to ±this many subpixels (±4M pixels).
/// Keeps every intermediate product comfortably inside i64; geometry that
/// far off-screen was already precision-noise under the old f32 scheme.
const COORD_LIMIT_SUB: i64 = 1 << 26;

/// One triangle edge as an exact integer half-space function over the
/// snapped subpixel grid, orientation-normalized so the interior is the
/// POSITIVE side regardless of the caller's winding.
///
/// `w(px, py) = step_x*px + step_y*py + c_pix` evaluates the edge function
/// at the CENTER of pixel `(px, py)`, in subpixel² units, with the top-left
/// tie bias already folded in: the inside test is exactly `w >= 0`.
/// Recover the unbiased weight (for barycentrics / distances) as `w + tie`.
#[derive(Debug, Clone, Copy)]
struct EdgeFn {
    /// dw per +1 subpixel in x (`-(dy)` of the snapped edge, normalized).
    a: i64,
    /// dw per +1 subpixel in y (`dx` of the snapped edge, normalized).
    b: i64,
    /// dw per +1 PIXEL in x (`16*a`).
    step_x: i64,
    /// dw per +1 PIXEL in y (`16*b`).
    step_y: i64,
    /// Constant term for pixel-center evaluation, tie bias included.
    c_pix: i64,
    /// 0 if this is a top or left edge (w == 0 counts as inside), else 1.
    tie: i64,
}

impl EdgeFn {
    /// Build the edge from snapped subpixel endpoints, multiplied through by
    /// `orient` (±1) so the triangle interior evaluates positive.
    fn new(from: [i64; 2], to: [i64; 2], orient: i64) -> Self {
        let dx = to[0] - from[0];
        let dy = to[1] - from[1];
        let a = -dy * orient;
        let b = dx * orient;
        let c = (dy * from[0] - dx * from[1]) * orient;
        // Top-left fill rule, derived for the interior-positive convention in
        // y-down screen space: a horizontal edge is a TOP edge when the
        // interior is below it (w grows with y ⇔ b > 0); a LEFT edge has the
        // interior to its right (w grows with x ⇔ a > 0). Pixels exactly on a
        // top or left edge are inside; on any other edge they are outside —
        // so a pixel on a shared edge lands in exactly one triangle.
        let top_left = a > 0 || (a == 0 && b > 0);
        let tie = i64::from(!top_left);
        // Pixel (px, py) has its center at subpixel (16*px + 8, 16*py + 8).
        let half = 1i64 << (SUBPIXEL_BITS - 1);
        Self {
            a,
            b,
            step_x: a << SUBPIXEL_BITS,
            step_y: b << SUBPIXEL_BITS,
            c_pix: a * half + b * half + c - tie,
            tie,
        }
    }

    /// Evaluate the (tie-biased) edge function at the center of pixel
    /// `(px, py)`. Exact — no rounding at any coordinate magnitude.
    #[inline(always)]
    fn eval_pixel(&self, px: i64, py: i64) -> i64 {
        self.step_x * px + self.step_y * py + self.c_pix
    }
}

/// Width, in pixels, of the border drawn by
/// [`rasterize_debug`](TriangleSetup::rasterize_debug).
pub const DEBUG_BORDER_PX: f32 = 1.0;

/// Barycentric coordinates for a point inside a triangle.
///
/// `b0`, `b1`, `b2` correspond to vertices v0, v1, v2 respectively.
/// For any point inside the triangle, `b0 + b1 + b2 ≈ 1.0` (within
/// floating-point tolerance).
#[derive(Debug, Clone, Copy)]
pub struct BaryCoords {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
}

/// Precomputed triangle setup for half-space rasterization.
///
/// Created via [`TriangleSetup::new`], which precomputes edge function
/// coefficients, bounding box, and area. The caller should check
/// [`signed_area`](TriangleSetup::signed_area) for culling decisions
/// before calling [`rasterize`](TriangleSetup::rasterize).
///
/// # Edge function convention
///
/// Uses the standard expression: `f(p) = (x1-x0)*(py-y0) - (y1-y0)*(px-x0)`.
/// Positive signed area indicates counter-clockwise winding in screen
/// space (Y-down). Callers using the opposite winding convention should
/// negate their culling check.
pub struct TriangleSetup {
    // Exact integer edge functions for each edge, orientation-normalized
    // (interior positive), tie-biased for the top-left fill rule:
    //   edge[0]: v1 → v2 (weight for v0)
    //   edge[1]: v2 → v0 (weight for v1)
    //   edge[2]: v0 → v1 (weight for v2)
    edge: [EdgeFn; 3],
    // Bounding box (clamped to clip rect), exclusive upper bounds
    min_x: i32,
    min_y: i32,
    max_x: i32,
    max_y: i32,
    // 1.0 / signed_area (pixel² units, ORIGINAL winding sign), for
    // normalizing the f32 seam weights into barycentrics.
    inv_area: f32,
    // The signed area (2x geometric area, pixel², original winding sign,
    // computed from the SNAPPED vertices), exposed for culling
    area: f32,
    // Converts an unbiased integer edge weight (subpixel² units, interior-
    // positive) into the f32 seam weight (pixel² units, original winding
    // sign): `orient / 256.0`. The seam therefore looks exactly like the old
    // f32 rasterizer's weights to every caller.
    seam_scale: f32,
    // 1 / |doubled area in subpixel²| — normalizes unbiased integer weights
    // straight to barycentrics (used by the AA traversal).
    bary_scale: f32,
    // Per-row scanline clipping (S1 optimisation): whether each edge provides
    // a left or right boundary (w grows with +x ⇔ left, after normalization).
    is_left: [bool; 3],
}

impl TriangleSetup {
    /// Compute triangle setup from 3 screen-space vertex positions.
    ///
    /// Returns `None` if any input is non-finite, if the triangle is
    /// degenerate (`|area| < epsilon`, or zero area after snapping to the
    /// 28.4 subpixel grid), or if the bounding box is empty after clipping.
    ///
    /// # Arguments
    ///
    /// * `v0`, `v1`, `v2` — Screen-space vertex positions as `[x, y]`.
    /// * `clip` — Clip rectangle (exclusive upper bounds). The caller should
    ///   precompute this from their viewport, scissor, or framebuffer bounds.
    /// * `epsilon` — Minimum absolute area for non-degenerate triangles.
    ///   Choose a tolerance appropriate to the input coordinate precision.
    pub fn new(
        v0: [f32; 2],
        v1: [f32; 2],
        v2: [f32; 2],
        clip: ClipRect,
        epsilon: f32,
    ) -> Option<Self> {
        Self::new_inner(v0, v1, v2, clip, epsilon, 0.0)
    }

    /// [`Self::new`] with the bounding box dilated by one pixel (still inside
    /// `clip`), for the [`rasterize_aa`](Self::rasterize_aa) traversal — edge
    /// coverage extends up to half a pixel outside the exact triangle, so the
    /// AA footprint needs the surrounding ring. Every other field is
    /// identical, and the non-AA traversals stay valid on the wider box (its
    /// extra ring simply fails their inside test).
    pub fn new_aa(
        v0: [f32; 2],
        v1: [f32; 2],
        v2: [f32; 2],
        clip: ClipRect,
        epsilon: f32,
    ) -> Option<Self> {
        Self::new_inner(v0, v1, v2, clip, epsilon, 1.0)
    }

    fn new_inner(
        v0: [f32; 2],
        v1: [f32; 2],
        v2: [f32; 2],
        clip: ClipRect,
        epsilon: f32,
        dilate: f32,
    ) -> Option<Self> {
        // Reject non-finite input before any setup work.
        //
        // Vertex coordinates come from guest memory as raw IEEE-754 bit
        // patterns, so NaN and the infinities are reachable and are not
        // filtered by the checks below. NaN defeats them coming and going:
        // `area.abs() < epsilon` is FALSE for NaN, so the degeneracy check
        // passes it through; the chained `min`/`max` against the finite clip
        // bounds then DISCARD the NaN and yield the whole clip rectangle, so
        // the empty-box check passes too. The result is a full-frame bounding
        // box whose per-pixel inside tests are all false — maximum host work
        // for zero pixels, repeatable as fast as the guest can submit
        // primitives (MDD-BUG-CRUCIBLE-00342).
        //
        // `epsilon` is checked as well: a NaN epsilon would defeat the
        // degeneracy test for otherwise valid geometry.
        if !v0[0].is_finite()
            || !v0[1].is_finite()
            || !v1[0].is_finite()
            || !v1[1].is_finite()
            || !v2[0].is_finite()
            || !v2[1].is_finite()
            || !epsilon.is_finite()
        {
            return None;
        }

        // Signed area = edge_function(v0, v1, v2), in the caller's raw f32
        // coordinates. Retained as the overflow guard and the epsilon
        // degeneracy contract (a finite-coordinate triangle can still have an
        // infinite f32 area — MDD-BUG-CRUCIBLE-00342).
        let area_raw = (v1[0] - v0[0]) * (v2[1] - v0[1]) - (v1[1] - v0[1]) * (v2[0] - v0[0]);
        if !area_raw.is_finite() || area_raw.abs() < epsilon {
            return None;
        }

        // Snap vertices to the 28.4 subpixel grid. From here on, coverage is
        // decided by EXACT integer arithmetic on these snapped coordinates —
        // two triangles sharing an edge compute exactly-negated edge
        // functions, which is the watertightness invariant (FLUX-00124).
        let snap = |v: [f32; 2]| -> [i64; 2] {
            let lim = COORD_LIMIT_SUB as f64;
            [
                (f64::from(v[0]) * SUBPIXEL_SCALE).round().clamp(-lim, lim) as i64,
                (f64::from(v[1]) * SUBPIXEL_SCALE).round().clamp(-lim, lim) as i64,
            ]
        };
        let p0 = snap(v0);
        let p1 = snap(v1);
        let p2 = snap(v2);

        // Doubled signed area on the snapped grid (subpixel² units). Snapping
        // can collapse a genuinely thin sliver to zero area — reject it, as
        // period hardware did below its own subpixel precision.
        let area2_sub = (p1[0] - p0[0]) * (p2[1] - p0[1]) - (p1[1] - p0[1]) * (p2[0] - p0[0]);
        if area2_sub == 0 {
            return None;
        }
        let orient: i64 = if area2_sub > 0 { 1 } else { -1 };
        let s_sub = area2_sub * orient;

        // Edge functions (each edge is opposite its corresponding vertex),
        // normalized so the interior is positive for either input winding.
        let edge = [
            EdgeFn::new(p1, p2, orient),
            EdgeFn::new(p2, p0, orient),
            EdgeFn::new(p0, p1, orient),
        ];

        // Bounding box from the snapped vertex positions (dilated for the AA
        // footprint), clipped to clip rect.
        let px = |c: i64| c as f64 / SUBPIXEL_SCALE;
        let dilate = f64::from(dilate);
        let v_min_x = px(p0[0].min(p1[0]).min(p2[0])) - dilate;
        let v_min_y = px(p0[1].min(p1[1]).min(p2[1])) - dilate;
        let v_max_x = px(p0[0].max(p1[0]).max(p2[0])) + dilate;
        let v_max_y = px(p0[1].max(p1[1]).max(p2[1])) + dilate;

        let min_x = v_min_x.max(f64::from(clip.min_x)).floor() as i32;
        let min_y = v_min_y.max(f64::from(clip.min_y)).floor() as i32;
        let max_x = v_max_x.min(f64::from(clip.max_x)).ceil() as i32;
        let max_y = v_max_y.min(f64::from(clip.max_y)).ceil() as i32;

        if min_x >= max_x || min_y >= max_y {
            return None;
        }

        // Snapped doubled area in pixel² units, original winding sign — the
        // value every public consumer sees (culling, gradients, seam
        // normalization). Using the snapped area keeps barycentrics exactly
        // consistent with the snapped coverage.
        let area_px = area2_sub as f64 / (SUBPIXEL_SCALE * SUBPIXEL_SCALE);

        Some(Self {
            edge,
            min_x,
            min_y,
            max_x,
            max_y,
            inv_area: (1.0 / area_px) as f32,
            area: area_px as f32,
            seam_scale: orient as f32 / (SUBPIXEL_SCALE * SUBPIXEL_SCALE) as f32,
            bary_scale: (1.0 / s_sub as f64) as f32,
            // S1 boundary direction for per-row scanline clipping: after
            // normalization the inside test is `w >= 0`, so an edge whose
            // weight grows with +x (a > 0) bounds the run on the LEFT.
            is_left: [edge[0].a > 0, edge[1].a > 0, edge[2].a > 0],
        })
    }

    /// The signed area of the triangle (2x the geometric area).
    ///
    /// In screen space (Y-down):
    /// - Positive → counter-clockwise vertex winding
    /// - Negative → clockwise vertex winding
    ///
    /// Use this for backface culling decisions before calling `rasterize()`.
    #[inline]
    pub fn signed_area(&self) -> f32 {
        self.area
    }

    /// Pixel count of the clip-intersected bounding box.
    ///
    /// This is an upper bound on the work `rasterize` / `rasterize_runs` can
    /// do: per-row scanline clipping visits a subset of the box (typically
    /// ~half), and it is also the cost of the box walk itself. Callers that
    /// must bound synchronous raster work driven by untrusted geometry can sum
    /// this across a batch before drawing any of it.
    #[inline]
    pub fn bounding_box_pixels(&self) -> u64 {
        // `new` rejects an empty box, so both extents are positive; the i64
        // widening keeps a far-apart pair from overflowing the subtraction.
        let w = (self.max_x as i64 - self.min_x as i64).max(0) as u64;
        let h = (self.max_y as i64 - self.min_y as i64).max(0) as u64;
        w.saturating_mul(h)
    }

    /// Iterate all pixels inside the triangle, calling `pixel_fn` for each.
    ///
    /// Uses per-row scanline clipping (S1 optimisation): for each row, the
    /// exact left and right x-boundaries are computed from edge zero-crossings,
    /// so the inner loop only iterates over pixels that are (approximately)
    /// inside the triangle.  This eliminates all leading and trailing outside
    /// pixels from the bounding-box iteration — typically ~50% of the bbox.
    ///
    /// Within the computed scan range, incremental edge stepping (3 additions
    /// per pixel) and a safety inside test preserve correctness for FP edge
    /// precision.
    ///
    /// Pixels are tested at their centers (integer coord + 0.5) against
    /// EXACT integer edge functions on the 28.4-snapped vertices, with the
    /// top-left fill rule: a pixel center exactly on an edge is inside only
    /// when that edge is a top or left edge. Two triangles sharing an edge
    /// therefore claim every boundary pixel exactly once — no gaps, no
    /// double-draw (MDD-BUG-FLUX-00124).
    ///
    /// The closure receives `(px, py, bary)` where `px`/`py` are integer
    /// pixel coordinates and `bary` contains normalized barycentric
    /// coordinates corresponding to vertices v0, v1, v2.
    #[inline(always)]
    pub fn rasterize<F>(&self, mut pixel_fn: F)
    where
        F: FnMut(i32, i32, BaryCoords),
    {
        // Implemented via `rasterize_runs`: expanding each run with the same
        // per-pixel edge-weight additions the monolithic loop performed
        // reproduces bit-identical barycentrics (same f32 ops, same order).
        let [a0, a1, a2] = self.x_steps();
        let inv_area = self.inv_area;
        self.rasterize_runs(|py, x_start, x_end, w| {
            let [mut w0, mut w1, mut w2] = w;
            for px in x_start..x_end {
                let b0 = w0 * inv_area;
                let b1 = w1 * inv_area;
                let b2 = w2 * inv_area;
                pixel_fn(px, py, BaryCoords { b0, b1, b2 });
                w0 += a0;
                w1 += a1;
                w2 += a2;
            }
        });
    }

    /// The reciprocal of the triangle's signed area, for normalizing raw
    /// edge weights into barycentrics (`b = w * inv_area`).
    #[inline]
    pub fn inv_area(&self) -> f32 {
        self.inv_area
    }

    /// Per-pixel +X steps of the three raw edge weights `[w0, w1, w2]`
    /// handed to a [`rasterize_runs`](Self::rasterize_runs) span callback.
    #[inline]
    pub fn x_steps(&self) -> [f32; 3] {
        [
            self.edge[0].step_x as f32 * self.seam_scale,
            self.edge[1].step_x as f32 * self.seam_scale,
            self.edge[2].step_x as f32 * self.seam_scale,
        ]
    }

    /// Iterate all maximal runs of covered pixels, calling `span_fn` once per
    /// run.
    ///
    /// A run is a maximal interval `[x_start, x_end)` on row `y` in which
    /// every pixel passes the exact per-pixel inside test that
    /// [`rasterize`](Self::rasterize) applies — so the union of runs is
    /// pixel-for-pixel identical to the per-pixel traversal, in the same
    /// order. `span_fn` receives `(y, x_start, x_end, w)` where `w` holds the
    /// UNNORMALIZED edge weights at the center of pixel `x_start`; stepping
    /// them by [`x_steps`](Self::x_steps) per +X pixel and multiplying by
    /// [`inv_area`](Self::inv_area) reproduces the per-pixel barycentrics
    /// bit-identically (the weights follow the same f32 accumulation path as
    /// the per-pixel loop).
    ///
    /// This is the span-shading seam: callers hoist per-pixel state dispatch
    /// to once per run and keep only interpolation work in the inner loop.
    #[inline(always)]
    pub fn rasterize_runs<F>(&self, mut span_fn: F)
    where
        F: FnMut(i32, i32, i32, [f32; 3]),
    {
        let [fa0, fa1, fa2] = self.x_steps();
        self.rasterize_scanlines(|py, scan_left_i, scan_right_i, wi, wf| {
            // Coverage steps the EXACT integer weights (the watertightness
            // invariant); the f32 seam weights step alongside so the values
            // handed to `span_fn` keep the historical accumulation path.
            let [mut i0, mut i1, mut i2] = wi;
            let [mut w0, mut w1, mut w2] = wf;
            let mut run_start: Option<(i32, [f32; 3])> = None;
            for px in scan_left_i..scan_right_i {
                // Tie bias is folded into the constant term, so `>= 0` IS the
                // top-left fill rule: a pixel exactly on a shared edge is
                // inside for exactly one of the two adjacent triangles.
                let inside = i0 >= 0 && i1 >= 0 && i2 >= 0;

                if inside {
                    if run_start.is_none() {
                        run_start = Some((px, [w0, w1, w2]));
                    }
                } else if let Some((start, w)) = run_start.take() {
                    span_fn(py, start, px, w);
                }

                // Incremental X step (integer: exact; f32: seam contract)
                i0 += self.edge[0].step_x;
                i1 += self.edge[1].step_x;
                i2 += self.edge[2].step_x;
                w0 += fa0;
                w1 += fa1;
                w2 += fa2;
            }
            if let Some((start, w)) = run_start.take() {
                span_fn(py, start, scan_right_i, w);
            }
        });
    }

    /// Iterate the approximate scan range in groups of up to four consecutive
    /// pixels, testing coverage exactly once and returning normalized
    /// barycentrics plus a lane mask.
    ///
    /// `lane_fn` receives `(y, x_start, lane_count, coverage_mask, b0, b1,
    /// b2)`. Bit `i` of `coverage_mask` corresponds to pixel `x_start + i`.
    /// Edge weights use the same scalar additions and normalization order as
    /// [`rasterize`](Self::rasterize), so every covered lane is bit-identical.
    /// Empty batches are omitted. The fixed four-lane shape is the portable
    /// contract used by SSE2 and NEON; wider backends may combine two calls.
    #[inline(always)]
    pub fn rasterize_lanes4<F>(&self, mut lane_fn: F)
    where
        F: FnMut(i32, i32, usize, u8, [f32; 4], [f32; 4], [f32; 4]),
    {
        let inv_area = self.inv_area;
        let [fa0, fa1, fa2] = self.x_steps();
        self.rasterize_scanlines(|py, scan_left_i, scan_right_i, wi, wf| {
            let [mut i0, mut i1, mut i2] = wi;
            let [mut w0, mut w1, mut w2] = wf;
            let mut px = scan_left_i;
            while px < scan_right_i {
                let n = ((scan_right_i - px) as usize).min(4);
                let mut coverage_mask = 0u8;
                let mut b0 = [0.0f32; 4];
                let mut b1 = [0.0f32; 4];
                let mut b2 = [0.0f32; 4];
                for lane in 0..n {
                    let inside = i0 >= 0 && i1 >= 0 && i2 >= 0;
                    if inside {
                        coverage_mask |= 1 << lane;
                    }
                    b0[lane] = w0 * inv_area;
                    b1[lane] = w1 * inv_area;
                    b2[lane] = w2 * inv_area;
                    i0 += self.edge[0].step_x;
                    i1 += self.edge[1].step_x;
                    i2 += self.edge[2].step_x;
                    w0 += fa0;
                    w1 += fa1;
                    w2 += fa2;
                }
                if coverage_mask != 0 {
                    lane_fn(py, px, n, coverage_mask, b0, b1, b2);
                }
                px += n as i32;
            }
        });
    }

    /// Iterate every pixel with non-zero fractional edge coverage, calling
    /// `pixel_fn` with `(px, py, bary, coverage)`.
    ///
    /// Coverage uses a single-sample edge-distance approximation for
    /// antialiasing: per edge, the pixel centre's signed
    /// distance `d` (in pixels, positive inside) maps to `clamp(d + 0.5, 0,
    /// 1)`, and the pixel's coverage is the product over the three edges.
    /// Any pixel at least half a pixel inside every edge therefore gets
    /// coverage exactly 1.0; the fade band spans one pixel across each edge,
    /// including up to half a pixel *outside* the exact triangle — build the
    /// setup with [`new_aa`](Self::new_aa) so the bounding box includes that
    /// ring. A sliver thinner than a pixel never reaches full coverage,
    /// which is the correct translucent rendering of subpixel geometry.
    ///
    /// Barycentrics for outside-the-edge pixels extrapolate slightly (a
    /// weight can be marginally negative); interpolating callers clamp where
    /// their semantics require it. The non-AA traversals are untouched by
    /// this path — bit-identical output with AA off is the engine contract.
    #[inline(always)]
    pub fn rasterize_aa<F>(&self, mut pixel_fn: F)
    where
        F: FnMut(i32, i32, BaryCoords, f32),
    {
        // Signed pixel distance per edge for the UNBIASED, orientation-
        // normalized integer weight (interior positive, subpixel² units):
        // d_px = w / (16 * |(a, b)|), since w carries 16² subpixel scaling
        // and |(a, b)| carries one factor of 16 relative to pixel units.
        let dist = |e: &EdgeFn| -> f32 {
            let len = ((e.a * e.a + e.b * e.b) as f64).sqrt();
            (1.0 / (SUBPIXEL_SCALE * len)) as f32
        };
        let dist0 = dist(&self.edge[0]);
        let dist1 = dist(&self.edge[1]);
        let dist2 = dist(&self.edge[2]);
        let bary_scale = self.bary_scale;

        let mut w0_row = self.edge[0].eval_pixel(i64::from(self.min_x), i64::from(self.min_y));
        let mut w1_row = self.edge[1].eval_pixel(i64::from(self.min_x), i64::from(self.min_y));
        let mut w2_row = self.edge[2].eval_pixel(i64::from(self.min_x), i64::from(self.min_y));

        for py in self.min_y..self.max_y {
            let [mut w0, mut w1, mut w2] = [w0_row, w1_row, w2_row];
            for px in self.min_x..self.max_x {
                // Unbiased weights: the top-left tie bias belongs to the
                // binary fill rule, not to analog coverage or barycentrics.
                let u0 = (w0 + self.edge[0].tie) as f32;
                let u1 = (w1 + self.edge[1].tie) as f32;
                let u2 = (w2 + self.edge[2].tie) as f32;
                let c0 = (u0 * dist0 + 0.5).clamp(0.0, 1.0);
                let c1 = (u1 * dist1 + 0.5).clamp(0.0, 1.0);
                let c2 = (u2 * dist2 + 0.5).clamp(0.0, 1.0);
                let coverage = c0 * c1 * c2;
                if coverage > 0.0 {
                    let bary = BaryCoords {
                        b0: u0 * bary_scale,
                        b1: u1 * bary_scale,
                        b2: u2 * bary_scale,
                    };
                    pixel_fn(px, py, bary, coverage);
                }
                w0 += self.edge[0].step_x;
                w1 += self.edge[1].step_x;
                w2 += self.edge[2].step_x;
            }
            w0_row += self.edge[0].step_y;
            w1_row += self.edge[1].step_y;
            w2_row += self.edge[2].step_y;
        }
    }

    /// Iterate all pixels inside the triangle, classifying each as interior or
    /// border, for the debug-texture diagnostic.
    ///
    /// The closure receives `(px, py, bary, is_border)`. Coverage is delegated
    /// to [`rasterize_runs`](Self::rasterize_runs), exactly as
    /// [`rasterize`](Self::rasterize) is, so the debug image marks precisely the
    /// pixels the real shading path would have covered — same fill rule, same
    /// order. That equality is the whole point: a diagnostic that disagrees with
    /// the engine it is diagnosing sends the reader hunting ghosts.
    ///
    /// `is_border` is true when the pixel centre lies within
    /// [`DEBUG_BORDER_PX`] of any edge, using the same signed pixel-distance
    /// expression as [`rasterize_aa`](Self::rasterize_aa): `w * sign / |(a, b)|`.
    /// This lives here rather than in the engines because the edge `b`
    /// coefficients are private, so a caller cannot compute pixel-space edge
    /// distance from the public API.
    ///
    /// A triangle whose inradius is at most [`DEBUG_BORDER_PX`] is *entirely*
    /// border. That is deliberate and not a defect — such a triangle is too thin
    /// to show an interior — but it is why callers must draw border pixels in a
    /// colour that cannot be confused with a cleared framebuffer.
    #[inline(always)]
    pub fn rasterize_debug<F>(&self, mut pixel_fn: F)
    where
        F: FnMut(i32, i32, BaryCoords, bool),
    {
        // Signed pixel distance per edge, positive inside (see `rasterize_aa`).
        // The seam weights below are `orient * w_true / 256` in pixel² units,
        // so the multiplier that turns one into pixel distance is
        // `orient * 16 / |(a, b)|` (orient² = 1 restores positive-inside).
        let orient = if self.seam_scale > 0.0 {
            1.0f64
        } else {
            -1.0f64
        };
        let dist = |e: &EdgeFn| -> f32 {
            let len = ((e.a * e.a + e.b * e.b) as f64).sqrt();
            (orient * SUBPIXEL_SCALE / len) as f32
        };
        let dist0 = dist(&self.edge[0]);
        let dist1 = dist(&self.edge[1]);
        let dist2 = dist(&self.edge[2]);

        let [a0, a1, a2] = self.x_steps();
        let inv_area = self.inv_area;
        self.rasterize_runs(|py, x_start, x_end, w| {
            let [mut w0, mut w1, mut w2] = w;
            for px in x_start..x_end {
                let is_border = w0 * dist0 < DEBUG_BORDER_PX
                    || w1 * dist1 < DEBUG_BORDER_PX
                    || w2 * dist2 < DEBUG_BORDER_PX;
                let bary = BaryCoords {
                    b0: w0 * inv_area,
                    b1: w1 * inv_area,
                    b2: w2 * inv_area,
                };
                pixel_fn(px, py, bary, is_border);
                w0 += a0;
                w1 += a1;
                w2 += a2;
            }
        });
    }

    /// Walk each clipped row once and provide its approximate scan interval
    /// plus the raw edge weights at the first pixel — the exact (tie-biased)
    /// integer weights that decide coverage, and the f32 seam weights handed
    /// to callers. Coverage policy belongs to the caller (`rasterize_runs`
    /// or `rasterize_lanes4`).
    #[inline(always)]
    fn rasterize_scanlines<F>(&self, mut scanline_fn: F)
    where
        F: FnMut(i32, i32, i32, [i64; 3], [f32; 3]),
    {
        // Integer weights at the center of the top-left pixel. Row stepping
        // is exact integer arithmetic — there is no accumulated error
        // anywhere in the coverage path.
        let mut w0_row = self.edge[0].eval_pixel(i64::from(self.min_x), i64::from(self.min_y));
        let mut w1_row = self.edge[1].eval_pixel(i64::from(self.min_x), i64::from(self.min_y));
        let mut w2_row = self.edge[2].eval_pixel(i64::from(self.min_x), i64::from(self.min_y));

        let min_x_f = f64::from(self.min_x);
        let max_x_f = f64::from(self.max_x);
        let seam_scale = self.seam_scale;

        for py in self.min_y..self.max_y {
            // S1: compute per-row scanline x-bounds from edge zero-crossings.
            //
            // Each edge is linear in px: w(px) = w_row + (px - min_x) * step_x.
            // The zero-crossing is at px_cross = min_x - w_row / step_x.
            // Left-bound edges (step_x > 0) constrain scan_left; right-bound
            // edges constrain scan_right. A horizontal edge (step_x == 0) is
            // constant along the row: it imposes no x-bound when the row is
            // inside it, and empties the row when it is not. (The old f32
            // code got this from the SIGNED zero — `1.0 / -0.0 = -inf` — a
            // trick integer coefficients cannot replay, so it is explicit.)
            let mut scan_left = min_x_f;
            let mut scan_right = max_x_f;

            if self.edge[0].step_x != 0 {
                let x0 = min_x_f - w0_row as f64 / self.edge[0].step_x as f64;
                if self.is_left[0] {
                    scan_left = scan_left.max(x0);
                } else {
                    scan_right = scan_right.min(x0);
                }
            } else if w0_row < 0 {
                scan_right = f64::NEG_INFINITY;
            }

            if self.edge[1].step_x != 0 {
                let x1 = min_x_f - w1_row as f64 / self.edge[1].step_x as f64;
                if self.is_left[1] {
                    scan_left = scan_left.max(x1);
                } else {
                    scan_right = scan_right.min(x1);
                }
            } else if w1_row < 0 {
                scan_right = f64::NEG_INFINITY;
            }

            if self.edge[2].step_x != 0 {
                let x2 = min_x_f - w2_row as f64 / self.edge[2].step_x as f64;
                if self.is_left[2] {
                    scan_left = scan_left.max(x2);
                } else {
                    scan_right = scan_right.min(x2);
                }
            } else if w2_row < 0 {
                scan_right = f64::NEG_INFINITY;
            }

            // Convert to integer pixel range, clamped to bounding box, then
            // widen by one pixel on each side (MDD-BUG-CRUCIBLE-00522): the
            // crossings are floating-point approximations, and the exact
            // per-pixel test below can only REJECT a pixel, never recover one
            // the bound already excluded — so the bound must be conservative.
            // Widening is free of false positives precisely because the exact
            // test still runs on every pixel in the range; the cost is at
            // most two extra rejected pixels per row. Both sides stay clamped
            // to the clipped bounding box, so this can never step outside it.
            let scan_left_i = (scan_left.ceil() as i32).saturating_sub(1).max(self.min_x);
            let scan_right_i = ((scan_right.floor() as i32).saturating_add(2)).min(self.max_x);

            if scan_left_i < scan_right_i {
                // Exact integer weights at scan_left_i (no drift), plus the
                // f32 seam weights derived from them: unbias the top-left
                // tie, restore the caller's winding sign, and rescale from
                // subpixel² to pixel² — the seam looks exactly like the old
                // f32 rasterizer's weights to every consumer.
                let offset = i64::from(scan_left_i - self.min_x);
                let i0 = w0_row + offset * self.edge[0].step_x;
                let i1 = w1_row + offset * self.edge[1].step_x;
                let i2 = w2_row + offset * self.edge[2].step_x;
                let w0 = (i0 + self.edge[0].tie) as f32 * seam_scale;
                let w1 = (i1 + self.edge[1].tie) as f32 * seam_scale;
                let w2 = (i2 + self.edge[2].tie) as f32 * seam_scale;
                scanline_fn(py, scan_left_i, scan_right_i, [i0, i1, i2], [w0, w1, w2]);
            }

            // Incremental Y step (exact)
            w0_row += self.edge[0].step_y;
            w1_row += self.edge[1].step_y;
            w2_row += self.edge[2].step_y;
        }
    }
}

/// Compute the edge function value for a single point.
///
/// This is the standalone version used during triangle setup and area
/// computation. For per-pixel evaluation, use the precomputed
/// [`EdgeCoeffs`] inside [`TriangleSetup`].
///
/// Returns `(x1-x0)*(py-y0) - (y1-y0)*(px-x0)`, which is positive
/// when `(px, py)` is to the left of the edge from `(x0, y0)` to
/// `(x1, y1)` (counter-clockwise).
#[inline]
pub fn edge_function(x0: f32, y0: f32, x1: f32, y1: f32, px: f32, py: f32) -> f32 {
    (x1 - x0) * (py - y0) - (y1 - y0) * (px - x0)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    // ── rasterize_debug ───────────────────────────────────────────

    /// The debug oracle is only worth anything if it marks exactly the pixels
    /// the real shading path covers. Anything else sends the reader hunting a
    /// geometry bug that isn't there.
    #[test]
    fn rasterize_debug_covers_exactly_the_same_pixels_as_rasterize() {
        let cases: [([f32; 2], [f32; 2], [f32; 2]); 4] = [
            ([10.0, 10.0], [90.0, 20.0], [30.0, 80.0]),
            // Opposite winding — exercises the `positive_area` sign flip.
            ([30.0, 80.0], [90.0, 20.0], [10.0, 10.0]),
            // Sliver (inradius well under a pixel).
            ([5.0, 5.0], [95.0, 5.4], [50.0, 6.2]),
            // Partly off the clip rect.
            ([-20.0, -10.0], [70.0, 30.0], [20.0, 90.0]),
        ];

        for (i, (v0, v1, v2)) in cases.iter().enumerate() {
            let setup = TriangleSetup::new(*v0, *v1, *v2, ClipRect::from_size(100, 100), 1e-6)
                .unwrap_or_else(|| panic!("case {i} should be rasterizable"));

            let mut plain = Vec::new();
            setup.rasterize(|px, py, b| plain.push((px, py, b.b0, b.b1, b.b2)));

            let mut debug = Vec::new();
            setup.rasterize_debug(|px, py, b, _| debug.push((px, py, b.b0, b.b1, b.b2)));

            assert_eq!(
                plain, debug,
                "case {i}: debug coverage/order/barycentrics must match rasterize exactly"
            );
        }
    }

    #[test]
    fn rasterize_debug_marks_a_border_ring_around_a_white_interior() {
        let setup = TriangleSetup::new(
            [10.0, 10.0],
            [90.0, 20.0],
            [30.0, 80.0],
            ClipRect::from_size(100, 100),
            1e-6,
        )
        .expect("rasterizable");

        let mut covered = std::collections::HashSet::new();
        let mut interior_pixels = Vec::new();
        let mut border = 0usize;
        setup.rasterize_debug(|px, py, _, is_border| {
            covered.insert((px, py));
            if is_border {
                border += 1;
            } else {
                interior_pixels.push((px, py));
            }
        });
        let interior = interior_pixels.len();

        assert!(interior > 0, "a large triangle must have an interior");
        assert!(border > 0, "a large triangle must have a border");
        assert!(
            border < interior,
            "border ({border}) should be a thin ring, not the bulk of {interior} interior pixels"
        );

        // An interior pixel is at least one pixel from every edge, so stepping
        // one pixel in any axis direction cannot leave the triangle. If a
        // supposedly-interior pixel sits on the coverage boundary, the border
        // rule is too thin and the outline would have gaps.
        for (px, py) in interior_pixels {
            for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                assert!(
                    covered.contains(&(px + dx, py + dy)),
                    "interior pixel ({px}, {py}) is on the coverage boundary — border rule too thin"
                );
            }
        }
    }

    /// A triangle thinner than the border width is entirely border. This is
    /// deliberate — it is why engines draw the border in a colour that cannot
    /// be mistaken for a cleared framebuffer.
    #[test]
    fn rasterize_debug_marks_a_sub_pixel_sliver_entirely_as_border() {
        let setup = TriangleSetup::new(
            [5.0, 5.0],
            [95.0, 5.4],
            [50.0, 6.2],
            ClipRect::from_size(100, 100),
            1e-6,
        )
        .expect("rasterizable");

        let mut total = 0usize;
        let mut interior = 0usize;
        setup.rasterize_debug(|_, _, _, is_border| {
            total += 1;
            if !is_border {
                interior += 1;
            }
        });

        assert!(total > 0, "the sliver must cover some pixels");
        assert_eq!(
            interior, 0,
            "a sliver has no interior — every pixel is border"
        );
    }

    // ── EdgeFn ────────────────────────────────────────────────────

    /// `eval_pixel` (the precomputed pixel-center form with the tie bias
    /// folded in) must agree with the raw definition evaluated directly on
    /// the snapped subpixel endpoints.
    #[test]
    fn edge_fn_matches_raw_definition_at_pixel_centers() {
        // from (1, 2), to (4, 6) in pixels → snapped subpixels ×16.
        let from = [16i64, 32];
        let to = [64, 96];
        for orient in [1i64, -1] {
            let e = EdgeFn::new(from, to, orient);
            let dx = to[0] - from[0];
            let dy = to[1] - from[1];
            for &(px, py) in &[(0i64, 0i64), (3, 4), (5, 1), (2, 3), (-7, 11)] {
                let sx = 16 * px + 8;
                let sy = 16 * py + 8;
                let raw = (dx * (sy - from[1]) - dy * (sx - from[0])) * orient;
                assert_eq!(
                    e.eval_pixel(px, py) + e.tie,
                    raw,
                    "mismatch at ({px}, {py}) orient {orient}"
                );
            }
        }
    }

    /// The two sides of a shared edge compute exactly negated weights, so a
    /// boundary pixel is claimed by exactly one of them — the FLUX-00124
    /// watertightness invariant at the single-edge level.
    #[test]
    fn shared_edge_weights_are_exact_negations_and_the_tie_goes_to_one_side() {
        let a = [37i64, -95];
        let b = [1811, 407];
        // The triangle on one side sees a→b, the other sees b→a; both are
        // orientation-normalized to interior-positive (orient chosen so each
        // side's third vertex is interior — signs modeled directly here).
        let e_ab = EdgeFn::new(a, b, 1);
        let e_ba = EdgeFn::new(b, a, 1);
        for &(px, py) in &[(0i64, 0), (5, -3), (100, 25), (113, 7)] {
            let w_ab = e_ab.eval_pixel(px, py) + e_ab.tie;
            let w_ba = e_ba.eval_pixel(px, py) + e_ba.tie;
            assert_eq!(w_ab, -w_ba, "unbiased weights must be exact negations");
            if w_ab == 0 {
                let in_ab = e_ab.eval_pixel(px, py) >= 0;
                let in_ba = e_ba.eval_pixel(px, py) >= 0;
                assert!(
                    in_ab != in_ba,
                    "a boundary pixel must be claimed by exactly one side"
                );
            }
        }
        assert_eq!(
            e_ab.tie + e_ba.tie,
            1,
            "exactly one direction of an edge is top-left"
        );
    }

    #[test]
    fn edge_function_sign_convention() {
        // Edge from (0,0) to (10,0) — horizontal, pointing right in Y-down screen space.
        // Point below (y=1) is to the LEFT of the directed edge → positive (CCW side).
        // Point above (y=-1) is to the RIGHT → negative.
        let above = edge_function(0.0, 0.0, 10.0, 0.0, 5.0, -1.0);
        let below = edge_function(0.0, 0.0, 10.0, 0.0, 5.0, 1.0);
        assert!(
            above < 0.0,
            "point above (y=-1) should be negative in Y-down"
        );
        assert!(
            below > 0.0,
            "point below (y=+1) should be positive in Y-down"
        );
    }

    #[test]
    fn edge_function_on_edge_is_zero() {
        // Point exactly on the edge
        let on = edge_function(0.0, 0.0, 10.0, 5.0, 5.0, 2.5);
        assert!(on.abs() < 1e-6);
    }

    // ── TriangleSetup::new ───────────────────────────────────────

    #[test]
    fn degenerate_triangle_returns_none() {
        // Collinear vertices
        let clip = ClipRect::from_size(100, 100);
        assert!(
            TriangleSetup::new([0.0, 0.0], [5.0, 0.0], [10.0, 0.0], clip, f32::EPSILON).is_none()
        );
    }

    #[test]
    fn zero_area_triangle_returns_none() {
        let clip = ClipRect::from_size(100, 100);
        // All vertices at same point
        assert!(
            TriangleSetup::new([5.0, 5.0], [5.0, 5.0], [5.0, 5.0], clip, f32::EPSILON).is_none()
        );
    }

    #[test]
    fn triangle_outside_clip_returns_none() {
        let clip = ClipRect {
            min_x: 0,
            min_y: 0,
            max_x: 10,
            max_y: 10,
        };
        // Triangle entirely to the right of clip
        assert!(
            TriangleSetup::new([20.0, 0.0], [30.0, 0.0], [25.0, 5.0], clip, f32::EPSILON).is_none()
        );
    }

    #[test]
    fn triangle_above_clip_returns_none() {
        let clip = ClipRect {
            min_x: 0,
            min_y: 10,
            max_x: 100,
            max_y: 100,
        };
        assert!(
            TriangleSetup::new([5.0, 0.0], [10.0, 0.0], [7.0, 5.0], clip, f32::EPSILON).is_none()
        );
    }

    #[test]
    fn signed_area_ccw_is_positive() {
        let clip = ClipRect::from_size(100, 100);
        // CCW triangle in screen space (Y-down)
        let setup =
            TriangleSetup::new([10.0, 10.0], [20.0, 10.0], [15.0, 20.0], clip, f32::EPSILON)
                .unwrap();
        assert!(setup.signed_area() > 0.0, "CCW should give positive area");
    }

    #[test]
    fn signed_area_cw_is_negative() {
        let clip = ClipRect::from_size(100, 100);
        // CW triangle (swap v1 and v2 from the CCW case)
        let setup =
            TriangleSetup::new([10.0, 10.0], [15.0, 20.0], [20.0, 10.0], clip, f32::EPSILON)
                .unwrap();
        assert!(setup.signed_area() < 0.0, "CW should give negative area");
    }

    #[test]
    fn custom_epsilon_rejects_small_triangles() {
        let clip = ClipRect::from_size(100, 100);
        // Small triangle that is exactly representable on the 28.4 snap grid
        // (doubled area 0.25 px²).
        let v0 = [50.0, 50.0];
        let v1 = [50.5, 50.0];
        let v2 = [50.0, 50.5];
        // With a tight epsilon, this passes
        assert!(TriangleSetup::new(v0, v1, v2, clip, f32::EPSILON).is_some());
        // With an epsilon above its area, it is rejected (checked on the raw
        // f32 area, preserving each backend's documented epsilon contract)
        assert!(TriangleSetup::new(v0, v1, v2, clip, 0.5).is_none());
        // A triangle smaller than the snap grid collapses to zero snapped
        // area and is rejected regardless of epsilon — real hardware drops
        // geometry below its subpixel precision the same way (FLUX-00124).
        let t1 = [50.01, 50.0];
        let t2 = [50.0, 50.02];
        assert!(TriangleSetup::new(v0, t1, t2, clip, f32::EPSILON).is_none());
    }

    // ── Rasterization ────────────────────────────────────────────

    #[test]
    fn simple_triangle_produces_pixels() {
        let clip = ClipRect::from_size(100, 100);
        let setup =
            TriangleSetup::new([10.0, 10.0], [20.0, 10.0], [15.0, 20.0], clip, f32::EPSILON)
                .unwrap();

        let mut count = 0;
        setup.rasterize(|_px, _py, _bary| count += 1);
        assert!(count > 0, "triangle should produce at least some pixels");
    }

    #[test]
    fn cw_triangle_also_produces_pixels() {
        let clip = ClipRect::from_size(100, 100);
        // Same triangle but CW winding
        let setup =
            TriangleSetup::new([10.0, 10.0], [15.0, 20.0], [20.0, 10.0], clip, f32::EPSILON)
                .unwrap();

        let mut count = 0;
        setup.rasterize(|_px, _py, _bary| count += 1);
        assert!(count > 0, "CW triangle should also produce pixels");
    }

    #[test]
    fn ccw_and_cw_produce_same_pixels() {
        let clip = ClipRect::from_size(100, 100);
        let mut ccw_pixels = Vec::new();
        let mut cw_pixels = Vec::new();

        let setup_ccw =
            TriangleSetup::new([10.0, 10.0], [20.0, 10.0], [15.0, 20.0], clip, f32::EPSILON)
                .unwrap();
        setup_ccw.rasterize(|px, py, _| ccw_pixels.push((px, py)));

        let setup_cw =
            TriangleSetup::new([10.0, 10.0], [15.0, 20.0], [20.0, 10.0], clip, f32::EPSILON)
                .unwrap();
        setup_cw.rasterize(|px, py, _| cw_pixels.push((px, py)));

        assert_eq!(
            ccw_pixels, cw_pixels,
            "CCW and CW should rasterize identical pixels"
        );
    }

    #[test]
    fn barycentric_coords_sum_to_one() {
        let clip = ClipRect::from_size(100, 100);
        let setup =
            TriangleSetup::new([10.0, 10.0], [30.0, 10.0], [20.0, 30.0], clip, f32::EPSILON)
                .unwrap();

        setup.rasterize(|px, py, bary| {
            let sum = bary.b0 + bary.b1 + bary.b2;
            assert!(
                approx_eq(sum, 1.0),
                "bary sum at ({px}, {py}) = {sum}, expected 1.0"
            );
        });
    }

    #[test]
    fn barycentric_coords_non_negative() {
        let clip = ClipRect::from_size(100, 100);
        let setup =
            TriangleSetup::new([10.0, 10.0], [30.0, 10.0], [20.0, 30.0], clip, f32::EPSILON)
                .unwrap();

        setup.rasterize(|px, py, bary| {
            assert!(
                bary.b0 >= 0.0 && bary.b1 >= 0.0 && bary.b2 >= 0.0,
                "negative bary at ({px}, {py}): b0={}, b1={}, b2={}",
                bary.b0,
                bary.b1,
                bary.b2,
            );
        });
    }

    #[test]
    fn vertex_barycentric_coords() {
        let clip = ClipRect::from_size(100, 100);
        // Use pixel-center-aligned vertices so we hit exact vertex positions
        let v0 = [10.5, 10.5];
        let v1 = [20.5, 10.5];
        let v2 = [15.5, 20.5];
        let setup = TriangleSetup::new(v0, v1, v2, clip, f32::EPSILON).unwrap();

        let mut found_v0 = false;
        let mut found_v1 = false;
        let mut found_v2 = false;

        setup.rasterize(|px, py, bary| {
            if px == 10 && py == 10 {
                // At v0: b0 ≈ 1.0
                assert!(approx_eq(bary.b0, 1.0), "at v0: b0={}", bary.b0);
                assert!(approx_eq(bary.b1, 0.0), "at v0: b1={}", bary.b1);
                assert!(approx_eq(bary.b2, 0.0), "at v0: b2={}", bary.b2);
                found_v0 = true;
            }
            if px == 20 && py == 10 {
                // At v1: b1 ≈ 1.0
                assert!(approx_eq(bary.b0, 0.0), "at v1: b0={}", bary.b0);
                assert!(approx_eq(bary.b1, 1.0), "at v1: b1={}", bary.b1);
                assert!(approx_eq(bary.b2, 0.0), "at v1: b2={}", bary.b2);
                found_v1 = true;
            }
            if px == 15 && py == 20 {
                // At v2: b2 ≈ 1.0
                assert!(approx_eq(bary.b0, 0.0), "at v2: b0={}", bary.b0);
                assert!(approx_eq(bary.b1, 0.0), "at v2: b1={}", bary.b1);
                assert!(approx_eq(bary.b2, 1.0), "at v2: b2={}", bary.b2);
                found_v2 = true;
            }
        });

        // Top-left fill rule (FLUX-00124): v0 sits on the triangle's top and
        // left edges, so its pixel is inside. v1 and v2 sit on right edges
        // (w == 0 on a non-top-left edge), so their pixels belong to the
        // hypothetical adjacent triangle and are correctly NOT rasterized.
        assert!(
            found_v0,
            "pixel at v0 (top-left corner) should be rasterized"
        );
        assert!(
            !found_v1,
            "pixel at v1 lies on a right edge — top-left rule excludes it"
        );
        assert!(
            !found_v2,
            "pixel at v2 lies on a right edge — top-left rule excludes it"
        );
    }

    #[test]
    fn clipping_reduces_pixel_count() {
        let full_clip = ClipRect::from_size(100, 100);
        let partial_clip = ClipRect {
            min_x: 13,
            min_y: 13,
            max_x: 18,
            max_y: 18,
        };

        let v0 = [10.0, 10.0];
        let v1 = [20.0, 10.0];
        let v2 = [15.0, 20.0];

        let full = TriangleSetup::new(v0, v1, v2, full_clip, f32::EPSILON).unwrap();
        let partial = TriangleSetup::new(v0, v1, v2, partial_clip, f32::EPSILON).unwrap();

        let mut full_count = 0;
        let mut partial_count = 0;
        full.rasterize(|_, _, _| full_count += 1);
        partial.rasterize(|_, _, _| partial_count += 1);

        assert!(
            partial_count < full_count,
            "clipping should reduce pixel count"
        );
        assert!(
            partial_count > 0,
            "partial clip should still produce some pixels"
        );
    }

    #[test]
    fn clipped_pixels_within_bounds() {
        let clip = ClipRect {
            min_x: 5,
            min_y: 5,
            max_x: 15,
            max_y: 15,
        };
        let setup =
            TriangleSetup::new([0.0, 0.0], [20.0, 0.0], [10.0, 20.0], clip, f32::EPSILON).unwrap();

        setup.rasterize(|px, py, _| {
            assert!(px >= 5 && px < 15, "px={px} out of clip bounds");
            assert!(py >= 5 && py < 15, "py={py} out of clip bounds");
        });
    }

    #[test]
    fn large_triangle_pixel_count() {
        // 50x50 right triangle → area = 50*50/2 = 1250, so ~1250 pixels
        let clip = ClipRect::from_size(200, 200);
        let setup =
            TriangleSetup::new([10.0, 10.0], [60.0, 10.0], [10.0, 60.0], clip, f32::EPSILON)
                .unwrap();

        let mut count = 0;
        setup.rasterize(|_, _, _| count += 1);
        // Should be approximately half of 50x50 = 1250
        assert!(count > 1000, "expected ~1250 pixels, got {count}");
        assert!(count < 1500, "expected ~1250 pixels, got {count}");
    }

    #[test]
    fn pixel_center_sampling() {
        // A very small triangle that contains only pixel centers at exactly +0.5
        let clip = ClipRect::from_size(100, 100);
        let setup =
            TriangleSetup::new([5.0, 5.0], [8.0, 5.0], [6.5, 8.0], clip, f32::EPSILON).unwrap();

        setup.rasterize(|px, py, _| {
            // All rasterized pixels should be within the bounding box
            assert!(px >= 5 && px <= 8, "px={px} outside expected range");
            assert!(py >= 5 && py <= 8, "py={py} outside expected range");
        });
    }

    #[test]
    fn row_major_iteration_order() {
        let clip = ClipRect::from_size(100, 100);
        let setup =
            TriangleSetup::new([10.0, 10.0], [20.0, 10.0], [15.0, 20.0], clip, f32::EPSILON)
                .unwrap();

        let mut pixels = Vec::new();
        setup.rasterize(|px, py, _| pixels.push((px, py)));

        // Verify row-major order: Y should be non-decreasing
        for w in pixels.windows(2) {
            assert!(
                w[1].1 >= w[0].1,
                "not row-major: ({}, {}) before ({}, {})",
                w[0].0,
                w[0].1,
                w[1].0,
                w[1].1,
            );
        }
    }

    // ── ClipRect ─────────────────────────────────────────────────

    #[test]
    fn clip_rect_from_size() {
        let clip = ClipRect::from_size(640, 480);
        assert_eq!(
            clip,
            ClipRect {
                min_x: 0,
                min_y: 0,
                max_x: 640,
                max_y: 480
            }
        );
    }

    // ── Standalone edge_function ─────────────────────────────────

    #[test]
    fn standalone_edge_function_matches_3dfx_convention() {
        // 3dfx: edge_function(x0, y0, x1, y1, px, py) = (x1-x0)*(py-y0) - (y1-y0)*(px-x0)
        let val = edge_function(1.0, 2.0, 4.0, 6.0, 3.0, 3.0);
        let expected = (4.0 - 1.0) * (3.0 - 2.0) - (6.0 - 2.0) * (3.0 - 1.0);
        assert!(approx_eq(val, expected));
    }

    // ── Interpolation accuracy ───────────────────────────────────

    #[test]
    fn barycentric_interpolation_at_centroid() {
        let clip = ClipRect::from_size(200, 200);
        // Vertices chosen so centroid lands on a pixel center (x.5, y.5)
        let v0 = [30.5, 30.5];
        let v1 = [90.5, 30.5];
        let v2 = [60.5, 90.5];

        // Centroid = ((30.5+90.5+60.5)/3, (30.5+30.5+90.5)/3) = (60.5, 50.5)
        let centroid_x = 60.5_f32;
        let centroid_y = 50.5_f32;

        let setup = TriangleSetup::new(v0, v1, v2, clip, f32::EPSILON).unwrap();

        let mut found = false;
        setup.rasterize(|px, py, bary| {
            let cx = px as f32 + 0.5;
            let cy = py as f32 + 0.5;
            if (cx - centroid_x).abs() < 0.01 && (cy - centroid_y).abs() < 0.01 {
                // At centroid, all barycentrics should be ≈ 1/3
                assert!(
                    approx_eq(bary.b0, 1.0 / 3.0)
                        && approx_eq(bary.b1, 1.0 / 3.0)
                        && approx_eq(bary.b2, 1.0 / 3.0),
                    "at centroid ({cx}, {cy}): b0={}, b1={}, b2={}",
                    bary.b0,
                    bary.b1,
                    bary.b2,
                );
                found = true;
            }
        });
        assert!(found, "centroid pixel should be rasterized");
    }

    #[test]
    fn edge_pixel_inclusion() {
        // Triangle with a horizontal top edge — pixels on the edge should be included
        let clip = ClipRect::from_size(100, 100);
        let setup =
            TriangleSetup::new([10.5, 10.5], [20.5, 10.5], [15.5, 20.5], clip, f32::EPSILON)
                .unwrap();

        let mut top_edge_pixels = Vec::new();
        setup.rasterize(|px, py, _| {
            if py == 10 {
                top_edge_pixels.push(px);
            }
        });

        // Pixels along a TOP edge are inside (top-left fill rule). The
        // rightmost vertex pixel also lies on the triangle's right edge, so
        // it is excluded — on a shared mesh it belongs to the neighbour
        // (FLUX-00124).
        assert!(!top_edge_pixels.is_empty(), "top edge should have pixels");
        assert!(
            top_edge_pixels.contains(&10),
            "should include leftmost vertex pixel (top ∩ left)"
        );
        assert!(
            top_edge_pixels.contains(&19),
            "should include interior top-edge pixels"
        );
        assert!(
            !top_edge_pixels.contains(&20),
            "rightmost vertex pixel lies on a right edge — excluded"
        );
    }

    // ── BaryCoords struct ────────────────────────────────────────

    #[test]
    fn bary_coords_debug_clone_copy() {
        let bary = BaryCoords {
            b0: 0.5,
            b1: 0.3,
            b2: 0.2,
        };
        let cloned = bary;
        let dbg = format!("{cloned:?}");
        assert!(dbg.contains("0.5"));
    }
    // ── MDD-BUG-CRUCIBLE-00342: non-finite vertices ───────────────

    /// A well-formed triangle that covers real pixels, used as the baseline
    /// for the poisoning cases below.
    fn healthy_triangle() -> ([f32; 2], [f32; 2], [f32; 2]) {
        ([10.0, 10.0], [100.0, 20.0], [40.0, 120.0])
    }

    /// Every coordinate, poisoned with every non-finite value, must be
    /// rejected at setup. Vertex data reaches this constructor as raw
    /// IEEE-754 bit patterns from guest memory, so all of these are
    /// reachable.
    #[test]
    fn non_finite_coordinates_are_rejected_crucible342() {
        let clip = ClipRect::from_size(320, 200);
        let poisons = [
            ("NaN", f32::NAN),
            ("+inf", f32::INFINITY),
            ("-inf", f32::NEG_INFINITY),
        ];

        for (label, poison) in poisons {
            for vertex in 0..3 {
                for axis in 0..2 {
                    let (mut v0, mut v1, mut v2) = healthy_triangle();
                    match vertex {
                        0 => v0[axis] = poison,
                        1 => v1[axis] = poison,
                        _ => v2[axis] = poison,
                    }
                    assert!(
                        TriangleSetup::new(v0, v1, v2, clip, f32::EPSILON).is_none(),
                        "{label} in v{vertex}[{axis}] must be rejected"
                    );
                }
            }
        }
    }

    /// A non-finite epsilon would defeat the degeneracy test for otherwise
    /// valid geometry, so it is rejected too.
    #[test]
    fn non_finite_epsilon_is_rejected_crucible342() {
        let clip = ClipRect::from_size(320, 200);
        let (v0, v1, v2) = healthy_triangle();
        for poison in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(
                TriangleSetup::new(v0, v1, v2, clip, poison).is_none(),
                "epsilon {poison} must be rejected"
            );
        }
    }

    /// Finite coordinates can still overflow the area computation to
    /// infinity, which would leave `inv_area` at zero and the barycentric
    /// weights meaningless.
    #[test]
    fn overflowing_area_is_rejected_crucible342() {
        let clip = ClipRect::from_size(320, 200);
        let huge = f32::MAX;
        assert!(
            TriangleSetup::new(
                [-huge, -huge],
                [huge, -huge],
                [0.0, huge],
                clip,
                f32::EPSILON
            )
            .is_none(),
            "an area that overflows to infinity must be rejected"
        );
    }

    /// The bug's headline symptom: an all-NaN triangle used to survive setup
    /// with a bounding box covering the WHOLE clip rect, then run the exact
    /// inside test for every pixel and emit none of them. Prove it is
    /// rejected outright, and that no raster callback ever fires.
    #[test]
    fn all_nan_triangle_does_no_full_frame_work_crucible342() {
        let clip = ClipRect::from_size(320, 200);
        let nan = [f32::NAN, f32::NAN];

        let setup = TriangleSetup::new(nan, nan, nan, clip, f32::EPSILON);
        assert!(
            setup.is_none(),
            "an all-NaN triangle must not produce a setup"
        );

        // Belt and braces: if a future change ever lets one through, the
        // callback counter proves whether it scans the frame.
        if let Some(setup) = setup {
            let mut pixels = 0usize;
            setup.rasterize(|_, _, _| pixels += 1);
            assert_eq!(pixels, 0, "an all-NaN triangle must not emit pixels");
        }
    }

    /// Control: the healthy triangle still rasterizes, so the finiteness
    /// gate cannot be rejecting real geometry.
    #[test]
    fn healthy_triangle_still_rasterizes_crucible342() {
        let clip = ClipRect::from_size(320, 200);
        let (v0, v1, v2) = healthy_triangle();
        let setup = TriangleSetup::new(v0, v1, v2, clip, f32::EPSILON)
            .expect("a well-formed triangle must still set up");
        let mut pixels = 0usize;
        setup.rasterize(|_, _, _| pixels += 1);
        assert!(pixels > 0, "the control triangle must cover pixels");
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod aa_tests {
    use super::*;

    /// Asymmetric, axis-unaligned triangle well inside the clip.
    fn tri() -> ([f32; 2], [f32; 2], [f32; 2]) {
        ([12.3, 8.7], [58.9, 15.2], [27.4, 47.6])
    }

    #[test]
    fn new_aa_dilates_the_box_one_pixel_inside_clip() {
        let clip = ClipRect::from_size(100, 100);
        let (v0, v1, v2) = tri();
        let plain = TriangleSetup::new(v0, v1, v2, clip, f32::EPSILON).unwrap();
        let aa = TriangleSetup::new_aa(v0, v1, v2, clip, f32::EPSILON).unwrap();
        assert_eq!(aa.min_x, plain.min_x - 1);
        assert_eq!(aa.min_y, plain.min_y - 1);
        assert_eq!(aa.max_x, plain.max_x + 1);
        assert_eq!(aa.max_y, plain.max_y + 1);
        // Clip still binds: a triangle touching the clip edge cannot dilate
        // past it.
        let touching =
            TriangleSetup::new_aa([0.5, 0.5], [30.0, 1.0], [1.0, 30.0], clip, f32::EPSILON)
                .unwrap();
        assert_eq!(touching.min_x, 0);
        assert_eq!(touching.min_y, 0);
    }

    #[test]
    fn interior_pixels_have_coverage_exactly_one() {
        let clip = ClipRect::from_size(100, 100);
        let (v0, v1, v2) = tri();
        let aa = TriangleSetup::new_aa(v0, v1, v2, clip, f32::EPSILON).unwrap();
        // The centroid pixel is far from every edge.
        let cx = ((v0[0] + v1[0] + v2[0]) / 3.0) as i32;
        let cy = ((v0[1] + v1[1] + v2[1]) / 3.0) as i32;
        let mut centroid_cov = None;
        let mut interior = 0usize;
        aa.rasterize_aa(|px, py, _, c| {
            if (px, py) == (cx, cy) {
                centroid_cov = Some(c);
            }
            if c == 1.0 {
                interior += 1;
            }
        });
        assert_eq!(centroid_cov, Some(1.0), "centroid must be fully covered");
        assert!(interior > 100, "a 47x39 triangle has a large full interior");
    }

    #[test]
    fn coverage_fades_monotonically_across_an_edge() {
        // Vertical left edge at x = 20.25: walking +x through it, coverage
        // must rise monotonically from 0 to 1 within about a pixel.
        let clip = ClipRect::from_size(100, 100);
        let aa = TriangleSetup::new_aa(
            [20.25, 5.0],
            [20.25, 60.0],
            [80.0, 30.0],
            clip,
            f32::EPSILON,
        )
        .unwrap();
        let mut row: Vec<(i32, f32)> = Vec::new();
        aa.rasterize_aa(|px, py, _, c| {
            if py == 30 {
                row.push((px, c));
            }
        });
        assert!(row.len() > 10, "row 30 crosses the triangle");
        let boundary: Vec<f32> = row
            .iter()
            .filter(|&&(px, _)| (19..=22).contains(&px))
            .map(|&(_, c)| c)
            .collect();
        assert!(
            boundary.windows(2).all(|w| w[0] <= w[1]),
            "coverage must not decrease entering the triangle: {boundary:?}"
        );
        assert!(
            boundary.iter().any(|&c| c > 0.0 && c < 1.0),
            "an edge not on a pixel-centre grid line must produce at least \
             one fractional pixel: {boundary:?}"
        );
    }

    /// Integral property: total coverage approximates the geometric area.
    /// (signed_area is 2x the geometric area.)
    #[test]
    fn total_coverage_approximates_the_triangle_area() {
        let clip = ClipRect::from_size(100, 100);
        let (v0, v1, v2) = tri();
        let aa = TriangleSetup::new_aa(v0, v1, v2, clip, f32::EPSILON).unwrap();
        let mut total = 0.0f64;
        aa.rasterize_aa(|_, _, _, c| total += c as f64);
        let geometric = (aa.signed_area().abs() / 2.0) as f64;
        let err = (total - geometric).abs() / geometric;
        assert!(
            err < 0.02,
            "total coverage {total:.1} vs area {geometric:.1} (err {err:.3})"
        );
    }

    /// The AA traversal visits a superset of the exact traversal, and its
    /// fully-covered pixels agree with the exact inside test.
    #[test]
    fn aa_traversal_is_a_superset_of_the_exact_one() {
        let clip = ClipRect::from_size(100, 100);
        let (v0, v1, v2) = tri();
        let plain = TriangleSetup::new(v0, v1, v2, clip, f32::EPSILON).unwrap();
        let aa = TriangleSetup::new_aa(v0, v1, v2, clip, f32::EPSILON).unwrap();
        let mut exact = std::collections::HashSet::new();
        plain.rasterize(|px, py, _| {
            exact.insert((px, py));
        });
        let mut aa_pixels = std::collections::HashSet::new();
        aa.rasterize_aa(|px, py, _, c| {
            aa_pixels.insert((px, py));
            if c == 1.0 {
                assert!(
                    exact.contains(&(px, py)),
                    "fully covered ({px},{py}) must be inside the exact triangle"
                );
            }
        });
        assert!(
            aa_pixels.is_superset(&exact),
            "AA must visit every exactly-inside pixel"
        );
        assert!(
            aa_pixels.len() > exact.len(),
            "AA must also visit a boundary ring"
        );
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod runs_equivalence_tests {
    use super::*;

    /// Verbatim copy of the production per-pixel traversal (integer
    /// coverage + f32 seam stepping), kept as the bit-exactness oracle
    /// for `rasterize` / `rasterize_runs`.
    fn reference_rasterize<F>(s: &TriangleSetup, mut pixel_fn: F)
    where
        F: FnMut(i32, i32, BaryCoords),
    {
        let mut w0_row = s.edge[0].eval_pixel(i64::from(s.min_x), i64::from(s.min_y));
        let mut w1_row = s.edge[1].eval_pixel(i64::from(s.min_x), i64::from(s.min_y));
        let mut w2_row = s.edge[2].eval_pixel(i64::from(s.min_x), i64::from(s.min_y));

        let min_x_f = f64::from(s.min_x);
        let max_x_f = f64::from(s.max_x);
        let [fa0, fa1, fa2] = s.x_steps();

        for py in s.min_y..s.max_y {
            let mut scan_left = min_x_f;
            let mut scan_right = max_x_f;

            if s.edge[0].step_x != 0 {
                let x0 = min_x_f - w0_row as f64 / s.edge[0].step_x as f64;
                if s.is_left[0] {
                    scan_left = scan_left.max(x0);
                } else {
                    scan_right = scan_right.min(x0);
                }
            } else if w0_row < 0 {
                scan_right = f64::NEG_INFINITY;
            }
            if s.edge[1].step_x != 0 {
                let x1 = min_x_f - w1_row as f64 / s.edge[1].step_x as f64;
                if s.is_left[1] {
                    scan_left = scan_left.max(x1);
                } else {
                    scan_right = scan_right.min(x1);
                }
            } else if w1_row < 0 {
                scan_right = f64::NEG_INFINITY;
            }
            if s.edge[2].step_x != 0 {
                let x2 = min_x_f - w2_row as f64 / s.edge[2].step_x as f64;
                if s.is_left[2] {
                    scan_left = scan_left.max(x2);
                } else {
                    scan_right = scan_right.min(x2);
                }
            } else if w2_row < 0 {
                scan_right = f64::NEG_INFINITY;
            }

            // Mirror the production widening (MDD-BUG-CRUCIBLE-00522). This
            // reference exists to pin the INCREMENTAL WEIGHT arithmetic bit
            // for bit, so it must visit the same pixels; it is deliberately
            // NOT the coverage oracle. Coverage is checked against
            // `brute_force_covered`, which uses no scan bounds at all — the
            // bug report noted that this reference copying the production
            // bound is exactly why the old equivalence tests reproduced the
            // omission instead of catching it.
            let scan_left_i = (scan_left.ceil() as i32).saturating_sub(1).max(s.min_x);
            let scan_right_i = ((scan_right.floor() as i32).saturating_add(2)).min(s.max_x);

            if scan_left_i < scan_right_i {
                let offset = i64::from(scan_left_i - s.min_x);
                let mut i0 = w0_row + offset * s.edge[0].step_x;
                let mut i1 = w1_row + offset * s.edge[1].step_x;
                let mut i2 = w2_row + offset * s.edge[2].step_x;
                let mut w0 = (i0 + s.edge[0].tie) as f32 * s.seam_scale;
                let mut w1 = (i1 + s.edge[1].tie) as f32 * s.seam_scale;
                let mut w2 = (i2 + s.edge[2].tie) as f32 * s.seam_scale;

                for px in scan_left_i..scan_right_i {
                    let inside = i0 >= 0 && i1 >= 0 && i2 >= 0;

                    if inside {
                        let b0 = w0 * s.inv_area;
                        let b1 = w1 * s.inv_area;
                        let b2 = w2 * s.inv_area;
                        pixel_fn(px, py, BaryCoords { b0, b1, b2 });
                    }

                    i0 += s.edge[0].step_x;
                    i1 += s.edge[1].step_x;
                    i2 += s.edge[2].step_x;
                    w0 += fa0;
                    w1 += fa1;
                    w2 += fa2;
                }
            }

            w0_row += s.edge[0].step_y;
            w1_row += s.edge[1].step_y;
            w2_row += s.edge[2].step_y;
        }
    }

    /// Triangle corpus: windings, slivers, clipped, horizontal edges,
    /// sub-pixel geometry, and large triangles.
    fn corpus() -> Vec<([f32; 2], [f32; 2], [f32; 2])> {
        vec![
            // Large CCW / CW pair (screen-space Y-down)
            ([10.3, 5.7], [100.9, 20.2], [40.4, 90.8]),
            ([10.3, 5.7], [40.4, 90.8], [100.9, 20.2]),
            // Vertex rotations of the same triangle
            ([100.9, 20.2], [40.4, 90.8], [10.3, 5.7]),
            ([40.4, 90.8], [10.3, 5.7], [100.9, 20.2]),
            // Thin sliver crossing many columns
            ([0.2, 0.1], [120.7, 1.9], [0.4, 2.3]),
            // Sub-pixel-height sliver
            ([5.1, 5.1], [90.7, 5.35], [5.2, 5.9]),
            // Horizontal top edge
            ([0.0, 10.0], [50.0, 10.0], [25.0, 60.0]),
            // Horizontal bottom edge
            ([0.0, 60.0], [50.0, 60.0], [25.0, 10.0]),
            // Extends past every clip boundary
            ([-40.5, -30.25], [160.75, 10.5], [30.25, 170.0]),
            // Tiny but non-degenerate
            ([7.4, 7.6], [8.9, 7.7], [7.9, 9.2]),
            // Right-angle axis-aligned
            ([0.5, 0.5], [63.5, 0.5], [0.5, 63.5]),
            // Vertex exactly on pixel centers
            ([2.5, 2.5], [60.5, 4.5], [30.5, 58.5]),
        ]
    }

    fn clips() -> Vec<ClipRect> {
        vec![
            ClipRect {
                min_x: 0,
                min_y: 0,
                max_x: 128,
                max_y: 128,
            },
            ClipRect {
                min_x: 0,
                min_y: 0,
                max_x: 64,
                max_y: 64,
            },
            ClipRect {
                min_x: 8,
                min_y: 16,
                max_x: 72,
                max_y: 80,
            },
        ]
    }

    #[test]
    fn rasterize_is_bit_identical_to_reference() {
        let mut cases = 0usize;
        for (v0, v1, v2) in corpus() {
            for clip in clips() {
                let Some(setup) = TriangleSetup::new(v0, v1, v2, clip, 1e-4) else {
                    continue;
                };
                let mut reference: Vec<(i32, i32, u32, u32, u32)> = Vec::new();
                reference_rasterize(&setup, |px, py, b| {
                    reference.push((px, py, b.b0.to_bits(), b.b1.to_bits(), b.b2.to_bits()));
                });
                let mut actual: Vec<(i32, i32, u32, u32, u32)> = Vec::new();
                setup.rasterize(|px, py, b| {
                    actual.push((px, py, b.b0.to_bits(), b.b1.to_bits(), b.b2.to_bits()));
                });
                assert_eq!(
                    reference, actual,
                    "pixel stream diverged for tri {v0:?},{v1:?},{v2:?} clip {clip:?}"
                );
                cases += 1;
                if !reference.is_empty() {
                    // Count only non-empty rasterizations as meaningful cases.
                }
            }
        }
        assert!(cases > 20, "corpus unexpectedly small: {cases}");
    }

    #[test]
    fn lanes4_are_bit_identical_to_reference() {
        let mut cases = 0usize;
        for (v0, v1, v2) in corpus() {
            for clip in clips() {
                let Some(setup) = TriangleSetup::new(v0, v1, v2, clip, 1e-4) else {
                    continue;
                };
                let mut reference: Vec<(i32, i32, u32, u32, u32)> = Vec::new();
                reference_rasterize(&setup, |px, py, b| {
                    reference.push((px, py, b.b0.to_bits(), b.b1.to_bits(), b.b2.to_bits()));
                });
                let mut lanes: Vec<(i32, i32, u32, u32, u32)> = Vec::new();
                setup.rasterize_lanes4(|py, x, n, mask, b0, b1, b2| {
                    assert!((1..=4).contains(&n));
                    assert_ne!(mask, 0);
                    assert_eq!(mask & !((1u8 << n) - 1), 0);
                    for lane in 0..n {
                        if mask & (1 << lane) != 0 {
                            lanes.push((
                                x + lane as i32,
                                py,
                                b0[lane].to_bits(),
                                b1[lane].to_bits(),
                                b2[lane].to_bits(),
                            ));
                        }
                    }
                });
                assert_eq!(
                    reference, lanes,
                    "lane stream diverged for tri {v0:?},{v1:?},{v2:?} clip {clip:?}"
                );
                cases += 1;
            }
        }
        assert!(cases > 20, "corpus unexpectedly small: {cases}");
    }

    #[test]
    fn runs_are_maximal_and_cover_reference_pixels() {
        for (v0, v1, v2) in corpus() {
            for clip in clips() {
                let Some(setup) = TriangleSetup::new(v0, v1, v2, clip, 1e-4) else {
                    continue;
                };
                let mut reference_px: Vec<(i32, i32)> = Vec::new();
                reference_rasterize(&setup, |px, py, _| reference_px.push((px, py)));

                let mut run_px: Vec<(i32, i32)> = Vec::new();
                let mut prev_run_end: Option<(i32, i32)> = None;
                setup.rasterize_runs(|py, x_start, x_end, w| {
                    assert!(x_start < x_end, "empty run emitted");
                    // Maximality: a new run on the same row must not be
                    // adjacent to the previous one (else they'd be one run).
                    if let Some((prev_y, prev_end)) = prev_run_end {
                        if prev_y == py {
                            assert!(
                                x_start > prev_end,
                                "adjacent runs not merged on row {py}: {prev_end} -> {x_start}"
                            );
                        }
                    }
                    prev_run_end = Some((py, x_end));
                    // Start weights normalize to the reference barycentrics.
                    let b0 = w[0] * setup.inv_area();
                    assert!(b0.is_finite());
                    for px in x_start..x_end {
                        run_px.push((px, py));
                    }
                });
                assert_eq!(
                    reference_px, run_px,
                    "run coverage diverged for tri {v0:?},{v1:?},{v2:?} clip {clip:?}"
                );
            }
        }
    }
}

// ── Conservative scan bounds (MDD-BUG-CRUCIBLE-00522) ─────────

#[cfg(test)]
mod conservative_scan_bounds_tests {
    use super::*;

    /// Coverage by brute force: every pixel in the clip rect, tested exactly.
    ///
    /// The independent oracle this bug asked for. It computes NO scan bounds —
    /// it walks the whole clip rect and evaluates the three edge functions at
    /// each pixel centre. That independence is the entire point: the existing
    /// `reference_rasterize` derives its range the same way production does,
    /// so an equivalence check between them REPRODUCES a bound that is one
    /// pixel too tight instead of detecting it. This oracle cannot, because it
    /// has no bound to get wrong.
    fn brute_force_covered(s: &TriangleSetup, clip: ClipRect) -> Vec<(i32, i32)> {
        let mut covered = Vec::new();
        for py in clip.min_y..clip.max_y {
            for px in clip.min_x..clip.max_x {
                // Direct integer evaluation — exact, no incremental stepping,
                // so it also cross-checks the production stepping arithmetic.
                let inside = s.edge[0].eval_pixel(i64::from(px), i64::from(py)) >= 0
                    && s.edge[1].eval_pixel(i64::from(px), i64::from(py)) >= 0
                    && s.edge[2].eval_pixel(i64::from(px), i64::from(py)) >= 0;
                if inside {
                    covered.push((px, py));
                }
            }
        }
        covered
    }

    fn production_covered(s: &TriangleSetup) -> Vec<(i32, i32)> {
        let mut covered = Vec::new();
        s.rasterize(|px, py, _bary| covered.push((px, py)));
        covered.sort_unstable_by_key(|&(px, py)| (py, px));
        covered
    }

    /// Construct a vertical right edge through pixel centres. Its boundary
    /// belongs to the adjacent triangle under the top-left rule, while the
    /// preceding column must remain covered by conservative scan bounds.
    #[test]
    fn right_edge_pixel_and_inside_neighbor_match_exact_coverage() {
        let clip = ClipRect::from_size(32, 32);
        let setup = TriangleSetup::new([2.5, 2.5], [18.5, 26.5], [18.5, 2.5], clip, f32::EPSILON)
            .expect("constructed triangle is non-degenerate");

        let expected = brute_force_covered(&setup, clip);
        let actual = production_covered(&setup);

        assert!(
            !expected.contains(&(18, 10)),
            "the right edge belongs to the adjacent triangle"
        );
        assert!(
            expected.contains(&(17, 10)),
            "the preceding column is strictly inside the triangle"
        );
        assert!(
            actual.contains(&(17, 10)),
            "conservative scan bounds must visit the inside neighbor"
        );
        assert_eq!(
            actual, expected,
            "production coverage must match the exact half-space test everywhere"
        );
    }

    /// Widening the bound must not INVENT pixels either.
    ///
    /// The complement row: the exact per-pixel test is what makes a
    /// conservative bound safe, so a triangle nowhere near its bounding-box
    /// edges must still produce exactly the half-space answer.
    #[test]
    fn widened_bounds_do_not_cover_pixels_outside_the_triangle() {
        let clip = ClipRect::from_size(64, 64);
        let setup =
            TriangleSetup::new([10.5, 10.5], [40.5, 12.5], [20.5, 45.5], clip, f32::EPSILON)
                .expect("non-degenerate");

        let expected = brute_force_covered(&setup, clip);
        let actual = production_covered(&setup);

        assert!(!expected.is_empty(), "fixture must cover something");
        assert_eq!(actual, expected);
    }

    /// A triangle clipped hard against the left and top edges still stays
    /// inside the clip rect after widening.
    ///
    /// Widening subtracts one from the left bound, so the clamp to `min_x` is
    /// what keeps it in range; this pins that the result never escapes the
    /// clip rect and still matches the exact test.
    #[test]
    fn widening_stays_within_the_clip_rect_at_the_origin() {
        let clip = ClipRect::from_size(32, 32);
        let setup = TriangleSetup::new([-8.0, -8.0], [20.5, 2.5], [2.5, 20.5], clip, f32::EPSILON)
            .expect("non-degenerate");

        let actual = production_covered(&setup);
        for &(px, py) in &actual {
            assert!(
                px >= clip.min_x && px < clip.max_x && py >= clip.min_y && py < clip.max_y,
                "pixel ({px}, {py}) escaped the clip rect"
            );
        }
        assert_eq!(actual, brute_force_covered(&setup, clip));
    }
}

// ── Watertightness (MDD-BUG-FLUX-00124) ───────────────────────

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod watertight_tests {
    use super::*;
    use std::collections::HashMap;

    /// Count how many triangles of `tris` claim each pixel.
    fn claim_counts(
        tris: &[([f32; 2], [f32; 2], [f32; 2])],
        clip: ClipRect,
    ) -> HashMap<(i32, i32), u32> {
        let mut counts = HashMap::new();
        for &(v0, v1, v2) in tris {
            if let Some(setup) = TriangleSetup::new(v0, v1, v2, clip, 1e-6) {
                setup.rasterize(|px, py, _| *counts.entry((px, py)).or_insert(0) += 1);
            }
        }
        counts
    }

    /// The defect's shape: a quad split into two triangles along a diagonal.
    /// Every pixel of the quad must be claimed by EXACTLY one triangle — a
    /// zero-claim pixel is the flickering black hairline of FLUX-00124, a
    /// double-claim pixel is the double-blend artifact on shared edges.
    /// Coordinates deliberately include screen-magnitude offsets and awkward
    /// fractions: under the old independently-rounded f32 edge functions,
    /// exactly this family produced tie pixels rejected by both triangles.
    #[test]
    fn quad_diagonals_partition_every_interior_pixel_exactly_once() {
        let clip = ClipRect::from_size(1024, 1024);
        type Quad = ([f32; 2], [f32; 2], [f32; 2], [f32; 2]);
        let quads: &[Quad] = &[
            // (a, b, c, d) as a fan a-b-c, a-c-d — diagonal a-c is shared.
            (
                [100.0, 100.0],
                [900.3, 141.7],
                [873.9, 811.1],
                [130.2, 700.6],
            ),
            (
                [631.9375, 12.0625],
                [1010.5, 400.25],
                [512.125, 1000.875],
                [14.4375, 505.5625],
            ),
            // Long thin quad — the sliver family radiating from fan centres.
            (
                [500.25, 500.75],
                [1015.9, 502.1],
                [1013.3, 509.7],
                [501.1, 503.4],
            ),
            // Near-degenerate diagonal split at odd subpixel positions.
            (
                [3.03125, 7.96875],
                [1020.4, 3.6],
                [1019.7, 1015.2],
                [8.8, 1013.9],
            ),
        ];
        for &(a, b, c, d) in quads {
            let tris = [(a, b, c), (a, c, d)];
            let counts = claim_counts(&tris, clip);
            for (&(px, py), &n) in &counts {
                assert_eq!(
                    n, 1,
                    "pixel ({px}, {py}) claimed {n} times across the shared \
                     diagonal of quad {a:?} {b:?} {c:?} {d:?}"
                );
            }
            assert!(!counts.is_empty(), "quad produced no pixels at all");
        }
    }

    /// A triangle fan around an interior point. No
    /// pixel may be claimed twice, and no interior pixel may go unclaimed
    /// (checked against an exact whole-polygon test on the snapped hull).
    #[test]
    fn triangle_fan_leaves_no_gaps_at_shared_spokes() {
        let clip = ClipRect::from_size(1024, 1024);
        let center = [517.34f32, 489.77f32];
        // 12-spoke irregular fan (convex), radii varied to make slivers.
        let n = 12usize;
        let mut rim: Vec<[f32; 2]> = Vec::new();
        for i in 0..n {
            // Deterministic irregular angles/radii (no RNG in tests).
            let ang = (i as f32) * 0.51 + 0.05 * ((i * i % 7) as f32);
            let r = 180.0 + 37.0 * ((i * 5 % 11) as f32);
            rim.push([center[0] + r * ang.cos(), center[1] + r * ang.sin()]);
        }
        let tris: Vec<_> = (0..n).map(|i| (center, rim[i], rim[(i + 1) % n])).collect();
        let counts = claim_counts(&tris, clip);

        for (&(px, py), &cnt) in &counts {
            assert_eq!(cnt, 1, "pixel ({px}, {py}) claimed {cnt} times in the fan");
        }

        // Exact hull coverage on the snapped rim: a pixel strictly inside
        // every rim edge (by more than one snapped subpixel²) must have been
        // claimed by some fan triangle.
        let snap = |v: [f32; 2]| -> [i64; 2] {
            [
                (f64::from(v[0]) * 16.0).round() as i64,
                (f64::from(v[1]) * 16.0).round() as i64,
            ]
        };
        let rim_s: Vec<[i64; 2]> = rim.iter().map(|&v| snap(v)).collect();
        let mut missing = Vec::new();
        for py in 0..1024i32 {
            for px in 0..1024i32 {
                let sx = 16 * i64::from(px) + 8;
                let sy = 16 * i64::from(py) + 8;
                let strictly_inside = (0..n).all(|i| {
                    let f = rim_s[i];
                    let t = rim_s[(i + 1) % n];
                    let w = (t[0] - f[0]) * (sy - f[1]) - (t[1] - f[1]) * (sx - f[0]);
                    w > 0
                }) || (0..n).all(|i| {
                    let f = rim_s[i];
                    let t = rim_s[(i + 1) % n];
                    let w = (t[0] - f[0]) * (sy - f[1]) - (t[1] - f[1]) * (sx - f[0]);
                    w < 0
                });
                if strictly_inside && !counts.contains_key(&(px, py)) {
                    missing.push((px, py));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "interior pixels unclaimed by every fan triangle (black hairlines): {missing:?}"
        );
    }
}
