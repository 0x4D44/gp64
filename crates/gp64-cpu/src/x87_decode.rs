//! Allocation-free decoder for the x87 D8 through DF escape maps.
//!
//! Decoding is separate from effective-address evaluation. The result contains
//! only the encoded address expression and is safe to cache across CPU states.

pub const MAX_INSTRUCTION_LENGTH: usize = 15;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeBitness {
    Bits16,
    Bits32,
}

impl CodeBitness {
    #[inline]
    const fn toggled(self) -> Self {
        match self {
            Self::Bits16 => Self::Bits32,
            Self::Bits32 => Self::Bits16,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum X87DecodeError {
    Truncated,
    InstructionTooLong,
    NotX87Opcode(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum X87Segment {
    Es,
    Cs,
    Ss,
    Ds,
    Fs,
    Gs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepeatPrefix {
    Repne,
    Rep,
}

/// Repeated 66/67 prefixes are idempotent. Segment and repeat fields contain
/// the last prefix of their class.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct X87Prefixes {
    pub lock: bool,
    pub operand_size_override: bool,
    pub address_size_override: bool,
    pub segment_override: Option<X87Segment>,
    pub repeat: Option<RepeatPrefix>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum X87AddressRegister {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum X87AddressSize {
    Bits16,
    Bits32,
}

impl From<CodeBitness> for X87AddressSize {
    fn from(value: CodeBitness) -> Self {
        match value {
            CodeBitness::Bits16 => Self::Bits16,
            CodeBitness::Bits32 => Self::Bits32,
        }
    }
}

/// Encoded memory address and access width. The raw displacement preserves the
/// immediate bits; displacement is sign extended for ordinary ModR/M forms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct X87MemoryOperand {
    pub address_size: X87AddressSize,
    pub base: Option<X87AddressRegister>,
    pub index: Option<X87AddressRegister>,
    pub scale: u8,
    pub displacement: i32,
    pub displacement_raw: u32,
    pub displacement_size: u8,
    pub default_segment: X87Segment,
    pub segment: X87Segment,
    pub width_bytes: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum X87Operand {
    None,
    Stack(u8),
    Ax,
    Memory(X87MemoryOperand),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum X87EnvironmentLayout {
    None,
    Environment14,
    Environment28,
    State94,
    State108,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum X87Operation {
    Invalid,
    Fadd,
    Faddp,
    Fiadd,
    Fmul,
    Fmulp,
    Fimul,
    Fsub,
    Fsubp,
    Fisub,
    Fsubr,
    Fsubrp,
    Fisubr,
    Fdiv,
    Fdivp,
    Fidiv,
    Fdivr,
    Fdivrp,
    Fidivr,
    Fcom,
    Fcomp,
    Fcompp,
    Ficom,
    Ficomp,
    Fucom,
    Fucomp,
    Fucompp,
    Fcomi,
    Fcomip,
    Fucomi,
    Fucomip,
    Fld,
    Fild,
    Fldz,
    Fld1,
    Fldpi,
    Fldl2e,
    Fldl2t,
    Fldlg2,
    Fldln2,
    Fst,
    Fstp,
    Fist,
    Fistp,
    Fisttp,
    Fxch,
    Ffree,
    Ffreep,
    Fbld,
    Fbstp,
    Fcmovb,
    Fcmovbe,
    Fcmove,
    Fcmovnb,
    Fcmovnbe,
    Fcmovne,
    Fcmovnu,
    Fcmovu,
    Fldcw,
    Fnstcw,
    Fnstsw,
    Fldenv,
    Fnstenv,
    Frstor,
    Fnsave,
    Fchs,
    Fabs,
    Ftst,
    Fxam,
    Fsin,
    Fcos,
    Fsincos,
    Fsqrt,
    Fptan,
    Fpatan,
    Fyl2x,
    Fyl2xp1,
    F2xm1,
    Fscale,
    Fxtract,
    Fprem,
    Fprem1,
    Frndint,
    Fdecstp,
    Fincstp,
    Fnop,
    Fninit,
    Fnclex,
    Fneni,
    Fndisi,
    Fnsetpm,
}

impl X87Operation {
    /// Number of variants. `Fnsetpm` is the last one declared and the enum
    /// carries no explicit discriminants, so its value is the maximum.
    /// Census arrays indexed by `operation as usize` size themselves from
    /// this instead of masking (EXP-A036 W6a: a `& 63` mask silently aliased
    /// every operation from the 65th onward onto an unrelated row).
    pub const COUNT: usize = Self::Fnsetpm as usize + 1;

    /// Whether this x87 operation first appeared with the P6 (Pentium Pro).
    #[inline]
    pub const fn requires_p6(self) -> bool {
        matches!(
            self,
            Self::Fcomi
                | Self::Fcomip
                | Self::Fucomi
                | Self::Fucomip
                | Self::Fcmovb
                | Self::Fcmovbe
                | Self::Fcmove
                | Self::Fcmovnb
                | Self::Fcmovnbe
                | Self::Fcmovne
                | Self::Fcmovnu
                | Self::Fcmovu
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodedX87 {
    pub operation: X87Operation,
    pub operand1: X87Operand,
    pub operand2: X87Operand,
    pub length: u8,
    pub prefixes: X87Prefixes,
    pub operand_size: CodeBitness,
    pub environment_layout: X87EnvironmentLayout,
}

impl DecodedX87 {
    /// LOCK is retained so the caller can preserve its #UD ordering and EIP
    /// rollback contract.
    #[inline]
    pub const fn has_invalid_lock_prefix(self) -> bool {
        self.prefixes.lock
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    #[inline]
    fn read_u8(&mut self) -> Result<u8, X87DecodeError> {
        if self.pos >= MAX_INSTRUCTION_LENGTH {
            return Err(X87DecodeError::InstructionTooLong);
        }
        let value = *self.bytes.get(self.pos).ok_or(X87DecodeError::Truncated)?;
        self.pos += 1;
        Ok(value)
    }

    #[inline]
    fn read_u16(&mut self) -> Result<u16, X87DecodeError> {
        Ok(u16::from_le_bytes([self.read_u8()?, self.read_u8()?]))
    }

    #[inline]
    fn read_u32(&mut self) -> Result<u32, X87DecodeError> {
        Ok(u32::from_le_bytes([
            self.read_u8()?,
            self.read_u8()?,
            self.read_u8()?,
            self.read_u8()?,
        ]))
    }
}

/// Decode one legacy-mode x87 instruction.
///
/// Reserved D8-DF forms decode successfully as Invalid. Truncation, the 15-byte
/// limit, and a non-x87 opcode remain distinct caller-level errors.
pub fn decode_x87(
    bytes: &[u8],
    default_bitness: CodeBitness,
) -> Result<DecodedX87, X87DecodeError> {
    let mut reader = Reader::new(bytes);
    let mut prefixes = X87Prefixes::default();
    let opcode = loop {
        let byte = reader.read_u8()?;
        match byte {
            0x26 => prefixes.segment_override = Some(X87Segment::Es),
            0x2e => prefixes.segment_override = Some(X87Segment::Cs),
            0x36 => prefixes.segment_override = Some(X87Segment::Ss),
            0x3e => prefixes.segment_override = Some(X87Segment::Ds),
            0x64 => prefixes.segment_override = Some(X87Segment::Fs),
            0x65 => prefixes.segment_override = Some(X87Segment::Gs),
            0x66 => prefixes.operand_size_override = true,
            0x67 => prefixes.address_size_override = true,
            0xf0 => prefixes.lock = true,
            0xf2 => prefixes.repeat = Some(RepeatPrefix::Repne),
            0xf3 => prefixes.repeat = Some(RepeatPrefix::Rep),
            _ => break byte,
        }
    };
    if !(0xd8..=0xdf).contains(&opcode) {
        return Err(X87DecodeError::NotX87Opcode(opcode));
    }

    let operand_size = if prefixes.operand_size_override {
        default_bitness.toggled()
    } else {
        default_bitness
    };
    let address_bitness = if prefixes.address_size_override {
        default_bitness.toggled()
    } else {
        default_bitness
    };
    let modrm = reader.read_u8()?;
    let mode = modrm >> 6;
    let reg = (modrm >> 3) & 7;
    let rm = modrm & 7;

    let (operation, operand1, operand2, environment_layout) = if mode == 3 {
        let (operation, operand1, operand2) = decode_register_form(opcode, modrm);
        (operation, operand1, operand2, X87EnvironmentLayout::None)
    } else {
        let (operation, width, layout) = decode_memory_form(opcode, reg, operand_size);
        let memory = decode_memory_operand(
            &mut reader,
            mode,
            rm,
            address_bitness.into(),
            prefixes.segment_override,
            width,
        )?;
        (
            operation,
            X87Operand::Memory(memory),
            X87Operand::None,
            layout,
        )
    };
    Ok(DecodedX87 {
        operation,
        operand1,
        operand2,
        length: reader.pos as u8,
        prefixes,
        operand_size,
        environment_layout,
    })
}

fn decode_memory_form(
    opcode: u8,
    reg: u8,
    operand_size: CodeBitness,
) -> (X87Operation, u16, X87EnvironmentLayout) {
    use X87EnvironmentLayout as Layout;
    use X87Operation as Op;

    let environment = match operand_size {
        CodeBitness::Bits16 => (14, Layout::Environment14),
        CodeBitness::Bits32 => (28, Layout::Environment28),
    };
    let state = match operand_size {
        CodeBitness::Bits16 => (94, Layout::State94),
        CodeBitness::Bits32 => (108, Layout::State108),
    };
    match (opcode, reg) {
        (0xd8, 0) => (Op::Fadd, 4, Layout::None),
        (0xd8, 1) => (Op::Fmul, 4, Layout::None),
        (0xd8, 2) => (Op::Fcom, 4, Layout::None),
        (0xd8, 3) => (Op::Fcomp, 4, Layout::None),
        (0xd8, 4) => (Op::Fsub, 4, Layout::None),
        (0xd8, 5) => (Op::Fsubr, 4, Layout::None),
        (0xd8, 6) => (Op::Fdiv, 4, Layout::None),
        (0xd8, 7) => (Op::Fdivr, 4, Layout::None),
        (0xd9, 0) => (Op::Fld, 4, Layout::None),
        (0xd9, 2) => (Op::Fst, 4, Layout::None),
        (0xd9, 3) => (Op::Fstp, 4, Layout::None),
        (0xd9, 4) => (Op::Fldenv, environment.0, environment.1),
        (0xd9, 5) => (Op::Fldcw, 2, Layout::None),
        (0xd9, 6) => (Op::Fnstenv, environment.0, environment.1),
        (0xd9, 7) => (Op::Fnstcw, 2, Layout::None),
        (0xda, 0) => (Op::Fiadd, 4, Layout::None),
        (0xda, 1) => (Op::Fimul, 4, Layout::None),
        (0xda, 2) => (Op::Ficom, 4, Layout::None),
        (0xda, 3) => (Op::Ficomp, 4, Layout::None),
        (0xda, 4) => (Op::Fisub, 4, Layout::None),
        (0xda, 5) => (Op::Fisubr, 4, Layout::None),
        (0xda, 6) => (Op::Fidiv, 4, Layout::None),
        (0xda, 7) => (Op::Fidivr, 4, Layout::None),
        (0xdb, 0) => (Op::Fild, 4, Layout::None),
        (0xdb, 1) => (Op::Fisttp, 4, Layout::None),
        (0xdb, 2) => (Op::Fist, 4, Layout::None),
        (0xdb, 3) => (Op::Fistp, 4, Layout::None),
        (0xdb, 5) => (Op::Fld, 10, Layout::None),
        (0xdb, 7) => (Op::Fstp, 10, Layout::None),
        (0xdc, 0) => (Op::Fadd, 8, Layout::None),
        (0xdc, 1) => (Op::Fmul, 8, Layout::None),
        (0xdc, 2) => (Op::Fcom, 8, Layout::None),
        (0xdc, 3) => (Op::Fcomp, 8, Layout::None),
        (0xdc, 4) => (Op::Fsub, 8, Layout::None),
        (0xdc, 5) => (Op::Fsubr, 8, Layout::None),
        (0xdc, 6) => (Op::Fdiv, 8, Layout::None),
        (0xdc, 7) => (Op::Fdivr, 8, Layout::None),
        (0xdd, 0) => (Op::Fld, 8, Layout::None),
        (0xdd, 1) => (Op::Fisttp, 8, Layout::None),
        (0xdd, 2) => (Op::Fst, 8, Layout::None),
        (0xdd, 3) => (Op::Fstp, 8, Layout::None),
        (0xdd, 4) => (Op::Frstor, state.0, state.1),
        (0xdd, 6) => (Op::Fnsave, state.0, state.1),
        (0xdd, 7) => (Op::Fnstsw, 2, Layout::None),
        (0xde, 0) => (Op::Fiadd, 2, Layout::None),
        (0xde, 1) => (Op::Fimul, 2, Layout::None),
        (0xde, 2) => (Op::Ficom, 2, Layout::None),
        (0xde, 3) => (Op::Ficomp, 2, Layout::None),
        (0xde, 4) => (Op::Fisub, 2, Layout::None),
        (0xde, 5) => (Op::Fisubr, 2, Layout::None),
        (0xde, 6) => (Op::Fidiv, 2, Layout::None),
        (0xde, 7) => (Op::Fidivr, 2, Layout::None),
        (0xdf, 0) => (Op::Fild, 2, Layout::None),
        (0xdf, 1) => (Op::Fisttp, 2, Layout::None),
        (0xdf, 2) => (Op::Fist, 2, Layout::None),
        (0xdf, 3) => (Op::Fistp, 2, Layout::None),
        (0xdf, 4) => (Op::Fbld, 10, Layout::None),
        (0xdf, 5) => (Op::Fild, 8, Layout::None),
        (0xdf, 6) => (Op::Fbstp, 10, Layout::None),
        (0xdf, 7) => (Op::Fistp, 8, Layout::None),
        _ => (Op::Invalid, 0, Layout::None),
    }
}

fn decode_register_form(opcode: u8, modrm: u8) -> (X87Operation, X87Operand, X87Operand) {
    match opcode {
        0xd8 => decode_register_d8(modrm),
        0xd9 => decode_register_d9(modrm),
        0xda => decode_register_da(modrm),
        0xdb => decode_register_db(modrm),
        0xdc => decode_register_dc(modrm),
        0xdd => decode_register_dd(modrm),
        0xde => decode_register_de(modrm),
        0xdf => decode_register_df(modrm),
        _ => unreachable!(),
    }
}

#[inline]
fn invalid_register_form() -> (X87Operation, X87Operand, X87Operand) {
    (X87Operation::Invalid, X87Operand::None, X87Operand::None)
}

fn decode_register_d8(modrm: u8) -> (X87Operation, X87Operand, X87Operand) {
    use X87Operand as Arg;
    use X87Operation as Op;

    let stack = Arg::Stack(modrm & 7);
    let st0 = Arg::Stack(0);
    let operation = match (modrm >> 3) & 7 {
        0 => Op::Fadd,
        1 => Op::Fmul,
        2 => Op::Fcom,
        3 => Op::Fcomp,
        4 => Op::Fsub,
        5 => Op::Fsubr,
        6 => Op::Fdiv,
        7 => Op::Fdivr,
        _ => unreachable!(),
    };
    (operation, st0, stack)
}

fn decode_register_d9(modrm: u8) -> (X87Operation, X87Operand, X87Operand) {
    use X87Operand as Arg;
    use X87Operation as Op;

    let none = Arg::None;
    match modrm {
        0xc0..=0xc7 => (Op::Fld, Arg::Stack(modrm & 7), none),
        0xc8..=0xcf => (Op::Fxch, Arg::Stack(0), Arg::Stack(modrm & 7)),
        0xd0 => (Op::Fnop, none, none),
        0xd8..=0xdf => (Op::Fstp, Arg::Stack(modrm & 7), none),
        0xe0 => (Op::Fchs, none, none),
        0xe1 => (Op::Fabs, none, none),
        0xe4 => (Op::Ftst, none, none),
        0xe5 => (Op::Fxam, none, none),
        0xe8 => (Op::Fld1, none, none),
        0xe9 => (Op::Fldl2t, none, none),
        0xea => (Op::Fldl2e, none, none),
        0xeb => (Op::Fldpi, none, none),
        0xec => (Op::Fldlg2, none, none),
        0xed => (Op::Fldln2, none, none),
        0xee => (Op::Fldz, none, none),
        0xf0 => (Op::F2xm1, none, none),
        0xf1 => (Op::Fyl2x, none, none),
        0xf2 => (Op::Fptan, none, none),
        0xf3 => (Op::Fpatan, none, none),
        0xf4 => (Op::Fxtract, none, none),
        0xf5 => (Op::Fprem1, none, none),
        0xf6 => (Op::Fdecstp, none, none),
        0xf7 => (Op::Fincstp, none, none),
        0xf8 => (Op::Fprem, none, none),
        0xf9 => (Op::Fyl2xp1, none, none),
        0xfa => (Op::Fsqrt, none, none),
        0xfb => (Op::Fsincos, none, none),
        0xfc => (Op::Frndint, none, none),
        0xfd => (Op::Fscale, none, none),
        0xfe => (Op::Fsin, none, none),
        0xff => (Op::Fcos, none, none),
        _ => invalid_register_form(),
    }
}

fn decode_register_da(modrm: u8) -> (X87Operation, X87Operand, X87Operand) {
    use X87Operand as Arg;
    use X87Operation as Op;

    let st0 = Arg::Stack(0);
    let stack = Arg::Stack(modrm & 7);
    match modrm {
        0xc0..=0xc7 => (Op::Fcmovb, st0, stack),
        0xc8..=0xcf => (Op::Fcmove, st0, stack),
        0xd0..=0xd7 => (Op::Fcmovbe, st0, stack),
        0xd8..=0xdf => (Op::Fcmovu, st0, stack),
        0xe9 => (Op::Fucompp, st0, Arg::Stack(1)),
        _ => invalid_register_form(),
    }
}

fn decode_register_db(modrm: u8) -> (X87Operation, X87Operand, X87Operand) {
    use X87Operand as Arg;
    use X87Operation as Op;

    let none = Arg::None;
    let st0 = Arg::Stack(0);
    let stack = Arg::Stack(modrm & 7);
    match modrm {
        0xc0..=0xc7 => (Op::Fcmovnb, st0, stack),
        0xc8..=0xcf => (Op::Fcmovne, st0, stack),
        0xd0..=0xd7 => (Op::Fcmovnbe, st0, stack),
        0xd8..=0xdf => (Op::Fcmovnu, st0, stack),
        0xe0 => (Op::Fneni, none, none),
        0xe1 => (Op::Fndisi, none, none),
        0xe2 => (Op::Fnclex, none, none),
        0xe3 => (Op::Fninit, none, none),
        0xe4 => (Op::Fnsetpm, none, none),
        0xe8..=0xef => (Op::Fucomi, st0, stack),
        0xf0..=0xf7 => (Op::Fcomi, st0, stack),
        _ => invalid_register_form(),
    }
}

fn decode_register_dc(modrm: u8) -> (X87Operation, X87Operand, X87Operand) {
    use X87Operand as Arg;
    use X87Operation as Op;

    let stack = Arg::Stack(modrm & 7);
    let st0 = Arg::Stack(0);
    let (operation, first, second) = match (modrm >> 3) & 7 {
        0 => (Op::Fadd, stack, st0),
        1 => (Op::Fmul, stack, st0),
        2 => (Op::Fcom, st0, stack),
        3 => (Op::Fcomp, st0, stack),
        4 => (Op::Fsubr, stack, st0),
        5 => (Op::Fsub, stack, st0),
        6 => (Op::Fdivr, stack, st0),
        7 => (Op::Fdiv, stack, st0),
        _ => unreachable!(),
    };
    (operation, first, second)
}

fn decode_register_dd(modrm: u8) -> (X87Operation, X87Operand, X87Operand) {
    use X87Operand as Arg;
    use X87Operation as Op;

    let none = Arg::None;
    let stack = Arg::Stack(modrm & 7);
    let st0 = Arg::Stack(0);
    match modrm {
        0xc0..=0xc7 => (Op::Ffree, stack, none),
        0xc8..=0xcf => (Op::Fxch, st0, stack),
        0xd0..=0xd7 => (Op::Fst, stack, none),
        0xd8..=0xdf => (Op::Fstp, stack, none),
        0xe0..=0xe7 => (Op::Fucom, st0, stack),
        0xe8..=0xef => (Op::Fucomp, st0, stack),
        _ => invalid_register_form(),
    }
}

fn decode_register_de(modrm: u8) -> (X87Operation, X87Operand, X87Operand) {
    use X87Operand as Arg;
    use X87Operation as Op;

    let stack = Arg::Stack(modrm & 7);
    let st0 = Arg::Stack(0);
    match modrm {
        0xc0..=0xc7 => (Op::Faddp, stack, st0),
        0xc8..=0xcf => (Op::Fmulp, stack, st0),
        0xd0..=0xd7 => (Op::Fcomp, st0, stack),
        0xd9 => (Op::Fcompp, st0, Arg::Stack(1)),
        0xe0..=0xe7 => (Op::Fsubrp, stack, st0),
        0xe8..=0xef => (Op::Fsubp, stack, st0),
        0xf0..=0xf7 => (Op::Fdivrp, stack, st0),
        0xf8..=0xff => (Op::Fdivp, stack, st0),
        _ => invalid_register_form(),
    }
}

fn decode_register_df(modrm: u8) -> (X87Operation, X87Operand, X87Operand) {
    use X87Operand as Arg;
    use X87Operation as Op;

    let none = Arg::None;
    let stack = Arg::Stack(modrm & 7);
    let st0 = Arg::Stack(0);
    match modrm {
        0xc0..=0xc7 => (Op::Ffreep, stack, none),
        0xc8..=0xcf => (Op::Fxch, st0, stack),
        0xd0..=0xdf => (Op::Fstp, stack, none),
        0xe0 => (Op::Fnstsw, Arg::Ax, none),
        0xe8..=0xef => (Op::Fucomip, st0, stack),
        0xf0..=0xf7 => (Op::Fcomip, st0, stack),
        _ => invalid_register_form(),
    }
}

fn decode_memory_operand(
    reader: &mut Reader<'_>,
    mode: u8,
    rm: u8,
    address_size: X87AddressSize,
    segment_override: Option<X87Segment>,
    width: u16,
) -> Result<X87MemoryOperand, X87DecodeError> {
    match address_size {
        X87AddressSize::Bits16 => {
            decode_memory_operand16(reader, mode, rm, segment_override, width)
        }
        X87AddressSize::Bits32 => {
            decode_memory_operand32(reader, mode, rm, segment_override, width)
        }
    }
}

fn decode_memory_operand16(
    reader: &mut Reader<'_>,
    mode: u8,
    rm: u8,
    override_segment: Option<X87Segment>,
    width: u16,
) -> Result<X87MemoryOperand, X87DecodeError> {
    use X87AddressRegister as Reg;
    use X87Segment as Seg;

    let (base, index, default_segment) = match rm {
        0 => (Some(Reg::Bx), Some(Reg::Si), Seg::Ds),
        1 => (Some(Reg::Bx), Some(Reg::Di), Seg::Ds),
        2 => (Some(Reg::Bp), Some(Reg::Si), Seg::Ss),
        3 => (Some(Reg::Bp), Some(Reg::Di), Seg::Ss),
        4 => (Some(Reg::Si), None, Seg::Ds),
        5 => (Some(Reg::Di), None, Seg::Ds),
        6 if mode == 0 => (None, None, Seg::Ds),
        6 => (Some(Reg::Bp), None, Seg::Ss),
        7 => (Some(Reg::Bx), None, Seg::Ds),
        _ => unreachable!(),
    };
    let (displacement, raw, size) = match mode {
        0 if rm == 6 => {
            let raw = reader.read_u16()?;
            (i32::from(raw), u32::from(raw), 2)
        }
        0 => (0, 0, 0),
        1 => {
            let raw = reader.read_u8()?;
            (i32::from(raw as i8), u32::from(raw), 1)
        }
        2 => {
            let raw = reader.read_u16()?;
            (i32::from(raw as i16), u32::from(raw), 2)
        }
        _ => unreachable!(),
    };
    Ok(X87MemoryOperand {
        address_size: X87AddressSize::Bits16,
        base,
        index,
        scale: 1,
        displacement,
        displacement_raw: raw,
        displacement_size: size,
        default_segment,
        segment: override_segment.unwrap_or(default_segment),
        width_bytes: width,
    })
}

fn decode_memory_operand32(
    reader: &mut Reader<'_>,
    mode: u8,
    rm: u8,
    override_segment: Option<X87Segment>,
    width: u16,
) -> Result<X87MemoryOperand, X87DecodeError> {
    use X87AddressRegister as Reg;
    use X87Segment as Seg;

    const REGS: [Reg; 8] = [
        Reg::Eax,
        Reg::Ecx,
        Reg::Edx,
        Reg::Ebx,
        Reg::Esp,
        Reg::Ebp,
        Reg::Esi,
        Reg::Edi,
    ];
    let (base, index, scale, default_segment, no_base) = if rm == 4 {
        let sib = reader.read_u8()?;
        let index_code = (sib >> 3) & 7;
        let base_code = sib & 7;
        let no_base = mode == 0 && base_code == 5;
        let base = if no_base {
            None
        } else {
            Some(REGS[base_code as usize])
        };
        let index = if index_code == 4 {
            None
        } else {
            Some(REGS[index_code as usize])
        };
        let scale = if index.is_none() { 1 } else { 1 << (sib >> 6) };
        let default_segment = match base {
            Some(Reg::Esp | Reg::Ebp) => Seg::Ss,
            _ => Seg::Ds,
        };
        (base, index, scale, default_segment, no_base)
    } else if mode == 0 && rm == 5 {
        (None, None, 1, Seg::Ds, true)
    } else {
        let base = Some(REGS[rm as usize]);
        let default_segment = match base {
            Some(Reg::Ebp) => Seg::Ss,
            _ => Seg::Ds,
        };
        (base, None, 1, default_segment, false)
    };
    let (displacement, raw, size) = match mode {
        0 if no_base => {
            let raw = reader.read_u32()?;
            (raw as i32, raw, 4)
        }
        0 => (0, 0, 0),
        1 => {
            let raw = reader.read_u8()?;
            (i32::from(raw as i8), u32::from(raw), 1)
        }
        2 => {
            let raw = reader.read_u32()?;
            (raw as i32, raw, 4)
        }
        _ => unreachable!(),
    };
    Ok(X87MemoryOperand {
        address_size: X87AddressSize::Bits32,
        base,
        index,
        scale,
        displacement,
        displacement_raw: raw,
        displacement_size: size,
        default_segment,
        segment: override_segment.unwrap_or(default_segment),
        width_bytes: width,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_x86::{Decoder, DecoderOptions};

    fn decode(bytes: &[u8], bitness: CodeBitness) -> DecodedX87 {
        decode_x87(bytes, bitness).unwrap()
    }

    fn memory(decoded: DecodedX87) -> X87MemoryOperand {
        match decoded.operand1 {
            X87Operand::Memory(memory) => memory,
            other => panic!("expected memory operand, got {other:?}"),
        }
    }

    #[test]
    fn p6_requirement_covers_all_conditional_move_and_integer_compare_operations() {
        for operation in [
            X87Operation::Fcomi,
            X87Operation::Fcomip,
            X87Operation::Fucomi,
            X87Operation::Fucomip,
            X87Operation::Fcmovb,
            X87Operation::Fcmovbe,
            X87Operation::Fcmove,
            X87Operation::Fcmovnb,
            X87Operation::Fcmovnbe,
            X87Operation::Fcmovne,
            X87Operation::Fcmovnu,
            X87Operation::Fcmovu,
        ] {
            assert!(operation.requires_p6(), "{operation:?}");
        }
        assert!(!X87Operation::Fucompp.requires_p6());
        assert!(!X87Operation::Fisttp.requires_p6());
    }

    #[test]
    fn prefixes_are_idempotent_and_last_override_wins() {
        let decoded = decode(
            &[0x66, 0x67, 0x26, 0xf2, 0x66, 0x67, 0x64, 0xf3, 0xd9, 0x00],
            CodeBitness::Bits32,
        );
        assert_eq!(decoded.length, 10);
        assert_eq!(decoded.operand_size, CodeBitness::Bits16);
        assert_eq!(decoded.prefixes.segment_override, Some(X87Segment::Fs));
        assert_eq!(decoded.prefixes.repeat, Some(RepeatPrefix::Rep));
        let memory = memory(decoded);
        assert_eq!(memory.address_size, X87AddressSize::Bits16);
        assert_eq!(memory.segment, X87Segment::Fs);
    }

    #[test]
    fn lock_is_retained_for_the_caller_level_ud_gate() {
        let decoded = decode(&[0xf0, 0xd9, 0xe8], CodeBitness::Bits32);
        assert_eq!(decoded.operation, X87Operation::Fld1);
        assert!(decoded.has_invalid_lock_prefix());
    }

    #[test]
    fn address16_base_roles_and_displacements_are_exact() {
        use X87AddressRegister as Reg;
        use X87Segment as Seg;

        let rows = [
            (0x00, Some(Reg::Bx), Some(Reg::Si), Seg::Ds),
            (0x01, Some(Reg::Bx), Some(Reg::Di), Seg::Ds),
            (0x02, Some(Reg::Bp), Some(Reg::Si), Seg::Ss),
            (0x03, Some(Reg::Bp), Some(Reg::Di), Seg::Ss),
            (0x04, Some(Reg::Si), None, Seg::Ds),
            (0x05, Some(Reg::Di), None, Seg::Ds),
            (0x47, Some(Reg::Bx), None, Seg::Ds),
        ];
        for (modrm, base, index, segment) in rows {
            let memory = memory(decode(&[0xd9, modrm, 0], CodeBitness::Bits16));
            assert_eq!((memory.base, memory.index), (base, index));
            assert_eq!(memory.default_segment, segment);
        }

        let direct = memory(decode(&[0xd9, 0x06, 0x34, 0x92], CodeBitness::Bits16));
        assert_eq!((direct.base, direct.index), (None, None));
        assert_eq!(direct.displacement_raw, 0x9234);
        assert_eq!(direct.displacement, 0x9234);
        assert_eq!(direct.default_segment, Seg::Ds);

        let bp_disp8 = memory(decode(&[0xd9, 0x46, 0x80], CodeBitness::Bits16));
        assert_eq!(bp_disp8.base, Some(Reg::Bp));
        assert_eq!(bp_disp8.displacement, -128);
        assert_eq!(bp_disp8.default_segment, Seg::Ss);
    }

    #[test]
    fn address32_sib_uses_only_the_actual_base_for_default_segment() {
        use X87AddressRegister as Reg;
        use X87Segment as Seg;

        // [EBP*4 + disp32]: no base, so DS despite EBP being the index.
        let no_base = memory(decode(
            &[0xd9, 0x04, 0xad, 0x78, 0x56, 0x34, 0x12],
            CodeBitness::Bits32,
        ));
        assert_eq!(no_base.base, None);
        assert_eq!(no_base.index, Some(Reg::Ebp));
        assert_eq!(no_base.scale, 4);
        assert_eq!(no_base.default_segment, Seg::Ds);
        assert_eq!(no_base.displacement_raw, 0x1234_5678);

        // [ESP + EDX*2 - 4]: the actual ESP base selects SS.
        let esp_base = memory(decode(&[0xd9, 0x44, 0x54, 0xfc], CodeBitness::Bits32));
        assert_eq!(esp_base.base, Some(Reg::Esp));
        assert_eq!(esp_base.index, Some(Reg::Edx));
        assert_eq!(esp_base.scale, 2);
        assert_eq!(esp_base.displacement, -4);
        assert_eq!(esp_base.default_segment, Seg::Ss);

        // A no-index SIB ignores the encoded scale bits.
        let no_index = memory(decode(&[0xd9, 0x04, 0xe3], CodeBitness::Bits32));
        assert_eq!(no_index.base, Some(Reg::Ebx));
        assert_eq!(no_index.index, None);
        assert_eq!(no_index.scale, 1);
    }

    #[test]
    fn register_aliases_and_fixed_encodings_are_explicit() {
        use X87Operation as Op;

        for (bytes, operation) in [
            ([0xd9, 0xd0], Op::Fnop),
            ([0xda, 0xe9], Op::Fucompp),
            ([0xdb, 0xe0], Op::Fneni),
            ([0xdb, 0xe1], Op::Fndisi),
            ([0xdb, 0xe2], Op::Fnclex),
            ([0xdb, 0xe3], Op::Fninit),
            ([0xdb, 0xe4], Op::Fnsetpm),
            ([0xdc, 0xd3], Op::Fcom),
            ([0xdc, 0xdb], Op::Fcomp),
            ([0xdd, 0xcb], Op::Fxch),
            ([0xde, 0xd3], Op::Fcomp),
            ([0xde, 0xd9], Op::Fcompp),
            ([0xdf, 0xc3], Op::Ffreep),
            ([0xdf, 0xcb], Op::Fxch),
            ([0xdf, 0xd3], Op::Fstp),
            ([0xdf, 0xdb], Op::Fstp),
            ([0xdf, 0xe0], Op::Fnstsw),
        ] {
            assert_eq!(decode(&bytes, CodeBitness::Bits32).operation, operation);
        }

        for bytes in [[0xd9, 0xd1], [0xda, 0xe8], [0xdf, 0xe1]] {
            assert_eq!(decode(&bytes, CodeBitness::Bits32).operation, Op::Invalid);
        }
    }

    #[test]
    fn operand_override_selects_environment_and_state_layout_once() {
        let env16 = decode(&[0x66, 0x66, 0xd9, 0x20], CodeBitness::Bits32);
        assert_eq!(
            env16.environment_layout,
            X87EnvironmentLayout::Environment14
        );
        assert_eq!(memory(env16).width_bytes, 14);

        let state108 = decode(&[0x66, 0x66, 0xdd, 0x20], CodeBitness::Bits16);
        assert_eq!(state108.environment_layout, X87EnvironmentLayout::State108);
        assert_eq!(memory(state108).width_bytes, 108);
    }

    #[test]
    fn reserved_memory_form_keeps_full_address_length() {
        // DB /4 is reserved, but its SIB and disp32 still belong to it.
        let decoded = decode(
            &[0xdb, 0x24, 0x25, 0x78, 0x56, 0x34, 0x12],
            CodeBitness::Bits32,
        );
        assert_eq!(decoded.operation, X87Operation::Invalid);
        assert_eq!(decoded.length, 7);
        assert_eq!(memory(decoded).width_bytes, 0);
    }

    #[test]
    fn truncation_and_fifteen_byte_limit_are_distinct() {
        assert_eq!(
            decode_x87(&[0xd9], CodeBitness::Bits32),
            Err(X87DecodeError::Truncated)
        );
        assert_eq!(
            decode_x87(&[0xd9, 0x05, 1, 2, 3], CodeBitness::Bits32),
            Err(X87DecodeError::Truncated)
        );

        let mut exactly_fifteen = vec![0x66; 13];
        exactly_fifteen.extend_from_slice(&[0xd9, 0xe8]);
        assert_eq!(decode(&exactly_fifteen, CodeBitness::Bits32).length, 15);

        let mut too_long = vec![0x66; 14];
        too_long.extend_from_slice(&[0xd9, 0xe8]);
        assert_eq!(
            decode_x87(&too_long, CodeBitness::Bits32),
            Err(X87DecodeError::InstructionTooLong)
        );
    }

    #[test]
    fn every_escape_and_modrm_pair_decodes_without_panicking() {
        for bitness in [CodeBitness::Bits16, CodeBitness::Bits32] {
            for opcode in 0xd8..=0xdf {
                for modrm in 0u8..=u8::MAX {
                    let bytes = [opcode, modrm, 0, 0, 0, 0, 0, 0];
                    let decoded = decode_x87(&bytes, bitness).unwrap();
                    assert!((2..=8).contains(&decoded.length));
                }
            }
        }
    }

    #[test]
    fn portable_x87_decoder_is_total_against_iced_for_all_escape_forms() {
        for (bitness, code_bitness) in [(16u32, CodeBitness::Bits16), (32, CodeBitness::Bits32)] {
            for opcode in 0xd8u8..=0xdf {
                for modrm in 0u8..=u8::MAX {
                    let mut bytes = [0u8; 15];
                    bytes[0] = opcode;
                    bytes[1] = modrm;
                    for (index, byte) in bytes[2..].iter_mut().enumerate() {
                        *byte = (index as u8).wrapping_mul(29).wrapping_add(modrm);
                    }

                    let mut iced = Decoder::new(bitness, &bytes, DecoderOptions::NONE);
                    let iced = iced.decode();
                    let portable = decode_x87(&bytes, code_bitness).unwrap_or_else(|error| {
                        panic!(
                            "portable decode failed for {bitness}-bit {opcode:02X} {modrm:02X}: {error:?}"
                        )
                    });
                    let adapted = crate::x87_iced_adapter::decode_instruction(&iced);

                    if iced.is_invalid() {
                        assert_eq!(
                            portable.operation,
                            X87Operation::Invalid,
                            "iced rejected {bitness}-bit {opcode:02X} {modrm:02X}, but portable accepted {portable:?}"
                        );
                    } else {
                        assert_eq!(
                            portable, adapted,
                            "descriptor mismatch for {bitness}-bit {opcode:02X} {modrm:02X}: iced={iced:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn decoded_value_stays_small_enough_for_the_hot_cache() {
        assert!(std::mem::size_of::<DecodedX87>() <= 64);
    }
}
