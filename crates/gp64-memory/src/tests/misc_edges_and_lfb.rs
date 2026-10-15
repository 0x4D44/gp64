use super::*;

// ========== TLB Invalidation Tests ==========

#[test]
fn test_invalidate_tlb() {
    let mut mem = Memory::new();

    // Can't directly test TLB without paging, but ensure method doesn't panic
    mem.invalidate_tlb();
}

#[test]
fn test_invalidate_tlb_page() {
    let mut mem = Memory::new();

    // Ensure method doesn't panic
    mem.invalidate_tlb_page(0x1000);
    mem.invalidate_tlb_page(0xFFFFF000);
}

// ========== read_u32_physical Tests ==========

#[test]
fn test_read_u32_physical() {
    let mut mem = Memory::new();

    mem.write_u32(0x5000, 0x12345678);
    // read_u32_physical is private, but we can test it indirectly through paging
    // For now, test the public interface
    assert_eq!(mem.read_u32(0x5000), 0x12345678);
}

// Note: Paging tests (test_paging_*, test_translate_address_*) have been moved to
// the main crate alongside the translate methods they test. These tests depend on
// CPU types (Cpu, CpuError, PageFaultInfo) which are not available in this crate.

// ========== Load Error Tests ==========

#[test]
fn test_load_out_of_bounds() {
    let mut mem = Memory::new();
    let data = vec![0xAB; 1000];

    // Try to load beyond RAM size (with A20 masking)
    mem.set_a20(true);
    let ram_size = mem.ram_size();
    let result = mem.load(ram_size, &data);

    assert!(result.is_err());
}

// ========== Read Bytes Edge Cases ==========

#[test]
fn test_read_bytes_partial_past_end() {
    let mut mem = Memory::new();
    mem.set_a20(true);

    // Write near end
    let near_end = mem.ram_size() - 2;
    mem.write_u8(near_end, 0xAA);
    mem.write_u8(near_end + 1, 0xBB);

    let mut buf = [0u8; 4];
    mem.read_bytes(near_end, &mut buf);

    // First two bytes from memory, rest filled with 0xFF
    assert_eq!(buf[0], 0xAA);
    assert_eq!(buf[1], 0xBB);
    assert_eq!(buf[2], 0xFF);
    assert_eq!(buf[3], 0xFF);
}

// ========== Non-Planar Video Memory Tests ==========

#[test]
fn test_non_planar_video_write() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13; // Non-planar Mode 13h

    // Write should go directly to RAM
    mem.write_u8(VIDEO_BASE, 0xAB);
    assert_eq!(mem.ram[VIDEO_BASE], 0xAB);
}

#[test]
fn test_non_planar_video_read() {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.update_vga_write_handler();

    mem.write_u8(VIDEO_BASE, 0xCD);
    assert_eq!(mem.read_u8(VIDEO_BASE), 0xCD);
}

// ========== Memory Error Display ==========

#[test]
fn test_memory_error_display() {
    let err1 = MemoryError::OutOfBounds(0x12345);
    assert!(err1.to_string().contains("0x12345"));

    let err2 = MemoryError::Unaligned(0xABCDE);
    assert!(err2.to_string().contains("0xABCDE"));
}

// ========== VBE 2.0 Linear Framebuffer Tests ==========

#[test]
fn test_lfb_init_and_enabled() {
    let mut mem = Memory::new();

    // Initially LFB is disabled and empty
    assert!(!mem.is_lfb_enabled());
    assert_eq!(mem.lfb().len(), 0);

    // Initialize LFB
    mem.init_lfb(640 * 480); // 640x480 8bpp
    assert_eq!(mem.lfb().len(), 640 * 480);

    // Enable LFB
    mem.set_lfb_enabled(true);
    assert!(mem.is_lfb_enabled());
}

#[test]
fn test_lfb_init_same_size_no_resize() {
    let mut mem = Memory::new();
    mem.init_lfb(32);
    mem.lfb_mut()[3] = 0xAA;
    mem.init_lfb(32);
    assert_eq!(mem.lfb().len(), 32);
    assert_eq!(mem.lfb()[3], 0xAA);
}

#[test]
fn test_lfb_read_write() {
    let mut mem = Memory::new();
    mem.init_lfb(1024);
    mem.set_lfb_enabled(true);

    // Test basic read/write
    mem.lfb_write_u8(0, 0xAB);
    assert_eq!(mem.lfb_read_u8(0), 0xAB);

    mem.lfb_write_u8(500, 0xCD);
    assert_eq!(mem.lfb_read_u8(500), 0xCD);
}

#[test]
fn test_lfb_write_u8_increments_video_write_count() {
    let mut mem = Memory::new();
    mem.init_lfb(1024);

    let before = mem.video_write_count();
    mem.lfb_write_u8(0, 0xAB);
    assert!(mem.video_write_count() > before);

    let after_in_bounds = mem.video_write_count();
    mem.lfb_write_u8(1024, 0xCD);
    assert_eq!(mem.video_write_count(), after_in_bounds);
}

#[test]
fn test_lfb_dirty_pages_track_canonical_writes() {
    let mut mem = Memory::new();
    mem.init_lfb(LFB_DIRTY_PAGE_SIZE * 3);
    mem.set_lfb_enabled(true);

    assert!(!mem.has_dirty_lfb_pages());
    mem.write_u8(LFB_BASE + 50, 0x77);
    mem.write_u16(LFB_BASE + LFB_DIRTY_PAGE_SIZE + 10, 0x1234);
    mem.write_u32(LFB_BASE + (LFB_DIRTY_PAGE_SIZE * 2) - 2, 0xDEADBEEF);
    mem.lfb_write_u8((LFB_DIRTY_PAGE_SIZE * 2) + 8, 0x99);

    assert!(mem.has_dirty_lfb_pages());
    let pages = mem.take_lfb_dirty_pages();
    let page_dirty = |page: usize| pages[page / 64] & (1u64 << (page % 64)) != 0;
    assert!(page_dirty(0));
    assert!(page_dirty(1));
    assert!(page_dirty(2));
    assert!(!mem.has_dirty_lfb_pages());
}

#[test]
fn test_lfb_out_of_bounds() {
    let mut mem = Memory::new();
    mem.init_lfb(100);

    // Read beyond buffer should return 0
    assert_eq!(mem.lfb_read_u8(100), 0);
    assert_eq!(mem.lfb_read_u8(200), 0);

    // Write beyond buffer should be silently ignored (no panic)
    mem.lfb_write_u8(100, 0xFF);
    mem.lfb_write_u8(200, 0xFF);
}

#[test]
fn test_lfb_memory_access_via_high_address() {
    let mut mem = Memory::new();
    mem.init_lfb(4096);
    mem.set_lfb_enabled(true);

    // Write to gp64's reported LFB aperture.
    mem.write_u8(LFB_BASE, 0x42);
    mem.write_u8(LFB_BASE + 100, 0x88);

    // Read back via LFB direct access
    assert_eq!(mem.lfb_read_u8(0), 0x42);
    assert_eq!(mem.lfb_read_u8(100), 0x88);

    // Read back via memory address
    assert_eq!(mem.read_u8(LFB_BASE), 0x42);
    assert_eq!(mem.read_u8(LFB_BASE + 100), 0x88);
}

#[test]
fn test_lfb_bochs_compat_alias_byte_access() {
    let mut mem = Memory::new();
    mem.init_lfb(4096);
    mem.set_lfb_enabled(true);

    assert!(mem.is_lfb_address(LFB_BOCHS_COMPAT_BASE));
    assert!(mem.is_lfb_address(LFB_BOCHS_COMPAT_BASE + 4095));
    assert!(!mem.is_lfb_address(LFB_BOCHS_COMPAT_BASE + 4096));

    let write_count_before = mem.video_write_count();
    mem.write_u8(LFB_BOCHS_COMPAT_BASE + 42, 0x7B);
    assert!(mem.video_write_count() > write_count_before);
    assert_eq!(mem.lfb_read_u8(42), 0x7B);
    assert_eq!(mem.read_u8(LFB_BASE + 42), 0x7B);

    mem.write_u8(LFB_BASE + 43, 0xA5);
    assert_eq!(mem.read_u8(LFB_BOCHS_COMPAT_BASE + 43), 0xA5);
}

#[test]
fn test_lfb_bochs_compat_alias_multi_byte_access() {
    let mut mem = Memory::new();
    mem.init_lfb(4096);
    mem.set_lfb_enabled(true);

    mem.write_u16(LFB_BOCHS_COMPAT_BASE + 100, 0xABCD);
    assert_eq!(mem.lfb_read_u8(100), 0xCD);
    assert_eq!(mem.lfb_read_u8(101), 0xAB);
    assert_eq!(mem.read_u16(LFB_BASE + 100), 0xABCD);

    mem.write_u32(LFB_BOCHS_COMPAT_BASE + 200, 0xDEADBEEF);
    assert_eq!(mem.read_u32(LFB_BASE + 200), 0xDEADBEEF);
}

#[test]
fn test_lfb_bochs_compat_alias_bounds_and_disabled() {
    let mut mem = Memory::new();
    mem.init_lfb(16);
    mem.set_lfb_enabled(true);

    mem.write_u8(LFB_BOCHS_COMPAT_BASE + 15, 0xAA);
    assert_eq!(mem.lfb_read_u8(15), 0xAA);
    mem.write_u8(LFB_BOCHS_COMPAT_BASE + 16, 0x55);
    assert_eq!(mem.lfb_read_u8(0), 0);

    mem.write_u16(LFB_BOCHS_COMPAT_BASE + 15, 0x1234);
    assert_eq!(
        mem.lfb_read_u8(15),
        0x34,
        "the in-aperture byte of a straddling word must commit"
    );

    mem.set_lfb_enabled(false);
    mem.write_u8(LFB_BOCHS_COMPAT_BASE, 0x99);
    assert_eq!(mem.lfb_read_u8(0), 0);
    assert_eq!(mem.read_u8(LFB_BOCHS_COMPAT_BASE), 0xFF);
}

/// An enabled device window overlapping system RAM must not shadow DRAM.
/// Descriptor reads must continue to see RAM through the overlap.
#[test]
fn s3_lfb_window_over_system_ram_never_shadows_dram() {
    const RAM: usize = 8 * 1024 * 1024;
    const IDT_GATE: usize = 0x0030_0040;
    const CONVENTIONAL: usize = 0x0004_0040;

    let mut mem = Memory::new();
    mem.resize_ram(RAM);
    mem.init_lfb(4 * 1024 * 1024);
    // Construct a synthetic interrupt gate to check DRAM routing through the overlap.
    let handler = 0x1234_5678u32.to_le_bytes();
    let gate = [
        handler[0], handler[1], 0x08, 0, 0, 0x8E, handler[2], handler[3],
    ];
    mem.ram[IDT_GATE..IDT_GATE + 8].copy_from_slice(&gate);
    mem.ram[CONVENTIONAL] = 0x55;

    mem.configure_lfb_decoder(LfbDecoderConfig::S3 {
        base: 0,
        size: 4 * 1024 * 1024,
        vram_offset: 0,
        enabled: true,
    });

    assert_eq!(
        mem.read_u8(IDT_GATE + 5),
        0x8E,
        "IDT gate must read from DRAM"
    );
    assert_eq!(mem.read_u32(IDT_GATE), 0x0008_5678);
    assert_eq!(
        mem.read_u8(CONVENTIONAL),
        0x55,
        "conventional RAM must read"
    );
    assert!(!mem.is_lfb_address(IDT_GATE));
    assert!(!mem.is_lfb_address(CONVENTIONAL));

    mem.write_u32(IDT_GATE, 0x1234_5678);
    mem.write_u8(CONVENTIONAL, 0x66);
    assert_eq!(mem.ram[IDT_GATE..IDT_GATE + 4], [0x78, 0x56, 0x34, 0x12]);
    assert_eq!(mem.ram[CONVENTIONAL], 0x66);
    assert_eq!(
        mem.lfb_read_u8(IDT_GATE),
        0,
        "a DRAM store must not reach VRAM"
    );
    assert_eq!(mem.lfb_read_u8(CONVENTIONAL), 0);

    // The legacy hole is not DRAM, so the window still claims it.
    assert!(mem.is_lfb_address(VIDEO_BASE));
    assert!(mem.is_lfb_address(0x000F_FFFF));
    assert!(!mem.is_lfb_address(LEGACY_HOLE_END));
}

#[test]
fn s3_lfb_decoder_relocates_and_removes_dispi_aliases() {
    const FIRST_BASE: usize = 0x0200_0000;
    const SECOND_BASE: usize = 0x0300_0000;

    let mut mem = Memory::new();
    // Both windows sit on the bus above installed RAM.
    mem.resize_ram(FIRST_BASE);
    mem.init_lfb(0x4000);

    mem.configure_lfb_decoder(LfbDecoderConfig::S3 {
        base: FIRST_BASE,
        size: 0x2000,
        vram_offset: 0,
        enabled: true,
    });
    mem.write_u32(FIRST_BASE + 0x100, 0xDEAD_BEEF);

    assert_eq!(mem.read_u32(FIRST_BASE + 0x100), 0xDEAD_BEEF);
    assert_eq!(
        mem.lfb_decoder(),
        LfbDecoderConfig::S3 {
            base: FIRST_BASE,
            size: 0x2000,
            vram_offset: 0,
            enabled: true,
        }
    );
    // The power-on aperture sits above every address the guest can be
    // given as RAM, so once S3 relocates the decoder nothing backs it.
    assert_eq!(mem.read_u8(LFB_BASE), 0xFF, "the DISPI base must be stale");
    assert_eq!(mem.read_u8(LFB_BOCHS_COMPAT_BASE), 0xFF);
    assert!(!mem.is_lfb_address(FIRST_BASE + 0x2000));

    mem.configure_lfb_decoder(LfbDecoderConfig::S3 {
        base: SECOND_BASE,
        size: 0x2000,
        vram_offset: 0,
        enabled: true,
    });
    assert_eq!(
        mem.read_u8(FIRST_BASE),
        0xFF,
        "the old S3 base must be stale"
    );
    assert_eq!(mem.read_u32(SECOND_BASE + 0x100), 0xDEAD_BEEF);

    mem.set_lfb_enabled(false);
    assert_eq!(mem.read_u8(SECOND_BASE), 0xFF);
    assert!(!mem.is_lfb_address(SECOND_BASE));
}

/// The bulk span path in `write_bytes_guest` must be a pure optimisation:
/// for any span it accepts, guest-visible RAM has to end up exactly as the
/// byte-by-byte path would leave it.
#[test]
fn write_bytes_guest_bulk_span_matches_the_byte_by_byte_path() {
    const BASE: usize = 0x0002_1003; // Synthetic unaligned low-RAM transfer span. // Unaligned transfer destination
    let data: Vec<u8> = (0..512u32).map(|i| (i * 7 + 3) as u8).collect();

    for a20 in [true, false] {
        let mut bulk = Memory::new();
        let mut per_byte = Memory::new();
        bulk.set_a20(a20);
        per_byte.set_a20(a20);

        // A20 only clears bit 20, so a span below 0xA0000 is eligible
        // whatever the gate says.
        assert!(
            bulk.bulk_write_span_is_plain_ram(BASE, data.len()),
            "plain low-RAM span should take the bulk path (a20 = {a20})"
        );

        bulk.write_bytes_guest(BASE, &data);
        for (i, byte) in data.iter().copied().enumerate() {
            per_byte.write_u8(BASE + i, byte);
        }

        let window = BASE - 4..BASE + data.len() + 4;
        assert_eq!(
            bulk.ram[window.clone()],
            per_byte.ram[window],
            "bulk span must match byte-by-byte (a20 = {a20})"
        );
    }
}

/// Spans the bulk path must refuse, because something in them decodes or
/// is observed per byte.
#[test]
fn write_bytes_guest_bulk_span_refuses_non_plain_ram() {
    let mut mem = Memory::new();

    assert!(!mem.bulk_write_span_is_plain_ram(0x1000, 0), "empty span");
    assert!(
        !mem.bulk_write_span_is_plain_ram(VIDEO_BASE - 4, 8),
        "span reaching into the VGA window"
    );
    assert!(
        !mem.bulk_write_span_is_plain_ram(VIDEO_BASE, 16),
        "span inside the VGA window"
    );
    assert!(
        !mem.bulk_write_span_is_plain_ram(usize::MAX - 4, 16),
        "span whose end overflows"
    );

    mem.enable_write_tracking();
    assert!(
        !mem.bulk_write_span_is_plain_ram(0x1000, 16),
        "Time Machine tracking observes every write"
    );
}

/// A span straddling the VGA window must still commit its RAM bytes and
/// still reach the video path — i.e. it stays on the per-byte path.
#[test]
fn write_bytes_guest_straddling_the_vga_window_still_commits_both_sides() {
    let mut mem = Memory::new();
    let start = VIDEO_BASE - 4;
    mem.write_bytes_guest(start, &[0xA1, 0xA2, 0xA3, 0xA4, 0xB1, 0xB2]);

    for (i, expected) in [0xA1u8, 0xA2, 0xA3, 0xA4].into_iter().enumerate() {
        assert_eq!(mem.read_u8(start + i), expected, "RAM byte {i}");
    }
    assert_eq!(mem.read_u8(VIDEO_BASE), 0xB1, "first VGA byte");
    assert_eq!(mem.read_u8(VIDEO_BASE + 1), 0xB2, "second VGA byte");
}

/// Time Machine still sees a tracked write for every byte of a span.
#[test]
fn write_bytes_guest_records_tracked_writes_when_tracking_is_on() {
    let mut mem = Memory::new();
    mem.enable_write_tracking();
    mem.set_write_context(0x1234, 0x5678);

    mem.write_bytes_guest(0x2000, &[1, 2, 3, 4]);

    assert_eq!(
        mem.drain_pending_writes().len(),
        4,
        "each byte of a tracked span must be recorded"
    );
}

#[test]
fn s3_lfb_decoder_bulk_access_observes_aperture_and_size() {
    const BASE: usize = 0x0200_0000;
    let mut mem = Memory::new();
    mem.resize_ram(BASE);
    mem.init_lfb(0x4000);
    mem.configure_lfb_decoder(LfbDecoderConfig::S3 {
        base: BASE,
        size: 0x2000,
        vram_offset: 0,
        enabled: true,
    });

    let bytes = [0x11, 0x22, 0x33, 0x44, 0x55];
    mem.write_bytes_guest(BASE + 0x0FFE, &bytes);
    let mut readback = [0; 5];
    mem.read_bytes(BASE + 0x0FFE, &mut readback);
    assert_eq!(readback, bytes);
    assert_eq!(&mem.lfb()[0x0FFE..0x1003], &bytes);

    assert_eq!(
        mem.read_u8(BASE + 0x2000),
        0xFF,
        "past the window is open bus"
    );
    assert!(!mem.bulk_read_ram(BASE, 4, &mut [0; 4]));
    assert!(!mem.bulk_copy_ram(0x1000, BASE, 4));
}

/// MDD-BUG-CRU-01489: a bulk copy onto the descriptor-snoop hull is refused
/// so the per-byte fallback runs the GDT snoop; one beside it is not.
#[test]
fn bulk_copy_refuses_a_destination_on_the_descriptor_tables() {
    let mut mem = Memory::new();
    mem.set_gdt_snoop_range(0x0011_0000, 0xFF);
    assert!(!mem.bulk_copy_ram(0x1000, 0x0011_0008, 8));
    assert!(!mem.bulk_copy_ram(0x1000, 0x0010_FFFC, 8));
    assert!(mem.bulk_copy_ram(0x1000, 0x0012_0000, 8));
    assert!(mem.bulk_copy_ram(0x0011_0008, 0x0012_0000, 8));
}

/// MDD-BUG-CRU-01492: with A20 gated off, a write and a read of the same
/// aperture address (bit 20 set) reach the same framebuffer bytes.
#[test]
fn lfb_writes_and_reads_agree_with_a20_off() {
    let mut mem = Memory::new();
    mem.init_lfb(LFB_MAX_SIZE);
    mem.set_lfb_enabled(true);
    mem.set_a20(false);
    let addr = LFB_BASE + 0x10_0010;
    mem.write_u8(addr, 0x5A);
    mem.write_u16(addr + 2, 0x1234);
    mem.write_u32(addr + 4, 0xDEAD_BEEF);
    assert_eq!(mem.read_u8(addr), 0x5A);
    assert_eq!(mem.read_u16(addr + 2), 0x1234);
    assert_eq!(mem.read_u32(addr + 4), 0xDEAD_BEEF);
    assert_eq!(mem.lfb()[0x10], 0x5A, "the masked offset holds the byte");
}

#[test]
fn s3_lfb_decoder_maps_a_banked_window_without_touching_ram() {
    const BASE: usize = 0x000A_0000;
    const VRAM_OFFSET: usize = 0x0031_0000;
    let mut mem = Memory::new();
    mem.init_lfb(LFB_MAX_SIZE);
    mem.ram[BASE + 0xFC00..BASE + 0xFC04].copy_from_slice(&[0xA1, 0xB2, 0xC3, 0xD4]);
    mem.configure_lfb_decoder(LfbDecoderConfig::S3 {
        base: BASE,
        size: 0x1_0000,
        vram_offset: VRAM_OFFSET,
        enabled: true,
    });

    mem.write_u32(BASE + 0xFC00, 0x4433_2211);

    assert_eq!(
        &mem.lfb()[VRAM_OFFSET + 0xFC00..VRAM_OFFSET + 0xFC04],
        &[0x11, 0x22, 0x33, 0x44]
    );
    assert_eq!(
        &mem.ram[BASE + 0xFC00..BASE + 0xFC04],
        &[0xA1, 0xB2, 0xC3, 0xD4],
        "the device aperture must not alias ordinary guest RAM"
    );
}

#[test]
fn dispi_lfb_aperture_edge_wide_writes_decode_each_byte() {
    let mut mem = Memory::new();
    mem.init_lfb(8);
    mem.set_lfb_enabled(true);

    mem.write_u16(LFB_BASE - 1, 0x2211);
    assert_eq!(mem.lfb()[0], 0x22, "word crossing the low edge");

    mem.write_u16(LFB_BASE + 7, 0x4433);
    assert_eq!(mem.lfb()[7], 0x33, "word crossing the high edge");

    mem.write_u32(LFB_BASE - 2, 0x8877_6655);
    assert_eq!(&mem.lfb()[..2], &[0x77, 0x88], "dword crossing low edge");

    mem.write_u32(LFB_BASE + 6, 0xCCBB_AA99);
    assert_eq!(&mem.lfb()[6..], &[0x99, 0xAA], "dword crossing high edge");
}

#[test]
fn s3_banked_lfb_aperture_edge_wide_writes_decode_each_byte() {
    const BASE: usize = 0x000A_0000;
    const SIZE: usize = 8;
    const VRAM_OFFSET: usize = 0x0031_0000;

    let mut mem = Memory::new();
    mem.init_lfb(LFB_MAX_SIZE);
    mem.configure_lfb_decoder(LfbDecoderConfig::S3 {
        base: BASE,
        size: SIZE,
        vram_offset: VRAM_OFFSET,
        enabled: true,
    });

    mem.write_u16(BASE - 1, 0x2211);
    assert_eq!(mem.lfb()[VRAM_OFFSET], 0x22, "word crossing the low edge");

    mem.write_u16(BASE + SIZE - 1, 0x4433);
    assert_eq!(
        mem.lfb()[VRAM_OFFSET + SIZE - 1],
        0x33,
        "word crossing the high edge"
    );

    mem.write_u32(BASE - 2, 0x8877_6655);
    assert_eq!(
        &mem.lfb()[VRAM_OFFSET..VRAM_OFFSET + 2],
        &[0x77, 0x88],
        "dword crossing the low edge"
    );

    mem.write_u32(BASE + SIZE - 2, 0xCCBB_AA99);
    assert_eq!(
        &mem.lfb()[VRAM_OFFSET + SIZE - 2..VRAM_OFFSET + SIZE],
        &[0x99, 0xAA],
        "dword crossing the high edge"
    );
}

#[test]
fn original_trio64_lfb_decodes_the_programmed_window_and_wraps_fitted_vram() {
    const BASE: usize = 0x0200_0000;
    let mut mem = Memory::new();
    mem.resize_ram(BASE);
    mem.init_lfb(1024 * 1024);
    mem.configure_lfb_decoder(LfbDecoderConfig::S3 {
        base: BASE,
        size: 4 * 1024 * 1024,
        vram_offset: 0,
        enabled: true,
    });

    mem.write_u8(BASE + 0x000123, 0x11);
    mem.write_u8(BASE + 0x100123, 0x22);
    mem.write_u8(BASE + 0x300123, 0x33);
    assert_eq!(mem.lfb()[0x123], 0x33);
    assert_eq!(mem.read_u8(BASE + 0x000123), 0x33);
    assert_eq!(mem.read_u8(BASE + 0x100123), 0x33);
    assert_eq!(mem.read_u8(BASE + 0x300123), 0x33);
    assert!(mem.is_lfb_address(BASE + 4 * 1024 * 1024 - 1));
    assert!(!mem.is_lfb_address(BASE + 4 * 1024 * 1024));
}

#[test]
fn original_trio64_cr53_nibble_swap_is_independent_of_store_width() {
    const BASE: usize = 0x0200_0000;
    let mut mem = Memory::new();
    mem.resize_ram(BASE);
    mem.init_lfb(64);
    mem.configure_lfb_decoder(LfbDecoderConfig::S3 {
        base: BASE,
        size: 64,
        vram_offset: 0,
        enabled: true,
    });
    mem.configure_s3_legacy_aperture(S3LegacyApertureConfig {
        active: true,
        memory_response: true,
        a000_response: true,
        enhanced_mapping: false,
        top_memory_access: false,
        bank_offset: 0,
        linear_nibble_swap: true,
    });

    mem.write_u8(BASE, 0x12);
    mem.write_u8_jit(BASE + 1, 0x34);
    mem.write_u16(BASE + 2, 0x7856);
    mem.write_u32(BASE + 4, 0xF0DE_BC9A);
    mem.write_u64(BASE + 8, 0x8967_4523_01EF_CDAB);

    assert_eq!(
        &mem.lfb()[..16],
        &[
            0x21, 0x43, 0x65, 0x87, 0xA9, 0xCB, 0xED, 0x0F, 0xBA, 0xDC, 0xFE, 0x10, 0x32, 0x54,
            0x76, 0x98,
        ],
        "CR53.6 transforms every byte before canonical VRAM storage"
    );
    assert_eq!(mem.read_u8(BASE), 0x12);
    assert_eq!(mem.read_u8(BASE + 1), 0x34);
    assert_eq!(mem.read_u16(BASE + 2), 0x7856);
    assert_eq!(mem.read_u32(BASE + 4), 0xF0DE_BC9A);
    assert_eq!(mem.read_u64(BASE + 8), 0x8967_4523_01EF_CDAB);
}

#[test]
fn original_trio64_enhanced_mapping_exposes_only_the_a0000_window() {
    let mut mem = Memory::new();
    mem.init_lfb(1024 * 1024);
    mem.ram[TEXT_VIDEO_BASE_COLOR] = 0xA5;
    mem.configure_s3_legacy_aperture(S3LegacyApertureConfig {
        active: true,
        memory_response: true,
        a000_response: true,
        enhanced_mapping: true,
        top_memory_access: false,
        bank_offset: 0,
        linear_nibble_swap: false,
    });

    mem.write_u8(VIDEO_BASE, 0x5A);
    assert_eq!(mem.lfb()[0], 0x5A);
    mem.write_u16(VIDEO_BASE + 2, 0x2211);
    mem.write_u32(VIDEO_BASE + 4, 0x6655_4433);
    assert_eq!(
        &mem.lfb()[2..8],
        &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
        "wide CPU stores must retain the same enhanced-aperture routing as byte stores"
    );
    assert_eq!(mem.read_u8(TEXT_VIDEO_BASE_COLOR), 0xFF);
    mem.write_u8(TEXT_VIDEO_BASE_COLOR, 0x19);
    assert_eq!(
        mem.ram[TEXT_VIDEO_BASE_COLOR], 0xA5,
        "B8000 must not fall through to RAM or the generic VGA decoder"
    );
}

#[test]
fn original_trio64_top_memory_access_uses_a13_to_select_the_top_window() {
    let mut mem = Memory::new();
    mem.init_lfb(1024 * 1024);
    mem.configure_s3_legacy_aperture(S3LegacyApertureConfig {
        active: true,
        memory_response: true,
        a000_response: true,
        enhanced_mapping: false,
        top_memory_access: true,
        bank_offset: 0,
        linear_nibble_swap: false,
    });

    mem.write_u8(VIDEO_BASE + 0x0123, 0x32);
    mem.write_u8(VIDEO_BASE + 0x2123, 0x64);

    assert_eq!(mem.lfb()[1024 * 1024 - 0x8000 + 0x0123], 0x32);
    assert_eq!(mem.lfb()[1024 * 1024 - 0x10000 + 0x2123], 0x64);
    assert_eq!(mem.read_u8(VIDEO_BASE + 0x0123), 0x32);
    assert_eq!(mem.read_u8(VIDEO_BASE + 0x2123), 0x64);
}

#[test]
fn s3_mmio_decoder_stages_typed_reads_and_writes() {
    let mut mem = Memory::new();
    mem.configure_s3_mmio_decoder(true);

    mem.write_u32(S3_MMIO_BASE + 0x8100, 0x1234_5678);
    mem.write_u16(S3_MMIO_BASE + 0x0004, 0xA55A);
    assert_eq!(
        mem.pending_s3_mmio_writes(),
        &[
            S3MmioWrite {
                offset: 0x8100,
                width: S3MmioWidth::Dword,
                value: 0x1234_5678,
            },
            S3MmioWrite {
                offset: 0x0004,
                width: S3MmioWidth::Word,
                value: 0x0000_A55A,
            },
        ]
    );

    mem.publish_s3_mmio_read_u16(0x42E8, 0xA55A);
    mem.publish_s3_mmio_read_u16(0x82E8, 0x1122);
    mem.publish_s3_mmio_read_u16(0x82EA, 0x3344);
    mem.publish_s3_mmio_read_u16(0x9AE8, 0x0400);
    assert_eq!(mem.read_u8(S3_MMIO_BASE + 0x42E9), 0xFF);
    assert_eq!(mem.read_u16(S3_MMIO_BASE + 0x9AE8), 0x0400);
    assert_eq!(mem.read_u32(S3_MMIO_BASE + 0x82E8), u32::MAX);
    assert_eq!(
        mem.read_u16(S3_MMIO_BASE + 0x8100),
        0xFFFF,
        "packed aliases are write-only on the original Trio64"
    );
    assert_eq!(
        mem.take_s3_mmio_reads(),
        vec![
            S3MmioRead {
                offset: 0x9AE8,
                width: S3MmioWidth::Word,
            },
            S3MmioRead {
                offset: 0x8100,
                width: S3MmioWidth::Word,
            },
        ],
        "each CPU read must retain its guest transaction width"
    );
    assert!(
        !mem.can_write_u16_vga_no_smc_jit(S3_MMIO_BASE),
        "the VGA fast writer must not bypass active S3 MMIO"
    );

    let drained: Vec<_> = mem.drain_s3_mmio_writes().collect();
    assert_eq!(drained.len(), 2);
    assert!(mem.pending_s3_mmio_writes().is_empty());
}

#[test]
fn disabling_s3_mmio_restores_the_vga_aperture_and_drops_stale_transactions() {
    let mut mem = Memory::new();
    mem.configure_s3_mmio_decoder(true);
    mem.write_u32(S3_MMIO_BASE + 0x8118, 0x40A0);
    let _ = mem.read_u16(S3_MMIO_BASE + 0xBEE8);
    assert_eq!(mem.pending_s3_mmio_writes().len(), 1);

    mem.configure_s3_mmio_decoder(false);
    assert!(!mem.s3_mmio_decoder_enabled());
    assert!(mem.pending_s3_mmio_writes().is_empty());
    assert!(
        mem.take_s3_mmio_reads().is_empty(),
        "decoder changes must discard stale read transactions"
    );

    mem.write_u8(S3_MMIO_BASE, 0x5A);
    assert_ne!(
        mem.read_u8(S3_MMIO_BASE),
        0xFF,
        "disabled MMIO must expose ordinary VGA memory semantics"
    );
}

/// The fused `s3_mmio_attention` flag must track
/// `read_pending || !writes.is_empty()` across every mutation path —
/// the interpreter polls only the fused flag once per instruction.
#[test]
fn s3_mmio_attention_tracks_reads_and_writes_independently() {
    let mut mem = Memory::new();
    mem.configure_s3_mmio_decoder(true);
    assert!(!mem.has_pending_s3_mmio_transactions());

    // Write staged -> attention up; drained -> down.
    mem.write_u32(S3_MMIO_BASE + 0x8118, 0x40A0);
    assert!(mem.has_pending_s3_mmio_transactions());
    let _ = mem.drain_s3_mmio_writes().collect::<Vec<_>>();
    assert!(!mem.has_pending_s3_mmio_transactions());

    // Read staged -> attention up; taken -> down.
    let _ = mem.read_u16(S3_MMIO_BASE + 0xBEE8);
    assert!(mem.has_pending_s3_mmio_transactions());
    let _ = mem.take_s3_mmio_reads();
    assert!(!mem.has_pending_s3_mmio_transactions());

    // Both staged: draining one side must keep attention up for the other.
    mem.write_u32(S3_MMIO_BASE + 0x8118, 0x40A0);
    let _ = mem.read_u16(S3_MMIO_BASE + 0xBEE8);
    let _ = mem.take_s3_mmio_reads();
    assert!(
        mem.has_pending_s3_mmio_transactions(),
        "writes still pend after reads taken"
    );
    let _ = mem.drain_s3_mmio_writes().collect::<Vec<_>>();
    assert!(!mem.has_pending_s3_mmio_transactions());

    // Decoder disable clears everything.
    mem.write_u32(S3_MMIO_BASE + 0x8118, 0x40A0);
    mem.configure_s3_mmio_decoder(false);
    assert!(!mem.has_pending_s3_mmio_transactions());
}

/// The combined GDT+LDT hull fast-out must never change which writes the
/// per-table snoop observes: writes inside either table set the dirty
/// bits; writes outside (including between two distant tables) do not.
#[test]
fn snoop_hull_fast_out_preserves_per_table_dirty_semantics() {
    let mut mem = Memory::new();
    mem.set_gdt_snoop_range(0x1000, 0xFF); // [0x1000, 0x1100)
    mem.set_ldt_snoop_range(0x8000, 0xFF); // [0x8000, 0x8100)
    let _ = mem.take_gdt_dirty();
    let _ = mem.take_ldt_dirty();

    mem.snoop_gdt_write_ext(0x1010, 0, 4);
    assert!(mem.take_gdt_dirty(), "write inside GDT must dirty GDT");
    assert!(!mem.take_ldt_dirty());

    mem.snoop_gdt_write_ext(0x8010, 0, 4);
    assert!(mem.take_ldt_dirty(), "write inside LDT must dirty LDT");
    assert!(!mem.take_gdt_dirty());

    // In the gap between the tables: inside the hull, outside both ranges.
    mem.snoop_gdt_write_ext(0x4000, 0, 4);
    assert!(!mem.take_gdt_dirty());
    assert!(!mem.take_ldt_dirty());

    // Far outside the hull (the fast-out path).
    mem.snoop_gdt_write_ext(0xF_0000, 0, 4);
    assert!(!mem.take_gdt_dirty());
    assert!(!mem.take_ldt_dirty());

    // Straddling the GDT base from below must still dirty.
    mem.snoop_gdt_write_ext(0x0FFE, 0, 4);
    assert!(mem.take_gdt_dirty(), "straddling write must dirty GDT");

    // Clearing the LDT shrinks the hull to the GDT alone.
    mem.clear_ldt_snoop_range();
    mem.snoop_gdt_write_ext(0x8010, 0, 4);
    assert!(!mem.take_ldt_dirty(), "cleared LDT must not dirty");
    mem.snoop_gdt_write_ext(0x1010, 0, 4);
    assert!(mem.take_gdt_dirty());
}

/// MDD-BUG-FLUX-00152: a page overlapping the GDT/LDT snoop hull must
/// not carry a `host_write` pointer. Direct stores bypass the snoop,
/// so descriptor edits would leave the table marked clean.
#[test]
fn a_page_overlapping_the_descriptor_snoop_hull_publishes_no_host_write() {
    let mut mem = Memory::new();
    mem.set_gdt_snoop_range(0x1000, 0xFF); // [0x1000, 0x1100), page 1

    mem.tlb_insert(1, 1, true, false, true);
    assert!(
        mem.tlb_direct_write_ptr(0x1000, 4, false).is_none(),
        "a page holding the GDT must not publish a native write pointer"
    );

    // Control: an unsnooped page still gets its fast path, so the assertion
    // above is about the snoop and not about the insert failing outright.
    mem.tlb_insert(8, 8, true, false, true);
    assert!(
        mem.tlb_direct_write_ptr(0x8000, 4, false).is_some(),
        "an unsnooped page must keep its native write pointer"
    );
}

/// MDD-BUG-FLUX-00152 — arming or moving a descriptor snoop must drop write
/// pointers published under the previous hull.
///
/// `tlb_insert` bakes the hull into publication, so entries filled before
/// the table existed would otherwise keep a live pointer into what is now a
/// descriptor table.
#[test]
fn arming_a_descriptor_snoop_drops_write_pointers_published_before_it() {
    let mut mem = Memory::new();

    mem.tlb_insert(1, 1, true, false, true);
    assert!(
        mem.tlb_direct_write_ptr(0x1000, 4, false).is_some(),
        "precondition: the page is natively writable before any snoop"
    );

    mem.set_gdt_snoop_range(0x1000, 0xFF);
    assert!(
        mem.tlb_direct_write_ptr(0x1000, 4, false).is_none(),
        "arming the snoop must not leave the earlier write pointer live"
    );

    // And it stays withheld once the page is filled again under the new hull.
    mem.tlb_insert(1, 1, true, false, true);
    assert!(
        mem.tlb_direct_write_ptr(0x1000, 4, false).is_none(),
        "a refill under the armed snoop must still withhold the pointer"
    );
}

#[test]
fn an_identical_gdtr_reload_keeps_the_tlb_warm() {
    let mut mem = Memory::new();
    mem.set_gdt_snoop_range(0x1000, 0xFF);

    // A page well outside the hull: its pointer survives iff nothing flushed.
    mem.tlb_insert(8, 8, true, false, true);
    assert!(mem.tlb_direct_write_ptr(0x8000, 4, false).is_some());

    mem.set_gdt_snoop_range(0x1000, 0xFF); // identical reload
    assert!(
        mem.tlb_direct_write_ptr(0x8000, 4, false).is_some(),
        "an identical GDTR reload must not flush the TLB"
    );

    mem.set_gdt_snoop_range(0x2000, 0xFF); // genuine move
    assert!(
        mem.tlb_direct_write_ptr(0x8000, 4, false).is_none(),
        "a moved GDTR must flush, since publication is hull-dependent"
    );
}

#[test]
fn option_rom_decoder_gates_relocates_and_protects_backing() {
    const FIRST_BASE: usize = 0x000C_0000;
    const SECOND_BASE: usize = 0x000D_0000;
    let mut mem = Memory::new();
    let rom = [0x55, 0xAA, 0x08, 0xDE, 0xAD, 0xBE, 0xEF, 0x42];
    mem.ram[FIRST_BASE..FIRST_BASE + rom.len()].fill(0x11);
    mem.ram[SECOND_BASE..SECOND_BASE + rom.len()].fill(0x22);
    mem.set_option_rom_backing(&rom);

    mem.configure_option_rom_decoder(FIRST_BASE, true);
    assert_eq!(
        mem.option_rom_decoder(),
        OptionRomDecoderConfig {
            base: FIRST_BASE,
            size: rom.len(),
            enabled: true,
        }
    );
    assert_eq!(mem.read_u32(FIRST_BASE), 0xDE08_AA55);
    let mut bulk = [0; 8];
    mem.read_bytes(FIRST_BASE, &mut bulk);
    assert_eq!(bulk, rom);

    mem.write_u8(FIRST_BASE, 0x00);
    mem.write_u16(FIRST_BASE + 1, 0x0000);
    mem.write_u32(FIRST_BASE + 3, 0);
    mem.write_u8_jit(FIRST_BASE, 0x00);
    mem.write_bytes_guest(FIRST_BASE, &[0; 8]);
    assert_eq!(mem.option_rom_backing(), rom);
    assert_eq!(&mem.ram[FIRST_BASE..FIRST_BASE + rom.len()], &[0x11; 8]);

    mem.configure_option_rom_decoder(SECOND_BASE, true);
    assert_eq!(
        mem.read_u8(FIRST_BASE),
        0x11,
        "old decode must be torn down"
    );
    assert_eq!(mem.read_u8(SECOND_BASE), 0x55);
    assert!(!mem.bulk_read_ram(SECOND_BASE, rom.len(), &mut bulk));
    assert!(!mem.bulk_copy_ram(SECOND_BASE, 0x1000, rom.len()));

    mem.configure_option_rom_decoder(SECOND_BASE, false);
    assert_eq!(mem.read_u8(SECOND_BASE), 0x22);
}

#[test]
fn test_lfb_bochs_compat_alias_does_not_post_trace_when_enabled() {
    let mut mem = Memory::new();
    mem.init_lfb(4096);
    mem.set_lfb_enabled(true);
    mem.set_post_trace_enabled(true);

    mem.write_u8(LFB_BOCHS_COMPAT_BASE + 3, 0x44);
    assert_eq!(mem.read_u8(LFB_BOCHS_COMPAT_BASE + 3), 0x44);
    assert_eq!(mem.post_trace_seen_len(), 0);
}

#[test]
fn test_lfb_disabled_no_access() {
    let mut mem = Memory::new();
    mem.init_lfb(4096);
    mem.set_lfb_enabled(false); // Keep disabled

    // Write to LFB buffer directly
    mem.lfb_write_u8(0, 0x42);

    // The aperture sits above every address the guest can be given as
    // RAM, so with the decoder off nothing responds there: reads are open
    // bus and writes go nowhere.
    assert_eq!(mem.read_u8(LFB_BASE), 0xFF);

    mem.write_u8(LFB_BASE, 0x99);
    assert_eq!(mem.read_u8(LFB_BASE), 0xFF);

    // Direct LFB buffer should be unchanged (the write was dropped)
    assert_eq!(mem.lfb_read_u8(0), 0x42);
}

#[test]
fn test_lfb_max_size_limit() {
    let mut mem = Memory::new();

    // Request more than max size
    mem.init_lfb(LFB_MAX_SIZE + 1024);

    // Should be clamped to max size
    assert_eq!(mem.lfb().len(), LFB_MAX_SIZE);
}
