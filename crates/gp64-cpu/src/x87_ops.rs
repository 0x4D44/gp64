//! F80 extended precision type and x87 inline asm operation wrappers.
//!
//! This module provides:
//! - `F80`: a thin wrapper over 10 bytes in Intel 80-bit extended precision format
//! - x87 operation functions using inline assembly for bit-exact hardware results
//!
//! All functions are `#[cfg(target_arch = "x86_64")]` since they require x87 asm.
#![cfg(target_arch = "x86_64")]
use crate::cpu::host_x87_enter_guarded;
use core::arch::asm;
/// Mask all x87 exceptions in a guest control word before loading it into
/// the host FPU via `FLDCW`.  The guest's exception masking (bits 0-5) must
/// never reach the real hardware — unmasked exceptions cause SIGFPE on the
/// host.  Rounding mode (bits 10-11) and precision control (bits 8-9) are
/// preserved.
#[inline(always)]
fn host_safe_cw(cw: u16) -> u16 {
    cw | 0x3F
}
/// Default x87 control word: round-to-nearest, extended precision, all exceptions masked.
/// Used by no-CW functions to ensure the host FPU doesn't raise SIGFPE when the guest
/// has set a dangerous control word (e.g. FLDCW with unmasked exceptions).
const SAFE_DEFAULT_CW: u16 = 0x037F;
/// [`SAFE_DEFAULT_CW`] with RC (bits 10-11) set to round-toward-zero.
///
/// Used by the portable truncation fallback: `FISTP` honours the control
/// word's rounding mode, so with RC=11 it matches `FISTTP`, which always
/// truncates regardless of RC.
const SAFE_TRUNCATE_CW: u16 = SAFE_DEFAULT_CW | 0x0C00;
/// Whether this host can execute `FISTTP`.
///
/// `FISTTP` is SSE3-era. Generic `x86_64` targets do not guarantee SSE3, and
/// this module is selected for every `x86_64` host, so emitting it
/// unconditionally could execute an unsupported instruction rather than
/// emulate the guest (MDD-BUG-CRUCIBLE-00260). Inline asm is passed straight
/// to the assembler and is not checked against the target's feature set, so
/// the gate has to be a runtime one.
///
/// When the target already guarantees SSE3,
/// the `cfg!` folds to a constant and the probe disappears entirely.
#[inline(always)]
fn host_has_fisttp() -> bool {
    cfg!(target_feature = "sse3") || std::is_x86_feature_detected!("sse3")
}
/// Intel 80-bit extended precision floating point value.
///
/// Stored in little-endian Intel format:
/// - bytes 0-7: 64-bit significand (with explicit integer bit)
/// - bytes 8-9: 15-bit exponent + 1-bit sign
#[derive(Clone, Copy, Eq, serde::Serialize, serde::Deserialize)]
#[allow(clippy::unsafe_derive_deserialize)]
#[repr(C, align(16))]
pub struct F80(pub [u8; 10]);
impl F80 {
    /// Positive zero
    pub const ZERO: F80 = F80([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    /// Negative zero (sign bit is bit 7 of byte 9)
    pub const NEG_ZERO: F80 = F80([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80]);
    /// Floating-point indefinite (QNaN) — Intel SDM Vol 1 §4.8.3.7.
    /// Written on masked invalid-operation exceptions (e.g., FLD of empty
    /// ST(i), FST of empty ST(0), FSTP/FIST on empty stack). Encoded as
    /// sign=1, exponent=0x7FFF, mantissa bits 63 and 62 set.
    pub const INDEFINITE: F80 = F80([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0xFF, 0xFF]);
    /// 1.0 (exponent 0x3FFF, mantissa 0x8000000000000000)
    pub const ONE: F80 = F80([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0xFF, 0x3F]);
    /// Pi
    pub const PI: F80 = F80([0x35, 0xC2, 0x68, 0x21, 0xA2, 0xDA, 0x0F, 0xC9, 0x00, 0x40]);
    /// log2(e)
    pub const LOG2_E: F80 = F80([0xBC, 0xF0, 0x17, 0x5C, 0x29, 0x3B, 0xAA, 0xB8, 0xFF, 0x3F]);
    /// log2(10)
    pub const LOG2_10: F80 = F80([0xFE, 0x8A, 0x1B, 0xCD, 0x4B, 0x78, 0x9A, 0xD4, 0x00, 0x40]);
    /// log10(2)
    pub const LOG10_2: F80 = F80([0x99, 0xF7, 0xCF, 0xFB, 0x84, 0x9A, 0x20, 0x9A, 0xFD, 0x3F]);
    /// ln(2)
    pub const LN_2: F80 = F80([0xAC, 0x79, 0xCF, 0xD1, 0xF7, 0x17, 0x72, 0xB1, 0xFE, 0x3F]);
    /// Convert to f64 via x87: FLD TBYTE PTR, FSTP QWORD PTR
    pub fn to_f64(&self) -> f64 {
        let safe_cw: u16 = SAFE_DEFAULT_CW;
        let mut result: f64 = 0.0;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fldcw word ptr [{safe}]", "fld tbyte ptr [{src}]",
                "fstp qword ptr [{dst}]", safe = in (reg) & safe_cw, src = in (reg) self
                .0.as_ptr(), dst = in (reg) & mut result as * mut f64, out("st(0)") _,
                out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
                out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
            );
        }
        result
    }
    /// Convert from f64 via x87: FLD QWORD PTR, FSTP TBYTE PTR
    pub fn from_f64(val: f64) -> F80 {
        let safe_cw: u16 = SAFE_DEFAULT_CW;
        let mut result = F80::ZERO;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fldcw word ptr [{safe}]", "fld qword ptr [{src}]",
                "fstp tbyte ptr [{dst}]", safe = in (reg) & safe_cw, src = in (reg) & val
                as * const f64, dst = in (reg) result.0.as_mut_ptr(), out("st(0)") _,
                out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
                out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
            );
        }
        result
    }
    /// Convert from f32 via x87: FLD DWORD PTR, FSTP TBYTE PTR
    pub fn from_f32(val: f32) -> F80 {
        Self::from_f32_via_host_x87(val)
    }
    /// Edge-case conversion oracle retained for inputs that need host x87's
    /// NaN/subnormal behavior and for byte-level tests of the binary32 fast path.
    fn from_f32_via_host_x87(val: f32) -> F80 {
        let safe_cw: u16 = SAFE_DEFAULT_CW;
        let mut result = F80::ZERO;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fldcw word ptr [{safe}]", "fld dword ptr [{src}]",
                "fstp tbyte ptr [{dst}]", safe = in (reg) & safe_cw, src = in (reg) & val
                as * const f32, dst = in (reg) result.0.as_mut_ptr(), out("st(0)") _,
                out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
                out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
            );
        }
        result
    }
    /// Convert to f32 via x87: FLD TBYTE PTR, FSTP DWORD PTR
    pub fn to_f32(&self) -> f32 {
        let safe_cw: u16 = SAFE_DEFAULT_CW;
        let mut result: f32 = 0.0;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fldcw word ptr [{safe}]", "fld tbyte ptr [{src}]",
                "fstp dword ptr [{dst}]", safe = in (reg) & safe_cw, src = in (reg) self
                .0.as_ptr(), dst = in (reg) & mut result as * mut f32, out("st(0)") _,
                out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
                out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
            );
        }
        result
    }
    /// Convert to f64 with explicit control word for rounding mode.
    pub fn to_f64_with_cw(&self, cw: u16) -> f64 {
        let safe_cw = host_safe_cw(cw);
        let mut result: f64 = 0.0;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fldcw word ptr [{cw}]", "fld tbyte ptr [{src}]",
                "fstp qword ptr [{dst}]", cw = in (reg) & safe_cw, src = in (reg) self.0
                .as_ptr(), dst = in (reg) & mut result as * mut f64, out("st(0)") _,
                out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
                out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
            );
        }
        result
    }
    /// Convert to f32 with explicit control word for rounding mode.
    pub fn to_f32_with_cw(&self, cw: u16) -> f32 {
        self.narrow_f32_via_host_x87(cw)
    }
    /// The status bits an `FST`/`FSTP` to memory can report: IE, OE, UE, PE
    /// and C1 (MDD-BUG-TEM-00168). A store raises no #D or #Z.
    pub const FST_STATUS_MASK: u16 = 0x0239;
    /// `FST m32`: the narrowed value and the status the store reports.
    pub fn to_f32_status_with_cw(&self, cw: u16) -> (f32, u16) {
        let safe_cw = host_safe_cw(cw);
        let mut result: f32 = 0.0;
        let mut sw: u16 = 0;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fldcw word ptr [{cw}]", "fnclex", "fld tbyte ptr [{src}]",
                "fstp dword ptr [{dst}]", "fnstsw word ptr [{sw}]", cw = in (reg) &
                safe_cw, src = in (reg) self.0.as_ptr(), dst = in (reg) & mut result as *
                mut f32, sw = in (reg) & mut sw as * mut u16, out("st(0)") _,
                out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
                out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
            );
        }
        (result, sw & Self::FST_STATUS_MASK)
    }
    /// `FST m64`: the narrowed value and the status the store reports.
    pub fn to_f64_status_with_cw(&self, cw: u16) -> (f64, u16) {
        let safe_cw = host_safe_cw(cw);
        let mut result: f64 = 0.0;
        let mut sw: u16 = 0;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fldcw word ptr [{cw}]", "fnclex", "fld tbyte ptr [{src}]",
                "fstp qword ptr [{dst}]", "fnstsw word ptr [{sw}]", cw = in (reg) &
                safe_cw, src = in (reg) self.0.as_ptr(), dst = in (reg) & mut result as *
                mut f64, sw = in (reg) & mut sw as * mut u16, out("st(0)") _,
                out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
                out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
            );
        }
        (result, sw & Self::FST_STATUS_MASK)
    }
    fn narrow_f32_via_host_x87(&self, cw: u16) -> f32 {
        let safe_cw = host_safe_cw(cw);
        let mut result: f32 = 0.0;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fldcw word ptr [{cw}]", "fld tbyte ptr [{src}]",
                "fstp dword ptr [{dst}]", cw = in (reg) & safe_cw, src = in (reg) self.0
                .as_ptr(), dst = in (reg) & mut result as * mut f32, out("st(0)") _,
                out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
                out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
            );
        }
        result
    }
    /// The x87 status bits `FIST`/`FISTP` may report on this conversion:
    /// `IE` (bit 0), `PE` (bit 5), and the `C1` condition code (bit 9).
    ///
    /// The host `FISTP` these helpers already execute reports the whole status
    /// word, and `to_i{16,32,64}` threw away everything except bit 0 — so the
    /// inexactness signal was being MEASURED and DISCARDED. `FNINIT; FLDPI; FIST dword` reads
    /// SW=0x3820 on host silicon against our 0x3800; the low six exception bits
    /// are exactly `0x0020`, PE alone, with the stored value already correct.
    ///
    /// Exception bits are masked to `IE | PE` because the SDM gives
    /// `FIST`/`FISTP` exactly those two numeric exceptions (`#IA` and `#P`)
    /// plus the stack fault `#IS`, which is not a conversion outcome. Passing
    /// the raw host word through would import bits this instruction cannot
    /// architecturally raise. `C1` rides along because `FIST`/`FISTP` also
    /// update it — set when the store rounded up, cleared otherwise — and the
    /// original `IE | PE` mask stripped the bit silicon had already computed
    /// (MDD-BUG-FLUX-00127). C0/C2/C3 stay masked: the SDM leaves them
    /// undefined here.
    pub const FIST_STATUS_MASK: u16 = 0x0221;
    /// Convert to i16 with rounding per control word.
    /// Returns `(value, invalid)`; see [`F80::to_i16_status`] for the flags.
    pub fn to_i16(&self, cw: u16) -> (i16, bool) {
        let (value, sw) = self.to_i16_status(cw);
        (value, (sw & 0x01) != 0)
    }
    /// Convert to i16, returning the exception bits rather than just `invalid`.
    pub fn to_i16_status(&self, cw: u16) -> (i16, u16) {
        if self.is_nan() || self.is_infinite() {
            return (i16::MIN, 0x01);
        }
        let safe_cw = host_safe_cw(cw);
        let mut result: i16 = 0;
        let mut sw: u16 = 0;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fldcw word ptr [{cw}]", "fnclex", "fld tbyte ptr [{src}]",
                "fistp word ptr [{dst}]", "fnstsw word ptr [{sw}]", cw = in (reg) &
                safe_cw, src = in (reg) self.0.as_ptr(), dst = in (reg) & mut result as *
                mut i16, sw = in (reg) & mut sw as * mut u16, out("st(0)") _,
                out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
                out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
            );
        }
        (result, sw & Self::FIST_STATUS_MASK)
    }
    /// Convert to i32 with rounding per control word.
    /// Returns `(value, invalid)`; see [`F80::to_i32_status`] for the flags.
    pub fn to_i32(&self, cw: u16) -> (i32, bool) {
        let (value, sw) = self.to_i32_status(cw);
        (value, (sw & 0x01) != 0)
    }
    /// Convert to i32, returning the exception bits rather than just `invalid`.
    pub fn to_i32_status(&self, cw: u16) -> (i32, u16) {
        self.round_i32_via_host_x87(cw)
    }
    fn round_i32_via_host_x87(&self, cw: u16) -> (i32, u16) {
        if self.is_nan() || self.is_infinite() {
            return (i32::MIN, 0x01);
        }
        let safe_cw = host_safe_cw(cw);
        let mut result: i32 = 0;
        let mut sw: u16 = 0;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fldcw word ptr [{cw}]", "fnclex", "fld tbyte ptr [{src}]",
                "fistp dword ptr [{dst}]", "fnstsw word ptr [{sw}]", cw = in (reg) &
                safe_cw, src = in (reg) self.0.as_ptr(), dst = in (reg) & mut result as *
                mut i32, sw = in (reg) & mut sw as * mut u16, out("st(0)") _,
                out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
                out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
            );
        }
        (result, sw & Self::FIST_STATUS_MASK)
    }
    /// Convert to i64 with rounding per control word.
    /// Returns `(value, invalid)`; see [`F80::to_i64_status`] for the flags.
    pub fn to_i64(&self, cw: u16) -> (i64, bool) {
        let (value, sw) = self.to_i64_status(cw);
        (value, (sw & 0x01) != 0)
    }
    /// Convert to i64, returning the exception bits rather than just `invalid`.
    pub fn to_i64_status(&self, cw: u16) -> (i64, u16) {
        if self.is_nan() || self.is_infinite() {
            return (i64::MIN, 0x01);
        }
        let safe_cw = host_safe_cw(cw);
        let mut result: i64 = 0;
        let mut sw: u16 = 0;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fldcw word ptr [{cw}]", "fnclex", "fld tbyte ptr [{src}]",
                "fistp qword ptr [{dst}]", "fnstsw word ptr [{sw}]", cw = in (reg) &
                safe_cw, src = in (reg) self.0.as_ptr(), dst = in (reg) & mut result as *
                mut i64, sw = in (reg) & mut sw as * mut u16, out("st(0)") _,
                out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
                out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
            );
        }
        (result, sw & Self::FIST_STATUS_MASK)
    }
    /// Convert to i16 with truncation (FISTTP). Returns (value, status_word).
    pub fn to_i16_truncate_status(&self) -> (i16, u16) {
        if self.is_nan() || self.is_infinite() {
            return (i16::MIN, 0x0001);
        }
        if host_has_fisttp() {
            self.to_i16_truncate_status_fisttp()
        } else {
            self.to_i16_truncate_status_x87()
        }
    }
    /// `FISTTP` path. Requires SSE3 — reach it only via
    /// [`host_has_fisttp`], never directly outside tests.
    pub(crate) fn to_i16_truncate_status_fisttp(self) -> (i16, u16) {
        let safe_cw: u16 = SAFE_DEFAULT_CW;
        let mut result: i16 = 0;
        let mut sw: u16 = 0;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fnclex", "fldcw word ptr [{safe}]", "fld tbyte ptr [{src}]",
                "fisttp word ptr [{dst}]", "fnstsw word ptr [{sw}]", safe = in (reg) &
                safe_cw, src = in (reg) self.0.as_ptr(), dst = in (reg) & mut result as *
                mut i16, sw = in (reg) & mut sw as * mut u16, out("st(0)") _,
                out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
                out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
            );
        }
        (result, sw)
    }
    /// Portable fallback for hosts without SSE3: `FISTP` under a
    /// round-toward-zero control word, which is what `FISTTP` does
    /// unconditionally (MDD-BUG-CRUCIBLE-00260).
    pub(crate) fn to_i16_truncate_status_x87(self) -> (i16, u16) {
        let truncate_cw: u16 = SAFE_TRUNCATE_CW;
        let mut result: i16 = 0;
        let mut sw: u16 = 0;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fnclex", "fldcw word ptr [{trunc}]", "fld tbyte ptr [{src}]",
                "fistp word ptr [{dst}]", "fnstsw word ptr [{sw}]", trunc = in (reg) &
                truncate_cw, src = in (reg) self.0.as_ptr(), dst = in (reg) & mut result
                as * mut i16, sw = in (reg) & mut sw as * mut u16, out("st(0)") _,
                out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
                out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
            );
        }
        (result, sw)
    }
    /// Convert to i16 with truncation (FISTTP). Returns (value, invalid).
    pub fn to_i16_truncate(&self) -> (i16, bool) {
        let (result, sw) = self.to_i16_truncate_status();
        let invalid = (sw & 0x0001) != 0;
        (result, invalid)
    }
    /// Convert to i32 with truncation (FISTTP). Returns (value, status_word).
    pub fn to_i32_truncate_status(&self) -> (i32, u16) {
        if self.is_nan() || self.is_infinite() {
            return (i32::MIN, 0x0001);
        }
        if host_has_fisttp() {
            self.to_i32_truncate_status_fisttp()
        } else {
            self.to_i32_truncate_status_x87()
        }
    }
    /// `FISTTP` path. Requires SSE3 — reach it only via
    /// [`host_has_fisttp`], never directly outside tests.
    pub(crate) fn to_i32_truncate_status_fisttp(self) -> (i32, u16) {
        let safe_cw: u16 = SAFE_DEFAULT_CW;
        let mut result: i32 = 0;
        let mut sw: u16 = 0;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fnclex", "fldcw word ptr [{safe}]", "fld tbyte ptr [{src}]",
                "fisttp dword ptr [{dst}]", "fnstsw word ptr [{sw}]", safe = in (reg) &
                safe_cw, src = in (reg) self.0.as_ptr(), dst = in (reg) & mut result as *
                mut i32, sw = in (reg) & mut sw as * mut u16, out("st(0)") _,
                out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
                out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
            );
        }
        (result, sw)
    }
    /// Portable fallback for hosts without SSE3: `FISTP` under a
    /// round-toward-zero control word, which is what `FISTTP` does
    /// unconditionally (MDD-BUG-CRUCIBLE-00260).
    pub(crate) fn to_i32_truncate_status_x87(self) -> (i32, u16) {
        let truncate_cw: u16 = SAFE_TRUNCATE_CW;
        let mut result: i32 = 0;
        let mut sw: u16 = 0;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fnclex", "fldcw word ptr [{trunc}]", "fld tbyte ptr [{src}]",
                "fistp dword ptr [{dst}]", "fnstsw word ptr [{sw}]", trunc = in (reg) &
                truncate_cw, src = in (reg) self.0.as_ptr(), dst = in (reg) & mut result
                as * mut i32, sw = in (reg) & mut sw as * mut u16, out("st(0)") _,
                out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
                out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
            );
        }
        (result, sw)
    }
    /// Convert to i32 with truncation (FISTTP). Returns (value, invalid).
    pub fn to_i32_truncate(&self) -> (i32, bool) {
        let (result, sw) = self.to_i32_truncate_status();
        let invalid = (sw & 0x0001) != 0;
        (result, invalid)
    }
    /// Convert to i64 with truncation (FISTTP). Returns (value, status_word).
    pub fn to_i64_truncate_status(&self) -> (i64, u16) {
        if self.is_nan() || self.is_infinite() {
            return (i64::MIN, 0x0001);
        }
        if host_has_fisttp() {
            self.to_i64_truncate_status_fisttp()
        } else {
            self.to_i64_truncate_status_x87()
        }
    }
    /// `FISTTP` path. Requires SSE3 — reach it only via
    /// [`host_has_fisttp`], never directly outside tests.
    pub(crate) fn to_i64_truncate_status_fisttp(self) -> (i64, u16) {
        let safe_cw: u16 = SAFE_DEFAULT_CW;
        let mut result: i64 = 0;
        let mut sw: u16 = 0;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fnclex", "fldcw word ptr [{safe}]", "fld tbyte ptr [{src}]",
                "fisttp qword ptr [{dst}]", "fnstsw word ptr [{sw}]", safe = in (reg) &
                safe_cw, src = in (reg) self.0.as_ptr(), dst = in (reg) & mut result as *
                mut i64, sw = in (reg) & mut sw as * mut u16, out("st(0)") _,
                out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
                out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
            );
        }
        (result, sw)
    }
    /// Portable fallback for hosts without SSE3: `FISTP` under a
    /// round-toward-zero control word, which is what `FISTTP` does
    /// unconditionally (MDD-BUG-CRUCIBLE-00260).
    pub(crate) fn to_i64_truncate_status_x87(self) -> (i64, u16) {
        let truncate_cw: u16 = SAFE_TRUNCATE_CW;
        let mut result: i64 = 0;
        let mut sw: u16 = 0;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fnclex", "fldcw word ptr [{trunc}]", "fld tbyte ptr [{src}]",
                "fistp qword ptr [{dst}]", "fnstsw word ptr [{sw}]", trunc = in (reg) &
                truncate_cw, src = in (reg) self.0.as_ptr(), dst = in (reg) & mut result
                as * mut i64, sw = in (reg) & mut sw as * mut u16, out("st(0)") _,
                out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
                out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
            );
        }
        (result, sw)
    }
    /// Convert to i64 with truncation (FISTTP). Returns (value, invalid).
    pub fn to_i64_truncate(&self) -> (i64, bool) {
        let (result, sw) = self.to_i64_truncate_status();
        let invalid = (sw & 0x0001) != 0;
        (result, invalid)
    }
    /// Convert i16 to F80 via FILD.
    pub fn from_i16(val: i16) -> F80 {
        let safe_cw: u16 = SAFE_DEFAULT_CW;
        let mut result = F80::ZERO;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fldcw word ptr [{safe}]", "fild word ptr [{src}]",
                "fstp tbyte ptr [{dst}]", safe = in (reg) & safe_cw, src = in (reg) & val
                as * const i16, dst = in (reg) result.0.as_mut_ptr(), out("st(0)") _,
                out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
                out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
            );
        }
        result
    }
    /// Convert i32 to F80 via FILD.
    pub fn from_i32(val: i32) -> F80 {
        Self::from_i32_via_host_x87(val)
    }
    fn from_i32_via_host_x87(val: i32) -> F80 {
        let safe_cw: u16 = SAFE_DEFAULT_CW;
        let mut result = F80::ZERO;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fldcw word ptr [{safe}]", "fild dword ptr [{src}]",
                "fstp tbyte ptr [{dst}]", safe = in (reg) & safe_cw, src = in (reg) & val
                as * const i32, dst = in (reg) result.0.as_mut_ptr(), out("st(0)") _,
                out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
                out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
            );
        }
        result
    }
    /// Convert i64 to F80 via FILD.
    pub fn from_i64(val: i64) -> F80 {
        let safe_cw: u16 = SAFE_DEFAULT_CW;
        let mut result = F80::ZERO;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fldcw word ptr [{safe}]", "fild qword ptr [{src}]",
                "fstp tbyte ptr [{dst}]", safe = in (reg) & safe_cw, src = in (reg) & val
                as * const i64, dst = in (reg) result.0.as_mut_ptr(), out("st(0)") _,
                out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
                out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
            );
        }
        result
    }
    /// Convert to BCD (FBSTP). Returns 10-byte packed BCD.
    pub fn to_bcd(&self, cw: u16) -> [u8; 10] {
        let safe_cw = host_safe_cw(cw);
        let mut result = [0u8; 10];
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fldcw word ptr [{cw}]", "fld tbyte ptr [{src}]",
                "fbstp tbyte ptr [{dst}]", cw = in (reg) & safe_cw, src = in (reg) self.0
                .as_ptr(), dst = in (reg) result.as_mut_ptr(), out("st(0)") _,
                out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
                out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
            );
        }
        result
    }
    /// Convert from BCD (FBLD).
    pub fn from_bcd(bytes: &[u8; 10]) -> F80 {
        let safe_cw: u16 = SAFE_DEFAULT_CW;
        let mut result = F80::ZERO;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fldcw word ptr [{safe}]", "fbld tbyte ptr [{src}]",
                "fstp tbyte ptr [{dst}]", safe = in (reg) & safe_cw, src = in (reg) bytes
                .as_ptr(), dst = in (reg) result.0.as_mut_ptr(), out("st(0)") _,
                out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
                out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
            );
        }
        result
    }
    /// Check if this value is NaN.
    /// NaN: exponent all 1s (0x7FFF), mantissa != 0x8000000000000000,
    /// and mantissa != 0 (must have at least one non-integer bit set).
    pub fn is_nan(&self) -> bool {
        let exp = self.exponent_bits();
        let mantissa = self.mantissa_bits();
        exp == 0x7FFF && mantissa != 0x8000_0000_0000_0000
    }
    /// Check if this value is infinity (+inf or -inf).
    /// Infinity: exponent all 1s (0x7FFF), mantissa == 0x8000000000000000.
    pub fn is_infinite(&self) -> bool {
        let exp = self.exponent_bits();
        let mantissa = self.mantissa_bits();
        exp == 0x7FFF && mantissa == 0x8000_0000_0000_0000
    }
    /// Check if this value is zero (+0 or -0).
    pub fn is_zero(&self) -> bool {
        let exp = self.exponent_bits();
        let mantissa = self.mantissa_bits();
        exp == 0 && mantissa == 0
    }
    /// Check if this value is a denormal (subnormal).
    /// Denormals have a zero exponent but non-zero mantissa.
    pub fn is_denormal(&self) -> bool {
        let exp = u16::from_le_bytes([self.0[8], self.0[9]]) & 0x7FFF;
        let mantissa = u64::from_le_bytes(self.0[0..8].try_into().unwrap());
        exp == 0 && mantissa != 0
    }
    /// Check if the sign bit is set (negative).
    pub fn is_sign_negative(&self) -> bool {
        (self.0[9] & 0x80) != 0
    }
    /// The x87 tag this value earns when it is written into a register:
    /// `0` Valid, `1` Zero, `2` Special.
    ///
    /// The tag word records what a register CONTAINS, not merely that it is
    /// occupied. We used to write Valid for everything that was not Empty, so
    /// `FLDZ` tagged Zero as Valid and a loaded NaN tagged Special as Valid —
    /// visible to `FXAM`, to any `FSTENV`/`FSAVE` consumer, and to a guest that
    /// tests the tag word for zero or NaN without touching the value.
    ///
    /// Special covers NaN, infinity, denormal and the unsupported encodings —
    /// every non-zero value that is not an ordinary finite number.
    ///
    /// **`3` (Empty) is deliberately not reachable here.** Empty is an
    /// occupancy fact, not a property of the bits, so it stays the caller's
    /// decision (`FFREE`, a pop, a stack fault) and cannot be produced by
    /// classifying a value.
    ///
    /// Not to be confused with `x87_oracle::classify`, which answers a
    /// different question — it triages divergences and deliberately lumps zero,
    /// normal and infinity together as "well behaved", which is exactly the
    /// distinction a tag has to make.
    #[inline]
    pub fn x87_tag(&self) -> u16 {
        if self.is_zero() {
            1
        } else if self.is_nan()
            || self.is_infinite()
            || self.is_denormal()
            || self.is_unsupported_encoding()
        {
            2
        } else {
            0
        }
    }
    /// True for encodings on which a 387+ arithmetic instruction raises #IA:
    /// an unnormal finite value, pseudo-infinity, or pseudo-NaN. The exponent-0
    /// pseudo-denormal form is deliberately excluded because silicon accepts it.
    ///
    /// Kept bit-for-bit identical to `x87_soft::F80::is_unsupported_encoding`:
    /// these two types are the same architectural encoding behind a
    /// `cfg(target_arch)` switch, so a classification that differed between
    /// them would make guest-visible status host-dependent.
    #[inline]
    pub fn is_unsupported_encoding(&self) -> bool {
        self.exponent_bits() != 0 && self.mantissa_bits() & (1 << 63) == 0
    }
    /// True for a signaling NaN: a NaN whose most-significant fraction bit is
    /// clear. Quiet NaNs have it set.
    ///
    /// Note this deliberately re-derives the NaN test from bits rather than
    /// calling [`F80::is_nan`], which on this backend also answers `true` for
    /// pseudo-NaNs (integer bit clear). Those are *unsupported*, not signaling,
    /// and are classified by [`F80::is_unsupported_encoding`] instead — the
    /// same split `x87_soft` makes.
    #[inline]
    pub fn is_signaling_nan(&self) -> bool {
        let mantissa = self.mantissa_bits();
        self.exponent_bits() == 0x7FFF
            && mantissa & (1 << 63) != 0
            && mantissa & 0x7FFF_FFFF_FFFF_FFFF != 0
            && mantissa & (1 << 62) == 0
    }
    /// Direct access to the raw bytes.
    pub fn bytes(&self) -> &[u8; 10] {
        &self.0
    }
    /// Extract the 15-bit biased exponent.
    #[inline]
    fn exponent_bits(&self) -> u16 {
        u16::from_le_bytes([self.0[8], self.0[9]]) & 0x7FFF
    }
    /// Extract the 64-bit mantissa (significand with explicit integer bit).
    #[inline]
    fn mantissa_bits(&self) -> u64 {
        u64::from_le_bytes([
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5], self.0[6], self.0[7],
        ])
    }
}
impl std::fmt::Debug for F80 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "F80({})", self.to_f64())
    }
}
impl PartialEq for F80 {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl Default for F80 {
    fn default() -> Self {
        Self::ZERO
    }
}
/// F80 addition: a + b, using the given control word for rounding/precision.
pub fn f80_add(a: &F80, b: &F80, cw: u16) -> F80 {
    let (result, _) = f80_add_status(a, b, cw);
    result
}
/// F80 addition: a + b, returning the isolated host x87 status word.
pub fn f80_add_status(a: &F80, b: &F80, cw: u16) -> (F80, u16) {
    let safe_cw = host_safe_cw(cw);
    let mut result = F80::ZERO;
    let mut sw: u16 = 0;
    unsafe {
        let _host_x87 = host_x87_enter_guarded();
        asm!(
            "fnclex", "fldcw word ptr [{cw}]", "fld tbyte ptr [{a}]",
            "fld tbyte ptr [{b}]", "faddp", "fnstsw word ptr [{sw}]",
            "fstp tbyte ptr [{result}]", a = in (reg) a.0.as_ptr(), b = in (reg) b.0
            .as_ptr(), result = in (reg) result.0.as_mut_ptr(), cw = in (reg) & safe_cw,
            sw = in (reg) & mut sw as * mut u16, out("st(0)") _, out("st(1)") _,
            out("st(2)") _, out("st(3)") _, out("st(4)") _, out("st(5)") _, out("st(6)")
            _, out("st(7)") _, options(nostack),
        );
    }
    (result, sw)
}
/// F80 subtraction: a - b, using the given control word.
pub fn f80_sub(a: &F80, b: &F80, cw: u16) -> F80 {
    let (result, _) = f80_sub_status(a, b, cw);
    result
}
/// F80 subtraction: a - b, returning the host x87 status word for the op.
pub fn f80_sub_status(a: &F80, b: &F80, cw: u16) -> (F80, u16) {
    let safe_cw = host_safe_cw(cw);
    let mut result = F80::ZERO;
    let mut sw: u16 = 0;
    unsafe {
        let _host_x87 = host_x87_enter_guarded();
        asm!(
            "fnclex", "fldcw word ptr [{cw}]", "fld tbyte ptr [{a}]",
            "fld tbyte ptr [{b}]", "fsubp", "fnstsw word ptr [{sw}]",
            "fstp tbyte ptr [{result}]", a = in (reg) a.0.as_ptr(), b = in (reg) b.0
            .as_ptr(), result = in (reg) result.0.as_mut_ptr(), cw = in (reg) & safe_cw,
            sw = in (reg) & mut sw as * mut u16, out("st(0)") _, out("st(1)") _,
            out("st(2)") _, out("st(3)") _, out("st(4)") _, out("st(5)") _, out("st(6)")
            _, out("st(7)") _, options(nostack),
        );
    }
    (result, sw)
}
/// F80 multiplication: a * b, using the given control word.
pub fn f80_mul(a: &F80, b: &F80, cw: u16) -> F80 {
    let (result, _) = f80_mul_status(a, b, cw);
    result
}
/// F80 multiplication: a * b, returning the isolated host x87 status word.
pub fn f80_mul_status(a: &F80, b: &F80, cw: u16) -> (F80, u16) {
    let safe_cw = host_safe_cw(cw);
    let mut result = F80::ZERO;
    let mut sw: u16 = 0;
    unsafe {
        let _host_x87 = host_x87_enter_guarded();
        asm!(
            "fnclex", "fldcw word ptr [{cw}]", "fld tbyte ptr [{a}]",
            "fld tbyte ptr [{b}]", "fmulp", "fnstsw word ptr [{sw}]",
            "fstp tbyte ptr [{result}]", a = in (reg) a.0.as_ptr(), b = in (reg) b.0
            .as_ptr(), result = in (reg) result.0.as_mut_ptr(), cw = in (reg) & safe_cw,
            sw = in (reg) & mut sw as * mut u16, out("st(0)") _, out("st(1)") _,
            out("st(2)") _, out("st(3)") _, out("st(4)") _, out("st(5)") _, out("st(6)")
            _, out("st(7)") _, options(nostack),
        );
    }
    (result, sw)
}
/// F80 division: a / b, using the given control word.
pub fn f80_div(a: &F80, b: &F80, cw: u16) -> F80 {
    let (result, _) = f80_div_status(a, b, cw);
    result
}
/// F80 division: a / b, returning the host x87 status word for the op.
pub fn f80_div_status(a: &F80, b: &F80, cw: u16) -> (F80, u16) {
    let safe_cw = host_safe_cw(cw);
    let mut result = F80::ZERO;
    let mut sw: u16 = 0;
    unsafe {
        let _host_x87 = host_x87_enter_guarded();
        asm!(
            "fnclex", "fldcw word ptr [{cw}]", "fld tbyte ptr [{a}]",
            "fld tbyte ptr [{b}]", "fdivp", "fnstsw word ptr [{sw}]",
            "fstp tbyte ptr [{result}]", a = in (reg) a.0.as_ptr(), b = in (reg) b.0
            .as_ptr(), result = in (reg) result.0.as_mut_ptr(), cw = in (reg) & safe_cw,
            sw = in (reg) & mut sw as * mut u16, out("st(0)") _, out("st(1)") _,
            out("st(2)") _, out("st(3)") _, out("st(4)") _, out("st(5)") _, out("st(6)")
            _, out("st(7)") _, options(nostack),
        );
    }
    (result, sw)
}
/// Square root of a, using the given control word.
pub fn f80_sqrt(a: &F80, cw: u16) -> F80 {
    f80_sqrt_status(a, cw).0
}
/// FSQRT returning the isolated host x87 status word (MDD-BUG-CRUCIBLE-00579).
///
/// Same shape as [`f80_add_status`]: `fnclex` first so the status reflects THIS
/// operation only, and `fnstsw` before the store so the store's own status
/// cannot contaminate it.
pub fn f80_sqrt_status(a: &F80, cw: u16) -> (F80, u16) {
    let safe_cw = host_safe_cw(cw);
    let mut result = F80::ZERO;
    let mut sw: u16 = 0;
    unsafe {
        let _host_x87 = host_x87_enter_guarded();
        asm!(
            "fnclex", "fldcw word ptr [{cw}]", "fld tbyte ptr [{a}]", "fsqrt",
            "fnstsw word ptr [{sw}]", "fstp tbyte ptr [{result}]", a = in (reg) a.0
            .as_ptr(), result = in (reg) result.0.as_mut_ptr(), cw = in (reg) & safe_cw,
            sw = in (reg) & mut sw as * mut u16, out("st(0)") _, out("st(1)") _,
            out("st(2)") _, out("st(3)") _, out("st(4)") _, out("st(5)") _, out("st(6)")
            _, out("st(7)") _, options(nostack),
        );
    }
    (result, sw)
}
/// Absolute value (pure bit manipulation — clears sign bit in byte 9).
pub fn f80_abs(a: &F80) -> F80 {
    let mut result = *a;
    result.0[9] &= 0x7F;
    result
}
/// Change sign (pure bit manipulation — flips sign bit in byte 9).
pub fn f80_chs(a: &F80) -> F80 {
    let mut result = *a;
    result.0[9] ^= 0x80;
    result
}
/// Round to integer (FRNDINT) using the rounding mode in the control word.
pub fn f80_rndint(a: &F80, cw: u16) -> F80 {
    f80_rndint_status(a, cw).0
}
/// FRNDINT returning the isolated host x87 status word (MDD-BUG-CRUCIBLE-00579).
pub fn f80_rndint_status(a: &F80, cw: u16) -> (F80, u16) {
    let safe_cw = host_safe_cw(cw);
    let mut result = F80::ZERO;
    let mut sw: u16 = 0;
    unsafe {
        let _host_x87 = host_x87_enter_guarded();
        asm!(
            "fnclex", "fldcw word ptr [{cw}]", "fld tbyte ptr [{a}]", "frndint",
            "fnstsw word ptr [{sw}]", "fstp tbyte ptr [{result}]", a = in (reg) a.0
            .as_ptr(), result = in (reg) result.0.as_mut_ptr(), cw = in (reg) & safe_cw,
            sw = in (reg) & mut sw as * mut u16, out("st(0)") _, out("st(1)") _,
            out("st(2)") _, out("st(3)") _, out("st(4)") _, out("st(5)") _, out("st(6)")
            _, out("st(7)") _, options(nostack),
        );
    }
    (result, sw)
}
/// FSIN: compute sin(a). Returns (result, C2_set) where C2 indicates out of range.
/// When C2 is set, ST(0) is unchanged (still the original operand).
pub fn f80_sin(a: &F80, cw: u16) -> (F80, bool) {
    let safe_cw = host_safe_cw(cw);
    let mut result = F80::ZERO;
    let mut sw: u16 = 0;
    unsafe {
        let _host_x87 = host_x87_enter_guarded();
        asm!(
            "fnclex", "fldcw word ptr [{safe}]", "fld tbyte ptr [{a}]", "fsin",
            "fnstsw word ptr [{sw}]", "fstp tbyte ptr [{result}]", safe = in (reg) &
            safe_cw, a = in (reg) a.0.as_ptr(), result = in (reg) result.0.as_mut_ptr(),
            sw = in (reg) & mut sw as * mut u16, out("st(0)") _, out("st(1)") _,
            out("st(2)") _, out("st(3)") _, out("st(4)") _, out("st(5)") _, out("st(6)")
            _, out("st(7)") _, options(nostack),
        );
    }
    let c2 = (sw & 0x0400) != 0;
    (result, c2)
}
/// FCOS: compute cos(a). Returns (result, C2_set).
/// When C2 is set, ST(0) is unchanged (still the original operand).
pub fn f80_cos(a: &F80, cw: u16) -> (F80, bool) {
    let safe_cw = host_safe_cw(cw);
    let mut result = F80::ZERO;
    let mut sw: u16 = 0;
    unsafe {
        let _host_x87 = host_x87_enter_guarded();
        asm!(
            "fnclex", "fldcw word ptr [{safe}]", "fld tbyte ptr [{a}]", "fcos",
            "fnstsw word ptr [{sw}]", "fstp tbyte ptr [{result}]", safe = in (reg) &
            safe_cw, a = in (reg) a.0.as_ptr(), result = in (reg) result.0.as_mut_ptr(),
            sw = in (reg) & mut sw as * mut u16, out("st(0)") _, out("st(1)") _,
            out("st(2)") _, out("st(3)") _, out("st(4)") _, out("st(5)") _, out("st(6)")
            _, out("st(7)") _, options(nostack),
        );
    }
    let c2 = (sw & 0x0400) != 0;
    (result, c2)
}
/// FSINCOS: compute sin and cos simultaneously.
/// Returns (sin, cos, C2_set).
/// After FSINCOS (success): ST(0) = cos, ST(1) = sin.
/// After FSINCOS (C2 set): ST(0) = original operand (no push occurred).
pub fn f80_sincos(a: &F80, cw: u16) -> (F80, F80, bool) {
    let safe_cw = host_safe_cw(cw);
    let mut sin_result = F80::ZERO;
    let mut cos_result = F80::ZERO;
    let mut sw: u16 = 0;
    unsafe {
        let _host_x87 = host_x87_enter_guarded();
        asm!(
            "fnclex", "fldcw word ptr [{safe}]", "fld tbyte ptr [{a}]", "fsincos",
            "fnstsw word ptr [{sw}]", "test word ptr [{sw}], 0x0400", "jnz 2f",
            "fstp tbyte ptr [{cos}]", "fstp tbyte ptr [{sin}]", "jmp 3f", "2:",
            "fstp tbyte ptr [{sin}]", "3:", safe = in (reg) & safe_cw, a = in (reg) a.0
            .as_ptr(), cos = in (reg) cos_result.0.as_mut_ptr(), sin = in (reg)
            sin_result.0.as_mut_ptr(), sw = in (reg) & mut sw as * mut u16, out("st(0)")
            _, out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
            out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
        );
    }
    let c2 = (sw & 0x0400) != 0;
    (sin_result, cos_result, c2)
}
/// FPTAN: compute partial tangent.
/// Returns (tan_result, 1.0, C2_set, status_word).
/// After FPTAN (success): ST(0) = 1.0, ST(1) = tan(a).
/// After FPTAN (C2 set): ST(0) = original operand (no push occurred).
pub fn f80_ptan(a: &F80, cw: u16) -> (F80, F80, bool, u16) {
    let safe_cw = host_safe_cw(cw);
    let mut tan_result = F80::ZERO;
    let mut one_result = F80::ZERO;
    let mut sw: u16 = 0;
    unsafe {
        let _host_x87 = host_x87_enter_guarded();
        asm!(
            "fnclex", "fldcw word ptr [{safe}]", "fld tbyte ptr [{a}]", "fptan",
            "fnstsw word ptr [{sw}]", "test word ptr [{sw}], 0x0400", "jnz 2f",
            "fstp tbyte ptr [{one}]", "fstp tbyte ptr [{tan}]", "jmp 3f", "2:",
            "fstp tbyte ptr [{tan}]", "3:", safe = in (reg) & safe_cw, a = in (reg) a.0
            .as_ptr(), one = in (reg) one_result.0.as_mut_ptr(), tan = in (reg)
            tan_result.0.as_mut_ptr(), sw = in (reg) & mut sw as * mut u16, out("st(0)")
            _, out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _,
            out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
        );
    }
    let c2 = (sw & 0x0400) != 0;
    if !c2 && (sw & 0x0020) != 0 {
        sw |= 0x0200;
    }
    (tan_result, one_result, c2, sw)
}
/// FPATAN: compute atan2(y, x).
/// Push y first (into ST(1)), then x (into ST(0)).
/// FPATAN computes atan2(ST(1), ST(0)), pops both, pushes result.
pub fn f80_patan(y: &F80, x: &F80, cw: u16) -> F80 {
    let safe_cw = host_safe_cw(cw);
    let mut result = F80::ZERO;
    unsafe {
        let _host_x87 = host_x87_enter_guarded();
        asm!(
            "fldcw word ptr [{safe}]", "fld tbyte ptr [{y}]", "fld tbyte ptr [{x}]",
            "fpatan", "fstp tbyte ptr [{result}]", safe = in (reg) & safe_cw, y = in
            (reg) y.0.as_ptr(), x = in (reg) x.0.as_ptr(), result = in (reg) result.0
            .as_mut_ptr(), out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)")
            _, out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
            options(nostack),
        );
    }
    result
}
/// FYL2X: compute y * log2(x).
/// Push y first (into ST(1)), then x (into ST(0)).
/// FYL2X computes ST(1) * log2(ST(0)), pops both, pushes result.
pub fn f80_yl2x(y: &F80, x: &F80, cw: u16) -> F80 {
    let safe_cw = host_safe_cw(cw);
    let mut result = F80::ZERO;
    unsafe {
        let _host_x87 = host_x87_enter_guarded();
        asm!(
            "fldcw word ptr [{safe}]", "fld tbyte ptr [{y}]", "fld tbyte ptr [{x}]",
            "fyl2x", "fstp tbyte ptr [{result}]", safe = in (reg) & safe_cw, y = in (reg)
            y.0.as_ptr(), x = in (reg) x.0.as_ptr(), result = in (reg) result.0
            .as_mut_ptr(), out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)")
            _, out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
            options(nostack),
        );
    }
    result
}
/// FYL2XP1: compute y * log2(x + 1).
/// Push y first (into ST(1)), then x (into ST(0)).
pub fn f80_yl2xp1(y: &F80, x: &F80, cw: u16) -> F80 {
    let safe_cw = host_safe_cw(cw);
    let mut result = F80::ZERO;
    unsafe {
        let _host_x87 = host_x87_enter_guarded();
        asm!(
            "fldcw word ptr [{safe}]", "fld tbyte ptr [{y}]", "fld tbyte ptr [{x}]",
            "fyl2xp1", "fstp tbyte ptr [{result}]", safe = in (reg) & safe_cw, y = in
            (reg) y.0.as_ptr(), x = in (reg) x.0.as_ptr(), result = in (reg) result.0
            .as_mut_ptr(), out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)")
            _, out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
            options(nostack),
        );
    }
    result
}
/// F2XM1: compute 2^a - 1. Input must be in range [-1.0, 1.0].
pub fn f80_f2xm1(a: &F80, cw: u16) -> F80 {
    let safe_cw = host_safe_cw(cw);
    let mut result = F80::ZERO;
    unsafe {
        let _host_x87 = host_x87_enter_guarded();
        asm!(
            "fldcw word ptr [{safe}]", "fld tbyte ptr [{a}]", "f2xm1",
            "fstp tbyte ptr [{result}]", safe = in (reg) & safe_cw, a = in (reg) a.0
            .as_ptr(), result = in (reg) result.0.as_mut_ptr(), out("st(0)") _,
            out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _, out("st(5)")
            _, out("st(6)") _, out("st(7)") _, options(nostack),
        );
    }
    result
}
/// FSCALE: compute a * 2^trunc(b).
/// Load b (scale factor) into ST(1), a (value) into ST(0).
/// FSCALE replaces ST(0) with result, ST(1) unchanged.
pub fn f80_scale(a: &F80, b: &F80, cw: u16) -> F80 {
    let safe_cw = host_safe_cw(cw);
    let mut result = F80::ZERO;
    unsafe {
        let _host_x87 = host_x87_enter_guarded();
        asm!(
            "fldcw word ptr [{safe}]", "fld tbyte ptr [{b}]", "fld tbyte ptr [{a}]",
            "fscale", "fstp tbyte ptr [{result}]", "fstp st(0)", safe = in (reg) &
            safe_cw, b = in (reg) b.0.as_ptr(), a = in (reg) a.0.as_ptr(), result = in
            (reg) result.0.as_mut_ptr(), out("st(0)") _, out("st(1)") _, out("st(2)") _,
            out("st(3)") _, out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)")
            _, options(nostack),
        );
    }
    result
}
/// FPREM: partial IEEE-incompatible remainder of a / b.
/// Load b (divisor) into ST(1), a (dividend) into ST(0).
/// Returns (remainder, status_word_bits).
/// Status word bits: C2 (bit 10) = 1 means reduction incomplete.
/// C0, C3, C1 contain the three low bits of the quotient.
pub fn f80_prem(a: &F80, b: &F80, cw: u16) -> (F80, u16) {
    let safe_cw = host_safe_cw(cw);
    let mut result = F80::ZERO;
    let mut sw: u16 = 0;
    unsafe {
        let _host_x87 = host_x87_enter_guarded();
        asm!(
            "fnclex", "fldcw word ptr [{safe}]", "fld tbyte ptr [{b}]",
            "fld tbyte ptr [{a}]", "fprem", "fnstsw word ptr [{sw}]",
            "fstp tbyte ptr [{result}]", "fstp st(0)", safe = in (reg) & safe_cw, b = in
            (reg) b.0.as_ptr(), a = in (reg) a.0.as_ptr(), result = in (reg) result.0
            .as_mut_ptr(), sw = in (reg) & mut sw as * mut u16, out("st(0)") _,
            out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _, out("st(5)")
            _, out("st(6)") _, out("st(7)") _, options(nostack),
        );
    }
    (result, sw)
}
/// FPREM1: IEEE-compliant partial remainder of a / b.
/// Same interface as `f80_prem`.
pub fn f80_prem1(a: &F80, b: &F80, cw: u16) -> (F80, u16) {
    let safe_cw = host_safe_cw(cw);
    let mut result = F80::ZERO;
    let mut sw: u16 = 0;
    unsafe {
        let _host_x87 = host_x87_enter_guarded();
        asm!(
            "fnclex", "fldcw word ptr [{safe}]", "fld tbyte ptr [{b}]",
            "fld tbyte ptr [{a}]", "fprem1", "fnstsw word ptr [{sw}]",
            "fstp tbyte ptr [{result}]", "fstp st(0)", safe = in (reg) & safe_cw, b = in
            (reg) b.0.as_ptr(), a = in (reg) a.0.as_ptr(), result = in (reg) result.0
            .as_mut_ptr(), sw = in (reg) & mut sw as * mut u16, out("st(0)") _,
            out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _, out("st(5)")
            _, out("st(6)") _, out("st(7)") _, options(nostack),
        );
    }
    (result, sw)
}
/// FXTRACT: decompose a into significand and exponent.
/// After FXTRACT: ST(0) = significand (pushed), ST(1) = exponent.
/// Returns (significand, exponent).
pub fn f80_xtract(a: &F80, cw: u16) -> (F80, F80) {
    let safe_cw = host_safe_cw(cw);
    let mut significand = F80::ZERO;
    let mut exponent = F80::ZERO;
    unsafe {
        let _host_x87 = host_x87_enter_guarded();
        asm!(
            "fldcw word ptr [{safe}]", "fld tbyte ptr [{a}]", "fxtract",
            "fstp tbyte ptr [{sig}]", "fstp tbyte ptr [{exp}]", safe = in (reg) &
            safe_cw, a = in (reg) a.0.as_ptr(), sig = in (reg) significand.0
            .as_mut_ptr(), exp = in (reg) exponent.0.as_mut_ptr(), out("st(0)") _,
            out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)") _, out("st(5)")
            _, out("st(6)") _, out("st(7)") _, options(nostack),
        );
    }
    (significand, exponent)
}
/// FUCOM: unordered comparison of a and b.
/// Returns status word bits (C0=bit 8, C2=bit 10, C3=bit 14).
pub fn f80_compare(a: &F80, b: &F80) -> u16 {
    let safe_cw: u16 = SAFE_DEFAULT_CW;
    let mut sw: u16 = 0;
    unsafe {
        let _host_x87 = host_x87_enter_guarded();
        asm!(
            "fldcw word ptr [{safe}]", "fld tbyte ptr [{b}]", "fld tbyte ptr [{a}]",
            "fucompp", "fnstsw word ptr [{sw}]", safe = in (reg) & safe_cw, b = in (reg)
            b.0.as_ptr(), a = in (reg) a.0.as_ptr(), sw = in (reg) & mut sw as * mut u16,
            out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _, out("st(4)")
            _, out("st(5)") _, out("st(6)") _, out("st(7)") _, options(nostack),
        );
    }
    sw
}
/// FUCOM with status word (same as f80_compare — provided for API completeness).
pub fn f80_compare_and_pop(a: &F80, b: &F80) -> u16 {
    f80_compare(a, b)
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::approx_constant)]
mod tests {
    use super::*;
    const CW_DEFAULT: u16 = 0x037F;
    const CW_TRUNCATE: u16 = 0x0F7F;
    const CW_ROUND_DOWN: u16 = 0x077F;
    const CW_ROUND_UP: u16 = 0x0B7F;
    /// Read the live host x87 control word.
    ///
    /// Deliberately unguarded: the guard would restore the control word on exit,
    /// which is exactly the state this probe exists to observe.
    fn host_cw() -> u16 {
        let mut cw: u16 = 0;
        unsafe {
            // x87-inventory-exempt: raw control-word probe; guarding it would erase the measurement.
            asm!("fnstcw word ptr [{cw}]", cw = in (reg) & mut cw, options(nostack));
        }
        cw
    }
    /// Set the live host x87 control word, returning the value the hardware
    /// actually round-trips.
    ///
    /// Reserved control-word bits do not necessarily read back as written — this
    /// CPU returns `0x007F` for a requested `0x003F` — so the caller must compare
    /// against what `FNSTCW` reports, not against the requested word.
    fn set_host_cw(cw: u16) -> u16 {
        unsafe {
            // x87-inventory-exempt: raw control-word probe; guarding it would erase the measurement.
            asm!("fldcw word ptr [{cw}]", cw = in (reg) & cw, options(nostack));
        }
        host_cw()
    }
    /// MDD-BUG-CRUCIBLE-00353 regression: every production x87 helper must leave
    /// the host control word exactly as it found it.
    ///
    /// The host x86-64 calling conventions preserve the x87 control word.
    /// Helpers that load a temporary word must restore it before returning
    /// so precision and rounding modes cannot leak into later host operations.
    ///
    /// Table-driven over several legal ambient words and one helper per family,
    /// asserting exact `FNSTCW` equality — a family that regains a bare
    /// `FLDCW` shows up as an inequality here, and a new asm site that skips the
    /// guard entirely is caught by the inventory oracles below.
    #[test]
    fn x87_helpers_preserve_the_ambient_host_control_word() {
        const AMBIENT: [u16; 4] = [0x027F, 0x037F, 0x077F, 0x0F7F];
        const GUEST_CW: [u16; 3] = [0x037F, 0x0F7F, 0x0000];
        let a = F80::from_f64(3.5);
        let b = F80::from_f64(1.25);
        let entry = host_cw();
        for &ambient in &AMBIENT {
            for &guest in &GUEST_CW {
                let ambient = set_host_cw(ambient);
                let _ = a.to_f64();
                assert_eq!(
                    host_cw(),
                    ambient,
                    "to_f64 clobbered CW (ambient {ambient:#06x})"
                );
                let _ = F80::from_f64(2.0);
                assert_eq!(
                    host_cw(),
                    ambient,
                    "from_f64 clobbered CW (ambient {ambient:#06x})"
                );
                let _ = a.to_f64_with_cw(guest);
                assert_eq!(
                    host_cw(),
                    ambient,
                    "to_f64_with_cw leaked guest CW {guest:#06x} (ambient {ambient:#06x})"
                );
                let _ = f80_add(&a, &b, guest);
                assert_eq!(
                    host_cw(),
                    ambient,
                    "f80_add leaked (ambient {ambient:#06x})"
                );
                let _ = f80_div(&a, &b, guest);
                assert_eq!(
                    host_cw(),
                    ambient,
                    "f80_div leaked (ambient {ambient:#06x})"
                );
                let _ = f80_sqrt(&a, guest);
                assert_eq!(
                    host_cw(),
                    ambient,
                    "f80_sqrt leaked (ambient {ambient:#06x})"
                );
                let (_, _) = f80_add_status(&a, &b, guest);
                assert_eq!(
                    host_cw(),
                    ambient,
                    "f80_add_status leaked (ambient {ambient:#06x})"
                );
            }
        }
        let _ = set_host_cw(entry);
    }
    /// Every inline-asm site in this crate arms the host-x87 residency and
    /// control-word guard, or is exempt by name with a reason.
    ///
    /// The per-site rule is old; the **totality** is the point. The two
    /// oracles this replaces read three hard-coded `include_str!` sources
    /// (`x87_ops.rs`, `fpu.rs`, `benches/x87_ops.rs`), so `cpu.rs`,
    /// `x87_oracle.rs` and both SSE kernel backends were never inventoried at
    /// all: an unguarded site in any of them passed, and a whole new file
    /// could ship unseen (MDD-BUG-TEM-00176).
    ///
    /// Unlike the SSE iced guard in `lib.rs`, there is deliberately no
    /// FILES/EXEMPT pair here. This scan reads EVERY `.rs` under the crate's
    /// `src/` and `benches/`, so a new source file is inventoried the moment
    /// it exists and no hand-written list can go stale; a file with no inline
    /// asm simply contributes no sites. Exemption is per SITE, not per file,
    /// and must carry its reason on the line above.
    ///
    /// Textual, so it inventories files this lane never compiles —
    /// `sse_host_kernels_a64.rs` is `cfg`'d out on every x86-64 host, and this
    /// module is itself x86-64-only, so this is the only lane that sees either
    /// (the `drc_a64.rs` precedent the SSE guard cites).
    #[test]
    fn every_x87_asm_site_enters_host_guard() {
        let asm_needle = concat!("as", "m!(");
        const SRC_GUARDS: [&str; 2] = [
            "let _host_x87 = host_x87_enter_guarded();",
            "let _host_x87 = crate::cpu::host_x87_enter_guarded();",
        ];
        const BENCH_GUARDS: [&str; 1] = ["ownership.certify_inline_asm();"];
        const FLOORS: [(&str, usize); 7] = [
            ("benches/x87_ops.rs", 6),
            ("src/cpu.rs", 2),
            ("src/fpu.rs", 6),
            ("src/sse_host_kernels.rs", 7),
            ("src/sse_host_kernels_a64.rs", 5),
            ("src/x87_oracle.rs", 6),
            ("src/x87_ops.rs", 40),
        ];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        let mut total_sites = 0usize;
        for dir in ["src", "benches"] {
            let mut names: Vec<String> = std::fs::read_dir(root.join(dir))
                .unwrap_or_else(|err| panic!("read {dir}/: {err}"))
                .map(|entry| {
                    entry
                        .unwrap_or_else(|err| panic!("{dir}/ entry: {err}"))
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .filter(|name| name.ends_with(".rs"))
                .collect();
            names.sort();
            for name in names {
                let relative = format!("{dir}/{name}");
                let source = std::fs::read_to_string(root.join(dir).join(&name))
                    .unwrap_or_else(|err| panic!("read {relative}: {err}"));
                let accepted: &[&str] = if dir == "benches" {
                    &BENCH_GUARDS
                } else {
                    &SRC_GUARDS
                };
                let mut sites = 0usize;
                let mut previous_nonempty = String::new();
                for (index, line) in source.lines().enumerate() {
                    if line.contains(asm_needle) {
                        sites += 1;
                        let previous = previous_nonempty.trim();
                        if !previous.contains("x87-inventory-exempt") {
                            assert!(
                                accepted.contains(&previous),
                                "inline x87 asm at {relative}:{} lacks the residency + \
                                 control-word guard: expected one of {accepted:?} on the line \
                                 above, found {previous:?}. Arm `host_x87_enter_guarded()` there, \
                                 or mark the site `x87-inventory-exempt` with its reason.",
                                index + 1
                            );
                        }
                    }
                    if !line.trim().is_empty() {
                        previous_nonempty = line.to_owned();
                    }
                }
                total_sites += sites;
                counts.insert(relative, sites);
            }
        }
        for (relative, floor) in FLOORS {
            let found = counts.get(relative).copied().unwrap_or_else(|| {
                panic!("{relative} is no longer part of the crate — restate or drop its floor")
            });
            assert!(
                found >= floor,
                "{relative} inventories {found} inline x87 asm sites, below its floor of {floor} \
                 — sites vanished, or the scan stopped seeing them"
            );
        }
        assert!(total_sites > 0, "x87 asm inventory unexpectedly empty");
    }
    #[test]
    fn x87_status_mutators_have_truthful_inline_asm_contracts() {
        let asm_needle = concat!("as", "m!(");
        let sources = [
            ("fpu.rs", include_str!("fpu.rs")),
            ("x87_ops.rs", include_str!("x87_ops.rs")),
            ("benches/x87_ops.rs", include_str!("../benches/x87_ops.rs")),
        ];
        let status_mutators = ["\"fnclex", "\"fninit", "\"fldenv", "\"frstor", "\"fnsave"];
        let mut mutating_sites = 0;
        for (path, source) in sources {
            for (offset, _) in source.match_indices(asm_needle) {
                let tail = &source[offset..];
                let end = tail
                    .find(");")
                    .unwrap_or_else(|| panic!("unterminated inline asm block in {path}"));
                let block = &tail[..end];
                if !status_mutators.iter().any(|opcode| block.contains(opcode)) {
                    continue;
                }
                mutating_sites += 1;
                let line = source[..offset].lines().count() + 1;
                assert!(
                    !block.contains("preserves_flags"),
                    "{path}:{line} mutates the x87 status word but claims preserves_flags"
                );
                if block.contains("\"fnsave") || block.contains("\"frstor") {
                    assert_eq!(
                        block.matches("out(\"st(").count(), 8,
                        "{path}:{line} changes the x87 stack but does not clobber all stack registers"
                    );
                }
            }
        }
        assert!(mutating_sites >= 6, "x87 status-mutator inventory shrank");
    }
    #[test]
    fn test_f80_is_nan() {
        let nan = F80::from_f64(f64::NAN);
        assert!(nan.is_nan());
        assert!(!nan.is_infinite());
        assert!(!nan.is_zero());
    }
    #[test]
    fn test_f80_is_infinite() {
        let pos_inf = F80::from_f64(f64::INFINITY);
        assert!(pos_inf.is_infinite());
        assert!(!pos_inf.is_nan());
        assert!(!pos_inf.is_zero());
        let neg_inf = F80::from_f64(f64::NEG_INFINITY);
        assert!(neg_inf.is_infinite());
        assert!(neg_inf.is_sign_negative());
    }
    #[test]
    fn test_f80_is_zero() {
        assert!(F80::ZERO.is_zero());
        assert!(!F80::ZERO.is_sign_negative());
        assert!(F80::NEG_ZERO.is_zero());
        assert!(F80::NEG_ZERO.is_sign_negative());
    }
    #[test]
    fn test_f80_is_sign_negative() {
        let pos = F80::from_f64(1.0);
        assert!(!pos.is_sign_negative());
        let neg = F80::from_f64(-1.0);
        assert!(neg.is_sign_negative());
    }
    #[test]
    fn test_f80_one_not_special() {
        assert!(!F80::ONE.is_nan());
        assert!(!F80::ONE.is_infinite());
        assert!(!F80::ONE.is_zero());
        assert!(!F80::ONE.is_sign_negative());
    }
    #[test]
    fn test_constant_one() {
        let val = F80::ONE.to_f64();
        assert_eq!(val, 1.0);
    }
    #[test]
    fn test_constant_zero() {
        let val = F80::ZERO.to_f64();
        assert_eq!(val, 0.0);
        assert!(!val.is_sign_negative());
    }
    #[test]
    fn test_constant_neg_zero() {
        let val = F80::NEG_ZERO.to_f64();
        assert_eq!(val, 0.0);
        assert!(val.is_sign_negative());
    }
    #[test]
    fn test_constant_pi() {
        let val = F80::PI.to_f64();
        assert!((val - std::f64::consts::PI).abs() < 1e-15);
    }
    #[test]
    fn test_constant_log2_e() {
        let val = F80::LOG2_E.to_f64();
        let diff = (val - std::f64::consts::LOG2_E).abs();
        assert!(diff < 1e-14, "LOG2_E diff={diff}");
    }
    #[test]
    fn test_constant_log2_10() {
        let val = F80::LOG2_10.to_f64();
        let expected = 10.0_f64.log2();
        assert!(
            (val - expected).abs() < 1e-14,
            "LOG2_10 diff={}",
            (val - expected).abs()
        );
    }
    #[test]
    fn test_constant_log10_2() {
        let val = F80::LOG10_2.to_f64();
        let expected = 2.0_f64.log10();
        let diff = (val - expected).abs();
        assert!(diff < 1e-15, "LOG10_2 diff={diff}");
    }
    #[test]
    fn test_constant_ln_2() {
        let val = F80::LN_2.to_f64();
        let diff = (val - std::f64::consts::LN_2).abs();
        assert!(diff < 1e-15, "LN_2 diff={diff}");
    }
    #[test]
    fn test_f64_round_trip_normal() {
        for &v in &[0.0, 1.0, -1.0, 3.14159, 1e100, -1e-100, 0.1, 42.0] {
            let f80 = F80::from_f64(v);
            let back = f80.to_f64();
            assert_eq!(v, back, "round-trip failed for {v}");
        }
    }
    #[test]
    fn test_f64_round_trip_zero() {
        let f80 = F80::from_f64(0.0);
        assert_eq!(f80.to_f64(), 0.0);
    }
    #[test]
    fn test_f64_round_trip_neg_zero() {
        let f80 = F80::from_f64(-0.0);
        let back = f80.to_f64();
        assert!(back.is_sign_negative());
        assert_eq!(back, 0.0);
    }
    #[test]
    fn test_f64_round_trip_infinity() {
        let f80 = F80::from_f64(f64::INFINITY);
        assert_eq!(f80.to_f64(), f64::INFINITY);
        let f80 = F80::from_f64(f64::NEG_INFINITY);
        assert_eq!(f80.to_f64(), f64::NEG_INFINITY);
    }
    #[test]
    fn test_f64_round_trip_nan() {
        let f80 = F80::from_f64(f64::NAN);
        assert!(f80.to_f64().is_nan());
    }
    #[test]
    fn test_f32_round_trip() {
        for &v in &[0.0f32, 1.0, -1.0, 3.14, 1e10, -1e-10] {
            let f80 = F80::from_f32(v);
            let back = f80.to_f32();
            assert_eq!(v, back, "f32 round-trip failed for {v}");
        }
    }
    /// MDD-BUG-CRUCIBLE-00261: this compares the INTEGER bit-mapping against
    /// host x87 silicon.
    ///
    /// It used to assert `F80::from_f32(v) == F80::from_f32_via_host_x87(v)`,
    /// but `from_f32` is a one-line delegation to
    /// `from_f32_via_host_x87` — so that was `f(x) == f(x)`, true for any
    /// behaviour of either side, and it oracled nothing. The function actually
    /// needing an oracle is `x87_common::f32_normal_or_zero_to_f80`, the
    /// integer widening the software backend uses in production
    /// (`fpu.rs:950`), which the old assertion never touched.
    ///
    /// Deliberately forward-vs-forward: a round trip through the common
    /// inverse would let paired encode/decode defects cancel and pass.
    #[test]
    fn test_normal_f32_integer_widening_matches_host_x87_bytes() {
        let mut state = 0xbb67_ae85_84ca_a73bu64;
        for _ in 0..20_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let exponent = (((state >> 32) as u32 % 254) + 1) << 23;
            let bits = ((state as u32) & 0x807f_ffff) | exponent;
            let value = f32::from_bits(bits);
            let widened = crate::x87_common::f32_normal_or_zero_to_f80(value)
                .expect("a normal binary32 must be widened by the integer path");
            assert_eq!(
                widened,
                F80::from_f32_via_host_x87(value).0,
                "integer widening of {value:e} ({bits:#010x}) disagrees with host x87"
            );
        }
        for zero in [0.0f32, -0.0f32] {
            let widened = crate::x87_common::f32_normal_or_zero_to_f80(zero)
                .expect("signed zero must be widened by the integer path");
            assert_eq!(
                widened,
                F80::from_f32_via_host_x87(zero).0,
                "integer widening of {zero} ({:#010x}) disagrees with host x87",
                zero.to_bits()
            );
        }
    }
    #[test]
    fn test_f32_infinity() {
        let f80 = F80::from_f32(f32::INFINITY);
        assert_eq!(f80.to_f32(), f32::INFINITY);
    }
    #[test]
    fn test_f32_nan() {
        let f80 = F80::from_f32(f32::NAN);
        assert!(f80.to_f32().is_nan());
    }
    #[test]
    fn test_from_i16_to_i16() {
        for &v in &[0i16, 1, -1, 32767, -32768, 100, -100] {
            let f80 = F80::from_i16(v);
            let (back, invalid) = f80.to_i16(CW_DEFAULT);
            assert!(!invalid, "invalid flag set for {v}");
            assert_eq!(v, back, "i16 round-trip failed for {v}");
        }
    }
    #[test]
    fn test_from_i32_to_i32() {
        for &v in &[0i32, 1, -1, i32::MAX, i32::MIN, 123456, -654321] {
            let f80 = F80::from_i32(v);
            let (back, invalid) = f80.to_i32(CW_DEFAULT);
            assert!(!invalid, "invalid flag set for {v}");
            assert_eq!(v, back, "i32 round-trip failed for {v}");
        }
    }
    #[test]
    fn i32_fast_paths_match_host_x87_oracles() {
        let mut state = 0xd1b5_4a32_d192_ed03u64;
        for _ in 0..20_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let value = state as i32;
            assert_eq!(
                F80(crate::x87_common::i32_to_f80(value)),
                F80::from_i32_via_host_x87(value)
            );
        }
        let values = [
            F80::ZERO,
            F80::NEG_ZERO,
            F80::from_f64(0.25),
            F80::from_f64(-0.25),
            F80::from_f64(0.5),
            F80::from_f64(-0.5),
            F80::from_f64(1.5),
            F80::from_f64(-1.5),
            F80::from_f64(2.5),
            F80::from_f64(-2.5),
            F80::from_f64(i32::MAX as f64),
            F80::from_f64(i32::MAX as f64 + 0.5),
            F80::from_f64(i32::MIN as f64),
            F80::from_f64(i32::MIN as f64 - 0.5),
        ];
        for cw in [CW_DEFAULT, CW_ROUND_DOWN, CW_ROUND_UP, CW_TRUNCATE] {
            for value in values {
                assert_eq!(
                    crate::x87_common::canonical_f80_to_i32(value.bytes(), cw).unwrap(),
                    value.to_i32_status(cw),
                    "value={value:?}, cw={cw:#06x}"
                );
            }
        }
        for _ in 0..20_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let exponent = ((state >> 48) as u16 % 0x7ffe) + 1;
            let sign_exponent = exponent | (((state >> 47) as u16 & 1) << 15);
            let significand = state.rotate_left(29) | 0x8000_0000_0000_0000;
            let mut bytes = [0u8; 10];
            bytes[..8].copy_from_slice(&significand.to_le_bytes());
            bytes[8..].copy_from_slice(&sign_exponent.to_le_bytes());
            let value = F80(bytes);
            for cw in [CW_DEFAULT, CW_ROUND_DOWN, CW_ROUND_UP, CW_TRUNCATE] {
                assert_eq!(
                    crate::x87_common::canonical_f80_to_i32(value.bytes(), cw).unwrap(),
                    value.to_i32_status(cw),
                    "bytes={bytes:02x?}, cw={cw:#06x}"
                );
            }
        }
        let max = crate::x87_common::i32_to_f80(i32::MAX);
        let max_significand = u64::from_le_bytes(max[..8].try_into().unwrap());
        let min = crate::x87_common::i32_to_f80(i32::MIN);
        let min_significand = u64::from_le_bytes(min[..8].try_into().unwrap());
        for (label, base, significand) in [
            ("one ULP below +MAX", max, max_significand - 1),
            ("one ULP above +MAX", max, max_significand + 1),
            ("one ULP more negative than -MIN", min, min_significand + 1),
        ] {
            let mut bytes = base;
            bytes[..8].copy_from_slice(&significand.to_le_bytes());
            let value = F80(bytes);
            for cw in [CW_DEFAULT, CW_ROUND_DOWN, CW_ROUND_UP, CW_TRUNCATE] {
                assert_eq!(
                    crate::x87_common::canonical_f80_to_i32(value.bytes(), cw).unwrap(),
                    value.to_i32_status(cw),
                    "{label}, cw={cw:#06x}"
                );
            }
        }
    }
    #[test]
    fn exact_f32_store_fast_path_matches_host_x87() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..20_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let exponent = (((state >> 32) as u32 % 254) + 1) << 23;
            let bits = ((state as u32) & 0x807f_ffff) | exponent;
            let value = F80::from_f32(f32::from_bits(bits));
            for cw in [CW_DEFAULT, CW_ROUND_DOWN, CW_ROUND_UP, CW_TRUNCATE] {
                assert_eq!(
                    crate::x87_common::f80_to_exact_normal_or_zero_f32(value.bytes())
                        .unwrap()
                        .to_bits(),
                    value.narrow_f32_via_host_x87(cw).to_bits()
                );
            }
        }
        assert_eq!(
            crate::x87_common::f80_to_exact_normal_or_zero_f32(F80::NEG_ZERO.bytes())
                .unwrap()
                .to_bits(),
            F80::NEG_ZERO.narrow_f32_via_host_x87(CW_DEFAULT).to_bits()
        );
    }
    #[test]
    fn test_from_i64_to_i64() {
        for &v in &[0i64, 1, -1, i64::MAX, i64::MIN, 123456789012345] {
            let f80 = F80::from_i64(v);
            let (back, invalid) = f80.to_i64(CW_DEFAULT);
            assert!(!invalid, "invalid flag set for {v}");
            assert_eq!(v, back, "i64 round-trip failed for {v}");
        }
    }
    #[test]
    fn test_to_i16_rounding_modes() {
        let val = F80::from_f64(2.5);
        let (r, _) = val.to_i16(CW_DEFAULT);
        assert_eq!(r, 2);
        let (r, _) = val.to_i16(CW_TRUNCATE);
        assert_eq!(r, 2);
        let (r, _) = val.to_i16(CW_ROUND_DOWN);
        assert_eq!(r, 2);
        let (r, _) = val.to_i16(CW_ROUND_UP);
        assert_eq!(r, 3);
        let neg = F80::from_f64(-2.5);
        let (r, _) = neg.to_i16(CW_DEFAULT);
        assert_eq!(r, -2);
        let (r, _) = neg.to_i16(CW_TRUNCATE);
        assert_eq!(r, -2);
        let (r, _) = neg.to_i16(CW_ROUND_DOWN);
        assert_eq!(r, -3);
        let (r, _) = neg.to_i16(CW_ROUND_UP);
        assert_eq!(r, -2);
    }
    #[test]
    fn test_to_i32_overflow_invalid() {
        let nan = F80::from_f64(f64::NAN);
        let (_, invalid) = nan.to_i32(CW_DEFAULT);
        assert!(invalid);
        let inf = F80::from_f64(f64::INFINITY);
        let (_, invalid) = inf.to_i32(CW_DEFAULT);
        assert!(invalid);
    }
    #[test]
    fn test_i16_truncate() {
        let val = F80::from_f64(2.9);
        let (r, invalid) = val.to_i16_truncate();
        assert!(!invalid);
        assert_eq!(r, 2);
        let neg = F80::from_f64(-2.9);
        let (r, invalid) = neg.to_i16_truncate();
        assert!(!invalid);
        assert_eq!(r, -2);
    }
    #[test]
    fn test_i32_truncate() {
        let val = F80::from_f64(99.99);
        let (r, invalid) = val.to_i32_truncate();
        assert!(!invalid);
        assert_eq!(r, 99);
    }
    #[test]
    fn test_i64_truncate() {
        let val = F80::from_f64(123456789.999);
        let (r, invalid) = val.to_i64_truncate();
        assert!(!invalid);
        assert_eq!(r, 123456789);
    }
    #[test]
    fn test_i64_truncate_reports_precision_status() {
        let val = F80::from_f64(1.5);
        let (r, sw) = val.to_i64_truncate_status();
        assert_eq!(r, 1);
        assert_ne!(sw & 0x0020, 0, "FISTTP inexact must report PE");
    }
    #[test]
    fn test_truncate_nan_invalid() {
        let nan = F80::from_f64(f64::NAN);
        let (_, inv) = nan.to_i16_truncate();
        assert!(inv);
        let (_, inv) = nan.to_i32_truncate();
        assert!(inv);
        let (_, inv) = nan.to_i64_truncate();
        assert!(inv);
    }
    #[test]
    fn test_add() {
        let a = F80::from_f64(1.5);
        let b = F80::from_f64(2.5);
        let r = f80_add(&a, &b, CW_DEFAULT);
        assert_eq!(r.to_f64(), 4.0);
    }
    #[test]
    fn test_sub() {
        let a = F80::from_f64(5.0);
        let b = F80::from_f64(3.0);
        let r = f80_sub(&a, &b, CW_DEFAULT);
        assert_eq!(r.to_f64(), 2.0);
    }
    #[test]
    fn test_sub_negative_result() {
        let a = F80::from_f64(3.0);
        let b = F80::from_f64(5.0);
        let r = f80_sub(&a, &b, CW_DEFAULT);
        assert_eq!(r.to_f64(), -2.0);
    }
    #[test]
    fn test_sub_status_reports_precision_and_c1() {
        let st0 = F80::from_f64(f64::from_bits(0x312A_231C_150E_0700));
        let mem = F80::from_i32(0x150E_0700);
        let (_result, sw) = f80_sub_status(&mem, &st0, CW_DEFAULT);
        assert_eq!(
            sw & 0x0220,
            0x0220,
            "inexact subtraction should report PE+C1"
        );
    }
    #[test]
    fn test_mul() {
        let a = F80::from_f64(3.0);
        let b = F80::from_f64(4.0);
        let r = f80_mul(&a, &b, CW_DEFAULT);
        assert_eq!(r.to_f64(), 12.0);
    }
    #[test]
    fn test_div() {
        let a = F80::from_f64(10.0);
        let b = F80::from_f64(4.0);
        let r = f80_div(&a, &b, CW_DEFAULT);
        assert_eq!(r.to_f64(), 2.5);
    }
    #[test]
    fn test_div_by_zero() {
        let a = F80::from_f64(1.0);
        let b = F80::ZERO;
        let r = f80_div(&a, &b, CW_DEFAULT);
        assert!(r.is_infinite());
    }
    #[test]
    fn test_div_status_zero_over_zero_reports_ie_not_ze() {
        let (_result, sw) = f80_div_status(&F80::ZERO, &F80::ZERO, CW_DEFAULT);
        assert_eq!(
            sw & 0x05,
            0x01,
            "0/0 should report invalid operation, not divide-by-zero"
        );
    }
    #[test]
    fn test_div_status_one_over_zero_reports_ze() {
        let (result, sw) = f80_div_status(&F80::ONE, &F80::ZERO, CW_DEFAULT);
        assert!(result.is_infinite());
        assert_eq!(
            sw & 0x05,
            0x04,
            "non-zero/0 should report divide-by-zero, not invalid operation"
        );
    }
    #[test]
    fn test_div_status_zero_over_one_reports_no_ie_or_ze() {
        let (result, sw) = f80_div_status(&F80::ZERO, &F80::ONE, CW_DEFAULT);
        assert!(result.is_zero());
        assert_eq!(
            sw & 0x05,
            0,
            "0/non-zero should not report invalid operation or divide-by-zero"
        );
    }
    #[test]
    fn test_f80_more_precise_than_f64() {
        let big = F80::from_f64(9007199254740992.0);
        let small_one = F80::ONE;
        let f80_sum = f80_add(&big, &small_one, CW_DEFAULT);
        let f80_diff = f80_sub(&f80_sum, &big, CW_DEFAULT);
        let f80_val = f80_diff.to_f64();
        let f64_val = (9007199254740992.0_f64 + 1.0) - 9007199254740992.0_f64;
        assert_eq!(f80_val, 1.0, "F80 should preserve precision: got {f80_val}");
        assert_eq!(f64_val, 0.0, "f64 should lose precision: got {f64_val}");
    }
    #[test]
    fn test_sin_pi_half() {
        let pi_half = f80_div(&F80::PI, &F80::from_f64(2.0), CW_DEFAULT);
        let (result, c2) = f80_sin(&pi_half, CW_DEFAULT);
        assert!(!c2, "C2 should not be set for small angles");
        assert!((result.to_f64() - 1.0).abs() < 1e-15);
    }
    #[test]
    fn test_cos_zero() {
        let zero = F80::ZERO;
        let (result, c2) = f80_cos(&zero, CW_DEFAULT);
        assert!(!c2);
        assert_eq!(result.to_f64(), 1.0);
    }
    #[test]
    fn test_sqrt_4() {
        let four = F80::from_f64(4.0);
        let result = f80_sqrt(&four, CW_DEFAULT);
        assert_eq!(result.to_f64(), 2.0);
    }
    #[test]
    fn test_sqrt_2() {
        let two = F80::from_f64(2.0);
        let result = f80_sqrt(&two, CW_DEFAULT);
        assert!((result.to_f64() - std::f64::consts::SQRT_2).abs() < 1e-15);
    }
    #[test]
    fn test_sincos() {
        let pi_half = f80_div(&F80::PI, &F80::from_f64(2.0), CW_DEFAULT);
        let (sin_val, cos_val, c2) = f80_sincos(&pi_half, CW_DEFAULT);
        assert!(!c2);
        assert!((sin_val.to_f64() - 1.0).abs() < 1e-15);
        assert!(cos_val.to_f64().abs() < 1e-15);
    }
    #[test]
    fn test_patan() {
        let one = F80::ONE;
        let result = f80_patan(&one, &one, CW_DEFAULT);
        let expected = std::f64::consts::FRAC_PI_4;
        assert!(
            (result.to_f64() - expected).abs() < 1e-15,
            "patan(1,1) = {}, expected {}",
            result.to_f64(),
            expected
        );
    }
    #[test]
    fn test_patan_y0_x1() {
        let zero = F80::ZERO;
        let one = F80::ONE;
        let result = f80_patan(&zero, &one, CW_DEFAULT);
        assert_eq!(result.to_f64(), 0.0);
    }
    #[test]
    fn test_yl2x() {
        let one = F80::ONE;
        let two = F80::from_f64(2.0);
        let result = f80_yl2x(&one, &two, CW_DEFAULT);
        assert_eq!(result.to_f64(), 1.0);
    }
    #[test]
    fn test_yl2x_base_change() {
        let one = F80::ONE;
        let eight = F80::from_f64(8.0);
        let result = f80_yl2x(&one, &eight, CW_DEFAULT);
        assert_eq!(result.to_f64(), 3.0);
    }
    #[test]
    fn test_yl2xp1() {
        let one = F80::ONE;
        let result = f80_yl2xp1(&one, &one, CW_DEFAULT);
        assert_eq!(result.to_f64(), 1.0);
    }
    #[test]
    fn test_prem() {
        let a = F80::from_f64(10.0);
        let b = F80::from_f64(3.0);
        let (result, sw) = f80_prem(&a, &b, CW_DEFAULT);
        assert_eq!(sw & 0x0400, 0, "C2 should be 0 for complete reduction");
        assert_eq!(result.to_f64(), 1.0);
    }
    #[test]
    fn test_prem_negative() {
        let a = F80::from_f64(-10.0);
        let b = F80::from_f64(3.0);
        let (result, _sw) = f80_prem(&a, &b, CW_DEFAULT);
        assert_eq!(result.to_f64(), -1.0);
    }
    #[test]
    fn test_prem1() {
        let a = F80::from_f64(10.0);
        let b = F80::from_f64(3.0);
        let (result, sw) = f80_prem1(&a, &b, CW_DEFAULT);
        assert_eq!(sw & 0x0400, 0, "C2 should be 0");
        assert_eq!(result.to_f64(), 1.0);
    }
    #[test]
    fn test_prem1_rounds_to_nearest() {
        let a = F80::from_f64(7.0);
        let b = F80::from_f64(4.0);
        let (result, _sw) = f80_prem1(&a, &b, CW_DEFAULT);
        assert_eq!(result.to_f64(), -1.0);
    }
    #[test]
    fn test_compare_less() {
        let a = F80::from_f64(1.0);
        let b = F80::from_f64(2.0);
        let sw = f80_compare(&a, &b);
        assert_ne!(sw & 0x0100, 0, "C0 should be set (a < b)");
        assert_eq!(sw & 0x4000, 0, "C3 should be clear");
        assert_eq!(sw & 0x0400, 0, "C2 should be clear");
    }
    #[test]
    fn test_compare_equal() {
        let a = F80::from_f64(3.0);
        let b = F80::from_f64(3.0);
        let sw = f80_compare(&a, &b);
        assert_eq!(sw & 0x0100, 0, "C0 should be clear");
        assert_ne!(sw & 0x4000, 0, "C3 should be set (equal)");
        assert_eq!(sw & 0x0400, 0, "C2 should be clear");
    }
    #[test]
    fn test_compare_greater() {
        let a = F80::from_f64(5.0);
        let b = F80::from_f64(2.0);
        let sw = f80_compare(&a, &b);
        assert_eq!(sw & 0x0100, 0, "C0 should be clear");
        assert_eq!(sw & 0x4000, 0, "C3 should be clear");
        assert_eq!(sw & 0x0400, 0, "C2 should be clear");
    }
    #[test]
    fn test_compare_unordered_nan() {
        let a = F80::from_f64(f64::NAN);
        let b = F80::from_f64(1.0);
        let sw = f80_compare(&a, &b);
        assert_ne!(sw & 0x0100, 0, "C0 should be set (unordered)");
        assert_ne!(sw & 0x0400, 0, "C2 should be set (unordered)");
        assert_ne!(sw & 0x4000, 0, "C3 should be set (unordered)");
    }
    #[test]
    fn test_abs_positive() {
        let a = F80::from_f64(3.0);
        let r = f80_abs(&a);
        assert_eq!(r.to_f64(), 3.0);
    }
    #[test]
    fn test_abs_negative() {
        let a = F80::from_f64(-3.0);
        let r = f80_abs(&a);
        assert_eq!(r.to_f64(), 3.0);
    }
    #[test]
    fn test_abs_neg_zero() {
        let r = f80_abs(&F80::NEG_ZERO);
        assert_eq!(r.to_f64(), 0.0);
        assert!(!r.to_f64().is_sign_negative());
    }
    #[test]
    fn test_chs_positive() {
        let a = F80::from_f64(3.0);
        let r = f80_chs(&a);
        assert_eq!(r.to_f64(), -3.0);
    }
    #[test]
    fn test_chs_negative() {
        let a = F80::from_f64(-3.0);
        let r = f80_chs(&a);
        assert_eq!(r.to_f64(), 3.0);
    }
    #[test]
    fn test_chs_zero() {
        let r = f80_chs(&F80::ZERO);
        assert!(r.to_f64().is_sign_negative());
    }
    #[test]
    fn test_xtract() {
        let a = F80::from_f64(8.0);
        let (sig, exp) = f80_xtract(&a, CW_DEFAULT);
        assert_eq!(sig.to_f64(), 1.0, "significand of 8.0");
        assert_eq!(exp.to_f64(), 3.0, "exponent of 8.0");
    }
    #[test]
    fn test_xtract_one() {
        let a = F80::ONE;
        let (sig, exp) = f80_xtract(&a, CW_DEFAULT);
        assert_eq!(sig.to_f64(), 1.0);
        assert_eq!(exp.to_f64(), 0.0);
    }
    #[test]
    fn test_xtract_fraction() {
        let a = F80::from_f64(0.5);
        let (sig, exp) = f80_xtract(&a, CW_DEFAULT);
        assert_eq!(sig.to_f64(), 1.0);
        assert_eq!(exp.to_f64(), -1.0);
    }
    #[test]
    fn test_scale() {
        let a = F80::ONE;
        let b = F80::from_f64(3.0);
        let result = f80_scale(&a, &b, CW_DEFAULT);
        assert_eq!(result.to_f64(), 8.0);
    }
    #[test]
    fn test_scale_negative_exponent() {
        let a = F80::ONE;
        let b = F80::from_f64(-1.0);
        let result = f80_scale(&a, &b, CW_DEFAULT);
        assert_eq!(result.to_f64(), 0.5);
    }
    #[test]
    fn test_scale_truncates_exponent() {
        let a = F80::ONE;
        let b = F80::from_f64(2.9);
        let result = f80_scale(&a, &b, CW_DEFAULT);
        assert_eq!(result.to_f64(), 4.0);
    }
    #[test]
    fn test_rndint_nearest() {
        let a = F80::from_f64(2.5);
        let r = f80_rndint(&a, CW_DEFAULT);
        assert_eq!(r.to_f64(), 2.0, "round to nearest even: 2.5 -> 2");
        let b = F80::from_f64(3.5);
        let r = f80_rndint(&b, CW_DEFAULT);
        assert_eq!(r.to_f64(), 4.0, "round to nearest even: 3.5 -> 4");
    }
    #[test]
    fn test_rndint_truncate() {
        let a = F80::from_f64(2.9);
        let r = f80_rndint(&a, CW_TRUNCATE);
        assert_eq!(r.to_f64(), 2.0);
        let b = F80::from_f64(-2.9);
        let r = f80_rndint(&b, CW_TRUNCATE);
        assert_eq!(r.to_f64(), -2.0);
    }
    #[test]
    fn test_rndint_down() {
        let a = F80::from_f64(2.1);
        let r = f80_rndint(&a, CW_ROUND_DOWN);
        assert_eq!(r.to_f64(), 2.0);
        let b = F80::from_f64(-2.1);
        let r = f80_rndint(&b, CW_ROUND_DOWN);
        assert_eq!(r.to_f64(), -3.0);
    }
    #[test]
    fn test_rndint_up() {
        let a = F80::from_f64(2.1);
        let r = f80_rndint(&a, CW_ROUND_UP);
        assert_eq!(r.to_f64(), 3.0);
        let b = F80::from_f64(-2.1);
        let r = f80_rndint(&b, CW_ROUND_UP);
        assert_eq!(r.to_f64(), -2.0);
    }
    #[test]
    fn test_bcd_round_trip() {
        let val = F80::from_f64(12345.0);
        let bcd = val.to_bcd(CW_DEFAULT);
        let back = F80::from_bcd(&bcd);
        assert_eq!(back.to_f64(), 12345.0);
    }
    #[test]
    fn test_bcd_negative() {
        let val = F80::from_f64(-999.0);
        let bcd = val.to_bcd(CW_DEFAULT);
        let back = F80::from_bcd(&bcd);
        assert_eq!(back.to_f64(), -999.0);
    }
    #[test]
    fn test_bcd_zero() {
        let val = F80::ZERO;
        let bcd = val.to_bcd(CW_DEFAULT);
        let back = F80::from_bcd(&bcd);
        assert_eq!(back.to_f64(), 0.0);
    }
    #[test]
    fn test_f80_bytes() {
        let one = F80::ONE;
        let bytes = one.bytes();
        assert_eq!(bytes[7], 0x80);
        assert_eq!(bytes[8], 0xFF);
        assert_eq!(bytes[9], 0x3F);
    }
    #[test]
    fn test_f80_default() {
        let d = F80::default();
        assert!(d.is_zero());
        assert_eq!(d, F80::ZERO);
    }
    #[test]
    fn test_f80_debug() {
        let one = F80::ONE;
        let s = format!("{:?}", one);
        assert_eq!(s, "F80(1)");
    }
    #[test]
    fn test_f80_partial_eq() {
        let a = F80::from_f64(1.0);
        let b = F80::from_f64(1.0);
        let c = F80::from_f64(2.0);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
    #[test]
    fn test_f80_clone_copy() {
        let a = F80::from_f64(42.0);
        let b = a;
        let c = a;
        assert_eq!(a, b);
        assert_eq!(a, c);
    }
    #[test]
    fn test_f2xm1() {
        let zero = F80::ZERO;
        let result = f80_f2xm1(&zero, CW_DEFAULT);
        assert_eq!(result.to_f64(), 0.0);
        let one = F80::ONE;
        let result = f80_f2xm1(&one, CW_DEFAULT);
        assert!((result.to_f64() - 1.0).abs() < 1e-15);
        let half = F80::from_f64(0.5);
        let result = f80_f2xm1(&half, CW_DEFAULT);
        let expected = 2.0_f64.sqrt() - 1.0;
        assert!((result.to_f64() - expected).abs() < 1e-15);
    }
    #[test]
    fn test_f2xm1_observes_control_word_rounding() {
        let half = F80::from_f64(0.5);
        let down = f80_f2xm1(&half, CW_ROUND_DOWN);
        let up = f80_f2xm1(&half, CW_ROUND_UP);
        assert_ne!(
            down.bytes(),
            up.bytes(),
            "directed rounding must affect inexact F2XM1 result"
        );
    }
    #[test]
    fn test_ptan() {
        let pi_quarter = f80_div(&F80::PI, &F80::from_f64(4.0), CW_DEFAULT);
        let (tan_val, one_val, c2, _sw) = f80_ptan(&pi_quarter, CW_DEFAULT);
        assert!(!c2);
        assert!((tan_val.to_f64() - 1.0).abs() < 1e-14);
        assert_eq!(one_val.to_f64(), 1.0);
    }
    #[test]
    fn test_ptan_status_reports_precision_and_c1() {
        let (_tan_val, _one_val, c2, sw) = f80_ptan(&F80::from_f64(1.0), CW_DEFAULT);
        assert!(!c2);
        assert_eq!(sw & 0x0220, 0x0220, "FPTAN should report PE+C1");
    }
    #[test]
    fn test_compare_and_pop() {
        let a = F80::from_f64(1.0);
        let b = F80::from_f64(2.0);
        let sw1 = f80_compare(&a, &b);
        let sw2 = f80_compare_and_pop(&a, &b);
        assert_eq!(sw1, sw2);
    }
    #[test]
    fn test_sincos_out_of_range() {
        let huge = F80::from_f64(1e20);
        let (sin_val, _cos_val, c2) = f80_sincos(&huge, CW_DEFAULT);
        assert!(c2, "C2 should be set for huge operand");
        assert_eq!(sin_val, huge, "out-of-range should return original operand");
    }
    #[test]
    fn test_ptan_out_of_range() {
        let huge = F80::from_f64(1e20);
        let (tan_val, _one_val, c2, _sw) = f80_ptan(&huge, CW_DEFAULT);
        assert!(c2, "C2 should be set for huge operand");
        assert_eq!(tan_val, huge, "out-of-range should return original operand");
    }
    #[test]
    fn test_stale_status_word_no_contamination() {
        let _inf = f80_div(&F80::ONE, &F80::ZERO, CW_DEFAULT);
        let val = F80::from_f64(42.0);
        let (result, invalid) = val.to_i32(CW_DEFAULT);
        assert!(!invalid, "stale ZE flag should not contaminate to_i32");
        assert_eq!(result, 42);
    }
    #[test]
    fn test_stale_status_word_no_contamination_truncate() {
        let _inf = f80_div(&F80::ONE, &F80::ZERO, CW_DEFAULT);
        let val = F80::from_f64(42.0);
        let (result, invalid) = val.to_i32_truncate();
        assert!(
            !invalid,
            "stale ZE flag should not contaminate to_i32_truncate"
        );
        assert_eq!(result, 42);
    }
    #[test]
    fn test_to_f32_with_cw_rounding() {
        let one = F80::from_f64(1.0);
        let tiny = F80::from_f64(f64::from_bits(0x3E60000000000000));
        let mid = F80::from_f64(f64::from_bits(0x3E70000000000000));
        let val = f80_add(&one, &f80_add(&mid, &tiny, CW_DEFAULT), CW_DEFAULT);
        let trunc = val.to_f32_with_cw(CW_TRUNCATE);
        assert_eq!(trunc, 1.0f32, "truncate should round toward zero");
        let up = val.to_f32_with_cw(CW_ROUND_UP);
        assert!(up > 1.0f32, "round up should give value > 1.0");
        let down = val.to_f32_with_cw(CW_ROUND_DOWN);
        assert_eq!(
            down, 1.0f32,
            "round down should give 1.0 for positive value"
        );
    }
    #[test]
    fn test_to_f64_with_cw_rounding() {
        let val = F80::from_f64(std::f64::consts::PI);
        let result = val.to_f64_with_cw(CW_DEFAULT);
        assert_eq!(result, std::f64::consts::PI);
    }
    #[test]
    fn test_to_i16_indefinite_nan() {
        let nan = F80::from_f64(f64::NAN);
        let (val, invalid) = nan.to_i16(CW_DEFAULT);
        assert!(invalid);
        assert_eq!(val, i16::MIN, "NaN should return integer indefinite 0x8000");
    }
    #[test]
    fn test_to_i16_indefinite_inf() {
        let inf = F80::from_f64(f64::INFINITY);
        let (val, invalid) = inf.to_i16(CW_DEFAULT);
        assert!(invalid);
        assert_eq!(val, i16::MIN, "Inf should return integer indefinite 0x8000");
    }
    #[test]
    fn test_to_i32_indefinite_nan() {
        let nan = F80::from_f64(f64::NAN);
        let (val, invalid) = nan.to_i32(CW_DEFAULT);
        assert!(invalid);
        assert_eq!(
            val,
            i32::MIN,
            "NaN should return integer indefinite 0x80000000"
        );
    }
    #[test]
    fn test_to_i32_indefinite_inf() {
        let inf = F80::from_f64(f64::INFINITY);
        let (val, invalid) = inf.to_i32(CW_DEFAULT);
        assert!(invalid);
        assert_eq!(
            val,
            i32::MIN,
            "Inf should return integer indefinite 0x80000000"
        );
    }
    #[test]
    fn test_to_i64_indefinite_nan() {
        let nan = F80::from_f64(f64::NAN);
        let (val, invalid) = nan.to_i64(CW_DEFAULT);
        assert!(invalid);
        assert_eq!(
            val,
            i64::MIN,
            "NaN should return integer indefinite 0x8000000000000000"
        );
    }
    #[test]
    fn test_to_i64_indefinite_inf() {
        let inf = F80::from_f64(f64::INFINITY);
        let (val, invalid) = inf.to_i64(CW_DEFAULT);
        assert!(invalid);
        assert_eq!(
            val,
            i64::MIN,
            "Inf should return integer indefinite 0x8000000000000000"
        );
    }
    #[test]
    fn test_to_i16_truncate_indefinite() {
        let nan = F80::from_f64(f64::NAN);
        let (val, invalid) = nan.to_i16_truncate();
        assert!(invalid);
        assert_eq!(val, i16::MIN);
        let inf = F80::from_f64(f64::INFINITY);
        let (val, invalid) = inf.to_i16_truncate();
        assert!(invalid);
        assert_eq!(val, i16::MIN);
    }
    #[test]
    fn test_to_i32_truncate_indefinite() {
        let nan = F80::from_f64(f64::NAN);
        let (val, invalid) = nan.to_i32_truncate();
        assert!(invalid);
        assert_eq!(val, i32::MIN);
        let inf = F80::from_f64(f64::INFINITY);
        let (val, invalid) = inf.to_i32_truncate();
        assert!(invalid);
        assert_eq!(val, i32::MIN);
    }
    #[test]
    fn test_to_i64_truncate_indefinite() {
        let nan = F80::from_f64(f64::NAN);
        let (val, invalid) = nan.to_i64_truncate();
        assert!(invalid);
        assert_eq!(val, i64::MIN);
        let inf = F80::from_f64(f64::INFINITY);
        let (val, invalid) = inf.to_i64_truncate();
        assert!(invalid);
        assert_eq!(val, i64::MIN);
    }
    #[test]
    fn test_abs_nan() {
        let nan = F80::from_f64(f64::NAN);
        let result = f80_abs(&nan);
        assert!(result.is_nan(), "abs of NaN should still be NaN");
        assert!(
            !result.is_sign_negative(),
            "abs of NaN should clear sign bit"
        );
    }
    #[test]
    fn test_abs_neg_nan() {
        let mut nan = F80::from_f64(f64::NAN);
        nan.0[9] |= 0x80;
        assert!(nan.is_sign_negative());
        let result = f80_abs(&nan);
        assert!(result.is_nan(), "abs of -NaN should still be NaN");
        assert!(
            !result.is_sign_negative(),
            "abs of -NaN should clear sign bit"
        );
    }
    #[test]
    fn test_chs_nan() {
        let nan = F80::from_f64(f64::NAN);
        let was_neg = nan.is_sign_negative();
        let result = f80_chs(&nan);
        assert!(result.is_nan(), "chs of NaN should still be NaN");
        assert_ne!(
            result.is_sign_negative(),
            was_neg,
            "chs should flip sign of NaN"
        );
    }
    #[test]
    fn test_abs_neg_infinity() {
        let neg_inf = F80::from_f64(f64::NEG_INFINITY);
        let result = f80_abs(&neg_inf);
        assert!(result.is_infinite());
        assert!(!result.is_sign_negative());
    }
    #[test]
    fn test_chs_infinity() {
        let inf = F80::from_f64(f64::INFINITY);
        let result = f80_chs(&inf);
        assert!(result.is_infinite());
        assert!(result.is_sign_negative());
    }
    #[test]
    fn test_is_denormal() {
        assert!(!F80::ONE.is_denormal());
        assert!(!F80::ZERO.is_denormal());
        assert!(!F80::NEG_ZERO.is_denormal());
        assert!(!F80::from_f64(f64::INFINITY).is_denormal());
        assert!(!F80::from_f64(f64::NAN).is_denormal());
        let mut denorm = F80::ZERO;
        denorm.0[0] = 1;
        assert!(
            denorm.is_denormal(),
            "zero exponent + nonzero mantissa = denormal"
        );
        let mut denorm2 = F80::ZERO;
        denorm2.0[7] = 0x40;
        assert!(denorm2.is_denormal());
    }
    #[test]
    fn test_f80_eq() {
        let a = F80::from_f64(1.0);
        let b = F80::from_f64(1.0);
        let c = F80::from_f64(1.0);
        assert_eq!(a, a);
        assert_eq!(a, b);
        assert_eq!(b, a);
        assert_eq!(a, c);
        assert_eq!(b, c);
    }
    #[test]
    fn test_helpers_survive_hostile_host_and_guest_fpu_cw() {
        let hostile_cw: u16 = 0x0300;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fnclex", "fldcw word ptr [{cw}]", cw = in (reg) & hostile_cw,
                options(nostack),
            );
        }
        let one = F80::ONE;
        let two = F80::from_f64(2.0);
        let _ = one.to_f64();
        let _ = one.to_f32();
        let _ = F80::from_f64(1.0);
        let _ = F80::from_f32(1.0);
        let _ = F80::from_i16(1);
        let _ = F80::from_i32(1);
        let _ = F80::from_i64(1);
        let _ = one.to_i16_truncate();
        let _ = one.to_i32_truncate();
        let _ = one.to_i64_truncate();
        let _ = f80_xtract(&one, hostile_cw);
        let _ = f80_compare(&one, &two);
        let _ = f80_sin(&one, hostile_cw);
        let _ = f80_cos(&one, hostile_cw);
        let _ = f80_sincos(&one, hostile_cw);
        let (_tan, _one, c2, sw) = f80_ptan(&one, hostile_cw);
        assert!(!c2);
        assert_ne!(sw & 0x0020, 0, "hostile CW must still mask host PE");
        let _ = f80_patan(&one, &two, hostile_cw);
        let _ = f80_yl2x(&two, &one, hostile_cw);
        let _ = f80_yl2xp1(&one, &F80::from_f64(0.5), hostile_cw);
        let _ = f80_f2xm1(&F80::from_f64(0.5), hostile_cw);
        let _ = f80_scale(&one, &two, hostile_cw);
        let _ = f80_prem(&one, &two, hostile_cw);
        let _ = f80_prem1(&one, &two, hostile_cw);
        let _ = F80::from_bcd(&[0x45, 0x23, 0x01, 0, 0, 0, 0, 0, 0, 0]);
        let safe: u16 = 0x037F;
        unsafe {
            let _host_x87 = host_x87_enter_guarded();
            asm!(
                "fnclex", "fldcw word ptr [{cw}]", cw = in (reg) & safe,
                options(nostack),
            );
        }
    }
    /// Values chosen to cover sign, fractional truncation direction, the
    /// exact representable boundaries, and out-of-range inputs (which must
    /// produce the x87 integer indefinite plus IE rather than saturate).
    const TRUNCATE_CASES: [f64; 15] = [
        0.0, -0.0, 1.0, -1.0, 1.9, -1.9, 0.5, -0.5, 2.5, -2.5, 100.75, -100.75, 3.0, -3.0,
        12345.678,
    ];
    /// The two backends must agree bit-for-bit on value AND status word. The
    /// fallback uses `FISTP` under a round-toward-zero control word, which is
    /// what `FISTTP` does unconditionally — if that equivalence ever breaks,
    /// a non-SSE3 host would silently round differently.
    #[test]
    fn fisttp_and_x87_truncation_backends_agree_crucible260() {
        for &v in TRUNCATE_CASES.iter() {
            let f = F80::from_f64(v);
            assert_eq!(
                f.to_i16_truncate_status_fisttp(),
                f.to_i16_truncate_status_x87(),
                "i16 backends disagree on {v}"
            );
            assert_eq!(
                f.to_i32_truncate_status_fisttp(),
                f.to_i32_truncate_status_x87(),
                "i32 backends disagree on {v}"
            );
            assert_eq!(
                f.to_i64_truncate_status_fisttp(),
                f.to_i64_truncate_status_x87(),
                "i64 backends disagree on {v}"
            );
        }
    }
    /// Both backends must agree with an independent oracle — Rust's `as`
    /// conversion, which also truncates toward zero — for in-range inputs.
    #[test]
    fn both_truncation_backends_match_the_rust_oracle_crucible260() {
        for &v in TRUNCATE_CASES.iter() {
            let f = F80::from_f64(v);
            assert_eq!(
                f.to_i16_truncate_status_fisttp().0,
                v as i16,
                "fisttp i16 {v}"
            );
            assert_eq!(f.to_i16_truncate_status_x87().0, v as i16, "x87 i16 {v}");
            assert_eq!(
                f.to_i32_truncate_status_fisttp().0,
                v as i32,
                "fisttp i32 {v}"
            );
            assert_eq!(f.to_i32_truncate_status_x87().0, v as i32, "x87 i32 {v}");
            assert_eq!(
                f.to_i64_truncate_status_fisttp().0,
                v as i64,
                "fisttp i64 {v}"
            );
            assert_eq!(f.to_i64_truncate_status_x87().0, v as i64, "x87 i64 {v}");
        }
    }
    /// Out-of-range inputs must give the x87 integer indefinite with IE set,
    /// on BOTH backends — not the saturation Rust's `as` performs.
    #[test]
    fn both_truncation_backends_signal_out_of_range_crucible260() {
        let too_big_16 = F80::from_f64(40_000.0);
        for (value, status) in [
            too_big_16.to_i16_truncate_status_fisttp(),
            too_big_16.to_i16_truncate_status_x87(),
        ] {
            assert_eq!(value, i16::MIN, "out-of-range i16 must be the indefinite");
            assert_ne!(status & 0x0001, 0, "out-of-range i16 must raise IE");
        }
        let too_big_32 = F80::from_f64(3.0e9);
        for (value, status) in [
            too_big_32.to_i32_truncate_status_fisttp(),
            too_big_32.to_i32_truncate_status_x87(),
        ] {
            assert_eq!(value, i32::MIN, "out-of-range i32 must be the indefinite");
            assert_ne!(status & 0x0001, 0, "out-of-range i32 must raise IE");
        }
    }
    /// The public entry point must route to a real backend and return what
    /// that backend returns — the SSE3 gate must not change results on a host
    /// that has SSE3, nor on one that does not.
    #[test]
    fn truncation_dispatch_matches_the_selected_backend_crucible260() {
        for &v in TRUNCATE_CASES.iter() {
            let f = F80::from_f64(v);
            let expected_32 = if host_has_fisttp() {
                f.to_i32_truncate_status_fisttp()
            } else {
                f.to_i32_truncate_status_x87()
            };
            assert_eq!(
                f.to_i32_truncate_status(),
                expected_32,
                "dispatch mismatch on {v}"
            );
            assert_eq!(f.to_i32_truncate(), (expected_32.0, expected_32.1 & 1 != 0));
        }
    }
    /// The fallback's control word must be the default one with RC set to
    /// round-toward-zero, matching the suite's own `CW_TRUNCATE`.
    #[test]
    fn truncating_control_word_is_round_toward_zero_crucible260() {
        assert_eq!(SAFE_TRUNCATE_CW, CW_TRUNCATE);
        assert_eq!(SAFE_TRUNCATE_CW & 0x0C00, 0x0C00, "RC must be truncate");
        assert_eq!(SAFE_TRUNCATE_CW & 0x3F, 0x3F, "all exceptions stay masked");
    }
}
