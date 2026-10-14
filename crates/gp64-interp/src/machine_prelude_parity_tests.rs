//! M2.2 parity oracle: the `vga_aware_*` funnels, now built on PC
//! `Memory`'s `X86Machine` access preludes, against verbatim copies of the
//! pre-M2.2 funnels on twin memories (LLD 2026.10.03 "M2 machine-generic
//! interpreter map" §1.1, §4).
//!
//! The legacy copies below keep every routing decision of the funnels as
//! they stood at M2.1 (`2c8ebe569b`), minus the observers, which never touch
//! memory. Every scenario must leave both twins with identical RAM,
//! identical VGA read-back and identical VGA bus write counts, and every read
//! must return the same value.

use super::*;
use gp64_types::VgaPipelineState;

type Scenario = (&'static str, fn(&mut Memory));

fn chain4_pipeline() -> VgaPipelineState {
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

fn scenarios() -> [Scenario; 8] {
    [
        ("default", |_| {}),
        ("rom_writes", |m| m.set_allow_rom_writes(true)),
        ("rom_page", |m| m.mark_rom_page(0xC8)),
        ("option_rom", |m| {
            m.set_option_rom_backing(&[0x55; 0x800]);
            m.configure_option_rom_decoder(0xD0000, true);
        }),
        ("ems", |m| {
            m.init_ems_pool(4).expect("EMS pool");
            m.claim_hle_ems_frame(EMS_FRAME_BASE, EMS_FRAME_SIZE)
                .expect("claim the EMS frame");
            m.set_ems_frame_page(0, Some(0)).expect("map slot 0");
            m.set_ems_frame_page(1, Some(1)).expect("map slot 1");
        }),
        ("chain4", |m| m.apply_vga_pipeline(&chain4_pipeline())),
        ("a20_off", |m| m.set_a20(false)),
        ("gdt_snoop", |m| m.set_gdt_snoop_range(0x5000, 0xFFF)),
    ]
}

/// The run-entry hoist, as `run_ext_inner_core` builds it.
fn guards_for(mem: &Memory) -> WriteGuards<PcAccessPolicy> {
    WriteGuards {
        policy: mem.access_policy(),
        diag: InterpDiag::default(),
    }
}

fn addresses(mem: &Memory) -> Vec<usize> {
    let end = mem.usable_len();
    let mut v = vec![
        0x1000, 0x5000, 0x5FFE, 0x9FFFC, 0x9FFFD, 0x9FFFE, 0x9FFFF, 0xA0000, 0xA0001, 0xA0002,
        0xA0003, 0xA0004, 0xAFFFE, 0xAFFFF, 0xB0000, 0xB8000, 0xBFFFE, 0xBFFFF, 0xC0000, 0xC7FFE,
        0xC7FFF, 0xC8000, 0xC8FFF, 0xCFFFE, 0xD0000, 0xD07FF, 0xD0800, 0xDFFFE, 0xDFFFF, 0xE0000,
        0xE3FFF, 0xEFFFE, 0xEFFFF, 0xF0000, 0xFFFFE, 0xFFFFF, 0x10_0000, 0x10_FFEF,
    ];
    v.extend([end - 4, end - 3, end - 2, end - 1, end]);
    v
}

// ── The pre-M2.2 funnels (M2.1, `2c8ebe569b`), observers removed ──────────

unsafe fn legacy_write_u8(ram: *mut u8, addr: usize, val: u8, mem: &mut Memory) -> *mut u8 {
    let allow_rom_writes = mem.allow_rom_writes;
    if addr >= VIDEO_BASE && addr < VGA_END {
        if mem.vga_native_commit_kind() == Some(VgaCommitKind::Chain4)
            && addr < VIDEO_BASE + VIDEO_WINDOW_64K
        {
            mem.interp_commit_vga_chain4_fast(addr, val);
            return ram;
        }
        mem.write_u8(addr, val);
        return mem.ram_ptr_mut();
    }
    if !allow_rom_writes && addr >= 0xF0000 && addr < 0x100000 {
        return ram;
    }
    if mem.is_rom_addr(addr) {
        return ram;
    }
    if mem.option_rom_region_overlaps(addr, 1) {
        return ram;
    }
    if ems_frame_overlaps(mem, addr, 1) {
        mem.write_u8(addr, val);
        return mem.ram_ptr_mut();
    }
    if addr >= mem.usable_len() {
        mem.write_u8(addr, val);
        return mem.ram_ptr_mut();
    }
    #[cfg(debug_assertions)]
    if mem.is_write_tracking_enabled() {
        mem.write_u8(addr, val);
        return mem.ram_ptr_mut();
    }
    if mem.has_smc_notifier() {
        mem.smc_before_write(addr, 1);
    }
    if mem.has_descriptor_snoop() {
        mem.snoop_gdt_write_ext(addr, val as u64, 1);
    }
    write_u8_fast(ram, addr, val);
    ram
}

unsafe fn legacy_write_u16(ram: *mut u8, addr: usize, val: u16, mem: &mut Memory) -> *mut u8 {
    let allow_rom_writes = mem.allow_rom_writes;
    if (addr >= VIDEO_BASE && addr.wrapping_add(1) < VGA_END)
        || (addr.wrapping_add(1) >= VIDEO_BASE && addr < VGA_END)
    {
        if mem.vga_native_commit_kind() == Some(VgaCommitKind::Chain4)
            && addr >= VIDEO_BASE
            && addr + 2 <= VIDEO_BASE + VIDEO_WINDOW_64K
            && (addr & 3) <= 2
        {
            let bytes = val.to_le_bytes();
            mem.interp_commit_vga_chain4_fast(addr, bytes[0]);
            mem.interp_commit_vga_chain4_fast(addr + 1, bytes[1]);
            return ram;
        }
        mem.write_u16(addr, val);
        return mem.ram_ptr_mut();
    }
    if (!allow_rom_writes && addr < 0x100000 && addr.wrapping_add(1) >= 0xF0000)
        || mem.is_rom_addr(addr)
        || mem.is_rom_addr(addr.wrapping_add(1))
        || mem.option_rom_region_overlaps(addr, 2)
        || ems_frame_overlaps(mem, addr, 2)
    {
        let ram = legacy_write_u8(ram, addr, val as u8, mem);
        return legacy_write_u8(ram, addr.wrapping_add(1), (val >> 8) as u8, mem);
    }
    if addr.wrapping_add(1) >= mem.usable_len() {
        mem.write_u16(addr, val);
        return mem.ram_ptr_mut();
    }
    #[cfg(debug_assertions)]
    if mem.is_write_tracking_enabled() {
        mem.write_u16(addr, val);
        return mem.ram_ptr_mut();
    }
    if mem.has_smc_notifier() {
        mem.smc_before_write(addr, 2);
    }
    if mem.has_descriptor_snoop() {
        mem.snoop_gdt_write_ext(addr, val as u64, 2);
    }
    write_u16_fast(ram, addr, val);
    ram
}

unsafe fn legacy_write_u32(ram: *mut u8, addr: usize, val: u32, mem: &mut Memory) -> *mut u8 {
    let allow_rom_writes = mem.allow_rom_writes;
    if (addr >= VIDEO_BASE && addr.wrapping_add(3) < VGA_END)
        || (addr.wrapping_add(3) >= VIDEO_BASE && addr < VGA_END)
    {
        if mem.vga_native_commit_kind() == Some(VgaCommitKind::Chain4)
            && addr >= VIDEO_BASE
            && addr + 4 <= VIDEO_BASE + VIDEO_WINDOW_64K
            && (addr & 3) == 0
        {
            for (i, b) in val.to_le_bytes().iter().copied().enumerate() {
                mem.interp_commit_vga_chain4_fast(addr + i, b);
            }
            return ram;
        }
        mem.write_u32(addr, val);
        return mem.ram_ptr_mut();
    }
    if (!allow_rom_writes && addr < 0x100000 && addr.wrapping_add(3) >= 0xF0000)
        || mem.is_rom_addr(addr)
        || mem.is_rom_addr(addr.wrapping_add(3))
        || mem.option_rom_region_overlaps(addr, 4)
        || ems_frame_overlaps(mem, addr, 4)
    {
        let mut ram = ram;
        for (i, byte) in val.to_le_bytes().iter().copied().enumerate() {
            ram = legacy_write_u8(ram, addr.wrapping_add(i), byte, mem);
        }
        return ram;
    }
    if addr.wrapping_add(3) >= mem.usable_len() {
        mem.write_u32(addr, val);
        return mem.ram_ptr_mut();
    }
    #[cfg(debug_assertions)]
    if mem.is_write_tracking_enabled() {
        mem.write_u32(addr, val);
        return mem.ram_ptr_mut();
    }
    if mem.has_smc_notifier() {
        mem.smc_before_write(addr, 4);
    }
    if mem.has_descriptor_snoop() {
        mem.snoop_gdt_write_ext(addr, val as u64, 4);
    }
    write_u32_fast(ram, addr, val);
    ram
}

unsafe fn legacy_read(ram: *const u8, width: usize, addr: usize, mem: &Memory) -> u32 {
    match width {
        1 => {
            if ems_frame_overlaps(mem, addr, 1)
                || addr >= mem.usable_len()
                || (addr >= VIDEO_BASE && addr < VGA_END)
                || mem.option_rom_region_overlaps(addr, 1)
            {
                u32::from(mem.read_u8(addr))
            } else {
                u32::from(read_u8_fast(ram, addr))
            }
        }
        2 => {
            if ems_frame_overlaps(mem, addr, 2)
                || addr.wrapping_add(1) >= mem.usable_len()
                || (addr >= VIDEO_BASE && addr.wrapping_add(1) < VGA_END)
                || (addr.wrapping_add(1) >= VIDEO_BASE && addr < VGA_END)
                || mem.option_rom_region_overlaps(addr, 2)
            {
                u32::from(mem.read_u16(addr))
            } else {
                u32::from(read_u16_fast(ram, addr))
            }
        }
        _ => {
            if ems_frame_overlaps(mem, addr, 4)
                || mem.option_rom_region_overlaps(addr, 4)
                || (addr.wrapping_add(3) >= VIDEO_BASE && addr < VGA_END)
                || addr.wrapping_add(3) >= mem.usable_len()
            {
                mem.read_u32(addr)
            } else {
                read_u32_fast(ram, addr)
            }
        }
    }
}

fn legacy_funnel_write(mem: &mut Memory, width: usize, addr: usize, val: u64) {
    let ram = mem.ram_ptr_mut();
    // SAFETY: the legacy funnels bounds-check before any raw store.
    unsafe {
        match width {
            1 => legacy_write_u8(ram, addr, val as u8, mem),
            2 => legacy_write_u16(ram, addr, val as u16, mem),
            _ => legacy_write_u32(ram, addr, val as u32, mem),
        };
    }
}

fn legacy_funnel_read(mem: &Memory, width: usize, addr: usize) -> u32 {
    // SAFETY: the legacy funnels bounds-check before any raw load.
    unsafe { legacy_read(mem.ram_ptr(), width, addr, mem) }
}

fn funnel_write(mem: &mut Memory, width: usize, addr: usize, val: u64) {
    let g = guards_for(mem);
    let ram = mem.ram_ptr_mut();
    // SAFETY: the funnels bounds-check before any raw store.
    unsafe {
        match width {
            1 => vga_aware_write_u8(ram, addr, val as u8, mem, g),
            2 => vga_aware_write_u16(ram, addr, val as u16, mem, g),
            _ => vga_aware_write_u32(ram, addr, val as u32, mem, g),
        };
    }
}

fn funnel_read(mem: &Memory, width: usize, addr: usize) -> u32 {
    let ram = mem.ram_ptr();
    // SAFETY: the funnels bounds-check before any raw load.
    unsafe {
        match width {
            1 => u32::from(vga_aware_read_u8(ram, addr, mem)),
            2 => u32::from(vga_aware_read_u16(ram, addr, mem)),
            _ => vga_aware_read_u32(ram, addr, mem),
        }
    }
}

fn assert_twins_agree(what: &str, a: &Memory, b: &Memory, addr: usize) {
    assert_eq!(
        a.vga_bus_write_count(),
        b.vga_bus_write_count(),
        "{what}: VGA bus write count"
    );
    let len = a.usable_len().min(0x11_0000);
    let (ra, rb) = (&a.ram[..len], &b.ram[..len]);
    if ra != rb {
        let first = ra.iter().zip(rb).position(|(x, y)| x != y).unwrap();
        panic!(
            "{what}: RAM differs first at {first:#x} (legacy {:#04x}, funnel {:#04x})",
            ra[first], rb[first]
        );
    }
    for off in addr.saturating_sub(4)..addr.saturating_add(8) {
        if (VIDEO_BASE..VIDEO_BASE + 0x20000).contains(&off) {
            assert_eq!(
                a.read_u8(off),
                b.read_u8(off),
                "{what}: VGA read-back at {off:#x}"
            );
        }
    }
}

#[test]
fn prelude_funnels_store_exactly_what_the_legacy_funnels_store() {
    for (name, setup) in scenarios() {
        for width in [1usize, 2, 4] {
            let mut seed = Memory::new();
            setup(&mut seed);
            for addr in addresses(&seed) {
                let mut via_legacy = Memory::new();
                let mut via_funnel = Memory::new();
                setup(&mut via_legacy);
                setup(&mut via_funnel);
                let val = 0xA1B2_C3D4u64 ^ (addr as u64);
                let mask = if width == 4 {
                    0xFFFF_FFFF
                } else {
                    (1u64 << (8 * width)) - 1
                };
                legacy_funnel_write(&mut via_legacy, width, addr, val & mask);
                funnel_write(&mut via_funnel, width, addr, val & mask);
                assert_twins_agree(
                    &format!("{name} w{width} @{addr:#x}"),
                    &via_legacy,
                    &via_funnel,
                    addr,
                );
            }
        }
    }
}

#[test]
fn prelude_funnels_load_exactly_what_the_legacy_funnels_load() {
    for (name, setup) in scenarios() {
        for width in [1usize, 2, 4] {
            let mut seed = Memory::new();
            setup(&mut seed);
            for addr in addresses(&seed) {
                let mut a = Memory::new();
                let mut b = Memory::new();
                setup(&mut a);
                setup(&mut b);
                // Distinct content everywhere a read can land.
                for m in [&mut a, &mut b] {
                    for (i, off) in (addr.saturating_sub(8)..addr.saturating_add(8)).enumerate() {
                        if off < m.usable_len() {
                            m.ram[off] = 0x30 + i as u8;
                        }
                    }
                }
                let got = legacy_funnel_read(&a, width, addr);
                let want = funnel_read(&b, width, addr);
                assert_eq!(got, want, "{name} r{width} @{addr:#x}");
                assert_twins_agree(&format!("{name} r{width} @{addr:#x}"), &a, &b, addr);
            }
        }
    }
}
