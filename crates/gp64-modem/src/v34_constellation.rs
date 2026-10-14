//! V.34 constellation: Z² odd-integer lattice with circular boundary and 4-coset partitioning.
//!
//! The V.34 constellation places points at all (a, b) where both a and b are odd integers
//! and a² + b² ≤ R². The radius R² is chosen so each of the 4 cosets has ≥ 256 points.
//! All points are normalized to unit average power. Points within each coset are sorted
//! by ascending radius.
//!
//! Coset assignment is by (a mod 4, b mod 4):
//!   C0: (1,1)  C1: (1,3)  C2: (3,1)  C3: (3,3)
//!
//! The 4-fold rotational symmetry of the lattice guarantees exactly equal coset sizes.

/// V.34 constellation with 4-coset partitioning on Z² odd-integer lattice.
#[derive(Debug, Clone)]
pub struct V34Constellation {
    /// Points per coset, stored as `cosets[coset_id][point_index] = (i, q)`.
    /// All coordinates are normalized to unit average power.
    cosets: Vec<Vec<(f64, f64)>>,
    /// Normalization scale factor: multiply raw odd-integer coords by this to get
    /// the normalized coords stored in `cosets`.
    scale: f64,
}

/// Minimum R² to start searching. Each coset needs ≥ 256 points.
const INITIAL_R_SQUARED: i64 = 1460;

/// Required minimum number of points per coset.
const MIN_POINTS_PER_COSET: usize = 256;

/// Number of cosets.
pub(crate) const NUM_COSETS: usize = 4;

// Counts `nearest_in_coset` invocations so a test can prove the Viterbi decoder
// performs one search per (received point, coset) rather than one per trellis
// branch (MDD-BUG-CRUCIBLE-00513). Thread-local, so parallel tests never see
// each other's counts.
#[cfg(test)]
thread_local! {
    pub(crate) static COSET_SEARCHES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

impl Default for V34Constellation {
    fn default() -> Self {
        Self::new()
    }
}

impl V34Constellation {
    /// Construct a V.34 constellation.
    ///
    /// Generates all odd-integer lattice points within the circular boundary,
    /// partitions into 4 cosets, verifies each coset has ≥ 256 points, sorts
    /// by ascending radius, and normalizes to unit average power across all
    /// points.
    ///
    /// # Panics
    ///
    /// Panics if the coset constraint cannot be met (should never happen with
    /// the automatic R² search).
    pub fn new() -> Self {
        // Search for R² that gives ≥ 256 points per coset.
        let mut r_squared = INITIAL_R_SQUARED;
        let (raw_cosets, scale) = loop {
            let (raw, counts) = Self::generate_raw_points(r_squared);
            if counts.iter().all(|&c| c >= MIN_POINTS_PER_COSET) {
                let scale = Self::compute_scale(&raw);
                break (raw, scale);
            }
            r_squared += 4; // Small increments; lattice symmetry means 4 is fine.
        };

        // Normalize all points.
        let cosets: Vec<Vec<(f64, f64)>> = raw_cosets
            .into_iter()
            .map(|coset_points| {
                coset_points
                    .into_iter()
                    .map(|(a, b)| (a as f64 * scale, b as f64 * scale))
                    .collect()
            })
            .collect();

        // Final safety check.
        for (i, c) in cosets.iter().enumerate() {
            assert!(
                c.len() >= MIN_POINTS_PER_COSET,
                "Coset C{i} has {} points, need at least {MIN_POINTS_PER_COSET}",
                c.len()
            );
        }

        Self { cosets, scale }
    }

    /// Generate raw (unnormalized) odd-integer points partitioned into 4 cosets.
    /// Returns (cosets_as_raw_integer_pairs, per_coset_counts).
    fn generate_raw_points(r_squared: i64) -> (Vec<Vec<(i64, i64)>>, [usize; NUM_COSETS]) {
        let mut cosets: Vec<Vec<(i64, i64)>> = vec![Vec::new(); NUM_COSETS];
        let mut counts = [0usize; NUM_COSETS];

        // The maximum coordinate magnitude: |a| ≤ sqrt(R²).
        let max_coord = (r_squared as f64).sqrt().ceil() as i64;

        // Iterate over all odd integers in range.
        let mut a = -max_coord | 1; // Make sure `a` is odd.
        if a % 2 == 0 {
            a -= 1;
        }
        // Ensure a is odd and negative enough.
        while a <= max_coord {
            let mut b = -max_coord | 1;
            if b % 2 == 0 {
                b -= 1;
            }
            while b <= max_coord {
                if a * a + b * b <= r_squared {
                    let coset = Self::coset_id(a, b);
                    cosets[coset].push((a, b));
                    counts[coset] += 1;
                }
                b += 2; // Stay on odd integers.
            }
            a += 2; // Stay on odd integers.
        }

        // Sort each coset by radius (ascending) for smooth power progression.
        for coset in cosets.iter_mut() {
            coset.sort_by(|&(a1, b1), &(a2, b2)| {
                let r1 = a1 * a1 + b1 * b1;
                let r2 = a2 * a2 + b2 * b2;
                r1.cmp(&r2)
            });
        }

        (cosets, counts)
    }

    /// Determine coset ID from raw integer coordinates.
    ///   C0: (a mod 4, b mod 4) = (1, 1)
    ///   C1: (a mod 4, b mod 4) = (1, 3)
    ///   C2: (a mod 4, b mod 4) = (3, 1)
    ///   C3: (a mod 4, b mod 4) = (3, 3)
    fn coset_id(a: i64, b: i64) -> usize {
        // Rust's `%` can return negative values, so use `rem_euclid`.
        let am = (a.rem_euclid(4)) as usize;
        let bm = (b.rem_euclid(4)) as usize;
        match (am, bm) {
            (1, 1) => 0,
            (1, 3) => 1,
            (3, 1) => 2,
            (3, 3) => 3,
            _ => unreachable!("Odd integer ({a}, {b}) has unexpected mod-4 residues ({am}, {bm})"),
        }
    }

    /// Compute the normalization scale factor so that average power = 1.0.
    /// `scale` is applied as: normalized = raw * scale.
    fn compute_scale(raw_cosets: &[Vec<(i64, i64)>]) -> f64 {
        let mut sum_sq: f64 = 0.0;
        let mut total: usize = 0;
        for coset in raw_cosets {
            for &(a, b) in coset {
                sum_sq += (a * a + b * b) as f64;
                total += 1;
            }
        }
        let avg_power = sum_sq / total as f64;
        1.0 / avg_power.sqrt()
    }

    /// Average power of the mapper range (first 256 points per coset).
    ///
    /// Since points are sorted by ascending radius and the full constellation
    /// has unit average power, the mapper range (inner 256 points) has
    /// slightly below-unit average power. This ratio is needed to set the
    /// correct `symbol_rms` when transitioning from Phase 3 training (which
    /// uses the full constellation scale) to data mode (which only uses the
    /// mapper range).
    pub fn mapper_range_avg_power(&self) -> f64 {
        let mut sum_sq: f64 = 0.0;
        let mut total: usize = 0;
        for coset in &self.cosets {
            for &(i, q) in coset.iter().take(MIN_POINTS_PER_COSET) {
                sum_sq += i * i + q * q;
                total += 1;
            }
        }
        sum_sq / total as f64
    }

    /// Get the (I, Q) coordinates for point `index` within `coset`.
    ///
    /// # Panics
    ///
    /// Panics if `coset >= 4` or `index >= coset_size()`.
    pub fn point(&self, coset: u8, index: u16) -> (f64, f64) {
        self.cosets[coset as usize][index as usize]
    }

    /// Number of points per coset (equal for all cosets by 4-fold symmetry).
    pub fn coset_size(&self) -> u16 {
        self.cosets[0].len() as u16
    }

    /// Total number of constellation points across all cosets.
    pub fn total_points(&self) -> usize {
        self.cosets.iter().map(|c| c.len()).sum()
    }

    /// Find the nearest point within a specific coset.
    ///
    /// Returns `(index, squared_distance)` where `index` is the point index
    /// within the coset.
    ///
    /// # Panics
    ///
    /// Panics if `coset >= 4`.
    pub fn nearest_in_coset(&self, i: f64, q: f64, coset: u8) -> (u16, f64) {
        // Search-count oracle (MDD-BUG-CRUCIBLE-00513): each call scans at
        // least 256 points, so a caller that repeats identical searches is
        // paying for them in full.
        #[cfg(test)]
        COSET_SEARCHES.with(|n| n.set(n.get() + 1));

        let points = &self.cosets[coset as usize];
        let mut best_idx: u16 = 0;
        let mut best_dist = f64::MAX;
        for (idx, &(pi, pq)) in points.iter().enumerate() {
            let di = i - pi;
            let dq = q - pq;
            let dist_sq = di * di + dq * dq;
            if dist_sq < best_dist {
                best_dist = dist_sq;
                best_idx = idx as u16;
            }
        }
        (best_idx, best_dist)
    }

    /// Find the nearest point in the entire constellation.
    ///
    /// Returns `(index_within_coset, coset_id)`.
    pub fn nearest(&self, i: f64, q: f64) -> (u16, u8) {
        let mut best_idx: u16 = 0;
        let mut best_coset: u8 = 0;
        let mut best_dist = f64::MAX;
        for coset in 0..NUM_COSETS as u8 {
            let (idx, dist) = self.nearest_in_coset(i, q, coset);
            if dist < best_dist {
                best_dist = dist;
                best_idx = idx;
                best_coset = coset;
            }
        }
        (best_idx, best_coset)
    }

    /// Normalization scale factor (for assertions on raw coordinates).
    pub fn normalization_scale(&self) -> f64 {
        self.scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_constellation() -> V34Constellation {
        V34Constellation::new()
    }

    // -----------------------------------------------------------------------
    // Point count: each coset has exactly equal size and ≥ 256 points.
    // -----------------------------------------------------------------------
    #[test]
    fn coset_sizes_are_equal_and_sufficient() {
        let c = make_constellation();
        let size = c.coset_size();
        assert!(
            size >= MIN_POINTS_PER_COSET as u16,
            "Coset size {size} < {MIN_POINTS_PER_COSET}"
        );
        for coset_id in 0..NUM_COSETS {
            assert_eq!(
                c.cosets[coset_id].len() as u16,
                size,
                "Coset C{coset_id} size {} != expected {size}",
                c.cosets[coset_id].len()
            );
        }
    }

    // -----------------------------------------------------------------------
    // All points have odd integer coordinates (before normalization).
    // -----------------------------------------------------------------------
    #[test]
    fn all_points_are_odd_integers_before_normalization() {
        let c = make_constellation();
        let inv_scale = 1.0 / c.normalization_scale();
        for coset_id in 0..NUM_COSETS {
            for (idx, &(ni, nq)) in c.cosets[coset_id].iter().enumerate() {
                // Recover raw integer coordinates.
                let raw_i = ni * inv_scale;
                let raw_q = nq * inv_scale;
                // Should be very close to an odd integer.
                let ri = raw_i.round() as i64;
                let rq = raw_q.round() as i64;
                assert!(
                    (raw_i - ri as f64).abs() < 1e-10,
                    "Coset C{coset_id} point {idx}: I={raw_i} not close to integer {ri}"
                );
                assert!(
                    (raw_q - rq as f64).abs() < 1e-10,
                    "Coset C{coset_id} point {idx}: Q={raw_q} not close to integer {rq}"
                );
                assert!(ri % 2 != 0, "Coset C{coset_id} point {idx}: I={ri} is even");
                assert!(rq % 2 != 0, "Coset C{coset_id} point {idx}: Q={rq} is even");
            }
        }
    }

    // -----------------------------------------------------------------------
    // All points within radius bound.
    // -----------------------------------------------------------------------
    #[test]
    fn all_points_within_radius() {
        let c = make_constellation();
        let inv_scale = 1.0 / c.normalization_scale();
        for coset_id in 0..NUM_COSETS {
            for &(ni, nq) in &c.cosets[coset_id] {
                let raw_i = (ni * inv_scale).round() as i64;
                let raw_q = (nq * inv_scale).round() as i64;
                let r_sq = raw_i * raw_i + raw_q * raw_q;
                // R² was chosen by the constructor; all points must be within it.
                // We don't know the exact R² chosen, but we know it's at least
                // INITIAL_R_SQUARED. Just check it's finite and reasonable.
                assert!(
                    r_sq <= 2000, // generous bound; actual R² is ~1460
                    "Point ({raw_i}, {raw_q}) has r²={r_sq}, too large"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Average power is 1.0 after normalization.
    // -----------------------------------------------------------------------
    #[test]
    fn average_power_is_unity() {
        let c = make_constellation();
        let mut sum_sq: f64 = 0.0;
        let mut count: usize = 0;
        for coset_id in 0..NUM_COSETS {
            for &(i, q) in &c.cosets[coset_id] {
                sum_sq += i * i + q * q;
                count += 1;
            }
        }
        let avg_power = sum_sq / count as f64;
        assert!(
            (avg_power - 1.0).abs() < 1e-12,
            "Average power {avg_power} != 1.0"
        );
    }

    // -----------------------------------------------------------------------
    // Mapper range average power matches what mapper_range_avg_power() returns.
    // -----------------------------------------------------------------------
    #[test]
    fn mapper_range_avg_power_is_consistent() {
        let c = make_constellation();
        let reported = c.mapper_range_avg_power();
        // The mapper range (inner 256 points, ascending radius) should have
        // below-unit average power since we normalize to the full constellation.
        assert!(
            reported < 1.0,
            "Mapper range avg power {reported} should be < 1.0 (full constellation avg = 1.0)"
        );
        assert!(
            reported > 0.5,
            "Mapper range avg power {reported} too low — something wrong with sort/normalization"
        );
    }

    // -----------------------------------------------------------------------
    // Intra-coset minimum distance = 4.0 / sqrt(avg_power_raw) for all cosets.
    // After normalization, min distance² = 16 * scale² = 16 / avg_power_raw.
    // Since normalization makes avg_power = 1.0, the intra-coset min distance
    // is 4 * scale, and min_dist² = 16 * scale².
    // -----------------------------------------------------------------------
    #[test]
    fn intra_coset_min_distance() {
        let c = make_constellation();
        let expected_min_dist_sq = 16.0 * c.scale * c.scale;

        for coset_id in 0..NUM_COSETS {
            let points = &c.cosets[coset_id];
            let mut actual_min_dist_sq = f64::MAX;

            for i in 0..points.len() {
                for j in (i + 1)..points.len() {
                    let di = points[i].0 - points[j].0;
                    let dq = points[i].1 - points[j].1;
                    let dist_sq = di * di + dq * dq;
                    if dist_sq < actual_min_dist_sq {
                        actual_min_dist_sq = dist_sq;
                    }
                }
            }

            assert!(
                (actual_min_dist_sq - expected_min_dist_sq).abs() < 1e-10,
                "Coset C{coset_id}: min_dist²={actual_min_dist_sq:.10}, expected {expected_min_dist_sq:.10}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // nearest_in_coset returns exact point for all constellation points.
    // -----------------------------------------------------------------------
    #[test]
    fn nearest_in_coset_exact_match() {
        let c = make_constellation();
        for coset_id in 0..NUM_COSETS as u8 {
            let points = &c.cosets[coset_id as usize];
            for (idx, &(pi, pq)) in points.iter().enumerate() {
                let (found_idx, dist_sq) = c.nearest_in_coset(pi, pq, coset_id);
                assert_eq!(
                    found_idx, idx as u16,
                    "Coset C{coset_id} point {idx}: nearest returned index {found_idx}"
                );
                assert!(
                    dist_sq < 1e-20,
                    "Coset C{coset_id} point {idx}: dist²={dist_sq}, expected ~0"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // nearest_in_coset returns closest-in-coset for perturbed inputs.
    // -----------------------------------------------------------------------
    #[test]
    fn nearest_in_coset_perturbed() {
        let c = make_constellation();
        // Small perturbation that stays within the Voronoi region of the original point.
        let eps = c.scale * 0.5; // Half of the inter-lattice spacing (within coset).

        for coset_id in 0..NUM_COSETS as u8 {
            // Test a sample of points (all of them would be slow for the min-distance test).
            let points = &c.cosets[coset_id as usize];
            for idx in (0..points.len()).step_by(10) {
                let (pi, pq) = points[idx];
                let perturbed_i = pi + eps * 0.1;
                let perturbed_q = pq - eps * 0.1;
                let (found_idx, _dist_sq) = c.nearest_in_coset(perturbed_i, perturbed_q, coset_id);
                assert_eq!(
                    found_idx, idx as u16,
                    "Coset C{coset_id} point {idx}: perturbed nearest returned {found_idx}"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Global nearest agrees with best per-coset result.
    // -----------------------------------------------------------------------
    #[test]
    fn global_nearest_agrees_with_per_coset() {
        let c = make_constellation();
        // Test a grid of query points.
        let test_values = [-0.5, -0.2, 0.0, 0.1, 0.3, 0.7];
        for &ti in &test_values {
            for &tq in &test_values {
                let (global_idx, global_coset) = c.nearest(ti, tq);

                // Find best across all cosets manually.
                let mut best_idx: u16 = 0;
                let mut best_coset: u8 = 0;
                let mut best_dist = f64::MAX;
                for coset in 0..NUM_COSETS as u8 {
                    let (idx, dist) = c.nearest_in_coset(ti, tq, coset);
                    if dist < best_dist {
                        best_dist = dist;
                        best_idx = idx;
                        best_coset = coset;
                    }
                }

                assert_eq!(
                    (global_idx, global_coset),
                    (best_idx, best_coset),
                    "Global nearest at ({ti}, {tq}): got ({global_idx}, C{global_coset}), \
                     expected ({best_idx}, C{best_coset})"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // 4-fold symmetry: coset sizes are exactly equal.
    // (This is the same assertion as coset_sizes_are_equal_and_sufficient,
    // but let's also verify that rotating a point maps it to a different coset.)
    // -----------------------------------------------------------------------
    #[test]
    fn four_fold_rotational_symmetry() {
        let c = make_constellation();
        let inv_scale = 1.0 / c.normalization_scale();

        // Every point (a, b) in coset Cx should have its 90° rotation (-b, a)
        // in some other coset, and the rotation cycle visits all 4 cosets.
        let points_in_coset: Vec<Vec<(i64, i64)>> = (0..NUM_COSETS)
            .map(|cid| {
                c.cosets[cid]
                    .iter()
                    .map(|&(ni, nq)| {
                        (
                            (ni * inv_scale).round() as i64,
                            (nq * inv_scale).round() as i64,
                        )
                    })
                    .collect()
            })
            .collect();

        // Check that rotating a point from C0 produces points in C1, C2, C3
        // (the exact mapping depends on the coset definition, but all 4 should appear).
        let sample = points_in_coset[0][0]; // First point of C0.
        let mut visited_cosets = std::collections::HashSet::new();
        let (mut a, mut b) = sample;
        for _ in 0..4 {
            let cid = V34Constellation::coset_id(a, b);
            visited_cosets.insert(cid);
            let (new_a, new_b) = (-b, a); // 90° rotation
            a = new_a;
            b = new_b;
        }
        assert_eq!(
            visited_cosets.len(),
            4,
            "90° rotation should visit all 4 cosets, but visited {:?}",
            visited_cosets
        );

        // All coset sizes must be identical.
        let size = c.coset_size();
        for cid in 0..NUM_COSETS {
            assert_eq!(c.cosets[cid].len() as u16, size);
        }
    }

    // -----------------------------------------------------------------------
    // Sanity: total points is reasonable for V.34 28800 bps.
    // -----------------------------------------------------------------------
    #[test]
    fn total_point_count_reasonable() {
        let c = make_constellation();
        let total = c.total_points();
        // V.34 at 28800 bps should have ~960 total points (4 cosets × ~240 each).
        // With R² = 1460 we expect somewhat more than 4×256 = 1024.
        assert!(
            total >= 1024,
            "Total points {total} < 1024 (too few for V.34)"
        );
        assert!(
            total <= 1200,
            "Total points {total} > 1200 (unexpectedly many)"
        );
    }
}
