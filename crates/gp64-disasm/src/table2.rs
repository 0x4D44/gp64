//! The two-byte opcode map (`0F xx`).
//!
//! Covers the integer, system and control-flow half of the map: `Jcc near`,
//! `SETcc`, `CMOVcc`, the bit instructions, `MOVZX`/`MOVSX`, `BSWAP`, the far
//! segment loads and groups 6, 7, 8, 9, 15 and 16.
//!
//! The SSE and MMX half (`0F 10`-`0F 17`, `0F 28`-`0F 7F`, `0F C2`-`0F FF`) is
//! deliberately absent. It needs two things this module does not yet have: XMM
//! and MMX operand kinds, and dispatch on a MANDATORY PREFIX — `0F 10` is
//! `movups` bare, `movupd` under `66`, `movss` under `F3` and `movsd` under
//! `F2`, four different instructions sharing an opcode where the `66` byte is
//! not an operand-size override at all but part of the encoding. Those opcodes
//! decode as invalid here rather than half-right, so the differential sweep
//! counts them honestly as not-ours instead of silently agreeing.

use crate::operand::Op;
use crate::table1::{e, ef, Entry};

/// The prefix byte that selects which instruction an opcode encodes.
///
/// Across most of this map a `66` is an operand-size override. Across the
/// SSE/MMX half it is part of the encoding: `0F 6F` bare is the MMX `movq`,
/// under `66` it is the SSE2 `movdqa`, under `F3` the SSE2 `movdqu`. They are
/// three different instructions, not one instruction at three widths.
///
/// Only the bare forms are decoded here — those are the MMX ones. The prefixed
/// forms are SSE and refuse, so a `66 0F 6F` reads as not-ours rather than
/// being confidently mislabelled as an MMX move.
///
/// `F2` and `F3` outrank `66`: `F3 66 0F 10` is `movss`, not `movupd`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mandatory {
    None,
    P66,
    PF2,
    PF3,
}

const fn e0(m: &'static str) -> Entry {
    e(m, Op::None, Op::None, Op::None)
}

/// One operand, and the mnemonic has exactly one legal memory size — see
/// [`crate::table1::Entry::fixed_size`].
const fn f1(m: &'static str, a: Op) -> Entry {
    ef(m, a, Op::None, Op::None)
}

const fn e1(m: &'static str, a: Op) -> Entry {
    e(m, a, Op::None, Op::None)
}

const fn e2(m: &'static str, a: Op, b: Op) -> Entry {
    e(m, a, b, Op::None)
}

/// Group 8 (`0F BA`), selected by ModRM.reg. `/0`-`/3` are reserved.
pub(crate) const GROUP8: [&str; 8] = ["", "", "", "", "bt", "bts", "btr", "btc"];

/// Look up a two-byte opcode.
///
/// Returns `None` for an opcode that is not an instruction on the CPUs this
/// project models.
#[allow(clippy::too_many_lines, clippy::match_same_arms)]
pub(crate) fn lookup(op2: u8, mandatory: Mandatory, modrm: u8) -> Option<Entry> {
    // Across this range the prefix picks the instruction, not the width. The
    // bare forms are MMX and the prefixed ones are SSE.
    if is_sse_mmx_opcode(op2) {
        if mandatory != Mandatory::None {
            return crate::sse::lookup(op2, mandatory, modrm);
        }
        if let Some(entry) = mmx(op2) {
            return Some(entry);
        }
    }
    // The float half has no MMX forms at all, so the bare encoding is SSE too.
    if let Some(entry) = crate::sse::lookup(op2, mandatory, modrm) {
        return Some(entry);
    }
    let entry = match op2 {
        // -- 00-0F: system, groups 6 and 7, and the first SSE moves ----------
        // Group 6 and group 7 take their mnemonic from ModRM.reg.
        0x00 | 0x01 => e1("", Op::Ev),
        0x02 => ef("lar", Op::Gv, Op::EwOrV, Op::None),
        0x03 => ef("lsl", Op::Gv, Op::EwOrV, Op::None),
        0x05 => e0("syscall"),
        0x06 => e0("clts"),
        0x07 => e0("sysret"),
        0x08 => e0("invd"),
        0x09 => e0("wbinvd"),
        0x0B => e0("ud2"),
        // -- 0D-0F: AMD 3DNow! ----------------------------------------------
        // No Intel CPU has these, but the K6-2 and K6-III do, they are squarely
        // period-correct for what this emulator targets, and real software used
        // would execute them is a separate question, answered by a #UD; naming
        // them is this crate's job either way.
        // 0D is a group, patched from ModRM.reg.
        0x0D => e1("", Op::M),
        0x0E => e0("femms"),
        // 0F is unique in the whole map: a ModRM AND a trailing byte that
        // selects the operation. Patched from that suffix once the ModRM's
        // displacement has been walked, since the suffix sits after it.
        0x0F => ef("", Op::Pq, Op::Qq, Op::None),

        // -- 10-17: the SSE move block ---------------------------------------
        // Deferred to the SSE stage: these take XMM operands, which the operand
        // model does not carry yet, and they are selected by their mandatory
        // prefix rather than by the opcode alone. Left undecoded rather than
        // half-decoded, so the sweep counts them honestly as not-ours.

        // -- 18-1F: the hint-NOP group, including the multi-byte NOP ---------
        // 18 is group 16 (the prefetch hints), patched from ModRM.reg.
        0x18 => e1("", Op::Ev),
        // The reserved-NOP block. These take a full ModRM and print BOTH its
        // fields — `nop [bx+si],sp` — because the encoding really does name two
        // operands, even though the instruction reads neither.
        0x19..=0x1E => e2("nop", Op::Ev, Op::Gv),
        // 1F is the same block, except that `/0` is the one encoding Intel
        // actually documents as the multi-byte NOP, and is written with a
        // single operand. Patched from ModRM.reg like a group.
        0x1F => e1("", Op::Ev),

        // -- 20-23: MOV to/from CR and DR ------------------------------------
        // These ignore the ModRM mod field entirely and are always register
        // form, which is why `crate::instruction_length` special-cases them.
        // The general register is 32-bit whatever the operand-size prefix says.
        0x20 => e2("mov", Op::Rd, Op::Cd),
        0x21 => e2("mov", Op::Rd, Op::Dd),
        0x22 => e2("mov", Op::Cd, Op::Rd),
        0x23 => e2("mov", Op::Dd, Op::Rd),

        // -- 30-37: model-specific registers and timing ----------------------
        0x30 => e0("wrmsr"),
        0x31 => e0("rdtsc"),
        0x32 => e0("rdmsr"),
        0x33 => e0("rdpmc"),
        0x34 => e0("sysenter"),
        0x35 => e0("sysexit"),

        // -- 40-4F: CMOVcc ---------------------------------------------------
        0x40..=0x4F => e2(CMOV[(op2 & 0xF) as usize], Op::Gv, Op::Ev),

        // -- 80-8F: Jcc near -------------------------------------------------
        0x80..=0x8F => e1(JCC_NEAR[(op2 & 0xF) as usize], Op::Jz),

        // -- 90-9F: SETcc ----------------------------------------------------
        0x90..=0x9F => f1(SETCC[(op2 & 0xF) as usize], Op::Eb),

        // -- A0-AF: segment pushes, bit tests, shifts, CPUID -----------------
        0xA0 => e1("push", Op::FixedSeg(4)),
        0xA1 => e1("pop", Op::FixedSeg(4)),
        0xA2 => e0("cpuid"),
        0xA3 => e2("bt", Op::Ev, Op::Gv),
        0xA4 => e("shld", Op::Ev, Op::Gv, Op::Ib),
        0xA5 => e("shld", Op::Ev, Op::Gv, Op::Cl),
        0xA8 => e1("push", Op::FixedSeg(5)),
        0xA9 => e1("pop", Op::FixedSeg(5)),
        0xAA => e0("rsm"),
        0xAB => e2("bts", Op::Ev, Op::Gv),
        0xAC => e("shrd", Op::Ev, Op::Gv, Op::Ib),
        0xAD => e("shrd", Op::Ev, Op::Gv, Op::Cl),
        // The FXSAVE group takes its mnemonic from ModRM.reg.
        0xAE => e1("", Op::M),
        0xAF => e2("imul", Op::Gv, Op::Ev),

        // -- B0-BF: compare-exchange, far loads, bit scans, extends ----------
        0xB0 => e2("cmpxchg", Op::Eb, Op::Gb),
        0xB1 => e2("cmpxchg", Op::Ev, Op::Gv),
        0xB2 => e2("lss", Op::Gv, Op::Mp),
        0xB3 => e2("btr", Op::Ev, Op::Gv),
        0xB4 => e2("lfs", Op::Gv, Op::Mp),
        0xB5 => e2("lgs", Op::Gv, Op::Mp),
        0xB6 => e2("movzx", Op::Gv, Op::Eb),
        0xB7 => e2("movzx", Op::Gv, Op::Ew),
        // UD1 and UD0 are the encodings Intel guarantees will fault. They take
        // a ModRM and print both its fields, and naming them beats printing
        // raw bytes in the dump of a program that just executed one.
        0xB9 => e2("ud1", Op::Gv, Op::Ev),
        0xBA => e2("", Op::Ev, Op::Ib),
        0xBB => e2("btc", Op::Ev, Op::Gv),
        0xBC => e2("bsf", Op::Gv, Op::Ev),
        0xBD => e2("bsr", Op::Gv, Op::Ev),
        0xBE => e2("movsx", Op::Gv, Op::Eb),
        0xBF => e2("movsx", Op::Gv, Op::Ew),

        // -- C0-CF: exchange-and-add, and BSWAP ------------------------------
        0xC0 => e2("xadd", Op::Eb, Op::Gb),
        0xC1 => e2("xadd", Op::Ev, Op::Gv),
        // C3 (MOVNTI) is deferred with the SSE stage: it is memory-only and its
        // operands are 32-bit whatever the operand-size prefix says, which needs
        // an operand kind the model does not carry yet.
        // The CMPXCHG8B group takes its mnemonic from ModRM.reg.
        0xC7 => e1("", Op::M),
        0xFF => e2("ud0", Op::Gv, Op::Ev),
        // BSWAP takes its register from the opcode and NO ModRM byte.
        0xC8..=0xCF => e1("bswap", Op::OpcodeV),

        _ => e0(""),
    };

    if entry.mnemonic.is_empty() && !is_group(op2) && !defers_to_suffix(op2) {
        None
    } else {
        Some(entry)
    }
}

/// The 3DNow! opcode, whose mnemonic comes from a byte after the ModRM and any
/// displacement — so an empty mnemonic is expected here too, but it cannot be
/// resolved from the ModRM byte the way a group's can.
pub(crate) const fn defers_to_suffix(op2: u8) -> bool {
    op2 == 0x0F
}

/// Opcodes whose mnemonic comes from ModRM.reg, so an empty one is expected.
pub(crate) const fn is_group(op2: u8) -> bool {
    matches!(
        op2,
        0x00 | 0x01 | 0x0D | 0x18 | 0x1F | 0x71..=0x73 | 0xAE | 0xBA | 0xC7 | 0xD7 | 0xE7 | 0xF7
    )
}

const CMOV: [&str; 16] = [
    "cmovo", "cmovno", "cmovb", "cmovae", "cmove", "cmovne", "cmovbe", "cmova", "cmovs", "cmovns",
    "cmovp", "cmovnp", "cmovl", "cmovge", "cmovle", "cmovg",
];

const JCC_NEAR: [&str; 16] = [
    "jo", "jno", "jb", "jae", "je", "jne", "jbe", "ja", "js", "jns", "jp", "jnp", "jl", "jge",
    "jle", "jg",
];

const SETCC: [&str; 16] = [
    "seto", "setno", "setb", "setae", "sete", "setne", "setbe", "seta", "sets", "setns", "setp",
    "setnp", "setl", "setge", "setle", "setg",
];

/// Resolve a two-byte group opcode, whose mnemonic comes from ModRM.reg.
///
/// Some of these also depend on ModRM.mod, which is why the raw field is taken
/// rather than a "is it memory" boolean: `0F 01 /2` is `lgdt` against memory and
/// a *different instruction entirely* (`xgetbv`/`xsetbv`) against a register.
/// Those register forms are all post-Pentium-4, so they decode as invalid here
/// rather than as an `lgdt` with a nonsense operand.
///
/// Returns `None` when the encoding is not an instruction on our CPUs.
pub(crate) fn group_entry(op2: u8, modrm: u8, m: Mandatory) -> Option<Entry> {
    let reg = (modrm >> 3) & 7;
    let r = reg as usize;
    let is_reg_form = modrm >> 6 == 3;

    // The opcodes MMX and SSE share resolve their prefixed forms in one place,
    // next to the rest of SSE, rather than being half-decided here.
    if m != Mandatory::None && matches!(op2, 0xD7 | 0xE7 | 0xF7) {
        return crate::sse::lookup(op2, m, modrm);
    }

    let entry = match op2 {
        // Group 6. Every entry accepts both a register and a memory operand.
        0x00 => match r {
            0 => f1("sldt", Op::EwOrV),
            1 => f1("str", Op::EwOrV),
            2 => f1("lldt", Op::Ew),
            3 => f1("ltr", Op::Ew),
            4 => f1("verr", Op::Ew),
            5 => f1("verw", Op::Ew),
            _ => return None,
        },
        // Group 7. The descriptor-table ops are memory-only.
        0x01 => match (r, is_reg_form) {
            (0, false) => f1("sgdt", Op::M),
            (1, false) => f1("sidt", Op::M),
            (2, false) => f1("lgdt", Op::M),
            (3, false) => f1("lidt", Op::M),
            (4, _) => f1("smsw", Op::EwOrV),
            (6, _) => f1("lmsw", Op::Ew),
            (7, false) => f1("invlpg", Op::M),
            // Every register form of /0, /1, /2, /3 and /7 is a post-Pentium-4
            // instruction: VMX, MONITOR/MWAIT, XGETBV/XSETBV, SVM, SWAPGS,
            // RDTSCP. None of them exist on a CPU we model.
            _ => return None,
        },
        // AMD's 3DNow! prefetch group. `/0` reads and `/1` writes; AMD
        // documents `/2`-`/7` as reserved encodings that behave as PREFETCH,
        // which is what they are named here. Like Intel's group 16 next door,
        // the register forms are reserved NOPs — there is nothing to prefetch
        // in a register.
        0x0D => {
            if is_reg_form {
                e2("nop", Op::Ev, Op::Gv)
            } else if r == 1 {
                f1("prefetchw", Op::M)
            } else {
                f1("prefetch", Op::M)
            }
        }
        // Group 16, the prefetch hints. The register form of every entry is a
        // reserved NOP rather than a prefetch.
        0x18 => match (r, is_reg_form) {
            (0..=3, false) => f1(PREFETCH[r], Op::M),
            _ => e2("nop", Op::Ev, Op::Gv),
        },
        // The documented multi-byte NOP is `/0` alone; the other seven are
        // reserved encodings, which are written with both ModRM fields.
        0x1F => {
            if r == 0 {
                f1("nop", Op::Ev)
            } else {
                e2("nop", Op::Ev, Op::Gv)
            }
        }
        // Group 15. Split by mod: the fences are register-form-only, and the
        // save/restore and MXCSR ops are memory-only.
        0xAE => match (r, is_reg_form) {
            (0, false) => f1("fxsave", Op::M),
            (1, false) => f1("fxrstor", Op::M),
            (2, false) => f1("ldmxcsr", Op::M),
            (3, false) => f1("stmxcsr", Op::M),
            (5, true) => e0("lfence"),
            (6, true) => e0("mfence"),
            (7, true) => e0("sfence"),
            (7, false) => f1("clflush", Op::M),
            _ => return None,
        },
        // The MMX shift-by-immediate groups. The operand is always an MMX
        // register — these have no memory form, so a memory ModRM is not a
        // shift with a memory operand, it is not an instruction.
        0x71..=0x73 => {
            if !is_reg_form {
                return None;
            }
            // Under `66` these shift XMM registers and gain the two byte-wise
            // whole-register shifts; bare they are the MMX forms.
            let (name, operand) = if m == Mandatory::P66 {
                (crate::sse::sse2_shift_group(op2, reg), Op::Ux)
            } else if m == Mandatory::None {
                (mmx_shift_group(op2, reg), Op::Nq)
            } else {
                return None;
            };
            if name.is_empty() {
                return None;
            }
            e(name, operand, Op::Ib, Op::None)
        }
        // PMOVMSKB collects sign bits out of an MMX register into a general
        // one. Register-only on both halves.
        0xD7 if is_reg_form => ef("pmovmskb", Op::Gd, Op::Nq, Op::None),
        // MOVNTQ is a non-temporal store, so it is memory-only — the mirror
        // restriction. The `mm` operand beside it already says 64 bits, so no
        // size marker is printed.
        0xE7 if !is_reg_form => ef("movntq", Op::M, Op::Pq, Op::None),
        // MASKMOVQ writes through DS:DI/EDI, which Intel syntax leaves implicit.
        0xF7 if is_reg_form => ef("maskmovq", Op::Pq, Op::Nq, Op::None),
        0xD7 | 0xE7 | 0xF7 => return None,
        // Group 8. `/0`-`/3` are reserved.
        0xBA => {
            let m = GROUP8[r];
            if m.is_empty() {
                return None;
            }
            e2(m, Op::Ev, Op::Ib)
        }
        // Group 9. Only `/1` is an instruction, and it is memory-only.
        0xC7 => match (r, is_reg_form) {
            (1, false) => f1("cmpxchg8b", Op::M),
            _ => return None,
        },
        _ => return None,
    };

    Some(entry)
}

const PREFETCH: [&str; 4] = ["prefetchnta", "prefetcht0", "prefetcht1", "prefetcht2"];

/// Opcodes where a mandatory prefix selects a different instruction rather than
/// a different operand size.
const fn is_sse_mmx_opcode(op2: u8) -> bool {
    matches!(op2, 0x60..=0x7F | 0xD0..=0xFF)
}

/// The packed-integer mnemonics MMX and SSE2 share. SSE2 is the same
/// instruction on a wider register file, so the name is the same and only the
/// operand kinds differ.
pub(crate) fn packed_integer_mnemonic(op2: u8) -> Option<&'static str> {
    mmx(op2)
        .map(|entry| entry.mnemonic)
        .filter(|m| !m.is_empty())
}

/// One MMX register operand from ModRM.reg and one from r/m — the shape almost
/// every MMX arithmetic instruction takes.
const fn pq(m: &'static str) -> Entry {
    ef(m, Op::Pq, Op::Qq, Op::None)
}

/// The MMX half of the two-byte map.
///
/// Every entry is `fixed_size`: an MMX instruction has exactly one legal memory
/// operand width, so naming it would say nothing. `movd` reads four bytes and
/// everything else reads eight, and no prefix changes either — a `66` here
/// selects a different instruction rather than a wider one.
///
/// Spans three CPU generations, all of which this project models: the original
/// Pentium MMX set, the "MMX extensions" that arrived with SSE on the Pentium
/// III (the averages, min/max, `pmulhuw`, `pshufw`, `pmovmskb`, `psadbw`,
/// `maskmovq`, `movntq`), and the three 64-bit forms SSE2 added on the
/// Pentium 4 (`paddq`, `psubq`, `pmuludq`).
#[allow(clippy::too_many_lines)]
fn mmx(op2: u8) -> Option<Entry> {
    let entry = match op2 {
        0x60 => pq("punpcklbw"),
        0x61 => pq("punpcklwd"),
        0x62 => pq("punpckldq"),
        0x63 => pq("packsswb"),
        0x64 => pq("pcmpgtb"),
        0x65 => pq("pcmpgtw"),
        0x66 => pq("pcmpgtd"),
        0x67 => pq("packuswb"),
        0x68 => pq("punpckhbw"),
        0x69 => pq("punpckhwd"),
        0x6A => pq("punpckhdq"),
        0x6B => pq("packssdw"),
        // MOVD moves four bytes between an MMX register and a general register
        // or memory; it is the one MMX opcode whose memory operand is not 64
        // bits, and the width does not follow the operand-size prefix.
        0x6E => ef("movd", Op::Pq, Op::Ed, Op::None),
        0x6F => pq("movq"),
        0x70 => ef("pshufw", Op::Pq, Op::Qq, Op::Ib),
        // 71-73 are shift groups: ModRM.reg picks the direction and width, and
        // the operand is always an MMX REGISTER — there is no memory form.
        0x71..=0x73 => e("", Op::Nq, Op::Ib, Op::None),
        0x74 => pq("pcmpeqb"),
        0x75 => pq("pcmpeqw"),
        0x76 => pq("pcmpeqd"),
        // EMMS takes no ModRM at all, which is why the length table treats it
        // separately from its neighbours.
        0x77 => e("emms", Op::None, Op::None, Op::None),
        0x7E => ef("movd", Op::Ed, Op::Pq, Op::None),
        0x7F => ef("movq", Op::Qq, Op::Pq, Op::None),

        0xD1 => pq("psrlw"),
        0xD2 => pq("psrld"),
        0xD3 => pq("psrlq"),
        0xD4 => pq("paddq"),
        0xD5 => pq("pmullw"),

        0xD8 => pq("psubusb"),
        0xD9 => pq("psubusw"),
        0xDA => pq("pminub"),
        0xDB => pq("pand"),
        0xDC => pq("paddusb"),
        0xDD => pq("paddusw"),
        0xDE => pq("pmaxub"),
        0xDF => pq("pandn"),

        0xE0 => pq("pavgb"),
        0xE1 => pq("psraw"),
        0xE2 => pq("psrad"),
        0xE3 => pq("pavgw"),
        0xE4 => pq("pmulhuw"),
        0xE5 => pq("pmulhw"),

        0xE8 => pq("psubsb"),
        0xE9 => pq("psubsw"),
        0xEA => pq("pminsw"),
        0xEB => pq("por"),
        0xEC => pq("paddsb"),
        0xED => pq("paddsw"),
        0xEE => pq("pmaxsw"),
        0xEF => pq("pxor"),

        0xF1 => pq("psllw"),
        0xF2 => pq("pslld"),
        0xF3 => pq("psllq"),
        0xF4 => pq("pmuludq"),
        0xF5 => pq("pmaddwd"),
        0xF6 => pq("psadbw"),

        0xF8 => pq("psubb"),
        0xF9 => pq("psubw"),
        0xFA => pq("psubd"),
        0xFB => pq("psubq"),
        0xFC => pq("paddb"),
        0xFD => pq("paddw"),
        0xFE => pq("paddd"),
        _ => return None,
    };
    Some(entry)
}

/// The MMX shift groups (`0F 71`-`0F 73`), selected by ModRM.reg. The gaps are
/// the SSE2 whole-register shifts, which need a `66` prefix and are not MMX.
pub(crate) const fn mmx_shift_group(op2: u8, reg: u8) -> &'static str {
    match (op2, reg) {
        (0x71, 2) => "psrlw",
        (0x71, 4) => "psraw",
        (0x71, 6) => "psllw",
        (0x72, 2) => "psrld",
        (0x72, 4) => "psrad",
        (0x72, 6) => "pslld",
        (0x73, 2) => "psrlq",
        (0x73, 6) => "psllq",
        _ => "",
    }
}

/// The 3DNow! suffix table.
///
/// `0F 0F` carries a ModRM and then a trailing byte, and it is that byte — not
/// the opcode — that says which operation was written. Nothing else in the map
/// works this way, which is why it is a lookup of its own rather than an entry.
///
/// The assignments are AMD's, and cover both the original 3DNow! set and the
/// "3DNow! Extensions" the Athlon added (`pi2fw`, `pf2iw`, `pfnacc`, `pfpnacc`,
/// `pswapd`).
pub(crate) const fn threednow(suffix: u8) -> Option<&'static str> {
    Some(match suffix {
        0x0C => "pi2fw",
        0x0D => "pi2fd",
        0x1C => "pf2iw",
        0x1D => "pf2id",
        0x8A => "pfnacc",
        0x8E => "pfpnacc",
        0x90 => "pfcmpge",
        0x94 => "pfmin",
        0x96 => "pfrcp",
        0x97 => "pfrsqrt",
        0x9A => "pfsub",
        0x9E => "pfadd",
        0xA0 => "pfcmpgt",
        0xA4 => "pfmax",
        0xA6 => "pfrcpit1",
        0xA7 => "pfrsqit1",
        0xAA => "pfsubr",
        0xAE => "pfacc",
        0xB0 => "pfcmpeq",
        0xB4 => "pfmul",
        0xB6 => "pfrcpit2",
        0xB7 => "pmulhrw",
        0xBB => "pswapd",
        0xBF => "pavgusb",
        _ => return None,
    })
}
