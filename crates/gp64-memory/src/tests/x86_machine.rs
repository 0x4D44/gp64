//! PC `Memory` against the `X86Machine` contract (M1), and the TLB
//! publication oracle for the `SoftwareTlb::insert` -> `tlb_fill` move.

use super::*;
use gp64_memory_core::{
    AccessIntent, AccessPolicyCore, ArchPhys, BackingPage, GuestPhys, MachineId, PhysPage,
    PortSync, X86Machine,
};

/// A named policy set-up applied to a fresh `Memory`.
type Scenario = (&'static str, fn(&mut Memory));

/// Every PC `tlb_insert` outcome across the predicates that moved out of the
/// core (VGA window, RAM bound) and the ones that stayed in the PC wrapper
/// (F-segment, MP table, descriptor snoop, A20), as text rows.
fn tlb_publication_rows() -> Vec<String> {
    let scenarios: [Scenario; 6] = [
        ("default", |_| {}),
        ("rom_writes", |m| m.set_allow_rom_writes(true)),
        ("mp_suppressed", |m| {
            m.set_firmware_mp_table_suppression_enabled(true);
        }),
        ("gdt_snoop", |m| m.set_gdt_snoop_range(0x5000, 0xFFF)),
        ("ldt_snoop", |m| m.set_ldt_snoop_range(0x9F800, 0x7FF)),
        ("a20_off", |m| m.set_a20(false)),
    ];
    let pages: [u32; 18] = [
        0x00, 0x05, 0x06, 0x9F, 0xA0, 0xB8, 0xBF, 0xC0, 0xC8, 0xEF, 0xF0, 0xFF, 0x100, 0x105,
        0x10F, 0x110, 0x1FF, 0x8000,
    ];
    let perms = [
        (true, true, true),
        (true, false, false),
        (false, true, true),
        (true, true, false),
    ];
    let mut rows = Vec::new();
    for (name, setup) in scenarios {
        let mut mem = Memory::new();
        setup(&mut mem);
        let base = mem.ram.host_base() as usize;
        let off = |p: usize| {
            if p == 0 {
                "-".to_string()
            } else {
                format!("{:x}", p.wrapping_sub(base))
            }
        };
        for (i, &page) in pages.iter().enumerate() {
            for (j, &(w, u, d)) in perms.iter().enumerate() {
                let linear = 0x400 + (i * perms.len() + j) as u32;
                mem.tlb_insert(linear, page, w, u, d);
                // SAFETY: the index is inside the 4096-entry table.
                let e = unsafe { *mem.tlb_entries_ptr().add(SoftwareTlb::index(linear)) };
                rows.push(format!(
                    "{name} p={page:x} {}{}{} tag={:x} r={} w={} pp={:x} f={:x}",
                    u8::from(w),
                    u8::from(u),
                    u8::from(d),
                    e.linear_page,
                    off(e.host_read as usize),
                    off(e.host_write as usize),
                    e.physical_page,
                    e.flags
                ));
            }
        }
    }
    rows
}

/// LLD 6 (M1): the PC wrapper must publish exactly what the parent did. The
/// digest was recorded by running `tlb_publication_rows` on the parent commit
/// (`a278ab9be6`), where `SoftwareTlb::insert` still tested the VGA window and
/// the RAM bound itself.
#[test]
fn tlb_insert_publishes_exactly_what_the_parent_published() {
    let rows = tlb_publication_rows();
    let digest = blake3::hash(rows.join("\n").as_bytes()).to_hex();
    assert_eq!(
        (rows.len(), digest.as_str()),
        (432, PARENT_TLB_PUBLICATION_DIGEST),
        "TLB publication drifted from the parent; rows:\n{}",
        rows.join("\n")
    );
}

const PARENT_TLB_PUBLICATION_DIGEST: &str =
    "669897dec4cd98ab72634b0027542bbff8c4409e0fdcd158d812420665a76512";

/// The publication proof never admits the VGA window (the test that left
/// `SoftwareTlb::insert`).
#[test]
fn tlb_insert_never_publishes_a_vga_window_page() {
    let mut mem = Memory::new();
    for page in 0xA0..0xC0u32 {
        mem.tlb_insert(page, page, true, true, true);
        // SAFETY: the index is inside the 4096-entry table.
        let e = unsafe { *mem.tlb_entries_ptr().add(SoftwareTlb::index(page)) };
        assert!(
            e.host_read.is_null() && e.host_write.is_null(),
            "page {page:#x}"
        );
    }
}

fn assert_machine<M: X86Machine>() {}

#[test]
fn pc_memory_is_the_audited_pc_machine() {
    assert_machine::<Memory>();
    assert_eq!(Memory::MACHINE_ID, MachineId::PC);
    const {
        assert!(!Memory::DEVICE_DISPATCH);
        assert!(Memory::NATIVE_DEVICE_COMMIT);
        assert!(Memory::WRITE_TRACKING);
        assert!(Memory::DPMI_MAPPINGS);
    }
    let mem = Memory::new();
    assert!(std::ptr::eq(X86Machine::core(&mem), &mem.core));
}

/// The captured policy equals the live state it replaces (R-AP1).
#[test]
fn pc_access_policy_captures_the_live_hoists() {
    let mut mem = Memory::new();
    let p = mem.access_policy();
    assert!(p.a20_enabled);
    assert_eq!(p.linear_mask(), 0xFFFF_FFFF);
    assert_eq!(
        (
            p.smc_notifier,
            p.descriptor_snoop,
            p.write_tracking,
            p.allow_rom_writes
        ),
        (false, false, false, mem.allow_rom_writes)
    );
    assert_eq!(p.traced_writes, mem.has_traced_write_config());

    mem.set_a20(false);
    mem.set_allow_rom_writes(true);
    mem.set_gdt_snoop_range(0x5000, 0xFF);
    mem.enable_write_tracking();
    let p = mem.access_policy();
    assert!(!p.a20_enabled);
    assert_eq!(p.linear_mask(), !(1u32 << 20));
    assert!(p.allow_rom_writes && p.write_tracking);
    assert!(p.descriptor_snoop() && p.descriptor_snoop);
    assert!(!p.smc_notifier());
}

/// `phys_mask` is `Memory::mask_address` for every address, including the
/// flat-memory-layout physical range above 4 GiB that a `u32` mask would truncate.
#[test]
fn pc_phys_mask_matches_mask_address_in_both_a20_states() {
    let mut mem = Memory::new();
    for enabled in [true, false] {
        mem.set_a20(enabled);
        let p = mem.access_policy();
        for addr in [
            0usize,
            0xF_FFFF,
            0x10_0000,
            0x10_FFEF,
            0x1F_FFFF,
            0x30_0000,
            0xFFFF_FFFF,
            0x1_00FE_FFFF,
            0x1_0010_0000,
        ] {
            assert_eq!(
                Memory::phys_mask(p, ArchPhys(addr)),
                GuestPhys(mem.mask_address(addr)),
                "a20={enabled} addr={addr:#x}"
            );
        }
    }
}

#[test]
fn pc_fetch_alias_is_the_reset_rom_fold_only_when_enabled() {
    let mut mem = Memory::new();
    for a in [0xFFFF_FFF0, HIGH_MEM_ALIAS_BASE, HIGH_MEM_ALIAS_END] {
        assert_eq!(mem.fetch_alias(GuestPhys(a)), GuestPhys(a));
    }
    mem.set_high_mem_alias_enabled(true);
    assert_eq!(mem.fetch_alias(GuestPhys(0xFFFF_FFF0)), GuestPhys(0xF_FFF0));
    assert_eq!(mem.fetch_alias(GuestPhys(0xFFFF_0000)), GuestPhys(0xF_0000));
    assert_eq!(
        mem.fetch_alias(GuestPhys(0xFFFE_FFFF)),
        GuestPhys(0xFFFE_FFFF)
    );
    // Both edges of the inclusive alias window.
    assert_eq!(
        mem.fetch_alias(GuestPhys(HIGH_MEM_ALIAS_BASE)),
        GuestPhys(HIGH_MEM_ALIAS_FOLD_BASE)
    );
    assert_eq!(
        mem.fetch_alias(GuestPhys(HIGH_MEM_ALIAS_BASE - 1)),
        GuestPhys(HIGH_MEM_ALIAS_BASE - 1)
    );
    assert_eq!(
        mem.fetch_alias(GuestPhys(HIGH_MEM_ALIAS_END)),
        GuestPhys(HIGH_MEM_ALIAS_FOLD_BASE + (HIGH_MEM_ALIAS_END - HIGH_MEM_ALIAS_BASE))
    );
    assert_eq!(
        mem.fetch_alias(GuestPhys(HIGH_MEM_ALIAS_END + 1)),
        GuestPhys(HIGH_MEM_ALIAS_END + 1)
    );
    // Never the A20/HMA wrap.
    assert_eq!(mem.fetch_alias(GuestPhys(0x10_0010)), GuestPhys(0x10_0010));
}

/// `direct_span` is `direct_host_ram_range` for fetch, plus the TLB fill's
/// read and write withholds for the data intents (LLD 3.3).
#[test]
fn pc_direct_span_applies_the_intent_withholds() {
    let mut mem = Memory::new();
    let fetch =
        |m: &Memory, a: usize, len: usize| m.direct_span(GuestPhys(a), len, AccessIntent::Fetch);
    for (a, len) in [
        (0x1000, 16),
        (0x9_FFF0, 16),
        (0xA_0000, 1),
        (0xB_8000, 2),
        (0xF_0000, 4),
    ] {
        assert_eq!(
            fetch(&mem, a, len).map(|s| s.ptr()),
            mem.direct_host_ram_range(a, len),
            "fetch {a:#x}+{len}"
        );
    }
    let span = fetch(&mem, 0x1000, 16).unwrap();
    assert_eq!(
        span.backing().0,
        0x1000,
        "D2: PC backing offset == physical"
    );

    // F-segment: reads and fetches are direct, stores only with ROM writes on.
    let any = |m: &Memory, a: usize, i| m.direct_span(GuestPhys(a), 4, i).is_some();
    if mem.direct_host_ram_range(0xF_0100, 4).is_some() {
        assert!(any(&mem, 0xF_0100, AccessIntent::Read));
        assert!(!any(&mem, 0xF_0100, AccessIntent::Write));
        assert!(!any(&mem, 0xF_0100, AccessIntent::ReadWrite));
        mem.set_allow_rom_writes(true);
        assert!(any(&mem, 0xF_0100, AccessIntent::Write));
        mem.set_allow_rom_writes(false);
    }

    // Descriptor snoop hull: no direct stores.
    mem.set_gdt_snoop_range(0x5000, 0xFF);
    assert!(any(&mem, 0x5010, AccessIntent::Read));
    assert!(!any(&mem, 0x5010, AccessIntent::Write));
    assert!(any(&mem, 0x6010, AccessIntent::Write));

    // MP-table suppression: low memory is neither read nor written directly,
    // but code fetch is not content-filtered.
    mem.set_firmware_mp_table_suppression_enabled(true);
    assert!(!any(&mem, 0x2000, AccessIntent::Read));
    assert!(!any(&mem, 0x2000, AccessIntent::Write));
    assert!(any(&mem, 0x2000, AccessIntent::Fetch));
}

#[test]
fn pc_backing_page_is_identity_inside_usable_ram() {
    let mem = Memory::new();
    let last = (mem.usable_len() >> 12) as u32;
    assert_eq!(mem.backing_page(PhysPage(0)), Some(BackingPage(0)));
    assert_eq!(
        mem.backing_page(PhysPage(last - 1)),
        Some(BackingPage(last - 1))
    );
    assert_eq!(mem.backing_page(PhysPage(last)), None);
}

#[test]
fn pc_write_duties_delegate_to_the_memory_funnels() {
    let mut mem = Memory::new();
    mem.mark_instr_exec_page(0x3000);
    mem.core.take_instr_cache_dirty();
    mem.note_direct_write(GuestPhys(0x3010), 4);
    assert!(mem.core.is_instr_cache_dirty(), "V6 decode-cache duty");
    assert!(!X86Machine::smc_before_write(
        &mut mem,
        GuestPhys(0x3010),
        4
    ));
    X86Machine::set_page_watched(&mut mem, BackingPage(0x3), true);
    assert!(mem.core.is_page_watched(0x3));
    assert_eq!(
        mem.device_transaction_pending(),
        mem.has_pending_s3_mmio_transactions()
    );
    assert!(mem.physical_span_is_passive(GuestPhys(0xA_0000), 4));
}

struct VgaDirtyIo {
    dirty: bool,
}

impl gp64_types::IoPort for VgaDirtyIo {
    fn read_u8(&mut self, _port: u16) -> u8 {
        0xFF
    }
    fn read_u16(&mut self, _port: u16) -> u16 {
        0xFFFF
    }
    fn read_u32(&mut self, _port: u16) -> u32 {
        u32::MAX
    }
    fn write_u8(&mut self, _port: u16, _value: u8) {}
    fn write_u16(&mut self, _port: u16, _value: u16) {}
    fn write_u32(&mut self, _port: u16, _value: u32) {}
    fn vga_pipeline_dirty(&self) -> bool {
        self.dirty
    }
    fn take_vga_pipeline_state(&mut self) -> VgaPipelineState {
        self.dirty = false;
        VgaPipelineState {
            plane_mask: 0x3,
            ..VgaPipelineState::default()
        }
    }
}

/// The interpreter's OUT sync, verbatim: only VGA ports with a dirty pipeline.
#[test]
fn pc_after_port_write_syncs_the_vga_pipeline() {
    let mut mem = Memory::new();
    let mut io = VgaDirtyIo { dirty: true };
    assert_eq!(mem.after_port_write(&mut io, 0x60), PortSync::None);
    assert!(io.dirty, "a non-VGA port never drains the pipeline");
    assert_eq!(mem.after_port_write(&mut io, 0x3C5), PortSync::Republished);
    assert!(!io.dirty);
    assert_eq!(mem.vga.plane_mask, 0x3);
    assert_eq!(mem.after_port_write(&mut io, 0x3C5), PortSync::None);
}

/// The plain chain-4 Mode 13h pipeline that arms the native commit admission.
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

fn write_route<const W: usize>(mem: &mut Memory, addr: usize) -> gp64_memory_core::WriteRoute {
    let p = mem.access_policy();
    // SAFETY: only the route is inspected; no raw store follows.
    unsafe { mem.write_prelude::<W>(p, GuestPhys(addr), 0x1122_3344) }
}

fn read_route<const W: usize>(mem: &Memory, addr: usize) -> gp64_memory_core::ReadRoute {
    // SAFETY: only the route is inspected.
    unsafe { mem.read_prelude::<W>(GuestPhys(addr)) }
}

/// The PC write prelude follows the interpreter funnels' order (M2 LLD §1.1,
/// C §3.5): VGA first (Chain-4 commit or the checked route), then the ROM
/// drops, then the EMS frame, then the RAM bound, else raw. Multi-byte
/// stores split per byte over any ROM aperture or the EMS frame.
#[test]
fn pc_write_prelude_routes_follow_the_funnel_order() {
    use gp64_memory_core::WriteRoute::{Checked, Dropped, Handled, HandledReload, Raw, SplitBytes};

    let mut mem = Memory::new();
    let tracking = cfg!(debug_assertions) && mem.is_write_tracking_enabled();
    assert!(!tracking, "a fresh Memory does not track writes");
    let end = mem.usable_len();
    assert_eq!(write_route::<1>(&mut mem, 0x1000), Raw);
    assert_eq!(write_route::<2>(&mut mem, 0x1000), Raw);
    assert_eq!(write_route::<4>(&mut mem, 0x1000), Raw);
    // VGA without an admitted native commit: the checked route, any width,
    // including a store that only straddles into the window.
    assert_eq!(write_route::<1>(&mut mem, 0xA0000), Checked);
    assert_eq!(write_route::<2>(&mut mem, 0x9FFFF), Checked);
    assert_eq!(write_route::<4>(&mut mem, 0x9FFFD), Checked);
    assert_eq!(write_route::<4>(&mut mem, 0xBFFFF), Checked);
    // The F-segment drops byte stores and splits straddling ones.
    assert_eq!(write_route::<1>(&mut mem, 0xF0000), Dropped);
    assert_eq!(write_route::<1>(&mut mem, 0xFFFFF), Dropped);
    assert_eq!(write_route::<2>(&mut mem, 0xEFFFF), SplitBytes);
    assert_eq!(write_route::<4>(&mut mem, 0xEFFFE), SplitBytes);
    assert_eq!(write_route::<1>(&mut mem, 0x100000), Raw);
    // The RAM bound.
    assert_eq!(write_route::<1>(&mut mem, end - 1), Raw);
    assert_eq!(write_route::<1>(&mut mem, end), Checked);
    assert_eq!(write_route::<2>(&mut mem, end - 1), Checked);
    assert_eq!(write_route::<4>(&mut mem, end - 3), Checked);
    assert_eq!(write_route::<4>(&mut mem, end - 4), Raw);

    // Real boot opens the F-segment.
    let mut open = Memory::new();
    open.set_allow_rom_writes(true);
    assert_eq!(write_route::<1>(&mut open, 0xF0000), Raw);
    assert_eq!(write_route::<4>(&mut open, 0xEFFFE), Raw);

    // A registered ROM page drops and splits.
    let mut rom = Memory::new();
    rom.mark_rom_page(0xC8);
    assert_eq!(write_route::<1>(&mut rom, 0xC8000), Dropped);
    assert_eq!(write_route::<1>(&mut rom, 0xC7FFF), Raw);
    assert_eq!(write_route::<2>(&mut rom, 0xC7FFF), SplitBytes);
    assert_eq!(write_route::<4>(&mut rom, 0xC8FFE), SplitBytes);

    // The option-ROM window drops and splits.
    let mut opt = Memory::new();
    opt.set_option_rom_backing(&[0x55; 0x800]);
    opt.configure_option_rom_decoder(0xD0000, true);
    assert_eq!(write_route::<1>(&mut opt, 0xD0000), Dropped);
    assert_eq!(write_route::<1>(&mut opt, 0xD0800), Raw);
    assert_eq!(write_route::<4>(&mut opt, 0xCFFFE), SplitBytes);

    // An owned EMS frame: the machine performs byte stores through its own
    // checked path; wider ones split so each byte is decoded on its own.
    let mut ems = Memory::new();
    ems.claim_hle_ems_frame(EMS_FRAME_BASE, EMS_FRAME_SIZE)
        .expect("claim the EMS frame");
    assert_eq!(write_route::<1>(&mut ems, EMS_FRAME_BASE), HandledReload);
    assert_eq!(write_route::<2>(&mut ems, EMS_FRAME_BASE - 1), SplitBytes);
    assert_eq!(
        write_route::<4>(&mut ems, EMS_FRAME_BASE + 0x100),
        SplitBytes
    );
    assert_eq!(write_route::<1>(&mut ems, EMS_FRAME_BASE - 1), Raw);

    // Chain-4: the native commit handles aperture stores that stay inside
    // one group of four; others take the full-width checked route.
    let mut c4 = Memory::new();
    c4.apply_vga_pipeline(&chain4_pipeline());
    assert_eq!(c4.vga_native_commit_kind(), Some(VgaCommitKind::Chain4));
    let before = c4.vga_bus_write_count();
    assert_eq!(write_route::<1>(&mut c4, 0xA0005), Handled);
    assert_eq!(write_route::<2>(&mut c4, 0xA0002), Handled);
    assert_eq!(write_route::<2>(&mut c4, 0xA0003), Checked);
    assert_eq!(write_route::<4>(&mut c4, 0xA0004), Handled);
    assert_eq!(write_route::<4>(&mut c4, 0xA0006), Checked);
    assert_eq!(write_route::<1>(&mut c4, 0xB0000), Checked);
    assert_eq!(write_route::<2>(&mut c4, 0x9FFFF), Checked);
    assert_eq!(
        c4.vga_bus_write_count() - before,
        7,
        "each Handled store committed its bytes through the native commit"
    );
}

/// The PC read prelude: EMS, the RAM bound, VGA and the option ROM take the
/// checked route; everything else is raw.
#[test]
fn pc_read_prelude_routes_follow_the_funnel_order() {
    use gp64_memory_core::ReadRoute::{Checked, Raw};

    let mem = Memory::new();
    let end = mem.usable_len();
    assert_eq!(read_route::<1>(&mem, 0x1000), Raw);
    assert_eq!(read_route::<2>(&mem, 0x1000), Raw);
    assert_eq!(read_route::<4>(&mem, 0x1000), Raw);
    assert_eq!(read_route::<1>(&mem, 0xA0000), Checked);
    assert_eq!(read_route::<2>(&mem, 0x9FFFF), Checked);
    assert_eq!(read_route::<4>(&mem, 0x9FFFD), Checked);
    assert_eq!(read_route::<4>(&mem, 0x9FFFC), Raw);
    assert_eq!(
        read_route::<1>(&mem, 0xF0000),
        Raw,
        "reads of the F-segment are raw"
    );
    assert_eq!(read_route::<1>(&mem, end), Checked);
    assert_eq!(read_route::<2>(&mem, end - 1), Checked);
    assert_eq!(read_route::<4>(&mem, end - 3), Checked);

    let mut opt = Memory::new();
    opt.set_option_rom_backing(&[0x55; 0x800]);
    opt.configure_option_rom_decoder(0xD0000, true);
    assert_eq!(read_route::<1>(&opt, 0xD0000), Checked);
    assert_eq!(read_route::<4>(&opt, 0xCFFFE), Checked);
    assert_eq!(read_route::<1>(&opt, 0xD0800), Raw);

    let mut ems = Memory::new();
    ems.claim_hle_ems_frame(EMS_FRAME_BASE, EMS_FRAME_SIZE)
        .expect("claim the EMS frame");
    assert_eq!(read_route::<1>(&ems, EMS_FRAME_BASE), Checked);
    assert_eq!(read_route::<2>(&ems, EMS_FRAME_BASE - 1), Checked);
    assert_eq!(read_route::<1>(&ems, EMS_FRAME_BASE - 1), Raw);
}

/// The new M2.1 items: the bulk raw window is today's REP gate (A20 open,
/// below the VGA window), the linear mask is the A20 mask, and the device
/// write count is the VGA bus write count.
#[test]
fn pc_bulk_window_linear_mask_and_device_write_count() {
    const { assert!(<Memory as X86Machine>::DEVICE_WRITE_DELAY) };
    let mut mem = Memory::new();
    let open = mem.access_policy();
    assert_eq!(open.linear_mask(), 0xFFFF_FFFF);
    assert_eq!(Memory::bulk_raw_limit(open), Some(VIDEO_BASE));
    mem.set_a20(false);
    let closed = mem.access_policy();
    assert_eq!(closed.linear_mask(), !(1u32 << 20));
    assert_eq!(Memory::bulk_raw_limit(closed), None);
    for a in [0x0010_0000u32, 0x001F_FFFF, 0xFFFF_FFFF, 0x1234] {
        assert_eq!(
            (a & closed.linear_mask()) as usize,
            Memory::phys_mask(closed, ArchPhys(a as usize)).0,
            "linear_mask agrees with phys_mask for u32 addresses"
        );
    }

    let mut c4 = Memory::new();
    c4.apply_vga_pipeline(&chain4_pipeline());
    let before = c4.device_write_count();
    assert_eq!(before, c4.vga_bus_write_count());
    c4.write_u8(0xA0000, 1);
    assert_eq!(c4.device_write_count(), before + 1);
}

// ── M2.4a items: the trait bodies against the inherent methods they call ──

/// A GDT base and limit, and an optional LDT base and limit.
type SnoopLayout = (u32, u16, Option<(u32, u32)>);

/// The descriptor-snoop setters and the funnel snoop give the same snoop
/// windows and dirty latches through the trait as through `Memory`'s own
/// methods, across GDT/LDT layouts and store sizes straddling the edges.
#[test]
fn descriptor_snoop_items_match_the_inherent_methods() {
    let layouts: [SnoopLayout; 3] = [
        (0x5000, 0xFFF, None),
        (0x5000, 0x7F, Some((0x9F800, 0x7FF))),
        (0x10_0000, 0x3F, Some((0x10_0100, 0x0F))),
    ];
    let addrs: [usize; 10] = [
        0x4FFC, 0x4FFF, 0x5000, 0x5005, 0x507E, 0x5080, 0x9F7FE, 0x9FFFF, 0x10_00FE, 0x10_0108,
    ];
    for (gdt, gdt_limit, ldt) in layouts {
        let mut by_trait = Memory::new();
        let mut by_inherent = Memory::new();
        <Memory as X86Machine>::descriptor_snoop_set_gdt(&mut by_trait, gdt, gdt_limit);
        by_inherent.set_gdt_snoop_range(gdt, gdt_limit);
        match ldt {
            Some((base, limit)) => {
                <Memory as X86Machine>::descriptor_snoop_set_ldt(&mut by_trait, base, limit);
                by_inherent.set_ldt_snoop_range(base, limit);
            }
            None => {
                <Memory as X86Machine>::descriptor_snoop_clear_ldt(&mut by_trait);
                by_inherent.clear_ldt_snoop_range();
            }
        }
        assert_eq!(
            by_trait.core().snoop_hull(),
            by_inherent.core().snoop_hull()
        );
        for addr in addrs {
            for size in [1u8, 2, 4] {
                let val = 0x00CF_9A00_0000_FFFFu64;
                <Memory as X86Machine>::snoop_descriptor_write(&by_trait, addr, val, size);
                by_inherent.snoop_gdt_write_ext(addr, val, size);
                assert_eq!(
                    (by_trait.take_gdt_dirty(), by_trait.take_ldt_dirty()),
                    (by_inherent.take_gdt_dirty(), by_inherent.take_ldt_dirty()),
                    "gdt={gdt:#X} ldt={ldt:?} addr={addr:#X} size={size}"
                );
            }
        }
    }
}

/// RAM and MMIO coverage cannot create an explicit linear mapping.
#[test]
fn unregistered_linear_mapping_stays_absent_with_ram_or_mmio() {
    for linear in [0x0010_0000, 0x0020_0000, 0x0100_0000] {
        for mmio in [false, true] {
            let mut mem = Memory::new();
            if mmio {
                mem.add_mmio_region(linear as usize, 0x1000);
            }
            let before = mem.usable_len();
            assert_eq!(mem.map_linear_physical(linear), None);
            assert_eq!(mem.usable_len(), before);
        }
    }
}

#[test]
fn diag_items_reach_the_pc_diagnostics() {
    let mut mem = Memory::new();
    mem.write_u8(0x1234, 0x5A);
    mem.write_u8(0x10_1234, 0xA5);
    for addr in [
        0usize,
        0x1234,
        0x10_1234,
        mem.usable_len() - 1,
        mem.usable_len() + 0x1000,
    ] {
        assert_eq!(
            mem.diag_peek_phys_u8(addr),
            mem.read_u8_physical(addr),
            "{addr:#X}"
        );
    }
    let before = mem.interp_tlb_device_reuse_hits();
    <Memory as X86Machine>::diag_count_tlb_device_reuse(&mut mem);
    assert_eq!(mem.interp_tlb_device_reuse_hits(), before + 1);
}

/// Fetch adds A20 continuity to the raw backing proof, across
/// the route-table scenarios (ROM writes, MP-table suppression, GDT and LDT
/// snoops, A20 off) and pages spanning low memory, the BIOS segment, the
/// HMA and beyond RAM, at sizes that straddle page edges.
#[test]
fn raw_ram_span_is_fetch_proof_with_a20_continuity() {
    let scenarios: [Scenario; 6] = [
        ("default", |_| {}),
        ("rom_writes", |m| m.set_allow_rom_writes(true)),
        ("mp_suppressed", |m| {
            m.set_firmware_mp_table_suppression_enabled(true);
        }),
        ("gdt_snoop", |m| m.set_gdt_snoop_range(0x5000, 0xFFF)),
        ("ldt_snoop", |m| m.set_ldt_snoop_range(0x9F800, 0x7FF)),
        ("a20_off", |m| m.set_a20(false)),
    ];
    let pages: [usize; 18] = [
        0x00, 0x05, 0x06, 0x9F, 0xA0, 0xB8, 0xBF, 0xC0, 0xC8, 0xEF, 0xF0, 0xFF, 0x100, 0x105,
        0x10F, 0x110, 0x1FF, 0x8000,
    ];
    let mut raw_count = 0;
    for (name, setup) in scenarios {
        let mut mem = Memory::new();
        setup(&mut mem);
        for page in pages {
            for (offset, len) in [(0usize, 4usize), (0xFFE, 4), (0, 0x1000), (0x800, 0x1000)] {
                let a = (page << 12) + offset;
                let raw = mem.raw_ram_span(GuestPhys(a), len);
                let fetch = mem
                    .direct_span(GuestPhys(a), len, AccessIntent::Fetch)
                    .is_some();
                let contiguous = mem.a20_enabled() || (a >> 20) == ((a + len - 1) >> 20);
                assert_eq!(raw && contiguous, fetch, "{name} a={a:#X} len={len:#X}");
                raw_count += usize::from(raw);
            }
        }
    }
    assert!(raw_count > 0, "the grid must include plain RAM spans");
}

/// The DRC fetch path's `direct_span(Fetch)` (M3b.0 S2) is exactly the
/// direct-host range it replaced, pointer included, with the additional A20
/// continuity requirement. Fetch applies no read or write withhold.
#[test]
fn pc_fetch_direct_span_is_the_direct_host_range() {
    let scenarios: [Scenario; 6] = [
        ("default", |_| {}),
        ("rom_writes", |m| m.set_allow_rom_writes(true)),
        ("mp_suppressed", |m| {
            m.set_firmware_mp_table_suppression_enabled(true);
        }),
        ("gdt_snoop", |m| m.set_gdt_snoop_range(0x5000, 0xFFF)),
        ("ldt_snoop", |m| m.set_ldt_snoop_range(0x9F800, 0x7FF)),
        ("a20_off", |m| m.set_a20(false)),
    ];
    let pages: [usize; 18] = [
        0x00, 0x05, 0x06, 0x9F, 0xA0, 0xB8, 0xBF, 0xC0, 0xC8, 0xEF, 0xF0, 0xFF, 0x100, 0x105,
        0x10F, 0x110, 0x1FF, 0x8000,
    ];
    let mut proven = 0;
    for (name, setup) in scenarios {
        let mut mem = Memory::new();
        setup(&mut mem);
        for page in pages {
            for (offset, len) in [(0usize, 1usize), (0xFFF, 1), (0xFFE, 4), (0, 0x1000)] {
                let a = (page << 12) + offset;
                let contiguous = mem.a20_enabled() || (a >> 20) == ((a + len - 1) >> 20);
                let expected = mem.direct_host_ram_range(a, len).filter(|_| contiguous);
                let fetch = mem
                    .direct_span(GuestPhys(a), len, AccessIntent::Fetch)
                    .map(|span| span.ptr());
                assert_eq!(expected, fetch, "{name} a={a:#X} len={len:#X}");
                proven += usize::from(fetch.is_some());
            }
        }
    }
    assert!(proven > 0, "the grid must include plain RAM spans");
}

/// `mask_phys` (M3b.0 S2) is `phys_mask` under a policy captured now, in both
/// A20 states, above 4 GiB included.
#[test]
fn pc_mask_phys_is_phys_mask_under_a_live_capture() {
    let mut mem = Memory::new();
    for enabled in [true, false] {
        mem.set_a20(enabled);
        let p = mem.access_policy();
        for addr in [
            0usize,
            0xF_FFFF,
            0x10_0000,
            0x10_FFEF,
            0x1F_FFFF,
            0xFFFF_FFFF,
            0x1_0010_0000,
        ] {
            assert_eq!(
                mem.mask_phys(ArchPhys(addr)),
                Memory::phys_mask(p, ArchPhys(addr)),
                "a20={enabled} addr={addr:#x}"
            );
        }
    }
}

/// `has_linear_physical_mappings` (M3b.0 S2) follows DPMI mapping
/// registration and removal; the PC declares the capability.
#[test]
fn pc_linear_physical_mappings_item_tracks_registration() {
    const { assert!(<Memory as X86Machine>::DPMI_MAPPINGS) };
    let mut mem = Memory::new();
    assert!(!mem.has_linear_physical_mappings());
    assert!(mem.add_physical_mapping(0x8000_0000, 0x20_0000, 0x1000));
    assert!(mem.has_linear_physical_mappings());
    assert!(mem.remove_physical_mapping(0x8000_0000));
    assert!(!mem.has_linear_physical_mappings());
}

/// The SMC binding items (M3b.0 S3) do what the inherent methods do: bind
/// and remove the core's notifier, and keep the funnels' `smc_interest`
/// hoist in step with the core's has-code-pages flag.
#[test]
fn pc_smc_items_match_the_inherent_binding() {
    let mut mem = Memory::new();
    let notifier: Arc<dyn SmcNotify> = Arc::new(TestSmcNotify::default());
    mem.smc_bind_notifier(Some(notifier));
    assert!(mem.core().has_smc_notifier());
    mem.smc_set_has_code_pages(true);
    assert!(mem.core().smc_has_code_pages());
    assert!(mem.smc_interest, "the PC hoist follows the core flag");
    mem.smc_set_has_code_pages(false);
    assert!(!mem.core().smc_has_code_pages());
    assert!(!mem.smc_interest);
    mem.smc_bind_notifier(None);
    assert!(!mem.core().has_smc_notifier());
}

/// The M3b.0 S5a items match the inherent methods the DRC called before:
/// `ram_base` is `ram_ptr`, and the PC's `tlb_direct_read_ptr` keeps the
/// firmware MP-table filter's global refusal on a page the core's predicate
/// alone would admit.
#[test]
fn pc_ram_base_and_direct_read_items_match_the_inherent_methods() {
    let mut mem = Memory::new();
    assert_eq!(X86Machine::ram_base(&mem), mem.ram_ptr());
    mem.tlb_insert(0x21, 0x21, true, true, true);
    assert_eq!(
        X86Machine::tlb_direct_read_ptr(&mem, 0x2_1234, 4, false),
        mem.tlb_direct_read_ptr(0x2_1234, 4, false)
    );
    assert!(X86Machine::tlb_direct_read_ptr(&mem, 0x2_1234, 4, false).is_some());

    mem.set_firmware_mp_table_suppression_enabled(true);
    // Above the filter's low-memory window the page keeps its read view, so
    // the core's predicate admits; the PC item refuses globally.
    mem.tlb_insert(0x200, 0x200, true, true, true);
    assert!(mem
        .core()
        .tlb_direct_read_ptr(0x20_0000, 4, false)
        .is_some());
    assert!(X86Machine::tlb_direct_read_ptr(&mem, 0x20_0000, 4, false).is_none());
}

/// The PC's `tlb_direct_write_span` item is the inherent method: the same
/// span on a warm dirty page, and the same refusal once the frame is a
/// recorded paging structure.
#[test]
fn pc_direct_write_span_item_matches_the_inherent_method() {
    let mut mem = Memory::new();
    mem.tlb_insert(0x21, 0x22, true, true, true);
    let span = X86Machine::tlb_direct_write_span(&mem, 0x2_1234, 4, false);
    assert_eq!(span, mem.tlb_direct_write_span(0x2_1234, 4, false));
    assert_eq!(span.map(|(_, physical)| physical), Some(0x2_2234));
    PagedMemory::record_pt_frame(&mut mem, 0x22);
    assert!(mem.tlb_direct_write_span(0x2_1234, 4, false).is_none());
    assert!(X86Machine::tlb_direct_write_span(&mem, 0x2_1234, 4, false).is_none());
}

/// The PC's `pt_tripwire_note_physical_write` item is the inherent tripwire.
#[test]
fn pc_pt_tripwire_item_matches_the_inherent_method() {
    let mut mem = Memory::new();
    PagedMemory::record_pt_frame(&mut mem, 0x30);
    let before = mem.core().dynrec_pt_write_epoch();
    X86Machine::pt_tripwire_note_physical_write(&mut mem, GuestPhys(0x31_0000), 4);
    assert_eq!(mem.core().dynrec_pt_write_epoch(), before);
    X86Machine::pt_tripwire_note_physical_write(&mut mem, GuestPhys(0x3_0010), 4);
    assert_ne!(mem.core().dynrec_pt_write_epoch(), before);
}

/// `X86Machine::CORE_OFFSET` (M3b.6 LLD §2.3) locates the core `core()`
/// returns, and the a64 hot-imm32 reload's word through it is the core's
/// direct-read RAM limit.
#[test]
fn pc_core_offset_locates_its_core_and_raw_ram_limit() {
    let mem = Memory::new();
    let base = core::ptr::from_ref(&mem).cast::<u8>();
    assert_eq!(
        base.wrapping_add(<Memory as X86Machine>::CORE_OFFSET)
            .cast::<MemoryCore>(),
        core::ptr::from_ref(mem.core())
    );
    let limit_at =
        base.wrapping_add(<Memory as X86Machine>::CORE_OFFSET + MemoryCore::RAW_RAM_LIMIT_OFFSET);
    // SAFETY: `limit_at` is the `raw_ram_limit` word inside `mem`'s core
    // (the assertion above), a live, aligned `usize`.
    let limit = unsafe { limit_at.cast::<usize>().read() };
    assert_ne!(limit, 0, "premise: the PC raises the limit to its RAM");
    assert_eq!(limit, mem.raw_ram_limit());
}

/// Fetch proofs must end before the next disabled-A20 address discontinuity.
#[test]
fn a20_fetch_direct_span_requires_one_contiguous_bank() {
    let mut mem = Memory::new();
    let fetch = |m: &Memory, addr, len| {
        m.direct_span(GuestPhys(addr), len, AccessIntent::Fetch)
            .is_some()
    };
    assert!(fetch(&mem, 0xfffff, 15), "A20 on permits a crossing fetch");
    mem.set_a20(false);
    assert!(
        fetch(&mem, 0xffff0, 15),
        "a window before the boundary is direct"
    );
    assert!(fetch(&mem, 0xffff0, 16), "an exact-fit window is direct");
    assert!(fetch(&mem, 0xfffff, 1), "the final byte is direct");
    assert!(fetch(&mem, 0, 15), "the wrapped destination is direct");
    assert!(
        !fetch(&mem, 0xfffff, 15),
        "A20 off must reject a fetch spanning low memory and HMA"
    );
}

#[test]
fn a20_fetch_direct_span_rejects_multiple_banks() {
    let mut mem = Memory::new();
    mem.set_a20(false);
    // Both endpoints have bit 20 clear; an endpoint-only mask comparison
    // would miss the intervening discontinuities.
    mem.resize_ram(0x500000);
    assert!(
        mem.direct_span(GuestPhys(0x200000), 0x200001, AccessIntent::Fetch)
            .is_none(),
        "A20 off must also reject a fetch spanning multiple banks"
    );
}
