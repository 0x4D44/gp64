//! Core unit tests: the standalone software TLB (moved from
//! `gp64-memory` in M1), layout pins, the `tlb_fill` publication rule and
//! the `X86Machine` contract over `NullMachine`.

use std::mem::{align_of, offset_of, size_of};

use gp64_types::PagedMemory;

use crate::testing::NullMachine;
use crate::*;

/// Watch-bitmap size the standalone TLB tests use (PC: `MAX_RAM_SIZE >> 12`).
const WATCH_PAGES: usize = (768 * 1024 * 1024) >> 12;

// ========== TLB Tests ==========

/// Helper: insert into a standalone TLB with no host page (host pointers
/// will be null, but tag/flag logic is still exercised).
fn tlb_test_insert(
    tlb: &mut SoftwareTlb,
    linear_page: u32,
    physical_page: u32,
    writable: bool,
    user: bool,
    dirty: bool,
) {
    // SAFETY: a null host page publishes no pointer.
    unsafe {
        tlb.insert(
            linear_page,
            physical_page,
            writable,
            user,
            dirty,
            std::ptr::null_mut(),
            true,
            true,
        );
    }
}

#[test]
fn test_tlb_default() {
    let tlb = SoftwareTlb::new(WATCH_PAGES);
    // All entries should be invalid initially (sentinel tag)
    assert!(tlb.lookup(0).is_none());
    assert!(tlb.lookup(1).is_none());
    assert!(tlb.lookup(0x12345).is_none());
}

#[test]
fn test_tlb_insert_and_lookup() {
    let mut tlb = SoftwareTlb::new(WATCH_PAGES);

    // Insert a translation
    tlb_test_insert(&mut tlb, 0x1000, 0x2000, true, false, false);

    // Should find it
    let result = tlb.lookup(0x1000);
    assert!(result.is_some());
    let (phys, writable, user, dirty) = result.unwrap();
    assert_eq!(phys, 0x2000);
    assert!(writable);
    assert!(!user);
    assert!(!dirty);
}

/// Arming nulls the flat projection but deliberately keeps the authoritative
/// hashed tags — that asymmetry IS the forced-miss sweep. Disarming used to
/// clear the flag and nothing else, so those tags stayed live: `tlb_lookup`
/// kept hitting, the translators never walked the page tables, `tlb_insert`
/// was never reached, and the flat slots stayed null indefinitely.
#[test]
fn disarming_flat_suppression_drops_the_tags_arming_left_incoherent() {
    let mut tlb = SoftwareTlb::new(WATCH_PAGES);
    tlb_test_insert(&mut tlb, 0x1000, 0x2000, true, false, false);
    assert!(
        tlb.lookup(0x1000).is_some(),
        "fixture must start from a live entry, or the rest proves nothing"
    );

    tlb.set_flat_suppressed(true);
    assert!(
        tlb.lookup(0x1000).is_some(),
        "arming must KEEP the authoritative entry; nulling the projection              alone is what makes every emitted probe miss"
    );

    tlb.set_flat_suppressed(false);
    assert!(
        tlb.lookup(0x1000).is_none(),
        "disarming must drop the tags whose projection arming nulled, so the              next access re-walks and republishes both halves together"
    );
}

/// MDD-BUG-CRUCIBLE-00418: O-P4 must catch a stale host pointer even with
/// the O3 forced-miss sweep armed.
///
/// O3 empties the projection and shifts every native miss onto the
/// interpreter, which dereferences `host_read`/`host_write` directly. O-P4
/// used to be a check on the PROJECTION pointer nested inside the loop, so
/// under O3 it could never fire - and the suppressed branch returned before
/// reaching it regardless. The check least able to run was the one guarding
/// the mode that relies on those pointers most.
#[test]
fn o_p4_still_catches_a_stale_host_pointer_under_o3_suppression() {
    let mut ram = vec![0u8; 0x4000];
    let base = ram.as_mut_ptr();
    let mut tlb = SoftwareTlb::new(WATCH_PAGES);
    // SAFETY: `base + 0x1000` is a whole page inside `ram`.
    unsafe {
        tlb.insert(
            0x1000,
            0x1,
            true,
            false,
            false,
            base.add(0x1000),
            true,
            true,
        );
    }
    assert!(
        tlb.oracle_check_live_tags(base, ram.len()).is_ok(),
        "a freshly filled entry must satisfy the oracle"
    );

    tlb.set_flat_suppressed(true);
    // Still coherent for O3's own contract: views nulled, tag live.
    assert!(
        tlb.oracle_check_live_tags(base, ram.len()).is_ok(),
        "arming alone must not trip the oracle"
    );

    // Now model the defect O-P4 exists for: RAM moves, nothing flushes the
    // TLB, so the live hashed pointers dangle into the old allocation.
    let mut moved = vec![0u8; 0x4000];
    let err = tlb
        .oracle_check_live_tags(moved.as_mut_ptr(), moved.len())
        .expect_err("a stale host pointer must be caught even under O3");
    assert!(
        err.contains("O-P4"),
        "expected the pointer-lifetime oracle to fire, got: {err}"
    );
}

#[test]
fn test_tlb_insert_with_dirty() {
    let mut tlb = SoftwareTlb::new(WATCH_PAGES);

    // Insert a dirty page
    tlb_test_insert(&mut tlb, 0x1000, 0x2000, true, true, true);

    let result = tlb.lookup(0x1000);
    assert!(result.is_some());
    let (phys, writable, user, dirty) = result.unwrap();
    assert_eq!(phys, 0x2000);
    assert!(writable);
    assert!(user);
    assert!(dirty);
}

#[test]
fn test_tlb_lookup_miss() {
    let mut tlb = SoftwareTlb::new(WATCH_PAGES);

    // Insert one page
    tlb_test_insert(&mut tlb, 0x1000, 0x2000, true, true, false);

    // Different page should miss
    assert!(tlb.lookup(0x1001).is_none());
    assert!(tlb.lookup(0x2000).is_none());
}

#[test]
fn test_tlb_flush() {
    let mut tlb = SoftwareTlb::new(WATCH_PAGES);

    // Insert several translations with different hash indices
    tlb_test_insert(&mut tlb, 0, 0x2000, true, true, false);
    tlb_test_insert(&mut tlb, 1, 0x4000, false, true, false);
    tlb_test_insert(&mut tlb, 2, 0x6000, true, false, false);

    // All should be findable
    assert!(tlb.lookup(0).is_some());
    assert!(tlb.lookup(1).is_some());
    assert!(tlb.lookup(2).is_some());

    // Flush
    tlb.flush();

    // All should be gone
    assert!(tlb.lookup(0).is_none());
    assert!(tlb.lookup(1).is_none());
    assert!(tlb.lookup(2).is_none());
}

#[test]
fn test_tlb_invalidate_page() {
    let mut tlb = SoftwareTlb::new(WATCH_PAGES);

    // Use page numbers that hash to different indices
    tlb_test_insert(&mut tlb, 0, 0x2000, true, true, false);
    tlb_test_insert(&mut tlb, 1, 0x4000, false, true, false);

    // Invalidate just one page
    tlb.invalidate_page(0);

    // Only that page should be gone
    assert!(tlb.lookup(0).is_none());
    assert!(tlb.lookup(1).is_some());
}

#[test]
fn test_tlb_collision() {
    let mut tlb = SoftwareTlb::new(WATCH_PAGES);

    // With XOR hash, pages that collide are those where
    // (page ^ (page >> 8)) & (TLB_SIZE-1) are equal.
    // Find two pages that hash to the same TLB index.
    // index(p) = (p ^ (p >> 8)) & (TLB_SIZE - 1)
    // page1=0 -> index=0.
    // For page2, we need (p ^ (p>>8)) & 0xFFF == 0.
    // p=0x1010: (0x1010 ^ 0x10) & 0xFFF = 0x1000 & 0xFFF = 0
    let page1 = 0u32;
    let page2 = 0x1010u32;
    assert_eq!(SoftwareTlb::index(page1), SoftwareTlb::index(page2));

    tlb_test_insert(&mut tlb, page1, 0x1000, true, true, false);
    assert!(tlb.lookup(page1).is_some());

    // Insert colliding entry
    tlb_test_insert(&mut tlb, page2, 0x2000, false, false, false);

    // First should be evicted
    assert!(tlb.lookup(page1).is_none());
    assert!(tlb.lookup(page2).is_some());
}

#[test]
fn test_tlb_xor_hash_reduces_collisions() {
    // Verify that identity-mapped first-1MB pages (0x000..0x0FF) and
    // program pages at 0x100..0x1FF no longer 100% collide.
    // With old mask-only hash, page 0x000 and 0x100 would both -> index 0.
    // With XOR hash: page 0x100 -> (0x100 ^ 0x1) & 0xFF = 0x01, not 0x00.
    let idx_low = SoftwareTlb::index(0x000);
    let idx_high = SoftwareTlb::index(0x100);
    assert_ne!(idx_low, idx_high);

    // Also verify a few more pairs
    assert_ne!(SoftwareTlb::index(0x010), SoftwareTlb::index(0x110));
    assert_ne!(SoftwareTlb::index(0x080), SoftwareTlb::index(0x180));
}

#[test]
fn test_tlb_size_is_4096() {
    assert_eq!(TLB_SIZE, 4096);
    let tlb = SoftwareTlb::new(WATCH_PAGES);
    assert_eq!(tlb.entries.len(), 4096);
}

// ========== Layout pins (LLD 2.1; M0c `memory_hot_layout`) ==========

/// Generated code and the interpreter read these through raw pointers, so
/// any drift is an ABI break, not a refactor.
#[test]
fn tlb_entry_and_flat_view_layout_is_pinned() {
    assert_eq!(size_of::<TlbEntry>(), 32);
    assert_eq!(align_of::<TlbEntry>(), 32);
    assert_eq!(
        [
            offset_of!(TlbEntry, host_read),
            offset_of!(TlbEntry, host_write),
            offset_of!(TlbEntry, linear_page),
            offset_of!(TlbEntry, physical_page),
            offset_of!(TlbEntry, flags),
            offset_of!(TlbEntry, pad),
        ],
        [0, 8, 16, 20, 24, 25]
    );
    assert_eq!(TLB_SIZE, 4096);
    assert_eq!(TLB_HASH_SHIFT, 8);
    assert_eq!(
        [
            flat_tlb::FLAT_VIEW_R0_BYTE_OFFSET,
            flat_tlb::FLAT_VIEW_W0_BYTE_OFFSET,
            flat_tlb::FLAT_VIEW_R3_BYTE_OFFSET,
            flat_tlb::FLAT_VIEW_W3_BYTE_OFFSET,
        ],
        [0, 0x0080_0000, 0x0100_0000, 0x0180_0000]
    );
    assert_eq!(flat_tlb::FLAT_VIEW_WRITE_DELTA, 0x0080_0000);
    assert_eq!(flat_tlb::FLAT_VIEW_CPL3_DELTA, 0x0100_0000);
}

/// The hash is the documented expression, and `SoftwareTlb::index` is it.
#[test]
fn tlb_index_is_the_documented_hash() {
    for page in [0u32, 1, 0xFF, 0x100, 0x1010, 0xABCDE, 0xFFFFF, u32::MAX] {
        let want = ((page ^ (page >> 8)) as usize) & (TLB_SIZE - 1);
        assert_eq!(tlb_index(page), want, "page {page:#x}");
        assert_eq!(SoftwareTlb::index(page), want, "page {page:#x}");
    }
}

/// The core's field order is the M0c layout: the hashed TLB first, and the
/// size the PC `Memory` bound was measured with.
#[test]
fn memory_core_layout_is_pinned() {
    assert_eq!(MemoryCore::TLB_OFFSET, 0);
    assert_eq!(
        MemoryCore::SMC_HAS_CODE_PAGES_OFFSET,
        offset_of!(MemoryCore, smc_has_code_pages)
    );
    assert_eq!(
        MemoryCore::RAW_RAM_LIMIT_OFFSET,
        offset_of!(MemoryCore, raw_ram_limit)
    );
    #[cfg(all(target_arch = "aarch64", target_os = "macos", debug_assertions))]
    {
        // 624 + the appended `raw_ram_limit` word; the hot flags stay put.
        assert_eq!(size_of::<MemoryCore>(), 632);
        assert_eq!(MemoryCore::SMC_HAS_CODE_PAGES_OFFSET, 616);
        assert_eq!(MemoryCore::RAW_RAM_LIMIT_OFFSET, 624);
    }
}

/// A fresh core never lets generated code read RAM directly: the machine
/// must publish a limit first (PC at construction, machine at X11).
#[test]
fn raw_ram_limit_starts_at_zero_and_only_lowers_safely() {
    let mut machine = NullMachine::new(64 * 4096);
    let core = machine.core_mut();
    assert_eq!(core.raw_ram_limit(), 0);
    // SAFETY: the null machine commits 64 pages behind its RAM base.
    unsafe { core.set_raw_ram_limit(64 * 4096) };
    assert_eq!(core.raw_ram_limit(), 64 * 4096);
    core.lower_raw_ram_limit(usize::MAX);
    assert_eq!(core.raw_ram_limit(), 64 * 4096);
    core.lower_raw_ram_limit(4096);
    assert_eq!(core.raw_ram_limit(), 4096);
}

// ========== tlb_fill (HLD R2, LLD 2.2) ==========

fn fill(
    core: &mut MemoryCore,
    linear: u32,
    phys: u32,
    perms: PagePerms,
    direct: Option<DirectPage>,
) {
    // SAFETY: tests pass proofs only for pages inside the machine's RAM.
    unsafe {
        core.tlb_fill(TlbFill {
            linear: LinearPage(linear),
            phys: PhysPage(phys),
            perms,
            direct,
        });
    }
}

const RW_DIRTY: PagePerms = PagePerms {
    writable: true,
    user: true,
    dirty: true,
};

/// Host pointers come from the core's RAM base plus the proof's backing page;
/// the write view needs the proof AND writable AND dirty.
#[test]
fn tlb_fill_derives_pointers_from_the_backing_page_and_the_perms() {
    let mut machine = NullMachine::new(64 * 4096);
    let base = machine.core().ram_base_ptr() as usize;
    let core = machine.core_mut();

    fill(
        core,
        0x10,
        5,
        RW_DIRTY,
        Some(DirectPage::new(BackingPage(5), true, true)),
    );
    let e = core.tlb().entries[tlb_index(0x10)];
    assert_eq!(e.host_read as usize, base + 5 * 4096);
    assert_eq!(e.host_write as usize, base + 5 * 4096);
    assert_eq!(e.physical_page, 5);

    let clean = PagePerms {
        dirty: false,
        ..RW_DIRTY
    };
    fill(
        core,
        0x11,
        6,
        clean,
        Some(DirectPage::new(BackingPage(6), true, true)),
    );
    let e = core.tlb().entries[tlb_index(0x11)];
    assert_eq!(e.host_read as usize, base + 6 * 4096);
    assert!(
        e.host_write.is_null(),
        "a clean page never publishes a write view"
    );

    let read_only = PagePerms {
        writable: false,
        ..RW_DIRTY
    };
    fill(
        core,
        0x12,
        7,
        read_only,
        Some(DirectPage::new(BackingPage(7), true, true)),
    );
    assert!(core.tlb().entries[tlb_index(0x12)].host_write.is_null());

    fill(
        core,
        0x13,
        8,
        RW_DIRTY,
        Some(DirectPage::new(BackingPage(8), true, false)),
    );
    let e = core.tlb().entries[tlb_index(0x13)];
    assert!(!e.host_read.is_null());
    assert!(
        e.host_write.is_null(),
        "a read-only proof withholds the write view"
    );

    fill(
        core,
        0x14,
        9,
        RW_DIRTY,
        Some(DirectPage::new(BackingPage(9), false, false)),
    );
    let e = core.tlb().entries[tlb_index(0x14)];
    assert!(e.host_read.is_null() && e.host_write.is_null());

    fill(core, 0x15, 10, RW_DIRTY, None);
    let e = core.tlb().entries[tlb_index(0x15)];
    assert!(e.host_read.is_null() && e.host_write.is_null());
    assert_eq!(
        e.linear_page, 0x15,
        "a proof-less fill still records the translation"
    );
    assert_eq!(core.tlb_lookup(0x15), Some((10, true, true, true)));
}

/// LLD D2: a TLB proof's backing page must equal its physical page, because
/// the watch and PT-frame fold key on the physical page.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "LLD D2")]
fn tlb_fill_rejects_a_proof_whose_backing_differs_from_its_physical_page() {
    let mut machine = NullMachine::new(64 * 4096);
    fill(
        machine.core_mut(),
        0x10,
        5,
        RW_DIRTY,
        Some(DirectPage::new(BackingPage(6), true, true)),
    );
}

/// A watched page still publishes its read view but never its write view.
#[test]
fn tlb_fill_folds_the_watch_into_the_write_view() {
    let mut machine = NullMachine::new(64 * 4096);
    machine.set_page_watched(BackingPage(5), true);
    fill(
        machine.core_mut(),
        0x10,
        5,
        RW_DIRTY,
        Some(DirectPage::new(BackingPage(5), true, true)),
    );
    let flat = machine.core().tlb().flat();
    assert_ne!(flat.entry(flat_tlb::FlatView::Read0, 0x10), 0);
    assert_eq!(flat.entry(flat_tlb::FlatView::Write0, 0x10), 0);
}

// ========== X86Machine over NullMachine (LLD 3) ==========

struct NoIo;

impl gp64_types::IoPort for NoIo {
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
}

fn assert_machine<M: X86Machine>() {}

/// The identity, consts and M1 defaults a generic engine will rely on.
#[test]
fn null_machine_satisfies_the_contract_defaults() {
    assert_machine::<NullMachine>();
    assert_eq!(NullMachine::MACHINE_ID, MachineId::NULL);
    assert_ne!(MachineId::NULL, MachineId::PC);
    assert_ne!(MachineId::PC, MachineId::GENERIC);
    const {
        assert!(!NullMachine::DEVICE_DISPATCH);
        assert!(!NullMachine::NATIVE_DEVICE_COMMIT);
        assert!(!NullMachine::WRITE_TRACKING);
        assert!(!NullMachine::DPMI_MAPPINGS);
    }

    let mut machine = NullMachine::new(16 * 4096);
    let policy = machine.access_policy();
    assert!(!policy.smc_notifier() && !policy.descriptor_snoop());
    // The preludes default to the checked route: never a raw licence.
    // SAFETY: the routes are only inspected.
    unsafe {
        assert_eq!(
            machine.write_prelude::<4>(policy, GuestPhys(0x100), 0),
            WriteRoute::Checked
        );
        assert_eq!(
            machine.read_prelude::<2>(GuestPhys(0x100)),
            ReadRoute::Checked
        );
    }
    assert_eq!(machine.after_port_write(&mut NoIo, 0x3C5), PortSync::None);
    assert!(machine
        .native_device_store(GuestPhys(0xA0000), 0, 1)
        .is_none());
    assert!(machine
        .native_device_rep_store(GuestPhys(0xA0000), &[0; 4])
        .is_none());
    machine.complete_device_boundary();
    assert!(!machine.device_transaction_pending());
    assert!(machine.physical_span_is_passive(GuestPhys(0), 4096));
    assert!(std::ptr::eq(machine.core(), machine.core()));
}

/// Direct spans: whole-span bounds, the pointer, the intent and D2.
#[test]
fn null_machine_direct_spans_cover_exactly_its_ram() {
    let machine = NullMachine::new(16 * 4096);
    let len = machine.ram_len();
    let base = machine.core().ram_base_ptr() as usize;
    let span = machine
        .direct_span(GuestPhys(0x1234), 8, AccessIntent::Write)
        .expect("plain RAM is direct");
    assert_eq!(span.ptr().as_ptr() as usize, base + 0x1234);
    assert_eq!(span.backing(), BackingOffset(0x1234));
    assert_eq!((span.len(), span.intent()), (8, AccessIntent::Write));
    assert!(machine
        .direct_span(GuestPhys(len - 4), 4, AccessIntent::Read)
        .is_some());
    assert!(
        machine
            .direct_span(GuestPhys(len - 4), 5, AccessIntent::Read)
            .is_none(),
        "V2: the proof covers the whole span, not its first byte"
    );
    assert_eq!(machine.backing_page(PhysPage(3)), Some(BackingPage(3)));
    assert_eq!(machine.backing_page(PhysPage(15)), Some(BackingPage(15)));
    assert_eq!(machine.backing_page(PhysPage(16)), None);
    assert_eq!(
        NullMachine::phys_mask(machine.access_policy(), ArchPhys(1 << 20)),
        GuestPhys(1 << 20)
    );
    assert_eq!(
        machine.fetch_alias(GuestPhys(0xFFFF_FFF0)),
        GuestPhys(0xFFFF_FFF0)
    );
}

/// The page walker's TLB entry points through the core into the machine RAM.
#[test]
fn null_machine_tlb_insert_publishes_through_the_core() {
    let mut machine = NullMachine::new(16 * 4096);
    let base = machine.core().ram_base_ptr() as usize;
    machine.tlb_insert(0x40, 3, true, false, true);
    let e = machine.core().tlb().entries[tlb_index(0x40)];
    assert_eq!(e.host_write as usize, base + 3 * 4096);
    machine.tlb_insert(0x41, 99, true, false, true);
    let e = machine.core().tlb().entries[tlb_index(0x41)];
    assert!(e.host_read.is_null(), "a page outside RAM gets no proof");
}

/// M2.4a defaults on a machine with no extras: the snoop is the core duty,
/// there is no explicit linear mapping, and the diagnostic peek reads core RAM below
/// `raw_limit` and 0 above it.
#[test]
fn null_machine_m24a_defaults() {
    let mut machine = NullMachine::new(16 * 4096);
    machine.descriptor_snoop_set_gdt(0x1000, 0xFF);
    machine.snoop_descriptor_write(0x0FFE, 0, 4);
    assert!(machine.core().take_gdt_dirty(), "straddles the GDT start");
    machine.snoop_descriptor_write(0x1100, 0, 4);
    assert!(!machine.core().take_gdt_dirty(), "past the GDT limit");
    machine.descriptor_snoop_set_ldt(0x2000, 0x0F);
    machine.snoop_descriptor_write(0x200F, 0, 1);
    assert!(machine.core().take_ldt_dirty());
    machine.descriptor_snoop_clear_ldt();
    machine.snoop_descriptor_write(0x200F, 0, 1);
    assert!(
        !machine.core().take_ldt_dirty(),
        "the LDT is no longer snooped"
    );

    assert_eq!(machine.map_linear_physical(0x0020_0000), None);

    let limit = machine.raw_limit();
    // SAFETY: test-only write inside the machine's RAM.
    unsafe { *machine.core().ram_base_ptr().add(0x123) = 0x77 };
    assert_eq!(machine.diag_peek_phys_u8(0x123), 0x77);
    assert_eq!(machine.diag_peek_phys_u8(limit), 0);
}

/// `NullMachine` opts its whole RAM into raw bulk stores, and nothing past it.
#[test]
fn null_machine_raw_ram_span_covers_exactly_its_ram() {
    let machine = NullMachine::new(16 * 4096);
    let limit = machine.raw_limit();
    assert!(machine.raw_ram_span(GuestPhys(0), limit));
    assert!(machine.raw_ram_span(GuestPhys(limit - 4), 4));
    assert!(!machine.raw_ram_span(GuestPhys(limit - 4), 5));
    assert!(!machine.raw_ram_span(GuestPhys(usize::MAX), 2));
}

// ========== X7a core primitives ==========

/// Both write views and the stored pointer of the entry for `linear`.
fn write_state(core: &MemoryCore, linear: u32) -> (u64, u64, bool) {
    let flat = core.tlb().flat();
    (
        flat.entry(flat_tlb::FlatView::Write0, linear),
        flat.entry(flat_tlb::FlatView::Write3, linear),
        core.tlb_hashed_slot(linear).host_write.is_null(),
    )
}

/// W-REVOKE (§12.1): warm a write view; `revoke_host_write`; unwatch an
/// unrelated SMC watch on the same page; refill as a machine that withholds
/// the write proof would. The view stays null each time, the read view and an
/// unrelated page are untouched, and every alias of the page is revoked.
#[test]
fn w_revoke_keeps_the_write_view_null_through_an_unwatch_and_a_refill() {
    let mut machine = NullMachine::new(64 * 4096);
    let direct = Some(DirectPage::new(BackingPage(5), true, true));
    // Two linear aliases of page 5, and page 6 as the unrelated control.
    fill(machine.core_mut(), 0x10, 5, RW_DIRTY, direct);
    fill(machine.core_mut(), 0x20, 5, RW_DIRTY, direct);
    fill(
        machine.core_mut(),
        0x11,
        6,
        RW_DIRTY,
        Some(DirectPage::new(BackingPage(6), true, true)),
    );
    for linear in [0x10, 0x20, 0x11] {
        let (w0, w3, stored_null) = write_state(machine.core(), linear);
        assert!(w0 != 0 && w3 != 0 && !stored_null, "warm view {linear:#x}");
    }

    // An SMC watch on the same page nulls the flat view but keeps the stored
    // pointer; the machine then protects the page.
    machine.set_page_watched(BackingPage(5), true);
    assert_eq!(machine.core_mut().revoke_host_write(5), 2);
    // The flat views only: the stored pointer is proved by the unwatch below,
    // the observable consequence the LLD's mutant names.
    for linear in [0x10, 0x20] {
        let (w0, w3, _) = write_state(machine.core(), linear);
        assert_eq!((w0, w3), (0, 0), "revoked {linear:#x}");
    }

    // Unwatching republishes the stored pointer, which must now be null.
    machine.set_page_watched(BackingPage(5), false);
    for linear in [0x10, 0x20] {
        assert_eq!(
            write_state(machine.core(), linear),
            (0, 0, true),
            "an unwatch must not re-grant {linear:#x}"
        );
        let flat = machine.core().tlb().flat();
        assert_ne!(flat.entry(flat_tlb::FlatView::Read0, linear), 0);
    }
    let (w0, _, stored_null) = write_state(machine.core(), 0x11);
    assert!(w0 != 0 && !stored_null, "an unrelated page keeps its view");

    // The machine withholds the write proof on refill.
    fill(
        machine.core_mut(),
        0x10,
        5,
        RW_DIRTY,
        Some(DirectPage::new(BackingPage(5), true, false)),
    );
    assert_eq!(write_state(machine.core(), 0x10), (0, 0, true), "refill");
    assert_eq!(machine.core_mut().revoke_host_write(7), 0);
}

/// `stale_guarded_page` (§13.1): a guarded page bumps its own word, then the
/// proof word once; an unguarded page moves nothing; the sweep counters and
/// other pages' words never move.
#[test]
fn stale_guarded_page_bumps_only_a_guarded_page_and_the_proof_word() {
    let machine = NullMachine::new(64 * 4096);
    let core = machine.core();
    core.register_guarded_page(5);
    core.register_guarded_page(6);
    let words = |core: &MemoryCore| -> Vec<u32> {
        (4..8).map(|p| core.page_generation(p).unwrap()).collect()
    };
    let before = (core.direct_generations().snapshot(), words(core));
    let sweeps = (
        core.dynrec_generation_sweeps(),
        core.dynrec_swept_pages_total(),
    );

    assert!(!core.stale_guarded_page(7));
    assert_eq!((core.direct_generations().snapshot(), words(core)), before);

    assert!(core.stale_guarded_page(5));
    let after = core.direct_generations().snapshot();
    assert_eq!(
        after.proof_generation,
        before.0.proof_generation.wrapping_add(1)
    );
    assert_eq!(
        DirectGenerationsSnapshot {
            proof_generation: before.0.proof_generation,
            ..after
        },
        before.0,
        "only the proof word moves"
    );
    assert_eq!(
        words(core),
        vec![before.1[0], before.1[1] + 1, before.1[2], before.1[3]]
    );
    assert_eq!(
        (
            core.dynrec_generation_sweeps(),
            core.dynrec_swept_pages_total()
        ),
        sweeps
    );
}

/// `mark_descriptor_dirty` (§11.2): sets only that table's dirty flag and
/// bumps the descriptor generation once, with no snoop range programmed.
#[test]
fn mark_descriptor_dirty_sets_the_table_flag_and_bumps_the_generation() {
    let machine = NullMachine::new(16 * 4096);
    let core = machine.core();
    assert!(!core.has_descriptor_snoop());
    let generation = core.dynrec_descriptor_generation();
    core.mark_descriptor_dirty(DescriptorTable::Ldt);
    assert_eq!(core.dynrec_descriptor_generation(), generation + 1);
    assert_eq!(
        core.direct_generations().snapshot().descriptor_generation as u64,
        generation + 1
    );
    assert!(!core.take_gdt_dirty());
    assert!(core.take_ldt_dirty());
    core.mark_descriptor_dirty(DescriptorTable::Gdt);
    assert_eq!(core.dynrec_descriptor_generation(), generation + 2);
    assert!(core.take_gdt_dirty());
    assert!(!core.take_ldt_dirty());
}

/// `note_paging_state` defaults to nothing (A-M5-6).
#[test]
fn note_paging_state_defaults_to_nothing() {
    let mut machine = NullMachine::new(16 * 4096);
    let before = machine.core().direct_generations().snapshot();
    machine.note_paging_state(PagingState {
        pg: true,
        cr3: 0x1000,
        pse: true,
        pae: false,
    });
    assert_eq!(machine.core().direct_generations().snapshot(), before);
    assert_eq!(PagingState::default().cr3, 0);
}

/// `PagingState::from_control_registers` reads CR0.PG (bit 31), CR4.PSE
/// (bit 4) and CR4.PAE (bit 5), and nothing else.
#[test]
fn paging_state_reads_pg_pse_and_pae_bits() {
    assert_eq!(
        PagingState::from_control_registers(0x8000_0000, 0x1234_5000, 0),
        PagingState {
            pg: true,
            cr3: 0x1234_5000,
            pse: false,
            pae: false,
        }
    );
    assert_eq!(
        PagingState::from_control_registers(0, 0, 1 << 4),
        PagingState {
            pse: true,
            ..PagingState::default()
        }
    );
    assert_eq!(
        PagingState::from_control_registers(0, 0, 1 << 5),
        PagingState {
            pae: true,
            ..PagingState::default()
        }
    );
    // Every other bit of CR0 and CR4 (PE, WP, PGE, ...) is not an input.
    assert_eq!(
        PagingState::from_control_registers(0x7FFF_FFFF, 0, !((1 << 4) | (1 << 5))),
        PagingState::default()
    );
}

/// Growth above pinned RAM: the page-generation table grows to cover backed
/// physical pages above the RAM backing. The old table is retired, not
/// freed, and the identity moves so cached proofs re-read the new table.
#[test]
fn ensure_page_generation_coverage_grows_and_retires() {
    let mut machine = NullMachine::new(64 * 4096);
    let core = machine.core_mut();
    let identity = core.page_generation_table_identity();
    assert_eq!(core.page_generation(63), Some(0));
    assert_eq!(core.page_generation(200), None);
    core.ensure_page_generation_coverage(64 * 4096);
    assert_eq!(
        core.page_generation_table_identity(),
        identity,
        "covered: no-op"
    );
    core.ensure_page_generation_coverage(256 * 4096);
    assert_eq!(core.page_generation(255), Some(0));
    assert_eq!(core.page_generation(256), None);
    assert_ne!(core.page_generation_table_identity(), identity);
}

#[test]
fn ensure_watch_pages_extends_core_watching() {
    let mut machine = NullMachine::new(16 * 4096);
    let core = machine.core_mut();
    core.set_page_watched(40, true);
    assert!(!core.is_page_watched(40), "beyond the initial bitmap");
    core.ensure_watch_pages(64);
    core.set_page_watched(40, true);
    assert!(core.is_page_watched(40));
}

// ========== NullMachine armed pages (seam D; LLD 2026.10.08 M3b.6 §6.5) ==========

/// An armed page refuses every direct span of its armed kind, including one
/// that only straddles into it; a read-armed page refuses `Fetch` too; the
/// other kind and other pages keep their spans; disarming republishes.
#[test]
fn armed_page_withholds_direct_spans_of_its_kind() {
    use crate::testing::NullMachine;
    let mut machine = NullMachine::new(16 * 4096);
    let page = GuestPhys(0x3000);
    let all = [
        AccessIntent::Read,
        AccessIntent::Write,
        AccessIntent::ReadWrite,
        AccessIntent::Fetch,
    ];
    for intent in all {
        assert!(
            machine.direct_span(page, 4, intent).is_some(),
            "{intent:?} unarmed"
        );
    }

    machine.arm_page(3, false, true);
    assert_eq!(machine.page_armed(3), (false, true));
    assert!(machine.direct_span(page, 4, AccessIntent::Write).is_none());
    assert!(machine
        .direct_span(page, 4, AccessIntent::ReadWrite)
        .is_none());
    assert!(
        machine
            .direct_span(GuestPhys(0x2FFE), 4, AccessIntent::Write)
            .is_none(),
        "a span straddling into the armed page"
    );
    assert!(machine.direct_span(page, 4, AccessIntent::Read).is_some());
    assert!(machine.direct_span(page, 4, AccessIntent::Fetch).is_some());
    assert!(machine
        .direct_span(GuestPhys(0x2000), 4, AccessIntent::Write)
        .is_some());

    machine.arm_page(3, true, false);
    assert!(machine.direct_span(page, 4, AccessIntent::Read).is_none());
    assert!(machine
        .direct_span(page, 4, AccessIntent::ReadWrite)
        .is_none());
    assert!(machine.direct_span(page, 4, AccessIntent::Fetch).is_none());
    assert!(machine.direct_span(page, 4, AccessIntent::Write).is_some());

    machine.arm_page(3, false, false);
    assert_eq!(machine.page_armed(3), (false, false));
    for intent in all {
        assert!(
            machine.direct_span(page, 4, intent).is_some(),
            "{intent:?} disarmed"
        );
    }
}

/// The arm commit bumps the direct memory policy epoch and flushes the TLB,
/// and the refill withholds exactly the armed kind's host view.
#[test]
fn arming_flushes_and_the_refill_withholds_the_armed_view() {
    use crate::testing::NullMachine;
    let mut machine = NullMachine::new(16 * 4096);
    let views = |machine: &NullMachine| {
        let slot = machine.core().tlb_hashed_slot(3);
        assert_eq!(slot.linear_page, 3, "the slot holds linear page 3");
        (!slot.host_read.is_null(), !slot.host_write.is_null())
    };
    machine.tlb_insert(3, 3, true, false, true);
    assert_eq!(views(&machine), (true, true));

    let epoch = machine.core().direct_memory_policy_epoch();
    machine.arm_page(3, false, true);
    assert!(machine.core().direct_memory_policy_epoch() > epoch);
    assert_ne!(
        machine.core().tlb_hashed_slot(3).linear_page,
        3,
        "the arm commit flushed the TLB"
    );
    machine.tlb_insert(3, 3, true, false, true);
    assert_eq!(
        views(&machine),
        (true, false),
        "write-armed: read view only"
    );

    machine.arm_page(3, true, false);
    machine.tlb_insert(3, 3, true, false, true);
    assert_eq!(
        views(&machine),
        (false, true),
        "read-armed: write view only"
    );

    machine.arm_page(3, false, false);
    machine.tlb_insert(3, 3, true, false, true);
    assert_eq!(views(&machine), (true, true), "disarmed: both views again");
}

/// Every checked-route access that touches a page armed for its kind is
/// logged once, with its first byte and width, however many bytes it spans;
/// accesses to unarmed pages or of the other kind are not; the bytes land.
#[test]
fn checked_accesses_to_armed_pages_are_logged_once_each() {
    use crate::testing::{ArmedAccess, NullMachine};
    use gp64_types::MemoryAccess;
    let mut machine = NullMachine::new(16 * 4096);
    machine.arm_page(3, false, true);

    machine.write_u32(0x2FFE, 0xDDCC_BBAA);
    machine.write_u8(0x2000, 0x11);
    machine.write_u64(0x3008, 0x8877_6655_4433_2211);
    machine.write_u16(0x3FFF, 0xBEEF);
    assert_eq!(
        machine.read_u32(0x3008),
        0x4433_2211,
        "a read is not write-armed"
    );
    assert_eq!(
        machine.take_armed_accesses(),
        [
            ArmedAccess {
                addr: 0x2FFE,
                width: 4,
                write: true
            },
            ArmedAccess {
                addr: 0x3008,
                width: 8,
                write: true
            },
            ArmedAccess {
                addr: 0x3FFF,
                width: 2,
                write: true
            },
        ]
    );
    assert_eq!(
        machine.read_u32(0x2FFE),
        0xDDCC_BBAA,
        "the straddling store landed"
    );

    machine.arm_page(3, true, true);
    assert_eq!(machine.read_u16(0x3FFF), 0xBEEF);
    machine.write_u8(0x4000, 0x22);
    assert_eq!(
        machine.take_armed_accesses(),
        [ArmedAccess {
            addr: 0x3FFF,
            width: 2,
            write: false
        }],
        "the read touched the armed page; the store past it did not"
    );
}

/// Bulk raw spans and page-walk passivity both exclude armed pages.
#[test]
fn raw_spans_and_passivity_exclude_armed_pages() {
    use crate::testing::NullMachine;
    let mut machine = NullMachine::new(16 * 4096);
    assert!(machine.raw_ram_span(GuestPhys(0x2000), 0x2000));
    assert!(machine.physical_span_is_passive(GuestPhys(0x2FFC), 8));
    machine.arm_page(3, true, false);
    assert!(!machine.raw_ram_span(GuestPhys(0x2000), 0x2000));
    assert!(!machine.physical_span_is_passive(GuestPhys(0x2FFC), 8));
    assert!(machine.raw_ram_span(GuestPhys(0x4000), 0x1000));
    assert!(machine.physical_span_is_passive(GuestPhys(0x4000), 4));
}

/// The M3b.0 S5a defaults: `ram_base` is the core's RAM base, and
/// `tlb_direct_read_ptr` is the core's TLB predicate (a warm page admits,
/// another page or a cross-page read refuses).
#[test]
fn null_machine_ram_base_and_direct_read_defaults_are_the_core() {
    let mut machine = NullMachine::new(64 * 4096);
    assert_eq!(
        X86Machine::ram_base(&machine),
        machine.core().ram_base_ptr().cast_const()
    );
    fill(
        machine.core_mut(),
        5,
        5,
        RW_DIRTY,
        Some(DirectPage::new(BackingPage(5), true, true)),
    );
    let ptr = X86Machine::tlb_direct_read_ptr(&machine, 0x5010, 4, false)
        .expect("a warm RAM page is directly readable");
    assert_eq!(ptr, unsafe { X86Machine::ram_base(&machine).add(0x5010) });
    assert_eq!(
        X86Machine::tlb_direct_read_ptr(&machine, 0x5010, 4, false),
        machine.core().tlb_direct_read_ptr(0x5010, 4, false)
    );
    assert!(X86Machine::tlb_direct_read_ptr(&machine, 0x6000, 4, false).is_none());
    assert!(X86Machine::tlb_direct_read_ptr(&machine, 0x5FFE, 4, false).is_none());
}

/// The `tlb_direct_write_span` default (M3b.0 S5a) is the core's predicate:
/// a warm dirty RAM page answers its host pointer and guest physical address,
/// and a recorded paging-structure frame or a page-crossing span refuses.
#[test]
fn null_machine_direct_write_span_default_is_the_core() {
    let mut machine = NullMachine::new(64 * 4096);
    fill(
        machine.core_mut(),
        5,
        6,
        RW_DIRTY,
        Some(DirectPage::new(BackingPage(6), true, true)),
    );
    let (ptr, physical) = X86Machine::tlb_direct_write_span(&machine, 0x5010, 4, false)
        .expect("a warm dirty RAM page is directly writable");
    assert_eq!(ptr.cast_const(), unsafe {
        X86Machine::ram_base(&machine).add(0x6010)
    });
    assert_eq!(physical, 0x6010);
    assert_eq!(
        X86Machine::tlb_direct_write_span(&machine, 0x5010, 4, false),
        machine.core().tlb_direct_write_span(0x5010, 4, false)
    );
    assert!(X86Machine::tlb_direct_write_span(&machine, 0x5FFE, 4, false).is_none());
    machine.core_mut().record_pt_frame(6);
    assert!(X86Machine::tlb_direct_write_span(&machine, 0x5010, 4, false).is_none());
}

/// `pt_tripwire_note_physical_write` (M3b.0 S5a) reaches the core's
/// tripwire: a write to a recorded paging-structure frame bumps the PT-write
/// epoch, a write elsewhere does not.
#[test]
fn null_machine_pt_tripwire_item_reaches_the_core() {
    let mut machine = NullMachine::new(64 * 4096);
    machine.core_mut().record_pt_frame(7);
    let before = machine.core().dynrec_pt_write_epoch();
    X86Machine::pt_tripwire_note_physical_write(&mut machine, GuestPhys(0x9000), 4);
    assert_eq!(machine.core().dynrec_pt_write_epoch(), before);
    X86Machine::pt_tripwire_note_physical_write(&mut machine, GuestPhys(0x7010), 4);
    assert_ne!(machine.core().dynrec_pt_write_epoch(), before);
}

/// `X86Machine::CORE_OFFSET` (M3b.6 LLD §2.3) is the offset of the core
/// `core()` returns, so generated code that adds a `MemoryCore::*_OFFSET` to
/// it reads the machine's own core word.
#[test]
fn null_machine_core_offset_locates_its_core() {
    let machine = NullMachine::new(0x10000);
    let base = core::ptr::from_ref(&machine).cast::<u8>();
    assert_eq!(
        base.wrapping_add(NullMachine::CORE_OFFSET)
            .cast::<MemoryCore>(),
        core::ptr::from_ref(machine.core())
    );
}
