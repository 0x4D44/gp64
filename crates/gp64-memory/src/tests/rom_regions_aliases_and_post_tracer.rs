use super::*;

// ========== RomRegions (Package 7) ==========
//
// Cross-boundary write policy: a u16/u32 write
// straddling a ROM/non-ROM boundary PARTIALLY COMMITS — the non-ROM
// bytes land, only the ROM bytes are ignored. A write lying wholly
// inside a ROM aperture still drops entirely. Rationale and the
// hardware rationale live on the RomRegions API notes above.

#[test]
fn rom_region_write_u8_dropped() {
    let mut mem = Memory::new();
    mem.mark_rom_range(0xC0000..=0xFFFFF);
    // Existing BIOS area (0xF0000+) was already write-protected via
    // `allow_rom_writes`; the new gate extends this to 0xC0000..0xEFFFF.
    for addr in [0xC0000usize, 0xF0000, 0xFFFFF] {
        let before = mem.read_u8(addr);
        mem.write_u8(addr, 0xAB);
        assert_eq!(
            mem.read_u8(addr),
            before,
            "write_u8 should be dropped at ROM addr {addr:#X}"
        );
    }
}

#[test]
fn rom_region_write_u8_passes_below_rom() {
    let mut mem = Memory::new();
    mem.mark_rom_range(0xC0000..=0xFFFFF);
    // Byte immediately below the ROM window must remain writable.
    let addr = 0xBFFFF;
    mem.write_u8(addr, 0xAB);
    assert_eq!(mem.read_u8(addr), 0xAB);
}

#[test]
fn rom_region_write_u16_dropped() {
    let mut mem = Memory::new();
    mem.mark_rom_range(0xC0000..=0xC0FFF);
    let addr = 0xC0000;
    let b0_before = mem.read_u8(addr);
    let b1_before = mem.read_u8(addr + 1);
    mem.write_u16(addr, 0xABCD);
    assert_eq!(mem.read_u8(addr), b0_before);
    assert_eq!(mem.read_u8(addr + 1), b1_before);
}

#[test]
fn rom_region_write_u32_dropped() {
    let mut mem = Memory::new();
    mem.mark_rom_range(0xC0000..=0xC0FFF);
    let addr = 0xC0000;
    let before: [u8; 4] = [
        mem.read_u8(addr),
        mem.read_u8(addr + 1),
        mem.read_u8(addr + 2),
        mem.read_u8(addr + 3),
    ];
    mem.write_u32(addr, 0x1234_5678);
    assert_eq!(mem.read_u8(addr), before[0]);
    assert_eq!(mem.read_u8(addr + 1), before[1]);
    assert_eq!(mem.read_u8(addr + 2), before[2]);
    assert_eq!(mem.read_u8(addr + 3), before[3]);
}

#[test]
fn rom_region_cross_boundary_write_u32() {
    // Cross-boundary write_u32: first byte at 0xBFFFF (non-ROM),
    // last three bytes at 0xC0000..=0xC0002 (ROM).
    //
    // Policy: PARTIAL COMMIT. The straddle is an
    // unaligned access that the CPU splits into two independently-decoded
    // bus cycles, so the RAM byte lands and only the ROM bytes are
    // ignored. This reverses the earlier whole-transaction drop and now
    // agrees with `test_write_u32_partial_rom_non_contiguous`, which pins
    // the same semantics for the fixed BIOS window.
    let mut mem = Memory::new();
    mem.mark_rom_range(0xC0000..=0xFFFFF);
    let low_addr = 0xBFFFF;
    let before_c0 = mem.read_u8(0xC0000);
    mem.write_u32(low_addr, 0xDEAD_BEEF);
    assert_eq!(
        mem.read_u8(low_addr),
        0xEF,
        "cross-boundary u32 must commit its non-ROM byte"
    );
    assert_eq!(
        mem.read_u8(0xC0000),
        before_c0,
        "the ROM bytes of the same access must still be ignored"
    );
    assert_eq!(mem.read_u8(0xC0001), before_c0);
    assert_eq!(mem.read_u8(0xC0002), before_c0);
}

#[test]
fn option_rom_cross_boundary_write_u32_commits_the_outside_bytes() {
    // The option-ROM decoder is a SEPARATE aperture from RomRegions, and
    // it is the one used by the real-boot path (the VGA
    // option-ROM window of bug 71). Both pre-existing option-ROM write
    // tests write wholly inside the backing, so they passed identically
    // regardless of the partial-write policy and pinned nothing about the
    // straddle. Pin both edges here.
    let mut mem = Memory::new();
    mem.set_option_rom_backing(&[0x55; 0x8000]);
    mem.configure_option_rom_decoder(0xC0000, true);

    // Trailing edge: span starts inside RAM below the aperture.
    mem.write_u32(0xBFFFE, 0xCAFE_BABE);
    assert_eq!(
        &mem.slice(0xBFFFE, 2)[..],
        &[0xBE, 0xBA],
        "the two bytes below the option-ROM aperture must commit"
    );
    assert!(
        mem.option_rom_backing().iter().all(|&b| b == 0x55),
        "no byte of the same access may reach the option-ROM backing"
    );
}

#[test]
fn rom_region_cross_boundary_write_u16() {
    // The u16 mirror of the above: low byte in RAM, high byte in ROM.
    let mut mem = Memory::new();
    mem.mark_rom_range(0xC0000..=0xFFFFF);
    let before_c0 = mem.read_u8(0xC0000);
    mem.write_u16(0xBFFFF, 0xBEEF);
    assert_eq!(
        mem.read_u8(0xBFFFF),
        0xEF,
        "cross-boundary u16 must commit its non-ROM byte"
    );
    assert_eq!(mem.read_u8(0xC0000), before_c0);
}

#[test]
fn rom_region_fully_inside_write_u32_still_drops() {
    // Partial commit must not weaken the wholly-in-ROM case: every byte
    // is on a ROM page, so nothing may change.
    let mut mem = Memory::new();
    mem.mark_rom_range(0xC0000..=0xFFFFF);
    let before = [
        mem.read_u8(0xC1000),
        mem.read_u8(0xC1001),
        mem.read_u8(0xC1002),
        mem.read_u8(0xC1003),
    ];
    mem.write_u32(0xC1000, 0xDEAD_BEEF);
    for (i, b) in before.iter().enumerate() {
        assert_eq!(mem.read_u8(0xC1000 + i), *b, "byte {i} must be unchanged");
    }
}

#[test]
fn rom_region_default_empty_allows_writes() {
    // No `mark_rom_*` calls → `has_rom_regions` stays false →
    // writes at 0xC0000 succeed (modulo existing `allow_rom_writes`
    // gate which only covers 0xF0000..=0xFFFFF, not 0xC0000).
    let mut mem = Memory::new();
    mem.write_u8(0xC0000, 0xAB);
    assert_eq!(mem.read_u8(0xC0000), 0xAB);
}

#[test]
fn rom_region_page_granularity() {
    // Mark only 0xC0000..=0xCFFFF (16 pages) as ROM. The page at 0xD0000
    // must remain writable; the last byte of the marked range must be
    // dropped.
    let mut mem = Memory::new();
    mem.mark_rom_range(0xC0000..=0xCFFFF);

    // Unmarked page immediately above: writable.
    mem.write_u8(0xD0000, 0xAB);
    assert_eq!(mem.read_u8(0xD0000), 0xAB);

    // Last byte of the marked range: write dropped.
    let before = mem.read_u8(0xCFFFF);
    mem.write_u8(0xCFFFF, 0xCD);
    assert_eq!(mem.read_u8(0xCFFFF), before);
    assert_eq!(before, 0);
}

#[test]
fn rom_region_is_rom_addr_direct() {
    // Mark a single page via `mark_rom_page` and verify `is_rom_addr`
    // reports true for any address on that page and false elsewhere.
    let mut mem = Memory::new();
    let marked_page = 0xC0000 >> ROM_PAGE_SHIFT;
    mem.mark_rom_page(marked_page);

    assert!(mem.is_rom_addr(0xC0000));
    assert!(mem.is_rom_addr(0xC0FFF));
    assert!(!mem.is_rom_addr(0xC1000));
    assert!(!mem.is_rom_addr(0xBFFFF));
}

// ========== Real-boot high-memory alias (Package 8) ==========
//
// Reads from 0xFFFF_0000..=0xFFFF_FFFF fold back to 0xF0000..=0xFFFFF.
// Writes in the alias window are dropped (ROM). The default is disabled.

#[test]
fn high_mem_alias_disabled_by_default() {
    // `allow_rom_writes` lets us seed the BIOS region; the alias is the
    // thing under test. With the alias disabled, reads at 0xFFFF_FFF0
    // must NOT fold — they return open-bus (0xFF) since the backing RAM
    // does not extend that far.
    let mut mem = Memory::new();
    mem.allow_rom_writes = true;
    mem.write_u8(0xFFFF0, 0x5A);
    mem.write_u8(0xFFFFF, 0xA5);
    assert!(!mem.high_mem_alias_enabled());
    // Folded target reads back the sentinel.
    assert_eq!(mem.read_u8(0xFFFF0), 0x5A);
    assert_eq!(mem.read_u8(0xFFFFF), 0xA5);
    // Alias window is NOT folded → open-bus, not the sentinel.
    assert_eq!(mem.read_u8(0xFFFF_FFF0), 0xFF);
    assert_eq!(mem.read_u8(0xFFFF_FFFF), 0xFF);
}

#[test]
fn high_mem_alias_fold_read_u8() {
    let mut mem = Memory::new();
    mem.allow_rom_writes = true;
    // Sentinels at three representative offsets in the 64 KiB window.
    mem.write_u8(0xF0000, 0x11);
    mem.write_u8(0xFFFF0, 0x22);
    mem.write_u8(0xFFFFF, 0x33);
    mem.set_high_mem_alias_enabled(true);
    // Corresponding alias addresses fold back to the BIOS window.
    assert_eq!(mem.read_u8(0xFFFF_0000), 0x11);
    assert_eq!(mem.read_u8(0xFFFF_FFF0), 0x22);
    assert_eq!(mem.read_u8(0xFFFF_FFFF), 0x33);
}

#[test]
fn high_mem_alias_fold_read_u16_u32() {
    let mut mem = Memory::new();
    mem.allow_rom_writes = true;
    // Seed the last 4 bytes of the BIOS image (the reset-vector word
    // lives at 0xFFFF_FFF0 which folds to 0xFFFF0; the last 4 bytes
    // at 0xFFFFC..=0xFFFFF alias to 0xFFFF_FFFC..=0xFFFF_FFFF).
    mem.write_u8(0xFFFFC, 0x78);
    mem.write_u8(0xFFFFD, 0x56);
    mem.write_u8(0xFFFFE, 0x34);
    mem.write_u8(0xFFFFF, 0x12);
    mem.set_high_mem_alias_enabled(true);
    assert_eq!(mem.read_u16(0xFFFF_FFFC), 0x5678);
    assert_eq!(mem.read_u32(0xFFFF_FFFC), 0x1234_5678);
}

#[test]
fn high_mem_alias_write_dropped() {
    let mut mem = Memory::new();
    mem.allow_rom_writes = true;
    // Seed the target byte, then flip to non-ROM (drop writes at the
    // BIOS window via allow_rom_writes=false) and enable the alias.
    mem.write_u8(0xFFFF0, 0x5A);
    mem.allow_rom_writes = false;
    mem.set_high_mem_alias_enabled(true);
    // Writing through the alias must NOT mutate the BIOS byte: the
    // alias window is ROM.
    mem.write_u8(0xFFFF_FFF0, 0xAB);
    mem.write_u16(0xFFFF_FFF0, 0xCDEF);
    mem.write_u32(0xFFFF_FFF0, 0xDEAD_BEEF);
    // Both the alias read and the direct BIOS read must return the
    // original sentinel.
    assert_eq!(mem.read_u8(0xFFFF_FFF0), 0x5A);
    assert_eq!(mem.read_u8(0xFFFF0), 0x5A);
}

#[test]
fn high_mem_alias_setter_roundtrip() {
    let mut mem = Memory::new();
    mem.allow_rom_writes = true;
    mem.write_u8(0xFFFF0, 0x5A);

    mem.set_high_mem_alias_enabled(true);
    assert!(mem.high_mem_alias_enabled());
    assert_eq!(mem.read_u8(0xFFFF_FFF0), 0x5A);

    mem.set_high_mem_alias_enabled(false);
    assert!(!mem.high_mem_alias_enabled());
    // With the alias disabled, the high address is open-bus again.
    assert_eq!(mem.read_u8(0xFFFF_FFF0), 0xFF);
}

#[test]
fn high_mem_alias_reset_vector_word() {
    // Five synthetic sentinel bytes exercise little-endian reads and the
    // instruction-fetch path through the architectural reset-ROM alias.
    let mut mem = Memory::new();
    mem.allow_rom_writes = true;
    let bytes = [0x11, 0x22, 0x33, 0x44, 0x55];
    for (i, b) in bytes.iter().enumerate() {
        mem.write_u8(0xFFFF0 + i, *b);
    }
    mem.set_high_mem_alias_enabled(true);

    // First two sentinel bytes, little-endian.
    assert_eq!(mem.read_u16(0xFFFF_FFF0), 0x2211);
    // First four sentinel bytes, little-endian.
    assert_eq!(mem.read_u32(0xFFFF_FFF0), 0x4433_2211);
    // All five sentinel bytes across the alias.
    let mut buf = [0u8; 5];
    mem.read_bytes(0xFFFF_FFF0, &mut buf);
    assert_eq!(buf, bytes);
}

fn write_bytes_for_test(mem: &mut Memory, addr: usize, bytes: &[u8]) {
    for (offset, byte) in bytes.iter().copied().enumerate() {
        mem.write_u8(addr + offset, byte);
    }
}

fn fix_mp_checksum(bytes: &mut [u8], checksum_index: usize) {
    bytes[checksum_index] = 0;
    let sum = bytes.iter().fold(0u8, |acc, byte| acc.wrapping_add(*byte));
    bytes[checksum_index] = 0u8.wrapping_sub(sum);
}

fn valid_mp_floating_pointer_bytes(config_addr: usize) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(b"_MP_");
    bytes[4..8].copy_from_slice(&(config_addr as u32).to_le_bytes());
    bytes[8] = 1;
    bytes[9] = 4;
    fix_mp_checksum(&mut bytes, 10);
    bytes
}

fn valid_mp_config_table_bytes() -> [u8; 44] {
    let mut bytes = [0u8; 44];
    let len = bytes.len() as u16;
    bytes[0..4].copy_from_slice(b"PCMP");
    bytes[4..6].copy_from_slice(&len.to_le_bytes());
    bytes[6] = 4;
    bytes[8..16].copy_from_slice(b"GP64    ");
    bytes[16..28].copy_from_slice(b"PIC ONLY    ");
    bytes[36..40].copy_from_slice(&0xFEE0_0000u32.to_le_bytes());
    fix_mp_checksum(&mut bytes, 7);
    bytes
}

/// MDD-BUG-CRU-01477: the MP-signature filter does not depend on operand
/// size.
///
/// The filter is content-keyed, and only `read_u8` applied it. A guest
/// that read a suppressed `_MP_` one byte at a time saw 0x00, while the
/// same address read with a 16- or 32-bit load saw the real signature -
/// so an MP-aware guest scanning with word loads found the very table the
/// suppression exists to hide.
///
/// The address matters: `0xF54D0`, which the neighbouring suppression
/// tests use, is ABOVE `VIDEO_BASE` and so never reached the fast paths.
/// This one sits in the EBDA, inside conventional memory, where the fast
/// paths serve every read.
#[test]
fn firmware_mp_suppression_holds_at_every_operand_width() {
    let mut mem = Memory::new();
    mem.allow_rom_writes = true;
    // Paragraph-aligned, below VIDEO_BASE: the fast paths' own domain.
    let fp_addr = 0x9_FC00usize;
    let config_addr = 0x9_FC40usize;
    assert!(
        fp_addr + 4 < VIDEO_BASE,
        "the fixture has to sit where the fast paths serve it, or it \
         tests the slow path twice"
    );
    write_bytes_for_test(
        &mut mem,
        fp_addr,
        &valid_mp_floating_pointer_bytes(config_addr),
    );
    write_bytes_for_test(&mut mem, config_addr, &valid_mp_config_table_bytes());
    mem.set_firmware_mp_table_suppression_enabled(true);

    for addr in [fp_addr, config_addr] {
        assert_eq!(
            mem.read_u8(addr),
            0x00,
            "the byte read must be suppressed at {addr:#X}, or the widths \
             below agree on the wrong answer"
        );

        let bytes = [
            mem.read_u8(addr),
            mem.read_u8(addr + 1),
            mem.read_u8(addr + 2),
            mem.read_u8(addr + 3),
        ];
        assert_eq!(
            mem.read_u16(addr),
            u16::from_le_bytes([bytes[0], bytes[1]]),
            "a word load defeated the filter at {addr:#X}"
        );
        assert_eq!(
            mem.read_u32(addr),
            u32::from_le_bytes(bytes),
            "a dword load defeated the filter at {addr:#X}"
        );
    }

    // And the filter still only touches the signature byte it claims:
    // suppression off, every width sees the real bytes again.
    mem.set_firmware_mp_table_suppression_enabled(false);
    assert_eq!(mem.read_u8(fp_addr), b'_');
    assert_eq!(
        mem.read_u32(fp_addr).to_le_bytes(),
        *b"_MP_",
        "clearing suppression must restore the signature at every width"
    );
}

#[test]
fn firmware_mp_table_signatures_visible_by_default() {
    let mut mem = Memory::new();
    mem.allow_rom_writes = true;
    write_bytes_for_test(&mut mem, 0xF54D0, b"_MP_");
    write_bytes_for_test(&mut mem, 0xF54E0, b"PCMP");

    assert!(!mem.firmware_mp_table_suppression_enabled());
    assert_eq!(mem.read_u8(0xF54D0), b'_');
    assert_eq!(mem.read_u8(0xF54E0), b'P');

    let mut fp = [0u8; 4];
    mem.read_bytes(0xF54D0, &mut fp);
    assert_eq!(&fp, b"_MP_");

    let mut config = [0u8; 4];
    mem.read_bytes(0xF54E0, &mut config);
    assert_eq!(&config, b"PCMP");
}

#[test]
fn firmware_mp_table_suppression_hides_paragraph_aligned_signatures() {
    let mut mem = Memory::new();
    mem.allow_rom_writes = true;
    let fp_addr = 0xF54D0;
    let config_addr = 0xF54E0;
    let fp = valid_mp_floating_pointer_bytes(config_addr);
    let config = valid_mp_config_table_bytes();
    write_bytes_for_test(&mut mem, fp_addr, &fp);
    write_bytes_for_test(&mut mem, config_addr, &config);
    mem.set_firmware_mp_table_suppression_enabled(true);

    assert_eq!(mem.read_u8(fp_addr), 0x00);
    assert_eq!(mem.read_u8(fp_addr + 1), b'M');
    assert_eq!(mem.read_u8(config_addr), 0x00);
    assert_eq!(mem.read_u8(config_addr + 1), b'C');

    let mut fp = [0u8; 4];
    mem.read_bytes(fp_addr, &mut fp);
    assert_eq!(fp, [0x00, b'M', b'P', b'_']);

    let mut config = [0u8; 4];
    mem.read_bytes(config_addr, &mut config);
    assert_eq!(config, [0x00, b'C', b'M', b'P']);
}

/// x87 load fast-path panel finding 1 (both lenses' top finding): while
/// the firmware MP-table filter is armed, an emitted `[r15 + page*8]`
/// read-probe hit must never serve bytes the helper/interpreter byte
/// service would rewrite. At HEAD the flat projection had no counterpart
/// to `tlb_direct_read_ptr`'s global refusal: arming neither flushed the
/// TLB nor withheld `host_read` on refill, so a warm low-RAM page kept a
/// live read view serving the raw `_MP_` byte where `read_u8` returns
/// 0x00 — a divergence the integer inline probes already carried and the
/// x87 fast path would have widened.
#[test]
fn firmware_mp_suppression_withholds_flat_read_views_for_low_memory() {
    // Probe mirror: exactly what emitted dyncore code executes on a read
    // probe hit — load the supervisor read-view slot, bias-add the raw
    // linear address, read the host byte.
    fn probe_read_u8(mem: &Memory, linear: u32) -> Option<u8> {
        let slot = unsafe { *mem.flat_views_ptr().add((linear >> 12) as usize) };
        if slot == 0 {
            return None;
        }
        let host = slot.wrapping_add(u64::from(linear)) as *const u8;
        Some(unsafe { *host })
    }
    // The subset invariant: a probe HIT must agree with the filtered
    // byte service; a miss (None) is always a correct decline.
    fn assert_probe_subset_of_helper(mem: &Memory, linear: u32) {
        if let Some(raw) = probe_read_u8(mem, linear) {
            assert_eq!(
                raw,
                mem.read_u8(linear as usize),
                "flat read view serves bytes the armed MP filter rewrites at {linear:#x}"
            );
        }
    }

    let mut mem = Memory::new();
    // A valid MP floating pointer + config table in ordinary low RAM
    // (EBDA-like), identity mapped: exactly the page class whose
    // host_read the projection publishes.
    let fp_addr = 0x9_F4D0usize;
    let config_addr = fp_addr + 0x10;
    let fp = valid_mp_floating_pointer_bytes(config_addr);
    let config = valid_mp_config_table_bytes();
    write_bytes_for_test(&mut mem, fp_addr, &fp);
    write_bytes_for_test(&mut mem, config_addr, &config);
    let page = (fp_addr >> 12) as u32;

    // Warm the page BEFORE arming: arming must null the published view.
    mem.tlb_insert(page, page, true, true, true);
    assert_eq!(
        probe_read_u8(&mem, fp_addr as u32),
        Some(b'_'),
        "seed sanity: unfiltered low RAM publishes a live read view"
    );
    mem.set_firmware_mp_table_suppression_enabled(true);
    assert_eq!(
        mem.read_u8(fp_addr),
        0x00,
        "the byte service filters the armed signature byte"
    );
    assert_probe_subset_of_helper(&mem, fp_addr as u32);

    // Refill WHILE armed: the fill itself must withhold host_read below
    // LOW_MEMORY_LIMIT (arming-time flush alone is not enough — post-arm
    // inserts republish).
    mem.tlb_insert(page, page, true, true, true);
    assert_probe_subset_of_helper(&mem, fp_addr as u32);
    assert!(
        mem.tlb_direct_read_ptr(fp_addr as u32, 4, false).is_none(),
        "the helper fast leg keeps refusing globally while armed"
    );

    // The withhold is scoped: pages at or above 1 MiB cannot hold a
    // filterable signature, so they keep their fast read views.
    mem.tlb_insert(0x150, 0x150, true, true, true);
    assert!(
        probe_read_u8(&mem, 0x15_0000).is_some(),
        "the low-memory withhold must not tax pages above 1 MiB"
    );

    // Disarm flushes too, so no stale null (or stale filtered state)
    // outlives the policy flip; the next fill republishes the raw view.
    mem.set_firmware_mp_table_suppression_enabled(false);
    assert_eq!(
        probe_read_u8(&mem, fp_addr as u32),
        None,
        "disarm must flush rather than leave any armed-era view live"
    );
    mem.tlb_insert(page, page, true, true, true);
    assert_eq!(probe_read_u8(&mem, fp_addr as u32), Some(b'_'));
    assert_eq!(mem.read_u8(fp_addr), b'_');
}

#[test]
fn firmware_mp_table_suppression_ignores_invalid_checksums() {
    let mut mem = Memory::new();
    mem.allow_rom_writes = true;
    let fp_addr = 0xF54D0;
    let config_addr = 0xF54E0;
    let mut fp = valid_mp_floating_pointer_bytes(config_addr);
    let mut config = valid_mp_config_table_bytes();
    fp[10] = fp[10].wrapping_add(1);
    config[7] = config[7].wrapping_add(1);
    write_bytes_for_test(&mut mem, fp_addr, &fp);
    write_bytes_for_test(&mut mem, config_addr, &config);
    mem.set_firmware_mp_table_suppression_enabled(true);

    assert_eq!(mem.read_u8(fp_addr), b'_');
    assert_eq!(mem.read_u8(config_addr), b'P');

    let mut fp_read = [0u8; 4];
    mem.read_bytes(fp_addr, &mut fp_read);
    assert_eq!(&fp_read, b"_MP_");

    let mut config_read = [0u8; 4];
    mem.read_bytes(config_addr, &mut config_read);
    assert_eq!(&config_read, b"PCMP");
}

#[test]
fn firmware_mp_table_suppression_ignores_unaligned_matches() {
    let mut mem = Memory::new();
    mem.allow_rom_writes = true;
    write_bytes_for_test(&mut mem, 0xF54D1, b"_MP_");
    mem.set_firmware_mp_table_suppression_enabled(true);

    assert_eq!(mem.read_u8(0xF54D1), b'_');
}

#[test]
fn firmware_mp_table_scrub_is_disabled_by_default() {
    let mut mem = Memory::new();
    mem.allow_rom_writes = true;
    let fp_addr = 0xF54D0;
    let config_addr = 0xF54E0;
    let fp = valid_mp_floating_pointer_bytes(config_addr);
    let config = valid_mp_config_table_bytes();
    write_bytes_for_test(&mut mem, fp_addr, &fp);
    write_bytes_for_test(&mut mem, config_addr, &config);

    let scrubbed = mem.scrub_firmware_mp_table_signatures();

    assert_eq!(scrubbed, FirmwareMpTableScrubResult::default());
    assert_eq!(&mem.ram[fp_addr..fp_addr + 4], b"_MP_");
    assert_eq!(&mem.ram[config_addr..config_addr + 4], b"PCMP");
}

#[test]
fn firmware_mp_table_scrub_zeroes_valid_raw_signatures() {
    let mut mem = Memory::new();
    mem.allow_rom_writes = true;
    let fp_addr = 0xF54D0;
    let config_addr = 0xF54E0;
    let fp = valid_mp_floating_pointer_bytes(config_addr);
    let config = valid_mp_config_table_bytes();
    write_bytes_for_test(&mut mem, fp_addr, &fp);
    write_bytes_for_test(&mut mem, config_addr, &config);
    mem.set_firmware_mp_table_suppression_enabled(true);

    let scrubbed = mem.scrub_firmware_mp_table_signatures();

    assert_eq!(
        scrubbed,
        FirmwareMpTableScrubResult {
            floating_pointer_signatures: 1,
            configuration_table_signatures: 1,
        }
    );
    assert_eq!(&mem.ram[fp_addr..fp_addr + 4], &[0x00, b'M', b'P', b'_']);
    assert_eq!(
        &mem.ram[config_addr..config_addr + 4],
        &[0x00, b'C', b'M', b'P']
    );
    assert_eq!(mem.read_u8(fp_addr), 0x00);
    assert_eq!(mem.read_u8(config_addr), 0x00);

    let mut fp_read = [0u8; 4];
    mem.read_bytes(fp_addr, &mut fp_read);
    assert_eq!(fp_read, [0x00, b'M', b'P', b'_']);

    let mut config_read = [0u8; 4];
    mem.read_bytes(config_addr, &mut config_read);
    assert_eq!(config_read, [0x00, b'C', b'M', b'P']);

    assert_ne!(mem.read_u16(fp_addr), u16::from_le_bytes(*b"_M"));
    assert_ne!(mem.read_u16(config_addr), u16::from_le_bytes(*b"PC"));
    assert_ne!(mem.read_u32(fp_addr), u32::from_le_bytes(*b"_MP_"));
    assert_ne!(mem.read_u32(config_addr), u32::from_le_bytes(*b"PCMP"));
}

#[test]
fn firmware_mp_table_scrub_ignores_invalid_checksums() {
    let mut mem = Memory::new();
    mem.allow_rom_writes = true;
    let fp_addr = 0xF54D0;
    let config_addr = 0xF54E0;
    let mut fp = valid_mp_floating_pointer_bytes(config_addr);
    let mut config = valid_mp_config_table_bytes();
    fp[10] = fp[10].wrapping_add(1);
    config[7] = config[7].wrapping_add(1);
    write_bytes_for_test(&mut mem, fp_addr, &fp);
    write_bytes_for_test(&mut mem, config_addr, &config);
    mem.set_firmware_mp_table_suppression_enabled(true);

    let scrubbed = mem.scrub_firmware_mp_table_signatures();

    assert_eq!(scrubbed, FirmwareMpTableScrubResult::default());
    assert_eq!(&mem.ram[fp_addr..fp_addr + 4], b"_MP_");
    assert_eq!(&mem.ram[config_addr..config_addr + 4], b"PCMP");
}

// ========== Declared regions + POST tracer (Package 10) ==========

#[test]
fn declared_regions_hle_default() {
    // Fresh memory: no ROM pages marked, alias disabled. We expect the
    // conventional / video / UMB / system-bios-hle / hma split.
    let mem = Memory::new();
    let regions = mem.declared_regions();
    let labels: Vec<&str> = regions.iter().map(|r| r.label).collect();
    assert_eq!(
        labels,
        vec!["conventional", "video", "umb", "system-bios-hle", "hma"]
    );
    let by_label = |l: &str| {
        regions
            .iter()
            .find(|r| r.label == l)
            .map(|r| r.range.clone())
            .unwrap_or_else(|| panic!("label {l} missing"))
    };
    assert_eq!(by_label("conventional"), 0x00000..=0x9FFFF);
    assert_eq!(by_label("video"), 0xA0000..=0xBFFFF);
    assert_eq!(by_label("umb"), 0xC0000..=0xEFFFF);
    assert_eq!(by_label("system-bios-hle"), 0xF0000..=0xFFFFF);
    assert_eq!(by_label("hma"), 0x100000..=0x10FFFF);
}

#[test]
fn declared_regions_real_boot() {
    // Real-boot wiring: mark the full ROM range and enable the high
    // memory alias. The ROM split collapses to "bios-rom" and a
    // "bios-alias" region is appended for the chipset window.
    let mut mem = Memory::new();
    mem.mark_rom_range(0xC0000..=0xFFFFF);
    mem.set_high_mem_alias_enabled(true);
    let regions = mem.declared_regions();
    let labels: Vec<&str> = regions.iter().map(|r| r.label).collect();
    assert_eq!(
        labels,
        vec!["conventional", "video", "bios-rom", "hma", "bios-alias"]
    );
    let bios_rom = regions.iter().find(|r| r.label == "bios-rom").unwrap();
    assert_eq!(bios_rom.range, 0xC0000..=0xFFFFF);
    let bios_alias = regions.iter().find(|r| r.label == "bios-alias").unwrap();
    assert_eq!(bios_alias.range, HIGH_MEM_ALIAS_BASE..=HIGH_MEM_ALIAS_END);
}

#[test]
fn mem_post_trace_out_of_region_read_logs_once() {
    // Out-of-region read at 0x2000_0000 (well above declared regions)
    // should insert one (page, dir=0) entry. A second read on the same
    // page must not grow the set; a read on a different page must.
    let mut mem = Memory::new();
    mem.set_post_trace_enabled(true);
    assert_eq!(mem.post_trace_seen_len(), 0);

    // Out of range so the MMIO miss path fires.
    let addr_a = MAX_RAM_SIZE + 0x1000; // 256 MiB + 4 KiB
    let _ = mem.read_u8(addr_a);
    assert_eq!(mem.post_trace_seen_len(), 1);

    // Same page, second read: no new entry.
    let _ = mem.read_u8(addr_a + 0x10);
    assert_eq!(mem.post_trace_seen_len(), 1);

    // Different page: +1.
    let _ = mem.read_u8(addr_a + 0x1000);
    assert_eq!(mem.post_trace_seen_len(), 2);

    // Disabling clears the dedup set.
    mem.set_post_trace_enabled(false);
    assert_eq!(mem.post_trace_seen_len(), 0);
}

#[test]
fn mem_post_trace_out_of_region_write_logs_once() {
    // Writes above MAX_RAM_SIZE with no MMIO match are silently
    // dropped by prepare_write_addr. With the tracer enabled they
    // log one line per page. Reads and writes get separate entries.
    let mut mem = Memory::new();
    mem.set_post_trace_enabled(true);

    let addr = MAX_RAM_SIZE + 0x2000;
    mem.write_u8(addr, 0x42);
    assert_eq!(mem.post_trace_seen_len(), 1);
    mem.write_u8(addr + 4, 0x43);
    assert_eq!(mem.post_trace_seen_len(), 1, "same page dedupes");

    // A read on the same page is a different direction → +1.
    let _ = mem.read_u8(addr);
    assert_eq!(mem.post_trace_seen_len(), 2);
}

#[test]
fn mem_post_trace_in_region_does_not_log() {
    // Reads inside declared regions must not log. Covers the normal
    // RAM, the BIOS image window (with ROM marked), and the HMA.
    let mut mem = Memory::new();
    mem.mark_rom_range(0xC0000..=0xFFFFF);
    mem.set_high_mem_alias_enabled(true);
    mem.set_post_trace_enabled(true);
    let _ = mem.read_u8(0x500); // conventional
    let _ = mem.read_u8(0xA0000); // video
    let _ = mem.read_u8(0xF0000); // bios-rom
    let _ = mem.read_u8(0x10_0000); // hma
    let _ = mem.read_u8(0xFFFF_FFF0); // bios-alias (via fold)
    assert_eq!(mem.post_trace_seen_len(), 0);
}

#[test]
fn test_read_bytes_matches_read_u8_over_the_vga_plane_window() {
    for (mode, setup) in [
        ("planar 12h", 0x12u8),
        ("chain4 13h", 0x13u8),
        ("mode-x 13h", 0x13u8),
    ] {
        let mut mem = Memory::new();
        mem.set_video_mode(setup);
        mem.vga.chain4 = mode == "chain4 13h";
        mem.vga.is_mode_x = mode == "mode-x 13h";
        mem.update_vga_write_handler();

        // Distinct data in every plane, and a different pattern in the RAM
        // underneath, so reading the wrong backing store cannot agree by
        // accident.
        for plane in 0..4 {
            for offset in 0..0x40 {
                mem.planes_flat[plane * PLANE_SIZE + offset] =
                    (plane as u8) << 6 | (offset as u8 ^ 0x2A);
            }
        }
        for offset in 0..0x100 {
            mem.ram[VIDEO_BASE + offset] = 0x5A;
        }

        for (addr, len) in [
            (VIDEO_BASE, 0x40),
            (VIDEO_BASE + 0x10, 0x10),
            (VIDEO_BASE - 0x8, 0x20),
        ] {
            let per_byte: Vec<u8> = (addr..addr + len).map(|a| mem.read_u8(a)).collect();
            let mut bulk = vec![0u8; len];
            mem.read_bytes(addr, &mut bulk);
            assert_eq!(bulk, per_byte, "{mode}: read_bytes({addr:#X}, {len:#X})");
        }
    }
}

#[test]
fn test_read_bytes_over_the_vga_window_in_text_mode_reads_ram() {
    let mut mem = Memory::new();
    mem.set_video_mode(0x03);
    for offset in 0..0x40 {
        mem.ram[TEXT_VIDEO_BASE_COLOR + offset] = (offset as u8) ^ 0x3C;
        mem.planes_flat[offset] = 0xE7;
    }
    assert!(
        !mem.vga_plane_window_overlaps(TEXT_VIDEO_BASE_COLOR, 0x40),
        "plain text mode must keep the bulk copy"
    );
    mem.set_video_mode(0x12);
    assert!(
        mem.vga_plane_window_overlaps(TEXT_VIDEO_BASE_COLOR, 0x40),
        "a planar mode must defer the same span"
    );
    mem.set_video_mode(0x03);

    let mut bulk = [0u8; 0x40];
    mem.read_bytes(TEXT_VIDEO_BASE_COLOR, &mut bulk);
    let per_byte: Vec<u8> = (TEXT_VIDEO_BASE_COLOR..TEXT_VIDEO_BASE_COLOR + 0x40)
        .map(|a| mem.read_u8(a))
        .collect();
    assert_eq!(bulk.to_vec(), per_byte);
    assert_eq!(bulk[0], 0x3C, "text-mode VGA memory is the RAM behind it");
}

#[test]
fn fill_ram_basic() {
    let mut mem = Memory::new();
    // Sentinel byte just outside the range — must be untouched.
    mem.write_u8(0x0FFF, 0xAB);
    // Pre-set a byte just past the end — must be untouched.
    mem.write_u8(0x11000, 0xBA);

    mem.fill_ram(0x1000, 0x11000, 0xCF);

    // Spot-check a handful of bytes inside the range.
    for addr in [0x1000, 0x1234, 0x5000, 0xABCD, 0x10FFF] {
        assert_eq!(mem.read_u8(addr), 0xCF, "addr {addr:#X}");
    }
    // Bytes outside the range stay as they were.
    assert_eq!(mem.read_u8(0x0FFF), 0xAB);
    assert_eq!(mem.read_u8(0x11000), 0xBA);
}

#[test]
fn fill_ram_clamped() {
    let mut mem = Memory::new();
    let ram_len = mem.ram.len();
    let usable = mem.usable_len();
    assert!(
        usable < ram_len,
        "fixture: the backing buffer carries fetch padding"
    );
    // Request an end that extends past the backing buffer — must not panic.
    mem.fill_ram(usable.saturating_sub(64), ram_len + 0x1000, 0xCF);
    // The last guest byte was filled; the fetch padding past it was not
    // (MDD-BUG-CRU-01485).
    assert_eq!(mem.ram[usable - 1], 0xCF);
    assert!(
        mem.ram[usable..].iter().all(|&b| b == 0),
        "fill_ram reached into the fetch padding"
    );
}

#[test]
fn fill_ram_empty() {
    let mut mem = Memory::new();
    mem.write_u8(0x2000, 0x42);
    // start == end: no-op.
    mem.fill_ram(0x2000, 0x2000, 0xCF);
    assert_eq!(mem.read_u8(0x2000), 0x42);
    // start > end: no-op.
    mem.fill_ram(0x3000, 0x2000, 0xCF);
    assert_eq!(mem.read_u8(0x2000), 0x42);
}

#[test]
fn fill_ram_skips_rom() {
    let mut mem = Memory::new();
    // Mark a single 4-KiB page in the middle of our fill range as ROM.
    // ROM_PAGE_SIZE is 4096 = 0x1000.
    let rom_page_addr = 0x10000usize;
    let rom_page_index = rom_page_addr >> ROM_PAGE_SHIFT;
    mem.mark_rom_page(rom_page_index);

    // Sentinel inside the ROM page: write via direct ram[] so the ROM
    // gate doesn't intercept us — fill_ram must NOT overwrite this.
    mem.ram[rom_page_addr + 0x100] = 0x77;

    mem.fill_ram(0x0F000, 0x12000, 0xCF);

    // Page before the ROM page: filled.
    assert_eq!(mem.read_u8(0x0F000), 0xCF);
    assert_eq!(mem.read_u8(0x0FFFF), 0xCF);
    // ROM page: untouched.
    assert_eq!(mem.ram[rom_page_addr], 0x00);
    assert_eq!(mem.ram[rom_page_addr + 0x100], 0x77);
    assert_eq!(mem.ram[rom_page_addr + 0xFFF], 0x00);
    // Page after the ROM page: filled.
    assert_eq!(mem.read_u8(0x11000), 0xCF);
    assert_eq!(mem.read_u8(0x11FFF), 0xCF);
}

#[test]
fn fill_ram_invalidates_decoded_page() {
    let mut mem = Memory::new();
    mem.mark_instr_exec_page(0x3000);
    mem.core.take_instr_cache_dirty();

    mem.fill_ram(0x3000, 0x3010, 0xCF);

    assert!(mem.core.is_instr_cache_dirty());
}

#[test]
fn fill_ram_page_walk_invalidates_only_filled_pages() {
    let mut mem = Memory::new();
    let rom_page_addr = 0x10000usize;
    mem.mark_rom_page(rom_page_addr >> ROM_PAGE_SHIFT);

    // A decoded ROM page is skipped by the fill, so it stays valid.
    mem.mark_instr_exec_page(rom_page_addr);
    mem.core.take_instr_cache_dirty();
    mem.fill_ram(0x0F000, 0x12000, 0xCF);
    assert!(!mem.core.is_instr_cache_dirty());

    // A decoded RAM page past the ROM page is filled, so it is not.
    mem.mark_instr_exec_page(0x11000);
    mem.fill_ram(0x0F000, 0x12000, 0xCF);
    assert!(mem.core.is_instr_cache_dirty());
}
