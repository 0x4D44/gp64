//! Width bucketing shared by the A052 span-write engagement counters.
//!
//! Two censuses report against these buckets and must agree on them:
//! `gp64_cpu::addressing`'s funnel-side census (operands the resolved
//! operand-write funnel committed by replaying its recorded runs) and
//! `Memory`'s outcome-side census (spans `write_bytes_guest` served from the
//! bulk arm vs the per-byte fallback). Keeping the buckets here is what lets
//! the end-of-run report print the two side by side.
//!
//! The widths are the ones the funnel actually produces: 1/2/4/8 for integer
//! and x87 m32/m64 operands, 10 for m80, 14/28 for FNSTENV's real- and
//! protected-mode images, 94/108 for FNSAVE's, plus a catch-all that also
//! collects the block-device transfers reaching `write_bytes_guest` from
//! outside the funnel.

/// Number of width buckets, catch-all included.
pub const WIDTH_BUCKETS: usize = 10;

/// The exact widths that get their own bucket, in bucket order.
pub const BUCKET_WIDTHS: [usize; WIDTH_BUCKETS - 1] = [1, 2, 4, 8, 10, 14, 28, 94, 108];

/// The bucket a `len`-byte span belongs to. Anything not in
/// [`BUCKET_WIDTHS`] lands in the final catch-all bucket.
#[inline]
pub fn width_bucket(len: usize) -> usize {
    let mut bucket = 0;
    while bucket < BUCKET_WIDTHS.len() {
        if BUCKET_WIDTHS[bucket] == len {
            return bucket;
        }
        bucket += 1;
    }
    WIDTH_BUCKETS - 1
}

/// The report label for a bucket — `w8` for an exact width, `wother` for the
/// catch-all.
#[inline]
pub fn bucket_label(bucket: usize) -> String {
    BUCKET_WIDTHS
        .get(bucket)
        .map_or_else(|| "wother".to_string(), |width| format!("w{width}"))
}

/// `label=count` for every bucket that saw traffic, joined by spaces. Empty
/// when the census is entirely cold, so a report row stays short.
pub fn nonzero_buckets(counts: &[u64; WIDTH_BUCKETS]) -> String {
    counts
        .iter()
        .enumerate()
        .filter(|(_, &count)| count != 0)
        .map(|(bucket, count)| format!("{}={count}", bucket_label(bucket)))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn every_exact_width_gets_its_own_bucket() {
        let mut seen = std::collections::BTreeSet::new();
        for width in BUCKET_WIDTHS {
            let bucket = width_bucket(width);
            assert!(seen.insert(bucket), "width {width} collided on {bucket}");
            assert_eq!(bucket_label(bucket), format!("w{width}"));
        }
        assert_eq!(seen.len(), WIDTH_BUCKETS - 1);
    }

    #[test]
    fn unlisted_widths_land_in_the_catch_all() {
        for width in [0usize, 3, 512, 4096] {
            assert_eq!(width_bucket(width), WIDTH_BUCKETS - 1, "width {width}");
        }
        assert_eq!(bucket_label(WIDTH_BUCKETS - 1), "wother");
    }

    #[test]
    fn nonzero_buckets_lists_only_live_widths() {
        let mut counts = [0u64; WIDTH_BUCKETS];
        counts[width_bucket(8)] = 12;
        counts[width_bucket(108)] = 3;
        assert_eq!(nonzero_buckets(&counts), "w8=12 w108=3");
        assert_eq!(nonzero_buckets(&[0u64; WIDTH_BUCKETS]), "");
    }
}
