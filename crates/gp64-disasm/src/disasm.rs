//! Decoding to text: Intel syntax, one instruction at a time.
//!
//! Layered on the same prefix and ModRM machinery as [`crate::instruction_length`],
//! so there is one set of sizing rules rather than two that can drift.
//!
//! # Why the output matches iced's exactly
//!
//! The formatter is written to reproduce `iced_x86::IntelFormatter` character
//! for character. That is not deference — it makes the ORACLE a plain string
//! comparison over the entire opcode space, which is the strongest verification
//! available for a component like this, and it means a mismatch is a precise bug
//! report rather than a judgement call about style. The conventions it commits
//! to are ordinary Intel syntax anyway: lower-case mnemonics and registers,
//! `1234h` for hex with a leading zero where a literal would otherwise start
//! with a letter, no space after the operand comma, and an explicit `dword ptr`
//! only where nothing else fixes the memory operand's width.

use crate::operand::{Op, REG16, REG32, REG8, RM16, SEG};
use crate::table1;
use crate::table2::{self, Mandatory};
use crate::table3;
use crate::{Bitness, DecodeError, MAX_INSTRUCTION_LEN};
use core::fmt::Write as _;

/// A decoded instruction, ready to print.
#[derive(Clone, Debug)]
pub struct Decoded {
    /// Encoded length in bytes.
    pub len: usize,
    /// Intel-syntax text, e.g. `mov dword ptr [esi+10h],eax`.
    pub text: String,
    /// What the instruction does to the flow of control.
    pub flow: ControlFlow,
    /// The mnemonic alone, as printed — no prefixes, no operands.
    ///
    /// This is the spelling that reached the text, so it reflects the operand
    /// size: `iret` in 16-bit code and `iretd` in 32-bit, not the table's name
    /// for the entry.
    pub mnemonic: &'static str,
}

/// What an instruction does to the flow of control.
///
/// Deliberately coarse: this exists so a caller tracking call depth or
/// stepping over a call can ask a question the decoder can answer, instead of
/// matching on mnemonic strings — which is the shape that quietly breaks when a
/// mnemonic is spelled slightly differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlFlow {
    /// `CALL`, near or far. Pushes a return address.
    Call,
    /// `RET` or `RETF`. Pops one.
    Return,
    /// `INT`, `INT1`, `INT3` or `INTO`. Enters a handler.
    Interrupt,
    /// `IRET` or `IRETD`. Leaves one.
    InterruptReturn,
    /// Everything else, including the jumps — a jump does not nest, so nothing
    /// that tracks depth cares about it.
    Other,
}

impl ControlFlow {
    fn of(mnemonic: &str) -> Self {
        match mnemonic {
            "call" => Self::Call,
            // The far forms are spelled `ret far` in Intel syntax, not `retf`.
            "ret" | "ret far" => Self::Return,
            "int" | "int1" | "int3" | "into" => Self::Interrupt,
            "iret" | "iretd" => Self::InterruptReturn,
            _ => Self::Other,
        }
    }
}

/// Everything the operand formatter needs about the instruction's context.
struct Ctx<'a> {
    bytes: &'a [u8],
    /// Guest address of byte 0, for computing branch targets.
    rip: u64,
    op_size32: bool,
    addr_size32: bool,
    seg_override: Option<&'static str>,
    rep: Option<&'static str>,
    lock: bool,
    saw_66: bool,
    saw_67: bool,
    /// Offset of the ModRM byte, once one has been read.
    modrm_at: Option<usize>,
    /// Offset just past the ModRM byte, SIB and displacement.
    after_modrm: usize,
    /// Offset of the first immediate byte.
    imm_at: usize,
    /// The opcode byte, for the opcode-embedded register forms.
    opcode: u8,
}

impl Ctx<'_> {
    fn modrm(&self) -> u8 {
        self.modrm_at.map_or(0, |i| self.bytes[i])
    }

    fn reg_field(&self) -> u8 {
        (self.modrm() >> 3) & 7
    }

    fn rm_field(&self) -> u8 {
        self.modrm() & 7
    }

    fn mod_field(&self) -> u8 {
        self.modrm() >> 6
    }

    fn read_u8(&self, at: usize) -> u8 {
        self.bytes.get(at).copied().unwrap_or(0)
    }

    fn read_u16(&self, at: usize) -> u16 {
        u16::from(self.read_u8(at)) | (u16::from(self.read_u8(at + 1)) << 8)
    }

    fn read_u32(&self, at: usize) -> u32 {
        u32::from(self.read_u16(at)) | (u32::from(self.read_u16(at + 2)) << 16)
    }
}

/// Render an unsigned value the way Intel syntax does.
///
/// Hex with a trailing `h`, and a leading `0` when the first digit would be a
/// letter — otherwise `ABCDh` would read as an identifier. Values below ten are
/// printed bare, since `9h` is noise.
fn hex(value: u64) -> String {
    if value < 10 {
        return value.to_string();
    }
    let digits = format!("{value:X}");
    if digits.starts_with(|c: char| c.is_ascii_alphabetic()) {
        format!("0{digits}h")
    } else {
        format!("{digits}h")
    }
}

/// Render a signed displacement inside a memory expression: `+8`, `-4`.
fn signed_disp(value: i64) -> String {
    if value < 0 {
        format!("-{}", hex(value.unsigned_abs()))
    } else {
        format!("+{}", hex(value as u64))
    }
}

fn reg_name(index: u8, size32: bool, byte: bool) -> &'static str {
    let i = (index & 7) as usize;
    if byte {
        REG8[i]
    } else if size32 {
        REG32[i]
    } else {
        REG16[i]
    }
}

/// The explicit size marker Intel syntax puts before a memory operand.
const fn size_marker(byte: bool, size32: bool, word: bool) -> &'static str {
    if byte {
        "byte ptr "
    } else if word || !size32 {
        "word ptr "
    } else {
        "dword ptr "
    }
}

/// Format a ModRM memory reference, without the size marker.
fn memory(ctx: &Ctx) -> String {
    let mut out = String::with_capacity(24);
    if let Some(seg) = ctx.seg_override {
        let _ = write!(out, "{seg}:");
    }
    out.push('[');

    let md = ctx.mod_field();
    let rm = ctx.rm_field();
    let mut base = ctx.modrm_at.map_or(0, |i| i + 1);

    if ctx.addr_size32 {
        let mut parts = String::new();
        let mut disp: i64 = 0;
        let mut have_base = true;

        if rm == 4 {
            let sib = ctx.read_u8(base);
            base += 1;
            let scale = 1u8 << (sib >> 6);
            let index = (sib >> 3) & 7;
            let sib_base = sib & 7;

            if sib_base == 5 && md == 0 {
                have_base = false;
                disp = i64::from(ctx.read_u32(base) as i32);
                base += 4;
            } else {
                parts.push_str(REG32[sib_base as usize]);
            }
            // Index 4 encodes "no index"; ESP cannot be an index register.
            if index != 4 {
                if !parts.is_empty() {
                    parts.push('+');
                }
                let _ = write!(parts, "{}", REG32[index as usize]);
                // With no base register the scale is always shown, including
                // `*1`: `[eax*1+41414141h]`. With a base, a scale of one is
                // implicit and omitted.
                if scale != 1 || !have_base {
                    let _ = write!(parts, "*{scale}");
                }
            }
        } else if md == 0 && rm == 5 {
            have_base = false;
            disp = i64::from(ctx.read_u32(base));
            base += 4;
        } else {
            parts.push_str(REG32[rm as usize]);
        }

        match md {
            1 => disp = i64::from(ctx.read_u8(base) as i8),
            2 => disp = i64::from(ctx.read_u32(base) as i32),
            _ => {}
        }

        if have_base {
            out.push_str(&parts);
            if disp != 0 {
                out.push_str(&signed_disp(disp));
            }
        } else if parts.is_empty() {
            // A bare absolute address prints unsigned, not as a displacement.
            out.push_str(&hex(disp as u64 & 0xFFFF_FFFF));
        } else {
            out.push_str(&parts);
            out.push_str(&signed_disp(disp));
        }
    } else if md == 0 && rm == 6 {
        // The one 16-bit form with no base register: a bare disp16.
        out.push_str(&hex(u64::from(ctx.read_u16(base))));
    } else {
        out.push_str(RM16[rm as usize]);
        let disp: i64 = match md {
            1 => i64::from(ctx.read_u8(base) as i8),
            2 => i64::from(ctx.read_u16(base) as i16),
            _ => 0,
        };
        if disp != 0 {
            out.push_str(&signed_disp(disp));
        }
    }

    out.push(']');
    out
}

/// Format one operand.
///
/// `needs_marker` says whether a memory operand must carry its explicit size,
/// which depends on the OTHER operands and so is decided by the caller.
fn operand(ctx: &Ctx, op: Op, needs_marker: bool, imm_cursor: &mut usize) -> String {
    let byte = matches!(
        op,
        Op::Eb | Op::Gb | Op::FixedB(_) | Op::OpcodeB | Op::Ob | Op::Xb | Op::Yb
    );
    let word = matches!(op, Op::Ew | Op::EwOrV | Op::Sw);

    match op {
        Op::None => String::new(),

        Op::Eb | Op::Ev | Op::Ew | Op::EwOrV | Op::M | Op::Mp | Op::Ma => {
            if ctx.mod_field() == 3 && !matches!(op, Op::M | Op::Mp | Op::Ma) {
                // `Ew` is word-sized whatever the operand size, so `mov es,ax`
                // stays `ax` under a 66 prefix.
                let size32 = ctx.op_size32 && !matches!(op, Op::Ew);
                reg_name(ctx.rm_field(), size32, byte).to_string()
            } else {
                let mem = memory(ctx);
                // `Ma` always marks, regardless of the other operands.
                if needs_marker || matches!(op, Op::Ma) {
                    let marker = if matches!(op, Op::Mp) {
                        if ctx.op_size32 {
                            "fword ptr "
                        } else {
                            "dword ptr "
                        }
                    } else if matches!(op, Op::Ma) {
                        if ctx.op_size32 {
                            "qword ptr "
                        } else {
                            "dword ptr "
                        }
                    } else {
                        size_marker(byte, ctx.op_size32, word)
                    };
                    format!("{marker}{mem}")
                } else {
                    mem
                }
            }
        }

        Op::Gb => reg_name(ctx.reg_field(), false, true).to_string(),
        Op::Gv => reg_name(ctx.reg_field(), ctx.op_size32, false).to_string(),
        Op::Gw => REG16[ctx.reg_field() as usize].to_string(),
        Op::Rd => REG32[ctx.rm_field() as usize].to_string(),
        Op::Cd => format!("cr{}", ctx.reg_field()),
        Op::Dd => format!("dr{}", ctx.reg_field()),
        Op::Sw => SEG[ctx.reg_field() as usize].to_string(),

        Op::Ib => {
            let v = ctx.read_u8(*imm_cursor);
            *imm_cursor += 1;
            hex(u64::from(v))
        }
        Op::Ibs => {
            // Sign-extended to the operand size, which is what makes
            // `add eax,0FFFFFFFFh` print as it does rather than as `0FFh`.
            let v = ctx.read_u8(*imm_cursor) as i8;
            *imm_cursor += 1;
            let widened = if ctx.op_size32 {
                u64::from(v as i32 as u32)
            } else {
                u64::from(v as i16 as u16)
            };
            hex(widened)
        }
        Op::Iw => {
            let v = ctx.read_u16(*imm_cursor);
            *imm_cursor += 2;
            hex(u64::from(v))
        }
        Op::Iz => {
            if ctx.op_size32 {
                let v = ctx.read_u32(*imm_cursor);
                *imm_cursor += 4;
                hex(u64::from(v))
            } else {
                let v = ctx.read_u16(*imm_cursor);
                *imm_cursor += 2;
                hex(u64::from(v))
            }
        }

        Op::Jb => {
            // The rel8 form is spelled `short` in Intel syntax, distinguishing it
            // from the near form that reaches the same address.
            let rel = i64::from(ctx.read_u8(*imm_cursor) as i8);
            *imm_cursor += 1;
            let target = branch_target(ctx, rel, *imm_cursor);
            if matches!(ctx.opcode, 0x70..=0x7F | 0xEB) {
                format!("short {target}")
            } else {
                target
            }
        }
        Op::Jz => {
            let rel = if ctx.op_size32 {
                let v = i64::from(ctx.read_u32(*imm_cursor) as i32);
                *imm_cursor += 4;
                v
            } else {
                let v = i64::from(ctx.read_u16(*imm_cursor) as i16);
                *imm_cursor += 2;
                v
            };
            branch_target(ctx, rel, *imm_cursor)
        }

        Op::Ob | Op::Ov => {
            let addr = if ctx.addr_size32 {
                let v = u64::from(ctx.read_u32(*imm_cursor));
                *imm_cursor += 4;
                v
            } else {
                let v = u64::from(ctx.read_u16(*imm_cursor));
                *imm_cursor += 2;
                v
            };
            // The accumulator on the other side fixes the width, and the
            // default segment is implied, so only an override is printed.
            match ctx.seg_override {
                Some(seg) => format!("{seg}:[{}]", hex(addr)),
                None => format!("[{}]", hex(addr)),
            }
        }

        Op::Ap => {
            let off = if ctx.op_size32 {
                let v = u64::from(ctx.read_u32(*imm_cursor));
                *imm_cursor += 4;
                v
            } else {
                let v = u64::from(ctx.read_u16(*imm_cursor));
                *imm_cursor += 2;
                v
            };
            let seg = ctx.read_u16(*imm_cursor);
            *imm_cursor += 2;
            // Printed as two operands, offset first, with `far` on the mnemonic.
            format!("{},{}", hex(off), hex(u64::from(seg)))
        }

        Op::FixedB(i) => REG8[(i & 7) as usize].to_string(),
        Op::FixedV(i) => reg_name(i, ctx.op_size32, false).to_string(),
        Op::OpcodeB => REG8[(ctx.opcode & 7) as usize].to_string(),
        Op::OpcodeV => reg_name(ctx.opcode & 7, ctx.op_size32, false).to_string(),
        Op::FixedSeg(i) => SEG[(i & 7) as usize].to_string(),
        Op::One => "1".to_string(),
        Op::Cl => "cl".to_string(),
        Op::Dx => "dx".to_string(),
        Op::St => format!("st({})", ctx.rm_field()),
        Op::St0 => "st".to_string(),
        Op::FixedAx => "ax".to_string(),
        Op::Pq => format!("mm{}", ctx.reg_field()),
        Op::Gd => REG32[ctx.reg_field() as usize].to_string(),
        Op::Nq => format!("mm{}", ctx.rm_field()),
        Op::Vx => format!("xmm{}", ctx.reg_field()),
        Op::Ux => format!("xmm{}", ctx.rm_field()),
        Op::Wx => {
            if ctx.mod_field() == 3 {
                format!("xmm{}", ctx.rm_field())
            } else {
                memory(ctx)
            }
        }
        Op::Qq => {
            if ctx.mod_field() == 3 {
                format!("mm{}", ctx.rm_field())
            } else {
                memory(ctx)
            }
        }
        Op::Ed => {
            if ctx.mod_field() == 3 {
                REG32[ctx.rm_field() as usize].to_string()
            } else if needs_marker {
                format!("dword ptr {}", memory(ctx))
            } else {
                memory(ctx)
            }
        }
        Op::Mx(size) => format!(
            "{}{}",
            size.mode_marker(ctx.saw_66, ctx.op_size32),
            memory(ctx)
        ),
        Op::XlatTable => {
            // XLAT reads DS:[BX+AL] by default, and honours a segment override.
            let reg = if ctx.addr_size32 { "ebx" } else { "bx" };
            match ctx.seg_override {
                Some(seg) => format!("{seg}:[{reg}]"),
                None => format!("[{reg}]"),
            }
        }

        Op::Xb | Op::Xv | Op::Yb | Op::Yv => {
            // A string operand's width and segment are both implied by the
            // mnemonic (`movsb` is a byte through DS:SI to ES:DI), so Intel
            // syntax prints the pointer register alone. Only an explicit
            // override is worth showing, and never on the ES-fixed side.
            let si = matches!(op, Op::Xb | Op::Xv);
            let reg = match (si, ctx.addr_size32) {
                (true, true) => "esi",
                (true, false) => "si",
                (false, true) => "edi",
                (false, false) => "di",
            };
            match (si, ctx.seg_override) {
                (true, Some(seg)) => format!("{seg}:[{reg}]"),
                // The destination side is fixed to ES and ignores any override.
                // When one is present the segment is spelled out anyway, so the
                // reader can see that it did not apply.
                (false, Some(_)) => format!("es:[{reg}]"),
                _ => format!("[{reg}]"),
            }
        }
    }
}

/// Mnemonics that change with the operand size.
///
/// The tables carry the 32-bit spelling; this maps to the 16-bit one. These are
/// the same instruction — `pusha` and `pushad` are one opcode — but Intel syntax
/// names them apart because the width is not otherwise visible in the text.
fn size_variant(mnemonic: &'static str, op_size32: bool, opcode: u8) -> &'static str {
    if op_size32 {
        return mnemonic;
    }
    match mnemonic {
        "pushad" => "pusha",
        "popad" => "popa",
        "pushfd" => "pushf",
        "popfd" => "popf",
        "cwde" => "cbw",
        "cdq" => "cwd",
        "iretd" => "iret",
        // Some names are also used by scalar SSE2 operations. The width
        // variants belong only to their one-byte string opcode families.
        "movsd" if opcode == 0xA5 => "movsw",
        "cmpsd" if opcode == 0xA7 => "cmpsw",
        "stosd" if opcode == 0xAB => "stosw",
        "lodsd" if opcode == 0xAD => "lodsw",
        "scasd" if opcode == 0xAF => "scasw",
        "insd" if opcode == 0x6D => "insw",
        "outsd" if opcode == 0x6F => "outsw",
        other => other,
    }
}

fn branch_target(ctx: &Ctx, rel: i64, next: usize) -> String {
    let target = ctx.rip.wrapping_add(next as u64).wrapping_add(rel as u64);
    // Branch targets print at the code segment's width, zero-padded.
    let digits = if ctx.op_size32 {
        format!("{:08X}", target as u32)
    } else {
        format!("{:04X}", target as u16)
    };
    if digits.starts_with(|c: char| c.is_ascii_alphabetic()) {
        format!("0{digits}h")
    } else {
        format!("{digits}h")
    }
}

/// Decode and format the instruction at the start of `bytes`.
///
/// # Errors
///
/// The same three verdicts [`crate::instruction_length`] gives.
pub fn disassemble(bytes: &[u8], bitness: Bitness, rip: u64) -> Result<Decoded, DecodeError> {
    let len = crate::instruction_length(bytes, bitness)?;

    let mut pos = 0usize;
    let mut ctx = Ctx {
        bytes,
        rip,
        op_size32: bitness == Bitness::Bits32,
        addr_size32: bitness == Bitness::Bits32,
        seg_override: None,
        rep: None,
        lock: false,
        saw_66: false,
        saw_67: false,
        modrm_at: None,
        after_modrm: 0,
        imm_at: 0,
        opcode: 0,
    };

    while let Some(&b) = bytes.get(pos) {
        match b {
            0x66 => {
                ctx.op_size32 = bitness == Bitness::Bits16;
                ctx.saw_66 = true;
            }
            0x67 => {
                ctx.addr_size32 = bitness == Bitness::Bits16;
                ctx.saw_67 = true;
            }
            0xF0 => ctx.lock = true,
            0xF2 => ctx.rep = Some("repne"),
            0xF3 => ctx.rep = Some("rep"),
            0x2E => ctx.seg_override = Some("cs"),
            0x36 => ctx.seg_override = Some("ss"),
            0x3E => ctx.seg_override = Some("ds"),
            0x26 => ctx.seg_override = Some("es"),
            0x64 => ctx.seg_override = Some("fs"),
            0x65 => ctx.seg_override = Some("gs"),
            _ => break,
        }
        pos += 1;
    }

    let op = *bytes.get(pos).ok_or(DecodeError::NoMoreBytes)?;
    ctx.opcode = op;
    pos += 1;

    // `F2`/`F3` outrank `66` when an opcode is selected by its prefix.
    let mandatory = match (ctx.rep, ctx.saw_66) {
        (Some("repne"), _) => Mandatory::PF2,
        (Some("rep"), _) => Mandatory::PF3,
        (_, true) => Mandatory::P66,
        _ => Mandatory::None,
    };

    let two_byte = op == 0x0F;
    let mut entry = if two_byte {
        let op2 = *bytes.get(pos).ok_or(DecodeError::NoMoreBytes)?;
        pos += 1;
        ctx.opcode = op2;
        // The SSE half picks its instruction from the ModRM byte as well as
        // the opcode, so it has to be read before an entry exists. Opcodes
        // without a ModRM never look at it.
        let modrm_peek = bytes.get(pos).copied().unwrap_or(0);
        table2::lookup(op2, mandatory, modrm_peek).ok_or(DecodeError::Invalid)?
    } else if matches!(op, 0xD8..=0xDF) {
        // The x87 escapes are selected by the whole ModRM byte, so it has to be
        // read before there is an entry at all.
        let modrm = *bytes.get(pos).ok_or(DecodeError::NoMoreBytes)?;
        table3::lookup(op, modrm).ok_or(DecodeError::Invalid)?
    } else {
        table1::lookup(op).ok_or(DecodeError::Invalid)?
    };

    // `90` is `xchg eAX,eAX`, which is spelled NOP — but only in its bare form.
    // With an operand-size prefix the encoding is no longer the canonical NOP
    // and is written out.
    // `F3 90` is PAUSE, a distinct instruction rather than a prefixed NOP. Real
    // spin loops emit it, so a fault dump inside one should say so.
    if op == 0x90 && ctx.rep == Some("rep") {
        entry = table1::e("pause", Op::None, Op::None, Op::None);
    } else if op == 0x90 && ctx.saw_66 {
        entry = table1::e("xchg", Op::FixedV(0), Op::FixedV(0), Op::None);
    }

    // Groups take their mnemonic, and sometimes their operand count, from
    // ModRM.reg — so the byte has to be read before the entry is complete.
    let group_modrm = if entry.ops[0].needs_modrm()
        || matches!(op, 0xF6 | 0xF7 | 0xFE | 0xFF)
        || (two_byte && table2::is_group(ctx.opcode))
    {
        Some(*bytes.get(pos).ok_or(DecodeError::NoMoreBytes)?)
    } else {
        None
    };

    if two_byte {
        if let Some(modrm) = group_modrm {
            if table2::is_group(ctx.opcode) {
                entry = table2::group_entry(ctx.opcode, modrm, mandatory)
                    .ok_or(DecodeError::Invalid)?;
            }
        }
    } else if let Some(modrm) = group_modrm {
        let r = ((modrm >> 3) & 7) as usize;
        match op {
            0x80..=0x83 => entry.mnemonic = table1::GROUP1[r],
            0xC0 | 0xC1 | 0xD0..=0xD3 => entry.mnemonic = table1::GROUP2[r],
            0xF6 => {
                entry = table1::e(
                    table1::GROUP3[r],
                    Op::Eb,
                    if r <= 1 { Op::Ib } else { Op::None },
                    Op::None,
                );
            }
            0xF7 => {
                entry = table1::e(
                    table1::GROUP3[r],
                    Op::Ev,
                    if r <= 1 { Op::Iz } else { Op::None },
                    Op::None,
                );
            }
            0xFE => {
                if r > 1 {
                    return Err(DecodeError::Invalid);
                }
                entry = table1::e(
                    if r == 0 { "inc" } else { "dec" },
                    Op::Eb,
                    Op::None,
                    Op::None,
                );
            }
            0xFF => {
                let m = table1::GROUP5[r];
                if m.is_empty() {
                    return Err(DecodeError::Invalid);
                }
                // /3 and /5 are the far forms and take an m16:16/m16:32.
                let operand = if r == 3 || r == 5 { Op::Mp } else { Op::Ev };
                entry = table1::e(m, operand, Op::None, Op::None);
            }
            _ => {}
        }
    }

    // A 3DNow! entry is still nameless at this point by design — its suffix
    // sits past the displacement, which has not been walked yet.
    let awaiting_suffix = two_byte && (table2::defers_to_suffix(ctx.opcode) || ctx.opcode == 0xC2);
    if entry.mnemonic.is_empty() && !awaiting_suffix {
        return Err(DecodeError::Invalid);
    }

    // Position the ModRM and immediate cursors.
    if entry.ops.iter().any(|o| o.needs_modrm()) {
        ctx.modrm_at = Some(pos);
        // `0F 20`-`0F 23` are the exception to the whole ModRM machinery: the
        // mod field is ignored, the form is always register-to-register, and
        // there is never a displacement or SIB however the byte reads.
        let consumed = if two_byte && matches!(ctx.opcode, 0x20..=0x23) {
            1
        } else {
            crate::modrm_span(&bytes[pos..], ctx.addr_size32)?
        };
        ctx.after_modrm = pos + consumed;
    } else {
        ctx.after_modrm = pos;
    }
    ctx.imm_at = ctx.after_modrm;

    // 3DNow! names its operation in a byte AFTER the ModRM and any
    // displacement, so this is the first point at which the instruction is
    // known at all. The suffix is not an operand and is never printed; it is
    // part of the opcode that happens to be encoded last.
    if two_byte && ctx.opcode == 0x0F {
        let suffix = *bytes.get(ctx.imm_at).ok_or(DecodeError::NoMoreBytes)?;
        entry.mnemonic = table2::threednow(suffix).ok_or(DecodeError::Invalid)?;
    }

    // CMPPS and friends spell their predicate into the mnemonic, so the
    // immediate names the instruction for the first eight values and stops
    // being an operand. Above that it has no spelling and stays one.
    if two_byte && ctx.opcode == 0xC2 {
        let predicate = *bytes.get(ctx.imm_at).ok_or(DecodeError::NoMoreBytes)?;
        let (mnemonic, keeps_immediate) = crate::sse::cmp_mnemonic(predicate, mandatory);
        entry.mnemonic = mnemonic;
        if !keeps_immediate {
            entry.ops[2] = Op::None;
        }
    }

    // An explicit size marker is needed only when no operand implies the width.
    // The size marker describes the MEMORY operand, so a register operand only
    // makes it redundant when it is the same width. `bt [bx],ax` needs no marker
    // because `ax` is exactly what is read; `movzx ax,byte ptr [bx]` needs one
    // precisely because `ax` is NOT — the whole point of the instruction is that
    // its two operands differ in size.
    let memory = entry
        .ops
        .iter()
        .find(|o| {
            matches!(
                o,
                Op::Eb | Op::Ev | Op::Ew | Op::EwOrV | Op::M | Op::Mp | Op::Ma | Op::Ed
            )
        })
        .filter(|_| ctx.mod_field() != 3);
    let any = |f: fn(&Op) -> bool| entry.ops.iter().any(f);
    let size_is_evident = match memory {
        None => true,
        Some(Op::Eb) => any(|o| matches!(o, Op::Gb | Op::FixedB(_) | Op::OpcodeB)),
        Some(Op::Ev) => any(|o| matches!(o, Op::Gv | Op::FixedV(_) | Op::OpcodeV)),
        Some(Op::Ew | Op::EwOrV) => any(|o| matches!(o, Op::Gw | Op::Sw)),
        // A 32-bit integer source is self-evident beside a 32-bit general
        // register. Beside an XMM one it is not — which is the case CVTSI2SS
        // and CVTSI2SD are in, and why they alone in this block say `dword
        // ptr`. Same rule as MOVZX: the operands differ in size, so the
        // register cannot stand in for the memory operand's width.
        Some(Op::Ed) => any(|o| matches!(o, Op::Gd)),
        // `M`, `Mp` and `Ma` name a shape rather than a width — a far pointer is
        // "two more bytes than the register beside it", so ANY register operand
        // does pin it.
        Some(_) => any(Op::implies_size_ref),
    };
    let has_memory = memory.is_some();
    let needs_marker = has_memory && !entry.fixed_size && !size_is_evident;

    // A size prefix is printed explicitly only when it is RELEVANT to this
    // instruction yet INVISIBLE in the operands.
    //
    // Relevance matters as much as visibility: `66` in front of a byte-only
    // operation such as `add al,0` selects nothing, so marking it would be
    // noise. Where it does select something — a `v`-sized operand, a stack
    // push, a branch target — but no register or size marker reveals which
    // width won, the prefix has to say so itself.
    let reg_form = ctx.mod_field() == 3;
    let byte_or_size_free = |o: &Op| {
        // `EwOrV` only widens in its register form; as memory it is always two
        // bytes, so a prefix in front of `mov [bx+si],es` selects nothing.
        if matches!(o, Op::EwOrV) {
            return !reg_form;
        }
        matches!(
            o,
            Op::None
                | Op::Eb
                | Op::Gb
                // x87 operands take their width from the opcode, so a `66` in
                // front of one selects nothing.
                | Op::Mx(_)
                | Op::Pq
                | Op::Qq
                | Op::Nq
                | Op::Ed
                | Op::Gd
                | Op::Vx
                | Op::Wx
                | Op::Ux
                | Op::St
                | Op::St0
                | Op::FixedAx
                | Op::FixedB(_)
                | Op::OpcodeB
                // The control- and debug-register moves are 32-bit whatever the
                // prefix says, so a `66` in front of one selects nothing.
                | Op::Rd
                | Op::Cd
                | Op::Dd
                | Op::Ib
                | Op::Xb
                | Op::Yb
                | Op::Ob
                | Op::Cl
                | Op::Dx
                | Op::One
                | Op::XlatTable
                | Op::Gw
                | Op::Ew
                | Op::Sw
        )
    };
    const SIZE_FREE: &[&str] = &[
        "nop", "hlt", "cli", "sti", "cld", "std", "clc", "stc", "cmc", "int3", "into", "salc",
        "xlat", "daa", "das", "aaa", "aas", "aam", "aad", "fwait", "int1", "arpl", "sahf", "lahf",
        // The two-byte map's operandless instructions. None of them reads or
        // writes anything whose width a `66` prefix could change.
        "syscall", "sysret", "clts", "invd", "wbinvd", "ud2", "wrmsr", "rdtsc", "rdmsr", "rdpmc",
        "sysenter", "sysexit", "cpuid", "rsm", "lfence", "mfence", "sfence",
        // AMD 3DNow! and Intel MMX both leave the FPU stack; neither reads
        // anything an operand-size prefix could widen.
        "femms", "emms",
    ];
    // An entry with exactly one legal memory size cannot be changed by an
    // operand-size prefix either, so the prefix is not worth printing — the same
    // fact that suppresses the size marker.
    let size_prefix_relevant = if entry.fixed_size {
        false
    } else if entry.ops.iter().all(|o| *o == Op::None) {
        !SIZE_FREE.contains(&entry.mnemonic)
    } else {
        !entry.ops.iter().all(byte_or_size_free)
    };
    let operand_size_visible = entry.ops.iter().any(|o| {
        matches!(o, Op::Gv | Op::FixedV(_) | Op::OpcodeV)
            || (matches!(o, Op::Ev | Op::EwOrV) && ctx.mod_field() == 3)
    }) || needs_marker
        // A mnemonic that spells its own width — `insd` against `insw` — has
        // already said which size won.
        || size_variant(entry.mnemonic, ctx.op_size32, op)
            != size_variant(entry.mnemonic, !ctx.op_size32, op);

    let mut text = String::with_capacity(32);
    // Before a conditional branch, a CS or DS override is not a segment override
    // at all — it is the Pentium 4 branch hint, and Intel syntax names it.
    // LEA computes an address and never touches memory, so a segment override
    // in front of it selects nothing and is not printed.
    if entry.mnemonic == "lea" {
        ctx.seg_override = None;
    }
    if matches!(op, 0x70..=0x7F) || (two_byte && matches!(ctx.opcode, 0x80..=0x8F)) {
        match ctx.seg_override {
            Some("cs") => text.push_str("hint-not-taken "),
            Some("ds") => text.push_str("hint-taken "),
            _ => {}
        }
    }
    if ctx.lock {
        text.push_str("lock ");
    }
    if ctx.saw_66 && size_prefix_relevant && !operand_size_visible {
        text.push_str(if ctx.op_size32 { "data32 " } else { "data16 " });
    }
    // A `moffs` prints a bare address, whose digits do not say whether it is a
    // 16- or 32-bit offset, so an address-size prefix has to be shown. Every
    // other memory form names registers that reveal it.
    let addr_size_hidden = entry.ops.iter().any(|o| matches!(o, Op::Ob | Op::Ov))
        // LOOP counts in CX or ECX and MASKMOVQ writes through DS:DI or DS:EDI.
        // Neither names the register, so an address-size prefix in front of one
        // is invisible in the operands and has to be printed.
        || matches!(
            entry.mnemonic,
            "loop" | "loope" | "loopne" | "maskmovq" | "maskmovdqu"
        );
    if ctx.saw_67 && addr_size_hidden {
        text.push_str(if ctx.addr_size32 {
            "addr32 "
        } else {
            "addr16 "
        });
    }
    if let Some(rep) = ctx.rep {
        // The repeat prefixes print only on the string instructions they apply
        // to; elsewhere they are ignored bytes and Intel syntax omits them.
        // The string operations, plus `ret`: `F3 C3` is the AMD-recommended
        // `rep ret` idiom that avoids a branch-predictor stall, and it appears
        // in real compiled code often enough to be worth naming.
        if matches!(op, 0x6C..=0x6F | 0xA4..=0xA7 | 0xAA..=0xAF | 0xC3) {
            // On the COMPARISON string operations F3 means "repeat while equal",
            // so it is spelled `repe`; elsewhere it is a plain `rep`.
            let compares = matches!(op, 0xA6 | 0xA7 | 0xAE | 0xAF);
            text.push_str(match (rep, compares) {
                ("rep", true) => "repe",
                (other, _) => other,
            });
            text.push(' ');
        }
    }
    let mnemonic = if entry.mnemonic == "jecxz" && !ctx.addr_size32 {
        "jcxz"
    } else {
        size_variant(entry.mnemonic, ctx.op_size32, op)
    };
    text.push_str(mnemonic);
    // The far forms of CALL and JMP carry `far` after the mnemonic.
    if matches!(entry.mnemonic, "call" | "jmp")
        && entry.ops.iter().any(|o| matches!(o, Op::Ap | Op::Mp))
    {
        text.push_str(" far");
    }

    let mut cursor = ctx.imm_at;
    let mut rendered: Vec<String> = Vec::with_capacity(3);
    for &o in &entry.ops {
        if o == Op::None {
            continue;
        }
        let r = operand(&ctx, o, needs_marker, &mut cursor);
        if !r.is_empty() {
            rendered.push(r);
        }
    }

    // `imul dst, dst, imm` is conventionally written `imul dst, imm`: the
    // three-operand form with a coincident source reads as the two-operand one,
    // and Intel syntax collapses it.
    if entry.mnemonic == "imul" && rendered.len() == 3 && rendered[0] == rendered[1] {
        rendered.remove(1);
    }

    // MASKMOVQ's destination is DS:DI, which it does not name — so a segment
    // override in front of it has nowhere to appear. Intel syntax writes it as
    // a trailing operand rather than dropping it, since it does change where
    // the store lands. Every other overridable instruction names its memory
    // operand, and the segment shows up inside the brackets.
    if matches!(entry.mnemonic, "maskmovq" | "maskmovdqu") {
        if let Some(seg) = ctx.seg_override {
            rendered.push(seg.to_string());
        }
    }

    for (i, r) in rendered.iter().enumerate() {
        text.push_str(if i == 0 { " " } else { "," });
        text.push_str(r);
    }

    debug_assert!(len <= MAX_INSTRUCTION_LEN);
    let flow = ControlFlow::of(entry.mnemonic);
    Ok(Decoded {
        len,
        text,
        flow,
        mnemonic,
    })
}
