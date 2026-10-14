//! The one-byte opcode map.
//!
//! Laid out to be diffable against the SDM's Appendix A table: rows of sixteen,
//! in opcode order, using the same operand abbreviations. Where an entry needs a
//! comment it gets one; where the manual's own row is regular, the code is too.
//!
//! Mnemonics are `&'static str` rather than an enum on purpose. Every consumer
//! of this crate wants TEXT — a fault dump, a `disasm` command — and none of
//! them dispatches on which instruction it is, so an enum with seven hundred
//! variants would be a large amount of code buying nothing.

use crate::operand::Op;

/// One opcode's mnemonic and up to three operands.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Entry {
    pub(crate) mnemonic: &'static str,
    pub(crate) ops: [Op; 3],
    /// This mnemonic has exactly ONE legal memory-operand size, so an explicit
    /// size marker would say nothing.
    ///
    /// The marker exists to disambiguate: `inc byte ptr [bx]` needs it because
    /// `inc word ptr [bx]` is a different encoding of the same mnemonic. `lldt
    /// [bx]` does not, because there is no other `lldt`. That is a property of
    /// the instruction rather than of the operand slot — `Ew` wants a marker
    /// under `movzx` and none under `lldt` — which is why it lives here and not
    /// in `Op`.
    pub(crate) fixed_size: bool,
}

pub(crate) const fn e(mnemonic: &'static str, a: Op, b: Op, c: Op) -> Entry {
    Entry {
        mnemonic,
        ops: [a, b, c],
        fixed_size: false,
    }
}

/// An entry whose memory operand has exactly one legal size. See
/// [`Entry::fixed_size`].
pub(crate) const fn ef(mnemonic: &'static str, a: Op, b: Op, c: Op) -> Entry {
    Entry {
        mnemonic,
        ops: [a, b, c],
        fixed_size: true,
    }
}

const fn e0(mnemonic: &'static str) -> Entry {
    e(mnemonic, Op::None, Op::None, Op::None)
}

const fn e1(mnemonic: &'static str, a: Op) -> Entry {
    e(mnemonic, a, Op::None, Op::None)
}

const fn e2(mnemonic: &'static str, a: Op, b: Op) -> Entry {
    e(mnemonic, a, b, Op::None)
}

/// An opcode that is not an instruction on the CPUs this project models.
const NONE: Entry = e0("");

/// The eight ALU operations, in the order the map repeats them.
const ALU: [&str; 8] = ["add", "or", "adc", "sbb", "and", "sub", "xor", "cmp"];

/// The one-byte map.
///
/// Returns `None` for an opcode that is not an instruction. Prefixes are
/// consumed before this is consulted, so their slots are unreachable and left
/// as `NONE`.
pub(crate) fn lookup(op: u8) -> Option<Entry> {
    // 00-3F: eight ALU groups of six, each followed by two segment-register
    // pushes/pops. The regularity is worth exploiting — spelling out
    // forty-eight near-identical rows invites a transcription error in one of
    // them that no reader would spot.
    if op < 0x40 && (op & 7) < 6 {
        let name = ALU[(op >> 3) as usize];
        return Some(match op & 7 {
            0 => e2(name, Op::Eb, Op::Gb),
            1 => e2(name, Op::Ev, Op::Gv),
            2 => e2(name, Op::Gb, Op::Eb),
            3 => e2(name, Op::Gv, Op::Ev),
            4 => e2(name, Op::FixedB(0), Op::Ib),
            _ => e2(name, Op::FixedV(0), Op::Iz),
        });
    }

    let entry = match op {
        0x06 => e1("push", Op::FixedSeg(0)),
        0x07 => e1("pop", Op::FixedSeg(0)),
        0x0E => e1("push", Op::FixedSeg(1)),
        // 0x0F is the two-byte escape and never reaches here.
        0x16 => e1("push", Op::FixedSeg(2)),
        0x17 => e1("pop", Op::FixedSeg(2)),
        0x1E => e1("push", Op::FixedSeg(3)),
        0x1F => e1("pop", Op::FixedSeg(3)),
        0x27 => e0("daa"),
        0x2F => e0("das"),
        0x37 => e0("aaa"),
        0x3F => e0("aas"),

        0x40..=0x47 => e1("inc", Op::OpcodeV),
        0x48..=0x4F => e1("dec", Op::OpcodeV),
        0x50..=0x57 => e1("push", Op::OpcodeV),
        0x58..=0x5F => e1("pop", Op::OpcodeV),

        0x60 => e0("pushad"),
        0x61 => e0("popad"),
        0x62 => e2("bound", Op::Gv, Op::Ma),
        0x63 => e2("arpl", Op::Ew, Op::Gw),
        0x68 => e1("push", Op::Iz),
        0x69 => e("imul", Op::Gv, Op::Ev, Op::Iz),
        0x6A => e1("push", Op::Ibs),
        0x6B => e("imul", Op::Gv, Op::Ev, Op::Ibs),
        0x6C => e2("insb", Op::Yb, Op::Dx),
        0x6D => e2("insd", Op::Yv, Op::Dx),
        0x6E => e2("outsb", Op::Dx, Op::Xb),
        0x6F => e2("outsd", Op::Dx, Op::Xv),

        0x70..=0x7F => e1(JCC[(op & 0xF) as usize], Op::Jb),

        // Group 1: the operation comes from ModRM.reg, so the caller patches the
        // mnemonic in. `0x82` is an undocumented alias of `0x80` that silicon
        // accepts, and the map keeps it for that reason.
        0x80 | 0x82 => e2("", Op::Eb, Op::Ib),
        0x81 => e2("", Op::Ev, Op::Iz),
        0x83 => e2("", Op::Ev, Op::Ibs),

        0x84 => e2("test", Op::Eb, Op::Gb),
        0x85 => e2("test", Op::Ev, Op::Gv),
        0x86 => e2("xchg", Op::Eb, Op::Gb),
        0x87 => e2("xchg", Op::Ev, Op::Gv),
        0x88 => e2("mov", Op::Eb, Op::Gb),
        0x89 => e2("mov", Op::Ev, Op::Gv),
        0x8A => e2("mov", Op::Gb, Op::Eb),
        0x8B => e2("mov", Op::Gv, Op::Ev),
        0x8C => e2("mov", Op::EwOrV, Op::Sw),
        0x8D => e2("lea", Op::Gv, Op::M),
        0x8E => e2("mov", Op::Sw, Op::Ew),
        0x8F => e1("pop", Op::Ev),

        // 0x90 is XCHG eAX,eAX, which every assembler spells NOP.
        0x90 => e0("nop"),
        // Operand order is register-then-accumulator: 91 is `xchg cx,ax`.
        0x91..=0x97 => e2("xchg", Op::OpcodeV, Op::FixedV(0)),
        0x98 => e0("cwde"),
        0x99 => e0("cdq"),
        0x9A => e1("call", Op::Ap),
        0x9B => e0("fwait"),
        0x9C => e0("pushfd"),
        0x9D => e0("popfd"),
        0x9E => e0("sahf"),
        0x9F => e0("lahf"),

        0xA0 => e2("mov", Op::FixedB(0), Op::Ob),
        0xA1 => e2("mov", Op::FixedV(0), Op::Ov),
        0xA2 => e2("mov", Op::Ob, Op::FixedB(0)),
        0xA3 => e2("mov", Op::Ov, Op::FixedV(0)),
        0xA4 => e2("movsb", Op::Yb, Op::Xb),
        0xA5 => e2("movsd", Op::Yv, Op::Xv),
        0xA6 => e2("cmpsb", Op::Xb, Op::Yb),
        0xA7 => e2("cmpsd", Op::Xv, Op::Yv),
        0xA8 => e2("test", Op::FixedB(0), Op::Ib),
        0xA9 => e2("test", Op::FixedV(0), Op::Iz),
        0xAA => e1("stosb", Op::Yb),
        0xAB => e1("stosd", Op::Yv),
        0xAC => e1("lodsb", Op::Xb),
        0xAD => e1("lodsd", Op::Xv),
        0xAE => e1("scasb", Op::Yb),
        0xAF => e1("scasd", Op::Yv),

        0xB0..=0xB7 => e2("mov", Op::OpcodeB, Op::Ib),
        0xB8..=0xBF => e2("mov", Op::OpcodeV, Op::Iz),

        // Group 2 (shifts): mnemonic from ModRM.reg.
        0xC0 => e2("", Op::Eb, Op::Ib),
        0xC1 => e2("", Op::Ev, Op::Ib),
        0xC2 => e1("ret", Op::Iw),
        0xC3 => e0("ret"),
        0xC4 => e2("les", Op::Gv, Op::Mp),
        0xC5 => e2("lds", Op::Gv, Op::Mp),
        0xC6 => e2("mov", Op::Eb, Op::Ib),
        0xC7 => e2("mov", Op::Ev, Op::Iz),
        0xC8 => e2("enter", Op::Iw, Op::Ib),
        0xC9 => e0("leave"),
        0xCA => e1("ret far", Op::Iw),
        0xCB => e0("ret far"),
        0xCC => e0("int3"),
        0xCD => e1("int", Op::Ib),
        0xCE => e0("into"),
        0xCF => e0("iretd"),

        0xD0 => e2("", Op::Eb, Op::One),
        0xD1 => e2("", Op::Ev, Op::One),
        0xD2 => e2("", Op::Eb, Op::Cl),
        0xD3 => e2("", Op::Ev, Op::Cl),
        0xD4 => e1("aam", Op::Ib),
        0xD5 => e1("aad", Op::Ib),
        0xD6 => e0("salc"),
        0xD7 => e1("xlat", Op::XlatTable),
        // D8-DF are the x87 escapes, handled by their own table.
        0xD8..=0xDF => NONE,

        0xE0 => e1("loopne", Op::Jb),
        0xE1 => e1("loope", Op::Jb),
        0xE2 => e1("loop", Op::Jb),
        0xE3 => e1("jecxz", Op::Jb),
        0xE4 => e2("in", Op::FixedB(0), Op::Ib),
        0xE5 => e2("in", Op::FixedV(0), Op::Ib),
        0xE6 => e2("out", Op::Ib, Op::FixedB(0)),
        0xE7 => e2("out", Op::Ib, Op::FixedV(0)),
        0xE8 => e1("call", Op::Jz),
        0xE9 => e1("jmp", Op::Jz),
        0xEA => e1("jmp", Op::Ap),
        0xEB => e1("jmp", Op::Jb),
        0xEC => e2("in", Op::FixedB(0), Op::Dx),
        0xED => e2("in", Op::FixedV(0), Op::Dx),
        0xEE => e2("out", Op::Dx, Op::FixedB(0)),
        0xEF => e2("out", Op::Dx, Op::FixedV(0)),

        0xF1 => e0("int1"),
        0xF4 => e0("hlt"),
        0xF5 => e0("cmc"),
        // Group 3: mnemonic AND operand count both come from ModRM.reg.
        0xF6 | 0xF7 => NONE,
        0xF8 => e0("clc"),
        0xF9 => e0("stc"),
        0xFA => e0("cli"),
        0xFB => e0("sti"),
        0xFC => e0("cld"),
        0xFD => e0("std"),
        // Groups 4 and 5: mnemonic from ModRM.reg.
        0xFE | 0xFF => NONE,

        _ => NONE,
    };

    // Group opcodes legitimately arrive here with an empty mnemonic: theirs
    // comes from ModRM.reg and the caller patches it in.
    if entry.mnemonic.is_empty()
        && !matches!(op, 0x80..=0x83 | 0xC0 | 0xC1 | 0xD0..=0xD3 | 0xF6 | 0xF7 | 0xFE | 0xFF)
    {
        None
    } else {
        Some(entry)
    }
}

/// The sixteen condition suffixes, in encoding order.
///
/// Several conditions have two accepted spellings (`jz`/`je`, `jnae`/`jb`).
/// These are the ones Intel syntax conventionally prints.
pub(crate) const JCC: [&str; 16] = [
    "jo", "jno", "jb", "jae", "je", "jne", "jbe", "ja", "js", "jns", "jp", "jnp", "jl", "jge",
    "jle", "jg",
];

/// Group 1 (`80`-`83`), selected by ModRM.reg.
pub(crate) const GROUP1: [&str; 8] = ALU;

/// Group 2 (`C0`, `C1`, `D0`-`D3`), selected by ModRM.reg.
///
/// `/6` is an undocumented alias of `/4` that silicon executes as SHL.
pub(crate) const GROUP2: [&str; 8] = ["rol", "ror", "rcl", "rcr", "shl", "shr", "shl", "sar"];

/// Group 3 (`F6`, `F7`), selected by ModRM.reg. `/0` and `/1` take an immediate;
/// the rest are single-operand.
pub(crate) const GROUP3: [&str; 8] = ["test", "test", "not", "neg", "mul", "imul", "div", "idiv"];

/// Group 5 (`FF`), selected by ModRM.reg.
pub(crate) const GROUP5: [&str; 8] = ["inc", "dec", "call", "call", "jmp", "jmp", "push", ""];
