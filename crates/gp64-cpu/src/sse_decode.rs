//! Allocation-free decoder for the legacy SSE/SSE2 `0F` map.
//!
//! Decoding is separate from effective-address evaluation: the result holds
//! only the encoded address expression, so it is safe to cache across CPU
//! states. This mirrors [`crate::x87_decode`], which made the same migration
//! for the x87 escape maps.
//!
//! The admitted set is exactly the set the interpreter's SIMD path executed
//! before the port: an encoding is ours when iced-x86 decodes it as a valid
//! instruction *and* the pre-port executor's `level()` (now frozen as the
//! test-only `crate::sse_exec_reference`) reports an extension for it. That
//! boundary is pinned exhaustively by the differential below. MMX-proper
//! encodings stay with the interpreter's compact `0F` switch, so they are
//! reported as not-ours here even though they share opcodes with the forms we
//! do own.

use crate::sse_exec::SseLevel;

/// Longest legal x86 instruction, prefixes included.
pub const MAX_INSTRUCTION_LENGTH: usize = 15;

/// Result of one decode attempt.
///
/// `NotOurs::consumed` is the number of leading bytes the rejection decision
/// actually read, so a caller's byte-revalidating cache can key on just those.
/// `Truncated::required` is the smallest total length that would let the
/// decision be made or the instruction be completed; it is never larger than
/// the instruction's true length, and a caller that supplies that many bytes
/// always makes progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SseDecode {
    Decoded(SseInstr),
    NotOurs { consumed: u8 },
    Truncated { required: usize },
}

/// One decoded SSE/SSE2 instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SseInstr {
    pub op: SseOp,
    /// Total encoded length; the caller's EIP advance.
    pub len: u8,
    pub operands: SseOperands,
}

const _: () = assert!(std::mem::size_of::<SseInstr>() <= 32);

/// Segment a memory operand resolves against.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SseSegment {
    Es,
    Cs,
    Ss,
    #[default]
    Ds,
    Fs,
    Gs,
}

/// Register that may appear in an encoded address expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SseAddrReg {
    Bx,
    Bp,
    Si,
    Di,
    Eax,
    Ecx,
    Edx,
    Ebx,
    Esp,
    Ebp,
    Esi,
    Edi,
}

/// Encoded memory address and access width.
///
/// `width` is the per-encoding access size in bytes (2/4/8/16/512, and 1 for
/// the prefetch hints), which is what drives the segment-limit check.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SseMemory {
    pub addr16: bool,
    pub base: Option<SseAddrReg>,
    pub index: Option<SseAddrReg>,
    pub scale: u8,
    pub disp: i32,
    pub default_segment: SseSegment,
    pub segment: SseSegment,
    pub width: u16,
}

/// One architectural operand. `Memory` refers to
/// [`SseOperands::memory`]; at most one operand is ever memory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SseOperand {
    #[default]
    None,
    Xmm(u8),
    Mm(u8),
    Gpr(u8),
    Memory,
}

/// Decoded operands in architectural order: `dst`, `src`, and the second
/// source that only `MASKMOVQ`/`MASKMOVDQU` carry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SseOperands {
    pub dst: SseOperand,
    pub src: SseOperand,
    pub src2: SseOperand,
    pub imm8: u8,
    /// Meaningful only when some operand is [`SseOperand::Memory`].
    pub memory: SseMemory,
}

impl SseOperands {
    /// The single memory operand, when this encoding has one.
    #[inline]
    #[must_use]
    pub fn memory_operand(&self) -> Option<&SseMemory> {
        let any = matches!(self.dst, SseOperand::Memory)
            || matches!(self.src, SseOperand::Memory)
            || matches!(self.src2, SseOperand::Memory);
        any.then_some(&self.memory)
    }
}

/// Every encoding form the SSE path owns.
///
/// Granularity is per *encoding form*, not per mnemonic: forms that differ in
/// architectural level, register file, or operand direction get their own
/// variant, so the executor never probes an operand at run time to find out
/// which it is. The group-encoded immediate shifts (`0F 71/72/73`) likewise get
/// one variant per sub-opcode rather than carrying the sub-opcode in the
/// operands, for the same reason — and it matches iced's own `Code`
/// granularity, which is what the differential compares against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum SseOp {
    // ---- no mandatory prefix -------------------------------------------
    MovupsLoad,
    MovupsStore,
    MovlpsLoad,
    MovlpsStore,
    Movhlps,
    Movlhps,
    MovhpsLoad,
    MovhpsStore,
    Unpcklps,
    Unpckhps,
    MovapsLoad,
    MovapsStore,
    Cvtpi2ps,
    Movntps,
    Cvttps2pi,
    Cvtps2pi,
    Ucomiss,
    Comiss,
    Movmskps,
    Sqrtps,
    Rsqrtps,
    Rcpps,
    Andps,
    Andnps,
    Orps,
    Xorps,
    Addps,
    Mulps,
    Cvtps2pd,
    Cvtdq2ps,
    Subps,
    Minps,
    Divps,
    Maxps,
    Pshufw,
    Cmpps,
    Movnti,
    PinsrwMm,
    PextrwMm,
    Shufps,
    PmovmskbMm,
    PminubMm,
    PmaxubMm,
    PavgbMm,
    PavgwMm,
    PmulhuwMm,
    Movntq,
    PminswMm,
    PmaxswMm,
    PmuludqMm,
    PsadbwMm,
    Maskmovq,
    PsubqMm,
    Fxsave,
    Fxrstor,
    Ldmxcsr,
    Stmxcsr,
    Lfence,
    Mfence,
    Sfence,
    Prefetchnta,
    Prefetcht0,
    Prefetcht1,
    Prefetcht2,

    // ---- mandatory 66 --------------------------------------------------
    MovupdLoad,
    MovupdStore,
    MovlpdLoad,
    MovlpdStore,
    Unpcklpd,
    Unpckhpd,
    MovhpdLoad,
    MovhpdStore,
    MovapdLoad,
    MovapdStore,
    Cvtpi2pd,
    Movntpd,
    Cvttpd2pi,
    Cvtpd2pi,
    Ucomisd,
    Comisd,
    Movmskpd,
    Sqrtpd,
    Andpd,
    Andnpd,
    Orpd,
    Xorpd,
    Addpd,
    Mulpd,
    Cvtpd2ps,
    Cvtps2dq,
    Subpd,
    Minpd,
    Divpd,
    Maxpd,
    Punpcklbw,
    Punpcklwd,
    Punpckldq,
    Packsswb,
    Pcmpgtb,
    Pcmpgtw,
    Pcmpgtd,
    Packuswb,
    Punpckhbw,
    Punpckhwd,
    Punpckhdq,
    Packssdw,
    Punpcklqdq,
    Punpckhqdq,
    MovdXmmLoad,
    MovdqaLoad,
    Pshufd,
    PsrlwImm,
    PsrawImm,
    PsllwImm,
    PsrldImm,
    PsradImm,
    PslldImm,
    PsrlqImm,
    PsrldqImm,
    PsllqImm,
    PslldqImm,
    Pcmpeqb,
    Pcmpeqw,
    Pcmpeqd,
    MovdXmmStore,
    MovdqaStore,
    Cmppd,
    PinsrwXmm,
    PextrwXmm,
    Shufpd,
    Psrlw,
    Psrld,
    Psrlq,
    Paddq,
    Pmullw,
    MovqStore,
    PmovmskbXmm,
    Psubusb,
    Psubusw,
    Pminub,
    Pand,
    Paddusb,
    Paddusw,
    Pmaxub,
    Pandn,
    Pavgb,
    Psraw,
    Psrad,
    Pavgw,
    Pmulhuw,
    Pmulhw,
    Cvttpd2dq,
    Movntdq,
    Psubsb,
    Psubsw,
    Pminsw,
    Por,
    Paddsb,
    Paddsw,
    Pmaxsw,
    Pxor,
    Psllw,
    Pslld,
    Psllq,
    Pmuludq,
    Pmaddwd,
    Psadbw,
    Maskmovdqu,
    Psubb,
    Psubw,
    Psubd,
    Psubq,
    Paddb,
    Paddw,
    Paddd,

    // ---- mandatory F2 --------------------------------------------------
    MovsdLoad,
    MovsdStore,
    Cvtsi2sd,
    Cvttsd2si,
    Cvtsd2si,
    Sqrtsd,
    Addsd,
    Mulsd,
    Cvtsd2ss,
    Subsd,
    Minsd,
    Divsd,
    Maxsd,
    Pshuflw,
    Cmpsd,
    Movdq2q,
    Cvtpd2dq,

    // ---- mandatory F3 --------------------------------------------------
    MovssLoad,
    MovssStore,
    Cvtsi2ss,
    Cvttss2si,
    Cvtss2si,
    Sqrtss,
    Rsqrtss,
    Rcpss,
    Addss,
    Mulss,
    Cvtss2sd,
    Cvttps2dq,
    Subss,
    Minss,
    Divss,
    Maxss,
    MovdquLoad,
    Pshufhw,
    MovqXmmLoad,
    MovdquStore,
    Cmpss,
    Movq2dq,
    Cvtdq2pd,
}

/// Register file an operand slot draws from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RegKind {
    Xmm,
    Mm,
    Gpr,
}

/// How the ModR/M byte maps onto architectural operands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    /// `dst` is the ModR/M reg field, the source is r/m.
    RegRm { dst: RegKind, rm: RegKind },
    /// The destination is r/m, `src` is the ModR/M reg field.
    RmReg { rm: RegKind, src: RegKind },
    /// Group-encoded immediate shift: r/m is a read-modify-write register.
    ImmShift(RegKind),
    /// Implicit `DS:rDI` destination plus two register operands.
    Maskmov(RegKind),
    /// Single memory source operand.
    MemSrc,
    /// Single memory destination operand.
    MemDst,
    /// No operands.
    Nullary,
}

/// ModR/M shapes an encoding accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModRmForm {
    Any,
    MemOnly,
    RegOnly,
}

struct OpInfo {
    level: SseLevel,
    name: &'static str,
    form: Form,
    modrm: ModRmForm,
    /// Access width in bytes of the memory operand this encoding can carry.
    width: u16,
    imm8: bool,
}

const fn info(
    level: SseLevel,
    name: &'static str,
    form: Form,
    modrm: ModRmForm,
    width: u16,
    imm8: bool,
) -> OpInfo {
    OpInfo {
        level,
        name,
        form,
        modrm,
        width,
        imm8,
    }
}

impl SseOp {
    /// Number of variants. `Cvtdq2pd` is declared last and the enum carries no
    /// explicit discriminants, so its value is the maximum.
    pub const COUNT: usize = Self::Cvtdq2pd as usize + 1;

    /// Recover a form from the dense discriminant `op as u16`.
    ///
    /// SAFETY of the transmute: `SseOp` is `#[repr(u16)]` and declares no
    /// explicit discriminants, so its values are exactly `0..COUNT`, and the
    /// bound below proves `code` names one.
    /// `every_form_round_trips_through_its_discriminant` drives this over the
    /// whole canonical corpus, which the totality test proves is every
    /// variant — so a hand-written discriminant that broke the premise fails
    /// there rather than silently mis-naming a census row.
    #[inline]
    #[must_use]
    pub fn from_code(code: u16) -> Option<Self> {
        (usize::from(code) < Self::COUNT).then(|| {
            // SAFETY: see the discriminant argument above; `code` is in range.
            unsafe { core::mem::transmute::<u16, Self>(code) }
        })
    }

    fn info(self) -> OpInfo {
        use Form::{ImmShift, Maskmov, MemDst, MemSrc, Nullary, RegRm, RmReg};
        use ModRmForm::{Any, MemOnly, RegOnly};
        use RegKind::{Gpr, Mm, Xmm};
        use SseLevel::{Fxsr, Sse, Sse2};

        // Two-operand register/memory shapes, by far the common case.
        const fn xx(rm: RegKind) -> Form {
            RegRm {
                dst: RegKind::Xmm,
                rm,
            }
        }
        const fn mm_(rm: RegKind) -> Form {
            RegRm {
                dst: RegKind::Mm,
                rm,
            }
        }

        match self {
            // ---- no mandatory prefix ---------------------------------
            Self::MovupsLoad => info(Sse, "movups", xx(Xmm), Any, 16, false),
            Self::MovupsStore => info(
                Sse,
                "movups.store",
                RmReg { rm: Xmm, src: Xmm },
                Any,
                16,
                false,
            ),
            Self::MovlpsLoad => info(Sse, "movlps", xx(Xmm), MemOnly, 8, false),
            Self::MovlpsStore => info(
                Sse,
                "movlps.store",
                RmReg { rm: Xmm, src: Xmm },
                MemOnly,
                8,
                false,
            ),
            Self::Movhlps => info(Sse, "movhlps", xx(Xmm), RegOnly, 0, false),
            Self::Movlhps => info(Sse, "movlhps", xx(Xmm), RegOnly, 0, false),
            Self::MovhpsLoad => info(Sse, "movhps", xx(Xmm), MemOnly, 8, false),
            Self::MovhpsStore => info(
                Sse,
                "movhps.store",
                RmReg { rm: Xmm, src: Xmm },
                MemOnly,
                8,
                false,
            ),
            Self::Unpcklps => info(Sse, "unpcklps", xx(Xmm), Any, 16, false),
            Self::Unpckhps => info(Sse, "unpckhps", xx(Xmm), Any, 16, false),
            Self::MovapsLoad => info(Sse, "movaps", xx(Xmm), Any, 16, false),
            Self::MovapsStore => info(
                Sse,
                "movaps.store",
                RmReg { rm: Xmm, src: Xmm },
                Any,
                16,
                false,
            ),
            Self::Cvtpi2ps => info(Sse, "cvtpi2ps", xx(Mm), Any, 8, false),
            Self::Movntps => info(
                Sse,
                "movntps",
                RmReg { rm: Xmm, src: Xmm },
                MemOnly,
                16,
                false,
            ),
            Self::Cvttps2pi => info(Sse, "cvttps2pi", mm_(Xmm), Any, 8, false),
            Self::Cvtps2pi => info(Sse, "cvtps2pi", mm_(Xmm), Any, 8, false),
            Self::Ucomiss => info(Sse, "ucomiss", xx(Xmm), Any, 4, false),
            Self::Comiss => info(Sse, "comiss", xx(Xmm), Any, 4, false),
            Self::Movmskps => info(
                Sse,
                "movmskps",
                RegRm { dst: Gpr, rm: Xmm },
                RegOnly,
                0,
                false,
            ),
            Self::Sqrtps => info(Sse, "sqrtps", xx(Xmm), Any, 16, false),
            Self::Rsqrtps => info(Sse, "rsqrtps", xx(Xmm), Any, 16, false),
            Self::Rcpps => info(Sse, "rcpps", xx(Xmm), Any, 16, false),
            Self::Andps => info(Sse, "andps", xx(Xmm), Any, 16, false),
            Self::Andnps => info(Sse, "andnps", xx(Xmm), Any, 16, false),
            Self::Orps => info(Sse, "orps", xx(Xmm), Any, 16, false),
            Self::Xorps => info(Sse, "xorps", xx(Xmm), Any, 16, false),
            Self::Addps => info(Sse, "addps", xx(Xmm), Any, 16, false),
            Self::Mulps => info(Sse, "mulps", xx(Xmm), Any, 16, false),
            Self::Cvtps2pd => info(Sse2, "cvtps2pd", xx(Xmm), Any, 8, false),
            Self::Cvtdq2ps => info(Sse2, "cvtdq2ps", xx(Xmm), Any, 16, false),
            Self::Subps => info(Sse, "subps", xx(Xmm), Any, 16, false),
            Self::Minps => info(Sse, "minps", xx(Xmm), Any, 16, false),
            Self::Divps => info(Sse, "divps", xx(Xmm), Any, 16, false),
            Self::Maxps => info(Sse, "maxps", xx(Xmm), Any, 16, false),
            Self::Pshufw => info(Sse, "pshufw", mm_(Mm), Any, 8, true),
            Self::Cmpps => info(Sse, "cmpps", xx(Xmm), Any, 16, true),
            Self::Movnti => info(
                Sse2,
                "movnti",
                RmReg { rm: Gpr, src: Gpr },
                MemOnly,
                4,
                false,
            ),
            Self::PinsrwMm => info(Sse, "pinsrw.mm", mm_(Gpr), Any, 2, true),
            Self::PextrwMm => info(
                Sse,
                "pextrw.mm",
                RegRm { dst: Gpr, rm: Mm },
                RegOnly,
                0,
                true,
            ),
            Self::Shufps => info(Sse, "shufps", xx(Xmm), Any, 16, true),
            Self::PmovmskbMm => info(
                Sse,
                "pmovmskb.mm",
                RegRm { dst: Gpr, rm: Mm },
                RegOnly,
                0,
                false,
            ),
            Self::PminubMm => info(Sse, "pminub.mm", mm_(Mm), Any, 8, false),
            Self::PmaxubMm => info(Sse, "pmaxub.mm", mm_(Mm), Any, 8, false),
            Self::PavgbMm => info(Sse, "pavgb.mm", mm_(Mm), Any, 8, false),
            Self::PavgwMm => info(Sse, "pavgw.mm", mm_(Mm), Any, 8, false),
            Self::PmulhuwMm => info(Sse, "pmulhuw.mm", mm_(Mm), Any, 8, false),
            Self::Movntq => info(Sse, "movntq", RmReg { rm: Mm, src: Mm }, MemOnly, 8, false),
            Self::PminswMm => info(Sse, "pminsw.mm", mm_(Mm), Any, 8, false),
            Self::PmaxswMm => info(Sse, "pmaxsw.mm", mm_(Mm), Any, 8, false),
            Self::PmuludqMm => info(Sse2, "pmuludq.mm", mm_(Mm), Any, 8, false),
            Self::PsadbwMm => info(Sse, "psadbw.mm", mm_(Mm), Any, 8, false),
            Self::Maskmovq => info(Sse, "maskmovq", Maskmov(Mm), RegOnly, 8, false),
            Self::PsubqMm => info(Sse2, "psubq.mm", mm_(Mm), Any, 8, false),
            Self::Fxsave => info(Fxsr, "fxsave", MemDst, MemOnly, 512, false),
            Self::Fxrstor => info(Fxsr, "fxrstor", MemSrc, MemOnly, 512, false),
            Self::Ldmxcsr => info(Sse, "ldmxcsr", MemSrc, MemOnly, 4, false),
            Self::Stmxcsr => info(Sse, "stmxcsr", MemDst, MemOnly, 4, false),
            Self::Lfence => info(Sse2, "lfence", Nullary, RegOnly, 0, false),
            Self::Mfence => info(Sse2, "mfence", Nullary, RegOnly, 0, false),
            Self::Sfence => info(Sse, "sfence", Nullary, RegOnly, 0, false),
            Self::Prefetchnta => info(Sse, "prefetchnta", MemSrc, MemOnly, 1, false),
            Self::Prefetcht0 => info(Sse, "prefetcht0", MemSrc, MemOnly, 1, false),
            Self::Prefetcht1 => info(Sse, "prefetcht1", MemSrc, MemOnly, 1, false),
            Self::Prefetcht2 => info(Sse, "prefetcht2", MemSrc, MemOnly, 1, false),

            // ---- mandatory 66 ----------------------------------------
            Self::MovupdLoad => info(Sse2, "movupd", xx(Xmm), Any, 16, false),
            Self::MovupdStore => info(
                Sse2,
                "movupd.store",
                RmReg { rm: Xmm, src: Xmm },
                Any,
                16,
                false,
            ),
            Self::MovlpdLoad => info(Sse2, "movlpd", xx(Xmm), MemOnly, 8, false),
            Self::MovlpdStore => info(
                Sse2,
                "movlpd.store",
                RmReg { rm: Xmm, src: Xmm },
                MemOnly,
                8,
                false,
            ),
            Self::Unpcklpd => info(Sse2, "unpcklpd", xx(Xmm), Any, 16, false),
            Self::Unpckhpd => info(Sse2, "unpckhpd", xx(Xmm), Any, 16, false),
            Self::MovhpdLoad => info(Sse2, "movhpd", xx(Xmm), MemOnly, 8, false),
            Self::MovhpdStore => info(
                Sse2,
                "movhpd.store",
                RmReg { rm: Xmm, src: Xmm },
                MemOnly,
                8,
                false,
            ),
            Self::MovapdLoad => info(Sse2, "movapd", xx(Xmm), Any, 16, false),
            Self::MovapdStore => info(
                Sse2,
                "movapd.store",
                RmReg { rm: Xmm, src: Xmm },
                Any,
                16,
                false,
            ),
            Self::Cvtpi2pd => info(Sse2, "cvtpi2pd", xx(Mm), Any, 8, false),
            Self::Movntpd => info(
                Sse2,
                "movntpd",
                RmReg { rm: Xmm, src: Xmm },
                MemOnly,
                16,
                false,
            ),
            Self::Cvttpd2pi => info(Sse2, "cvttpd2pi", mm_(Xmm), Any, 16, false),
            Self::Cvtpd2pi => info(Sse2, "cvtpd2pi", mm_(Xmm), Any, 16, false),
            Self::Ucomisd => info(Sse2, "ucomisd", xx(Xmm), Any, 8, false),
            Self::Comisd => info(Sse2, "comisd", xx(Xmm), Any, 8, false),
            Self::Movmskpd => info(
                Sse2,
                "movmskpd",
                RegRm { dst: Gpr, rm: Xmm },
                RegOnly,
                0,
                false,
            ),
            Self::Sqrtpd => info(Sse2, "sqrtpd", xx(Xmm), Any, 16, false),
            Self::Andpd => info(Sse2, "andpd", xx(Xmm), Any, 16, false),
            Self::Andnpd => info(Sse2, "andnpd", xx(Xmm), Any, 16, false),
            Self::Orpd => info(Sse2, "orpd", xx(Xmm), Any, 16, false),
            Self::Xorpd => info(Sse2, "xorpd", xx(Xmm), Any, 16, false),
            Self::Addpd => info(Sse2, "addpd", xx(Xmm), Any, 16, false),
            Self::Mulpd => info(Sse2, "mulpd", xx(Xmm), Any, 16, false),
            Self::Cvtpd2ps => info(Sse2, "cvtpd2ps", xx(Xmm), Any, 16, false),
            Self::Cvtps2dq => info(Sse2, "cvtps2dq", xx(Xmm), Any, 16, false),
            Self::Subpd => info(Sse2, "subpd", xx(Xmm), Any, 16, false),
            Self::Minpd => info(Sse2, "minpd", xx(Xmm), Any, 16, false),
            Self::Divpd => info(Sse2, "divpd", xx(Xmm), Any, 16, false),
            Self::Maxpd => info(Sse2, "maxpd", xx(Xmm), Any, 16, false),
            Self::Punpcklbw => info(Sse2, "punpcklbw", xx(Xmm), Any, 16, false),
            Self::Punpcklwd => info(Sse2, "punpcklwd", xx(Xmm), Any, 16, false),
            Self::Punpckldq => info(Sse2, "punpckldq", xx(Xmm), Any, 16, false),
            Self::Packsswb => info(Sse2, "packsswb", xx(Xmm), Any, 16, false),
            Self::Pcmpgtb => info(Sse2, "pcmpgtb", xx(Xmm), Any, 16, false),
            Self::Pcmpgtw => info(Sse2, "pcmpgtw", xx(Xmm), Any, 16, false),
            Self::Pcmpgtd => info(Sse2, "pcmpgtd", xx(Xmm), Any, 16, false),
            Self::Packuswb => info(Sse2, "packuswb", xx(Xmm), Any, 16, false),
            Self::Punpckhbw => info(Sse2, "punpckhbw", xx(Xmm), Any, 16, false),
            Self::Punpckhwd => info(Sse2, "punpckhwd", xx(Xmm), Any, 16, false),
            Self::Punpckhdq => info(Sse2, "punpckhdq", xx(Xmm), Any, 16, false),
            Self::Packssdw => info(Sse2, "packssdw", xx(Xmm), Any, 16, false),
            Self::Punpcklqdq => info(Sse2, "punpcklqdq", xx(Xmm), Any, 16, false),
            Self::Punpckhqdq => info(Sse2, "punpckhqdq", xx(Xmm), Any, 16, false),
            Self::MovdXmmLoad => info(Sse2, "movd.xmm", xx(Gpr), Any, 4, false),
            Self::MovdqaLoad => info(Sse2, "movdqa", xx(Xmm), Any, 16, false),
            Self::Pshufd => info(Sse2, "pshufd", xx(Xmm), Any, 16, true),
            Self::PsrlwImm => info(Sse2, "psrlw.imm", ImmShift(Xmm), RegOnly, 0, true),
            Self::PsrawImm => info(Sse2, "psraw.imm", ImmShift(Xmm), RegOnly, 0, true),
            Self::PsllwImm => info(Sse2, "psllw.imm", ImmShift(Xmm), RegOnly, 0, true),
            Self::PsrldImm => info(Sse2, "psrld.imm", ImmShift(Xmm), RegOnly, 0, true),
            Self::PsradImm => info(Sse2, "psrad.imm", ImmShift(Xmm), RegOnly, 0, true),
            Self::PslldImm => info(Sse2, "pslld.imm", ImmShift(Xmm), RegOnly, 0, true),
            Self::PsrlqImm => info(Sse2, "psrlq.imm", ImmShift(Xmm), RegOnly, 0, true),
            Self::PsrldqImm => info(Sse2, "psrldq.imm", ImmShift(Xmm), RegOnly, 0, true),
            Self::PsllqImm => info(Sse2, "psllq.imm", ImmShift(Xmm), RegOnly, 0, true),
            Self::PslldqImm => info(Sse2, "pslldq.imm", ImmShift(Xmm), RegOnly, 0, true),
            Self::Pcmpeqb => info(Sse2, "pcmpeqb", xx(Xmm), Any, 16, false),
            Self::Pcmpeqw => info(Sse2, "pcmpeqw", xx(Xmm), Any, 16, false),
            Self::Pcmpeqd => info(Sse2, "pcmpeqd", xx(Xmm), Any, 16, false),
            Self::MovdXmmStore => info(
                Sse2,
                "movd.store",
                RmReg { rm: Gpr, src: Xmm },
                Any,
                4,
                false,
            ),
            Self::MovdqaStore => info(
                Sse2,
                "movdqa.store",
                RmReg { rm: Xmm, src: Xmm },
                Any,
                16,
                false,
            ),
            Self::Cmppd => info(Sse2, "cmppd", xx(Xmm), Any, 16, true),
            Self::PinsrwXmm => info(Sse2, "pinsrw", xx(Gpr), Any, 2, true),
            Self::PextrwXmm => info(
                Sse2,
                "pextrw",
                RegRm { dst: Gpr, rm: Xmm },
                RegOnly,
                0,
                true,
            ),
            Self::Shufpd => info(Sse2, "shufpd", xx(Xmm), Any, 16, true),
            Self::Psrlw => info(Sse2, "psrlw", xx(Xmm), Any, 16, false),
            Self::Psrld => info(Sse2, "psrld", xx(Xmm), Any, 16, false),
            Self::Psrlq => info(Sse2, "psrlq", xx(Xmm), Any, 16, false),
            Self::Paddq => info(Sse2, "paddq", xx(Xmm), Any, 16, false),
            Self::Pmullw => info(Sse2, "pmullw", xx(Xmm), Any, 16, false),
            Self::MovqStore => info(
                Sse2,
                "movq.store",
                RmReg { rm: Xmm, src: Xmm },
                Any,
                8,
                false,
            ),
            Self::PmovmskbXmm => info(
                Sse2,
                "pmovmskb",
                RegRm { dst: Gpr, rm: Xmm },
                RegOnly,
                0,
                false,
            ),
            Self::Psubusb => info(Sse2, "psubusb", xx(Xmm), Any, 16, false),
            Self::Psubusw => info(Sse2, "psubusw", xx(Xmm), Any, 16, false),
            Self::Pminub => info(Sse2, "pminub", xx(Xmm), Any, 16, false),
            Self::Pand => info(Sse2, "pand", xx(Xmm), Any, 16, false),
            Self::Paddusb => info(Sse2, "paddusb", xx(Xmm), Any, 16, false),
            Self::Paddusw => info(Sse2, "paddusw", xx(Xmm), Any, 16, false),
            Self::Pmaxub => info(Sse2, "pmaxub", xx(Xmm), Any, 16, false),
            Self::Pandn => info(Sse2, "pandn", xx(Xmm), Any, 16, false),
            Self::Pavgb => info(Sse2, "pavgb", xx(Xmm), Any, 16, false),
            Self::Psraw => info(Sse2, "psraw", xx(Xmm), Any, 16, false),
            Self::Psrad => info(Sse2, "psrad", xx(Xmm), Any, 16, false),
            Self::Pavgw => info(Sse2, "pavgw", xx(Xmm), Any, 16, false),
            Self::Pmulhuw => info(Sse2, "pmulhuw", xx(Xmm), Any, 16, false),
            Self::Pmulhw => info(Sse2, "pmulhw", xx(Xmm), Any, 16, false),
            Self::Cvttpd2dq => info(Sse2, "cvttpd2dq", xx(Xmm), Any, 16, false),
            Self::Movntdq => info(
                Sse2,
                "movntdq",
                RmReg { rm: Xmm, src: Xmm },
                MemOnly,
                16,
                false,
            ),
            Self::Psubsb => info(Sse2, "psubsb", xx(Xmm), Any, 16, false),
            Self::Psubsw => info(Sse2, "psubsw", xx(Xmm), Any, 16, false),
            Self::Pminsw => info(Sse2, "pminsw", xx(Xmm), Any, 16, false),
            Self::Por => info(Sse2, "por", xx(Xmm), Any, 16, false),
            Self::Paddsb => info(Sse2, "paddsb", xx(Xmm), Any, 16, false),
            Self::Paddsw => info(Sse2, "paddsw", xx(Xmm), Any, 16, false),
            Self::Pmaxsw => info(Sse2, "pmaxsw", xx(Xmm), Any, 16, false),
            Self::Pxor => info(Sse2, "pxor", xx(Xmm), Any, 16, false),
            Self::Psllw => info(Sse2, "psllw", xx(Xmm), Any, 16, false),
            Self::Pslld => info(Sse2, "pslld", xx(Xmm), Any, 16, false),
            Self::Psllq => info(Sse2, "psllq", xx(Xmm), Any, 16, false),
            Self::Pmuludq => info(Sse2, "pmuludq", xx(Xmm), Any, 16, false),
            Self::Pmaddwd => info(Sse2, "pmaddwd", xx(Xmm), Any, 16, false),
            Self::Psadbw => info(Sse2, "psadbw", xx(Xmm), Any, 16, false),
            Self::Maskmovdqu => info(Sse2, "maskmovdqu", Maskmov(Xmm), RegOnly, 16, false),
            Self::Psubb => info(Sse2, "psubb", xx(Xmm), Any, 16, false),
            Self::Psubw => info(Sse2, "psubw", xx(Xmm), Any, 16, false),
            Self::Psubd => info(Sse2, "psubd", xx(Xmm), Any, 16, false),
            Self::Psubq => info(Sse2, "psubq", xx(Xmm), Any, 16, false),
            Self::Paddb => info(Sse2, "paddb", xx(Xmm), Any, 16, false),
            Self::Paddw => info(Sse2, "paddw", xx(Xmm), Any, 16, false),
            Self::Paddd => info(Sse2, "paddd", xx(Xmm), Any, 16, false),

            // ---- mandatory F2 ----------------------------------------
            Self::MovsdLoad => info(Sse2, "movsd", xx(Xmm), Any, 8, false),
            Self::MovsdStore => info(
                Sse2,
                "movsd.store",
                RmReg { rm: Xmm, src: Xmm },
                Any,
                8,
                false,
            ),
            Self::Cvtsi2sd => info(Sse2, "cvtsi2sd", xx(Gpr), Any, 4, false),
            Self::Cvttsd2si => info(
                Sse2,
                "cvttsd2si",
                RegRm { dst: Gpr, rm: Xmm },
                Any,
                8,
                false,
            ),
            Self::Cvtsd2si => info(Sse2, "cvtsd2si", RegRm { dst: Gpr, rm: Xmm }, Any, 8, false),
            Self::Sqrtsd => info(Sse2, "sqrtsd", xx(Xmm), Any, 8, false),
            Self::Addsd => info(Sse2, "addsd", xx(Xmm), Any, 8, false),
            Self::Mulsd => info(Sse2, "mulsd", xx(Xmm), Any, 8, false),
            Self::Cvtsd2ss => info(Sse2, "cvtsd2ss", xx(Xmm), Any, 8, false),
            Self::Subsd => info(Sse2, "subsd", xx(Xmm), Any, 8, false),
            Self::Minsd => info(Sse2, "minsd", xx(Xmm), Any, 8, false),
            Self::Divsd => info(Sse2, "divsd", xx(Xmm), Any, 8, false),
            Self::Maxsd => info(Sse2, "maxsd", xx(Xmm), Any, 8, false),
            Self::Pshuflw => info(Sse2, "pshuflw", xx(Xmm), Any, 16, true),
            Self::Cmpsd => info(Sse2, "cmpsd", xx(Xmm), Any, 8, true),
            Self::Movdq2q => info(Sse2, "movdq2q", mm_(Xmm), RegOnly, 0, false),
            Self::Cvtpd2dq => info(Sse2, "cvtpd2dq", xx(Xmm), Any, 16, false),

            // ---- mandatory F3 ----------------------------------------
            Self::MovssLoad => info(Sse, "movss", xx(Xmm), Any, 4, false),
            Self::MovssStore => info(
                Sse,
                "movss.store",
                RmReg { rm: Xmm, src: Xmm },
                Any,
                4,
                false,
            ),
            Self::Cvtsi2ss => info(Sse, "cvtsi2ss", xx(Gpr), Any, 4, false),
            Self::Cvttss2si => info(Sse, "cvttss2si", RegRm { dst: Gpr, rm: Xmm }, Any, 4, false),
            Self::Cvtss2si => info(Sse, "cvtss2si", RegRm { dst: Gpr, rm: Xmm }, Any, 4, false),
            Self::Sqrtss => info(Sse, "sqrtss", xx(Xmm), Any, 4, false),
            Self::Rsqrtss => info(Sse, "rsqrtss", xx(Xmm), Any, 4, false),
            Self::Rcpss => info(Sse, "rcpss", xx(Xmm), Any, 4, false),
            Self::Addss => info(Sse, "addss", xx(Xmm), Any, 4, false),
            Self::Mulss => info(Sse, "mulss", xx(Xmm), Any, 4, false),
            Self::Cvtss2sd => info(Sse2, "cvtss2sd", xx(Xmm), Any, 4, false),
            Self::Cvttps2dq => info(Sse2, "cvttps2dq", xx(Xmm), Any, 16, false),
            Self::Subss => info(Sse, "subss", xx(Xmm), Any, 4, false),
            Self::Minss => info(Sse, "minss", xx(Xmm), Any, 4, false),
            Self::Divss => info(Sse, "divss", xx(Xmm), Any, 4, false),
            Self::Maxss => info(Sse, "maxss", xx(Xmm), Any, 4, false),
            Self::MovdquLoad => info(Sse2, "movdqu", xx(Xmm), Any, 16, false),
            Self::Pshufhw => info(Sse2, "pshufhw", xx(Xmm), Any, 16, true),
            Self::MovqXmmLoad => info(Sse2, "movq", xx(Xmm), Any, 8, false),
            Self::MovdquStore => info(
                Sse2,
                "movdqu.store",
                RmReg { rm: Xmm, src: Xmm },
                Any,
                16,
                false,
            ),
            Self::Cmpss => info(Sse, "cmpss", xx(Xmm), Any, 4, true),
            Self::Movq2dq => info(Sse2, "movq2dq", xx(Mm), RegOnly, 0, false),
            Self::Cvtdq2pd => info(Sse2, "cvtdq2pd", xx(Xmm), Any, 8, false),
        }
    }

    /// Architectural extension this encoding form requires.
    #[inline]
    #[must_use]
    pub fn level(self) -> SseLevel {
        self.info().level
    }

    /// Whether this encoding form requires CR4.OSFXSR to be set.
    ///
    /// The OSFXSR gate protects the XMM register state, not the MMX aliases
    /// in the x87 register file or the SSE-era instructions that touch no
    /// vector register file. The MMX-register forms use SSE-era opcodes, but
    /// remain executable with OSFXSR clear when the corresponding SSE feature
    /// is present. Keep this separate from [`Self::level`], which is only the
    /// architectural extension level.
    #[inline]
    #[must_use]
    pub fn requires_osfxsr(self) -> bool {
        !matches!(
            self,
            Self::Pshufw
                | Self::PinsrwMm
                | Self::PextrwMm
                | Self::PmovmskbMm
                | Self::PminubMm
                | Self::PmaxubMm
                | Self::PavgbMm
                | Self::PavgwMm
                | Self::PmulhuwMm
                | Self::Movntq
                | Self::PminswMm
                | Self::PmaxswMm
                | Self::PmuludqMm
                | Self::PsadbwMm
                | Self::Maskmovq
                | Self::PsubqMm
                | Self::Movnti
                | Self::Lfence
                | Self::Mfence
                | Self::Sfence
                | Self::Prefetchnta
                | Self::Prefetcht0
                | Self::Prefetcht1
                | Self::Prefetcht2
        )
    }

    /// Whether the memory operand requires 16-byte alignment (#GP(0) if
    /// misaligned): aligned moves, non-temporal 16-byte stores and
    /// FXSAVE/FXRSTOR. Packed arithmetic permits misalignment
    /// (MDD-BUG-CRUCIBLE-00775). The executor uses this classification.
    #[inline]
    #[must_use]
    pub fn requires_16_byte_alignment(self) -> bool {
        matches!(
            self,
            Self::MovapsLoad
                | Self::MovapsStore
                | Self::MovapdLoad
                | Self::MovapdStore
                | Self::MovdqaLoad
                | Self::MovdqaStore
                | Self::Movntps
                | Self::Movntpd
                | Self::Movntdq
                | Self::Fxsave
                | Self::Fxrstor
        )
    }

    /// Stable lowercase name; unique per encoding form.
    #[inline]
    #[must_use]
    pub fn name(self) -> &'static str {
        self.info().name
    }

    /// Access width in bytes of the memory operand this form can carry, or 0
    /// when the form has no memory operand.
    #[inline]
    #[must_use]
    pub fn memory_width(self) -> u16 {
        self.info().width
    }

    /// Whether the encoding carries a trailing `imm8`.
    #[inline]
    #[must_use]
    pub fn has_imm8(self) -> bool {
        self.info().imm8
    }

    /// The four compare-and-set-EFLAGS forms.
    #[inline]
    #[must_use]
    pub fn updates_eflags(self) -> bool {
        matches!(
            self,
            Self::Comiss | Self::Ucomiss | Self::Comisd | Self::Ucomisd
        )
    }

    /// Whether the form can read its architectural destination — either
    /// because it is a true read-modify-write, or because it leaves part of
    /// the destination unmodified.
    ///
    /// This is a conservative "may read": `MOVSS` and `MOVSD` merge only when
    /// the other operand is a register, and the register/memory choice is not
    /// part of the encoding form, so both of their forms report `true`.
    /// `MASKMOVQ`/`MASKMOVDQU` report `true` because the executor implements
    /// the byte-masked store as a read-modify-write of its memory destination.
    #[inline]
    #[must_use]
    pub fn reads_destination(self) -> bool {
        match self {
            // Whole-destination writes and pure stores.
            Self::MovupsLoad
            | Self::MovupsStore
            | Self::MovlpsStore
            | Self::MovhpsStore
            | Self::MovapsLoad
            | Self::MovapsStore
            | Self::Movntps
            | Self::Cvttps2pi
            | Self::Cvtps2pi
            | Self::Movmskps
            | Self::Sqrtps
            | Self::Rsqrtps
            | Self::Rcpps
            | Self::Cvtps2pd
            | Self::Cvtdq2ps
            | Self::Pshufw
            | Self::Movnti
            | Self::PextrwMm
            | Self::PmovmskbMm
            | Self::Movntq
            | Self::Fxsave
            | Self::Stmxcsr
            | Self::Lfence
            | Self::Mfence
            | Self::Sfence
            | Self::Prefetchnta
            | Self::Prefetcht0
            | Self::Prefetcht1
            | Self::Prefetcht2
            | Self::MovupdLoad
            | Self::MovupdStore
            | Self::MovlpdStore
            | Self::MovhpdStore
            | Self::MovapdLoad
            | Self::MovapdStore
            | Self::Cvtpi2pd
            | Self::Movntpd
            | Self::Cvttpd2pi
            | Self::Cvtpd2pi
            | Self::Movmskpd
            | Self::Sqrtpd
            | Self::Cvtpd2ps
            | Self::Cvtps2dq
            | Self::MovdXmmLoad
            | Self::MovdqaLoad
            | Self::Pshufd
            | Self::MovdXmmStore
            | Self::MovdqaStore
            | Self::PextrwXmm
            | Self::MovqStore
            | Self::PmovmskbXmm
            | Self::Cvttpd2dq
            | Self::Movntdq
            | Self::Cvttsd2si
            | Self::Cvtsd2si
            | Self::Pshuflw
            | Self::Movdq2q
            | Self::Cvtpd2dq
            | Self::Cvttss2si
            | Self::Cvtss2si
            | Self::Cvttps2dq
            | Self::MovdquLoad
            | Self::Pshufhw
            | Self::MovdquStore
            | Self::Movq2dq
            | Self::Cvtdq2pd
            | Self::Fxrstor
            | Self::Ldmxcsr => false,
            // Everything else merges into, or reads, the destination.
            _ => true,
        }
    }
}

// -----------------------------------------------------------------------
// Prefix scanning and the opcode table
// -----------------------------------------------------------------------

/// Mandatory-prefix class selected by the byte prefixes.
///
/// `F2`/`F3` take precedence over `66` regardless of order, and the *last*
/// `F2`-or-`F3` wins — verified against iced for `F2 F3`, `F3 F2`, and
/// `F2 F3 F2`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrefixClass {
    None,
    P66,
    Pf2,
    Pf3,
}

#[derive(Clone, Copy, Default)]
struct Prefixes {
    lock: bool,
    operand_size: bool,
    address_size: bool,
    segment: Option<SseSegment>,
    repeat: Option<bool>, // Some(true) = F3, Some(false) = F2
}

impl Prefixes {
    fn class(self) -> PrefixClass {
        match self.repeat {
            Some(true) => PrefixClass::Pf3,
            Some(false) => PrefixClass::Pf2,
            None if self.operand_size => PrefixClass::P66,
            None => PrefixClass::None,
        }
    }
}

/// Whether any ModR/M byte could make this (class, opcode) one of ours.
///
/// Kept separate from [`op_for`] so a rejection that never inspected the
/// ModR/M byte reports the smaller `consumed`. The two are pinned equivalent
/// by `candidate_opcodes_agree_with_the_op_table`.
const fn candidate_opcode(class: PrefixClass, opcode: u8) -> bool {
    if opcode == 0x18 {
        // The prefetch hints ignore the mandatory prefix entirely.
        return true;
    }
    match class {
        PrefixClass::None => matches!(
            opcode,
            0x10..=0x17
                | 0x28..=0x2F
                | 0x50..=0x5F
                | 0x70
                | 0xAE
                | 0xC2..=0xC6
                | 0xD7
                | 0xDA
                | 0xDE
                | 0xE0
                | 0xE3
                | 0xE4
                | 0xE7
                | 0xEA
                | 0xEE
                | 0xF4
                | 0xF6
                | 0xF7
                | 0xFB
        ),
        PrefixClass::P66 => matches!(
            opcode,
            0x10..=0x17
                | 0x28..=0x2F
                | 0x50
                | 0x51
                | 0x54..=0x5F
                | 0x60..=0x6F
                | 0x70..=0x76
                | 0x7E
                | 0x7F
                | 0xC2
                | 0xC4..=0xC6
                | 0xD1..=0xD7
                | 0xD8..=0xDF
                | 0xE0..=0xE7
                | 0xE8..=0xEF
                | 0xF1..=0xF7
                | 0xF8..=0xFE
        ),
        PrefixClass::Pf2 => matches!(
            opcode,
            0x10 | 0x11
                | 0x2A
                | 0x2C
                | 0x2D
                | 0x51
                | 0x58..=0x5A
                | 0x5C..=0x5F
                | 0x70
                | 0xC2
                | 0xD6
                | 0xE6
        ),
        PrefixClass::Pf3 => matches!(
            opcode,
            0x10 | 0x11
                | 0x2A
                | 0x2C
                | 0x2D
                | 0x51..=0x53
                | 0x58..=0x5F
                | 0x6F
                | 0x70
                | 0x7E
                | 0x7F
                | 0xC2
                | 0xD6
                | 0xE6
        ),
    }
}

/// Resolve one encoding to its form, or `None` when it is not ours.
const fn op_for(class: PrefixClass, opcode: u8, modrm: u8) -> Option<SseOp> {
    use SseOp as O;

    let reg = (modrm >> 3) & 7;
    let is_reg_form = modrm >= 0xC0;

    // The prefetch hints sit outside the mandatory-prefix tables.
    if opcode == 0x18 {
        return match reg {
            0 => Some(O::Prefetchnta),
            1 => Some(O::Prefetcht0),
            2 => Some(O::Prefetcht1),
            3 => Some(O::Prefetcht2),
            _ => None,
        };
    }

    match class {
        PrefixClass::None => match opcode {
            0x10 => Some(O::MovupsLoad),
            0x11 => Some(O::MovupsStore),
            0x12 => {
                if is_reg_form {
                    Some(O::Movhlps)
                } else {
                    Some(O::MovlpsLoad)
                }
            }
            0x13 => Some(O::MovlpsStore),
            0x14 => Some(O::Unpcklps),
            0x15 => Some(O::Unpckhps),
            0x16 => {
                if is_reg_form {
                    Some(O::Movlhps)
                } else {
                    Some(O::MovhpsLoad)
                }
            }
            0x17 => Some(O::MovhpsStore),
            0x28 => Some(O::MovapsLoad),
            0x29 => Some(O::MovapsStore),
            0x2A => Some(O::Cvtpi2ps),
            0x2B => Some(O::Movntps),
            0x2C => Some(O::Cvttps2pi),
            0x2D => Some(O::Cvtps2pi),
            0x2E => Some(O::Ucomiss),
            0x2F => Some(O::Comiss),
            0x50 => Some(O::Movmskps),
            0x51 => Some(O::Sqrtps),
            0x52 => Some(O::Rsqrtps),
            0x53 => Some(O::Rcpps),
            0x54 => Some(O::Andps),
            0x55 => Some(O::Andnps),
            0x56 => Some(O::Orps),
            0x57 => Some(O::Xorps),
            0x58 => Some(O::Addps),
            0x59 => Some(O::Mulps),
            0x5A => Some(O::Cvtps2pd),
            0x5B => Some(O::Cvtdq2ps),
            0x5C => Some(O::Subps),
            0x5D => Some(O::Minps),
            0x5E => Some(O::Divps),
            0x5F => Some(O::Maxps),
            0x70 => Some(O::Pshufw),
            0xAE => {
                if is_reg_form {
                    match reg {
                        5 => Some(O::Lfence),
                        6 => Some(O::Mfence),
                        7 => Some(O::Sfence),
                        _ => None,
                    }
                } else {
                    match reg {
                        0 => Some(O::Fxsave),
                        1 => Some(O::Fxrstor),
                        2 => Some(O::Ldmxcsr),
                        3 => Some(O::Stmxcsr),
                        _ => None,
                    }
                }
            }
            0xC2 => Some(O::Cmpps),
            0xC3 => Some(O::Movnti),
            0xC4 => Some(O::PinsrwMm),
            0xC5 => Some(O::PextrwMm),
            0xC6 => Some(O::Shufps),
            0xD7 => Some(O::PmovmskbMm),
            0xDA => Some(O::PminubMm),
            0xDE => Some(O::PmaxubMm),
            0xE0 => Some(O::PavgbMm),
            0xE3 => Some(O::PavgwMm),
            0xE4 => Some(O::PmulhuwMm),
            0xE7 => Some(O::Movntq),
            0xEA => Some(O::PminswMm),
            0xEE => Some(O::PmaxswMm),
            0xF4 => Some(O::PmuludqMm),
            0xF6 => Some(O::PsadbwMm),
            0xF7 => Some(O::Maskmovq),
            0xFB => Some(O::PsubqMm),
            _ => None,
        },
        PrefixClass::P66 => match opcode {
            0x10 => Some(O::MovupdLoad),
            0x11 => Some(O::MovupdStore),
            0x12 => Some(O::MovlpdLoad),
            0x13 => Some(O::MovlpdStore),
            0x14 => Some(O::Unpcklpd),
            0x15 => Some(O::Unpckhpd),
            0x16 => Some(O::MovhpdLoad),
            0x17 => Some(O::MovhpdStore),
            0x28 => Some(O::MovapdLoad),
            0x29 => Some(O::MovapdStore),
            0x2A => Some(O::Cvtpi2pd),
            0x2B => Some(O::Movntpd),
            0x2C => Some(O::Cvttpd2pi),
            0x2D => Some(O::Cvtpd2pi),
            0x2E => Some(O::Ucomisd),
            0x2F => Some(O::Comisd),
            0x50 => Some(O::Movmskpd),
            0x51 => Some(O::Sqrtpd),
            0x54 => Some(O::Andpd),
            0x55 => Some(O::Andnpd),
            0x56 => Some(O::Orpd),
            0x57 => Some(O::Xorpd),
            0x58 => Some(O::Addpd),
            0x59 => Some(O::Mulpd),
            0x5A => Some(O::Cvtpd2ps),
            0x5B => Some(O::Cvtps2dq),
            0x5C => Some(O::Subpd),
            0x5D => Some(O::Minpd),
            0x5E => Some(O::Divpd),
            0x5F => Some(O::Maxpd),
            0x60 => Some(O::Punpcklbw),
            0x61 => Some(O::Punpcklwd),
            0x62 => Some(O::Punpckldq),
            0x63 => Some(O::Packsswb),
            0x64 => Some(O::Pcmpgtb),
            0x65 => Some(O::Pcmpgtw),
            0x66 => Some(O::Pcmpgtd),
            0x67 => Some(O::Packuswb),
            0x68 => Some(O::Punpckhbw),
            0x69 => Some(O::Punpckhwd),
            0x6A => Some(O::Punpckhdq),
            0x6B => Some(O::Packssdw),
            0x6C => Some(O::Punpcklqdq),
            0x6D => Some(O::Punpckhqdq),
            0x6E => Some(O::MovdXmmLoad),
            0x6F => Some(O::MovdqaLoad),
            0x70 => Some(O::Pshufd),
            0x71 => match reg {
                2 => Some(O::PsrlwImm),
                4 => Some(O::PsrawImm),
                6 => Some(O::PsllwImm),
                _ => None,
            },
            0x72 => match reg {
                2 => Some(O::PsrldImm),
                4 => Some(O::PsradImm),
                6 => Some(O::PslldImm),
                _ => None,
            },
            0x73 => match reg {
                2 => Some(O::PsrlqImm),
                3 => Some(O::PsrldqImm),
                6 => Some(O::PsllqImm),
                7 => Some(O::PslldqImm),
                _ => None,
            },
            0x74 => Some(O::Pcmpeqb),
            0x75 => Some(O::Pcmpeqw),
            0x76 => Some(O::Pcmpeqd),
            0x7E => Some(O::MovdXmmStore),
            0x7F => Some(O::MovdqaStore),
            0xC2 => Some(O::Cmppd),
            0xC4 => Some(O::PinsrwXmm),
            0xC5 => Some(O::PextrwXmm),
            0xC6 => Some(O::Shufpd),
            0xD1 => Some(O::Psrlw),
            0xD2 => Some(O::Psrld),
            0xD3 => Some(O::Psrlq),
            0xD4 => Some(O::Paddq),
            0xD5 => Some(O::Pmullw),
            0xD6 => Some(O::MovqStore),
            0xD7 => Some(O::PmovmskbXmm),
            0xD8 => Some(O::Psubusb),
            0xD9 => Some(O::Psubusw),
            0xDA => Some(O::Pminub),
            0xDB => Some(O::Pand),
            0xDC => Some(O::Paddusb),
            0xDD => Some(O::Paddusw),
            0xDE => Some(O::Pmaxub),
            0xDF => Some(O::Pandn),
            0xE0 => Some(O::Pavgb),
            0xE1 => Some(O::Psraw),
            0xE2 => Some(O::Psrad),
            0xE3 => Some(O::Pavgw),
            0xE4 => Some(O::Pmulhuw),
            0xE5 => Some(O::Pmulhw),
            0xE6 => Some(O::Cvttpd2dq),
            0xE7 => Some(O::Movntdq),
            0xE8 => Some(O::Psubsb),
            0xE9 => Some(O::Psubsw),
            0xEA => Some(O::Pminsw),
            0xEB => Some(O::Por),
            0xEC => Some(O::Paddsb),
            0xED => Some(O::Paddsw),
            0xEE => Some(O::Pmaxsw),
            0xEF => Some(O::Pxor),
            0xF1 => Some(O::Psllw),
            0xF2 => Some(O::Pslld),
            0xF3 => Some(O::Psllq),
            0xF4 => Some(O::Pmuludq),
            0xF5 => Some(O::Pmaddwd),
            0xF6 => Some(O::Psadbw),
            0xF7 => Some(O::Maskmovdqu),
            0xF8 => Some(O::Psubb),
            0xF9 => Some(O::Psubw),
            0xFA => Some(O::Psubd),
            0xFB => Some(O::Psubq),
            0xFC => Some(O::Paddb),
            0xFD => Some(O::Paddw),
            0xFE => Some(O::Paddd),
            _ => None,
        },
        PrefixClass::Pf2 => match opcode {
            0x10 => Some(O::MovsdLoad),
            0x11 => Some(O::MovsdStore),
            0x2A => Some(O::Cvtsi2sd),
            0x2C => Some(O::Cvttsd2si),
            0x2D => Some(O::Cvtsd2si),
            0x51 => Some(O::Sqrtsd),
            0x58 => Some(O::Addsd),
            0x59 => Some(O::Mulsd),
            0x5A => Some(O::Cvtsd2ss),
            0x5C => Some(O::Subsd),
            0x5D => Some(O::Minsd),
            0x5E => Some(O::Divsd),
            0x5F => Some(O::Maxsd),
            0x70 => Some(O::Pshuflw),
            0xC2 => Some(O::Cmpsd),
            0xD6 => Some(O::Movdq2q),
            0xE6 => Some(O::Cvtpd2dq),
            _ => None,
        },
        PrefixClass::Pf3 => match opcode {
            0x10 => Some(O::MovssLoad),
            0x11 => Some(O::MovssStore),
            0x2A => Some(O::Cvtsi2ss),
            0x2C => Some(O::Cvttss2si),
            0x2D => Some(O::Cvtss2si),
            0x51 => Some(O::Sqrtss),
            0x52 => Some(O::Rsqrtss),
            0x53 => Some(O::Rcpss),
            0x58 => Some(O::Addss),
            0x59 => Some(O::Mulss),
            0x5A => Some(O::Cvtss2sd),
            0x5B => Some(O::Cvttps2dq),
            0x5C => Some(O::Subss),
            0x5D => Some(O::Minss),
            0x5E => Some(O::Divss),
            0x5F => Some(O::Maxss),
            0x6F => Some(O::MovdquLoad),
            0x70 => Some(O::Pshufhw),
            0x7E => Some(O::MovqXmmLoad),
            0x7F => Some(O::MovdquStore),
            0xC2 => Some(O::Cmpss),
            0xD6 => Some(O::Movq2dq),
            0xE6 => Some(O::Cvtdq2pd),
            _ => None,
        },
    }
}

/// Resolve one encoding, applying the ModR/M shape each form accepts.
///
/// [`op_for`] only disambiguates forms that share an opcode; the shape
/// constraint is stated once, in each form's descriptor.
fn resolve(class: PrefixClass, opcode: u8, modrm: u8) -> Option<SseOp> {
    let op = op_for(class, opcode, modrm)?;
    let is_reg_form = modrm >= 0xC0;
    match op.info().modrm {
        ModRmForm::Any => Some(op),
        ModRmForm::MemOnly if !is_reg_form => Some(op),
        ModRmForm::RegOnly if is_reg_form => Some(op),
        _ => None,
    }
}

// -----------------------------------------------------------------------
// Decoder
// -----------------------------------------------------------------------

/// Decode one legacy-mode SSE/SSE2 instruction from `bytes`.
///
/// `bitness` is the code segment's default size, 16 or 32.
#[must_use]
pub fn decode(bytes: &[u8], bitness: u8) -> SseDecode {
    debug_assert!(bitness == 16 || bitness == 32, "bitness must be 16 or 32");
    let default16 = bitness == 16;

    let mut pos = 0usize;
    let mut prefixes = Prefixes::default();
    let opcode_escape = loop {
        if pos >= MAX_INSTRUCTION_LENGTH {
            // A 15-byte prefix run leaves no room for an opcode.
            return SseDecode::NotOurs {
                consumed: MAX_INSTRUCTION_LENGTH as u8,
            };
        }
        let Some(&byte) = bytes.get(pos) else {
            // The shortest encoding we own is `0F` + opcode + ModR/M.
            return truncated_or_too_long(pos, pos + 1);
        };
        pos += 1;
        match byte {
            0x26 => prefixes.segment = Some(SseSegment::Es),
            0x2E => prefixes.segment = Some(SseSegment::Cs),
            0x36 => prefixes.segment = Some(SseSegment::Ss),
            0x3E => prefixes.segment = Some(SseSegment::Ds),
            0x64 => prefixes.segment = Some(SseSegment::Fs),
            0x65 => prefixes.segment = Some(SseSegment::Gs),
            0x66 => prefixes.operand_size = true,
            0x67 => prefixes.address_size = true,
            0xF0 => prefixes.lock = true,
            0xF2 => prefixes.repeat = Some(false),
            0xF3 => prefixes.repeat = Some(true),
            _ => break byte,
        }
    };

    if opcode_escape != 0x0F {
        return SseDecode::NotOurs {
            consumed: pos as u8,
        };
    }
    // A LOCK prefix makes every one of these encodings invalid, so the caller's
    // pre-dispatch LOCK validator owns the #UD and we never claim the bytes.
    if prefixes.lock {
        return SseDecode::NotOurs {
            consumed: pos as u8,
        };
    }

    let Some(&opcode) = bytes.get(pos) else {
        return truncated_or_too_long(pos, pos + 1);
    };
    pos += 1;

    let class = prefixes.class();
    if !candidate_opcode(class, opcode) {
        return SseDecode::NotOurs {
            consumed: pos as u8,
        };
    }

    let Some(&modrm) = bytes.get(pos) else {
        return truncated_or_too_long(pos, pos + 1);
    };
    pos += 1;

    let Some(op) = resolve(class, opcode, modrm) else {
        return SseDecode::NotOurs {
            consumed: pos as u8,
        };
    };
    let spec = op.info();
    let is_reg_form = modrm >= 0xC0;

    let addr16 = if prefixes.address_size {
        !default16
    } else {
        default16
    };

    let mut operands = SseOperands::default();
    let reg_field = (modrm >> 3) & 7;
    let rm_field = modrm & 7;

    // Address bytes come before the immediate.
    if !is_reg_form && !matches!(spec.form, Form::Maskmov(_)) {
        let memory = if addr16 {
            decode_memory16(bytes, &mut pos, modrm, prefixes.segment, spec.width)
        } else {
            decode_memory32(bytes, &mut pos, modrm, prefixes.segment, spec.width)
        };
        match memory {
            Ok(memory) => operands.memory = memory,
            Err(required) => return truncated_or_too_long(pos, required),
        }
    }

    match spec.form {
        Form::RegRm { dst, rm } => {
            operands.dst = reg_operand(dst, reg_field);
            operands.src = if is_reg_form {
                reg_operand(rm, rm_field)
            } else {
                SseOperand::Memory
            };
        }
        Form::RmReg { rm, src } => {
            operands.dst = if is_reg_form {
                reg_operand(rm, rm_field)
            } else {
                SseOperand::Memory
            };
            operands.src = reg_operand(src, reg_field);
        }
        Form::ImmShift(kind) => operands.dst = reg_operand(kind, rm_field),
        Form::Maskmov(kind) => {
            operands.dst = SseOperand::Memory;
            operands.src = reg_operand(kind, reg_field);
            operands.src2 = reg_operand(kind, rm_field);
            operands.memory = SseMemory {
                addr16,
                base: Some(if addr16 {
                    SseAddrReg::Di
                } else {
                    SseAddrReg::Edi
                }),
                index: None,
                scale: 1,
                disp: 0,
                default_segment: SseSegment::Ds,
                segment: prefixes.segment.unwrap_or(SseSegment::Ds),
                width: spec.width,
            };
        }
        Form::MemSrc => operands.src = SseOperand::Memory,
        Form::MemDst => operands.dst = SseOperand::Memory,
        Form::Nullary => {}
    }

    if spec.imm8 {
        let Some(&imm) = bytes.get(pos) else {
            return truncated_or_too_long(pos, pos + 1);
        };
        pos += 1;
        operands.imm8 = imm;
    }

    if pos > MAX_INSTRUCTION_LENGTH {
        return SseDecode::NotOurs {
            consumed: MAX_INSTRUCTION_LENGTH as u8,
        };
    }

    SseDecode::Decoded(SseInstr {
        op,
        len: pos as u8,
        operands,
    })
}

/// A truncation that can only complete past the 15-byte limit is not ours.
#[inline]
const fn truncated_or_too_long(pos: usize, required: usize) -> SseDecode {
    if required > MAX_INSTRUCTION_LENGTH {
        SseDecode::NotOurs {
            consumed: if pos > MAX_INSTRUCTION_LENGTH {
                MAX_INSTRUCTION_LENGTH as u8
            } else {
                pos as u8
            },
        }
    } else {
        SseDecode::Truncated { required }
    }
}

#[inline]
const fn reg_operand(kind: RegKind, index: u8) -> SseOperand {
    match kind {
        RegKind::Xmm => SseOperand::Xmm(index),
        RegKind::Mm => SseOperand::Mm(index),
        RegKind::Gpr => SseOperand::Gpr(index),
    }
}

const ADDR32: [SseAddrReg; 8] = [
    SseAddrReg::Eax,
    SseAddrReg::Ecx,
    SseAddrReg::Edx,
    SseAddrReg::Ebx,
    SseAddrReg::Esp,
    SseAddrReg::Ebp,
    SseAddrReg::Esi,
    SseAddrReg::Edi,
];

/// Read `count` bytes, or report the total length that would be needed.
#[inline]
fn take(bytes: &[u8], pos: &mut usize, count: usize) -> Result<u32, usize> {
    if bytes.len() < *pos + count {
        return Err(*pos + count);
    }
    let mut value = 0u32;
    for shift in 0..count {
        value |= u32::from(bytes[*pos + shift]) << (shift * 8);
    }
    *pos += count;
    Ok(value)
}

fn decode_memory16(
    bytes: &[u8],
    pos: &mut usize,
    modrm: u8,
    override_segment: Option<SseSegment>,
    width: u16,
) -> Result<SseMemory, usize> {
    use SseAddrReg as R;
    use SseSegment as S;

    let mode = modrm >> 6;
    let rm = modrm & 7;
    let (base, index, default_segment) = match rm {
        0 => (Some(R::Bx), Some(R::Si), S::Ds),
        1 => (Some(R::Bx), Some(R::Di), S::Ds),
        2 => (Some(R::Bp), Some(R::Si), S::Ss),
        3 => (Some(R::Bp), Some(R::Di), S::Ss),
        4 => (Some(R::Si), None, S::Ds),
        5 => (Some(R::Di), None, S::Ds),
        6 if mode == 0 => (None, None, S::Ds),
        6 => (Some(R::Bp), None, S::Ss),
        _ => (Some(R::Bx), None, S::Ds),
    };
    let disp = match mode {
        0 if rm == 6 => take(bytes, pos, 2)? as i32,
        0 => 0,
        1 => i32::from(take(bytes, pos, 1)? as u8 as i8),
        _ => i32::from(take(bytes, pos, 2)? as u16 as i16),
    };
    Ok(SseMemory {
        addr16: true,
        base,
        index,
        scale: 1,
        disp,
        default_segment,
        segment: override_segment.unwrap_or(default_segment),
        width,
    })
}

fn decode_memory32(
    bytes: &[u8],
    pos: &mut usize,
    modrm: u8,
    override_segment: Option<SseSegment>,
    width: u16,
) -> Result<SseMemory, usize> {
    use SseAddrReg as R;
    use SseSegment as S;

    let mode = modrm >> 6;
    let rm = modrm & 7;
    let (base, index, scale, default_segment, no_base) = if rm == 4 {
        let sib = take(bytes, pos, 1)? as u8;
        let index_code = (sib >> 3) & 7;
        let base_code = sib & 7;
        let no_base = mode == 0 && base_code == 5;
        let base = if no_base {
            None
        } else {
            Some(ADDR32[base_code as usize])
        };
        let index = if index_code == 4 {
            None
        } else {
            Some(ADDR32[index_code as usize])
        };
        // With no index register the encoded scale bits cannot affect the
        // address, so the descriptor normalises them to 1 -- the same choice
        // `x87_decode` makes, and what keeps two encodings of one address
        // compare equal. iced reports the raw bits instead.
        let scale = if index.is_none() { 1 } else { 1 << (sib >> 6) };
        let default_segment = match base {
            Some(R::Esp | R::Ebp) => S::Ss,
            _ => S::Ds,
        };
        (base, index, scale, default_segment, no_base)
    } else if mode == 0 && rm == 5 {
        (None, None, 1, S::Ds, true)
    } else {
        let base = ADDR32[rm as usize];
        let default_segment = if matches!(base, R::Ebp) { S::Ss } else { S::Ds };
        (Some(base), None, 1, default_segment, false)
    };
    let disp = match mode {
        0 if no_base => take(bytes, pos, 4)? as i32,
        0 => 0,
        1 => i32::from(take(bytes, pos, 1)? as u8 as i8),
        _ => take(bytes, pos, 4)? as i32,
    };
    Ok(SseMemory {
        addr16: false,
        base,
        index,
        scale,
        disp,
        default_segment,
        segment: override_segment.unwrap_or(default_segment),
        width,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use iced_x86::{Decoder, DecoderOptions, Instruction, OpKind, Register};
    use std::collections::BTreeSet;
    use std::time::Instant;

    /// The mandatory-prefix shapes the partition boundary has to reproduce,
    /// including both orders of every 66/F2 and 66/F3 pair.
    const PREFIX_SHAPES: [&[u8]; 8] = [
        &[],
        &[0x66],
        &[0xF2],
        &[0xF3],
        &[0x66, 0xF2],
        &[0x66, 0xF3],
        &[0xF2, 0x66],
        &[0xF3, 0x66],
    ];

    // ------------------------------------------------------------------
    // iced adapter: reduce both decoders to one comparable snapshot
    // ------------------------------------------------------------------

    #[derive(Debug, PartialEq, Eq)]
    struct MemSnapshot {
        base: Option<SseAddrReg>,
        index: Option<SseAddrReg>,
        scale: u8,
        disp: u32,
        segment: SseSegment,
        addr16: bool,
        width: u16,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Snapshot {
        operands: Vec<SseOperand>,
        imm8: Option<u8>,
        memory: Option<MemSnapshot>,
    }

    fn map_register(reg: Register) -> SseOperand {
        match reg {
            Register::XMM0 => SseOperand::Xmm(0),
            Register::XMM1 => SseOperand::Xmm(1),
            Register::XMM2 => SseOperand::Xmm(2),
            Register::XMM3 => SseOperand::Xmm(3),
            Register::XMM4 => SseOperand::Xmm(4),
            Register::XMM5 => SseOperand::Xmm(5),
            Register::XMM6 => SseOperand::Xmm(6),
            Register::XMM7 => SseOperand::Xmm(7),
            Register::MM0 => SseOperand::Mm(0),
            Register::MM1 => SseOperand::Mm(1),
            Register::MM2 => SseOperand::Mm(2),
            Register::MM3 => SseOperand::Mm(3),
            Register::MM4 => SseOperand::Mm(4),
            Register::MM5 => SseOperand::Mm(5),
            Register::MM6 => SseOperand::Mm(6),
            Register::MM7 => SseOperand::Mm(7),
            Register::EAX => SseOperand::Gpr(0),
            Register::ECX => SseOperand::Gpr(1),
            Register::EDX => SseOperand::Gpr(2),
            Register::EBX => SseOperand::Gpr(3),
            Register::ESP => SseOperand::Gpr(4),
            Register::EBP => SseOperand::Gpr(5),
            Register::ESI => SseOperand::Gpr(6),
            Register::EDI => SseOperand::Gpr(7),
            other => panic!("unexpected register {other:?}"),
        }
    }

    fn map_address_register(reg: Register) -> Option<SseAddrReg> {
        Some(match reg {
            Register::None => return None,
            Register::BX => SseAddrReg::Bx,
            Register::BP => SseAddrReg::Bp,
            Register::SI => SseAddrReg::Si,
            Register::DI => SseAddrReg::Di,
            Register::EAX => SseAddrReg::Eax,
            Register::ECX => SseAddrReg::Ecx,
            Register::EDX => SseAddrReg::Edx,
            Register::EBX => SseAddrReg::Ebx,
            Register::ESP => SseAddrReg::Esp,
            Register::EBP => SseAddrReg::Ebp,
            Register::ESI => SseAddrReg::Esi,
            Register::EDI => SseAddrReg::Edi,
            other => panic!("unexpected address register {other:?}"),
        })
    }

    fn map_segment(reg: Register) -> SseSegment {
        match reg {
            Register::ES => SseSegment::Es,
            Register::CS => SseSegment::Cs,
            Register::SS => SseSegment::Ss,
            Register::DS => SseSegment::Ds,
            Register::FS => SseSegment::Fs,
            Register::GS => SseSegment::Gs,
            other => panic!("unexpected segment {other:?}"),
        }
    }

    const fn mask_for(addr16: bool) -> u32 {
        if addr16 {
            0xFFFF
        } else {
            u32::MAX
        }
    }

    fn iced_snapshot(instr: &Instruction, addr16: bool) -> Snapshot {
        let mut operands = Vec::new();
        let mut imm8 = None;
        let mut memory = None;
        for index in 0..instr.op_count() {
            match instr.op_kind(index) {
                OpKind::Register => operands.push(map_register(instr.op_register(index))),
                OpKind::Immediate8 => imm8 = Some(instr.immediate8()),
                OpKind::Memory => {
                    operands.push(SseOperand::Memory);
                    memory = Some(MemSnapshot {
                        base: map_address_register(instr.memory_base()),
                        index: map_address_register(instr.memory_index()),
                        // iced keeps the encoded scale bits even with no index
                        // register; our descriptor normalises them (see
                        // `decode_memory32`), so normalise iced to compare.
                        scale: if instr.memory_index() == Register::None {
                            1
                        } else {
                            instr.memory_index_scale() as u8
                        },
                        disp: instr.memory_displacement32() & mask_for(addr16),
                        segment: map_segment(instr.memory_segment()),
                        addr16,
                        width: instr.memory_size().size() as u16,
                    });
                }
                // The implicit DS:rDI destination of MASKMOVQ/MASKMOVDQU.
                kind @ (OpKind::MemorySegDI | OpKind::MemorySegEDI) => {
                    operands.push(SseOperand::Memory);
                    memory = Some(MemSnapshot {
                        base: Some(if kind == OpKind::MemorySegDI {
                            SseAddrReg::Di
                        } else {
                            SseAddrReg::Edi
                        }),
                        index: None,
                        scale: 1,
                        disp: 0,
                        segment: map_segment(instr.memory_segment()),
                        addr16,
                        width: instr.memory_size().size() as u16,
                    });
                }
                other => panic!("unexpected operand kind {other:?} in {instr:?}"),
            }
        }
        Snapshot {
            operands,
            imm8,
            memory,
        }
    }

    fn our_snapshot(instr: &SseInstr) -> Snapshot {
        let mut operands = Vec::new();
        for operand in [instr.operands.dst, instr.operands.src, instr.operands.src2] {
            if operand != SseOperand::None {
                operands.push(operand);
            }
        }
        let memory = instr.operands.memory_operand().map(|memory| MemSnapshot {
            base: memory.base,
            index: memory.index,
            scale: memory.scale,
            disp: (memory.disp as u32) & mask_for(memory.addr16),
            segment: memory.segment,
            addr16: memory.addr16,
            width: memory.width,
        });
        Snapshot {
            operands,
            imm8: instr.op.has_imm8().then_some(instr.operands.imm8),
            memory,
        }
    }

    // ------------------------------------------------------------------
    // Canonical encodings: one per `SseOp` variant
    // ------------------------------------------------------------------

    const NP: &[u8] = &[];
    const P66: &[u8] = &[0x66];
    const PF2: &[u8] = &[0xF2];
    const PF3: &[u8] = &[0xF3];

    /// `(op, mandatory prefix, opcode, ModR/M)`. ModR/M `0xC1` is the register
    /// form `reg=0, rm=1`; `0x01` is `[ECX]` in 32-bit code and `[BX+DI]` in
    /// 16-bit code, neither of which needs a SIB or displacement byte.
    ///
    /// `pub(crate)` so the executor's migration differential can drive the
    /// same corpus (HLD §3.6.2).
    #[rustfmt::skip]
    pub(crate) const CANONICAL: &[(SseOp, &[u8], u8, u8)] = &[
        (SseOp::MovupsLoad,   NP,  0x10, 0xC1),
        (SseOp::MovupsStore,  NP,  0x11, 0xC1),
        (SseOp::MovlpsLoad,   NP,  0x12, 0x01),
        (SseOp::Movhlps,      NP,  0x12, 0xC1),
        (SseOp::MovlpsStore,  NP,  0x13, 0x01),
        (SseOp::Unpcklps,     NP,  0x14, 0xC1),
        (SseOp::Unpckhps,     NP,  0x15, 0xC1),
        (SseOp::MovhpsLoad,   NP,  0x16, 0x01),
        (SseOp::Movlhps,      NP,  0x16, 0xC1),
        (SseOp::MovhpsStore,  NP,  0x17, 0x01),
        (SseOp::Prefetchnta,  NP,  0x18, 0x01),
        (SseOp::Prefetcht0,   NP,  0x18, 0x09),
        (SseOp::Prefetcht1,   NP,  0x18, 0x11),
        (SseOp::Prefetcht2,   NP,  0x18, 0x19),
        (SseOp::MovapsLoad,   NP,  0x28, 0xC1),
        (SseOp::MovapsStore,  NP,  0x29, 0xC1),
        (SseOp::Cvtpi2ps,     NP,  0x2A, 0xC1),
        (SseOp::Movntps,      NP,  0x2B, 0x01),
        (SseOp::Cvttps2pi,    NP,  0x2C, 0xC1),
        (SseOp::Cvtps2pi,     NP,  0x2D, 0xC1),
        (SseOp::Ucomiss,      NP,  0x2E, 0xC1),
        (SseOp::Comiss,       NP,  0x2F, 0xC1),
        (SseOp::Movmskps,     NP,  0x50, 0xC1),
        (SseOp::Sqrtps,       NP,  0x51, 0xC1),
        (SseOp::Rsqrtps,      NP,  0x52, 0xC1),
        (SseOp::Rcpps,        NP,  0x53, 0xC1),
        (SseOp::Andps,        NP,  0x54, 0xC1),
        (SseOp::Andnps,       NP,  0x55, 0xC1),
        (SseOp::Orps,         NP,  0x56, 0xC1),
        (SseOp::Xorps,        NP,  0x57, 0xC1),
        (SseOp::Addps,        NP,  0x58, 0xC1),
        (SseOp::Mulps,        NP,  0x59, 0xC1),
        (SseOp::Cvtps2pd,     NP,  0x5A, 0xC1),
        (SseOp::Cvtdq2ps,     NP,  0x5B, 0xC1),
        (SseOp::Subps,        NP,  0x5C, 0xC1),
        (SseOp::Minps,        NP,  0x5D, 0xC1),
        (SseOp::Divps,        NP,  0x5E, 0xC1),
        (SseOp::Maxps,        NP,  0x5F, 0xC1),
        (SseOp::Pshufw,       NP,  0x70, 0xC1),
        (SseOp::Fxsave,       NP,  0xAE, 0x01),
        (SseOp::Fxrstor,      NP,  0xAE, 0x09),
        (SseOp::Ldmxcsr,      NP,  0xAE, 0x11),
        (SseOp::Stmxcsr,      NP,  0xAE, 0x19),
        (SseOp::Lfence,       NP,  0xAE, 0xE9),
        (SseOp::Mfence,       NP,  0xAE, 0xF1),
        (SseOp::Sfence,       NP,  0xAE, 0xF9),
        (SseOp::Cmpps,        NP,  0xC2, 0xC1),
        (SseOp::Movnti,       NP,  0xC3, 0x01),
        (SseOp::PinsrwMm,     NP,  0xC4, 0xC1),
        (SseOp::PextrwMm,     NP,  0xC5, 0xC1),
        (SseOp::Shufps,       NP,  0xC6, 0xC1),
        (SseOp::PmovmskbMm,   NP,  0xD7, 0xC1),
        (SseOp::PminubMm,     NP,  0xDA, 0xC1),
        (SseOp::PmaxubMm,     NP,  0xDE, 0xC1),
        (SseOp::PavgbMm,      NP,  0xE0, 0xC1),
        (SseOp::PavgwMm,      NP,  0xE3, 0xC1),
        (SseOp::PmulhuwMm,    NP,  0xE4, 0xC1),
        (SseOp::Movntq,       NP,  0xE7, 0x01),
        (SseOp::PminswMm,     NP,  0xEA, 0xC1),
        (SseOp::PmaxswMm,     NP,  0xEE, 0xC1),
        (SseOp::PmuludqMm,    NP,  0xF4, 0xC1),
        (SseOp::PsadbwMm,     NP,  0xF6, 0xC1),
        (SseOp::Maskmovq,     NP,  0xF7, 0xC1),
        (SseOp::PsubqMm,      NP,  0xFB, 0xC1),

        (SseOp::MovupdLoad,   P66, 0x10, 0xC1),
        (SseOp::MovupdStore,  P66, 0x11, 0xC1),
        (SseOp::MovlpdLoad,   P66, 0x12, 0x01),
        (SseOp::MovlpdStore,  P66, 0x13, 0x01),
        (SseOp::Unpcklpd,     P66, 0x14, 0xC1),
        (SseOp::Unpckhpd,     P66, 0x15, 0xC1),
        (SseOp::MovhpdLoad,   P66, 0x16, 0x01),
        (SseOp::MovhpdStore,  P66, 0x17, 0x01),
        (SseOp::MovapdLoad,   P66, 0x28, 0xC1),
        (SseOp::MovapdStore,  P66, 0x29, 0xC1),
        (SseOp::Cvtpi2pd,     P66, 0x2A, 0xC1),
        (SseOp::Movntpd,      P66, 0x2B, 0x01),
        (SseOp::Cvttpd2pi,    P66, 0x2C, 0xC1),
        (SseOp::Cvtpd2pi,     P66, 0x2D, 0xC1),
        (SseOp::Ucomisd,      P66, 0x2E, 0xC1),
        (SseOp::Comisd,       P66, 0x2F, 0xC1),
        (SseOp::Movmskpd,     P66, 0x50, 0xC1),
        (SseOp::Sqrtpd,       P66, 0x51, 0xC1),
        (SseOp::Andpd,        P66, 0x54, 0xC1),
        (SseOp::Andnpd,       P66, 0x55, 0xC1),
        (SseOp::Orpd,         P66, 0x56, 0xC1),
        (SseOp::Xorpd,        P66, 0x57, 0xC1),
        (SseOp::Addpd,        P66, 0x58, 0xC1),
        (SseOp::Mulpd,        P66, 0x59, 0xC1),
        (SseOp::Cvtpd2ps,     P66, 0x5A, 0xC1),
        (SseOp::Cvtps2dq,     P66, 0x5B, 0xC1),
        (SseOp::Subpd,        P66, 0x5C, 0xC1),
        (SseOp::Minpd,        P66, 0x5D, 0xC1),
        (SseOp::Divpd,        P66, 0x5E, 0xC1),
        (SseOp::Maxpd,        P66, 0x5F, 0xC1),
        (SseOp::Punpcklbw,    P66, 0x60, 0xC1),
        (SseOp::Punpcklwd,    P66, 0x61, 0xC1),
        (SseOp::Punpckldq,    P66, 0x62, 0xC1),
        (SseOp::Packsswb,     P66, 0x63, 0xC1),
        (SseOp::Pcmpgtb,      P66, 0x64, 0xC1),
        (SseOp::Pcmpgtw,      P66, 0x65, 0xC1),
        (SseOp::Pcmpgtd,      P66, 0x66, 0xC1),
        (SseOp::Packuswb,     P66, 0x67, 0xC1),
        (SseOp::Punpckhbw,    P66, 0x68, 0xC1),
        (SseOp::Punpckhwd,    P66, 0x69, 0xC1),
        (SseOp::Punpckhdq,    P66, 0x6A, 0xC1),
        (SseOp::Packssdw,     P66, 0x6B, 0xC1),
        (SseOp::Punpcklqdq,   P66, 0x6C, 0xC1),
        (SseOp::Punpckhqdq,   P66, 0x6D, 0xC1),
        (SseOp::MovdXmmLoad,  P66, 0x6E, 0xC1),
        (SseOp::MovdqaLoad,   P66, 0x6F, 0xC1),
        (SseOp::Pshufd,       P66, 0x70, 0xC1),
        (SseOp::PsrlwImm,     P66, 0x71, 0xD1),
        (SseOp::PsrawImm,     P66, 0x71, 0xE1),
        (SseOp::PsllwImm,     P66, 0x71, 0xF1),
        (SseOp::PsrldImm,     P66, 0x72, 0xD1),
        (SseOp::PsradImm,     P66, 0x72, 0xE1),
        (SseOp::PslldImm,     P66, 0x72, 0xF1),
        (SseOp::PsrlqImm,     P66, 0x73, 0xD1),
        (SseOp::PsrldqImm,    P66, 0x73, 0xD9),
        (SseOp::PsllqImm,     P66, 0x73, 0xF1),
        (SseOp::PslldqImm,    P66, 0x73, 0xF9),
        (SseOp::Pcmpeqb,      P66, 0x74, 0xC1),
        (SseOp::Pcmpeqw,      P66, 0x75, 0xC1),
        (SseOp::Pcmpeqd,      P66, 0x76, 0xC1),
        (SseOp::MovdXmmStore, P66, 0x7E, 0xC1),
        (SseOp::MovdqaStore,  P66, 0x7F, 0xC1),
        (SseOp::Cmppd,        P66, 0xC2, 0xC1),
        (SseOp::PinsrwXmm,    P66, 0xC4, 0xC1),
        (SseOp::PextrwXmm,    P66, 0xC5, 0xC1),
        (SseOp::Shufpd,       P66, 0xC6, 0xC1),
        (SseOp::Psrlw,        P66, 0xD1, 0xC1),
        (SseOp::Psrld,        P66, 0xD2, 0xC1),
        (SseOp::Psrlq,        P66, 0xD3, 0xC1),
        (SseOp::Paddq,        P66, 0xD4, 0xC1),
        (SseOp::Pmullw,       P66, 0xD5, 0xC1),
        (SseOp::MovqStore,    P66, 0xD6, 0xC1),
        (SseOp::PmovmskbXmm,  P66, 0xD7, 0xC1),
        (SseOp::Psubusb,      P66, 0xD8, 0xC1),
        (SseOp::Psubusw,      P66, 0xD9, 0xC1),
        (SseOp::Pminub,       P66, 0xDA, 0xC1),
        (SseOp::Pand,         P66, 0xDB, 0xC1),
        (SseOp::Paddusb,      P66, 0xDC, 0xC1),
        (SseOp::Paddusw,      P66, 0xDD, 0xC1),
        (SseOp::Pmaxub,       P66, 0xDE, 0xC1),
        (SseOp::Pandn,        P66, 0xDF, 0xC1),
        (SseOp::Pavgb,        P66, 0xE0, 0xC1),
        (SseOp::Psraw,        P66, 0xE1, 0xC1),
        (SseOp::Psrad,        P66, 0xE2, 0xC1),
        (SseOp::Pavgw,        P66, 0xE3, 0xC1),
        (SseOp::Pmulhuw,      P66, 0xE4, 0xC1),
        (SseOp::Pmulhw,       P66, 0xE5, 0xC1),
        (SseOp::Cvttpd2dq,    P66, 0xE6, 0xC1),
        (SseOp::Movntdq,      P66, 0xE7, 0x01),
        (SseOp::Psubsb,       P66, 0xE8, 0xC1),
        (SseOp::Psubsw,       P66, 0xE9, 0xC1),
        (SseOp::Pminsw,       P66, 0xEA, 0xC1),
        (SseOp::Por,          P66, 0xEB, 0xC1),
        (SseOp::Paddsb,       P66, 0xEC, 0xC1),
        (SseOp::Paddsw,       P66, 0xED, 0xC1),
        (SseOp::Pmaxsw,       P66, 0xEE, 0xC1),
        (SseOp::Pxor,         P66, 0xEF, 0xC1),
        (SseOp::Psllw,        P66, 0xF1, 0xC1),
        (SseOp::Pslld,        P66, 0xF2, 0xC1),
        (SseOp::Psllq,        P66, 0xF3, 0xC1),
        (SseOp::Pmuludq,      P66, 0xF4, 0xC1),
        (SseOp::Pmaddwd,      P66, 0xF5, 0xC1),
        (SseOp::Psadbw,       P66, 0xF6, 0xC1),
        (SseOp::Maskmovdqu,   P66, 0xF7, 0xC1),
        (SseOp::Psubb,        P66, 0xF8, 0xC1),
        (SseOp::Psubw,        P66, 0xF9, 0xC1),
        (SseOp::Psubd,        P66, 0xFA, 0xC1),
        (SseOp::Psubq,        P66, 0xFB, 0xC1),
        (SseOp::Paddb,        P66, 0xFC, 0xC1),
        (SseOp::Paddw,        P66, 0xFD, 0xC1),
        (SseOp::Paddd,        P66, 0xFE, 0xC1),

        (SseOp::MovsdLoad,    PF2, 0x10, 0xC1),
        (SseOp::MovsdStore,   PF2, 0x11, 0xC1),
        (SseOp::Cvtsi2sd,     PF2, 0x2A, 0xC1),
        (SseOp::Cvttsd2si,    PF2, 0x2C, 0xC1),
        (SseOp::Cvtsd2si,     PF2, 0x2D, 0xC1),
        (SseOp::Sqrtsd,       PF2, 0x51, 0xC1),
        (SseOp::Addsd,        PF2, 0x58, 0xC1),
        (SseOp::Mulsd,        PF2, 0x59, 0xC1),
        (SseOp::Cvtsd2ss,     PF2, 0x5A, 0xC1),
        (SseOp::Subsd,        PF2, 0x5C, 0xC1),
        (SseOp::Minsd,        PF2, 0x5D, 0xC1),
        (SseOp::Divsd,        PF2, 0x5E, 0xC1),
        (SseOp::Maxsd,        PF2, 0x5F, 0xC1),
        (SseOp::Pshuflw,      PF2, 0x70, 0xC1),
        (SseOp::Cmpsd,        PF2, 0xC2, 0xC1),
        (SseOp::Movdq2q,      PF2, 0xD6, 0xC1),
        (SseOp::Cvtpd2dq,     PF2, 0xE6, 0xC1),

        (SseOp::MovssLoad,    PF3, 0x10, 0xC1),
        (SseOp::MovssStore,   PF3, 0x11, 0xC1),
        (SseOp::Cvtsi2ss,     PF3, 0x2A, 0xC1),
        (SseOp::Cvttss2si,    PF3, 0x2C, 0xC1),
        (SseOp::Cvtss2si,     PF3, 0x2D, 0xC1),
        (SseOp::Sqrtss,       PF3, 0x51, 0xC1),
        (SseOp::Rsqrtss,      PF3, 0x52, 0xC1),
        (SseOp::Rcpss,        PF3, 0x53, 0xC1),
        (SseOp::Addss,        PF3, 0x58, 0xC1),
        (SseOp::Mulss,        PF3, 0x59, 0xC1),
        (SseOp::Cvtss2sd,     PF3, 0x5A, 0xC1),
        (SseOp::Cvttps2dq,    PF3, 0x5B, 0xC1),
        (SseOp::Subss,        PF3, 0x5C, 0xC1),
        (SseOp::Minss,        PF3, 0x5D, 0xC1),
        (SseOp::Divss,        PF3, 0x5E, 0xC1),
        (SseOp::Maxss,        PF3, 0x5F, 0xC1),
        (SseOp::MovdquLoad,   PF3, 0x6F, 0xC1),
        (SseOp::Pshufhw,      PF3, 0x70, 0xC1),
        (SseOp::MovqXmmLoad,  PF3, 0x7E, 0xC1),
        (SseOp::MovdquStore,  PF3, 0x7F, 0xC1),
        (SseOp::Cmpss,        PF3, 0xC2, 0xC1),
        (SseOp::Movq2dq,      PF3, 0xD6, 0xC1),
        (SseOp::Cvtdq2pd,     PF3, 0xE6, 0xC1),
    ];

    /// The shortest byte sequence that decodes to `op`.
    pub(crate) fn canonical_encoding(op: SseOp) -> Vec<u8> {
        let (_, prefix, opcode, modrm) = *CANONICAL
            .iter()
            .find(|(candidate, ..)| *candidate == op)
            .unwrap_or_else(|| panic!("no canonical encoding for {op:?}"));
        let mut bytes = prefix.to_vec();
        bytes.extend_from_slice(&[0x0F, opcode, modrm]);
        if op.has_imm8() {
            bytes.push(0x5A);
        }
        bytes
    }

    fn decoded(bytes: &[u8], bitness: u8) -> SseInstr {
        match decode(bytes, bitness) {
            SseDecode::Decoded(instr) => instr,
            other => panic!("expected Decoded for {bytes:02X?} @{bitness}, got {other:?}"),
        }
    }

    // ------------------------------------------------------------------
    // Structural properties
    // ------------------------------------------------------------------

    #[test]
    fn decoded_value_stays_small_enough_for_the_hot_cache() {
        assert!(std::mem::size_of::<SseInstr>() <= 32);
    }

    #[test]
    fn candidate_opcodes_agree_with_the_op_table() {
        for class in [
            PrefixClass::None,
            PrefixClass::P66,
            PrefixClass::Pf2,
            PrefixClass::Pf3,
        ] {
            for opcode in 0u16..=0xFF {
                let opcode = opcode as u8;
                let reachable =
                    (0u16..=0xFF).any(|modrm| resolve(class, opcode, modrm as u8).is_some());
                assert_eq!(
                    candidate_opcode(class, opcode),
                    reachable,
                    "{class:?} opcode {opcode:02X}"
                );
            }
        }
    }

    /// Every declared variant is reachable, and the canonical table names each
    /// exactly once -- so a later chunk's dispatch sweep covers the whole enum.
    #[test]
    fn canonical_encodings_cover_every_variant_exactly_once() {
        let mut reachable = BTreeSet::new();
        for class in [
            PrefixClass::None,
            PrefixClass::P66,
            PrefixClass::Pf2,
            PrefixClass::Pf3,
        ] {
            for opcode in 0u16..=0xFF {
                for modrm in 0u16..=0xFF {
                    if let Some(op) = resolve(class, opcode as u8, modrm as u8) {
                        reachable.insert(op);
                    }
                }
            }
        }
        assert_eq!(
            reachable.len(),
            SseOp::COUNT,
            "unreachable variants declared in SseOp"
        );

        let listed: BTreeSet<SseOp> = CANONICAL.iter().map(|(op, ..)| *op).collect();
        assert_eq!(listed.len(), CANONICAL.len(), "duplicate canonical entry");
        assert_eq!(listed, reachable);

        for (op, ..) in CANONICAL {
            for bitness in [16u8, 32] {
                let bytes = canonical_encoding(*op);
                let instr = decoded(&bytes, bitness);
                assert_eq!(instr.op, *op, "{bytes:02X?} @{bitness}");
                assert_eq!(usize::from(instr.len), bytes.len(), "{op:?} @{bitness}");
            }
        }
    }

    /// `SseOp::from_code` transmutes, so its premise — contiguous
    /// discriminants `0..COUNT` — is proved here over every variant rather
    /// than assumed from the declaration.
    #[test]
    fn every_form_round_trips_through_its_discriminant() {
        for (op, ..) in CANONICAL {
            let code = *op as u16;
            assert!(usize::from(code) < SseOp::COUNT, "{op:?} discriminant");
            assert_eq!(SseOp::from_code(code), Some(*op));
        }
        assert_eq!(
            SseOp::from_code(u16::try_from(SseOp::COUNT).expect("COUNT fits u16")),
            None,
            "one past the last variant is not a form"
        );
    }

    #[test]
    fn form_names_are_unique() {
        let mut names = BTreeSet::new();
        for (op, ..) in CANONICAL {
            assert!(names.insert(op.name()), "duplicate name {}", op.name());
        }
    }

    #[test]
    fn only_the_four_compare_forms_write_eflags() {
        let updating: Vec<&str> = CANONICAL
            .iter()
            .filter(|(op, ..)| op.updates_eflags())
            .map(|(op, ..)| op.name())
            .collect();
        assert_eq!(updating, ["ucomiss", "comiss", "ucomisd", "comisd"]);
    }

    /// `reads_destination` has no external oracle, so the classification is
    /// pinned by hand where it is easy to get wrong: a form that overwrites
    /// its whole destination must not claim to read it, and every merging or
    /// read-modify-write form must.
    #[test]
    fn the_merge_classification_separates_overwrites_from_merges() {
        for op in [
            // Whole-destination writes.
            SseOp::MovapsLoad,
            SseOp::MovdqaStore,
            SseOp::Movntdq,
            SseOp::Movmskps,
            SseOp::PextrwXmm,
            SseOp::Pshufd,
            // PSHUFLW/PSHUFHW take the untouched half from the source, not
            // from the destination.
            SseOp::Pshuflw,
            SseOp::Pshufhw,
            // MOVQ xmm/m64, xmm zeroes the upper quadword of a register
            // destination; MOVQ2DQ zero-extends into the whole register.
            SseOp::MovqStore,
            SseOp::Movq2dq,
            SseOp::Movdq2q,
            SseOp::Cvtpd2ps,
            SseOp::Cvtpi2pd,
            SseOp::Sqrtps,
            SseOp::Lfence,
            SseOp::Prefetcht0,
        ] {
            assert!(!op.reads_destination(), "{} overwrites", op.name());
        }
        for op in [
            // True read-modify-writes.
            SseOp::Addps,
            SseOp::Pxor,
            SseOp::PsllwImm,
            SseOp::Shufps,
            SseOp::Unpcklps,
            // Scalar forms merge into the untouched lanes.
            SseOp::Addss,
            SseOp::Sqrtsd,
            SseOp::Cvtsi2ss,
            SseOp::Cvtss2sd,
            // CVTPI2PS writes only the low quadword.
            SseOp::Cvtpi2ps,
            SseOp::MovlpsLoad,
            SseOp::Movhlps,
            SseOp::PinsrwXmm,
            // MOVSS/MOVSD merge whenever the other operand is a register,
            // in either direction.
            SseOp::MovssLoad,
            SseOp::MovssStore,
            SseOp::MovsdLoad,
            SseOp::MovsdStore,
            // Byte-masked stores read the memory they only partly overwrite.
            SseOp::Maskmovq,
            SseOp::Maskmovdqu,
        ] {
            assert!(op.reads_destination(), "{} merges", op.name());
        }
    }

    #[test]
    fn osfxsr_requirement_is_classified_per_register_file() {
        for op in [
            SseOp::Pshufw,
            SseOp::PinsrwMm,
            SseOp::PextrwMm,
            SseOp::PmovmskbMm,
            SseOp::PminubMm,
            SseOp::PmaxubMm,
            SseOp::PavgbMm,
            SseOp::PavgwMm,
            SseOp::PmulhuwMm,
            SseOp::Movntq,
            SseOp::PminswMm,
            SseOp::PmaxswMm,
            SseOp::PmuludqMm,
            SseOp::PsadbwMm,
            SseOp::Maskmovq,
            SseOp::PsubqMm,
            SseOp::Movnti,
            SseOp::Lfence,
            SseOp::Mfence,
            SseOp::Sfence,
            SseOp::Prefetchnta,
            SseOp::Prefetcht0,
            SseOp::Prefetcht1,
            SseOp::Prefetcht2,
        ] {
            assert!(
                !op.requires_osfxsr(),
                "{} does not touch XMM state",
                op.name()
            );
        }
        for op in [
            SseOp::Cvtpi2ps,
            SseOp::Cvttps2pi,
            SseOp::Cvtps2pi,
            SseOp::Xorps,
            SseOp::Ldmxcsr,
            SseOp::Stmxcsr,
        ] {
            assert!(
                op.requires_osfxsr(),
                "{} touches OS-managed SSE state",
                op.name()
            );
        }
    }

    // ------------------------------------------------------------------
    // Exhaustive differential against iced
    // ------------------------------------------------------------------

    /// One case: our result must match iced's admission decision and, where
    /// admitted, iced's length, level, operands, and memory descriptor.
    fn compare_case(bytes: &[u8; 15], iced: &Instruction, bitness: u8, addr16: bool, label: &str) {
        let expected_ours = !iced.is_invalid() && crate::sse_exec_reference::level(iced).is_some();
        match decode(bytes, bitness) {
            SseDecode::Decoded(instr) => {
                assert!(
                    expected_ours,
                    "{label}: we admitted {instr:?}, iced invalid={}",
                    iced.is_invalid()
                );
                assert_eq!(usize::from(instr.len), iced.len(), "{label}: length");
                // Our per-form names carry a `.suffix` where one mnemonic has
                // several forms; the stem is iced's mnemonic.
                assert_eq!(
                    instr.op.name().split('.').next().expect("name stem"),
                    format!("{:?}", iced.mnemonic()).to_lowercase(),
                    "{label}: mnemonic"
                );
                assert_eq!(
                    Some(instr.op.level()),
                    crate::sse_exec_reference::level(iced),
                    "{label}: level"
                );
                assert_eq!(
                    our_snapshot(&instr),
                    iced_snapshot(iced, addr16),
                    "{label}: operands ({:?} vs {:?})",
                    instr.op,
                    iced.code()
                );
            }
            SseDecode::NotOurs { .. } => assert!(
                !expected_ours,
                "{label}: we rejected what iced admits as {:?}",
                iced.code()
            ),
            SseDecode::Truncated { .. } => {
                panic!("{label}: a full 15-byte window must never report truncation")
            }
        }
    }

    /// Sweep every mandatory-prefix shape x opcode x ModR/M x bitness at each
    /// supplied tail pattern. This pins the partition boundary itself, which
    /// cannot be sampled: one wrong cell is a silently mis-executed
    /// instruction, or a silent guest #UD.
    fn differential_sweep(tails: &[u8]) -> usize {
        let mut cases = 0usize;
        let mut window = [0u8; 15];
        let mut buffer = vec![0u8; 256 * 15];
        for &tail in tails {
            for bitness in [16u8, 32] {
                for prefix in PREFIX_SHAPES {
                    for opcode in 0u16..=0xFF {
                        // One buffer per opcode holds all 256 ModR/M cases at a
                        // 15-byte stride, so the iced Decoder is built once.
                        buffer.fill(tail);
                        for modrm in 0..256usize {
                            let mut at = modrm * 15;
                            for &byte in prefix {
                                buffer[at] = byte;
                                at += 1;
                            }
                            buffer[at] = 0x0F;
                            buffer[at + 1] = opcode as u8;
                            buffer[at + 2] = modrm as u8;
                        }
                        let mut decoder =
                            Decoder::new(u32::from(bitness), &buffer, DecoderOptions::NONE);
                        for modrm in 0..256usize {
                            let base = modrm * 15;
                            decoder.set_position(base).expect("in-range position");
                            let iced = decoder.decode();
                            window.copy_from_slice(&buffer[base..base + 15]);
                            let label = format!(
                                "tail {tail:02X} b{bitness} prefix {prefix:02X?} 0F {opcode:02X} \
                                 modrm {modrm:02X}"
                            );
                            compare_case(&window, &iced, bitness, bitness == 16, &label);
                            cases += 1;
                        }
                    }
                }
            }
        }
        cases
    }

    #[test]
    fn gate_decode_differential_matches_iced() {
        let started = Instant::now();
        // Two tails, not one: 0x11 gives a SIB with an ECX base, and 0x25 a
        // SIB with an EBP base (SS by default) or, at mod=0, the no-base
        // disp32 form. A single tail leaves the SIB segment default untested
        // -- it survived the mutation round.
        let cases = differential_sweep(&[0x11, 0x25]);
        eprintln!(
            "sse_decode gate differential: {cases} cases in {:?}",
            started.elapsed()
        );
    }

    #[test]
    #[ignore = "exhaustive SIB/disp sweep; full_test (--include-ignored)"]
    fn nightly_decode_differential_sweeps_sib_and_displacement_shapes() {
        // 0x00 and 0xFF pin the signed-displacement edges; 0xE5, 0x6D and
        // 0xAC give scaled-index SIB shapes the gate tails do not reach.
        let started = Instant::now();
        let cases = differential_sweep(&[0x00, 0xFF, 0xE5, 0x6D, 0xAC]);
        eprintln!(
            "sse_decode nightly differential: {cases} cases in {:?}",
            started.elapsed()
        );
    }

    // ------------------------------------------------------------------
    // Hand-enumerated cases
    // ------------------------------------------------------------------

    #[test]
    fn repeat_prefixes_outrank_the_operand_size_prefix_and_the_last_one_wins() {
        // Verified against iced: F2/F3 beat 66 in either order, and the last
        // F2-or-F3 selects the mandatory-prefix table.
        for (bytes, expected) in [
            (vec![0x66, 0xF3, 0x0F, 0x10, 0xC1], SseOp::MovssLoad),
            (vec![0xF3, 0x66, 0x0F, 0x10, 0xC1], SseOp::MovssLoad),
            (vec![0x66, 0xF2, 0x0F, 0x10, 0xC1], SseOp::MovsdLoad),
            (vec![0xF2, 0x66, 0x0F, 0x10, 0xC1], SseOp::MovsdLoad),
            (vec![0xF2, 0xF3, 0x0F, 0x10, 0xC1], SseOp::MovssLoad),
            (vec![0xF3, 0xF2, 0x0F, 0x10, 0xC1], SseOp::MovsdLoad),
            (vec![0xF2, 0xF3, 0xF2, 0x0F, 0x10, 0xC1], SseOp::MovsdLoad),
            (vec![0x66, 0x0F, 0x10, 0xC1], SseOp::MovupdLoad),
            (vec![0x0F, 0x10, 0xC1], SseOp::MovupsLoad),
        ] {
            let instr = decoded(&bytes, 32);
            assert_eq!(instr.op, expected, "{bytes:02X?}");
            assert_eq!(usize::from(instr.len), bytes.len());
        }
    }

    #[test]
    fn redundant_prefixes_are_idempotent_but_still_counted_in_the_length() {
        let instr = decoded(&[0x66, 0x66, 0x0F, 0x58, 0xC1], 32);
        assert_eq!(instr.op, SseOp::Addpd);
        assert_eq!(instr.len, 5);

        let instr = decoded(&[0xF3, 0xF3, 0x0F, 0x58, 0xC1], 32);
        assert_eq!(instr.op, SseOp::Addss);
        assert_eq!(instr.len, 5);
    }

    #[test]
    fn address_size_override_flips_the_addressing_form_in_both_bitnesses() {
        // 32-bit code plus 67 gives 16-bit addressing: ModR/M 0x01 is [BX+DI].
        let instr = decoded(&[0x67, 0x0F, 0x10, 0x01], 32);
        let memory = instr.operands.memory_operand().expect("memory operand");
        assert!(memory.addr16);
        assert_eq!(memory.base, Some(SseAddrReg::Bx));
        assert_eq!(memory.index, Some(SseAddrReg::Di));

        // 16-bit code plus 67 gives 32-bit addressing: ModR/M 0x01 is [ECX].
        let instr = decoded(&[0x67, 0x0F, 0x10, 0x01], 16);
        let memory = instr.operands.memory_operand().expect("memory operand");
        assert!(!memory.addr16);
        assert_eq!(memory.base, Some(SseAddrReg::Ecx));
        assert_eq!(memory.index, None);

        // Without it, each bitness keeps its default.
        assert!(
            decoded(&[0x0F, 0x10, 0x01], 16)
                .operands
                .memory_operand()
                .expect("memory operand")
                .addr16
        );
        assert!(
            !decoded(&[0x0F, 0x10, 0x01], 32)
                .operands
                .memory_operand()
                .expect("memory operand")
                .addr16
        );
    }

    #[test]
    fn segment_overrides_combine_with_mandatory_prefixes_and_the_last_one_wins() {
        // An FS override before the mandatory F3, then GS after it.
        let instr = decoded(&[0x64, 0xF3, 0x65, 0x0F, 0x10, 0x01], 32);
        assert_eq!(instr.op, SseOp::MovssLoad);
        let memory = instr.operands.memory_operand().expect("memory operand");
        assert_eq!(memory.segment, SseSegment::Gs);
        assert_eq!(memory.default_segment, SseSegment::Ds);

        // An override reaches MASKMOVDQU's implicit DS:rDI destination too.
        let instr = decoded(&[0x36, 0x66, 0x0F, 0xF7, 0xC1], 32);
        assert_eq!(instr.op, SseOp::Maskmovdqu);
        let memory = instr.operands.memory_operand().expect("memory operand");
        assert_eq!(memory.segment, SseSegment::Ss);
        assert_eq!(memory.base, Some(SseAddrReg::Edi));
        assert_eq!(memory.width, 16);

        // 67 switches that implicit destination to DI.
        let instr = decoded(&[0x67, 0x0F, 0xF7, 0xC1], 32);
        assert_eq!(instr.op, SseOp::Maskmovq);
        let memory = instr.operands.memory_operand().expect("memory operand");
        assert_eq!(memory.base, Some(SseAddrReg::Di));
        assert_eq!(memory.width, 8);
    }

    /// The encoded scale bits are dead when the SIB names no index register,
    /// so the descriptor normalises them rather than carrying two unequal
    /// spellings of one address.
    #[test]
    fn an_indexless_sib_normalises_its_scale() {
        // SIB 0xE5: scale bits 3, index 4 (none), base 5 -> [disp32].
        let instr = decoded(&[0x0F, 0x10, 0x04, 0xE5, 0x78, 0x56, 0x34, 0x12], 32);
        let memory = instr.operands.memory_operand().expect("memory operand");
        assert_eq!((memory.base, memory.index, memory.scale), (None, None, 1));
        assert_eq!(memory.disp, 0x1234_5678);

        // SIB 0xE3: same scale bits, index 4 (none), base 3 -> [EBX].
        let instr = decoded(&[0x0F, 0x10, 0x04, 0xE3], 32);
        let memory = instr.operands.memory_operand().expect("memory operand");
        assert_eq!(
            (memory.base, memory.index, memory.scale),
            (Some(SseAddrReg::Ebx), None, 1)
        );

        // With an index the scale is preserved.
        let instr = decoded(&[0x0F, 0x10, 0x04, 0xD3], 32);
        let memory = instr.operands.memory_operand().expect("memory operand");
        assert_eq!(memory.index, Some(SseAddrReg::Edx));
        assert_eq!(memory.scale, 8);
    }

    #[test]
    fn an_ss_default_segment_survives_an_absent_override() {
        let instr = decoded(&[0x0F, 0x10, 0x45, 0xF0], 32);
        let memory = instr.operands.memory_operand().expect("memory operand");
        assert_eq!(memory.base, Some(SseAddrReg::Ebp));
        assert_eq!(memory.default_segment, SseSegment::Ss);
        assert_eq!(memory.segment, SseSegment::Ss);
        assert_eq!(memory.disp, -16);
    }

    #[test]
    fn a_prefix_run_that_cannot_fit_in_fifteen_bytes_is_not_ours() {
        // 12 prefixes plus 0F 58 C1 is exactly 15 bytes and still ours.
        let mut exactly_fifteen = vec![0x66u8; 12];
        exactly_fifteen.extend_from_slice(&[0x0F, 0x58, 0xC1]);
        assert_eq!(exactly_fifteen.len(), 15);
        assert_eq!(decoded(&exactly_fifteen, 32).op, SseOp::Addpd);

        // One more prefix pushes it past the architectural limit.
        let mut too_long = vec![0x66u8; 13];
        too_long.extend_from_slice(&[0x0F, 0x58, 0xC1]);
        assert_eq!(decode(&too_long, 32), SseDecode::NotOurs { consumed: 15 });

        // A run of nothing but prefixes is likewise not ours.
        assert_eq!(
            decode(&[0x66u8; 20], 32),
            SseDecode::NotOurs { consumed: 15 }
        );
    }

    #[test]
    fn lock_prefixed_forms_are_never_ours() {
        // iced marks every LOCK-prefixed SSE encoding invalid, and the
        // interpreter's pre-dispatch LOCK validator owns the #UD, so the
        // decoder never claims these bytes.
        for bytes in [
            vec![0xF0, 0x0F, 0x58, 0xC1],
            vec![0xF0, 0x0F, 0x58, 0x01],
            vec![0xF0, 0x66, 0x0F, 0xFE, 0xC1],
            vec![0x66, 0xF0, 0x0F, 0xFE, 0xC1],
        ] {
            assert!(
                matches!(decode(&bytes, 32), SseDecode::NotOurs { .. }),
                "{bytes:02X?}"
            );
        }
    }

    #[test]
    fn group_fifteen_splits_by_reg_field_and_reports_what_it_read() {
        // /2 mem is LDMXCSR, ours.
        assert_eq!(decoded(&[0x0F, 0xAE, 0x11], 32).op, SseOp::Ldmxcsr);
        // /7 mem is CLFLUSH, not ours -- and the rejection needed the ModR/M.
        assert_eq!(
            decode(&[0x0F, 0xAE, 0x39], 32),
            SseDecode::NotOurs { consumed: 3 }
        );
        // /4 mem is XSAVE, likewise.
        assert_eq!(
            decode(&[0x0F, 0xAE, 0x21], 32),
            SseDecode::NotOurs { consumed: 3 }
        );
        // Every register form from /5 up is a fence, whatever the r/m field.
        for rm in 0u8..8 {
            assert_eq!(decoded(&[0x0F, 0xAE, 0xE8 | rm], 32).op, SseOp::Lfence);
            assert_eq!(decoded(&[0x0F, 0xAE, 0xF0 | rm], 32).op, SseOp::Mfence);
            assert_eq!(decoded(&[0x0F, 0xAE, 0xF8 | rm], 32).op, SseOp::Sfence);
        }
        // The whole group needs a bare opcode: 66 and F3 take it elsewhere.
        for bytes in [
            vec![0x66, 0x0F, 0xAE, 0x11],
            vec![0xF3, 0x0F, 0xAE, 0xE9],
            vec![0x66, 0x0F, 0xAE, 0xE9],
        ] {
            assert!(
                matches!(decode(&bytes, 32), SseDecode::NotOurs { .. }),
                "{bytes:02X?}"
            );
        }
    }

    #[test]
    fn a_rejection_that_never_read_the_modrm_reports_the_shorter_prefix() {
        // 0F 54 has no F3 form at all, so the opcode alone settles it.
        assert_eq!(
            decode(&[0xF3, 0x0F, 0x54, 0xC1], 32),
            SseDecode::NotOurs { consumed: 3 }
        );
        // 0F 60 is MMX-only without a prefix; the opcode settles that too.
        assert_eq!(
            decode(&[0x0F, 0x60, 0xC1], 32),
            SseDecode::NotOurs { consumed: 2 }
        );
        // A non-0F opcode is settled by its first byte.
        assert_eq!(
            decode(&[0x90, 0x0F, 0x58, 0xC1], 32),
            SseDecode::NotOurs { consumed: 1 }
        );
    }

    /// iced reports no SSE level for `0F D4` (PADDQ mm) while it does for
    /// `0F FB` (PSUBQ mm) and `0F F4` (PMULUDQ mm), even though all three are
    /// SSE2 additions to the MMX register file. We reproduce that split
    /// deliberately: today's interpreter routes PADDQ mm through its compact
    /// `0F` switch and the other two through the SSE executor.
    #[test]
    fn the_mmx_form_sse2_integer_split_matches_iced() {
        assert!(matches!(
            decode(&[0x0F, 0xD4, 0xC1], 32),
            SseDecode::NotOurs { .. }
        ));
        assert_eq!(decoded(&[0x0F, 0xFB, 0xC1], 32).op, SseOp::PsubqMm);
        assert_eq!(decoded(&[0x0F, 0xF4, 0xC1], 32).op, SseOp::PmuludqMm);
    }

    #[test]
    fn the_prefetch_hints_ignore_the_mandatory_prefix() {
        for prefix in PREFIX_SHAPES {
            let mut bytes = prefix.to_vec();
            bytes.extend_from_slice(&[0x0F, 0x18, 0x09]);
            assert_eq!(decoded(&bytes, 32).op, SseOp::Prefetcht0, "{prefix:02X?}");
        }
        // /4 and up, and every register form, are reserved NOPs instead.
        assert!(matches!(
            decode(&[0x0F, 0x18, 0x21], 32),
            SseDecode::NotOurs { .. }
        ));
        assert!(matches!(
            decode(&[0x0F, 0x18, 0xC1], 32),
            SseDecode::NotOurs { .. }
        ));
    }

    #[test]
    fn every_encoding_rejects_all_of_its_own_proper_prefixes() {
        let mut long_forms: Vec<Vec<u8>> = CANONICAL
            .iter()
            .map(|(op, ..)| canonical_encoding(*op))
            .collect();
        // Plus the address shapes that add SIB and displacement bytes.
        long_forms.push(vec![0x0F, 0x10, 0x84, 0xCD, 0x78, 0x56, 0x34, 0x12]);
        long_forms.push(vec![0x66, 0x0F, 0x70, 0x44, 0x8D, 0x10, 0x1B]);
        long_forms.push(vec![0x67, 0x0F, 0x10, 0x86, 0x34, 0x12]);

        for bytes in long_forms {
            for bitness in [16u8, 32] {
                let SseDecode::Decoded(full) = decode(&bytes, bitness) else {
                    continue;
                };
                let length = usize::from(full.len);
                assert!(length <= bytes.len(), "{bytes:02X?} @{bitness}");
                for cut in 1..length {
                    match decode(&bytes[..cut], bitness) {
                        SseDecode::Truncated { required } => assert!(
                            required <= length,
                            "{bytes:02X?} @{bitness} cut {cut}: required {required} > {length}"
                        ),
                        SseDecode::NotOurs { .. } => {}
                        SseDecode::Decoded(partial) => panic!(
                            "{bytes:02X?} @{bitness} cut {cut} decoded as {partial:?} from a \
                             partial encoding"
                        ),
                    }
                }
            }
        }
    }

    #[test]
    fn a_short_buffer_asks_for_the_bytes_that_would_settle_it() {
        assert_eq!(decode(&[], 32), SseDecode::Truncated { required: 1 });
        assert_eq!(decode(&[0x0F], 32), SseDecode::Truncated { required: 2 });
        assert_eq!(
            decode(&[0x0F, 0x58], 32),
            SseDecode::Truncated { required: 3 }
        );
        assert_eq!(
            decode(&[0x0F, 0xC2, 0xC1], 32),
            SseDecode::Truncated { required: 4 }
        );
        // The displacement bytes count too.
        assert_eq!(
            decode(&[0x0F, 0x10, 0x05, 0x78, 0x56], 32),
            SseDecode::Truncated { required: 7 }
        );
    }
}
