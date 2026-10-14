use super::*;

fn chained_memory() -> Memory {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.memory_map_select = 1;
    mem.update_vga_write_handler();
    mem
}

// Chain-4 mapping (vga_memory.cpp:398-407):
// CPU byte i selects plane i&3 and cell i&~3, including the top of A000.
#[test]
fn chain4remap_write_unchain_read_map_select() {
    let mut mem = chained_memory();
    let offsets = [4, 5, 6, 7, 0x3fff, 0x4000, 0xfffc, 0xfffd, 0xfffe, 0xffff];
    for (n, &i) in offsets.iter().enumerate() {
        mem.write_u8(VIDEO_BASE + i, 0x80 + n as u8);
    }
    for (n, &i) in offsets.iter().enumerate() {
        assert_eq!(mem.read_u8(VIDEO_BASE + i), 0x80 + n as u8);
        assert_eq!(mem.peek_video_u8(VIDEO_BASE + i), 0x80 + n as u8);
        let mut out = [0];
        mem.peek_video_range(VIDEO_BASE + i, &mut out);
        assert_eq!(out[0], 0x80 + n as u8);
        assert_eq!(mem.video_memory()[i], 0x80 + n as u8);
    }
    let mut state = mem.vga;
    state.chain4 = false;
    state.is_mode_x = true;
    mem.apply_vga_pipeline(&state);
    for (n, &i) in offsets.iter().enumerate() {
        mem.vga.read_plane = (i & 3) as u8;
        assert_eq!(
            mem.read_u8(VIDEO_BASE + (i & !3)),
            0x80 + n as u8,
            "pixel {i:#x}"
        );
    }
}

#[test]
fn chain4remap_resync_round_trip() {
    let mut mem = chained_memory();
    for i in 0..VIDEO_WINDOW_64K {
        mem.video_memory_mut()[i] = (i ^ (i >> 8)) as u8;
    }
    mem.video_memory_mut()[VIDEO_WINDOW_64K..].fill(0x5a);
    mem.take_vga_plane_rewrites();
    mem.sync_chain4_to_planes();
    for i in 0..VIDEO_WINDOW_64K {
        assert_eq!(
            mem.plane(i & 3)[i & !3],
            (i ^ (i >> 8)) as u8,
            "cell for {i:#x}"
        );
    }
    assert_eq!(mem.take_vga_plane_rewrites(), VGA_PLANE_VRAM_ALL_PAGES);
    mem.video_memory_mut()[..VIDEO_WINDOW_64K].fill(0);
    mem.sync_planes_to_chain4();
    for i in 0..VIDEO_WINDOW_64K {
        assert_eq!(mem.video_memory()[i], (i ^ (i >> 8)) as u8, "mirror {i:#x}");
    }
    assert!(mem.video_memory()[VIDEO_WINDOW_64K..]
        .iter()
        .all(|&v| v == 0x5a));
}

#[test]
fn chain4remap_native_span_matches_byte_cells() {
    let mut byte = chained_memory();
    let mut span = chained_memory();
    let mut batch = chained_memory();
    for start in [5, 0x3ffd, 0xfffc] {
        let bytes = [0x91, 0xa2, 0xb3, 0xc4];
        assert!(span.vga_chain4_plain_store_range(VIDEO_BASE + start, bytes.len()));
        for (n, &v) in bytes.iter().enumerate() {
            byte.commit_vga_chain4_plain_byte(VIDEO_BASE + start + n, v);
        }
        span.drc_commit_vga_native_span(
            VgaCommitKind::Chain4,
            VIDEO_BASE + start,
            u32::from_le_bytes(bytes) as u64,
            4,
        );
        batch.drc_commit_vga_native_bytes(VgaCommitKind::Chain4, VIDEO_BASE + start, &bytes);
        for (n, &v) in bytes.iter().enumerate() {
            let i = start + n;
            for mem in [&byte, &span, &batch] {
                assert_eq!(mem.plane(i & 3)[i & !3], v, "native pixel {i:#x}");
                assert_eq!(mem.video_memory()[i], v);
            }
        }
    }
    // S3 writes physical interleaved VRAM; only chain-addressable cells
    // map back into the scanout mirror (inverse of chain4remap).
    batch.store_s3_vram_to_planes(4 * 0xfffc + 3, &[0x7e]);
    assert_eq!(batch.video_memory()[0xffff], 0x7e);
    assert_eq!(batch.plane(3)[0xfffc], 0x7e);
    batch.store_s3_vram_to_planes(4 * 5, &[0x6d]);
    assert_eq!(batch.plane(0)[5], 0x6d);
    assert_eq!(batch.video_memory()[5], byte.video_memory()[5]);
    // Restore those probe bytes before checking the commit paths' full image.
    batch.plane_mut(0)[5] = byte.plane(0)[5];
    batch.store_s3_vram_to_planes(4 * 0xfffc + 3, &[byte.video_memory()[0xffff]]);
    assert_eq!(span.planes_flat, byte.planes_flat);
    assert_eq!(batch.planes_flat, byte.planes_flat);
    assert!(!span.vga_chain4_plain_store_range(VIDEO_BASE + VIDEO_WINDOW_64K - 1, 2));
    assert!(!span.vga_chain4_plain_store_range(usize::MAX, 2));
}
