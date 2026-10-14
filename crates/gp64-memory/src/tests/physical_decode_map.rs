use super::*;

/// A high physical span above every shipped RAM size and below the lowest
/// fixed 32-bit PCI aperture: the flat-memory shared arena's physical image.
const HIGH_SPAN: Range<usize> = 0x80FF_0000..0xC10F_0000;

#[test]
fn a_default_memory_decodes_nothing_but_ram_in_a_high_span() {
    let mut mem = Memory::new();
    mem.set_a20(true);
    mem.init_lfb(LFB_MAX_SIZE);
    mem.set_lfb_enabled(true);
    assert_eq!(mem.first_decoder_overlapping(HIGH_SPAN), None);
    // The legacy windows are still reported where they are.
    assert_eq!(
        mem.first_decoder_overlapping(VIDEO_BASE..VIDEO_BASE + 1),
        Some((
            PhysicalDecoder::VgaWindow,
            VIDEO_BASE..VIDEO_BASE + VIDEO_SIZE
        ))
    );
    assert_eq!(
        mem.first_decoder_overlapping(LFB_BASE..LFB_BASE + 1),
        Some((
            PhysicalDecoder::LinearFramebuffer,
            LFB_BASE..LFB_BASE + LFB_MAX_SIZE
        ))
    );
}

#[test]
fn every_device_class_placed_in_a_high_span_is_reported() {
    let mut mem = Memory::new();
    mem.set_a20(true);
    mem.add_mmio_region(0x9000_0000, 0x1000);
    assert_eq!(
        mem.first_decoder_overlapping(HIGH_SPAN),
        Some((PhysicalDecoder::MmioRegion, 0x9000_0000..0x9000_1000))
    );

    let mut mem = Memory::new();
    mem.set_a20(true);
    mem.init_lfb(LFB_MAX_SIZE);
    mem.configure_lfb_decoder(LfbDecoderConfig::S3 {
        base: 0xC100_0000,
        size: LFB_MAX_SIZE,
        vram_offset: 0,
        enabled: true,
    });
    assert_eq!(
        mem.first_decoder_overlapping(HIGH_SPAN),
        Some((
            PhysicalDecoder::LinearFramebuffer,
            0xC100_0000..0xC100_0000 + LFB_MAX_SIZE
        ))
    );

    // A20 masked aliases the span's first address with bit 20 set (the
    // span's own start, 0x80FF_0000).
    let mut mem = Memory::new();
    mem.set_a20(false);
    assert_eq!(
        mem.first_decoder_overlapping(HIGH_SPAN),
        Some((PhysicalDecoder::A20Wrap, 0x80FF_0000..0x80FF_0001))
    );
}
