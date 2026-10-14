use super::{shl8_carry, shr8_carry};

/// Width-multiple counts report the bit that would have left first; every
/// other count above the width clears CF (MDD-BUG-CRU-01381).
#[test]
fn width_multiple_byte_shift_counts_carry_the_original_edge_bit() {
    for count in [8, 16, 24] {
        assert!(shl8_carry(0x01, count), "SHL by {count} carries bit 0");
        assert!(!shl8_carry(0xfe, count), "SHL by {count}: bit 0 clear");
        assert!(shr8_carry(0x80, count), "SHR by {count} carries bit 7");
        assert!(!shr8_carry(0x7f, count), "SHR by {count}: bit 7 clear");
    }
    for count in [9, 15, 17, 23, 25, 31] {
        assert!(!shl8_carry(0xff, count), "SHL by {count} clears CF");
        assert!(!shr8_carry(0xff, count), "SHR by {count} clears CF");
    }
    // The 386 corpus pair at count 16 (C0.4.json idx 2 and 31).
    assert!(shl8_carry(0xe3, 16));
    assert!(!shl8_carry(0xb4, 16));
}
