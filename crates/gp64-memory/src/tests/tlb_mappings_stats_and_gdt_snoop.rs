use super::*;

#[test]
fn global_lifetime_epoch_tracks_only_live_global_flushes() {
    let mut mem = Memory::new();
    let initial = mem.dynrec_global_tlb_flush_epoch();
    mem.invalidate_tlb();
    assert_eq!(mem.dynrec_global_tlb_flush_epoch(), initial);
    mem.write_u32_physical(0x1000, 0x3103);
    mem.tlb_insert(0, 3, true, false, false);
    mem.mark_tlb_global(0, 0x1000, false);
    mem.mark_tlb_global(0, 0x1000, false); // Marking twice must not count twice.
    mem.invalidate_tlb_non_global();
    mem.invalidate_tlb();
    assert_eq!(mem.dynrec_global_tlb_flush_epoch(), initial + 1);
    mem.invalidate_tlb();
    assert_eq!(mem.dynrec_global_tlb_flush_epoch(), initial + 1);

    // Replacing the same page, hash eviction and INVLPG all retire globals.
    for retirement in 0..3 {
        mem.tlb_insert(0, 3, true, false, false);
        mem.mark_tlb_global(0, 0x1000, false);
        match retirement {
            0 => mem.tlb_insert(0, 4, true, false, true),
            1 => {
                let collision = (1..0x10_000)
                    .find(|page| SoftwareTlb::index(*page) == SoftwareTlb::index(0))
                    .unwrap();
                mem.tlb_insert(collision, 4, true, false, true);
            }
            _ => mem.invalidate_tlb_page(0),
        }
        mem.invalidate_tlb();
        assert_eq!(mem.dynrec_global_tlb_flush_epoch(), initial + 1);
    }
    mem.tlb_insert(0, 3, true, false, false);
    mem.mark_tlb_global(0, 0x1000, false);
    mem.invalidate_tlb_global_flush();
    assert_eq!(
        mem.dynrec_global_tlb_flush_epoch(),
        initial + 2,
        "CR4 flush must bump exactly once"
    );
    mem.invalidate_tlb_global_flush();
    assert_eq!(
        mem.dynrec_global_tlb_flush_epoch(),
        initial + 3,
        "CR4 changes count even without live globals"
    );
}

// ========== TLB entries_ptr and pub access tests ==========

#[test]
fn test_tlb_entries_ptr_valid() {
    let mut mem = Memory::new();
    mem.tlb_insert(0x42, 0x100, true, false, false);

    let ptr = mem.tlb_entries_ptr();
    assert!(!ptr.is_null());

    // Verify we can read back through the pointer
    let index = SoftwareTlb::index(0x42);
    let entry = unsafe { &*ptr.add(index) };
    assert_eq!(entry.linear_page, 0x42);
    assert_eq!(entry.physical_page, 0x100);
    assert!(entry.flags & TLB_FLAG_WRITABLE != 0);
    assert_eq!(entry.flags & TLB_FLAG_USER, 0);
    assert_eq!(entry.flags & TLB_FLAG_DIRTY, 0);
    // Physical page 0x100 is within RAM — host_read should be non-null
    assert!(!entry.host_read.is_null());
}

#[test]
fn test_tlb_entry_fields_pub() {
    // Verify all fields of TlbEntry are accessible
    let entry = TlbEntry {
        host_read: std::ptr::null(),
        host_write: std::ptr::null_mut(),
        linear_page: 0xABCDE,
        physical_page: 0x12345,
        flags: TLB_FLAG_WRITABLE | TLB_FLAG_DIRTY,
        pad: [0; 7],
    };
    assert_eq!(entry.linear_page, 0xABCDE);
    assert_eq!(entry.physical_page, 0x12345);
    assert!(entry.flags & TLB_FLAG_WRITABLE != 0);
    assert_eq!(entry.flags & TLB_FLAG_USER, 0);
    assert!(entry.flags & TLB_FLAG_DIRTY != 0);
}

// ========== add/remove_physical_mapping TLB flush tests ==========

#[test]
fn test_add_physical_mapping_flushes_tlb_entries() {
    let mut mem = Memory::new();

    // Pre-populate TLB with entries in the range we'll map
    let linear = 0x200_000u32; // 2MB
    let page = linear >> 12; // page 0x200
    mem.tlb_insert(page, 0x300, true, true, false);
    assert!(mem.tlb_lookup(page).is_some());

    // Add a mapping overlapping that page
    mem.add_physical_mapping(linear, 0x300_000, 0x1000);

    // TLB entry should have been flushed
    assert!(mem.tlb_lookup(page).is_none());
}

/// MDD-BUG-CRU-01483: a mapping larger than the TLB is flushed whole, not
/// walked page by page. A 1 GiB span is 262,144 pages; the walk issued one
/// invalidation each, on add and again on remove. The warmed page inside
/// the span must still be gone either way.
#[test]
fn a_physical_mapping_larger_than_the_tlb_is_flushed_whole() {
    const ONE_GIB: u32 = 0x4000_0000;
    let mut mem = Memory::new();
    mem.tlb_stats_mut().enable();
    let linear = 0x1000_0000u32;
    let page = (linear >> 12) + 0x1234;
    mem.tlb_insert(page, 0x300, true, true, false);

    assert!(mem.add_physical_mapping(linear, 0x300_000, ONE_GIB));
    assert!(
        mem.tlb_lookup(page).is_none(),
        "the span is still invalidated"
    );
    assert!(mem.remove_physical_mapping(linear));

    assert!(
        mem.tlb_stats().flush_single_count() <= TLB_SIZE as u64,
        "per-page invalidations must not exceed the TLB's size, got {}",
        mem.tlb_stats().flush_single_count()
    );
    assert_eq!(
        mem.tlb_stats().flush_full_count(),
        2,
        "one full flush each way"
    );
}

/// A span that fits the TLB is still invalidated page by page.
#[test]
fn a_physical_mapping_within_the_tlb_is_flushed_per_page() {
    let mut mem = Memory::new();
    mem.tlb_stats_mut().enable();
    assert!(mem.add_physical_mapping(0x20_0000, 0x300_000, 0x3000));
    assert_eq!(mem.tlb_stats().flush_single_count(), 3);
    assert_eq!(mem.tlb_stats().flush_full_count(), 0);
}

/// SHRINKING a same-base mapping must flush the pages it no longer covers.
///
/// `add_physical_mapping` replaces any mapping with the same linear base,
/// but flushed only the pages of the NEW one. A three-page mapping with
/// warmed TLB entries, replaced by a one-page mapping, therefore kept live
/// translations for its two tail pages — pointing at the old physical base,
/// for linear addresses the guest no longer has mapped there.
/// `bump_direct_translation_policy_epoch` does not clear the hashed TLB, so
/// nothing downstream caught it. DPMI 1.0 map-physical and map-conventional
/// pass guest-selected page counts straight to this API
/// (`crates/gp64-dos/src/dpmi/memory.rs`), so the shrink is
/// guest-reachable (MDD-BUG-KIL-01041).
#[test]
fn test_shrinking_a_physical_mapping_flushes_the_pages_it_gave_up() {
    let mut mem = Memory::new();
    let linear = 0x400_000u32;
    let base_page = linear >> 12;

    // A three-page mapping, every page warmed.
    assert!(mem.add_physical_mapping(linear, 0x700_000, 0x3000));
    for i in 0..3 {
        mem.tlb_insert(base_page + i, 0x700 + i, true, true, false);
        assert!(
            mem.tlb_lookup(base_page + i).is_some(),
            "precondition: page {} is warm before the shrink",
            base_page + i
        );
    }

    // Replace it with a ONE-page mapping at the same base.
    assert!(mem.add_physical_mapping(linear, 0x900_000, 0x1000));

    for i in 0..3 {
        let page = base_page + i;
        assert!(
            mem.tlb_lookup(page).is_none(),
            "page {page} kept its translation across a shrink from 3 pages \
             to 1; pages {} and {} are outside the new mapping entirely, so \
             a guest read there still resolves through the OLD physical base",
            base_page + 1,
            base_page + 2
        );
    }
}

/// The converse: growing a same-base mapping still flushes the new pages,
/// so the union flush did not quietly narrow to the old range.
#[test]
fn test_growing_a_physical_mapping_flushes_the_pages_it_gained() {
    let mut mem = Memory::new();
    let linear = 0x500_000u32;
    let base_page = linear >> 12;

    assert!(mem.add_physical_mapping(linear, 0x700_000, 0x1000));
    for i in 0..3 {
        mem.tlb_insert(base_page + i, 0x800 + i, true, true, false);
    }

    assert!(mem.add_physical_mapping(linear, 0x900_000, 0x3000));

    for i in 0..3 {
        let page = base_page + i;
        assert!(
            mem.tlb_lookup(page).is_none(),
            "page {page} kept its translation across a grow from 1 page to 3"
        );
    }
}

#[test]
fn test_add_physical_mapping_flushes_multi_page() {
    let mut mem = Memory::new();

    // Pre-populate TLB entries across a 3-page range
    let linear = 0x100_000u32;
    let size = 0x3000u32; // 3 pages
    for i in 0..3 {
        let page = (linear >> 12) + i;
        mem.tlb_insert(page, 0x500 + i, true, true, false);
        assert!(mem.tlb_lookup(page).is_some());
    }

    mem.add_physical_mapping(linear, 0x500_000, size);

    // All 3 TLB entries should be flushed
    for i in 0..3 {
        let page = (linear >> 12) + i;
        assert!(
            mem.tlb_lookup(page).is_none(),
            "TLB entry for page {} should have been flushed",
            page
        );
    }
}

#[test]
fn test_remove_physical_mapping_flushes_tlb_entries() {
    let mut mem = Memory::new();

    let linear = 0x200_000u32;
    let size = 0x2000u32; // 2 pages
    mem.add_physical_mapping(linear, 0x300_000, size);

    // Populate TLB for the mapped pages
    let page0 = linear >> 12;
    let page1 = page0 + 1;
    mem.tlb_insert(page0, 0x300, true, true, false);
    mem.tlb_insert(page1, 0x301, true, true, false);
    assert!(mem.tlb_lookup(page0).is_some());
    assert!(mem.tlb_lookup(page1).is_some());

    // Remove the mapping
    assert!(mem.remove_physical_mapping(linear));

    // TLB entries should be flushed
    assert!(mem.tlb_lookup(page0).is_none());
    assert!(mem.tlb_lookup(page1).is_none());
}

#[test]
fn test_remove_nonexistent_mapping_doesnt_flush() {
    let mut mem = Memory::new();

    // Insert a TLB entry
    mem.tlb_insert(0x42, 0x100, true, true, false);
    assert!(mem.tlb_lookup(0x42).is_some());

    // Remove a non-existent mapping
    assert!(!mem.remove_physical_mapping(0x42_000));

    // TLB entry should still be there
    assert!(mem.tlb_lookup(0x42).is_some());
}

// ========== TLB stats tests ==========

#[test]
fn test_tlb_stats_disabled_by_default() {
    let stats = TlbStats::disabled();
    assert!(!stats.is_enabled());
    assert_eq!(stats.hit_count(), 0);
    assert_eq!(stats.miss_count(), 0);
}

#[test]
fn test_tlb_stats_no_overhead_when_disabled() {
    let stats = TlbStats::disabled();
    // These should be no-ops
    stats.record_hit();
    stats.record_miss();
    stats.record_flush_full();
    stats.record_flush_single();
    assert_eq!(stats.hit_count(), 0);
    assert_eq!(stats.miss_count(), 0);
    assert_eq!(stats.flush_full_count(), 0);
    assert_eq!(stats.flush_single_count(), 0);
}

#[test]
fn test_tlb_stats_counts_when_enabled() {
    let mut stats = TlbStats::disabled();
    stats.enable();
    assert!(stats.is_enabled());

    stats.record_hit();
    stats.record_hit();
    stats.record_miss();
    assert_eq!(stats.hit_count(), 2);
    assert_eq!(stats.miss_count(), 1);
    assert_eq!(stats.total_lookups(), 3);
}

#[test]
fn test_tlb_stats_hit_rate() {
    let mut stats = TlbStats::disabled();
    stats.enable();

    // No lookups => 0% hit rate
    assert_eq!(stats.hit_rate(), 0.0);

    // 3 hits, 1 miss => 75% hit rate
    stats.record_hit();
    stats.record_hit();
    stats.record_hit();
    stats.record_miss();
    assert!((stats.hit_rate() - 75.0).abs() < 0.01);
}

#[test]
fn test_tlb_stats_flush_counters() {
    let mut stats = TlbStats::disabled();
    stats.enable();

    stats.record_flush_full();
    stats.record_flush_full();
    stats.record_flush_single();
    stats.record_flush_single();
    stats.record_flush_single();
    assert_eq!(stats.flush_full_count(), 2);
    assert_eq!(stats.flush_single_count(), 3);
}

#[test]
fn test_tlb_stats_summary_disabled() {
    let stats = TlbStats::disabled();
    let summary = stats.summary();
    assert!(summary.contains("disabled"));
}

#[test]
fn test_tlb_stats_summary_enabled() {
    let mut stats = TlbStats::disabled();
    stats.enable();
    stats.record_hit();
    stats.record_miss();
    stats.record_flush_full();

    let summary = stats.summary();
    assert!(summary.contains("TLB stats:"));
    assert!(summary.contains("lookups:"));
    assert!(summary.contains("hits:"));
    assert!(summary.contains("misses:"));
    assert!(summary.contains("flush_full:"));
    assert!(summary.contains("flush_single:"));
}

#[test]
fn test_tlb_stats_integration_with_memory() {
    let mut mem = Memory::new();
    mem.tlb_stats_mut().enable();

    // Insert a TLB entry and look it up (hit)
    mem.tlb_insert(0x42, 0x100, true, true, true);
    assert!(mem.tlb_lookup(0x42).is_some());
    assert_eq!(mem.tlb_stats().hit_count(), 1);
    assert_eq!(mem.tlb_stats().miss_count(), 0);

    // Look up a page not in the TLB (miss)
    assert!(mem.tlb_lookup(0x99).is_none());
    assert_eq!(mem.tlb_stats().hit_count(), 1);
    assert_eq!(mem.tlb_stats().miss_count(), 1);

    // Full flush
    mem.invalidate_tlb();
    assert_eq!(mem.tlb_stats().flush_full_count(), 1);

    // Now the previously-cached page misses
    assert!(mem.tlb_lookup(0x42).is_none());
    assert_eq!(mem.tlb_stats().miss_count(), 2);
}

#[test]
fn test_tlb_stats_single_page_invalidation() {
    let mut mem = Memory::new();
    mem.tlb_stats_mut().enable();

    mem.tlb_insert(0x42, 0x100, true, true, true);
    mem.invalidate_tlb_page(0x42 << 12);
    assert_eq!(mem.tlb_stats().flush_single_count(), 1);
}

#[test]
fn test_tlb_stats_summary_method_none_when_disabled() {
    let mem = Memory::new();
    // Stats are disabled by default (env var not set in tests)
    assert!(mem.tlb_stats_summary().is_none());
}

#[test]
fn test_tlb_stats_summary_method_some_when_enabled() {
    let mut mem = Memory::new();
    mem.tlb_stats_mut().enable();
    let summary = mem.tlb_stats_summary();
    assert!(summary.is_some());
    assert!(summary.unwrap().contains("TLB stats:"));
}

#[test]
fn test_tlb_stats_debug_impl() {
    let mut stats = TlbStats::disabled();
    stats.enable();
    stats.record_hit();
    let debug = format!("{:?}", stats);
    assert!(debug.contains("TlbStats"));
    assert!(debug.contains("enabled: true"));
    assert!(debug.contains("hits: 1"));
}

// -- MMIO region tests --

#[test]
fn mmio_add_and_read() {
    let mut mem = Memory::new();
    mem.add_mmio_region(0x6000_0000, 1024);
    mem.mmio_slice_mut(0x6000_0000).unwrap()[0] = 0x42;
    mem.mmio_slice_mut(0x6000_0000).unwrap()[1023] = 0xAB;
    assert_eq!(mem.read_u8(0x6000_0000), 0x42);
    assert_eq!(mem.read_u8(0x6000_0000 + 1023), 0xAB);
    assert_eq!(mem.read_u8(0x6000_0000 + 1024), 0xFF);
}

#[test]
fn mmio_write_u8() {
    let mut mem = Memory::new();
    mem.add_mmio_region(0x6000_0000, 256);
    mem.write_u8(0x6000_0000, 0x12);
    mem.write_u8(0x6000_0000 + 255, 0x34);
    assert_eq!(mem.mmio_slice(0x6000_0000).unwrap()[0], 0x12);
    assert_eq!(mem.mmio_slice(0x6000_0000).unwrap()[255], 0x34);
}

/// A device bus interface that swaps bytes on PCI accesses (the Vérité's
/// MEMENDIAN register) is modelled as an XOR on the CPU access's byte
/// offset. It applies to reads and writes alike, composes with the wide
/// writers' byte decomposition, and never touches device-side slice
/// access.
#[test]
fn mmio_byte_swizzle_applies_to_cpu_accesses_only() {
    let base = 0x6000_0000;
    let mut mem = Memory::new();
    mem.add_mmio_region(base, 256);
    assert!(mem.set_mmio_byte_swizzle(base, 2), "region exists");
    assert!(
        !mem.set_mmio_byte_swizzle(base + 0x1000, 2),
        "no region starts there"
    );

    // Half-word swap (2): a DWORD store lands with its half-words exchanged
    // in the device's view, and the CPU reads it back unchanged.
    mem.write_u32(base, 0x1122_3344);
    assert_eq!(
        &mem.mmio_slice(base).unwrap()[0..4],
        &[0x22, 0x11, 0x44, 0x33],
        "device sees the half-words swapped"
    );
    assert_eq!(
        mem.read_u32(base),
        0x1122_3344,
        "CPU round-trips its own store"
    );
    assert_eq!(mem.read_u8(base + 1), 0x33);

    // A device-side write is read by the CPU through the same swizzle.
    mem.mmio_slice_mut(base).unwrap()[8..12].copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
    assert_eq!(mem.read_u32(base + 8), 0xBBAA_DDCC);

    // Mode 0 is the identity again.
    mem.set_mmio_byte_swizzle(base, 0);
    assert_eq!(mem.read_u32(base + 8), 0xDDCC_BBAA);
}

#[test]
fn mmio_wide_writes_commit_like_byte_stores() {
    // Regression: `write_u8` was the ONLY writer that
    // consulted the MMIO table; `write_u16`/`write_u32`/`write_u64` fell
    // through to `prepare_write_addr`, which silently discards every byte
    // at or above MAX_RAM_SIZE. A guest could therefore read back a device
    // dword it was unable to write.
    let base = MAX_RAM_SIZE + 0x2000; // Synthetic aperture above RAM.
    let mut mem = Memory::new();
    mem.add_mmio_region(base, 256);

    mem.write_u16(base, 0xBEEF);
    assert_eq!(
        &mem.mmio_slice(base).unwrap()[0..2],
        &0xBEEFu16.to_le_bytes(),
        "write_u16 must reach the device backing"
    );

    mem.write_u32(base + 4, 0xDEAD_BEEF);
    assert_eq!(
        &mem.mmio_slice(base).unwrap()[4..8],
        &0xDEAD_BEEFu32.to_le_bytes(),
        "write_u32 must reach the device backing"
    );

    mem.write_u64(base + 8, 0x0123_4567_89AB_CDEF);
    assert_eq!(
        &mem.mmio_slice(base).unwrap()[8..16],
        &0x0123_4567_89AB_CDEFu64.to_le_bytes(),
        "write_u64 must reach the device backing"
    );
}

#[test]
fn mmio_every_width_round_trips() {
    // The oracle that encodes the symmetry this bug broke: for every
    // width, what `write_uN` stores is what `read_uN` returns. ABOVE
    // MAX_RAM_SIZE the read side was always correct and the write side
    // was not. (Below MAX_RAM_SIZE a registered region is RAM-shadowed on
    // both sides — see `add_mmio_region`'s placement contract — so the
    // invariant is pinned here only for the band every shipped device
    // actually occupies.)
    let base = MAX_RAM_SIZE + 0x1000; // Synthetic aperture above RAM.
    let mut mem = Memory::new();
    mem.add_mmio_region(base, 256);

    mem.write_u8(base, 0xA5);
    assert_eq!(mem.read_u8(base), 0xA5, "u8 round-trip");

    mem.write_u16(base + 16, 0x1234);
    assert_eq!(mem.read_u16(base + 16), 0x1234, "u16 round-trip");

    mem.write_u32(base + 32, 0x89AB_CDEF);
    assert_eq!(mem.read_u32(base + 32), 0x89AB_CDEF, "u32 round-trip");

    mem.write_u64(base + 48, 0xFEDC_BA98_7654_3210);
    assert_eq!(
        mem.read_u64(base + 48),
        0xFEDC_BA98_7654_3210,
        "u64 round-trip"
    );
}

#[test]
fn mmio_wide_writes_to_ram_are_unaffected_by_a_registered_region() {
    // The decomposition arm must not fire for ordinary RAM traffic just
    // because some device happens to be registered elsewhere.
    let base = 0x6800_0000;
    let mut mem = Memory::new();
    mem.add_mmio_region(base, 256);

    mem.write_u32(0x1000, 0xCAFE_BABE);
    assert_eq!(
        mem.read_u32(0x1000),
        0xCAFE_BABE,
        "RAM dword must still land"
    );
    assert!(
        mem.mmio_slice(base).unwrap().iter().all(|&b| b == 0),
        "a RAM write must not touch the device backing"
    );
}

#[test]
fn mmio_wide_write_partly_past_the_region_end_commits_the_covered_bytes() {
    // A dword straddling the end of a registered region: the covered
    // bytes reach the device, the rest are discarded as before (there is
    // no RAM backing above MAX_RAM_SIZE). Per-byte decode, as on the bus.
    let base = 0x6800_0000;
    let mut mem = Memory::new();
    mem.add_mmio_region(base, 4);

    mem.write_u32(base + 2, 0xDEAD_BEEF);
    // Whole-region compare, not a 2-byte window: this also pins that the
    // out-of-region bytes did not alias back into the region at some
    // other offset.
    assert_eq!(
        mem.mmio_slice(base).unwrap(),
        &[0x00, 0x00, 0xEF, 0xBE],
        "only the two in-region bytes commit; the rest of the region is untouched"
    );
}

#[test]
fn mmio_wide_write_partly_before_the_region_base_commits_the_covered_bytes() {
    // The mirror shape of the above: the span STARTS below the region
    // base. Per-byte decode, so only the covered bytes land.
    let base = 0x6800_0000;
    let mut mem = Memory::new();
    mem.add_mmio_region(base, 4);

    mem.write_u32(base - 2, 0xDEAD_BEEF);
    assert_eq!(
        mem.mmio_slice(base).unwrap(),
        &[0xAD, 0xDE, 0x00, 0x00],
        "only the two in-region bytes commit"
    );
}

#[test]
fn mmio_is_mmio_address() {
    let mut mem = Memory::new();
    assert!(!mem.is_mmio_address(0x6000_0000));
    mem.add_mmio_region(0x6000_0000, 1024);
    assert!(mem.is_mmio_address(0x6000_0000));
    assert!(mem.is_mmio_address(0x6000_0000 + 512));
    assert!(mem.is_mmio_address(0x6000_0000 + 1023));
    assert!(!mem.is_mmio_address(0x6000_0000 + 1024));
    assert!(!mem.is_mmio_address(0x5000_0000));
}

#[test]
fn mmio_remove() {
    let mut mem = Memory::new();
    mem.add_mmio_region(0x6000_0000, 1024);
    assert!(mem.is_mmio_address(0x6000_0000));
    mem.remove_mmio_region(0x6000_0000);
    assert!(!mem.is_mmio_address(0x6000_0000));
    assert_eq!(mem.read_u8(0x6000_0000), 0xFF);
}

#[test]
fn mmio_replace() {
    let mut mem = Memory::new();
    mem.add_mmio_region(0x6000_0000, 1024);
    mem.mmio_slice_mut(0x6000_0000).unwrap()[0] = 0xAA;
    mem.add_mmio_region(0x6000_0000, 2048);
    assert_eq!(mem.mmio_slice(0x6000_0000).unwrap()[0], 0x00);
    assert_eq!(mem.mmio_slice(0x6000_0000).unwrap().len(), 2048);
}

#[test]
fn mmio_does_not_expand_ram() {
    let mut mem = Memory::new();
    let ram_before = mem.ram.len();
    mem.add_mmio_region(0x6000_0000, 4 * 1024 * 1024);
    mem.write_u8(0x6000_0000, 0xFF);
    assert_eq!(mem.read_u8(0x6000_0000), 0xFF);
    assert_eq!(mem.ram.len(), ram_before);
}

#[test]
fn mmio_normal_ram_unaffected() {
    let mut mem = Memory::new();
    mem.add_mmio_region(0x6000_0000, 1024);
    mem.write_u8(0x1000, 0xAB);
    assert_eq!(mem.read_u8(0x1000), 0xAB);
}

#[test]
fn expand_ram_grows_ram() {
    let mut mem = Memory::new();
    // Memory::new() exposes the 128 MiB default while reserving the DOS ceiling.
    let old_usable = mem.usable_len();
    mem.expand_ram(DOS_RAM_CEILING);
    assert_eq!(old_usable, DEFAULT_RAM_SIZE);
    assert_eq!(mem.usable_len(), DOS_RAM_CEILING);

    // Expand beyond the reserved DOS ceiling to actually test growth
    let new_size = DOS_RAM_CEILING + 4 * 1024 * 1024; // 260 MB
    mem.expand_ram(new_size);
    assert_eq!(mem.usable_len(), new_size);
    assert_eq!(mem.ram.len(), new_size + FETCH_PADDING);
}

#[test]
fn expand_ram_rebuilds_instr_exec_pages() {
    let mut mem = Memory::new();
    // Expand beyond the reserved DOS ceiling to trigger rebuild
    let new_size = DOS_RAM_CEILING + 64 * 1024 * 1024; // 320 MB
    mem.expand_ram(new_size);

    let pages = new_size.div_ceil(INSTR_EXEC_PAGE_SIZE);
    let words = pages.div_ceil(64);
    assert_eq!(mem.core.instr_exec_pages().len(), words);
}

#[test]
fn expand_ram_preserves_existing_data() {
    let mut mem = Memory::new();
    // Write some data in conventional memory
    mem.ram[0x1000] = 0xAB;
    mem.ram[0x2000] = 0xCD;

    let new_size = DOS_RAM_CEILING + 4 * 1024 * 1024; // expand beyond current size
    mem.expand_ram(new_size);

    // Existing data should be preserved
    assert_eq!(mem.ram[0x1000], 0xAB);
    assert_eq!(mem.ram[0x2000], 0xCD);
}

#[test]
fn expand_ram_is_idempotent_if_not_larger() {
    let mut mem = Memory::new();
    let initial_len = mem.usable_len();
    // Calling with a size <= current usable_len should be a no-op (no panic)
    mem.expand_ram(1024);
    assert_eq!(mem.usable_len(), initial_len);
}

#[test]
fn smc_watch_bitmap_covers_expanded_ram() {
    // MDD-BUG-FLUXHOMEARPA-00022: RAM above DOS_RAM_CEILING must remain
    // coverable by the SMC watch bitmap after `expand_ram`.
    let mut mem = Memory::new();
    mem.expand_ram(DOS_RAM_CEILING + 64 * 1024 * 1024); // 320 MiB
    let addr = DOS_RAM_CEILING + 44 * 1024 * 1024; // ~300 MiB
    let page = (addr >> 12) as u32;
    // Inside the bitmap a clear bit is authoritative ("no code here")...
    assert!(!mem.smc_span_may_hold_code(addr, 4));
    // ...and setting the watch actually lands — it was silently ignored
    // when the bitmap ended at the 256 MiB bound.
    mem.set_page_watched(page, true);
    assert!(mem.smc_span_may_hold_code(addr, 4));
    mem.set_page_watched(page, false);
    assert!(!mem.smc_span_may_hold_code(addr, 4));
}

#[test]
#[should_panic(expected = "exceeds MAX_RAM_SIZE")]
fn expand_ram_past_max_ram_size_panics() {
    // The architectural bound is enforced at the resize_ram funnel; growth
    // past it must fail loudly, never silently outrun the watch bitmap.
    let mut mem = Memory::new();
    mem.expand_ram(MAX_RAM_SIZE + 0x1000);
}

// --- bulk_copy_ram / bulk_read_ram tests ---

#[test]
fn test_bulk_copy_ram_basic() {
    let mut mem = Memory::new();
    // Write a recognizable pattern at 0xD0000
    for i in 0..16 {
        mem.ram[0xD0000 + i] = (i as u8).wrapping_mul(0x11);
    }
    assert!(mem.bulk_copy_ram(0xD0000, 0xD1000, 16));
    for i in 0..16 {
        assert_eq!(mem.ram[0xD1000 + i], (i as u8).wrapping_mul(0x11));
    }
}

#[test]
fn test_bulk_copy_ram_overlap_forward() {
    let mut mem = Memory::new();
    // Write [1,2,3,4,5,6,7,8] at 0xD0000
    for i in 0..8 {
        mem.ram[0xD0000 + i] = (i + 1) as u8;
    }
    // Overlapping copy: src=D0000, dst=D0004, len=8 (forward overlap)
    assert!(mem.bulk_copy_ram(0xD0000, 0xD0004, 8));
    // copy_within uses memmove semantics, so source is read before overwrite
    let expected = [1u8, 2, 3, 4, 1, 2, 3, 4, 5, 6, 7, 8];
    for i in 0..12 {
        assert_eq!(
            mem.ram[0xD0000 + i],
            expected[i],
            "mismatch at offset {}",
            i
        );
    }
}

#[test]
fn test_bulk_copy_ram_rejects_vga_overlap() {
    let mut mem = Memory::new();
    // Source overlaps VGA range [0xA0000..0xC0000)
    assert!(!mem.bulk_copy_ram(0xA0000, 0xD0000, 0x100));
    // Dest overlaps VGA range
    assert!(!mem.bulk_copy_ram(0xD0000, 0xB0000, 0x100));
}

/// MDD-BUG-CRU-01470: a bulk copy into the write-protected BIOS
/// F-segment is refused and leaves the HLE stubs intact, so the caller
/// falls back to the per-byte path that drops the writes. Copying OUT of
/// the F-segment is still allowed.
#[test]
fn test_bulk_copy_ram_refuses_the_write_protected_bios_segment() {
    let mut mem = Memory::new();
    let iret = Memory::seg_offset_to_linear(0xF000, 0xFF00);
    assert_eq!(mem.read_u8(iret), 0xCF, "precondition: the IRET stub");
    for (dst, len) in [(0xF0000, 0x1000), (0xFF000, 0x1000), (0xEFF00, 0x200)] {
        assert!(
            !mem.bulk_copy_ram(0x1000, dst, len),
            "copy to {dst:#x}+{len:#x}"
        );
    }
    assert_eq!(mem.read_u8(iret), 0xCF, "the IRET stub survives");

    assert!(
        mem.bulk_copy_ram(iret, 0x2000, 1),
        "reading the ROM is fine"
    );
    assert_eq!(mem.read_u8(0x2000), 0xCF);
}

#[test]
fn test_bulk_copy_ram_zero_length() {
    let mut mem = Memory::new();
    // Zero-length copies always succeed (even with bogus addresses)
    assert!(mem.bulk_copy_ram(0xFFFF_FFFF, 0xFFFF_FFFF, 0));
}

#[test]
fn test_bulk_copy_ram_clamps_to_ram_bounds() {
    let mut mem = Memory::new();
    let ram_len = mem.ram.len();
    // Write a byte near the end of RAM
    if ram_len > 16 {
        mem.ram[ram_len - 16] = 0xAB;
        // Copy that starts within RAM but len would exceed — should not panic
        assert!(mem.bulk_copy_ram(ram_len - 16, 0xD0000, 32));
    }
}

#[test]
fn test_bulk_read_ram_basic() {
    let mut mem = Memory::new();
    // Write pattern at 0xD0000
    for i in 0..8 {
        mem.ram[0xD0000 + i] = 0xAA + i as u8;
    }
    let mut buf = [0u8; 8];
    assert!(mem.bulk_read_ram(0xD0000, 8, &mut buf));
    for i in 0..8 {
        assert_eq!(buf[i], 0xAA + i as u8);
    }
}

#[test]
fn test_bulk_read_ram_rejects_vga() {
    let mem = Memory::new();
    let mut buf = [0u8; 256];
    // Reading from VGA range should fail
    assert!(!mem.bulk_read_ram(0xA0000, 256, &mut buf));
}

// ========== A20 gate edge cases ==========

#[test]
fn test_a20_disabled_wraps_bit20_on_read() {
    let mut mem = Memory::new();
    // Write a recognisable value just above 1MB (in HMA)
    let hma_addr = 0x10_0000; // Exactly at 1MB boundary (bit 20 set)
    mem.ram[hma_addr] = 0xAB;
    mem.ram[0] = 0x00; // Zero at wrapped location

    // With A20 enabled, we read HMA directly
    assert_eq!(mem.read_u8(hma_addr), 0xAB);

    // Disable A20: bit 20 cleared → wraps to 0x00000
    mem.set_a20(false);
    assert!(!mem.a20_enabled());
    assert_eq!(mem.read_u8(hma_addr), 0x00);
}

#[test]
fn test_a20_disabled_wraps_bit20_on_write() {
    let mut mem = Memory::new();
    mem.set_a20(false);

    // Write to address with bit 20 set — should wrap to low address
    let target = 0x10_0100; // bit 20 set → wraps to 0x0100
    mem.write_u8(target, 0xCC);
    assert_eq!(mem.ram[0x0100], 0xCC);

    // The HMA location should be unchanged
    assert_ne!(mem.ram[target], 0xCC);
}

#[test]
fn test_a20_disabled_read_u16_wraps() {
    let mut mem = Memory::new();
    mem.ram[0x500] = 0x11;
    mem.ram[0x501] = 0x22;
    mem.set_a20(false);

    // Read from 0x100500 — bit 20 set, wraps to 0x500
    let val = mem.read_u16(0x10_0500);
    assert_eq!(val, 0x2211);
}

#[test]
fn test_a20_disabled_read_u32_wraps() {
    let mut mem = Memory::new();
    mem.ram[0x600] = 0xAA;
    mem.ram[0x601] = 0xBB;
    mem.ram[0x602] = 0xCC;
    mem.ram[0x603] = 0xDD;
    mem.set_a20(false);

    let val = mem.read_u32(0x10_0600);
    assert_eq!(val, 0xDDCCBBAA);
}

#[test]
fn test_a20_disabled_write_u16_wraps() {
    let mut mem = Memory::new();
    mem.set_a20(false);
    mem.write_u16(0x10_0800, 0xBEEF);
    assert_eq!(mem.ram[0x800], 0xEF);
    assert_eq!(mem.ram[0x801], 0xBE);
}

#[test]
fn test_a20_disabled_write_u32_wraps() {
    let mut mem = Memory::new();
    mem.set_a20(false);
    mem.write_u32(0x10_0900, 0xDEADBEEF);
    assert_eq!(
        u32::from_le_bytes([
            mem.ram[0x900],
            mem.ram[0x901],
            mem.ram[0x902],
            mem.ram[0x903],
        ]),
        0xDEADBEEF
    );
}

#[test]
fn test_a20_disabled_load_wraps() {
    let mut mem = Memory::new();
    mem.set_a20(false);
    mem.load(0x10_0A00, &[0x11, 0x22, 0x33]).unwrap();
    assert_eq!(mem.ram[0xA00], 0x11);
    assert_eq!(mem.ram[0xA01], 0x22);
    assert_eq!(mem.ram[0xA02], 0x33);
}

#[test]
fn test_a20_toggle_state() {
    let mut mem = Memory::new();
    assert!(mem.a20_enabled()); // starts enabled
    mem.set_a20(false);
    assert!(!mem.a20_enabled());
    mem.set_a20(true);
    assert!(mem.a20_enabled());
}

// ========== Read edge cases ==========

#[test]
fn test_read_u8_beyond_usable_returns_0xff() {
    let mut mem = Memory::new();
    mem.set_a20(true);
    // Read beyond RAM (no MMIO) returns open-bus 0xFF
    let val = mem.read_u8(MAX_RAM_SIZE + 0x1000);
    assert_eq!(val, 0xFF);
}

#[test]
fn test_read_byte_checked_within_bounds() {
    let mut mem = Memory::new();
    mem.write_u8(0x3000, 0x42);
    assert_eq!(mem.read_byte_checked(0x3000), Some(0x42));
}

#[test]
fn test_read_u16_slow_path_video_range() {
    let mut mem = Memory::new();
    // In default text mode (linear handler), writes go directly to RAM
    mem.ram[VIDEO_BASE] = 0x41;
    mem.ram[VIDEO_BASE + 1] = 0x07;
    // read_u16 at VIDEO_BASE should use the slow path (per-byte read_u8)
    let val = mem.read_u16(VIDEO_BASE);
    assert_eq!(val, 0x0741);
}

#[test]
fn test_read_u32_slow_path_video_range() {
    let mut mem = Memory::new();
    mem.ram[VIDEO_BASE] = 0x11;
    mem.ram[VIDEO_BASE + 1] = 0x22;
    mem.ram[VIDEO_BASE + 2] = 0x33;
    mem.ram[VIDEO_BASE + 3] = 0x44;
    let val = mem.read_u32(VIDEO_BASE);
    assert_eq!(val, 0x44332211);
}

// ========== GDT Snoop Tests ==========

/// A memory with the HLE EMS frame claimed and a two-page backing pool.
fn ems_frame_memory() -> Memory {
    let mut mem = Memory::new();
    mem.init_ems_pool(2).expect("EMS pool");
    mem.claim_hle_ems_frame(EMS_FRAME_BASE, EMS_FRAME_SIZE)
        .expect("claim EMS frame");
    mem
}

/// Clears both dirty flags and returns the current descriptor generation.
fn settle_descriptor_state(mem: &Memory) -> u64 {
    mem.take_gdt_dirty();
    mem.take_ldt_dirty();
    mem.dynrec_descriptor_generation()
}

/// MDD-BUG-CRU-01033: a GDT in an unmapped page-frame slot still sees
/// guest byte writes.
#[test]
fn unmapped_ems_frame_write_dirties_a_gdt_there() {
    let mut mem = ems_frame_memory();
    mem.set_gdt_snoop_range(EMS_FRAME_BASE as u32, 0xFF);
    let generation = settle_descriptor_state(&mem);

    mem.write_u8(EMS_FRAME_BASE + 13, 0x92);

    assert!(mem.take_gdt_dirty(), "GDT byte written through the frame");
    assert!(mem.dynrec_descriptor_generation() > generation);
}

/// MDD-BUG-CRU-01033: a scalar write to a mapped slot lands in the pool;
/// an LDT at that frame address must still be marked dirty.
#[test]
fn mapped_ems_frame_write_dirties_an_ldt_there() {
    let mut mem = ems_frame_memory();
    mem.set_ems_frame_page(1, Some(0)).expect("map slot 1");
    let ldt = EMS_FRAME_BASE + EMS_PAGE_SIZE;
    mem.set_ldt_snoop_range(ldt as u32, 0xFF);
    let generation = settle_descriptor_state(&mem);

    mem.write_u16(ldt + 8, 0xFFFF);

    assert!(
        mem.take_ldt_dirty(),
        "LDT word written through a mapped slot"
    );
    assert!(mem.dynrec_descriptor_generation() > generation);
}

/// MDD-BUG-CRU-01033: an EMS move writes the backing pool directly; the
/// frame alias that shows those bytes holds the GDT. A pool write to an
/// unmapped page is invisible in the frame and leaves the GDT clean.
#[test]
fn ems_backing_pool_write_dirties_the_gdt_at_its_frame_alias() {
    let mut mem = ems_frame_memory();
    mem.set_ems_frame_page(0, Some(0)).expect("map slot 0");
    mem.set_gdt_snoop_range(EMS_FRAME_BASE as u32, 0xFF);
    let generation = settle_descriptor_state(&mem);

    mem.write_ems_backing_range(1, 16, &[0xAA; 8])
        .expect("unmapped pool page");
    assert!(!mem.take_gdt_dirty(), "page 1 is not in the frame");
    assert_eq!(mem.dynrec_descriptor_generation(), generation);

    mem.write_ems_backing_range(0, 16, &[0xFF, 0xFF, 0, 0, 0, 0x9A, 0xCF, 0])
        .expect("mapped pool page");
    assert!(mem.take_gdt_dirty(), "descriptor rewritten via the pool");
    assert!(mem.dynrec_descriptor_generation() > generation);
}

/// MDD-BUG-CRU-01033: remapping the slot under a GDT swaps its bytes.
#[test]
fn ems_frame_remap_dirties_a_gdt_in_that_slot() {
    let mut mem = ems_frame_memory();
    mem.set_ems_frame_page(0, Some(0)).expect("map slot 0");
    mem.set_gdt_snoop_range(EMS_FRAME_BASE as u32, 0xFF);
    let generation = settle_descriptor_state(&mem);

    mem.set_ems_frame_page(0, Some(1)).expect("remap slot 0");

    assert!(mem.take_gdt_dirty(), "the GDT now reads page 1's bytes");
    assert!(mem.dynrec_descriptor_generation() > generation);
}

#[test]
fn test_gdt_snoop_range_and_accessors() {
    let mut mem = Memory::new();
    assert_eq!(mem.gdt_snoop_base(), 0);
    assert_eq!(mem.gdt_snoop_end(), 0);
    assert_eq!(mem.ldt_snoop_base(), 0);
    assert_eq!(mem.ldt_snoop_end(), 0);
    assert!(!mem.has_descriptor_snoop());

    mem.set_gdt_snoop_range(0x10000, 0xFF); // 256-byte GDT
    assert_eq!(mem.gdt_snoop_base(), 0x10000);
    assert_eq!(mem.gdt_snoop_end(), 0x10100);
    assert!(mem.has_descriptor_snoop());
}

#[test]
fn test_take_gdt_dirty_when_clean() {
    let mut mem = Memory::new();
    mem.set_gdt_snoop_range(0x10000, 0xFF);
    // Not dirty yet
    assert!(!mem.take_gdt_dirty());
}

#[test]
fn test_take_gdt_dirty_when_dirty() {
    let mut mem = Memory::new();
    mem.set_gdt_snoop_range(0x10000, 0xFF);
    // Manually trigger dirty via snoop_gdt_write_ext
    mem.snoop_gdt_write_ext(0x10000, 0x42, 1);
    assert!(mem.take_gdt_dirty());
    // Should be cleared after take
    assert!(!mem.take_gdt_dirty());
}

#[test]
fn test_snoop_gdt_write_ext_outside_range() {
    let mut mem = Memory::new();
    mem.set_gdt_snoop_range(0x10000, 0xFF);
    // Write outside GDT range should not set dirty
    mem.snoop_gdt_write_ext(0x20000, 0x42, 1);
    assert!(!mem.take_gdt_dirty());
}

#[test]
fn test_snoop_gdt_write_ext_lower_bytes() {
    let mut mem = Memory::new();
    mem.set_gdt_snoop_range(0x10000, 0xFF);
    // Write to byte 0 of entry 0 (lower bytes, no upper-byte tracing)
    mem.snoop_gdt_write_ext(0x10000, 0x42, 1);
    assert!(mem.take_gdt_dirty());
}

#[test]
fn test_snoop_gdt_write_ext_upper_bytes_triggers_trace() {
    let mut mem = Memory::new();
    mem.set_gdt_snoop_range(0x10000, 0xFF);
    // Write to byte 5 of entry 0 (access byte - upper descriptor region)
    with_trace_subscriber(|| {
        mem.snoop_gdt_write_ext(0x10005, 0x93, 1);
    });
    assert!(mem.take_gdt_dirty());
}

#[test]
fn test_snoop_gdt_write_ext_byte6_write() {
    let mut mem = Memory::new();
    // Use entry index >= 8 to trigger detailed byte 6 logging
    mem.set_gdt_snoop_range(0x10000, 0x1FF); // 512-byte GDT, 64 entries
                                             // Entry 8, byte 6 = offset 64 + 6 = 70 = 0x46
    with_trace_subscriber(|| {
        mem.snoop_gdt_write_ext(0x10046, 0x40, 1);
    });
    assert!(mem.take_gdt_dirty());
}

#[test]
fn test_snoop_gdt_write_ext_word_covering_byte6() {
    let mut mem = Memory::new();
    mem.set_gdt_snoop_range(0x10000, 0x1FF);
    // Entry 8, byte 5-6: word write at byte 5 of entry 8 = offset 64+5=69
    with_trace_subscriber(|| {
        mem.snoop_gdt_write_ext(0x10045, 0x9340, 2);
    });
    assert!(mem.take_gdt_dirty());
}

#[test]
fn test_snoop_gdt_write_ext_dword_covering_byte6() {
    let mut mem = Memory::new();
    mem.set_gdt_snoop_range(0x10000, 0x1FF);
    // Entry 8, byte 4-7: dword write at byte 4 of entry 8 = offset 64+4=68
    with_trace_subscriber(|| {
        mem.snoop_gdt_write_ext(0x10044, 0x0040_9300, 4);
    });
    assert!(mem.take_gdt_dirty());
}

#[test]
fn test_snoop_gdt_write_ext_overlapping_range_starting_before_table() {
    let mut mem = Memory::new();
    mem.set_gdt_snoop_range(0x10000, 0xFF);

    mem.snoop_gdt_write_ext(0x0FFFF, 0xAABB, 2);

    assert!(
        mem.take_gdt_dirty(),
        "unaligned write starting before the GDT must still dirty it"
    );
}

#[test]
fn test_write_u32_physical_snoops_overlapping_descriptor_tables() {
    let mut mem = Memory::new();
    mem.set_gdt_snoop_range(0x10000, 0xFF);

    mem.write_u32_physical(0x0FFFF, 0xAABB_CCDD);

    assert!(
        mem.take_gdt_dirty(),
        "physical page-table writes overlapping the GDT must dirty it"
    );
    assert!(!mem.take_ldt_dirty());

    mem.set_ldt_snoop_range(0x20000, 0xFF);

    mem.write_u32_physical(0x200FE, 0x1122_3344);

    assert!(
        mem.take_ldt_dirty(),
        "physical page-table writes overlapping the LDT must dirty it"
    );
    assert!(!mem.take_gdt_dirty());
}

#[test]
fn test_snoop_gdt_write_ext_unset_range() {
    let mem = Memory::new();
    // gdt_snoop_end == 0, should not set dirty
    mem.snoop_gdt_write_ext(0x10000, 0x42, 1);
    assert!(!mem.take_gdt_dirty());
}

#[test]
fn test_take_ldt_dirty_when_dirty() {
    let mut mem = Memory::new();
    mem.set_ldt_snoop_range(0x20000, 0xFF);
    assert_eq!(mem.ldt_snoop_base(), 0x20000);
    assert_eq!(mem.ldt_snoop_end(), 0x20100);
    assert!(mem.has_descriptor_snoop());
    mem.snoop_gdt_write_ext(0x20008, 0x9B, 1);
    assert!(mem.take_ldt_dirty());
    assert!(!mem.take_ldt_dirty());
}

#[test]
fn test_memory_write_u8_dirties_ldt_only_snoop_range() {
    let mut mem = Memory::new();
    mem.set_ldt_snoop_range(0x20000, 0xFF);

    mem.write_u8(0x20010, 0x9B);

    assert!(
        mem.take_ldt_dirty(),
        "generic memory writes used by dynrec helpers must dirty LDT ranges"
    );
    assert!(!mem.take_gdt_dirty());
}

#[test]
fn test_ldt_snoop_does_not_mark_gdt_dirty() {
    let mut mem = Memory::new();
    mem.set_gdt_snoop_range(0x10000, 0xFF);
    mem.set_ldt_snoop_range(0x20000, 0xFF);
    mem.snoop_gdt_write_ext(0x20008, 0x9B, 1);
    assert!(!mem.take_gdt_dirty());
    assert!(mem.take_ldt_dirty());
}

#[test]
fn test_clear_ldt_snoop_range_disables_dirty_tracking() {
    let mut mem = Memory::new();
    mem.set_ldt_snoop_range(0x20000, 0xFF);
    mem.clear_ldt_snoop_range();
    mem.snoop_gdt_write_ext(0x20008, 0x9B, 1);
    assert!(!mem.take_ldt_dirty());
}

// ========== MMIO Region Tests ==========

#[test]
fn test_mmio_region_add_and_check() {
    let mut mem = Memory::new();
    let addr = 0x6000_0000;
    assert!(!mem.has_mmio_region(addr));

    mem.add_mmio_region(addr, 1024);
    assert!(mem.has_mmio_region(addr));
    assert!(mem.is_mmio_address(addr));
    assert!(mem.is_mmio_address(addr + 512));
    assert!(!mem.is_mmio_address(addr + 1024));
}

#[test]
fn test_mmio_region_find_containing() {
    let mut mem = Memory::new();
    let addr = 0x6000_0000;
    mem.add_mmio_region(addr, 4096);

    let found = mem.find_mmio_region_containing(addr + 100);
    assert_eq!(found, Some((addr, 4096)));

    // Outside region
    let not_found = mem.find_mmio_region_containing(addr + 8000);
    assert!(not_found.is_none());
}

#[test]
fn test_mmio_region_first() {
    let mut mem = Memory::new();
    assert!(mem.first_mmio_region().is_none());

    mem.add_mmio_region(0x7000_0000, 256);
    assert_eq!(mem.first_mmio_region(), Some((0x7000_0000, 256)));
}

#[test]
fn test_mmio_region_remove() {
    let mut mem = Memory::new();
    let addr = 0x6000_0000;
    mem.add_mmio_region(addr, 1024);
    assert!(mem.has_mmio_region(addr));

    mem.remove_mmio_region(addr);
    assert!(!mem.has_mmio_region(addr));
}

#[test]
fn test_mmio_slice_mut_and_slice() {
    let mut mem = Memory::new();
    let addr = 0x6000_0000;
    mem.add_mmio_region(addr, 64);

    // Write via mutable slice
    if let Some(slice) = mem.mmio_slice_mut(addr) {
        slice[0] = 0xAB;
        slice[1] = 0xCD;
    }

    // Read back via immutable slice
    let slice = mem.mmio_slice(addr).unwrap();
    assert_eq!(slice[0], 0xAB);
    assert_eq!(slice[1], 0xCD);

    // Non-existent region
    assert!(mem.mmio_slice(0x9999_0000).is_none());
    assert!(mem.mmio_slice_mut(0x9999_0000).is_none());
}

// ========== Write Tracking Tests ==========

#[test]
fn test_write_tracking_lifecycle() {
    let mut mem = Memory::new();
    assert!(!mem.is_write_tracking_enabled());

    mem.enable_write_tracking();
    assert!(mem.is_write_tracking_enabled());

    mem.set_write_context(0x1000, 0x2000);
    assert!(mem.write_context().is_some());
    let ctx = mem.write_context().unwrap();
    assert_eq!(ctx.cs, 0x1000);
    assert_eq!(ctx.ip, 0x2000);

    mem.clear_write_context();
    assert!(mem.write_context().is_none());

    mem.disable_write_tracking();
    assert!(!mem.is_write_tracking_enabled());
}

#[test]
fn test_write_tracking_records_writes() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x0100, 0x0200);

    // Write through the slow path (need A20 off or tracking enabled)
    mem.write_u8(0x50000, 0xAB);
    assert_eq!(mem.pending_write_count(), 1);

    let writes = mem.drain_pending_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].source_cs, 0x0100);
    assert_eq!(writes[0].source_ip, 0x0200);
    assert_eq!(writes[0].new_value, 0xAB);
    assert_eq!(writes[0].size, 1);
    assert!(!writes[0].is_hle_handler);

    // Drain empties the buffer
    assert_eq!(mem.pending_write_count(), 0);
}

#[test]
fn test_write_tracking_hle_handler() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x0100, 0x0200);

    assert!(!mem.is_hle_handler_active());
    mem.set_hle_handler_active(true);
    assert!(mem.is_hle_handler_active());

    mem.write_u8(0x50000, 0xCC);

    let writes = mem.drain_pending_writes();
    assert_eq!(writes.len(), 1);
    assert!(writes[0].is_hle_handler);

    mem.set_hle_handler_active(false);
}

#[test]
fn test_write_tracking_no_context_no_record() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    // No write context set
    mem.write_u8(0x50000, 0xDD);
    assert_eq!(mem.pending_write_count(), 0);
}

// ========== Instruction Cache / SMC Tests ==========

#[test]
fn test_instr_cache_dirty_flag() {
    let mut mem = Memory::new();
    assert!(!mem.is_instr_cache_dirty());

    // Mark a page as executing
    mem.mark_instr_exec_page(0x10000);

    // Write to that page should set dirty
    mem.write_u8(0x10000, 0x90);

    assert!(mem.is_instr_cache_dirty());
    // take should clear
    assert!(mem.take_instr_cache_dirty());
    assert!(!mem.is_instr_cache_dirty());
}

#[test]
fn test_smc_notifier_bind_and_take() {
    let mut mem = Memory::new();
    let notifier = Arc::new(TestSmcNotify::default());
    mem.bind_smc(Some(notifier.clone()));
    mem.set_smc_has_code_pages(true);
    mem.set_page_watched(0x50000 >> 12, true);

    // Write should trigger SMC notification
    mem.write_u8(0x50000, 0xCC);
    let writes = notifier.snapshot();
    assert!(!writes.is_empty());

    // Take notifier
    let taken = mem.take_smc_notifier();
    assert!(taken.is_some());

    // After take, no notifier
    assert!(mem.take_smc_notifier().is_none());
}

// ========== Physical Mapping Tests ==========

#[test]
fn test_physical_mapping_add_remove_lookup() {
    let mut mem = Memory::new();

    // Add a mapping
    mem.add_physical_mapping(0x200000, 0x100000, 0x10000);

    // Lookup should find it
    let phys = mem.map_linear_physical(0x200000);
    assert_eq!(phys, Some(0x100000));

    let phys_mid = mem.map_linear_physical(0x200100);
    assert_eq!(phys_mid, Some(0x100100));

    // Outside mapping
    assert!(mem.map_linear_physical(0x300000).is_none());

    // Remove
    assert!(mem.remove_physical_mapping(0x200000));
    assert!(mem.map_linear_physical(0x200000).is_none());
}

#[test]
fn test_physical_mapping_remove_nonexistent() {
    let mut mem = Memory::new();
    assert!(!mem.remove_physical_mapping(0x999999));
}

#[test]
fn test_physical_mapping_zero_size() {
    let mut mem = Memory::new();
    mem.add_physical_mapping(0x200000, 0x100000, 0);
    assert!(mem.map_linear_physical(0x200000).is_none());
}

#[test]
fn test_physical_mapping_replace() {
    let mut mem = Memory::new();
    mem.add_physical_mapping(0x200000, 0x100000, 0x10000);
    // Replace with new physical address
    mem.add_physical_mapping(0x200000, 0x300000, 0x10000);
    assert_eq!(mem.map_linear_physical(0x200000), Some(0x300000));
}

// ========== TLB Stats Tests ==========

#[test]
fn test_tlb_stats_disabled() {
    let stats = TlbStats::disabled();
    assert!(!stats.is_enabled());
    assert_eq!(stats.total_lookups(), 0);
    assert_eq!(stats.hit_rate(), 0.0);
    assert_eq!(stats.miss_rate(), 0.0);

    // Recording should be no-op when disabled
    stats.record_hit();
    stats.record_miss();
    stats.record_flush_full();
    stats.record_flush_single();
    assert_eq!(stats.hit_count(), 0);
    assert_eq!(stats.miss_count(), 0);
    assert_eq!(stats.flush_full_count(), 0);
    assert_eq!(stats.flush_single_count(), 0);
}

#[test]
fn test_tlb_stats_enabled_counting() {
    let mut stats = TlbStats::disabled();
    stats.enable();
    assert!(stats.is_enabled());

    stats.record_hit();
    stats.record_hit();
    stats.record_miss();

    assert_eq!(stats.hit_count(), 2);
    assert_eq!(stats.miss_count(), 1);
    assert_eq!(stats.total_lookups(), 3);
    assert!((stats.hit_rate() - 66.666).abs() < 1.0);
    assert!((stats.miss_rate() - 33.333).abs() < 1.0);

    stats.record_flush_full();
    stats.record_flush_single();
    stats.record_flush_single();
    assert_eq!(stats.flush_full_count(), 1);
    assert_eq!(stats.flush_single_count(), 2);
}

#[test]
fn test_tlb_stats_summary_shows_disabled_message() {
    let stats = TlbStats::disabled();
    let summary = stats.summary();
    assert!(summary.contains("disabled"));
}

#[test]
fn test_tlb_stats_summary_with_data() {
    let mut stats = TlbStats::disabled();
    stats.enable();
    stats.record_hit();
    stats.record_miss();
    let summary = stats.summary();
    assert!(summary.contains("lookups:"));
    assert!(summary.contains("hits:"));
    assert!(summary.contains("misses:"));
}

#[test]
fn test_tlb_stats_debug_format() {
    let stats = TlbStats::disabled();
    let debug = format!("{:?}", stats);
    assert!(debug.contains("TlbStats"));
    assert!(debug.contains("enabled"));
}

#[test]
fn test_tlb_stats_summary_on_memory() {
    let mem = Memory::new();
    // Default stats are from env — likely disabled in test
    let summary = mem.tlb_stats_summary();
    // Either None (disabled) or Some (enabled via env) is fine
    let _ = summary;

    // With enabled stats
    let mut mem2 = Memory::new();
    mem2.tlb_stats_mut().enable();
    mem2.tlb_lookup(0x42); // miss
    let summary = mem2.tlb_stats_summary();
    assert!(summary.is_some());
    assert!(summary.unwrap().contains("lookups:"));
}
