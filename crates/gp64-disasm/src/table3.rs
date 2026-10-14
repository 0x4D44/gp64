//! The x87 escape opcodes, `D8`-`DF`.
//!
//! These do not work like the rest of the map. The eight opcodes are an escape
//! hatch, and it is the ModRM byte — the whole byte, not just its `reg` field —
//! that says which instruction was written. Below `C0` the `reg` field selects
//! one of eight operations on a memory operand whose width the OPCODE fixes:
//! `D8 /0` adds four bytes, `DC /0` adds eight, and no prefix changes either.
//! At `C0` and above there is no memory operand at all and the byte addresses
//! the register stack, mostly as an operation in `reg` and a stack slot in
//! `rm` — but with enough singletons (`D9 D0` is FNOP, `DF E0` is FNSTSW AX)
//! that whole blocks have to be written out.
//!
//! Two traps are worth naming, because both are silent:
//!
//! The reversed subtract and divide. In the `DC` and `DE` register blocks the
//! reversed forms and the plain forms are the opposite way round from `D8`.
//! `D8 E0` is FSUB and `DC E0` is FSUBR, not FSUB — and likewise `F0` and `F8`
//! for divide. Get it backwards and the disassembly reads fine and means the
//! opposite thing.
//!
//! FNSTSW AX writes `ax`, never `eax`. It is the only operand here that names a
//! general register, and its width does not follow the operand size.

use crate::operand::{MemSize, Op};
use crate::table1::{ef, Entry};

const fn f0(m: &'static str) -> Entry {
    ef(m, Op::None, Op::None, Op::None)
}

const fn f1(m: &'static str, a: Op) -> Entry {
    ef(m, a, Op::None, Op::None)
}

const fn f2(m: &'static str, a: Op, b: Op) -> Entry {
    ef(m, a, b, Op::None)
}

/// Look up an x87 instruction from its escape opcode and ModRM byte.
///
/// This includes the encodings Intel never documented — FFREEP, the second
/// encodings of FSTP, FXCH, FCOM and FCOMP, and the 8087/287-era FNENI, FNDISI
/// and FNSETPM. They are named rather than refused, for two reasons.
///
/// A disassembler's job is to say what the bytes are. Whether this CPU would
/// execute them is the `--cpu` identity's question, answered by a #UD, and
/// refusing to name an encoding does not prevent anything — it just loses the
/// information in the dump where it was wanted.
///
/// And we execute them. `gp64-cpu`'s `x87_decode.rs` — the portable decoder
/// the shipped interpreter runs on — decodes every one of these, and `fpu.rs`
/// implements them down to FFREEP's stack semantics. A disassembler that
/// refused to name what the executor beside it runs would be lying by omission,
/// which is worse in a fault dump than any naming convention.
///
/// So the identities here follow `x87_decode.rs` rather than any outside
/// disassembler, and `x87_matches_the_executing_decoder` holds the two tables
/// to each other.
pub(crate) fn lookup(op: u8, modrm: u8) -> Option<Entry> {
    if modrm < 0xC0 {
        memory_form(op, (modrm >> 3) & 7)
    } else {
        register_form(op, modrm)
    }
}

/// The arithmetic escapes share one operation list and differ only in the width
/// they read.
const ARITH: [&str; 8] = [
    "fadd", "fmul", "fcom", "fcomp", "fsub", "fsubr", "fdiv", "fdivr",
];

/// The same eight, against an integer operand.
const IARITH: [&str; 8] = [
    "fiadd", "fimul", "ficom", "ficomp", "fisub", "fisubr", "fidiv", "fidivr",
];

/// The `mod != 11` forms: eight operations, one memory operand each.
///
/// Every arithmetic and data-movement form here also names the top of stack,
/// even though the encoding does not carry it — and Intel syntax puts it on the
/// side that says which way the data moves. A load reads memory INTO the stack
/// and is written `fld st,dword ptr [..]`; a store goes the other way and is
/// written `fstp dword ptr [..],st`. The control-word and environment ops name
/// no stack register at all, because they do not touch one.
fn memory_form(op: u8, reg: u8) -> Option<Entry> {
    use MemSize::{Dword, FpuEnv, FpuState, Qword, Tbyte, Unsized, Word};
    let r = reg as usize;

    // Memory into the stack.
    let load = |m: &'static str, size| f2(m, Op::St0, Op::Mx(size));
    // The stack into memory.
    let store = |m: &'static str, size| f2(m, Op::Mx(size), Op::St0);

    let entry = match op {
        0xD8 => load(ARITH[r], Dword),
        0xDC => load(ARITH[r], Qword),
        0xDA => load(IARITH[r], Dword),
        0xDE => load(IARITH[r], Word),
        0xD9 => match r {
            0 => load("fld", Dword),
            2 => store("fst", Dword),
            3 => store("fstp", Dword),
            // The environment block is the one x87 memory operand the opcode
            // does NOT fix: 14 bytes in 16-bit code, 28 in 32-bit.
            4 => f1("fldenv", Op::Mx(FpuEnv)),
            5 => f1("fldcw", Op::Mx(Unsized)),
            6 => f1("fnstenv", Op::Mx(FpuEnv)),
            7 => f1("fnstcw", Op::Mx(Unsized)),
            _ => return None,
        },
        0xDB => match r {
            0 => load("fild", Dword),
            1 => store("fisttp", Dword),
            2 => store("fist", Dword),
            3 => store("fistp", Dword),
            5 => load("fld", Tbyte),
            7 => store("fstp", Tbyte),
            _ => return None,
        },
        0xDD => match r {
            0 => load("fld", Qword),
            1 => store("fisttp", Qword),
            2 => store("fst", Qword),
            3 => store("fstp", Qword),
            4 => f1("frstor", Op::Mx(FpuState)),
            6 => f1("fnsave", Op::Mx(FpuState)),
            7 => f1("fnstsw", Op::Mx(Unsized)),
            _ => return None,
        },
        0xDF => match r {
            0 => load("fild", Word),
            1 => store("fisttp", Word),
            2 => store("fist", Word),
            3 => store("fistp", Word),
            4 => load("fbld", Tbyte),
            5 => load("fild", Qword),
            6 => store("fbstp", Tbyte),
            7 => store("fistp", Qword),
            _ => return None,
        },
        _ => return None,
    };

    Some(entry)
}

/// Instructions whose one-operand-implied form addresses `st(1)`.
///
/// `FXCH` written bare means `FXCH ST(1)`, `FADDP` means `FADDP ST(1),ST`, and
/// `FCOM` means `FCOM ST(1)` — the assembler's default operand, not a separate
/// encoding. So Intel syntax writes those encodings with no operands at all,
/// and spelling them out would be a different (if equivalent) source line.
///
/// `FADD ST,ST(1)` is deliberately absent: there is no bare `FADD`, so its
/// `st(1)` encoding has nothing shorter to collapse to.
const CANONICAL_AT_ST1: [&str; 11] = [
    "faddp", "fmulp", "fsubp", "fsubrp", "fdivp", "fdivrp", "fcom", "fcomp", "fucom", "fucomp",
    "fxch",
];

/// Drop the operands from an encoding that addresses `st(1)` and has a bare
/// form meaning exactly that.
fn canonical_form(entry: Entry, low: usize) -> Entry {
    if low == 1 && CANONICAL_AT_ST1.contains(&entry.mnemonic) {
        f0(entry.mnemonic)
    } else {
        entry
    }
}

/// The `mod == 11` forms, addressed by the whole low six bits of the ModRM
/// byte: a block in `reg`, and usually a stack slot in `rm`.
#[allow(clippy::too_many_lines)]
fn register_form(op: u8, modrm: u8) -> Option<Entry> {
    let block = (modrm >> 3) & 7;
    let low = (modrm & 7) as usize;

    // `st(0), st(i)` — the destination is the top of stack.
    const fn to_top(m: &'static str) -> Entry {
        f2(m, Op::St0, Op::St)
    }
    // `st(i), st(0)` — the destination is the addressed slot. The `DC` and `DE`
    // blocks are written this way round.
    const fn to_slot(m: &'static str) -> Entry {
        f2(m, Op::St, Op::St0)
    }

    let entry = match (op, block) {
        // -- D8: arithmetic against the top of stack ------------------------
        (0xD8, 0) => to_top("fadd"),
        (0xD8, 1) => to_top("fmul"),
        (0xD8, 2) => to_top("fcom"),
        (0xD8, 3) => to_top("fcomp"),
        (0xD8, 4) => to_top("fsub"),
        (0xD8, 5) => to_top("fsubr"),
        (0xD8, 6) => to_top("fdiv"),
        (0xD8, 7) => to_top("fdivr"),

        // -- D9: stack moves, constants and the transcendentals -------------
        (0xD9, 0) => to_top("fld"),
        (0xD9, 1) => to_top("fxch"),
        (0xD9, 2) if modrm == 0xD0 => f0("fnop"),
        // Undocumented: a second encoding of FSTP. `x87_decode.rs:508` runs it
        // as one, and so do we. (Other disassemblers call it FSTP1 or FSTPNCE;
        // our own executor's name wins here.)
        (0xD9, 3) => to_slot("fstp"),
        (0xD9, 4) => match modrm {
            0xE0 => f0("fchs"),
            0xE1 => f0("fabs"),
            0xE4 => f0("ftst"),
            0xE5 => f0("fxam"),
            _ => return None,
        },
        (0xD9, 5) => {
            const CONSTANTS: [&str; 8] = [
                "fld1", "fldl2t", "fldl2e", "fldpi", "fldlg2", "fldln2", "fldz", "",
            ];
            let m = CONSTANTS[low];
            if m.is_empty() {
                return None;
            }
            f0(m)
        }
        (0xD9, 6) => {
            const OPS: [&str; 8] = [
                "f2xm1", "fyl2x", "fptan", "fpatan", "fxtract", "fprem1", "fdecstp", "fincstp",
            ];
            f0(OPS[low])
        }
        (0xD9, 7) => {
            const OPS: [&str; 8] = [
                "fprem", "fyl2xp1", "fsqrt", "fsincos", "frndint", "fscale", "fsin", "fcos",
            ];
            f0(OPS[low])
        }

        // -- DA and DB: the conditional moves and the ordered compares -------
        (0xDA, 0) => to_top("fcmovb"),
        (0xDA, 1) => to_top("fcmove"),
        (0xDA, 2) => to_top("fcmovbe"),
        (0xDA, 3) => to_top("fcmovu"),
        (0xDA, 5) if modrm == 0xE9 => f0("fucompp"),

        (0xDB, 0) => to_top("fcmovnb"),
        (0xDB, 1) => to_top("fcmovne"),
        (0xDB, 2) => to_top("fcmovnbe"),
        (0xDB, 3) => to_top("fcmovnu"),
        (0xDB, 4) => match modrm {
            // FNENI, FNDISI and FNSETPM configured the 8087's interrupt and
            // protection behaviour. The 387 turned them into no-ops and Intel
            // stopped documenting them, but code from the era still contains
            // them and `x87_decode.rs:568` still runs them.
            0xE0 => f0("fneni"),
            0xE1 => f0("fndisi"),
            0xE2 => f0("fnclex"),
            0xE3 => f0("fninit"),
            0xE4 => f0("fnsetpm"),
            _ => return None,
        },
        (0xDB, 5) => to_top("fucomi"),
        (0xDB, 6) => to_top("fcomi"),

        // -- DC: arithmetic INTO the addressed slot -------------------------
        // Note the reversal against D8: `E0` is the reversed form here.
        (0xDC, 0) => to_slot("fadd"),
        (0xDC, 1) => to_slot("fmul"),
        // Undocumented: second encodings of FCOM and FCOMP, identical to the
        // `D8` forms right down to the operand order.
        (0xDC, 2) => to_top("fcom"),
        (0xDC, 3) => to_top("fcomp"),
        (0xDC, 4) => to_slot("fsubr"),
        (0xDC, 5) => to_slot("fsub"),
        (0xDC, 6) => to_slot("fdivr"),
        (0xDC, 7) => to_slot("fdiv"),

        // -- DD: stack management and the unordered compares ----------------
        (0xDD, 0) => f1("ffree", Op::St),
        // Undocumented: a second encoding of FXCH.
        (0xDD, 1) => to_top("fxch"),
        (0xDD, 2) => to_slot("fst"),
        (0xDD, 3) => to_slot("fstp"),
        (0xDD, 4) => to_top("fucom"),
        (0xDD, 5) => to_top("fucomp"),

        // -- DE: the popping arithmetic, reversed the same way as DC --------
        (0xDE, 0) => to_slot("faddp"),
        (0xDE, 1) => to_slot("fmulp"),
        // Undocumented: a third encoding of FCOMP.
        (0xDE, 2) => to_top("fcomp"),
        (0xDE, 3) if modrm == 0xD9 => f0("fcompp"),
        (0xDE, 4) => to_slot("fsubrp"),
        (0xDE, 5) => to_slot("fsubp"),
        (0xDE, 6) => to_slot("fdivrp"),
        (0xDE, 7) => to_slot("fdivp"),

        // -- DF: the status word and the popping ordered compares -----------
        // Undocumented FFREEP frees the selected register and pops the stack.
        (0xDF, 0) => f1("ffreep", Op::St),
        // ...followed by yet more FXCH and FSTP encodings.
        (0xDF, 1) => to_top("fxch"),
        (0xDF, 2 | 3) => to_slot("fstp"),
        (0xDF, 4) if modrm == 0xE0 => f1("fnstsw", Op::FixedAx),
        (0xDF, 5) => to_top("fucomip"),
        (0xDF, 6) => to_top("fcomip"),

        _ => return None,
    };

    Some(canonical_form(entry, low))
}
