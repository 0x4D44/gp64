//! `Memory::peek_video_range` must equal `peek_video_u8` byte for byte in
//! every routing state, and must take the bulk route for every 4 KiB page of
//! the legacy window when the routing is uniform over it.
use super::*;

fn xorshift64(mut x: u64) -> u64 {
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x
}

fn fill_random(bytes: &mut [u8], seed: &mut u64) {
    for byte in bytes {
        *seed = xorshift64(*seed);
        *byte = (*seed >> 24) as u8;
    }
}

/// A `Memory` with random RAM under the legacy window, random planes and a
/// random VRAM backing of `lfb_len` bytes (fixed seed).
fn random_mem_with_lfb(seed: u64, lfb_len: usize) -> Memory {
    let mut mem = Memory::new();
    mem.init_lfb(lfb_len);
    let mut s = seed;
    fill_random(&mut mem.ram[VIDEO_BASE..VIDEO_BASE + VIDEO_SIZE], &mut s);
    fill_random(&mut mem.planes_flat[..], &mut s);
    fill_random(mem.lfb_mut(), &mut s);
    mem
}

fn random_mem(seed: u64) -> Memory {
    random_mem_with_lfb(seed, 1024 * 1024)
}

fn per_byte(mem: &Memory, base: usize, len: usize) -> Vec<u8> {
    (0..len).map(|j| mem.peek_video_u8(base + j)).collect()
}

/// Compare every 4 KiB page of `A0000h..C0000h` plus a batch of random
/// sub-ranges; return how many aligned pages the bulk route declined.
fn check(mem: &Memory, label: &str, seed: u64) -> usize {
    let mut declined = 0;
    for page in 0..VIDEO_SIZE / 4096 {
        let base = VIDEO_BASE + page * 4096;
        let expected = per_byte(mem, base, 4096);
        let mut bulk = [0u8; 4096];
        if !mem.peek_video_range_bulk(base, &mut bulk) {
            declined += 1;
        } else {
            assert!(
                bulk[..] == expected[..],
                "{label}: bulk route diverges at page {base:#X}"
            );
        }
        let mut out = [0u8; 4096];
        mem.peek_video_range(base, &mut out);
        assert!(
            out[..] == expected[..],
            "{label}: peek_video_range diverges at page {base:#X}"
        );
    }
    let mut s = seed;
    for _ in 0..64 {
        s = xorshift64(s);
        let base = VIDEO_BASE + (s as usize % VIDEO_SIZE);
        s = xorshift64(s);
        let len = (s as usize % 6000).min(VIDEO_BASE + VIDEO_SIZE - base);
        let mut out = vec![0u8; len];
        mem.peek_video_range(base, &mut out);
        assert_eq!(
            out,
            per_byte(mem, base, len),
            "{label}: range {base:#X}+{len:#X} diverges"
        );
    }
    declined
}

fn aperture(
    memory_response: bool,
    a000_response: bool,
    enhanced_mapping: bool,
    top_memory_access: bool,
    bank_offset: usize,
    linear_nibble_swap: bool,
) -> S3LegacyApertureConfig {
    S3LegacyApertureConfig {
        active: true,
        memory_response,
        a000_response,
        enhanced_mapping,
        top_memory_access,
        bank_offset,
        linear_nibble_swap,
    }
}

/// Every VGA read-routing state (text RAM, chain-4, planar, Mode X, text
/// font-plane read) crossed with every map window and read plane, with and
/// without a transparent S3 legacy aperture.
#[test]
fn peek_video_range_matches_per_byte_peek_across_vga_states() {
    let mut seed = 0x9E37_79B9_7F4A_7C15;
    let mut bulk_chain4 = false;
    let mut bulk_planar = false;
    for transparent_s3 in [false, true] {
        for mode in [0x03u8, 0x07, 0x0D, 0x12, 0x13] {
            for chain4 in [false, true] {
                for mode_x in [false, true] {
                    for gc_odd_even_read in [false, true] {
                        for gc_graphics_mode in [false, true] {
                            seed = xorshift64(seed);
                            let mut mem = random_mem_with_lfb(seed, 0);
                            if transparent_s3 {
                                mem.configure_s3_legacy_aperture(aperture(
                                    true, true, false, false, 0, false,
                                ));
                            }
                            for map in 0..4u8 {
                                for read_plane in 0..4u8 {
                                    seed = xorshift64(seed);
                                    mem.vga.video_mode = mode;
                                    mem.vga.chain4 = chain4;
                                    mem.vga.is_mode_x = mode_x;
                                    mem.vga.gc_graphics_mode = gc_graphics_mode;
                                    mem.vga.gc_odd_even_read = gc_odd_even_read;
                                    mem.vga.memory_map_select = map;
                                    mem.vga.read_plane = read_plane;
                                    let label = format!(
                                    "s3={transparent_s3} mode={mode:#X} chain4={chain4} \
                                     mode_x={mode_x} gc={gc_graphics_mode} odd_even_read={gc_odd_even_read} map={map} \
                                     plane={read_plane}"
                                );
                                    assert_eq!(check(&mem, &label, seed), 0, "{label}: declined");
                                    bulk_chain4 |= mode == 0x13 && chain4;
                                    bulk_planar |=
                                        mem.is_planar_mode() || mem.is_cpu_sequential_planar_read();
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(
        bulk_chain4 && bulk_planar,
        "chain-4 and planar arms exercised"
    );
}

/// S3 legacy aperture blocked, direct-to-VRAM (enhanced mapping and CR3A
/// top-of-memory, with and without mid-page VRAM wrap), and the relocatable
/// S3 linear window over the legacy hole (nibble swap on and off, aligned and
/// mid-page window edges).
#[test]
fn peek_video_range_matches_per_byte_peek_across_s3_routes() {
    let mut seed = 0xD1B5_4A32_D192_ED03;
    let mut next = || {
        seed = xorshift64(seed);
        seed
    };
    let vga_state = |mem: &mut Memory| {
        mem.vga.video_mode = 0x12;
        mem.vga.read_plane = 1;
    };

    // Blocked: no memory response; A000 decode off; enhanced mapping's B000
    // half. Every page is uniform.
    for config in [
        aperture(false, true, false, false, 0, false),
        aperture(true, false, false, false, 0, false),
        aperture(true, false, true, false, 0, false),
    ] {
        let mut mem = random_mem(next());
        vga_state(&mut mem);
        mem.configure_s3_legacy_aperture(config);
        assert_eq!(check(&mem, &format!("blocked {config:?}"), next()), 0);
    }

    // Direct window: page-aligned bank offsets are uniform on every page.
    for (enhanced, top, bank) in [
        (true, false, 0usize),
        (true, false, 0x30000),
        (true, false, 0xF0000),
        (false, true, 0),
        (true, true, 0x10000),
    ] {
        let mut mem = random_mem(next());
        vga_state(&mut mem);
        mem.configure_s3_legacy_aperture(aperture(true, true, enhanced, top, bank, false));
        let label = format!("direct enhanced={enhanced} top={top} bank={bank:#X}");
        assert_eq!(check(&mem, &label, next()), 0, "{label}");
    }
    // Direct window wrapping mid-page (bank offset near the end of VRAM, or
    // a VRAM backing smaller than the CR3A window): the wrapping page must
    // decline and fall back.
    for (lfb_len, top, bank) in [(1024 * 1024, false, 0xFF800usize), (0x6800, true, 0)] {
        let mut mem = random_mem_with_lfb(next(), lfb_len);
        vga_state(&mut mem);
        mem.configure_s3_legacy_aperture(aperture(true, true, !top, top, bank, false));
        let label = format!("direct wrap lfb={lfb_len:#X} top={top} bank={bank:#X}");
        assert!(
            check(&mem, &label, next()) > 0,
            "{label}: wrap page declined"
        );
    }

    // Relocatable S3 linear window over A0000h.
    for swap in [false, true] {
        for (base, size, vram_offset, uniform) in [
            (VIDEO_BASE, 0x20000usize, 0usize, true),
            (VIDEO_BASE, 0x10000, 0x2000, true),
            (0xB0000, 0x10000, 0x40000, true),
            (0xA8800, 0x800, 0, false),
            (VIDEO_BASE, 0x20000, 0xFF800, false),
        ] {
            let mut mem = random_mem(next());
            vga_state(&mut mem);
            mem.configure_s3_legacy_aperture(aperture(true, true, false, false, 0, swap));
            mem.configure_lfb_decoder(LfbDecoderConfig::S3 {
                base,
                size,
                vram_offset,
                enabled: true,
            });
            let label =
                format!("s3 lfb base={base:#X} size={size:#X} vram={vram_offset:#X} swap={swap}");
            let declined = check(&mem, &label, next());
            assert_eq!(declined == 0, uniform, "{label}: declined {declined}");
        }
    }

    // DISPI decoder enabled: its fixed apertures never reach A0000h.
    let mut mem = random_mem(next());
    vga_state(&mut mem);
    mem.configure_lfb_decoder(LfbDecoderConfig::Dispi { enabled: true });
    assert_eq!(check(&mem, "dispi", next()), 0);
}

/// Usable RAM ending inside the legacy window: the straddling page declines,
/// pages wholly above it read open bus.
#[test]
fn peek_video_range_matches_per_byte_peek_with_ram_ending_in_the_window() {
    let mut mem = random_mem(0x0123_4567_89AB_CDEF);
    mem.resize_ram(0xA8800);
    mem.vga.video_mode = 0x03;
    assert_eq!(check(&mem, "short ram", 0x55AA), 1);
}
