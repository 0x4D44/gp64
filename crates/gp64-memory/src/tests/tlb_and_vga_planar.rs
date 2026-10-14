use super::*;

/// MDD-BUG-CRUCIBLE-00566: a physical write to an EXECUTED page must dirty
/// the interpreter's instruction cache.
///
/// `write_u32_physical` is the path page walks use to set PDE/PTE Accessed
/// and Dirty bits. It told dyncore via `smc_before_write` but never marked
/// the interpreter's cache, so a guest whose page-table page is also
/// executable could have a hardware A/D update change bytes the interpreter
/// kept serving from a stale decode.
#[test]
fn physical_write_to_an_executed_page_dirties_the_instruction_cache() {
    let mut mem = Memory::new();
    let addr = 0x3000usize;
    mem.mark_instr_exec_page(addr);
    assert!(
        !mem.is_instr_cache_dirty(),
        "marking a page as executed must not itself dirty the cache, or the              assertion below would pass for the wrong reason"
    );

    mem.write_u32_physical(addr, 0xDEAD_C0DE);

    assert!(
        mem.is_instr_cache_dirty(),
        "a physical write over executed bytes must invalidate the decode cache"
    );
}

/// The control: the same write to a page that was never executed must NOT
/// dirty the cache. Without this, marking every physical write dirty would
/// pass the test above while throwing away the whole point of the
/// executed-page filter.
#[test]
fn physical_write_to_a_non_executed_page_leaves_the_cache_clean() {
    let mut mem = Memory::new();
    mem.write_u32_physical(0x4000, 0xDEAD_C0DE);
    assert!(
        !mem.is_instr_cache_dirty(),
        "only writes over EXECUTED pages may dirty the interpreter cache"
    );
}

/// MDD-BUG-FLUX-00145: the dynrec-drc raw direct-RAM store leg must dirty
/// the interpreter's decode cache, the same as an ordinary write.
///
/// That leg writes `ram[addr]` directly once every byte carries a
/// `direct_host_ram_range` proof, which skips `write_u8`'s
/// `mark_instr_cache_dirty_range`. Since the proof excludes video, LFB, EMS
/// and ROM, those stores land in ordinary guest RAM — where guest code lives
/// — and the interpreter still runs under `--engine dynrec-drc` for every
/// 16-bit, real-mode, V86 and gap fallback, so it could serve a stale decode
/// of code the drc had just patched.
#[test]
fn a_drc_direct_ram_write_over_executed_bytes_dirties_the_instruction_cache() {
    let mut mem = Memory::new();
    let addr = 0x3000usize;
    mem.mark_instr_exec_page(addr);
    assert!(
        !mem.is_instr_cache_dirty(),
        "marking a page as executed must not itself dirty the cache, or the \
         assertion below would pass for the wrong reason"
    );

    mem.drc_note_direct_ram_write(addr, 4);

    assert!(
        mem.is_instr_cache_dirty(),
        "a drc-native raw store over executed bytes must invalidate the \
         interpreter's decode cache"
    );
}

/// The control, matching the pair above it: the executed-page filter must
/// still do its job, or the fix would dirty the cache on every drc store and
/// throw away the filter that makes it cheap.
#[test]
fn a_drc_direct_ram_write_to_a_non_executed_page_leaves_the_cache_clean() {
    let mut mem = Memory::new();
    mem.drc_note_direct_ram_write(0x4000, 4);
    assert!(
        !mem.is_instr_cache_dirty(),
        "only writes over EXECUTED pages may dirty the interpreter cache"
    );
}

/// MDD-BUG-CRUCIBLE-00417: disarming O3 must leave the TLB coherent.
///
// ========== Host Pointer TLB Tests ==========

#[test]
fn test_tlb_host_read_ptr_for_ram_page() {
    let mut mem = Memory::new();
    // Physical page 0x10 = address 0x10000, well within 1MB+HMA RAM.
    mem.tlb_insert(0x42, 0x10, true, true, false);

    let ptr = mem.tlb_entries_ptr();
    let index = SoftwareTlb::index(0x42);
    let entry = unsafe { &*ptr.add(index) };

    // host_read should point into RAM at physical offset 0x10000
    assert!(!entry.host_read.is_null());
    let expected = unsafe { mem.ram.host_base().cast_const().add(0x10000) };
    assert_eq!(entry.host_read, expected);
}

#[test]
fn test_tlb_host_write_ptr_writable_dirty() {
    let mut mem = Memory::new();
    // writable=true, dirty=true -> host_write should be non-null
    mem.tlb_insert(0x42, 0x10, true, true, true);

    let ptr = mem.tlb_entries_ptr();
    let index = SoftwareTlb::index(0x42);
    let entry = unsafe { &*ptr.add(index) };

    assert!(!entry.host_write.is_null());
    let expected = mem.ram.host_base();
    let expected = unsafe { expected.add(0x10000) };
    assert_eq!(entry.host_write, expected);
}

#[test]
fn test_tlb_host_write_null_when_not_writable() {
    let mut mem = Memory::new();
    // writable=false -> host_write should be null (forces slow path)
    mem.tlb_insert(0x42, 0x10, false, true, true);

    let ptr = mem.tlb_entries_ptr();
    let index = SoftwareTlb::index(0x42);
    let entry = unsafe { &*ptr.add(index) };

    assert!(!entry.host_read.is_null()); // reads still work
    assert!(entry.host_write.is_null()); // writes take slow path
}

#[test]
fn test_tlb_host_write_null_when_not_dirty() {
    let mut mem = Memory::new();
    // dirty=false -> host_write should be null (forces slow path for dirty-bit handling)
    mem.tlb_insert(0x42, 0x10, true, true, false);

    let ptr = mem.tlb_entries_ptr();
    let index = SoftwareTlb::index(0x42);
    let entry = unsafe { &*ptr.add(index) };

    assert!(!entry.host_read.is_null());
    assert!(entry.host_write.is_null());
}

#[test]
fn test_tlb_vga_pages_have_null_host_ptrs() {
    let mut mem = Memory::new();
    // Physical page 0xA0 = address 0xA0000 (start of VGA range)
    mem.tlb_insert(0x42, 0xA0, true, true, true);

    let ptr = mem.tlb_entries_ptr();
    let index = SoftwareTlb::index(0x42);
    let entry = unsafe { &*ptr.add(index) };

    // VGA pages must have null host pointers to force slow path
    assert!(entry.host_read.is_null());
    assert!(entry.host_write.is_null());
    // But lookup should still work (physical_page, flags retained)
    let (phys, writable, user, dirty) = mem.tlb_lookup(0x42).unwrap();
    assert_eq!(phys, 0xA0);
    assert!(writable);
    assert!(user);
    assert!(dirty);
}

#[test]
fn test_tlb_vga_end_boundary_null_ptrs() {
    let mut mem = Memory::new();
    // Physical page 0xBF = address 0xBF000 (last page in VGA range)
    mem.tlb_insert(0x42, 0xBF, true, true, true);
    let ptr = mem.tlb_entries_ptr();
    let index = SoftwareTlb::index(0x42);
    let entry = unsafe { &*ptr.add(index) };
    assert!(entry.host_read.is_null());
    assert!(entry.host_write.is_null());
}

#[test]
fn tlb_direct_read_ptr_admits_a_warm_ram_page_and_lands_on_the_right_byte() {
    let mut mem = Memory::new();
    mem.ram[0x2_1234] = 0x5A;
    mem.tlb_insert(0x21, 0x21, true, true, true);

    let ptr = mem
        .tlb_direct_read_ptr(0x2_1234, 4, false)
        .expect("plain RAM inside a warm page is directly readable");
    assert_eq!(unsafe { *ptr }, 0x5A);
    assert_eq!(ptr, unsafe {
        mem.ram.host_base().cast_const().add(0x2_1234)
    });
}

#[test]
fn tlb_direct_read_ptr_refuses_everything_the_slow_path_must_own() {
    let mut mem = Memory::new();
    mem.tlb_insert(0x21, 0x21, true, true, true);
    assert!(mem.tlb_direct_read_ptr(0x2_1000, 4, false).is_some());

    // Zero width, and any span leaving the page, need the full transaction.
    assert!(mem.tlb_direct_read_ptr(0x2_1000, 0, false).is_none());
    assert!(mem.tlb_direct_read_ptr(0x2_1FFC, 4, false).is_some());
    assert!(mem.tlb_direct_read_ptr(0x2_1FFD, 4, false).is_none());
    assert!(mem.tlb_direct_read_ptr(0x2_1FFF, 2, false).is_none());
    assert!(mem.tlb_direct_read_ptr(0x2_1FFF, 1, false).is_some());

    // A page with no live entry.
    assert!(mem.tlb_direct_read_ptr(0x2_2000, 1, false).is_none());

    // Device and firmware pages never publish a read pointer.
    mem.tlb_insert(0xA0, 0xA0, true, true, true);
    assert!(mem.tlb_direct_read_ptr(0xA_0000, 1, false).is_none());

    // A supervisor-only entry is invisible at CPL 3.
    mem.tlb_insert(0x30, 0x30, true, false, true);
    assert!(mem.tlb_direct_read_ptr(0x3_0000, 1, false).is_some());
    assert!(mem.tlb_direct_read_ptr(0x3_0000, 1, true).is_none());
}

#[test]
fn tlb_direct_write_ptr_requires_dirty_ram_permission_and_no_code_watch() {
    let mut mem = Memory::new();
    mem.tlb_insert(0x21, 0x21, true, true, true);
    let ptr = mem
        .tlb_direct_write_ptr(0x2_1234, 4, true)
        .expect("warm writable dirty user RAM is directly writable");
    assert_eq!(ptr, unsafe { mem.ram.host_base().add(0x2_1234) });

    assert!(mem.tlb_direct_write_ptr(0x2_1FFD, 4, false).is_none());
    mem.set_page_watched(0x21, true);
    assert!(
        mem.tlb_direct_write_ptr(0x2_1234, 4, false).is_none(),
        "a code-page watch must force the synchronous checked-store path"
    );
    mem.set_page_watched(0x21, false);
    assert!(mem.tlb_direct_write_ptr(0x2_1234, 4, false).is_some());

    mem.tlb_insert(0x30, 0x30, true, false, true);
    assert!(mem.tlb_direct_write_ptr(0x3_0000, 1, false).is_some());
    assert!(mem.tlb_direct_write_ptr(0x3_0000, 1, true).is_none());

    mem.tlb_insert(0x40, 0x40, true, true, false);
    assert!(
        mem.tlb_direct_write_ptr(0x4_0000, 1, false).is_none(),
        "a first write must visit the dirty-bit transaction"
    );
    mem.tlb_insert(0x50, 0x50, false, true, true);
    assert!(mem.tlb_direct_write_ptr(0x5_0000, 1, false).is_none());
}

#[test]
fn tlb_direct_read_ptr_stands_down_while_the_mp_table_filter_is_armed() {
    // The filter rewrites paragraph-aligned low-memory bytes chosen by
    // CONTENT, on pages that are otherwise ordinary RAM, so no per-page
    // admission can see it.
    let mut mem = Memory::new();
    mem.tlb_insert(0x21, 0x21, true, true, true);
    assert!(mem.tlb_direct_read_ptr(0x2_1000, 4, false).is_some());

    mem.set_firmware_mp_table_suppression_enabled(true);
    assert!(mem.tlb_direct_read_ptr(0x2_1000, 4, false).is_none());

    // Arming and disarming now FLUSH the TLB (the arm/disarm policy flip
    // must null published read views, see
    // `firmware_mp_suppression_withholds_flat_read_views_for_low_memory`),
    // so the disarmed fast path resumes only after the page is re-walked.
    mem.set_firmware_mp_table_suppression_enabled(false);
    assert!(mem.tlb_direct_read_ptr(0x2_1000, 4, false).is_none());
    mem.tlb_insert(0x21, 0x21, true, true, true);
    assert!(mem.tlb_direct_read_ptr(0x2_1000, 4, false).is_some());
}

#[test]
fn tlb_direct_read_ptr_refuses_a_page_holding_a_registered_mmio_region() {
    let mut mem = Memory::new();
    let base = 0x2_1000usize;
    // A sub-page MMIO span still disqualifies the WHOLE page, because
    // `tlb_insert` admits at 4-KiB granularity.
    mem.add_mmio_region(base + 0x100, 0x10);
    mem.tlb_insert(0x21, 0x21, true, true, true);
    assert!(mem.tlb_direct_read_ptr(base as u32, 1, false).is_none());
}

#[test]
fn test_tlb_page_just_past_vga_has_host_ptrs() {
    let mut mem = Memory::new();
    // Physical page 0xC0 = address 0xC0000 (just past VGA range)
    mem.tlb_insert(0x42, 0xC0, true, true, true);
    let ptr = mem.tlb_entries_ptr();
    let index = SoftwareTlb::index(0x42);
    let entry = unsafe { &*ptr.add(index) };
    assert!(!entry.host_read.is_null());
    assert!(!entry.host_write.is_null());
}

/// BIOS F-segment (0xF0000..0x100000): writes are silently dropped when
/// ROM writes are disallowed (the F7 rule in prepare_write_addr), so TLB
/// fill must never publish host_write for those pages — otherwise the
/// interpreter's TLB write-hit fast paths bypass the drop (EXP-B005
/// adversarial-panel finding). Reads stay fast: host_read is published.
#[test]
fn test_tlb_bios_fseg_pages_have_null_host_write_by_default() {
    let mut mem = Memory::new();
    assert!(!mem.allow_rom_writes());
    for page in [0xF0u32, 0xF8, 0xFF] {
        mem.tlb_insert(0x42, page, true, true, true);
        let ptr = mem.tlb_entries_ptr();
        let index = SoftwareTlb::index(0x42);
        let entry = unsafe { &*ptr.add(index) };
        assert!(
            !entry.host_read.is_null(),
            "F-seg page {page:#X}: reads stay on the fast path"
        );
        assert!(
            entry.host_write.is_null(),
            "F-seg page {page:#X}: host_write must be null (ROM drop semantics)"
        );
    }
    // Boundaries: 0xEF (below) and 0x100 (above, HMA) keep write pointers.
    for page in [0xEFu32, 0x100] {
        mem.tlb_insert(0x42, page, true, true, true);
        let ptr = mem.tlb_entries_ptr();
        let index = SoftwareTlb::index(0x42);
        let entry = unsafe { &*ptr.add(index) };
        assert!(
            !entry.host_write.is_null(),
            "page {page:#X} outside the F-seg window keeps host_write"
        );
    }
}

#[test]
fn test_tlb_bios_fseg_write_ptr_published_when_rom_writes_allowed() {
    let mut mem = Memory::new();
    mem.set_allow_rom_writes(true);
    mem.tlb_insert(0x42, 0xF0, true, true, true);
    let ptr = mem.tlb_entries_ptr();
    let index = SoftwareTlb::index(0x42);
    let entry = unsafe { &*ptr.add(index) };
    assert!(!entry.host_read.is_null());
    assert!(
        !entry.host_write.is_null(),
        "real-boot POST (allow_rom_writes) keeps the F-seg write fast path"
    );
}

#[test]
fn test_set_allow_rom_writes_flushes_tlb() {
    let mut mem = Memory::new();
    mem.tlb_insert(0x42, 0x10, true, true, true);
    assert!(mem.tlb_lookup(0x42).is_some());
    mem.set_allow_rom_writes(true);
    assert!(
        mem.tlb_lookup(0x42).is_none(),
        "ROM-write policy flip must flush the TLB (policy is baked in at fill)"
    );
    // No-op set (same value) must NOT flush.
    mem.tlb_insert(0x42, 0x10, true, true, true);
    mem.set_allow_rom_writes(true);
    assert!(mem.tlb_lookup(0x42).is_some());
}

#[test]
fn test_claim_hle_ems_frame_flushes_tlb() {
    let mut mem = Memory::new();
    // Entry over the EMS frame filled BEFORE the claim.
    mem.tlb_insert(0x42, (EMS_FRAME_BASE >> 12) as u32, true, true, true);
    assert!(mem.tlb_lookup(0x42).is_some());
    mem.claim_hle_ems_frame(EMS_FRAME_BASE, EMS_FRAME_SIZE)
        .unwrap();
    assert!(
        mem.tlb_lookup(0x42).is_none(),
        "EMS frame claim must flush the TLB so pre-claim direct pointers die"
    );
}

#[test]
fn test_tlb_host_ptr_dereferences_correctly() {
    let mut mem = Memory::new();
    // Write a known pattern at physical address 0x10042
    mem.ram[0x10042] = 0xAB;

    // Insert TLB entry for page 0x10 (physical page 0x10)
    mem.tlb_insert(0x42, 0x10, true, true, true);
    let ptr = mem.tlb_entries_ptr();
    let index = SoftwareTlb::index(0x42);
    let entry = unsafe { &*ptr.add(index) };

    // Read through host_read pointer + offset 0x42
    let val = unsafe { *entry.host_read.add(0x42) };
    assert_eq!(val, 0xAB);

    // Write through host_write pointer + offset 0x42
    unsafe { *entry.host_write.add(0x42) = 0xCD };
    assert_eq!(mem.ram[0x10042], 0xCD);
}

#[test]
fn test_tlb_sentinel_tag_default() {
    let entry = TlbEntry::default();
    // Default should have sentinel tag (not 0, which is a valid page number)
    assert_eq!(entry.linear_page, 0xFFFF_FFFF);
    assert!(entry.host_read.is_null());
    assert!(entry.host_write.is_null());
    assert_eq!(entry.flags, 0);
}

#[test]
fn test_tlb_flush_clears_host_ptrs() {
    let mut mem = Memory::new();
    mem.tlb_insert(0x42, 0x10, true, true, true);
    // Verify entry exists
    assert!(mem.tlb_lookup(0x42).is_some());

    mem.invalidate_tlb();

    // After flush, entry should be gone
    assert!(mem.tlb_lookup(0x42).is_none());

    // And host pointers should be null
    let ptr = mem.tlb_entries_ptr();
    let index = SoftwareTlb::index(0x42);
    let entry = unsafe { &*ptr.add(index) };
    assert!(entry.host_read.is_null());
    assert!(entry.host_write.is_null());
}

#[test]
fn test_tlb_invalidate_page_clears_host_ptrs() {
    let mut mem = Memory::new();
    mem.tlb_insert(0x42, 0x10, true, true, true);
    assert!(mem.tlb_lookup(0x42).is_some());

    mem.invalidate_tlb_page(0x42 << 12); // takes linear address, not page number

    assert!(mem.tlb_lookup(0x42).is_none());
    let ptr = mem.tlb_entries_ptr();
    let index = SoftwareTlb::index(0x42);
    let entry = unsafe { &*ptr.add(index) };
    assert!(entry.host_read.is_null());
    assert!(entry.host_write.is_null());
}

#[test]
fn test_tlb_entry_size_is_32_bytes() {
    assert_eq!(std::mem::size_of::<TlbEntry>(), 32);
}

#[test]
fn test_tlb_flags_packing() {
    let mut mem = Memory::new();

    // All flags set
    mem.tlb_insert(0x42, 0x10, true, true, true);
    let ptr = mem.tlb_entries_ptr();
    let index = SoftwareTlb::index(0x42);
    let entry = unsafe { &*ptr.add(index) };
    assert_eq!(
        entry.flags,
        TLB_FLAG_WRITABLE | TLB_FLAG_USER | TLB_FLAG_DIRTY
    );

    // No flags
    mem.tlb_insert(0x42, 0x10, false, false, false);
    let entry = unsafe { &*ptr.add(index) };
    assert_eq!(entry.flags, 0);

    // Just writable
    mem.tlb_insert(0x42, 0x10, true, false, false);
    let entry = unsafe { &*ptr.add(index) };
    assert_eq!(entry.flags, TLB_FLAG_WRITABLE);
}

#[test]
fn test_prepare_write_addr_flushes_tlb_on_resize() {
    let mut mem = Memory::new();
    assert_eq!(mem.ram_size(), DEFAULT_RAM_SIZE);

    // Insert a TLB entry
    mem.tlb_insert(0x42, 0x10, true, true, true);
    assert!(mem.tlb_lookup(0x42).is_some());

    // Writes within the default usable range don't trigger growth.
    mem.write_u8(DEFAULT_RAM_SIZE - 1, 0x42);

    // TLB remains intact because no reallocation occurred
    assert!(
        mem.tlb_lookup(0x42).is_some(),
        "TLB should survive when no reallocation occurs"
    );
}

// ========== Plane Access Tests ==========

#[test]
fn test_plane_access() {
    let mut mem = Memory::new();

    // Write to plane 0
    mem.plane_mut(0)[0] = 0xAA;
    mem.plane_mut(0)[100] = 0xBB;

    // Read back
    assert_eq!(mem.plane(0)[0], 0xAA);
    assert_eq!(mem.plane(0)[100], 0xBB);

    // Other planes should be unaffected
    assert_eq!(mem.plane(1)[0], 0x00);
    assert_eq!(mem.plane(2)[0], 0x00);
    assert_eq!(mem.plane(3)[0], 0x00);
}

#[test]
fn test_plane_mut_all_planes() {
    let mut mem = Memory::new();

    // Write different values to each plane
    mem.plane_mut(0)[50] = 0x11;
    mem.plane_mut(1)[50] = 0x22;
    mem.plane_mut(2)[50] = 0x33;
    mem.plane_mut(3)[50] = 0x44;

    assert_eq!(mem.plane(0)[50], 0x11);
    assert_eq!(mem.plane(1)[50], 0x22);
    assert_eq!(mem.plane(2)[50], 0x33);
    assert_eq!(mem.plane(3)[50], 0x44);
}

// ========== Planar Mode Detection Tests ==========

#[test]
fn test_is_planar_mode() {
    let mut mem = Memory::new();

    // Non-planar modes
    mem.vga.video_mode = 0x03; // Text mode
    assert!(!mem.is_planar_mode());

    mem.vga.video_mode = 0x13; // Mode 13h (linear, chain-4)
    mem.vga.chain4 = true;
    mem.vga.is_mode_x = false;
    assert!(!mem.is_planar_mode());

    // Mode 13h with chain-4 disabled AND Mode X confirmed (CR9/CR17
    // reprogrammed): planar dispatch.
    mem.vga.chain4 = false;
    mem.vga.is_mode_x = true;
    assert!(mem.is_planar_mode());

    // Bare `(0x13, !chain4)` without is_mode_x is the VGA firmware
    // Mode-13h-init transient — must NOT be classified as planar.
    mem.vga.is_mode_x = false;
    assert!(!mem.is_planar_mode());

    // Planar modes
    mem.vga.video_mode = 0x0D; // 320x200 16-color
    assert!(mem.is_planar_mode());

    mem.vga.video_mode = 0x0E; // 640x200 16-color
    assert!(mem.is_planar_mode());

    mem.vga.video_mode = 0x10; // 640x350 16-color
    assert!(mem.is_planar_mode());

    mem.vga.video_mode = 0x12; // 640x480 16-color
    assert!(mem.is_planar_mode());
}

// ========== VGA Write Handler Dispatch Tests ==========

#[test]
fn test_vga_write_handler_per_mode() {
    let mut mem = Memory::new();

    // Default: text mode 0x03 → Linear
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Linear);

    // Mode 13h with chain4=true → Chain4
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.update_vga_write_handler();
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Chain4);

    // Mode 13h with chain4=false AND is_mode_x=true (real Mode X)
    // → Planar. Without is_mode_x, this would be the VGA firmware
    // Mode-13h-init transient and dispatch as Linear.
    mem.vga.chain4 = false;
    mem.vga.is_mode_x = true;
    mem.update_vga_write_handler();
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Planar);

    // Mode 12h (640x480 16-color) → Planar
    mem.vga.is_mode_x = false;
    mem.vga.video_mode = 0x12;
    mem.update_vga_write_handler();
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Planar);

    // Mode 0Dh (320x200 16-color) → Planar
    mem.vga.video_mode = 0x0D;
    mem.update_vga_write_handler();
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Planar);

    // Mode 0Eh (640x200 16-color) → Planar
    mem.vga.video_mode = 0x0E;
    mem.update_vga_write_handler();
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Planar);

    // Mode 10h (640x350 16-color) → Planar
    mem.vga.video_mode = 0x10;
    mem.update_vga_write_handler();
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Planar);

    // Mode 0Fh (640x350 mono) → Planar
    mem.vga.video_mode = 0x0F;
    mem.update_vga_write_handler();
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Planar);

    // Mode 11h (640x480 mono) → Planar
    mem.vga.video_mode = 0x11;
    mem.update_vga_write_handler();
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Planar);
}

#[test]
fn test_vga_write_handler_mode_transitions() {
    let mut mem = Memory::new();

    // Start in text mode
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Linear);

    // Switch to mode 13h (chain4) via set_video_mode
    mem.vga.chain4 = true;
    mem.set_video_mode(0x13);
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Chain4);

    // Disable chain4 → Mode X (planar). Set is_mode_x to mark this as
    // a confirmed Mode X (CR9 / CR17 reprogrammed); without it, the
    // dispatch correctly falls back to Linear since the bare
    // `(0x13, !chain4)` state is the VGA firmware Mode 13h init transient.
    mem.vga.chain4 = false;
    mem.vga.is_mode_x = true;
    mem.update_vga_write_handler();
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Planar);

    // Back to text mode
    mem.set_video_mode(0x03);
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Linear);
}

#[test]
fn test_vga_write_handler_apply_pipeline() {
    let mut mem = Memory::new();
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Linear);

    // Apply pipeline for mode 13h chain4
    let state = VgaPipelineState {
        video_mode: 0x13,
        chain4: true,
        is_mode_x: false,
        plane_mask: 0x0F,
        read_plane: 0,
        memory_map_select: 1,
        seq_odd_even_disabled: true,
        gc_odd_even_read: false,
        gc_graphics_mode: true,
        write_mode: 0,
        bit_mask: 0xFF,
        set_reset: 0,
        enable_set_reset: 0,
        data_rotate: 0,
        read_mode: 0,
        color_compare: 0,
        color_dont_care: 0x0F,
    };
    mem.apply_vga_pipeline(&state);
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Chain4);

    // Apply pipeline for mode 12h (planar)
    let state = VgaPipelineState {
        video_mode: 0x12,
        chain4: false,
        is_mode_x: false,
        plane_mask: 0x0F,
        read_plane: 0,
        memory_map_select: 1,
        seq_odd_even_disabled: true,
        gc_odd_even_read: false,
        gc_graphics_mode: true,
        write_mode: 0,
        bit_mask: 0xFF,
        set_reset: 0,
        enable_set_reset: 0,
        data_rotate: 0,
        read_mode: 0,
        color_compare: 0,
        color_dont_care: 0x0F,
    };
    mem.apply_vga_pipeline(&state);
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Planar);
}

#[test]
fn test_vga_write_handler_consistent_with_is_planar() {
    let mut mem = Memory::new();

    // Exhaustively check all common modes: handler should agree with is_planar_mode.
    // Mode-X classification requires both chain-4 disabled AND is_mode_x set
    // (CR9 / CR17 confirmation) — bare `(0x13, !chain4)` is the VGA firmware
    // Mode 13h init transient and must NOT dispatch Planar.
    let modes: &[(u8, bool, bool)] = &[
        (0x03, true, false),  // text
        (0x03, false, false), // text
        (0x0D, false, false), // 16-color
        (0x0E, false, false), // 16-color
        (0x0F, false, false), // mono
        (0x10, false, false), // 16-color
        (0x11, false, false), // mono
        (0x12, false, false), // 16-color
        (0x13, true, false),  // mode 13h chain4
        (0x13, false, true),  // mode X (confirmed)
        (0x13, false, false), // 256-colour transition before chain4/Mode X
    ];

    for &(mode, chain4, is_mode_x) in modes {
        mem.vga.video_mode = mode;
        mem.vga.chain4 = chain4;
        mem.vga.is_mode_x = is_mode_x;
        mem.update_vga_write_handler();

        let is_planar = mem.is_planar_mode();
        let handler = mem.vga_write_handler();

        if is_planar {
            assert_eq!(
                handler,
                VgaWriteHandler::Planar,
                "mode={:#04x} chain4={} is_mode_x={}: is_planar=true but handler={:?}",
                mode,
                chain4,
                is_mode_x,
                handler
            );
        } else if mode == 0x13 && chain4 {
            assert_eq!(
                handler,
                VgaWriteHandler::Chain4,
                "mode=0x13 chain4=true: expected Chain4 but got {:?}",
                handler
            );
        } else {
            assert_eq!(
                handler,
                VgaWriteHandler::Linear,
                "mode={:#04x} chain4={} is_mode_x={}: expected Linear but got {:?}",
                mode,
                chain4,
                is_mode_x,
                handler
            );
        }
    }
}

#[test]
fn test_mode13_register_transition_preserves_linear_vram() {
    let mut mem = Memory::new();

    // 1. Initial: text mode 0x03, chain4 off.
    mem.vga.video_mode = 0x03;
    mem.vga.chain4 = false;
    mem.vga.is_mode_x = false;
    mem.update_vga_write_handler();
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Linear);

    // 2. Set AR[0x10] bit 6 (256-colour) BEFORE SR[4]:
    //    video_mode flips to 0x13 while chain4 is still off.
    //    is_mode_x is FALSE because CR9 still has the text-mode value
    //    (max_scan_line != 0). This is the transient.
    let transient = VgaPipelineState {
        video_mode: 0x13,
        chain4: false,
        seq_odd_even_disabled: false,
        gc_odd_even_read: true,
        is_mode_x: false, // Mode X NOT confirmed
        plane_mask: 0x0F,
        ..Default::default()
    };
    mem.apply_vga_pipeline(&transient);
    assert_eq!(
        mem.vga_write_handler(),
        VgaWriteHandler::Linear,
        "transient (0x13, !chain4, !is_mode_x) must NOT dispatch Planar — \
         the register state has not enabled planar access"
    );

    // 3. Some VRAM write happens in the transient window (e.g.,
    //    a linear VRAM clear). Linear dispatch
    //    sends it straight to ram.
    const SENTINEL: u8 = 0x5A;
    mem.write_u8(VIDEO_BASE, SENTINEL);
    mem.write_u8(VIDEO_BASE + 1234, SENTINEL);
    mem.write_u8(VIDEO_BASE + 9999, SENTINEL);
    assert_eq!(mem.read_u8(VIDEO_BASE), SENTINEL);
    assert_eq!(mem.read_u8(VIDEO_BASE + 1234), SENTINEL);
    assert_eq!(mem.read_u8(VIDEO_BASE + 9999), SENTINEL);

    // 4. VGA firmware sets SR[4] bit 3 (chain-4 enabled). Memory should
    //    now dispatch Chain4. The crucial bit: the transition must NOT
    //    sync planes_flat → ram (planes are stale; ram has the data).
    let mode13 = VgaPipelineState {
        video_mode: 0x13,
        chain4: true,
        is_mode_x: false,
        plane_mask: 0x0F,
        ..Default::default()
    };
    mem.apply_vga_pipeline(&mode13);
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Chain4);

    // 5. The Linear-period writes must survive intact. Before the fix,
    //    `sync_planes_to_chain4` would have stomped these with stale
    //    planes_flat (zeros from boot).
    assert_eq!(
        mem.read_u8(VIDEO_BASE),
        SENTINEL,
        "transient-window write at offset 0 must survive Linear→Chain4 transition"
    );
    assert_eq!(
        mem.read_u8(VIDEO_BASE + 1234),
        SENTINEL,
        "transient-window write at offset 1234 must survive Linear→Chain4 transition"
    );
    assert_eq!(
        mem.read_u8(VIDEO_BASE + 9999),
        SENTINEL,
        "transient-window write at offset 9999 must survive Linear→Chain4 transition"
    );

    // 6. A real Mode-X transition (CR9 max_scan_line = 0 → is_mode_x
    //    set) must still dispatch Planar — we only blocked the bare
    //    `(0x13, !chain4)` case, not real Mode X.
    let mode_x = VgaPipelineState {
        video_mode: 0x13,
        chain4: false,
        is_mode_x: true, // CR9 max-scan = 0 confirmed
        plane_mask: 0x0F,
        ..Default::default()
    };
    mem.apply_vga_pipeline(&mode_x);
    assert_eq!(
        mem.vga_write_handler(),
        VgaWriteHandler::Planar,
        "real Mode X (is_mode_x = true) must dispatch Planar"
    );
}

// ========== Planar Read Tests (Latch Loading) ==========

#[test]
fn test_planar_read_loads_latches() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12; // Planar mode

    // Set up plane data at offset 0
    mem.plane_mut(0)[0] = 0x11;
    mem.plane_mut(1)[0] = 0x22;
    mem.plane_mut(2)[0] = 0x33;
    mem.plane_mut(3)[0] = 0x44;

    // Read from video memory should load latches
    mem.vga.read_plane = 0;
    let val = mem.read_u8(VIDEO_BASE);
    assert_eq!(val, 0x11); // Should return plane 0

    // Check latches were loaded
    let latches = mem.latches.get();
    assert_eq!(latches[0], 0x11);
    assert_eq!(latches[1], 0x22);
    assert_eq!(latches[2], 0x33);
    assert_eq!(latches[3], 0x44);
}

/// MDD-BUG-ANVIL-00144: `peek_video_u8` returns the same byte a Read
/// Mode 0 CPU read would, but must NOT reload the latches — it is the
/// read for host-side surface syncs, which run asynchronously to the
/// guest instruction stream.
#[test]
fn test_peek_video_u8_planar_matches_read_and_leaves_latches_alone() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12; // planar

    mem.plane_mut(0)[0] = 0x11;
    mem.plane_mut(1)[0] = 0x22;
    mem.plane_mut(2)[0] = 0x33;
    mem.plane_mut(3)[0] = 0x44;
    mem.vga.read_plane = 2;

    let sentinel = [0xDE, 0xAD, 0xBE, 0xEF];
    mem.latches.set(sentinel);

    let peeked = mem.peek_video_u8(VIDEO_BASE);
    assert_eq!(peeked, 0x33, "peek returns the Read Mode 0 byte");
    assert_eq!(
        mem.latches.get(),
        sentinel,
        "peek_video_u8 must not reload the latches"
    );

    // The CPU read path returns the same byte but DOES load latches —
    // the differential that makes the peek necessary.
    assert_eq!(mem.read_u8(VIDEO_BASE), peeked);
    assert_eq!(mem.latches.get(), [0x11, 0x22, 0x33, 0x44]);
}

/// MDD-BUG-ANVIL-00144: chain-4 arm of the same invariant.
#[test]
fn test_peek_video_u8_chain4_matches_read_and_leaves_latches_alone() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;

    // Chain-4 mapping maps byte offset 5 to plane 1, cell 5&~3=4.
    mem.plane_mut(1)[4] = 0x77;

    let sentinel = [0x01, 0x02, 0x03, 0x04];
    mem.latches.set(sentinel);

    let peeked = mem.peek_video_u8(VIDEO_BASE + 5);
    assert_eq!(peeked, 0x77);
    assert_eq!(
        mem.latches.get(),
        sentinel,
        "peek_video_u8 must not reload the latches"
    );
    assert_eq!(mem.read_u8(VIDEO_BASE + 5), peeked);
    assert_ne!(mem.latches.get(), sentinel, "read_u8 loads latches");
}

#[test]
fn test_planar_read_select_plane() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;

    mem.plane_mut(0)[0] = 0xAA;
    mem.plane_mut(1)[0] = 0xBB;
    mem.plane_mut(2)[0] = 0xCC;
    mem.plane_mut(3)[0] = 0xDD;

    // Read from different planes
    mem.vga.read_plane = 0;
    assert_eq!(mem.read_u8(VIDEO_BASE), 0xAA);

    mem.vga.read_plane = 1;
    assert_eq!(mem.read_u8(VIDEO_BASE), 0xBB);

    mem.vga.read_plane = 2;
    assert_eq!(mem.read_u8(VIDEO_BASE), 0xCC);

    mem.vga.read_plane = 3;
    assert_eq!(mem.read_u8(VIDEO_BASE), 0xDD);
}

#[test]
fn test_planar_read_mode_1_color_compare_match() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.read_mode = 1;
    mem.vga.color_compare = 0x05; // planes 0 and 2 should be 1
    mem.vga.color_dont_care = 0x0F;

    mem.plane_mut(0)[0] = 0xFF;
    mem.plane_mut(1)[0] = 0x00;
    mem.plane_mut(2)[0] = 0xFF;
    mem.plane_mut(3)[0] = 0x00;

    let result = mem.read_u8(VIDEO_BASE);
    assert_eq!(result, 0xFF);
}

#[test]
fn test_planar_read_mode_1_color_compare_mismatch_and_dont_care() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.read_mode = 1;
    mem.vga.color_compare = 0x01; // only plane 0 = 1
    mem.vga.color_dont_care = 0x01; // only plane 0 matters

    mem.plane_mut(0)[0] = 0x00; // mismatch
    mem.plane_mut(1)[0] = 0xFF; // should be ignored
    mem.plane_mut(2)[0] = 0xFF; // should be ignored
    mem.plane_mut(3)[0] = 0xFF; // should be ignored

    let result = mem.read_u8(VIDEO_BASE);
    assert_eq!(result, 0x00);

    mem.plane_mut(0)[0] = 0xFF; // match plane 0
    let result = mem.read_u8(VIDEO_BASE);
    assert_eq!(result, 0xFF);
}

// ========== Write Mode Tests ==========

#[test]
fn test_write_mode_0_basic() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12; // Planar mode
    mem.vga.write_mode = 0;
    mem.vga.plane_mask = 0x0F; // All planes
    mem.vga.bit_mask = 0xFF; // All bits
    mem.vga.enable_set_reset = 0; // No set/reset
    mem.vga.data_rotate = 0; // No rotation, no logical op
    mem.update_vga_write_handler();

    // Write a value
    mem.write_u8(VIDEO_BASE, 0xAB);

    // All planes should have the value
    assert_eq!(mem.plane(0)[0], 0xAB);
    assert_eq!(mem.plane(1)[0], 0xAB);
    assert_eq!(mem.plane(2)[0], 0xAB);
    assert_eq!(mem.plane(3)[0], 0xAB);
}

#[test]
fn test_write_mode_0_plane_mask() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.write_mode = 0;
    mem.vga.plane_mask = 0x05; // Planes 0 and 2 only
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.vga.data_rotate = 0;
    mem.update_vga_write_handler();

    mem.write_u8(VIDEO_BASE, 0xAB);

    assert_eq!(mem.plane(0)[0], 0xAB);
    assert_eq!(mem.plane(1)[0], 0x00); // Not written
    assert_eq!(mem.plane(2)[0], 0xAB);
    assert_eq!(mem.plane(3)[0], 0x00); // Not written
}

#[test]
fn test_write_mode_0_set_reset() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.write_mode = 0;
    mem.vga.plane_mask = 0x0F;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0x0A; // Planes 1 and 3 use set/reset
    mem.vga.set_reset = 0x02; // Plane 1 = 0xFF, plane 3 = 0x00
    mem.vga.data_rotate = 0;
    mem.update_vga_write_handler();

    mem.write_u8(VIDEO_BASE, 0x55);

    assert_eq!(mem.plane(0)[0], 0x55); // CPU data
    assert_eq!(mem.plane(1)[0], 0xFF); // Set/reset (bit 1 = 1)
    assert_eq!(mem.plane(2)[0], 0x55); // CPU data
    assert_eq!(mem.plane(3)[0], 0x00); // Set/reset (bit 3 = 0)
}

#[test]
fn test_write_mode_0_rotation() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.write_mode = 0;
    mem.vga.plane_mask = 0x0F;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.vga.data_rotate = 0x02; // Rotate right by 2
    mem.update_vga_write_handler();

    // 0x81 (10000001) rotated right 2 = 0x60 (01100000)
    mem.write_u8(VIDEO_BASE, 0x81);

    assert_eq!(mem.plane(0)[0], 0x60);
}

#[test]
fn test_write_mode_0_logical_and() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.write_mode = 0;
    mem.vga.plane_mask = 0x0F;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.vga.data_rotate = 0x08; // Logical AND (bits 3-4 = 01)
    mem.update_vga_write_handler();

    // Pre-fill latches by reading
    mem.plane_mut(0)[0] = 0xFF;
    mem.read_u8(VIDEO_BASE); // Load latches

    // Write with AND
    mem.write_u8(VIDEO_BASE, 0xF0);

    // 0xF0 AND 0xFF = 0xF0
    assert_eq!(mem.plane(0)[0], 0xF0);
}

#[test]
fn test_write_mode_0_logical_or() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.write_mode = 0;
    mem.vga.plane_mask = 0x0F;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.vga.data_rotate = 0x10; // Logical OR (bits 3-4 = 10)
    mem.update_vga_write_handler();

    mem.plane_mut(0)[0] = 0x0F;
    mem.read_u8(VIDEO_BASE);

    mem.write_u8(VIDEO_BASE, 0xF0);

    // 0xF0 OR 0x0F = 0xFF
    assert_eq!(mem.plane(0)[0], 0xFF);
}

#[test]
fn test_write_mode_0_logical_xor() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.write_mode = 0;
    mem.vga.plane_mask = 0x0F;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.vga.data_rotate = 0x18; // Logical XOR (bits 3-4 = 11)
    mem.update_vga_write_handler();

    mem.plane_mut(0)[0] = 0xFF;
    mem.read_u8(VIDEO_BASE);

    mem.write_u8(VIDEO_BASE, 0xAA);

    // 0xAA XOR 0xFF = 0x55
    assert_eq!(mem.plane(0)[0], 0x55);
}

#[test]
fn test_write_mode_0_bit_mask() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.write_mode = 0;
    mem.vga.plane_mask = 0x0F;
    mem.vga.bit_mask = 0xF0; // Only high nibble
    mem.vga.enable_set_reset = 0;
    mem.vga.data_rotate = 0;
    mem.update_vga_write_handler();

    // Set latch value
    mem.plane_mut(0)[0] = 0x0F;
    mem.read_u8(VIDEO_BASE);

    mem.write_u8(VIDEO_BASE, 0xFF);

    // High nibble from CPU (0xF0), low nibble from latch (0x0F)
    assert_eq!(mem.plane(0)[0], 0xFF);
}

#[test]
fn test_write_mode_1_latch_copy() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.write_mode = 1;
    mem.vga.plane_mask = 0x0F;
    mem.update_vga_write_handler();

    // Set up source data
    mem.plane_mut(0)[0] = 0x11;
    mem.plane_mut(1)[0] = 0x22;
    mem.plane_mut(2)[0] = 0x33;
    mem.plane_mut(3)[0] = 0x44;

    // Load latches
    mem.read_u8(VIDEO_BASE);

    // Write to different location - CPU data is ignored in mode 1
    mem.write_u8(VIDEO_BASE + 1, 0xFF);

    // Latches should be copied
    assert_eq!(mem.plane(0)[1], 0x11);
    assert_eq!(mem.plane(1)[1], 0x22);
    assert_eq!(mem.plane(2)[1], 0x33);
    assert_eq!(mem.plane(3)[1], 0x44);
}

#[test]
fn test_write_mode_1_partial_plane_mask() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.write_mode = 1;
    mem.vga.plane_mask = 0x05; // planes 0 and 2 only
    mem.update_vga_write_handler();

    mem.plane_mut(0)[0] = 0x11;
    mem.plane_mut(1)[0] = 0x22;
    mem.plane_mut(2)[0] = 0x33;
    mem.plane_mut(3)[0] = 0x44;
    mem.read_u8(VIDEO_BASE);

    mem.write_u8(VIDEO_BASE + 2, 0xFF);
    assert_eq!(mem.plane(0)[2], 0x11);
    assert_eq!(mem.plane(1)[2], 0x00);
    assert_eq!(mem.plane(2)[2], 0x33);
    assert_eq!(mem.plane(3)[2], 0x00);
}

#[test]
fn test_write_mode_2_color_expansion() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.write_mode = 2;
    mem.vga.plane_mask = 0x0F;
    mem.vga.bit_mask = 0xFF;
    mem.vga.data_rotate = 0; // No logical op
    mem.update_vga_write_handler();

    // Color value 0x05 = 0101 binary
    // Plane 0: bit 0 = 1 -> 0xFF
    // Plane 1: bit 1 = 0 -> 0x00
    // Plane 2: bit 2 = 1 -> 0xFF
    // Plane 3: bit 3 = 0 -> 0x00
    mem.write_u8(VIDEO_BASE, 0x05);

    assert_eq!(mem.plane(0)[0], 0xFF);
    assert_eq!(mem.plane(1)[0], 0x00);
    assert_eq!(mem.plane(2)[0], 0xFF);
    assert_eq!(mem.plane(3)[0], 0x00);
}

#[test]
fn test_write_mode_2_with_bit_mask() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.write_mode = 2;
    mem.vga.plane_mask = 0x0F;
    mem.vga.bit_mask = 0xF0; // Only high nibble
    mem.vga.data_rotate = 0;
    mem.update_vga_write_handler();

    // Pre-fill ALL planes to load latches correctly
    mem.plane_mut(0)[0] = 0x0F;
    mem.plane_mut(1)[0] = 0x0F;
    mem.plane_mut(2)[0] = 0x0F;
    mem.plane_mut(3)[0] = 0x0F;
    mem.read_u8(VIDEO_BASE); // Load latches from all planes

    // Color 0x01 (only plane 0)
    mem.write_u8(VIDEO_BASE, 0x01);

    // High nibble from expansion (0xF0), low from latch (0x0F)
    assert_eq!(mem.plane(0)[0], 0xFF);
    // Plane 1: high nibble 0x00, low from latch 0x0F = 0x0F
    assert_eq!(mem.plane(1)[0], 0x0F);
}

#[test]
fn test_write_mode_3_vga() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.write_mode = 3;
    mem.vga.plane_mask = 0x0F;
    mem.vga.bit_mask = 0xFF;
    mem.vga.set_reset = 0x05; // Color 5 (planes 0 and 2)
    mem.vga.data_rotate = 0;
    mem.update_vga_write_handler();

    // Pre-fill latches
    mem.plane_mut(0)[0] = 0x00;
    mem.plane_mut(1)[0] = 0x00;
    mem.plane_mut(2)[0] = 0x00;
    mem.plane_mut(3)[0] = 0x00;
    mem.read_u8(VIDEO_BASE);

    // CPU data acts as mask (ANDed with bit_mask)
    mem.write_u8(VIDEO_BASE, 0xF0);

    // Effective mask = 0xF0 & 0xFF = 0xF0
    // Plane 0: set_reset bit 0 = 1, so (0xFF & 0xF0) | (0x00 & 0x0F) = 0xF0
    assert_eq!(mem.plane(0)[0], 0xF0);
    // Plane 1: set_reset bit 1 = 0, so (0x00 & 0xF0) | (0x00 & 0x0F) = 0x00
    assert_eq!(mem.plane(1)[0], 0x00);
    // Plane 2: set_reset bit 2 = 1, so (0xFF & 0xF0) | (0x00 & 0x0F) = 0xF0
    assert_eq!(mem.plane(2)[0], 0xF0);
    // Plane 3: set_reset bit 3 = 0, so 0x00
    assert_eq!(mem.plane(3)[0], 0x00);
}

/// Write Mode 3 in Mode X (mode 0x13, chain4=false).
/// GFX[5]=0x0B combines write mode 3 and read mode 1.
/// This test verifies the full read-modify-write pattern with latch preservation.
#[test]
fn test_write_mode_3_mode_x_latch_preservation() {
    let mut mem = Memory::new();
    // Mode X: mode 0x13 with chain4 disabled and is_mode_x set
    // (CR9 max-scan = 0 reprogrammed, real Mode-X register state).
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = false;
    mem.vga.is_mode_x = true;
    mem.vga.memory_map_select = 1; // A0000-AFFFF (64K)

    // Register setup: write mode 3, read mode 1
    mem.vga.write_mode = 3;
    mem.vga.read_mode = 1;
    mem.vga.plane_mask = 0x0F; // All planes
    mem.vga.bit_mask = 0xFF;
    mem.vga.data_rotate = 0;
    mem.vga.set_reset = 0x03; // Color 3 (planes 0 and 1 set)
    mem.vga.color_compare = 0x00;
    mem.vga.color_dont_care = 0x0F;
    mem.update_vga_write_handler();

    // Pre-fill VRAM with a background pattern in all planes
    for i in 0..80 {
        mem.plane_mut(0)[i] = 0xAA;
        mem.plane_mut(1)[i] = 0x55;
        mem.plane_mut(2)[i] = 0xAA;
        mem.plane_mut(3)[i] = 0x55;
    }

    // Step 1: Read VRAM to load latches (read mode 1 returns color compare result,
    // but latches are still loaded with actual plane data)
    let _read_result = mem.read_u8(VIDEO_BASE); // loads latches from offset 0
    let latches = mem.get_latches();
    assert_eq!(latches[0], 0xAA, "Latch plane 0 should be 0xAA");
    assert_eq!(latches[1], 0x55, "Latch plane 1 should be 0x55");
    assert_eq!(latches[2], 0xAA, "Latch plane 2 should be 0xAA");
    assert_eq!(latches[3], 0x55, "Latch plane 3 should be 0x55");

    // Step 2: Write with write mode 3 — CPU data 0xF0, bit_mask 0xFF
    // effective_mask = 0xF0 & 0xFF = 0xF0
    // For bits where mask=1: use set/reset; for bits where mask=0: preserve latch
    mem.write_u8(VIDEO_BASE, 0xF0);

    // Plane 0: set_reset bit 0 = 1 → expanded 0xFF
    //   (0xFF & 0xF0) | (0xAA & 0x0F) = 0xF0 | 0x0A = 0xFA
    assert_eq!(
        mem.plane(0)[0],
        0xFA,
        "Plane 0 should blend set/reset(1) with latch"
    );
    // Plane 1: set_reset bit 1 = 1 → expanded 0xFF
    //   (0xFF & 0xF0) | (0x55 & 0x0F) = 0xF0 | 0x05 = 0xF5
    assert_eq!(
        mem.plane(1)[0],
        0xF5,
        "Plane 1 should blend set/reset(1) with latch"
    );
    // Plane 2: set_reset bit 2 = 0 → expanded 0x00
    //   (0x00 & 0xF0) | (0xAA & 0x0F) = 0x00 | 0x0A = 0x0A
    assert_eq!(
        mem.plane(2)[0],
        0x0A,
        "Plane 2 should blend set/reset(0) with latch"
    );
    // Plane 3: set_reset bit 3 = 0 → expanded 0x00
    //   (0x00 & 0xF0) | (0x55 & 0x0F) = 0x00 | 0x05 = 0x05
    assert_eq!(
        mem.plane(3)[0],
        0x05,
        "Plane 3 should blend set/reset(0) with latch"
    );

    // Step 3: Verify unchanged adjacent byte still has original pattern
    assert_eq!(mem.plane(0)[1], 0xAA);
    assert_eq!(mem.plane(1)[1], 0x55);
}

/// Write Mode 1 (latch copy) in Mode X for VRAM copies.
#[test]
fn test_write_mode_1_mode_x_latch_copy() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = false;
    mem.vga.is_mode_x = true; // confirmed Mode X
    mem.vga.memory_map_select = 1;
    mem.vga.write_mode = 1;
    mem.vga.plane_mask = 0x0F;
    mem.update_vga_write_handler();

    // Pre-fill source location with different values per plane
    mem.plane_mut(0)[10] = 0x11;
    mem.plane_mut(1)[10] = 0x22;
    mem.plane_mut(2)[10] = 0x33;
    mem.plane_mut(3)[10] = 0x44;

    // Read source to load latches
    let _ = mem.read_u8(VIDEO_BASE + 10);
    let latches = mem.get_latches();
    assert_eq!(latches, [0x11, 0x22, 0x33, 0x44]);

    // Write mode 1: copy latches to destination (CPU data ignored)
    mem.write_u8(VIDEO_BASE + 20, 0x00); // CPU data doesn't matter
    assert_eq!(mem.plane(0)[20], 0x11);
    assert_eq!(mem.plane(1)[20], 0x22);
    assert_eq!(mem.plane(2)[20], 0x33);
    assert_eq!(mem.plane(3)[20], 0x44);
}

/// Write Mode 3 with partial bit_mask for sub-byte pixel writes.
#[test]
fn test_write_mode_3_partial_bitmask() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = false;
    mem.vga.is_mode_x = true; // confirmed Mode X
    mem.vga.memory_map_select = 1;
    mem.vga.write_mode = 3;
    mem.vga.plane_mask = 0x0F;
    mem.vga.bit_mask = 0x80; // Only the top bit from bit mask
    mem.vga.data_rotate = 0;
    mem.vga.set_reset = 0x0F; // All planes set
    mem.update_vga_write_handler();

    // Pre-fill with zeros
    for p in 0..4 {
        mem.plane_mut(p)[0] = 0x00;
    }
    mem.read_u8(VIDEO_BASE); // load latches (all zeros)

    // Write 0xFF with bit_mask=0x80: effective_mask = 0xFF & 0x80 = 0x80
    mem.write_u8(VIDEO_BASE, 0xFF);
    // All planes: (0xFF & 0x80) | (0x00 & 0x7F) = 0x80
    for p in 0..4 {
        assert_eq!(
            mem.plane(p)[0],
            0x80,
            "Plane {p} should have only top bit set"
        );
    }
}

// ========== VGA Oracle Invariant Tests ==========
// These tests establish checkable invariants that the fuzzer can verify.

/// Invariant 1: Write Mode 0 Simple Roundtrip
/// With all planes enabled, bit_mask=0xFF, no set/reset, no rotation,
/// writing a value should make that value appear in all planes.
#[test]
fn test_vga_oracle_write_mode_0_roundtrip() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12; // Planar mode (640x480 16-color)
    mem.vga.write_mode = 0;
    mem.vga.plane_mask = 0x0F; // All planes
    mem.vga.bit_mask = 0xFF; // All bits
    mem.vga.enable_set_reset = 0; // No set/reset
    mem.vga.data_rotate = 0; // No rotation, no logical op
    mem.update_vga_write_handler();

    // Test multiple offsets and values
    for offset in [0, 100, 1000, 0xFFF0] {
        for value in [0x00, 0x55, 0xAA, 0xFF] {
            let addr = VIDEO_BASE + offset;
            mem.write_u8(addr, value);

            // All planes should have the written value
            for plane in 0..4 {
                assert_eq!(mem.plane(plane)[offset], value);
            }
        }
    }
}

/// Invariant 2: Plane Isolation
/// Writing with restricted plane_mask should not affect non-selected planes.
#[test]
fn test_vga_oracle_plane_isolation() {
    for selected_plane in 0..4u8 {
        let mut mem = Memory::new();
        mem.vga.video_mode = 0x12;
        mem.vga.write_mode = 0;
        mem.vga.plane_mask = 1 << selected_plane; // Only this plane
        mem.vga.bit_mask = 0xFF;
        mem.vga.enable_set_reset = 0;
        mem.vga.data_rotate = 0;
        mem.update_vga_write_handler();

        // Initialize all planes with 0x42
        for plane in 0..4 {
            mem.plane_mut(plane)[0] = 0x42;
        }

        // Write a different value
        mem.write_u8(VIDEO_BASE, 0xFF);

        // Only the selected plane should have changed
        for plane in 0..4 {
            let expected = if plane == selected_plane as usize {
                0xFF
            } else {
                0x42
            };
            assert_eq!(mem.plane(plane)[0], expected);
        }
    }
}

/// Invariant 3: Write Mode 1 Latch Copy
/// In write mode 1, writing copies the latch values (loaded by a previous read).
#[test]
fn test_vga_oracle_write_mode_1_latch_copy() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.plane_mask = 0x0F;
    mem.update_vga_write_handler();

    // Set up source data with distinct values per plane
    mem.plane_mut(0)[100] = 0x11;
    mem.plane_mut(1)[100] = 0x22;
    mem.plane_mut(2)[100] = 0x33;
    mem.plane_mut(3)[100] = 0x44;

    // Read to load latches
    mem.vga.read_mode = 0;
    mem.vga.read_plane = 0; // Doesn't affect latch loading
    let _ = mem.read_u8(VIDEO_BASE + 100);

    // Verify latches are loaded
    let latches = mem.get_latches();
    assert_eq!(latches, [0x11, 0x22, 0x33, 0x44]);

    // Switch to write mode 1 and write to a different location
    mem.vga.write_mode = 1;
    mem.write_u8(VIDEO_BASE + 200, 0xFF); // CPU data ignored in WM1

    // Destination should have latch values
    assert_eq!(mem.plane(0)[200], 0x11);
    assert_eq!(mem.plane(1)[200], 0x22);
    assert_eq!(mem.plane(2)[200], 0x33);
    assert_eq!(mem.plane(3)[200], 0x44);
}

/// Invariant 4: Read Mode 0 Plane Selection
/// Reading with different read_plane values returns data from the correct plane.
#[test]
fn test_vga_oracle_read_mode_0_plane_select() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.read_mode = 0;

    // Set up distinct data in each plane
    mem.plane_mut(0)[500] = 0xAA;
    mem.plane_mut(1)[500] = 0xBB;
    mem.plane_mut(2)[500] = 0xCC;
    mem.plane_mut(3)[500] = 0xDD;

    // Read from each plane
    let expected = [0xAA, 0xBB, 0xCC, 0xDD];
    for plane in 0..4 {
        mem.vga.read_plane = plane as u8;
        let value = mem.read_u8(VIDEO_BASE + 500);
        assert_eq!(value, expected[plane]);
    }
}

/// Invariant 5: Chain-4 Mode Roundtrip
/// In mode 13h with chain4 enabled, address bits select planes automatically.
#[test]
fn test_vga_oracle_chain4_roundtrip() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.plane_mask = 0x0F;
    mem.vga.write_mode = 0;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.vga.data_rotate = 0;
    mem.vga.read_mode = 0;
    mem.update_vga_write_handler();

    // Chain-4 mapping selects plane x&3 and
    // stores at plane cell x&~3.
    for linear_offset in [0, 1, 2, 3, 4, 5, 100, 256, 1000] {
        let value = ((linear_offset * 7 + 13) & 0xFF) as u8;
        let addr = VIDEO_BASE + linear_offset;

        mem.write_u8(addr, value);
        let read_back = mem.read_u8(addr);

        assert_eq!(read_back, value);

        // Verify plane storage
        let expected_plane = linear_offset & 3;
        let plane_offset = linear_offset & !3;
        assert_eq!(mem.plane(expected_plane)[plane_offset], value);
    }
}

/// Invariant 6: Bit Mask Preserves Latch Bits
/// With bit_mask limiting which bits are writable, unmasked bits come from latches.
#[test]
fn test_vga_oracle_bit_mask_latch_preservation() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.write_mode = 0;
    mem.vga.plane_mask = 0x0F;
    mem.vga.enable_set_reset = 0;
    mem.vga.data_rotate = 0;
    mem.update_vga_write_handler();

    // Set initial plane data
    mem.plane_mut(0)[0] = 0x0F; // Low nibble set

    // Load latches
    mem.read_u8(VIDEO_BASE);

    // Write with only high nibble enabled
    mem.vga.bit_mask = 0xF0;
    mem.write_u8(VIDEO_BASE, 0xFF); // Write all 1s

    // Result: high nibble from CPU (0xF0), low nibble from latch (0x0F)
    assert_eq!(mem.plane(0)[0], 0xFF); // 0xF0 | 0x0F
}

/// Invariant 7: Write Mode 2 Color Expansion
/// In WM2, low 4 bits of CPU data expand to full bytes per plane.
#[test]
fn test_vga_oracle_write_mode_2_expansion() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.write_mode = 2;
    mem.vga.plane_mask = 0x0F;
    mem.vga.bit_mask = 0xFF;
    mem.vga.data_rotate = 0;
    mem.update_vga_write_handler();

    // Write color 0x09 = 1001 binary
    // Plane 0: bit 0 = 1 -> 0xFF
    // Plane 1: bit 1 = 0 -> 0x00
    // Plane 2: bit 2 = 0 -> 0x00
    // Plane 3: bit 3 = 1 -> 0xFF
    mem.write_u8(VIDEO_BASE + 1000, 0x09);

    assert_eq!(mem.plane(0)[1000], 0xFF);
    assert_eq!(mem.plane(1)[1000], 0x00);
    assert_eq!(mem.plane(2)[1000], 0x00);
    assert_eq!(mem.plane(3)[1000], 0xFF);
}

/// Invariant 8: Read Mode 1 Color Compare
/// In read mode 1, bits are set where plane colors match color_compare.
#[test]
fn test_vga_oracle_read_mode_1_color_compare() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x12;
    mem.vga.read_mode = 1;
    mem.vga.color_compare = 0x05; // Compare against color 5 (0101)
    mem.vga.color_dont_care = 0x0F; // All planes matter

    // Set up planes to have color 5 (planes 0 and 2 set) at offset 0
    // Each bit position in plane data represents a pixel
    mem.plane_mut(0)[0] = 0xFF; // All pixels have bit 0 set
    mem.plane_mut(1)[0] = 0x00; // No pixels have bit 1 set
    mem.plane_mut(2)[0] = 0xFF; // All pixels have bit 2 set
    mem.plane_mut(3)[0] = 0x00; // No pixels have bit 3 set
                                // This means all 8 pixels at offset 0 have color 0x05

    let result = mem.read_u8(VIDEO_BASE);
    // All pixels should match color 5
    assert_eq!(result, 0xFF);

    // Now set up mixed colors
    mem.plane_mut(0)[1] = 0xF0; // Upper 4 pixels have bit 0 set
    mem.plane_mut(1)[1] = 0x00;
    mem.plane_mut(2)[1] = 0xF0; // Upper 4 pixels have bit 2 set
    mem.plane_mut(3)[1] = 0x00;
    // Upper 4 pixels are color 5, lower 4 pixels are color 0

    let result2 = mem.read_u8(VIDEO_BASE + 1);
    // Only upper 4 bits should be set (pixels 4-7 match color 5)
    assert_eq!(result2, 0xF0);
}
