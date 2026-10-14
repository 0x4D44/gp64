use super::test_helpers::setup;
use super::*;

#[test]
fn reuse_serves_a_flags_legal_held_device_translation() {
    let (cpu, mut mem) = setup(&[0xF4]);
    // Device signature: direct_host_ram_range refuses the VGA window, so
    // this fill records writable+user+dirty flags with null host ptrs.
    mem.tlb_insert(0xA0, 0xA0, true, true, true);
    let mut ram = mem.ram_ptr_mut();
    let mut tlb = mem.tlb_entries_ptr();
    let phys = translate_write_miss(&cpu, &mut mem, &mut ram, &mut tlb, 0xA0123)
        .expect("reuse must translate");
    assert_eq!(phys, 0xA0123, "held translation: phys page 0xA0 + offset");
    assert_eq!(
        mem.interp_tlb_device_reuse_hits(),
        1,
        "the reuse arm must serve this store — fails without U4"
    );
}

#[test]
fn not_dirty_and_user_violations_decline_to_the_walk() {
    let (mut cpu, mut mem) = setup(&[0xF4]);
    // D=0: hardware re-walks on write; the reuse arm must too. The
    // fixture has no page tables, so the walk's own outcome (here a #PF)
    // is incidental — the pin is that the reuse counter stays untouched.
    mem.tlb_insert(0xA1, 0xA1, true, true, false);
    let mut ram = mem.ram_ptr_mut();
    let mut tlb = mem.tlb_entries_ptr();
    let _ = translate_write_miss(&cpu, &mut mem, &mut ram, &mut tlb, 0xA1044);
    assert_eq!(mem.interp_tlb_device_reuse_hits(), 0, "D=0 must not reuse");

    // Supervisor-only entry probed at CPL3 must decline.
    mem.tlb_insert(0xA2, 0xA2, true, false, true);
    cpu.regs.cpl = 3;
    let mut ram = mem.ram_ptr_mut();
    let mut tlb = mem.tlb_entries_ptr();
    let _ = translate_write_miss(&cpu, &mut mem, &mut ram, &mut tlb, 0xA2000);
    assert_eq!(
        mem.interp_tlb_device_reuse_hits(),
        0,
        "CPL3 vs supervisor-only entry must not reuse"
    );
}
