use super::*;

#[test]
fn vga_plane_replacement_rejects_wrong_length_without_mutation() {
    let mut memory = Memory::new();
    memory.vga_planes_mut()[0] = 0xA5;
    let original_ptr = memory.vga_planes_mut().as_mut_ptr();

    let error = memory
        .replace_vga_planes(&vec![0; VGA_PLANES_SIZE - 1])
        .expect_err("short VGA plane backing must be rejected");

    assert_eq!(
        error,
        VgaPlanesSizeError {
            actual: VGA_PLANES_SIZE - 1
        }
    );
    assert_eq!(memory.vga_planes().len(), VGA_PLANES_SIZE);
    assert_eq!(memory.vga_planes()[0], 0xA5);

    memory
        .replace_vga_planes(&vec![0x5A; VGA_PLANES_SIZE])
        .expect("exact VGA plane backing should be accepted");
    assert_eq!(memory.vga_planes_mut().as_mut_ptr(), original_ptr);
    assert!(memory.vga_planes().iter().all(|&byte| byte == 0x5A));
}

fn assert_direct_epoch(mem: &Memory, expected: u64) {
    assert_eq!(mem.direct_memory_policy_epoch(), expected);
}

fn assert_dynrec_translation_epoch(mem: &Memory, expected: u64) {
    assert_eq!(mem.dynrec_translation_epoch(), expected);
}

fn assert_dynrec_a20_generation(mem: &Memory, expected: u64) {
    assert_eq!(mem.dynrec_a20_generation(), expected);
}

#[test]
fn dynrec_source_admission_accepts_only_notifier_backed_ram() {
    let mut mem = Memory::new();

    assert!(mem.dynrec_source_range_is_notifier_backed_ram(0x2000, 1));
    assert!(!mem.dynrec_source_range_is_notifier_backed_ram(VIDEO_BASE, 1));
    assert!(!mem.dynrec_source_range_is_notifier_backed_ram(VIDEO_BASE - 1, 2));
    assert!(!mem.dynrec_source_range_is_notifier_backed_ram(BIOS_BASE, 1));
    assert!(!mem.dynrec_source_range_is_notifier_backed_ram(BIOS_BASE - 1, 2));

    mem.mark_rom_range(0x5000..=0x5fff);
    assert!(!mem.dynrec_source_range_is_notifier_backed_ram(0x5000, 1));
    assert!(!mem.dynrec_source_range_is_notifier_backed_ram(0x4fff, 2));

    mem.init_lfb(16);
    mem.set_lfb_enabled(true);
    assert!(!mem.dynrec_source_range_is_notifier_backed_ram(LFB_BASE, 1));

    mem.add_mmio_region(0x7000, 16);
    assert!(!mem.dynrec_source_range_is_notifier_backed_ram(0x7000, 1));
    assert!(!mem.dynrec_source_range_is_notifier_backed_ram(MAX_RAM_SIZE, 1));

    mem.claim_hle_ems_frame(EMS_FRAME_BASE, EMS_FRAME_SIZE)
        .expect("claim EMS frame");
    assert!(!mem.dynrec_source_range_is_notifier_backed_ram(EMS_FRAME_BASE, 1));
    assert!(!mem.dynrec_source_range_is_notifier_backed_ram(EMS_FRAME_BASE - 1, 2));
}

#[test]
fn memory_policy_epoch_starts_at_zero_and_data_writes_do_not_increment() {
    let mut mem = Memory::new();
    assert_epoch(&mem, 0);
    assert_direct_epoch(&mem, 0);
    assert_dynrec_translation_epoch(&mem, 0);
    assert_dynrec_a20_generation(&mem, 0);

    mem.write_u8(0x1000, 0x12);
    mem.write_u16(0x1002, 0x3456);
    mem.write_u32(0x1004, 0x789A_BCDE);

    assert_epoch(&mem, 0);
    assert_direct_epoch(&mem, 0);
    assert_dynrec_translation_epoch(&mem, 0);
    assert_dynrec_a20_generation(&mem, 0);
    assert_eq!(mem.read_u8(0x1000), 0x12);
    assert_eq!(mem.read_u16(0x1002), 0x3456);
    assert_eq!(mem.read_u32(0x1004), 0x789A_BCDE);
}

#[test]
fn memory_policy_epoch_increments_on_a20_changes() {
    let mut mem = Memory::new();

    mem.set_a20(false);
    assert_epoch(&mem, 1);
    assert_direct_epoch(&mem, 1);
    assert_dynrec_translation_epoch(&mem, 1);
    assert_dynrec_a20_generation(&mem, 1);

    mem.set_a20(false);
    assert_epoch(&mem, 1);
    assert_direct_epoch(&mem, 1);
    assert_dynrec_translation_epoch(&mem, 1);
    assert_dynrec_a20_generation(&mem, 1);

    mem.set_a20(true);
    assert_epoch(&mem, 2);
    assert_direct_epoch(&mem, 2);
    assert_dynrec_translation_epoch(&mem, 2);
    assert_dynrec_a20_generation(&mem, 2);
}

#[test]
fn memory_policy_epoch_increments_on_rom_write_policy_setter() {
    let mut mem = Memory::new();

    mem.set_allow_rom_writes(true);
    assert!(mem.allow_rom_writes());
    assert_epoch(&mem, 1);

    mem.set_allow_rom_writes(true);
    assert_epoch(&mem, 1);

    mem.set_allow_rom_writes(false);
    assert!(!mem.allow_rom_writes());
    assert_epoch(&mem, 2);
}

#[test]
fn memory_policy_epoch_increments_on_rom_region_policy_changes() {
    let mut mem = Memory::new();

    mem.mark_rom_page(0xC0);
    assert_epoch(&mem, 1);

    mem.mark_rom_page(0xC0);
    assert_epoch(&mem, 1);

    mem.mark_rom_range(0xC0000..=0xC2FFF);
    assert_epoch(&mem, 2);

    mem.mark_rom_range(usize::MAX - 0x1000..=usize::MAX);
    assert_epoch(&mem, 2);
}

#[test]
fn memory_policy_epoch_increments_on_alias_policy_changes() {
    let mut mem = Memory::new();

    mem.set_high_mem_alias_enabled(true);
    assert_epoch(&mem, 1);

    mem.set_high_mem_alias_enabled(true);
    assert_epoch(&mem, 1);

    mem.set_high_mem_alias_enabled(false);
    assert_epoch(&mem, 2);

    mem.set_high_mem_alias_enabled(false);
    assert_epoch(&mem, 2);
}

#[test]
fn memory_policy_epoch_increments_on_physical_mapping_and_tlb_policy_changes() {
    let mut mem = Memory::new();

    mem.add_physical_mapping(0x2000, 0x3000, 0);
    assert_epoch(&mem, 0);
    assert_direct_epoch(&mem, 0);
    assert_dynrec_translation_epoch(&mem, 0);

    mem.add_physical_mapping(0x2000, 0x3000, 0x1000);
    assert_epoch(&mem, 1);
    assert_direct_epoch(&mem, 1);
    assert_dynrec_translation_epoch(&mem, 1);

    assert!(!mem.remove_physical_mapping(0x4000));
    assert_epoch(&mem, 1);
    assert_direct_epoch(&mem, 1);
    assert_dynrec_translation_epoch(&mem, 1);

    assert!(mem.remove_physical_mapping(0x2000));
    assert_epoch(&mem, 2);
    assert_direct_epoch(&mem, 2);
    assert_dynrec_translation_epoch(&mem, 2);

    mem.invalidate_tlb();
    assert_epoch(&mem, 3);
    assert_direct_epoch(&mem, 2);
    assert_dynrec_translation_epoch(&mem, 3);

    mem.invalidate_tlb_page(0x5000);
    assert_epoch(&mem, 4);
    assert_direct_epoch(&mem, 2);
    assert_dynrec_translation_epoch(&mem, 4);
}

#[test]
fn dynrec_translation_epoch_increments_on_physical_mapping_churn() {
    let mut mem = Memory::new();

    mem.add_physical_mapping(0x2000, 0x3000, 0x1000);
    assert_dynrec_translation_epoch(&mem, 1);

    mem.add_physical_mapping(0x2000, 0x4000, 0x1000);
    assert_dynrec_translation_epoch(&mem, 2);

    assert!(mem.remove_physical_mapping(0x2000));
    assert_dynrec_translation_epoch(&mem, 3);

    assert!(!mem.remove_physical_mapping(0x2000));
    assert_dynrec_translation_epoch(&mem, 3);
}

#[test]
fn direct_memory_policy_epoch_ignores_tlb_only_invalidations() {
    let mut mem = Memory::new();

    mem.invalidate_tlb();
    mem.invalidate_tlb_page(0x5000);
    assert_epoch(&mem, 2);
    assert_direct_epoch(&mem, 0);
    assert_dynrec_translation_epoch(&mem, 2);

    mem.set_a20(false);
    assert_epoch(&mem, 3);
    assert_direct_epoch(&mem, 1);
    assert_dynrec_translation_epoch(&mem, 3);
    assert_dynrec_a20_generation(&mem, 1);
}

#[test]
fn claim_hle_ems_frame_advances_direct_memory_policy_epoch() {
    // Claiming the EMS frame changes its direct-RAM admission. It must
    // advance the memory-policy epoch for both the interpreter's fetch
    // proofs and direct-page proofs; a bare TLB flush leaves them stale.
    let mut mem = Memory::new();
    assert_direct_epoch(&mem, 0);
    assert!(
        mem.direct_host_ram_range(EMS_FRAME_BASE, 15).is_some(),
        "frame must be plain RAM before the claim"
    );

    mem.claim_hle_ems_frame(EMS_FRAME_BASE, EMS_FRAME_SIZE)
        .expect("claim HLE EMS frame");

    assert_direct_epoch(&mem, 1);
    assert!(
        mem.direct_host_ram_range(EMS_FRAME_BASE, 15).is_none(),
        "frame must no longer be plain RAM after the claim"
    );
}

/// Every TLB-flush edge must advance the translation-policy epoch so
/// epoch-keyed consumers cannot retain a remapped EMS translation.
#[test]
fn an_ems_frame_invalidation_advances_the_translation_epoch() {
    let mut mem = Memory::new();
    mem.claim_hle_ems_frame(EMS_FRAME_BASE, EMS_FRAME_SIZE)
        .expect("claim HLE EMS frame");

    let before = mem.dynrec_translation_epoch();
    mem.clear_ems_frame_map();
    assert!(
        mem.dynrec_translation_epoch() > before,
        "an EMS frame invalidation flushes the TLB and must say so"
    );
}

#[test]
fn a_moved_descriptor_snoop_hull_advances_the_translation_epoch() {
    let mut mem = Memory::new();

    let before = mem.dynrec_translation_epoch();
    mem.set_gdt_snoop_range(0x1000, 0xFF);
    assert!(
        mem.dynrec_translation_epoch() > before,
        "a moved snoop hull flushes the TLB and must say so"
    );

    let after = mem.dynrec_translation_epoch();
    mem.set_gdt_snoop_range(0x1000, 0xFF);
    assert_eq!(
        mem.dynrec_translation_epoch(),
        after,
        "an identical GDTR reload moves no hull, flushes nothing, and must \
         not churn the epoch"
    );
}

#[test]
fn direct_host_ram_range_pointer_may_be_stored_through() {
    // The escape hatch hands out a mutable pointer, so a store through it
    // must be sound. Deriving it from the shared `&self` `Vec::as_ptr`
    // provenance made that store undefined behaviour under the Rust
    // aliasing model (DD-UNSAFE-1, MDD-BUG-CRUCIBLE-00510); it now comes
    // from the mutable-provenance `ram_base`.
    //
    // Miri-oriented: under a normal `cargo test` this pins the round trip,
    // but run under Miri it is the check that catches a regression to
    // shared provenance.
    let mut mem = Memory::new();
    let addr = 0x2_0000usize;
    let ptr = mem
        .direct_host_ram_range(addr, 4)
        .expect("plain RAM must be admitted");

    // SAFETY: the range was just admitted, so the pointer is valid for 4
    // bytes and carries write provenance over the RAM allocation.
    unsafe {
        ptr.as_ptr().write(0xDE);
        ptr.as_ptr().add(1).write(0xAD);
        ptr.as_ptr().add(2).write(0xBE);
        ptr.as_ptr().add(3).write(0xEF);
    }

    assert_eq!(mem.read_u8(addr), 0xDE);
    assert_eq!(mem.read_u8(addr + 1), 0xAD);
    assert_eq!(mem.read_u8(addr + 2), 0xBE);
    assert_eq!(mem.read_u8(addr + 3), 0xEF);

    // And the safe API is still the source of truth for the same bytes.
    mem.write_u8(addr, 0x11);
    // SAFETY: same admitted range, still valid.
    assert_eq!(unsafe { ptr.as_ptr().read() }, 0x11);
}

#[test]
fn direct_host_ram_range_addresses_the_same_byte_as_ram_ptr_mut() {
    // ram_base and the Vec are the same allocation, so switching the
    // derivation changes provenance only — never the address. This is the
    // part of MDD-BUG-CRUCIBLE-00510 that is checkable without Miri.
    let mut mem = Memory::new();
    for addr in [0usize, 1, 0x1_0000, 0x2_ABCD] {
        let via_hatch = mem
            .direct_host_ram_range(addr, 1)
            .expect("plain RAM must be admitted")
            .as_ptr();
        // SAFETY: addr < usable_len, so this stays inside the allocation.
        let via_base = unsafe { mem.ram_ptr_mut().add(addr) };
        assert_eq!(via_hatch, via_base, "addr {addr:#x}");
    }
}

#[test]
fn dynrec_a20_mask_uses_memory_core_rule() {
    let mut mem = Memory::new();

    assert_eq!(mem.dynrec_mask_physical_address(0x0030_1234), 0x0030_1234);

    mem.set_a20(false);

    assert_eq!(mem.dynrec_mask_physical_address(0x0030_1234), 0x0020_1234);
}

#[test]
fn memory_policy_epoch_increments_on_lfb_and_mmio_mapping_changes() {
    let mut mem = Memory::new();

    mem.init_lfb(256);
    assert_epoch(&mem, 1);

    mem.init_lfb(256);
    assert_epoch(&mem, 1);

    mem.set_lfb_enabled(true);
    assert_epoch(&mem, 2);

    mem.set_lfb_enabled(true);
    assert_epoch(&mem, 2);

    mem.add_mmio_region(0x6000_0000, 1024);
    assert_epoch(&mem, 3);

    mem.add_mmio_region(0x6000_0000, 1024);
    assert_epoch(&mem, 4);

    mem.remove_mmio_region(0x6000_0000);
    assert_epoch(&mem, 5);

    mem.remove_mmio_region(0x6000_0000);
    assert_epoch(&mem, 5);
}

#[test]
fn memory_policy_epoch_increments_on_vga_mapping_policy_changes() {
    let mut mem = Memory::new();

    mem.set_vga_memory_map_select(1);
    assert_epoch(&mem, 1);

    mem.set_vga_memory_map_select(1);
    assert_epoch(&mem, 1);

    mem.set_video_mode(0x13);
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Chain4);
    assert_epoch(&mem, 2);

    mem.vga.chain4 = false;
    mem.update_vga_write_handler();
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Linear);
    assert_epoch(&mem, 3);

    let mut state = mem.vga;
    state.memory_map_select = 2;
    mem.apply_vga_pipeline(&state);
    assert_epoch(&mem, 4);
}

/// MDD-BUG-FLUX-00146: reporting a pending notifier write must not drain
/// it. A device can write guest RAM during I/O; checking for that write
/// must leave the record available for its consumer.
#[test]
fn smc_snoop_pending_reports_a_recorded_write_without_draining_it() {
    // A notifier that reports pending exactly when it has seen a write.
    #[derive(Default)]
    struct PendingNotify {
        seen: Mutex<bool>,
    }
    impl SmcNotify for PendingNotify {
        fn notify_write(&self, _address: usize, _size: usize) {
            if let Ok(mut seen) = self.seen.lock() {
                *seen = true;
            }
        }
        fn notify_write_range(&self, _start: usize, _end: usize) {}
        fn is_pending(&self) -> bool {
            self.seen.lock().map(|seen| *seen).unwrap_or(false)
        }
    }

    let mut mem = Memory::new();
    assert!(
        !mem.smc_snoop_pending(),
        "no notifier bound means nothing can be pending"
    );

    let notifier = Arc::new(PendingNotify::default());
    mem.bind_smc(Some(notifier.clone()));
    assert!(!mem.smc_snoop_pending(), "a fresh notifier is not pending");

    notifier.notify_write(0x2000, 4);
    assert!(
        mem.smc_snoop_pending(),
        "Memory must surface the notifier's pending record"
    );
    assert!(
        mem.smc_snoop_pending(),
        "asking must not drain — the engine root still needs the record"
    );
}

/// A stateless notifier's default must never report a pending write.
#[test]
fn smc_snoop_pending_is_false_for_a_stateless_notifier() {
    let mut mem = Memory::new();
    mem.bind_smc(Some(Arc::new(TestSmcNotify::default())));
    mem.write_u8(0x2000, 0xAA);
    assert!(!mem.smc_snoop_pending());
}

#[test]
fn memory_policy_epoch_increments_on_ram_size_changes() {
    let mut mem = Memory::new();
    mem.ram.resize(0x1000 + FETCH_PADDING, 0);

    mem.ensure_ram_size(0x2000);
    assert_epoch(&mem, 1);

    mem.ensure_ram_size(0x2000);
    assert_epoch(&mem, 1);

    mem.expand_ram(0x3000);
    assert_epoch(&mem, 2);

    mem.expand_ram(0x3000);
    assert_epoch(&mem, 2);
}

fn new_hle_ems_memory(pool_pages: u16) -> Memory {
    let mut mem = Memory::new();
    mem.init_ems_pool(pool_pages).unwrap();
    mem.claim_hle_ems_frame(EMS_FRAME_BASE, EMS_FRAME_SIZE)
        .unwrap();
    mem
}

#[test]
fn ems_mapped_slot_reads_and_writes_backing_pool() {
    let mut mem = new_hle_ems_memory(4);
    mem.write_ems_backing_byte(2, 0, 0x5A).unwrap();
    mem.set_ems_frame_page(0, Some(2)).unwrap();

    assert_eq!(mem.read_u8(EMS_FRAME_BASE), 0x5A);

    mem.write_u8(EMS_FRAME_BASE, 0xA5);

    assert_eq!(mem.read_ems_backing_byte(2, 0).unwrap(), 0xA5);
    assert_eq!(mem.ram[EMS_FRAME_BASE], 0x00);
}

#[test]
fn ems_read_bytes_uses_mapped_backing_pool() {
    let mut mem = new_hle_ems_memory(4);
    mem.ram[EMS_FRAME_BASE] = 0x11;
    mem.ram[EMS_FRAME_BASE + 1] = 0x22;
    mem.write_ems_backing_byte(2, 0, 0xA0).unwrap();
    mem.write_ems_backing_byte(2, 1, 0xB1).unwrap();
    mem.set_ems_frame_page(0, Some(2)).unwrap();

    let mut bytes = [0u8; 2];
    mem.read_bytes(EMS_FRAME_BASE, &mut bytes);

    assert_eq!(bytes, [0xA0, 0xB1]);
}

#[test]
fn ems_same_pool_page_aliases_across_two_slots() {
    let mut mem = new_hle_ems_memory(4);
    mem.set_ems_frame_page(0, Some(1)).unwrap();
    mem.set_ems_frame_page(3, Some(1)).unwrap();

    let offset = 0x1234;
    mem.write_u8(EMS_FRAME_BASE + offset, 0x11);
    assert_eq!(
        mem.read_u8(EMS_FRAME_BASE + 3 * EMS_PAGE_SIZE + offset),
        0x11
    );

    mem.write_u8(EMS_FRAME_BASE + 3 * EMS_PAGE_SIZE + offset, 0x22);
    assert_eq!(mem.read_u8(EMS_FRAME_BASE + offset), 0x22);
    assert_eq!(mem.read_ems_backing_byte(1, offset).unwrap(), 0x22);
}

#[test]
fn ems_unmapped_slot_uses_reserved_frame_ram() {
    let mut mem = new_hle_ems_memory(2);

    mem.write_u8(EMS_FRAME_BASE, 0x77);

    assert_eq!(mem.read_u8(EMS_FRAME_BASE), 0x77);
    assert_eq!(mem.read_ems_backing_byte(0, 0).unwrap(), 0x00);

    mem.write_ems_backing_byte(0, 0, 0xCC).unwrap();
    mem.set_ems_frame_page(0, Some(0)).unwrap();
    assert_eq!(mem.read_u8(EMS_FRAME_BASE), 0xCC);

    mem.set_ems_frame_page(0, None).unwrap();
    assert_eq!(mem.read_u8(EMS_FRAME_BASE), 0x77);
}

#[test]
fn load_rejects_owned_hle_ems_frame() {
    let mut mem = new_hle_ems_memory(2);
    mem.write_u8(EMS_FRAME_BASE, 0x77);
    mem.write_ems_backing_byte(0, 0, 0xCC).unwrap();
    mem.set_ems_frame_page(0, Some(0)).unwrap();

    let err = mem.load(EMS_FRAME_BASE, &[0x99]).unwrap_err();

    assert!(matches!(
        err,
        MemoryError::RawLoadOverlapsHleEmsFrame {
            addr: EMS_FRAME_BASE,
            len: 1
        }
    ));
    assert_eq!(mem.read_ems_backing_byte(0, 0).unwrap(), 0xCC);
    mem.set_ems_frame_page(0, None).unwrap();
    assert_eq!(mem.read_u8(EMS_FRAME_BASE), 0x77);
}

#[test]
fn fill_ram_updates_mapped_hle_ems_backing() {
    let mut mem = new_hle_ems_memory(2);
    mem.write_u8(EMS_FRAME_BASE, 0x77);
    mem.write_u8(EMS_FRAME_BASE + 1, 0x88);
    mem.write_ems_backing_byte(0, 0, 0xCC).unwrap();
    mem.write_ems_backing_byte(0, 1, 0xDD).unwrap();
    mem.set_ems_frame_page(0, Some(0)).unwrap();

    mem.fill_ram(EMS_FRAME_BASE, EMS_FRAME_BASE + 2, 0x99);

    assert_eq!(mem.read_ems_backing_byte(0, 0).unwrap(), 0x99);
    assert_eq!(mem.read_ems_backing_byte(0, 1).unwrap(), 0x99);
    assert_eq!(mem.ram[EMS_FRAME_BASE], 0x77);
    assert_eq!(mem.ram[EMS_FRAME_BASE + 1], 0x88);
    assert_eq!(mem.read_u8(EMS_FRAME_BASE), 0x99);
    assert_eq!(mem.read_u8(EMS_FRAME_BASE + 1), 0x99);
}

#[test]
fn ems_remap_changes_visible_page_without_copying_frame_ram() {
    let mut mem = new_hle_ems_memory(2);
    mem.write_u8(EMS_FRAME_BASE + 4, 0x10);
    mem.write_ems_backing_byte(0, 4, 0x20).unwrap();
    mem.write_ems_backing_byte(1, 4, 0x30).unwrap();

    mem.set_ems_frame_page(0, Some(0)).unwrap();
    assert_eq!(mem.read_u8(EMS_FRAME_BASE + 4), 0x20);
    mem.write_u8(EMS_FRAME_BASE + 4, 0x21);
    assert_eq!(mem.read_ems_backing_byte(0, 4).unwrap(), 0x21);

    mem.set_ems_frame_page(0, Some(1)).unwrap();

    assert_eq!(mem.read_u8(EMS_FRAME_BASE + 4), 0x30);
    assert_eq!(mem.ram[EMS_FRAME_BASE + 4], 0x10);
    assert_eq!(mem.read_ems_backing_byte(0, 4).unwrap(), 0x21);
}

#[test]
fn ems_backing_range_reports_visible_aliases_across_pages() {
    let mut mem = new_hle_ems_memory(4);
    mem.set_ems_frame_page(0, Some(2)).unwrap();
    mem.set_ems_frame_page(1, Some(3)).unwrap();

    let aliases = mem
        .write_ems_backing_range(2, EMS_PAGE_SIZE - 2, &[1, 2, 3, 4])
        .unwrap();

    assert_eq!(
        aliases.as_slice(),
        [
            EmsAliasRange {
                frame_addr: EMS_FRAME_BASE + EMS_PAGE_SIZE - 2,
                len: 2,
            },
            EmsAliasRange {
                frame_addr: EMS_FRAME_BASE + EMS_PAGE_SIZE,
                len: 2,
            },
        ]
    );
    assert_eq!(mem.read_u8(EMS_FRAME_BASE + EMS_PAGE_SIZE - 1), 2);
    assert_eq!(mem.read_u8(EMS_FRAME_BASE + EMS_PAGE_SIZE), 3);
}

#[test]
fn ems_direct_host_ram_and_tlb_exclude_owned_frame() {
    let mut mem = Memory::new();
    assert!(mem.direct_host_ram_range(EMS_FRAME_BASE, 1).is_some());

    mem.init_ems_pool(1).unwrap();
    mem.claim_hle_ems_frame(EMS_FRAME_BASE, EMS_FRAME_SIZE)
        .unwrap();

    assert!(mem.direct_host_ram_range(EMS_FRAME_BASE, 1).is_none());
    assert!(mem.direct_host_ram_range(EMS_FRAME_BASE - 1, 2).is_none());
    assert!(mem.direct_host_ram_range(0x8000, 16).is_some());

    let linear_page = 0x1234;
    mem.tlb_insert(linear_page, (EMS_FRAME_BASE >> 12) as u32, true, true, true);
    let index = SoftwareTlb::index(linear_page);
    let entry = mem.core.tlb().entries[index];
    assert_eq!(entry.host_read, std::ptr::null());
    assert_eq!(entry.host_write, std::ptr::null_mut());

    mem.tlb_insert(linear_page + 1, 0x08, true, true, true);
    let normal = mem.core.tlb().entries[SoftwareTlb::index(linear_page + 1)];
    assert!(!normal.host_read.is_null());
    assert!(!normal.host_write.is_null());
}

#[test]
fn ems_mapped_write_dirties_all_active_frame_aliases() {
    let mut mem = new_hle_ems_memory(2);
    mem.set_ems_frame_page(0, Some(1)).unwrap();
    mem.set_ems_frame_page(2, Some(1)).unwrap();
    let alias_addr = EMS_FRAME_BASE + 2 * EMS_PAGE_SIZE + 7;
    mem.mark_instr_exec_page(alias_addr);
    assert!(!mem.take_instr_cache_dirty());

    mem.write_u8(EMS_FRAME_BASE + 7, 0x44);

    assert!(mem.take_instr_cache_dirty());
}

#[test]
fn ems_physical_u32_write_crossing_frame_end_preserves_raw_bytes() {
    let mut mem = new_hle_ems_memory(2);
    mem.set_ems_frame_page(3, Some(1)).unwrap();
    mem.ram[EMS_FRAME_END - 1] = 0x11;
    mem.ram[EMS_FRAME_END] = 0x22;
    mem.ram[EMS_FRAME_END + 1] = 0x33;
    mem.ram[EMS_FRAME_END + 2] = 0x44;

    mem.write_u32_physical(EMS_FRAME_END - 1, 0xA1B2C3D4);

    assert_eq!(
        mem.read_ems_backing_byte(1, EMS_PAGE_SIZE - 1).unwrap(),
        0xD4
    );
    assert_eq!(mem.ram[EMS_FRAME_END - 1], 0x11);
    assert_eq!(mem.ram[EMS_FRAME_END], 0xC3);
    assert_eq!(mem.ram[EMS_FRAME_END + 1], 0xB2);
    assert_eq!(mem.ram[EMS_FRAME_END + 2], 0xA1);
    assert_eq!(mem.read_u32_physical(EMS_FRAME_END - 1), 0xA1B2C3D4);
}

#[test]
fn ems_guest_byte_slice_write_crosses_mapped_slots() {
    let mut mem = new_hle_ems_memory(2);
    mem.set_ems_frame_page(0, Some(0)).unwrap();
    mem.set_ems_frame_page(1, Some(1)).unwrap();
    mem.ram[EMS_FRAME_BASE + EMS_PAGE_SIZE - 1] = 0xCC;
    mem.ram[EMS_FRAME_BASE + EMS_PAGE_SIZE] = 0xDD;

    mem.write_bytes_guest(EMS_FRAME_BASE + EMS_PAGE_SIZE - 1, &[0x12, 0x34]);

    assert_eq!(
        mem.read_ems_backing_byte(0, EMS_PAGE_SIZE - 1).unwrap(),
        0x12
    );
    assert_eq!(mem.read_ems_backing_byte(1, 0).unwrap(), 0x34);
    assert_eq!(mem.read_u8(EMS_FRAME_BASE + EMS_PAGE_SIZE - 1), 0x12);
    assert_eq!(mem.read_u8(EMS_FRAME_BASE + EMS_PAGE_SIZE), 0x34);
    assert_eq!(mem.ram[EMS_FRAME_BASE + EMS_PAGE_SIZE - 1], 0xCC);
    assert_eq!(mem.ram[EMS_FRAME_BASE + EMS_PAGE_SIZE], 0xDD);
}

/// Counts ERROR events on this thread.
struct ErrorCounter(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl tracing::Subscriber for ErrorCounter {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        if *event.metadata().level() == tracing::Level::ERROR {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

/// MDD-BUG-CRU-01484: the IVT-corruption diagnostic is one-shot per Memory
/// instance, not per process, so a second instance still reports.
#[test]
fn ivt_corruption_is_reported_once_per_memory_instance() {
    let errors = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let corrupt = |mem: &mut Memory| {
        mem.set_a20(false); // take the checked write path
        mem.write_u8(0x0003, 0xA0); // vector 0 segment -> 0xA000
    };
    tracing::subscriber::with_default(ErrorCounter(errors.clone()), || {
        let mut first = Memory::new();
        corrupt(&mut first);
        corrupt(&mut first);
        let mut second = Memory::new();
        corrupt(&mut second);
    });
    assert_eq!(errors.load(std::sync::atomic::Ordering::Relaxed), 2);
}

#[test]
fn test_seg_offset_to_linear() {
    assert_eq!(Memory::seg_offset_to_linear(0x1000, 0x0000), 0x10000);
    assert_eq!(Memory::seg_offset_to_linear(0x0000, 0x7C00), 0x07C00);
    assert_eq!(Memory::seg_offset_to_linear(0xFFFF, 0x000F), 0xFFFFF);
}

#[test]
fn test_read_write_u8() {
    let mut mem = Memory::new();
    mem.write_u8(0x1000, 0x42);
    assert_eq!(mem.read_u8(0x1000), 0x42);
}

#[test]
fn test_read_write_u16() {
    let mut mem = Memory::new();
    mem.write_u16(0x1000, 0x1234);
    assert_eq!(mem.read_u16(0x1000), 0x1234);
    assert_eq!(mem.read_u8(0x1000), 0x34); // Little-endian
    assert_eq!(mem.read_u8(0x1001), 0x12);
}

#[test]
fn test_default() {
    let mem = Memory::default();
    assert!(mem.ram_size() > 0);
}

#[test]
fn test_ram_ptr() {
    let mem = Memory::new();
    let ptr = mem.ram_ptr();
    assert!(!ptr.is_null());
}

#[test]
fn test_ram_ptr_mut() {
    let mut mem = Memory::new();
    let ptr = mem.ram_ptr_mut();
    assert!(!ptr.is_null());
}

#[test]
fn test_read_write_u32() {
    let mut mem = Memory::new();
    mem.write_u32(0x1000, 0x12345678);
    assert_eq!(mem.read_u32(0x1000), 0x12345678);
    assert_eq!(mem.read_u16(0x1000), 0x5678); // Little-endian low word
    assert_eq!(mem.read_u16(0x1002), 0x1234); // Little-endian high word
}

#[test]
fn test_read_u8_out_of_bounds() {
    let mem = Memory::new();
    // Reading beyond RAM with A20 disabled wraps, so test with A20 enabled
    // The mem is larger than expected due to initialization, just verify behavior
    // Read at max masked address - should return 0 or 0xFF depending on state
    let result = mem.read_u8(mem.ram_size() + 100);
    // With A20 disabled, this wraps; either way, shouldn't panic
    let _ = result;
}

#[test]
fn test_write_u8_rom_protection() {
    let mut mem = Memory::new();
    // Write to ROM area should be ignored
    let rom_addr = BIOS_BASE + 0x100;
    let original = mem.read_u8(rom_addr);
    mem.write_u8(rom_addr, 0xFF);
    assert_eq!(mem.read_u8(rom_addr), original);
}

#[test]
fn test_write_rom_bypasses_protection() {
    let mut mem = Memory::new();
    // write_rom should bypass ROM protection
    let rom_addr = BIOS_BASE + 0x100;
    let original = mem.read_u8(rom_addr);
    mem.write_rom(rom_addr, 0xAB);
    assert_eq!(mem.read_u8(rom_addr), 0xAB);
    assert_ne!(original, 0xAB);
}

#[test]
fn test_write_rom_out_of_range_ignored() {
    let mut mem = Memory::new();
    let before = mem.slice(0, 4);
    mem.write_rom(MAX_RAM_SIZE + 0x10, 0xCC);
    assert_eq!(mem.slice(0, 4), before);
}

#[test]
fn test_write_u8_within_bounds() {
    let mut mem = Memory::new();

    // Write within the normal RAM area
    mem.write_u8(0x50000, 0xAB);
    assert_eq!(mem.read_u8(0x50000), 0xAB);
}

#[test]
fn test_write_u8_out_of_range_ignored() {
    let mut mem = Memory::new();
    mem.set_a20(true);

    // Write beyond addressable range should not panic
    mem.write_u8(0x200000, 0xFF);
    // Just verify it doesn't crash
}

#[test]
fn test_read_bytes() {
    let mut mem = Memory::new();
    mem.write_u8(0x1000, 0x11);
    mem.write_u8(0x1001, 0x22);
    mem.write_u8(0x1002, 0x33);
    mem.write_u8(0x1003, 0x44);

    let mut buf = [0u8; 4];
    mem.read_bytes(0x1000, &mut buf);
    assert_eq!(buf, [0x11, 0x22, 0x33, 0x44]);
}

#[test]
fn read_run_matches_per_byte_read_u8() {
    let mut mem = Memory::new();
    // Distinctive fill across conventional RAM and up past the VGA window.
    for addr in (0x1F00..0x2100)
        .chain(0x9FFF0..0xA0010)
        .chain(0xBFFF0..0xC0010)
    {
        mem.write_u8(addr, (addr as u8).wrapping_mul(31).wrapping_add(7));
    }
    // Representative ranges: plain RAM, VGA-window straddle (slow path),
    // option-ROM area, EMS frame, end-of-usable-RAM straddle, and the
    // widest x87 operand (108 bytes, FRSTOR).
    // Distinctive fill in extended memory for the high fast-path window.
    for addr in (0x1F_FFF0..0x20_0010).chain(0xFFFF0..0x10_0010) {
        mem.write_u8(addr, (addr as u8).wrapping_mul(29).wrapping_add(3));
    }
    let usable = mem.usable_len();
    let cases: [(usize, usize); 11] = [
        (0x2000, 4),
        (0x2000, 8),
        (0x1FFD, 10),
        (0x2000, 108),
        (0x9FFFC, 8),     // straddles into the VGA window
        (0xC0000, 8),     // option-ROM region
        (0xE0000, 8),     // EMS frame base
        (usable - 4, 8),  // straddles end of usable RAM
        (0x20_0000, 8),   // extended RAM (high fast-path window)
        (0x20_0000, 108), // widest x87 operand in extended RAM
        (0xF_FFFC, 8),    // straddles the 1 MiB boundary (falls back)
    ];
    for (addr, len) in cases {
        let mut bulk = vec![0u8; len];
        mem.read_run(addr, &mut bulk);
        let per_byte: Vec<u8> = (0..len).map(|i| mem.read_u8(addr + i)).collect();
        assert_eq!(bulk, per_byte, "addr {addr:#X} len {len}");
    }
    // A20 disabled: bit 20 masks per byte, so high runs must still match
    // the per-byte dispatch (the fast path must not engage).
    mem.set_a20(false);
    for (addr, len) in [(0x1F_FFF8usize, 16usize), (0x10_0000, 8)] {
        let mut bulk = vec![0u8; len];
        mem.read_run(addr, &mut bulk);
        let per_byte: Vec<u8> = (0..len).map(|i| mem.read_u8(addr + i)).collect();
        assert_eq!(bulk, per_byte, "a20-off addr {addr:#X} len {len}");
    }
    mem.set_a20(true);
    // Real-boot high-memory alias: runs inside the fold must match.
    mem.set_high_mem_alias_enabled(true);
    for (addr, len) in [(HIGH_MEM_ALIAS_BASE + 0x10, 16usize), (0x20_0000, 8)] {
        let mut bulk = vec![0u8; len];
        mem.read_run(addr, &mut bulk);
        let per_byte: Vec<u8> = (0..len).map(|i| mem.read_u8(addr + i)).collect();
        assert_eq!(bulk, per_byte, "alias addr {addr:#X} len {len}");
    }
}

/// A052: the write-side peer of `read_run_matches_per_byte_read_u8`.
///
/// `MemoryAccess::write_run` is defined as a `write_u8` loop, so the
/// `Memory` override must be indistinguishable from one — over plain RAM,
/// the VGA window, the option-ROM/EMS apertures, an SMC-watched page, a
/// recorded page-table frame, an armed descriptor snoop, and armed write
/// tracking. Two side effects are span-granular BY DESIGN and are checked
/// as such: the SMC notifier reports one span instead of N single bytes
/// (same byte set — a conservative superset for every consumer), and the
/// PT tripwire bumps its epoch once instead of N times (every consumer is
/// a change detector). Everything else must match exactly.
#[test]
fn write_run_matches_per_byte_write_u8() {
    /// Every fixture is grown to back this much RAM, so a high-window
    /// case exercises the window rather than the `usable_len()` refusal.
    const FIXTURE_RAM_TOP: usize = 0x30_0000;

    /// The bytes a list of `(addr, len)` notifications covers.
    fn notified_bytes(spans: &[(usize, usize)]) -> std::collections::BTreeSet<usize> {
        spans
            .iter()
            .flat_map(|&(addr, len)| addr..addr + len)
            .collect()
    }

    /// Order-sensitive digest of the WHOLE RAM image. The windowed
    /// comparison below cannot see a byte that landed somewhere else
    /// entirely — which is exactly what an A20 mask or an aperture
    /// redirect does — so the digest is the assertion that catches it.
    fn ram_digest(mem: &Memory) -> u64 {
        let mut acc = 0xcbf2_9ce4_8422_2325u64;
        let (chunks, tail) = mem.ram[..mem.ram.len()].as_chunks::<8>();
        for (i, chunk) in chunks.iter().enumerate() {
            let word = u64::from_le_bytes(*chunk);
            if word != 0 {
                acc = acc
                    .wrapping_mul(0x0000_0100_0000_01b3)
                    .wrapping_add((i as u64) ^ word);
            }
        }
        for (i, &byte) in tail.iter().enumerate() {
            acc = acc
                .wrapping_mul(0x0000_0100_0000_01b3)
                .wrapping_add((i as u64) ^ u64::from(byte));
        }
        acc
    }

    /// Everything a caller can observe about one span write.
    struct Observed {
        bytes: Vec<u8>,
        ram_digest: u64,
        smc: std::collections::BTreeSet<usize>,
        smc_calls: usize,
        instr_cache_dirty: bool,
        pt_epoch_bumped: bool,
        pending_writes: usize,
    }

    /// Run one span through `f` on a freshly configured `Memory` and
    /// collect every observable.
    fn observe(addr: usize, len: usize, f: impl FnOnce(&mut Memory, usize, &[u8])) -> Observed {
        let mut mem = Memory::new();
        // Every high-window case must be BACKED, or `end <= usable_len()`
        // refuses it and the case reads green while proving nothing about
        // the high window. `Memory::new()` exposes `DEFAULT_RAM_SIZE`,
        // which covers them — assert it rather than assume it, so a future
        // shrink of that default fails loudly here instead of silently
        // hollowing out the must-bulk group below.
        assert!(
            mem.usable_len() >= FIXTURE_RAM_TOP,
            "fixture RAM ({:#X}) must back every high-window case",
            mem.usable_len()
        );
        let notifier = Arc::new(TestSmcNotify::default());
        mem.bind_smc(Some(notifier.clone()));
        mem.set_smc_has_code_pages(true);
        // Watch the span's own pages and their neighbours, so the SMC
        // snoop and the instruction-cache mark are live for this case
        // wherever it sits — low RAM, the VGA band, or extended RAM.
        let first_page = (addr >> 12).saturating_sub(1) as u32;
        let last_page = ((addr + len) >> 12) as u32 + 1;
        for page in first_page..=last_page {
            mem.set_page_watched(page, true);
            mem.mark_instr_exec_page((page as usize) << 12);
        }
        // Record the span's own first page as a PT frame, so the tripwire
        // is armed for every case rather than one hand-picked address.
        mem.record_pt_frame(addr as u32 >> 12);
        let epoch_before = mem.dynrec_pt_write_epoch();

        let data: Vec<u8> = (0..len)
            .map(|i| (i as u8).wrapping_mul(37).wrapping_add(0x5A))
            .collect();
        f(&mut mem, addr, &data);

        let window = addr.saturating_sub(4)..addr + len + 4;
        Observed {
            bytes: window.map(|a| mem.read_u8(a)).collect(),
            ram_digest: ram_digest(&mem),
            smc: notified_bytes(&notifier.snapshot()),
            smc_calls: notifier.snapshot().len(),
            instr_cache_dirty: mem.is_instr_cache_dirty(),
            pt_epoch_bumped: mem.dynrec_pt_write_epoch() != epoch_before,
            pending_writes: mem.pending_write_count(),
        }
    }

    // Plain RAM (the bulk arm), the VGA window and its straddle, the
    // option-ROM and EMS apertures, and the widest x87 operand.
    let cases: [(usize, usize); 12] = [
        (0x2000, 1),
        (0x2000, 4),
        (0x1FFD, 10),
        (0x2000, 108),    // widest x87 operand, plain RAM
        (0x20FF8, 16),    // straddles the recorded PT frame's page
        (0x9FFFC, 8),     // H3-9: straddles the low arm into the VGA window
        (VIDEO_BASE, 8),  // inside the VGA window
        (0xC0000, 8),     // option-ROM region
        (0xE0000, 8),     // EMS frame base
        (0xF_FFFC, 8),    // straddles 1 MiB — satisfies NEITHER window
        (0x20_0000, 8),   // extended RAM, the new high window
        (0x20_0000, 108), // widest x87 operand in extended RAM
    ];
    for (addr, len) in cases {
        let bulk = observe(addr, len, |mem, addr, data| {
            MemoryAccess::write_run(mem, addr, data);
        });
        let per_byte = observe(addr, len, |mem, addr, data| {
            for (i, byte) in data.iter().copied().enumerate() {
                mem.write_u8(addr + i, byte);
            }
        });
        assert_eq!(
            bulk.bytes, per_byte.bytes,
            "bytes differ at {addr:#X} len {len}"
        );
        assert_eq!(
            bulk.ram_digest, per_byte.ram_digest,
            "RAM image differs at {addr:#X} len {len}"
        );
        assert_eq!(
            bulk.smc, per_byte.smc,
            "SMC-notified byte set differs at {addr:#X} len {len}"
        );
        assert!(
            bulk.smc_calls <= per_byte.smc_calls,
            "span notification must never be finer-grained than per-byte \
             at {addr:#X} len {len}"
        );
        assert_eq!(
            bulk.instr_cache_dirty, per_byte.instr_cache_dirty,
            "instruction-cache dirty flag differs at {addr:#X} len {len}"
        );
        assert_eq!(
            bulk.pt_epoch_bumped, per_byte.pt_epoch_bumped,
            "PT tripwire engagement differs at {addr:#X} len {len}"
        );
        assert_eq!(
            bulk.pending_writes, per_byte.pending_writes,
            "write-tracking records differ at {addr:#X} len {len}"
        );
    }

    type Arm = fn(&mut Memory);

    // POSITIVE ENGAGEMENT. Equality alone is satisfied by refusing every
    // span — the state this whole change exists to leave. Each case here
    // must actually BULK, proven by the SMC notifier collapsing from N
    // per-byte calls to one span call. Without these, widening the
    // predicate and reverting it both read green.
    let must_bulk: [(&str, Arm, usize, usize); 3] = [
        (
            // The headline: a PM32 guest always has a descriptor table
            // registered, so `has_descriptor_snoop()` refused EVERY span.
            // A hull that the span does not touch must not refuse.
            "descriptor snoop armed elsewhere",
            |mem: &mut Memory| mem.set_gdt_snoop_range(0x8000, 0xFF),
            0x2000,
            16,
        ),
        (
            // The high window itself: plain extended RAM, widest x87
            // operand. Refused outright before H2.
            "extended RAM",
            |_mem: &mut Memory| {},
            0x20_0000,
            108,
        ),
        (
            "extended RAM with the snoop armed low",
            |mem: &mut Memory| mem.set_gdt_snoop_range(0x8000, 0xFF),
            0x20_0000,
            108,
        ),
    ];
    for (label, arm, addr, len) in must_bulk {
        let bulk = observe(addr, len, |mem, addr, data| {
            arm(mem);
            MemoryAccess::write_run(mem, addr, data);
        });
        let per_byte = observe(addr, len, |mem, addr, data| {
            arm(mem);
            for (i, byte) in data.iter().copied().enumerate() {
                mem.write_u8(addr + i, byte);
            }
        });
        assert_eq!(bulk.bytes, per_byte.bytes, "bytes differ under {label}");
        assert_eq!(
            bulk.ram_digest, per_byte.ram_digest,
            "RAM image differs under {label}"
        );
        assert_eq!(
            bulk.smc, per_byte.smc,
            "SMC-notified byte set differs under {label}"
        );
        assert_eq!(
            bulk.instr_cache_dirty, per_byte.instr_cache_dirty,
            "instruction-cache dirty flag differs under {label}"
        );
        assert_eq!(
            bulk.pt_epoch_bumped, per_byte.pt_epoch_bumped,
            "PT tripwire engagement differs under {label}"
        );
        assert_eq!(
            bulk.pending_writes, per_byte.pending_writes,
            "write-tracking records differ under {label}"
        );
        assert!(
            bulk.smc_calls < per_byte.smc_calls,
            "{label} MUST take the bulk arm: {} span notifications vs {} per-byte \
             (equal means it refused, and this test would pass vacuously)",
            bulk.smc_calls,
            per_byte.smc_calls
        );
    }

    // The fetch-padding tail sits at whatever `usable_len()` the fixture
    // reports, so ASK — a hard-coded address silently stops straddling the
    // boundary the day the default RAM size moves.
    let tail_addr = {
        let probe = Memory::new();
        let addr = probe.usable_len() - 4;
        assert!(
            addr + 16 > probe.usable_len() && addr + 16 <= probe.ram.len(),
            "the tail case must end inside the fetch padding, not past it"
        );
        addr
    };

    // Gates the bulk arm must refuse outright: with each of these live the
    // two arms have to be observationally IDENTICAL, notification counts
    // included, because `write_run` must fall back to the per-byte path.
    let gates: [(&str, Arm, usize, usize); 10] = [
        (
            // The hull COVERS the span: the snoop would really fire, and
            // it is inexpressible for a span wider than its `u64` value,
            // so refusal is the only correct treatment.
            "descriptor snoop over the span",
            |mem: &mut Memory| mem.set_gdt_snoop_range(0x2000, 0xFF),
            0x2000,
            16,
        ),
        (
            // Hull edge, span straddling `snoop_min` from below.
            "descriptor snoop straddled at the low edge",
            |mem: &mut Memory| mem.set_gdt_snoop_range(0x2008, 0xFF),
            0x2000,
            16,
        ),
        (
            // Hull edge, span straddling `snoop_max` from above.
            "descriptor snoop straddled at the high edge",
            |mem: &mut Memory| mem.set_gdt_snoop_range(0x1F00, 0x107),
            0x2000,
            16,
        ),
        (
            // The hull is GDT+LDT combined, so an LDT-only registration
            // must refuse exactly the same way.
            "LDT snoop straddled",
            |mem: &mut Memory| mem.set_ldt_snoop_range(0x2008, 0xFF),
            0x2000,
            16,
        ),
        (
            // The fetch-padding tail. Admitting it would let the bulk arm
            // scribble on padding the per-byte path grows RAM to reach.
            "top-of-RAM tail",
            |_mem: &mut Memory| {},
            tail_addr,
            16,
        ),
        (
            // Real-boot alias folds high addresses to ROM.
            "high-memory alias over a high span",
            |mem: &mut Memory| mem.set_high_mem_alias_enabled(true),
            0x20_0000,
            16,
        ),
        (
            "write tracking",
            |mem: &mut Memory| {
                mem.enable_write_tracking();
                mem.set_write_context(0x1234, 0x5678);
            },
            0x2000,
            16,
        ),
        (
            "high-memory alias",
            |mem: &mut Memory| mem.set_high_mem_alias_enabled(true),
            0x2000,
            16,
        ),
        (
            // A ROM page SWALLOWS its bytes. A bulk copy that ignored the
            // gate would write them into `ram` regardless — invisible in
            // the read-back window, caught by the digest.
            "ROM page",
            |mem: &mut Memory| mem.mark_rom_page(0x2000 >> ROM_PAGE_SHIFT),
            0x2000,
            16,
        ),
        (
            // A20 off REDIRECTS: `mask_address` clears bit 20, so a span
            // at 1 MiB lands at 0 byte for byte and stops being
            // contiguous. This is what the high window's explicit
            // `a20_enabled` clause buys — the low window needs no such
            // clause because bit 20 is already clear below 0xA0000.
            "A20 masking",
            |mem: &mut Memory| mem.set_a20(false),
            0x10_0000,
            16,
        ),
    ];
    for (label, arm, addr, len) in gates {
        let bulk = observe(addr, len, |mem, addr, data| {
            arm(mem);
            MemoryAccess::write_run(mem, addr, data);
        });
        let per_byte = observe(addr, len, |mem, addr, data| {
            arm(mem);
            for (i, byte) in data.iter().copied().enumerate() {
                mem.write_u8(addr + i, byte);
            }
        });
        assert_eq!(bulk.bytes, per_byte.bytes, "bytes differ under {label}");
        assert_eq!(
            bulk.ram_digest, per_byte.ram_digest,
            "RAM image differs under {label}"
        );
        assert_eq!(
            bulk.smc_calls, per_byte.smc_calls,
            "{label} must force the per-byte path, notification for notification"
        );
        assert_eq!(
            bulk.pending_writes, per_byte.pending_writes,
            "write-tracking records differ under {label}"
        );
    }
}

#[test]
fn test_read_bytes_normal() {
    let mut mem = Memory::new();
    mem.write_u8(0x2000, 0xAA);
    mem.write_u8(0x2001, 0xBB);

    let mut buf = [0u8; 2];
    mem.read_bytes(0x2000, &mut buf);
    assert_eq!(buf, [0xAA, 0xBB]);
}

#[test]
fn test_read_u8_seg() {
    let mut mem = Memory::new();
    mem.write_u8(0x12345, 0xAB);
    assert_eq!(mem.read_u8_seg(0x1234, 0x0005), 0xAB);
}

#[test]
fn test_read_u16_seg() {
    let mut mem = Memory::new();
    mem.write_u16(0x12345, 0xCDEF);
    assert_eq!(mem.read_u16_seg(0x1234, 0x0005), 0xCDEF);
}

#[test]
fn test_write_u8_seg() {
    let mut mem = Memory::new();
    mem.write_u8_seg(0x1234, 0x0005, 0x42);
    assert_eq!(mem.read_u8(0x12345), 0x42);
}

#[test]
fn test_slice_mut_borrowed_writes_through() {
    let mut mem = Memory::new();

    {
        let mut slice = mem.slice_mut(0x2000, 2);
        slice[0] = 0xAA;
        slice[1] = 0xBB;
    }

    assert_eq!(mem.read_u8(0x2000), 0xAA);
    assert_eq!(mem.read_u8(0x2001), 0xBB);
}

#[test]
fn test_slice_mut_zero_len_is_noop() {
    let mut mem = Memory::new();
    mem.write_u8(0x1000, 0x11);
    {
        let slice = mem.slice_mut(0x1000, 0);
        assert!(slice.is_empty());
    }
    assert_eq!(mem.read_u8(0x1000), 0x11);
}

#[test]
fn test_write_u16_seg() {
    let mut mem = Memory::new();
    mem.write_u16_seg(0x1234, 0x0005, 0xABCD);
    assert_eq!(mem.read_u16(0x12345), 0xABCD);
}

#[test]
fn test_video_memory() {
    let mem = Memory::new();
    let video = mem.video_memory();
    assert_eq!(video.len(), VIDEO_SIZE);
}

#[test]
fn test_video_memory_mut() {
    let mut mem = Memory::new();
    {
        let video = mem.video_memory_mut();
        video[0] = 0xAB;
    }
    assert_eq!(mem.read_u8(VIDEO_BASE), 0xAB);
}

#[test]
fn test_write_video_planar() {
    let mut mem = Memory::new();

    // Write to plane 0 only
    mem.write_video_planar(0, 0xAA, 0x01);
    assert_eq!(mem.planes_flat[0], 0xAA);
    assert_eq!(mem.planes_flat[PLANE_SIZE], 0x00); // Plane 1 unchanged
    assert_eq!(mem.planes_flat[PLANE_SIZE * 2], 0x00); // Plane 2 unchanged
    assert_eq!(mem.planes_flat[PLANE_SIZE * 3], 0x00); // Plane 3 unchanged

    // Write to all planes
    mem.write_video_planar(1, 0xBB, 0x0F);
    assert_eq!(mem.planes_flat[1], 0xBB); // Plane 0
    assert_eq!(mem.planes_flat[PLANE_SIZE + 1], 0xBB); // Plane 1
    assert_eq!(mem.planes_flat[PLANE_SIZE * 2 + 1], 0xBB); // Plane 2
    assert_eq!(mem.planes_flat[PLANE_SIZE * 3 + 1], 0xBB); // Plane 3
}

#[test]
fn test_video_write_change_counts_linear_and_planar_writes() {
    let mut mem = Memory::new();
    assert!(mem.video_write_change_stats_enabled());
    assert_eq!(
        mem.video_write_change_counts(),
        VideoWriteChangeCounts::default()
    );

    mem.write_u8(TEXT_VIDEO_BASE_COLOR, 0x12);
    assert_eq!(
        mem.video_write_change_counts(),
        VideoWriteChangeCounts {
            changed: 1,
            unchanged: 0
        }
    );

    mem.write_u8(TEXT_VIDEO_BASE_COLOR, 0x12);
    assert_eq!(
        mem.video_write_change_counts(),
        VideoWriteChangeCounts {
            changed: 1,
            unchanged: 1
        }
    );

    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.memory_map_select = 1;
    mem.vga.plane_mask = 0x0F;
    mem.vga.write_mode = 0;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.vga.set_reset = 0;
    mem.vga.data_rotate = 0;
    mem.update_vga_write_handler();

    mem.write_u8(VIDEO_BASE, 0xA5);
    mem.write_u8(VIDEO_BASE, 0xA5);
    assert_eq!(
        mem.video_write_change_counts(),
        VideoWriteChangeCounts {
            changed: 2,
            unchanged: 2
        }
    );
}

#[test]
fn test_video_write_change_samples_capture_and_clear_planar_writes() {
    let mut mem = Memory::new();
    assert_eq!(
        mem.video_write_change_samples(),
        VideoWriteChangeSamples::default()
    );

    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.memory_map_select = 1;
    mem.vga.plane_mask = 0x0F;
    mem.vga.write_mode = 0;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.vga.set_reset = 0;
    mem.vga.data_rotate = 0;
    mem.update_vga_write_handler();

    mem.write_u8(VIDEO_BASE, 0xA5);
    mem.write_u8(VIDEO_BASE, 0xA5);

    let samples = mem.video_write_change_samples();
    assert_eq!(samples.dropped, 0);
    assert_eq!(
        samples.samples,
        vec![
            VideoWriteChangeSample {
                source: None,
                offset: 0,
                plane: 0,
                old_value: 0,
                new_value: 0xA5,
            },
            VideoWriteChangeSample {
                source: None,
                offset: 0,
                plane: 1,
                old_value: 0,
                new_value: 0xA5,
            },
            VideoWriteChangeSample {
                source: None,
                offset: 0,
                plane: 2,
                old_value: 0,
                new_value: 0xA5,
            },
            VideoWriteChangeSample {
                source: None,
                offset: 0,
                plane: 3,
                old_value: 0,
                new_value: 0xA5,
            },
        ]
    );

    mem.clear_video_write_change_samples();
    assert_eq!(
        mem.video_write_change_samples(),
        VideoWriteChangeSamples::default()
    );

    mem.set_write_context(0x1234, 0x5678);
    mem.write_u8(VIDEO_BASE + 1, 0x5A);
    let samples = mem.video_write_change_samples();
    assert_eq!(
        samples.samples,
        vec![
            VideoWriteChangeSample {
                source: Some(WriteContext {
                    cs: 0x1234,
                    ip: 0x5678,
                    regs: None,
                }),
                offset: 1,
                plane: 0,
                old_value: 0,
                new_value: 0x5A,
            },
            VideoWriteChangeSample {
                source: Some(WriteContext {
                    cs: 0x1234,
                    ip: 0x5678,
                    regs: None,
                }),
                offset: 1,
                plane: 1,
                old_value: 0,
                new_value: 0x5A,
            },
            VideoWriteChangeSample {
                source: Some(WriteContext {
                    cs: 0x1234,
                    ip: 0x5678,
                    regs: None,
                }),
                offset: 1,
                plane: 2,
                old_value: 0,
                new_value: 0x5A,
            },
            VideoWriteChangeSample {
                source: Some(WriteContext {
                    cs: 0x1234,
                    ip: 0x5678,
                    regs: None,
                }),
                offset: 1,
                plane: 3,
                old_value: 0,
                new_value: 0x5A,
            },
        ]
    );
}

#[test]
fn test_read_video_planar() {
    let mut mem = Memory::new();

    // Set up planar data
    mem.planes_flat[0] = 0x11; // Plane 0
    mem.planes_flat[PLANE_SIZE] = 0x22; // Plane 1
    mem.planes_flat[PLANE_SIZE * 2] = 0x33; // Plane 2
    mem.planes_flat[PLANE_SIZE * 3] = 0x44; // Plane 3

    assert_eq!(mem.read_video_planar(0, 0), 0x11);
    assert_eq!(mem.read_video_planar(0, 1), 0x22);
    assert_eq!(mem.read_video_planar(0, 2), 0x33);
    assert_eq!(mem.read_video_planar(0, 3), 0x44);
}

#[test]
fn test_read_video_planar_out_of_bounds() {
    let mem = Memory::new();
    // Very large offset that would be out of video memory
    let result = mem.read_video_planar(VIDEO_SIZE, 0);
    assert_eq!(result, 0);
}

#[test]
fn test_chain4_write_updates_selected_plane_and_linear() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.plane_mask = 0x0F;
    mem.vga.write_mode = 0;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.vga.set_reset = 0;
    mem.vga.data_rotate = 0;
    mem.vga.read_mode = 0;
    mem.update_vga_write_handler();

    mem.write_u8(VIDEO_BASE, 0x12);
    mem.write_u8(VIDEO_BASE + 1, 0x34);
    mem.write_u8(VIDEO_BASE + 2, 0x56);
    mem.write_u8(VIDEO_BASE + 3, 0x78);

    assert_eq!(mem.plane(0)[0], 0x12);
    assert_eq!(mem.plane(1)[0], 0x34);
    assert_eq!(mem.plane(2)[0], 0x56);
    assert_eq!(mem.plane(3)[0], 0x78);

    let vm = mem.video_memory();
    assert_eq!(vm[0], 0x12);
    assert_eq!(vm[1], 0x34);
    assert_eq!(vm[2], 0x56);
    assert_eq!(vm[3], 0x78);
}

#[test]
fn test_chain4_plane_mask_ignored_per_spec() {
    // Chain-4 selects the plane via A1:A0 and ignores SR[2] (Map Mask).
    // The address-selected plane overrides the map mask.
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.plane_mask = 0x01; // SR[2] = "only plane 0" — must be ignored
    mem.vga.write_mode = 0;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.vga.set_reset = 0;
    mem.vga.data_rotate = 0;
    mem.update_vga_write_handler();

    // Address 1 → plane 1 in chain-4. SR[2]=0x01 must NOT block this.
    mem.write_u8(VIDEO_BASE + 1, 0xAA);
    assert_eq!(mem.plane(1)[0], 0xAA, "chain-4 must ignore SR[2]");
    assert_eq!(mem.video_memory()[1], 0xAA);

    // Address 0 → plane 0. Lands as before.
    mem.write_u8(VIDEO_BASE, 0x55);
    assert_eq!(mem.plane(0)[0], 0x55);
    assert_eq!(mem.video_memory()[0], 0x55);
}

#[test]
fn test_chain4_writes_all_four_planes_with_restrictive_map_mask() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.plane_mask = 0x01; // SR[2] would gate planes 1-3 under the bug
    mem.vga.write_mode = 0;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.vga.set_reset = 0;
    mem.vga.data_rotate = 0;
    mem.update_vga_write_handler();

    mem.write_u8(VIDEO_BASE, 0x11); // plane 0
    mem.write_u8(VIDEO_BASE + 1, 0x22); // plane 1 — would speckle pre-fix
    mem.write_u8(VIDEO_BASE + 2, 0x33); // plane 2 — would speckle pre-fix
    mem.write_u8(VIDEO_BASE + 3, 0x44); // plane 3 — would speckle pre-fix

    assert_eq!(mem.plane(0)[0], 0x11);
    assert_eq!(mem.plane(1)[0], 0x22);
    assert_eq!(mem.plane(2)[0], 0x33);
    assert_eq!(mem.plane(3)[0], 0x44);

    let vm = mem.video_memory();
    assert_eq!(vm[0], 0x11);
    assert_eq!(vm[1], 0x22);
    assert_eq!(vm[2], 0x33);
    assert_eq!(vm[3], 0x44);
}

#[test]
fn test_chain4_writes_with_zero_map_mask_still_land() {
    // Edge case: SR[2] = 0x00 (all planes "disabled").
    // Chain-4 ignores SR[2] entirely — even a fully-zero SR[2]
    // does not block writes.
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.plane_mask = 0x00; // would block all writes pre-fix
    mem.vga.write_mode = 0;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.vga.set_reset = 0;
    mem.vga.data_rotate = 0;
    mem.update_vga_write_handler();

    mem.write_u8(VIDEO_BASE, 0xAA);
    mem.write_u8(VIDEO_BASE + 1, 0xBB);
    assert_eq!(
        mem.plane(0)[0],
        0xAA,
        "chain-4 with SR[2]=0x00 must still write"
    );
    assert_eq!(mem.plane(1)[0], 0xBB);
}

#[test]
fn test_chain4_write_mode_1_latch_copy_still_runs_pipeline() {
    // Pin: chain-4 + write_mode 1 (latch copy) must still run the GC
    // pipeline. Defends against a regression where someone "simplifies"
    // the chain-4 arm into a direct `planes_flat[idx] = value`,
    // breaking latch-copy.
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.plane_mask = 0x0F; // immaterial under chain-4
    mem.vga.write_mode = 1; // latch copy
    mem.vga.bit_mask = 0xFF;
    mem.vga.read_mode = 0;
    mem.update_vga_write_handler();

    // Seed plane bytes, then trigger a chain-4 read to load the latches.
    mem.plane_mut(0)[0] = 0xDE;
    mem.plane_mut(1)[0] = 0xAD;
    mem.plane_mut(2)[0] = 0xBE;
    mem.plane_mut(3)[0] = 0xEF;
    let _ = mem.read_u8(VIDEO_BASE);

    // Write to plane-2 selector with arbitrary CPU data — write_mode 1
    // ignores CPU data and writes the latch into the addressed plane.
    mem.write_u8(VIDEO_BASE + 2, 0x00);
    assert_eq!(
        mem.plane(2)[0],
        0xBE,
        "chain-4 + write_mode 1 must drop the plane-2 latch into plane 2"
    );
    // The other three planes must remain untouched: chain-4 forces
    // plane via A1:A0, so writing to address 2 only affects plane 2.
    assert_eq!(mem.plane(0)[0], 0xDE, "plane 0 untouched");
    assert_eq!(mem.plane(1)[0], 0xAD, "plane 1 untouched");
    assert_eq!(mem.plane(3)[0], 0xEF, "plane 3 untouched");
}

#[test]
fn test_chain4_write_with_enable_set_reset_still_applies_pipeline() {
    // Chain-4 only reshapes plane selection; the GC pipeline (set_reset,
    // bit_mask, write_mode) still applies. Verify by enabling set/reset
    // for plane 1 and writing to address 1 (chain-4 plane = 1).
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.plane_mask = 0x0F;
    mem.vga.write_mode = 0;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0x02; // plane 1 driven by set_reset
    mem.vga.set_reset = 0x02; // set_reset bit 1 = 1 → plane 1 expands to 0xFF
    mem.vga.data_rotate = 0;
    mem.update_vga_write_handler();

    // CPU data is 0x00, but set_reset for plane 1 forces 0xFF.
    mem.write_u8(VIDEO_BASE + 1, 0x00);
    assert_eq!(
        mem.plane(1)[0],
        0xFF,
        "chain-4 + enable_set_reset must still apply set/reset expansion"
    );
}

#[test]
fn test_chain4_write_mode_2_color_expansion_still_runs_pipeline() {
    // Chain-4 only forces plane selection; the GC pipeline (write_mode 2
    // colour expansion) still applies. With chain-4, only the addressed
    // plane receives the expansion result.
    //
    // Without this pin, a future refactor that bypasses
    // `write_planar_ega_masked` for chain-4 (e.g. a "fast-path" direct
    // store) would silently break write_mode 2.
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.plane_mask = 0x0F; // immaterial under chain-4
    mem.vga.write_mode = 2;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.vga.set_reset = 0;
    mem.vga.data_rotate = 0;
    mem.update_vga_write_handler();

    // Address 1 → plane 1 (chain-4 forces plane via A1:A0).
    // CPU data bit 1 = 1 → plane 1 expansion = 0xFF.
    mem.write_u8(VIDEO_BASE + 1, 0b0000_0010);
    assert_eq!(
        mem.plane(1)[0],
        0xFF,
        "chain-4 + write_mode 2: plane-1 bit set in CPU data → 0xFF in plane 1"
    );

    // Address 2 → plane 2. CPU data bit 2 = 0 → plane 2 expansion = 0x00.
    mem.write_u8(VIDEO_BASE + 2, 0b0000_0010);
    assert_eq!(
        mem.plane(2)[0],
        0x00,
        "chain-4 + write_mode 2: plane-2 bit clear in CPU data → 0x00 in plane 2"
    );
}

#[test]
fn test_chain4_write_mode_3_set_reset_still_runs_pipeline() {
    // Chain-4 + write_mode 3: set/reset values still apply per plane,
    // gated by effective_mask = (cpu_data ROR rotate) & bit_mask.
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.plane_mask = 0x0F;
    mem.vga.write_mode = 3;
    mem.vga.bit_mask = 0xFF;
    mem.vga.set_reset = 0x05; // planes 0 and 2 expand to 0xFF (bits 0,2 set)
    mem.vga.enable_set_reset = 0; // unused in write_mode 3 (set_reset always applies)
    mem.vga.data_rotate = 0;
    mem.update_vga_write_handler();

    // Pre-load plane 0 byte with a sentinel.
    mem.plane_mut(0)[0] = 0x00;
    let _ = mem.read_u8(VIDEO_BASE); // load latches

    // Write to address 0 → plane 0. effective_mask = cpu_data & 0xFF = 0xFF.
    // Plane 0 set/reset = 1 → result = 0xFF.
    mem.write_u8(VIDEO_BASE, 0xFF);
    assert_eq!(
        mem.plane(0)[0],
        0xFF,
        "chain-4 + write_mode 3: plane 0 set/reset bit set → 0xFF"
    );

    // Plane 1 not addressed → unchanged.
    assert_eq!(mem.plane(1)[0], 0x00, "plane 1 untouched");
}

#[test]
fn test_chain4_read_loads_latches_and_returns_plane() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.read_mode = 0;

    // CPU offset 22 selects plane 2 and maps to plane cell 22&~3=20.
    let plane_cell = 20usize;
    mem.plane_mut(0)[plane_cell] = 0x10;
    mem.plane_mut(1)[plane_cell] = 0x20;
    mem.plane_mut(2)[plane_cell] = 0x30;
    mem.plane_mut(3)[plane_cell] = 0x40;

    let addr = VIDEO_BASE + plane_cell + 2;
    let val = mem.read_u8(addr);
    assert_eq!(val, 0x30);
    assert_eq!(mem.get_latches(), [0x10, 0x20, 0x30, 0x40]);
}

#[test]
fn test_chain4_map_select_a000_64k_does_not_alias_b800() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.plane_mask = 0x0F;
    mem.vga.write_mode = 0;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.vga.set_reset = 0;
    mem.vga.data_rotate = 0;
    mem.vga.read_mode = 0;
    mem.set_vga_memory_map_select(1); // A0000-AFFFF

    let b800 = 0xB8000usize;
    mem.write_u8(b800, 0xA5);
    assert_eq!(mem.ram[b800], 0xA5);
    assert_eq!(mem.read_u8(b800), 0xA5);

    let chain4_cell = (b800 - VIDEO_BASE) & !3;
    assert!(chain4_cell >= PLANE_SIZE);
    assert!(mem.planes_flat.iter().all(|&byte| byte == 0));
}

#[test]
fn test_chain4_map_select_b800_32k_routes_through_planes() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.plane_mask = 0x0F;
    mem.vga.write_mode = 0;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.vga.set_reset = 0;
    mem.vga.data_rotate = 0;
    mem.vga.read_mode = 0;
    mem.update_vga_write_handler();
    mem.set_vga_memory_map_select(3); // B8000-BFFFF

    mem.write_u8(0xB8000, 0xCC);
    assert_eq!(mem.plane(0)[0], 0xCC);
    assert_eq!(mem.read_u8(0xB8000), 0xCC);

    // A0000 is outside the selected map and should not hit chain-4 logic.
    mem.write_u8(VIDEO_BASE, 0x11);
    assert_eq!(mem.ram[VIDEO_BASE], 0x11);
}

#[test]
fn test_load() {
    let mut mem = Memory::new();
    let data = [0x11, 0x22, 0x33, 0x44];
    mem.load(0x5000, &data).unwrap();

    assert_eq!(mem.read_u8(0x5000), 0x11);
    assert_eq!(mem.read_u8(0x5001), 0x22);
    assert_eq!(mem.read_u8(0x5002), 0x33);
    assert_eq!(mem.read_u8(0x5003), 0x44);
}

#[test]
fn test_load_large_data() {
    let mut mem = Memory::new();
    let data = vec![0xAB; 1000];

    // Load in valid area
    let result = mem.load(0x10000, &data);
    assert!(result.is_ok());
    assert_eq!(mem.read_u8(0x10000), 0xAB);
    assert_eq!(mem.read_u8(0x103E7), 0xAB);
}

#[test]
fn test_ram_size() {
    assert_eq!(DEFAULT_RAM_SIZE, 128 * 1024 * 1024);
    // MAX_RAM_SIZE must cover the flat-memory identity map's full reach
    // (VA ceiling 0x2E00_0000 + offset 0x00FF_0000 < 752 MiB), plus the
    // 16 MiB identity thunk window ending at 768 MiB, while staying below
    // the MMIO placement floor (0x6000_0000).
    assert_eq!(MAX_RAM_SIZE, 768 * 1024 * 1024);
    assert_eq!(DOS_RAM_CEILING, 256 * 1024 * 1024);
    let mem = Memory::new();
    assert_eq!(mem.ram_size(), DEFAULT_RAM_SIZE);
    // Only the DOS-era ceiling is reserved up front; the flat-memory band above
    // it is committed lazily by expand_ram.
    assert!(mem.ram.capacity() >= DOS_RAM_CEILING + FETCH_PADDING);
}

#[test]
fn test_ensure_ram_size() {
    let mut mem = Memory::new();
    let original = mem.ram_size();
    assert_eq!(original, DEFAULT_RAM_SIZE);

    // Smaller than current - no change (natural no-op with maximum RAM pre-allocated)
    mem.ensure_ram_size(1000);
    assert_eq!(mem.ram_size(), original);

    // Equal to current - no change
    mem.ensure_ram_size(DEFAULT_RAM_SIZE);
    assert_eq!(mem.ram_size(), original);
}

#[test]
fn test_ensure_ram_size_cap_path_without_resize() {
    let mut mem = Memory::new();
    assert_eq!(mem.ram_size(), DEFAULT_RAM_SIZE);
    mem.ensure_ram_size(DOS_RAM_CEILING + 1);
    assert_eq!(mem.ram_size(), DOS_RAM_CEILING);
}

#[test]
fn test_ensure_ram_size_flushes_tlb_when_growing_within_pinned_capacity() {
    let mut mem = Memory::new();
    assert_eq!(mem.ram_size(), DEFAULT_RAM_SIZE);

    // Populate a TLB entry pointing into current RAM
    let page: u32 = 0x10; // page 0x10 = physical address 0x10000
    mem.tlb_insert(page, page, true, true, false);
    assert!(
        mem.tlb_lookup(page).is_some(),
        "TLB entry should exist before ensure_ram_size"
    );

    // Growing to the ceiling changes the usable length but not the allocation.
    mem.ensure_ram_size(DOS_RAM_CEILING);
    assert_eq!(mem.ram_size(), DOS_RAM_CEILING);

    // The backing allocation is stable, but the newly valid physical range
    // changes translation policy and therefore flushes cached entries.
    assert!(
        mem.tlb_lookup(page).is_none(),
        "TLB entry should be flushed when usable RAM grows"
    );
}

#[test]
fn test_slice() {
    let mut mem = Memory::new();
    mem.write_u8(0x100, 0xAA);
    mem.write_u8(0x101, 0xBB);
    mem.write_u8(0x102, 0xCC);

    let slice = mem.slice(0x100, 3);
    assert_eq!(slice, &[0xAA, 0xBB, 0xCC]);
}

#[test]
fn test_slice_mut() {
    let mut mem = Memory::new();
    {
        let mut slice = mem.slice_mut(0x200, 3);
        slice[0] = 0x11;
        slice[1] = 0x22;
        slice[2] = 0x33;
    }

    assert_eq!(mem.read_u8(0x200), 0x11);
    assert_eq!(mem.read_u8(0x201), 0x22);
    assert_eq!(mem.read_u8(0x202), 0x33);
}

#[test]
fn test_slice_works() {
    let mut mem = Memory::new();
    mem.write_u8(0x3000, 0x12);
    mem.write_u8(0x3001, 0x34);

    let slice = mem.slice(0x3000, 2);
    assert_eq!(slice.len(), 2);
    assert_eq!(slice[0], 0x12);
    assert_eq!(slice[1], 0x34);
}

#[test]
fn test_bda_initialization() {
    let mem = Memory::new();

    // Check BDA values are initialized
    assert_eq!(mem.read_u8(0x410), 0x21); // Equipment list
    assert_eq!(mem.read_u8(0x449), 0x03); // Video mode
    assert_eq!(mem.read_u8(0x44A), 80); // Columns
    assert_eq!(mem.read_u8(0x484), 24); // Rows - 1

    // Keyboard buffer pointers
    assert_eq!(mem.read_u16(0x41A), 0x1E);
    assert_eq!(mem.read_u16(0x41C), 0x1E);
}

#[test]
fn test_iret_stub() {
    let mem = Memory::new();
    let iret_addr = Memory::seg_offset_to_linear(0xF000, 0xFF00);
    assert_eq!(mem.read_u8(iret_addr), 0xCF); // IRET opcode
}

#[test]
fn test_write_video_planar_out_of_bounds() {
    let mut mem = Memory::new();
    // Writing to offset that would go beyond video memory
    // Should not panic
    mem.write_video_planar(PLANE_SIZE, 0xFF, 0x0F);
}

#[test]
fn test_write_video_planar_tracked_out_of_bounds() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x1234, 0x5678);
    mem.write_video_planar_tracked(PLANE_SIZE * 4, 0xFF, 0x0F);
    assert!(mem.drain_pending_writes().is_empty());
}

#[test]
fn test_sync_chain4_to_planes() {
    let mut mem = Memory::new();
    for offset in 0..VIDEO_SIZE {
        mem.ram[VIDEO_BASE + offset] = (offset & 0xFF) as u8;
    }

    mem.sync_chain4_to_planes();
    // Chain-4 mapping maps byte x to plane x&3, cell x&~3. The
    // chain-4 aperture therefore resynchronizes only its first 64 KiB.
    for cell in (0..PLANE_SIZE).step_by(4) {
        for plane in 0..4 {
            assert_eq!(
                mem.planes_flat[plane * PLANE_SIZE + cell],
                mem.ram[VIDEO_BASE + cell + plane]
            );
        }
    }
}

#[test]
fn test_sync_planes_to_chain4() {
    let mut mem = Memory::new();
    mem.ram[VIDEO_BASE + PLANE_SIZE..VIDEO_BASE + VIDEO_SIZE].fill(0xA5);
    for cell in (0..PLANE_SIZE).step_by(4) {
        for plane in 0..4 {
            mem.planes_flat[plane * PLANE_SIZE + cell] = ((cell + plane) & 0xFF) as u8;
        }
    }

    mem.sync_planes_to_chain4();
    // The inverse sync mirrors each aligned cell and its four plane bytes
    // into the first 64 KiB; the other half of the 128 KiB aperture is raw RAM.
    for cell in (0..PLANE_SIZE).step_by(4) {
        for plane in 0..4 {
            assert_eq!(
                mem.ram[VIDEO_BASE + cell + plane],
                mem.planes_flat[plane * PLANE_SIZE + cell]
            );
        }
    }
    assert!(mem.ram[VIDEO_BASE + PLANE_SIZE..VIDEO_BASE + VIDEO_SIZE]
        .iter()
        .all(|&byte| byte == 0xA5));
}

/// The direct-read limit tracks the backing length through construction,
/// shrink and growth. Resize republishes any temporary restriction.
#[test]
fn raw_ram_limit_tracks_ram_backing_length() {
    let mut mem = Memory::new();
    assert_eq!(mem.raw_ram_limit(), mem.ram.len());
    assert_eq!(mem.raw_ram_limit(), DEFAULT_RAM_SIZE + FETCH_PADDING);
    assert_eq!(
        mem.raw_ram_limit(),
        mem.core.raw_ram_limit(),
        "Memory reports the core's word"
    );

    mem.resize_ram(0x10_0000);
    assert_eq!(mem.raw_ram_limit(), 0x10_0000 + FETCH_PADDING);

    mem.resize_ram(DOS_RAM_CEILING);
    assert_eq!(mem.raw_ram_limit(), DOS_RAM_CEILING + FETCH_PADDING);

    mem.restrict_raw_ram_limit(0);
    assert_eq!(mem.raw_ram_limit(), 0);
    mem.restrict_raw_ram_limit(usize::MAX);
    assert_eq!(mem.raw_ram_limit(), 0, "restricting never raises");

    // Over-capacity growth re-pins the backing; the limit follows it.
    mem.resize_ram(DOS_RAM_CEILING + 0x10_0000);
    assert_eq!(mem.raw_ram_limit(), mem.ram.len());
    assert_eq!(
        mem.raw_ram_limit(),
        DOS_RAM_CEILING + 0x10_0000 + FETCH_PADDING
    );
}

/// LLD E4: on a shrink, `resize_ram` delivers the lowered direct-read limit
/// BEFORE the direct-policy epoch bump announces the resize, so nothing that
/// observes the new epoch can still read past the new end. On growth the
/// bump still sees the old (smaller) limit; it rises only once the backing
/// exists.
#[test]
fn raw_ram_limit_is_lowered_before_the_direct_epoch_bump() {
    use crate::direct_proof::RAW_RAM_LIMIT_AT_DIRECT_EPOCH_BUMP as SEEN;
    let mut mem = Memory::new();
    let old = mem.raw_ram_limit();
    let epoch = mem.direct_memory_policy_epoch();

    SEEN.with(|seen| seen.set(None));
    mem.resize_ram(0x10_0000);
    assert_ne!(mem.direct_memory_policy_epoch(), epoch, "the shrink bumps");
    let shrunk = 0x10_0000 + FETCH_PADDING;
    assert!(shrunk < old);
    assert_eq!(SEEN.with(|seen| seen.get()), Some(shrunk));

    SEEN.with(|seen| seen.set(None));
    mem.resize_ram(0x20_0000);
    assert_eq!(SEEN.with(|seen| seen.get()), Some(shrunk));
    assert_eq!(mem.raw_ram_limit(), 0x20_0000 + FETCH_PADDING);
}
