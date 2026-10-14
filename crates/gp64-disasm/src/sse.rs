//! SSE and SSE2.
//!
//! Split out of the two-byte map because it is selected differently from
//! everything around it. Across the rest of the map an opcode names an
//! instruction and a `66` prefix widens it. Here the prefix is part of the
//! encoding, and the same opcode is four different instructions: `0F 58` is
//! `addps` bare, `addpd` under `66`, `addss` under `F3` and `addsd` under `F2`.
//! Three of those four are not "ADD at another width" — they operate on
//! different numbers of different-sized lanes.
//!
//! Two consequences shape this module. Lookups take the mandatory prefix, and
//! an opcode that has no form under some prefix must REFUSE rather than fall
//! back to the bare one — `F2 0F 54` is not `andps` with a stray prefix, it is
//! not an instruction.
//!
//! Several opcodes also split on ModRM.mod, and not into a memory form and a
//! register form of the same instruction: `0F 12` is `movlps` against memory
//! and `movhlps` — a different instruction moving a different half — against a
//! register. So the ModRM byte is an input here too.
//!
//! Scope is SSE and SSE2, which is what `--cpu pentium-iii` and `--cpu
//! pentium-4` model. SSE3 and later are absent, and so is AMD's SSE4a
//! (`movntss`/`movntsd` at `0F 2B`).

use crate::operand::{MemSize, Op};
use crate::table1::{e, ef, Entry};
use crate::table2::Mandatory;

/// Two XMM operands, the common shape.
const fn vw(m: &'static str) -> Entry {
    ef(m, Op::Vx, Op::Wx, Op::None)
}

/// The suffix a mandatory prefix puts on a packed/scalar float mnemonic.
const fn float_suffix(m: Mandatory) -> &'static str {
    match m {
        Mandatory::None => "ps",
        Mandatory::P66 => "pd",
        Mandatory::PF3 => "ss",
        Mandatory::PF2 => "sd",
    }
}

/// Which of the four forms an arithmetic opcode actually has, as a bitmask over
/// (bare, 66, F3, F2).
///
/// This is the part that cannot be inferred: `sqrt` has all four, but the
/// bitwise ops are packed-only and `rsqrt`/`rcp` are single-precision-only.
/// Treating a missing form as "the bare one with an ignored prefix" would name
/// an instruction that does not exist.
const fn float_forms(op2: u8) -> Option<(&'static str, u8)> {
    Some(match op2 {
        0x51 => ("sqrt", 0b1111),
        0x52 => ("rsqrt", 0b0101),
        0x53 => ("rcp", 0b0101),
        0x54 => ("and", 0b0011),
        0x55 => ("andn", 0b0011),
        0x56 => ("or", 0b0011),
        0x57 => ("xor", 0b0011),
        0x58 => ("add", 0b1111),
        0x59 => ("mul", 0b1111),
        0x5C => ("sub", 0b1111),
        0x5D => ("min", 0b1111),
        0x5E => ("div", 0b1111),
        0x5F => ("max", 0b1111),
        _ => return None,
    })
}

const fn form_bit(m: Mandatory) -> u8 {
    match m {
        Mandatory::None => 0b0001,
        Mandatory::P66 => 0b0010,
        Mandatory::PF3 => 0b0100,
        Mandatory::PF2 => 0b1000,
    }
}

/// The comparison mnemonics `CMPPS` and friends fold their immediate into.
///
/// `0F C2` carries an imm8, but Intel syntax does not print it as an operand
/// for the first eight values — it spells the predicate into the name, so
/// `cmpps xmm0,xmm1,2` is written `cmpleps xmm0,xmm1`. Values from 8 up have no
/// spelling and keep the raw three-operand form.
///
/// Written out rather than composed at runtime so a mnemonic stays a
/// `&'static str`, as everywhere else in the crate.
#[rustfmt::skip]
const CMP_FOLDED: [[&str; 8]; 4] = [
    ["cmpeqps", "cmpltps", "cmpleps", "cmpunordps", "cmpneqps", "cmpnltps", "cmpnleps", "cmpordps"],
    ["cmpeqpd", "cmpltpd", "cmplepd", "cmpunordpd", "cmpneqpd", "cmpnltpd", "cmpnlepd", "cmpordpd"],
    ["cmpeqss", "cmpltss", "cmpless", "cmpunordss", "cmpneqss", "cmpnltss", "cmpnless", "cmpordss"],
    ["cmpeqsd", "cmpltsd", "cmplesd", "cmpunordsd", "cmpneqsd", "cmpnltsd", "cmpnlesd", "cmpordsd"],
];

const fn prefix_index(m: Mandatory) -> usize {
    match m {
        Mandatory::None => 0,
        Mandatory::P66 => 1,
        Mandatory::PF3 => 2,
        Mandatory::PF2 => 3,
    }
}

/// The unfolded name, used when the predicate has no spelling.
const fn cmp_raw(m: Mandatory) -> &'static str {
    match m {
        Mandatory::None => "cmpps",
        Mandatory::P66 => "cmppd",
        Mandatory::PF3 => "cmpss",
        Mandatory::PF2 => "cmpsd",
    }
}

/// Resolve `0F C2` from its immediate.
///
/// Returns the mnemonic and whether the immediate is still an operand — it is
/// not, once it has been folded into the name.
pub(crate) const fn cmp_mnemonic(predicate: u8, m: Mandatory) -> (&'static str, bool) {
    if predicate < 8 {
        (CMP_FOLDED[prefix_index(m)][predicate as usize], false)
    } else {
        (cmp_raw(m), true)
    }
}

/// Look up an SSE/SSE2 instruction.
///
/// `modrm` is needed because several opcodes name a different instruction for a
/// register operand than for a memory one.
#[allow(clippy::too_many_lines)]
pub(crate) fn lookup(op2: u8, m: Mandatory, modrm: u8) -> Option<Entry> {
    let is_reg = modrm >> 6 == 3;
    use Mandatory::{None as Bare, P66, PF2, PF3};

    // The float arithmetic block, which is regular enough to drive from a table.
    if let Some((base, mask)) = float_forms(op2) {
        if mask & form_bit(m) == 0 {
            return Option::None;
        }
        // The mnemonic is built at decode time rather than stored, so the
        // sixty-odd names in this block do not each need a table entry.
        return Some(vw(leak_float(base, float_suffix(m))));
    }

    let entry = match (op2, m) {
        // -- 10-17: the move block ------------------------------------------
        (0x10, _) => vw(match m {
            Bare => "movups",
            P66 => "movupd",
            PF3 => "movss",
            PF2 => "movsd",
        }),
        // 11 is the same instruction storing rather than loading, so its
        // operands are the other way round.
        (0x11, _) => ef(
            match m {
                Bare => "movups",
                P66 => "movupd",
                PF3 => "movss",
                PF2 => "movsd",
            },
            Op::Wx,
            Op::Vx,
            Op::None,
        ),
        // 12 and 16 are the mod-dependent pair: against memory they move a
        // half, against a register they move a half BETWEEN halves, which is a
        // different instruction with a different name.
        (0x12, Bare) if is_reg => ef("movhlps", Op::Vx, Op::Ux, Op::None),
        (0x12, Bare) => ef("movlps", Op::Vx, Op::Mx(MemSize::Unsized), Op::None),
        (0x12, P66) if !is_reg => ef("movlpd", Op::Vx, Op::Mx(MemSize::Unsized), Op::None),
        (0x16, Bare) if is_reg => ef("movlhps", Op::Vx, Op::Ux, Op::None),
        (0x16, Bare) => ef("movhps", Op::Vx, Op::Mx(MemSize::Unsized), Op::None),
        (0x16, P66) if !is_reg => ef("movhpd", Op::Vx, Op::Mx(MemSize::Unsized), Op::None),
        // 13 and 17 are the stores, and have no register form at all.
        (0x13, Bare) if !is_reg => ef("movlps", Op::Mx(MemSize::Unsized), Op::Vx, Op::None),
        (0x13, P66) if !is_reg => ef("movlpd", Op::Mx(MemSize::Unsized), Op::Vx, Op::None),
        (0x17, Bare) if !is_reg => ef("movhps", Op::Mx(MemSize::Unsized), Op::Vx, Op::None),
        (0x17, P66) if !is_reg => ef("movhpd", Op::Mx(MemSize::Unsized), Op::Vx, Op::None),
        (0x14, Bare) => vw("unpcklps"),
        (0x14, P66) => vw("unpcklpd"),
        (0x15, Bare) => vw("unpckhps"),
        (0x15, P66) => vw("unpckhpd"),

        // -- 28-2F: aligned moves, conversions and the ordered compares ------
        (0x28, Bare) => vw("movaps"),
        (0x28, P66) => vw("movapd"),
        (0x29, Bare) => ef("movaps", Op::Wx, Op::Vx, Op::None),
        (0x29, P66) => ef("movapd", Op::Wx, Op::Vx, Op::None),
        // The integer conversions cross register files: the packed forms use
        // MMX, and the scalar forms a general register.
        (0x2A, Bare) => ef("cvtpi2ps", Op::Vx, Op::Qq, Op::None),
        (0x2A, P66) => ef("cvtpi2pd", Op::Vx, Op::Qq, Op::None),
        // These two alone in the block carry a size marker. Their source is a
        // general-register-or-memory integer while the destination is an XMM
        // register, so the register beside the memory operand does not say how
        // much of it is read — the MOVZX situation.
        (0x2A, PF3) => e("cvtsi2ss", Op::Vx, Op::Ed, Op::None),
        (0x2A, PF2) => e("cvtsi2sd", Op::Vx, Op::Ed, Op::None),
        // MOVNTPS is a non-temporal store, so it is memory-only.
        (0x2B, Bare) if !is_reg => ef("movntps", Op::Mx(MemSize::Unsized), Op::Vx, Op::None),
        (0x2B, P66) if !is_reg => ef("movntpd", Op::Mx(MemSize::Unsized), Op::Vx, Op::None),
        (0x2C, Bare) => ef("cvttps2pi", Op::Pq, Op::Wx, Op::None),
        (0x2C, P66) => ef("cvttpd2pi", Op::Pq, Op::Wx, Op::None),
        (0x2C, PF3) => ef("cvttss2si", Op::Gd, Op::Wx, Op::None),
        (0x2C, PF2) => ef("cvttsd2si", Op::Gd, Op::Wx, Op::None),
        (0x2D, Bare) => ef("cvtps2pi", Op::Pq, Op::Wx, Op::None),
        (0x2D, P66) => ef("cvtpd2pi", Op::Pq, Op::Wx, Op::None),
        (0x2D, PF3) => ef("cvtss2si", Op::Gd, Op::Wx, Op::None),
        (0x2D, PF2) => ef("cvtsd2si", Op::Gd, Op::Wx, Op::None),
        (0x2E, Bare) => vw("ucomiss"),
        (0x2E, P66) => vw("ucomisd"),
        (0x2F, Bare) => vw("comiss"),
        (0x2F, P66) => vw("comisd"),

        // -- 50, 5A, 5B: the irregulars in the arithmetic block --------------
        // MOVMSKPS collects sign bits into a general register, so its source is
        // register-only.
        (0x50, Bare) if is_reg => ef("movmskps", Op::Gd, Op::Ux, Op::None),
        (0x50, P66) if is_reg => ef("movmskpd", Op::Gd, Op::Ux, Op::None),
        (0x5A, Bare) => vw("cvtps2pd"),
        (0x5A, P66) => vw("cvtpd2ps"),
        (0x5A, PF3) => vw("cvtss2sd"),
        (0x5A, PF2) => vw("cvtsd2ss"),
        (0x5B, Bare) => vw("cvtdq2ps"),
        (0x5B, P66) => vw("cvtps2dq"),
        (0x5B, PF3) => vw("cvttps2dq"),

        // -- C2-C6: compares and shuffles ------------------------------------
        // C2's mnemonic is completed from its immediate, which sits past the
        // ModRM, so it is patched later like 3DNow!'s suffix.
        (0xC2, Bare | P66 | PF3 | PF2) => e("", Op::Vx, Op::Wx, Op::Ib),
        // PINSRW takes its source from a general register, PEXTRW writes to
        // one, and both have an MMX form as well as an XMM one.
        // PINSRW's source is `r32/m16` — a 32-bit register or a 16-bit memory
        // operand — and there is only the one memory width, so no marker.
        (0xC4, Bare) => ef("pinsrw", Op::Pq, Op::Ed, Op::Ib),
        (0xC4, P66) => ef("pinsrw", Op::Vx, Op::Ed, Op::Ib),
        (0xC5, Bare) if is_reg => e("pextrw", Op::Gd, Op::Nq, Op::Ib),
        (0xC5, P66) if is_reg => e("pextrw", Op::Gd, Op::Ux, Op::Ib),
        (0xC6, Bare) => e("shufps", Op::Vx, Op::Wx, Op::Ib),
        (0xC6, P66) => e("shufpd", Op::Vx, Op::Wx, Op::Ib),

        // -- The SSE2 integer block ------------------------------------------
        // Under `66` these are the MMX instructions at twice the width, sharing
        // their mnemonics; the handful that differ are listed first.
        (0x6C, P66) => vw("punpcklqdq"),
        (0x6D, P66) => vw("punpckhqdq"),
        (0xE6, P66) => vw("cvttpd2dq"),
        (0xE6, PF3) => vw("cvtdq2pd"),
        (0xE6, PF2) => vw("cvtpd2dq"),
        // MOVNTI is a non-temporal store of a general register, so it is
        // memory-only and its operands are 32-bit whatever the prefix says.
        (0xC3, Bare) if !is_reg => ef("movnti", Op::M, Op::Gd, Op::None),
        (0x6F, P66) => vw("movdqa"),
        (0x6F, PF3) => vw("movdqu"),
        (0x7F, P66) => ef("movdqa", Op::Wx, Op::Vx, Op::None),
        (0x7F, PF3) => ef("movdqu", Op::Wx, Op::Vx, Op::None),
        (0x6E, P66) => ef("movd", Op::Vx, Op::Ed, Op::None),
        (0x7E, P66) => ef("movd", Op::Ed, Op::Vx, Op::None),
        // `F3 0F 7E` is MOVQ — the one place a `F3` names a move rather than a
        // scalar operation.
        (0x7E, PF3) => vw("movq"),
        (0xD6, P66) => ef("movq", Op::Wx, Op::Vx, Op::None),
        (0xD6, PF2) if is_reg => ef("movdq2q", Op::Pq, Op::Ux, Op::None),
        (0xD6, PF3) if is_reg => ef("movq2dq", Op::Vx, Op::Nq, Op::None),
        // The shift groups take their mnemonic from ModRM.reg, so this is a
        // placeholder the group patcher completes — same as the bare MMX ones.
        (0x71..=0x73, P66) => e("", Op::Ux, Op::Ib, Op::None),
        (0x70, P66) => e("pshufd", Op::Vx, Op::Wx, Op::Ib),
        (0x70, PF3) => e("pshufhw", Op::Vx, Op::Wx, Op::Ib),
        (0x70, PF2) => e("pshuflw", Op::Vx, Op::Wx, Op::Ib),
        (0xD7, P66) if is_reg => ef("pmovmskb", Op::Gd, Op::Ux, Op::None),
        (0xE7, P66) if !is_reg => ef("movntdq", Op::Mx(MemSize::Unsized), Op::Vx, Op::None),
        (0xF7, P66) if is_reg => ef("maskmovdqu", Op::Vx, Op::Ux, Op::None),
        // EMMS is operandless and has no SSE2 form; don't give it the generic
        // XMM operand pair used by the packed-integer fallback below.
        (0x77, P66) => return None,
        // Everything else in the block is the MMX mnemonic with XMM operands.
        (_, P66) => vw(crate::table2::packed_integer_mnemonic(op2)?),

        _ => return Option::None,
    };

    Some(entry)
}

/// Build a float mnemonic without allocating.
///
/// The block is thirteen operations across four suffixes, and every one of the
/// fifty-two names is spelled out here rather than formatted at runtime, so the
/// entry can stay `&'static str` like every other mnemonic in the crate.
fn leak_float(base: &'static str, suffix: &'static str) -> &'static str {
    match (base, suffix) {
        ("sqrt", "ps") => "sqrtps",
        ("sqrt", "pd") => "sqrtpd",
        ("sqrt", "ss") => "sqrtss",
        ("sqrt", "sd") => "sqrtsd",
        ("rsqrt", "ps") => "rsqrtps",
        ("rsqrt", "ss") => "rsqrtss",
        ("rcp", "ps") => "rcpps",
        ("rcp", "ss") => "rcpss",
        ("and", "ps") => "andps",
        ("and", "pd") => "andpd",
        ("andn", "ps") => "andnps",
        ("andn", "pd") => "andnpd",
        ("or", "ps") => "orps",
        ("or", "pd") => "orpd",
        ("xor", "ps") => "xorps",
        ("xor", "pd") => "xorpd",
        ("add", "ps") => "addps",
        ("add", "pd") => "addpd",
        ("add", "ss") => "addss",
        ("add", "sd") => "addsd",
        ("mul", "ps") => "mulps",
        ("mul", "pd") => "mulpd",
        ("mul", "ss") => "mulss",
        ("mul", "sd") => "mulsd",
        ("sub", "ps") => "subps",
        ("sub", "pd") => "subpd",
        ("sub", "ss") => "subss",
        ("sub", "sd") => "subsd",
        ("min", "ps") => "minps",
        ("min", "pd") => "minpd",
        ("min", "ss") => "minss",
        ("min", "sd") => "minsd",
        ("div", "ps") => "divps",
        ("div", "pd") => "divpd",
        ("div", "ss") => "divss",
        ("div", "sd") => "divsd",
        ("max", "ps") => "maxps",
        ("max", "pd") => "maxpd",
        ("max", "ss") => "maxss",
        ("max", "sd") => "maxsd",
        _ => unreachable!(),
    }
}

/// The `66`-prefixed shift groups.
///
/// Same three opcodes as MMX's, with the same three operations at each — plus
/// two whole-register byte shifts at `0F 73 /3` and `/7` that have no MMX form,
/// because there is no 64-bit register to shift across.
pub(crate) const fn sse2_shift_group(op2: u8, reg: u8) -> &'static str {
    match (op2, reg) {
        (0x71, 2) => "psrlw",
        (0x71, 4) => "psraw",
        (0x71, 6) => "psllw",
        (0x72, 2) => "psrld",
        (0x72, 4) => "psrad",
        (0x72, 6) => "pslld",
        (0x73, 2) => "psrlq",
        (0x73, 3) => "psrldq",
        (0x73, 6) => "psllq",
        (0x73, 7) => "pslldq",
        _ => "",
    }
}
