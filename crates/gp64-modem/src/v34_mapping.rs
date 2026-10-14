//! V.34 direct mapping: 256-point per-coset indexing.
//!
//! Maps 8 uncoded data bits to/from a constellation point index within a coset.
//! Shell mapping is deferred — on a noiseless TCP channel the shaping gain provides
//! zero functional benefit. This direct mapper uses the low 8 bits of the uncoded
//! data as a simple point index.

use crate::v34_constellation::V34Constellation;

/// Direct 256-point per-coset mapper for V.34.
///
/// Each coset has P >= 256 points. The mapper uses `data_bits` (0..255) as a
/// direct index into the coset's point table. Points beyond index 255 exist in
/// the constellation but are not addressable by the mapper — the Viterbi decoder
/// may still use them for boundary nearest-neighbor decisions.
pub struct V34DirectMapper {
    /// Points available per coset (must be >= 256).
    points_per_coset: u16,
}

impl V34DirectMapper {
    /// Construct a direct mapper from a constellation.
    ///
    /// # Panics
    ///
    /// Panics if any coset has fewer than 256 points.
    pub fn new(constellation: &V34Constellation) -> Self {
        assert!(
            constellation.coset_size() >= 256,
            "Each coset needs >= 256 points for 8-bit indexing, got {}",
            constellation.coset_size()
        );
        Self {
            points_per_coset: constellation.coset_size(),
        }
    }

    /// Map 8 uncoded data bits to a constellation point within the given coset.
    ///
    /// `coset` selects the coset (0..3), `data_bits` (0..255) selects the point
    /// within that coset.
    pub fn encode(&self, constellation: &V34Constellation, coset: u8, data_bits: u8) -> (f64, f64) {
        constellation.point(coset, data_bits as u16)
    }

    /// Map a point index within a coset back to 8 data bits.
    ///
    /// Returns `None` if `point_index >= 256` (boundary point outside mapping range).
    pub fn decode(&self, point_index: u16) -> Option<u8> {
        if point_index < 256 {
            Some(point_index as u8)
        } else {
            None
        }
    }

    /// Number of points per coset in the underlying constellation.
    pub fn points_per_coset(&self) -> u16 {
        self.points_per_coset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_mapper() -> (V34Constellation, V34DirectMapper) {
        let c = V34Constellation::new();
        let m = V34DirectMapper::new(&c);
        (c, m)
    }

    // -----------------------------------------------------------------------
    // Construction: mapper accepts a valid constellation.
    // -----------------------------------------------------------------------
    #[test]
    fn construction_succeeds() {
        let (_, m) = make_mapper();
        assert!(m.points_per_coset() >= 256);
    }

    // -----------------------------------------------------------------------
    // Encode -> decode round-trip for ALL 256 indices in each of 4 cosets.
    // 4 x 256 = 1024 round-trips.
    // -----------------------------------------------------------------------
    #[test]
    fn round_trip_all_cosets_all_indices() {
        let (c, m) = make_mapper();
        for coset in 0..4u8 {
            for bits in 0..=255u8 {
                // Encode: data_bits -> (I, Q)
                let (i, q) = m.encode(&c, coset, bits);

                // The point must exist at this coset and index.
                let (found_idx, dist_sq) = c.nearest_in_coset(i, q, coset);
                assert_eq!(
                    found_idx, bits as u16,
                    "Coset C{coset}, bits={bits}: nearest_in_coset returned index {found_idx}"
                );
                assert!(
                    dist_sq < 1e-20,
                    "Coset C{coset}, bits={bits}: distance to own point is {dist_sq}"
                );

                // Decode: point_index -> data_bits
                let decoded = m.decode(found_idx);
                assert_eq!(
                    decoded,
                    Some(bits),
                    "Coset C{coset}, bits={bits}: decode({found_idx}) = {decoded:?}"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Injectivity: distinct data_bits produce distinct (I, Q) within each coset.
    // -----------------------------------------------------------------------
    #[test]
    fn distinct_inputs_produce_distinct_outputs() {
        let (c, m) = make_mapper();
        for coset in 0..4u8 {
            let mut seen = std::collections::HashSet::new();
            for bits in 0..=255u8 {
                let (i, q) = m.encode(&c, coset, bits);
                // Use bit representation for exact comparison.
                let key = (i.to_bits(), q.to_bits());
                assert!(
                    seen.insert(key),
                    "Coset C{coset}: bits={bits} produced duplicate point ({i}, {q})"
                );
            }
            assert_eq!(
                seen.len(),
                256,
                "Coset C{coset}: expected 256 distinct points"
            );
        }
    }

    // -----------------------------------------------------------------------
    // All 256 indices map to valid constellation points within the correct coset.
    // -----------------------------------------------------------------------
    #[test]
    fn all_indices_map_to_valid_points() {
        let (c, m) = make_mapper();
        for coset in 0..4u8 {
            for bits in 0..=255u8 {
                let (i, q) = m.encode(&c, coset, bits);
                // The point should be finite.
                assert!(
                    i.is_finite(),
                    "Coset C{coset}, bits={bits}: I is not finite"
                );
                assert!(
                    q.is_finite(),
                    "Coset C{coset}, bits={bits}: Q is not finite"
                );

                // Nearest-in-coset should be this exact point (index matches).
                let (idx, _) = c.nearest_in_coset(i, q, coset);
                assert_eq!(
                    idx, bits as u16,
                    "Coset C{coset}, bits={bits}: point not at expected index"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Boundary check: data_bits=0 and data_bits=255 produce different points.
    // -----------------------------------------------------------------------
    #[test]
    fn boundary_points_differ() {
        let (c, m) = make_mapper();
        for coset in 0..4u8 {
            let (i0, q0) = m.encode(&c, coset, 0);
            let (i255, q255) = m.encode(&c, coset, 255);
            assert!(
                i0 != i255 || q0 != q255,
                "Coset C{coset}: data_bits=0 and data_bits=255 map to the same point"
            );
        }
    }

    // -----------------------------------------------------------------------
    // decode returns None for index >= 256.
    // -----------------------------------------------------------------------
    #[test]
    fn decode_out_of_range_returns_none() {
        let (_, m) = make_mapper();
        assert_eq!(m.decode(256), None);
        assert_eq!(m.decode(257), None);
        assert_eq!(m.decode(511), None);
        assert_eq!(m.decode(u16::MAX), None);
    }

    // -----------------------------------------------------------------------
    // decode returns Some for all valid indices 0..255.
    // -----------------------------------------------------------------------
    #[test]
    fn decode_valid_range() {
        let (_, m) = make_mapper();
        for idx in 0..256u16 {
            assert_eq!(
                m.decode(idx),
                Some(idx as u8),
                "decode({idx}) should return Some({}) but didn't",
                idx as u8
            );
        }
    }
}
