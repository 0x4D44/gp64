//! Tests for direct VGA chain-4 commits. Admission must select exactly
//! the plain pipeline states where `commit_vga_chain4_plain_byte` matches
//! the interpreter's `write_u8` handler byte for byte.
use super::*;

/// A `Memory` in the plain chain-4 Mode 13h state the fast path targets:
/// `video_mode = 0x13`, chain-4 on, not Mode X, the 0xA0000 aperture live,
/// default (pass-through) GC pipeline. `update_vga_write_handler` latches the
/// cached handler to `Chain4`.
fn plain_chain4_mem(memory_map_select: u8) -> Memory {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = true;
    mem.vga.is_mode_x = false;
    mem.vga.memory_map_select = memory_map_select;
    // Default GC pipeline is already plain; state it explicitly.
    mem.vga.write_mode = 0;
    mem.vga.data_rotate = 0;
    mem.vga.enable_set_reset = 0;
    mem.vga.bit_mask = 0xFF;
    mem.update_vga_write_handler();
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Chain4);
    assert!(mem.vga_chain4_plain_store_range(VIDEO_BASE, 1));
    mem
}

/// MDD-BUG-CRU-01474: a chain-4 commit into a recorded page-table frame
/// inside the VGA aperture must move the PT-write tripwire.
#[test]
fn interp_chain4_fast_commit_into_a_pt_frame_bumps_the_tripwire() {
    let mut mem = plain_chain4_mem(0);
    mem.set_smc_has_code_pages(true); // links can only exist alongside code pages
    let frame = (VIDEO_BASE >> 12) as u32;
    mem.record_pt_frame(frame);
    let before = mem.dynrec_pt_write_epoch();
    mem.interp_commit_vga_chain4_fast(VIDEO_BASE + 0x10, 0x5A);
    assert!(
        mem.dynrec_pt_write_epoch() > before,
        "a PTE edit through the chain-4 fast arm must move pt_write_epoch"
    );
}

fn plane_idx(offset: usize) -> usize {
    // Chain-4 mapping: plane x&3, cell x&~3.
    (offset & 3) * PLANE_SIZE + (offset & !3)
}

fn xorshift64(mut x: u64) -> u64 {
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x
}

/// Set the full VGA pipeline state from a pseudo-random word, biased so the
/// plain-chain4 predicate is admitted often enough to exercise the true
/// branch (verified non-vacuous below).
fn set_random_vga_state(mem: &mut Memory, r: u64) {
    mem.vga.video_mode = if r & 1 == 0 { 0x13 } else { (r >> 1) as u8 };
    mem.vga.chain4 = (r >> 2) & 3 != 0; // true ~3/4
    mem.vga.is_mode_x = (r >> 4) & 3 == 0; // true ~1/4
    mem.vga.memory_map_select = ((r >> 6) & 3) as u8;
    mem.vga.write_mode = if (r >> 8) & 1 == 0 {
        0
    } else {
        ((r >> 9) & 3) as u8
    };
    mem.vga.data_rotate = if (r >> 11) & 1 == 0 {
        0
    } else {
        ((r >> 12) & 0x1F) as u8
    };
    mem.vga.enable_set_reset = if (r >> 17) & 1 == 0 {
        0
    } else {
        ((r >> 18) & 0x0F) as u8
    };
    mem.vga.bit_mask = if (r >> 22) & 1 == 0 {
        0xFF
    } else {
        (r >> 23) as u8
    };
    mem.vga.set_reset = (r >> 31) as u8;
    mem.vga.read_mode = ((r >> 39) & 1) as u8;
    mem.vga.gc_graphics_mode = (r >> 40) & 1 == 0;
    mem.vga.plane_mask = ((r >> 41) & 0x0F) as u8;
    mem.vga.read_plane = ((r >> 45) & 3) as u8;
    mem.vga.color_compare = (r >> 47) as u8;
    mem.vga.color_dont_care = (r >> 55) as u8;
    mem.update_vga_write_handler();
}

/// §4.1 commit-math differential: over a fixed boundary table covering all
/// four plane residues and the aperture edges, the native commit reproduces
/// the interpreter chain-4 write byte-for-byte (ram + planes_flat +
/// hash_state).
#[test]
fn vga_native_commit_is_byte_identical_to_interpreter_chain4() {
    // offset&3 residues 0..3, the last visible Mode-13h pixel (320*200-1 =
    // 0xF9FF), just past it, and the 64 KB aperture edge 0xFFFF.
    let offsets: [usize; 8] = [0, 1, 2, 3, 0xF9FF, 0xFA00, 0xFFFE, 0xFFFF];
    let values: [u8; 8] = [0x00, 0xFF, 0x5A, 0xA5, 0x01, 0x80, 0x7F, 0xC3];

    let mut mem_interp = plain_chain4_mem(1);
    let mut mem_native = plain_chain4_mem(1);

    for (i, &offset) in offsets.iter().enumerate() {
        let value = values[i];
        let phys = VIDEO_BASE + offset;
        mem_interp.write_u8(phys, value);
        mem_native.commit_vga_chain4_plain_byte(phys, value);

        let idx = plane_idx(offset);
        assert_eq!(
            mem_interp.ram[phys], mem_native.ram[phys],
            "ram mismatch at offset {offset:#x}"
        );
        assert_eq!(
            mem_interp.planes_flat[idx], mem_native.planes_flat[idx],
            "planes_flat mismatch at offset {offset:#x}"
        );
        // In the plain state both write exactly `value`.
        assert_eq!(mem_native.ram[phys], value);
        assert_eq!(mem_native.planes_flat[idx], value);
    }

    // Whole-buffer + full-state parity after the sequence.
    assert_eq!(
        &mem_interp.ram[VIDEO_BASE..VIDEO_BASE + VIDEO_WINDOW_64K],
        &mem_native.ram[VIDEO_BASE..VIDEO_BASE + VIDEO_WINDOW_64K]
    );
    assert_eq!(mem_interp.planes_flat, mem_native.planes_flat);
    assert_eq!(mem_interp.hash_state(), mem_native.hash_state());
}

/// §4.1 by-construction coupling guard (the real invariant the design leans
/// on): over randomized FULL pipeline states, whenever the predicate admits
/// a store, `commit_vga_chain4_plain_byte` yields ram+planes_flat
/// byte-identical to `write_u8`. ANY future predicate loosening that admits a
/// non-interpreter-equivalent store fails this by construction.
#[test]
fn vga_predicate_admits_only_interpreter_equivalent_states() {
    let mut mem_i = Memory::new();
    let mut mem_n = Memory::new();
    let mut seed: u64 = 0x0BAD_F00D_D15E_A5E5;
    let mut admitted = 0usize;
    for _ in 0..4000 {
        seed = xorshift64(seed);
        let state = seed;
        set_random_vga_state(&mut mem_i, state);
        set_random_vga_state(&mut mem_n, state);
        seed = xorshift64(seed);
        let offset = (seed as usize) & 0xFFFF;
        let value = (seed >> 16) as u8;
        let phys = VIDEO_BASE + offset;
        if mem_i.vga_chain4_plain_store_range(phys, 1) {
            admitted += 1;
            mem_i.write_u8(phys, value);
            mem_n.commit_vga_chain4_plain_byte(phys, value);
            let idx = plane_idx(offset);
            assert_eq!(
                mem_i.ram[phys], mem_n.ram[phys],
                "ram divergence @ {phys:#x} state {state:#x}"
            );
            assert_eq!(
                mem_i.planes_flat[idx], mem_n.planes_flat[idx],
                "planes divergence @ {phys:#x} state {state:#x}"
            );
        }
    }
    // Non-vacuity: the plain branch was actually exercised.
    assert!(admitted > 0, "predicate never admitted — test is vacuous");
    // Whole-state parity after the full identical write sequence.
    assert_eq!(mem_i.hash_state(), mem_n.hash_state());
}

/// `hash_vga_parity` is deterministic, ignores general RAM, and changes
/// on plane data, CPU-visible video bytes or VGA pipeline state changes.
#[test]
fn hash_vga_parity_scopes_to_vga_only() {
    // (1) Deterministic (A/A): the same Memory hashed twice is equal.
    let mut mem = Memory::new();
    let base = mem.hash_vga_parity();
    assert_eq!(
        base,
        mem.hash_vga_parity(),
        "hash_vga_parity must be deterministic"
    );

    // (2) General-RAM independence — the exact false-FAIL fix. A write far
    // outside the video window must NOT move the VGA parity hash. 0x1000 is
    // three orders of magnitude below VIDEO_BASE (0xA0000).
    assert!(
        0x1000 < VIDEO_BASE,
        "0x1000 must sit outside the video window"
    );
    mem.write_u8(0x1000, 0xAB);
    assert_eq!(mem.read_u8(0x1000), 0xAB, "sanity: the RAM write landed");
    assert_eq!(
        mem.hash_vga_parity(),
        base,
        "general (non-video) RAM must not affect the VGA parity hash"
    );

    // (3) Catches VGA divergence on each axis it must cover.
    // 3a. planes_flat (the de-interleaved Mode-X/planar view).
    let mut m = Memory::new();
    let h = m.hash_vga_parity();
    m.planes_flat[0] ^= 0xFF;
    assert_ne!(
        m.hash_vga_parity(),
        h,
        "planes_flat mutation must change the hash"
    );

    // 3b. The CPU-visible video window ram[VIDEO_BASE..].
    let mut m = Memory::new();
    let h = m.hash_vga_parity();
    m.ram[VIDEO_BASE + 0x100] ^= 0xFF;
    assert_ne!(
        m.hash_vga_parity(),
        h,
        "video-window RAM mutation must change the hash"
    );

    // 3c. A VGA-pipeline state field the write handler dispatches on.
    let mut m = Memory::new();
    let h = m.hash_vga_parity();
    m.vga.plane_mask ^= 0x0F;
    assert_ne!(
        m.hash_vga_parity(),
        h,
        "VGA-state field mutation must change the hash"
    );
}

/// With Mode 13h constructed through `apply_vga_pipeline`, direct
/// chain-4 commits must match `write_u8` in both `planes_flat` and the
/// RAM video mirror across all four chain-4 plane selects.
#[test]
fn chain4_native_commit_byte_identical_to_interpreter() {
    // Plain chain-4 Mode 13h via the production pipeline-application entry
    // point: mode 0x13, chain4, not Mode X, write_mode 0, no rotate/set-reset,
    // full bit mask, memory-map select 0 (matches the real Mode-13h state).
    let plain = VgaPipelineState {
        video_mode: 0x13,
        chain4: true,
        is_mode_x: false,
        plane_mask: 0x0F,
        memory_map_select: 0,
        write_mode: 0,
        data_rotate: 0,
        enable_set_reset: 0,
        bit_mask: 0xFF,
        ..Default::default()
    };

    let mut mem_interp = Memory::new();
    let mut mem_native = Memory::new();
    mem_interp.apply_vga_pipeline(&plain);
    mem_native.apply_vga_pipeline(&plain);
    assert_eq!(mem_interp.vga_write_handler(), VgaWriteHandler::Chain4);

    // Offsets covering all four chain-4 plane residues (off & 3 == 0,1,2,3)
    // plus an interior offset and the aperture edge, each a distinct value.
    let cases: [(usize, u8); 6] = [
        (0, 0x11),      // plane 0
        (1, 0x22),      // plane 1
        (2, 0x33),      // plane 2
        (3, 0x44),      // plane 3
        (0x1234, 0xA5), // residue 0, interior
        (0xFFFF, 0xC3), // residue 3, 64 KB aperture edge
    ];

    for (off, value) in cases {
        let phys = VIDEO_BASE + off;
        // The predicate must admit this exact store on the initialised state.
        assert!(
            mem_interp.vga_chain4_plain_store_range(phys, 1),
            "predicate must admit the plain chain-4 store at offset {off:#x}"
        );

        // Interpreter path on one instance, native commit on the other.
        mem_interp.write_u8(phys, value);
        mem_native.commit_vga_chain4_plain_byte(phys, value);

        let idx = plane_idx(off);
        assert_eq!(
            mem_interp.planes_flat[idx], mem_native.planes_flat[idx],
            "planes_flat divergence at offset {off:#x}"
        );
        assert_eq!(
            mem_interp.ram[phys], mem_native.ram[phys],
            "ram-mirror divergence at offset {off:#x}"
        );
    }

    // Whole video-window + planes parity after the full identical sequence.
    assert_eq!(
        &mem_interp.ram[VIDEO_BASE..VIDEO_BASE + VIDEO_WINDOW_64K],
        &mem_native.ram[VIDEO_BASE..VIDEO_BASE + VIDEO_WINDOW_64K],
        "video-window mirrors must be byte-identical after the write sequence"
    );
    assert_eq!(
        mem_interp.planes_flat, mem_native.planes_flat,
        "planes_flat must be byte-identical after the write sequence"
    );
}

/// §4.1 focused negative enumeration (documentation-grade): the predicate is
/// true ONLY for the plain chain-4 state fully inside the 0xA0000 aperture,
/// and false for each single field departure.
#[test]
fn native_chain4_commit_advances_the_video_write_count() {
    let mut mem = plain_chain4_mem(1);
    let before = mem.video_write_count();
    let bus_before = mem.vga_bus_write_count();
    mem.commit_vga_chain4_plain_byte(VIDEO_BASE, 0x5A);
    assert!(
        mem.video_write_count() > before,
        "a native chain-4 commit must count as VGA progress"
    );
    assert_eq!(mem.vga_bus_write_count(), bus_before + 1);
}

#[test]
fn linear_framebuffer_write_does_not_count_as_legacy_vga_bus_traffic() {
    let mut mem = Memory::new();
    mem.set_lfb_enabled(true);
    let before = mem.vga_bus_write_count();

    mem.write_u8(LFB_BASE, 0x5A);

    assert_eq!(mem.vga_bus_write_count(), before);
}

#[test]
fn vga_chain4_plain_store_range_predicate_table() {
    let base = plain_chain4_mem(1);
    // Positive: in-window plain chain4, both select 0 and 1.
    assert!(base.vga_chain4_plain_store_range(VIDEO_BASE, 1));
    assert!(base.vga_chain4_plain_store_range(VIDEO_BASE, 4));
    assert!(base.vga_chain4_plain_store_range(VIDEO_BASE + 0xFFFF, 1));
    assert!(plain_chain4_mem(0).vga_chain4_plain_store_range(VIDEO_BASE, 1));

    // Negative: each single perturbation must reject.
    let mut m = plain_chain4_mem(1);
    m.vga.write_mode = 1;
    assert!(
        !m.vga_chain4_plain_store_range(VIDEO_BASE, 1),
        "write_mode=1"
    );
    let mut m = plain_chain4_mem(1);
    m.vga.data_rotate = 1;
    assert!(
        !m.vga_chain4_plain_store_range(VIDEO_BASE, 1),
        "data_rotate=1 rotate"
    );
    let mut m = plain_chain4_mem(1);
    m.vga.data_rotate = 0x08;
    assert!(
        !m.vga_chain4_plain_store_range(VIDEO_BASE, 1),
        "data_rotate=0x08 logic-op"
    );
    let mut m = plain_chain4_mem(1);
    m.vga.enable_set_reset = 1;
    assert!(
        !m.vga_chain4_plain_store_range(VIDEO_BASE, 1),
        "enable_set_reset=1"
    );
    let mut m = plain_chain4_mem(1);
    m.vga.bit_mask = 0xFE;
    assert!(
        !m.vga_chain4_plain_store_range(VIDEO_BASE, 1),
        "bit_mask=0xFE"
    );
    let mut m = plain_chain4_mem(1);
    m.vga.memory_map_select = 2;
    assert!(
        !m.vga_chain4_plain_store_range(VIDEO_BASE, 1),
        "memory_map_select=2"
    );
    let mut m = plain_chain4_mem(1);
    m.vga.memory_map_select = 3;
    assert!(
        !m.vga_chain4_plain_store_range(VIDEO_BASE, 1),
        "memory_map_select=3"
    );
    // Handler Planar (Mode X at 0x13) — must key on handler, not video_mode.
    let mut m = plain_chain4_mem(1);
    m.vga.is_mode_x = true;
    m.update_vga_write_handler();
    assert_eq!(m.vga_write_handler(), VgaWriteHandler::Planar);
    assert!(
        !m.vga_chain4_plain_store_range(VIDEO_BASE, 1),
        "is_mode_x@0x13 => Planar"
    );
    // Handler Linear (text mode).
    let mut m = plain_chain4_mem(1);
    m.vga.video_mode = 0x03;
    m.update_vga_write_handler();
    assert_eq!(m.vga_write_handler(), VgaWriteHandler::Linear);
    assert!(
        !m.vga_chain4_plain_store_range(VIDEO_BASE, 1),
        "video_mode 0x03 => Linear"
    );
    // Below the aperture, and spans crossing the 0xB0000 top edge.
    assert!(
        !base.vga_chain4_plain_store_range(VIDEO_BASE - 1, 1),
        "below 0xA0000"
    );
    assert!(
        !base.vga_chain4_plain_store_range(VIDEO_BASE + VIDEO_WINDOW_64K - 2, 4),
        "span crosses 0xB0000"
    );
    assert!(
        !base.vga_chain4_plain_store_range(VIDEO_BASE + VIDEO_WINDOW_64K, 1),
        "at 0xB0000 (one past the aperture)"
    );
}

/// A `Memory` in the plain unchained-planar (Mode-Y) state the planar fast
/// path targets: `video_mode = 0x13`, chain-4 OFF, `is_mode_x` set (so the
/// cached handler latches to `Planar`), the 0xA0000 aperture live, default
/// pass-through GC, and `plane_mask` selecting the requested planes.
fn plain_planar_mem(memory_map_select: u8, plane_mask: u8) -> Memory {
    let mut mem = Memory::new();
    mem.vga.video_mode = 0x13;
    mem.vga.chain4 = false;
    mem.vga.is_mode_x = true;
    mem.vga.memory_map_select = memory_map_select;
    mem.vga.plane_mask = plane_mask;
    mem.vga.write_mode = 0;
    mem.vga.data_rotate = 0;
    mem.vga.enable_set_reset = 0;
    mem.vga.bit_mask = 0xFF;
    mem.vga.gc_graphics_mode = true;
    mem.update_vga_write_handler();
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Planar);
    assert!(mem.vga_planar_plain_store_range(VIDEO_BASE, 1));
    mem
}

/// §a1.1 commit-math differential over a boundary offset table AND a set of
/// Map-Mask values (single-plane, all-planes, alternating, plane-3-only,
/// none): the native planar commit reproduces the interpreter Planar write
/// byte-for-byte in every plane, leaves the `ram` mirror untouched (Mode-X
/// scanout reads planes), marks the video page dirty like the interpreter, and
/// matches `hash_state`.
#[test]
fn vga_native_commit_is_byte_identical_to_interpreter_planar() {
    let offsets: [usize; 6] = [0, 1, 0x100, 0x4B00, 0xFFFE, 0xFFFF];
    let values: [u8; 6] = [0x00, 0xFF, 0x5A, 0xA5, 0x3C, 0xC3];
    let masks: [u8; 6] = [0x01, 0x0F, 0x05, 0x0A, 0x08, 0x00];

    for &mask in &masks {
        let mut mem_interp = plain_planar_mem(1, mask);
        let mut mem_native = plain_planar_mem(1, mask);
        for (i, &offset) in offsets.iter().enumerate() {
            let value = values[i];
            let phys = VIDEO_BASE + offset;
            assert!(mem_interp.vga_planar_plain_store_range(phys, 1));
            mem_interp.write_u8(phys, value);
            mem_native.commit_vga_planar_plain_byte(phys, value);

            for plane in 0..4usize {
                let idx = plane * PLANE_SIZE + offset;
                assert_eq!(
                    mem_interp.planes_flat[idx], mem_native.planes_flat[idx],
                    "planes_flat mismatch mask={mask:#04x} plane={plane} offset={offset:#x}"
                );
                if mask & (1 << plane) != 0 {
                    assert_eq!(
                        mem_native.planes_flat[idx], value,
                        "selected plane {plane} not written to value"
                    );
                }
            }
            // Planar writes do NOT touch the linear ram mirror (both stay equal).
            assert_eq!(
                mem_native.ram[phys], mem_interp.ram[phys],
                "ram mirror diverged at offset {offset:#x}"
            );
        }
        assert_eq!(mem_interp.planes_flat, mem_native.planes_flat);
        assert_eq!(mem_interp.hash_state(), mem_native.hash_state());
        assert!(
            mem_native.has_dirty_video_pages(),
            "native planar commit must mark the video page dirty"
        );
    }
}

/// §a1.2 by-construction coupling guard for the planar predicate: over
/// randomized FULL pipeline states, whenever `vga_planar_plain_store_range`
/// admits, the native commit is planes- and hash_state-identical to the
/// interpreter Planar write. Any future loosening that admits a
/// non-interpreter-equivalent store fails this by construction.
#[test]
fn vga_planar_predicate_admits_only_interpreter_equivalent_states() {
    let mut mem_i = Memory::new();
    let mut mem_n = Memory::new();
    let mut seed: u64 = 0x5EED_1234_ABCD_0F0F;
    let mut admitted = 0usize;
    for _ in 0..8000 {
        seed = xorshift64(seed);
        let state = seed;
        set_random_vga_state(&mut mem_i, state);
        set_random_vga_state(&mut mem_n, state);
        seed = xorshift64(seed);
        let offset = (seed as usize) & 0xFFFF;
        let value = (seed >> 16) as u8;
        let phys = VIDEO_BASE + offset;
        if mem_i.vga_planar_plain_store_range(phys, 1) {
            admitted += 1;
            mem_i.write_u8(phys, value);
            mem_n.commit_vga_planar_plain_byte(phys, value);
            for plane in 0..4usize {
                let idx = plane * PLANE_SIZE + offset;
                assert_eq!(
                    mem_i.planes_flat[idx], mem_n.planes_flat[idx],
                    "planes divergence @ {phys:#x} state {state:#x} plane {plane}"
                );
            }
            assert_eq!(
                mem_i.ram[phys], mem_n.ram[phys],
                "ram divergence @ {phys:#x} state {state:#x}"
            );
            // Guest-observable side effects beyond planes/ram must also stay in
            // lockstep. `planar_font_latch_pending` is NOT in `hash_state`, so
            // without this the font-latch divergence (adversarial-review finding)
            // would slip through: any admitted store that set the latch on the
            // interpreter but not on the native commit fails here.
            assert_eq!(
                mem_i.planar_font_latch_pending(),
                mem_n.planar_font_latch_pending(),
                "font-latch divergence @ {phys:#x} state {state:#x}"
            );
        }
    }
    assert!(
        admitted > 0,
        "planar predicate never admitted — test is vacuous"
    );
    assert_eq!(mem_i.hash_state(), mem_n.hash_state());
}

/// §a1.4 REGRESSION (adversarial-review finding 2026-07-08): the interpreter's
/// planar write sets the one-way, guest-observable `planar_font_latch_pending`
/// flag for the plane-2 font-upload pattern (`!gc_graphics_mode &&
/// plane_mask==0x04 && !chain4 && mms∈{0,1}`); the planes-only native commit
/// does not. The predicate MUST exclude the latch-SETTING store so the two
/// engines cannot diverge, while leaving the hot Mode-Y path
/// (gc_graphics_mode=true) and post-latch stores admitted.
#[test]
fn vga_planar_predicate_excludes_font_latch_state() {
    // Build the font-upload access pattern directly (plain_planar_mem sets
    // gc_graphics_mode=true, the opposite of what triggers the latch).
    let make = || {
        let mut mem = Memory::new();
        mem.vga.video_mode = 0x13;
        mem.vga.chain4 = false;
        mem.vga.is_mode_x = true;
        mem.vga.memory_map_select = 1;
        mem.vga.plane_mask = 0x04;
        mem.vga.gc_graphics_mode = false; // GC06 bit0 clear → font/text path
        mem.vga.write_mode = 0;
        mem.vga.data_rotate = 0;
        mem.vga.enable_set_reset = 0;
        mem.vga.bit_mask = 0xFF;
        mem.update_vga_write_handler();
        mem
    };

    let mut mem = make();
    assert_eq!(
        mem.vga_write_handler(),
        VgaWriteHandler::Planar,
        "Planar handler"
    );
    assert!(!mem.planar_font_latch_pending(), "latch starts clear");
    // (1) the predicate REJECTS the latch-SETTING store — it must fall back.
    assert!(
        !mem.vga_planar_plain_store_range(VIDEO_BASE, 1),
        "font-latch-setting store must NOT be admitted for native commit"
    );

    // (2) an INTERPRETER write in this exact state DOES set the one-way latch —
    // the guest-observable side effect the native commit cannot replicate.
    mem.write_u8(VIDEO_BASE, 0x5A);
    assert!(
        mem.planar_font_latch_pending(),
        "interpreter write sets the font latch — the divergence the exclusion prevents"
    );

    // (3) once the latch is set (one-way), later stores in the SAME state are
    // admitted again: both engines then leave the flag set, so no divergence.
    assert!(
        mem.vga_planar_plain_store_range(VIDEO_BASE, 1),
        "with the latch already set, later stores are admitted (no divergence)"
    );

    // (4) the hot Mode-Y render path (gc_graphics_mode=true) is unaffected even
    // at plane_mask=0x04.
    assert!(
        plain_planar_mem(1, 0x04).vga_planar_plain_store_range(VIDEO_BASE, 1),
        "gc_graphics_mode=true stays admitted at plane_mask=0x04"
    );

    // (5) plane_mask != 0x04 with gc_graphics_mode=false is not the font-upload
    // pattern and stays admitted.
    let mut nonfont = make();
    nonfont.vga.plane_mask = 0x0F;
    nonfont.update_vga_write_handler();
    assert!(
        nonfont.vga_planar_plain_store_range(VIDEO_BASE, 1),
        "plane_mask != 0x04 is not the font-upload pattern — still admitted"
    );
}

/// §a1.3 focused negative enumeration for the planar predicate: true ONLY for
/// the plain unchained-planar state fully inside the aperture; false for each
/// single field departure — and crucially false for the chain-4 state (the two
/// predicates are mutually exclusive by handler).
#[test]
fn vga_planar_plain_store_range_predicate_table() {
    let base = plain_planar_mem(1, 0x0F);
    // Positive: in-window plain planar, both selects, and any Map-Mask value
    // (plane_mask is NOT an admission field).
    assert!(base.vga_planar_plain_store_range(VIDEO_BASE, 1));
    assert!(base.vga_planar_plain_store_range(VIDEO_BASE, 4));
    assert!(base.vga_planar_plain_store_range(VIDEO_BASE + 0xFFFF, 1));
    assert!(plain_planar_mem(0, 0x0F).vga_planar_plain_store_range(VIDEO_BASE, 1));
    assert!(plain_planar_mem(1, 0x00).vga_planar_plain_store_range(VIDEO_BASE, 1));
    assert!(plain_planar_mem(1, 0x05).vga_planar_plain_store_range(VIDEO_BASE, 1));

    // Mutual exclusion: the chain-4 state (handler Chain4) must be REJECTED.
    assert!(
        !plain_chain4_mem(1).vga_planar_plain_store_range(VIDEO_BASE, 1),
        "chain4 handler => planar predicate rejects"
    );

    // Each plain-GC departure rejects.
    let mut m = plain_planar_mem(1, 0x0F);
    m.vga.write_mode = 1;
    m.update_vga_write_handler();
    assert!(
        !m.vga_planar_plain_store_range(VIDEO_BASE, 1),
        "write_mode=1"
    );
    let mut m = plain_planar_mem(1, 0x0F);
    m.vga.data_rotate = 1;
    assert!(
        !m.vga_planar_plain_store_range(VIDEO_BASE, 1),
        "data_rotate rotate"
    );
    let mut m = plain_planar_mem(1, 0x0F);
    m.vga.data_rotate = 0x08;
    assert!(
        !m.vga_planar_plain_store_range(VIDEO_BASE, 1),
        "data_rotate logic-op"
    );
    let mut m = plain_planar_mem(1, 0x0F);
    m.vga.enable_set_reset = 1;
    assert!(
        !m.vga_planar_plain_store_range(VIDEO_BASE, 1),
        "enable_set_reset=1"
    );
    let mut m = plain_planar_mem(1, 0x0F);
    m.vga.bit_mask = 0xFE;
    assert!(
        !m.vga_planar_plain_store_range(VIDEO_BASE, 1),
        "bit_mask=0xFE"
    );
    let mut m = plain_planar_mem(1, 0x0F);
    m.vga.memory_map_select = 2;
    assert!(!m.vga_planar_plain_store_range(VIDEO_BASE, 1), "mms=2");
    let mut m = plain_planar_mem(1, 0x0F);
    m.vga.memory_map_select = 3;
    assert!(!m.vga_planar_plain_store_range(VIDEO_BASE, 1), "mms=3");

    // Handler Linear (text mode) rejects.
    let mut m = plain_planar_mem(1, 0x0F);
    m.vga.video_mode = 0x03;
    m.vga.is_mode_x = false;
    m.update_vga_write_handler();
    assert_eq!(m.vga_write_handler(), VgaWriteHandler::Linear);
    assert!(
        !m.vga_planar_plain_store_range(VIDEO_BASE, 1),
        "video_mode 0x03 => Linear"
    );

    // Aperture bounds.
    assert!(
        !base.vga_planar_plain_store_range(VIDEO_BASE - 1, 1),
        "below 0xA0000"
    );
    assert!(
        !base.vga_planar_plain_store_range(VIDEO_BASE + VIDEO_WINDOW_64K - 2, 4),
        "span crosses 0xB0000"
    );
    assert!(
        !base.vga_planar_plain_store_range(VIDEO_BASE + VIDEO_WINDOW_64K, 1),
        "at 0xB0000 (one past the aperture)"
    );
}

/// §Stage1 the fast-path arming contract: `vga_native_commit_kind() == Some(kind)`
/// MUST imply the per-store predicate holds for EVERY Map-Mask AND EVERY aperture
/// address — i.e. the flag is plane_mask- and address-independent. If it ever
/// arms for a state where some plane_mask would be rejected (e.g. the font-upload
/// pattern at plane_mask=0x04), the fast path would commit a store the interpreter
/// treats differently — this test is the guard.
#[test]
fn vga_native_commit_kind_is_plane_mask_and_address_independent() {
    // Explicit positives.
    assert_eq!(
        plain_planar_mem(1, 0x0F).vga_native_commit_kind(),
        Some(VgaCommitKind::Planar)
    );
    assert_eq!(
        plain_chain4_mem(1).vga_native_commit_kind(),
        Some(VgaCommitKind::Chain4)
    );
    // The font-upload-latch-able planar state (gc_graphics_mode=false) must NOT arm.
    let mut m = plain_planar_mem(1, 0x0F);
    m.vga.gc_graphics_mode = false;
    m.update_vga_write_handler();
    assert_eq!(
        m.vga_native_commit_kind(),
        None,
        "font-latch-able planar must not arm"
    );

    // Randomized contract check.
    let mut mem = Memory::new();
    let mut seed: u64 = 0xC0FF_EE12_3456_789A;
    let (mut n_planar, mut n_chain4) = (0usize, 0usize);
    for _ in 0..8000 {
        seed = xorshift64(seed);
        set_random_vga_state(&mut mem, seed);
        let Some(kind) = mem.vga_native_commit_kind() else {
            continue;
        };
        match kind {
            VgaCommitKind::Planar => n_planar += 1,
            VgaCommitKind::Chain4 => n_chain4 += 1,
        }
        for pm in 0u8..=0x0F {
            mem.vga.plane_mask = pm; // vary Map-Mask WITHOUT re-arming
            for &off in &[0usize, 1, 0x1234, 0xFFFF] {
                let phys = VIDEO_BASE + off;
                let holds = match kind {
                    VgaCommitKind::Planar => mem.vga_planar_plain_store_range(phys, 1),
                    VgaCommitKind::Chain4 => mem.vga_chain4_plain_store_range(phys, 1),
                };
                assert!(
                    holds,
                    "armed {kind:?} but predicate false @ off={off:#x} pm={pm:#x} state={seed:#x}"
                );
            }
        }
    }
    assert!(
        n_planar > 0,
        "no planar-armed states sampled — test vacuous"
    );
    assert!(
        n_chain4 > 0,
        "no chain4-armed states sampled — test vacuous"
    );
}

/// EXP-IQ002 panel amendment (lens 2): the S3 old-MMIO decoder window IS
/// the chain-4 aperture, and `write_u8_inner` consults it BEFORE the VGA
/// arm — so an armed commit kind while the decoder is enabled would let a
/// native/interp fast path silently bypass an architectural MMIO
/// transaction. The admission must disarm while the decoder is live and
/// re-arm on disable, without waiting for `update_vga_write_handler`.
#[test]
fn s3_mmio_decoder_disarms_the_native_commit_kind() {
    let mut m = plain_chain4_mem(1);
    assert_eq!(m.vga_native_commit_kind(), Some(VgaCommitKind::Chain4));
    m.configure_s3_mmio_decoder(true);
    assert_eq!(
        m.vga_native_commit_kind(),
        None,
        "armed kind must drop the moment the S3 MMIO decoder claims A0000"
    );
    m.configure_s3_mmio_decoder(false);
    assert_eq!(
        m.vga_native_commit_kind(),
        Some(VgaCommitKind::Chain4),
        "disable must re-arm without an update_vga_write_handler call"
    );
}

/// Companion to the S3-MMIO disarm: the S3 legacy aperture can redirect
/// (`s3_legacy_direct_lfb_offset`) or block (`s3_legacy_access_blocked`)
/// A-window bytes ahead of the VGA arm, so `active` must likewise disarm
/// the cached admission at its own configure site.
#[test]
fn s3_legacy_aperture_disarms_the_native_commit_kind() {
    let mut m = plain_chain4_mem(1);
    assert_eq!(m.vga_native_commit_kind(), Some(VgaCommitKind::Chain4));
    let config = S3LegacyApertureConfig {
        active: true,
        ..Default::default()
    };
    m.configure_s3_legacy_aperture(config);
    assert_eq!(
        m.vga_native_commit_kind(),
        None,
        "armed kind must drop while the legacy aperture can claim the window"
    );
    m.configure_s3_legacy_aperture(S3LegacyApertureConfig::default());
    assert_eq!(
        m.vga_native_commit_kind(),
        Some(VgaCommitKind::Chain4),
        "deactivation must re-arm without an update_vga_write_handler call"
    );
}

/// The real-boot S3 card in mode 13h: the legacy aperture is active but
/// answers A0000h as plain VGA (memory and A000 response on, no enhanced or
/// top-of-VRAM mapping, no linear window over the hole).
fn transparent_s3_aperture() -> S3LegacyApertureConfig {
    S3LegacyApertureConfig {
        active: true,
        memory_response: true,
        a000_response: true,
        enhanced_mapping: false,
        top_memory_access: false,
        bank_offset: 0x10000,
        linear_nibble_swap: false,
    }
}

#[test]
fn transparent_s3_legacy_aperture_keeps_the_native_commit_kind() {
    for mms in [0u8, 1] {
        let mut m = plain_chain4_mem(mms);
        m.init_lfb(1 << 20);
        m.configure_s3_legacy_aperture(transparent_s3_aperture());
        assert_eq!(
            m.vga_native_commit_kind(),
            Some(VgaCommitKind::Chain4),
            "a transparent S3 aperture must keep the chain-4 kind (mms={mms})"
        );
        let mut p = plain_planar_mem(mms, 0x0F);
        p.init_lfb(1 << 20);
        p.configure_s3_legacy_aperture(transparent_s3_aperture());
        assert_eq!(p.vga_native_commit_kind(), Some(VgaCommitKind::Planar));
    }
}

/// Byte identity under the transparent S3 aperture: `write_u8` (which runs
/// the S3 redirect/block checks first) and the native commit leave the same
/// window, planes, VRAM backing and state hash.
#[test]
fn native_commit_matches_write_u8_under_a_transparent_s3_aperture() {
    let offsets: [usize; 6] = [0, 1, 2, 3, 0x9FFF, 0xFFFF];
    let mut via_write = plain_chain4_mem(1);
    let mut via_commit = plain_chain4_mem(1);
    for m in [&mut via_write, &mut via_commit] {
        m.init_lfb(1 << 20);
        m.configure_s3_legacy_aperture(transparent_s3_aperture());
    }
    for (i, &offset) in offsets.iter().enumerate() {
        let value = 0x31u8.wrapping_mul(i as u8 + 1);
        via_write.write_u8(VIDEO_BASE + offset, value);
        via_commit.commit_vga_chain4_plain_byte(VIDEO_BASE + offset, value);
    }
    assert_eq!(via_write.planes_flat, via_commit.planes_flat);
    assert_eq!(
        &via_write.ram[VIDEO_BASE..VIDEO_BASE + VIDEO_WINDOW_64K],
        &via_commit.ram[VIDEO_BASE..VIDEO_BASE + VIDEO_WINDOW_64K]
    );
    assert_eq!(via_write.lfb(), via_commit.lfb(), "no byte may reach VRAM");
    assert_eq!(via_write.hash_state(), via_commit.hash_state());
}

/// Each S3 state that makes `write_u8` block or redirect an A0000h byte must
/// disarm the kind, and restoring the transparent state must re-arm it.
#[test]
fn s3_aperture_states_that_claim_a000_disarm_the_native_commit_kind() {
    type ApertureClaim = (&'static str, fn(&mut S3LegacyApertureConfig));
    let claims: [ApertureClaim; 4] = [
        ("no memory response", |c| c.memory_response = false),
        ("no A000 response", |c| c.a000_response = false),
        ("enhanced mapping", |c| c.enhanced_mapping = true),
        ("top-of-VRAM access", |c| c.top_memory_access = true),
    ];
    for (name, claim) in claims {
        let mut m = plain_chain4_mem(1);
        m.init_lfb(1 << 20);
        let mut config = transparent_s3_aperture();
        claim(&mut config);
        m.configure_s3_legacy_aperture(config);
        assert_eq!(m.vga_native_commit_kind(), None, "{name} must disarm");
        m.configure_s3_legacy_aperture(transparent_s3_aperture());
        assert_eq!(
            m.vga_native_commit_kind(),
            Some(VgaCommitKind::Chain4),
            "{name}: the transparent state must re-arm"
        );
    }
}

/// An S3 linear window placed over the legacy hole takes A0000h bytes ahead
/// of the VGA arm, so it disarms the kind at its own configure site. A window
/// above RAM leaves the kind armed.
#[test]
fn s3_linear_window_over_the_legacy_hole_disarms_the_native_commit_kind() {
    let mut m = plain_chain4_mem(1);
    m.init_lfb(1 << 20);
    m.configure_s3_legacy_aperture(transparent_s3_aperture());
    let high = LfbDecoderConfig::S3 {
        base: 0xE000_0000,
        size: 1 << 20,
        vram_offset: 0,
        enabled: true,
    };
    m.configure_lfb_decoder(high);
    assert_eq!(m.vga_native_commit_kind(), Some(VgaCommitKind::Chain4));
    let over_hole = LfbDecoderConfig::S3 {
        base: VIDEO_BASE,
        size: 0x10000,
        vram_offset: 0,
        enabled: true,
    };
    m.configure_lfb_decoder(over_hole);
    assert_eq!(
        m.vga_native_commit_kind(),
        None,
        "a linear window over A0000h must disarm"
    );
    m.configure_lfb_decoder(high);
    assert_eq!(m.vga_native_commit_kind(), Some(VgaCommitKind::Chain4));
}

/// The REP span commit equals the per-store commit applied byte by byte, in
/// order, for both kinds and several Map Masks: planes, linear mirror, dirty
/// pages, bus and native-write counts, and the VGA parity hash.
#[test]
fn span_commit_matches_the_per_store_commit_byte_by_byte() {
    let bytes: Vec<u8> = (0..0x1800u32)
        .map(|i| (i as u8).wrapping_mul(29) ^ (i >> 8) as u8)
        .collect();
    for (offset, len) in [
        (0usize, 0x1000usize),
        (0x0FF3, 0x0D),
        (0x1001, 0x1800),
        (0xF000, 0x1000),
    ] {
        let fresh: [fn() -> Memory; 3] = [
            || plain_chain4_mem(1),
            || plain_planar_mem(1, 0x0F),
            || plain_planar_mem(0, 0b1010),
        ];
        for make in fresh {
            let mut per_store = make();
            let mut span = make();
            let kind = per_store.vga_native_commit_kind().expect("armed");
            per_store.video_dirty_pages.store(0, Ordering::Relaxed);
            span.video_dirty_pages.store(0, Ordering::Relaxed);
            let phys = VIDEO_BASE + offset;
            for (i, &byte) in bytes[..len].iter().enumerate() {
                per_store.drc_commit_vga_native_span(kind, phys + i, u64::from(byte), 1);
            }
            span.drc_commit_vga_native_bytes(kind, phys, &bytes[..len]);
            let what = format!("{kind:?} at {offset:#x}+{len:#x}");
            assert_eq!(per_store.planes_flat, span.planes_flat, "{what}: planes");
            assert_eq!(
                &per_store.ram[VIDEO_BASE..VIDEO_BASE + VIDEO_WINDOW_64K],
                &span.ram[VIDEO_BASE..VIDEO_BASE + VIDEO_WINDOW_64K],
                "{what}: linear mirror"
            );
            assert_eq!(
                per_store.video_dirty_pages.load(Ordering::Relaxed),
                span.video_dirty_pages.load(Ordering::Relaxed),
                "{what}: dirty pages"
            );
            assert_eq!(
                per_store.vga_bus_write_count(),
                span.vga_bus_write_count(),
                "{what}"
            );
            assert_eq!(
                per_store.native_video_writes(),
                span.native_video_writes(),
                "{what}"
            );
            assert_eq!(
                per_store.hash_vga_parity(),
                span.hash_vga_parity(),
                "{what}: VGA hash"
            );
        }
    }
}

// ── chain-4 stores must mark the page dirty ──

/// The whole point: a native chain-4 commit must make the touched page
/// visible to the renderer, exactly as the interpreter's chain-4 path and
/// the native PLANAR commit already do.
#[test]
fn native_chain4_commit_marks_the_touched_page_dirty() {
    let mut mem = plain_chain4_mem(1);
    mem.video_dirty_pages.store(0, Ordering::Relaxed);

    mem.commit_vga_chain4_plain_byte(VIDEO_BASE, 0xAB);

    assert_eq!(
        mem.video_dirty_pages.load(Ordering::Relaxed) & 1,
        1,
        "a chain-4 store into page 0 must mark page 0 dirty"
    );
}

/// It must mark the page it actually touched, not page 0 by accident.
#[test]
fn native_chain4_commit_marks_the_correct_page() {
    let mut mem = plain_chain4_mem(1);
    mem.video_dirty_pages.store(0, Ordering::Relaxed);

    // Page 3 of the aperture.
    let addr = VIDEO_BASE + 3 * 0x1000;
    mem.commit_vga_chain4_plain_byte(addr, 0xCD);

    assert_eq!(
        mem.video_dirty_pages.load(Ordering::Relaxed),
        1 << 3,
        "exactly the touched page must be marked, and no other"
    );
}

/// The fix must not over-mark. Marking every page would satisfy a naive
/// "is it dirty" assertion while destroying the unchanged-frame skip that
/// publication relies on (MDD-BUG-CRUCIBLE-00347).
#[test]
fn native_chain4_commit_does_not_dirty_untouched_pages() {
    let mut mem = plain_chain4_mem(1);
    mem.video_dirty_pages.store(0, Ordering::Relaxed);

    mem.commit_vga_chain4_plain_byte(VIDEO_BASE + 0x1000, 0x11);
    let after_one = mem.video_dirty_pages.load(Ordering::Relaxed);
    assert_eq!(after_one.count_ones(), 1, "one store, one dirty page");
}

/// The planar sibling's behaviour is unchanged — it already marked, and
/// still must.
#[test]
fn native_planar_commit_still_marks_its_page() {
    let mut mem = plain_planar_mem(1, 0x0F);
    mem.video_dirty_pages.store(0, Ordering::Relaxed);

    mem.commit_vga_planar_plain_byte(VIDEO_BASE, 0x5A);

    assert_eq!(mem.video_dirty_pages.load(Ordering::Relaxed) & 1, 1);
}

/// M1 S1: `store_would_dirty_instr_cache` answers exactly the question
/// `mark_instr_cache_dirty_range` decides for an aperture store: an
/// exec-marked page in the span owes a mark only while the flag is clear,
/// and an unmarked span never does.
#[test]
fn store_would_dirty_instr_cache_matches_the_commit_seams_mark() {
    let mut mem = Memory::new();
    let _ = mem.take_instr_cache_dirty();
    assert!(!mem.store_would_dirty_instr_cache(VIDEO_BASE, VIDEO_WINDOW_64K));

    mem.mark_instr_exec_page(VIDEO_BASE + 0x3000);
    assert!(mem.store_would_dirty_instr_cache(VIDEO_BASE, VIDEO_WINDOW_64K));
    assert!(mem.store_would_dirty_instr_cache(VIDEO_BASE + 0x3FFF, 1));
    assert!(!mem.store_would_dirty_instr_cache(VIDEO_BASE + 0x2FFF, 1));
    assert!(!mem.store_would_dirty_instr_cache(VIDEO_BASE + 0x4000, 4));
    assert!(!mem.store_would_dirty_instr_cache(VIDEO_BASE, 0));

    // The real seam agrees: a commit into the marked page sets the flag,
    // after which nothing more is owed.
    let mut planar = Memory::new();
    planar.vga.video_mode = 0x13;
    planar.vga.chain4 = false;
    planar.vga.is_mode_x = true;
    planar.vga.memory_map_select = 1;
    planar.vga.gc_graphics_mode = true;
    planar.update_vga_write_handler();
    let _ = planar.take_instr_cache_dirty();
    planar.mark_instr_exec_page(VIDEO_BASE + 0x3000);
    assert!(planar.store_would_dirty_instr_cache(VIDEO_BASE + 0x3010, 4));
    planar.drc_commit_vga_native_span(VgaCommitKind::Planar, VIDEO_BASE + 0x3010, 0x11, 4);
    assert!(planar.is_instr_cache_dirty());
    assert!(!planar.store_would_dirty_instr_cache(VIDEO_BASE + 0x3010, 4));
}
