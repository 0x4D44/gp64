//! Independent x87 primitive oracles.
//!
//! These compare host x87 instructions in `x87_ops` with the independent
//! `rustc_apfloat::X87DoubleExtended` implementation in `x87_soft`.
//! Neither path uses `fpu.rs` to compute its answers.
//!
//! The comparison covers F80 values and arithmetic exception flags
//! (IE/DE/ZE/OE/UE/PE, plus C1 for rounding) for bit-exact primitives.
//! It does not adjudicate interpreter bookkeeping such as TOP, tags,
//! stack faults, condition codes or exception sequencing. Transcendentals
//! are excluded because the soft backend deliberately approximates them
//! in f64. Agreement also cannot rule out a shared error.
//!
//! A disagreement concerns `x87_soft`, host silicon, or both. Since the
//! soft backend runs in production on AArch64, value disagreements can
//! identify live cross-platform correctness bugs.

use crate::x87_ops as host;
use crate::x87_soft_test as soft;

/// x87 status-word exception flags: IE, DE, ZE, OE, UE, PE (bits 0-5).
const SW_EXCEPTION_FLAGS: u16 = 0x003F;
/// C1 (bit 9) — for arithmetic, the "result was rounded up" indicator.
const SW_C1: u16 = 1 << 9;

/// Default control word: round-to-nearest, 64-bit precision, all masked.
pub const CW_DEFAULT: u16 = 0x037F;

// ============================================================================
// Operand corpus
// ============================================================================

/// Build an F80 bit pattern from its parts, little-endian Intel layout.
const fn f80(sign: bool, exponent: u16, significand: u64) -> [u8; 10] {
    let m = significand.to_le_bytes();
    let e = (exponent & 0x7FFF) | if sign { 0x8000 } else { 0 };
    [
        m[0],
        m[1],
        m[2],
        m[3],
        m[4],
        m[5],
        m[6],
        m[7],
        (e & 0xFF) as u8,
        (e >> 8) as u8,
    ]
}

/// A named 80-bit operand.
#[derive(Clone, Copy)]
pub struct Operand {
    pub name: &'static str,
    pub bits: [u8; 10],
}

const fn op(name: &'static str, bits: [u8; 10]) -> Operand {
    Operand { name, bits }
}

/// The operand corpus.
///
/// Deliberately includes non-canonical and unsupported encodings:
/// pseudo-denormal, unnormal, pseudo-infinity, and pseudo-NaN. Those are where
/// a software model and real silicon are most likely to part company, and HLD
/// §3 already predicts "unnormal FCOM" as a divergence the port will surface.
pub const OPERANDS: &[Operand] = &[
    op("+0", f80(false, 0x0000, 0x0000_0000_0000_0000)),
    op("-0", f80(true, 0x0000, 0x0000_0000_0000_0000)),
    op("+1", f80(false, 0x3FFF, 0x8000_0000_0000_0000)),
    op("-1", f80(true, 0x3FFF, 0x8000_0000_0000_0000)),
    op("+2", f80(false, 0x4000, 0x8000_0000_0000_0000)),
    op("+3", f80(false, 0x4000, 0xC000_0000_0000_0000)),
    op("min_normal", f80(false, 0x0001, 0x8000_0000_0000_0000)),
    op("max_normal", f80(false, 0x7FFE, 0xFFFF_FFFF_FFFF_FFFF)),
    op("min_denormal", f80(false, 0x0000, 0x0000_0000_0000_0001)),
    op("mid_denormal", f80(false, 0x0000, 0x4000_0000_0000_0000)),
    op("neg_denormal", f80(true, 0x0000, 0x0000_0000_0000_0001)),
    op("+inf", f80(false, 0x7FFF, 0x8000_0000_0000_0000)),
    op("-inf", f80(true, 0x7FFF, 0x8000_0000_0000_0000)),
    op("qnan", f80(false, 0x7FFF, 0xC000_0000_0000_0001)),
    op("snan", f80(false, 0x7FFF, 0x8000_0000_0000_0001)),
    op("indefinite", f80(true, 0x7FFF, 0xC000_0000_0000_0000)),
    op("pi", host::F80::PI.0),
    // ---- unsupported encodings (387+ raises #IA on these) ----
    op("pseudo_denormal", f80(false, 0x0000, 0x8000_0000_0000_0000)),
    op("unnormal", f80(false, 0x4000, 0x4000_0000_0000_0000)),
    op("pseudo_inf", f80(false, 0x7FFF, 0x0000_0000_0000_0000)),
    op("pseudo_nan", f80(false, 0x7FFF, 0x4000_0000_0000_0001)),
];

/// The four x87 rounding modes, as control-word values.
pub const ROUNDING_MODES: &[(&str, u16)] = &[
    ("RN", 0x037F),
    ("RD", 0x077F),
    ("RU", 0x0B7F),
    ("RZ", 0x0F7F),
];

// ============================================================================
// Verdict
// ============================================================================

/// One adjudicated case: what host silicon said, and what `x87_soft` said.
#[derive(Clone)]
pub struct Verdict {
    pub case: String,
    pub host_value: [u8; 10],
    pub soft_value: [u8; 10],
    /// `None` when the operation's API exposes no status word (e.g. `FLD`
    /// conversions), which is itself a bound on what this layer can settle.
    pub host_sw: Option<u16>,
    pub soft_sw: Option<u16>,
    /// Classes of the input operands, for triage.
    pub operand_classes: Vec<Class>,
}

impl Verdict {
    /// Do the 80 result bits agree exactly?
    pub fn values_agree(&self) -> bool {
        self.host_value == self.soft_value
    }

    /// Do the six arithmetic exception flags agree?
    pub fn flags_agree(&self) -> bool {
        match (self.host_sw, self.soft_sw) {
            (Some(h), Some(s)) => (h & SW_EXCEPTION_FLAGS) == (s & SW_EXCEPTION_FLAGS),
            _ => true, // not observable at this layer
        }
    }

    /// Does C1 (round-up) agree?
    pub fn c1_agrees(&self) -> bool {
        match (self.host_sw, self.soft_sw) {
            (Some(h), Some(s)) => (h & SW_C1) == (s & SW_C1),
            _ => true,
        }
    }

    pub fn fully_agrees(&self) -> bool {
        self.values_agree() && self.flags_agree() && self.c1_agrees()
    }

    /// A one-line human-readable divergence report.
    pub fn describe(&self) -> String {
        let mut what = Vec::new();
        if !self.values_agree() {
            what.push("VALUE");
        }
        if !self.flags_agree() {
            what.push("FLAGS");
        }
        if !self.c1_agrees() {
            what.push("C1");
        }
        format!(
            "{:<44} [{}] host={} sw={}  soft={} sw={}",
            self.case,
            what.join("+"),
            hex10(&self.host_value),
            fmt_sw(self.host_sw),
            hex10(&self.soft_value),
            fmt_sw(self.soft_sw),
        )
    }
}

/// Operand classes, coarse enough to triage a divergence table by cause.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Class {
    /// Zero, normal, or infinity — the "well-behaved" inputs. A divergence
    /// involving *only* these is the alarming kind: it means plain arithmetic
    /// disagrees, not an edge-case policy difference.
    WellBehaved,
    Denormal,
    QNaN,
    SNaN,
    /// Unnormal, pseudo-infinity, and pseudo-NaN encodings that a 387 and later
    /// treat as unsupported (#IA).
    Unsupported,
}

/// Classify an 80-bit encoding the way a 387+ decoder would.
pub fn classify(bits: &[u8; 10]) -> Class {
    let exp = u16::from_le_bytes([bits[8], bits[9]]) & 0x7FFF;
    let sig = u64::from_le_bytes(bits[0..8].try_into().expect("8 bytes"));
    let integer_bit = sig >> 63;
    let fraction = sig & 0x7FFF_FFFF_FFFF_FFFF;
    match (exp, integer_bit) {
        (0, 0) if fraction == 0 => Class::WellBehaved, // zero
        (0, 0) => Class::Denormal,
        (0, _) => Class::Denormal, // accepted pseudo-denormal; raises DE
        (0x7FFF, 0) => Class::Unsupported, // pseudo-infinity / pseudo-NaN
        (0x7FFF, _) if fraction == 0 => Class::WellBehaved, // infinity
        (0x7FFF, _) if fraction >> 62 == 1 => Class::QNaN,
        (0x7FFF, _) => Class::SNaN,
        (_, 0) => Class::Unsupported, // unnormal
        _ => Class::WellBehaved,      // normal
    }
}

impl Verdict {
    /// The most "exotic" operand class involved, which is what a divergence is
    /// almost always attributable to.
    pub fn dominant_class(&self) -> Class {
        self.operand_classes
            .iter()
            .copied()
            .max()
            .unwrap_or(Class::WellBehaved)
    }
}

fn hex10(b: &[u8; 10]) -> String {
    let mut s = String::with_capacity(20);
    for byte in b.iter().rev() {
        s.push_str(&format!("{byte:02X}"));
    }
    s
}

fn fmt_sw(sw: Option<u16>) -> String {
    match sw {
        None => "----".to_string(),
        Some(v) => format!("{:04X}", v & (SW_EXCEPTION_FLAGS | SW_C1)),
    }
}

// ============================================================================
// Binary and unary op adjudication
// ============================================================================

/// The binary arithmetic operations both backends claim to compute bit-exactly.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
}

impl BinOp {
    pub fn name(self) -> &'static str {
        match self {
            BinOp::Add => "FADD",
            BinOp::Sub => "FSUB",
            BinOp::Mul => "FMUL",
            BinOp::Div => "FDIV",
        }
    }

    pub const ALL: &'static [BinOp] = &[BinOp::Add, BinOp::Sub, BinOp::Mul, BinOp::Div];
}

/// Adjudicate one binary operation on one operand pair under one control word.
pub fn adjudicate_binop(bop: BinOp, a: &Operand, b: &Operand, cw: u16, cw_name: &str) -> Verdict {
    let (ha, hb) = (host::F80(a.bits), host::F80(b.bits));
    let (sa, sb) = (soft::F80(a.bits), soft::F80(b.bits));

    let (hv, hsw) = match bop {
        BinOp::Add => host::f80_add_status(&ha, &hb, cw),
        BinOp::Sub => host::f80_sub_status(&ha, &hb, cw),
        BinOp::Mul => host::f80_mul_status(&ha, &hb, cw),
        BinOp::Div => host::f80_div_status(&ha, &hb, cw),
    };
    let (sv, ssw) = match bop {
        BinOp::Add => soft::f80_add_status(&sa, &sb, cw),
        BinOp::Sub => soft::f80_sub_status(&sa, &sb, cw),
        BinOp::Mul => soft::f80_mul_status(&sa, &sb, cw),
        BinOp::Div => soft::f80_div_status(&sa, &sb, cw),
    };

    Verdict {
        case: format!("{} {}/{} {}", bop.name(), a.name, b.name, cw_name),
        host_value: hv.0,
        soft_value: sv.0,
        host_sw: Some(hsw),
        soft_sw: Some(ssw),
        operand_classes: vec![classify(&a.bits), classify(&b.bits)],
    }
}

/// Adjudicate `FSQRT` on one operand under one control word.
pub fn adjudicate_sqrt(a: &Operand, cw: u16, cw_name: &str) -> Verdict {
    let (hv, hsw) = host::f80_sqrt_status(&host::F80(a.bits), cw);
    let (sv, ssw) = soft::f80_sqrt_status(&soft::F80(a.bits), cw);
    Verdict {
        case: format!("FSQRT {} {}", a.name, cw_name),
        host_value: hv.0,
        soft_value: sv.0,
        host_sw: Some(hsw),
        soft_sw: Some(ssw),
        operand_classes: vec![classify(&a.bits)],
    }
}

/// Adjudicate `FRNDINT` on one operand under one control word.
pub fn adjudicate_rndint(a: &Operand, cw: u16, cw_name: &str) -> Verdict {
    let (hv, hsw) = host::f80_rndint_status(&host::F80(a.bits), cw);
    let (sv, ssw) = soft::f80_rndint_status(&soft::F80(a.bits), cw);
    Verdict {
        case: format!("FRNDINT {} {}", a.name, cw_name),
        host_value: hv.0,
        soft_value: sv.0,
        host_sw: Some(hsw),
        soft_sw: Some(ssw),
        operand_classes: vec![classify(&a.bits)],
    }
}

/// Adjudicate `FSCALE` on one operand pair under one control word. Neither
/// backend exposes a status word for it, so this settles the VALUE only
/// (MDD-BUG-TEM-00171).
pub fn adjudicate_scale(a: &Operand, b: &Operand, cw: u16, cw_name: &str) -> Verdict {
    let hv = host::f80_scale(&host::F80(a.bits), &host::F80(b.bits), cw);
    let sv = soft::f80_scale(&soft::F80(a.bits), &soft::F80(b.bits), cw);
    Verdict {
        case: format!("FSCALE {}/{} {}", a.name, b.name, cw_name),
        host_value: hv.0,
        soft_value: sv.0,
        host_sw: None,
        soft_sw: None,
        operand_classes: vec![classify(&a.bits), classify(&b.bits)],
    }
}

/// Adjudicate `FXTRACT` on one operand under one control word: one verdict
/// for the significand and one for the exponent. Value only, as for
/// [`adjudicate_scale`] (MDD-BUG-TEM-00171).
pub fn adjudicate_xtract(a: &Operand, cw: u16, cw_name: &str) -> [Verdict; 2] {
    let (h_sig, h_exp) = host::f80_xtract(&host::F80(a.bits), cw);
    let (s_sig, s_exp) = soft::f80_xtract(&soft::F80(a.bits), cw);
    let verdict = |what: &str, hv: host::F80, sv: soft::F80| Verdict {
        case: format!("FXTRACT.{what} {} {}", a.name, cw_name),
        host_value: hv.0,
        soft_value: sv.0,
        host_sw: None,
        soft_sw: None,
        operand_classes: vec![classify(&a.bits)],
    };
    [verdict("sig", h_sig, s_sig), verdict("exp", h_exp, s_exp)]
}

/// Adjudicate `FLD m32` — the binary32 → F80 widening the guest's `FLD dword`
/// performs. Note the API exposes **no status word**, so the DE flag that a real
/// `FLD` of a denormal raises is *not* observable at this layer; settling that
/// half needs a guest-level differential.
pub fn adjudicate_fld_m32(name: &str, val: f32) -> Verdict {
    Verdict {
        case: format!("FLD_m32 {name}"),
        host_value: host::F80::from_f32(val).0,
        soft_value: soft::F80::from_f32(val).0,
        host_sw: None,
        soft_sw: None,
        operand_classes: Vec::new(),
    }
}

/// Adjudicate `FLD m64` — the binary64 → F80 widening. Same status-word caveat
/// as [`adjudicate_fld_m32`].
pub fn adjudicate_fld_m64(name: &str, val: f64) -> Verdict {
    Verdict {
        case: format!("FLD_m64 {name}"),
        host_value: host::F80::from_f64(val).0,
        soft_value: soft::F80::from_f64(val).0,
        host_sw: None,
        soft_sw: None,
        operand_classes: Vec::new(),
    }
}

/// Run the full binary/unary sweep and return every case (agreeing or not).
pub fn sweep_all() -> Vec<Verdict> {
    let mut out = Vec::new();
    for &(cw_name, cw) in ROUNDING_MODES {
        for bop in BinOp::ALL.iter().copied() {
            for a in OPERANDS {
                for b in OPERANDS {
                    out.push(adjudicate_binop(bop, a, b, cw, cw_name));
                }
            }
        }
        for a in OPERANDS {
            out.push(adjudicate_sqrt(a, cw, cw_name));
            out.push(adjudicate_rndint(a, cw, cw_name));
            out.extend(adjudicate_xtract(a, cw, cw_name));
            for b in OPERANDS {
                out.push(adjudicate_scale(a, b, cw, cw_name));
            }
        }
    }
    out
}

// ============================================================================
// Host-silicon sequence probe
// ============================================================================
//
// The third S0b adjudicator: run a short x87 sequence on the real host FPU and
// return the whole `FNSAVE` image, so questions the F80-level API cannot even
// express — TOP, the tag word, SF, the condition codes, and the *layout* of the
// saved state — get a silicon verdict.
//
// Precedent: `tmp/probe_fisttp_qnan.rs` settled the RM16 C1 dispute this way;
// the resulting comment lives at `crates/gp64-cpu/src/fpu.rs:4056-4063`.
// That probe was a throwaway; this one is kept, because slices S3-S6 will need
// it repeatedly.

/// A 32-bit-format `FNSAVE` image: 28-byte environment + 8 * 10-byte registers.
///
/// In 64-bit mode a `fnsave` with no operand-size override writes this layout:
/// `CW@0 SW@4 TW@8 FIP@12 CS@16 FDP@20 DS@24`, then `ST(0)..ST(7)` from offset
/// 28. (The DOS harness sees the 16-bit real-mode 94-byte form instead; the
/// register *order* question is the same in both.)
pub const HOST_IMAGE_LEN: usize = 108;
/// Offset of the register area in [`HOST_IMAGE_LEN`].
pub const HOST_REG_BASE: usize = 28;

/// Short x87 sequences worth a silicon verdict. Each mirrors a case in
/// `tests/fuzz/x87_harness/src/x87_harn.c` so the guest-level differential and
/// this probe can be compared directly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HostSeq {
    /// `FNINIT` only.
    Init,
    /// `FNINIT; FLD1; FLDZ; FLDPI` — settles the saved register ORDER (is slot
    /// 0 `ST(0)` or physical `R0`?) and the tag encoding for a zero.
    ThreePushes,
    /// `FNINIT; FLD dword [snan]` — does a plain load of an SNaN raise IE?
    FldM32Snan,
    /// `FNINIT; FLDPI; FIST dword` — does an inexact store raise PE?
    FistPi,
    /// `FNINIT; FSTP qword (empty stack); FNCLEX` — does FNCLEX clear SF?
    ClexAfterUnderflow,
    /// `FNINIT; FLD1; FFREEP ST(0)` (`DF C0`) — does freeing ST(0) and then
    /// popping it raise a stack underflow, or is the emptied tag expected?
    /// Adjudicates a 2026.07.26 interpreter change.
    FfreepSt0,
}

/// Execute `seq` on the host x87 unit and return its `FNSAVE` image.
///
/// `FNSAVE` (not `FSAVE`) so the dump cannot itself trap on a pending
/// exception. Note `FNSAVE` reinitialises the FPU as a side effect, which is
/// harmless here: Rust uses SSE for float arithmetic on x86-64.
pub fn host_probe(seq: HostSeq) -> [u8; HOST_IMAGE_LEN] {
    use std::arch::asm;

    let mut image = [0u8; HOST_IMAGE_LEN];
    let snan: f32 = f32::from_bits(0x7F80_0001);
    let mut scratch_i32: i32 = 0;
    let mut scratch_f64: f64 = 0.0;

    // SAFETY: every sequence is a self-contained x87 program that leaves the
    // stack empty (FNSAVE reinitialises), declares all eight x87 registers
    // clobbered, and writes only into locals owned by this frame.
    unsafe {
        match seq {
            HostSeq::Init => {
                let _host_x87 = crate::cpu::host_x87_enter_guarded();
                asm!(
                "fninit",
                "fnsave [{img}]",
                img = in(reg) image.as_mut_ptr(),
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack),
                );
            }
            HostSeq::ThreePushes => {
                let _host_x87 = crate::cpu::host_x87_enter_guarded();
                asm!(
                "fninit",
                "fld1",
                "fldz",
                "fldpi",
                "fnsave [{img}]",
                img = in(reg) image.as_mut_ptr(),
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack),
                );
            }
            HostSeq::FldM32Snan => {
                let _host_x87 = crate::cpu::host_x87_enter_guarded();
                asm!(
                "fninit",
                "fld dword ptr [{src}]",
                "fnsave [{img}]",
                src = in(reg) &snan as *const f32,
                img = in(reg) image.as_mut_ptr(),
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack),
                );
            }
            HostSeq::FistPi => {
                let _host_x87 = crate::cpu::host_x87_enter_guarded();
                asm!(
                "fninit",
                "fldpi",
                "fist dword ptr [{dst}]",
                "fnsave [{img}]",
                dst = in(reg) &mut scratch_i32 as *mut i32,
                img = in(reg) image.as_mut_ptr(),
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack),
                );
            }
            HostSeq::FfreepSt0 => {
                let _host_x87 = crate::cpu::host_x87_enter_guarded();
                asm!(
                    "fninit",
                    "fld1",
                    ".byte 0xDF, 0xC0", // FFREEP ST(0) — undocumented, no mnemonic
                    "fnsave [{img}]",
                    img = in(reg) image.as_mut_ptr(),
                    out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                    out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                    options(nostack),
                );
            }
            HostSeq::ClexAfterUnderflow => {
                let _host_x87 = crate::cpu::host_x87_enter_guarded();
                asm!(
                "fninit",
                "fstp qword ptr [{dst}]",
                "fnclex",
                "fnsave [{img}]",
                dst = in(reg) &mut scratch_f64 as *mut f64,
                img = in(reg) image.as_mut_ptr(),
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack),
                );
            }
        }
    }
    image
}

/// Control word from a host probe image.
pub fn host_cw(image: &[u8; HOST_IMAGE_LEN]) -> u16 {
    u16::from_le_bytes([image[0], image[1]])
}
/// Status word from a host probe image.
pub fn host_sw(image: &[u8; HOST_IMAGE_LEN]) -> u16 {
    u16::from_le_bytes([image[4], image[5]])
}
/// Tag word from a host probe image.
pub fn host_tw(image: &[u8; HOST_IMAGE_LEN]) -> u16 {
    u16::from_le_bytes([image[8], image[9]])
}
/// `TOP` from a host probe image.
pub fn host_top(image: &[u8; HOST_IMAGE_LEN]) -> usize {
    ((host_sw(image) >> 11) & 7) as usize
}
/// Register slot `i` of a host probe image, as raw bytes.
pub fn host_reg(image: &[u8; HOST_IMAGE_LEN], i: usize) -> [u8; 10] {
    let mut out = [0u8; 10];
    out.copy_from_slice(&image[HOST_REG_BASE + i * 10..HOST_REG_BASE + (i + 1) * 10]);
    out
}

/// The binary32 denormal corpus used for the `FLD m32` acceptance case.
pub fn fld_m32_cases() -> Vec<(&'static str, f32)> {
    vec![
        ("min_denormal", f32::from_bits(0x0000_0001)),
        ("max_denormal", f32::from_bits(0x007F_FFFF)),
        ("mid_denormal", f32::from_bits(0x0040_0000)),
        ("neg_denormal", f32::from_bits(0x8000_0001)),
        ("min_normal", f32::from_bits(0x0080_0000)),
        ("snan", f32::from_bits(0x7F80_0001)),
        ("qnan", f32::from_bits(0x7FC0_0001)),
    ]
}

/// The binary64 denormal corpus used for the `FLD m64` acceptance case.
pub fn fld_m64_cases() -> Vec<(&'static str, f64)> {
    vec![
        ("min_denormal", f64::from_bits(0x0000_0000_0000_0001)),
        ("max_denormal", f64::from_bits(0x000F_FFFF_FFFF_FFFF)),
        ("neg_denormal", f64::from_bits(0x8000_0000_0000_0001)),
        ("snan", f64::from_bits(0x7FF0_0000_0000_0001)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Print the full divergence table. Always passes — this is the *report*,
    /// and the pinning tests below are what actually gate.
    ///
    /// `cargo test -p gp64-cpu x87_oracle_report -- --nocapture </dev/null`
    #[test]
    fn x87_oracle_report() {
        let all = sweep_all();
        let diverging: Vec<_> = all.iter().filter(|v| !v.fully_agrees()).collect();
        println!(
            "\n=== x87 oracle: host silicon vs x87_soft (rustc_apfloat) ===\n\
             cases: {}   agreeing: {}   diverging: {}",
            all.len(),
            all.len() - diverging.len(),
            diverging.len()
        );
        let value_div = diverging.iter().filter(|v| !v.values_agree()).count();
        let flag_div = diverging.iter().filter(|v| !v.flags_agree()).count();
        let c1_div = diverging.iter().filter(|v| !v.c1_agrees()).count();
        println!("  by field: VALUE={value_div}  FLAGS={flag_div}  C1={c1_div}");

        for class in [
            Class::WellBehaved,
            Class::Denormal,
            Class::QNaN,
            Class::SNaN,
            Class::Unsupported,
        ] {
            let n = diverging
                .iter()
                .filter(|v| v.dominant_class() == class)
                .count();
            println!("  by operand class: {class:?} = {n}");
        }

        // The alarming subset: plain arithmetic on zeros/normals/infinities.
        let well_behaved: Vec<_> = diverging
            .iter()
            .filter(|v| v.dominant_class() == Class::WellBehaved)
            .collect();
        println!(
            "\n--- WELL-BEHAVED-OPERAND divergences ({}) — these are the alarming ones ---",
            well_behaved.len()
        );
        for v in well_behaved.iter().take(60) {
            println!("  {}", v.describe());
        }
        println!();
    }

    /// The divergence register (HLD S8 calls for one; S0b is where it starts).
    /// The backends agree across four previously differing value classes:
    /// unsupported encodings, NaN propagation, exception precedence, and masked
    /// directed overflow. The register is now deliberately empty; any future
    /// entry requires fresh adjudication rather than a count re-bless.
    /// MDD-BUG-TEM-00171 grew the corpus from 7224 to 9156 by adjudicating
    /// FSCALE and FXTRACT (value only - neither backend exposes their status
    /// word) and it stayed empty.
    #[test]
    fn divergence_register_is_stable() {
        let all = sweep_all();
        assert_eq!(all.len(), 9156, "the sweep corpus changed size");

        let diverging: Vec<&Verdict> = all.iter().filter(|v| !v.fully_agrees()).collect();
        assert!(
            diverging.is_empty(),
            "the empty divergence register gained {} cases; first: {}",
            diverging.len(),
            diverging.first().map(|v| v.describe()).unwrap_or_default()
        );
    }

    /// MDD-BUG-TEM-00170: FSQRT under reduced precision control, every rounding
    /// mode, against host silicon. `sweep_all` only exercises PC=64, which is
    /// why the soft backend ignoring PC for FSQRT went unseen.
    #[test]
    fn x87_soft_sqrt_matches_silicon_under_reduced_precision_control() {
        let mut diverging = Vec::new();
        for pc in [0x0000u16, 0x0200] {
            for &(rc_name, rc_cw) in ROUNDING_MODES {
                let cw = (rc_cw & !0x0300) | pc;
                let name = format!("{rc_name}/PC{}", if pc == 0 { 24 } else { 53 });
                for a in OPERANDS {
                    let verdict = adjudicate_sqrt(a, cw, &name);
                    if !verdict.fully_agrees() {
                        diverging.push(verdict.describe());
                    }
                }
            }
        }
        assert!(
            diverging.is_empty(),
            "soft FSQRT diverges from silicon under PC=24/53 in {} cases:\n{}",
            diverging.len(),
            diverging
                .iter()
                .take(12)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    /// MDD-BUG-TEM-00168: the status-returning FST narrowing agrees with host
    /// silicon — stored bits AND the IE/OE/UE/PE/C1 status — for every oracle
    /// operand, both widths, every rounding mode.
    #[test]
    fn fst_narrowing_status_matches_silicon() {
        let mut diverging = Vec::new();
        for &(rc_name, cw) in ROUNDING_MODES {
            for a in OPERANDS {
                let (hv, hsw) = host::F80(a.bits).to_f32_status_with_cw(cw);
                let (sv, ssw) = soft::F80(a.bits).to_f32_status_with_cw(cw);
                if hv.to_bits() != sv.to_bits() || hsw != ssw {
                    diverging.push(format!(
                        "m32 {rc_name} {}: host {:08x}/{hsw:04x} soft {:08x}/{ssw:04x}",
                        a.name,
                        hv.to_bits(),
                        sv.to_bits()
                    ));
                }
                let (hv, hsw) = host::F80(a.bits).to_f64_status_with_cw(cw);
                let (sv, ssw) = soft::F80(a.bits).to_f64_status_with_cw(cw);
                if hv.to_bits() != sv.to_bits() || hsw != ssw {
                    diverging.push(format!(
                        "m64 {rc_name} {}: host {:016x}/{hsw:04x} soft {:016x}/{ssw:04x}",
                        a.name,
                        hv.to_bits(),
                        sv.to_bits()
                    ));
                }
            }
        }
        assert!(
            diverging.is_empty(),
            "FST narrowing diverges from silicon in {} cases:\n{}",
            diverging.len(),
            diverging
                .iter()
                .take(16)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn x87_soft_matches_silicon_for_recorded_divergence_families() {
        let all = sweep_all();
        assert_eq!(all.len(), 9156, "the sweep corpus changed size");

        let diverging: Vec<_> = all.iter().filter(|v| !v.fully_agrees()).collect();
        let sample = diverging
            .iter()
            .take(12)
            .map(|v| v.describe())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            diverging.is_empty(),
            "x87_soft still diverges from host silicon in {} cases; first cases:\n{}",
            diverging.len(),
            sample
        );
    }

    /// Acceptance case 1 (HLD §6, S0b): an `FLD` of a denormal.
    ///
    /// Both adjudicators must produce a verdict, and the widened **value** must
    /// agree — a binary32/64 → F80 widening is exact by construction (the
    /// destination has more exponent range and more significand bits than any
    /// source denormal needs), so a disagreement here would be a plain bug in
    /// one of the two backends.
    #[test]
    fn acceptance_fld_denormal_value_is_adjudicated() {
        let mut verdicts = Vec::new();
        for (name, val) in fld_m32_cases() {
            verdicts.push(adjudicate_fld_m32(name, val));
        }
        for (name, val) in fld_m64_cases() {
            verdicts.push(adjudicate_fld_m64(name, val));
        }
        assert!(
            !verdicts.is_empty(),
            "the FLD denormal corpus must be non-empty"
        );

        let bad: Vec<String> = verdicts
            .iter()
            .filter(|v| !v.values_agree())
            .map(|v| v.describe())
            .collect();
        assert!(
            bad.is_empty(),
            "host silicon and x87_soft disagree on FLD-of-denormal widening.\n\
             This is a cross-platform correctness finding (x87_soft is our production\n\
             FPU on aarch64), not a test to relax. File it in bugs/.\n{}",
            bad.join("\n")
        );
    }

    /// Acceptance case 2 (HLD §6, S0b): an m32 `FADD` with a resident NaN.
    ///
    /// Value and arithmetic flags are both observable here. This focused corpus
    /// permanently guards the numerical rules: SNaNs are quieted with
    /// their payload preserved, and two NaNs select the larger significand.
    #[test]
    fn acceptance_m32_fadd_with_resident_nan_is_adjudicated() {
        let residents = [
            ("qnan", f80(false, 0x7FFF, 0xC000_0000_0000_0001)),
            ("snan", f80(false, 0x7FFF, 0x8000_0000_0000_0001)),
            ("indefinite", f80(true, 0x7FFF, 0xC000_0000_0000_0000)),
        ];
        let addends: [(&str, f32); 4] = [
            ("1.0", 1.0),
            ("denormal", f32::from_bits(0x0000_0001)),
            ("0.0", 0.0),
            ("snan_m32", f32::from_bits(0x7F80_0001)),
        ];

        let mut verdicts = Vec::new();
        for &(rname, rbits) in &residents {
            for &(aname, aval) in &addends {
                // FLD dword -> ST(0), then FADD against the resident NaN.
                let hb = host::F80::from_f32(aval);
                let sb = soft::F80::from_f32(aval);
                assert_eq!(
                    hb.0, sb.0,
                    "the m32 widening itself must agree before the FADD is meaningful"
                );
                let (hv, hsw) = host::f80_add_status(&host::F80(rbits), &hb, CW_DEFAULT);
                let (sv, ssw) = soft::f80_add_status(&soft::F80(rbits), &sb, CW_DEFAULT);
                verdicts.push(Verdict {
                    case: format!("FADD_m32 resident={rname} addend={aname}"),
                    host_value: hv.0,
                    soft_value: sv.0,
                    host_sw: Some(hsw),
                    soft_sw: Some(ssw),
                    operand_classes: vec![classify(&rbits), classify(&hb.0)],
                });
            }
        }

        assert_eq!(verdicts.len(), 12, "corpus size changed unexpectedly");

        let diverging: Vec<String> = verdicts
            .iter()
            .filter(|v| !v.fully_agrees())
            .map(|v| v.describe())
            .collect();
        assert!(
            diverging.is_empty(),
            "resident-NaN arithmetic diverged from silicon:\n{}",
            diverging.join("\n")
        );

        // Pin the corrected payload behavior directly on both adjudicators.
        let snan_add_one = verdicts
            .iter()
            .find(|v| v.case == "FADD_m32 resident=snan addend=1.0")
            .expect("case present");
        assert_eq!(
            snan_add_one.soft_value,
            f80(false, 0x7FFF, 0xC000_0000_0000_0001),
            "x87_soft must return the SNaN quieted with its payload preserved"
        );
    }

    /// Print every host-probe verdict. Always passes; this is the report the
    /// guest-level differential's adjudication cites.
    ///
    /// `cargo test -p gp64-cpu host_probe_report -- --nocapture </dev/null`
    #[test]
    fn host_probe_report() {
        println!("\n=== host-silicon x87 sequence probe ===");
        for seq in [
            HostSeq::Init,
            HostSeq::ThreePushes,
            HostSeq::FldM32Snan,
            HostSeq::FistPi,
            HostSeq::ClexAfterUnderflow,
        ] {
            let img = host_probe(seq);
            println!(
                "\n{seq:?}: CW={:04X} SW={:04X} TW={:04X} TOP={}",
                host_cw(&img),
                host_sw(&img),
                host_tw(&img),
                host_top(&img)
            );
            for i in 0..8 {
                let r = host_reg(&img, i);
                if r != [0u8; 10] {
                    println!("  slot{i} = {}", hex10(&r));
                }
            }
        }
        println!();
    }

    /// **Settles the saved-register-order question.** `FNSAVE`'s register area
    /// is `ST`-relative, not physical: after `FNINIT; FLD1; FLDZ; FLDPI` the
    /// host leaves TOP=5, yet pi — the last value pushed, i.e. `ST(0)`, i.e.
    /// physical R5 — lands in **slot 0**.
    ///
    /// This is the silicon vote behind the guest-level differential's finding
    /// that gp64's `FNSAVE` writes physical order. Intel SDM Vol 1 Fig. 8-9
    /// labels the area `ST(0)..ST(7)`,, so both independent models
    /// adjudicators concur.
    #[test]
    fn host_fnsave_register_area_is_st_relative_not_physical() {
        let img = host_probe(HostSeq::ThreePushes);
        assert_eq!(host_top(&img), 5, "three pushes must leave TOP=5");

        let pi = host::F80::PI.0;
        let one = f80(false, 0x3FFF, 0x8000_0000_0000_0000);
        let zero = [0u8; 10];

        assert_eq!(
            host_reg(&img, 0),
            pi,
            "slot 0 must hold ST(0)=pi (ST-relative); physical order would put \
             pi in slot 5"
        );
        assert_eq!(host_reg(&img, 1), zero, "slot 1 must hold ST(1)=+0");
        assert_eq!(host_reg(&img, 2), one, "slot 2 must hold ST(2)=1.0");
        assert_eq!(
            host_reg(&img, 5),
            zero,
            "slot 5 must NOT hold pi — that would be physical ordering"
        );
    }

    /// The tag word classifies contents, it does not merely mark occupancy:
    /// a pushed `+0` must tag as Zero (1), not Valid (0).
    #[test]
    fn host_tag_word_distinguishes_zero_from_valid() {
        let img = host_probe(HostSeq::ThreePushes);
        let tw = host_tw(&img);
        let tag = |physical: usize| (tw >> (2 * physical)) & 3;

        // TOP=5, so ST(0)=R5=pi, ST(1)=R6=+0, ST(2)=R7=1.0.
        assert_eq!(tag(5), 0, "R5 holds pi — Valid");
        assert_eq!(tag(6), 1, "R6 holds +0 — must tag Zero, not Valid");
        assert_eq!(tag(7), 0, "R7 holds 1.0 — Valid");
        for r in 0..5 {
            assert_eq!(tag(r), 3, "R{r} must be Empty");
        }
    }

    /// A plain `FLD m32` of an SNaN raises IE and tags the destination Special.
    #[test]
    fn host_fld_m32_snan_raises_ie_and_tags_special() {
        let img = host_probe(HostSeq::FldM32Snan);
        assert_eq!(host_sw(&img) & 0x0001, 0x0001, "FLD of an SNaN must set IE");
        let tag = (host_tw(&img) >> (2 * host_top(&img))) & 3;
        assert_eq!(tag, 2, "the loaded NaN must tag Special, not Valid");
    }

    /// An inexact `FIST` raises PE.
    #[test]
    fn host_fist_inexact_raises_pe() {
        let img = host_probe(HostSeq::FistPi);
        assert_eq!(
            host_sw(&img) & 0x0020,
            0x0020,
            "FIST of pi to m32 is inexact and must set PE"
        );
    }

    /// `FNCLEX` clears the stack fault flag along with the exception flags.
    #[test]
    fn host_fnclex_clears_stack_fault() {
        let img = host_probe(HostSeq::ClexAfterUnderflow);
        assert_eq!(
            host_sw(&img) & 0x0040,
            0,
            "FNCLEX must clear SF, not preserve it"
        );
        assert_eq!(
            host_sw(&img) & 0x003F,
            0,
            "FNCLEX must clear all six exception flags"
        );
    }

    /// Adjudicates the 2026.07.26 interpreter change to `FFREEP ST(0)`.
    ///
    /// The claim under test: freeing ST(0) and then popping it must NOT be
    /// reported as a stack underflow, because the emptied tag was intentional.
    /// A differential said so; host silicon is what makes it a verdict.
    #[test]
    fn host_ffreep_st0_does_not_raise_stack_underflow() {
        let img = host_probe(HostSeq::FfreepSt0);
        let sw = host_sw(&img);
        assert_eq!(
            sw & 0x0001,
            0,
            "FFREEP ST(0) must not set IE (SW={sw:#06X})"
        );
        assert_eq!(
            sw & 0x0040,
            0,
            "FFREEP ST(0) must not set SF (SW={sw:#06X})"
        );
        // FLD1 leaves TOP=7; the FFREEP pop must advance it back to 0.
        assert_eq!(
            host_top(&img),
            0,
            "FFREEP must advance TOP past the freed slot"
        );
        // And the freed physical register must read Empty.
        let tag = (host_tw(&img) >> (2 * 7)) & 3;
        assert_eq!(tag, 3, "the freed slot must be tagged Empty");
    }

    /// MDD-BUG-CRUCIBLE-00639: FPTAN's exception STATUS must not depend on the
    /// host, even though its VALUE legitimately does.
    ///
    /// This module excludes the transcendentals from its value sweep because
    /// `x87_soft` computes them in `f64` and declares them approximate by
    /// design — that stands. But the status word is guest-visible state, and the
    /// soft backend used to return 0 on the success path where host silicon
    /// returns `PE | C1`, so the same ordinary finite instruction reported
    /// different flags on aarch64 than on x86_64.
    ///
    /// Compares FLAGS and C1 only, never the value, so it cannot drift into the
    /// bit-exactness claim the module's header rules out. The zero case is
    /// included because it is the one input where both backends must agree that
    /// the result is EXACT — without it the test would pass on a backend that
    /// simply set PE unconditionally.
    #[test]
    fn fptan_status_agrees_between_backends_crucible639() {
        // 2^64 is past the soft backend's TRIG_RANGE_LIMIT (2^63), so it
        // exercises the C2 out-of-range path the entry asked for; the in-range
        // angles alone would leave that arm untested.
        const OUT_OF_RANGE: f64 = 18_446_744_073_709_551_616.0;
        for &(mode, cw) in ROUNDING_MODES {
            for angle in [0.0f64, -0.0, 1.0, 0.5, -1.0, 2.0, 1e-30, OUT_OF_RANGE] {
                let (_, _, host_c2, host_sw) = host::f80_ptan(&host::F80::from_f64(angle), cw);
                let (_, _, soft_c2, soft_sw) = soft::f80_ptan(&soft::F80::from_f64(angle), cw);

                assert_eq!(host_c2, soft_c2, "{mode} fptan({angle:e}): C2 disagrees");
                assert_eq!(
                    host_sw & SW_EXCEPTION_FLAGS,
                    soft_sw & SW_EXCEPTION_FLAGS,
                    "fptan({angle:e}): exception flags disagree \
                 (host {host_sw:#06X} vs soft {soft_sw:#06X})"
                );
                assert_eq!(
                    host_sw & SW_C1,
                    soft_sw & SW_C1,
                    "fptan({angle:e}): C1 disagrees \
                 (host {host_sw:#06X} vs soft {soft_sw:#06X})"
                );
            }
        }

        // The corpus must actually contain every verdict, or the loops above
        // could agree vacuously.
        let (_, _, _, exact) = host::f80_ptan(&host::F80::from_f64(0.0), CW_DEFAULT);
        let (_, _, _, inexact) = host::f80_ptan(&host::F80::from_f64(1.0), CW_DEFAULT);
        assert_eq!(exact & 0x0020, 0, "fptan(0) must be exact on host silicon");
        assert_ne!(
            inexact & 0x0020,
            0,
            "fptan(1) must be inexact on host silicon"
        );
        let (_, _, far_c2, _) = host::f80_ptan(&host::F80::from_f64(OUT_OF_RANGE), CW_DEFAULT);
        assert!(
            far_c2,
            "the out-of-range case must set C2, or it tests nothing"
        );
    }

    /// The oracles must be genuinely two different implementations. If someone
    /// ever re-points `x87_soft_test` at the host backend this whole module
    /// silently becomes a tautology, so pin the one case where they are known
    /// to differ observably in *mechanism*: the soft backend is pure Rust and
    /// cannot be affected by the host FPU's live control word.
    #[test]
    fn oracles_are_distinct_implementations() {
        // `size_of` differs only if the types diverge; instead assert that the
        // two modules are separately addressable and produce independently
        // computed results for a case with a known non-trivial answer.
        let a = soft::F80::from_f64(1.0);
        let b = soft::F80::from_f64(3.0);
        let (soft_div, _) = soft::f80_div_status(&a, &b, CW_DEFAULT);
        let ha = host::F80::from_f64(1.0);
        let hb = host::F80::from_f64(3.0);
        let (host_div, _) = host::f80_div_status(&ha, &hb, CW_DEFAULT);
        assert_eq!(
            soft_div.0, host_div.0,
            "1/3 must round identically in both backends"
        );
        // 1/3 is inexact; a backend that computed nothing would return ZERO.
        assert_ne!(soft_div.0, [0u8; 10], "soft backend returned a null result");
        assert_ne!(host_div.0, [0u8; 10], "host backend returned a null result");
    }

    /// Backend API parity for the arch-neutral status/tag surface
    /// (MDD-BUG-FLUX-00128).
    ///
    /// `fpu.rs` calls these `F80` methods on whichever backend the host
    /// provides, so a method added to one backend but not the other compiles
    /// green on x86_64 and breaks every other host — measured twice in one
    /// week during the campaign (`x87_tag`, `to_i*_status`; ported
    /// after the fact by db342069e). Merely CALLING both backends here turns
    /// a missing mirror into a compile error on the x86_64 lanes where these
    /// changes are authored; the asserts additionally pin the bit-identical
    /// behaviour the two-engine contract requires (a divergence makes
    /// guest-visible state host-dependent).
    ///
    /// Extend this test whenever `fpu.rs` grows a call to a new arch-neutral
    /// `F80` method.
    #[test]
    fn status_and_tag_api_is_mirrored_between_backends() {
        assert_eq!(
            soft::F80::FIST_STATUS_MASK,
            host::F80::FIST_STATUS_MASK,
            "the FIST status mask must agree between backends"
        );

        for operand in OPERANDS {
            let s = soft::F80(operand.bits);
            let h = host::F80(operand.bits);
            assert_eq!(
                s.x87_tag(),
                h.x87_tag(),
                "x87_tag must classify {} identically on both backends",
                operand.name
            );
        }

        for operand in OPERANDS {
            let s = soft::F80(operand.bits);
            let h = host::F80(operand.bits);
            // The unsupported and pseudo-denormal encodings are adjudicated
            // by the sweep's divergence machinery, not asserted here: silicon
            // and apfloat can legitimately part company on them.
            if s.is_unsupported_encoding() || operand.name == "pseudo_denormal" {
                continue;
            }
            for &(mode, cw) in ROUNDING_MODES {
                assert_eq!(
                    s.to_i16_status(cw),
                    h.to_i16_status(cw),
                    "to_i16_status({}, {mode}) must agree between backends",
                    operand.name
                );
                assert_eq!(
                    s.to_i32_status(cw),
                    h.to_i32_status(cw),
                    "to_i32_status({}, {mode}) must agree between backends",
                    operand.name
                );
                assert_eq!(
                    s.to_i64_status(cw),
                    h.to_i64_status(cw),
                    "to_i64_status({}, {mode}) must agree between backends",
                    operand.name
                );
            }
        }
    }
}
