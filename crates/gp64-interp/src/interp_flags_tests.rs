use super::*;

#[test]
fn test_interp_flags_new_16bit() {
    let f = InterpFlags::new(100, false, false, false, false);
    assert_eq!(f.remaining(), 100);
    assert_eq!(f.executed(), 0);
    assert!(!f.op_size_32());
    assert!(!f.addr_size_32());
    assert!(!f.cs_is_32());
    assert!(!f.paging());
    assert!(!f.cpl3());
    assert!(!f.wp_enabled());
    assert!(f.second_page_present());
    assert!(!f.has_lock());
    assert!(!f.ip_set());
    assert!(!f.seg_dirty());
    assert!(!f.pe_changed());
    assert_eq!(f.rep_prefix(), RepPrefix::None);
    assert_eq!(f.seg_override(), None);
}

#[test]
fn test_interp_flags_new_32bit() {
    let f = InterpFlags::new(4096, true, true, true, true);
    assert_eq!(f.remaining(), 4096);
    assert_eq!(f.executed(), 0);
    assert!(f.op_size_32());
    assert!(f.addr_size_32());
    assert!(f.cs_is_32());
    assert!(f.paging());
    assert!(f.cpl3());
    assert!(f.wp_enabled());
    assert!(f.second_page_present());
}

#[test]
fn test_interp_flags_toggle_op_size_32() {
    let mut f = InterpFlags::new(10, true, false, false, false);
    assert!(f.op_size_32());
    f.toggle_op_size_32();
    assert!(!f.op_size_32());
    f.toggle_op_size_32();
    assert!(f.op_size_32());
}

#[test]
fn test_interp_flags_toggle_addr_size_32() {
    let mut f = InterpFlags::new(10, false, false, false, false);
    assert!(!f.addr_size_32());
    f.toggle_addr_size_32();
    assert!(f.addr_size_32());
    f.toggle_addr_size_32();
    assert!(!f.addr_size_32());
}

#[test]
fn test_interp_flags_set_op_size_32() {
    let mut f = InterpFlags::new(10, false, false, false, false);
    assert!(!f.op_size_32());
    f.set_op_size_32(true);
    assert!(f.op_size_32());
    f.set_op_size_32(false);
    assert!(!f.op_size_32());
}

#[test]
fn test_interp_flags_boolean_setters() {
    let mut f = InterpFlags::new(10, false, false, false, false);

    f.set_has_lock();
    assert!(f.has_lock());

    f.set_ip_set();
    assert!(f.ip_set());

    f.set_seg_dirty();
    assert!(f.seg_dirty());
    f.clear_seg_dirty();
    assert!(!f.seg_dirty());

    f.set_pe_changed();
    assert!(f.pe_changed());
}

#[test]
fn test_interp_flags_paging_cpl3_wp() {
    let mut f = InterpFlags::new(10, false, false, false, false);

    f.set_paging(true);
    assert!(f.paging());
    f.set_paging(false);
    assert!(!f.paging());

    f.set_cpl3(true);
    assert!(f.cpl3());
    f.set_cpl3(false);
    assert!(!f.cpl3());

    f.set_wp_enabled(true);
    assert!(f.wp_enabled());
    f.set_wp_enabled(false);
    assert!(!f.wp_enabled());
}

#[test]
fn test_interp_flags_cs_is_32() {
    let mut f = InterpFlags::new(10, false, false, false, false);
    assert!(!f.cs_is_32());
    f.set_cs_is_32(true);
    assert!(f.cs_is_32());
    f.set_cs_is_32(false);
    assert!(!f.cs_is_32());
}

#[test]
fn test_interp_flags_second_page_present() {
    let mut f = InterpFlags::new(10, false, false, false, false);
    assert!(f.second_page_present()); // defaults to true
    f.set_second_page_present(false);
    assert!(!f.second_page_present());
    f.set_second_page_present(true);
    assert!(f.second_page_present());
}

#[test]
fn test_interp_flags_rep_prefix_roundtrip() {
    let mut f = InterpFlags::new(10, false, false, false, false);

    assert_eq!(f.rep_prefix(), RepPrefix::None);

    f.set_rep_prefix(RepPrefix::Rep);
    assert_eq!(f.rep_prefix(), RepPrefix::Rep);

    f.set_rep_prefix(RepPrefix::Repne);
    assert_eq!(f.rep_prefix(), RepPrefix::Repne);

    f.set_rep_prefix(RepPrefix::None);
    assert_eq!(f.rep_prefix(), RepPrefix::None);
}

#[test]
fn test_interp_flags_seg_override_corrected_index() {
    let mut f = InterpFlags::new(10, false, false, false, false);
    assert_eq!(f.seg_override(), None);

    // Set each segment override and verify corrected index (0-5, not 1-6)
    for idx in 0..6u8 {
        f.set_seg_override(idx);
        assert_eq!(f.seg_override(), Some(idx), "seg_override({}) failed", idx);
    }

    f.clear_seg_override();
    assert_eq!(f.seg_override(), None);
}

#[test]
fn test_interp_flags_decrement_remaining() {
    let mut f = InterpFlags::new(5, false, false, false, false);
    assert_eq!(f.remaining(), 5);

    f.decrement_remaining();
    assert_eq!(f.remaining(), 4);

    f.decrement_remaining();
    assert_eq!(f.remaining(), 3);
}

#[test]
fn test_interp_flags_increment_executed() {
    let mut f = InterpFlags::new(100, false, false, false, false);
    assert_eq!(f.executed(), 0);

    f.increment_executed();
    assert_eq!(f.executed(), 1);

    f.increment_executed();
    assert_eq!(f.executed(), 2);
}

#[test]
fn test_interp_flags_increment_executed_does_not_corrupt_remaining() {
    let mut f = InterpFlags::new(100, false, false, false, false);
    assert_eq!(f.remaining(), 100);
    assert_eq!(f.executed(), 0);

    f.increment_executed();
    assert_eq!(
        f.remaining(),
        100,
        "remaining corrupted by increment_executed"
    );
    assert_eq!(f.executed(), 1);

    f.increment_executed();
    assert_eq!(
        f.remaining(),
        100,
        "remaining corrupted by second increment_executed"
    );
    assert_eq!(f.executed(), 2);
}

#[test]
fn test_interp_flags_decrement_remaining_does_not_corrupt_executed() {
    let mut f = InterpFlags::new(100, false, false, false, false);
    f.set_executed(50);
    assert_eq!(f.executed(), 50);

    f.decrement_remaining();
    assert_eq!(f.remaining(), 99);
    assert_eq!(
        f.executed(),
        50,
        "executed corrupted by decrement_remaining"
    );
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "assertion")]
fn test_interp_flags_decrement_remaining_panics_at_zero() {
    let mut f = InterpFlags::new(0, false, false, false, false);
    f.decrement_remaining(); // should panic in debug
}

#[test]
fn test_interp_flags_set_remaining() {
    let mut f = InterpFlags::new(0, false, false, false, false);
    f.set_remaining(4096);
    assert_eq!(f.remaining(), 4096);
    f.set_remaining(0);
    assert_eq!(f.remaining(), 0);
    f.set_remaining(8191);
    assert_eq!(f.remaining(), 8191);
}

#[test]
fn test_interp_flags_set_executed() {
    let mut f = InterpFlags::new(100, false, false, false, false);
    f.set_executed(4096);
    assert_eq!(f.executed(), 4096);
    assert_eq!(f.remaining(), 100); // not corrupted
    f.set_executed(0);
    assert_eq!(f.executed(), 0);
}

#[test]
fn test_interp_flags_overflow_4096_fits() {
    // Verify that remaining=4096 and executed=4096 both fit in 13 bits
    let mut f = InterpFlags::new(4096, false, false, false, false);
    f.set_executed(4096);
    assert_eq!(f.remaining(), 4096);
    assert_eq!(f.executed(), 4096);
}

#[test]
fn test_interp_flags_reset_instruction() {
    let mut f = InterpFlags::new(100, true, true, true, true);
    f.set_executed(50);
    f.set_has_lock();
    f.set_ip_set();
    f.set_rep_prefix(RepPrefix::Rep);
    f.set_seg_override(3);
    f.set_second_page_present(false);

    f.reset_instruction();

    // Cleared fields
    assert!(!f.has_lock());
    assert!(!f.ip_set());
    assert_eq!(f.rep_prefix(), RepPrefix::None);
    assert_eq!(f.seg_override(), None);

    // second_page_present restored to true
    assert!(f.second_page_present());

    // cs_is_32 replicated to op_size_32 and addr_size_32
    assert!(f.cs_is_32());
    assert!(f.op_size_32());
    assert!(f.addr_size_32());

    // Preserved fields
    assert_eq!(f.remaining(), 100);
    assert_eq!(f.executed(), 50);
    assert!(f.paging());
    assert!(f.cpl3());
    assert!(f.wp_enabled());
}

#[test]
fn test_interp_flags_reset_instruction_16bit_mode() {
    let mut f = InterpFlags::new(100, false, false, false, false);
    f.set_executed(25);
    // Manually set op_size_32 — reset should clear it because cs_is_32=false
    f.set_op_size_32(true);

    f.reset_instruction();

    assert!(!f.cs_is_32());
    assert!(!f.op_size_32());
    assert!(!f.addr_size_32());
    assert_eq!(f.remaining(), 100);
    assert_eq!(f.executed(), 25);
}

#[test]
fn test_interp_flags_reset_preserves_seg_dirty() {
    let mut f = InterpFlags::new(10, false, false, false, false);
    f.set_seg_dirty();
    f.reset_instruction();
    assert!(
        f.seg_dirty(),
        "reset_instruction should not clear seg_dirty"
    );
}

#[test]
fn test_interp_flags_reset_preserves_pe_changed() {
    let mut f = InterpFlags::new(10, false, false, false, false);
    f.set_pe_changed();
    f.reset_instruction();
    assert!(
        f.pe_changed(),
        "reset_instruction should not clear pe_changed"
    );
}
