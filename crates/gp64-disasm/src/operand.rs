//! The operand model, in Intel's own notation.
//!
//! Opcode tables are written with the abbreviations the SDM's opcode maps use —
//! `Eb`, `Gv`, `Iz`, `Jb` — because that is what the reference material says and
//! a table written in any other vocabulary has to be mentally translated every
//! time it is checked against the manual. A capital letter picks the addressing
//! method, a lower-case one the operand size.
//!
//! The size letters that matter here:
//!
//! * `b` — always one byte
//! * `w` — always two
//! * `v` — two or four, following the operand size
//! * `z` — two or four, following the operand size (differs from `v` only at
//!   64-bit operand size, which nothing here has)
//! * `p` — a far pointer: `ptr16:16` or `ptr16:32`

/// How an operand is found and how wide it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    /// No operand in this slot.
    None,
    /// ModRM r/m, byte.
    Eb,
    /// ModRM r/m, operand size.
    Ev,
    /// ModRM r/m, word.
    Ew,
    /// ModRM r/m that is word in memory but operand-sized in a register — the
    /// destination of `MOV r/m16, Sreg`, where a register destination takes the
    /// full width while a memory one stays two bytes.
    EwOrV,
    /// ModRM r/m, forced to memory (`LES`, `LDS`, `BOUND`, `LEA`).
    M,
    /// ModRM r/m, forced to memory, at a size fixed by the OPCODE rather than
    /// by the operand-size prefix — the x87 loads and stores, where `D8 /0`
    /// reads four bytes and `DC /0` reads eight however the code is prefixed.
    Mx(MemSize),
    /// A fixed `ax`, at that width whatever the operand size — `FNSTSW AX`.
    FixedAx,
    /// ModRM r/m as a far pointer (`m16:16` / `m16:32`).
    Mp,
    /// ModRM r/m holding a PAIR of operands, at twice the operand size — the
    /// SDM's `Ma`, used only by `BOUND`. It always carries an explicit size
    /// marker, because the register operand beside it is half as wide and so
    /// does not say how much memory is read.
    Ma,
    /// ModRM reg, byte.
    Gb,
    /// ModRM reg, operand size.
    Gv,
    /// ModRM reg, always word — `ARPL`'s register operand.
    Gw,
    /// ModRM reg as a segment register.
    Sw,
    /// ModRM reg as an MMX register.
    Pq,
    /// ModRM r/m as an MMX register or a 64-bit memory operand.
    Qq,
    /// ModRM r/m forced to an MMX REGISTER — the shift-by-immediate groups and
    /// MASKMOVQ, which have no memory form at all.
    Nq,
    /// ModRM r/m as a 32-bit general register or a 32-bit memory operand,
    /// whatever the operand-size prefix says. MOVD's non-MMX half.
    Ed,
    /// ModRM reg as a 32-bit general register, whatever the prefix says.
    Gd,
    /// ModRM reg as an XMM register.
    Vx,
    /// ModRM r/m as an XMM register or a memory operand. The width is fixed by
    /// the opcode and its mandatory prefix — 16 bytes packed, 4 or 8 scalar —
    /// and the XMM register beside it says which, so no size marker is printed.
    Wx,
    /// ModRM r/m forced to an XMM REGISTER, with no memory form.
    Ux,
    /// ModRM r/m as a 32-bit general register, ALWAYS — the `mod` field is
    /// ignored entirely, so there is no memory form and no displacement, and
    /// the operand-size prefix does not narrow it. Only `MOV` to and from the
    /// control and debug registers encodes this way.
    Rd,
    /// ModRM reg as a control register.
    Cd,
    /// ModRM reg as a debug register.
    Dd,
    /// Immediate byte.
    Ib,
    /// Immediate byte, sign-extended to the operand size when printed.
    Ibs,
    /// Immediate word.
    Iw,
    /// Immediate, operand size.
    Iz,
    /// Relative displacement, byte — printed as an absolute target.
    Jb,
    /// Relative displacement, operand size — printed as an absolute target.
    Jz,
    /// Direct memory offset, ADDRESS size (`moffs`), byte operand.
    Ob,
    /// Direct memory offset, ADDRESS size, operand size.
    Ov,
    /// Far pointer immediate (`ptr16:16` / `ptr16:32`).
    Ap,
    /// A fixed 8-bit register, by name.
    FixedB(u8),
    /// A fixed register at the operand size, by index (0 = eAX).
    FixedV(u8),
    /// A register taken from the low three bits of the opcode, byte size.
    OpcodeB,
    /// A register taken from the low three bits of the opcode, operand size.
    OpcodeV,
    /// A fixed segment register, by index.
    FixedSeg(u8),
    /// The literal `1`, for the shift-by-one forms.
    One,
    /// The `CL` register, for the shift-by-CL forms.
    Cl,
    /// The `DX` register, for the port forms.
    Dx,
    /// A memory operand implied by a string instruction, addressed by SI.
    Xb,
    /// As `Xb`, at the operand size.
    Xv,
    /// A memory operand implied by a string instruction, addressed by DI.
    Yb,
    /// As `Yb`, at the operand size.
    Yv,
    /// An x87 stack register taken from the low three bits of the ModRM byte.
    ///
    /// Declared ahead of the x87 table that will use it, so the operand model is
    /// complete in one place rather than growing a second time.
    #[allow(dead_code)]
    St,
    /// The implicit `st(0)`.
    #[allow(dead_code)]
    St0,
    /// `XLAT`'s implicit table pointer, `[bx]` or `[ebx]`.
    XlatTable,
}

impl Op {
    /// Whether this operand fixes the memory operand size on its own.
    ///
    /// Intel syntax prints an explicit `dword ptr` only when nothing else in the
    /// instruction says how wide the memory access is. A register operand does
    /// say, so `mov ebx,[esi+10h]` needs no marker while `push dword ptr [esi]`
    /// does. Getting this backwards produces output that is not wrong so much as
    /// visibly not Intel syntax, which is worse in a fault dump than being terse.
    /// [`Self::implies_size`] by reference, for use as a predicate.
    pub(crate) fn implies_size_ref(op: &Self) -> bool {
        op.implies_size()
    }

    pub(crate) const fn implies_size(self) -> bool {
        matches!(
            self,
            Self::Gb
                | Self::Gv
                | Self::Gw
                | Self::Sw
                | Self::FixedB(_)
                | Self::FixedV(_)
                | Self::OpcodeB
                | Self::OpcodeV
                | Self::EwOrV
                | Self::Mx(_)
                | Self::Pq
                | Self::Qq
                | Self::Nq
                | Self::Ed
                | Self::Gd
                | Self::Vx
                | Self::Wx
                | Self::Ux
                | Self::FixedAx
                | Self::Rd
                | Self::Cd
                | Self::Dd
                | Self::FixedSeg(_)
                | Self::St
                | Self::St0
        )
    }

    /// Whether this operand reads a ModRM byte.
    pub(crate) const fn needs_modrm(self) -> bool {
        matches!(
            self,
            Self::Eb
                | Self::Ev
                | Self::Ew
                | Self::EwOrV
                | Self::M
                | Self::Mp
                | Self::Ma
                | Self::Gb
                | Self::Gv
                | Self::Gw
                | Self::Sw
                | Self::Pq
                | Self::Qq
                | Self::Nq
                | Self::Ed
                | Self::Gd
                | Self::Vx
                | Self::Wx
                | Self::Ux
                | Self::Mx(_)
                | Self::Rd
                | Self::Cd
                | Self::Dd
                | Self::St
        )
    }
}

/// The 8-bit register names, indexed as the encoding numbers them.
pub(crate) const REG8: [&str; 8] = ["al", "cl", "dl", "bl", "ah", "ch", "dh", "bh"];
/// The 16-bit register names.
pub(crate) const REG16: [&str; 8] = ["ax", "cx", "dx", "bx", "sp", "bp", "si", "di"];
/// The 32-bit register names.
pub(crate) const REG32: [&str; 8] = ["eax", "ecx", "edx", "ebx", "esp", "ebp", "esi", "edi"];
/// The segment register names, in encoding order.
pub(crate) const SEG: [&str; 8] = ["es", "cs", "ss", "ds", "fs", "gs", "?", "?"];

/// The sixteen-bit addressing forms, indexed by ModRM r/m.
///
/// There is no SIB byte in 16-bit addressing; the eight r/m values name fixed
/// base+index pairs instead, and `[bp]` with `mod == 0` is the disp16-only form.
pub(crate) const RM16: [&str; 8] = ["bx+si", "bx+di", "bp+si", "bp+di", "si", "di", "bp", "bx"];

/// The width of an x87 memory operand, which the opcode fixes outright.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MemSize {
    /// `m16int` and the two-byte control and status words.
    Word,
    /// `m32fp` and `m32int`.
    Dword,
    /// `m64fp` and `m64int`.
    Qword,
    /// `m80fp` and `m80bcd`.
    Tbyte,
    /// No size at all — the operand's width is not a property Intel syntax
    /// names here.
    Unsized,
    /// The x87 environment block: 14 bytes in 16-bit code, 28 in 32-bit. The
    /// opcode does not fix it — the operand-size prefix does — so it is the one
    /// x87 memory operand a `66` byte genuinely changes.
    FpuEnv,
    /// The full save-state block, 94 bytes or 108 by the same rule.
    FpuState,
}

impl MemSize {
    pub(crate) const fn marker(self) -> &'static str {
        match self {
            Self::Word => "word ptr ",
            Self::Dword => "dword ptr ",
            Self::Qword => "qword ptr ",
            Self::Tbyte => "tbyte ptr ",
            Self::Unsized | Self::FpuEnv | Self::FpuState => "",
        }
    }

    /// The marker for a block whose size follows the operand-size prefix.
    ///
    /// Intel syntax names one only when the prefix is actually present: at the
    /// mode's own default the bare form already says which it is, and a keyword
    /// would be noise. When the prefix IS there it is the keyword that records
    /// it, which is why these operands never also print `data16`/`data32`.
    pub(crate) const fn mode_marker(self, prefixed: bool, size32: bool) -> &'static str {
        match (self, prefixed, size32) {
            (Self::FpuEnv, true, true) => "fpuenv28 ptr ",
            (Self::FpuEnv, true, false) => "fpuenv14 ptr ",
            (Self::FpuState, true, true) => "fpustate108 ptr ",
            (Self::FpuState, true, false) => "fpustate94 ptr ",
            _ => self.marker(),
        }
    }
}
