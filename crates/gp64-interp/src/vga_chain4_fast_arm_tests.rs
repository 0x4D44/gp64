use super::*;
use gp64_types::VgaPipelineState;

fn plain_chain4_state() -> VgaPipelineState {
    VgaPipelineState {
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
    }
}

fn chain4_mem() -> Memory {
    let mut m = Memory::new();
    m.apply_vga_pipeline(&plain_chain4_state());
    assert_eq!(
        m.vga_native_commit_kind(),
        Some(VgaCommitKind::Chain4),
        "fixture must arm the chain-4 admission"
    );
    m
}

fn guards() -> WriteGuards<PcAccessPolicy> {
    WriteGuards::default()
}

/// Twin-memory differential: identical stores through the fast-arm leaves
/// vs the full `Memory::write_*` route must leave identical guest-visible
/// VGA state (read-back over the touched span) and identical watchdog
/// counts. Fails without U3 only via the engagement assertion — the
/// parity half is the inviolable oracle.
#[test]
fn chain4_fast_arm_matches_the_full_route_byte_for_byte() {
    let mut fast = chain4_mem();
    let mut full = chain4_mem();
    let g = guards();

    // (addr, width, value) covering: plain byte, offset-in-group word,
    // aligned dword, GROUP-CROSSING word (fast arm must decline and
    // still match), aperture tail byte, B-segment word (outside the
    // 64K aperture — full route on both sides).
    let cases: &[(usize, usize, u32)] = &[
        (0xA0000, 1, 0x5A),
        (0xA0001, 2, 0xBEEF),
        (0xA1000, 4, 0x0102_0304),
        (0xA0003, 2, 0xCAFE), // crosses the group of four
        (0xAFFFF, 1, 0x77),   // last aperture byte
        (0xB8000, 2, 0x1234), // B-segment: not chain-4 aperture
        (0xA2000, 4, 0xDEAD_BEEF),
    ];
    for &(addr, width, val) in cases {
        // SAFETY: live ram/tlb pointers from the same Memory the leaf
        // mutates; single-threaded test.
        unsafe {
            match width {
                1 => {
                    vga_aware_write_u8(fast.ram_ptr_mut(), addr, val as u8, &mut fast, g);
                }
                2 => {
                    vga_aware_write_u16(fast.ram_ptr_mut(), addr, val as u16, &mut fast, g);
                }
                _ => {
                    vga_aware_write_u32(fast.ram_ptr_mut(), addr, val, &mut fast, g);
                }
            }
        }
        match width {
            1 => full.write_u8(addr, val as u8),
            2 => full.write_u16(addr, val as u16),
            _ => full.write_u32(addr, val),
        }
    }

    for &(addr, width, _) in cases {
        for i in 0..width {
            assert_eq!(
                fast.read_u8(addr + i),
                full.read_u8(addr + i),
                "guest-visible divergence at {:#x}",
                addr + i
            );
        }
    }
    assert_eq!(
        fast.video_write_count(),
        full.video_write_count(),
        "watchdog accounting must match the full route"
    );
    // Engagement: byte+word+dword+tail-byte+dword = 1+2+4+1+4 = 12
    // committed bytes; the group-crossing word and the B-segment word
    // must NOT engage.
    assert_eq!(
        fast.interp_vga_chain4_fast_commits(),
        12,
        "fast arm engagement (byte units) — fails without U3"
    );
    assert_eq!(full.interp_vga_chain4_fast_commits(), 0);
}

/// consume it (an `is_some()`-shaped gate would corrupt planar stores).
#[test]
fn mode_y_arms_planar_and_never_the_chain4_fast_arm() {
    let mut m = Memory::new();
    let state = VgaPipelineState {
        chain4: false,
        is_mode_x: true,
        plane_mask: 0x05,
        ..plain_chain4_state()
    };
    m.apply_vga_pipeline(&state);
    assert_eq!(
        m.vga_native_commit_kind(),
        Some(VgaCommitKind::Planar),
        "Mode-Y must arm Planar"
    );
    let g = guards();
    // SAFETY: as above.
    unsafe {
        vga_aware_write_u8(m.ram_ptr_mut(), 0xA0010, 0x33, &mut m, g);
    }
    assert_eq!(
        m.interp_vga_chain4_fast_commits(),
        0,
        "chain-4 fast arm must never fire under a Planar kind"
    );
}

/// S3 disarm end-to-end: with the decoder live the fast arm must stand
/// down (the U1 predicate gate observed from the interpreter side).
#[test]
fn s3_mmio_decoder_stands_the_fast_arm_down() {
    let mut m = chain4_mem();
    m.configure_s3_mmio_decoder(true);
    let g = guards();
    // SAFETY: as above.
    unsafe {
        vga_aware_write_u8(m.ram_ptr_mut(), 0xA0020, 0x44, &mut m, g);
    }
    assert_eq!(
        m.interp_vga_chain4_fast_commits(),
        0,
        "fast arm must not engage while the S3 MMIO decoder claims A0000"
    );
}
