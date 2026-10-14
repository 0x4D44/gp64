//! Pure software scaling of a frozen RGB frame into a letterboxed host buffer.
use crate::crt::CrtFilter;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scaler {
    #[default]
    Integer,
    Smooth,
    Area,
    Crt,
}
impl Scaler {
    /// Every mode in menu order; a new mode joins the cycle by being listed here.
    pub const ALL: [Scaler; 4] = [Scaler::Integer, Scaler::Smooth, Scaler::Area, Scaler::Crt];
    pub fn label(self) -> &'static str {
        match self {
            Scaler::Integer => "INTEGER",
            Scaler::Smooth => "SMOOTH",
            Scaler::Area => "AREA",
            Scaler::Crt => "CRT",
        }
    }
    /// The neighbouring mode in [`Scaler::ALL`], wrapping at both ends.
    pub fn cycle(self, forward: bool) -> Scaler {
        let index = Scaler::ALL
            .iter()
            .position(|&mode| mode == self)
            .unwrap_or(0);
        let count = Scaler::ALL.len();
        Scaler::ALL[if forward {
            (index + 1) % count
        } else {
            (index + count - 1) % count
        }]
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct Viewport {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}
pub fn viewport(source_width: usize, width: usize, height: usize, scaler: Scaler) -> Viewport {
    let (mut w, mut h) = if width * 3 <= height * 4 {
        (width, width * 3 / 4)
    } else {
        (height * 4 / 3, height)
    };
    let corrected_height = source_width * 3 / 4;
    if scaler == Scaler::Integer && source_width > 0 && corrected_height > 0 {
        let factor = (width / source_width).min(height / corrected_height);
        if factor > 0 {
            w = source_width * factor;
            h = corrected_height * factor;
        }
    }
    Viewport {
        x: (width - w) / 2,
        y: (height - h) / 2,
        width: w,
        height: h,
    }
}

/// Viewports larger than this many pixels are scaled on several threads.
const PARALLEL_MIN_PIXELS: usize = 640 * 480;
const MAX_THREADS: usize = 8;

/// Worker count for a viewport: `requested` when non-zero, else the machine's
/// parallelism (capped) for large viewports and one for small ones.
pub(crate) fn thread_count(requested: usize, pixels: usize) -> usize {
    if requested > 0 {
        return requested;
    }
    if pixels <= PARALLEL_MIN_PIXELS {
        return 1;
    }
    static AUTO: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *AUTO.get_or_init(|| {
        std::thread::available_parallelism().map_or(1, |n| n.get().min(MAX_THREADS))
    })
}

/// Run `work(first_row, rows)` over `count` full-width rows of `target`
/// starting at row `y0`, split into contiguous chunks across `threads`.
/// `rows` holds whole rows, so each call sees a disjoint slice.
pub(crate) fn par_rows<F>(
    target: &mut [u32],
    width: usize,
    y0: usize,
    count: usize,
    threads: usize,
    work: F,
) where
    F: Fn(usize, &mut [u32]) + Sync,
{
    let region = &mut target[y0 * width..(y0 + count) * width];
    let threads = threads.clamp(1, count.max(1));
    if threads == 1 {
        work(0, region);
        return;
    }
    let per = count.div_ceil(threads);
    std::thread::scope(|scope| {
        let work = &work;
        let mut rest = region;
        let mut first = 0;
        while first < count {
            let rows = per.min(count - first);
            let (chunk, tail) = rest.split_at_mut(rows * width);
            rest = tail;
            let start = first;
            first += rows;
            if first >= count {
                work(start, chunk);
            } else {
                scope.spawn(move || work(start, chunk));
            }
        }
    });
}

/// One output coordinate's run of source pixels, with integer weights in
/// `weights[start..start + len]` (offsets into the owning [`Axis`]).
#[derive(Clone, Copy)]
struct Span {
    first: u32,
    len: u32,
    offset: u32,
}

/// Box-filter spans along one axis. Output pixel `o` covers the source
/// interval `[o * s, (o + 1) * s)` measured in `1 / out` source-pixel units,
/// so every weight is an exact integer and a span's weights sum to `s`.
struct Axis {
    spans: Vec<Span>,
    weights: Vec<u32>,
}

impl Axis {
    fn area(source: usize, out: usize) -> Axis {
        let mut spans = Vec::with_capacity(out);
        let mut weights = Vec::new();
        for o in 0..out {
            let lo = o * source;
            let hi = lo + source;
            let first = lo / out;
            let last = (hi - 1) / out;
            spans.push(Span {
                first: first as u32,
                len: (last - first + 1) as u32,
                offset: weights.len() as u32,
            });
            for i in first..=last {
                weights.push((hi.min((i + 1) * out) - lo.max(i * out)) as u32);
            }
        }
        Axis { spans, weights }
    }
}

/// Per-axis bilinear sample positions: a source index and a 16-bit fraction.
fn bilinear_axis(source: usize, out: usize) -> Vec<(u32, u32)> {
    let den = out.saturating_sub(1).max(1) as u64;
    (0..out)
        .map(|o| {
            let num = o as u64 * source.saturating_sub(1) as u64;
            (
                (num / den) as u32,
                ((((num % den) << 16) + den / 2) / den) as u32,
            )
        })
        .collect()
}

/// Horizontally interpolate one source row into 16.16-weighted channel sums.
fn smooth_row(row: &[u32], cols: &[(u32, u32)], out: &mut [[u32; 3]]) {
    let last = row.len() - 1;
    for (sums, &(ix, fx)) in out.iter_mut().zip(cols) {
        let ix = ix as usize;
        let (a, b) = (row[ix], row[(ix + 1).min(last)]);
        for (c, sum) in sums.iter_mut().enumerate() {
            let shift = 8 * c;
            *sum = ((a >> shift) & 255) * (65536 - fx) + ((b >> shift) & 255) * fx;
        }
    }
}

struct AreaPlan {
    key: (usize, usize, usize, usize),
    cols: Axis,
    rows: Axis,
    /// `2^40 / (source_width * source_height)`, rounded up.
    reciprocal: u64,
}

struct SmoothPlan {
    key: (usize, usize, usize, usize),
    cols: Vec<(u32, u32)>,
    rows: Vec<(u32, u32)>,
}

/// Scaler state owned by the UI thread: lookup tables that are rebuilt only
/// when the source or viewport size changes.
#[derive(Default)]
pub struct Blitter {
    area: Option<AreaPlan>,
    smooth: Option<SmoothPlan>,
    crt: CrtFilter,
    /// Worker threads; 0 picks automatically from the viewport size.
    threads: usize,
}

impl Blitter {
    pub fn blit(
        &mut self,
        source: &[u32],
        sw: usize,
        sh: usize,
        target: &mut [u32],
        width: usize,
        height: usize,
        scaler: Scaler,
    ) {
        target.fill(0);
        if sw == 0 || sh == 0 || source.len() < sw * sh || target.len() < width * height {
            return;
        }
        let view = viewport(sw, width, height, scaler);
        if view.width == 0 || view.height == 0 {
            return;
        }
        match scaler {
            Scaler::Integer => {
                let columns: Vec<usize> = (0..view.width).map(|x| x * sw / view.width).collect();
                let threads = thread_count(self.threads, view.width * view.height);
                par_rows(
                    target,
                    width,
                    view.y,
                    view.height,
                    threads,
                    |first, rows| {
                        for (i, line) in rows.chunks_exact_mut(width).enumerate() {
                            let row = &source[((first + i) * sh / view.height) * sw..][..sw];
                            for (pixel, &column) in
                                line[view.x..][..view.width].iter_mut().zip(&columns)
                            {
                                *pixel = row[column];
                            }
                        }
                    },
                );
            }
            Scaler::Smooth => self.smooth(source, sw, sh, target, width, &view),
            Scaler::Area => self.area(source, sw, sh, target, width, &view),
            Scaler::Crt => {
                self.crt.threads = self.threads;
                self.crt.apply(
                    source,
                    sw,
                    sh,
                    target,
                    width,
                    height,
                    (view.width, view.height, view.x, view.y),
                )
            }
        }
    }

    fn smooth(
        &mut self,
        source: &[u32],
        sw: usize,
        sh: usize,
        target: &mut [u32],
        width: usize,
        view: &Viewport,
    ) {
        let key = (sw, sh, view.width, view.height);
        if self.smooth.as_ref().is_none_or(|plan| plan.key != key) {
            self.smooth = Some(SmoothPlan {
                key,
                cols: bilinear_axis(sw, view.width),
                rows: bilinear_axis(sh, view.height),
            });
        }
        let plan = self.smooth.as_ref().expect("plan was just built");
        let threads = thread_count(self.threads, view.width * view.height);
        par_rows(
            target,
            width,
            view.y,
            view.height,
            threads,
            |first, rows| {
                // Horizontal passes are shared by every output row that straddles
                // the same pair of source rows, so each source row is filtered once.
                let mut slots: [(usize, Vec<[u32; 3]>); 2] = [
                    (usize::MAX, vec![[0; 3]; view.width]),
                    (usize::MAX, vec![[0; 3]; view.width]),
                ];
                for (i, line) in rows.chunks_exact_mut(width).enumerate() {
                    let (iy, fy) = plan.rows[first + i];
                    let top = iy as usize;
                    let bottom = (top + 1).min(sh - 1);
                    for need in [top, bottom] {
                        if !slots.iter().any(|slot| slot.0 == need) {
                            let victim = slots
                                .iter()
                                .position(|slot| slot.0 != top && slot.0 != bottom)
                                .expect("one slot is always free");
                            slots[victim].0 = need;
                            smooth_row(
                                &source[need * sw..][..sw],
                                &plan.cols,
                                &mut slots[victim].1,
                            );
                        }
                    }
                    let upper = &slots.iter().find(|slot| slot.0 == top).expect("filtered").1;
                    let lower = &slots
                        .iter()
                        .find(|slot| slot.0 == bottom)
                        .expect("filtered")
                        .1;
                    let (wt, wb) = (u64::from(65536 - fy), u64::from(fy));
                    let out = &mut line[view.x..][..view.width];
                    for ((pixel, u), l) in out.iter_mut().zip(upper).zip(lower) {
                        let channel = |c: usize| {
                            (((u64::from(u[c]) * wt + u64::from(l[c]) * wb + (1 << 31)) >> 32)
                                as u32)
                                << (8 * c)
                        };
                        *pixel = channel(0) | channel(1) | channel(2);
                    }
                }
            },
        );
    }

    fn area(
        &mut self,
        source: &[u32],
        sw: usize,
        sh: usize,
        target: &mut [u32],
        width: usize,
        view: &Viewport,
    ) {
        let key = (sw, sh, view.width, view.height);
        if self.area.as_ref().is_none_or(|plan| plan.key != key) {
            let total = (sw * sh) as u64;
            self.area = Some(AreaPlan {
                key,
                cols: Axis::area(sw, view.width),
                rows: Axis::area(sh, view.height),
                reciprocal: (1u64 << 40).div_ceil(total),
            });
        }
        let plan = self.area.as_ref().expect("plan was just built");
        let threads = thread_count(self.threads, view.width * view.height);
        par_rows(
            target,
            width,
            view.y,
            view.height,
            threads,
            |first, rows| {
                for (i, line) in rows.chunks_exact_mut(width).enumerate() {
                    let row = &plan.rows.spans[first + i];
                    let out = &mut line[view.x..][..view.width];
                    let row_weights = &plan.rows.weights[row.offset as usize..][..row.len as usize];
                    for (pixel, col) in out.iter_mut().zip(&plan.cols.spans) {
                        let col_weights =
                            &plan.cols.weights[col.offset as usize..][..col.len as usize];
                        if row.len == 1 && col.len == 1 {
                            *pixel = source[row.first as usize * sw + col.first as usize];
                            continue;
                        }
                        let mut sums = [0u64; 3];
                        for (r, &wy) in row_weights.iter().enumerate() {
                            let line = &source
                                [(row.first as usize + r) * sw + col.first as usize..]
                                [..col.len as usize];
                            let mut line_sums = [0u32; 3];
                            for (&p, &wx) in line.iter().zip(col_weights) {
                                line_sums[0] += wx * (p & 255);
                                line_sums[1] += wx * ((p >> 8) & 255);
                                line_sums[2] += wx * ((p >> 16) & 255);
                            }
                            for (sum, line_sum) in sums.iter_mut().zip(line_sums) {
                                *sum += u64::from(wy) * u64::from(line_sum);
                            }
                        }
                        let channel =
                            |sum: u64| (((sum * plan.reciprocal) + (1 << 39)) >> 40) as u32;
                        *pixel = channel(sums[0]) | channel(sums[1]) << 8 | channel(sums[2]) << 16;
                    }
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blit(
        source: &[u32],
        sw: usize,
        sh: usize,
        target: &mut [u32],
        width: usize,
        height: usize,
        scaler: Scaler,
    ) {
        Blitter::default().blit(source, sw, sh, target, width, height, scaler);
    }

    fn whole(width: usize, height: usize) -> Viewport {
        Viewport {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    /// The original floating-point bilinear sampler, kept as the reference.
    fn reference_smooth(source: &[u32], sw: usize, sh: usize, vw: usize, vh: usize) -> Vec<u32> {
        let mut out = vec![0; vw * vh];
        for y in 0..vh {
            for x in 0..vw {
                let sx =
                    x as f64 * sw.saturating_sub(1) as f64 / vw.saturating_sub(1).max(1) as f64;
                let sy =
                    y as f64 * sh.saturating_sub(1) as f64 / vh.saturating_sub(1).max(1) as f64;
                let (ix, iy) = (sx as usize, sy as usize);
                let (fx, fy) = (sx.fract(), sy.fract());
                let a = source[iy * sw + ix];
                let b = source[iy * sw + (ix + 1).min(sw - 1)];
                let c = source[(iy + 1).min(sh - 1) * sw + ix];
                let d = source[(iy + 1).min(sh - 1) * sw + (ix + 1).min(sw - 1)];
                let mut rgb = 0;
                for shift in [0, 8, 16] {
                    let channel = |pixel: u32| f64::from((pixel >> shift) & 255);
                    let top = channel(a) * (1.0 - fx) + channel(b) * fx;
                    let bottom = channel(c) * (1.0 - fx) + channel(d) * fx;
                    rgb |= ((top * (1.0 - fy) + bottom * fy).round() as u32) << shift;
                }
                out[y * vw + x] = rgb;
            }
        }
        out
    }

    fn noise(len: usize) -> Vec<u32> {
        let mut state = 0x1234_5678u32;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                state >> 8
            })
            .collect()
    }

    #[test]
    fn solid_frames_stay_solid_and_never_leave_the_viewport() {
        for scaler in Scaler::ALL {
            for (w, h) in [(13, 7), (32, 20)] {
                let (tw, th) = (w + 11, h + 9);
                let mut out = vec![0xdead_beef; tw * th];
                blit(&[0x00ff_ffff; 8 * 5], 8, 5, &mut out, tw, th, scaler);
                let view = viewport(8, tw, th, scaler);
                for y in 0..th {
                    for x in 0..tw {
                        let inside = (view.x..view.x + view.width).contains(&x)
                            && (view.y..view.y + view.height).contains(&y);
                        let pixel = out[y * tw + x];
                        if inside && scaler != Scaler::Crt {
                            assert_eq!(pixel, 0x00ff_ffff, "{scaler:?} {x},{y}");
                        } else if !inside {
                            assert_eq!(pixel, 0, "{scaler:?} letterbox {x},{y}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn area_preserves_the_mean_and_averages_a_checker() {
        let solid = [0x0012_34ab; 8 * 5];
        for (w, h) in [(13, 7), (32, 20), (3, 2), (7, 5)] {
            let mut out = vec![0; w * h];
            blit(&solid, 8, 5, &mut out, w, h, Scaler::Area);
            let view = viewport(8, w, h, Scaler::Area);
            for y in 0..view.height {
                for x in 0..view.width {
                    assert_eq!(out[(view.y + y) * w + view.x + x], 0x0012_34ab);
                }
            }
        }
        // A 4x2 checker of black and white reduced to 2x1 averages each 2x2 block.
        let checker = [0, 0xff_ffff, 0, 0xff_ffff, 0xff_ffff, 0, 0xff_ffff, 0];
        let mut out = vec![0; 2];
        Blitter::default().area(&checker, 4, 2, &mut out, 2, &whole(2, 1));
        assert_eq!(out, [0x80_8080, 0x80_8080]);
        // Uneven ratios weight by overlap: 3 columns into 2.
        let mut out = vec![0; 2];
        Blitter::default().area(&[30, 60, 90], 3, 1, &mut out, 2, &whole(2, 1));
        assert_eq!(out, [40, 80]);
    }

    #[test]
    fn smooth_fixed_point_matches_the_float_reference() {
        for (sw, sh) in [(8, 5), (1, 1), (320, 200)] {
            let source = noise(sw * sh);
            for (w, h) in [(13, 7), (32, 20), (1, 1), (640, 480), (7, 3)] {
                let mut out = vec![0; w * h];
                let view = viewport(sw, w, h, Scaler::Smooth);
                blit(&source, sw, sh, &mut out, w, h, Scaler::Smooth);
                let expected = reference_smooth(&source, sw, sh, view.width, view.height);
                for y in 0..view.height {
                    for x in 0..view.width {
                        let got = out[(view.y + y) * w + view.x + x];
                        let want = expected[y * view.width + x];
                        for shift in [0, 8, 16] {
                            let (g, e) = ((got >> shift) & 255, (want >> shift) & 255);
                            assert!(g.abs_diff(e) <= 1, "{sw}x{sh}->{w}x{h} {x},{y}: {g} vs {e}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn crt_darkens_alternate_rows_deterministically_and_stays_inside() {
        let (w, h) = (960, 720);
        let source = vec![0x00c0_c0c0; 320 * 240];
        let mut first = vec![0xdead_beef; w * h];
        blit(&source, 320, 240, &mut first, w, h, Scaler::Crt);
        let mut second = vec![0; w * h];
        blit(&source, 320, 240, &mut second, w, h, Scaler::Crt);
        assert_eq!(first, second);
        let rows: Vec<_> = (350..362).map(|y| first[y * w + w / 2] & 255).collect();
        assert!(rows.iter().max() > rows.iter().min(), "{rows:?}");
        assert!(rows.iter().all(|&v| v <= 0xc0), "{rows:?}");
        assert!(first.iter().all(|&p| p >> 24 == 0), "nothing escaped RGB");
    }

    /// The original single-threaded scalers, kept as the oracle.
    fn reference_blit(
        source: &[u32],
        (sw, sh): (usize, usize),
        (width, height): (usize, usize),
        scaler: Scaler,
    ) -> Vec<u32> {
        let mut target = vec![0; width * height];
        let view = viewport(sw, width, height, scaler);
        if view.width == 0 || view.height == 0 {
            return target;
        }
        match scaler {
            Scaler::Integer => {
                for y in 0..view.height {
                    let row = &source[(y * sh / view.height) * sw..][..sw];
                    let out = &mut target[(view.y + y) * width + view.x..][..view.width];
                    for (x, pixel) in out.iter_mut().enumerate() {
                        *pixel = row[x * sw / view.width];
                    }
                }
            }
            Scaler::Smooth => {
                let cols = bilinear_axis(sw, view.width);
                let rows = bilinear_axis(sh, view.height);
                for (y, &(iy, fy)) in rows.iter().enumerate() {
                    let top = &source[iy as usize * sw..][..sw];
                    let bottom = &source[(iy as usize + 1).min(sh - 1) * sw..][..sw];
                    let out = &mut target[(view.y + y) * width + view.x..][..view.width];
                    for (pixel, &(ix, fx)) in out.iter_mut().zip(&cols) {
                        let ix = ix as usize;
                        let nx = (ix + 1).min(sw - 1);
                        let (a, b, c, d) = (top[ix], top[nx], bottom[ix], bottom[nx]);
                        let mut rgb = 0;
                        for shift in [0, 8, 16] {
                            let channel = |p: u32| (p >> shift) & 255;
                            let upper = channel(a) * (65536 - fx) + channel(b) * fx;
                            let lower = channel(c) * (65536 - fx) + channel(d) * fx;
                            let value = u64::from(upper) * u64::from(65536 - fy)
                                + u64::from(lower) * u64::from(fy);
                            rgb |= (((value + (1 << 31)) >> 32) as u32) << shift;
                        }
                        *pixel = rgb;
                    }
                }
            }
            Scaler::Area => {
                let cols = Axis::area(sw, view.width);
                let rows = Axis::area(sh, view.height);
                let reciprocal = (1u64 << 40).div_ceil((sw * sh) as u64);
                for (y, row) in rows.spans.iter().enumerate() {
                    let out = &mut target[(view.y + y) * width + view.x..][..view.width];
                    let row_weights = &rows.weights[row.offset as usize..][..row.len as usize];
                    for (pixel, col) in out.iter_mut().zip(&cols.spans) {
                        let col_weights = &cols.weights[col.offset as usize..][..col.len as usize];
                        let mut sums = [0u64; 3];
                        for (r, &wy) in row_weights.iter().enumerate() {
                            for (c, &wx) in col_weights.iter().enumerate() {
                                let p =
                                    source[(row.first as usize + r) * sw + col.first as usize + c];
                                let weight = u64::from(wy) * u64::from(wx);
                                sums[0] += weight * u64::from(p & 255);
                                sums[1] += weight * u64::from((p >> 8) & 255);
                                sums[2] += weight * u64::from((p >> 16) & 255);
                            }
                        }
                        let channel = |sum: u64| (((sum * reciprocal) + (1 << 39)) >> 40) as u32;
                        *pixel = channel(sums[0]) | channel(sums[1]) << 8 | channel(sums[2]) << 16;
                    }
                }
            }
            Scaler::Crt => crate::crt::reference_apply(
                source,
                (sw, sh),
                &mut target,
                width,
                (view.width, view.height, view.x, view.y),
            ),
        }
        target
    }

    #[test]
    fn threaded_output_is_bit_identical_to_the_reference() {
        for (sw, sh) in [(1, 1), (8, 5), (320, 200)] {
            let source = noise(sw * sh);
            for size in [(1, 1), (2, 3), (13, 7), (640, 480), (641, 481), (1000, 700)] {
                for scaler in Scaler::ALL {
                    let want = reference_blit(&source, (sw, sh), size, scaler);
                    for threads in [0, 1, 2, 3, 8, 64] {
                        let mut blitter = Blitter {
                            threads,
                            ..Blitter::default()
                        };
                        let mut got = vec![0xdead_beef; size.0 * size.1];
                        blitter.blit(&source, sw, sh, &mut got, size.0, size.1, scaler);
                        assert!(
                            got == want,
                            "{scaler:?} {sw}x{sh}->{}x{} threads={threads}",
                            size.0,
                            size.1
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn large_window_matches_the_reference_when_threaded() {
        let source = noise(320 * 200);
        for scaler in Scaler::ALL {
            let want = reference_blit(&source, (320, 200), (1920, 1440), scaler);
            let mut blitter = Blitter::default();
            let mut got = vec![0; 1920 * 1440];
            blitter.blit(&source, 320, 200, &mut got, 1920, 1440, scaler);
            assert!(got == want, "{scaler:?}");
        }
    }

    #[test]
    fn plans_rebuild_when_the_geometry_changes() {
        let source = noise(8 * 5);
        let mut blitter = Blitter::default();
        for scaler in [Scaler::Smooth, Scaler::Area] {
            let mut small = vec![0; 13 * 7];
            let mut large = vec![0; 32 * 20];
            blitter.blit(&source, 8, 5, &mut small, 13, 7, scaler);
            blitter.blit(&source, 8, 5, &mut large, 32, 20, scaler);
            let mut again = vec![0; 13 * 7];
            blitter.blit(&source, 8, 5, &mut again, 13, 7, scaler);
            assert_eq!(small, again, "{scaler:?}");
            let mut fresh = vec![0; 32 * 20];
            Blitter::default().blit(&source, 8, 5, &mut fresh, 32, 20, scaler);
            assert_eq!(large, fresh, "{scaler:?}");
        }
    }

    #[test]
    #[ignore = "timing guard; run in release with --nocapture"]
    fn scaler_frame_cost() {
        let source = noise(320 * 200);
        for (width, height) in [(1920, 1440), (640, 480)] {
            let mut out = vec![0; width * height];
            for scaler in Scaler::ALL {
                let mut blitter = Blitter::default();
                blitter.blit(&source, 320, 200, &mut out, width, height, scaler);
                let start = std::time::Instant::now();
                for _ in 0..100 {
                    blitter.blit(&source, 320, 200, &mut out, width, height, scaler);
                }
                let ms = start.elapsed().as_secs_f64() * 10.0;
                println!("{width}x{height} {:8} {ms:7.2} ms/frame", scaler.label());
            }
        }
    }
    #[test]
    fn size_aspect_and_letterbox() {
        assert_eq!(
            viewport(320, 1000, 700, Scaler::Integer),
            Viewport {
                x: 180,
                y: 110,
                width: 640,
                height: 480
            }
        );
        assert_eq!(
            viewport(320, 1000, 700, Scaler::Smooth),
            Viewport {
                x: 33,
                y: 0,
                width: 933,
                height: 700
            }
        );
        assert_eq!(
            viewport(320, 160, 120, Scaler::Integer),
            Viewport {
                x: 0,
                y: 0,
                width: 160,
                height: 120
            }
        );
        let mut out = vec![1; 1000 * 700];
        blit(
            &vec![0xff; 320 * 200],
            320,
            200,
            &mut out,
            1000,
            700,
            Scaler::Integer,
        );
        assert_eq!(out[109 * 1000 + 180], 0);
        assert_eq!(out[110 * 1000 + 180], 0xff);
        assert_eq!(out[589 * 1000 + 819], 0xff);
        assert_eq!(out[590 * 1000 + 819], 0);
    }
    #[test]
    fn edges_and_nearest_pixel_mapping() {
        let source: Vec<_> = (0..320 * 200).map(|p| p as u32).collect();
        for scaler in [Scaler::Integer, Scaler::Smooth] {
            let mut out = vec![0; 640 * 480];
            blit(&source, 320, 200, &mut out, 640, 480, scaler);
            assert_eq!(out[0], source[0]);
            assert_eq!(out[639], source[319]);
            assert_eq!(out[479 * 640], source[199 * 320]);
            assert_eq!(out[479 * 640 + 639], source[199 * 320 + 319]);
            if scaler == Scaler::Integer {
                assert_eq!(out[1], source[0]);
                assert_eq!(out[2], source[1]);
                assert_eq!(out[3 * 640 + 2], source[320 + 1]);
            }
        }
    }
    #[test]
    fn modes_cycle_and_wrap_both_ways() {
        assert_eq!(Scaler::Integer.cycle(true), Scaler::Smooth);
        assert_eq!(Scaler::Smooth.cycle(true), Scaler::Area);
        assert_eq!(Scaler::Area.cycle(true), Scaler::Crt);
        assert_eq!(Scaler::Crt.cycle(true), Scaler::Integer);
        assert_eq!(Scaler::Integer.cycle(false), Scaler::Crt);
        for mode in Scaler::ALL {
            assert_eq!(mode.cycle(true).cycle(false), mode);
        }
    }
    #[test]
    fn bilinear_interpolates_and_empty_windows_are_safe() {
        let mut out = vec![0; 12];
        blit(
            &[0, 0xffffff, 0, 0xffffff],
            2,
            2,
            &mut out,
            4,
            3,
            Scaler::Smooth,
        );
        assert_eq!(out[1], 0x555555);
        assert_eq!(out[2], 0xaaaaaa);
        blit(&[1], 1, 1, &mut [], 0, 0, Scaler::Integer);
    }
}
