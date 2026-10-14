//! Native x86 instruction decoding for 16- and 32-bit guest code.
//!
//! This crate exists so `gp64` does not link a third-party decoder. It knows
//! nothing about the emulator — it takes bytes and a bitness and reports what
//! the instruction is — so it sits below every consumer and depends on nothing.
//!
//! # What it answers today
//!
//! [`instruction_length`] returns the encoded length of the instruction at the
//! start of a byte slice, or says why it cannot. That is what the interpreter's
//! three cold fault checks need, and they are the reason this exists: deciding
//! whether an instruction crosses a missing page, runs past the CS limit, or
//! exceeds the architectural fifteen-byte window all require the LENGTH of an
//! arbitrary instruction *before* any of it is executed. The interpreter cannot
//! answer that itself — its decode is fused with execute, so it learns a length
//! by having already consumed the instruction.
//!
//! The three-way verdict matters as much as the number. A caller must be able to
//! tell "this is longer than the bytes you gave me" from "these bytes are not an
//! instruction at all", because the first is a #GP for crossing a boundary and
//! the second is a #UD. Collapsing them would mean the guest sees the wrong
//! fault.
//!
//! # Precision, and why it is safe to be imprecise in one direction
//!
//! Length decoding needs far less than full disassembly: which opcodes take a
//! ModRM byte, how a ModRM plus its optional SIB and displacement size under
//! each address size, and how wide each immediate is under each operand size.
//! It does not need to know what any instruction MEANS.
//!
//! # This measures; it does not validate
//!
//! The contract is deliberately narrower than a full decoder's, and the boundary
//! is worth stating precisely because it is what makes the crate small enough to
//! trust.
//!
//! Where an opcode is recognised, the length is exact. Where the opcode is
//! unknown, [`DecodeError::Invalid`] says so. But this decoder does NOT reject
//! encodings that are the right shape and the wrong semantics: a reserved
//! ModRM.reg sub-opcode (`0F 00 /6`), a memory-only form used with `mod=11`
//! (`BOUND r, r`), or an illegal `LOCK` prefix all have a well-defined LENGTH,
//! and this returns it. A full decoder rejects them; we are not one.
//!
//! That is the correct scope for every caller here, because all three of them
//! ask a question about BOUNDARIES — how many bytes must be fetched — and the
//! answer is the same whether or not the CPU will go on to raise #UD. The
//! instruction still has to be fetched before it can be rejected. Deciding
//! legality remains the interpreter's job, where it already lives.
//!
//! Being lenient in this direction is also the safe one. Reporting a length for
//! something the CPU will refuse costs nothing: the guest still gets its #UD
//! from the execute path. Reporting [`DecodeError::Invalid`] for something legal
//! would be a spurious #UD on working guest code.
//!
//! # How it is trusted
//!
//! `tests/` sweeps this against `iced-x86` exhaustively — every one-byte and
//! two-byte opcode against every ModRM byte, under both bitnesses and every
//! meaningful prefix combination — and compares both the length and the
//! error class. iced is a dev-dependency for that and must never become a
//! normal one, or the crate would defeat its own purpose.

#![forbid(unsafe_code)]

mod disasm;
mod operand;
mod sse;
mod table1;
mod table2;
mod table3;

pub use disasm::{disassemble, ControlFlow, Decoded};

/// The code segment's default operand and address size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bitness {
    Bits16,
    Bits32,
}

impl Bitness {
    /// Accepts the `cs_bits` values the interpreter carries.
    #[must_use]
    pub const fn from_cs_bits(cs_bits: u32) -> Self {
        if cs_bits == 32 {
            Self::Bits32
        } else {
            Self::Bits16
        }
    }
}

/// Why an instruction could not be measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The instruction is longer than the bytes supplied, but would fit inside
    /// the architectural window. The caller decides what that means: past a page
    /// boundary or past a segment limit are different faults.
    NoMoreBytes,
    /// The encoding structurally requires more than [`MAX_INSTRUCTION_LEN`]
    /// bytes, so no CPU will ever complete it.
    ///
    /// Answering this directly is why the interpreter no longer needs the
    /// prefix-compaction trick it used against iced. iced conflates "too long"
    /// with "invalid" in a single `InvalidInstruction`, so distinguishing them
    /// meant re-decoding a compacted copy to see which it had been. Here the
    /// length is computed structurally, so the question is simply answered.
    TooLong,
    /// The opcode is not one this decoder recognises.
    Invalid,
}

/// The architectural maximum instruction length.
pub const MAX_INSTRUCTION_LEN: usize = 15;

/// How wide an instruction's trailing immediate is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Imm {
    None,
    /// One byte, whether or not it is sign-extended — only the width matters.
    Ib,
    /// Two bytes regardless of operand size.
    Iw,
    /// Two or four bytes, following the operand size.
    Iz,
    /// `ENTER imm16, imm8`.
    IwIb,
    /// A far pointer: `ptr16:16` or `ptr16:32`, following the operand size.
    Ap,
    /// A direct memory offset, whose width follows the ADDRESS size, not the
    /// operand size. The classic trap in this table.
    Moffs,
}

/// The shape of an opcode: whether it takes a ModRM byte, and its immediate.
#[derive(Clone, Copy, Debug)]
struct Shape {
    modrm: bool,
    imm: Imm,
}

const fn s(modrm: bool, imm: Imm) -> Shape {
    Shape { modrm, imm }
}

/// Shape of a one-byte opcode.
///
/// Returns `None` for opcodes that are not instructions in 16/32-bit mode.
#[allow(clippy::match_same_arms)]
const fn one_byte_shape(op: u8) -> Option<Shape> {
    Some(match op {
        // ALU groups: 00-05, 08-0D, ... 38-3D follow a strict pattern of
        // /r, /r, /r, /r, ib(AL), iz(eAX) repeating every 8.
        0x00..=0x05
        | 0x08..=0x0D
        | 0x10..=0x15
        | 0x18..=0x1D
        | 0x20..=0x25
        | 0x28..=0x2D
        | 0x30..=0x35
        | 0x38..=0x3D => match op & 7 {
            0..=3 => s(true, Imm::None),
            4 => s(false, Imm::Ib),
            _ => s(false, Imm::Iz),
        },
        // PUSH/POP segment registers, and the 0x0F escape handled by the caller.
        0x06 | 0x07 | 0x0E | 0x16 | 0x17 | 0x1E | 0x1F => s(false, Imm::None),
        // DAA/DAS/AAA/AAS
        0x27 | 0x2F | 0x37 | 0x3F => s(false, Imm::None),
        // INC/DEC r32, PUSH/POP r32
        0x40..=0x5F => s(false, Imm::None),
        // PUSHA/POPA
        0x60 | 0x61 => s(false, Imm::None),
        // BOUND, ARPL.
        //
        // NOT EVEX. On a CPU with AVX-512, `62` with `mod=11` introduces an EVEX
        // prefix, and a general decoder reads it that way. No CPU this project
        // models has AVX at all — the ceiling is `--cpu pentium-4`, which is
        // SSE2 — so `62` is always BOUND here. The same applies to `C4`/`C5`
        // below, which are VEX on an AVX machine and always LES/LDS on ours.
        0x62 | 0x63 => s(true, Imm::None),
        // Prefixes 64-67 are consumed before this table is consulted.
        0x68 => s(false, Imm::Iz),
        0x69 => s(true, Imm::Iz),
        0x6A => s(false, Imm::Ib),
        0x6B => s(true, Imm::Ib),
        // INS/OUTS
        0x6C..=0x6F => s(false, Imm::None),
        // Jcc rel8
        0x70..=0x7F => s(false, Imm::Ib),
        // Immediate group 1
        0x80 => s(true, Imm::Ib),
        0x81 => s(true, Imm::Iz),
        0x82 => s(true, Imm::Ib), // undocumented alias of 0x80, accepted by silicon
        0x83 => s(true, Imm::Ib),
        // TEST/XCHG/MOV /r
        0x84..=0x8B => s(true, Imm::None),
        // MOV Sreg, LEA, MOV Sreg
        0x8C..=0x8E => s(true, Imm::None),
        // POP r/m
        0x8F => s(true, Imm::None),
        // NOP / XCHG eAX
        0x90..=0x97 => s(false, Imm::None),
        // CBW/CWD
        0x98 | 0x99 => s(false, Imm::None),
        // CALL far ptr
        0x9A => s(false, Imm::Ap),
        // WAIT, PUSHF, POPF, SAHF, LAHF
        0x9B..=0x9F => s(false, Imm::None),
        // MOV AL/eAX, moffs — ADDRESS-size-relative operand
        0xA0..=0xA3 => s(false, Imm::Moffs),
        // MOVS/CMPS
        0xA4..=0xA7 => s(false, Imm::None),
        // TEST AL/eAX, imm
        0xA8 => s(false, Imm::Ib),
        0xA9 => s(false, Imm::Iz),
        // STOS/LODS/SCAS
        0xAA..=0xAF => s(false, Imm::None),
        // MOV r8, imm8
        0xB0..=0xB7 => s(false, Imm::Ib),
        // MOV r32, imm32
        0xB8..=0xBF => s(false, Imm::Iz),
        // Shift group by imm8
        0xC0 | 0xC1 => s(true, Imm::Ib),
        // RET imm16 / RET
        0xC2 => s(false, Imm::Iw),
        0xC3 => s(false, Imm::None),
        // LES/LDS
        0xC4 | 0xC5 => s(true, Imm::None),
        // MOV r/m, imm
        0xC6 => s(true, Imm::Ib),
        0xC7 => s(true, Imm::Iz),
        // ENTER / LEAVE
        0xC8 => s(false, Imm::IwIb),
        0xC9 => s(false, Imm::None),
        // RETF imm16 / RETF
        0xCA => s(false, Imm::Iw),
        0xCB => s(false, Imm::None),
        // INT3 / INT imm8 / INTO / IRET
        0xCC => s(false, Imm::None),
        0xCD => s(false, Imm::Ib),
        0xCE | 0xCF => s(false, Imm::None),
        // Shift group by 1 / by CL
        0xD0..=0xD3 => s(true, Imm::None),
        // AAM/AAD take an immediate base byte; SALC and XLAT do not.
        0xD4 | 0xD5 => s(false, Imm::Ib),
        0xD6 | 0xD7 => s(false, Imm::None),
        // x87 escapes: always ModRM, never an immediate.
        0xD8..=0xDF => s(true, Imm::None),
        // LOOPNE/LOOPE/LOOP/JCXZ rel8
        0xE0..=0xE3 => s(false, Imm::Ib),
        // IN/OUT imm8
        0xE4..=0xE7 => s(false, Imm::Ib),
        // CALL/JMP rel, JMP far, JMP rel8
        0xE8 | 0xE9 => s(false, Imm::Iz),
        0xEA => s(false, Imm::Ap),
        0xEB => s(false, Imm::Ib),
        // IN/OUT DX
        0xEC..=0xEF => s(false, Imm::None),
        // Prefixes F0/F2/F3 are consumed earlier. F1 is ICEBP.
        0xF1 => s(false, Imm::None),
        // HLT/CMC
        0xF4 | 0xF5 => s(false, Imm::None),
        // F6/F7 are handled by the caller: the immediate depends on ModRM.reg.
        // CLC..STD
        0xF8..=0xFD => s(false, Imm::None),
        // INC/DEC group, and the FF group (CALL/JMP/PUSH)
        0xFE | 0xFF => s(true, Imm::None),
        _ => return None,
    })
}

/// Shape of a two-byte (`0F xx`) opcode.
#[allow(clippy::match_same_arms)]
const fn two_byte_shape(op: u8) -> Option<Shape> {
    Some(match op {
        // Group 6/7 (LLDT, LGDT, ...), LAR, LSL
        0x00..=0x03 => s(true, Imm::None),
        // 0x04 is undefined; SYSCALL/CLTS/SYSRET
        0x05..=0x07 => s(false, Imm::None),
        // INVD/WBINVD/UD2
        0x08..=0x0B => s(false, Imm::None),
        // 3DNow! prefetch group
        0x0D => s(true, Imm::None),
        0x0E => s(false, Imm::None),
        // 3DNow!: ModRM plus a trailing opcode byte, which sizes like an imm8.
        0x0F => s(true, Imm::Ib),
        // SSE moves and conversions — all ModRM, no immediate.
        0x10..=0x17 => s(true, Imm::None),
        // Hint-NOP group (includes the 0F 1F multi-byte NOP)
        0x18..=0x1F => s(true, Imm::None),
        // MOV to/from CR/DR. Handled by the caller: these ignore the ModRM mod
        // field entirely and are always register form, so they never carry a
        // displacement no matter what mod says.
        0x20..=0x23 => s(true, Imm::None),
        // 0x24-0x27 are undefined on the CPUs we model.
        0x28..=0x2F => s(true, Imm::None),
        // WRMSR/RDTSC/RDMSR/RDPMC/SYSENTER/SYSEXIT
        0x30..=0x37 => s(false, Imm::None),
        // CMOVcc
        0x40..=0x4F => s(true, Imm::None),
        // SSE arithmetic and logic
        0x50..=0x6F => s(true, Imm::None),
        // PSHUF group takes imm8
        0x70 => s(true, Imm::Ib),
        // MMX/SSE shift groups: ModRM.reg selects, immediate always present
        0x71..=0x73 => s(true, Imm::Ib),
        // PCMPEQB/W/D take a ModRM; EMMS (0F 77) does NOT — it is a standalone
        // opcode, and grouping it with its neighbours invents a byte.
        0x74..=0x76 => s(true, Imm::None),
        0x77 => s(false, Imm::None),
        0x78 | 0x79 => s(true, Imm::None),
        0x7C..=0x7F => s(true, Imm::None),
        // Jcc rel16/32
        0x80..=0x8F => s(false, Imm::Iz),
        // SETcc
        0x90..=0x9F => s(true, Imm::None),
        // PUSH/POP FS, CPUID
        0xA0..=0xA2 => s(false, Imm::None),
        // BT
        0xA3 => s(true, Imm::None),
        // SHLD imm8 / SHLD CL
        0xA4 => s(true, Imm::Ib),
        0xA5 => s(true, Imm::None),
        // XBTS/IBTS: 386-era encodings that later CPUs reused. Measured, since
        // they take a ModRM like their neighbours.
        0xA6 | 0xA7 => s(true, Imm::None),
        // PUSH/POP GS, RSM
        0xA8..=0xAA => s(false, Imm::None),
        // BTS
        0xAB => s(true, Imm::None),
        // SHRD imm8 / SHRD CL
        0xAC => s(true, Imm::Ib),
        0xAD => s(true, Imm::None),
        // FXSAVE group
        0xAE => s(true, Imm::None),
        // IMUL
        0xAF => s(true, Imm::None),
        // CMPXCHG, LSS/BTR/LFS/LGS, MOVZX
        0xB0..=0xB7 => s(true, Imm::None),
        // POPCNT-shaped slot, group 10, group 8 (BT/BTS/BTR/BTC imm8), BTC, BSF/BSR
        0xB8 | 0xB9 => s(true, Imm::None),
        0xBA => s(true, Imm::Ib),
        0xBB..=0xBF => s(true, Imm::None),
        // XADD
        0xC0 | 0xC1 => s(true, Imm::None),
        // CMPPS/CMPSS take a predicate imm8
        0xC2 => s(true, Imm::Ib),
        0xC3 => s(true, Imm::None),
        // PINSRW/PEXTRW/SHUFPS take imm8
        0xC4..=0xC6 => s(true, Imm::Ib),
        // CMPXCHG8B group
        0xC7 => s(true, Imm::None),
        // BSWAP
        0xC8..=0xCF => s(false, Imm::None),
        // The rest of the SSE/MMX arithmetic space, plus `0F FF` (UD0), which
        // takes a ModRM despite being undefined.
        0xD0..=0xFF => s(true, Imm::None),
        _ => return None,
    })
}

/// Reads a ModRM byte plus its SIB and displacement, returning how many bytes
/// they occupy in total (including the ModRM byte itself).
///
/// `addr32` selects the addressing form, which is the operand-size-independent
/// half of the encoding and the one most often confused with it.
pub(crate) fn modrm_span(bytes: &[u8], addr32: bool) -> Result<usize, DecodeError> {
    modrm_len(bytes, addr32)
}

fn modrm_len(bytes: &[u8], addr32: bool) -> Result<usize, DecodeError> {
    let modrm = *bytes.first().ok_or(DecodeError::NoMoreBytes)?;
    let md = modrm >> 6;
    let rm = modrm & 7;

    if md == 3 {
        return Ok(1); // register form: no SIB, no displacement
    }

    if addr32 {
        let mut len = 1;
        // rm == 4 selects a SIB byte, which can itself introduce a disp32 when
        // base == 5 and mod == 0.
        let mut disp = match md {
            0 => 0,
            1 => 1,
            _ => 4,
        };
        if rm == 4 {
            let sib = *bytes.get(1).ok_or(DecodeError::NoMoreBytes)?;
            len += 1;
            if md == 0 && (sib & 7) == 5 {
                disp = 4;
            }
        } else if md == 0 && rm == 5 {
            disp = 4; // disp32 with no base
        }
        Ok(len + disp)
    } else {
        let disp = match md {
            0 => {
                if rm == 6 {
                    2 // disp16 with no base
                } else {
                    0
                }
            }
            1 => 1,
            _ => 2,
        };
        Ok(1 + disp)
    }
}

/// Whether a byte is a legacy prefix, and which group it belongs to.
///
/// Grouping matters to callers that need to compact repeated prefixes; the
/// length decode itself only needs to know that a byte IS one.
#[must_use]
pub const fn legacy_prefix_group(byte: u8) -> Option<usize> {
    match byte {
        0xF0 | 0xF2 | 0xF3 => Some(0),
        0x2E | 0x36 | 0x3E | 0x26 | 0x64 | 0x65 => Some(1),
        0x66 => Some(2),
        0x67 => Some(3),
        _ => None,
    }
}

/// The encoded length of the instruction at the start of `bytes`.
///
/// `bytes` should hold as much as the caller can supply, up to
/// [`MAX_INSTRUCTION_LEN`]. A short slice yields [`DecodeError::NoMoreBytes`]
/// when the instruction needs more, which is exactly the signal a boundary
/// check wants.
///
/// # Errors
///
/// [`DecodeError::NoMoreBytes`] when the instruction extends past the slice, and
/// [`DecodeError::Invalid`] when the bytes are not an instruction at all.
pub fn instruction_length(bytes: &[u8], bitness: Bitness) -> Result<usize, DecodeError> {
    match decode_length(bytes, bitness) {
        // Running out of bytes when the caller already supplied a full
        // architectural window is not "give me more" — there is no more to give.
        // The encoding needs a sixteenth byte, which no CPU will fetch, so this
        // is structurally TooLong. Resolving it here keeps every caller from
        // having to know the rule.
        Err(DecodeError::NoMoreBytes) if bytes.len() >= MAX_INSTRUCTION_LEN => {
            Err(DecodeError::TooLong)
        }
        other => other,
    }
}

fn decode_length(bytes: &[u8], bitness: Bitness) -> Result<usize, DecodeError> {
    let mut pos = 0usize;
    let mut op_size32 = bitness == Bitness::Bits32;
    let mut addr_size32 = bitness == Bitness::Bits32;

    // Legacy prefixes. The architectural window bounds how many can be consumed:
    // past fifteen bytes there is no instruction regardless of what follows.
    while let Some(&byte) = bytes.get(pos) {
        match byte {
            0x66 => op_size32 = bitness == Bitness::Bits16,
            0x67 => addr_size32 = bitness == Bitness::Bits16,
            _ if legacy_prefix_group(byte).is_some() => {}
            _ => break,
        }
        pos += 1;
        if pos >= MAX_INSTRUCTION_LEN {
            // Fifteen prefix bytes with no opcode: the window is exhausted
            // before an opcode was reached, so no completion exists.
            return Err(DecodeError::TooLong);
        }
    }

    let op = *bytes.get(pos).ok_or(DecodeError::NoMoreBytes)?;
    pos += 1;

    let mut modrm_is_always_register = false;
    let (shape, modrm_reg_immediate) = if op == 0x0F {
        let op2 = *bytes.get(pos).ok_or(DecodeError::NoMoreBytes)?;
        pos += 1;
        // The three-byte maps. Their opcodes belong to SSSE3 and later, which no
        // CPU this project currently models — a guest reaching one gets #UD from
        // the execute path. They are measured anyway rather than rejected,
        // because measuring is this crate's job and the shapes are uniform: the
        // 0F 38 map is ModRM-only, the 0F 3A map is ModRM plus imm8. That keeps
        // the boundary checks correct for such an instruction (the CPU must
        // still fetch it before refusing) and means the table does not need
        // revisiting if a later CPU model admits them.
        if op2 == 0x38 || op2 == 0x3A {
            let op3 = *bytes.get(pos).ok_or(DecodeError::NoMoreBytes)?;
            let _ = op3;
            pos += 1;
            let imm = if op2 == 0x3A { Imm::Ib } else { Imm::None };
            return finish(
                bytes,
                pos,
                s(true, imm),
                false,
                false,
                op,
                op_size32,
                addr_size32,
            );
        }
        modrm_is_always_register = matches!(op2, 0x20..=0x23);
        (two_byte_shape(op2).ok_or(DecodeError::Invalid)?, false)
    } else if op == 0xF6 || op == 0xF7 {
        // Group 3: TEST (/0 and /1) carries an immediate, NOT/NEG/MUL/IMUL/
        // DIV/IDIV do not. The immediate therefore depends on the ModRM byte,
        // which is why this cannot live in the table.
        (s(true, Imm::None), true)
    } else {
        (one_byte_shape(op).ok_or(DecodeError::Invalid)?, false)
    };

    finish(
        bytes,
        pos,
        shape,
        modrm_reg_immediate,
        modrm_is_always_register,
        op,
        op_size32,
        addr_size32,
    )
}

/// Size the ModRM (if any) and the trailing immediate, and bound the result.
///
/// Shared by the one-, two- and three-byte paths so the sizing rules — which are
/// where the subtle mistakes live — exist once.
#[allow(clippy::too_many_arguments)]
fn finish(
    bytes: &[u8],
    mut pos: usize,
    shape: Shape,
    modrm_reg_immediate: bool,
    modrm_is_always_register: bool,
    op: u8,
    op_size32: bool,
    addr_size32: bool,
) -> Result<usize, DecodeError> {
    let mut imm = shape.imm;

    if shape.modrm {
        let rest = bytes.get(pos..).ok_or(DecodeError::NoMoreBytes)?;
        if modrm_reg_immediate {
            let modrm = *rest.first().ok_or(DecodeError::NoMoreBytes)?;
            if (modrm >> 3) & 7 <= 1 {
                imm = if op == 0xF6 { Imm::Ib } else { Imm::Iz };
            }
        }
        if modrm_is_always_register {
            // The ModRM byte is present but its mod field is ignored: `MOV r32,
            // CR0` is encoded with mod bits that do not mean memory. Treating
            // them as an addressing mode would invent a displacement.
            let _ = *rest.first().ok_or(DecodeError::NoMoreBytes)?;
            pos += 1;
        } else {
            pos += modrm_len(rest, addr_size32)?;
        }
    }

    pos += match imm {
        Imm::None => 0,
        Imm::Ib => 1,
        Imm::Iw => 2,
        Imm::Iz => {
            if op_size32 {
                4
            } else {
                2
            }
        }
        Imm::IwIb => 3,
        Imm::Ap => {
            if op_size32 {
                6
            } else {
                4
            }
        }
        // The one immediate whose width follows the ADDRESS size.
        Imm::Moffs => {
            if addr_size32 {
                4
            } else {
                2
            }
        }
    };

    // Order matters: an encoding past the window is TooLong regardless of how
    // many bytes the caller happened to supply, and that is the stronger fact.
    if pos > MAX_INSTRUCTION_LEN {
        return Err(DecodeError::TooLong);
    }
    if pos > bytes.len() {
        return Err(DecodeError::NoMoreBytes);
    }
    Ok(pos)
}
