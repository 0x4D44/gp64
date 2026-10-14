use super::test_helpers::setup;
use super::*;

fn run_and_snapshot(code: &[u8]) -> (u16, u16, usize, u64, u64, u16) {
    let (mut cpu, mut mem) = setup(code);
    let mut io = ();
    let _ = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 16, false);
    (
        cpu.fpu.status_word,
        cpu.fpu.tag_word,
        cpu.fpu.get_top(),
        cpu.fpu.get_st(0).to_f64().to_bits(),
        cpu.fpu.get_st(1).to_f64().to_bits(),
        cpu.regs.flags.bits(),
    )
}

/// EXP-IQ007 two-route differential: `D9 C9` executes via the inline
/// lane; `66 D9 C9` (operand-size prefix — x87-ignored, byte at
/// pos_start defeats the lane) executes via the full decode-cache +
/// wrapper route. Both must leave IDENTICAL x87 state and EFLAGS.
#[test]
fn fxch_lane_matches_the_full_route() {
    // Loaded stack: FLD1, FLDZ, then the exchange.
    let lane = run_and_snapshot(&[0xD9, 0xE8, 0xD9, 0xEE, 0xD9, 0xC9, 0xF4]);
    let full = run_and_snapshot(&[0xD9, 0xE8, 0xD9, 0xEE, 0x66, 0xD9, 0xC9, 0xF4]);
    assert_eq!(lane, full, "loaded-stack FXCH: lane vs full route diverged");
    // Sanity: the exchange actually happened (ST0 == 1.0 after swap).
    assert_eq!(lane.3, 1.0f64.to_bits(), "ST0 must be 1.0 post-swap");

    // Empty-register path: masked-IE QNaN indefinite fill + swap.
    let lane_e = run_and_snapshot(&[0xD9, 0xC9, 0xF4]);
    let full_e = run_and_snapshot(&[0x66, 0xD9, 0xC9, 0xF4]);
    assert_eq!(
        lane_e, full_e,
        "empty-register FXCH: lane vs full route diverged"
    );
}

/// The lane must decline every non-FXCH D9 form and every prefixed
/// escape (guard against an over-wide byte match): FLD ST(1) (`D9 C1`)
/// is one below the FXCH row, FSTP ST(1) (`DD D9`) has the C9 byte in
/// the modrm position of a DIFFERENT escape.
#[test]
fn fxch_lane_byte_match_is_exact() {
    // FLD1; FLDZ; FLD ST(1) — a PUSH of the 1.0, not an exchange. A
    // mis-wide lane would FXCH instead: same visible ST0/ST1 pair but
    // no push, so TOP discriminates (3 pushes → 5; an exchange → 6).
    let fld = run_and_snapshot(&[0xD9, 0xE8, 0xD9, 0xEE, 0xD9, 0xC1, 0xF4]);
    assert_eq!(fld.2, 5, "FLD ST(1) must PUSH (TOP=5), not exchange");
    assert_eq!(fld.3, 1.0f64.to_bits());
    assert_eq!(fld.4, 0.0f64.to_bits());
    let fstp = run_and_snapshot(&[0xD9, 0xE8, 0xDD, 0xD9, 0xF4]); // FLD1; FSTP ST(1)
                                                                  // FSTP ST(1) pops the 1.0 into ST(1)'s slot — stack empties.
    assert_eq!(fstp.2, 0, "TOP back to 0 after the pop");
}
